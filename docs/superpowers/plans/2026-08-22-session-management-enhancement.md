# 会话管理功能增强 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement message persistence (full history), session archive/unarchive, and session delete for the Friday diagnosis agent.

**Architecture:** Backend persists messages at stream boundaries in `consume_stream` using an in-memory `MessageAccumulator` that flushes to SQLite on completion. Three-tier session lifecycle: `active` → `closed` → `archived`. Hard delete with manual cascade. Frontend loads history via IPC on session select, sidebar gains a toggle between main and archive views with context menus.

**Tech Stack:** Rust (sqlx, tokio, Tauri), React (zustand, Phosphor Icons, Tailwind CSS v4)

**Spec:** `docs/superpowers/specs/2026-08-22-session-management-enhancement-design.md`

---

## File Structure

### Backend (Rust — `src-tauri/`)

| File | Action | Responsibility |
|------|--------|---------------|
| `migrations/0005_session_messages.sql` | Create | `session_messages` + `session_message_parts` table schema |
| `src/infra/db.rs` | Modify | Load 0005 migration + add `archived_at` column |
| `src/app/session.rs` | Modify | Message CRUD functions, archive/unarchive/delete, `list_sessions` with `include_archived` param, `SessionRow` + `MessageRow` + `MessagePartRow` structs |
| `src/app/events.rs` | Modify | Add `SessionDeleted` variant to `AppEvent` enum |
| `src/agent/stream.rs` | Modify | `MessageAccumulator` struct, `consume_stream` gains `agent_message_id` param + persistence logic |
| `src/app/lifecycle.rs` | Modify | 4 new commands, `send_message_cmd` persistence steps, `list_sessions_cmd` gains `include_archived` param |
| `src/lib.rs` | Modify | Register 4 new handlers |

### Frontend (React — `src/`)

| File | Action | Responsibility |
|------|--------|---------------|
| `src/lib/types.ts` | Modify | `SessionRow.archived_at`, `MessageRow`, `MessagePartRow`, `session_deleted` event, status +"archived" |
| `src/lib/ipc.ts` | Modify | `listSessions(includeArchived)`, `getSessionMessages`, `archiveSession`, `unarchiveSession`, `deleteSession` |
| `src/store/sessionStore.ts` | Modify | `sidebarView`, `loadArchivedSessions`, `selectSession` loads history, `archiveSession`, `unarchiveSession`, `deleteSession`, `setSidebarView`, `convertMessages`, `handleEvent` + `session_deleted` |
| `src/components/layout/SessionSidebar.tsx` | Modify | Toggle bar, context menu, archive view rendering |
| `src/components/chat/DeleteConfirmDialog.tsx` | Create | Delete confirmation modal |
| `src/pages/DiagnosisPage.tsx` | Modify | Load archived sessions on mount |

---

## Task 1: Migration + DB Init

**Files:**
- Create: `src-tauri/migrations/0005_session_messages.sql`
- Modify: `src-tauri/src/infra/db.rs:4-19` (init function)
- Test: `src-tauri/src/infra/db.rs` (existing test module)

- [ ] **Step 1: Create the migration SQL file**

Create `src-tauri/migrations/0005_session_messages.sql`:

```sql
CREATE TABLE IF NOT EXISTS session_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    status TEXT,
    seq INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    part_type TEXT NOT NULL,
    seq INTEGER NOT NULL,
    text TEXT,
    tool_name TEXT,
    tool_args TEXT,
    tool_status TEXT,
    tool_output TEXT,
    tool_elapsed_ms INTEGER,
    FOREIGN KEY (message_id) REFERENCES session_messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_messages_session ON session_messages(session_id);
CREATE INDEX IF NOT EXISTS idx_session_message_parts_message ON session_message_parts(message_id);
```

- [ ] **Step 2: Write the failing test in `db.rs`**

Add this test to the `#[cfg(test)] mod tests` block at the end of `src-tauri/src/infra/db.rs`:

```rust
    #[tokio::test]
    async fn test_db_init_creates_message_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('session_messages', 'session_message_parts')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_db_init_creates_message_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('idx_session_messages_session', 'idx_session_message_parts_message')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_db_init_adds_archived_at_column() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'archived_at'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 1);
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_db_init_creates_message_tables test_db_init_creates_message_indexes test_db_init_adds_archived_at_column -- --exact`
Expected: FAIL — tables don't exist yet, column doesn't exist

- [ ] **Step 4: Implement — load migration + add `archived_at` column in `db.rs::init`**

In `src-tauri/src/infra/db.rs`, modify the `init` function. After the existing `add_column_if_not_exists` calls for `agent_session_id` and `title` (line 16-17), add:

```rust
    add_column_if_not_exists(&pool, "sessions", "archived_at", "TEXT").await?;
    let schema5 = include_str!("../../migrations/0005_session_messages.sql");
    sqlx::query(schema5).execute(&pool).await?;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_db_init_creates_message_tables test_db_init_creates_message_indexes test_db_init_adds_archived_at_column`
Expected: PASS

- [ ] **Step 6: Run full test suite to verify no regressions**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: All existing tests still pass

- [ ] **Step 7: Commit**

```bash
git add src-tauri/migrations/0005_session_messages.sql src-tauri/src/infra/db.rs
git commit -m "feat: add session_messages tables and archived_at column migration"
```

---

## Task 2: Session Row + Message Structs

**Files:**
- Modify: `src-tauri/src/app/session.rs:23-29` (SessionRow struct)
- Test: `src-tauri/src/app/session.rs` (existing test module)

- [ ] **Step 1: Update `SessionRow` struct and add `MessagePartRow` + `MessageRow`**

In `src-tauri/src/app/session.rs`, replace the existing `SessionRow` struct (lines 23-29):

```rust
#[derive(Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub archived_at: Option<String>,
}
```

Then add these new structs after `SessionRow`:

```rust
#[derive(Serialize)]
pub struct MessagePartRow {
    pub part_type: String,
    pub seq: i64,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub tool_status: Option<String>,
    pub tool_output: Option<String>,
    pub tool_elapsed_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct MessageRow {
    pub id: String,
    pub role: String,
    pub content: Option<String>,
    pub status: Option<String>,
    pub seq: i64,
    pub parts: Vec<MessagePartRow>,
}
```

- [ ] **Step 2: Update existing `list_sessions` and `get_session` to include `archived_at`**

In `src-tauri/src/app/session.rs`, replace the `list_sessions` function (lines 82-99):

```rust
pub async fn list_sessions(
    pool: &SqlitePool,
    include_archived: bool,
) -> Result<Vec<SessionRow>, sqlx::Error> {
    let rows = if include_archived {
        sqlx::query(
            "SELECT id, title, status, created_at, archived_at \
             FROM sessions WHERE status = 'archived' \
             ORDER BY archived_at DESC",
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, title, status, created_at, archived_at \
             FROM sessions WHERE status IN ('active', 'closed') \
             ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await?
    };

    rows.into_iter()
        .map(|row| {
            Ok(SessionRow {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                status: row.try_get("status")?,
                created_at: row.try_get("created_at")?,
                archived_at: row.try_get("archived_at")?,
            })
        })
        .collect()
}
```

