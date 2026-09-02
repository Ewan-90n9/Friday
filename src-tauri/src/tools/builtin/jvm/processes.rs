use crate::tools::builtin::jvm::core::{clamp_or, error_output, resolve_environment, JvmExecCore};
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;

pub struct ListProcessesHandler {
    pub core: Arc<JvmExecCore>,
}

#[async_trait]
impl ToolHandler for ListProcessesHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let timeout_secs = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            DEFAULT_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        );
        // keyword 可选：空串视为未传（返回全部进程）
        let keyword = args
            .get("keyword")
            .and_then(|v| v.as_str())
            .filter(|kw| !kw.is_empty());

        let (env, channel) = match resolve_environment(&self.core.db, &self.core.exec_pool, environment).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!("环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"),
                );
            }
            Err(e) => return error_output("connection_error", &e),
        };

        // keyword 插值进远端 shell 命令（注入面），必须单引号转义；无 keyword 时纯 ps 返回全部进程
        let command = match keyword {
            Some(kw) => format!(
                "ps -eo pid=,user=,args= | grep -i {} | grep -v grep",
                crate::exec::ssh::shell_quote_single(kw)
            ),
            None => "ps -eo pid=,user=,args=".to_string(),
        };
        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, command = %command, "list_processes executing");

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            channel.run(&command),
        )
        .await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs, "list_processes timed out, dropping ssh connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                error_output("timeout_error", &format!("command timed out after {timeout_secs}s"))
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "list_processes exec failed");
                error_output("connection_error", &e.to_string())
            }
            Ok(Ok(output)) => {
                // Rust 侧再过滤一次（防御远端 shell 差异）；无 keyword 时保留全部行
                let kw_lower = keyword.map(|kw| kw.to_lowercase());
                let lines: Vec<&str> = output
                    .stdout
                    .lines()
                    .filter(|l| match &kw_lower {
                        Some(kw) => l.to_lowercase().contains(kw),
                        None => true,
                    })
                    .collect();
                let processes = lines.join("\n");
                tracing::info!(session_id = %ctx.session_id, env_id = %env.id, found = lines.len(), elapsed_ms, "list_processes done");
                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "command": command,
                        "processes": processes,
                        "count": lines.len(),
                        "note": "每行格式: PID USER 命令行。从命令行中识别目标服务并取 PID。",
                        "exit_code": output.exit_code,
                        "elapsed_ms": elapsed_ms,
                    }),
                    raw_stdout: Some(output.stdout),
                }
            }
        }
    }
}

pub fn list_processes_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "list_processes".to_string(),
        description: "列出目标环境上的进程（PID、用户、完整命令行），按 keyword（服务名/关键字，大小写不敏感）过滤；不传 keyword 返回全部进程。诊断第一步：用用户提到的服务名作 keyword 查 PID，再配合 jvm_* 等工具。不依赖 JDK 装备。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "keyword": { "type": "string", "description": "过滤关键字（服务名等，大小写不敏感；缺省返回全部进程）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 30，上限 120" }
            },
            "required": ["environment"]
        }),
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Environment,
        needs_channel: false,
        handler: Arc::new(ListProcessesHandler { core }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct PsChannel {
        stdout: &'static str,
        calls: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for PsChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            Ok(ExecOutput { stdout: self.stdout.to_string(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>) {
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
        exec_pool.lock().await.insert_channel(env_id, channel).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore {
            db,
            exec_pool,
            jdk_cache: Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new()),
            artifacts_dir: artifacts,
        });
        (tmp, core)
    }

    const PS_OUTPUT: &str = "  1234 root /opt/jdk/bin/java -Xmx4g -jar oomservice.jar\n  5678 root /usr/bin/python3 script.py\n  9999 app nginx: worker process\n";

    #[tokio::test]
    async fn test_keyword_filters_and_quotes_command() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch.clone()).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler
            .execute(serde_json::json!({"environment": "prod", "keyword": "OOMService"}), &ctx)
            .await;
        assert!(out.success);
        // Rust 侧大小写不敏感过滤
        assert_eq!(out.data["count"], 1);
        assert!(out.data["processes"].as_str().unwrap().contains("1234"));
        assert!(!out.data["processes"].as_str().unwrap().contains("python"));
        // 命令构造：keyword 单引号包裹（注入面）
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("grep -i 'OOMService'"), "cmd: {}", calls[0]);
        assert!(calls[0].contains("grep -v grep"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_keyword_injection_quoted() {
        let ch = Arc::new(PsChannel { stdout: "", calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch.clone()).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let _ = handler
            .execute(serde_json::json!({"environment": "prod", "keyword": "x'; rm -rf /; echo '"}), &ctx)
            .await;
        let calls = ch.calls.lock().await;
        // 注入内容被单引号转义，命令仍是单条 grep
        assert!(calls[0].contains(r"grep -i 'x'\''; rm -rf /; echo '\''"), "cmd: {}", calls[0]);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_no_keyword_returns_all_processes() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch.clone()).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod"}), &ctx).await;
        assert!(out.success);
        assert_eq!(out.data["count"], 3);
        // 无 keyword：纯 ps，无 grep 管道
        let calls = ch.calls.lock().await;
        assert_eq!(calls[0], "ps -eo pid=,user=,args=");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_empty_keyword_returns_all_processes() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch.clone()).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "keyword": ""}), &ctx).await;
        assert!(out.success);
        // 空串视为未传：返回全部进程，纯 ps 命令
        assert_eq!(out.data["count"], 3);
        let calls = ch.calls.lock().await;
        assert_eq!(calls[0], "ps -eo pid=,user=,args=");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_no_match_returns_empty() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler
            .execute(serde_json::json!({"environment": "prod", "keyword": "nonexistent-svc"}), &ctx)
            .await;
        assert!(out.success);
        assert_eq!(out.data["count"], 0);
        assert_eq!(out.data["processes"], "");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_missing_environment_param() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment_guides_agent() {
        let ch = Arc::new(PsChannel { stdout: PS_OUTPUT, calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let handler = ListProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "nope"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        assert!(out.data["message"].as_str().unwrap().contains("list_environments"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let ch = Arc::new(PsChannel { stdout: "", calls: tokio::sync::Mutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let def = list_processes_tool_def(core);
        assert_eq!(def.name, "list_processes");
        assert_eq!(def.risk_level, RiskLevel::ReadOnly);
        assert!(!def.needs_channel);
        assert!(def.input_schema["properties"]["keyword"].is_object());
        drop(tmp);
    }
}
