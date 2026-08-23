# 工具系统框架 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the MCP-based tool system framework that connects Agent to remote diagnostic capabilities, with tool registry, risk interception, session routing, and ExecChannel integration.

**Architecture:** In-process MCP server (rmcp SDK) with SSE transport, served via hyper. Tool registry with risk-level dispatch. Session routing via `session_id` tool parameter (auto-injected by server). Risk interception via oneshot channels with 120s timeout.

**Tech Stack:** Rust, rmcp SDK v3, hyper, tokio, Tauri 2, SQLite (sqlx), React/TypeScript frontend

**Spec:** `docs/superpowers/specs/2026-08-23-tool-system-design.md`

---

## File Structure

```
src-tauri/src/
├── tools/
│   ├── mod.rs                  # unchanged
│   ├── registry.rs             # REWRITE: ToolDef, ToolHandler, ToolContext, ToolOutput, ToolRegistry
│   ├── risk.rs                 # unchanged
│   ├── confirm.rs              # NEW: ConfirmRegistry, PendingConfirm, ConfirmResult
│   └── builtin/
│       └── mod.rs              # REWRITE: EchoHandler
├── mcp/                        # NEW module
│   ├── mod.rs                  # module declarations
│   ├── server.rs               # FridayMcpServer (impl ServerHandler)
│   ├── config.rs               # opencode config auto-merge
│   └── transport.rs            # hyper serve StreamableHttpService
├── exec/
│   ├── channel.rs              # unchanged
│   ├── pool.rs                 # NEW: ExecChannelPool
│   ├── ssh.rs                  # MODIFY: todo!() → Err
│   ├── k8s.rs                  # MODIFY: todo!() → Err
│   └── mod.rs                  # MODIFY: add pub mod pool
├── app/
│   ├── lifecycle.rs            # MODIFY: confirm_tool_cmd, list_tools_cmd, stop cleanup
│   ├── events.rs               # MODIFY: ConfirmRequired add confirm_id
│   └── ...
├── agent/
│   ├── prompt.rs               # MODIFY: build_prompt add session_id
│   ├── spawn.rs                # MODIFY: pass session_id to build_prompt
│   └── ...
├── infra/
│   └── db.rs                   # MODIFY: add environment_id migration
└── lib.rs                      # MODIFY: AppState, setup(), register commands

src-tauri/migrations/
└── 0007_environment_link.sql   # NEW

src/lib/
├── ipc.ts                      # MODIFY: confirmTool params, add listTools
└── types.ts                    # MODIFY: ConfirmRequired add confirm_id, add ToolInfo
```

---

## Task 1: DB Migration — environment_id

**Files:**
- Create: `src-tauri/migrations/0007_environment_link.sql`
- Modify: `src-tauri/src/infra/db.rs`

- [ ] **Step 1: Create migration SQL file**

Create `src-tauri/migrations/0007_environment_link.sql`:

```sql
-- Add environment_id to sessions for linking to environments table.
-- Uses add_column_if_not_exists in db.rs since SQLite ALTER TABLE lacks IF NOT EXISTS.
```

(The actual migration is done via `add_column_if_not_exists` in `db.rs`, matching the existing pattern for `agent_session_id`, `title`, etc. The SQL file is a placeholder for documentation.)

- [ ] **Step 2: Add migration call to db.rs**

In `src-tauri/src/infra/db.rs`, after the `add_column_if_not_exists(&pool, "sessions", "language", "TEXT").await?;` line (line 23), add:

```rust
    add_column_if_not_exists(&pool, "sessions", "environment_id", "TEXT").await?;
```

Also add the include_str after schema6:

```rust
    let _schema7 = include_str!("../../migrations/0007_environment_link.sql");
```

- [ ] **Step 3: Write the failing test**

Add to `src-tauri/src/infra/db.rs` tests module:

```rust
    #[tokio::test]
    async fn test_db_init_adds_environment_id_column() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='environment_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 1);
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_adds_environment_id_column`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/migrations/0007_environment_link.sql src-tauri/src/infra/db.rs
git commit -m "feat: add environment_id column to sessions table"
```

---

## Task 2: Tool Registry Refactor

**Files:**
- Rewrite: `src-tauri/src/tools/registry.rs`

- [ ] **Step 1: Write the failing tests**

Create the new `src-tauri/src/tools/registry.rs` with tests first. Write the full file:

```rust
use super::risk::RiskLevel;
use crate::exec::channel::ExecChannel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub data: serde_json::Value,
    pub raw_stdout: Option<String>,
}

pub struct ToolContext {
    pub session_id: String,
    pub channel: Arc<dyn ExecChannel>,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput;
}

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub risk_level: RiskLevel,
    pub handler: Arc<dyn ToolHandler>,
}

pub struct ToolRegistry {
    tools: HashMap<String, ToolDef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, def: ToolDef) {
        self.tools.insert(def.name.clone(), def);
    }

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    pub fn list(&self) -> Vec<&ToolDef> {
        self.tools.values().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHandler;

    #[async_trait]
    impl ToolHandler for DummyHandler {
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput {
                success: true,
                data: serde_json::json!({"result": "ok"}),
                raw_stdout: None,
            }
        }
    }

    fn make_tool_def(name: &str, risk: RiskLevel) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: format!("Test tool {}", name),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                }
            }),
            risk_level: risk,
            handler: Arc::new(DummyHandler),
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));

        assert!(registry.get("jstat").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_list_returns_all_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));
        registry.register(make_tool_def("arthas_trace", RiskLevel::Low));

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_empty_registry() {
        let registry = ToolRegistry::new();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_register_overwrites_same_name() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));
        registry.register(make_tool_def("jstat", RiskLevel::High));

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(registry.get("jstat").unwrap().risk_level, RiskLevel::High);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::registry`
Expected: PASS (4 tests)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tools/registry.rs
git commit -m "feat: refactor ToolRegistry — ToolDef, ToolHandler, ToolContext, ToolOutput"
```

---

## Task 3: friday_echo Test Tool

**Files:**
- Rewrite: `src-tauri/src/tools/builtin/mod.rs`

- [ ] **Step 1: Write the EchoHandler with tests**

Rewrite `src-tauri/src/tools/builtin/mod.rs`:

```rust
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub struct EchoHandler;

#[async_trait]
impl ToolHandler for EchoHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        ToolOutput {
            success: true,
            data: serde_json::json!({
                "echo": args,
                "session_id": ctx.session_id,
            }),
            raw_stdout: None,
        }
    }
}

pub fn echo_tool_def() -> ToolDef {
    ToolDef {
        name: "friday_echo".to_string(),
        description: "Echo test tool. Returns the arguments and session_id. Used for verifying tool system connectivity.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Message to echo back"
                }
            },
            "required": ["message"]
        }),
        risk_level: RiskLevel::ReadOnly,
        handler: Arc::new(EchoHandler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct MockChannel;

    #[async_trait]
    impl ExecChannel for MockChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
    }

    #[tokio::test]
    async fn test_echo_handler_returns_args_and_session_id() {
        let handler = EchoHandler;
        let channel: Arc<dyn ExecChannel> = Arc::new(MockChannel);
        let ctx = ToolContext {
            session_id: "test-session-123".to_string(),
            channel,
        };
        let args = serde_json::json!({"message": "hello", "session_id": "test-session-123"});

        let output = handler.execute(args, &ctx).await;

        assert!(output.success);
        assert_eq!(output.data["session_id"], "test-session-123");
        assert_eq!(output.data["echo"]["message"], "hello");
    }

    #[test]
    fn test_echo_tool_def_has_correct_metadata() {
        let def = echo_tool_def();

        assert_eq!(def.name, "friday_echo");
        assert_eq!(def.risk_level, RiskLevel::ReadOnly);
        assert!(def.description.contains("echo"));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin`
Expected: PASS (2 tests)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/tools/builtin/mod.rs
git commit -m "feat: add friday_echo test tool"
```

---

## Task 4: ConfirmRegistry

**Files:**
- Create: `src-tauri/src/tools/confirm.rs`
- Modify: `src-tauri/src/tools/mod.rs`

- [ ] **Step 1: Write the ConfirmRegistry with tests**

Create `src-tauri/src/tools/confirm.rs`:

```rust
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmResult {
    Confirmed,
    Cancelled,
}

struct PendingConfirm {
    session_id: String,
    tx: oneshot::Sender<ConfirmResult>,
}

pub struct ConfirmRegistry {
    pending: std::collections::HashMap<String, PendingConfirm>,
}