Replace the `get_session` function (lines 101-121):

```rust
pub async fn get_session(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, title, status, created_at, archived_at FROM sessions WHERE id = ?",
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
            archived_at: row.try_get("archived_at")?,
        })
    })
    .transpose()
}
```

- [ ] **Step 3: Update existing tests that construct SessionRow or call list_sessions**

In the test module of `src-tauri/src/app/session.rs`, the test `test_list_sessions_returns_descending_by_created_at` calls `list_sessions(&pool)` — update it to `list_sessions(&pool, false)`:

```rust
    #[tokio::test]
    async fn test_list_sessions_returns_descending_by_created_at() {
        let pool = setup().await;
        let s1 = create_session(&pool, "first").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let s2 = create_session(&pool, "second").await.unwrap();

        let rows = list_sessions(&pool, false).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, s2.id.0);
        assert_eq!(rows[1].id, s1.id.0);
    }
```

The test `test_get_session_returns_row` checks `row.title` and `row.status` — these still work. No change needed there since `archived_at` will be `None` for a fresh session.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_list_sessions_returns_descending test_get_session_returns_row`
Expected: PASS

- [ ] **Step 5: Run `cargo check` to verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors (lifecycle.rs will fail because `list_sessions` signature changed — we fix that in Task 5. For now, fix only the call in lifecycle.rs to unblock compilation)

In `src-tauri/src/app/lifecycle.rs`, update the `list_sessions_cmd` call (line 206):

```rust
    session::list_sessions(&state.db, false)
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/session.rs src-tauri/src/app/lifecycle.rs
git commit -m "feat: add archived_at to SessionRow, MessageRow/MessagePartRow structs, list_sessions include_archived param"
```

---

## Task 3: Message CRUD Functions

**Files:**
- Modify: `src-tauri/src/app/session.rs` (add functions after existing ones, before tests)
- Test: `src-tauri/src/app/session.rs` (test module)

- [ ] **Step 1: Write failing tests for message CRUD**

Add these tests to the test module in `src-tauri/src/app/session.rs`:

```rust
    #[tokio::test]
    async fn test_next_message_seq_starts_at_zero() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let seq = next_message_seq(&pool, &session.id.0).await.unwrap();
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn test_next_message_seq_increments() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 1).await.unwrap();
        let seq = next_message_seq(&pool, &session.id.0).await.unwrap();
        assert_eq!(seq, 2);
    }

    #[tokio::test]
    async fn test_insert_message_returns_id() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let id = insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn test_update_message_status() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        update_message_status(&pool, &msg_id, "done").await.unwrap();

        let status: String = sqlx::query_scalar("SELECT status FROM session_messages WHERE id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "done");
    }

    #[tokio::test]
    async fn test_insert_text_part() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        insert_text_part(&pool, &msg_id, 0, "Hello world").await.unwrap();

        let text: String = sqlx::query_scalar("SELECT text FROM session_message_parts WHERE message_id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(text, "Hello world");
    }

    #[tokio::test]
    async fn test_insert_tool_part() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        insert_tool_part(&pool, &msg_id, 0, "bash", r#"{"command":"ls"}"#, "completed", "file1\nfile2", 800).await.unwrap();

        let (name, status, output, elapsed): (String, String, String, i64) =
            sqlx::query_as("SELECT tool_name, tool_status, tool_output, tool_elapsed_ms FROM session_message_parts WHERE message_id = ?")
                .bind(&msg_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "bash");
        assert_eq!(status, "completed");
        assert_eq!(output, "file1\nfile2");
        assert_eq!(elapsed, 800);
    }

    #[tokio::test]
    async fn test_get_session_messages_returns_messages_with_parts() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        let user_id = insert_message(&pool, &session.id.0, "user", Some("diagnose OOM"), Some("done"), 0).await.unwrap();
        let agent_id = insert_message(&pool, &session.id.0, "agent", None, Some("done"), 1).await.unwrap();
        insert_text_part(&pool, &agent_id, 0, "Analysis complete").await.unwrap();
        insert_tool_part(&pool, &agent_id, 1, "jstat", "{}", "completed", "output", 100).await.unwrap();

        let messages = get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages.len(), 2);

        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, Some("diagnose OOM".to_string()));
        assert_eq!(messages[0].seq, 0);
        assert_eq!(messages[0].parts.len(), 0);

        assert_eq!(messages[1].role, "agent");
        assert_eq!(messages[1].seq, 1);
        assert_eq!(messages[1].parts.len(), 2);
        assert_eq!(messages[1].parts[0].part_type, "text");
        assert_eq!(messages[1].parts[0].text, Some("Analysis complete".to_string()));
        assert_eq!(messages[1].parts[1].part_type, "tool");
        assert_eq!(messages[1].parts[1].tool_name, Some("jstat".to_string()));
    }

    #[tokio::test]
    async fn test_get_session_messages_empty_for_nonexistent() {
        let pool = setup().await;
        let messages = get_session_messages(&pool, "nonexistent").await.unwrap();
        assert!(messages.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_next_message_seq_starts_at_zero test_insert_message_returns_id test_update_message_status test_insert_text_part test_insert_tool_part test_get_session_messages_returns_messages_with_parts test_get_session_messages_empty_for_nonexistent`
Expected: FAIL — functions don't exist yet

- [ ] **Step 3: Implement the message CRUD functions**

Add these functions to `src-tauri/src/app/session.rs`, after the existing `update_agent_session_id` function (after line 146) and before the `#[cfg(test)]` block:

```rust
pub async fn next_message_seq(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<i64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_messages WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

pub async fn insert_message(
    pool: &SqlitePool,
    session_id: &str,
    role: &str,
    content: Option<&str>,
    status: Option<&str>,
    seq: i64,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    sqlx::query(
        "INSERT INTO session_messages (id, session_id, role, content, status, seq, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(status)
    .bind(seq)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn update_message_status(
    pool: &SqlitePool,
    message_id: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE session_messages SET status = ? WHERE id = ?")
        .bind(status)
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_text_part(
    pool: &SqlitePool,
    message_id: &str,
    seq: i64,
    text: &str,
) -> Result<(), sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO session_message_parts (id, message_id, part_type, seq, text) \
         VALUES (?, ?, 'text', ?, ?)",
    )
    .bind(&id)
    .bind(message_id)
    .bind(seq)
    .bind(text)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_tool_part(
    pool: &SqlitePool,
    message_id: &str,
    seq: i64,
    tool_name: &str,
    tool_args: &str,
    tool_status: &str,
    tool_output: &str,
    tool_elapsed_ms: i64,
) -> Result<(), sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO session_message_parts \
         (id, message_id, part_type, seq, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms) \
         VALUES (?, ?, 'tool', ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(message_id)
    .bind(seq)
    .bind(tool_name)
    .bind(tool_args)
    .bind(tool_status)
    .bind(tool_output)
    .bind(tool_elapsed_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_session_messages(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    let messages: Vec<(String, String, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, role, content, status, seq FROM session_messages \
         WHERE session_id = ? ORDER BY seq ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(messages.len());
    for (msg_id, role, content, status, seq) in messages {
        let parts: Vec<(String, String, i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT id, part_type, seq, text, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms \
             FROM session_message_parts WHERE message_id = ? ORDER BY seq ASC",
        )
        .bind(&msg_id)
        .fetch_all(pool)
        .await?;

        let part_rows: Vec<MessagePartRow> = parts
            .into_iter()
            .map(|(_, part_type, seq, text, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms)| MessagePartRow {
                part_type,
                seq,
                text,
                tool_name,
                tool_args,
                tool_status,
                tool_output,
                tool_elapsed_ms,
            })
            .collect();

        result.push(MessageRow {
            id: msg_id,
            role,
            content,
            status,
            seq,
            parts: part_rows,
        });
    }
    Ok(result)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_next_message_seq_starts_at_zero test_next_message_seq_increments test_insert_message_returns_id test_update_message_status test_insert_text_part test_insert_tool_part test_get_session_messages_returns_messages_with_parts test_get_session_messages_empty_for_nonexistent`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/app/session.rs
git commit -m "feat: add message CRUD functions (insert, update, get, parts)"
```

---

## Task 4: Archive/Unarchive/Delete Session Functions

**Files:**
- Modify: `src-tauri/src/app/session.rs` (add functions)
- Test: `src-tauri/src/app/session.rs` (test module)

- [ ] **Step 1: Write failing tests for archive/unarchive/delete + list_sessions filtering**

Add these tests to the test module in `src-tauri/src/app/session.rs`:

```rust
    #[tokio::test]
    async fn test_archive_session_sets_status_and_timestamp() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        archive_session(&pool, &session.id.0).await.unwrap();

        let (status, archived_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, archived_at FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "archived");
        assert!(archived_at.is_some());
    }

    #[tokio::test]
    async fn test_unarchive_session_resets_status_and_clears_timestamp() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        archive_session(&pool, &session.id.0).await.unwrap();
        unarchive_session(&pool, &session.id.0).await.unwrap();

        let (status, archived_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, archived_at FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "closed");
        assert!(archived_at.is_none());
    }

    #[tokio::test]
    async fn test_delete_session_removes_row() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        delete_session(&pool, &session.id.0).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_delete_session_cascades_messages_and_parts() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        insert_text_part(&pool, &msg_id, 0, "text content").await.unwrap();

        delete_session(&pool, &session.id.0).await.unwrap();

        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(msg_count, 0);

        let part_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_message_parts WHERE message_id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(part_count, 0);
    }

    #[tokio::test]
    async fn test_list_sessions_excludes_archived() {
        let pool = setup().await;
        let s1 = create_session(&pool, "active session").await.unwrap();
        let s2 = create_session(&pool, "will be archived").await.unwrap();
        archive_session(&pool, &s2.id.0).await.unwrap();

        let rows = list_sessions(&pool, false).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, s1.id.0);
    }

    #[tokio::test]
    async fn test_list_sessions_archived_only() {
        let pool = setup().await;
        let s1 = create_session(&pool, "active session").await.unwrap();
        let s2 = create_session(&pool, "archived session").await.unwrap();
        archive_session(&pool, &s2.id.0).await.unwrap();

        let rows = list_sessions(&pool, true).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, s2.id.0);
        assert_eq!(rows[0].status, "archived");
        assert!(rows[0].archived_at.is_some());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_archive_session_sets_status_and_timestamp test_unarchive_session_resets_status_and_clears_timestamp test_delete_session_removes_row test_delete_session_cascades_messages_and_parts test_list_sessions_excludes_archived test_list_sessions_archived_only`
Expected: FAIL — functions don't exist

- [ ] **Step 3: Implement archive/unarchive/delete functions**

Add these functions to `src-tauri/src/app/session.rs`, after `get_session_messages` and before the `#[cfg(test)]` block:

