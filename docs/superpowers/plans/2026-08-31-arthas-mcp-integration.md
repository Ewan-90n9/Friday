# Arthas MCP Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Friday 对接官方 Arthas 4.x 内置 MCP Server——agent 通过 `arthas_*` 系列工具（经 SSH 隧道代理）诊断远程 JVM，含 arthas 工具包下发、attach 用户对齐、环境多用户凭证管理。

**Architecture:** Friday 作为 MCP client（rmcp streamable-http-client + Bearer）经新增的 SSH direct-tcpip 隧道连到目标机上的 arthas MCP server；ArthasManager（移植 HeapAnalyzerManager 模式）管理 (env_id, pid) 会话生命周期（Attaching/Ready/Failed、LRU 3、空闲 15min 回收、传输错误 invalidate）；27 个 `arthas_*` 工具注册进 Tool Registry（风险分级拦截复用现有机制）。环境多用户凭证：新表 `env_credentials` + keychain `env/{id}/cred/{cred_id}`，默认凭证即现有 SSH 用户，attach 时 JVM 用户与 SSH 用户不一致则用对应用户凭证建临时连接。

**Tech Stack:** Rust (Tauri 2, russh 0.45, rmcp 3.1.4, sqlx), React (TypeScript)。

**Spec:** `docs/superpowers/specs/2026-08-31-arthas-mcp-integration-design.md`（本计划的唯一需求来源，实现中有疑问先查 spec）

**验证命令：**

- Rust 测试：`cargo test --manifest-path src-tauri/Cargo.toml`
- Rust 编译：`cargo check --manifest-path src-tauri/Cargo.toml`
- 前端类型：`pnpm typecheck`

**日志规范（全程遵守）：** 新增的每个 command / manager 入口有 `#[instrument]` 或入口 `info!`；错误路径 `tracing::error!`/`warn!`；SSH 远端命令的 stderr 必须读取记录（拼进日志或错误消息）；日志不截断、不脱敏。

---

## 文件结构总览

| 文件 | 职责 | 任务 |
|---|---|---|
| `src-tauri/Cargo.toml` | rmcp 增加 streamable-http-client feature | 1 |
| `src-tauri/src/exec/ssh.rs` | `open_direct_tcpip`（direct-tcpip 原语）、cred_id 字段 | 2, 6 |
| `src-tauri/src/exec/tunnel.rs` | **新建** TunnelManager（本地端口转发） | 3 |
| `src-tauri/src/exec/mod.rs` | 注册 tunnel 模块 | 3 |
| `src-tauri/migrations/0009_env_credentials.sql` | **新建** 凭证表 | 4 |
| `src-tauri/src/infra/db.rs` | 挂载 0009 | 4 |
| `src-tauri/src/app/env_credentials.rs` | **新建** 凭证数据层 + commands + legacy 迁移 | 4, 8 |
| `src-tauri/src/app/mod.rs` | 注册 env_credentials 模块 | 4 |
| `src-tauri/src/app/credentials.rs` | cred 维度 keychain 函数 | 5 |
| `src-tauri/src/exec/pool.rs` | fetch_environment 读默认凭证 | 6 |
| `src-tauri/src/app/environments.rs` | 增/改/删环境同步凭证；test_connection 凭证化 | 7 |
| `src-tauri/src/lib.rs` | migrate_legacy 调用、AppState、全部装配 | 7, 16 |
| `src-tauri/src/provision/arthas.rs` | **新建** ArthasPackage（artifactory 下发） | 10 |
| `src-tauri/src/provision/mod.rs` / `jdk.rs` | 模块注册；run_remote/try_remote_download 提为 pub(crate) | 10 |
| `src-tauri/src/arthas/mod.rs` | **新建** 模块根 | 11 |
| `src-tauri/src/arthas/client.rs` | **新建** McpArthasClient | 12 |
| `src-tauri/src/arthas/manager.rs` | **新建** ArthasManager（生命周期） | 13 |
| `src-tauri/src/arthas/attach.rs` | **新建** 命令构造纯函数 + 生产 attach 编排 + StopHandle | 11, 14 |
| `src-tauri/src/tools/builtin/arthas/mod.rs` | **新建** 27 个工具 | 15 |
| `src-tauri/src/tools/builtin/arthas/mapping.rs` | **新建** 参数映射 + 子操作过滤 | 15 |
| `src-tauri/src/tools/builtin/mod.rs` | 注册 arthas 模块 | 15 |
| `src-tauri/src/agent/prompt.rs` | TOOL_GUIDANCE / 系统提示更新 | 16 |
| `src/lib/types.ts`、`src/lib/ipc.ts` | 凭证类型与 IPC 绑定 | 9 |
| `src/components/environments/EnvironmentDialog.tsx` | 凭证管理 UI | 9 |
| `AGENTS.md`、`docs/architecture/overview.md` | 文档同步 | 16 |

---

### Task 1: rmcp streamable-http-client feature

**Files:**

- Modify: `src-tauri/Cargo.toml:37`

- [ ] **Step 1: 修改 rmcp feature 列表**

```toml
rmcp = { version = "3", features = ["server", "macros", "transport-streamable-http-server", "client", "transport-child-process", "transport-async-rw", "transport-streamable-http-client", "transport-streamable-http-client-reqwest"] }
```

- [ ] **Step 2: 验证编译**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过（reqwest 作为 rmcp 依赖进入 lock 文件）

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "build: enable rmcp streamable-http-client features"
```

---

### Task 2: SshTransport.open_direct_tcpip

**Files:**

- Modify: `src-tauri/src/exec/ssh.rs`（`impl SshTransport` 内添加方法；文件如无 tests 模块则末尾新建）

- [ ] **Step 1: 写失败测试（文件末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_direct_tcpip_without_connection_errors() {
        let t = SshTransport::new("env-1", "h", 22, "u", SshAuth::Password);
        let r = t.open_direct_tcpip("127.0.0.1", 8563).await;
        assert!(r.is_err());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml open_direct_tcpip`
Expected: 编译失败（方法不存在）

- [ ] **Step 3: 实现（`impl SshTransport` 块内，`connect_once` 之后）**

```rust
    /// 打开 direct-tcpip 转发 channel（SSH 本地端口转发的底层原语）。
    /// 未连接时返回错误；channel 的读写与关闭由调用方负责。
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
    ) -> Result<russh::Channel<russh::ChannelMsg>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.conn.lock().await;
        let handle = conn
            .as_mut()
            .ok_or_else(|| format!("ssh connection to {} not established", self.host).into())?;
        handle
            .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| format!("open direct-tcpip {host}:{port} failed: {e}").into())
    }
```

注：russh 0.45 的 `channel_open_direct_tcpip` 签名若与预期不同（如需要 `host.to_string()` 传 `impl Into<String>`），按编译器提示调整；目标只有一个——拿到已打开的 channel。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml open_direct_tcpip`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/exec/ssh.rs
git commit -m "feat: ssh direct-tcpip channel primitive"
```

---

### Task 3: TunnelManager（SSH 本地端口转发）

**Files:**

- Create: `src-tauri/src/exec/tunnel.rs`
- Modify: `src-tauri/src/exec/mod.rs`（加 `pub mod tunnel;`）

- [ ] **Step 1: 写失败测试（tunnel.rs 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    #[test]
    fn test_tunnel_key_format() {
        assert_eq!(tunnel_key("env-1", "127.0.0.1", 8563), "env-1/127.0.0.1/8563");
    }

    #[tokio::test]
    async fn test_open_unknown_environment_errors() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool);
        let r = mgr.open("no-such-env", "127.0.0.1", 8563).await;
        assert!(matches!(r, Err(TunnelError::EnvironmentNotFound(_))));
    }

    #[tokio::test]
    async fn test_close_nonexistent_is_noop() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool);
        mgr.close("env-1", "127.0.0.1", 8563).await;
        assert_eq!(mgr.tunnel_count().await, 0);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tunnel`
Expected: 编译失败（模块不存在）

- [ ] **Step 3: 实现 tunnel.rs**

```rust
use super::pool::{build_transport, fetch_environment};
use super::ssh::SshTransport;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("environment not found: {0}")]
    EnvironmentNotFound(String),
    #[error("ssh connection failed: {0}")]
    Connection(String),
    #[error("local listen failed: {0}")]
    Listen(String),
}

/// 一条已建立隧道的描述（值类型，调用方只读）
#[derive(Clone, Debug)]
pub struct TunnelLease {
    pub env_id: String,
    pub remote_host: String,
    pub remote_port: u16,
    pub local_port: u16,
}

struct TunnelEntry {
    local_port: u16,
    transport: Arc<SshTransport>,
    accept_task: tokio::task::JoinHandle<()>,
    refs: u32,
}

/// SSH 本地端口转发管理器（russh direct-tcpip）。
/// 按 (env_id, remote_host, remote_port) 复用隧道，引用计数，归零即拆除。
/// 隧道独享一条 SSH 连接（不与 exec 池混用 channel），
/// 避免 russh 多路复用下 exec 大输出阻塞转发数据通道。
pub struct TunnelManager {
    db: sqlx::SqlitePool,
    inner: Mutex<HashMap<String, TunnelEntry>>,
}

fn tunnel_key(env_id: &str, remote_host: &str, remote_port: u16) -> String {
    format!("{env_id}/{remote_host}/{remote_port}")
}

impl TunnelManager {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self { db, inner: Mutex::new(HashMap::new()) }
    }

    /// 打开（或复用）一条到目标机 remote_host:remote_port 的隧道。
    /// 本地监听 127.0.0.1 临时端口（OS 分配），返回 TunnelLease。
    #[tracing::instrument(skip(self))]
    pub async fn open(
        &self,
        env_id: &str,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<TunnelLease, TunnelError> {
        let key = tunnel_key(env_id, remote_host, remote_port);
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.get_mut(&key) {
            entry.refs += 1;
            return Ok(TunnelLease {
                env_id: env_id.to_string(),
                remote_host: remote_host.to_string(),
                remote_port,
                local_port: entry.local_port,
            });
        }

        let env = fetch_environment(&self.db, env_id)
            .await
            .map_err(|e| TunnelError::EnvironmentNotFound(e.to_string()))?;
        let transport = build_transport(env_id, &env)
            .map_err(|e| TunnelError::Connection(e.to_string()))?;
        transport
            .connect()
            .await
            .map_err(|e| TunnelError::Connection(e.to_string()))?;
        let transport = Arc::new(transport);

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| TunnelError::Listen(e.to_string()))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| TunnelError::Listen(e.to_string()))?
            .port();

        let accept_task = tokio::spawn(accept_loop(
            listener,
            transport.clone(),
            remote_host.to_string(),
            remote_port,
        ));
        inner.insert(key, TunnelEntry { local_port, transport, accept_task, refs: 1 });
        tracing::info!(env_id, remote_host, remote_port, local_port, "ssh tunnel opened");
        Ok(TunnelLease {
            env_id: env_id.to_string(),
            remote_host: remote_host.to_string(),
            remote_port,
            local_port,
        })
    }

    /// 引用计数减一；归零时拆除隧道（停 accept + 断开隧道专属 SSH 连接）。
    /// 已建立的转发连接随 SSH 连接断开而终止。幂等。
    pub async fn close(&self, env_id: &str, remote_host: &str, remote_port: u16) {
        let key = tunnel_key(env_id, remote_host, remote_port);
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get_mut(&key) else { return };
        entry.refs = entry.refs.saturating_sub(1);
        if entry.refs == 0 {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                let env_id_owned = env_id.to_string();
                tokio::spawn(async move {
                    transport.disconnect().await;
                    tracing::info!(env_id = %env_id_owned, remote_port, "ssh tunnel closed");
                });
            }
        }
    }

    /// 拆除某环境全部隧道（环境删除联动）。幂等。
    pub async fn close_all_for_env(&self, env_id: &str) {
        let mut inner = self.inner.lock().await;
        let prefix = format!("{env_id}/");
        let keys: Vec<String> =
            inner.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for key in keys {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                tokio::spawn(async move { transport.disconnect().await; });
            }
        }
    }

    pub async fn tunnel_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

async fn accept_loop(
    listener: TcpListener,
    transport: Arc<SshTransport>,
    remote_host: String,
    remote_port: u16,
) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else { break };
        let transport = transport.clone();
        let remote_host = remote_host.clone();
        tokio::spawn(async move {
            if let Err(e) = forward(stream, transport, &remote_host, remote_port).await {
                tracing::warn!(remote_host = %remote_host, remote_port, error = %e, "tunnel forward ended with error");
            }
        });
    }
}

/// 单条本地 TCP 连接的双向转发：local TCP ⇄ direct-tcpip channel
async fn forward(
    stream: tokio::net::TcpStream,
    transport: Arc<SshTransport>,
    remote_host: &str,
    remote_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let channel = transport.open_direct_tcpip(remote_host, remote_port).await?;
    let (mut tcp_read, mut tcp_write) = tokio::io::split(stream);
    let (mut ch_read, mut ch_write) = tokio::io::split(channel);
    // 任一方向结束即结束（HTTP 客户端按需开新连接）
    tokio::select! {
        r = tokio::io::copy(&mut tcp_read, &mut ch_write) => { r?; }
        r = tokio::io::copy(&mut ch_read, &mut tcp_write) => { r?; }
    }
    Ok(())
}
```

并在 `src-tauri/src/exec/mod.rs` 加：

```rust
pub mod tunnel;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tunnel`
Expected: PASS（3 个测试）

- [ ] **Step 5: cargo check + Commit**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
git add src-tauri/src/exec/tunnel.rs src-tauri/src/exec/mod.rs
git commit -m "feat: ssh tunnel manager (direct-tcpip local forward)"
```

---

### Task 4: env_credentials 表 + 数据层

**Files:**

- Create: `src-tauri/migrations/0009_env_credentials.sql`
- Modify: `src-tauri/src/infra/db.rs`（挂载 0009）
- Create: `src-tauri/src/app/env_credentials.rs`
- Modify: `src-tauri/src/app/mod.rs`（加 `pub mod env_credentials;`）

- [ ] **Step 1: 新建 migration 0009_env_credentials.sql**

```sql
-- 环境多用户凭证：一个环境可录多个用户（密码或私钥），其中一个默认。
-- 默认凭证 = Friday 日常连接（连接池 / run_command / jvm_*）使用的 SSH 用户。
-- 密钥本体不入库，存 OS keychain：friday/env/{env_id}/cred/{cred_id}
CREATE TABLE IF NOT EXISTS env_credentials (
    id TEXT PRIMARY KEY,
    environment_id TEXT NOT NULL,
    username TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'private_key',
    private_key_path TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_env_credentials_env ON env_credentials(environment_id);
```

- [ ] **Step 2: db.rs 挂载（在 schema8 块之后、`tracing::info!` 之前）**

```rust
    // Migration (arthas)：环境多用户凭证表
    let schema9 = include_str!("../../migrations/0009_env_credentials.sql");
    sqlx::query(schema9).execute(&pool).await?;
```

- [ ] **Step 3: 写失败测试（env_credentials.rs 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    async fn add_env(pool: &SqlitePool, id: &str, user: &str) {
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES (?, 'e', '10.0.0.1', 22, ?, 'ssh', 'password', '2026-01-01T00:00:00Z')",
        )
        .bind(id).bind(user).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn test_add_list_and_default() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        let first = add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        assert!(first.is_default);
        let second = add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        assert!(!second.is_default);

        let list = list_credentials(&pool, "env-1").await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].is_default); // default 排前

        let def = default_credential(&pool, "env-1").await.unwrap().unwrap();
        assert_eq!(def.username, "opc");
    }

    #[tokio::test]
    async fn test_add_duplicate_username_rejected() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let err = add_credential(&pool, "env-1", "opc", "password", None, None, false).await.unwrap_err();
        assert!(matches!(err, EnvCredentialError::Validation(_)));
    }

    #[tokio::test]
    async fn test_find_by_username() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        let found = find_credential_by_username(&pool, "env-1", "svcapp").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().username, "svcapp");
        assert!(find_credential_by_username(&pool, "env-1", "nobody").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_delete_default_rejected() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        let cred = add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let err = delete_credential(&pool, "env-1", &cred.id).await.unwrap_err();
        assert!(matches!(err, EnvCredentialError::Validation(_)));
    }

    #[tokio::test]
    async fn test_delete_non_default_ok() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let extra = add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        delete_credential(&pool, "env-1", &extra.id).await.unwrap();
        assert_eq!(list_credentials(&pool, "env-1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_set_default_syncs_environments_user() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let svc = add_credential(&pool, "env-1", "svcapp", "private_key", Some("~/.ssh/svc"), None, false).await.unwrap();

        set_default_credential(&pool, "env-1", &svc.id).await.unwrap();
        let def = default_credential(&pool, "env-1").await.unwrap().unwrap();
        assert_eq!(def.username, "svcapp");
        // environments.user 镜像默认凭证用户名
        let (user,): (String,) = sqlx::query_as("SELECT user FROM environments WHERE id = 'env-1'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(user, "svcapp");
        let (auth, key_path): (String, Option<String>) =
            sqlx::query_as("SELECT auth_type, private_key_path FROM environments WHERE id = 'env-1'")
                .fetch_one(&pool).await.unwrap();
        assert_eq!(auth, "private_key");
        assert_eq!(key_path.as_deref(), Some("~/.ssh/svc"));
    }
}
```

注：测试不传 secret（None），不会触碰真实 keychain。

- [ ] **Step 4: 实现 env_credentials.rs（数据层部分；commands 在 Task 8）**

```rust
use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum EnvCredentialError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("credential not found: {0}")]
    NotFound(String),
    #[error("keychain error: {0}")]
    Keychain(String),
}

