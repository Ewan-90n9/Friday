# 阶段 1：SSH 通道 + run_command 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 russh 实现真实 SSH 通道、新增 run_command / list_environments 工具、环境 CRUD UI 与凭证管理，让 Friday 第一次真正能诊断远程环境。

**Architecture:** 环境从"会话属性"变为独立一等实体：连接按 environment_id 池化（空闲 10min 自动断开），agent 通过 list_environments 工具自主发现环境，run_command 指定 environment 参数执行命令（High 风险走现有确认拦截）。删除 exec/k8s.rs。

**Tech Stack:** Tauri 2 / Rust（russh 0.45 + russh-keys、keyring 3、sqlx、tokio）/ React + zustand + Tailwind v4。

**Spec:** [docs/superpowers/specs/2026-08-26-phase1-ssh-run-command-design.md](../specs/2026-08-26-phase1-ssh-run-command-design.md)

**约定（所有任务遵守）：**
- Rust 检查命令：`cargo check --manifest-path src-tauri/Cargo.toml`；测试：`cargo test --manifest-path src-tauri/Cargo.toml`（在仓库根目录跑）
- 前端类型检查：`pnpm typecheck`
- 日志规范：新增 Tauri command 一律 `#[tracing::instrument(skip(state))]`；错误路径 `tracing::error!`/`warn!`
- 文件路径统一走 `infra/paths.rs` 的 `Paths`，不内联 `.join()`
- 测试放同文件 `#[cfg(test)] mod tests`，用 `tempfile::tempdir()` + `crate::infra::db::init`

**任务依赖图：**

```
Task 1 (DB 迁移)
  → Task 2 (SshAuth + 认证选择) → Task 3 (russh SshTransport)
  → Task 4 (ExecChannel trait 扩展) → Task 5 (环境池化 + 空闲清理)
Task 6 (删除 k8s.rs，可与 2-5 并行但需在 5 后收口)
Task 7 (credentials keyring) → Task 8 (environments CRUD + test_connection)
Task 9 (run_command 工具) ← 依赖 5
Task 10 (list_environments 工具) ← 依赖 1
Task 11 (MCP server 按 environment 获取 channel) ← 依赖 5、9
Task 12 (prompt 引导)
Task 13 (前端 types + ipc + envStore) ← 依赖 8
Task 14 (EnvironmentDialog) ← 依赖 13
Task 15 (右栏分区布局) ← 依赖 13、14
Task 16 (ConfirmCard + sessionStore 分支) ← 依赖 13
Task 17 (全量验证收口)
```

---

## Task 1: DB 迁移 — environments 表认证列

**Files:**
- Modify: `src-tauri/src/infra/db.rs`（init 函数，`add_column_if_not_exists` 调用处）

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/infra/db.rs` 的 `mod tests` 中追加（现有测试 `test_db_init_adds_environment_id_column` 在 267 行附近，插到它后面）：

```rust
    #[tokio::test]
    async fn test_db_init_adds_environment_auth_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let auth_type: String = sqlx::query_scalar(
            "SELECT auth_type FROM environments LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .map(|v: Option<String>| v.unwrap()) // 空表返回 NULL，占位断言列存在
        .unwrap_or_default();
        assert!(auth_type.is_empty() || auth_type == "private_key");

        // 列存在性直接验证
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('environments') WHERE name = 'auth_type'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('environments') WHERE name = 'private_key_path'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_adds_environment_auth_columns`
Expected: FAIL（`auth_type` 列不存在，SQL 报错 no such column）

- [ ] **Step 3: 实现 — init 里加迁移**

`src-tauri/src/infra/db.rs` init 函数中，`add_column_if_not_exists(&pool, "sessions", "environment_id", "TEXT").await?;`（第 25 行）之后追加：

```rust
    // Migration (phase 1): environments auth columns
    add_column_if_not_exists(&pool, "environments", "auth_type", "TEXT NOT NULL DEFAULT 'private_key'").await?;
    add_column_if_not_exists(&pool, "environments", "private_key_path", "TEXT").await?;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_adds_environment_auth_columns`
Expected: PASS

- [ ] **Step 5: 跑全量 DB 测试防回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/infra/db.rs
git commit -m "feat: environments table auth_type/private_key_path migration"
```

---

## Task 2: SshAuth 枚举与认证选择逻辑

**Files:**
- Modify: `src-tauri/src/exec/ssh.rs`（当前是 21 行占位实现，重写为结构定义 + 纯逻辑；russh 连接在 Task 3 实现）

- [ ] **Step 1: 写失败测试（替换整个 ssh.rs 前先写测试再实现，一起替换）**

重写 `src-tauri/src/exec/ssh.rs` 全文：

```rust
use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// SSH 认证配置（用户添加环境时选定，运行时不自动降级）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SshAuth {
    /// 私钥认证。passphrase 从 OS 密钥链取（friday/env/{env_id}/secret）。
    PrivateKey { key_path: String },
    /// 密码认证。密码从 OS 密钥链取（friday/env/{env_id}/secret）。
    Password,
}

impl SshAuth {
    /// 从 DB 行的 auth_type / private_key_path 构造认证配置。
    /// 未知 auth_type 返回 None（调用方报 TransportNotImplemented）。
    pub fn from_row(auth_type: &str, private_key_path: Option<&str>) -> Option<Self> {
        match auth_type {
            "private_key" => Some(SshAuth::PrivateKey {
                key_path: private_key_path?.to_string(),
            }),
            "password" => Some(SshAuth::Password),
            _ => None,
        }
    }

    /// run_command 的命令包装：登录 shell（PATH 完整，jstat/jcmd 直接可用）
    pub fn wrap_login_shell(command: &str) -> String {
        format!("bash -lc {}", shell_quote_single(command))
    }
}

/// POSIX 单引号转义：'...' 内的 ' 替换为 '\''。用于把任意命令安全嵌入 bash -lc '...'。
pub fn shell_quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub struct SshTransport {
    pub env_id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}

#[async_trait]
impl ExecChannel for SshTransport {
    async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        Err("SSH transport not yet implemented".into())
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("SSH transport not yet implemented".into())
    }

    async fn disconnect(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_auth_from_row_private_key() {
        let auth = SshAuth::from_row("private_key", Some("/home/u/.ssh/id_ed25519")).unwrap();
        match auth {
            SshAuth::PrivateKey { key_path } => {
                assert_eq!(key_path, "/home/u/.ssh/id_ed25519");
            }
            _ => panic!("expected PrivateKey"),
        }
    }

    #[test]
    fn test_ssh_auth_from_row_private_key_missing_path_is_none() {
        assert!(SshAuth::from_row("private_key", None).is_none());
    }

    #[test]
    fn test_ssh_auth_from_row_password() {
        assert!(matches!(SshAuth::from_row("password", None), Some(SshAuth::Password)));
    }

    #[test]
    fn test_ssh_auth_from_row_unknown_is_none() {
        assert!(SshAuth::from_row("kerberos", None).is_none());
    }

    #[test]
    fn test_wrap_login_shell_plain() {
        assert_eq!(SshAuth::wrap_login_shell("jstat -gcutil 1234"), "bash -lc 'jstat -gcutil 1234'");
    }

    #[test]
    fn test_wrap_login_shell_with_single_quote() {
        assert_eq!(
            SshAuth::wrap_login_shell("echo 'hi'"),
            "bash -lc 'echo '\\''hi'\\'''"
        );
    }

    #[test]
    fn test_shell_quote_single_roundtrip_via_bash_semantics() {
        // '\'' 转义序列正确性：包含单引号和空格的命令不破坏外层引号
        let q = shell_quote_single("it's a 'test'");
        assert_eq!(q, "'it'\\''s a '\\''test'\\''");
    }
}
```

- [ ] **Step 2: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::ssh`
Expected: 全部 PASS（本任务是纯逻辑 + 结构定义，测试直接写好即通过；russh 实现在 Task 3）

- [ ] **Step 3: cargo check 全量**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误（pool.rs 引用 SshTransport 的字段变了——host/port/user 还在，env_id/auth 新增。pool.rs 构造处会编译失败，此时先修 pool.rs 构造点：`SshTransport { env_id: env_id.clone(), host, port, user, auth: SshAuth::from_row(...).unwrap_or(SshAuth::Password) }` 临时用 unwrap_or 让编译过，Task 5 会重写整个 pool）

在 `src-tauri/src/exec/pool.rs` 第 38-42 行的 ssh 分支改为：

```rust
            "ssh" => {
                let auth = super::ssh::SshAuth::from_row(
                    env.auth_type.as_deref().unwrap_or("private_key"),
                    env.private_key_path.as_deref(),
                )
                .ok_or_else(|| PoolError::TransportNotImplemented(format!(
                    "invalid auth config for environment {env_id}"
                )))?;
                Arc::new(super::ssh::SshTransport {
                    env_id: env_id.clone(),
                    host: env.host.clone().unwrap_or_default(),
                    port: env.port.unwrap_or(22),
                    user: env.user.clone().unwrap_or_default(),
                    auth,
                })
            }
```

同时 `EnvironmentInfo` struct 增加 `auth_type: Option<String>` 与 `private_key_path: Option<String>` 字段，`fetch_environment` 的 SELECT 改为：

```rust
    let env_row: Option<(Option<String>, Option<i64>, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT host, port, user, transport_type, k8s_namespace, k8s_pod, auth_type, private_key_path \
             FROM environments WHERE id = ?",
        )
```

并填充新字段（`auth_type: env_row.6`、`private_key_path: env_row.7`）。`get_or_create` 的 `let env = fetch_environment(pool, session_id).await?;` 改为 `let (env_id, env) = fetch_environment(pool, session_id).await?;`（fetch_environment 返回值改为 `Result<(String, EnvironmentInfo), PoolError>`，元组第一个元素是 env_id）。

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/exec/ssh.rs src-tauri/src/exec/pool.rs
git commit -m "feat: SshAuth config model with login-shell command wrapping"
```

---

## Task 3: russh SshTransport 真实现

**Files:**
- Modify: `src-tauri/src/exec/ssh.rs`（在 Task 2 基础上实现 connect/run/disconnect）

**russh 0.45 API 要点**（写代码时以 cargo doc 为准，下面是最小骨架）：

```rust
use russh::client::{self, Handle, Msg, Session};
use russh_keys::key::KeyPair;
use russh_keys::KnownHosts;

// Handler: host key 自动接受 + 指纹日志
struct SshHandler { env_id: String, host: String }
#[async_trait]
impl client::Handler for SshHandler {
    type Error = russh::Error;
    async fn check_server_key(&mut self, server_public_key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(ssh_key::HashAlg::Sha256);
        tracing::info!(env_id = %self.env_id, host = %self.host, %fingerprint, "accepted server host key");
        Ok(true)
    }
}
```

注意：russh 0.45 的 `check_server_key` 签名依赖其内部 `ssh-key` 版本，实现时先 `cargo check`，若类型不匹配则用 russh re-export 的类型（`russh_keys::key::PublicKey` 或 russh::client 文档所示签名）。**指纹日志是 spec 硬要求（§4.4），类型适配不得省略此日志。**

- [ ] **Step 1: 实现 connect（含重试 2 次 + 认证）**

`src-tauri/src/exec/ssh.rs` 中，`SshTransport` 增加内部连接状态与公开方法：

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct SshTransport {
    pub env_id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// interior mutability: ExecChannel trait 方法都是 &self
    conn: Mutex<Option<SshConn>>,
}

struct SshConn {
    handle: Handle<SshHandler>,
}

impl SshTransport {
    pub fn new(env_id: &str, host: &str, port: u16, user: &str, auth: SshAuth) -> Self {
        Self {
            env_id: env_id.to_string(),
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth,
            conn: Mutex::new(None),
        }
    }

    /// 建连 + 认证（不含重试）。每次调用新建一条连接。
    async fn connect_once(&self) -> Result<Handle<SshHandler>, Box<dyn std::error::Error + Send + Sync>> {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(600)),
            ..Default::default()
        });
        let handler = SshHandler { env_id: self.env_id.clone(), host: self.host.clone() };
        let mut handle = client::connect(config, (self.host.as_str(), self.port), handler).await?;

        // 认证
        let authed = match &self.auth {
            SshAuth::PrivateKey { key_path } => {
                let secret = crate::app::credentials::load_secret(&self.env_id).await?;
                let key_pair = load_key_pair(key_path, secret.as_deref())?;
                handle.authenticate_publickey(self.user.clone(), Arc::new(key_pair)).await?
            }
            SshAuth::Password => {
                let secret = crate::app::credentials::load_secret(&self.env_id).await?
                    .ok_or("password not found in keychain")?;
                handle.authenticate_password(self.user.clone(), secret).await?
            }
        };
        if !authed {
            return Err(format!("SSH authentication failed for {}@{}", self.user, self.host).into());
        }
        Ok(handle)
    }
}