```rust
pub async fn archive_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = now_iso8601();
    sqlx::query("UPDATE sessions SET status = 'archived', archived_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn unarchive_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sessions SET status = 'closed', archived_at = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    // Manual cascade: SQLite doesn't enforce foreign keys by default
    let message_ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM session_messages WHERE session_id = ?")
        .bind(id)
        .fetch_all(pool)
        .await?;

    for (msg_id,) in &message_ids {
        sqlx::query("DELETE FROM session_message_parts WHERE message_id = ?")
            .bind(msg_id)
            .execute(pool)
            .await?;
    }

    sqlx::query("DELETE FROM session_messages WHERE session_id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_archive_session_sets_status_and_timestamp test_unarchive_session_resets_status_and_clears_timestamp test_delete_session_removes_row test_delete_session_cascades_messages_and_parts test_list_sessions_excludes_archived test_list_sessions_archived_only`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/app/session.rs
git commit -m "feat: add archive/unarchive/delete session functions with manual cascade"
```

---

## Task 5: SessionDeleted AppEvent

**Files:**
- Modify: `src-tauri/src/app/events.rs:5-47` (AppEvent enum)
- Test: `src-tauri/src/app/events.rs` (test module)

- [ ] **Step 1: Write failing test for SessionDeleted serialization**

Add this test to the test module in `src-tauri/src/app/events.rs`:

```rust
    #[test]
    fn test_session_deleted_serialization() {
        let event = AppEvent::SessionDeleted {
            session_id: "s99".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("session_deleted"));
        assert!(json.contains("s99"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_session_deleted_serialization`
Expected: FAIL — `SessionDeleted` variant doesn't exist

- [ ] **Step 3: Add SessionDeleted variant to AppEvent enum**

In `src-tauri/src/app/events.rs`, add after `SessionClosed` (line 44-46):

```rust
    SessionDeleted {
        session_id: String,
    },
```

The full enum should end like:
```rust
    SessionClosed {
        session_id: String,
    },
    SessionDeleted {
        session_id: String,
    },
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_session_deleted_serialization`
Expected: PASS

- [ ] **Step 5: Run `cargo check` to verify no compilation errors**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/events.rs
git commit -m "feat: add SessionDeleted variant to AppEvent enum"
```

---

## Task 6: MessageAccumulator + consume_stream Persistence

**Files:**
- Modify: `src-tauri/src/agent/stream.rs` (add MessageAccumulator, modify consume_stream)
- Test: `src-tauri/src/agent/stream.rs` (test module)

- [ ] **Step 1: Write failing tests for MessageAccumulator**

Add these tests to the test module in `src-tauri/src/agent/stream.rs`:

```rust
    use crate::infra::db;

    fn make_llm_thinking(token: &str) -> AppEvent {
        AppEvent::LlmThinking {
            session_id: "s1".to_string(),
            token: token.to_string(),
        }
    }

    fn make_tool_executing(name: &str) -> AppEvent {
        AppEvent::ToolExecuting {
            session_id: "s1".to_string(),
            tool: name.to_string(),
            args: serde_json::Value::Null,
        }
    }

    fn make_tool_result(name: &str, output: &str, elapsed: u64) -> AppEvent {
        AppEvent::ToolResult {
            session_id: "s1".to_string(),
            tool: name.to_string(),
            output: serde_json::Value::String(output.to_string()),
            elapsed_ms: elapsed,
        }
    }

    #[tokio::test]
    async fn test_accumulator_text_accumulation() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        acc.handle_event(&make_llm_thinking("Hello "));
        acc.handle_event(&make_llm_thinking("world!"));

        acc.flush_to_db(&pool).await;
        crate::app::session::update_message_status(&pool, &msg_id, "done").await.unwrap();

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].parts.len(), 1);
        assert_eq!(messages[0].parts[0].part_type, "text");
        assert_eq!(messages[0].parts[0].text, Some("Hello world!".to_string()));
    }

    #[tokio::test]
    async fn test_accumulator_tool_result_persists_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        acc.handle_event(&make_tool_executing("bash"));
        acc.handle_event(&make_tool_result("bash", "file1\nfile2", 500));
        acc.handle_event(&make_llm_thinking("Done."));
        acc.flush_to_db(&pool).await;

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages[0].parts.len(), 2);
        assert_eq!(messages[0].parts[0].part_type, "tool");
        assert_eq!(messages[0].parts[0].tool_name, Some("bash".to_string()));
        assert_eq!(messages[0].parts[0].tool_status, Some("completed".to_string()));
        assert_eq!(messages[0].parts[1].part_type, "text");
        assert_eq!(messages[0].parts[1].text, Some("Done.".to_string()));
    }

    #[tokio::test]
    async fn test_accumulator_multiple_text_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        // Text, then tool, then more text — should create 2 text parts
        acc.handle_event(&make_llm_thinking("First text"));
        acc.handle_event(&make_tool_executing("bash"));
        acc.handle_event(&make_tool_result("bash", "output", 100));
        acc.handle_event(&make_llm_thinking("Second text"));
        acc.flush_to_db(&pool).await;

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages[0].parts.len(), 3);
        assert_eq!(messages[0].parts[0].part_type, "text");
        assert_eq!(messages[0].parts[0].text, Some("First text".to_string()));
        assert_eq!(messages[0].parts[1].part_type, "tool");
        assert_eq!(messages[0].parts[2].part_type, "text");
        assert_eq!(messages[0].parts[2].text, Some("Second text".to_string()));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_accumulator_text_accumulation test_accumulator_tool_result_persists_immediately test_accumulator_multiple_text_parts`
Expected: FAIL — `MessageAccumulator` doesn't exist

- [ ] **Step 3: Implement MessageAccumulator**

Add this struct and implementation in `src-tauri/src/agent/stream.rs`, after the `compute_elapsed_ms` function (after line 213) and before `read_stderr_lines`:

```rust
/// Accumulates message parts in memory during streaming, flushing to DB
/// at stream end. Text accumulates between tool calls.
struct MessageAccumulator {
    message_id: String,
    parts: Vec<AccumulatedPart>,
    current_text: String,
    pending_tool_args: Option<String>,
    pending_tool_name: Option<String>,
}

enum AccumulatedPart {
    Text(String),
    Tool {
        name: String,
        args: String,
        status: String,
        output: String,
        elapsed_ms: i64,
    },
}

impl MessageAccumulator {
    fn new(message_id: String) -> Self {
        Self {
            message_id,
            parts: Vec::new(),
            current_text: String::new(),
            pending_tool_args: None,
            pending_tool_name: None,
        }
    }

    /// Handle an AppEvent by accumulating in memory.
    fn handle_event(&mut self, event: &AppEvent) {
        match event {
            AppEvent::LlmThinking { token, .. } => {
                self.current_text.push_str(token);
            }
            AppEvent::ToolExecuting { tool, args, .. } => {
                self.flush_current_text();
                self.pending_tool_args = Some(serde_json::to_string(args).unwrap_or_default());
                self.pending_tool_name = Some(tool.clone());
            }
            AppEvent::ToolResult { tool, output, elapsed_ms, .. } => {
                self.flush_current_text();
                let args = self.pending_tool_args.take().unwrap_or_default();
                let output_str = match output {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                self.parts.push(AccumulatedPart::Tool {
                    name: tool.clone(),
                    args,
                    status: "completed".to_string(),
                    output: output_str,
                    elapsed_ms: *elapsed_ms as i64,
                });
            }
            _ => {}
        }
    }

    /// Push accumulated text as a text part, if non-empty.
    fn flush_current_text(&mut self) {
        if !self.current_text.is_empty() {
            self.parts.push(AccumulatedPart::Text(std::mem::take(&mut self.current_text)));
        }
    }

    /// Write all accumulated parts to the DB in seq order.
    async fn flush_to_db(&mut self, pool: &sqlx::SqlitePool) {
        self.flush_current_text();
        for (seq, part) in self.parts.iter().enumerate() {
            let seq = seq as i64;
            match part {
                AccumulatedPart::Text(text) => {
                    if let Err(e) = crate::app::session::insert_text_part(
                        pool, &self.message_id, seq, text,
                    ).await {
                        tracing::error!(?e, message_id = %self.message_id, seq, "failed to persist text part");
                    }
                }
                AccumulatedPart::Tool { name, args, status, output, elapsed_ms } => {
                    if let Err(e) = crate::app::session::insert_tool_part(
                        pool, &self.message_id, seq, name, args, status, output, *elapsed_ms,
                    ).await {
                        tracing::error!(?e, message_id = %self.message_id, seq, tool = %name, "failed to persist tool part");
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Modify `consume_stream` to accept `agent_message_id` and use MessageAccumulator**

In `src-tauri/src/agent/stream.rs`, update the `consume_stream` function signature (line 243-250). Add `agent_message_id: String` parameter:

```rust
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    agent_message_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
) {
```

Inside the function body, after `let mut line_count = 0u64;` (line 257), add:

```rust
    let mut accumulator = MessageAccumulator::new(agent_message_id.clone());
```

In the event loop, after `let events = parse_event(&line, &session_id);` and before the `for event in events` loop (around line 285-289), modify to also feed the accumulator:

```rust
                        let events = parse_event(&line, &session_id);
                        for event in &events {
                            accumulator.handle_event(event);
                        }
                        for event in events {
                            tracing::debug!(event_type = ?std::mem::discriminant(&event), "emitting event");
                            bus.emit(&session_id, event);
                        }
```

After the natural exit (after `child.wait().await` and the `DiagnosisDone`/`AgentCrashed` emit, around line 340), before the final `map.remove`, add:

```rust
    // Flush accumulated message parts to DB
    let final_status = if exit_ok { "done" } else { "error" };
    accumulator.flush_to_db(&pool).await;
    if let Err(e) = crate::app::session::update_message_status(&pool, &agent_message_id, final_status).await {
        tracing::error!(?e, message_id = %agent_message_id, "failed to update message status");
    }
```

For the cancellation path (around line 301-309), before `return;`, add:

```rust
                // Flush accumulated parts before stopping
                accumulator.flush_to_db(&pool).await;
                if let Err(e) = crate::app::session::update_message_status(&pool, &agent_message_id, "stopped").await {
                    tracing::error!(?e, message_id = %agent_message_id, "failed to update message status on stop");
                }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_accumulator_text_accumulation test_accumulator_tool_result_persists_immediately test_accumulator_multiple_text_parts`
Expected: PASS

- [ ] **Step 6: Run `cargo check` to verify compilation (will fail — `send_message_cmd` in lifecycle.rs needs to pass `agent_message_id`)**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: FAIL — `consume_stream` call in `lifecycle.rs` doesn't pass `agent_message_id` yet. This is fixed in Task 7. For now, we accept this and move on.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent/stream.rs
git commit -m "feat: add MessageAccumulator and integrate persistence into consume_stream"
```

---

## Task 7: Lifecycle Commands + send_message_cmd Persistence

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs` (add commands, modify send_message_cmd + list_sessions_cmd)
- Modify: `src-tauri/src/lib.rs:52-64` (register handlers)
- Test: `src-tauri/src/app/lifecycle.rs` (test module)

- [ ] **Step 1: Write failing tests for new commands**

Add these tests to the test module in `src-tauri/src/app/lifecycle.rs`:

```rust
    #[tokio::test]
    async fn test_delete_session_removes_session() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let s = session::create_session(&pool, "to delete").await.unwrap();

        session::delete_session(&pool, &s.id.0).await.unwrap();

        let row = session::get_session(&pool, &s.id.0).await.unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn test_archive_then_unarchive_session() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let s = session::create_session(&pool, "test").await.unwrap();

        session::archive_session(&pool, &s.id.0).await.unwrap();
        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "archived");

        session::unarchive_session(&pool, &s.id.0).await.unwrap();
        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "closed");
    }
