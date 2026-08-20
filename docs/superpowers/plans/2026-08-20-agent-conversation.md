# Agent 对话管道 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the end-to-end conversation pipeline from user input to streaming agent response in the Friday Tauri app.

**Architecture:** Each user message spawns `opencode run --format json --auto --thinking` as a child process. stdout NDJSON is parsed line-by-line and mapped to Friday AppEvents. Events are pushed to the frontend via Tauri's event system. Multi-turn conversations use `-s <opencode_session_id>` to continue prior sessions.

**Tech Stack:** Rust (tokio, sqlx, serde_json, tokio-util), React 19 + TypeScript + Zustand + Tailwind v4

**Spec:** `docs/superpowers/specs/2026-08-20-agent-conversation-design.md`

---

## File Structure

### Backend (Rust)

| File | Action | Responsibility |
|------|--------|----------------|
| `src-tauri/migrations/0003_conversation.sql` | Create | Add `opencode_session_id` and `title` columns to sessions table |
| `src-tauri/src/infra/db.rs` | Modify | Load migration 0003 |
| `src-tauri/src/app/session.rs` | Rewrite | Session CRUD: create, list, close, get, update_opencode_session_id |
| `src-tauri/src/agent/spawn.rs` | Modify | Construct `opencode run` command with proper args, pipe stdout |
| `src-tauri/src/agent/stream.rs` | Rewrite | NDJSON line parser → AppEvent mapper + process lifecycle (cancel/exit) |
| `src-tauri/src/agent/prompt.rs` | Modify | Simple passthrough (v1: return message as-is) |
| `src-tauri/src/app/lifecycle.rs` | Rewrite | Tauri commands: send_message, stop_agent, close_session, list_sessions |
| `src-tauri/src/lib.rs` | Modify | AppState gains `agents` map; handler registration updated |
| `src-tauri/Cargo.toml` | Modify | Add `tokio-util` dependency |

### Frontend (React/TypeScript)

| File | Action | Responsibility |
|------|--------|----------------|
| `src/lib/types.ts` | Modify | Add SessionRow, ChatMessage, ChatPart types |
| `src/lib/ipc.ts` | Modify | Replace startDiagnosis with sendMessage, add listSessions |
| `src/store/sessionStore.ts` | Rewrite | Session list, message accumulation, agent running state, event handling |
| `src/components/chat/MessageList.tsx` | Create | Scrollable message list with auto-scroll |
| `src/components/chat/UserMessage.tsx` | Create | User message bubble (right-aligned) |
| `src/components/chat/AgentMessage.tsx` | Create | Agent message container (reasoning + text + tool cards) |
| `src/components/chat/ToolCallCard.tsx` | Create | Tool call card (collapsible, status badge, output area) |
| `src/components/chat/InputArea.tsx` | Create | Input textarea + stop/send buttons |
| `src/components/layout/SessionSidebar.tsx` | Rewrite | Session list rendering + selection + new session |
| `src/components/layout/MainDiagnosisArea.tsx` | Rewrite | Mount MessageList + InputArea |
| `src/pages/DiagnosisPage.tsx` | Modify | Hook up event listener + session loading |

---

## Task 1: Database Migration — Add conversation columns

**Files:**
- Create: `src-tauri/migrations/0003_conversation.sql`
- Modify: `src-tauri/src/infra/db.rs:11-14`
- Test: `src-tauri/src/infra/db.rs` (add test)

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/infra/db.rs` in the `#[cfg(test)] mod tests` block, after the existing `test_db_init_creates_indexes` test:

```rust
    #[tokio::test]
    async fn test_db_init_adds_conversation_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().to_path_buf()).await.unwrap();

        let col_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name IN ('opencode_session_id', 'title')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(col_count, 2);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_adds_conversation_columns -- --exact`
Expected: FAIL with panic "assertion failed: left == 0, right == 2" (columns don't exist yet)

- [ ] **Step 3: Create migration file**

Create `src-tauri/migrations/0003_conversation.sql`:

```sql
ALTER TABLE sessions ADD COLUMN opencode_session_id TEXT;
ALTER TABLE sessions ADD COLUMN title TEXT;
```

- [ ] **Step 4: Load migration in db.rs**

Modify `src-tauri/src/infra/db.rs` — add after line 14 (after `schema2` execution):

```rust
    let schema3 = include_str!("../../migrations/0003_conversation.sql");
    sqlx::query(schema3).execute(&pool).await?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_adds_conversation_columns -- --exact`
Expected: PASS

Also run all db tests to ensure no regression:
Run: `cargo test --manifest-path src-tauri/Cargo.toml -- infra::db`
Expected: All 4 tests PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/migrations/0003_conversation.sql src-tauri/src/infra/db.rs
git commit -m "feat: add conversation columns migration"
```

---

## Task 2: Session CRUD — create, list, close, get, update_opencode_session_id

**Files:**
- Modify: `src-tauri/src/app/session.rs` (rewrite)
- Test: `src-tauri/src/app/session.rs` (add tests)

- [ ] **Step 1: Write the failing tests**

Replace the entire contents of `src-tauri/src/app/session.rs` with:

```rust
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub env: String,
    pub service: String,
    pub symptom: String,
    pub status: SessionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Closed,
}

#[derive(Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn truncate_title(message: &str) -> String {
    let chars: Vec<char> = message.trim().chars().take(40).collect();
    chars.into_iter().collect()
}

pub async fn create_session(
    pool: &SqlitePool,
    message: &str,
) -> Result<Session, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    let title = truncate_title(message);

    sqlx::query(
        "INSERT INTO sessions (id, env, service, symptom, status, created_at) \
         VALUES (?, '', '', '', 'active', ?)",
    )
    .bind(&id)
    .bind(&now)
    .execute(pool)
    .await?;

    sqlx::query("UPDATE sessions SET title = ? WHERE id = ?")
        .bind(&title)
        .bind(&id)
        .execute(pool)
        .await?;

    Ok(Session {
        id: SessionId(id),
        env: String::new(),
        service: String::new(),
        symptom: String::new(),
        status: SessionStatus::Active,
    })
}

pub async fn close_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = now_iso8601();
    sqlx::query("UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_sessions(pool: &SqlitePool) -> Result<Vec<SessionRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, title, status, created_at FROM sessions ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(SessionRow {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                status: row.try_get("status")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

pub async fn get_session(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, title, status, created_at FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    row.map(|row| {
        Ok(SessionRow {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            status: row.try_get("status")?,
            created_at: row.try_get("created_at")?,
        })
    })
    .transpose()
}

pub async fn get_opencode_session_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT opencode_session_id FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(oc_id,)| oc_id))
}

