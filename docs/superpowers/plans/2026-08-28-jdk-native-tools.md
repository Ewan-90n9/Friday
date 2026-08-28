# JDK 原生命令结构化工具实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 spec（`docs/superpowers/specs/2026-08-28-jdk-native-tools-design.md`）定义的 7 个 JVM 语义工具（list_java_processes、jvm_gc_stats、jvm_thread_dump、jvm_heap_info、jvm_vm_info、jvm_class_histogram、jvm_heap_dump），含 JDK 路径按环境缓存与 heap dump SFTP 拉回。

**Architecture:** `tools/builtin/jvm/` 新模块：`core.rs` 共享执行内核（环境解析 → channel → JdkCache 查路径 → 拼命令 → 超时执行 → 输出组装），`simple.rs` 5 个标准工具薄定义，`processes.rs` / `heap_dump.rs` 特例 handler。`ExecChannel` trait 增加 `download` 方法（对齐现有 `upload` 模式）。`ensure_tool` 成功时写入 JdkCache。

**Tech Stack:** Rust (tokio / rmcp / russh-sftp / sqlx / serde_json)，测试用 tempfile + MockChannel 注入 ExecChannelPool（项目既有模式）。

**约定（全程适用）：**
- 所有命令在仓库根目录运行；Rust 测试命令统一为 `cargo test --manifest-path src-tauri/Cargo.toml jvm -- --nocapture`（按任务注明过滤词）。
- 每个 handler 遵从日志规范：入口 `#[instrument]` 或首行 info!，错误路径 warn!/error!，远端 stderr 完整记录。
- 错误输出统一 `fn error_output(error: &str, message: &str) -> ToolOutput`（与 run_command.rs 相同形态）。
- Mock 测试环境搭建复用 run_command.rs 测试的形态：tempdir + `crate::infra::db::init` + `add_environment("prod", …)` + `exec_pool.insert_channel(env_id, mock)`。

---

### Task 1: JdkCache（JDK 路径按环境缓存）

**Files:**
- Create: `src-tauri/src/tools/builtin/jvm/mod.rs`
- Create: `src-tauri/src/tools/builtin/jvm/jdk_cache.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`（加 `pub mod jvm;`）

- [ ] **Step 1: 写失败测试**

`src-tauri/src/tools/builtin/jvm/jdk_cache.rs`：

```rust
use std::collections::HashMap;
use tokio::sync::Mutex;

/// 按环境缓存的 JDK 布局（字段对齐 provision::package::ProvisionResult）
#[derive(Clone, Debug, PartialEq)]
pub struct JdkLayout {
    pub tool_home: String,
    pub bins: HashMap<String, String>,
}

/// 进程内缓存：env_id → JdkLayout。ensure_tool 成功时写入；
/// 执行遇 exit 127 / "No such file or directory" 时清除并引导重新 ensure_tool。
/// 不持久化——Friday 重启后为空，ensure_tool 幂等恢复。
#[derive(Default)]
pub struct JdkCache {
    layouts: Mutex<HashMap<String, JdkLayout>>,
}

impl JdkCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set(&self, env_id: &str, layout: JdkLayout) {
        self.layouts.lock().await.insert(env_id.to_string(), layout);
    }

    pub async fn get(&self, env_id: &str) -> Option<JdkLayout> {
        self.layouts.lock().await.get(env_id).cloned()
    }

    pub async fn clear(&self, env_id: &str) {
        self.layouts.lock().await.remove(env_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> JdkLayout {
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/friday-tools/jdk-21.0.11/bin/jcmd".to_string());
        bins.insert("jstat".to_string(), "/tmp/friday-tools/jdk-21.0.11/bin/jstat".to_string());
        JdkLayout { tool_home: "/tmp/friday-tools/jdk-21.0.11".to_string(), bins }
    }

    #[tokio::test]
    async fn test_set_get_roundtrip() {
        let cache = JdkCache::new();
        cache.set("env-1", layout()).await;
        let got = cache.get("env-1").await.unwrap();
        assert_eq!(got, layout());
        assert_eq!(
            got.bins.get("jcmd").unwrap(),
            "/tmp/friday-tools/jdk-21.0.11/bin/jcmd"
        );
    }

    #[tokio::test]
    async fn test_get_missing_returns_none() {
        let cache = JdkCache::new();
        assert!(cache.get("nope").await.is_none());
    }

    #[tokio::test]
    async fn test_clear_removes_entry() {
        let cache = JdkCache::new();
        cache.set("env-1", layout()).await;
        cache.clear("env-1").await;
        assert!(cache.get("env-1").await.is_none());
    }

    #[tokio::test]
    async fn test_clear_missing_is_noop() {
        let cache = JdkCache::new();
        cache.clear("nope").await; // must not panic
    }
}
```

`src-tauri/src/tools/builtin/jvm/mod.rs`：

```rust
pub mod jdk_cache;
```

`src-tauri/src/tools/builtin/mod.rs` 顶部模块声明区加一行：

```rust
pub mod jvm;
```

- [ ] **Step 2: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jdk_cache`
Expected: 5 passed

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm/ src-tauri/src/tools/builtin/mod.rs
git commit -m "feat: JdkCache for per-environment JDK path caching"
```

---

### Task 2: ExecChannel::download trait 方法（默认未实现）

**Files:**
- Modify: `src-tauri/src/exec/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/exec/channel.rs` 测试模块（`mod tests`）末尾追加：