impl ConfirmRegistry {
    pub fn new() -> Self {
        Self {
            pending: std::collections::HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        confirm_id: String,
        session_id: String,
        tx: oneshot::Sender<ConfirmResult>,
    ) {
        self.pending.insert(
            confirm_id,
            PendingConfirm {
                session_id,
                tx,
            },
        );
    }

    pub fn resolve(&mut self, confirm_id: &str) -> Option<oneshot::Sender<ConfirmResult>> {
        self.pending.remove(confirm_id).map(|pc| pc.tx)
    }

    pub fn cancel_for_session(&mut self, session_id: &str) -> usize {
        let mut to_remove = Vec::new();
        for (id, pc) in &self.pending {
            if pc.session_id == session_id {
                to_remove.push(id.clone());
            }
        }
        let count = to_remove.len();
        for id in to_remove {
            if let Some(pc) = self.pending.remove(&id) {
                let _ = pc.tx.send(ConfirmResult::Cancelled);
            }
        }
        count
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

impl Default for ConfirmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_resolve() {
        let mut registry = ConfirmRegistry::new();
        let (tx, rx) = oneshot::channel();
        registry.insert("c1".to_string(), "s1".to_string(), tx);

        let resolved_tx = registry.resolve("c1");
        assert!(resolved_tx.is_some());

        resolved_tx.unwrap().send(ConfirmResult::Confirmed).unwrap();
        let result = rx.await.unwrap();
        assert_eq!(result, ConfirmResult::Confirmed);
    }

    #[tokio::test]
    async fn test_resolve_nonexistent_returns_none() {
        let mut registry = ConfirmRegistry::new();
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_cancel_for_session_sends_cancelled() {
        let mut registry = ConfirmRegistry::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        registry.insert("c1".to_string(), "s1".to_string(), tx1);
        registry.insert("c2".to_string(), "s2".to_string(), tx2);

        let count = registry.cancel_for_session("s1");
        assert_eq!(count, 1);

        let result = rx1.await.unwrap();
        assert_eq!(result, ConfirmResult::Cancelled);

        // s2 should still be pending
        assert_eq!(registry.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_cancel_for_session_multiple_pending() {
        let mut registry = ConfirmRegistry::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        registry.insert("c1".to_string(), "s1".to_string(), tx1);
        registry.insert("c2".to_string(), "s1".to_string(), tx2);

        let count = registry.cancel_for_session("s1");
        assert_eq!(count, 2);

        assert_eq!(rx1.await.unwrap(), ConfirmResult::Cancelled);
        assert_eq!(rx2.await.unwrap(), ConfirmResult::Cancelled);
        assert_eq!(registry.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_cancel_for_nonexistent_session_returns_zero() {
        let mut registry = ConfirmRegistry::new();
        let (tx, _rx) = oneshot::channel();
        registry.insert("c1".to_string(), "s1".to_string(), tx);

        let count = registry.cancel_for_session("nonexistent");
        assert_eq!(count, 0);
    }
}
```

- [ ] **Step 2: Add module declaration**

In `src-tauri/src/tools/mod.rs`, add:

```rust
pub mod builtin;
pub mod confirm;
pub mod registry;
pub mod risk;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::confirm`
Expected: PASS (5 tests)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tools/confirm.rs src-tauri/src/tools/mod.rs
git commit -m "feat: add ConfirmRegistry with session-based cancellation"
```

---

## Task 5: ExecChannelPool

**Files:**
- Create: `src-tauri/src/exec/pool.rs`
- Modify: `src-tauri/src/exec/mod.rs`

- [ ] **Step 1: Write the ExecChannelPool with tests**

Create `src-tauri/src/exec/pool.rs`:

```rust
use super::channel::ExecChannel;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("session {session_id} has no associated environment")]
    NoEnvironment { session_id: String },
    #[error("environment {env_id} not found")]
    EnvironmentNotFound { env_id: String },
    #[error("transport not yet implemented: {0}")]
    TransportNotImplemented(String),
}

pub struct ExecChannelPool {
    connections: HashMap<String, Arc<dyn ExecChannel>>,
}

impl ExecChannelPool {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    pub async fn get_or_create(
        &mut self,
        session_id: &str,
        pool: &sqlx::SqlitePool,
    ) -> Result<Arc<dyn ExecChannel>, PoolError> {
        if let Some(channel) = self.connections.get(session_id) {
            return Ok(channel.clone());
        }

        let env = fetch_environment(pool, session_id).await?;

        let channel: Arc<dyn ExecChannel> = match env.transport_type.as_str() {
            "ssh" => Arc::new(super::ssh::SshTransport {
                host: env.host.unwrap_or_default(),
                port: env.port.unwrap_or(22),
                user: env.user.unwrap_or_default(),
            }),
            "k8s" => Arc::new(super::k8s::K8sTransport {
                namespace: env.k8s_namespace.unwrap_or_default(),
                pod: env.k8s_pod.unwrap_or_default(),
                container: String::new(),
            }),
            other => return Err(PoolError::TransportNotImplemented(other.to_string())),
        };

        channel
            .connect()
            .await
            .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

        self.connections.insert(session_id.to_string(), channel.clone());
        Ok(channel)
    }

    pub async fn disconnect(&mut self, session_id: &str) {
        if let Some(channel) = self.connections.remove(session_id) {
            channel.disconnect().await;
        }
    }

    pub async fn disconnect_all(&mut self) {
        let channels: Vec<_> = self.connections.drain().collect();
        for (_, channel) in channels {
            channel.disconnect().await;
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

impl Default for ExecChannelPool {
    fn default() -> Self {
        Self::new()
    }
}

struct EnvironmentInfo {
    transport_type: String,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    k8s_namespace: Option<String>,
    k8s_pod: Option<String>,
}

async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    session_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT environment_id FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

    let env_id = row
        .and_then(|(id,)| id)
        .ok_or(PoolError::NoEnvironment {
            session_id: session_id.to_string(),
        })?;

    let env_row: Option<(Option<String>, Option<i64>, Option<String>, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT host, port, user, transport_type, k8s_namespace, k8s_pod \
             FROM environments WHERE id = ?",
        )
        .bind(&env_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

    let env_row = env_row.ok_or(PoolError::EnvironmentNotFound { env_id })?;

    Ok(EnvironmentInfo {
        transport_type: env_row.3,
        host: env_row.0,
        port: env_row.1.map(|p| p as u16),
        user: env_row.2,
        k8s_namespace: env_row.4,
        k8s_pod: env_row.5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct MockChannel;

    #[async_trait]
    impl ExecChannel for MockChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
    }

    #[tokio::test]
    async fn test_disconnect_removes_connection() {
        let mut pool = ExecChannelPool::new();
        pool.connections.insert("s1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);

        pool.disconnect("s1").await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_disconnect_nonexistent_is_noop() {
        let mut pool = ExecChannelPool::new();
        pool.disconnect("nonexistent").await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_disconnect_all_removes_all() {
        let mut pool = ExecChannelPool::new();
        pool.connections.insert("s1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);
        pool.connections.insert("s2".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);

        pool.disconnect_all().await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_get_or_create_no_environment_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&db_pool, "test").await.unwrap();

        let mut pool = ExecChannelPool::new();
        let result = pool.get_or_create(&session.id.0, &db_pool).await;

        assert!(matches!(result, Err(PoolError::NoEnvironment { .. })));
    }
}
```

- [ ] **Step 2: Add module declaration**

In `src-tauri/src/exec/mod.rs`, change to:

```rust
pub mod channel;
pub mod k8s;
pub mod pool;
pub mod ssh;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::pool`
Expected: PASS (4 tests)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/exec/pool.rs src-tauri/src/exec/mod.rs
git commit -m "feat: add ExecChannelPool with lazy connection creation"
```

---

## Task 6: SSH/K8s Transport Placeholder

**Files:**
- Modify: `src-tauri/src/exec/ssh.rs`
- Modify: `src-tauri/src/exec/k8s.rs`

- [ ] **Step 1: Replace todo!() in ssh.rs**

Rewrite `src-tauri/src/exec/ssh.rs`:

```rust
use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;

pub struct SshTransport {
    pub host: String,
    pub port: u16,
    pub user: String,
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
```

- [ ] **Step 2: Replace todo!() in k8s.rs**

Rewrite `src-tauri/src/exec/k8s.rs`:

```rust
use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;

pub struct K8sTransport {
    pub namespace: String,
    pub pod: String,
    pub container: String,
}

#[async_trait]
impl ExecChannel for K8sTransport {
    async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        Err("K8s transport not yet implemented".into())
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("K8s transport not yet implemented".into())
    }

    async fn disconnect(&self) {}
}
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/exec/ssh.rs src-tauri/src/exec/k8s.rs
git commit -m "feat: replace todo!() with Err placeholders in SSH/K8s transports"
```

---

## Task 7: SessionMapper

**Files:**
- Create: `src-tauri/src/mcp/mod.rs` (partial — just SessionMapper for now)
- Create: `src-tauri/src/mcp/session_mapper.rs`

- [ ] **Step 1: Write the SessionMapper with tests**

Create `src-tauri/src/mcp/mod.rs`:

```rust
pub mod session_mapper;
```

Create `src-tauri/src/mcp/session_mapper.rs`:

```rust
use std::collections::HashMap;

pub struct SessionMapper {
    next_session: Option<String>,
    mapping: HashMap<String, String>,
}

impl SessionMapper {
    pub fn new() -> Self {
        Self {
            next_session: None,
            mapping: HashMap::new(),
        }
    }

    pub fn enqueue(&mut self, session_id: String) {
        self.next_session = Some(session_id);
    }

    pub fn dequeue_and_map(&mut self, mcp_session_id: String) {
        if let Some(friday_session_id) = self.next_session.take() {
            self.mapping.insert(mcp_session_id, friday_session_id);
        }
    }

    pub fn lookup(&self, mcp_session_id: &str) -> Option<String> {
        self.mapping.get(mcp_session_id).cloned()
    }

    pub fn pending_count(&self) -> usize {
        self.mapping.len()
    }

    pub fn has_queued(&self) -> bool {
        self.next_session.is_some()
    }
}

impl Default for SessionMapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_and_dequeue_creates_mapping() {
        let mut mapper = SessionMapper::new();
        mapper.enqueue("friday-s1".to_string());
        assert!(mapper.has_queued());

        mapper.dequeue_and_map("mcp-session-abc".to_string());
        assert!(!mapper.has_queued());

        assert_eq!(mapper.lookup("mcp-session-abc"), Some("friday-s1".to_string()));
    }

    #[test]
    fn test_dequeue_without_enqueue_is_noop() {
        let mut mapper = SessionMapper::new();
        mapper.dequeue_and_map("mcp-session-xyz".to_string());
        assert_eq!(mapper.lookup("mcp-session-xyz"), None);
    }

    #[test]
    fn test_lookup_nonexistent_returns_none() {
        let mapper = SessionMapper::new();
        assert_eq!(mapper.lookup("nonexistent"), None);
    }

    #[test]
    fn test_multiple_mappings() {
        let mut mapper = SessionMapper::new();
        mapper.enqueue("s1".to_string());
        mapper.dequeue_and_map("mcp1".to_string());
        mapper.enqueue("s2".to_string());
        mapper.dequeue_and_map("mcp2".to_string());

        assert_eq!(mapper.lookup("mcp1"), Some("s1".to_string()));
        assert_eq!(mapper.lookup("mcp2"), Some("s2".to_string()));
        assert_eq!(mapper.pending_count(), 2);
    }
}
```

- [ ] **Step 2: Add mcp module to lib.rs**

In `src-tauri/src/lib.rs`, add at the top with other mod declarations:

```rust
mod mcp;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml mcp::session_mapper`
Expected: PASS (4 tests)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/mcp/mod.rs src-tauri/src/mcp/session_mapper.rs src-tauri/src/lib.rs
git commit -m "feat: add SessionMapper for Mcp-Session-Id routing"
```

---

## Task 8: System Prompt session_id Injection

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`
- Modify: `src-tauri/src/agent/spawn.rs`

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/agent/prompt.rs` tests module, add:

```rust
    #[test]
    fn test_build_prompt_injects_session_id() {
        let result = build_prompt("hello", None, "session-abc-123");
        assert!(result.contains("session-abc-123"));
        assert!(result.contains("工具使用"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_build_prompt_with_experiences_injects_session_id() {
        let exps = vec![make_test_experience(Outcome::Positive, Some("root cause"))];
        let result = build_prompt_with_experiences("hello", None, "session-xyz", &exps);

        assert!(result.contains("session-xyz"));
        assert!(result.contains("工具使用"));
        assert!(result.contains("历史经验参考"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml prompt::tests::test_build_prompt_injects_session_id`
Expected: FAIL — function signature mismatch

- [ ] **Step 3: Update build_prompt functions**

In `src-tauri/src/agent/prompt.rs`, update the function signatures and implementations:

Change `build_prompt`:
```rust
pub fn build_prompt(message: &str, override_path: Option<&Path>, session_id: &str) -> String {
    let system = build_system_prompt(override_path);
    format!(
        "{system}\n\n---\n\n## 工具使用\n- 调用诊断工具时，必须传入 session_id 参数。\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
    )
}
```

Change `build_prompt_with_experiences`:
```rust
pub fn build_prompt_with_experiences(
    message: &str,
    override_path: Option<&Path>,
    session_id: &str,
    experiences: &[Experience],
) -> String {
    let system = build_system_prompt(override_path);

    if experiences.is_empty() {
        return format!(
            "{system}\n\n---\n\n## 工具使用\n- 调用诊断工具时，必须传入 session_id 参数。\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
        );
    }

    let mut exp_section = String::from("## 历史经验参考\n");
    for (i, exp) in experiences.iter().enumerate() {
        let label = match exp.outcome {
            Outcome::Positive => "成功",
            Outcome::Negative => "未成功",
            Outcome::Uncertain => "不确定",
        };
        let title = format!("{} {}", exp.service, exp.symptom);
        writeln!(exp_section, "### 经验 {}（{}）：{}", i + 1, label, title).ok();
        writeln!(exp_section, "症状：{}", exp.symptom).ok();
        if let Some(rc) = &exp.root_cause {
            writeln!(exp_section, "根因：{}", rc).ok();
        }
        writeln!(exp_section, "排查路径：{}", exp.investigation_path).ok();
        if !exp.experience_lesson.is_empty() {
            writeln!(exp_section, "经验：{}", exp.experience_lesson).ok();
        }
        writeln!(exp_section).ok();
    }

    format!(
        "{system}\n\n---\n\n## 工具使用\n- 调用诊断工具时，必须传入 session_id 参数。\n- 当前会话的 session_id：{session_id}\n\n---\n\n{exp_section}\n---\n\n用户消息：{message}"
    )
}
```

Update existing tests that call `build_prompt` and `build_prompt_with_experiences` to add the `session_id` parameter. For example:

- `test_build_prompt_includes_system_and_message`: add `"test-session"` as third arg
- `test_build_prompt_uses_override_system_prompt`: add `"test-session"` as third arg
- `test_build_prompt_with_experiences_injects_section`: add `"test-session"` as third arg
- `test_build_prompt_with_empty_experiences_no_section`: add `"test-session"` as third arg

- [ ] **Step 4: Update spawn.rs to pass session_id**

In `src-tauri/src/agent/spawn.rs`, update the `spawn_active` function. The `session_id` parameter already exists (it's the second parameter). Update the prompt building calls:

Change:
```rust
        prompt::build_prompt_with_experiences(&message, prompt_override_path.as_deref(), exps)
```
to:
```rust
        prompt::build_prompt_with_experiences(&message, prompt_override_path.as_deref(), &session_id, exps)
```

Change:
```rust
        prompt::build_prompt(&message, prompt_override_path.as_deref())
```
to:
```rust
        prompt::build_prompt(&message, prompt_override_path.as_deref(), &session_id)
```

- [ ] **Step 5: Run all prompt tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml prompt`
Expected: PASS (all prompt tests)

- [ ] **Step 6: Run cargo check to catch any other call sites**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent/prompt.rs src-tauri/src/agent/spawn.rs
git commit -m "feat: inject session_id into system prompt for tool routing"
```

---

## Task 9: ConfirmRequired Event — Add confirm_id

**Files:**
- Modify: `src-tauri/src/app/events.rs`

- [ ] **Step 1: Add confirm_id to ConfirmRequired variant**

In `src-tauri/src/app/events.rs`, update the `ConfirmRequired` variant:

```rust
    ConfirmRequired {
        session_id: String,
        confirm_id: String,
        tool: String,
        args: serde_json::Value,
        risk_level: RiskLevel,
    },
```

- [ ] **Step 2: Update the test**

In the tests module, update `test_confirm_required_serialization`:

```rust
    #[test]
    fn test_confirm_required_serialization() {
        let event = AppEvent::ConfirmRequired {
            session_id: "s1".to_string(),
            confirm_id: "c1".to_string(),
            tool: "arthas trace".to_string(),
            args: serde_json::json!({"class": "com.example.Foo"}),
            risk_level: RiskLevel::Low,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("confirm_required"));
        assert!(json.contains("low"));
        assert!(json.contains("c1"));
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml events`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/app/events.rs
git commit -m "feat: add confirm_id to ConfirmRequired event"
```

---

## Task 10: Opencode Config Auto-Merge

**Files:**
- Create: `src-tauri/src/mcp/config.rs`
- Modify: `src-tauri/src/mcp/mod.rs`

- [ ] **Step 1: Write the config merge module with tests**

Create `src-tauri/src/mcp/config.rs`:

```rust
use serde_json::Value;
use std::path::PathBuf;

pub fn merge_friday_mcp_config(config_path: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{port}/mcp");

    let existing = read_config(&config_path)?;
    let merged = inject_friday_entry(existing, &url);

    write_config(&config_path, &merged)?;

    tracing::info!(path = %config_path.display(), port, url = %url, "merged Friday MCP config into opencode");
    Ok(())
}

fn read_config(path: &PathBuf) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return Ok(Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(path)?;

    match serde_json::from_str::<Value>(&content) {
        Ok(v) => Ok(v),
        Err(e) => {
            tracing::warn!(?e, path = %path.display(), "failed to parse opencode config, backing up and starting fresh");
            let backup = path.with_extension("jsonc.bak");
            std::fs::rename(path, &backup)?;
            Ok(Value::Object(serde_json::Map::new()))
        }
    }
}

fn inject_friday_entry(mut config: Value, url: &str) -> Value {
    if config.get("mcp").is_none() {
        config["mcp"] = Value::Object(serde_json::Map::new());
    }

    let mcp = config.get_mut("mcp").unwrap();
    if mcp.get("friday").is_none() {
        mcp["friday"] = Value::Object(serde_json::Map::new());
    }

    let friday = mcp.get_mut("friday").unwrap();
    friday["type"] = Value::String("remote".to_string());
    friday["url"] = Value::String(url.to_string());
    friday["enabled"] = Value::Bool(true);
    friday["timeout"] = Value::Number(serde_json::Number::from(10000));

    config
}

fn write_config(path: &PathBuf, config: &Value) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pretty = serde_json::to_string_pretty(config)?;
    std::fs::write(path, pretty)?;
    Ok(())
}

pub fn default_opencode_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("opencode").join("opencode.jsonc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_friday_entry_into_empty_config() {
        let config = Value::Object(serde_json::Map::new());
        let result = inject_friday_entry(config, "http://127.0.0.1:12345/mcp");

        assert_eq!(result["mcp"]["friday"]["type"], "remote");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
        assert_eq!(result["mcp"]["friday"]["enabled"], true);
        assert_eq!(result["mcp"]["friday"]["timeout"], 10000);
    }

    #[test]
    fn test_inject_friday_entry_preserves_existing_config() {
        let mut config = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "disabled_providers": ["zhipu"],
            "provider": {
                "zhipu": { "name": "Zhipu AI" }
            },
            "mcp": {
                "other_server": {
                    "type": "local",
                    "command": ["npx", "other"]
                }
            }
        });

        let result = inject_friday_entry(config, "http://127.0.0.1:9999/mcp");

        assert_eq!(result["disabled_providers"][0], "zhipu");
        assert_eq!(result["mcp"]["other_server"]["type"], "local");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:9999/mcp");
    }

    #[test]
    fn test_inject_friday_entry_updates_existing_friday() {
        let config = serde_json::json!({
            "mcp": {
                "friday": {
                    "type": "remote",
                    "url": "http://127.0.0.1:OLD/mcp"
                }
            }
        });

        let result = inject_friday_entry(config, "http://127.0.0.1:NEW/mcp");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:NEW/mcp");
    }

    #[tokio::test]
    async fn test_merge_friday_mcp_config_creates_file_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");

        merge_friday_mcp_config(config_path.clone(), 12345).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
    }

    #[tokio::test]
    async fn test_merge_friday_mcp_config_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");
        std::fs::write(&config_path, r#"{"$schema":"https://opencode.ai/config.json","disabled_providers":["zhipu"]}"#).unwrap();

        merge_friday_mcp_config(config_path.clone(), 54321).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["disabled_providers"][0], "zhipu");
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:54321/mcp");
    }

    #[tokio::test]
    async fn test_merge_friday_mcp_config_backs_up_corrupted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");
        std::fs::write(&config_path, "not valid json {{{").unwrap();

        merge_friday_mcp_config(config_path.clone(), 11111).unwrap();

        assert!(config_path.with_extension("jsonc.bak").exists());
        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:11111/mcp");
    }
}
```

- [ ] **Step 2: Update mcp/mod.rs**

In `src-tauri/src/mcp/mod.rs`, add:

```rust
pub mod config;
pub mod session_mapper;
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml mcp::config`
Expected: PASS (6 tests)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/mcp/config.rs src-tauri/src/mcp/mod.rs
git commit -m "feat: opencode config auto-merge for Friday MCP server"
```

