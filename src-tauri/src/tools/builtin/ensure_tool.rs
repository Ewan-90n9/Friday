use crate::provision::package::{ProvisionContext, StageTimeouts, ToolPackage};
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct EnsureToolHandler {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub cache_dir: std::path::PathBuf,
    pub bus: crate::app::events::EventBus,
    /// jvm_* 工具共享的 JDK 布局缓存：成功后写入
    pub jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
    /// (env_id, package) → 串行化锁
    pub inflight: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[async_trait]
impl ToolHandler for EnsureToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(tool) = args.get("tool").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: tool");
        };
        let java_bin = args.get("java_bin").and_then(|v| v.as_str()).unwrap_or("java");

        if tool != "jdk" {
            return error_output(
                "invalid_params",
                &format!("unknown tool package: {tool:?}. supported packages: jdk"),
            );
        }

        // 按名称查环境
        let env = match crate::app::environments::find_by_name(&self.db, environment).await {
            Ok(Some(env)) => env,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("lookup_failed", &format!("查询环境失败: {e}")),
        };

        // 获取 channel
        let channel = {
            let mut pool = self.exec_pool.lock().await;
            match pool.get_or_create(&env.id, &self.db).await {
                Ok(ch) => ch,
                Err(e) => {
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "ensure_tool: failed to get exec channel");
                    return error_output("connection_error", &format!("{e} (host: {})", env.host));
                }
            }
        };

        // 读取 Artifactory 设置
        let base_url = match crate::app::settings::artifactory_base_url(&self.db).await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "ensure_tool: read artifactory base url failed");
                return error_output("internal_error", &format!("读取 Artifactory 设置失败: {e}"));
            }
        };

        let pctx = ProvisionContext {
            session_id: ctx.session_id.clone(),
            env_id: env.id.clone(),
            channel,
            cache_dir: self.cache_dir.clone(),
            artifactory_base_url: base_url,
            arthas_zip: None,
            timeouts: StageTimeouts::default(),
            bus: self.bus.clone(),
        };

        // (env_id, package) 串行化：并发请求排队，后者进锁后 ensure 会重新查远端缓存
        let lock_key = format!("{}/{}", env.id, tool);
        let per_key = {
            let mut inflight = self.inflight.lock().await;
            inflight
                .entry(lock_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = per_key.lock().await;

        let package = crate::provision::jdk::JdkPackage;
        match package.ensure(&pctx, java_bin).await {
            Ok(result) => {
                tracing::info!(session_id = %ctx.session_id, env_id = %env.id, tool, cached = result.cached, elapsed_ms = result.elapsed_ms, "ensure_tool succeeded");
                // 成功即写入 JdkCache：jvm_* 工具按 env_id 取路径
                let layout = crate::tools::builtin::jvm::jdk_cache::JdkLayout {
                    tool_home: result.tool_home.clone(),
                    bins: result.bins.clone(),
                };
                self.jdk_cache.set(&env.id, layout).await;
                ToolOutput {
                    success: true,
                    data: serde_json::to_value(&result).unwrap_or_default(),
                    raw_stdout: None,
                }
            }
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, tool, code = %e.code, stage = %e.stage, error = %e.message, "ensure_tool failed");
                let mut data = serde_json::json!({
                    "error": e.code,
                    "stage": e.stage,
                    "message": e.message,
                });
                if let Some(url) = &e.url {
                    data["url"] = serde_json::json!(url);
                }
                ToolOutput { success: false, data, raw_stdout: None }
            }
        }
    }
}