```rust
    struct RecordingDownloadChannel {
        downloaded: tokio::sync::Mutex<Vec<(String, std::path::PathBuf)>>,
    }

    #[async_trait]
    impl ExecChannel for RecordingDownloadChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn download(&self, remote_path: &str, local: &Path)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.downloaded.lock().await.push((remote_path.to_string(), local.to_path_buf()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_download_trait_method_dispatches() {
        let ch = RecordingDownloadChannel { downloaded: tokio::sync::Mutex::new(Vec::new()) };
        let dyn_ch: &dyn ExecChannel = &ch;
        dyn_ch.download("/tmp/friday-tools/dump.hprof", Path::new("/local/dump.hprof")).await.unwrap();
        let recorded = ch.downloaded.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "/tmp/friday-tools/dump.hprof");
        assert_eq!(recorded[0].1, Path::new("/local/dump.hprof"));
    }

    struct DefaultDownloadChannel;

    #[async_trait]
    impl ExecChannel for DefaultDownloadChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_download_default_returns_not_implemented() {
        let ch = DefaultDownloadChannel;
        let dyn_ch: &dyn ExecChannel = &ch;
        let err = dyn_ch.download("/tmp/x.hprof", Path::new("/tmp/local.hprof")).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"), "err: {err}");
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_download`
Expected: 编译失败——`download` 方法在 trait 中不存在

- [ ] **Step 3: 最小实现**

在 `src-tauri/src/exec/channel.rs` 的 `ExecChannel` trait 中，`upload` 方法之后加：

```rust
    /// 从远端下载文件到本地路径（SFTP 或等价实现）。供 heap dump 回拉等
    /// artifacts 下载复用。默认返回未实现错误——Mock/测试实现按需覆盖。
    async fn download(&self, _remote_path: &str, _local: &std::path::Path)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("download not implemented for this channel".into())
    }
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_download`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/exec/channel.rs
git commit -m "feat: ExecChannel download trait method with not-implemented default"
```

---

### Task 3: SshTransport SFTP download 实现

**Files:**
- Modify: `src-tauri/src/exec/ssh.rs`（ExecChannel impl 的 `upload` 之后）

- [ ] **Step 1: 实现 download（镜像 upload，读远端写本地）**

在 `src-tauri/src/exec/ssh.rs` 的 `impl ExecChannel for SshTransport` 中 `upload` 方法后加：

```rust
    async fn download(&self, remote_path: &str, local: &std::path::Path)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut conn = self.conn.lock().await;
        let Some(c) = conn.as_mut() else {
            return Err("ssh not connected (call connect first)".into());
        };

        let channel = c.handle.channel_open_session().await?;
        // 对齐 upload：慢速链路传 GB 级 dump 时 10s 默认超时不够
        let sftp_cfg = russh_sftp::client::Config {
            request_timeout_secs: 600,
            max_concurrent_writes: 16,
            ..Default::default()
        };
        let sftp = russh_sftp::client::SftpSession::new_with_config(channel.into_stream(), sftp_cfg).await?;

        let mut remote_file = sftp.open(remote_path).await?;
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut local_file = tokio::fs::File::create(local).await?;

        let mut buf = vec![0u8; 32 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = remote_file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            local_file.write_all(&buf[..n]).await?;
            total += n as u64;
        }
        local_file.flush().await?;
        sftp.close().await?;

        tracing::info!(
            env_id = %self.env_id,
            remote_path,
            local = %local.display(),
            bytes = total,
            "sftp download complete"
        );
        Ok(())
    }
```

- [ ] **Step 2: 编译验证（无真机 SSH，无单测）**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过无警告

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/exec/ssh.rs
git commit -m "feat: SshTransport SFTP download via russh-sftp"
```

---

### Task 4: 共享执行内核 JvmExecCore

**Files:**
- Create: `src-tauri/src/tools/builtin/jvm/core.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`

- [ ] **Step 1: 写失败测试**

`src-tauri/src/tools/builtin/jvm/core.rs`：

```rust
use crate::exec::channel::ExecChannel;
use crate::tools::builtin::run_command::{
    artifact_dir_for, clamp_timeout, truncate_output, DEFAULT_TIMEOUT_SECS,
};
use crate::tools::registry::{ToolContext, ToolOutput};
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

/// JDK 布局解析：缓存条目 → (jstat 路径, jcmd 路径)；缺哪个工具即失败
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

/// jstat/jcmd 缓存失效检测：exit 127 或 stderr 提示文件不存在
pub fn is_jdk_missing(exit_code: i32, stderr: &str) -> bool {
    exit_code == 127 || stderr.contains("No such file or directory")
}

/// 执行一条 JDK 命令并组装输出（截断 + 落 artifacts + 注记路径）。
/// 返回 Err(()) 仅用于连接错误/超时（此时已产出 ToolOutput）；
/// 其余情况命令结果以 Ok(ToolOutput) 返回（含业务错误透传）。
pub struct JvmExecCore {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    pub jdk_cache: Arc<super::jdk_cache::JdkCache>,
    pub artifacts_dir: std::path::PathBuf,
}

#[allow(clippy::too_many_arguments)]
impl JvmExecCore {
    /// 命令执行 + 输出组装（不含环境/JDK 解析）。output_kind 用于 artifacts 文件扩展名。
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
    use async_trait::async_trait;

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

    fn core(tmp_dir: &std::path::Path) -> (JvmExecCore, sqlx::SqlitePool, Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>, Arc<super::jdk_cache::JdkCache>) {
        let db = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        let jdk_cache = Arc::new(super::jdk_cache::JdkCache::new());
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
        cache.set("env-1", super::jdk_cache::JdkLayout { tool_home: "/tmp/jdk".into(), bins: HashMap::new() }).await;
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
```

注意：测试 `test_exec_timeout_drops_connection` 需要 pool 暴露一个测试辅助方法。在 `src-tauri/src/exec/pool.rs` 的 `#[cfg(test)]` 块内（`mark_last_used_for_test` 旁）加：

```rust
    #[cfg(test)]
    pub async fn get_or_create_unchecked_for_test(&mut self, environment_id: &str) -> Arc<dyn ExecChannel> {
        self.connections.get(environment_id).map(|c| c.channel.clone()).unwrap()
    }
```

`src-tauri/src/tools/builtin/jvm/mod.rs` 更新为：