---

## Task 11: FridayMcpServer — ServerHandler Implementation

**Files:**
- Create: `src-tauri/src/mcp/server.rs`
- Modify: `src-tauri/src/mcp/mod.rs`
- Modify: `src-tauri/Cargo.toml` (add rmcp dependency)

This is the largest task. It implements the `ServerHandler` trait with `list_tools`, `get_tool`, and `call_tool`.

- [ ] **Step 1: Add rmcp and HTTP dependencies to Cargo.toml**

In `src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
rmcp = { version = "3", features = ["server", "macros", "transport-streamable-http-server"] }
hyper = "1"
hyper-util = { version = "0.1", features = ["server", "http1", "tokio"] }
http = "1"
http-body-util = "0.1"
bytes = "1"
tower-service = "0.3"
```

- [ ] **Step 2: Write the FridayMcpServer**

Create `src-tauri/src/mcp/server.rs`:

```rust
use crate::app::events::{AppEvent, EventBus};
use crate::tools::confirm::{ConfirmRegistry, ConfirmResult};
use crate::tools::registry::{ToolContext, ToolOutput};
use crate::tools::risk::RiskLevel;
use crate::exec::pool::ExecChannelPool;
use crate::mcp::session_mapper::SessionMapper;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ErrorCode, ListToolsResult, McpError,
    ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::model::RoleServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing;

use crate::tools::registry::ToolRegistry;

pub struct FridayMcpServer {
    pub tool_registry: Arc<ToolRegistry>,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<SessionMapper>>,
    pub bus: EventBus,
    pub pool: sqlx::SqlitePool,
}

const SESSION_ID_PARAM: &str = "session_id";
const CONFIRM_TIMEOUT_SECS: u64 = 120;

impl ServerHandler for FridayMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "Friday".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: rmcp::model::ServerCapabilities {
                tools: Some(rmcp::model::ToolsCapability {
                    list_changed: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools: Vec<Tool> = self
            .tool_registry
            .list()
            .into_iter()
            .map(|def| {
                let mut schema = def.input_schema.clone();
                inject_session_id_param(&mut schema);
                Tool {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    input_schema: schema,
                    annotations: None,
                }
            })
            .collect();

        async move {
            Ok(ListToolsResult {
                tools,
                next_cursor: None,
            })
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        let def = self.tool_registry.get(name)?;
        let mut schema = def.input_schema.clone();
        inject_session_id_param(&mut schema);
        Some(Tool {
            name: def.name.clone(),
            description: def.description.clone(),
            input_schema: schema,
            annotations: None,
        })
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            self.dispatch_tool_call(request).await
        }
    }
}

impl FridayMcpServer {
    async fn dispatch_tool_call(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = &request.name;
        let args = request.arguments.clone().unwrap_or(serde_json::Value::Null);

        // Step 1: Extract session_id
        let session_id = match extract_session_id(&args) {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "缺少 session_id 参数".to_string(),
                )]));
            }
        };

        // Step 2: Find tool definition
        let def = match self.tool_registry.get(tool_name) {
            Some(d) => d,
            None => {
                tracing::warn!(tool = %tool_name, "tool not found");
                return Err(McpError::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("tool not found: {tool_name}"),
                    None,
                ));
            }
        };

        // Step 3: Risk interception
        match def.risk_level {
            RiskLevel::ReadOnly => {}
            RiskLevel::Low | RiskLevel::High => {
                let confirm_result = self
                    .request_confirmation(&session_id, tool_name, &args, def.risk_level)
                    .await;
                match confirm_result {
                    ConfirmResult::Confirmed => {}
                    ConfirmResult::Cancelled => {
                        return Ok(CallToolResult::error(vec![Content::text(
                            "用户取消了工具执行".to_string(),
                        )]));
                    }
                }
            }
        }

        // Step 4: Get or create ExecChannel
        let channel = {
            let mut exec_pool = self.exec_pool.lock().await;
            match exec_pool.get_or_create(&session_id, &self.pool).await {
                Ok(ch) => ch,
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "无法获取执行通道: {e}"
                    ))]));
                }
            }
        };

        // Step 5: Execute tool
        let ctx = ToolContext {
            session_id: session_id.clone(),
            channel,
        };

        self.bus.emit(
            &session_id,
            AppEvent::ToolExecuting {
                session_id: session_id.clone(),
                tool: tool_name.clone(),
                args: args.clone(),
            },
        );

        let start = std::time::Instant::now();
        let output = def.handler.execute(args.clone(), &ctx).await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        self.bus.emit(
            &session_id,
            AppEvent::ToolResult {
                session_id: session_id.clone(),
                tool: tool_name.clone(),
                output: output.data.clone(),
                elapsed_ms,
            },
        );

        // Step 6: Persist to tool_calls table
        if let Err(e) = persist_tool_call(
            &self.pool,
            &session_id,
            tool_name,
            &args,
            def.risk_level,
            &output,
            elapsed_ms,
        )
        .await
        {
            tracing::error!(?e, "failed to persist tool_call");
        }

        // Step 7: Convert to CallToolResult
        Ok(tool_output_to_result(output))
    }

    async fn request_confirmation(
        &self,
        session_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
        risk_level: RiskLevel,
    ) -> ConfirmResult {
        let confirm_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut registry = self.confirm_registry.lock().await;
            registry.insert(confirm_id.clone(), session_id.to_string(), tx);
        }

        self.bus.emit(
            session_id,
            AppEvent::ConfirmRequired {
                session_id: session_id.to_string(),
                confirm_id: confirm_id.clone(),
                tool: tool_name.to_string(),
                args: args.clone(),
                risk_level,
            },
        );

        match tokio::time::timeout(
            std::time::Duration::from_secs(CONFIRM_TIMEOUT_SECS),
            rx,
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                tracing::warn!(confirm_id = %confirm_id, "confirmation sender dropped");
                ConfirmResult::Cancelled
            }
            Err(_) => {
                tracing::warn!(confirm_id = %confirm_id, "confirmation timed out");
                let mut registry = self.confirm_registry.lock().await;
                registry.resolve(&confirm_id);
                ConfirmResult::Cancelled
            }
        }
    }
}

fn extract_session_id(args: &serde_json::Value) -> Option<String> {
    args.get(SESSION_ID_PARAM)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn inject_session_id_param(schema: &mut serde_json::Value) {
    if schema.get("properties").is_none() {
        schema["properties"] = serde_json::json!({});
    }

    if let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
        props.insert(
            SESSION_ID_PARAM.to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Current session ID for routing"
            }),
        );
    }

    if schema.get("required").is_none() {
        schema["required"] = serde_json::json!([]);
    }

    if let Some(required) = schema.get_mut("required").and_then(|r| r.as_array_mut()) {
        let already_has = required
            .iter()
            .any(|v| v.as_str() == Some(SESSION_ID_PARAM));
        if !already_has {
            required.push(serde_json::Value::String(SESSION_ID_PARAM.to_string()));
        }
    }
}

fn tool_output_to_result(output: ToolOutput) -> CallToolResult {
    let content_text = if output.success {
        let mut text = serde_json::to_string_pretty(&output.data).unwrap_or_default();
        if let Some(raw) = output.raw_stdout {
            text.push_str("\n\n--- raw stdout ---\n");
            text.push_str(&raw);
        }
        text
    } else {
        format!(
            "Tool error: {}",
            serde_json::to_string_pretty(&output.data).unwrap_or_default()
        )
    };

    if output.success {
        CallToolResult::success(vec![Content::text(content_text)])
    } else {
        CallToolResult::error(vec![Content::text(content_text)])
    }
}

async fn persist_tool_call(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    tool_name: &str,
    args: &serde_json::Value,
    risk_level: RiskLevel,
    output: &ToolOutput,
    elapsed_ms: u64,
) -> Result<(), sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let args_str = serde_json::to_string(args).unwrap_or_default();
    let risk_str = match risk_level {
        RiskLevel::ReadOnly => "read_only",
        RiskLevel::Low => "low",
        RiskLevel::High => "high",
    };
    let status = if output.success { "completed" } else { "error" };
    let output_str = serde_json::to_string(&output.data).unwrap_or_default();

    sqlx::query(
        "INSERT INTO tool_calls (id, session_id, tool_name, args, risk_level, status, output, raw_stdout, elapsed_ms, created_at, completed_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(tool_name)
    .bind(&args_str)
    .bind(risk_str)
    .bind(status)
    .bind(&output_str)
    .bind(output.raw_stdout.as_ref())
    .bind(elapsed_ms as i64)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_session_id_param_into_empty_schema() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {}
        });
        inject_session_id_param(&mut schema);

        assert!(schema["properties"]["session_id"]["type"] == "string");
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::Value::String("session_id".to_string())));
    }

    #[test]
    fn test_inject_session_id_param_preserves_existing_properties() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            },
            "required": ["message"]
        });
        inject_session_id_param(&mut schema);

        assert!(schema["properties"]["message"]["type"] == "string");
        assert!(schema["properties"]["session_id"]["type"] == "string");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("message".to_string())));
        assert!(required.contains(&serde_json::Value::String("session_id".to_string())));
    }

    #[test]
    fn test_inject_session_id_param_idempotent() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {"type": "string"}
            },
            "required": ["session_id"]
        });
        inject_session_id_param(&mut schema);

        let required = schema["required"].as_array().unwrap();
        let count = required.iter().filter(|v| v.as_str() == Some("session_id")).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_extract_session_id_present() {
        let args = serde_json::json!({"session_id": "s123", "message": "hi"});
        assert_eq!(extract_session_id(&args), Some("s123".to_string()));
    }

    #[test]
    fn test_extract_session_id_missing() {
        let args = serde_json::json!({"message": "hi"});
        assert_eq!(extract_session_id(&args), None);
    }

    #[test]
    fn test_tool_output_to_result_success() {
        let output = ToolOutput {
            success: true,
            data: serde_json::json!({"result": "ok"}),
            raw_stdout: None,
        };
        let result = tool_output_to_result(output);
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_output_to_result_failure() {
        let output = ToolOutput {
            success: false,
            data: serde_json::json!({"error": "timeout"}),
            raw_stdout: None,
        };
        let result = tool_output_to_result(output);
        assert!(result.is_error);
    }
}
```