```

- [ ] **Step 2: Run tests to verify they pass (they test session functions, not commands yet)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --exact test_delete_session_removes_session test_archive_then_unarchive_session`
Expected: PASS (functions already exist from Task 4)

- [ ] **Step 3: Modify `send_message_cmd` to persist user + agent messages**

In `src-tauri/src/app/lifecycle.rs`, in the `send_message_cmd` function, after the session is resolved and the agent is spawned (around line 109-120), we need to add persistence. 

Find the section after `let agent_process = spawn_active(...)` succeeds and before `let pid = agent_process.pid;`. Insert the user message and agent message persistence:

Before the `spawn_active` call (after the "agent is already running" check), add:

```rust
    // Persist user message
    let user_seq = session::next_message_seq(&pool, &friday_session_id)
        .await
        .map_err(|e| {
            tracing::error!(?e, "failed to get next message seq");
            e.to_string()
        })?;
    session::insert_message(&pool, &friday_session_id, "user", Some(&message), Some("done"), user_seq)
        .await
        .map_err(|e| {
            tracing::error!(?e, "failed to persist user message");
            e.to_string()
        })?;

    // Create agent message record (status=streaming, will be finalized by consume_stream)
    let agent_seq = session::next_message_seq(&pool, &friday_session_id)
        .await
        .map_err(|e| e.to_string())?;
    let agent_message_id = session::insert_message(
        &pool, &friday_session_id, "agent", None, Some("streaming"), agent_seq,
    )
    .await
    .map_err(|e| {
        tracing::error!(?e, "failed to create agent message record");
        e.to_string()
    })?;
    tracing::info!(agent_message_id = %agent_message_id, "created agent message record");
```