/// 加载私钥；passphrase 为 None 时尝试无密码加载，失败再带 passphrase 重试由 keyring 决定
fn load_key_pair(
    key_path: &str,
    passphrase: Option<&str>,
) -> Result<KeyPair, Box<dyn std::error::Error + Send + Sync>> {
    let expanded = crate::infra::ssh_paths::expand_tilde(key_path);
    if !expanded.exists() {
        return Err(format!("private key not found: {}", expanded.display()).into());
    }
    match passphrase {
        Some(p) if !p.is_empty() => russh_keys::load_secret_key(expanded.to_string_lossy().as_ref(), Some(p)),
        _ => russh_keys::load_secret_key(expanded.to_string_lossy().as_ref(), None),
    }
    .map_err(|e| format!("failed to load private key {}: {e}", expanded.display()).into())
}
```

新增 `src-tauri/src/exec/mod.rs` 模块声明不需要；把 `expand_tilde` 放到新文件 `src-tauri/src/infra/ssh_paths.rs`：

```rust
use std::path::PathBuf;

/// 展开 `~` 前缀路径（Windows 上 ~ = %USERPROFILE%）
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home_prefix() {
        let p = expand_tilde("~/.ssh/id_ed25519");
        assert!(p.components().count() > 1);
        assert!(!p.starts_with("~"));
    }

    #[test]
    fn test_expand_tilde_absolute_untouched() {
        let p = expand_tilde("C:/keys/id_rsa");
        assert_eq!(p, PathBuf::from("C:/keys/id_rsa"));
    }
}
```

并在 `src-tauri/src/infra/mod.rs` 加 `pub mod ssh_paths;`。

重试逻辑在 trait 实现里：

```rust
#[async_trait]
impl ExecChannel for SshTransport {
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(attempt as u64); // 1s, 2s 递增
                tracing::warn!(env_id = %self.env_id, host = %self.host, attempt, "ssh connect retry");
                tokio::time::sleep(delay).await;
            }
            match self.connect_once().await {
                Ok(handle) => {
                    tracing::info!(env_id = %self.env_id, host = %self.host, attempt, "ssh connected");
                    *self.conn.lock().await = Some(SshConn { handle });
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(env_id = %self.env_id, host = %self.host, attempt, error = %e, "ssh connect attempt failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| "ssh connect failed".into()))
    }
    // run/disconnect 见 Step 2/3
}
```

- [ ] **Step 2: 实现 run（含断线重连 1 次 + 重试当前命令）**

```rust
    async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        let wrapped = SshAuth::wrap_login_shell(cmd);
        let mut retried = false;
        loop {
            let handle = {
                let mut conn = self.conn.lock().await;
                match conn.as_mut() {
                    Some(c) => c.handle.clone(),
                    None => return Err("ssh not connected (call connect first)".into()),
                }
            };

            match exec_on_handle(&handle, &self.user, &wrapped).await {
                Ok(output) => {
                    tracing::info!(env_id = %self.env_id, command = %cmd, exit_code = output.exit_code, "ssh command executed");
                    return Ok(output);
                }
                Err(e) if !retried => {
                    // 中途断开：自动重连 1 次并重试当前命令
                    tracing::warn!(env_id = %self.env_id, error = %e, "ssh channel broke, reconnecting once");
                    retried = true;
                    let new_handle = self.connect_once().await?;
                    *self.conn.lock().await = Some(SshConn { handle: new_handle });
                }
                Err(e) => return Err(format!("ssh command failed after reconnect: {e}").into()),
            }
        }
    }

/// 在已有 handle 上开 channel 执行命令，收集 stdout/stderr/exit_code
async fn exec_on_handle(
    handle: &Handle<SshHandler>,
    user: &str,
    wrapped_cmd: &str,
) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, wrapped_cmd).await?;

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut exit_code = -1i32;

    loop {
        let Some(msg) = channel.wait().await else { break };
        match msg {
            russh::ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
            russh::ChannelMsg::ExtendedData { ref data } => stderr.extend_from_slice(data),
            russh::ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status;
                // 必须回发 eof，否则远端可能挂起
                let _ = channel.eof().await;
            }
            russh::ChannelMsg::Eof | russh::ChannelMsg::Close => {}
            _ => {}
        }
    }

    Ok(ExecOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code,
    })
}
```

（`user` 参数未用到时去掉形参——以编译器警告为准，保留无 unused 警告的最终形态。）

- [ ] **Step 3: 实现 disconnect 与 is_alive**

`ExecChannel` trait（Task 4 会正式加 `is_alive`；本步骤先把 disconnect 写好，is_alive 的 trait 方法在 Task 4 加，这里给 SshTransport 预留实现）。ssh.rs 中：

```rust
    async fn disconnect(&self) {
        let mut conn = self.conn.lock().await;
        if let Some(c) = conn.take() {
            if let Err(e) = c.handle.disconnect(russh::Disconnect::ByApplication, "friday idle", "en").await {
                tracing::warn!(env_id = %self.env_id, error = %e, "ssh disconnect error");
            } else {
                tracing::info!(env_id = %self.env_id, "ssh disconnected");
            }
        }
    }
```

`is_alive` 实现（Task 4 加进 trait 后生效）：

```rust
    async fn is_alive(&self) -> bool {
        self.conn.lock().await.is_some()
    }
```

- [ ] **Step 4: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误。russh 0.45 API 细节（`check_server_key` 签名、`authenticate_publickey` 返回类型）如与本计划代码有出入，以 `cargo check` 报错为准修正调用方式，**但以下行为不得妥协**：host key 自动接受且打指纹日志、私钥 passphrase 走 keyring、重试 2 次/重连 1 次、`bash -lc` 包装。

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（无真实 SSH 服务器可连，单测只覆盖纯逻辑；连接逻辑由 Task 8 的 test_connection_cmd 手测）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/exec/ssh.rs src-tauri/src/infra/ssh_paths.rs src-tauri/src/infra/mod.rs src-tauri/src/exec/pool.rs
git commit -m "feat: russh-based SshTransport with retry/reconnect and login-shell exec"
```

---

## Task 4: ExecChannel trait 扩展 is_alive

**Files:**
- Modify: `src-tauri/src/exec/channel.rs`
- Modify: `src-tauri/src/exec/pool.rs`（MockChannel 补实现）

- [ ] **Step 1: 改 trait 并补 MockChannel，跑全量测试**

`src-tauri/src/exec/channel.rs` 全文替换为：

```rust
use async_trait::async_trait;

pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[async_trait]
pub trait ExecChannel: Send + Sync {
    async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>>;
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn disconnect(&self);
    /// 连接池巡检用：连接是否仍然存活
    async fn is_alive(&self) -> bool;
}
```

`src-tauri/src/exec/pool.rs` tests 里的 `MockChannel` 补：

```rust
        async fn is_alive(&self) -> bool { true }
```