#[derive(Serialize, Clone, Debug)]
pub struct EnvCredentialRow {
    pub id: String,
    pub environment_id: String,
    pub username: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    pub is_default: bool,
    pub created_at: String,
}

const CRED_COLUMNS: &str = "id, environment_id, username, auth_type, private_key_path, is_default, created_at";

fn row_to_cred(r: &sqlx::sqlite::SqliteRow) -> EnvCredentialRow {
    EnvCredentialRow {
        id: r.get("id"),
        environment_id: r.get("environment_id"),
        username: r.get("username"),
        auth_type: r.get("auth_type"),
        private_key_path: r.get("private_key_path"),
        is_default: r.get::<i64, _>("is_default") != 0,
        created_at: r.get("created_at"),
    }
}

pub async fn list_credentials(
    pool: &SqlitePool,
    environment_id: &str,
) -> Result<Vec<EnvCredentialRow>, EnvCredentialError> {
    let rows = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? \
         ORDER BY is_default DESC, created_at"
    ))
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_cred).collect())
}

pub async fn default_credential(
    pool: &SqlitePool,
    environment_id: &str,
) -> Result<Option<EnvCredentialRow>, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? AND is_default = 1"
    ))
    .bind(environment_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| row_to_cred(&r)))
}

pub async fn find_credential_by_username(
    pool: &SqlitePool,
    environment_id: &str,
    username: &str,
) -> Result<Option<EnvCredentialRow>, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? AND username = ? LIMIT 1"
    ))
    .bind(environment_id)
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| row_to_cred(&r)))
}

pub async fn add_credential(
    pool: &SqlitePool,
    environment_id: &str,
    username: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    secret: Option<&str>,
    make_default: bool,
) -> Result<EnvCredentialRow, EnvCredentialError> {
    if username.trim().is_empty() {
        return Err(EnvCredentialError::Validation("username 不能为空".to_string()));
    }
    if !matches!(auth_type, "private_key" | "password") {
        return Err(EnvCredentialError::Validation(
            "auth_type 必须是 private_key 或 password".to_string(),
        ));
    }
    if auth_type == "private_key"
        && private_key_path.map(str::trim).filter(|p| !p.is_empty()).is_none()
    {
        return Err(EnvCredentialError::Validation("私钥认证需要填写私钥路径".to_string()));
    }
    if find_credential_by_username(pool, environment_id, username.trim()).await?.is_some() {
        return Err(EnvCredentialError::Validation(format!("用户 {username} 的凭证已存在")));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    if make_default {
        sqlx::query("UPDATE env_credentials SET is_default = 0 WHERE environment_id = ?")
            .bind(environment_id)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(environment_id)
    .bind(username.trim())
    .bind(auth_type)
    .bind(private_key_path)
    .bind(if make_default { 1 } else { 0 })
    .bind(&now)
    .execute(pool)
    .await?;

    // keychain 写入失败 → 回滚凭证行（DB 与 keychain 保持一致）
    if let Some(secret) = secret {
        if !secret.is_empty() {
            if let Err(e) = crate::app::credentials::store_cred_secret(environment_id, &id, secret).await {
                tracing::error!(environment_id, cred_id = %id, ?e, "keychain store failed, rolling back credential insert");
                let _ = sqlx::query("DELETE FROM env_credentials WHERE id = ?")
                    .bind(&id)
                    .execute(pool)
                    .await;
                return Err(EnvCredentialError::Keychain(e.to_string()));
            }
        }
    }
    if make_default {
        sqlx::query("UPDATE environments SET user = ? WHERE id = ?")
            .bind(username.trim())
            .bind(environment_id)
            .execute(pool)
            .await?;
    }

    let row = sqlx::query(&format!("SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ?"))
        .bind(&id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| EnvCredentialError::NotFound(id.clone()))?;
    Ok(row_to_cred(&row))
}

pub async fn delete_credential(
    pool: &SqlitePool,
    environment_id: &str,
    cred_id: &str,
) -> Result<(), EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ? AND environment_id = ?"
    ))
    .bind(cred_id)
    .bind(environment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| EnvCredentialError::NotFound(cred_id.to_string()))?;
    let cred = row_to_cred(&row);
    if cred.is_default {
        return Err(EnvCredentialError::Validation(
            "不能删除默认凭证；请先把其他凭证设为默认".to_string(),
        ));
    }
    sqlx::query("DELETE FROM env_credentials WHERE id = ? AND environment_id = ?")
        .bind(cred_id)
        .bind(environment_id)
        .execute(pool)
        .await?;
    if let Err(e) = crate::app::credentials::delete_cred_secret(environment_id, cred_id).await {
        // DB 已删，keychain 残留仅告警（无引用条目无害）
        tracing::warn!(environment_id, cred_id, ?e, "failed to delete credential keychain entry");
    }
    Ok(())
}

pub async fn set_default_credential(
    pool: &SqlitePool,
    environment_id: &str,
    cred_id: &str,
) -> Result<EnvCredentialRow, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ? AND environment_id = ?"
    ))
    .bind(cred_id)
    .bind(environment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| EnvCredentialError::NotFound(cred_id.to_string()))?;
    let cred = row_to_cred(&row);
    sqlx::query("UPDATE env_credentials SET is_default = 0 WHERE environment_id = ?")
        .bind(environment_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE env_credentials SET is_default = 1 WHERE id = ?")
        .bind(cred_id)
        .execute(pool)
        .await?;
    // environments 行镜像默认凭证（user/auth_type/private_key_path），旧路径消费者保持一致
    sqlx::query("UPDATE environments SET user = ?, auth_type = ?, private_key_path = ? WHERE id = ?")
        .bind(&cred.username)
        .bind(&cred.auth_type)
        .bind(&cred.private_key_path)
        .bind(environment_id)
        .execute(pool)
        .await?;
    Ok(cred)
}

/// 一次性迁移：为没有凭证行的环境从 environments 列 + 旧 keychain 条目生成默认凭证。
/// 幂等（已有凭证行的环境跳过）；keychain 移动失败仅告警并保留旧条目，下次启动重试。
/// 由 lib.rs setup 在 db init 后调用。
pub async fn migrate_legacy(pool: &SqlitePool) {
    let envs: Vec<(String, String, String, Option<String>)> = match sqlx::query_as(
        "SELECT e.id, e.user, e.auth_type, e.private_key_path FROM environments e \
         WHERE NOT EXISTS (SELECT 1 FROM env_credentials c WHERE c.environment_id = e.id)",
    )
    .fetch_all(pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(?e, "env_credentials legacy migration query failed");
            return;
        }
    };
    for (env_id, user, auth_type, key_path) in envs {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let inserted = sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(&id)
        .bind(&env_id)
        .bind(&user)
        .bind(&auth_type)
        .bind(&key_path)
        .bind(&now)
        .execute(pool)
        .await;
        if let Err(e) = inserted {
            tracing::error!(env_id = %env_id, ?e, "env_credentials legacy migration insert failed");
            continue;
        }
        // keychain 移动：friday/env/{id}/secret → friday/env/{id}/cred/{cred_id}
        match crate::app::credentials::load_secret(&env_id).await {
            Ok(Some(secret)) => {
                match crate::app::credentials::store_cred_secret(&env_id, &id, &secret).await {
                    Ok(()) => {
                        if let Err(e) = crate::app::credentials::delete_secret(&env_id).await {
                            tracing::warn!(env_id = %env_id, ?e, "legacy secret cleanup failed, will retry next start");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(env_id = %env_id, ?e, "keychain move failed, keeping legacy entry")
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(env_id = %env_id, ?e, "legacy secret read failed"),
        }
        tracing::info!(env_id = %env_id, cred_id = %id, "migrated legacy environment credential");
    }
}
```

并在 `src-tauri/src/app/mod.rs` 加 `pub mod env_credentials;`。

注：`store_cred_secret` 等函数在 Task 5 实现。Task 4 与 Task 5 需一起完成后编译闭环；两任务可合并提交。

- [ ] **Step 5: 运行测试确认通过（需 Task 5 完成）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_credentials`
Expected: PASS（6 个测试）

---

### Task 5: cred 维度 keychain 函数

**Files:**

- Modify: `src-tauri/src/app/credentials.rs`

- [ ] **Step 1: 实现（在现有 `delete_secret` 函数之后追加）**

```rust
/// 凭证维度条目（环境多用户）：friday/env/{env_id}/cred/{cred_id}
fn cred_entry(env_id: &str, cred_id: &str) -> Result<Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, &format!("env/{env_id}/cred/{cred_id}"))
}

/// 存储用户凭证密钥（密码或私钥 passphrase）。空值时删除条目。
pub async fn store_cred_secret(
    env_id: &str,
    cred_id: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to delete stale cred secret"),
        }
        return Ok(());
    }
    entry.set_password(value).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to store cred secret in keychain");
        e
    })?;
    tracing::info!(env_id = %env_id, cred_id = %cred_id, "cred secret stored in keychain");
    Ok(())
}

/// 读取用户凭证密钥。无条目返回 None。
pub async fn load_cred_secret(
    env_id: &str,
    cred_id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => {
            tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to load cred secret from keychain");
            Err(e.into())
        }
    }
}

/// 删除用户凭证密钥。无条目时静默成功。
pub async fn delete_cred_secret(
    env_id: &str,
    cred_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => {
            tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to delete cred secret from keychain");
            Err(e.into())
        }
    }
}
```

- [ ] **Step 2: 运行 Task 4 测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_credentials`
Expected: PASS

- [ ] **Step 3: Commit（Task 4 + Task 5 合并）**

```bash
git add src-tauri/migrations/0009_env_credentials.sql src-tauri/src/infra/db.rs src-tauri/src/app/env_credentials.rs src-tauri/src/app/mod.rs src-tauri/src/app/credentials.rs
git commit -m "feat: env_credentials table, data layer and credential-scoped keychain"
```

---

### Task 6: 连接层凭证化（fetch_environment / SshTransport）

**Files:**

- Modify: `src-tauri/src/exec/ssh.rs`（cred_id 字段 + 构造器 + connect_once 密钥来源）
- Modify: `src-tauri/src/exec/pool.rs`（EnvironmentInfo.default_cred_id + fetch_environment JOIN + build_transport）

- [ ] **Step 1: 写失败测试（pool.rs tests 模块追加）**

```rust
    #[tokio::test]
    async fn test_fetch_environment_prefers_default_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;
        // 环境行：user=root/password；凭证行：svcapp/private_key
        sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES ('c1', 'env-1', 'svcapp', 'private_key', '~/.ssh/svc', 1, '2026-01-01T00:00:00Z')",
        )
        .execute(&db_pool).await.unwrap();

        let info = fetch_environment(&db_pool, "env-1").await.unwrap();
        assert_eq!(info.user.as_deref(), Some("svcapp"));
        assert_eq!(info.auth_type.as_deref(), Some("private_key"));
        assert_eq!(info.private_key_path.as_deref(), Some("~/.ssh/svc"));
        assert_eq!(info.default_cred_id.as_deref(), Some("c1"));

        let transport = build_transport("env-1", &info).unwrap();
        assert_eq!(transport.user, "svcapp");
        assert_eq!(transport.cred_id_as_ref(), Some("c1"));
    }

    #[tokio::test]
    async fn test_fetch_environment_falls_back_to_env_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;

        let info = fetch_environment(&db_pool, "env-1").await.unwrap();
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.auth_type.as_deref(), Some("password"));
        assert!(info.default_cred_id.is_none());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml fetch_environment`
Expected: 编译失败（default_cred_id / cred_id_as_ref 不存在）

- [ ] **Step 3: 实现 ssh.rs 变更**

3a. `SshTransport` struct 增加字段（`secret_override` 之后）：

```rust
    /// 凭证维度密钥链条目 id（env_credentials.id）；
    /// None = 旧路径 friday/env/{env_id}/secret（迁移前兜底）
    cred_id: Option<String>,
```

3b. 构造器调整：`new` 与 `with_secret` 的 struct 字面量补 `cred_id: None`；新增 `with_cred` 与 `cred_id_as_ref`：

```rust
    /// 使用 env_credentials 凭证构造（密钥从 friday/env/{env_id}/cred/{cred_id} 读取）
    pub fn with_cred(
        env_id: &str,
        host: &str,
        port: u16,
        user: &str,
        auth: SshAuth,
        cred_id: &str,
    ) -> Self {
        Self {
            cred_id: Some(cred_id.to_string()),
            ..Self::new(env_id, host, port, user, auth)
        }
    }

    /// 测试用：读取 cred_id
    pub fn cred_id_as_ref(&self) -> Option<&str> {
        self.cred_id.as_deref()
    }

    /// 密钥链读取：cred_id 存在走凭证路径，否则旧路径（迁移前兜底）
    async fn load_keychain_secret(
        &self,
    ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        match &self.cred_id {
            Some(cid) => crate::app::credentials::load_cred_secret(&self.env_id, cid).await,
            None => crate::app::credentials::load_secret(&self.env_id).await,
        }
    }
```

3c. `connect_once` 内两处 `crate::app::credentials::load_secret(&self.env_id).await?` 都替换为：

```rust
                let keychain = self.load_keychain_secret().await?;
```

- [ ] **Step 4: 实现 pool.rs 变更**

4a. `EnvironmentInfo` 增加字段：

```rust
    /// 默认凭证 id（env_credentials.id）；无凭证行时 None（退回 environments 列）
    pub default_cred_id: Option<String>,
```

4b. `build_transport` ssh 分支替换为：

```rust
        "ssh" => {
            let auth = super::ssh::SshAuth::from_row(
                env.auth_type.as_deref().unwrap_or("private_key"),
                env.private_key_path.as_deref(),
            )
            .ok_or_else(|| PoolError::TransportNotImplemented(format!(
                "invalid auth config for environment {environment_id}"
            )))?;
            let transport = match &env.default_cred_id {
                Some(cred_id) => super::ssh::SshTransport::with_cred(
                    environment_id,
                    env.host.as_deref().unwrap_or_default(),
                    env.port.unwrap_or(22),
                    env.user.as_deref().unwrap_or_default(),
                    auth,
                    cred_id,
                ),
                None => super::ssh::SshTransport::new(
                    environment_id,
                    env.host.as_deref().unwrap_or_default(),
                    env.port.unwrap_or(22),
                    env.user.as_deref().unwrap_or_default(),
                    auth,
                ),
            };
            Ok(transport)
        }
```

4c. `fetch_environment` 整体替换为（LEFT JOIN 默认凭证，COALESCE 兜底旧列）：

```rust
pub async fn fetch_environment(
    pool: &SqlitePool,
    environment_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
    let row: Option<(
        Option<String>,
        Option<i64>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT e.host, e.port, e.user, e.transport_type, \
                COALESCE(c.auth_type, e.auth_type), \
                COALESCE(c.private_key_path, e.private_key_path), \
                COALESCE(c.username, e.user), \
                c.id \
         FROM environments e \
         LEFT JOIN env_credentials c ON c.environment_id = e.id AND c.is_default = 1 \
         WHERE e.id = ?",
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
        user: row.6,
        auth_type: row.4,
        private_key_path: row.5,
        default_cred_id: row.7,
    })
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::pool`
Expected: PASS（原有全部 + 2 个新测试）

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/exec/ssh.rs src-tauri/src/exec/pool.rs
git commit -m "feat: exec layer resolves auth from default env credential"
```

---

### Task 7: 环境增删改同步凭证 + legacy 迁移接线

**Files:**

- Modify: `src-tauri/src/app/environments.rs`
- Modify: `src-tauri/src/lib.rs`（setup 调 migrate_legacy）

- [ ] **Step 1: 写失败测试（environments.rs tests 模块追加）**