Then in the `tokio::spawn` block that calls `consume_stream`, pass `agent_message_id`:

```rust
    let agent_message_id_clone = agent_message_id.clone();
    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            agent_message_id_clone,
            pool_clone,
            agents_clone,
            cancel_for_task,
        )
        .await;
    });
```

- [ ] **Step 4: Modify `list_sessions_cmd` to accept `include_archived` parameter**

In `src-tauri/src/app/lifecycle.rs`, replace the `list_sessions_cmd` function (lines 201-209):

```rust
#[tauri::command]
pub async fn list_sessions_cmd(
    state: State<'_, crate::AppState>,
    include_archived: bool,
) -> Result<Vec<session::SessionRow>, String> {
    tracing::info!(include_archived, "list_sessions_cmd called");
    session::list_sessions(&state.db, include_archived)
        .await
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 5: Add the 4 new Tauri commands**

Add these functions to `src-tauri/src/app/lifecycle.rs`, after the existing `set_log_level_cmd` function and before the test module:

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_session_messages_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<Vec<session::MessageRow>, String> {
    tracing::info!(session_id = %session_id, "get_session_messages_cmd called");
    session::get_session_messages(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn archive_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "archive_session_cmd called");
    session::archive_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn unarchive_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "unarchive_session_cmd called");
    session::unarchive_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "delete_session_cmd called");
    // Stop agent if running
    stop_agent_for_session(&state.agents, &session_id).await?;

    // Delete session with manual cascade
    session::delete_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())?;

    // Emit SessionDeleted
    state.bus.emit(
        &session_id,
        AppEvent::SessionDeleted {
            session_id: session_id.clone(),
        },
    );

    Ok(())
}
```

- [ ] **Step 6: Register new handlers in `lib.rs`**

In `src-tauri/src/lib.rs`, add the 4 new commands to the `invoke_handler` macro (after line 57, `app::lifecycle::list_sessions_cmd,`):

```rust
            app::lifecycle::get_session_messages_cmd,
            app::lifecycle::archive_session_cmd,
            app::lifecycle::unarchive_session_cmd,
            app::lifecycle::delete_session_cmd,
```

- [ ] **Step 7: Run `cargo check` to verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors

- [ ] **Step 8: Run full test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: All tests pass

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs src-tauri/src/lib.rs
git commit -m "feat: add 4 new lifecycle commands, persist messages in send_message_cmd, list_sessions include_archived param"
```

---

## Task 8: Frontend Types + IPC

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`

- [ ] **Step 1: Update types.ts**

In `src/lib/types.ts`, update the `SessionRow` interface (lines 41-46) to add `archived_at` and the `"archived"` status:

