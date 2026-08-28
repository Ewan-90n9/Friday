use crate::tools::builtin::jvm::core::{error_output, resolve_environment, JvmExecCore};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;

pub struct ListJavaProcessesHandler {
    pub core: Arc<JvmExecCore>,
}

#[async_trait]
impl ToolHandler for ListJavaProcessesHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let timeout_secs = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            DEFAULT_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        );

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

        // ps 输出 pid/user/完整命令行；管道在远端 shell 执行
        let command = "ps -eo pid=,user=,args= | grep -i java | grep -v grep";
        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, command, "list_java_processes executing");

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            channel.run(command),
        )
        .await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs, "list_java_processes timed out, dropping ssh connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                error_output("timeout_error", &format!("command timed out after {timeout_secs}s"))
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "list_java_processes exec failed");
                error_output("connection_error", &e.to_string())
            }
            Ok(Ok(output)) => {
                // Rust 侧再过滤一次（防御远端 shell 差异）
                let lines: Vec<&str> = output
                    .stdout
                    .lines()
                    .filter(|l| l.to_lowercase().contains("java"))
                    .collect();
                let processes = lines.join("\n");
                tracing::info!(session_id = %ctx.session_id, env_id = %env.id, found = lines.len(), elapsed_ms, "list_java_processes done");
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

fn clamp_or(v: Option<i64>, default: u64, max: u64) -> u64 {
    match v {
        Some(t) if t > 0 => (t as u64).min(max),
        _ => default,
    }
}

pub fn list_java_processes_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "list_java_processes".to_string(),
        description: "列出目标环境上所有 Java 进程（PID、用户、完整命令行）。JVM 诊断第一步：先用本工具找到目标服务的 PID，再配合 jvm_* 工具。不依赖 JDK 装备。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 30，上限 120" }
            },
            "required": ["environment"]
        }),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(ListJavaProcessesHandler { core }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct PsChannel { stdout: &'static str }

    #[async_trait]
    impl ExecChannel for PsChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: self.stdout.to_string(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
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

    const PS_OUTPUT: &str = "  1234 root /opt/jdk/bin/java -Xmx4g -jar app.jar\n  5678 root /usr/bin/python3 script.py\n  9999 app java -XX:+UseG1GC Main\n";

    #[tokio::test]
    async fn test_returns_java_lines_only() {
        let (tmp, core) = setup(Arc::new(PsChannel { stdout: PS_OUTPUT })).await;
        let handler = ListJavaProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod"}), &ctx).await;
        assert!(out.success);
        assert_eq!(out.data["count"], 2);
        let processes = out.data["processes"].as_str().unwrap();
        assert!(processes.contains("1234"));
        assert!(processes.contains("9999"));
        assert!(!processes.contains("python"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_no_java_processes_returns_empty() {
        let (tmp, core) = setup(Arc::new(PsChannel { stdout: "  1 root /sbin/init\n" })).await;
        let handler = ListJavaProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod"}), &ctx).await;
        assert!(out.success);
        assert_eq!(out.data["count"], 0);
        assert_eq!(out.data["processes"], "");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_missing_environment_param() {
        let (tmp, core) = setup(Arc::new(PsChannel { stdout: PS_OUTPUT })).await;
        let handler = ListJavaProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment_guides_agent() {
        let (tmp, core) = setup(Arc::new(PsChannel { stdout: PS_OUTPUT })).await;
        let handler = ListJavaProcessesHandler { core };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "nope"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        assert!(out.data["message"].as_str().unwrap().contains("list_environments"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let (tmp, core) = setup(Arc::new(PsChannel { stdout: "" })).await;
        let def = list_java_processes_tool_def(core);
        assert_eq!(def.name, "list_java_processes");
        assert_eq!(def.risk_level, RiskLevel::ReadOnly);
        assert!(!def.needs_channel);
        drop(tmp);
    }
}