- [ ] **Step 3: Update mcp/mod.rs**

In `src-tauri/src/mcp/mod.rs`:

```rust
pub mod config;
pub mod server;
pub mod session_mapper;
```

- [ ] **Step 4: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors (may need to fix import paths based on rmcp actual API)

If there are compilation errors, fix them. The rmcp API may differ slightly from what's written — check the actual types and adjust. Key things to verify:
- `ServerInfo` struct fields
- `ServerCapabilities` struct fields
- `CallToolResult::success()` and `CallToolResult::error()` method signatures
- `Content::text()` constructor
- `Tool` struct fields
- `McpError::new()` signature

- [ ] **Step 5: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml mcp::server`
Expected: PASS (7 tests)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/mcp/server.rs src-tauri/src/mcp/mod.rs
git commit -m "feat: implement FridayMcpServer with ServerHandler — list_tools, get_tool, call_tool dispatch"
```

---

## Task 12: MCP Transport — hyper serve

**Files:**
- Create: `src-tauri/src/mcp/transport.rs`
- Modify: `src-tauri/src/mcp/mod.rs`

- [ ] **Step 1: Write the transport module**

Create `src-tauri/src/mcp/transport.rs`:

```rust
use super::server::FridayMcpServer;
use crate::app::events::EventBus;
use crate::exec::pool::ExecChannelPool;
use crate::mcp::session_mapper::SessionMapper;
use crate::tools::confirm::ConfirmRegistry;
use crate::tools::registry::ToolRegistry;
use rmcp::transport::streamable_http_server::session::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct McpServerHandle {
    pub port: u16,
    pub cancel_token: CancellationToken,
    pub join_handle: tokio::task::JoinHandle<()>,
}

pub fn start_mcp_server(
    tool_registry: Arc<ToolRegistry>,
    exec_pool: Arc<Mutex<ExecChannelPool>>,
    confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    session_mapper: Arc<Mutex<SessionMapper>>,
    bus: EventBus,
    pool: sqlx::SqlitePool,
) -> Result<McpServerHandle, Box<dyn std::error::Error + Send + Sync>> {
    let cancel_token = CancellationToken::new();
    let server_cancel = cancel_token.clone();

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    tracing::info!(port, "MCP server binding to 127.0.0.1");

    let config = StreamableHttpServerConfig {
        cancellation_token: server_cancel,
        sse_keep_alive: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    };

    let session_manager = Arc::new(LocalSessionManager::default());

    let service_factory = move || {
        Ok::<_, std::io::Error>(FridayMcpServer {
            tool_registry: tool_registry.clone(),
            exec_pool: exec_pool.clone(),
            confirm_registry: confirm_registry.clone(),
            session_mapper: session_mapper.clone(),
            bus: bus.clone(),
            pool: pool.clone(),
        })
    };

    let service = StreamableHttpService::new(service_factory, session_manager, config);

    // Convert std::net::TcpListener to tokio
    listener.set_nonblocking(true)?;
    let tokio_listener = tokio::net::TcpListener::from_std(listener)?;

    let join_handle = tokio::spawn(async move {
        let service = Arc::new(service);

        loop {
            match tokio_listener.accept().await {
                Ok((stream, addr)) => {
                    let service = service.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, addr, service).await {
                            tracing::error!(?e, %addr, "connection error");
                        }
                    });
                }
                Err(e) => {
                    if service_cancel.is_cancelled() {
                        tracing::info!("MCP server listener shutting down");
                        break;
                    }
                    tracing::error!(?e, "accept error");
                }
            }
        }
    });

    Ok(McpServerHandle {
        port,
        cancel_token,
        join_handle,
    })
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    _addr: SocketAddr,
    service: Arc<StreamableHttpService<FridayMcpServer, LocalSessionManager>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let io = hyper_util::rt::TokioIo::new(stream);

    let service_clone = service.clone();
    let svc = hyper::service::service_fn(move |req| {
        let service = service_clone.clone();
        async move {
            let response = service.handle(req).await;
            Ok::<_, std::convert::Infallible>(response)
        }
    });

    hyper::server::conn::http1::Builder::new()
        .serve_connection(io, svc)
        .await?;

    Ok(())
}
```