pub async fn update_opencode_session_id(
    pool: &SqlitePool,
    id: &str,
    oc_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sessions SET opencode_session_id = ? WHERE id = ?")
        .bind(oc_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    async fn setup() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        db::init(tmp.path().to_path_buf()).await.unwrap()
    }

    #[tokio::test]
    async fn test_create_session_inserts_row() {
        let pool = setup().await;
        let session = create_session(&pool, "OOMService 频繁 OOM").await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_create_session_sets_title_to_first_40_chars() {
        let pool = setup().await;
        let short_msg = "OOMService OOM 了";
        let session = create_session(&pool, short_msg).await.unwrap();

        let title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, short_msg);
    }

    #[tokio::test]
    async fn test_create_session_truncates_long_title() {
        let pool = setup().await;
        let long_msg = "这是一条非常非常非常非常非常非常非常非常非常非常非常非常非常非常长的消息要超过四十个字符";
        let session = create_session(&pool, long_msg).await.unwrap();

        let title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        let title_chars: Vec<char> = title.chars().collect();
        assert_eq!(title_chars.len(), 40);
    }

    #[tokio::test]
    async fn test_close_session_updates_status() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        close_session(&pool, &session.id.0).await.unwrap();

        let status: String =
            sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "closed");
    }

    #[tokio::test]
    async fn test_list_sessions_returns_descending_by_created_at() {
        let pool = setup().await;
        let s1 = create_session(&pool, "first").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let s2 = create_session(&pool, "second").await.unwrap();

        let rows = list_sessions(&pool).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, s2.id.0);
        assert_eq!(rows[1].id, s1.id.0);
    }

    #[tokio::test]
    async fn test_get_session_returns_none_for_nonexistent() {
        let pool = setup().await;
        let result = get_session(&pool, "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_session_returns_row() {
        let pool = setup().await;
        let session = create_session(&pool, "test message").await.unwrap();

        let row = get_session(&pool, &session.id.0).await.unwrap().unwrap();
        assert_eq!(row.id, session.id.0);
        assert_eq!(row.title, Some("test message".to_string()));
        assert_eq!(row.status, "active");
    }

    #[tokio::test]
    async fn test_get_opencode_session_id_returns_none_initially() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        let result = get_opencode_session_id(&pool, &session.id.0).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_update_opencode_session_id_persists() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        update_opencode_session_id(&pool, &session.id.0, "oc-123").await.unwrap();

        let result = get_opencode_session_id(&pool, &session.id.0).await.unwrap();
        assert_eq!(result, Some("oc-123".to_string()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- app::session`
Expected: FAIL — the functions are defined but the old `create_session`/`close_session` had `todo!()`. After rewriting, tests should compile and run. If any test fails, check the SQL.

Actually, since we're rewriting the file entirely, the tests should compile and pass if the SQL is correct. If any fail, fix the SQL.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- app::session`
Expected: All 9 tests PASS

- [ ] **Step 4: Run cargo check for whole crate**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors (lifecycle.rs still references old Session struct, but that's okay — it uses `todo!()` and won't be called yet. If there are compilation errors in lifecycle.rs, they'll be fixed in Task 5.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/app/session.rs
git commit -m "feat: implement session CRUD (create, list, close, get, update_oc_session_id)"
```

---

## Task 3: Add tokio-util dependency

**Files:**
- Modify: `src-tauri/Cargo.toml:26` (add tokio-util)

- [ ] **Step 1: Add dependency**

Add to `src-tauri/Cargo.toml` in the `[dependencies]` section, after `thiserror = "2"`:

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: Compiles successfully (dependency downloaded)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml
git commit -m "feat: add tokio-util dependency for CancellationToken"
```

---

## Task 4: Agent spawn — construct opencode run command

**Files:**
- Modify: `src-tauri/src/agent/spawn.rs`
- Modify: `src-tauri/src/agent/prompt.rs`

- [ ] **Step 1: Write the failing test**

The existing test `test_spawn_active_returns_no_active_agent_when_db_empty` tests the NoActiveAgent path. Add a new test to verify the command is constructed correctly. Since we can't easily test the exact command args without mocking Command, we'll test that `spawn_active` correctly reads the opencode_session_id from DB and that the function signature changes.

Add to `src-tauri/src/agent/spawn.rs` test module:

```rust
    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().to_path_buf())
            .await
            .unwrap();
        let result = spawn_active(&pool, String::new(), None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_binary_missing_when_path_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().to_path_buf())
            .await
            .unwrap();

        // Insert an active agent with a non-existent path
        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES ('test-id', 'opencode', 'OpenCode', '/nonexistent/path/opencode', NULL, 'manual', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = spawn_active(&pool, "test message".to_string(), None).await;
        assert!(matches!(result, Err(SpawnError::BinaryMissing { .. })));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::spawn::tests::test_spawn_active_returns_binary_missing_when_path_invalid`
Expected: FAIL — the current `spawn_active` doesn't accept `Option<String>` as third arg. The test won't compile because the signature is `(pool, _prompt, _mcp_config_path)`.

- [ ] **Step 3: Rewrite spawn.rs**

Replace the entire contents of `src-tauri/src/agent/spawn.rs` with:

```rust
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, ChildStdout, ChildStderr};
use tokio_util::sync::CancellationToken;

pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("无可用 agent，请先检测或手动添加")]
    NoActiveAgent,
    #[error("agent 二进制不存在：{path}")]
    BinaryMissing { path: String },
    #[error("启动 agent 失败：{0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("DB 查询失败：{0}")]
    Db(#[from] sqlx::Error),
}

pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    message: String,
    opencode_session_id: Option<String>,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str,) = row.ok_or(SpawnError::NoActiveAgent)?;
    let path = PathBuf::from(&path_str);

    if !path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let mut cmd = tokio::process::Command::new(&path);
    cmd.arg("run")
        .arg("--format")
        .arg("json")
        .arg("--auto")
        .arg("--thinking");

    if let Some(ref oc_id) = opencode_session_id {
        cmd.arg("-s").arg(oc_id);
    }

    cmd.arg(&message);

    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    let stdout = child.stdout.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stdout not piped"),
    ))?;

    let stderr = child.stderr.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stderr not piped"),
    ))?;

    Ok(AgentProcess { pid, child, stdout, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();
        let result = spawn_active(&pool, String::new(), None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_binary_missing_when_path_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();

        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES ('test-id', 'opencode', 'OpenCode', '/nonexistent/path/opencode', NULL, 'manual', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = spawn_active(&pool, "test message".to_string(), None).await;
        assert!(matches!(result, Err(SpawnError::BinaryMissing { .. })));
    }
}
```

- [ ] **Step 4: Update prompt.rs**

Replace the entire contents of `src-tauri/src/agent/prompt.rs` with:

```rust
pub fn build_prompt(message: &str) -> String {
    message.to_string()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::spawn`
Expected: Both tests PASS

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::`
Expected: All agent tests PASS (detect + spawn)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent/spawn.rs src-tauri/src/agent/prompt.rs
git commit -m "feat: construct opencode run command with json format and auto flags"
```

---

## Task 5: NDJSON stream parser — opencode events → AppEvent mapping

**Files:**
- Modify: `src-tauri/src/agent/stream.rs` (rewrite)

This is the core parsing logic. We'll test it by feeding constructed NDJSON strings and verifying the resulting AppEvents.

- [ ] **Step 1: Write the failing tests**

Replace the entire contents of `src-tauri/src/agent/stream.rs` with:

```rust
use super::spawn::AgentProcess;
use crate::app::events::{AppEvent, EventBus};
use serde_json::Value;

/// Parse a single NDJSON line and return the corresponding AppEvent(s).
/// Returns None for events that should be ignored (not mapped to any AppEvent).
pub fn parse_event(line: &str, session_id: &str) -> Vec<AppEvent> {
    let json: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "message.part.updated" => parse_message_part_updated(&json, session_id),
        "session.error" => vec![AppEvent::AgentCrashed {
            session_id: session_id.to_string(),
            reason: json
                .get("properties")
                .and_then(|p| p.get("error"))
                .and_then(|e| e.get("data"))
                .and_then(|d| d.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string(),
        }],
        _ => vec![],
    }
}

/// Extract opencode session ID from a session.created event.
/// Returns None for non-session.created events or if ID is missing.
pub fn extract_session_id(line: &str) -> Option<String> {
    let json: Value = serde_json::from_str(line).ok()?;
    if json.get("type").and_then(|t| t.as_str()) != Some("session.created") {
        return None;
    }
    json.get("properties")
        .and_then(|p| p.get("info"))
        .and_then(|i| i.get("id"))
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
}

fn parse_message_part_updated(json: &Value, session_id: &str) -> Vec<AppEvent> {
    let properties = match json.get("properties") {
        Some(p) => p,
        None => return vec![],
    };

    let part = match properties.get("part") {
        Some(p) => p,
        None => return vec![],
    };

    let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match part_type {
        "text" => {
            // Text part: check for delta (streaming token)
            if let Some(delta) = properties.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: delta.to_string(),
                    }];
                }
            }
            // If no delta but has full text, also emit (for non-streaming parts)
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: text.to_string(),
                    }];
                }
            }
            vec![]
        }
        "reasoning" => {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: text.to_string(),
                    }];
                }
            }
            vec![]
        }
        "tool" => parse_tool_event(part, session_id),
        _ => vec![],
    }
}

fn parse_tool_event(part: &Value, session_id: &str) -> Vec<AppEvent> {
    let tool_name = part
        .get("tool")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");

    let state = match part.get("state") {
        Some(s) => s,
        None => return vec![],
    };

    let status = state.get("status").and_then(|s| s.as_str()).unwrap_or("");

    match status {
        "running" => {
            let input = state.get("input").cloned().unwrap_or(Value::Null);
            vec![AppEvent::ToolExecuting {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                args: input,
            }]
        }
        "completed" => {
            let output = state
                .get("output")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();
            let elapsed_ms = compute_elapsed_ms(state);
            vec![AppEvent::ToolResult {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                output: serde_json::Value::String(output),
                elapsed_ms,
            }]
        }
        "error" => {
            let error = state
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
                .to_string();
            let elapsed_ms = compute_elapsed_ms(state);
            vec![AppEvent::ToolResult {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                output: serde_json::Value::String(error),
                elapsed_ms,
            }]
        }
        _ => vec![],
    }
}

fn compute_elapsed_ms(state: &Value) -> u64 {
    let start = state
        .get("time")
        .and_then(|t| t.get("start"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    let end = state
        .get("time")
        .and_then(|t| t.get("end"))
        .and_then(|e| e.as_u64())
        .unwrap_or(start);
    if end >= start {
        end - start
    } else {
        0
    }
}

/// Consume the stdout stream of an opencode process, parse NDJSON lines,
/// and emit AppEvents via the EventBus. Handles process lifecycle:
/// - stdout EOF + exit 0 → DiagnosisDone
/// - stdout EOF + exit ≠0 → AgentCrashed
/// - cancellation → AgentStopped
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::app::lifecycle::RunningAgent>>>,
    cancel: tokio_util::sync::CancellationToken,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let AgentProcess { mut child, stdout, .. } = agent;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        // Check for session.created to extract opencode session ID
                        if let Some(oc_id) = extract_session_id(&line) {
                            let _ = crate::app::session::update_opencode_session_id(
                                &pool, &session_id, &oc_id,
                            ).await;
                        }

                        // Parse and emit events
                        let events = parse_event(&line, &session_id);
                        for event in events {
                            bus.emit(&session_id, event);
                        }
                    }
                    Ok(None) => break,  // EOF
                    Err(e) => {
                        tracing::error!(?e, "error reading stdout line");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                child.kill().await.ok();
                bus.emit(&session_id, AppEvent::AgentStopped {
                    session_id: session_id.clone(),
                });
                let mut map = agents.lock().await;
                map.remove(&session_id);
                return;
            }
        }
    }

    // Natural end: wait for child to exit
    let status = child.wait().await;
    let exit_ok = status.as_ref().map(|s| s.success()).unwrap_or(false);

    if exit_ok {
        bus.emit(&session_id, AppEvent::DiagnosisDone {
            session_id: session_id.clone(),
            conclusion: String::new(),
        });
    } else {
        let reason = match &status {
            Ok(s) => format!("exit code: {}", s.code().unwrap_or(-1)),
            Err(e) => format!("wait error: {}", e),
        };
        bus.emit(&session_id, AppEvent::AgentCrashed {
            session_id: session_id.clone(),
            reason,
        });
    }

    let mut map = agents.lock().await;
    map.remove(&session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_delta_emits_llm_thinking() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"text","text":"Hello"},"delta":"Hel"}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "Hel");
            }
            _ => panic!("expected LlmThinking, got {:?}", events[0]),
        }
    }

    #[test]
    fn test_parse_reasoning_emits_llm_thinking() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"reasoning","text":"analyzing the issue"}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "analyzing the issue");
            }
            _ => panic!("expected LlmThinking"),
        }
    }

    #[test]
    fn test_parse_tool_running_emits_tool_executing() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"running","input":{"command":"ls -la"}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolExecuting { session_id, tool, args } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(args["command"], "ls -la");
            }
            _ => panic!("expected ToolExecuting"),
        }
    }

    #[test]
    fn test_parse_tool_completed_emits_tool_result() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"completed","output":"file1\nfile2","time":{"start":1000,"end":1800}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolResult { session_id, tool, output, elapsed_ms } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(output, "file1\nfile2");
                assert_eq!(elapsed_ms, 800);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn test_parse_tool_error_emits_tool_result_with_error() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"error","error":"command failed","time":{"start":1000,"end":1001}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolResult { session_id, tool, output, .. } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(output, "command failed");
            }
            _ => panic!("expected ToolResult with error"),
        }
    }

    #[test]
    fn test_parse_session_error_emits_agent_crashed() {
        let line = r#"{"type":"session.error","properties":{"error":{"name":"APIError","data":{"message":"rate limited"}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::AgentCrashed { session_id, reason } => {
                assert_eq!(session_id, "s1");
                assert_eq!(reason, "rate limited");
            }
            _ => panic!("expected AgentCrashed"),
        }
    }

    #[test]
    fn test_parse_unmapped_event_returns_empty() {
        let line = r#"{"type":"session.updated","properties":{}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_invalid_json_returns_empty() {
        let events = parse_event("not valid json", "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_empty_delta_returns_empty() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"text","text":""},"delta":""}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_extract_session_id_from_session_created() {
        let line = r#"{"type":"session.created","properties":{"info":{"id":"oc-session-abc","title":"test"}}}"#;
        let result = extract_session_id(line);
        assert_eq!(result, Some("oc-session-abc".to_string()));
    }

    #[test]
    fn test_extract_session_id_returns_none_for_other_events() {
        let line = r#"{"type":"message.updated","properties":{}}"#;
        let result = extract_session_id(line);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_session_id_returns_none_for_invalid_json() {
        let result = extract_session_id("not json");
        assert!(result.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::stream`
Expected: Compilation errors because `RunningAgent` type doesn't exist yet in `lifecycle.rs`. We need to define it. But the pure parsing functions (`parse_event`, `extract_session_id`) should compile and pass if we stub `consume_stream`.

Actually, the `consume_stream` function references `crate::app::lifecycle::RunningAgent` which doesn't exist yet. Let's temporarily make the test only cover the pure parsing functions. The `consume_stream` integration will be tested in Task 6.

Remove the `consume_stream` function and its imports temporarily, keep only the parsing functions and tests. We'll add `consume_stream` back in Task 6 after `RunningAgent` is defined.

Actually, let's restructure: keep `consume_stream` but move the `RunningAgent` definition to a shared location. Better yet, define `RunningAgent` in `stream.rs` itself since it's the consumer.

Let me revise the plan: define `RunningAgent` in `stream.rs` (where it's used by `consume_stream`), and have `lifecycle.rs` import it from there.

- [ ] **Step 3: Fix compilation — move RunningAgent to stream.rs**

In the `src-tauri/src/agent/stream.rs` file above, replace the `consume_stream` signature's reference to `crate::app::lifecycle::RunningAgent` with a local struct. Add this struct definition to `stream.rs`:

```rust
pub struct RunningAgent {
    pub cancel: CancellationToken,
    pub handle: tokio::task::JoinHandle<()>,
}
```

And change the `consume_stream` parameter from:
```rust
agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::app::lifecycle::RunningAgent>>>,
```
to:
```rust
agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::stream`
Expected: All 12 parsing tests PASS. (consume_stream is not directly tested here—it's an async integration function tested via manual verification.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/stream.rs
git commit -m "feat: implement NDJSON stream parser with opencode event → AppEvent mapping"
```

---

## Task 6: Lifecycle commands — send_message, stop_agent, close_session, list_sessions

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs` (rewrite)
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Write the failing test**

We can't easily write unit tests for Tauri commands (they need State, AppHandle). Instead, we test the helper functions. But first, let's verify the command signatures compile correctly.

Add a test that just verifies the helper `stop_agent_for_session` function works on an empty agents map:

Add to the test module in lifecycle.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;
    use crate::app::session;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_stop_agent_when_no_agent_running() {
        let agents: Arc<Mutex<HashMap<String, RunningAgent>>> = Arc::new(Mutex::new(HashMap::new()));
        // No agent running for "s1" — should return Ok without error
        let result = stop_agent_for_session(&agents, "s1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_close_session_updates_status() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();
        let s = session::create_session(&pool, "test").await.unwrap();
        session::close_session(&pool, &s.id.0).await.unwrap();

        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "closed");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- app::lifecycle`
Expected: FAIL — `stop_agent_for_session` and `RunningAgent` don't exist in lifecycle.rs yet.

- [ ] **Step 3: Rewrite lifecycle.rs**

Replace the entire contents of `src-tauri/src/app/lifecycle.rs` with:

```rust
use super::session::{self, SessionId};
use crate::agent::prompt;
use crate::agent::spawn::{spawn_active, AgentProcess, SpawnError};
use crate::agent::stream::{self, RunningAgent};
use crate::app::events::{AppEvent, EventBus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct LifecycleManager;

impl LifecycleManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartDiagnosisParams {
    pub env: String,
    pub service: String,
    pub symptom: String,
}

/// Stop a running agent for a session. Returns Ok(()) if no agent was running.
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

#[tauri::command]
pub async fn send_message_cmd(
    state: State<'_, crate::AppState>,
    session_id: Option<String>,
    message: String,
) -> Result<String, String> {
    let pool = state.db.clone();
    let bus = state.bus.clone();
    let agents = state.agents.clone();

    // Determine session ID and opencode session ID
    let (friday_session_id, oc_session_id) = match session_id {
        None => {
            let session = session::create_session(&pool, &message)
                .await
                .map_err(|e| e.to_string())?;
            (session.id.0, None)
        }
        Some(id) => {
            let row = session::get_session(&pool, &id)
                .await
                .map_err(|e| e.to_string())?;
            match row {
                None => return Err("会话不存在".to_string()),
                Some(row) if row.status == "closed" => {
                    return Err("会话已关闭".to_string())
                }
                Some(_) => {}
            }
            let oc_id = session::get_opencode_session_id(&pool, &id)
                .await
                .map_err(|e| e.to_string())?;
            (id, oc_id)
        }
    };

    // Check if agent is already running for this session
    {
        let map = agents.lock().await;
        if map.contains_key(&friday_session_id) {
            return Err("agent 正在运行".to_string());
        }
    }

    // Build prompt and spawn opencode
    let prompt_text = prompt::build_prompt(&message);
    let agent_process = spawn_active(&pool, prompt_text, oc_session_id)
        .await
        .map_err(|e| e.to_string())?;

    let pid = agent_process.pid;

    // Emit AgentStarted
    bus.emit(
        &friday_session_id,
        AppEvent::AgentStarted {
            session_id: friday_session_id.clone(),
            agent_pid: pid,
        },
    );

    // Set up cancellation and background task
    let cancel = CancellationToken::new();
    let session_id_clone = friday_session_id.clone();
    let bus_clone = bus.clone();
    let pool_clone = pool.clone();
    let agents_clone = agents.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            pool_clone,
            agents_clone,
            cancel,
        )
        .await;
    });

    // Store RunningAgent
    {
        let mut map = agents.lock().await;
        map.insert(
            friday_session_id.clone(),
            RunningAgent {
                cancel: CancellationToken::new(),
                handle,
            },
        );
    }

    // Fix: we need to store the cancel token that the background task actually uses
    // The above creates a new token not connected to the task. Let's fix this.
    // Actually, we should create the cancel token BEFORE spawning, and pass it to both.
    // Let me revise...

    Ok(friday_session_id)
}

#[tauri::command]
pub async fn stop_agent_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    stop_agent_for_session(&state.agents, &session_id).await
}

#[tauri::command]
pub async fn close_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    // Stop agent if running
    stop_agent_for_session(&state.agents, &session_id).await?;

    // Mark session as closed
    session::close_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())?;

    // Emit SessionClosed
    state.bus.emit(
        &session_id,
        AppEvent::SessionClosed {
            session_id: session_id.clone(),
        },
    );

    Ok(())
}

#[tauri::command]
pub async fn list_sessions_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<session::SessionRow>, String> {
    session::list_sessions(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn confirm_tool_cmd(
    _state: State<'_, crate::AppState>,
    _session_id: String,
    _tool: String,
) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_stop_agent_when_no_agent_running() {
        let agents: Arc<Mutex<HashMap<String, RunningAgent>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let result = stop_agent_for_session(&agents, "s1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_close_session_updates_status() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();
        let s = session::create_session(&pool, "test").await.unwrap();
        session::close_session(&pool, &s.id.0).await.unwrap();

        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "closed");
    }
}
```

**IMPORTANT FIX NEEDED:** The `send_message_cmd` above has a bug — it creates a `CancellationToken` for the background task but stores a *different* new token in the map. Fix this by creating the token once and passing it to both:

In `send_message_cmd`, replace the block from `// Set up cancellation` to the end of the `map.insert` block with:

```rust
    // Set up cancellation and background task
    let cancel = CancellationToken::new();

    let session_id_clone = friday_session_id.clone();
    let bus_clone = bus.clone();
    let pool_clone = pool.clone();
    let agents_clone = agents.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            pool_clone,
            agents_clone,
            cancel,
        )
        .await;
    });

    // Store RunningAgent
    {
        let mut map = agents.lock().await;
        map.insert(
            friday_session_id.clone(),
            RunningAgent { cancel, handle },
        );
    }
```

Wait — but `cancel` was moved into the `tokio::spawn` closure. We need to clone it. `CancellationToken` implements `Clone`:

```rust
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            pool_clone,
            agents_clone,
            cancel_for_task,
        )
        .await;
    });

    {
        let mut map = agents.lock().await;
        map.insert(friday_session_id.clone(), RunningAgent { cancel, handle });
    }
```

Apply this fix when implementing.

- [ ] **Step 4: Update lib.rs — AppState and handler registration**

Modify `src-tauri/src/lib.rs`:

Replace the entire file with:

```rust
mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod tools;

use app::events::EventBus;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
}

pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let guard = infra::logging::init(data_dir.clone());
            let pool = tauri::async_runtime::block_on(infra::db::init(data_dir))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
            });
            app.manage(guard);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::lifecycle::send_message_cmd,
            app::lifecycle::stop_agent_cmd,
            app::lifecycle::close_session_cmd,
            app::lifecycle::confirm_tool_cmd,
            app::lifecycle::list_sessions_cmd,
            app::agents::detect_agents_cmd,
            app::agents::list_agents_cmd,
            app::agents::add_agent_cmd,
            app::agents::set_active_agent_cmd,
            app::agents::remove_agent_cmd,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
```

- [ ] **Step 5: Run tests and fix compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: Compiles. If there are errors, fix them.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs src-tauri/src/lib.rs
git commit -m "feat: implement lifecycle commands (send_message, stop_agent, close_session, list_sessions)"
```

---

## Task 7: Frontend types and IPC bindings

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`

- [ ] **Step 1: Write the types**

Modify `src/lib/types.ts` — add after the existing `AgentRow` interface (at end of file):

```ts
export interface SessionRow {
  id: string;
  title: string | null;
  status: "active" | "closed";
  created_at: string;
}

export type ChatPartType = "text" | "reasoning" | "tool";

export interface ToolCallInfo {
  name: string;
  args: unknown;
  status: "running" | "completed" | "error";
  output?: string;
  elapsedMs?: number;
}

export interface ChatPart {
  type: ChatPartType;
  text?: string;
  tool?: ToolCallInfo;
}

export type ChatMessageStatus = "streaming" | "done" | "stopped" | "error";

export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  content: string;
  parts: ChatPart[];
  status: ChatMessageStatus;
}
```

- [ ] **Step 2: Update IPC bindings**

Modify `src/lib/ipc.ts` — replace `startDiagnosis` with `sendMessage`, add `listSessions`, remove `cancelDiagnosis`:

Replace lines 5-23 (the startDiagnosis through cancelDiagnosis functions) with:

```ts
export async function sendMessage(sessionId: string | null, message: string): Promise<string> {
  return invoke<string>("send_message_cmd", { sessionId, message });
}

export async function stopAgent(sessionId: string): Promise<void> {
  return invoke<void>("stop_agent_cmd", { sessionId });
}

export async function closeSession(sessionId: string): Promise<void> {
  return invoke<void>("close_session_cmd", { sessionId });
}

export async function confirmTool(sessionId: string, tool: string): Promise<void> {
  return invoke<void>("confirm_tool_cmd", { sessionId, tool });
}

export async function listSessions(): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions_cmd");
}
```

Also update the import at the top to include `SessionRow`:

```ts
import type { EventPayload, AgentRow, SessionRow } from "@/lib/types";
```

- [ ] **Step 3: Run typecheck**

Run: `pnpm typecheck`
Expected: Errors in sessionStore.ts (references old Session type, will be fixed in Task 8). That's expected. Verify no errors in types.ts and ipc.ts themselves.

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts
git commit -m "feat: update frontend types and IPC bindings for conversation pipeline"
```

---

## Task 8: Rewrite sessionStore

**Files:**
- Modify: `src/store/sessionStore.ts` (rewrite)

- [ ] **Step 1: Write the new store**

Replace the entire contents of `src/store/sessionStore.ts` with:

```ts
import { create } from "zustand";
import type { SessionRow, ChatMessage, ChatPart, AppEvent } from "@/lib/types";
import { sendMessage, stopAgent, listSessions, onAppEvent } from "@/lib/ipc";

interface SessionStore {
  sessions: SessionRow[];
  currentSessionId: string | null;
  messagesBySession: Record<string, ChatMessage[]>;
  agentRunning: Record<string, boolean>;
  inputText: string;
  eventUnlisten: (() => void) | null;

  loadSessions: () => Promise<void>;
  selectSession: (id: string) => void;
  newSession: () => void;
  setInputText: (text: string) => void;
  sendMessage: () => Promise<void>;
  stopAgent: () => Promise<void>;
  initEventListener: () => Promise<void>;
  handleEvent: (payload: { session_id: string; event: AppEvent }) => void;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

let agentMessageCounter = 0;

export const useSessionStore = create<SessionStore>((set, get) => ({
  sessions: [],
  currentSessionId: null,
  messagesBySession: {},
  agentRunning: {},
  inputText: "",
  eventUnlisten: null,

  loadSessions: async () => {
    try {
      const sessions = await listSessions();
      set({ sessions });
    } catch (e) {
      console.error("Failed to load sessions:", errMsg(e));
    }
  },

  selectSession: (id) => set({ currentSessionId: id }),

  newSession: () => set({ currentSessionId: null, inputText: "" }),

  setInputText: (text) => set({ inputText: text }),

  sendMessage: async () => {
    const { inputText, currentSessionId, agentRunning } = get();
    const trimmed = inputText.trim();
    if (!trimmed) return;
    if (currentSessionId && agentRunning[currentSessionId]) return;

    set({ inputText: "" });

    try {
      const sessionId = await sendMessage(currentSessionId, trimmed);

      // Add user message to the session
      const userMsg: ChatMessage = {
        id: `user-${Date.now()}`,
        role: "user",
        content: trimmed,
        parts: [],
        status: "done",
      };

      set((state) => {
        const existing = state.messagesBySession[sessionId] ?? [];
        // If this is a new session, clear previous messages
        const messages =
          state.currentSessionId === null ? [userMsg] : [...existing, userMsg];
        return {
          currentSessionId: sessionId,
          messagesBySession: { ...state.messagesBySession, [sessionId]: messages },
        };
      });

      // Reload sessions to show the new session in sidebar
      await get().loadSessions();
    } catch (e) {
      console.error("Failed to send message:", errMsg(e));
      set({ inputText: trimmed });
    }
  },

  stopAgent: async () => {
    const { currentSessionId } = get();
    if (!currentSessionId) return;
    try {
      await stopAgent(currentSessionId);
    } catch (e) {
      console.error("Failed to stop agent:", errMsg(e));
    }
  },

  initEventListener: async () => {
    const { eventUnlisten } = get();
    if (eventUnlisten) return;

    const unlisten = await onAppEvent((payload) => {
      get().handleEvent(payload);
    });
    set({ eventUnlisten: unlisten });
  },

  handleEvent: (payload) => {
    const { session_id, event } = payload;
    const state = get();

    if (event.type === "agent_started") {
      set({
        agentRunning: { ...state.agentRunning, [session_id]: true },
      });
      // Create a new agent message
      const agentMsg: ChatMessage = {
        id: `agent-${agentMessageCounter++}`,
        role: "agent",
        content: "",
        parts: [],
        status: "streaming",
      };
      const existing = state.messagesBySession[session_id] ?? [];
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: [...existing, agentMsg],
        },
      });
      return;
    }

    if (event.type === "llm_thinking") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const updatedParts = [...lastMsg.parts];
      const lastPart = updatedParts[updatedParts.length - 1];

      if (lastPart && lastPart.type === "text" && lastPart.text) {
        // Append to existing text part
        updatedParts[updatedParts.length - 1] = {
          ...lastPart,
          text: lastPart.text + event.token,
        };
      } else {
        // Create new text part
        updatedParts.push({ type: "text", text: event.token });
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "tool_executing") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const toolPart: ChatPart = {
        type: "tool",
        tool: {
          name: event.tool,
          args: event.args,
          status: "running",
        },
      };

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = {
        ...lastMsg,
        parts: [...lastMsg.parts, toolPart],
      };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (event.type === "tool_result") {
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      // Find the last tool part with matching name and running status
      const updatedParts = [...lastMsg.parts];
      for (let i = updatedParts.length - 1; i >= 0; i--) {
        const part = updatedParts[i];
        if (
          part.type === "tool" &&
          part.tool &&
          part.tool.name === event.tool &&
          part.tool.status === "running"
        ) {
          const output =
            typeof event.output === "string"
              ? event.output
              : JSON.stringify(event.output, null, 2);
          updatedParts[i] = {
            ...part,
            tool: {
              ...part.tool,
              status: "completed",
              output,
              elapsedMs: event.elapsed_ms,
            },
          };
          break;
        }
      }

      const updatedMessages = [...messages];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }

    if (
      event.type === "diagnosis_done" ||
      event.type === "agent_stopped" ||
      event.type === "agent_crashed"
    ) {
      const newRunning = { ...state.agentRunning };
      delete newRunning[session_id];
      set({ agentRunning: newRunning });

      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length > 0) {
        const lastIdx = messages.length - 1;
        const lastMsg = messages[lastIdx];
        if (lastMsg.role === "agent") {
          const status =
            event.type === "diagnosis_done"
              ? "done"
              : event.type === "agent_stopped"
                ? "stopped"
                : "error";
          const updatedMessages = [...messages];
          updatedMessages[lastIdx] = { ...lastMsg, status };
          set({
            messagesBySession: {
              ...state.messagesBySession,
              [session_id]: updatedMessages,
            },
          });
        }
      }
      return;
    }

    if (event.type === "session_closed") {
      set({
        sessions: get().sessions.map((s) =>
          s.id === session_id ? { ...s, status: "closed" as const } : s,
        ),
      });
      return;
    }
  },
}));
```

- [ ] **Step 2: Run typecheck**

Run: `pnpm typecheck`
Expected: Errors in layout components (they reference old code), but sessionStore.ts itself should be clean.

- [ ] **Step 3: Commit**

```bash
git add src/store/sessionStore.ts
git commit -m "feat: rewrite sessionStore for conversation pipeline"
```

---

## Task 9: Chat components — MessageList, UserMessage, AgentMessage, ToolCallCard

**Files:**
- Create: `src/components/chat/ToolCallCard.tsx`
- Create: `src/components/chat/UserMessage.tsx`
- Create: `src/components/chat/AgentMessage.tsx`
- Create: `src/components/chat/MessageList.tsx`

- [ ] **Step 1: Create ToolCallCard**

Create `src/components/chat/ToolCallCard.tsx`:

```tsx
import { useState } from "react";
import { CaretRight, CaretDown, CheckCircle, XCircle, Spinner } from "@phosphor-icons/react";
import type { ToolCallInfo } from "@/lib/types";