```rust
    #[tokio::test]
    async fn test_add_environment_creates_default_credential_row() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "opc", "password", None, None).await.unwrap();
        let cred = crate::app::env_credentials::default_credential(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(cred.username, "opc");
        assert_eq!(cred.auth_type, "password");
    }

    #[tokio::test]
    async fn test_update_environment_syncs_default_credential() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "opc", "password", None, None).await.unwrap();
        update_environment(&pool, &env.id, "prod", "10.0.0.1", 22, "deploy", "private_key", Some("~/.ssh/deploy"), None).await.unwrap();
        let cred = crate::app::env_credentials::default_credential(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(cred.username, "deploy");
        assert_eq!(cred.auth_type, "private_key");
        assert_eq!(cred.private_key_path.as_deref(), Some("~/.ssh/deploy"));
        // 额外凭证不受影响
        crate::app::env_credentials::add_credential(&pool, &env.id, "svcapp", "password", None, None, false).await.unwrap();
        update_environment(&pool, &env.id, "prod2", "10.0.0.1", 22, "deploy", "private_key", Some("~/.ssh/deploy"), None).await.unwrap();
        assert!(crate::app::env_credentials::find_credential_by_username(&pool, &env.id, "svcapp").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_migrate_legacy_creates_default_credential() {
        let (_tmp, pool) = setup().await;
        // 直接走 SQL 模拟一个未迁移的旧环境（绕过新的 add_environment）
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES ('old-1', 'old', '10.0.0.1', 22, 'opc', 'ssh', 'password', '2026-01-01T00:00:00Z')",
        ).execute(&pool).await.unwrap();

        crate::app::env_credentials::migrate_legacy(&pool).await;
        let cred = crate::app::env_credentials::default_credential(&pool, "old-1").await.unwrap().unwrap();
        assert_eq!(cred.username, "opc");

        // 幂等：再跑一次不重复插入
        crate::app::env_credentials::migrate_legacy(&pool).await;
        let all = crate::app::env_credentials::list_credentials(&pool, "old-1").await.unwrap();
        assert_eq!(all.len(), 1);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_add_environment_creates_default_credential`
Expected: FAIL（default_credential 返回 None）

- [ ] **Step 3: 实现**

3a. `add_environment`：删除原有「密码/私钥 passphrase 入密钥链」的 if 块，替换为：

```rust
    // 创建默认凭证行（keychain 走 env/{id}/cred/{cred_id} 新路径）；
    // keychain 失败回滚（env 行 + 凭证行一起删，保持一致）
    if let Err(e) = crate::app::env_credentials::add_credential(
        pool, &id, user, auth_type, private_key_path, password, true,
    ).await {
        tracing::error!(env_id = %id, ?e, "default credential creation failed, rolling back environment insert");
        if let Err(del_err) = sqlx::query("DELETE FROM environments WHERE id = ?")
            .bind(&id).execute(pool).await
        {
            tracing::error!(env_id = %id, ?del_err, "rollback delete failed, orphaned environment row remains");
        }
        return Err(EnvironmentError::Keychain(e.to_string()));
    }
```

3b. `update_environment`：删除 UPDATE 之前的 `should_clear_secret_on_update` 提前块与 UPDATE 之后的旧 keychain if 块，在 `UPDATE environments ...` 执行成功（rows_affected 检查）之后追加：

```rust
    // 默认凭证行同步（environments.user/auth 与默认凭证保持一致）
    match crate::app::env_credentials::default_credential(pool, id).await {
        Ok(Some(cred)) => {
            // 认证切换且未提供新密钥 → 清除该凭证密钥（旧密钥不能跨认证模式残留）
            if should_clear_secret_on_update(&old.auth_type, auth_type, new_secret_provided) {
                tracing::info!(env_id = %id, cred_id = %cred.id, "auth_type switched without new secret, clearing cred keychain entry");
                if let Err(e) = crate::app::credentials::delete_cred_secret(id, &cred.id).await {
                    tracing::warn!(env_id = %id, ?e, "failed to clear cred secret");
                }
            }
            sqlx::query("UPDATE env_credentials SET username = ?, auth_type = ?, private_key_path = ? WHERE id = ?")
                .bind(user).bind(auth_type).bind(private_key_path).bind(&cred.id)
                .execute(pool).await?;
            if let Some(secret) = password {
                if !secret.is_empty() {
                    crate::app::credentials::store_cred_secret(id, &cred.id, secret).await
                        .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
                }
            }
        }
        Ok(None) => {
            // 无凭证行（迁移未跑）：退回旧路径行为
            if should_clear_secret_on_update(&old.auth_type, auth_type, new_secret_provided) {
                tracing::info!(env_id = %id, "auth_type switched without new secret, clearing legacy keychain entry");
                crate::app::credentials::delete_secret(id).await
                    .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
            }
            if let Some(secret) = password {
                if !secret.is_empty() {
                    crate::app::credentials::store_secret(id, secret).await
                        .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
                }
            }
        }
        Err(e) => {
            tracing::warn!(env_id = %id, ?e, "default credential lookup failed, keychain not updated");
        }
    }
```

3c. `delete_environment_cmd`：将「删 keychain 条目」处（`delete_secret(&id)` 调用）替换为凭证级联清理：

```rust
    // 删除该环境全部凭证（keychain 条目 + DB 行）与环境级 keychain（失败仅告警，不阻塞删除）
    if let Ok(creds) = crate::app::env_credentials::list_credentials(&state.db, &id).await {
        for cred in creds {
            if let Err(e) = crate::app::credentials::delete_cred_secret(&id, &cred.id).await {
                tracing::warn!(env_id = %id, cred_id = %cred.id, ?e, "failed to delete credential secret");
            }
        }
        if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?")
            .bind(&id).execute(&state.db).await
        {
            tracing::warn!(env_id = %id, ?e, "failed to delete credential rows");
        }
    }
    if let Err(e) = crate::app::credentials::delete_secret(&id).await {
        tracing::warn!(env_id = %id, ?e, "failed to delete legacy keychain secret");
    }
```

3d. `test_connection_params_cmd` 的 `FromKeychain` 分支替换为（优先凭证路径，回退旧路径）：

```rust
        TestSecret::FromKeychain(env_id) => {
            get_environment(&state.db, &env_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("环境不存在".to_string())?;
            match crate::app::env_credentials::default_credential(&state.db, &env_id).await {
                Ok(Some(cred)) => crate::app::credentials::load_cred_secret(&env_id, &cred.id)
                    .await
                    .map_err(|e| e.to_string())?
                    .or(crate::app::credentials::load_secret(&env_id).await.map_err(|e| e.to_string())?),
                _ => crate::app::credentials::load_secret(&env_id).await.map_err(|e| e.to_string())?,
            }
        }
```

3e. `lib.rs` setup：在 `detect_and_persist` 之后插入：

```rust
            // 环境多用户凭证：旧单用户数据迁移为默认凭证行（幂等）
            tauri::async_runtime::block_on(app::env_credentials::migrate_legacy(&pool));
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml environments`
Expected: PASS（原有 + 3 个新测试）