```ts
export interface SessionRow {
  id: string;
  title: string | null;
  status: "active" | "closed" | "archived";
  created_at: string;
  archived_at: string | null;
}
```

Add the `session_deleted` variant to the `AppEvent` union (after `session_closed`, line 23):

```ts
  | { type: "session_closed"; session_id: string }
  | { type: "session_deleted"; session_id: string };
```

Add `MessagePartRow` and `MessageRow` interfaces at the end of the file:

```ts
export interface MessagePartRow {
  part_type: "text" | "tool";
  seq: number;
  text: string | null;
  tool_name: string | null;
  tool_args: string | null;
  tool_status: string | null;
  tool_output: string | null;
  tool_elapsed_ms: number | null;
}

export interface MessageRow {
  id: string;
  role: "user" | "agent";
  content: string | null;
  status: string | null;
  seq: number;
  parts: MessagePartRow[];
}
```

- [ ] **Step 2: Update ipc.ts**

In `src/lib/ipc.ts`, update `listSessions` (line 9) to accept `includeArchived`:

```ts
export async function listSessions(includeArchived: boolean): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions_cmd", { includeArchived });
}
```

Add the new IPC functions after `closeSession` (after line 19):

```ts
export async function getSessionMessages(sessionId: string): Promise<MessageRow[]> {
  return invoke<MessageRow[]>("get_session_messages_cmd", { sessionId });
}

export async function archiveSession(sessionId: string): Promise<void> {
  return invoke<void>("archive_session_cmd", { sessionId });
}

export async function unarchiveSession(sessionId: string): Promise<void> {
  return invoke<void>("unarchive_session_cmd", { sessionId });
}

export async function deleteSession(sessionId: string): Promise<void> {
  return invoke<void>("delete_session_cmd", { sessionId });
}
```

Add `MessageRow` to the import from types (line 3):

```ts
import type { EventPayload, AgentRow, SessionRow, MessageRow } from "@/lib/types";
```

- [ ] **Step 3: Run typecheck**

Run: `pnpm typecheck`
Expected: Errors in `sessionStore.ts` and `DiagnosisPage.tsx` (callers of `listSessions` need updating) — we fix those in Task 9. For now, verify only types.ts and ipc.ts themselves are correct.

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts
git commit -m "feat: add MessageRow types, session_deleted event, update listSessions signature"
```

---

## Task 9: Frontend sessionStore

**Files:**
- Modify: `src/store/sessionStore.ts`
- Modify: `src/pages/DiagnosisPage.tsx`

- [ ] **Step 1: Update sessionStore — imports, interface, and state**

In `src/store/sessionStore.ts`, update the imports (line 3):

```ts
import { sendMessage as ipcSendMessage, stopAgent, listSessions, onAppEvent, getSessionMessages, archiveSession as ipcArchiveSession, unarchiveSession as ipcUnarchiveSession, deleteSession as ipcDeleteSession } from "@/lib/ipc";
import type { SessionRow, ChatMessage, ChatPart, AppEvent, MessageRow } from "@/lib/types";
```

Update the `SessionStore` interface to add `sidebarView` and new actions:

```ts
interface SessionStore {
  sessions: SessionRow[];
  archivedSessions: SessionRow[];
  currentSessionId: string | null;
  messagesBySession: Record<string, ChatMessage[]>;
  agentRunning: Record<string, boolean>;
  inputText: string;
  sidebarView: "sessions" | "archived";
  eventUnlisten: (() => void) | null | string;

  loadSessions: () => Promise<void>;
  loadArchivedSessions: () => Promise<void>;
  selectSession: (id: string) => void;
  newSession: () => void;
  setInputText: (text: string) => void;
  sendMessage: () => Promise<void>;
  stopAgent: () => Promise<void>;
  initEventListener: () => Promise<void>;
  handleEvent: (payload: { session_id: string; event: AppEvent }) => void;
  setSidebarView: (view: "sessions" | "archived") => void;
  archiveSession: (id: string) => Promise<void>;
  unarchiveSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
}
```

- [ ] **Step 2: Update store initial state and implement new actions**

In the `create<SessionStore>((set, get) => ({...}))` block, add `archivedSessions` and `sidebarView` to initial state:

```ts
  sessions: [],
  archivedSessions: [],
  currentSessionId: null,
  messagesBySession: {},
  agentRunning: {},
  inputText: "",
  sidebarView: "sessions",
  eventUnlisten: null,
```

Update `loadSessions` to pass `false`:

```ts
  loadSessions: async () => {
    try {
      const sessions = await listSessions(false);
      set({ sessions });
    } catch (e) {
      console.error("Failed to load sessions:", errMsg(e));
    }
  },
```

Add `loadArchivedSessions`, `setSidebarView`, `archiveSession`, `unarchiveSession`, `deleteSession`:

```ts
  loadArchivedSessions: async () => {
    try {
      const archivedSessions = await listSessions(true);
      set({ archivedSessions });
    } catch (e) {
      console.error("Failed to load archived sessions:", errMsg(e));
    }
  },

  setSidebarView: (view) => {
    if (view === "archived") {
      get().loadArchivedSessions();
    }
    set({ sidebarView: view });
  },

  archiveSession: async (id) => {
    try {
      await ipcArchiveSession(id);
      set((state) => ({
        sessions: state.sessions.filter((s) => s.id !== id),
        archivedSessions: [...state.archivedSessions, ...state.sessions.filter((s) => s.id === id)],
      }));
    } catch (e) {
      console.error("Failed to archive session:", errMsg(e));
    }
  },

  unarchiveSession: async (id) => {
    try {
      await ipcUnarchiveSession(id);
      const { archivedSessions } = get();
      const restored = archivedSessions.find((s) => s.id === id);
      set((state) => ({
        archivedSessions: state.archivedSessions.filter((s) => s.id !== id),
        sessions: restored ? [...state.sessions, restored] : state.sessions,
      }));
    } catch (e) {
      console.error("Failed to unarchive session:", errMsg(e));
    }
  },

  deleteSession: async (id) => {
    try {
      await ipcDeleteSession(id);
      const { messagesBySession, currentSessionId } = get();
      const newMessages = { ...messagesBySession };
      delete newMessages[id];
      set((state) => ({
        sessions: state.sessions.filter((s) => s.id !== id),
        archivedSessions: state.archivedSessions.filter((s) => s.id !== id),
        messagesBySession: newMessages,
        currentSessionId: currentSessionId === id ? null : currentSessionId,
      }));
    } catch (e) {
      console.error("Failed to delete session:", errMsg(e));
    }
  },