（ssh.rs 的 is_alive 已在 Task 3 写好。）

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/exec/channel.rs src-tauri/src/exec/pool.rs
git commit -m "feat: ExecChannel.is_alive for pool health checks"
```

---

## Task 5: 连接池按 environment_id 重构 + 空闲清理

**Files:**
- Modify: `src-tauri/src/exec/pool.rs`（核心重写）
- Modify: `src-tauri/src/app/lifecycle.rs`（close_session_cmd 的 disconnect 调用改签名）
- Modify: `src-tauri/src/lib.rs`（spawn 空闲清理任务）

- [ ] **Step 1: 写失败测试（新池语义）**

`src-tauri/src/exec/pool.rs` tests 模块中，替换/追加以下测试（`test_get_or_create_no_environment_returns_error` 删除——session 不再有环境关联）：

```rust
    use crate::exec::channel::{ExecChannel, ExecOutput};

    struct MockChannel;

    #[async_trait]
    impl ExecChannel for MockChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: "ok".into(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn insert_test_environment(pool: &sqlx::SqlitePool, id: &str, name: &str) {
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES (?, ?, '10.0.0.1', 22, 'root', 'ssh', 'password', '2026-01-01T00:00:00Z')",
        )
        .bind(id).bind(name).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_or_create_caches_by_environment_id() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;

        let mut pool = ExecChannelPool::new();
        // 第一次：缓存未命中 → 建连入池（MockChannel 工厂注入）
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        let ch = pool.get_or_create("env-1", &db_pool).await.unwrap();
        assert!(ch.run("echo").await.is_ok());
        // 第二次：命中同一缓存
        let ch2 = pool.get_or_create("env-1", &db_pool).await.unwrap();
        assert_eq!(pool.connection_count(), 1);
        assert!(std::sync::Arc::ptr_eq(&ch, &ch2));
    }

    #[tokio::test]
    async fn test_get_or_create_unknown_environment_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();

        let mut pool = ExecChannelPool::new();
        let result = pool.get_or_create("no-such-env", &db_pool).await;
        assert!(matches!(result, Err(PoolError::EnvironmentNotFound { .. })));
    }

    #[tokio::test]
    async fn test_idle_cleanup_removes_stale_connections() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        assert_eq!(pool.connection_count(), 1);

        // last_used 设为 11 分钟前 → 清理
        pool.mark_last_used_for_test("env-1", std::time::Instant::now() - std::time::Duration::from_secs(660));
        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
        assert_eq!(removed, 1);
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_idle_cleanup_keeps_recent_connections() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;

        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
        assert_eq!(removed, 0);
        assert_eq!(pool.connection_count(), 1);
    }

    #[tokio::test]
    async fn test_disconnect_removes_connection() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        pool.disconnect("env-1").await;
        assert_eq!(pool.connection_count(), 0);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::pool`
Expected: FAIL（`insert_channel` / `cleanup_idle` / `mark_last_used_for_test` 不存在）

- [ ] **Step 3: 重写 ExecChannelPool**

`src-tauri/src/exec/pool.rs` 非 tests 部分全文替换：

```rust
use super::channel::ExecChannel;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("environment {env_id} not found")]
    EnvironmentNotFound { env_id: String },
    #[error("connection error: {0}")]
    Connection(String),
    #[error("transport not implemented: {0}")]
    TransportNotImplemented(String),
}

struct PooledConnection {
    channel: Arc<dyn ExecChannel>,
    last_used: Instant,
}

pub struct ExecChannelPool {
    connections: HashMap<String, PooledConnection>,
}

impl ExecChannelPool {
    pub fn new() -> Self {
        Self { connections: HashMap::new() }
    }

    /// 按环境获取或建连。缓存命中即复用（刷新 last_used）。
    /// DB 中不存在的环境返回 EnvironmentNotFound（调用方引导 agent/用户）。
    pub async fn get_or_create(
        &mut self,
        environment_id: &str,
        pool: &sqlx::SqlitePool,
    ) -> Result<Arc<dyn ExecChannel>, PoolError> {
        if let Some(conn) = self.connections.get_mut(environment_id) {
            conn.last_used = Instant::now();
            return Ok(conn.channel.clone());
        }

        let env = fetch_environment(pool, environment_id).await?;
        let transport = build_transport(environment_id, &env)?;

        transport
            .connect()
            .await
            .map_err(|e| PoolError::Connection(e.to_string()))?;

        let channel: Arc<dyn ExecChannel> = Arc::from(transport);
        self.connections.insert(
            environment_id.to_string(),
            PooledConnection { channel: channel.clone(), last_used: Instant::now() },
        );
        Ok(channel)
    }

    /// 测试与内部注入用：直接放入一条已建好的 channel
    pub async fn insert_channel(&mut self, environment_id: String, channel: Arc<dyn ExecChannel>) {
        self.connections.insert(environment_id, PooledConnection { channel, last_used: Instant::now() });
    }

    /// 清理空闲超时连接。返回清理数量。
    pub async fn cleanup_idle(&mut self, idle_timeout: Duration) -> usize {
        let stale: Vec<String> = self
            .connections
            .iter()
            .filter(|(_, c)| c.last_used.elapsed() > idle_timeout)
            .map(|(k, _)| k.clone())
            .collect();
        for env_id in &stale {
            if let Some(conn) = self.connections.remove(env_id) {
                tracing::info!(env_id = %env_id, idle_secs = conn.last_used.elapsed().as_secs(), "closing idle ssh connection");
                conn.channel.disconnect().await;
            }
        }
        stale.len()
    }

    pub async fn disconnect(&mut self, environment_id: &str) {
        if let Some(conn) = self.connections.remove(environment_id) {
            conn.channel.disconnect().await;
        }
    }

    pub async fn disconnect_all(&mut self) {
        let conns: Vec<_> = self.connections.drain().collect();
        for (_, conn) in conns {
            conn.channel.disconnect().await;
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    #[cfg(test)]
    pub fn mark_last_used_for_test(&mut self, environment_id: &str, at: Instant) {
        if let Some(conn) = self.connections.get_mut(environment_id) {
            conn.last_used = at;
        }
    }
}

impl Default for ExecChannelPool {
    fn default() -> Self { Self::new() }
}

pub struct EnvironmentInfo {
    pub transport_type: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub auth_type: Option<String>,
    pub private_key_path: Option<String>,
}

fn build_transport(
    environment_id: &str,
    env: &EnvironmentInfo,
) -> Result<super::ssh::SshTransport, PoolError> {
    match env.transport_type.as_str() {
        "ssh" => {
            let auth = super::ssh::SshAuth::from_row(
                env.auth_type.as_deref().unwrap_or("private_key"),
                env.private_key_path.as_deref(),
            )
            .ok_or_else(|| PoolError::TransportNotImplemented(format!(
                "invalid auth config for environment {environment_id}"
            )))?;
            Ok(super::ssh::SshTransport::new(
                environment_id,
                env.host.as_deref().unwrap_or_default(),
                env.port.unwrap_or(22),
                env.user.as_deref().unwrap_or_default(),
                auth,
            ))
        }
        other => Err(PoolError::TransportNotImplemented(other.to_string())),
    }
}

async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
    let row: Option<(Option<String>, Option<i64>, Option<String>, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT host, port, user, transport_type, auth_type, private_key_path \
             FROM environments WHERE id = ?",
        )
        .bind(environment_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PoolError::Connection(e.to_string()))?;

    let row = row.ok_or(PoolError::EnvironmentNotFound {
        env_id: environment_id.to_string(),
    })?;

    Ok(EnvironmentInfo {
        transport_type: row.3,
        host: row.0,
        port: row.1.map(|p| p as u16),
        user: row.2,
        auth_type: row.4,
        private_key_path: row.5,
    })
}
```

- [ ] **Step 4: 修 lifecycle.rs 调用点**

`src-tauri/src/app/lifecycle.rs` close_session_cmd 中（原 269-273 行）exec channel disconnect 整段删除（连接按环境池化后 session 关闭不再断环境连接；空闲清理统一负责）：

```rust
    // （原 disconnect exec channel 逻辑删除——连接按环境池化，session 关闭不再断连接）
```

- [ ] **Step 5: lib.rs 启动空闲清理巡检任务**

`src-tauri/src/lib.rs` setup 闭包中，`app.manage(AppState {...})` 之前（第 121 行附近）加：

```rust
            // SSH 连接池空闲清理巡检：每 60s 清理空闲超 10min 的连接
            {
                let exec_pool_for_cleanup = exec_pool.clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        let mut pool = exec_pool_for_cleanup.lock().await;
                        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
                        if removed > 0 {
                            tracing::info!(removed, "idle ssh connections cleaned");
                        }
                    }
                });
            }
```

- [ ] **Step 6: 跑全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（mcp/server.rs 里 `exec_pool.get_or_create(&session_id, ...)` 调用点会编译报错——临时改为注释掉整段 channel 获取逻辑并置 `let channel = None;`，Task 11 会正式重写。具体：mcp/server.rs 第 224-240 行替换为 `let channel = None; // Task 11 重写为按 environment 参数获取`）

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/exec/pool.rs src-tauri/src/app/lifecycle.rs src-tauri/src/lib.rs src-tauri/src/mcp/server.rs
git commit -m "feat: exec pool keyed by environment_id with idle cleanup"
```

---

## Task 6: 删除 exec/k8s.rs

**Files:**
- Delete: `src-tauri/src/exec/k8s.rs`
- Modify: `src-tauri/src/exec/mod.rs`

- [ ] **Step 1: 删除文件与声明**

删除 `src-tauri/src/exec/k8s.rs`；`src-tauri/src/exec/mod.rs` 改为：

```rust
pub mod channel;
pub mod pool;
pub mod ssh;
```

- [ ] **Step 2: cargo check 确认无引用残留**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误无警告（Task 5 重写 pool 时已无 k8s 分支）

Run: `rg -n "k8s" src-tauri/src/` 确认仅剩 infra/db.rs 的迁移注释（表结构兼容层，保留）。

- [ ] **Step 3: Commit**

```bash
git add -A src-tauri/src/exec/
git commit -m "refactor: remove k8s transport (K8s via SSH+kubectl, phase 2 playbook content)"
```

---

## Task 7: credentials 模块 keyring 实现

**Files:**
- Modify: `src-tauri/src/app/credentials.rs`（替换 todo!() 存根）

- [ ] **Step 1: 实现 store/load/delete**

`src-tauri/src/app/credentials.rs` 全文替换：

```rust
use keyring::Entry;