- [ ] **Step 5: cargo check + Commit**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
git add src-tauri/src/app/environments.rs src-tauri/src/lib.rs
git commit -m "feat: environments sync multi-user credentials with legacy migration"
```

---

### Task 8: 凭证 Tauri commands

**Files:**

- Modify: `src-tauri/src/app/env_credentials.rs`（追加 commands）
- Modify: `src-tauri/src/lib.rs`（invoke_handler 注册）

- [ ] **Step 1: 实现 commands（env_credentials.rs 追加）**

```rust
// ── Tauri commands ──

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_env_credentials_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
) -> Result<Vec<EnvCredentialRow>, String> {
    list_credentials(&state.db, &environment_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn add_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    username: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
    make_default: Option<bool>,
) -> Result<EnvCredentialRow, String> {
    add_credential(
        &state.db,
        &environment_id,
        username.trim(),
        &auth_type,
        private_key_path.as_deref(),
        password.as_deref(),
        make_default.unwrap_or(false),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    credential_id: String,
) -> Result<(), String> {
    delete_credential(&state.db, &environment_id, &credential_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_default_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    credential_id: String,
) -> Result<EnvCredentialRow, String> {
    set_default_credential(&state.db, &environment_id, &credential_id)
        .await
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 2: lib.rs invoke_handler 注册（`delete_environment_cmd` 之后）**

```rust
            app::env_credentials::list_env_credentials_cmd,
            app::env_credentials::add_env_credential_cmd,
            app::env_credentials::delete_env_credential_cmd,
            app::env_credentials::set_default_env_credential_cmd,
```

- [ ] **Step 3: 验证编译**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/app/env_credentials.rs src-tauri/src/lib.rs
git commit -m "feat: env credential tauri commands"
```

---

### Task 9: 前端凭证管理

**Files:**

- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`
- Modify: `src/components/environments/EnvironmentDialog.tsx`

- [ ] **Step 1: types.ts 追加类型（与 Rust EnvCredentialRow 对齐）**

```typescript
export interface EnvCredentialRow {
  id: string;
  environment_id: string;
  username: string;
  auth_type: "private_key" | "password";
  private_key_path: string | null;
  is_default: boolean;
  created_at: string;
}
```

- [ ] **Step 2: ipc.ts 追加绑定（文件末尾；顶部 import 类型列表补 `EnvCredentialRow`）**

```typescript
export async function listEnvCredentials(environmentId: string): Promise<EnvCredentialRow[]> {
  return invoke<EnvCredentialRow[]>("list_env_credentials_cmd", { environmentId });
}

export async function addEnvCredential(params: {
  environmentId: string;
  username: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
  makeDefault?: boolean;
}): Promise<EnvCredentialRow> {
  return invoke<EnvCredentialRow>("add_env_credential_cmd", {
    environmentId: params.environmentId,
    username: params.username,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
    makeDefault: params.makeDefault ?? false,
  });
}

export async function deleteEnvCredential(environmentId: string, credentialId: string): Promise<void> {
  return invoke<void>("delete_env_credential_cmd", { environmentId, credentialId });
}

export async function setDefaultEnvCredential(environmentId: string, credentialId: string): Promise<EnvCredentialRow> {
  return invoke<EnvCredentialRow>("set_default_env_credential_cmd", {
    environmentId,
    credentialId,
  });
}
```

- [ ] **Step 3: EnvironmentDialog 增加凭证区（仅编辑模式显示）**

3a. import 增加：

```typescript
import { listEnvCredentials, addEnvCredential, deleteEnvCredential, setDefaultEnvCredential } from "@/lib/ipc";
import type { EnvCredentialRow } from "@/lib/types";
```

3b. 组件内 state（`formError` 之后）：

```typescript
  const [creds, setCreds] = useState<EnvCredentialRow[]>([]);
  const [credForm, setCredForm] = useState({
    username: "",
    authType: "password" as "private_key" | "password",
    privateKeyPath: "",
    password: "",
    makeDefault: false,
  });
  const [credError, setCredError] = useState<string | null>(null);
  const [credBusy, setCredBusy] = useState(false);
```

3c. `useEffect([open, editing])` 内 `if (open)` 分支追加（`setFormError(null)` 之后）：

```typescript
      setCreds([]);
      setCredError(null);
      setCredForm({ username: "", authType: "password", privateKeyPath: "", password: "", makeDefault: false });
      if (editing) {
        listEnvCredentials(editing.id).then(setCreds).catch(() => setCreds([]));
      }
```

3d. 凭证操作 handlers（`handleTest` 之后）：

```typescript
  const handleAddCred = async () => {
    if (!editing) return;
    if (!credForm.username.trim()) {
      setCredError("用户名不能为空");
      return;
    }
    if (credForm.authType === "private_key" && !credForm.privateKeyPath.trim()) {
      setCredError("私钥认证需要填写私钥路径");
      return;
    }
    setCredBusy(true);
    setCredError(null);
    try {
      await addEnvCredential({
        environmentId: editing.id,
        username: credForm.username.trim(),
        authType: credForm.authType,
        privateKeyPath: credForm.authType === "private_key" ? credForm.privateKeyPath.trim() : null,
        password: credForm.password || null,
        makeDefault: credForm.makeDefault,
      });
      setCreds(await listEnvCredentials(editing.id));
      setCredForm({ username: "", authType: "password", privateKeyPath: "", password: "", makeDefault: false });
    } catch (e) {
      setCredError(String(e));
    } finally {
      setCredBusy(false);
    }
  };

  const handleDeleteCred = async (cred: EnvCredentialRow) => {
    if (!editing) return;
    try {
      await deleteEnvCredential(editing.id, cred.id);
      setCreds(await listEnvCredentials(editing.id));
    } catch (e) {
      setCredError(String(e));
    }
  };

  const handleSetDefaultCred = async (cred: EnvCredentialRow) => {
    if (!editing) return;
    try {
      await setDefaultEnvCredential(editing.id, cred.id);
      setCreds(await listEnvCredentials(editing.id));
    } catch (e) {
      setCredError(String(e));
    }
  };
```

3e. JSX：在 `testResult` 条件块之前插入（密钥字段 Field 之后）：

```tsx
          {editing && (
            <div className="pt-2 border-t border-border space-y-2">
              <p className="text-xs text-muted-foreground">
                多用户凭证：目标 JVM 以其他用户运行时（arthas attach 需要同用户），为该环境录入对应用户的
                SSH 凭证。默认凭证即日常连接使用的用户。
              </p>
              {creds.length > 0 && (
                <ul className="space-y-1">
                  {creds.map((cred) => (
                    <li
                      key={cred.id}
                      className="flex items-center gap-2 text-xs px-3 py-1.5 rounded-md border border-border bg-surface-2"
                    >
                      <span className="font-mono">{cred.username}</span>
                      <span className="text-muted-foreground">
                        {cred.auth_type === "private_key" ? "私钥" : "密码"}
                      </span>
                      {cred.is_default && (
                        <span className="px-1.5 py-0.5 rounded bg-accent/15 text-accent text-[10px]">默认</span>
                      )}
                      <span className="flex-1" />
                      {!cred.is_default && (
                        <button
                          onClick={() => handleSetDefaultCred(cred)}
                          className="text-muted-foreground hover:text-foreground cursor-pointer"
                        >
                          设为默认
                        </button>
                      )}
                      {!cred.is_default && (
                        <button
                          onClick={() => handleDeleteCred(cred)}
                          className="text-muted-foreground hover:text-destructive cursor-pointer"
                        >
                          删除
                        </button>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              <div className="flex gap-2">
                <input
                  type="text"
                  aria-label="凭证用户名"
                  placeholder="用户名（如 svcapp）"
                  value={credForm.username}
                  onChange={(e) => setCredForm({ ...credForm, username: e.target.value })}
                  className={`${inputCls} flex-1`}
                />
                <select
                  aria-label="凭证认证方式"
                  value={credForm.authType}
                  onChange={(e) =>
                    setCredForm({ ...credForm, authType: e.target.value as "private_key" | "password" })
                  }
                  className={`${inputCls} w-28 cursor-pointer`}
                >
                  <option value="password">密码</option>
                  <option value="private_key">私钥</option>
                </select>
              </div>
              {credForm.authType === "private_key" && (
                <input
                  type="text"
                  aria-label="凭证私钥路径"
                  placeholder="私钥路径（~/.ssh/...）"
                  value={credForm.privateKeyPath}
                  onChange={(e) => setCredForm({ ...credForm, privateKeyPath: e.target.value })}
                  className={inputCls}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
              )}
              <div className="flex gap-2 items-center">
                <input
                  type="password"
                  aria-label="凭证密钥"
                  placeholder={credForm.authType === "private_key" ? "私钥口令（可选）" : "密码"}
                  value={credForm.password}
                  onChange={(e) => setCredForm({ ...credForm, password: e.target.value })}
                  className={`${inputCls} flex-1`}
                />
                <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap">
                  <input
                    type="checkbox"
                    checked={credForm.makeDefault}
                    onChange={(e) => setCredForm({ ...credForm, makeDefault: e.target.checked })}
                  />
                  设为默认
                </label>
                <button
                  onClick={handleAddCred}
                  disabled={credBusy}
                  className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50 whitespace-nowrap"
                >
                  添加凭证
                </button>
              </div>
              {credError && <p className="text-xs text-destructive break-words">{credError}</p>}
            </div>
          )}
```

- [ ] **Step 4: 类型检查**

Run: `pnpm typecheck`
Expected: 通过

- [ ] **Step 5: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts src/components/environments/EnvironmentDialog.tsx
git commit -m "feat: environment multi-user credential management UI"
```

---

### Task 10: ArthasPackage（artifactory 下发）

**Files:**

- Create: `src-tauri/src/provision/arthas.rs`
- Modify: `src-tauri/src/provision/mod.rs`（加 `pub mod arthas;`）
- Modify: `src-tauri/src/provision/jdk.rs`（`run_remote`、`try_remote_download` 改 `pub(crate)`）

- [ ] **Step 1: 写失败测试（arthas.rs 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arthas_home() {
        assert_eq!(arthas_home(), "/tmp/friday-tools/arthas-4.3.5");
    }

    #[test]
    fn test_download_url() {
        assert_eq!(
            arthas_download_url("https://artifactory.example.com/artifactory/tools"),
            "https://artifactory.example.com/artifactory/tools/arthas/arthas-bin-4.3.5.zip",
        );
        // 尾部斜杠容忍
        assert_eq!(
            arthas_download_url("https://a.example.com/b/"),
            "https://a.example.com/b/arthas/arthas-bin-4.3.5.zip",
        );
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas_home`
Expected: 编译失败（模块不存在）

- [ ] **Step 3: 实现 arthas.rs**

```rust
use crate::provision::package::{
    emit_progress, ProvisionContext, ProvisionError, ProvisionResult, ToolPackage,
};
use crate::provision::jdk::{run_remote, try_remote_download, JvmProbe, REMOTE_TOOLS_DIR};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

/// arthas 版本（官方 arthas-bin.zip 对应版本；升级只改这里 + artifactory 放包）
pub const ARTHAS_VERSION: &str = "4.3.5";
/// 进度事件携带的工具名：与 MCP 工具名一致（前端按 tool.name 匹配工具卡片）
pub const ARTHAS_TOOL_NAME: &str = "arthas_open";

pub fn arthas_home() -> String {
    format!("{REMOTE_TOOLS_DIR}/arthas-{ARTHAS_VERSION}")
}

pub fn arthas_download_url(base: &str) -> String {
    format!("{}/arthas/arthas-bin-{ARTHAS_VERSION}.zip", base.trim_end_matches('/'))
}

pub struct ArthasPackage;

#[async_trait]
impl ToolPackage for ArthasPackage {
    fn name(&self) -> &str {
        "arthas"
    }

    /// arthas 包与目标 JVM 版本无关，无需探测
    async fn probe(
        &self,
        _ctx: &ProvisionContext,
        _java_bin: &str,
    ) -> Result<JvmProbe, ProvisionError> {
        Ok(JvmProbe {
            openjdk_version: String::new(),
            bisheng_version: String::new(),
            arch: String::new(),
        })
    }

    async fn ensure(
        &self,
        ctx: &ProvisionContext,
        _java_bin: &str,
    ) -> Result<ProvisionResult, ProvisionError> {
        let start = std::time::Instant::now();
        let home = arthas_home();

        // 1. 远端缓存检查
        emit_progress(ctx, ARTHAS_TOOL_NAME, "check_cache", &format!("checking {home}/arthas-boot.jar"));
        let check = run_remote(
            ctx,
            &format!("mkdir -p {REMOTE_TOOLS_DIR} && test -f {home}/arthas-boot.jar"),
            Duration::from_secs(ctx.timeouts.probe),
            "check_cache",
        )
        .await?;
        if check.exit_code == 0 {
            return Ok(ProvisionResult {
                tool: "arthas".to_string(),
                cached: true,
                java_version: String::new(),
                bisheng_version: String::new(),
                arch: String::new(),
                tool_home: home,
                bins: HashMap::new(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }

        // 2. 下载（通道 A：目标自拉；通道 B：本地下载 + SFTP 上传）
        let url = arthas_download_url(&ctx.artifactory_base_url);
        let remote_zip = format!("{REMOTE_TOOLS_DIR}/arthas-bin-{ARTHAS_VERSION}.zip");
        emit_progress(ctx, ARTHAS_TOOL_NAME, "download", "channel A: remote curl/wget");
        if let Err(a_err) = try_remote_download(ctx, &url, &remote_zip).await {
            tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, error = %a_err, "channel A failed, falling back to channel B");
            emit_progress(ctx, ARTHAS_TOOL_NAME, "download", "channel B: local download + sftp upload");
            let local = crate::provision::transfer::download_to_cache(&url, &ctx.cache_dir)
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                })?;
            if let Err(e) = crate::provision::transfer::validate_download(&local, 5 * 1024 * 1024) {
                tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, path = %local.display(), error = %e, "local cached arthas zip failed validation, removing");
                let _ = std::fs::remove_file(&local);
                return Err(ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                });
            }
            ctx.channel
                .upload(&local, &remote_zip)
                .await
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "upload", e.to_string())
                })?;
        }

        // 3. 解压（unzip → python3 兜底）+ 顶层目录扁平化 + 清理
        //    find arthas-boot.jar 所在目录作为包根，兼容 zip 内有无顶层目录两种布局
        emit_progress(ctx, ARTHAS_TOOL_NAME, "extract", &format!("extracting arthas-bin-{ARTHAS_VERSION}.zip"));
        let extract_cmd = format!(
            "cd {REMOTE_TOOLS_DIR} && rm -rf arthas-tmp-{ARTHAS_VERSION} arthas-{ARTHAS_VERSION} && \
             mkdir arthas-tmp-{ARTHAS_VERSION} && \
             if command -v unzip >/dev/null 2>&1; then \
               unzip -q -o arthas-bin-{ARTHAS_VERSION}.zip -d arthas-tmp-{ARTHAS_VERSION}/; \
             elif command -v python3 >/dev/null 2>&1; then \
               python3 -m zipfile -e arthas-bin-{ARTHAS_VERSION}.zip arthas-tmp-{ARTHAS_VERSION}/; \
             else \
               echo 'neither unzip nor python3 available' >&2; exit 3; \
             fi && \
             d=$(dirname \"$(find arthas-tmp-{ARTHAS_VERSION} -name arthas-boot.jar | head -1)\") && \
             [ -n \"$d\" ] && mv \"$d\" arthas-{ARTHAS_VERSION} && \
             rm -rf arthas-tmp-{ARTHAS_VERSION} arthas-bin-{ARTHAS_VERSION}.zip && \
             chmod -R 755 arthas-{ARTHAS_VERSION}"
        );
        let extract = run_remote(ctx, &extract_cmd, Duration::from_secs(ctx.timeouts.extract), "extract").await?;
        if extract.exit_code != 0 {
            // 失败清理半成品（后台执行）
            let ch = ctx.channel.clone();
            let cleanup = format!(
                "rm -rf {REMOTE_TOOLS_DIR}/arthas-tmp-{ARTHAS_VERSION} {REMOTE_TOOLS_DIR}/arthas-{ARTHAS_VERSION}"
            );
            tokio::spawn(async move {
                let _ = ch.run(&cleanup).await;
            });
            return Err(ProvisionError::new(
                "provision_failed",
                "extract",
                format!(
                    "unzip failed (exit {}): {} —— 目标机需要 unzip 或 python3 之一",
                    extract.exit_code, extract.stderr
                ),
            ));
        }

        // 4. 验证
        emit_progress(ctx, ARTHAS_TOOL_NAME, "verify", &format!("verifying {home}/arthas-boot.jar"));
        let verify = run_remote(
            ctx,
            &format!("test -f {home}/arthas-boot.jar"),
            Duration::from_secs(ctx.timeouts.verify),
            "verify",
        )
        .await?;
        if verify.exit_code != 0 {
            return Err(ProvisionError::new(
                "provision_failed",
                "verify",
                format!("arthas-boot.jar missing after extract; check artifactory package layout ({url})"),
            ));
        }

        Ok(ProvisionResult {
            tool: "arthas".to_string(),
            cached: false,
            java_version: String::new(),
            bisheng_version: String::new(),
            arch: String::new(),
            tool_home: home,
            bins: HashMap::new(),
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }
}
```

jdk.rs 修改：`async fn run_remote(` → `pub(crate) async fn run_remote(`；`async fn try_remote_download(` → `pub(crate) async fn try_remote_download(`。

provision/mod.rs 加 `pub mod arthas;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas`
Expected: PASS（2 个纯函数测试）

- [ ] **Step 5: cargo check + Commit**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
git add src-tauri/src/provision/arthas.rs src-tauri/src/provision/mod.rs src-tauri/src/provision/jdk.rs
git commit -m "feat: arthas tool package provisioning"
```

---

### Task 11: attach 命令构造纯函数

**Files:**

- Create: `src-tauri/src/arthas/mod.rs`（本任务只声明 `pub mod attach;`，后续任务追加）
- Create: `src-tauri/src/arthas/attach.rs`（本任务只实现纯函数 + 常量；生产编排在 Task 14 追加）
- Modify: `src-tauri/src/lib.rs`（加 `mod arthas;`，按字母序放在 `mod app;` 之后）

- [ ] **Step 1: arthas/mod.rs**

```rust
pub mod attach;
```

- [ ] **Step 2: 写失败测试（attach.rs 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arthas_properties_content() {
        let content = arthas_properties_content(18563, "abc123");
        assert!(content.contains("arthas.mcpEndpoint=/mcp\n"));
        assert!(content.contains("arthas.telnetPort=-1\n"));
        assert!(content.contains("arthas.httpPort=18563\n"));
        assert!(content.contains("arthas.password=abc123\n"));
        // 无单引号/美元符（安全嵌入 shell 单引号）
        assert!(!content.contains('\''));
        assert!(!content.contains('$'));
    }

    #[test]
    fn test_check_user_command() {
        assert_eq!(check_user_command(123), "ps -o user= -p 123 2>/dev/null; echo '---'; id -un");
    }

    #[test]
    fn test_parse_user_check() {
        let (jvm, ssh) = parse_user_check("svcapp\n---\nopc\n").unwrap();
        assert_eq!(jvm, "svcapp");
        assert_eq!(ssh, "opc");
        // ps 输出带空白
        let (jvm, ssh) = parse_user_check("  svcapp \n---\n opc \n").unwrap();
        assert_eq!(jvm, "svcapp");
        assert_eq!(ssh, "opc");
    }

    #[test]
    fn test_parse_user_check_pid_gone() {
        assert!(parse_user_check("\n---\nopc\n").is_err());
    }

    #[test]
    fn test_find_free_port_command_and_parse() {
        let cmd = find_free_port_command(18563, 3);
        assert!(cmd.contains("seq 18563 18565"));
        assert_eq!(parse_free_port("18563\n").unwrap(), 18563);
        assert!(parse_free_port("none\n").is_err());
    }

    #[test]
    fn test_port_probe_command() {
        assert!(port_probe_command(8563).contains("/dev/tcp/127.0.0.1/8563"));
    }

    #[test]
    fn test_attach_command() {
        let cmd = attach_command("/tmp/friday-tools/jdk-21/bin/java", "/tmp/friday-tools/arthas-4.3.5", 123);
        assert!(cmd.contains("cd /tmp/friday-tools/arthas-4.3.5"));
        assert!(cmd.contains("nohup /tmp/friday-tools/jdk-21/bin/java -jar arthas-boot.jar --pid 123"));
        assert!(cmd.contains("< /dev/null"));
        assert!(cmd.contains("&"));
    }

    #[test]
    fn test_write_properties_command_quotes_content() {
        let content = arthas_properties_content(18563, "tok123");
        let cmd = write_properties_command("/tmp/friday-tools/arthas-4.3.5", &content);
        assert!(cmd.starts_with("printf '%s' 'arthas."));
        assert!(cmd.contains("> /tmp/friday-tools/arthas-4.3.5/arthas.properties"));
        assert!(cmd.contains("chmod 644 /tmp/friday-tools/arthas-4.3.5/arthas.properties"));
    }

    #[test]
    fn test_stop_command_contains_auth_and_payload() {
        let cmd = stop_command(18563, "tok123");
        assert!(cmd.contains("Authorization: Bearer tok123"));
        assert!(cmd.contains("http://127.0.0.1:18563/api"));
        assert!(cmd.contains("\"command\":\"stop\""));
    }

    #[test]
    fn test_generate_token_charset() {
        for _ in 0..10 {
            let t = generate_token();
            assert_eq!(t.len(), 32);
            assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
        }
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml attach`
Expected: 编译失败

- [ ] **Step 4: 实现 attach.rs 纯函数部分**

```rust
use crate::exec::ssh::shell_quote_single;

/// 远端 arthas HTTP 端口分配起点（顺序向上探测）
pub const ARTHAS_PORT_START: u16 = 18563;
pub const ARTHAS_PORT_CANDIDATES: u16 = 10;

/// arthas.properties 内容（MCP endpoint 开启 / telnet 关闭 / Friday 分配端口 / 随机 Bearer）。
/// 内容不含单引号/美元符，可安全嵌入 shell 单引号（见测试）。
pub fn arthas_properties_content(http_port: u16, token: &str) -> String {
    format!(
        "arthas.mcpEndpoint=/mcp\narthas.telnetPort=-1\narthas.httpPort={http_port}\narthas.password={token}\n"
    )
}

/// 用户对齐 pre-flight：目标进程属主 + 当前 SSH 用户
pub fn check_user_command(pid: i64) -> String {
    format!("ps -o user= -p {pid} 2>/dev/null; echo '---'; id -un")
}

/// 解析 check_user_command 输出 → (jvm_user, ssh_user)。
/// jvm_user 为空 = 进程不存在（或已被回收）。
pub fn parse_user_check(stdout: &str) -> Result<(String, String), String> {
    let mut parts = stdout.splitn(2, "---");
    let jvm_raw = parts.next().unwrap_or_default();
    let ssh_raw = parts.next().unwrap_or_default();
    let jvm_user = jvm_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default();
    let ssh_user = ssh_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default();
    if jvm_user.is_empty() {
        return Err("目标进程不存在或已退出（ps 无属主输出）".to_string());
    }
    if ssh_user.is_empty() {
        return Err("无法确定当前 SSH 用户（id -un 无输出）".to_string());
    }
    Ok((jvm_user.to_string(), ssh_user.to_string()))
}

/// 目标机端口占用探测（bash /dev/tcp）：busy = 可连（占用）；free = 连不上
pub fn port_probe_command(port: u16) -> String {
    format!(
        "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then exec 3>&- 3<&-; echo busy; else echo free; fi"
    )
}

/// 从 start 起找第一个空闲端口（探测命令，候选 count 个）
pub fn find_free_port_command(start: u16, count: u16) -> String {
    let end = start + count - 1;
    format!(
        "for p in $(seq {start} {end}); do \
         if (exec 3<>/dev/tcp/127.0.0.1/$p) 2>/dev/null; then exec 3>&- 3<&-; else echo $p; exit 0; fi; \
         done; echo none"
    )
}

/// 解析 find_free_port_command 输出
pub fn parse_free_port(stdout: &str) -> Result<u16, String> {
    let first = stdout.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first == "none" || first.is_empty() {
        return Err(format!(
            "端口 {ARTHAS_PORT_START}~{} 均被占用，请减少同机并发 attach 的 JVM 数或稍后重试",
            ARTHAS_PORT_START + ARTHAS_PORT_CANDIDATES - 1
        ));
    }
    first
        .parse::<u16>()
        .map_err(|_| format!("端口探测输出无法解析: {stdout:?}"))
}

/// 写 arthas.properties（内容经单引号转义；chmod 644 保证 jvm_user 可读）
pub fn write_properties_command(home: &str, content: &str) -> String {
    format!(
        "printf '%s' {} > {home}/arthas.properties && chmod 644 {home}/arthas.properties",
        shell_quote_single(content)
    )
}

/// attach 命令：cd 到 arthas home（arthas-boot 从当前目录读 arthas.properties），
/// nohup 后台驻留，stdin 接 /dev/null 防交互等待。java 为可执行文件完整路径（已做字符集校验）。
pub fn attach_command(java: &str, home: &str, pid: i64) -> String {
    format!(
        "cd {home} && nohup {java} -jar arthas-boot.jar --pid {pid} < /dev/null >> {home}/arthas-attach-{pid}.log 2>&1 & echo attach-started"
    )
}

/// HTTP stop（best-effort）：arthas HTTP API 执行 stop 命令，卸载 agent。
/// curl 缺失时 wget 兜底，再失败吞掉（stop 尽力而为）。
pub fn stop_command(port: u16, token: &str) -> String {
    format!(
        "curl -s -m 10 -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' \
         -d '{{\"action\":\"exec\",\"command\":\"stop\"}}' http://127.0.0.1:{port}/api \
         || wget -q -O /dev/null --header='Authorization: Bearer {token}' \
         --post-data='{{\"action\":\"exec\",\"command\":\"stop\"}}' http://127.0.0.1:{port}/api \
         || true"
    )
}

/// 生成 Bearer token（32 位十六进制，无 shell 特殊字符）
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml attach`
Expected: PASS（9 个测试）

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/arthas/mod.rs src-tauri/src/arthas/attach.rs src-tauri/src/lib.rs
git commit -m "feat: arthas attach command builders (pure functions)"
```

---

### Task 12: ArthasManager 类型 + McpArthasClient

**Files:**

- Create: `src-tauri/src/arthas/manager.rs`（本任务只写类型定义段；生命周期逻辑 Task 13）
- Create: `src-tauri/src/arthas/client.rs`
- Modify: `src-tauri/src/arthas/mod.rs`（追加 `pub mod manager; pub mod client;`）

- [ ] **Step 1: manager.rs 类型定义段（文件顶部；impl 部分 Task 13 追加）**

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// 同时保持 attach 的 JVM 会话上限（LRU 逐出，对齐 heap analyzer 的 MAX_OPEN_DUMPS）
pub const MAX_SESSIONS: usize = 3;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("attach 失败：{0}")]
    Attach(String),
    #[error("该 JVM 尚未 attach arthas")]
    NotOpen { attaching: bool },
    #[error("arthas 调用超时（{0}s）")]
    Timeout(u64),
    #[error("{0}")]
    Upstream(String),
    #[error("arthas 通道传输错误：{0}")]
    Transport(String),
}

/// 一次上游工具调用结果
#[derive(Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

/// arthas MCP client 抽象（测试注入 mock 的 seam，对齐 HeapAnalyzerClient）
#[async_trait]
pub trait ArthasClient: Send + Sync {
    /// Err = 传输层错误（通道死亡，调用方 invalidate 会话）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    async fn shutdown(&self);
}

/// attach 资源释放句柄：HTTP stop arthas + 拆隧道（尽力而为）
#[async_trait]
pub trait ArthasStopHandle: Send + Sync {
    async fn stop(&self);
}

pub struct AttachedSession {
    pub client: Arc<dyn ArthasClient>,
    pub stop_handle: Arc<dyn ArthasStopHandle>,
}

#[derive(Clone, Debug)]
pub struct AttachRequest {
    pub session_id: String,
    pub env_id: String,
    pub pid: i64,
    /// 目标机 java 可执行文件所在 JDK home 或 java 路径（arthas-boot 运行需要；默认 "java"）
    pub java_bin: String,
}

pub type AttachFactory = Arc<
    dyn Fn(AttachRequest) -> Pin<Box<dyn Future<Output = Result<AttachedSession, ManagerError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ArthasConfig {
    /// 距最后调用超过该时长且无 inflight → 自动 stop
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for ArthasConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}
```

- [ ] **Step 2: 实现 client.rs**

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use super::manager::{ArthasClient, CallOutcome};

/// rmcp Streamable HTTP 实现：连接远端 arthas MCP Server（经 SSH 隧道到达 127.0.0.1:local_port）。
/// Bearer token 即 arthas.password（Friday 生成、随 arthas.properties 下发）。
pub struct McpArthasClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 连接 + MCP 握手（30s 超时）
pub async fn connect_arthas_client(url: &str, token: &str) -> Result<McpArthasClient, String> {
    use rmcp::ServiceExt;

    let config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url)
        .auth_header(token);
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);

    let service = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .map_err(|_| format!("arthas MCP 握手超时（30s）: {url}"))?
        .map_err(|e| format!("arthas MCP 连接失败: {e}"))?;

    let peer = service.peer().clone();
    tracing::info!(url, "arthas mcp client connected");
    Ok(McpArthasClient {
        peer,
        service: tokio::sync::Mutex::new(Some(service)),
    })
}

#[async_trait]
impl ArthasClient for McpArthasClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        // rmcp 3.1.4：CallToolRequestParams 为 non_exhaustive，只能经 Default 构造
        // （对齐 analyzer client 的适配写法）
        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        } else {
            tracing::warn!(tool = %name, "non-object args passed to arthas client, treated as empty");
        }
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = Some(arguments);

        let result = self
            .peer
            .call_tool_once(params)
            .await
            .map_err(|e| format!("arthas MCP 调用失败: {e}"))?;
        // 一次性请求/响应：非 Complete 一律按传输层错误处理（调用方 invalidate 会话）
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            other => return Err(format!("arthas MCP 调用返回非最终结果: {other:?}")),
        };
        Ok(CallOutcome {
            text: crate::analyzer::client::extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn shutdown(&self) {
        if let Some(service) = self.service.lock().await.take() {
            match service.cancel().await {
                Ok(reason) => tracing::info!(reason = ?reason, "arthas mcp client shut down"),
                Err(e) => tracing::warn!(?e, "arthas mcp service cancel failed"),
            }
        }
    }
}
```

- [ ] **Step 3: mod.rs 更新**

```rust
pub mod attach;
pub mod client;
pub mod manager;
```

- [ ] **Step 4: 验证编译**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过（client 无独立单测：无真实 arthas server 时 connect 无法进行；集成冒烟见 Task 17）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/arthas/manager.rs src-tauri/src/arthas/client.rs src-tauri/src/arthas/mod.rs
git commit -m "feat: arthas manager types and mcp client (streamable http + bearer)"
```

---

### Task 13: ArthasManager（会话生命周期）

**Files:**

- Modify: `src-tauri/src/arthas/manager.rs`（追加状态机 + 方法 + 测试）

- [ ] **Step 1: 写失败测试（manager.rs 底部追加）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockClient {
        behavior: Arc<dyn Fn(&str) -> Result<CallOutcome, String> + Send + Sync>,
    }

    #[async_trait]
    impl ArthasClient for MockClient {
        async fn call_tool(&self, name: &str, _args: &Value) -> Result<CallOutcome, String> {
            (self.behavior)(name)
        }
        async fn shutdown(&self) {}
    }

    struct MockStop {
        stops: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ArthasStopHandle for MockStop {
        async fn stop(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn ok_client() -> Arc<dyn ArthasClient> {
        Arc::new(MockClient {
            behavior: Arc::new(|_| Ok(CallOutcome { text: "ok".into(), is_error: false })),
        })
    }

    /// 计数工厂：前 fail_first 次返回 Err，之后成功；记录总调用数
    struct CountingFactory {
        calls: Arc<AtomicUsize>,
        fail_first: usize,
    }

    impl CountingFactory {
        fn into_factory(self: Arc<Self>) -> AttachFactory {
            Arc::new(move |req| {
                let f = self.clone();
                Box::pin(async move {
                    let n = f.calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if n <= f.fail_first {
                        return Err(ManagerError::Attach(format!("mock attach failure #{n}")));
                    }
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    })
                    .map(|s| {
                        let _ = &req; // 引用 req 避免未使用告警
                        s
                    })
                })
            })
        }
    }

    fn always_ok_factory() -> AttachFactory {
        Arc::new(|_req| {
            Box::pin(async move {
                Ok(AttachedSession {
                    client: ok_client(),
                    stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                })
            })
        })
    }

    #[tokio::test]
    async fn test_open_then_query_roundtrip() {
        let factory = Arc::new(CountingFactory { calls: Arc::new(AtomicUsize::new(0)), fail_first: 0 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let out = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap();
        assert_eq!(out.text, "ok");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn test_open_dedupes_concurrent_opens() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_factory = calls.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let calls = calls_for_factory.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    })
                })
            }),
            ArthasConfig::default(),
        );
        let (a, b) = tokio::join!(
            mgr.open("sess-1", "env-1", 123, "java", 30),
            mgr.open("sess-2", "env-1", 123, "java", 30),
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "concurrent opens must dedupe to one attach");
    }

    #[tokio::test]
    async fn test_query_without_open_errors() {
        let factory = Arc::new(CountingFactory { calls: Arc::new(AtomicUsize::new(0)), fail_first: 0 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: false }));
    }

    #[tokio::test]
    async fn test_query_while_attaching_reports_attaching() {
        // 工厂被信号门控：open 进入 Attaching 后挂起，期间并发查询应报 attaching
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let gate_for_factory = gate.clone();
        let mgr = Arc::new(ArthasManager::new(
            Arc::new(move |_req| {
                let gate = gate_for_factory.clone();
                Box::pin(async move {
                    gate.acquire().await.unwrap();
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    })
                })
            }),
            ArthasConfig::default(),
        ));
        let mgr_for_task = mgr.clone();
        let open_task = tokio::spawn(async move {
            mgr_for_task.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        });
        // 等 attach 条目进入 Attaching
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: true }));
        gate.add_permits(1);
        open_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_transport_error_invalidates_session() {
        let failing_client: Arc<dyn ArthasClient> = Arc::new(MockClient {
            behavior: Arc::new(|_| Err("connection reset".to_string())),
        });
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let client = failing_client.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client,
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::Transport(_)));
        // 会话已移除：再查报 NotOpen
        let err2 = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err2, ManagerError::NotOpen { attaching: false }));
    }

    #[tokio::test]
    async fn test_open_failure_then_retry_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(CountingFactory { calls: calls.clone(), fail_first: 1 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        let err = mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap_err();
        assert!(matches!(err, ManagerError::Attach(_)));
        // 失败条目已清除：重试成功
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_close_stops_and_is_idempotent() {
        let stops = Arc::new(AtomicUsize::new(0));
        let stops_for_factory = stops.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let stops = stops_for_factory.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops }),
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        assert!(mgr.close("env-1", 123).await);
        assert!(!mgr.close("env-1", 123).await); // 幂等
        // stop 由后台 spawn：轮询断言最终恰好 1 次
        let mut waited = 0;
        while stops.load(Ordering::SeqCst) == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waited += 1;
        }
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_lru_eviction_at_capacity() {
        let mgr = ArthasManager::new(always_ok_factory(), ArthasConfig::default());
        mgr.open("s", "env-1", 1, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        mgr.open("s", "env-1", 2, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        mgr.open("s", "env-1", 3, "java", 30).await.unwrap();
        mgr.open("s", "env-1", 4, "java", 30).await.unwrap(); // 逐出 pid=1
        let err = mgr.query("env-1", 1, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { .. }));
        // 其余仍在
        mgr.query("env-1", 2, "dashboard", &json!({}), 10).await.unwrap();
        mgr.query("env-1", 3, "dashboard", &json!({}), 10).await.unwrap();
        mgr.query("env-1", 4, "dashboard", &json!({}), 10).await.unwrap();
    }

    #[tokio::test]
    async fn test_idle_reaper_stops_session() {
        let config = ArthasConfig {
            idle_timeout: std::time::Duration::from_millis(80),
            idle_tick: std::time::Duration::from_millis(30),
        };
        let stops = Arc::new(AtomicUsize::new(0));
        let stops_for_factory = stops.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let stops = stops_for_factory.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops }),
                    })
                })
            }),
            config,
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: false }));
        // stop 由 reaper 后台 spawn：轮询断言恰好 1 次
        let mut waited = 0;
        while stops.load(Ordering::SeqCst) == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waited += 1;
        }
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_query_timeout_returns_timeout() {
        struct SlowClient;
        #[async_trait]
        impl ArthasClient for SlowClient {
            async fn call_tool(&self, _n: &str, _a: &Value) -> Result<CallOutcome, String> {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Ok(CallOutcome { text: "late".into(), is_error: false })
            }
            async fn shutdown(&self) {}
        }
        let mgr = ArthasManager::new(
            Arc::new(|_req| {
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: Arc::new(SlowClient),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let err = mgr.query("env-1", 123, "watch", &json!({}), 1).await.unwrap_err();
        assert!(matches!(err, ManagerError::Timeout(_)));
    }

    #[tokio::test]
    async fn test_close_for_environment_removes_all() {
        let mgr = ArthasManager::new(always_ok_factory(), ArthasConfig::default());
        mgr.open("s", "env-1", 1, "java", 30).await.unwrap();
        mgr.open("s", "env-1", 2, "java", 30).await.unwrap();
        mgr.open("s", "env-2", 3, "java", 30).await.unwrap();
        mgr.close_for_environment("env-1").await;
        assert!(mgr.query("env-1", 1, "d", &json!({}), 5).await.is_err());
        assert!(mgr.query("env-1", 2, "d", &json!({}), 5).await.is_err());
        // 其他环境不受影响
        mgr.query("env-2", 3, "d", &json!({}), 5).await.unwrap();
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas::manager`
Expected: 编译失败（ArthasManager 不存在）

- [ ] **Step 3: 实现 manager.rs（类型定义段之后追加）**

```rust
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::watch;

/// attach 任务内部硬超时（工厂 future 兜底；调用方超时只是不再等待）
const ATTACH_TASK_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub enum ArthasPhase {
    Attaching,
    Ready,
    Failed { error: ManagerError },
}

struct ArthasEntry {
    phase_tx: watch::Sender<ArthasPhase>,
    client: Option<Arc<dyn ArthasClient>>,
    stop_handle: Option<Arc<dyn ArthasStopHandle>>,
    last_active: Instant,
    inflight: u32,
}

pub struct OpenOutcome {
    pub env_id: String,
    pub pid: i64,
    pub summary: String,
}

pub struct ArthasManager {
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    attach_factory: AttachFactory,
    config: ArthasConfig,
}

struct ManagerInner {
    sessions: HashMap<(String, i64), ArthasEntry>,
    reaper_spawned: bool,
}

/// 空闲回收判定（纯函数便于单测）
fn is_reapable(entry: &ArthasEntry, idle_timeout: Duration) -> bool {
    if entry.last_active.elapsed() <= idle_timeout {
        return false;
    }
    match *entry.phase_tx.borrow() {
        ArthasPhase::Ready => entry.inflight == 0,
        ArthasPhase::Failed { .. } => true, // 失败残留条目一并清理
        ArthasPhase::Attaching => false,    // attach 中的条目不回收（有 ATTACH_TASK_TIMEOUT 兜底）
    }
}

impl ArthasManager {
    pub fn new(attach_factory: AttachFactory, config: ArthasConfig) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(ManagerInner {
                sessions: HashMap::new(),
                reaper_spawned: false,
            })),
            attach_factory,
            config,
        }
    }

    /// attach arthas 到 (env_id, pid)。幂等：Ready 秒回；Attaching 等待合流；
    /// 失败条目即时清除（下次 open 重新走完整 attach）。
    pub async fn open(
        &self,
        session_id: &str,
        env_id: &str,
        pid: i64,
        java_bin: &str,
        timeout_secs: u64,
    ) -> Result<OpenOutcome, ManagerError> {
        let mut rx = {
            let mut inner = self.inner.lock().await;
            self.ensure_reaper(&mut inner);
            let key = (env_id.to_string(), pid);
            if let Some(entry) = inner.sessions.get_mut(&key) {
                if matches!(*entry.phase_tx.borrow(), ArthasPhase::Ready) {
                    entry.last_active = Instant::now();
                    return Ok(OpenOutcome {
                        env_id: env_id.to_string(),
                        pid,
                        summary: "arthas 已就绪（复用现有 attach）".to_string(),
                    });
                }
                entry.phase_tx.subscribe()
            } else {
                // LRU：满员时逐出最久未用的 Ready 条目
                while inner.sessions.len() >= MAX_SESSIONS {
                    let victim = lru_ready_victim(&inner.sessions);
                    let Some(victim) = victim else { break };
                    if let Some(entry) = inner.sessions.remove(&victim) {
                        if let Some(stop) = entry.stop_handle {
                            tracing::info!(env_id = %victim.0, pid = victim.1, "arthas session evicted (LRU)");
                            tokio::spawn(async move { stop.stop().await; });
                        }
                    }
                }
                let (tx, rx) = watch::channel(ArthasPhase::Attaching);
                inner.sessions.insert(
                    key.clone(),
                    ArthasEntry {
                        phase_tx: tx,
                        client: None,
                        stop_handle: None,
                        last_active: Instant::now(),
                        inflight: 0,
                    },
                );
                // spawn attach 任务（对齐 heap analyzer 的 run_open_task 模式）
                let inner_clone = self.inner.clone();
                let factory = self.attach_factory.clone();
                let req = AttachRequest {
                    session_id: session_id.to_string(),
                    env_id: env_id.to_string(),
                    pid,
                    java_bin: java_bin.to_string(),
                };
                tokio::spawn(async move {
                    run_attach_task(inner_clone, factory, req).await;
                });
                rx
            }
        };

        // 等待 phase 落定（调用方超时只是不再等待；任务继续跑满 ATTACH_TASK_TIMEOUT）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match rx.borrow().clone() {
                ArthasPhase::Ready => {
                    return Ok(OpenOutcome {
                        env_id: env_id.to_string(),
                        pid,
                        summary: "arthas 已就绪".to_string(),
                    });
                }
                ArthasPhase::Failed { error } => {
                    // 清除失败条目，让下次 open 走全新 attach
                    self.inner.lock().await.sessions.remove(&(env_id.to_string(), pid));
                    return Err(error);
                }
                ArthasPhase::Attaching => {}
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(ManagerError::Timeout(timeout_secs));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                // 超时
                Err(_) => return Err(ManagerError::Timeout(timeout_secs)),
                // 发送端随条目移除而 drop（等待期间被关闭/逐出）→ 引导重试
                Ok(Err(_)) => {
                    return Err(ManagerError::Attach(
                        "attach 会话已被回收，请重试 arthas_open".to_string(),
                    ));
                }
                // phase 变化，回到循环头重新读取
                Ok(Ok(())) => {}
            }
        }
    }

    /// 调用上游 arthas MCP 工具。传输错误 → invalidate 会话。
    pub async fn query(
        &self,
        env_id: &str,
        pid: i64,
        tool: &str,
        args: &Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        let client = {
            let mut inner = self.inner.lock().await;
            let key = (env_id.to_string(), pid);
            let Some(entry) = inner.sessions.get_mut(&key) else {
                return Err(ManagerError::NotOpen { attaching: false });
            };
            match *entry.phase_tx.borrow() {
                ArthasPhase::Ready => {
                    let client = entry
                        .client
                        .clone()
                        .ok_or(ManagerError::NotOpen { attaching: false })?;
                    entry.inflight += 1;
                    entry.last_active = Instant::now();
                    client
                }
                ArthasPhase::Attaching => return Err(ManagerError::NotOpen { attaching: true }),
                ArthasPhase::Failed { .. } => return Err(ManagerError::NotOpen { attaching: false }),
            }
        };

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            client.call_tool(tool, args),
        )
        .await;

        // inflight 回落 + touch（会话可能已被并发关闭，忽略即可）
        {
            let mut inner = self.inner.lock().await;
            if let Some(entry) = inner.sessions.get_mut(&(env_id.to_string(), pid)) {
                entry.inflight = entry.inflight.saturating_sub(1);
                entry.last_active = Instant::now();
            }
        }

        match result {
            Err(_) => Err(ManagerError::Timeout(timeout_secs)),
            Ok(Err(transport)) => {
                tracing::warn!(env_id, pid, tool, error = %transport, "arthas transport error, invalidating session");
                self.invalidate(env_id, pid).await;
                Err(ManagerError::Transport(transport))
            }
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    /// 显式关闭（arthas_close 工具）。返回是否原本处于打开状态。
    pub async fn close(&self, env_id: &str, pid: i64) -> bool {
        let entry = { self.inner.lock().await.sessions.remove(&(env_id.to_string(), pid)) };
        match entry {
            Some(e) => {
                if let Some(stop) = e.stop_handle {
                    tokio::spawn(async move { stop.stop().await; });
                }
                if let Some(client) = e.client {
                    tokio::spawn(async move { client.shutdown().await; });
                }
                tracing::info!(env_id, pid, "arthas session closed");
                true
            }
            None => false,
        }
    }

    /// 关闭某环境全部会话（环境删除联动）
    pub async fn close_for_environment(&self, env_id: &str) {
        let entries: Vec<ArthasEntry> = {
            let mut inner = self.inner.lock().await;
            let keys: Vec<(String, i64)> = inner
                .sessions
                .keys()
                .filter(|(e, _)| e == env_id)
                .cloned()
                .collect();
            keys.iter().filter_map(|k| inner.sessions.remove(k)).collect()
        };
        for e in entries {
            if let Some(stop) = e.stop_handle {
                tokio::spawn(async move { stop.stop().await; });
            }
            if let Some(client) = e.client {
                tokio::spawn(async move { client.shutdown().await; });
            }
        }
        if !entries.is_empty() {
            tracing::info!(env_id, count = entries.len(), "arthas sessions closed for environment");
        }
    }

    /// 传输错误 → 移除会话 + best-effort stop（下次 open 重新 attach）
    async fn invalidate(&self, env_id: &str, pid: i64) {
        let stop = {
            let mut inner = self.inner.lock().await;
            inner
                .sessions
                .remove(&(env_id.to_string(), pid))
                .and_then(|e| e.stop_handle)
        };
        if let Some(stop) = stop {
            tokio::spawn(async move { stop.stop().await; });
        }
    }

    /// reaper 只在首个 open 时 spawn 一次（构造在 async 上下文之外，不能 tokio::spawn）
    fn ensure_reaper(&self, inner: &mut ManagerInner) {
        if inner.reaper_spawned {
            return;
        }
        inner.reaper_spawned = true;
        let inner_clone = self.inner.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.idle_tick);
            loop {
                interval.tick().await;
                let stops: Vec<Arc<dyn ArthasStopHandle>> = {
                    let mut inner = inner_clone.lock().await;
                    let keys: Vec<(String, i64)> = inner
                        .sessions
                        .iter()
                        .filter(|(_, e)| is_reapable(e, config.idle_timeout))
                        .map(|(k, _)| k.clone())
                        .collect();
                    keys.iter()
                        .filter_map(|k| inner.sessions.remove(k))
                        .filter_map(|e| e.stop_handle)
                        .collect()
                };
                for stop in stops {
                    tracing::info!("arthas session idle, stopping");
                    tokio::spawn(async move { stop.stop().await; });
                }
            }
        });
    }
}