- [ ] **Step 2: Update mcp/mod.rs**

```rust
pub mod config;
pub mod server;
pub mod session_mapper;
pub mod transport;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors (fix any rmcp API discrepancies)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/mcp/transport.rs src-tauri/src/mcp/mod.rs
git commit -m "feat: MCP transport — hyper serve StreamableHttpService"
```

---

## Task 13: AppState & setup() Integration

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Update AppState struct**

In `src-tauri/src/lib.rs`, add the new fields to `AppState`:

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
    pub embedding: Option<Arc<crate::knowledge::embedding::EmbeddingService>>,
    pub vec_store: Option<Arc<crate::knowledge::vec_store::VecStore>>,
    pub tool_registry: Arc<crate::tools::registry::ToolRegistry>,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<crate::tools::confirm::ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<crate::mcp::session_mapper::SessionMapper>>,
    pub mcp_server: Option<crate::mcp::transport::McpServerHandle>,
}
```

- [ ] **Step 2: Update setup() to build tool registry and start MCP server**

In `setup()`, after the `vec_store` initialization and before `app.manage(AppState {...})`, add:

```rust
            // Build tool registry
            let mut tool_registry = crate::tools::registry::ToolRegistry::new();
            tool_registry.register(crate::tools::builtin::echo_tool_def());
            let tool_registry = Arc::new(tool_registry);

            // Create shared state for MCP server
            let exec_pool = Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new()));
            let confirm_registry = Arc::new(Mutex::new(crate::tools::confirm::ConfirmRegistry::new()));
            let session_mapper = Arc::new(Mutex::new(crate::mcp::session_mapper::SessionMapper::new()));

            // Start MCP server
            let mcp_server = match crate::mcp::transport::start_mcp_server(
                tool_registry.clone(),
                exec_pool.clone(),
                confirm_registry.clone(),
                session_mapper.clone(),
                EventBus::new(handle.clone()),
                pool.clone(),
            ) {
                Ok(handle) => {
                    tracing::info!(port = handle.port, "MCP server started");

                    // Merge Friday MCP config into opencode
                    if let Some(config_path) = crate::mcp::config::default_opencode_config_path() {
                        if let Err(e) = crate::mcp::config::merge_friday_mcp_config(config_path, handle.port) {
                            tracing::warn!(?e, "failed to merge opencode config");
                        }
                    }

                    Some(handle)
                }
                Err(e) => {
                    tracing::error!(?e, "failed to start MCP server");
                    None
                }
            };