```

- [ ] **Step 3: Update `selectSession` to load message history**

Replace the existing `selectSession` (line 46):

```ts
  selectSession: async (id) => {
    set({ currentSessionId: id });
    const { messagesBySession } = get();
    if (!messagesBySession[id]) {
      try {
        const rows = await getSessionMessages(id);
        const messages = convertMessages(rows);
        set((state) => ({
          messagesBySession: { ...state.messagesBySession, [id]: messages },
        }));
      } catch (e) {
        console.error("Failed to load session messages:", errMsg(e));
      }
    }
  },
```

- [ ] **Step 4: Add `convertMessages` helper function**

Add this function before `export const useSessionStore`:

```ts
function convertMessages(rows: MessageRow[]): ChatMessage[] {
  return rows.map((row) => {
    const parts: ChatPart[] = row.parts.map((p) => {
      if (p.part_type === "text") {
        return { type: "text", text: p.text ?? "" };
      } else {
        let args: unknown;
        try {
          args = p.tool_args ? JSON.parse(p.tool_args) : null;
        } catch {
          args = p.tool_args;
        }
        return {
          type: "tool",
          tool: {
            name: p.tool_name ?? "unknown",
            args,
            status: (p.tool_status as "running" | "completed" | "error") ?? "completed",
            output: p.tool_output ?? undefined,
            elapsedMs: p.tool_elapsed_ms ?? undefined,
          },
        };
      }
    });

    return {
      id: row.id,
      role: row.role as "user" | "agent",
      content: row.content ?? "",
      parts,
      status: (row.status as ChatMessage["status"]) ?? "done",
    };
  });
}
```

- [ ] **Step 5: Handle `session_deleted` event in `handleEvent`**

In the `handleEvent` function, add handling for `session_deleted` after the `session_closed` block (after line 315):

```ts
    if (event.type === "session_deleted") {
      const { messagesBySession, currentSessionId } = get();
      const newMessages = { ...messagesBySession };
      delete newMessages[session_id];
      set({
        sessions: get().sessions.filter((s) => s.id !== session_id),
        archivedSessions: get().archivedSessions.filter((s) => s.id !== session_id),
        messagesBySession: newMessages,
        currentSessionId: currentSessionId === session_id ? null : currentSessionId,
      });
      return;
    }
```

- [ ] **Step 6: Update DiagnosisPage to load archived sessions**

In `src/pages/DiagnosisPage.tsx`, add `loadArchivedSessions`:

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
  const loadArchivedSessions = useSessionStore((s) => s.loadArchivedSessions);
  const initEventListener = useSessionStore((s) => s.initEventListener);

  useEffect(() => {
    refreshAgents();
    loadSessions();
    loadArchivedSessions();
    initEventListener();
  }, [refreshAgents, loadSessions, loadArchivedSessions, initEventListener]);

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

- [ ] **Step 7: Run typecheck**

Run: `pnpm typecheck`
Expected: No errors (SessionSidebar.tsx may still need updating — that's Task 10)

- [ ] **Step 8: Commit**

```bash
git add src/store/sessionStore.ts src/pages/DiagnosisPage.tsx
git commit -m "feat: sessionStore — sidebarView, load history, archive/unarchive/delete actions, convertMessages"
```

---

## Task 10: SessionSidebar — Toggle + Context Menu + Archive View

**Files:**
- Modify: `src/components/layout/SessionSidebar.tsx`

- [ ] **Step 1: Rewrite SessionSidebar with toggle bar, context menu, and archive view**

Replace the entire content of `src/components/layout/SessionSidebar.tsx`:

```tsx
import { useState, useRef, useEffect } from "react";
import { ChatCircle, Plus, DotsThree, Archive, Trash, ArrowUUpLeft } from "@phosphor-icons/react";
import { useSessionStore } from "@/store/sessionStore";
import { DeleteConfirmDialog } from "@/components/chat/DeleteConfirmDialog";