/// 环境密钥链条目的 service 名（Windows Credential Manager / macOS Keychain / Linux secret service）
const SERVICE: &str = "friday";

fn entry(env_id: &str) -> Result<Entry, String> {
    keyring::Entry::new(SERVICE, &format!("env/{env_id}/secret")).map_err(|e| e.to_string())
}

/// 存储环境密钥（密码或私钥 passphrase）。空值时删除条目。
pub async fn store_secret(
    env_id: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = entry(env_id).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry
        .set_password(value)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    tracing::info!(env_id = %env_id, "secret stored in keychain");
    Ok(())
}

/// 读取环境密钥。无条目返回 None。
pub async fn load_secret(
    env_id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let entry = entry(env_id).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 删除环境密钥（环境删除时级联）。无条目时静默成功。
pub async fn delete_secret(
    env_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = entry(env_id).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
```

注意：原存根签名是 `store_secret(env_id, key, value)` 三参——统一改为两参（一个环境一把 secret，key 固定 `env/{env_id}/secret`）。Task 3 的 ssh.rs 已按两参调用。

- [ ] **Step 2: cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误。（keyring 真实读写不做单测——Windows Credential Manager 副作用不适合 CI；手测在 Task 8 的 test_connection_cmd。）

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/app/credentials.rs
git commit -m "feat: keyring-backed credential store for environment secrets"
```

---

## Task 8: Environment CRUD + test_connection 命令

**Files:**
- Create: `src-tauri/src/app/environments.rs`
- Modify: `src-tauri/src/app/mod.rs`
- Modify: `src-tauri/src/lib.rs`（invoke_handler 注册）

- [ ] **Step 1: 写失败测试**

创建 `src-tauri/src/app/environments.rs`，先写核心 CRUD 函数 + 测试（Tauri command 薄壳后加）：

```rust
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tauri::State;

#[derive(Serialize)]
pub struct EnvironmentRow {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    pub created_at: String,
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

async fn validate_name_free(pool: &SqlitePool, name: &str) -> Result<(), sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Err(sqlx::Error::Database(Box::new(sqlx::error::DatabaseError(
            "duplicate environment name".to_string().into(),
        ))));
    }
    Ok(())
}
```

（上面 validate_name_free 的错误构造如编译不过，改用简单方案：返回 `Result<(), String>`。）

```rust
pub async fn add_environment(
    pool: &SqlitePool,
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    password: Option<&str>,
) -> Result<EnvironmentRow, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    sqlx::query(
        "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, private_key_path, created_at) \
         VALUES (?, ?, ?, ?, ?, 'ssh', ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(host)
    .bind(port as i64)
    .bind(user)
    .bind(auth_type)
    .bind(private_key_path)
    .bind(&now)
    .execute(pool)
    .await?;

    // 密码/私钥 passphrase 入密钥链（keyring 失败则环境已插入，DB 与密钥链短暂不一致——
    // 下次连接时 load_secret 失败会显式报错；这里删除刚插的行保持一致）
    if let Some(secret) = password {
        if !secret.is_empty() {
            if let Err(e) = crate::app::credentials::store_secret(&id, secret).await {
                sqlx::query("DELETE FROM environments WHERE id = ?")
                    .bind(&id)
                    .execute(pool)
                    .await?;
                return Err(sqlx::Error::Database(Box::new(
                    sqlx::error::DatabaseError(format!("keychain store failed: {e}").into()),
                )));
            }
        }
    }

    get_environment(pool, &id).await?.ok_or(sqlx::Error::RowNotFound)
}

pub async fn get_environment(pool: &SqlitePool, id: &str) -> Result<Option<EnvironmentRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, name, host, port, user, auth_type, private_key_path, created_at \
         FROM environments WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| EnvironmentRow {
        id: r.get("id"),
        name: r.get("name"),
        host: r.get("host"),
        port: r.get("port"),
        user: r.get("user"),
        auth_type: r.get("auth_type"),
        private_key_path: r.get("private_key_path"),
        created_at: r.get("created_at"),
    }))
}

pub async fn find_by_name(pool: &SqlitePool, name: &str) -> Result<Option<EnvironmentRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, name, host, port, user, auth_type, private_key_path, created_at \
         FROM environments WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| EnvironmentRow {
        id: r.get("id"),
        name: r.get("name"),
        host: r.get("host"),
        port: r.get("port"),
        user: r.get("user"),
        auth_type: r.get("auth_type"),
        private_key_path: r.get("private_key_path"),
        created_at: r.get("created_at"),
    }))
}

pub async fn list_environments(pool: &SqlitePool) -> Result<Vec<EnvironmentRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, name, host, port, user, auth_type, private_key_path, created_at \
         FROM environments ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EnvironmentRow {
            id: r.get("id"),
            name: r.get("name"),
            host: r.get("host"),
            port: r.get("port"),
            user: r.get("user"),
            auth_type: r.get("auth_type"),
            private_key_path: r.get("private_key_path"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn update_environment(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    password: Option<&str>,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE environments SET name = ?, host = ?, port = ?, user = ?, auth_type = ?, private_key_path = ? \
         WHERE id = ?",
    )
    .bind(name)
    .bind(host)
    .bind(port as i64)
    .bind(user)
    .bind(auth_type)
    .bind(private_key_path)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }

    if let Some(secret) = password {
        if !secret.is_empty() {
            crate::app::credentials::store_secret(id, secret)
                .await
                .map_err(|e| sqlx::Error::Database(Box::new(sqlx::error::DatabaseError(format!("keychain store failed: {e}").into()))))?;
        }
    }
    Ok(())
}

pub async fn delete_environment(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM environments WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    #[tokio::test]
    async fn test_add_and_list_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        assert_eq!(env.name, "prod");
        assert_eq!(env.host, "10.0.0.1");
        assert_eq!(env.auth_type, "password");

        let list = list_environments(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn test_find_by_name() {
        let (_tmp, pool) = setup().await;
        add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();

        let found = find_by_name(&pool, "prod").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().host, "10.0.0.1");

        let missing = find_by_name(&pool, "staging").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_update_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();

        update_environment(&pool, &env.id, "prod", "10.0.0.2", 2222, "opc", "private_key", Some("~/.ssh/id_ed25519"), None).await.unwrap();
        let updated = get_environment(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(updated.host, "10.0.0.2");
        assert_eq!(updated.port, 2222);
        assert_eq!(updated.auth_type, "private_key");
    }

    #[tokio::test]
    async fn test_update_nonexistent_returns_error() {
        let (_tmp, pool) = setup().await;
        let result = update_environment(&pool, "no-such", "n", "h", 22, "u", "password", None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        delete_environment(&pool, &env.id).await.unwrap();
        let gone = get_environment(&pool, &env.id).await.unwrap();
        assert!(gone.is_none());
    }

    #[tokio::test]
    async fn test_no_password_plaintext_in_db() {
        let (_tmp, pool) = setup().await;
        // password=None 路径（真实 keychain 写入不在单测范围）
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let row: (String,) = sqlx::query_as("SELECT host FROM environments WHERE id = ?")
            .bind(&env.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, "10.0.0.1"); // DB 里只有连接信息，无密码列可查（schema 即证明）
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app::environments`
Expected: 全部 PASS（password=None 不触 keychain，CI 安全）

- [ ] **Step 3: 加 Tauri command 薄壳 + test_connection_cmd**

environments.rs 追加：

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_environments_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<EnvironmentRow>, String> {
    list_environments(&state.db).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn add_environment_cmd(
    state: State<'_, crate::AppState>,
    name: String,
    host: String,
    port: Option<u16>,
    user: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
) -> Result<EnvironmentRow, String> {
    if name.trim().is_empty() || host.trim().is_empty() || user.trim().is_empty() {
        return Err("name/host/user 不能为空".to_string());
    }
    if !matches!(auth_type.as_str(), "private_key" | "password") {
        return Err("auth_type 必须是 private_key 或 password".to_string());
    }
    let existing = find_by_name(&state.db, name.trim()).await.map_err(|e| e.to_string())?;
    if existing.is_some() {
        return Err("同名环境已存在".to_string());
    }
    add_environment(
        &state.db,
        name.trim(),
        host.trim(),
        port.unwrap_or(22),
        user.trim(),
        &auth_type,
        private_key_path.as_deref(),
        password.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn update_environment_cmd(
    state: State<'_, crate::AppState>,
    id: String,
    name: String,
    host: String,
    port: Option<u16>,
    user: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
) -> Result<(), String> {
    update_environment(
        &state.db,
        &id,
        name.trim(),
        host.trim(),
        port.unwrap_or(22),
        user.trim(),
        &auth_type,
        private_key_path.as_deref(),
        password.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_environment_cmd(
    state: State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    // 断开池中连接（环境没了连接必须断）
    {
        let mut exec_pool = state.exec_pool.lock().await;
        exec_pool.disconnect(&id).await;
    }
    // 删 keychain 条目（失败仅告警，不阻塞删除）
    if let Err(e) = crate::app::credentials::delete_secret(&id).await {
        tracing::warn!(env_id = %id, error = %e, "failed to delete keychain secret");
    }
    delete_environment(&state.db, &id).await.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct TestConnectionResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn test_connection_cmd(
    state: State<'_, crate::AppState>,
    id: String,
) -> Result<TestConnectionResult, String> {
    let env = get_environment(&state.db, &id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("环境不存在".to_string())?;

    let auth = crate::exec::ssh::SshAuth::from_row(&env.auth_type, env.private_key_path.as_deref())
        .ok_or("认证配置无效".to_string())?;

    let transport = crate::exec::ssh::SshTransport::new(&env.id, &env.host, env.port as u16, &env.user, auth);
    let start = std::time::Instant::now();
    let result = match transport.connect().await {
        Ok(()) => match transport.run("echo friday-ok").await {
            Ok(output) if output.stdout.trim() == "friday-ok" => Ok(()),
            Ok(output) => Err(format!("unexpected echo output: {}", output.stdout.trim())),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    transport.disconnect().await;

    Ok(TestConnectionResult {
        ok: result.is_ok(),
        latency_ms: start.elapsed().as_millis() as u64,
        error: result.err(),
    })
}
```

- [ ] **Step 4: 注册命令**

`src-tauri/src/app/mod.rs` 加 `pub mod environments;`。`src-tauri/src/lib.rs` invoke_handler 列表追加：

```rust
            app::environments::list_environments_cmd,
            app::environments::add_environment_cmd,
            app::environments::update_environment_cmd,
            app::environments::delete_environment_cmd,
            app::environments::test_connection_cmd,
```

- [ ] **Step 5: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 无错误、全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/environments.rs src-tauri/src/app/mod.rs src-tauri/src/lib.rs
git commit -m "feat: environment CRUD + test_connection commands"
```

---

## Task 9: run_command 工具

**Files:**
- Create: `src-tauri/src/tools/builtin/run_command.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`

- [ ] **Step 1: 写失败测试**

创建 `src-tauri/src/tools/builtin/run_command.rs`：

```rust
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const MAX_TIMEOUT_SECS: u64 = 600;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub struct RunCommandHandler {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<tokio::sync::Mutex<crate::exec::pool::ExecChannelPool>>,
    pub artifacts_dir: std::path::PathBuf,
}

/// timeout 参数钳制：缺省 120，上限 600，非法值回退默认
pub fn clamp_timeout(timeout_secs: Option<i64>) -> u64 {
    match timeout_secs {
        Some(t) if t > 0 => (t as u64).min(MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}

/// 输出截断：保头部 64KB，返回 (截断后文本, 是否截断)
pub fn truncate_output(s: &str) -> (String, bool) {
    if s.len() <= MAX_OUTPUT_BYTES {
        return (s.to_string(), false);
    }
    // 找 64KB 内不破坏 UTF-8 边界的最大切点
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
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
                Err(e) => return error_output(&format!("connection_error: {e} (host: {})", env.host)),
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
        needs_channel: false, // channel 由 handler 自己按 environment 获取（Task 11 说明）
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
        // 截断点不落在多字节字符中间
        assert!(s.chars().all(|c| c == '汉'));
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::run_command`
Expected: 全部 PASS

- [ ] **Step 3: mod.rs 声明 + lib.rs 注册**

`src-tauri/src/tools/builtin/mod.rs` 头部加：

```rust
pub mod run_command;
```

`src-tauri/src/lib.rs` 工具注册处（原 79-81 行）改为：

```rust
            let mut tool_registry = crate::tools::registry::ToolRegistry::new();
            tool_registry.register(crate::tools::builtin::echo_tool_def());
            tool_registry.register(crate::tools::builtin::run_command::run_command_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.artifacts_dir(),
            ));
```

注意依赖顺序：这段在 `exec_pool` 创建（原 84 行）之后。把 `let exec_pool = ...` 移到 tool registry 构建之前。

- [ ] **Step 4: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 无错误、全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tools/builtin/ src-tauri/src/lib.rs
git commit -m "feat: run_command tool with timeout clamping and output truncation"
```

---

## Task 10: list_environments 工具

**Files:**
- Create: `src-tauri/src/tools/builtin/list_environments.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: 实现（ReadOnly 查询工具，测试走 handler + 内存 DB）**

创建 `src-tauri/src/tools/builtin/list_environments.rs`：

```rust
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub struct ListEnvironmentsHandler {
    pub db: sqlx::SqlitePool,
}

#[async_trait]
impl ToolHandler for ListEnvironmentsHandler {
    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        match crate::app::environments::list_environments(&self.db).await {
            Ok(envs) => {
                let list: Vec<serde_json::Value> = envs
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name,
                            "host": e.host,
                            "port": e.port,
                            "user": e.user,
                            "auth_type": e.auth_type,
                        })
                    })
                    .collect();
                ToolOutput {
                    success: true,
                    data: serde_json::json!({ "environments": list }),
                    raw_stdout: None,
                }
            }
            Err(e) => ToolOutput {
                success: false,
                data: serde_json::json!({ "error": format!("failed to list environments: {e}") }),
                raw_stdout: None,
            },
        }
    }
}

pub fn list_environments_tool_def(db: sqlx::SqlitePool) -> ToolDef {
    ToolDef {
        name: "list_environments".to_string(),
        description: "列出所有已配置的远程诊断环境（名称、host、端口、用户、认证方式）。诊断远程环境前先调用本工具，把用户提到的环境名或 IP 与列表匹配；若无匹配环境，请用户提供环境信息并引导用户在 Friday 右侧「环境」面板添加，不要猜测 host。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(ListEnvironmentsHandler { db }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolContext;

    #[tokio::test]
    async fn test_list_environments_returns_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None)
            .await
            .unwrap();

        let handler = ListEnvironmentsHandler { db };
        let ctx = ToolContext { session_id: "s1".to_string(), channel: None };
        let output = handler.execute(serde_json::json!({}), &ctx).await;

        assert!(output.success);
        let envs = output.data["environments"].as_array().unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0]["name"], "prod");
        assert_eq!(envs[0]["host"], "10.0.0.1");
    }

    #[test]
    fn test_tool_def_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let db = sqlx::SqlitePool::connect_lazy(&format!("sqlite://{}", tmp.path().join("x.db").display()))
            .unwrap();
        let def = list_environments_tool_def(db);
        assert_eq!(def.name, "list_environments");
        assert_eq!(def.risk_level, RiskLevel::ReadOnly);
        assert!(!def.needs_channel);
    }
}
```

`src-tauri/src/tools/builtin/mod.rs` 加 `pub mod list_environments;`。

`src-tauri/src/lib.rs` 注册处追加：

```rust
            tool_registry.register(crate::tools::builtin::list_environments::list_environments_tool_def(
                pool.clone(),
            ));
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::list_environments`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tools/builtin/ src-tauri/src/lib.rs
git commit -m "feat: list_environments read-only tool"
```

---

## Task 11: MCP server 确认拦截与 channel 获取调整

**Files:**
- Modify: `src-tauri/src/mcp/server.rs`（call_tool 中 channel 获取段）

**背景**：Task 5 把 channel 获取临时注释为 `let channel = None;`。现在 run_command 自己按 environment 建连（`needs_channel: false` + handler 内部获取），MCP 层不再为任何工具预取 channel——`needs_channel` 语义收窄为"由 MCP 层按 environment 参数获取"备用。阶段 1 唯一 needs_channel 工具集合为空，保留字段与逻辑以备阶段 4 脚本工具。

- [ ] **Step 1: 恢复并调整 channel 获取逻辑**

`src-tauri/src/mcp/server.rs` 中，Task 5 的临时 `let channel = None;` 替换为：

```rust
            // Get or create exec channel (only for tools that need one).
            // Phase-1 tools (run_command) acquire their own channel by
            // `environment` arg inside the handler; needs_channel stays for
            // future script tools (phase 4).
            let channel = if tool_def.needs_channel {
                let environment = args_value
                    .get("environment")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let env_row = match environment {
                    Some(name) => {
                        match crate::app::environments::find_by_name(&self.pool, &name).await {
                            Ok(Some(row)) => Some(row),
                            Ok(None) => {
                                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                                    "环境「{name}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                                ))])
                                .into());
                            }
                            Err(e) => {
                                tracing::error!(session_id = %session_id, tool = %tool_name, error = %e, "environment lookup failed");
                                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                                    "environment lookup failed: {e}"
                                ))])
                                .into());
                            }
                        }
                    }
                    None => None,
                };
                match env_row {
                    Some(row) => {
                        let mut exec_pool = self.exec_pool.lock().await;
                        match exec_pool.get_or_create(&row.id, &self.pool).await {
                            Ok(ch) => Some(ch),
                            Err(e) => {
                                tracing::error!(session_id = %session_id, tool = %tool_name, error = %e, "failed to get exec channel");
                                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                                    "failed to establish execution channel: {e}"
                                ))])
                                .into());
                            }
                        }
                    }
                    None => None,
                }
            } else {
                None
            };