```

Then update the `app.manage(AppState {...})` call to include the new fields:

```rust
            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle.clone()),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
                paths,
                embedding,
                vec_store,
                tool_registry,
                exec_pool,
                confirm_registry,
                session_mapper,
                mcp_server,
            });
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: integrate MCP server into AppState and setup()"
```

---

## Task 14: confirm_tool_cmd & list_tools_cmd & stop cleanup

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs`
- Modify: `src-tauri/src/lib.rs` (register new command)

- [ ] **Step 1: Rewrite confirm_tool_cmd**

In `src-tauri/src/app/lifecycle.rs`, replace the existing `confirm_tool_cmd`:

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn confirm_tool_cmd(
    state: State<'_, crate::AppState>,
    confirm_id: String,
    approved: bool,
) -> Result<(), String> {
    tracing::info!(confirm_id = %confirm_id, approved, "confirm_tool_cmd called");
    let mut registry = state.confirm_registry.lock().await;
    match registry.resolve(&confirm_id) {
        Some(tx) => {
            let result = if approved {
                crate::tools::confirm::ConfirmResult::Confirmed
            } else {
                crate::tools::confirm::ConfirmResult::Cancelled
            };
            tx.send(result).ok();
            Ok(())
        }
        None => Err("确认请求不存在或已过期".to_string()),
    }
}
```

- [ ] **Step 2: Add list_tools_cmd**

In `src-tauri/src/app/lifecycle.rs`, add:

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub risk_level: crate::tools::risk::RiskLevel,
}

#[tauri::command]
pub async fn list_tools_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<ToolInfo>, String> {
    let tools = state.tool_registry.list();
    Ok(tools
        .into_iter()
        .map(|def| ToolInfo {
            name: def.name.clone(),
            description: def.description.clone(),
            risk_level: def.risk_level,
        })
        .collect())
}
```

