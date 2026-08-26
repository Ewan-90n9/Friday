use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const MAX_TIMEOUT_SECS: u64 = 600;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// timeout 参数钳制：缺省 120，上限 600，非法值（<=0）回退默认
pub fn clamp_timeout(timeout_secs: Option<i64>) -> u64 {
    match timeout_secs {
        Some(t) if t > 0 => (t as u64).min(MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}

/// 输出截断：保头部 64KB，返回 (截断后文本, 是否截断)。切点不破坏 UTF-8 边界。
pub fn truncate_output(s: &str) -> (String, bool) {
    if s.len() <= MAX_OUTPUT_BYTES {
        return (s.to_string(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

pub struct RunCommandHandler {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    pub artifacts_dir: std::path::PathBuf,
}

#[async_trait]
impl ToolHandler for RunCommandHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("missing required parameter: environment");
        };
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return error_output("missing required parameter: command");
        };
        let timeout_secs = clamp_timeout(args.get("timeout_secs").and_then(|v| v.as_i64()));

        // 按名称查环境
        let env = match crate::app::environments::find_by_name(&self.db, environment).await {
            Ok(Some(env)) => env,
            Ok(None) => {
                return error_output(&format!(
                    "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                ));
            }
            Err(e) => return error_output(&format!("查询环境失败: {e}")),
        };

        // 获取或建连
        let channel = {
            let mut pool = self.exec_pool.lock().await;
            match pool.get_or_create(&env.id, &self.db).await {
                Ok(ch) => ch,
                Err(e) => {
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "run_command: failed to get exec channel");
                    return error_output(&format!("connection_error: {e} (host: {})", env.host));
                }
            }
        };

        // 执行（超时包裹）
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            channel.run(command),
        )
        .await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs, "run_command timed out");
                ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "timeout_error",
                        "message": format!("command timed out after {timeout_secs}s and the remote process was terminated"),
                        "elapsed_ms": elapsed_ms,
                    }),
                    raw_stdout: None,
                }
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "run_command failed");
                ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "connection_error",
                        "message": e.to_string(),
                        "host": env.host,
                    }),
                    raw_stdout: None,
                }
            }
            Ok(Ok(output)) => {
                let (stdout, stdout_truncated) = truncate_output(&output.stdout);
                let (stderr, stderr_truncated) = truncate_output(&output.stderr);
                let truncated = stdout_truncated || stderr_truncated;

                // 完整输出落 artifacts（失败仅告警）
                let artifact_path = self
                    .artifacts_dir
                    .join(&ctx.session_id)
                    .join(format!("{}.log", uuid::Uuid::new_v4()));
                let full = format!(
                    "--- stdout ---\n{}\n--- stderr ---\n{}\n--- exit_code: {} ---\n",
                    output.stdout, output.stderr, output.exit_code
                );
                if let Err(e) = std::fs::create_dir_all(artifact_path.parent().unwrap())
                    .and_then(|_| std::fs::write(&artifact_path, &full))
                {
                    tracing::warn!(session_id = %ctx.session_id, error = %e, "failed to persist full tool output");
                }

                let stdout_field = if stdout_truncated {
                    format!("{stdout}\n[truncated, full output: {}]", artifact_path.display())
                } else {
                    stdout
                };
                let stderr_field = if stderr_truncated {
                    format!("{stderr}\n[truncated, full output: {}]", artifact_path.display())
                } else {
                    stderr
                };

                tracing::info!(session_id = %ctx.session_id, env_id = %env.id, exit_code = output.exit_code, elapsed_ms, "run_command executed");

                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "stdout": stdout_field,
                        "stderr": stderr_field,
                        "exit_code": output.exit_code,
                        "elapsed_ms": elapsed_ms,
                        "truncated": truncated,
                    }),
                    raw_stdout: Some(output.stdout.clone()),
                }
            }
        }
    }
}

fn error_output(message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": message }),
        raw_stdout: None,
    }
}