```rust
pub mod core;
pub mod jdk_cache;
```

`core.rs` 顶部 use 需补充 `use std::collections::HashMap;`（测试用到）——直接放进测试模块 `use super::*;` 之后的 `use std::collections::HashMap;`。

- [ ] **Step 2: 运行测试验证（先失败后实现循环已合并——本步验证编译+通过）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jvm::core`
Expected: 8 passed

注：`resolve_environment` 的集成路径在 Task 5/6 的 handler 测试中覆盖（含 environment_not_found 引导文案）。

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm/ src-tauri/src/exec/pool.rs
git commit -m "feat: JvmExecCore shared execution kernel with cache-invalidation guidance"
```

---

### Task 5: list_java_processes 工具

**Files:**
- Create: `src-tauri/src/tools/builtin/jvm/processes.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`

- [ ] **Step 1: 写失败测试 + 实现（同文件 TDD）**

`src-tauri/src/tools/builtin/jvm/processes.rs`：

```rust
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
        let timeout_secs = clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

        let Some((env, channel)) = match resolve_environment(&self.core.db, &self.core.exec_pool, environment).await {
            Ok(v) => match v {
                Some(pair) => pair,
                None => {
                    return error_output(
                        "environment_not_found",
                        &format!("环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"),
                    );
                }
            },
            Err(e) => return error_output("connection_error", &e),
        };

        // ps 输出 pid/user/完整命令行；Rust 侧过滤含 "java" 的行
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

    struct PsChannel {
        stdout: &'static str,
    }

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
            jdk_cache: Arc::new(super::super::jdk_cache::JdkCache::new()),
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
```

`src-tauri/src/tools/builtin/jvm/mod.rs` 更新：

```rust
pub mod core;
pub mod jdk_cache;
pub mod processes;
```

- [ ] **Step 2: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jvm::processes`
Expected: 5 passed

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm/
git commit -m "feat: list_java_processes tool"
```

---

### Task 6: 5 个标准 jvm_* 工具（simple.rs）

**Files:**
- Create: `src-tauri/src/tools/builtin/jvm/simple.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`

- [ ] **Step 1: 写失败测试 + 实现**

`src-tauri/src/tools/builtin/jvm/simple.rs`：