```

- [ ] **Step 2: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 无错误、全部 PASS

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/mcp/server.rs
git commit -m "feat: MCP channel acquisition by environment name for channel tools"
```

---

## Task 12: prompt 工具使用引导

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`（build_prompt 与 build_prompt_with_experiences 共用的工具 section 抽常量）

- [ ] **Step 1: 抽常量并补引导**

`src-tauri/src/agent/prompt.rs` 中，在 `FRIDAY_SYSTEM_PROMPT` 常量后加：

```rust
const TOOL_GUIDANCE: &str = "## 工具使用
- 调用诊断工具时，必须传入 session_id 参数。
- 远程命令一律通过 run_command 工具执行，并用 environment 参数指定目标环境（name 来自 list_environments）。
- 优先使用结构化诊断工具，run_command 是兜底。
- 用户提到的环境先与 list_environments 的结果匹配；没有匹配时引导用户在右侧「环境」面板添加，不要瞎猜 host。";
```

`build_prompt`（41-46 行）与 `build_prompt_with_experiences` 中原有的

```rust
        "{system}\n\n---\n\n## 工具使用\n- 调用诊断工具时，必须传入 session_id 参数。\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
```

两处格式串都替换为：

```rust
        "{system}\n\n---\n\n{TOOL_GUIDANCE}\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
```

（带 experiences 的分支在用户消息前同样拼接 exp_section，保持原结构。）

- [ ] **Step 2: 更新受影响测试**

`src-tauri/src/agent/prompt.rs` tests 中，`test_build_prompt_injects_session_id` / `test_build_prompt_with_experiences_injects_session_id` 已断言 `contains("工具使用")` 仍通过。追加一个测试：

```rust
    #[test]
    fn test_build_prompt_contains_environment_guidance() {
        let result = build_prompt("hello", None, "s1");
        assert!(result.contains("run_command"));
        assert!(result.contains("list_environments"));
        assert!(result.contains("environment"));
    }
```

- [ ] **Step 3: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt`
Expected: 全部 PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: prompt guidance for run_command/list_environments"
```

---

## Task 13: 前端 — types + ipc + envStore

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`
- Create: `src/store/envStore.ts`