pub fn ensure_tool_tool_def(
    db: sqlx::SqlitePool,
    exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    cache_dir: std::path::PathBuf,
    bus: crate::app::events::EventBus,
    jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
) -> ToolDef {
    ToolDef {
        name: "ensure_tool".to_string(),
        description: "确保目标环境已装备指定诊断工具包（当前支持 jdk）。生产环境通常只有 JRE，缺少 jstat/jcmd 等诊断工具；本工具探测目标 JVM 版本并下载匹配的 JDK 到 /tmp/friday-tools（不影响系统 Java）。装备成功后即可直接调用 jvm_gc_stats / jvm_thread_dump / jvm_heap_info / jvm_vm_info / jvm_class_histogram / jvm_heap_dump 等结构化工具。重复调用安全：已装备时直接返回。JVM 诊断流程：list_environments → list_processes（keyword=服务名）找 pid → ensure_tool → jvm_* 工具。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "tool": { "type": "string", "enum": ["jdk"], "description": "要装备的工具包名" },
                "java_bin": { "type": "string", "description": "目标服务使用的 java 可执行文件路径，默认 java（多版本共存时从服务进程命令行确认后传入）" }
            },
            "required": ["environment", "tool"]
        }),
        risk_level: RiskLevel::Low,
        category: ToolCategory::Environment,
        needs_channel: false,
        handler: Arc::new(EnsureToolHandler {
            db,
            exec_pool,
            cache_dir,
            bus,
            jdk_cache,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }),
    }
}

fn error_output(error: &str, message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": error, "message": message }),
        raw_stdout: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};

    /// 对所有命令返回探测输出且 exit 0：probe 成功 + 缓存检查命中 → cached:true
    struct ProbeOkChannel;

    #[async_trait]
    impl ExecChannel for ProbeOkChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput {
                stdout: "openjdk version \"21.0.11\" 2025-04-15\nBiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n".into(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, _local: &std::path::Path, _remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
    }

    async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool, Arc<Mutex<crate::exec::pool::ExecChannelPool>>, std::path::PathBuf, crate::app::events::EventBus) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::env_save::save_environment(
            &db, None, "prod", "10.0.0.1", 22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        ).await.unwrap();
        let exec_pool = Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        (tmp, db, exec_pool, cache, crate::app::events::EventBus::disabled())
    }

    fn make_handler(
        db: sqlx::SqlitePool,
        exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
        cache: std::path::PathBuf,
        bus: crate::app::events::EventBus,
    ) -> EnsureToolHandler {
        EnsureToolHandler {
            db,
            exec_pool,
            cache_dir: cache,
            bus,
            jdk_cache: Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new()),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn test_missing_environment_param() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"tool": "jdk"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_missing_tool_param() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_tool_package() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "tool": "arthas"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        assert!(out.data["message"].as_str().unwrap().contains("jdk"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment_guides_agent() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "nope", "tool": "jdk"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        assert!(out.data["message"].as_str().unwrap().contains("list_environments"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_probe_and_cache_hit_flow() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        exec_pool.lock().await.insert_channel(env_id, Arc::new(ProbeOkChannel) as Arc<dyn ExecChannel>).await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "tool": "jdk"}), &ctx).await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["cached"], true);
        assert_eq!(out.data["tool_home"], "/tmp/friday-tools/jdk-21.0.11");
        assert_eq!(out.data["bins"]["jcmd"], "/tmp/friday-tools/jdk-21.0.11/bin/jcmd");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let def = ensure_tool_tool_def(
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new())),
            std::path::PathBuf::from("/tmp/x"),
            crate::app::events::EventBus::disabled(),
            Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new()),
        );
        assert_eq!(def.name, "ensure_tool");
        assert_eq!(def.risk_level, RiskLevel::Low);
        assert!(!def.needs_channel);
    }

    #[tokio::test]
    async fn test_ensure_success_populates_jdk_cache() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        exec_pool.lock().await.insert_channel(env_id.clone(), Arc::new(ProbeOkChannel) as Arc<dyn ExecChannel>).await;
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        let handler = EnsureToolHandler {
            db,
            exec_pool,
            cache_dir: cache,
            bus,
            jdk_cache: jdk_cache.clone(),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "tool": "jdk"}), &ctx).await;
        assert!(out.success, "out: {}", out.data);
        let layout = jdk_cache.get(&env_id).await.expect("cache must be populated");
        assert_eq!(layout.tool_home, "/tmp/friday-tools/jdk-21.0.11");
        assert!(layout.bins.contains_key("jcmd"));
        drop(tmp);
    }
}