pub fn run_command_tool_def(
    db: sqlx::SqlitePool,
    exec_pool: Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    artifacts_dir: std::path::PathBuf,
) -> ToolDef {
    ToolDef {
        name: "run_command".to_string(),
        description: "在目标远程环境上执行一条 shell 命令（登录 shell，PATH 完整）。这是兜底工具：优先使用结构化诊断工具，只有没有专用工具时才用本工具。每次执行都需要用户确认。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": {
                    "type": "string",
                    "description": "目标环境名称（list_environments 返回的 name）"
                },
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令"
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "超时秒数，默认 120，上限 600"
                }
            },
            "required": ["environment", "command"]
        }),
        risk_level: RiskLevel::High,
        needs_channel: false, // handler 自己按 environment 参数获取 channel
        handler: Arc::new(RunCommandHandler { db, exec_pool, artifacts_dir }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_timeout_default_when_missing() {
        assert_eq!(clamp_timeout(None), 120);
    }

    #[test]
    fn test_clamp_timeout_invalid_falls_back() {
        assert_eq!(clamp_timeout(Some(0)), 120);
        assert_eq!(clamp_timeout(Some(-5)), 120);
    }

    #[test]
    fn test_clamp_timeout_caps_at_max() {
        assert_eq!(clamp_timeout(Some(9999)), 600);
    }

    #[test]
    fn test_clamp_timeout_passes_valid() {
        assert_eq!(clamp_timeout(Some(300)), 300);
    }

    #[test]
    fn test_truncate_output_small_passthrough() {
        let (s, truncated) = truncate_output("hello");
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_output_large_truncated() {
        let big = "x".repeat(MAX_OUTPUT_BYTES + 100);
        let (s, truncated) = truncate_output(&big);
        assert!(truncated);
        assert_eq!(s.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn test_truncate_output_utf8_boundary() {
        let big = "汉".repeat(30000); // 3 bytes/char → 90KB
        let (s, truncated) = truncate_output(&big);
        assert!(truncated);
        assert!(s.chars().all(|c| c == '汉'));
    }

    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct MockChannel {
        stdout: String,
        exit_code: i32,
    }

    #[async_trait]
    impl ExecChannel for MockChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: self.stdout.clone(), stderr: String::new(), exit_code: self.exit_code })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn setup_with_env() -> (tempfile::TempDir, sqlx::SqlitePool, Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None)
            .await.unwrap();
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        (tmp, db, exec_pool, artifacts)
    }

    #[tokio::test]
    async fn test_handler_missing_environment_param() {
        let (tmp, db, exec_pool, artifacts) = setup_with_env().await;
        let handler = RunCommandHandler { db, exec_pool, artifacts_dir: artifacts };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"command": "ls"}), &ctx).await;
        assert!(!out.success);
        assert!(out.data["error"].as_str().unwrap().contains("environment"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_handler_unknown_environment_guides_agent() {
        let (tmp, db, exec_pool, artifacts) = setup_with_env().await;
        let handler = RunCommandHandler { db, exec_pool, artifacts_dir: artifacts };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "nope", "command": "ls", "session_id": "s1"}), &ctx).await;
        assert!(!out.success);
        let msg = out.data["error"].as_str().unwrap();
        assert!(msg.contains("list_environments"), "error should guide agent to list_environments: {msg}");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_handler_executes_via_injected_channel() {
        let (tmp, db, exec_pool, artifacts) = setup_with_env().await;
        // 注入 mock channel（get_or_create 缓存命中路径）
        exec_pool.lock().await.insert_channel(
            /* need env id — query it */
            crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id,
            Arc::new(MockChannel { stdout: "friday-ok".into(), exit_code: 0 }) as Arc<dyn ExecChannel>,
        ).await;
        let handler = RunCommandHandler { db, exec_pool, artifacts_dir: artifacts };
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "command": "echo friday-ok", "session_id": "s1"}), &ctx).await;
        assert!(out.success);
        assert_eq!(out.data["stdout"], "friday-ok");
        assert_eq!(out.data["exit_code"], 0);
        drop(tmp);
    }
}