```rust
use crate::tools::builtin::jvm::core::{error_output, parse_pid, require_bins, resolve_environment, JvmExecCore};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

/// 每工具的超时配置（默认/上限）
struct Timeouts {
    default_secs: u64,
    max_secs: u64,
}

fn clamp_or(v: Option<i64>, t: &Timeouts) -> u64 {
    match v {
        Some(x) if x > 0 => (x as u64).min(t.max_secs),
        _ => t.default_secs,
    }
}

const GC_STATS: Timeouts = Timeouts { default_secs: 30, max_secs: 300 };
const THREAD_DUMP: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const HEAP_INFO: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const VM_INFO: Timeouts = Timeouts { default_secs: 60, max_secs: 300 };
const CLASS_HISTOGRAM: Timeouts = Timeouts { default_secs: 120, max_secs: 600 };

/// 通用 JVM 命令 handler：bin_key（jstat/jcmd）+ 命令构造器
pub struct JvmSimpleHandler {
    pub core: Arc<JvmExecCore>,
    pub bin_key: &'static str,
    pub timeouts: &'static Timeouts,
    /// 由 (bin_path, args) 构造完整命令
    pub build_command: fn(&str, &serde_json::Value, u32) -> Result<String, String>,
}

fn get_env_and_pid(
    args: &serde_json::Value,
) -> Result<(String, u32), ToolOutput> {
    let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
        return Err(error_output("invalid_params", "missing required parameter: environment"));
    };
    let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
        return Err(error_output("invalid_params", "pid 必须是正整数字符串"));
    };
    Ok((environment.to_string(), pid))
}

#[async_trait]
impl ToolHandler for JvmSimpleHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let (environment, pid) = match get_env_and_pid(&args) {
            Ok(v) => v,
            Err(out) => return out,
        };
        let timeout_secs = clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts);

        let Some((env, channel)) = match resolve_environment(&self.core.db, &self.core.exec_pool, &environment).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!("环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"),
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
        let bins = match require_bins(&layout, &[self.bin_key]) {
            Ok(b) => b,
            Err(e) => return error_output("jdk_not_provisioned", &e),
        };
        let bin_path = &bins[0];

        let command = match (self.build_command)(bin_path, &args, pid) {
            Ok(c) => c,
            Err(e) => return error_output("invalid_params", &e),
        };

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command, "jvm tool executing");
        self.core
            .exec_jdk_command(&ctx.session_id, &env.id, &channel, bin_path, &command, timeout_secs, "log")
            .await
    }
}

// ── 命令构造器 ──

fn build_gc_stats(bin: &str, args: &serde_json::Value, pid: u32) -> Result<String, String> {
    let mut cmd = format!("{bin} -gcutil {pid}");
    if let Some(interval) = args.get("interval_ms").and_then(|v| v.as_i64()) {
        if interval <= 0 {
            return Err("interval_ms 必须是正整数（毫秒）".into());
        }
        let count = args.get("count").and_then(|v| v.as_i64()).unwrap_or(10);
        if count <= 0 {
            return Err("count 必须是正整数".into());
        }
        cmd.push_str(&format!(" {interval} {count}"));
    }
    Ok(cmd)
}

fn build_thread_dump(bin: &str, _args: &serde_json::Value, pid: u32) -> Result<String, String> {
    Ok(format!("{bin} {pid} Thread.print -l"))
}

fn build_heap_info(bin: &str, _args: &serde_json::Value, pid: u32) -> Result<String, String> {
    Ok(format!("{bin} {pid} GC.heap_info"))
}

fn build_vm_info(bin: &str, args: &serde_json::Value, pid: u32) -> Result<String, String> {
    let info_type = args.get("info_type").and_then(|v| v.as_str()).unwrap_or("command_line");
    let sub = match info_type {
        "version" => "VM.version",
        "uptime" => "VM.uptime",
        "command_line" => "VM.command_line",
        "flags" => "VM.flags",
        "system_properties" => "VM.system_properties",
        other => return Err(format!("info_type 非法: {other:?}（可选 version/uptime/command_line/flags/system_properties）")),
    };
    Ok(format!("{bin} {pid} {sub}"))
}

fn build_class_histogram(bin: &str, args: &serde_json::Value, pid: u32) -> Result<String, String> {
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    if all {
        Ok(format!("{bin} {pid} GC.class_histogram -all"))
    } else {
        Ok(format!("{bin} {pid} GC.class_histogram"))
    }
}

// ── ToolDef 工厂 ──

pub fn jvm_gc_stats_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_gc_stats".to_string(),
        description: "采集目标 JVM 的 GC 统计（jstat -gcutil：各代占用百分比、GC 次数/耗时）。诊断 OOM/GC 频繁/内存泄漏的首选。可传 interval_ms + count 连续采样观察趋势（如 interval_ms=1000, count=5）。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: schema(&["environment", "pid", "interval_ms", "count", "timeout_secs"],
            "pid 为 list_java_processes 返回的进程号"),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats }),
    }
}

pub fn jvm_thread_dump_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_thread_dump".to_string(),
        description: "抓取目标 JVM 线程转储（jcmd Thread.print -l，含死锁检测信息）。诊断 CPU 飙高、死锁、线程阻塞。输出较长，可直接读关键段（BLOCKED/死锁/等待）。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: schema(&["environment", "pid", "timeout_secs"], ""),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &THREAD_DUMP, build_command: build_thread_dump }),
    }
}

pub fn jvm_heap_info_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_heap_info".to_string(),
        description: "查看目标 JVM 堆概况（jcmd GC.heap_info：各代容量/已用、GC 策略）。OOM 时确认堆配置与实际占用。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: schema(&["environment", "pid", "timeout_secs"], ""),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &HEAP_INFO, build_command: build_heap_info }),
    }
}

pub fn jvm_vm_info_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_vm_info".to_string(),
        description: "查看目标 JVM 基础信息（jcmd VM.*）：info_type 可选 version/uptime/command_line/flags/system_properties（默认 command_line）。确认 JVM 版本、启动参数、系统属性。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: schema(&["environment", "pid", "info_type", "timeout_secs"], "info_type: version/uptime/command_line/flags/system_properties"),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &VM_INFO, build_command: build_vm_info }),
    }
}

pub fn jvm_class_histogram_tool_def(core: Arc<JvmExecCore>) -> ToolDef {
    ToolDef {
        name: "jvm_class_histogram".to_string(),
        description: "统计目标 JVM 存活对象直方图（jcmd GC.class_histogram，按类聚合实例数/字节）。定位大对象/内存泄漏（哪个类实例最多）。注意：默认 live 视图会触发一次 Full GC；传 all=true 含死对象不强制 GC。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: schema(&["environment", "pid", "all", "timeout_secs"], "all=true 跳过 Full GC（含死对象）"),
        risk_level: RiskLevel::Low,
        needs_channel: false,
        handler: Arc::new(JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &CLASS_HISTOGRAM, build_command: build_class_histogram }),
    }
}

fn schema(extra_props: &[&str], _note: &str) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    props.insert("environment".to_string(), serde_json::json!({
        "type": "string", "description": "目标环境名称（list_environments 返回的 name）"
    }));
    props.insert("pid".to_string(), serde_json::json!({
        "type": "string", "description": "目标 Java 进程 PID（list_java_processes 返回）"
    }));
    if extra_props.contains(&"interval_ms") {
        props.insert("interval_ms".to_string(), serde_json::json!({
            "type": "number", "description": "采样间隔毫秒（与 count 搭配连续采样）"
        }));
    }
    if extra_props.contains(&"count") {
        props.insert("count".to_string(), serde_json::json!({
            "type": "number", "description": "采样次数，默认 10（与 interval_ms 搭配）"
        }));
    }
    if extra_props.contains(&"info_type") {
        props.insert("info_type".to_string(), serde_json::json!({
            "type": "string", "enum": ["version", "uptime", "command_line", "flags", "system_properties"],
            "description": "信息类型，默认 command_line"
        }));
    }
    if extra_props.contains(&"all") {
        props.insert("all".to_string(), serde_json::json!({
            "type": "boolean", "description": "true 时含死对象且不触发 Full GC（-all），默认 false"
        }));
    }
    props.insert("timeout_secs".to_string(), serde_json::json!({
        "type": "number", "description": "超时秒数（见各工具默认/上限）"
    }));
    serde_json::json!({ "type": "object", "properties": props, "required": ["environment", "pid"] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use async_trait::async_trait;
    use std::collections::HashMap;

    struct OkChannel {
        cmd_prefix: String,
    }

    #[async_trait]
    impl ExecChannel for OkChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            assert!(cmd.starts_with(&self.cmd_prefix), "unexpected cmd: {cmd}");
            Ok(ExecOutput { stdout: "S0 S1 E O M YGC FGC".into(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn setup() -> (tempfile::TempDir, Arc<JvmExecCore>, String) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), Arc::new(OkChannel { cmd_prefix: "/tmp/jdk/bin/jstat".into() })).await;
        let mut bins = HashMap::new();
        bins.insert("jstat".to_string(), "/tmp/jdk/bin/jstat".to_string());
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache.set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins }).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db, exec_pool, jdk_cache, artifacts_dir: artifacts });
        (tmp, core, env_id)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    #[tokio::test]
    async fn test_gc_stats_builds_jstat_command() {
        let (tmp, core, _) = setup().await;
        let handler = JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler.execute(
            serde_json::json!({"environment": "prod", "pid": "1234", "interval_ms": 1000, "count": 5}),
            &ctx(),
        ).await;
        assert!(out.success, "out: {}", out.data);
        assert!(out.data["command"].as_str().unwrap().starts_with("/tmp/jdk/bin/jstat -gcutil 1234 1000 5"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let (tmp, core, _) = setup().await;
        let handler = JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler.execute(
            serde_json::json!({"environment": "prod", "pid": "1234; rm -rf /"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned_guides_ensure_tool() {
        let (tmp, core, env_id) = setup().await;
        core.jdk_cache.clear(&env_id).await;
        let handler = JvmSimpleHandler { core, bin_key: "jstat", timeouts: &GC_STATS, build_command: build_gc_stats };
        let out = handler.execute(
            serde_json::json!({"environment": "prod", "pid": "1234"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        assert!(out.data["message"].as_str().unwrap().contains("ensure_tool"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_vm_info_rejects_bad_info_type() {
        let (tmp, core, _) = setup().await;
        let handler = JvmSimpleHandler { core, bin_key: "jcmd", timeouts: &VM_INFO, build_command: build_vm_info };
        let out = handler.execute(
            serde_json::json!({"environment": "prod", "pid": "1234", "info_type": "evil; rm -rf /"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[test]
    fn test_build_vm_info_all_types() {
        for (t, sub) in [
            ("version", "VM.version"),
            ("uptime", "VM.uptime"),
            ("command_line", "VM.command_line"),
            ("flags", "VM.flags"),
            ("system_properties", "VM.system_properties"),
        ] {
            let cmd = build_vm_info("/jdk/bin/jcmd", &serde_json::json!({"info_type": t}), 42).unwrap();
            assert_eq!(cmd, format!("/jdk/bin/jcmd 42 {sub}"));
        }
        // 默认 command_line
        let cmd = build_vm_info("/jdk/bin/jcmd", &serde_json::json!({}), 42).unwrap();
        assert_eq!(cmd, "/jdk/bin/jcmd 42 VM.command_line");
    }

    #[test]
    fn test_build_gc_stats_validation() {
        assert!(build_gc_stats("/b/jstat", &serde_json::json!({"interval_ms": 0}), 1).is_err());
        assert!(build_gc_stats("/b/jstat", &serde_json::json!({"interval_ms": 1000, "count": -1}), 1).is_err());
        assert_eq!(
            build_gc_stats("/b/jstat", &serde_json::json!({}), 1).unwrap(),
            "/b/jstat -gcutil 1"
        );
    }

    #[test]
    fn test_build_class_histogram_all_flag() {
        assert_eq!(
            build_class_histogram("/b/jcmd", &serde_json::json!({}), 1).unwrap(),
            "/b/jcmd 1 GC.class_histogram"
        );
        assert_eq!(
            build_class_histogram("/b/jcmd", &serde_json::json!({"all": true}), 1).unwrap(),
            "/b/jcmd 1 GC.class_histogram -all"
        );
    }

    #[tokio::test]
    async fn test_tool_defs_metadata() {
        let (tmp, core, _) = setup().await;
        assert_eq!(jvm_gc_stats_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_thread_dump_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_heap_info_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_vm_info_tool_def(core.clone()).risk_level, RiskLevel::ReadOnly);
        assert_eq!(jvm_class_histogram_tool_def(core.clone()).risk_level, RiskLevel::Low);
        assert_eq!(jvm_class_histogram_tool_def(core).name, "jvm_class_histogram");
        drop(tmp);
    }
}
```

