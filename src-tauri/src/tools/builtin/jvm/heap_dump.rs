use crate::tools::builtin::jvm::core::{
    clamp_or, error_output, is_jdk_missing, parse_pid, require_bins, resolve_environment,
    JvmExecCore,
};
use crate::tools::builtin::run_command::artifact_dir_for;
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

const DUMP_DEFAULT_TIMEOUT_SECS: u64 = 300;
const DUMP_MAX_TIMEOUT_SECS: u64 = 600;

pub struct HeapDumpHandler {
    pub core: Arc<JvmExecCore>,
    pub bus: crate::app::events::EventBus,
    pub transfer: Arc<crate::transfer::TransferManager>,
}

#[async_trait]
impl ToolHandler for HeapDumpHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
            return error_output("invalid_params", "pid 必须是正整数字符串");
        };
        let dump_timeout = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            DUMP_DEFAULT_TIMEOUT_SECS,
            DUMP_MAX_TIMEOUT_SECS,
        );

        let (env, channel) = match resolve_environment(
            &self.core.db,
            &self.core.exec_pool,
            environment,
        )
        .await
        {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("connection_error", &e),
        };

        // JDK 路径：查缓存，miss 引导 ensure_tool
        let Some(layout) = self.core.jdk_cache.get(&env.id).await else {
            tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk not provisioned (cache miss)");
            return error_output(
                "jdk_not_provisioned",
                "该环境尚未装备 JDK。请先调用 ensure_tool(environment, tool=\"jdk\") 装备，然后重试本工具。",
            );
        };
        let bins = match require_bins(&layout, &["jcmd"]) {
            Ok(b) => b,
            Err(e) => return error_output("jdk_not_provisioned", &e),
        };
        let jcmd = &bins[0];

        // ① 生成（文件名 Friday 固定构造——不开放自定义，注入面）
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let remote_path = format!("/tmp/friday-tools/heapdump-{pid}-{ts}.hprof");
        let dump_cmd = format!("{jcmd} {pid} GC.heap_dump {remote_path}");

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command = %dump_cmd, "heap dump: generating");
        let dump_start = std::time::Instant::now();

        let dump_result = tokio::time::timeout(
            std::time::Duration::from_secs(dump_timeout),
            channel.run(&dump_cmd),
        )
        .await;
        let dump_elapsed_ms = dump_start.elapsed().as_millis() as u64;

        let dump_output = match dump_result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs = dump_timeout, "heap dump generation timed out, dropping connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                return error_output(
                    "timeout_error",
                    &format!("heap dump generation timed out after {dump_timeout}s; ssh connection closed"),
                );
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "heap dump exec failed");
                return error_output("connection_error", &e.to_string());
            }
            Ok(Ok(output)) => {
                if is_jdk_missing(output.exit_code, &output.stderr) {
                    tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk missing on remote, clearing cache");
                    self.core.jdk_cache.clear(&env.id).await;
                    return error_output(
                        "jdk_missing_on_remote",
                        "远端 JDK 已不存在（可能 /tmp 被清理）。请重新调用 ensure_tool 装备后重试。",
                    );
                }
                if output.exit_code != 0 {
                    // dump 命令失败：透传 jcmd 输出
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, exit_code = output.exit_code, "heap dump command failed");
                    return ToolOutput {
                        success: false,
                        data: serde_json::json!({
                            "error": "dump_failed",
                            "message": "GC.heap_dump 失败",
                            "stdout": output.stdout,
                            "stderr": output.stderr,
                            "exit_code": output.exit_code,
                        }),
                        raw_stdout: Some(output.stdout),
                    };
                }
                output
            }
        };

        // ② 校验：stat 文件存在且大小 > 0
        let stat_cmd = format!("stat -c %s {remote_path}");
        let stat_output = channel.run(&stat_cmd).await;
        let remote_size: u64 = match stat_output {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().unwrap_or(0),
            _ => 0,
        };
        if remote_size == 0 {
            tracing::error!(session_id = %ctx.session_id, env_id = %env.id, remote_path, "heap dump file missing or empty after dump");
            return error_output(
                "dump_failed",
                &format!("dump 文件不存在或为空: {remote_path}（jcmd exit 0 但无产物）"),
            );
        }

        // ③ 后台拉回：TransferManager（MCP 同步调用秒回，Agent 轮询 transfer_status）
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(format!("heapdump-{pid}-{ts}.hprof"));
        let state = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            &ctx.session_id,
            &env.id,
            &remote_path,
            local_path.clone(),
            true, // 下载成功后清理远端（Friday 自己生成的文件）
        );
        let transfer_id = self.transfer.start(state).await;

        self.emit_progress(&ctx.session_id, "dump 生成完成，后台拉回已启动（轮询 transfer_status 获取进度）");

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            transfer_id = %transfer_id,
            remote_path, remote_size,
            dump_elapsed_ms, "heap dump generated, background download started"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "remote_path": remote_path,
                "remote_size": remote_size,
                "dump_elapsed_ms": dump_elapsed_ms,
                "local_path": local_path.to_string_lossy(),
                "note": "dump 已生成，正在后台拉回。请轮询 transfer_status(transfer_id)；completed 后自动预热分析，用 heap_open(local_path) 起步做根因分析；failed 时远端文件保留，可用 file_download 重试（断点续传）。",
            }),
            raw_stdout: Some(dump_output.stdout),
        }
    }
}