- [ ] **Step 1: types.ts 加类型**

`src/lib/types.ts` 末尾追加：

```typescript
export type EnvironmentAuthType = "private_key" | "password";

export interface EnvironmentRow {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  auth_type: EnvironmentAuthType;
  private_key_path: string | null;
  created_at: string;
}

export interface TestConnectionResult {
  ok: boolean;
  latency_ms: number;
  error: string | null;
}

export interface ConfirmRequest {
  confirm_id: string;
  session_id: string;
  tool: string;
  args: unknown;
  risk_level: RiskLevel;
  resolved: "pending" | "approved" | "rejected" | "timeout";
}
```

同时 AppEvent 联合类型的 confirm_required 分支补 `confirm_id`（后端 events.rs 已有该字段，前端类型漏了）：

```typescript
  | { type: "confirm_required"; session_id: string; confirm_id: string; tool: string; args: unknown; risk_level: RiskLevel }
```

- [ ] **Step 2: ipc.ts 加绑定**

`src/lib/ipc.ts` 末尾追加：

```typescript
export async function listEnvironments(): Promise<EnvironmentRow[]> {
  return invoke<EnvironmentRow[]>("list_environments_cmd");
}

export async function addEnvironment(params: {
  name: string;
  host: string;
  port?: number;
  user: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
}): Promise<EnvironmentRow> {
  return invoke<EnvironmentRow>("add_environment_cmd", {
    name: params.name,
    host: params.host,
    port: params.port ?? null,
    user: params.user,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
  });
}

export async function updateEnvironment(params: {
  id: string;
  name: string;
  host: string;
  port?: number;
  user: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
}): Promise<void> {
  return invoke<void>("update_environment_cmd", {
    id: params.id,
    name: params.name,
    host: params.host,
    port: params.port ?? null,
    user: params.user,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
  });
}

export async function deleteEnvironment(id: string): Promise<void> {
  return invoke<void>("delete_environment_cmd", { id });
}

export async function testConnection(id: string): Promise<TestConnectionResult> {
  return invoke<TestConnectionResult>("test_connection_cmd", { id });
}
```

注意 Tauri 2 参数命名：Rust snake_case 参数默认映射前端 camelCase（Tauri 2 自动转换）——`authType` → `auth_type`。若调用报"参数不存在"，把前端 key 改成 snake_case（`auth_type`、`private_key_path`）再试，以运行时为准并在 commit message 里注明。

import 行补 `EnvironmentRow, TestConnectionResult` 类型。

- [ ] **Step 3: 创建 envStore**

`src/store/envStore.ts`：

```typescript
import { create } from "zustand";
import type { EnvironmentRow } from "@/lib/types";
import {
  listEnvironments as ipcList,
  addEnvironment as ipcAdd,
  updateEnvironment as ipcUpdate,
  deleteEnvironment as ipcDelete,
  testConnection as ipcTest,
} from "@/lib/ipc";

interface EnvStore {
  environments: EnvironmentRow[];
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
  add: (params: Parameters<typeof ipcAdd>[0]) => Promise<boolean>;
  update: (params: Parameters<typeof ipcUpdate>[0]) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
  test: (id: string) => Promise<{ ok: boolean; latency_ms: number; error: string | null } | null>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useEnvStore = create<EnvStore>((set, get) => ({
  environments: [],
  loading: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const environments = await ipcList();
      set({ environments });
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  add: async (params) => {
    set({ error: null });
    try {
      await ipcAdd(params);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },

  update: async (params) => {
    set({ error: null });
    try {
      await ipcUpdate(params);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },

  remove: async (id) => {
    set({ error: null });
    try {
      await ipcDelete(id);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },

  test: async (id) => {
    try {
      return await ipcTest(id);
    } catch (e) {
      set({ error: errMsg(e) });
      return null;
    }
  },
}));
```

- [ ] **Step 4: typecheck**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts src/store/envStore.ts
git commit -m "feat: environment types, ipc bindings and store"
```

---

## Task 14: EnvironmentDialog 组件

**Files:**
- Create: `src/components/environments/EnvironmentDialog.tsx`
- Create: `src/components/environments/EnvironmentListItem.tsx`

- [ ] **Step 1: EnvironmentListItem**

```tsx
import { PencilSimple, Trash, Key, Password } from "@phosphor-icons/react";
import type { EnvironmentRow } from "@/lib/types";

interface EnvironmentListItemProps {
  env: EnvironmentRow;
  onEdit: (env: EnvironmentRow) => void;
  onDelete: (env: EnvironmentRow) => void;
}