/// 找最久未访问的 Ready 条目 key（Attaching/Failed 不参与逐出）
fn lru_ready_victim(sessions: &HashMap<(String, i64), ArthasEntry>) -> Option<(String, i64)> {
    sessions
        .iter()
        .filter(|(_, e)| matches!(*e.phase_tx.borrow(), ArthasPhase::Ready))
        .min_by_key(|(_, e)| e.last_active)
        .map(|(k, _)| k.clone())
}

/// attach 任务：调工厂 → 落定 phase（attach 任务自身有硬超时兜底）
async fn run_attach_task(
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    factory: AttachFactory,
    req: AttachRequest,
) {
    let key = (req.env_id.clone(), req.pid);
    let result = tokio::time::timeout(
        Duration::from_secs(ATTACH_TASK_TIMEOUT_SECS),
        factory(req.clone()),
    )
    .await;
    let mut inner = inner.lock().await;
    let Some(entry) = inner.sessions.get_mut(&key) else { return };
    match result {
        Ok(Ok(attached)) => {
            entry.client = Some(attached.client);
            entry.stop_handle = Some(attached.stop_handle);
            entry.last_active = Instant::now();
            entry.phase_tx.send_replace(ArthasPhase::Ready);
        }
        Ok(Err(e)) => {
            tracing::warn!(env_id = %req.env_id, pid = req.pid, error = %e, "arthas attach failed");
            entry.phase_tx.send_replace(ArthasPhase::Failed { error: e });
        }
        Err(_) => {
            tracing::error!(env_id = %req.env_id, pid = req.pid, "arthas attach task timed out");
            entry.phase_tx.send_replace(ArthasPhase::Failed {
                error: ManagerError::Attach(format!(
                    "attach 超时（{ATTACH_TASK_TIMEOUT_SECS}s 硬超时）"
                )),
            });
        }
    }
}
```

注意实现细节：

- `open` 等待循环中 `rx.changed()` 出错（发送端 drop）的判断：先 timeout 包一层，再单独调一次 `rx.changed()` 判断 Err——如上代码所示。
- `Instant` 用 `std::time::Instant`（`use std::time::Instant;`，与 `Duration` 同源）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas::manager`
Expected: PASS（10 个测试）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/arthas/manager.rs
git commit -m "feat: arthas manager lifecycle (dedup/lru/idle-reaper/invalidate)"
```

---

### Task 14: 生产 attach 编排 + StopHandle

**Files:**

- Modify: `src-tauri/src/arthas/attach.rs`（追加生产编排）

- [ ] **Step 1: 实现（attach.rs 追加；imports 补充）**

imports（文件顶部补齐）：

```rust
use crate::app::events::{AppEvent, EventBus};
use crate::exec::channel::ExecChannel;
use crate::exec::pool::ExecChannelPool;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::manager::{ArthasClient, ArthasStopHandle, AttachFactory, AttachRequest, AttachedSession, ManagerError};
```

生产编排主体：

```rust
/// 生产 attach 依赖集
#[derive(Clone)]
pub struct AttachDeps {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    pub jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
    pub cache_dir: PathBuf,
    pub bus: EventBus,
}