`src-tauri/src/tools/builtin/jvm/mod.rs` 更新：

```rust
pub mod core;
pub mod jdk_cache;
pub mod processes;
pub mod simple;
```

注意：测试 `OkChannel` 的 `cmd_prefix` 是 jstat；jcmd 系测试（vm_info/thread_dump handler 级）依赖 jcmd 前缀的 channel——`test_vm_info_rejects_bad_info_type` 在构造命令前就失败，不会触达 channel，无影响。若需跑通 jcmd 正常路径，可将 `setup()` 中 channel 换成不校验前缀的简单 OkChannel（去掉 assert）。

- [ ] **Step 2: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jvm::simple`
Expected: 8 passed

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm/
git commit -m "feat: five standard jvm_* tools via JvmExecCore"
```

---

### Task 7: jvm_heap_dump 三阶段工具

**Files:**
- Create: `src-tauri/src/tools/builtin/jvm/heap_dump.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`

- [ ] **Step 1: 写失败测试 + 实现**

`src-tauri/src/tools/builtin/jvm/heap_dump.rs`：

```rust
use crate::exec::channel::ExecChannel;
use crate::tools::builtin::jvm::core::{error_output, parse_pid, require_bins, resolve_environment, JvmExecCore};
use crate::tools::builtin::run_command::artifact_dir_for;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

const DUMP_DEFAULT_TIMEOUT_SECS: u64 = 300;
const DUMP_MAX_TIMEOUT_SECS: u64 = 600;
const DOWNLOAD_DEFAULT_TIMEOUT_SECS: u64 = 1800;
const DOWNLOAD_MAX_TIMEOUT_SECS: u64 = 3600;

fn clamp_or(v: Option<i64>, default: u64, max: u64) -> u64 {
    match v {
        Some(t) if t > 0 => (t as u64).min(max),
        _ => default,
    }
}

pub struct HeapDumpHandler {
    pub core: Arc<JvmExecCore>,
    pub bus: crate::app::events::EventBus,
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
        let dump_timeout = clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), DUMP_DEFAULT_TIMEOUT_SECS, DUMP_MAX_TIMEOUT_SECS);
        let download_timeout = clamp_or(args.get("download_timeout_secs").and_then(|v| v.as_i64()), DOWNLOAD_DEFAULT_TIMEOUT_SECS, DOWNLOAD_MAX_TIMEOUT_SECS);

        let Some((env, channel)) = match resolve_environment(&self.core.db, &self.core.exec_pool, environment).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!("环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"),
                );
            }
            Err(e) => return error_output("connection_error", &e),
        };

        let Some(layout) = self.core.jdk_cache.get(&env.id).await else {
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

        // ① 生成（文件名固定，Friday 构造，不开放自定义——注入面）
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
                return error_output("timeout_error", &format!("heap dump generation timed out after {dump_timeout}s; ssh connection closed"));
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "heap dump exec failed");
                return error_output("connection_error", &e.to_string());
            }
            Ok(Ok(output)) => {
                if crate::tools::builtin::jvm::core::is_jdk_missing(output.exit_code, &output.stderr) {
                    self.core.jdk_cache.clear(&env.id).await;
                    return error_output("jdk_missing_on_remote", "远端 JDK 已不存在（可能 /tmp 被清理）。请重新调用 ensure_tool 装备后重试。");
                }
                if output.exit_code != 0 {
                    // dump 失败：透传 jcmd 输出
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
            return error_output("dump_failed", &format!("dump 文件不存在或为空: {remote_path}（jcmd exit 0 但无产物）"));
        }

        self.emit_progress(&ctx.session_id, &format!("dump 生成完成 ({remote_size} bytes)，开始下载"));

        // ③ 拉回：SFTP → session artifacts
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(format!("heapdump-{pid}-{ts}.hprof"));
        let download_start = std::time::Instant::now();
        let download_result = tokio::time::timeout(
            std::time::Duration::from_secs(download_timeout),
            channel.download(&remote_path, &local_path),
        )
        .await;
        let download_elapsed_ms = download_start.elapsed().as_millis() as u64;

        match download_result {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, remote_path, "dump download timed out; remote file kept");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "download_failed",
                        "message": format!("dump 下载超时（{download_timeout}s）。远端文件保留: {remote_path}，可手动取回。"),
                        "remote_path": remote_path,
                        "remote_size": remote_size,
                        "dump_elapsed_ms": dump_elapsed_ms,
                    }),
                    raw_stdout: None,
                };
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "dump download failed; remote file kept");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({
                        "error": "download_failed",
                        "message": format!("dump 下载失败: {e}。远端文件保留: {remote_path}，可手动取回。"),
                        "remote_path": remote_path,
                        "remote_size": remote_size,
                        "dump_elapsed_ms": dump_elapsed_ms,
                    }),
                    raw_stdout: None,
                };
            }
            Ok(Ok(())) => {}
        }

        // 下载成功 → 清理远端（删 Friday 自己构造路径的文件；失败仅告警不影响结果）
        let cleanup = channel.run(&format!("rm -f {remote_path}")).await;
        if let Err(e) = &cleanup {
            tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "failed to cleanup remote dump file");
        }
        let remote_cleaned = matches!(&cleanup, Ok(o) if o.exit_code == 0);

        self.emit_progress(&ctx.session_id, "dump 下载完成，远端已清理");

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            local_path = %local_path.display(), remote_size,
            dump_elapsed_ms, download_elapsed_ms, remote_cleaned,
            "heap dump complete"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "local_path": local_path.to_string_lossy(),
                "remote_path": remote_path,
                "remote_size": remote_size,
                "dump_elapsed_ms": dump_elapsed_ms,
                "download_elapsed_ms": download_elapsed_ms,
                "remote_cleaned": remote_cleaned,
                "note": "dump 已拉回本地，可交给用户用 MAT 等工具分析。请把 local_path 告知用户。",
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

pub fn jvm_heap_dump_tool_def(core: Arc<JvmExecCore>, bus: crate::app::events::EventBus) -> ToolDef {
    ToolDef {
        name: "jvm_heap_dump".to_string(),
        description: "对目标 JVM 生成堆转储并自动拉回本地（jcmd GC.heap_dump）。⚠ 高风险：触发 Full GC（STW），大堆可能停顿数十秒；dump 文件可达 GB 级。产物保存在本机会话 artifacts 目录（返回 local_path），请告知用户路径以便用 MAT 等工具分析。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "pid": { "type": "string", "description": "目标 Java 进程 PID（list_java_processes 返回）" },
                "timeout_secs": { "type": "number", "description": "dump 生成超时秒数，默认 300，上限 600" },
                "download_timeout_secs": { "type": "number", "description": "dump 下载超时秒数，默认 1800，上限 3600（GB 级传输）" }
            },
            "required": ["environment", "pid"]
        }),
        risk_level: RiskLevel::High,
        needs_channel: false,
        handler: Arc::new(HeapDumpHandler { core, bus }),
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

    /// 可编程 mock：按命令内容路由（dump/stat/rm/download）
    struct DumpChannel {
        dump_exit: i32,
        stat_size: &'static str,
        download_ok: bool,
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
        async fn download(&self, _remote: &str, local: &std::path::Path)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if !self.download_ok {
                return Err("sftp read error".into());
            }
            std::fs::write(local, b"dump-bytes").map_err(|e| e.to_string())?;
            Ok(())
        }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id, channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache.set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins }).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db, exec_pool, jdk_cache, artifacts_dir: artifacts });
        (tmp, core)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    fn handler(core: Arc<JvmExecCore>) -> HeapDumpHandler {
        HeapDumpHandler { core, bus: crate::app::events::EventBus::disabled() }
    }

    #[tokio::test]
    async fn test_full_flow_success() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "12345", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch.clone()).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(out.success, "out: {}", out.data);
        let local = out.data["local_path"].as_str().unwrap();
        assert!(local.ends_with(".hprof"));
        assert!(std::path::Path::new(local).exists(), "local dump must exist");
        assert_eq!(out.data["remote_size"], 12345);
        assert_eq!(out.data["remote_cleaned"], true);
        // 调用序列：dump → stat → rm（download 走 trait 不进 calls）
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("GC.heap_dump"));
        assert!(calls[1].starts_with("stat -c %s"));
        assert!(calls[2].starts_with("rm -f"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_dump_cmd_failure_passthrough() {
        let ch = Arc::new(DumpChannel { dump_exit: 1, stat_size: "0", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_stat_empty_fails() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "0", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        assert!(out.data["message"].as_str().unwrap().contains("不存在或为空"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_failure_keeps_remote_file() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "12345", download_ok: false, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "download_failed");
        assert!(out.data["message"].as_str().unwrap().contains("远端文件保留"));
        assert!(out.data["remote_path"].as_str().unwrap().ends_with(".hprof"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned() {
        // 无 JDK 缓存：用空 pool（无 channel 也能走到 cache 检查前？不行——resolve 先建连。
        // 改为：注入 channel 但清空 cache。
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let env_id = crate::app::environments::find_by_name(&core.db, "prod").await.unwrap().unwrap().id;
        core.jdk_cache.clear(&env_id).await;
        let out = handler(core).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", download_ok: true, calls: TokioMutex::new(Vec::new()) });
        let (tmp, core) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled());
        assert_eq!(def.name, "jvm_heap_dump");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert!(!def.needs_channel);
        drop(tmp);
    }
}
```