interface ToolCallCardProps {
  tool: ToolCallInfo;
}

export function ToolCallCard({ tool }: ToolCallCardProps) {
  const [expanded, setExpanded] = useState(false);

  const argsStr =
    typeof tool.args === "string"
      ? tool.args
      : JSON.stringify(tool.args, null, 2);

  const isRunning = tool.status === "running";
  const isError = tool.status === "error";

  return (
    <div className="bg-card border border-border rounded-lg overflow-hidden mb-3">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 px-3 py-2 w-full hover:bg-surface-2 transition-colors text-left"
      >
        {expanded ? (
          <CaretDown size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        ) : (
          <CaretRight size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        )}
        <span
          className="text-xs font-semibold text-success bg-success/10 px-1.5 py-0.5 rounded shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {tool.name}
        </span>
        <span
          className="text-xs text-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {argsStr}
        </span>
        <span
          className="text-xs shrink-0 flex items-center gap-1"
          style={{ fontFamily: "var(--font-mono)" }
        >
          {isRunning ? (
            <span className="text-accent flex items-center gap-1">
              <Spinner size={12} className="animate-spin" aria-hidden="true" />
              执行中...
            </span>
          ) : isError ? (
            <span className="text-destructive flex items-center gap-1">
              <XCircle size={12} weight="fill" aria-hidden="true" />
              失败
            </span>
          ) : (
            <span className="text-success flex items-center gap-1">
              <CheckCircle size={12} weight="fill" aria-hidden="true" />
              {tool.elapsedMs ? `${(tool.elapsedMs / 1000).toFixed(1)}s` : ""}
            </span>
          )}
        </span>
      </button>
      {expanded && tool.output && (
        <div className="border-t border-border px-3 py-2 bg-background max-h-40 overflow-y-auto">
          <pre
            className="text-xs text-muted-foreground whitespace-pre-wrap break-all"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {tool.output}
          </pre>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Create UserMessage**

Create `src/components/chat/UserMessage.tsx`:

```tsx
interface UserMessageProps {
  content: string;
}

export function UserMessage({ content }: UserMessageProps) {
  return (
    <div className="flex justify-end mb-5">
      <div
        className="max-w-[70%] bg-surface-2 border border-border rounded-xl rounded-br-sm px-3.5 py-2.5 text-sm leading-5 text-foreground"
        style={{ fontFamily: "var(--font-sans)" }}
      >
        {content}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Create AgentMessage**

Create `src/components/chat/AgentMessage.tsx`:

```tsx
import { useState } from "react";
import { CaretRight, CaretDown } from "@phosphor-icons/react";
import type { ChatMessage } from "@/lib/types";
import { ToolCallCard } from "./ToolCallCard";

interface AgentMessageProps {
  message: ChatMessage;
}

export function AgentMessage({ message }: AgentMessageProps) {
  const [reasoningExpanded, setReasoningExpanded] = useState(true);

  const reasoningParts = message.parts.filter((p) => p.type === "reasoning");
  const textParts = message.parts.filter((p) => p.type === "text");
  const toolParts = message.parts.filter((p) => p.type === "tool");

  const isStreaming = message.status === "streaming";

  return (
    <div className="mb-5 max-w-[85%]">
      <div className="flex items-center gap-1.5 mb-2">
        <span
          className="text-xs text-muted-foreground"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          ● Friday Agent
        </span>
      </div>

      {/* Reasoning (collapsible) */}
      {reasoningParts.length > 0 && (
        <div className="bg-surface-1 border border-border rounded-lg mb-3 overflow-hidden">
          <button
            onClick={() => setReasoningExpanded(!reasoningExpanded)}
            className="flex items-center gap-1 px-2.5 py-1.5 text-xs text-muted-foreground hover:bg-surface-2 transition-colors w-full text-left"
          >
            {reasoningExpanded ? (
              <CaretDown size={10} weight="bold" aria-hidden="true" />
            ) : (
              <CaretRight size={10} weight="bold" aria-hidden="true" />
            )}
            推理过程
          </button>
          {reasoningExpanded && (
            <div
              className="px-3 py-2 text-xs leading-5 text-muted-foreground border-t border-border"
              style={{ fontFamily: "var(--font-mono)" }}
            >
              {reasoningParts.map((p, i) => (
                <span key={i}>{p.text}</span>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Tool call cards (in order they appear in parts) */}
      {message.parts.map((part, i) => {
        if (part.type === "tool" && part.tool) {
          return <ToolCallCard key={i} tool={part.tool} />;
        }
        return null;
      })}

      {/* Agent text output */}
      {textParts.length > 0 ? (
        <div
          className="text-sm leading-6 text-foreground mb-3"
          style={{ fontFamily: "var(--font-sans)" }}
        >
          {textParts.map((p, i) => (
            <span key={i}>{p.text}</span>
          ))}
          {isStreaming && (
            <span
              className="inline-block w-[7px] h-[15px] bg-accent ml-0.5 align-text-bottom animate-pulse"
              aria-hidden="true"
            />
          )}
        </div>
      ) : isStreaming && toolParts.length === 0 ? (
        <div
          className="text-sm text-muted-foreground mb-3"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          思考中
          <span
            className="inline-block w-[7px] h-[15px] bg-accent ml-0.5 align-text-bottom animate-pulse"
            aria-hidden="true"
          />
        </div>
      ) : null}

      {/* Status indicator for finished messages */}
      {!isStreaming && (
        <div
          className="text-xs text-muted-foreground"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {message.status === "done" && "✓ 完成"}
          {message.status === "stopped" && "■ 已停止"}
          {message.status === "error" && "✕ 出错"}
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Create MessageList**

Create `src/components/chat/MessageList.tsx`:

```tsx
import { useEffect, useRef } from "react";
import type { ChatMessage } from "@/lib/types";
import { UserMessage } from "./UserMessage";
import { AgentMessage } from "./AgentMessage";

interface MessageListProps {
  messages: ChatMessage[];
}

export function MessageList({ messages }: MessageListProps) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const isAtBottomRef = useRef(true);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleScroll = () => {
      const { scrollTop, scrollHeight, clientHeight } = container;
      isAtBottomRef.current = scrollHeight - scrollTop - clientHeight < 50;
    };

    container.addEventListener("scroll", handleScroll);
    return () => container.removeEventListener("scroll", handleScroll);
  }, []);

  useEffect(() => {
    if (isAtBottomRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [messages]);

  if (messages.length === 0) {
    return null;
  }

  return (
    <div ref={containerRef} className="flex-1 overflow-y-auto px-6 py-4">
      {messages.map((msg) =>
        msg.role === "user" ? (
          <UserMessage key={msg.id} content={msg.content} />
        ) : (
          <AgentMessage key={msg.id} message={msg} />
        ),
      )}
      <div ref={bottomRef} />
    </div>
  );
}
```

- [ ] **Step 5: Run typecheck**

Run: `pnpm typecheck`
Expected: Errors in layout components and DiagnosisPage (not yet updated), but the chat components should be clean.

- [ ] **Step 6: Commit**

```bash
git add src/components/chat/
git commit -m "feat: create chat components (MessageList, UserMessage, AgentMessage, ToolCallCard)"
```

---

## Task 10: InputArea component

**Files:**
- Create: `src/components/chat/InputArea.tsx`

- [ ] **Step 1: Create InputArea**

Create `src/components/chat/InputArea.tsx`:

```tsx
import { useRef } from "react";
import { PaperPlaneTilt, Stop } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";

export function InputArea() {
  const inputText = useSessionStore((s) => s.inputText);
  const setInputText = useSessionStore((s) => s.setInputText);
  const sendMessage = useSessionStore((s) => s.sendMessage);
  const stopAgent = useSessionStore((s) => s.stopAgent);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const isRunning = currentSessionId ? agentRunning[currentSessionId] ?? false : false;
  const hasContent = inputText.trim().length > 0;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (hasContent && !isRunning) {
        sendMessage();
      }
    }
  };

  const placeholder = isRunning
    ? "补充信息...  Enter 发送 · Shift+Enter 换行"
    : "描述环境、服务和症状…  Enter 发送 · Shift+Enter 换行";

  return (
    <div className="shrink-0 px-6 pb-4 pt-2 border-t border-border bg-background">
      <div
        className="rounded-xl border border-border bg-surface-1 transition-colors focus-within:border-accent/40"
        style={{
          boxShadow: "0 1px 3px rgba(0, 0, 0, 0.3)",
        }}
      >
        <textarea
          ref={textareaRef}
          value={inputText}
          onChange={(e) => setInputText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          rows={2}
          className="w-full bg-transparent text-foreground text-sm rounded-xl px-4 py-3 resize-none outline-none placeholder:text-muted-foreground/50"
          style={{ fontFamily: "var(--font-sans)" }}
        />
        <div className="flex items-center justify-between px-3 pb-2.5">
          <span className="text-muted-foreground/60 text-xs">
            {isRunning ? "Agent 运行中，输入可补充信息" : "Enter 发送 · Shift+Enter 换行"}
          </span>
          <div className="flex items-center gap-2">
            {isRunning && (
              <button
                onClick={stopAgent}
                className="flex items-center gap-1.5 px-2.5 py-1 bg-destructive/10 border border-destructive/20 rounded-md text-destructive text-xs hover:bg-destructive/20 transition-colors"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                <Stop size={10} weight="fill" aria-hidden="true" />
                停止
              </button>
            )}
            <button
              onClick={() => hasContent && !isRunning && sendMessage()}
              className={`flex items-center justify-center w-7 h-7 rounded-md transition-all ${
                hasContent && !isRunning
                  ? "bg-accent text-white hover:bg-accent/80 cursor-pointer"
                  : "bg-muted text-muted-foreground cursor-not-allowed"
              }`}
              disabled={!hasContent || isRunning}
              aria-label="发送"
            >
              <PaperPlaneTilt size={14} weight="fill" aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Run typecheck**

Run: `pnpm typecheck`
Expected: InputArea is clean. Errors remain in layout components.

- [ ] **Step 3: Commit**

```bash
git add src/components/chat/InputArea.tsx
git commit -m "feat: create InputArea component with stop/send buttons"
```

---

## Task 11: Rewrite SessionSidebar and MainDiagnosisArea

**Files:**
- Modify: `src/components/layout/SessionSidebar.tsx` (rewrite)
- Modify: `src/components/layout/MainDiagnosisArea.tsx` (rewrite)
- Modify: `src/pages/DiagnosisPage.tsx`

- [ ] **Step 1: Rewrite SessionSidebar**

Replace the entire contents of `src/components/layout/SessionSidebar.tsx` with:

```tsx
import { ChatCircle, Plus } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";

export function SessionSidebar() {
  const sessions = useSessionStore((s) => s.sessions);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const selectSession = useSessionStore((s) => s.selectSession);
  const newSession = useSessionStore((s) => s.newSession);

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col">
      <div className="flex-1 overflow-y-auto flex flex-col">
        <div className="flex items-center justify-between px-4 h-9 shrink-0">
          <span className="text-muted-foreground text-xs font-medium tracking-wide uppercase">
            会话
          </span>
        </div>

        {sessions.length === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center px-6 py-8 select-none">
            <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-muted/40 border border-border mb-3">
              <ChatCircle size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
            </div>
            <p className="text-muted-foreground text-xs text-center leading-relaxed">
              暂无诊断会话
            </p>
            <p className="text-muted-foreground/60 text-xs text-center mt-1">
              在下方输入框描述问题开始
            </p>
          </div>
        ) : (
          <div className="px-2">
            {sessions.map((s) => {
              const isActive = s.id === currentSessionId;
              const isRunning = agentRunning[s.id] ?? false;
              return (
                <button
                  key={s.id}
                  onClick={() => selectSession(s.id)}
                  className={`w-full text-left px-3 py-2 rounded-lg mb-0.5 transition-colors cursor-pointer ${
                    isActive
                      ? "bg-surface-2 border-l-2 border-success pl-[10px]"
                      : "hover:bg-surface-2"
                  }`}
                >
                  <div className="flex items-center gap-1.5 mb-0.5">
                    <span
                      className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                        isRunning ? "bg-success animate-pulse" : "bg-muted-foreground"
                      }`}
                      aria-hidden="true"
                    />
                    <span className="text-sm font-medium text-foreground truncate flex-1">
                      {s.title || "无标题会话"}
                    </span>
                  </div>
                  <span
                    className="text-xs text-muted-foreground"
                    style={{ fontFamily: "var(--font-mono)" }}
                  >
                    {s.created_at.slice(0, 10)}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="p-3 border-t border-border">
        <button
          onClick={newSession}
          className="w-full flex items-center justify-center gap-2 text-sm text-muted-foreground bg-surface-2 hover:bg-surface-3 hover:text-foreground rounded-lg px-3 py-2 transition-colors cursor-pointer border border-border"
        >
          <Plus size={16} weight="regular" aria-hidden="true" />
          新建会话
        </button>
      </div>
    </aside>
  );
}
```

- [ ] **Step 2: Rewrite MainDiagnosisArea**

Replace the entire contents of `src/components/layout/MainDiagnosisArea.tsx` with:

```tsx
import { Crosshair, ArrowRight } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";
import { MessageList } from "@/components/chat/MessageList";
import { InputArea } from "@/components/chat/InputArea";

const EXAMPLE_PROMPT = "10.0.1.23 生产环境 OOMService 频繁 OOM，帮我定位根因";

export function MainDiagnosisArea() {
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const messagesBySession = useSessionStore((s) => s.messagesBySession);
  const setInputText = useSessionStore((s) => s.setInputText);

  const messages = currentSessionId ? messagesBySession[currentSessionId] ?? [] : [];

  const handleExampleClick = () => {
    setInputText(EXAMPLE_PROMPT);
  };

  return (
    <main className="flex-1 flex flex-col min-w-0 bg-background">
      {messages.length > 0 ? (
        <MessageList messages={messages} />
      ) : (
        <div className="flex-1 overflow-y-auto">
          <EmptyState onExampleClick={handleExampleClick} />
        </div>
      )}
      <InputArea />
    </main>
  );
}

function EmptyState({ onExampleClick }: { onExampleClick: () => void }) {
  return (
    <div className="h-full flex flex-col items-center justify-center px-8 select-none">
      <div className="relative mb-6">
        <div
          className="flex items-center justify-center w-16 h-16 rounded-2xl border border-border bg-surface-1"
          style={{
            backgroundImage:
              "linear-gradient(135deg, var(--color-surface-2) 0%, var(--color-surface-1) 100%)",
          }}
        >
          <Crosshair size={30} weight="regular" className="text-muted-foreground" aria-hidden="true" />
        </div>
      </div>

      <h2
        className="text-foreground text-lg font-medium mb-2"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        开始诊断
      </h2>

      <p className="text-muted-foreground text-sm text-center max-w-sm leading-relaxed mb-8">
        描述目标环境、服务和故障症状，Friday 将自动连接环境并定位根因
      </p>

      <button
        onClick={onExampleClick}
        className="group flex items-center gap-3 px-4 py-2.5 rounded-lg border border-border bg-surface-1 hover:bg-surface-2 hover:border-border-strong transition-all cursor-pointer max-w-lg w-full"
      >
        <span
          className="text-muted-foreground text-xs shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          示例
        </span>
        <span className="text-muted-foreground text-sm text-left flex-1 truncate group-hover:text-foreground transition-colors">
          {EXAMPLE_PROMPT}
        </span>
        <ArrowRight
          size={14}
          weight="regular"
          className="text-muted-foreground/50 group-hover:text-muted-foreground shrink-0 transition-colors"
          aria-hidden="true"
        />
      </button>
    </div>
  );
}
```

- [ ] **Step 3: Update DiagnosisPage**

Replace the entire contents of `src/pages/DiagnosisPage.tsx` with:

```tsx
import { useEffect } from "react";
import { TopBar } from "@/components/layout/TopBar";
import { SessionSidebar } from "@/components/layout/SessionSidebar";
import { MainDiagnosisArea } from "@/components/layout/MainDiagnosisArea";
import { useAgentStore } from "@/store/agentStore";
import { useSessionStore } from "@/store/sessionStore";

export function DiagnosisPage() {
  const refreshAgents = useAgentStore((s) => s.refresh);
  const loadSessions = useSessionStore((s) => s.loadSessions);
  const initEventListener = useSessionStore((s) => s.initEventListener);

  useEffect(() => {
    refreshAgents();
    loadSessions();
    initEventListener();
  }, [refreshAgents, loadSessions, initEventListener]);

  return (
    <div className="flex flex-col h-screen bg-background">
      <TopBar />
      <div className="flex flex-1 min-h-0">
        <SessionSidebar />
        <MainDiagnosisArea />
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Run typecheck**

Run: `pnpm typecheck`
Expected: PASS (all type errors resolved)

- [ ] **Step 5: Commit**

```bash
git add src/components/layout/SessionSidebar.tsx src/components/layout/MainDiagnosisArea.tsx src/pages/DiagnosisPage.tsx
git commit -m "feat: rewrite layout components for conversation UI"
```

---

## Task 12: Final verification — cargo check, cargo test, pnpm typecheck

**Files:** None (verification only)

- [ ] **Step 1: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors

- [ ] **Step 2: Run cargo test**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: All tests pass

- [ ] **Step 3: Run pnpm typecheck**

Run: `pnpm typecheck`
Expected: No errors

- [ ] **Step 4: Run the app (manual verification)**

Run: `pnpm tauri dev`
Expected: App launches, can type a message, send it, see streaming output from opencode.

Test multi-turn:
1. Type "Hello, what can you do?" → press Enter
2. Wait for response
3. Type "Tell me more" → press Enter (should continue same session)

Test stop:
1. Send a message
2. While agent is running, click "停止" button
3. Agent should stop, message marked as stopped

- [ ] **Step 5: Final commit (if any fixes needed)**

If any fixes were needed during verification, commit them now.

---

## Self-Review Notes

### Spec coverage check:
- ✅ Migration 0003 (Task 1)
- ✅ Session CRUD (Task 2)
- ✅ tokio-util dependency (Task 3)
- ✅ spawn.rs with opencode run args (Task 4)
- ✅ prompt.rs passthrough (Task 4)
- ✅ stream.rs NDJSON parsing (Task 5)
- ✅ lifecycle.rs commands (Task 6)
- ✅ lib.rs AppState + handlers (Task 6)
- ✅ Frontend types + IPC (Task 7)
- ✅ sessionStore rewrite (Task 8)
- ✅ Chat components (Tasks 9-10)
- ✅ Layout rewrites (Task 11)
- ✅ Final verification (Task 12)

### Key risks:
1. **opencode JSON format** — The event mapping is based on SDK type definitions. Real output may differ. Task 12 manual verification will catch this.
2. **Cancellation token ownership** — The `send_message_cmd` must clone the token before passing to both the spawned task and the RunningAgent map. This is noted in Task 6.
3. **Message accumulation edge cases** — The sessionStore handleEvent logic assumes events come in order (agent_started first, then llm_thinking). This is guaranteed by opencode's sequential output.