pub fn production_attach_factory(deps: AttachDeps) -> AttachFactory {
    Arc::new(move |req| {
        let deps = deps.clone();
        Box::pin(attach_arthas(deps, req))
    })
}

async fn attach_arthas(deps: AttachDeps, req: AttachRequest) -> Result<AttachedSession, ManagerError> {
    let progress = |stage: &str, detail: String| {
        tracing::info!(session_id = %req.session_id, env_id = %req.env_id, stage, detail = %detail, "arthas attach progress");
        deps.bus.emit(
            &req.session_id,
            AppEvent::ProvisionProgress {
                session_id: req.session_id.clone(),
                tool: "arthas_open".to_string(),
                stage: stage.to_string(),
                detail,
            },
        );
    };

    // 0. 默认连接（连接池）
    let channel = get_default_channel(&deps, &req.env_id).await?;

    // 1. 确保 arthas 工具包（幂等，cached 快路径）
    progress("ensure_package", "确保 arthas 工具包".to_string());
    let pctx = provision_context(&deps, &req, channel.clone()).await?;
    let arthas_pkg = crate::provision::arthas::ArthasPackage;
    let arthas_result = arthas_pkg
        .ensure(&pctx, "java")
        .await
        .map_err(|e| ManagerError::Attach(format!("arthas 工具包下发失败: {}", e.message)))?;
    let arthas_home = arthas_result.tool_home;

    // 2. 解析 attach 用 java（JdkCache → PATH java → ensure JDK），返回可执行文件完整路径
    let java = resolve_attach_java(&deps, &req, &pctx).await?;

    // 3. 用户对齐 pre-flight
    progress("check_user", "检查目标 JVM 运行用户".to_string());
    let (jvm_user, ssh_user) = check_users(channel.as_ref(), req.pid).await?;
    let attach_exec: Box<dyn ExecChannel> = if jvm_user == ssh_user || ssh_user == "root" {
        Box::new(SharedChannel(channel.clone()))
    } else {
        progress(
            "check_user",
            format!("SSH 用户 {ssh_user} ≠ JVM 用户 {jvm_user}，使用 {jvm_user} 凭证临时连接"),
        );
        match crate::app::env_credentials::find_credential_by_username(&deps.db, &req.env_id, &jvm_user).await {
            Ok(Some(cred)) => Box::new(build_temp_transport(&deps, &req.env_id, &cred).await?),
            _ => {
                return Err(ManagerError::Attach(format!(
                    "目标 JVM 运行用户为 {jvm_user}，当前 SSH 用户为 {ssh_user} 且未录入 {jvm_user} 的凭证。\
                     请让用户在环境管理中为该环境添加用户 {jvm_user} 的凭证后重试"
                )))
            }
        }
    };

    // 4. 分配远端 HTTP 端口 + 写 arthas.properties（配置只走 properties，不传 CLI 端口参数）
    progress("allocate_port", "分配 arthas HTTP 端口".to_string());
    let port = find_free_remote_port(channel.as_ref()).await?;
    let token = generate_token();
    progress("write_config", format!("写入 arthas.properties（httpPort={port}）"));
    write_properties(channel.as_ref(), &arthas_home, &arthas_properties_content(port, &token)).await?;

    // 5. attach（nohup 后台驻留）
    progress("attach", format!("attach arthas 到 PID {}（java={java}）", req.pid));
    run_attach_command(attach_exec.as_ref(), &java, &arthas_home, req.pid).await?;

    // 6. 探活（端口可连 = arthas HTTP server 就绪）
    progress("probe", "等待 arthas HTTP 服务就绪".to_string());
    wait_http_ready(channel.as_ref(), port, std::time::Duration::from_secs(60)).await?;

    // 7. 隧道 + MCP 握手（失败要拆隧道）
    progress("tunnel", "建立 SSH 隧道".to_string());
    let lease = deps
        .tunnels
        .open(&req.env_id, "127.0.0.1", port)
        .await
        .map_err(|e| ManagerError::Attach(format!("SSH 隧道建立失败: {e}")))?;
    let url = format!("http://127.0.0.1:{}/mcp", lease.local_port);
    progress("handshake", format!("MCP 握手（{url}）"));
    let client: Arc<dyn ArthasClient> = match crate::arthas::client::connect_arthas_client(&url, &token).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            deps.tunnels.close(&req.env_id, "127.0.0.1", port).await;
            return Err(ManagerError::Attach(format!("arthas MCP 握手失败: {e}")));
        }
    };

    progress("ready", format!("arthas 就绪（远端端口 {port}，本地隧道端口 {}）", lease.local_port));
    let stop_handle: Arc<dyn ArthasStopHandle> = Arc::new(ProductionStopHandle {
        db: deps.db.clone(),
        exec_pool: deps.exec_pool.clone(),
        tunnels: deps.tunnels.clone(),
        env_id: req.env_id.clone(),
        remote_port: port,
        token,
        client: client.clone(),
    });
    Ok(AttachedSession { client, stop_handle })
}

/// Arc<dyn ExecChannel> → Box<dyn ExecChannel> 的轻量适配（避免 downcast）
struct SharedChannel(Arc<dyn ExecChannel>);

#[async_trait::async_trait]
impl ExecChannel for SharedChannel {
    async fn run(&self, cmd: &str) -> Result<crate::exec::channel::ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        self.0.run(cmd).await
    }
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.0.connect().await
    }
    async fn disconnect(&self) {
        self.0.disconnect().await;
    }
    async fn is_alive(&self) -> bool {
        self.0.is_alive().await
    }
}

/// best-effort stop：HTTP stop arthas（卸载 agent）+ 拆隧道 + 关 MCP client
struct ProductionStopHandle {
    db: sqlx::SqlitePool,
    exec_pool: Arc<Mutex<ExecChannelPool>>,
    tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    env_id: String,
    remote_port: u16,
    token: String,
    client: Arc<dyn ArthasClient>,
}

#[async_trait::async_trait]
impl ArthasStopHandle for ProductionStopHandle {
    async fn stop(&self) {
        // HTTP stop（尽力而为，失败仅告警——残留 agent 由用户 arthas_close 重试或目标机重启解决）
        if let Ok(channel) = get_default_channel_raw(&self.db, &self.exec_pool, &self.env_id).await {
            match run_with_timeout(channel.as_ref(), &stop_command(self.remote_port, &self.token), 15).await {
                Ok(_) => tracing::info!(env_id = %self.env_id, port = self.remote_port, "arthas stopped via http api"),
                Err(e) => tracing::warn!(env_id = %self.env_id, port = self.remote_port, error = %e, "arthas http stop failed (best-effort)"),
            }
        }
        self.tunnels.close(&self.env_id, "127.0.0.1", self.remote_port).await;
        self.client.shutdown().await;
    }
}

// ── 编排子步骤 ──

async fn get_default_channel(deps: &AttachDeps, env_id: &str) -> Result<Arc<dyn ExecChannel>, ManagerError> {
    get_default_channel_raw(&deps.db, &deps.exec_pool, env_id).await
}

async fn get_default_channel_raw(
    db: &sqlx::SqlitePool,
    exec_pool: &Arc<Mutex<ExecChannelPool>>,
    env_id: &str,
) -> Result<Arc<dyn ExecChannel>, ManagerError> {
    let mut pool = exec_pool.lock().await;
    pool.get_or_create(env_id, db)
        .await
        .map_err(|e| ManagerError::Attach(format!("SSH 连接失败: {e}")))
}

async fn provision_context(
    deps: &AttachDeps,
    req: &AttachRequest,
    channel: Arc<dyn ExecChannel>,
) -> Result<crate::provision::package::ProvisionContext, ManagerError> {
    let base = crate::app::settings::artifactory_base_url(&deps.db)
        .await
        .map_err(|e| ManagerError::Attach(format!("读取 Artifactory 设置失败: {e}")))?;
    if base.trim().is_empty() {
        return Err(ManagerError::Attach(
            "Artifactory 地址未配置，请在设置中配置后重试".to_string(),
        ));
    }
    Ok(crate::provision::package::ProvisionContext {
        session_id: req.session_id.clone(),
        env_id: req.env_id.clone(),
        channel,
        cache_dir: deps.cache_dir.clone(),
        artifactory_base_url: base,
        timeouts: crate::provision::package::StageTimeouts::default(),
        bus: deps.bus.clone(),
    })
}

/// attach 用 java 可执行文件解析：JdkCache → PATH java → ensure JDK（结果回写 JdkCache）。
/// 返回可执行文件完整路径（已做字符集校验，可安全嵌入 shell 命令）。
async fn resolve_attach_java(
    deps: &AttachDeps,
    req: &AttachRequest,
    pctx: &crate::provision::package::ProvisionContext,
) -> Result<String, ManagerError> {
    if let Some(layout) = deps.jdk_cache.get(&req.env_id).await {
        return Ok(format!("{}/bin/java", layout.tool_home));
    }
    // PATH 上有 java：直接用（JRE 也够跑 arthas-boot）
    if let Ok(out) = run_with_timeout(pctx.channel.as_ref(), "command -v java", 15).await {
        let java = out.stdout.trim().to_string();
        if out.exit_code == 0 && !java.is_empty() {
            // 字符集校验（防 shell 注入，与 ensure_tool 的 java_bin 同款规则）
            if crate::provision::jdk::validate_java_bin(&java).is_ok() {
                return Ok(java);
            }
            tracing::warn!(java = %java, "PATH java path failed charset validation, ignoring");
        }
    }
    // 兜底：ensure JDK（依赖 java_bin 参数指向可用 java；目标机无 java 时给 agent 可行动的错误）
    let jdk = crate::provision::jdk::JdkPackage;
    match jdk.ensure(pctx, &req.java_bin).await {
        Ok(result) => {
            deps.jdk_cache
                .set(
                    &req.env_id,
                    crate::tools::builtin::jvm::jdk_cache::JdkLayout {
                        tool_home: result.tool_home.clone(),
                        bins: result.bins.clone(),
                    },
                )
                .await;
            Ok(format!("{}/bin/java", result.tool_home))
        }
        Err(e) => Err(ManagerError::Attach(format!(
            "目标机找不到可用的 java（{}）。可用 run_command 确认目标服务的 java 路径后，\
             用 java_bin 参数重试 arthas_open。原始错误: {}",
            e.message, e.message
        ))),
    }
}

/// 用户对齐检查（jvm_user, ssh_user）
async fn check_users(
    channel: &dyn ExecChannel,
    pid: i64,
) -> Result<(String, String), ManagerError> {
    let out = run_with_timeout(channel, &check_user_command(pid), 15).await?;
    parse_user_check(&out.stdout).map_err(|e| ManagerError::Attach(format!("用户对齐检查失败: {e}; stderr: {}", out.stderr)))
}