- [ ] **Step 3: Add confirm cleanup to stop_agent_for_session**

In `src-tauri/src/app/lifecycle.rs`, update `stop_agent_for_session`:

```rust
pub async fn stop_agent_for_session(
    agents: &Arc<Mutex<HashMap<String, RunningAgent>>>,
    session_id: &str,
) -> Result<(), String> {
    let entry = {
        let mut map = agents.lock().await;
        map.remove(session_id)
    };

    if let Some(running) = entry {
        running.cancel.cancel();
        let _ = running.handle.await;
    }
    Ok(())
}
```

And in `stop_agent_cmd`, add confirm cleanup before calling `stop_agent_for_session`:

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn stop_agent_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    // Cancel any pending tool confirmations for this session
    {
        let mut registry = state.confirm_registry.lock().await;
        let count = registry.cancel_for_session(&session_id);
        if count > 0 {
            tracing::info!(session_id = %session_id, count, "cancelled pending confirms");
        }
    }

    stop_agent_for_session(&state.agents, &session_id).await
}
```

Similarly in `close_session_cmd`, add before the existing logic:

```rust
    // Cancel pending confirms
    {
        let mut registry = state.confirm_registry.lock().await;
        registry.cancel_for_session(&session_id);
    }

    // Disconnect exec channel
    {
        let mut exec_pool = state.exec_pool.lock().await;
        exec_pool.disconnect(&session_id).await;
    }
```

- [ ] **Step 4: Register list_tools_cmd in lib.rs**

In `src-tauri/src/lib.rs`, add `list_tools_cmd` to the `invoke_handler`:

```rust
        .invoke_handler(tauri::generate_handler![
            app::lifecycle::send_message_cmd,
            app::lifecycle::stop_agent_cmd,
            app::lifecycle::close_session_cmd,
            app::lifecycle::confirm_tool_cmd,
            app::lifecycle::list_sessions_cmd,
            app::lifecycle::set_log_level_cmd,
            app::lifecycle::get_session_messages_cmd,
            app::lifecycle::archive_session_cmd,
            app::lifecycle::unarchive_session_cmd,
            app::lifecycle::delete_session_cmd,
            app::lifecycle::get_session_summary_cmd,
            app::lifecycle::list_tools_cmd,
            app::agents::detect_agents_cmd,
            app::agents::list_agents_cmd,
            app::agents::add_agent_cmd,
            app::agents::set_active_agent_cmd,
            app::agents::remove_agent_cmd,
        ])
```

- [ ] **Step 5: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs src-tauri/src/lib.rs
git commit -m "feat: confirm_tool_cmd with approved param, list_tools_cmd, stop cleanup"
```

---

## Task 15: Frontend Adaptation

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`

- [ ] **Step 1: Update types.ts**

In `src/lib/types.ts`, update the `confirm_required` event to add `confirm_id`:

```typescript
export type AppEvent =
  | { type: "agent_started"; session_id: string; agent_pid: number }
  | { type: "tool_executing"; session_id: string; tool: string; args: unknown }
  | { type: "tool_result"; session_id: string; tool: string; output: unknown; elapsed_ms: number }
  | { type: "llm_thinking"; session_id: string; token: string }
  | { type: "confirm_required"; session_id: string; confirm_id: string; tool: string; args: unknown; risk_level: RiskLevel }
  | { type: "agent_stopped"; session_id: string }
  | { type: "agent_crashed"; session_id: string; reason: string }
  | { type: "diagnosis_done"; session_id: string; conclusion: string }
  | { type: "session_closed"; session_id: string }
  | { type: "session_deleted"; session_id: string };

export interface ToolInfo {
  name: string;
  description: string;
  risk_level: RiskLevel;
}
```

- [ ] **Step 2: Update ipc.ts**

In `src/lib/ipc.ts`, update `confirmTool` and add `listTools`:

```typescript
export async function confirmTool(confirmId: string, approved: boolean): Promise<void> {
  return invoke<void>("confirm_tool_cmd", { confirmId, approved });
}

export async function listTools(): Promise<ToolInfo[]> {
  return invoke<ToolInfo[]>("list_tools_cmd");
}
```

Also add the import for `ToolInfo`:

```typescript
import type { EventPayload, AgentRow, SessionRow, MessageRow, ToolInfo } from "@/lib/types";
```

- [ ] **Step 3: Run typecheck**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts
git commit -m "feat: frontend adaptation — confirmTool params, listTools, ToolInfo type"
```

---

## Task 16: SessionMapper Integration with spawn_active

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs`

- [ ] **Step 1: Enqueue session_id before spawning agent**

In `send_message_cmd`, after the experiences recall block and before `spawn_active`, add:

```rust
    // Enqueue session_id for MCP session mapping
    {
        let mut mapper = state.session_mapper.lock().await;
        mapper.enqueue(friday_session_id.clone());
    }
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs
git commit -m "feat: enqueue session_id for MCP session mapping before spawn"
```

---

## Task 17: Final Verification

- [ ] **Step 1: Run full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass

- [ ] **Step 2: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors, no warnings (or minimal warnings)

- [ ] **Step 3: Run frontend typecheck**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 4: Run pnpm tauri dev (manual smoke test)**

Run: `pnpm tauri dev`

Verify:
- App starts without errors
- MCP server starts (check logs for "MCP server binding to 127.0.0.1" and port number)
- opencode config is updated (check `~/.config/opencode/opencode.jsonc` has `friday` entry)
- Send a message, verify agent spawns and session_id is in prompt

- [ ] **Step 5: Final commit if any fixes were needed**

```bash
git add -A
git commit -m "fix: final adjustments from verification"
```