`src-tauri/src/tools/builtin/jvm/mod.rs` 更新：

```rust
pub mod core;
pub mod heap_dump;
pub mod jdk_cache;
pub mod processes;
pub mod simple;
```

- [ ] **Step 2: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jvm::heap_dump`
Expected: 7 passed

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm/
git commit -m "feat: jvm_heap_dump three-stage tool with SFTP pull-back"
```

---

### Task 8: ensure_tool 写入 JdkCache + 描述更新

**Files:**
- Modify: `src-tauri/src/tools/builtin/ensure_tool.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`

- [ ] **Step 1: 写失败测试**

在 `ensure_tool.rs` 测试模块追加（需在 `make_handler` / `setup` 存在的前提下改签名——handler 结构体加 `jdk_cache` 字段）：

```rust
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
```

同时更新既有测试 `make_handler`（所有用例共用）：

```rust
    fn make_handler(
        db: sqlx::SqlitePool,
        exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
        cache_dir: std::path::PathBuf,
        bus: crate::app::events::EventBus,
    ) -> EnsureToolHandler {
        EnsureToolHandler {
            db,
            exec_pool,
            cache_dir,
            bus,
            jdk_cache: Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new()),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml ensure_tool`
Expected: 编译失败——`EnsureToolHandler` 无 `jdk_cache` 字段