/// 分配远端空闲端口（18563 起顺序探测）
async fn find_free_remote_port(channel: &dyn ExecChannel) -> Result<u16, ManagerError> {
    let cmd = find_free_port_command(ARTHAS_PORT_START, ARTHAS_PORT_CANDIDATES);
    let out = run_with_timeout(channel, &cmd, 20).await?;
    parse_free_port(&out.stdout).map_err(|e| ManagerError::Attach(e))
}

/// 写 arthas.properties（经默认连接执行；chmod 644 保证 jvm_user 可读）
async fn write_properties(
    channel: &dyn ExecChannel,
    home: &str,
    content: &str,
) -> Result<(), ManagerError> {
    let out = run_with_timeout(channel, &write_properties_command(home, content), 15).await?;
    if out.exit_code != 0 {
        return Err(ManagerError::Attach(format!(
            "写入 arthas.properties 失败（exit {}）: {}",
            out.exit_code, out.stderr
        )));
    }
    Ok(())
}

/// 执行 attach 命令（临时连接场景用后即断）
async fn run_attach_command(
    exec: &dyn ExecChannel,
    java: &str,
    arthas_home: &str,
    pid: i64,
) -> Result<(), ManagerError> {
    let out = run_with_timeout(exec, &attach_command(java, arthas_home, pid), 30).await?;
    if out.exit_code != 0 {
        return Err(ManagerError::Attach(format!(
            "arthas attach 命令失败（exit {}）: {}",
            out.exit_code, out.stderr
        )));
    }
    Ok(())
}

/// 探活循环：端口可连即认为 arthas HTTP server 就绪（bash /dev/tcp，无 curl 依赖）
async fn wait_http_ready(
    channel: &dyn ExecChannel,
    port: u16,
    budget: std::time::Duration,
) -> Result<(), ManagerError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let out = run_with_timeout(channel, &port_probe_command(port), 15)
            .await
            .map_err(|e| ManagerError::Attach(format!("arthas 探活失败: {e}")))?;
        if out.stdout.trim() == "busy" {
            tracing::info!(port, attempt, "arthas http server ready");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ManagerError::Attach(format!(
                "arthas HTTP 服务在 {}s 内未就绪（端口 {port}）。\
                 可能原因：attach 失败（用户权限/attach 机制被禁用）、目标 JVM 拒绝 attach。\
                 可用 run_command 查看 {ARTHAS_LOG_HINT} 日志",
                budget.as_secs()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// attach 日志位置提示（错误消息用）
const ARTHAS_LOG_HINT: &str = "arthas home 下 arthas-attach-<pid>.log";

/// 临时 attach 连接：jvm_user 凭证 → 独立 SshTransport（用后由调用方 disconnect）
async fn build_temp_transport(
    deps: &AttachDeps,
    env_id: &str,
    cred: &crate::app::env_credentials::EnvCredentialRow,
) -> Result<TempAttachTransport, ManagerError> {
    let env = crate::exec::pool::fetch_environment(&deps.db, env_id)
        .await
        .map_err(|e| ManagerError::Attach(format!("环境查询失败: {e}")))?;
    let secret = crate::app::credentials::load_cred_secret(env_id, &cred.id)
        .await
        .map_err(|e| ManagerError::Attach(format!("读取用户 {} 密钥失败: {e}", cred.username)))?;
    let auth = crate::exec::ssh::SshAuth::from_row(&cred.auth_type, cred.private_key_path.as_deref())
        .ok_or_else(|| ManagerError::Attach(format!("用户 {} 的认证配置无效", cred.username)))?;
    let transport = crate::exec::ssh::SshTransport::with_secret(
        env_id,
        env.host.as_deref().unwrap_or_default(),
        env.port.unwrap_or(22),
        &cred.username,
        auth,
        secret,
    );
    transport
        .connect()
        .await
        .map_err(|e| ManagerError::Attach(format!("以用户 {} 建立连接失败: {e}", cred.username)))?;
    Ok(TempAttachTransport { inner: transport })
}

/// 临时连接包装：Drop 时 fire-and-forget 断开（russh 无 async Drop，用 spawn）
struct TempAttachTransport {
    inner: crate::exec::ssh::SshTransport,
}

#[async_trait::async_trait]
impl ExecChannel for TempAttachTransport {
    async fn run(&self, cmd: &str) -> Result<crate::exec::channel::ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        self.inner.run(cmd).await
    }
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.connect().await
    }
    async fn disconnect(&self) {
        self.inner.disconnect().await;
    }
    async fn is_alive(&self) -> bool {
        self.inner.is_alive().await
    }
}

/// 统一的带超时远端执行（命令本身都应秒级返回）
async fn run_with_timeout(
    channel: &dyn ExecChannel,
    cmd: &str,
    secs: u64,
) -> Result<crate::exec::channel::ExecOutput, ManagerError> {
    match tokio::time::timeout(std::time::Duration::from_secs(secs), channel.run(cmd)).await {
        Err(_) => Err(ManagerError::Attach(format!("远端命令执行超时（{secs}s）: {cmd}"))),
        Ok(Err(e)) => Err(ManagerError::Attach(format!("远端命令执行失败: {e}（命令: {cmd}）"))),
        Ok(Ok(out)) => {
            if !out.stderr.trim().is_empty() {
                tracing::debug!(cmd, stderr = %out.stderr, "remote command stderr");
            }
            Ok(out)
        }
    }
}
```

注意：`run_with_timeout` 与各子步骤函数统一收 `&dyn ExecChannel`；`Arc<dyn ExecChannel>` 调用点传 `channel.as_ref()`（已在代码中体现）。`attach_command` 的 `java` 参数是可执行文件完整路径（来自 `resolve_attach_java`，已做字符集校验）。

- [ ] **Step 2: 单测（attach.rs tests 模块追加）**

Task 11 的纯函数测试已覆盖命令构造；生产编排无单测——依赖真实 SSH/arthas，靠 Task 17 集成冒烟。本步骤无新增测试，跳过。

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml attach`
Expected: PASS（Task 11 的 9 个测试；生产编排编译通过即可）

- [ ] **Step 4: cargo check + Commit**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
git add src-tauri/src/arthas/attach.rs
git commit -m "feat: production arthas attach orchestration with stop handle"
```

---

### Task 15: arthas_* 工具层（27 个工具）

**Files:**

- Create: `src-tauri/src/tools/builtin/arthas/mod.rs`
- Create: `src-tauri/src/tools/builtin/arthas/mapping.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`（加 `pub mod arthas;`）

- [ ] **Step 1: 写失败测试（mapping.rs 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_upstream_names() {
        assert_eq!(upstream_name(ArthasToolKind::Dashboard), "dashboard");
        assert_eq!(upstream_name(ArthasToolKind::Watch), "watch");
        assert_eq!(upstream_name(ArthasToolKind::Vmoption), "vmoption");
        assert_eq!(upstream_name(ArthasToolKind::Profiler), "profiler");
    }

    #[test]
    fn test_build_args_passthrough() {
        let args = build_args(ArthasToolKind::Watch, &json!({"args": {"classPattern": "com.foo.Bar"}})).unwrap();
        assert_eq!(args, json!({"classPattern": "com.foo.Bar"}));
        // 缺省 args → 空对象
        let args = build_args(ArthasToolKind::Dashboard, &json!({})).unwrap();
        assert_eq!(args, json!({}));
    }

    #[test]
    fn test_build_args_rejects_non_object() {
        let err = build_args(ArthasToolKind::Watch, &json!({"args": "watch com.foo.Bar m"}));
        assert!(err.is_err());
    }

    #[test]
    fn test_thread_interrupt_filtered() {
        let err = build_args(ArthasToolKind::Thread, &json!({"args": {"id": 1, "interrupt": true}}));
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("interrupt"));
        // 不带 interrupt 的正常透传
        let ok = build_args(ArthasToolKind::Thread, &json!({"args": {"id": 1}})).unwrap();
        assert_eq!(ok, json!({"id": 1}));
    }

    #[test]
    fn test_vmtool_interrupt_filtered() {
        let err = build_args(ArthasToolKind::Vmtool, &json!({"args": {"action": "interrupt", "threadId": 3}}));
        assert!(err.is_err());
        let ok = build_args(ArthasToolKind::Vmtool, &json!({"args": {"action": "forceGc"}})).unwrap();
        assert_eq!(ok, json!({"action": "forceGc"}));
    }
}
```

- [ ] **Step 2: 实现 mapping.rs**

```rust
use serde_json::Value;

/// Friday 工具 kind（Open/Close 之外 25 个代理到 arthas MCP 同名工具）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArthasToolKind {
    Open,
    Close,
    Dashboard,
    Jvm,
    Memory,
    Sysenv,
    Perfcounter,
    Sc,
    Sm,
    Jad,
    Classloader,
    Getstatic,
    Mbean,
    Dump,
    Thread,
    Viewfile,
    Options,
    Watch,
    Trace,
    Stack,
    Monitor,
    Tt,
    Ognl,
    Vmtool,
    Sysprop,
    Vmoption,
    Profiler,
}

/// 上游 arthas MCP 工具名（与 arthas 命令同名）
pub fn upstream_name(kind: ArthasToolKind) -> &'static str {
    match kind {
        ArthasToolKind::Open | ArthasToolKind::Close => "",
        ArthasToolKind::Dashboard => "dashboard",
        ArthasToolKind::Jvm => "jvm",
        ArthasToolKind::Memory => "memory",
        ArthasToolKind::Sysenv => "sysenv",
        ArthasToolKind::Perfcounter => "perfcounter",
        ArthasToolKind::Sc => "sc",
        ArthasToolKind::Sm => "sm",
        ArthasToolKind::Jad => "jad",
        ArthasToolKind::Classloader => "classloader",
        ArthasToolKind::Getstatic => "getstatic",
        ArthasToolKind::Mbean => "mbean",
        ArthasToolKind::Dump => "dump",
        ArthasToolKind::Thread => "thread",
        ArthasToolKind::Viewfile => "viewfile",
        ArthasToolKind::Options => "options",
        ArthasToolKind::Watch => "watch",
        ArthasToolKind::Trace => "trace",
        ArthasToolKind::Stack => "stack",
        ArthasToolKind::Monitor => "monitor",
        ArthasToolKind::Tt => "tt",
        ArthasToolKind::Ognl => "ognl",
        ArthasToolKind::Vmtool => "vmtool",
        ArthasToolKind::Sysprop => "sysprop",
        ArthasToolKind::Vmoption => "vmoption",
        ArthasToolKind::Profiler => "profiler",
    }
}

/// Friday 工具参数 → 上游 arthas MCP 工具参数（args 对象原样透传）。
/// 子操作过滤：thread/vmtool 拒绝 interrupt。
pub fn build_args(kind: ArthasToolKind, args: &Value) -> Result<Value, String> {
    match kind {
        ArthasToolKind::Open | ArthasToolKind::Close => {
            Err("内部错误：open/close 不经 mapping".to_string())
        }
        ArthasToolKind::Thread => {
            let upstream = passthrough(args)?;
            if upstream.get("interrupt").is_some() {
                return Err(
                    "thread 的 interrupt 子操作不被支持（会打断目标线程）；支持查看线程列表/栈/状态"
                        .to_string(),
                );
            }
            Ok(upstream)
        }
        ArthasToolKind::Vmtool => {
            let upstream = passthrough(args)?;
            if upstream.get("action").and_then(|v| v.as_str()) == Some("interrupt") {
                return Err(
                    "vmtool 的 interrupt 子操作不被支持；支持 forceGc / getInstances".to_string()
                );
            }
            Ok(upstream)
        }
        _ => passthrough(args),
    }
}

fn passthrough(args: &Value) -> Result<Value, String> {
    match args.get("args") {
        None | Some(Value::Null) => Ok(serde_json::json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(_) => Err("args 必须是对象（arthas 命令参数的字段形式）".to_string()),
    }
}
```

- [ ] **Step 3: 运行 mapping 测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas::mapping`
Expected: PASS（5 个测试）

- [ ] **Step 4: 实现 mod.rs（handler + 27 个 ToolDef）**

```rust
pub mod mapping;

use crate::arthas::manager::{ArthasManager, ManagerError};
use crate::tools::builtin::jvm::core::{clamp_or, error_output, parse_pid};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use mapping::{ArthasToolKind, build_args, upstream_name};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const OPEN: Timeouts = (120, 300);
const CLOSE: Timeouts = (30, 60);
const FAST: Timeouts = (30, 60);
const STREAM: Timeouts = (120, 600);
const PROFILER: Timeouts = (300, 1800);

pub struct ArthasToolHandler {
    pub manager: Arc<ArthasManager>,
    pub db: sqlx::SqlitePool,
    pub artifacts_dir: PathBuf,
    pub kind: ArthasToolKind,
    pub timeouts: Timeouts,
}

#[async_trait]
impl ToolHandler for ArthasToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(pid) = args
            .get("pid")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_pid(&serde_json::json!(s)))
        else {
            return error_output("invalid_params", "missing required parameter: pid（正整数字符串）");
        };
        // 按名称查环境
        let env = match crate::app::environments::find_by_name(&self.db, environment).await {
            Ok(Some(env)) => env,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；\
                         若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("lookup_failed", &format!("查询环境失败: {e}")),
        };
        let timeout_secs = clamp_or(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            self.timeouts.0,
            self.timeouts.1,
        );
        let start = Instant::now();
        let label = format!("{}/{}", environment, pid);
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, env_id = %env.id, pid, "arthas tool executing");

        match self.kind {
            ArthasToolKind::Open => {
                let java_bin = args.get("java_bin").and_then(|v| v.as_str()).unwrap_or("java");
                match self.manager.open(&ctx.session_id, &env.id, pid as i64, java_bin, timeout_secs).await {
                    Ok(outcome) => render(&ctx.session_id, &self.artifacts_dir, "arthas_open", &label, &outcome.summary, start, true).await,
                    Err(e) => self.manager_error_output(e, &ctx.session_id, "arthas_open", &label, start).await,
                }
            }
            ArthasToolKind::Close => {
                let was_open = self.manager.close(&env.id, pid as i64).await;
                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "tool": "arthas_close",
                        "environment": environment,
                        "pid": pid,
                        "was_open": was_open,
                    }),
                    raw_stdout: None,
                }
            }
            kind => {
                let upstream = upstream_name(kind);
                let upstream_args = match build_args(kind, &args) {
                    Ok(v) => v,
                    Err(e) => return error_output("invalid_params", &e),
                };
                match self.manager.query(&env.id, pid as i64, upstream, &upstream_args, timeout_secs).await {
                    Ok(outcome) => {
                        render(&ctx.session_id, &self.artifacts_dir, upstream, &label, &outcome.text, start, !outcome.is_error).await
                    }
                    Err(e) => self.manager_error_output(e, &ctx.session_id, upstream, &label, start).await,
                }
            }
        }
    }
}

impl ArthasToolHandler {
    /// ManagerError → 结构化错误输出（对齐 heap 工具的 manager_error_output 模式）
    async fn manager_error_output(
        &self,
        e: ManagerError,
        session_id: &str,
        upstream_tool: &str,
        label: &str,
        start: Instant,
    ) -> ToolOutput {
        match e {
            ManagerError::Attach(m) => error_output("arthas_attach_failed", &m),
            ManagerError::NotOpen { attaching } => {
                if attaching {
                    error_output("arthas_not_open", "该 JVM 正在 attach 中（首次需下发工具包/建隧道，约 10-60s）。请稍候后重试。")
                } else {
                    error_output("arthas_not_open", "该 JVM 尚未 attach arthas。请先调用 arthas_open(environment, pid)。")
                }
            }
            ManagerError::Timeout(t) => error_output(
                "arthas_timeout",
                &format!("arthas 调用超时（{t}s）。会话未受影响，可加大 timeout_secs 重试。"),
            ),
            ManagerError::Transport(m) => error_output(
                "arthas_transport",
                &format!("arthas 通道传输错误：{m}。会话已失效，请重新调用 arthas_open。"),
            ),
            ManagerError::Upstream(text) => {
                render(session_id, &self.artifacts_dir, upstream_tool, label, &text, start, false).await
            }
        }
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    label: &str,
    text: &str,
    start: Instant,
    success: bool,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("arthas-{}.md", uuid::Uuid::new_v4()));
    let full = format!(
        "--- tool: {upstream_tool} ---\n--- target: {label} ---\n--- full output ---\n{text}\n"
    );
    let artifact = tokio::fs::write(&artifact_path, full).await.ok().map(|_| artifact_path);
    let mut data = serde_json::json!({
        "tool": upstream_tool,
        "target": label,
        "elapsed_ms": elapsed_ms,
        "output": body,
        "truncated": truncated,
    });
    if let Some(p) = artifact {
        data["full_output_path"] = serde_json::json!(p.display().to_string());
    }
    ToolOutput { success, data, raw_stdout: None }
}