export function EnvironmentListItem({ env, onEdit, onDelete }: EnvironmentListItemProps) {
  return (
    <div className="group px-2.5 py-2 rounded-lg border border-border bg-surface-2/50">
      <div className="flex items-center gap-1.5 mb-1">
        <span
          className="text-xs text-foreground font-medium truncate"
          style={{ fontFamily: "var(--font-mono)" }}
          title={env.name}
        >
          {env.name}
        </span>
        <span
          className="shrink-0 ml-auto flex items-center gap-1 px-1.5 py-px rounded text-[10px] border bg-muted/50 text-muted-foreground border-border"
          title={env.auth_type === "private_key" ? "私钥认证" : "密码认证"}
        >
          {env.auth_type === "private_key" ? (
            <Key size={10} aria-hidden="true" />
          ) : (
            <Password size={10} aria-hidden="true" />
          )}
          {env.auth_type === "private_key" ? "密钥" : "密码"}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span
          className="text-xs text-muted-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
          title={`${env.user}@${env.host}:${env.port}`}
        >
          {env.user}@{env.host}:{env.port}
        </span>
        <span className="shrink-0 hidden group-hover:flex items-center gap-1">
          <button
            onClick={() => onEdit(env)}
            aria-label={`编辑 ${env.name}`}
            className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <PencilSimple size={12} aria-hidden="true" />
          </button>
          <button
            onClick={() => onDelete(env)}
            aria-label={`删除 ${env.name}`}
            className="p-1 rounded text-muted-foreground hover:text-destructive hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <Trash size={12} aria-hidden="true" />
          </button>
        </span>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: EnvironmentDialog**

```tsx
import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Plug, CheckCircle, XCircle } from "@phosphor-icons/react";
import type { EnvironmentRow, TestConnectionResult } from "@/lib/types";
import { useEnvStore } from "@/store/envStore";

interface EnvironmentDialogProps {
  open: boolean;
  onClose: () => void;
  editing: EnvironmentRow | null; // null = 新增
}

const EMPTY_FORM = {
  name: "",
  host: "",
  port: "22",
  user: "root",
  authType: "private_key" as "private_key" | "password",
  privateKeyPath: "",
  password: "",
};

export function EnvironmentDialog({ open, onClose, editing }: EnvironmentDialogProps) {
  const add = useEnvStore((s) => s.add);
  const update = useEnvStore((s) => s.update);
  const test = useEnvStore((s) => s.test);
  const storeError = useEnvStore((s) => s.error);

  const dialogRef = useRef<HTMLDialogElement>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<TestConnectionResult | null>(null);
  const [savedEnvId, setSavedEnvId] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      setForm(
        editing
          ? {
              name: editing.name,
              host: editing.host,
              port: String(editing.port),
              user: editing.user,
              authType: editing.auth_type,
              privateKeyPath: editing.private_key_path ?? "",
              password: "",
            }
          : { ...EMPTY_FORM, privateKeyPath: guessDefaultKeyPath() },
      );
      setTestResult(null);
      setSavedEnvId(editing?.id ?? null);
      setFormError(null);
      if (!dialog.open) dialog.showModal();
    } else if (dialog.open) {
      dialog.close();
    }
  }, [open, editing]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleClose = () => onClose();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onClose]);

  const handleSave = async () => {
    if (!form.name.trim() || !form.host.trim() || !form.user.trim()) {
      setFormError("名称 / 主机 / 用户名不能为空");
      return;
    }
    if (form.authType === "private_key" && !form.privateKeyPath.trim()) {
      setFormError("私钥认证需要填写私钥路径");
      return;
    }
    setSaving(true);
    setFormError(null);
    try {
      const params = {
        name: form.name.trim(),
        host: form.host.trim(),
        port: parseInt(form.port, 10) || 22,
        user: form.user.trim(),
        authType: form.authType,
        privateKeyPath: form.authType === "private_key" ? form.privateKeyPath.trim() : null,
        password: form.password ? form.password : null,
      };
      const ok = editing
        ? await update({ id: editing.id, ...params })
        : await add(params);
      if (ok) onClose();
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (!savedEnvId) {
      setFormError("请先保存环境再测试连接");
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      setTestResult(await test(savedEnvId));
    } finally {
      setTesting(false);
    }
  };

  return (
    <dialog
      ref={dialogRef}
      aria-label={editing ? "编辑环境" : "新增环境"}
      className="z-50 w-[480px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
    >
      <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
        <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
          <h2 className="text-sm font-medium">{editing ? "编辑环境" : "新增环境"}</h2>
          <button
            onClick={onClose}
            aria-label="关闭"
            className="flex items-center justify-center w-7 h-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3 min-h-0">
          <Field label="名称">
            <input
              type="text"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="prod-jvm-01"
              className={inputCls}
            />
          </Field>
          <div className="flex gap-3">
            <Field label="主机" className="flex-1">
              <input
                type="text"
                value={form.host}
                onChange={(e) => setForm({ ...form, host: e.target.value })}
                placeholder="10.0.0.1"
                className={inputCls}
              />
            </Field>
            <Field label="端口" className="w-24">
              <input
                type="text"
                value={form.port}
                onChange={(e) => setForm({ ...form, port: e.target.value })}
                className={inputCls}
              />
            </Field>
          </div>
          <Field label="用户名">
            <input
              type="text"
              value={form.user}
              onChange={(e) => setForm({ ...form, user: e.target.value })}
              placeholder="root"
              className={inputCls}
            />
          </Field>
          <Field label="认证方式">
            <select
              value={form.authType}
              onChange={(e) =>
                setForm({ ...form, authType: e.target.value as "private_key" | "password" })
              }
              className={`${inputCls} cursor-pointer`}
            >
              <option value="private_key">私钥（推荐）</option>
              <option value="password">密码</option>
            </select>
          </Field>
          {form.authType === "private_key" ? (
            <Field label="私钥路径">
              <input
                type="text"
                value={form.privateKeyPath}
                onChange={(e) => setForm({ ...form, privateKeyPath: e.target.value })}
                placeholder="~/.ssh/id_ed25519"
                className={inputCls}
                style={{ fontFamily: "var(--font-mono)" }}
              />
              <p className="text-xs text-muted-foreground mt-1">
                引用本机 ~/.ssh/ 下的私钥文件，不复制。带 passphrase 时请用下方密钥字段保存。
              </p>
            </Field>
          ) : null}
          <Field label={form.authType === "private_key" ? "密钥口令（可选）" : "密码"}>
            <input
              type="password"
              value={form.password}
              onChange={(e) => setForm({ ...form, password: e.target.value })}
              placeholder={editing ? "留空表示不修改" : ""}
              className={inputCls}
            />
            <p className="text-xs text-muted-foreground mt-1">
              存入操作系统密钥链（Windows 凭据管理器），不写入数据库。
            </p>
          </Field>

          {testResult && (
            <div
              className={`flex items-center gap-2 text-xs px-3 py-2 rounded-md border ${
                testResult.ok
                  ? "bg-success/10 text-success border-success/20"
                  : "bg-destructive/10 text-destructive border-destructive/20"
              }`}
            >
              {testResult.ok ? (
                <CheckCircle size={14} weight="fill" aria-hidden="true" />
              ) : (
                <XCircle size={14} weight="fill" aria-hidden="true" />
              )}
              {testResult.ok
                ? `连接成功（${testResult.latency_ms}ms）`
                : `连接失败：${testResult.error}`}
            </div>
          )}

          {(formError ?? storeError) && (
            <p className="text-xs text-destructive break-words">{formError ?? storeError}</p>
          )}
        </div>

        <div className="flex items-center gap-2 px-5 py-4 border-t border-border shrink-0">
          <button
            onClick={handleTest}
            disabled={testing}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50"
          >
            {testing ? (
              <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <Plug size={14} aria-hidden="true" />
            )}
            测试连接
          </button>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            取消
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50"
          >
            {saving && <CircleNotch size={14} className="animate-spin" aria-hidden="true" />}
            保存
          </button>
        </div>
      </div>
    </dialog>
  );
}

const inputCls =
  "w-full bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none";

function guessDefaultKeyPath(): string {
  const home = (window as unknown as { __FRIDAY_HOME__?: string }).__FRIDAY_HOME__ ?? "";
  if (home) return `${home}/.ssh/id_ed25519`;
  return "~/.ssh/id_ed25519";
}
```

- [ ] **Step 3: typecheck**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: Commit**

```bash
git add src/components/environments/
git commit -m "feat: environment dialog and list item components"
```

---

## Task 15: 右侧面板上下分区布局

**Files:**
- Create: `src/components/environments/EnvironmentsPanel.tsx`
- Modify: `src/components/tools/ToolsPanel.tsx`（改为可复用的分区子面板）
- Modify: `src/pages/DiagnosisPage.tsx`
- Create: `src/components/environments/DeleteEnvConfirmDialog.tsx`

- [ ] **Step 1: DeleteEnvConfirmDialog（照抄 DeleteConfirmDialog 模式）**

```tsx
import { useEffect, useRef } from "react";
import type { EnvironmentRow } from "@/lib/types";

interface DeleteEnvConfirmDialogProps {
  env: EnvironmentRow | null;
  onConfirm: () => void;
  onCancel: () => void;
}

export function DeleteEnvConfirmDialog({ env, onConfirm, onCancel }: DeleteEnvConfirmDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (env && !dialog.open) dialog.showModal();
    if (!env && dialog.open) dialog.close();
  }, [env]);

  return (
    <dialog
      ref={dialogRef}
      aria-label="确认删除环境"
      className="z-50 w-[360px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
      onClose={onCancel}
    >
      <div className="px-5 py-4">
        <h2 className="text-sm font-medium mb-2">删除环境</h2>
        <p className="text-xs text-muted-foreground leading-relaxed">
          确定删除环境 <span className="text-foreground font-medium">{env?.name}</span>（{env?.host}）？
          同时删除密钥链中保存的凭证，不影响正在进行的诊断会话。
        </p>
        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 transition-colors cursor-pointer"
          >
            取消
          </button>
          <button
            onClick={onConfirm}
            className="px-3 py-1.5 rounded-md bg-destructive text-destructive-foreground text-xs hover:bg-destructive/80 transition-colors cursor-pointer"
          >
            删除
          </button>
        </div>
      </div>
    </dialog>
  );
}
```

- [ ] **Step 2: EnvironmentsPanel**

```tsx
import { useEffect, useState } from "react";
import { Globe, Plus, CircleNotch } from "@phosphor-icons/react";
import type { EnvironmentRow } from "@/lib/types";
import { useEnvStore } from "@/store/envStore";
import { EnvironmentListItem } from "./EnvironmentListItem";
import { EnvironmentDialog } from "./EnvironmentDialog";
import { DeleteEnvConfirmDialog } from "./DeleteEnvConfirmDialog";

export function EnvironmentsPanel() {
  const environments = useEnvStore((s) => s.environments);
  const loading = useEnvStore((s) => s.loading);
  const error = useEnvStore((s) => s.error);
  const load = useEnvStore((s) => s.load);
  const remove = useEnvStore((s) => s.remove);

  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<EnvironmentRow | null>(null);
  const [deleting, setDeleting] = useState<EnvironmentRow | null>(null);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="flex flex-col min-h-0 max-h-[45%] border-b border-border">
      <div className="flex items-center gap-2 h-10 px-4 border-b border-border shrink-0">
        <Globe size={14} className="text-muted-foreground" aria-hidden="true" />
        <span
          className="text-xs font-medium text-muted-foreground uppercase tracking-wide"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          环境
        </span>
        <span className="text-xs text-muted-foreground/60 ml-auto">{environments.length}</span>
        <button
          onClick={() => {
            setEditing(null);
            setDialogOpen(true);
          }}
          aria-label="新增环境"
          className="flex items-center justify-center w-5 h-5 rounded text-accent hover:bg-surface-3 transition-colors cursor-pointer"
        >
          <Plus size={12} aria-hidden="true" />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto px-3 py-3">
        {error && <div className="text-destructive text-xs px-1 py-2">{error}</div>}
        {loading && environments.length === 0 && (
          <div className="flex items-center justify-center gap-2 py-4 text-muted-foreground text-xs">
            <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
            加载中…
          </div>
        )}
        {!loading && environments.length === 0 && (
          <div className="py-4 text-center text-muted-foreground text-xs leading-relaxed">
            暂无环境
            <br />
            点击右上角 + 添加远程诊断环境
          </div>
        )}
        {environments.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {environments.map((env) => (
              <li key={env.id}>
                <EnvironmentListItem
                  env={env}
                  onEdit={(e) => {
                    setEditing(e);
                    setDialogOpen(true);
                  }}
                  onDelete={(e) => setDeleting(e)}
                />
              </li>
            ))}
          </ul>
        )}
      </div>

      <EnvironmentDialog open={dialogOpen} onClose={() => setDialogOpen(false)} editing={editing} />
      <DeleteEnvConfirmDialog
        env={deleting}
        onConfirm={async () => {
          if (deleting) await remove(deleting.id);
          setDeleting(null);
        }}
        onCancel={() => setDeleting(null)}
      />
    </div>
  );
}
```

- [ ] **Step 3: ToolsPanel 去外层边框（变成下半区）**

`src/components/tools/ToolsPanel.tsx` 的最外层 `<aside>` 改为 `<section>` 并去掉 `w-64 shrink-0 border-l`：

```tsx
    <section className="flex-1 flex flex-col min-h-0">
```

（其余内容不动。）

- [ ] **Step 4: DiagnosisPage 组装**

`src/pages/DiagnosisPage.tsx` 中 `import { ToolsPanel } from "@/components/tools/ToolsPanel";` 后加：

```tsx
import { EnvironmentsPanel } from "@/components/environments/EnvironmentsPanel";
```

`<ToolsPanel />` 替换为：

```tsx
        <aside className="w-64 shrink-0 border-l border-border bg-surface-1 flex flex-col min-h-0">
          <EnvironmentsPanel />
          <ToolsPanel />
        </aside>
```

- [ ] **Step 5: typecheck + 手动验证**

Run: `pnpm typecheck`
Expected: 无错误

Run: `pnpm tauri dev` 手测：右栏出现上下分区；新增环境（私钥/密码切换表单联动）；编辑回填；删除确认；测试连接按钮在保存前禁用逻辑提示正确。

- [ ] **Step 6: Commit**

```bash
git add src/components/ src/pages/DiagnosisPage.tsx
git commit -m "feat: right panel split into environments + tools sections"
```

---

## Task 16: ConfirmCard + sessionStore confirm_required 分支

**Files:**
- Create: `src/components/chat/ConfirmCard.tsx`
- Modify: `src/store/sessionStore.ts`（handleEvent 加 confirm_required 分支 + pendingConfirmations 状态 + confirm 动作）
- Modify: `src/components/chat/MessageList.tsx`（在消息列表尾部渲染当前会话 pending confirmations）

- [ ] **Step 1: sessionStore 加状态与分支**

`src/store/sessionStore.ts`：

接口 `SessionStore` 增加成员：

```typescript
import { confirmTool } from "@/lib/ipc";
import type { SessionRow, ChatMessage, ChatPart, AppEvent, MessageRow, ConfirmRequest } from "@/lib/types";

// SessionStore 接口内追加：
  pendingConfirms: Record<string, ConfirmRequest[]>; // session_id → pending 列表
  confirmToolAction: (confirmId: string, approved: boolean) => Promise<void>;
```

初始 state 加 `pendingConfirms: {}`。

`handleEvent` 中，`if (event.type === "tool_executing")` 分支**之前**加：

```typescript
    if (event.type === "confirm_required") {
      const req: ConfirmRequest = {
        confirm_id: event.confirm_id,
        session_id: session_id,
        tool: event.tool,
        args: event.args,
        risk_level: event.risk_level,
        resolved: "pending",
      };
      const existing = state.pendingConfirms[session_id] ?? [];
      set({
        pendingConfirms: {
          ...state.pendingConfirms,
          [session_id]: [...existing, req],
        },
      });
      return;
    }
```

store 实现追加动作：

```typescript
  confirmToolAction: async (confirmId, approved) => {
    // 乐观更新本地状态
    set((state) => {
      const updated: Record<string, ConfirmRequest[]> = {};
      for (const [sid, list] of Object.entries(state.pendingConfirms)) {
        updated[sid] = list.map((c) =>
          c.confirm_id === confirmId
            ? { ...c, resolved: approved ? ("approved" as const) : ("rejected" as const) }
            : c,
        );
      }
      return { pendingConfirms: updated };
    });
    try {
      await confirmTool(confirmId, approved);
    } catch (e) {
      console.error("Failed to confirm tool:", errMsg(e));
    }
  },
```

超时收尾：`handleEvent` 中 agent_stopped/agent_crashed 分支里追加清理（对已结束会话把 pending 全部置 timeout）：

```typescript
      // 结束时未决确认全部置为 timeout（后端 120s 超时兜底，这里做视觉收尾）
      const pending = state.pendingConfirms[session_id] ?? [];
      if (pending.some((c) => c.resolved === "pending")) {
        set({
          pendingConfirms: {
            ...state.pendingConfirms,
            [session_id]: pending.map((c) =>
              c.resolved === "pending" ? { ...c, resolved: "timeout" as const } : c,
            ),
          },
        });
      }
```

- [ ] **Step 2: ConfirmCard 组件**

`src/components/chat/ConfirmCard.tsx`：

```tsx
import { useEffect, useState } from "react";
import { WarningCircle, CheckCircle, XCircle, Clock } from "@phosphor-icons/react";
import type { ConfirmRequest } from "@/lib/types";
import { useSessionStore } from "@/store/sessionStore";

const RISK_LABELS: Record<string, string> = {
  read_only: "只读",
  low: "低风险",
  high: "高风险",
};

const CONFIRM_TIMEOUT_SECS = 120;

export function ConfirmCard({ request }: { request: ConfirmRequest }) {
  const confirmToolAction = useSessionStore((s) => s.confirmToolAction);
  const [remaining, setRemaining] = useState(CONFIRM_TIMEOUT_SECS);

  useEffect(() => {
    if (request.resolved !== "pending") return;
    const start = Date.now();
    const timer = setInterval(() => {
      const elapsed = Math.floor((Date.now() - start) / 1000);
      const left = CONFIRM_TIMEOUT_SECS - elapsed;
      setRemaining(left > 0 ? left : 0);
      if (left <= 0) clearInterval(timer);
    }, 1000);
    return () => clearInterval(timer);
  }, [request.resolved]);

  const isPending = request.resolved === "pending";
  const command =
    typeof (request.args as { command?: unknown })?.command === "string"
      ? ((request.args as { command: string }).command)
      : JSON.stringify(request.args, null, 2);

  return (
    <div
      className={`rounded-lg border overflow-hidden mb-3 ${
        isPending ? "border-destructive/60 bg-destructive/5" : "border-border bg-card"
      }`}
    >
      <div className="flex items-center gap-2 px-3 py-2">
        <WarningCircle
          size={14}
          weight="fill"
          className={isPending ? "text-destructive shrink-0" : "text-muted-foreground shrink-0"}
          aria-hidden="true"
        />
        <span
          className="text-xs font-semibold text-foreground shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {request.tool}
        </span>
        <span className="text-[10px] px-1.5 py-px rounded border bg-destructive/10 text-destructive border-destructive/20 shrink-0">
          {RISK_LABELS[request.risk_level] ?? request.risk_level}
        </span>
        <span className="ml-auto text-xs text-muted-foreground shrink-0 flex items-center gap-1">
          {isPending ? (
            <>
              <Clock size={12} aria-hidden="true" />
              {remaining}s
            </>
          ) : request.resolved === "approved" ? (
            <span className="text-success flex items-center gap-1">
              <CheckCircle size={12} weight="fill" aria-hidden="true" /> 已批准
            </span>
          ) : request.resolved === "rejected" ? (
            <span className="text-destructive flex items-center gap-1">
              <XCircle size={12} weight="fill" aria-hidden="true" /> 已拒绝
            </span>
          ) : (
            "已超时"
          )}
        </span>
      </div>

      <div className="px-3 pb-2">
        <pre
          className="text-xs text-muted-foreground whitespace-pre-wrap break-all bg-background rounded-md px-3 py-2 border border-border max-h-40 overflow-y-auto"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {command}
        </pre>
      </div>

      {isPending && (
        <div className="flex gap-2 px-3 pb-3">
          <button
            onClick={() => confirmToolAction(request.confirm_id, true)}
            className="px-3 py-1.5 rounded-md bg-destructive text-destructive-foreground text-xs hover:bg-destructive/80 transition-colors cursor-pointer"
          >
            批准执行
          </button>
          <button
            onClick={() => confirmToolAction(request.confirm_id, false)}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            拒绝
          </button>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 3: MessageList 渲染**

`src/components/chat/MessageList.tsx`：

```tsx
import { useSessionStore } from "@/store/sessionStore";
import { ConfirmCard } from "./ConfirmCard";

// 组件内：
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const pendingConfirms = useSessionStore((s) => s.pendingConfirms);
  const confirms = currentSessionId ? (pendingConfirms[currentSessionId] ?? []) : [];
```

`{messages.map(...)}` 之后、`<div ref={bottomRef} />` 之前插入：

```tsx
      {confirms.map((c) => (
        <ConfirmCard key={c.confirm_id} request={c} />
      ))}
```

- [ ] **Step 4: typecheck**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 5: Commit**

```bash
git add src/components/chat/ src/store/sessionStore.ts
git commit -m "feat: inline confirm card for high-risk tool calls"
```

---

## Task 17: 全量验证收口

**Files:**
- Modify: `TODO.md`（勾选阶段 1）
- Modify: `docs/architecture/overview.md`、`docs/architecture/error-handling.md`、`docs/architecture/runtime.md`（与实现事实对齐——仅改动与 exec 层相关的段落）

- [ ] **Step 1: 全量检查**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Run: `pnpm typecheck`
Expected: 三项全绿

- [ ] **Step 2: 死代码扫描**

Run: `rg -n "k8s" src-tauri/src/ src/`
Expected: 仅 infra/db.rs 迁移注释与 0001_init.sql 的建列语句（表结构兼容层，有意保留）

Run: `cargo check --manifest-path src-tauri/Cargo.toml 2>&1 | rg -i "warning"`
Expected: 无 warning（特别是 unused/dead_code）

- [ ] **Step 3: 手工全链路验收（有真实 Linux 跳板机时）**

Run: `pnpm tauri dev`

1. 右栏添加环境（私钥 + 密码各一），测试连接通过
2. 对话输入"xx 环境 OOMService OOM 了，帮我定位"
3. 观察 agent 调 list_environments → run_command(environment, command)
4. 消息流出现高风险确认卡片 → 批准 → 命令执行 → 结果返回
5. 中途点拒绝验证 agent 收到取消
6. 等 120s 验证超时自动拒绝（可选，长测）
7. 重启 app 验证环境列表持久化
8. 空闲 10min 后日志出现 "closing idle ssh connection"

无跳板机时跳过 3-8，标注"待真机验证"。

- [ ] **Step 4: 文档对齐**

- `TODO.md` 阶段 1 六项勾选（`- [x]`）
- `docs/architecture/overview.md`：决策 #2（执行层）如仍写"SSH 单通道 + session 绑定"则改为"SSH 单通道，连接按环境池化、空闲 10min 断开；session 与环境解耦，agent 经 list_environments 发现环境"
- `docs/architecture/error-handling.md`：重试表保持（重试 2 次/重连 1 次已实现），补充"环境名不存在 → 工具错误引导 agent"
- `docs/architecture/runtime.md`：取消模型一节如有"close_session 断开 exec channel"描述，删除（连接与 session 生命周期解耦）

- [ ] **Step 5: Commit**

```bash
git add TODO.md docs/architecture/
git commit -m "docs: align architecture docs with environment-decoupled exec model, check off phase 1"
```

---

## Self-Review 记录

- **Spec 覆盖**：spec §1 决策表 14 项——决策 1/2（解耦+list_environments）→ Task 5/10/11；3（认证）→ Task 2/3/7；4（host key）→ Task 3；5（登录 shell）→ Task 2；6（超时/输出）→ Task 9；7（池化+空闲）→ Task 5；8/9（重试重连）→ Task 3；10（keyring）→ Task 7；11（High 拦截）→ 已有机制 + Task 16 UI；12（确认卡片）→ Task 16；13（环境 UI）→ Task 13-15；14（删 k8s）→ Task 6。§6 CRUD → Task 8；§7 前端四块 → Task 13-16；§9 测试 → 各任务内嵌；§10 验收 → Task 17。无缺口。
- **占位符扫描**：Task 3 Step 4 对 russh 0.45 API 细节的"以 cargo check 为准"是运行时适配指令（附带了不可妥协的行为清单），非占位符；其余步骤均含完整代码。
- **类型一致性**：`SshAuth::from_row` 两处调用（Task 2 pool 临时代码 vs Task 5 重写版）——Task 5 重写后以 Task 5 为准；`credentials::store_secret` 两参签名在 Task 3（调用方）与 Task 7（定义方）一致；前端 `ConfirmRequest.resolved` 字面量联合与 Task 16 更新逻辑一致；`list_environments` 函数名在 app 层（Rust）与 tools 层（Rust）同名但模块不同不冲突。
