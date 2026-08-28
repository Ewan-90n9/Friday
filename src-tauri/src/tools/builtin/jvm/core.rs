use crate::exec::channel::ExecChannel;
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::registry::ToolOutput;
use std::sync::Arc;

/// 环境名 → env 记录 + channel（run_command / ensure_tool 同款语义，提取共享）。
/// Ok(None) = 环境不存在（调用方引导 list_environments）。
pub async fn resolve_environment(
    db: &sqlx::SqlitePool,
    exec_pool: &Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    environment: &str,
) -> Result<Option<(crate::app::environments::EnvironmentRow, Arc<dyn ExecChannel>)>, String> {
    let env = match crate::app::environments::find_by_name(db, environment).await {
        Ok(Some(env)) => env,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("查询环境失败: {e}")),
    };
    let channel = {
        let mut pool = exec_pool.lock().await;
        pool.get_or_create(&env.id, db).await.map_err(|e| e.to_string())?
    };
    Ok(Some((env, channel)))
}

pub fn error_output(error: &str, message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": error, "message": message }),
        raw_stdout: None,
    }
}

/// pid 参数校验：必须正整数字符串（拼 shell 的注入面）
pub fn parse_pid(value: &serde_json::Value) -> Option<u32> {
    let s = value.as_str()?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|&p| p > 0)
}

/// JDK 布局解析：缓存条目 → 所需工具路径列表；缺哪个工具即失败
pub fn require_bins(
    layout: &super::jdk_cache::JdkLayout,
    needed: &[&str],
) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for tool in needed {
        let p = layout.bins.get(*tool).ok_or_else(|| {
            format!("jdk layout missing bin: {tool} (tool_home: {})", layout.tool_home)
        })?;
        paths.push(p.clone());
    }
    Ok(paths)
}

/// timeout_secs 参数收敛：缺失/非正数 → default，超过 max 截断到 max
pub fn clamp_or(v: Option<i64>, default: u64, max: u64) -> u64 {
    match v {
        Some(t) if t > 0 => (t as u64).min(max),
        _ => default,
    }
}

/// jstat/jcmd 缓存失效检测：exit 127 或 stderr 提示文件不存在
pub fn is_jdk_missing(exit_code: i32, stderr: &str) -> bool {
    exit_code == 127 || stderr.contains("No such file or directory")
}

/// JVM 工具共享执行内核：数据库/连接池/JDK 缓存/artifacts 目录
pub struct JvmExecCore {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    pub jdk_cache: Arc<super::jdk_cache::JdkCache>,
    pub artifacts_dir: std::path::PathBuf,
}