/// 注册全部 27 个 arthas 工具
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    manager: Arc<ArthasManager>,
    db: sqlx::SqlitePool,
    artifacts_dir: PathBuf,
) {
    // (name, description, risk, timeouts, kind)
    let defs: Vec<(&str, &str, RiskLevel, Timeouts, ArthasToolKind)> = vec![
        ("arthas_open",
         "attach arthas 到目标 JVM 并建立诊断通道（幂等，已 attach 秒回）。首次自动下发 arthas 工具包（需 Artifactory 已配置）；SSH 用户与 JVM 用户不一致时需要已录入对应用户凭证。加载 agent 侵入目标 JVM，需确认。",
         RiskLevel::Low, OPEN, ArthasToolKind::Open),
        ("arthas_close",
         "停止目标 JVM 上的 arthas agent 并释放通道（卸载字节码增强与 agent，幂等）。诊断完成后调用，或留给空闲自动回收。",
         RiskLevel::ReadOnly, CLOSE, ArthasToolKind::Close),
        ("arthas_dashboard",
         "实时 JVM 面板：线程/内存/GC/运行环境概览。args: {interval?, num?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Dashboard),
        ("arthas_jvm",
         "JVM 详细运行时信息（类加载/编译器/GC/线程/系统属性概览）。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Jvm),
        ("arthas_memory",
         "JVM 内存使用：各分代/元空间/堆外。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Memory),
        ("arthas_sysenv",
         "查看目标 JVM 进程环境变量。args: {variable?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sysenv),
        ("arthas_perfcounter",
         "JVM Perf Counter 性能计数器信息。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Perfcounter),
        ("arthas_sc",
         "搜索 JVM 已加载类，可看类详情（类加载器/父类/接口/字段）。args: {classPattern, details?, fields?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sc),
        ("arthas_sm",
         "搜索已加载类的方法信息（签名/参数/注解）。args: {classPattern, methodPattern?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Sm),
        ("arthas_jad",
         "反编译指定已加载类（JVM 实际运行的字节码 → Java 源码）。args: {classPattern, methodName?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Jad),
        ("arthas_classloader",
         "ClassLoader 诊断：统计/继承树/加载的 URL。args: {}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Classloader),
        ("arthas_getstatic",
         "查看类的静态字段值。args: {className, field?, classloader?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Getstatic),
        ("arthas_mbean",
         "查看/监控 MBean 属性信息。args: {name, attribute?, interval?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Mbean),
        ("arthas_dump",
         "导出指定类（已加载字节码）到目标机 arthas-output 目录，配合 arthas_viewfile/文件传输查看。args: {classPattern}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Dump),
        ("arthas_thread",
         "线程信息与堆栈：定位 BLOCKED/死锁/最忙线程。不支持 interrupt 子操作。args: {id?, state?, topN?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Thread),
        ("arthas_viewfile",
         "查看目标机 arthas-output 目录内文件（profiler 火焰图等）。args: {file, cursor?, offset?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Viewfile),
        ("arthas_options",
         "查看 arthas 全局开关选项。args: {option?, value?}",
         RiskLevel::ReadOnly, FAST, ArthasToolKind::Options),
        ("arthas_watch",
         "观察方法执行的入参/返回值/异常（实时，字节码增强）。args: {classPattern, methodPattern, express?, condition?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Watch),
        ("arthas_trace",
         "追踪方法内部调用链与各级耗时，定位慢调用。args: {classPattern, methodPattern, condition?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Trace),
        ("arthas_stack",
         "输出方法被调用的调用路径（谁调用了它）。args: {classPattern, methodPattern}",
         RiskLevel::Low, STREAM, ArthasToolKind::Stack),
        ("arthas_monitor",
         "监控方法调用统计：次数/成功率/平均 RT（周期采样）。args: {classPattern, methodPattern, interval?}",
         RiskLevel::Low, STREAM, ArthasToolKind::Monitor),
        ("arthas_tt",
         "方法执行数据时空隧道：记录每次调用的入参/返回，可事后查看/重放。args: {classPattern, methodPattern, ...}",
         RiskLevel::Low, STREAM, ArthasToolKind::Tt),
        ("arthas_ognl",
         "执行 OGNL 表达式（可调用方法/读写字段，能力很强，需确认）。args: {express, classloader?}",
         RiskLevel::Low, FAST, ArthasToolKind::Ognl),
        ("arthas_vmtool",
         "VM 工具集：forceGc（强制 GC）/ getInstances（获取类实例）。不支持 interrupt。args: {action, className?, limit?}",
         RiskLevel::Low, FAST, ArthasToolKind::Vmtool),
        ("arthas_sysprop",
         "查看/修改目标 JVM 系统属性（可写，需确认）。args: {name?, value?}",
         RiskLevel::Low, FAST, ArthasToolKind::Sysprop),
        ("arthas_vmoption",
         "查看/更新目标 JVM VM 选项（可写，需确认）。args: {name?, value?}",
         RiskLevel::Low, FAST, ArthasToolKind::Vmoption),
        ("arthas_profiler",
         "async-profiler 采样：CPU/alloc/lock，输出火焰图（到 arthas-output 目录，用 arthas_viewfile 或文件传输查看）。采样周期长，注意 timeout_secs。args: {action, event?, duration?}",
         RiskLevel::Low, PROFILER, ArthasToolKind::Profiler),
    ];
    for (name, desc, risk, timeouts, kind) in defs {
        registry.register(arthas_tool_def(name, desc, risk, timeouts, kind, manager.clone(), db.clone(), artifacts_dir.clone()));
    }
}

fn arthas_tool_def(
    name: &str,
    description: &str,
    risk: RiskLevel,
    timeouts: Timeouts,
    kind: ArthasToolKind,
    manager: Arc<ArthasManager>,
    db: sqlx::SqlitePool,
    artifacts_dir: PathBuf,
) -> ToolDef {
    let mut props = serde_json::json!({
        "environment": { "type": "string", "description": "目标环境名（来自 list_environments）" },
        "pid": { "type": "string", "description": "目标 JVM 进程号（来自 list_processes）" },
        "timeout_secs": { "type": "integer", "description": format!("超时秒数，默认 {}，最大 {}", timeouts.0, timeouts.1) },
    });
    if !matches!(kind, ArthasToolKind::Open | ArthasToolKind::Close) {
        props["args"] = serde_json::json!({
            "type": "object",
            "description": "arthas 命令参数对象（字段与 arthas 命令选项一致，原样透传给 arthas）"
        });
    }
    if matches!(kind, ArthasToolKind::Open) {
        props["java_bin"] = serde_json::json!({
            "type": "string",
            "description": "目标机 java 可执行文件路径（默认 java；目标机 PATH 无 java 时需指定）"
        });
    }
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": props,
            "required": ["environment", "pid"],
        }),
        risk_level: risk,
        needs_channel: false,
        handler: Arc::new(ArthasToolHandler {
            manager,
            db,
            artifacts_dir,
            kind,
            timeouts,
        }),
    }
}
```

并在 `src-tauri/src/tools/builtin/mod.rs` 加 `pub mod arthas;`。

注：`parse_pid` 收 `&Value`（返回 `Option<u32>`，做正整数字符串校验防 shell 注入——pid 会拼进远端命令），上面 `json!(s)` 包一层即满足签名。

- [ ] **Step 5: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas`
Expected: 编译通过；mapping 测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tools/builtin/arthas/mod.rs src-tauri/src/tools/builtin/arthas/mapping.rs src-tauri/src/tools/builtin/mod.rs
git commit -m "feat: arthas_* tool layer (27 tools, risk-stratified)"
```

---

### Task 16: 装配 + prompt 指引 + 环境删除联动 + 文档

**Files:**

- Modify: `src-tauri/src/lib.rs`（AppState、TunnelManager/ArthasManager 构造、工具注册、删除联动）
- Modify: `src-tauri/src/app/environments.rs`（delete_environment_cmd 加 arthas/tunnel 清理）
- Modify: `src-tauri/src/agent/prompt.rs`（TOOL_GUIDANCE + 系统提示）
- Modify: `AGENTS.md`、`docs/architecture/overview.md`

- [ ] **Step 1: lib.rs 装配**

1a. `AppState` 增加字段（`analyzer` 之后）：

```rust
    pub arthas: Arc<crate::arthas::manager::ArthasManager>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
```

1b. setup 内构造（`exec_pool` 创建之后、`transfer_manager` 之前）：

```rust
            // SSH 隧道（direct-tcpip 本地转发）：arthas MCP 通路，后续 JMX 等复用
            let tunnels = Arc::new(crate::exec::tunnel::TunnelManager::new(pool.clone()));
```

1c. arthas manager 构造（`jdk_cache`/`jvm_core` 构造之后）：

```rust
            // arthas 动态诊断：attach 编排依赖 jdk_cache / 连接池 / 隧道 / artifactory
            let attach_deps = crate::arthas::attach::AttachDeps {
                db: pool.clone(),
                exec_pool: exec_pool.clone(),
                tunnels: tunnels.clone(),
                jdk_cache: jdk_cache.clone(),
                cache_dir: paths.cache_dir(),
                bus: EventBus::new(handle.clone()),
            };
            let arthas_manager = Arc::new(crate::arthas::manager::ArthasManager::new(
                crate::arthas::attach::production_attach_factory(attach_deps),
                crate::arthas::manager::ArthasConfig::default(),
            ));
```

1d. 工具注册（`heap::register_all` 之后）：

```rust
            crate::tools::builtin::arthas::register_all(
                &mut tool_registry,
                arthas_manager.clone(),
                pool.clone(),
                paths.artifacts_dir(),
            );
```

1e. `AppState` 初始化补字段：

```rust
                arthas: arthas_manager,
                tunnels,
```

- [ ] **Step 2: delete_environment_cmd 联动（environments.rs）**

在 `exec_pool.disconnect(&id)` 之后、凭证清理之前插入：

```rust
    // 停掉该环境的 arthas 会话并拆除隧道（agent 卸载 best-effort）
    state.arthas.close_for_environment(&id).await;
    state.tunnels.close_all_for_env(&id).await;
```

- [ ] **Step 3: prompt.rs 更新**

3a. `FRIDAY_SYSTEM_PROMPT` 能力段（line 15 附近）替换为：

```rust
- 已集成 JVM 诊断工具（jstat/jcmd 封装）：GC 统计、线程转储、堆信息、类直方图、堆转储等；arthas 动态诊断（watch/trace/jad 等）；日志分析等能力后续扩展。
```

3b. `TOOL_GUIDANCE` 增加条目（堆快照分析条目之后）：

```rust
- arthas 动态诊断（attach 到运行中的 JVM）：list_processes 找 PID → arthas_open(environment, pid)（首次自动下发 arthas 包并 attach，需确认；已 attach 秒回）→ arthas_* 工具诊断（dashboard / thread / sc / sm / jad / watch / trace / stack / monitor / tt / ognl / vmtool / memory / jvm / sysprop / vmoption / profiler 等；args 对象的字段与 arthas 命令参数一致）→ 完成后 arthas_close 或留给空闲自动回收。注意：堆快照走 jvm_heap_dump（不用 arthas 的 heapdump）；arthas_open 报「运行用户不一致且未录入凭证」时，引导用户在环境管理中为该环境添加对应 JVM 用户的凭证后重试；arthas_not_open 报「正在 attach」时稍候重试即可。
```

- [ ] **Step 4: cargo check + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过；全部测试 PASS

- [ ] **Step 5: 文档同步**

5a. `AGENTS.md` 已实现功能列表追加（堆快照分析条目之后）：

```markdown
- **Arthas 动态诊断**：arthas_open / arthas_close + 25 个 arthas_* 代理工具（dashboard / thread / watch / trace / sc / jad / ognl 等，精选诊断集，剔除 redefine 热更新类）。Friday 作为 MCP client（rmcp streamable-http + Bearer）经 SSH direct-tcpip 隧道（`exec/tunnel.rs`，通用基础设施）连目标机 arthas 4.x 内置 MCP Server；arthas 包经 artifactory 统一下发（`provision/arthas.rs`）；ArthasManager 管理会话（并发去重、LRU 3、空闲 15min 回收、传输错误 invalidate）；attach 用户对齐（SSH 用户 ≠ JVM 用户时用对应用户凭证建临时连接）；环境多用户凭证管理（`env_credentials` 表 + 环境编辑弹窗，默认凭证即日常 SSH 用户）
```

5b. `docs/architecture/overview.md` 工具层说明（第 77-81 行 `结构化封装` 列表）更新：把 `arthas/读日志/读dump 后续批次` 中的 arthas 移出，改为：

```markdown
- 结构化封装（首批 JVM 工具已落地：
  list_processes / jvm_gc_stats / jvm_thread_dump
  / jvm_heap_info / jvm_vm_info / jvm_class_histogram
  / jvm_heap_dump；堆快照分析 heap_* 系列（MAT 引擎，
  自动预热）已落地；arthas 动态诊断 arthas_* 系列
  （官方 MCP Server 对接，SSH 隧道代理）已落地；读日志/读dump 后续批次）
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/app/environments.rs src-tauri/src/agent/prompt.rs AGENTS.md docs/architecture/overview.md
git commit -m "feat: wire arthas integration into app state, prompts and docs"
```

---

### Task 17: 全量验证 + 集成冒烟（手工）

- [ ] **Step 1: 全量验证命令**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
pnpm typecheck
```

Expected: 三项全部通过。

- [ ] **Step 2: 冒烟清单（需要一台可 SSH 的测试机 + 目标 JVM + artifactory 配置；手工执行，不阻塞交付）**

1. `pnpm tauri dev` 启动；环境面板添加环境（默认凭证）；编辑环境添加第二个用户凭证（若测试机 JVM 用户与 SSH 用户不同）
2. artifactory 放置 `arthas/arthas-bin-4.3.5.zip`（官方 release 的 arthas-bin.zip 重命名）
3. 对话：让 agent 诊断目标 JVM → 观察 `arthas_open` 确认弹窗（Low 风险）→ 进度事件（ensure_package / check_user / attach / probe / tunnel / handshake）→ `arthas_dashboard` 返回数据
4. 验证 arthas.properties 生效：目标机 `cat /tmp/friday-tools/arthas-4.3.5/arthas.properties`
5. 验证空闲回收：15 分钟不调用 → 日志出现 `arthas session idle, stopping` → 目标机 arthas 卸载（`ps` 无 arthas-boot 进程）
6. 验证 stop 后重开：`arthas_close` → `arthas_open` 重新走完整 attach
7. **实现期验证点落地**（spec 遗留）：
   - nohup 驻留形态下 attach 是否稳定（若目标机 arthas-boot 退出导致 HTTP 掉线，改用 `--batch` + 验证 agent 是否驻留）
   - arthas-boot 是否读取当前目录 arthas.properties（若未生效，加 `--arthas-home` 或 `-Darthas.home` 旗标，更新 `attach_command` 并补测试）
   - rmcp streamable-http-client 与 arthas MCP STREAMABLE 模式的会话兼容性（SSE keep-alive、404 重连）

- [ ] **Step 3: 最终提交（如有冒烟修复）**

```bash
git add -A
git commit -m "fix: arthas integration smoke test fixes"
```

---

## Self-Review 记录

- **Spec 覆盖**：通路架构（Task 2/3/12）、工具面 27 个与风险分级（Task 15）、arthas 下发（Task 10）、生命周期（Task 13）、用户对齐 + 多用户凭证（Task 4-7/9/14）、错误处理（Task 13/14/15 错误映射）、隧道基础设施（Task 3）、前端（Task 9）、TOOL_GUIDANCE（Task 16）、文档（Task 16）、验证（Task 17）——spec 各节均有对应任务
- **与 spec 的一处偏差**：spec 决策 A6 说 Bearer token「存 OS keychain」——计划改为 per-attach 内存态（token 随每次 attach 重新生成并写入 arthas.properties；Friday 重启后 manager 会话全失，重新 attach 生成新 token，持久化无意义）。简化无功能损失
- **占位符扫描**：无 TBD/TODO；重复性代码（27 个工具定义）以完整数据表给出
- **类型一致性**：`AttachFactory`/`AttachRequest`/`AttachedSession`/`ArthasStopHandle`/`CallOutcome` 在 Task 12 定义、Task 13/14/15 使用一致；`TunnelLease`/`TunnelManager` Task 3 定义、Task 14 使用；`EnvCredentialRow` Task 4 定义、Task 7/8/9/14 使用一致；`attach_command` 统一收 java 可执行文件路径（Task 11 定义 = Task 14 使用）
- **已修复的自审问题**：manager open() 等待循环的双 `changed()` 竞态；`run_with_timeout` 的 trait object 调用点；`resolve_attach_java` 返回值语义统一为可执行文件路径
- **已知实现期不确定点**（已列入 Task 17 冒烟清单）：russh direct-tcpip 精确签名、rmcp `from_config` 可用性、arthas-boot 读 properties 的路径行为、`--arthas-home` 旗标