- [ ] **Step 3: 实现**

`ensure_tool.rs` 修改：

1. struct 加字段：

```rust
pub struct EnsureToolHandler {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub cache_dir: std::path::PathBuf,
    pub bus: crate::app::events::EventBus,
    pub jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
    /// (env_id, package) → 串行化锁
    pub inflight: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}
```

2. `execute` 成功分支（`Ok(result)` 内）在 `tracing::info!` 之后加缓存写入：

```rust
                // 成功即写入 JdkCache：jvm_* 工具按 env_id 取路径
                let layout = crate::tools::builtin::jvm::jdk_cache::JdkLayout {
                    tool_home: result.tool_home.clone(),
                    bins: result.bins.clone(),
                };
                self.jdk_cache.set(&env.id, layout).await;
```

3. 工厂函数签名与构造更新：

```rust
pub fn ensure_tool_tool_def(
    db: sqlx::SqlitePool,
    exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    cache_dir: std::path::PathBuf,
    bus: crate::app::events::EventBus,
    jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
) -> ToolDef {
    // …（description 更新见 Step 4）
    handler: Arc::new(EnsureToolHandler {
        db,
        exec_pool,
        cache_dir,
        bus,
        jdk_cache,
        inflight: Arc::new(Mutex::new(HashMap::new())),
    }),
}
```

4. `test_tool_def_metadata` 更新构造调用（加 `Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new())` 参数）。

- [ ] **Step 4: 更新工具描述**

`ensure_tool_tool_def` 的 description 替换为：

```
"确保目标环境已装备指定诊断工具包（当前支持 jdk）。生产环境通常只有 JRE，缺少 jstat/jcmd 等诊断工具；本工具探测目标 JVM 版本并下载匹配的 JDK 到 /tmp/friday-tools（不影响系统 Java）。装备成功后即可直接调用 jvm_gc_stats / jvm_thread_dump / jvm_heap_info / jvm_vm_info / jvm_class_histogram / jvm_heap_dump 等结构化工具。重复调用安全：已装备时直接返回。JVM 诊断流程：list_environments → list_java_processes 找 pid → ensure_tool → jvm_* 工具。"
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml ensure_tool`
Expected: 全部通过（含既有 7 个用例 + 新增 1 个）

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/tools/builtin/ensure_tool.rs
git commit -m "feat: ensure_tool populates JdkCache and updates description"
```

---

### Task 9: lib.rs 注册 7 个工具

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`（注册入口函数）

- [ ] **Step 1: jvm/mod.rs 加统一注册函数**

`src-tauri/src/tools/builtin/jvm/mod.rs`：

```rust
pub mod core;
pub mod heap_dump;
pub mod jdk_cache;
pub mod processes;
pub mod simple;

use crate::app::events::EventBus;
use std::sync::Arc;

/// 注册全部 JVM 工具到 registry（lib.rs 调用）
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    core: Arc<core::JvmExecCore>,
    bus: EventBus,
) {
    registry.register(processes::list_java_processes_tool_def(core.clone()));
    registry.register(simple::jvm_gc_stats_tool_def(core.clone()));
    registry.register(simple::jvm_thread_dump_tool_def(core.clone()));
    registry.register(simple::jvm_heap_info_tool_def(core.clone()));
    registry.register(simple::jvm_vm_info_tool_def(core.clone()));
    registry.register(simple::jvm_class_histogram_tool_def(core.clone()));
    registry.register(heap_dump::jvm_heap_dump_tool_def(core, bus));
}
```