impl JvmExecCore {
    /// 命令执行 + 输出组装（不含环境/JDK 解析）。output_ext 用于 artifacts 文件扩展名。
    pub async fn exec_jdk_command(
        &self,
        session_id: &str,
        env_id: &str,
        channel: &Arc<dyn ExecChannel>,
        bin_path: &str,
        command: &str,
        timeout_secs: u64,
        output_ext: &str,
    ) -> ToolOutput {
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            channel.run(command),
        )
        .await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        match result {
            Err(_) => {
                tracing::warn!(session_id, env_id, timeout_secs, "jvm tool timed out, dropping ssh connection to terminate remote process");
                {
                    let mut pool = self.exec_pool.lock().await;
                    pool.disconnect(env_id).await;
                }
                error_output(
                    "timeout_error",
                    &format!("command timed out after {timeout_secs}s; ssh connection was closed to terminate the remote process"),
                )
            }
            Ok(Err(e)) => {
                tracing::error!(session_id, env_id, error = %e, "jvm tool exec failed");
                error_output("connection_error", &e.to_string())
            }
            Ok(Ok(output)) => {
                // 缓存失效：清缓存并引导重新装备
                if is_jdk_missing(output.exit_code, &output.stderr) {
                    tracing::warn!(session_id, env_id, bin_path, "jdk missing on remote, clearing cache");
                    self.jdk_cache.clear(env_id).await;
                    return error_output(
                        "jdk_missing_on_remote",
                        "远端 JDK 已不存在（可能 /tmp 被清理）。请重新调用 ensure_tool 装备后重试。",
                    );
                }

                let (stdout, stdout_truncated) = truncate_output(&output.stdout);
                let (stderr, stderr_truncated) = truncate_output(&output.stderr);
                let truncated = stdout_truncated || stderr_truncated;

                // 完整输出落 artifacts（失败仅告警，沿用 run_command 机制）
                let session_dir = artifact_dir_for(&self.artifacts_dir, session_id);
                let artifact_path = session_dir.join(format!("{}.{}", uuid::Uuid::new_v4(), output_ext));
                let full = format!(
                    "--- command: {command} ---\n--- stdout ---\n{}\n--- stderr ---\n{}\n--- exit_code: {} ---\n",
                    output.stdout, output.stderr, output.exit_code
                );
                let persisted: Option<std::path::PathBuf> = match tokio::fs::create_dir_all(&session_dir).await {
                    Err(e) => { tracing::warn!(session_id, error = %e, "failed to persist full tool output"); None }
                    Ok(_) => match tokio::fs::write(&artifact_path, &full).await {
                        Err(e) => { tracing::warn!(session_id, error = %e, "failed to persist full tool output"); None }
                        Ok(_) => Some(artifact_path),
                    },
                };

                let stdout_field = if stdout_truncated {
                    match &persisted {
                        Some(path) => format!("{stdout}\n[truncated, full output: {}]", path.display()),
                        None => format!("{stdout}\n[truncated]"),
                    }
                } else { stdout };
                let stderr_field = if stderr_truncated {
                    match &persisted {
                        Some(path) => format!("{stderr}\n[truncated, full output: {}]", path.display()),
                        None => format!("{stderr}\n[truncated]"),
                    }
                } else { stderr };

                tracing::info!(session_id, env_id, exit_code = output.exit_code, elapsed_ms, command, "jvm tool executed");

                ToolOutput {
                    success: output.exit_code == 0,
                    data: serde_json::json!({
                        "command": command,
                        "stdout": stdout_field,
                        "stderr": stderr_field,
                        "exit_code": output.exit_code,
                        "elapsed_ms": elapsed_ms,
                        "truncated": truncated,
                    }),
                    raw_stdout: Some(output.stdout),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::{JdkCache, JdkLayout};
    use async_trait::async_trait;
    use std::collections::HashMap;

    struct EchoChannel {
        exit_code: i32,
        stderr: String,
    }

    #[async_trait]
    impl ExecChannel for EchoChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: "ok".into(), stderr: self.stderr.clone(), exit_code: self.exit_code })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    fn core(tmp_dir: &std::path::Path) -> (JvmExecCore, sqlx::SqlitePool, Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>, Arc<JdkCache>) {
        let db = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        let jdk_cache = Arc::new(JdkCache::new());
        let artifacts = tmp_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        (JvmExecCore { db: db.clone(), exec_pool: exec_pool.clone(), jdk_cache: jdk_cache.clone(), artifacts_dir: artifacts }, db, exec_pool, jdk_cache)
    }

    #[test]
    fn test_parse_pid_valid() {
        assert_eq!(parse_pid(&serde_json::json!("12345")), Some(12345));
    }

    #[test]
    fn test_parse_pid_rejects_injection() {
        assert_eq!(parse_pid(&serde_json::json!("123; rm -rf /")), None);
        assert_eq!(parse_pid(&serde_json::json!("")), None);
        assert_eq!(parse_pid(&serde_json::json!("-1")), None);
        assert_eq!(parse_pid(&serde_json::json!("0")), None);
        assert_eq!(parse_pid(&serde_json::json!(12345)), None); // 非 string
    }

    #[test]
    fn test_is_jdk_missing_127_or_no_such_file() {
        assert!(is_jdk_missing(127, ""));
        assert!(is_jdk_missing(0, "sh: /tmp/x/jcmd: No such file or directory"));
        assert!(!is_jdk_missing(1, "Error: Process not found"));
    }

    #[test]
    fn test_require_bins_resolves_and_rejects_missing() {
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/friday-tools/jdk/bin/jcmd".to_string());
        let layout = JdkLayout { tool_home: "/tmp/friday-tools/jdk".into(), bins };
        assert_eq!(
            require_bins(&layout, &["jcmd", "jcmd"]).unwrap(),
            vec!["/tmp/friday-tools/jdk/bin/jcmd".to_string(), "/tmp/friday-tools/jdk/bin/jcmd".to_string()]
        );
        let err = require_bins(&layout, &["jstat"]).unwrap_err();
        assert!(err.contains("missing bin: jstat"), "err: {err}");
        assert!(err.contains("/tmp/friday-tools/jdk"), "err should mention tool_home: {err}");
    }

    #[tokio::test]
    async fn test_exec_success_assembles_output() {
        let tmp = tempfile::tempdir().unwrap();
        let (c, _db, _pool, _cache) = core(tmp.path());
        let ch: Arc<dyn ExecChannel> = Arc::new(EchoChannel { exit_code: 0, stderr: String::new() });
        let out = c.exec_jdk_command("s1", "env-1", &ch, "/jdk/bin/jcmd", "/jdk/bin/jcmd 1 GC.heap_info", 30, "log").await;
        assert!(out.success);
        assert_eq!(out.data["stdout"], "ok");
        assert_eq!(out.data["exit_code"], 0);
        assert_eq!(out.data["command"], "/jdk/bin/jcmd 1 GC.heap_info");
    }

    #[tokio::test]
    async fn test_exec_exit127_clears_cache_and_guides() {
        let tmp = tempfile::tempdir().unwrap();
        let (c, _db, _pool, cache) = core(tmp.path());
        cache.set("env-1", JdkLayout { tool_home: "/tmp/jdk".into(), bins: HashMap::new() }).await;
        let ch: Arc<dyn ExecChannel> = Arc::new(EchoChannel { exit_code: 127, stderr: String::new() });
        let out = c.exec_jdk_command("s1", "env-1", &ch, "/tmp/jdk/bin/jcmd", "/tmp/jdk/bin/jcmd 1 GC.heap_info", 30, "log").await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_missing_on_remote");
        assert!(cache.get("env-1").await.is_none(), "cache must be cleared");
    }

    #[tokio::test]
    async fn test_exec_business_error_passthrough() {
        let tmp = tempfile::tempdir().unwrap();
        let (c, _db, _pool, _cache) = core(tmp.path());
        let ch: Arc<dyn ExecChannel> = Arc::new(EchoChannel { exit_code: 1, stderr: "1:\nCould not attach to process".into() });
        let out = c.exec_jdk_command("s1", "env-1", &ch, "/jdk/bin/jcmd", "/jdk/bin/jcmd 99 Thread.print", 30, "log").await;
        assert!(!out.success);
        assert_eq!(out.data["error"], serde_json::Value::Null); // 无 error code：业务错误透传
        assert_eq!(out.data["exit_code"], 1);
        assert!(out.data["stderr"].as_str().unwrap().contains("Could not attach"));
    }

    struct SlowChannel;

    #[async_trait]
    impl ExecChannel for SlowChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_exec_timeout_drops_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let (c, _db, pool, _cache) = core(tmp.path());
        pool.lock().await.insert_channel("env-1".to_string(), Arc::new(SlowChannel) as Arc<dyn ExecChannel>).await;
        let ch = pool.lock().await.get_or_create_unchecked_for_test("env-1").await;
        let out = c.exec_jdk_command("s1", "env-1", &ch, "/jdk/bin/jcmd", "/jdk/bin/jcmd 1 GC.heap_info", 1, "log").await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "timeout_error");
        assert_eq!(pool.lock().await.connection_count(), 0, "timeout must drop pooled connection");
    }
}