export function SessionSidebar() {
  const sessions = useSessionStore((s) => s.sessions);
  const archivedSessions = useSessionStore((s) => s.archivedSessions);
  const currentSessionId = useSessionStore((s) => s.currentSessionId);
  const agentRunning = useSessionStore((s) => s.agentRunning);
  const sidebarView = useSessionStore((s) => s.sidebarView);
  const selectSession = useSessionStore((s) => s.selectSession);
  const newSession = useSessionStore((s) => s.newSession);
  const setSidebarView = useSessionStore((s) => s.setSidebarView);
  const archiveSession = useSessionStore((s) => s.archiveSession);
  const unarchiveSession = useSessionStore((s) => s.unarchiveSession);
  const deleteSession = useSessionStore((s) => s.deleteSession);

  const [contextMenu, setContextMenu] = useState<{ sessionId: string; x: number; y: number } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const handleContextMenu = (e: React.MouseEvent, sessionId: string) => {
    e.preventDefault();
    setContextMenu({ sessionId, x: e.clientX, y: e.clientY });
  };

  const handleDotsClick = (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setContextMenu({ sessionId, x: rect.right, y: rect.bottom });
  };

  const handleArchive = async () => {
    if (!contextMenu) return;
    await archiveSession(contextMenu.sessionId);
    setContextMenu(null);
  };

  const handleUnarchive = async () => {
    if (!contextMenu) return;
    await unarchiveSession(contextMenu.sessionId);
    setContextMenu(null);
  };

  const handleDeleteClick = () => {
    if (!contextMenu) return;
    setDeleteTarget(contextMenu.sessionId);
    setContextMenu(null);
  };

  const handleDeleteConfirm = async () => {
    if (!deleteTarget) return;
    await deleteSession(deleteTarget);
    setDeleteTarget(null);
  };

  const isArchiveView = sidebarView === "archived";
  const displaySessions = isArchiveView ? archivedSessions : sessions;

  const renderSessionItem = (s: { id: string; title: string | null; status: string; created_at: string; archived_at?: string | null }) => {
    const isActive = s.id === currentSessionId;
    const isRunning = agentRunning[s.id] ?? false;
    const isClosed = s.status === "closed";
    const isArchived = s.status === "archived";
    const dimmed = isClosed || isArchived;

    return (
      <div
        key={s.id}
        onClick={() => selectSession(s.id)}
        onContextMenu={(e) => handleContextMenu(e, s.id)}
        className={`relative w-full text-left px-3 py-2 rounded-lg mb-0.5 transition-colors cursor-pointer ${
          isActive
            ? "bg-surface-2 border-l-2 border-success pl-[10px]"
            : "hover:bg-surface-2"
        } ${dimmed ? "opacity-60" : ""}`}
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
          <button
            onClick={(e) => handleDotsClick(e, s.id)}
            className="text-muted-foreground hover:text-foreground shrink-0"
            aria-label="会话操作"
          >
            <DotsThree size={16} weight="bold" />
          </button>
        </div>
        <span
          className="text-xs text-muted-foreground"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {isArchived && s.archived_at
            ? `归档于 ${s.archived_at.slice(0, 10)}`
            : s.created_at.slice(0, 10)}
        </span>
      </div>
    );
  };

  return (
    <aside className="w-60 shrink-0 border-r border-border bg-surface-1 flex flex-col">
      {/* Toggle bar */}
      <div className="flex border-b border-border h-10 shrink-0">
        <button
          onClick={() => setSidebarView("sessions")}
          className={`flex-1 flex items-center justify-center text-xs font-medium transition-colors ${
            !isArchiveView
              ? "text-foreground border-b-2 border-success"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          会话
        </button>
        <button
          onClick={() => setSidebarView("archived")}
          className={`flex-1 flex items-center justify-center text-xs font-medium transition-colors ${
            isArchiveView
              ? "text-foreground border-b-2 border-success"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          归档
        </button>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto flex flex-col">
        {isArchiveView && displaySessions.length > 0 && (
          <div className="text-xs text-muted-foreground uppercase tracking-wide px-4 py-2">
            {displaySessions.length} 个已归档会话
          </div>
        )}

        {displaySessions.length === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center px-6 py-8 select-none">
            <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-muted/40 border border-border mb-3">
              {isArchiveView ? (
                <Archive size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
              ) : (
                <ChatCircle size={24} weight="regular" className="text-muted-foreground" aria-hidden="true" />
              )}
            </div>
            <p className="text-muted-foreground text-xs text-center leading-relaxed">
              {isArchiveView ? "暂无归档会话" : "暂无诊断会话"}
            </p>
            {!isArchiveView && (
              <p className="text-muted-foreground/60 text-xs text-center mt-1">
                在下方输入框描述问题开始
              </p>
            )}
          </div>
        ) : (
          <div className="px-2">
            {displaySessions.map(renderSessionItem)}
          </div>
        )}
      </div>

      {/* New session button — only in main view */}
      {!isArchiveView && (
        <div className="p-3 border-t border-border">
          <button
            onClick={newSession}
            className="w-full flex items-center justify-center gap-2 text-sm text-muted-foreground bg-surface-2 hover:bg-surface-3 hover:text-foreground rounded-lg px-3 py-2 transition-colors cursor-pointer border border-border"
          >
            <Plus size={16} weight="regular" aria-hidden="true" />
            新建会话
          </button>
        </div>
      )}

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={menuRef}
          className="fixed z-50 bg-surface-2 border border-border-strong rounded-lg py-1 shadow-xl"
          style={{ left: contextMenu.x, top: contextMenu.y, minWidth: 140 }}
        >
          {isArchiveView ? (
            <button
              onClick={handleUnarchive}
              className="flex items-center gap-2 w-full px-3 py-2 text-sm text-foreground hover:bg-surface-3 transition-colors text-left"
            >
              <ArrowUUpLeft size={14} weight="regular" aria-hidden="true" />
              取消归档
            </button>
          ) : (
            <button
              onClick={handleArchive}
              className="flex items-center gap-2 w-full px-3 py-2 text-sm text-foreground hover:bg-surface-3 transition-colors text-left"
            >
              <Archive size={14} weight="regular" aria-hidden="true" />
              归档会话
            </button>
          )}
          <button
            onClick={handleDeleteClick}
            className="flex items-center gap-2 w-full px-3 py-2 text-sm text-destructive hover:bg-destructive/10 transition-colors text-left"
          >
            <Trash size={14} weight="regular" aria-hidden="true" />
            删除会话
          </button>
        </div>
      )}

      {/* Delete confirmation dialog */}
      <DeleteConfirmDialog
        open={deleteTarget !== null}
        onCancel={() => setDeleteTarget(null)}
        onConfirm={handleDeleteConfirm}
      />
    </aside>
  );
}
```

- [ ] **Step 2: Run typecheck**

Run: `pnpm typecheck`
Expected: Error — `DeleteConfirmDialog` doesn't exist yet. That's the next task. Verify no other errors.

- [ ] **Step 3: Commit**

```bash
git add src/components/layout/SessionSidebar.tsx
git commit -m "feat: SessionSidebar — toggle bar, context menu, archive view"
```

---

## Task 11: DeleteConfirmDialog Component

**Files:**
- Create: `src/components/chat/DeleteConfirmDialog.tsx`

- [ ] **Step 1: Create the DeleteConfirmDialog component**

Create `src/components/chat/DeleteConfirmDialog.tsx`:

```tsx
import { Warning } from "@phosphor-icons/react";

interface DeleteConfirmDialogProps {
  open: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

export function DeleteConfirmDialog({ open, onCancel, onConfirm }: DeleteConfirmDialogProps) {
  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ backgroundColor: "rgba(0, 0, 0, 0.6)" }}
      onClick={onCancel}
    >
      <div
        className="bg-card border border-border rounded-xl p-6 max-w-sm w-full mx-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start gap-3 mb-4">
          <div className="flex items-center justify-center w-8 h-8 rounded-lg bg-destructive/10 shrink-0">
            <Warning size={18} weight="regular" className="text-destructive" aria-hidden="true" />
          </div>
          <div>
            <h3
              className="text-foreground text-sm font-medium mb-1"
              style={{ fontFamily: "var(--font-sans)" }}
            >
              删除会话
            </h3>
            <p className="text-muted-foreground text-xs leading-relaxed">
              确定删除该会话？删除后不可恢复。
            </p>
          </div>
        </div>
        <div className="flex items-center justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs text-muted-foreground hover:text-foreground bg-surface-2 hover:bg-surface-3 rounded-md transition-colors border border-border"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            取消
          </button>
          <button
            onClick={onConfirm}
            className="px-3 py-1.5 text-xs text-destructive-foreground bg-destructive hover:bg-destructive/80 rounded-md transition-colors"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            确认删除
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Run typecheck**

Run: `pnpm typecheck`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/components/chat/DeleteConfirmDialog.tsx
git commit -m "feat: add DeleteConfirmDialog component"
```

---

## Task 12: Final Verification

- [ ] **Step 1: Run `cargo check`**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: No errors

- [ ] **Step 2: Run `cargo test`**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: All tests pass

- [ ] **Step 3: Run `pnpm typecheck`**

Run: `pnpm typecheck`
Expected: No errors

- [ ] **Step 4: Manual end-to-end verification**

Run: `pnpm tauri dev`

Test the following:
1. Send a message → verify agent responds → close session → reopen → verify full history loads (user message, agent text, tool cards)
2. Right-click a session → select "归档会话" → verify it disappears from main list
3. Click "归档" tab → verify archived session is visible with "归档于" date
4. Right-click archived session → select "取消归档" → verify it returns to main list
5. Right-click any session → select "删除会话" → verify confirmation dialog → confirm → verify session removed from list
6. Send a message to create a session → while agent is running, right-click → "删除会话" → confirm → verify agent stopped + session removed

- [ ] **Step 5: Commit any final fixes**

If any fixes were needed during manual testing, commit them:

```bash
git add -A
git commit -m "fix: address issues found during end-to-end testing"
```