- [ ] **Step 2: lib.rs 接线**

`src-tauri/src/lib.rs` setup 中，`ensure_tool_tool_def` 调用处（第 93-98 行附近）改造：

```rust
            // JVM 语义工具共享内核与 JDK 路径缓存
            let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
            let jvm_core = Arc::new(crate::tools::builtin::jvm::core::JvmExecCore {
                db: pool.clone(),
                exec_pool: exec_pool.clone(),
                jdk_cache: jdk_cache.clone(),
                artifacts_dir: paths.artifacts_dir(),
            });

            let mut tool_registry = crate::tools::registry::ToolRegistry::new();
            tool_registry.register(crate::tools::builtin::echo_tool_def());
            tool_registry.register(crate::tools::builtin::run_command::run_command_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.artifacts_dir(),
            ));
            tool_registry.register(crate::tools::builtin::list_environments::list_environments_tool_def(
                pool.clone(),
            ));
            tool_registry.register(crate::tools::builtin::ensure_tool::ensure_tool_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.cache_dir(),
                EventBus::new(handle.clone()),
                jdk_cache,
            ));
            crate::tools::builtin::jvm::register_all(&mut tool_registry, jvm_core, EventBus::new(handle.clone()));
            let tool_registry = Arc::new(tool_registry);
```

（`use std::sync::Arc;` 已存在于 lib.rs。）

- [ ] **Step 3: 编译 + 全量测试验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过；全部测试通过（既有 + 新增 jvm 相关约 30 个）

- [ ] **Step 4: 提交**

```bash
git add src-tauri/src/lib.rs src-tauri/src/tools/builtin/jvm/mod.rs
git commit -m "feat: register 7 JVM tools with shared JdkCache wiring"
```

---

### Task 10: 系统提示词更新 + 测试

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs:30-35`（TOOL_GUIDANCE）

- [ ] **Step 1: 更新失败测试**

`prompt.rs` 测试模块中，替换 `test_tool_guidance_mentions_ensure_tool`：

```rust
    #[test]
    fn test_tool_guidance_mentions_ensure_tool() {
        assert!(TOOL_GUIDANCE.contains("ensure_tool"));
        assert!(TOOL_GUIDANCE.contains("list_java_processes"));
        assert!(TOOL_GUIDANCE.contains("jvm_"));
    }
```

- [ ] **Step 2: 运行验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_tool_guidance`
Expected: FAIL（TOOL_GUIDANCE 尚无 list_java_processes）

- [ ] **Step 3: 实现**

`TOOL_GUIDANCE` 常量整体替换为：

```rust
const TOOL_GUIDANCE: &str = "## 工具使用
- 调用诊断工具时，必须传入 session_id 参数。
- 用 environment 参数指定目标环境（name 来自 list_environments）。
- JVM 诊断流程：list_environments → list_java_processes 找 PID → ensure_tool 装备 JDK → 直接调用 jvm_* 结构化工具（jvm_gc_stats / jvm_thread_dump / jvm_heap_info / jvm_vm_info / jvm_class_histogram / jvm_heap_dump）。
- 目标环境通常只有 JRE：跳过 ensure_tool 直接调 jvm_* 会报 jdk_not_provisioned，先装备再重试即可（幂等）。
- run_command 是兜底：非 JVM 领域命令、jstat 其他视图（-gc/-gccapacity）等长尾场景才用它，每次执行需用户确认。
- 用户提到的环境先与 list_environments 的结果匹配；没有匹配时引导用户在右侧「环境」面板添加，不要瞎猜 host。";
```

- [ ] **Step 4: 运行验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml prompt`
Expected: 全部 prompt 测试通过（含既有 test_build_prompt_contains_environment_guidance——其中 `run_command` 与 `list_environments` 断言仍成立）

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: system prompt JVM diagnosis flow guidance"
```

---

### Task 11: 文档联动 + 收尾验证

**Files:**
- Modify: `docs/architecture/overview.md:77-79`（诊断工具层结构化封装行）
- Modify: `docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md:259`（§9 延后项表）

- [ ] **Step 1: overview.md**

将：

```
│ - 结构化封装（jstat/jcmd/arthas/读日志/读dump，           │
│   后续批次）→ 结构化输出                                  │
```

替换为：

```
│ - 结构化封装（首批 JVM 工具已落地：                      │
│   list_java_processes / jvm_gc_stats / jvm_thread_dump   │
│   / jvm_heap_info / jvm_vm_info / jvm_class_histogram    │
│   / jvm_heap_dump；arthas/读日志/读dump 后续批次）        │
```

- [ ] **Step 2: umbrella 设计 §9**

`docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md` §9 表中「结构化 JVM 工具批次」行改为：

```markdown
| 结构化 JVM 工具批次 | ✅ 已落地（见 [JDK 原生命令工具设计](2026-08-28-jdk-native-tools-design.md)）；playbook 步骤中的 run_command 命令模板逐步替换为结构化工具名，模型不用改 |
```

- [ ] **Step 3: 全量验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml && pnpm typecheck`
Expected: 全部通过

- [ ] **Step 4: 提交**

```bash
git add docs/architecture/overview.md docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md
git commit -m "docs: mark JVM structured tools batch as landed"
```

---

## 任务依赖

```
Task 1 (JdkCache) ─┬─► Task 4 (core) ─► Task 5 (processes) ─┐
                    │                  ─► Task 6 (simple)    ├─► Task 9 (注册) ─► Task 10 (prompt) ─► Task 11 (docs)
Task 2 (download) ─┴─► Task 7 (heap_dump) ───────────────────┘
Task 8 (ensure_tool 写缓存) ─────────────────────────────────┘（依赖 Task 1，与 4-7 可并行）
```

严格顺序执行即可（Task 1→11），依赖关系仅供并行调度参考。