impl HeapDumpHandler {
    fn emit_progress(&self, session_id: &str, detail: &str) {
        self.bus.emit(
            session_id,
            crate::app::events::AppEvent::ProvisionProgress {
                session_id: session_id.to_string(),
                tool: "jvm_heap_dump".to_string(),
                stage: "download".to_string(),
                detail: detail.to_string(),
            },
        );
    }
}

pub fn jvm_heap_dump_tool_def(
    core: Arc<JvmExecCore>,
    bus: crate::app::events::EventBus,
    transfer: Arc<crate::transfer::TransferManager>,
) -> ToolDef {
    ToolDef {
        name: "jvm_heap_dump".to_string(),
        description: "对目标 JVM 生成堆转储并后台拉回本地（jcmd GC.heap_dump）。⚠ 高风险：触发 Full GC（STW），大堆可能停顿数十秒；dump 文件可达 GB 级。生成后自动启动后台下载（返回 transfer_id），请轮询 transfer_status(transfer_id)，completed 后 dump 自动预热，直接用 heap_open(local_path) 等 heap_* 工具自主分析根因（MAT 引擎，本机需 Java 21+）。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "pid": { "type": "string", "description": "目标 Java 进程 PID（list_processes 返回）" },
                "timeout_secs": { "type": "number", "description": "dump 生成超时秒数，默认 300，上限 600" }
            },
            "required": ["environment", "pid"]
        }),
        risk_level: RiskLevel::High,
        category: ToolCategory::Jvm,
        needs_channel: false,
        handler: Arc::new(HeapDumpHandler { core, bus, transfer }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tokio::sync::Mutex as TokioMutex;

    /// 可编程 mock：按命令内容路由（dump/stat）
    struct DumpChannel {
        dump_exit: i32,
        stat_size: &'static str,
        calls: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for DumpChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            if cmd.contains("GC.heap_dump") {
                return Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: self.dump_exit });
            }
            if cmd.starts_with("stat -c %s") {
                return Ok(ExecOutput { stdout: self.stat_size.to_string(), stderr: String::new(), exit_code: 0 });
            }
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        // upload/download 不再被 heap_dump 使用
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>, Arc<crate::transfer::TransferManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let env_id = crate::app::env_save::save_environment(
            &db, None, "prod", "10.0.0.1", 22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        ).await.unwrap().environment.id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache.set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins }).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db: db.clone(), exec_pool, jdk_cache, artifacts_dir: artifacts });
        let mgr = Arc::new(crate::transfer::TransferManager::new(db, crate::app::events::EventBus::disabled()));
        (tmp, core, mgr)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    fn handler(core: Arc<JvmExecCore>, mgr: Arc<crate::transfer::TransferManager>) -> HeapDumpHandler {
        HeapDumpHandler { core, transfer: mgr, bus: crate::app::events::EventBus::disabled() }
    }

    #[tokio::test]
    async fn test_full_flow_starts_background_download() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "12345", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch.clone()).await;
        let out = handler(core, mgr.clone())
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(out.success, "out: {}", out.data);
        // 返回 transfer_id（后台任务已注册）而非同步下载结果
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["local_path"].as_str().unwrap().ends_with(".hprof"));
        assert!(out.data["note"].as_str().unwrap().contains("轮询"));
        // 注册表里能查到
        assert!(mgr.get(tid).await.is_some());
        // 调用序列：dump → stat（无 rm/download——rm 移到 worker 完成后）
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("GC.heap_dump"));
        assert!(calls[1].starts_with("stat -c %s"));
        assert_eq!(calls.len(), 2);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_dump_cmd_failure_passthrough() {
        let ch = Arc::new(DumpChannel { dump_exit: 1, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_stat_empty_fails() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        assert!(out.data["message"].as_str().unwrap().contains("不存在或为空"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr)
            .execute(serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let env_id = crate::app::environments::find_by_name(&core.db, "prod").await.unwrap().unwrap().id;
        core.jdk_cache.clear(&env_id).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled(), mgr);
        assert_eq!(def.name, "jvm_heap_dump");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert_eq!(def.category, ToolCategory::Jvm);
        assert!(!def.needs_channel);
        // schema 不再含 download_timeout_secs
        let schema_str = serde_json::to_string(&def.input_schema).unwrap();
        assert!(!schema_str.contains("download_timeout_secs"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_guides_to_heap_tools() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled(), mgr);
        assert!(def.description.contains("heap_open"), "description should guide to heap_* tools");
        drop(tmp);
    }
}
