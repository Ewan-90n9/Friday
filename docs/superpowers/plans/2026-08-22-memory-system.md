# Friday 记忆系统 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Friday accumulate diagnostic experiences across sessions — auto-generate summaries and experience cards after each diagnosis, store them with semantic vector indexing, and inject relevant past experiences into new diagnosis prompts.

**Architecture:** Two-layer memory built on existing SQLite infrastructure. Layer 1: session summaries (bound to session, for display). Layer 2: experience index (independent of session, vector-indexed via sqlite-vec + bge-small-zh-v1.5 local embedding model, semantically retrievable at spawn time). A `spawn_one_shot` function reuses the agent CLI for one-off LLM calls (summary generation, experience extraction). The `consume_stream` function is refactored to unify its three exit paths and spawn a background task for memory generation.

**Tech Stack:** Rust, Tauri, SQLite, sqlx, rusqlite, sqlite-vec (Rust bindings), fastembed (ONNX Runtime), bge-small-zh-v1.5, tokio, serde_json

**Spec:** [docs/superpowers/specs/2026-08-22-memory-system-design.md](../specs/2026-08-22-memory-system-design.md)

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src-tauri/Cargo.toml` | Modify | Add `fastembed`, `rusqlite`, `sqlite-vec` dependencies |
| `src-tauri/migrations/0006_memory.sql` | Create | `session_summaries`, `experiences`, `experiences_vec` tables |
| `src-tauri/src/infra/paths.rs` | Modify | Add `models_dir()` + add to `ensure_dirs()` |
| `src-tauri/src/infra/db.rs` | Modify | Load migration 0006 + `sessions.language` column + sqlite-vec init |
| `src-tauri/src/knowledge/mod.rs` | Modify | Add `pub mod memory; pub mod experience; pub mod summary; pub mod embedding; pub mod parsing; pub mod vec_store;` |
| `src-tauri/src/knowledge/memory.rs` | Create | Core orchestration: `recall_experiences()`, `generate_memory()`, `upsert_experience()` |
| `src-tauri/src/knowledge/experience.rs` | Create | Experience struct, Outcome enum, DB CRUD, dedup query functions |
| `src-tauri/src/knowledge/summary.rs` | Create | SessionSummary struct, DB CRUD with upsert |
| `src-tauri/src/knowledge/embedding.rs` | Create | EmbeddingService wrapper around fastembed bge-small-zh-v1.5 |
| `src-tauri/src/knowledge/vec_store.rs` | Create | VecStore wrapper around sqlite-vec (rusqlite), upsert/query/filtered query |
| `src-tauri/src/knowledge/parsing.rs` | Create | LLM output parsing with layered fallback (JSON block → raw scan → partial degradation → rule fallback) |
| `src-tauri/src/agent/prompt.rs` | Modify | `build_prompt` accepts experiences for injection |
| `src-tauri/src/agent/spawn.rs` | Modify | Add `spawn_one_shot`; add `experiences: Option<&[Experience]>` param to `spawn_active` |
| `src-tauri/src/agent/stream.rs` | Modify | Unify three exit paths; spawn background memory task |
| `src-tauri/src/app/lifecycle.rs` | Modify | Retrieve experiences before spawn; pass to `spawn_active`; pass embedding+vec_store to `consume_stream` |
| `src-tauri/src/lib.rs` | Modify | Add `embedding: Option<Arc<EmbeddingService>>` + `vec_store: Option<Arc<VecStore>>` to AppState; preload model in setup |

**Rationale for splitting `knowledge/memory.rs` into multiple files:**

The memory module has distinct responsibilities: embedding (model loading + inference), experience CRUD (DB operations + dedup/merge), summary CRUD, output parsing (LLM response → structs), and orchestration (tie it all together). Keeping these in one file would create a 500+ line module that's hard to reason about. Splitting by responsibility keeps each file focused and testable.

---

## Task 1: Add Dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add dependencies to Cargo.toml**

Add to the `[dependencies]` section in `src-tauri/Cargo.toml`:

```toml
fastembed = "4"
rusqlite = { version = "0.32", features = ["bundled"] }
sqlite-vec = "0.1"
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS (compiles with new deps, may take a while to download/build)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "deps: add fastembed, rusqlite, sqlite-vec for memory system"
```

---

## Task 2: Database Migration — Memory Tables

**Files:**
- Create: `src-tauri/migrations/0006_memory.sql`
- Modify: `src-tauri/src/infra/db.rs`

- [ ] **Step 1: Write the migration SQL file**

Create `src-tauri/migrations/0006_memory.sql`:

```sql
-- Session summaries (bound to session, cascade delete)
CREATE TABLE IF NOT EXISTS session_summaries (
    session_id TEXT PRIMARY KEY,
    summary_text TEXT NOT NULL,
    generated_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- Experiences (fully independent, no session reference)
CREATE TABLE IF NOT EXISTS experiences (
    id TEXT PRIMARY KEY,
    symptom TEXT NOT NULL,
    service TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'unknown',
    root_cause TEXT,
    investigation_path TEXT NOT NULL DEFAULT '',
    experience_lesson TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK(outcome IN ('positive', 'negative', 'uncertain')),
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    last_seen_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    query_text TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_experiences_outcome ON experiences(outcome);
```

Note: The `experiences_vec` virtual table (sqlite-vec) is created at runtime via `sqlite-vec` crate, not in this SQL file — because sqlx cannot load the vec0 extension. It will be created in the `VecStore` init code (Task 5).

- [ ] **Step 2: Write the failing test**

Add to `src-tauri/src/infra/db.rs` test module:

```rust
#[tokio::test]
async fn test_migration_0006_creates_memory_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = init(tmp.path().join("friday.db")).await.unwrap();

    // session_summaries table exists
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_summaries'"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);

    // experiences table exists
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='experiences'"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);

    // sessions.language column exists
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='language'"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_migration_0006_creates_memory_tables -- --nocapture`
Expected: FAIL — tables don't exist yet

- [ ] **Step 4: Implement migration loading in db.rs `init`**

In `src-tauri/src/infra/db.rs`, add after the `schema5` line (after line 20, before `tracing::info!`):

```rust
    let schema6 = include_str!("../../migrations/0006_memory.sql");
    sqlx::query(schema6).execute(&pool).await?;
    add_column_if_not_exists(&pool, "sessions", "language", "TEXT").await?;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_migration_0006_creates_memory_tables -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/migrations/0006_memory.sql src-tauri/src/infra/db.rs
git commit -m "feat: add migration 0006 — memory tables (session_summaries, experiences, sessions.language)"
```

---

## Task 3: Paths — models_dir

**Files:**
- Modify: `src-tauri/src/infra/paths.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `src-tauri/src/infra/paths.rs`:

```rust
    #[test]
    fn test_models_dir_returns_root_join_models() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.models_dir(), tmp.path().join("models"));
    }
```

Also update the existing `test_ensure_dirs_creates_all_five_subdirs` test — it will need to check 6 dirs now. Change the test name and body:

```rust
    #[test]
    fn test_ensure_dirs_creates_all_six_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(tmp.path().join("logs").is_dir());
        assert!(tmp.path().join("playbooks").is_dir());
        assert!(tmp.path().join("skills").is_dir());
        assert!(tmp.path().join("prompts").is_dir());
        assert!(tmp.path().join("artifacts").is_dir());
        assert!(tmp.path().join("models").is_dir());
    }
```

Remove the old `test_ensure_dirs_creates_all_five_subdirs` test.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_models_dir_returns_root_join_models -- --nocapture`
Expected: FAIL — `models_dir` method doesn't exist

- [ ] **Step 3: Implement models_dir and update ensure_dirs**

In `src-tauri/src/infra/paths.rs`, add the method to the `impl Paths` block (after `artifacts_dir`, before `session_artifacts_dir`):

```rust
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }
```

In `ensure_dirs`, add `self.models_dir()` to the array:

```rust
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.log_dir(),
            self.playbooks_dir(),
            self.skills_dir(),
            self.prompts_dir(),
            self.artifacts_dir(),
            self.models_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_models_dir test_ensure_dirs -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/infra/paths.rs
git commit -m "feat: add models_dir to Paths for embedding model storage"
```

---

## Task 4: Knowledge Module — Experience Types

**Files:**
- Create: `src-tauri/src/knowledge/experience.rs`
- Modify: `src-tauri/src/knowledge/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/knowledge/experience.rs` with the test first:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Outcome {
    Positive,
    Negative,
    Uncertain,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Positive => "positive",
            Outcome::Negative => "negative",
            Outcome::Uncertain => "uncertain",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "positive" => Outcome::Positive,
            "negative" => Outcome::Negative,
            _ => Outcome::Uncertain,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experience {
    pub id: String,
    pub symptom: String,
    pub service: String,
    pub language: String,
    pub root_cause: Option<String>,
    pub investigation_path: String,
    pub experience_lesson: String,
    pub outcome: Outcome,
    pub occurrence_count: i64,
    pub last_seen_at: String,
    pub created_at: String,
    pub query_text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outcome_round_trip() {
        for outcome in [Outcome::Positive, Outcome::Negative, Outcome::Uncertain] {
            let s = outcome.as_str();
            assert_eq!(Outcome::from_str(s), outcome);
        }
    }

    #[test]
    fn test_outcome_from_str_unknown_defaults_uncertain() {
        assert_eq!(Outcome::from_str("garbage"), Outcome::Uncertain);
    }
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src-tauri/src/knowledge/mod.rs`, change:

```rust
pub mod playbook;
```

to:

```rust
pub mod experience;
pub mod playbook;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_outcome -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/experience.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: add Experience types — Outcome enum, Experience struct"
```

---

## Task 5: Experience DB CRUD

**Files:**
- Modify: `src-tauri/src/knowledge/experience.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `src-tauri/src/knowledge/experience.rs`:

```rust
    use crate::infra::db;

    fn make_test_experience() -> Experience {
        Experience {
            id: uuid::Uuid::new_v4().to_string(),
            symptom: "OOM".to_string(),
            service: "OrderService".to_string(),
            language: "java".to_string(),
            root_cause: Some("ThreadPool leak".to_string()),
            investigation_path: "jstat -> arthas thread".to_string(),
            experience_lesson: "Check thread count first".to_string(),
            outcome: Outcome::Positive,
            occurrence_count: 1,
            last_seen_at: "2026-08-22T00:00:00Z".to_string(),
            created_at: "2026-08-22T00:00:00Z".to_string(),
            query_text: "OrderService OOM".to_string(),
        }
    }

    #[tokio::test]
    async fn test_insert_and_get_experience() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();

        insert_experience(&pool, &exp).await.unwrap();

        let fetched = get_experience(&pool, &exp.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.symptom, "OOM");
        assert_eq!(fetched.service, "OrderService");
        assert_eq!(fetched.outcome, Outcome::Positive);
        assert_eq!(fetched.root_cause.as_deref(), Some("ThreadPool leak"));
    }

    #[tokio::test]
    async fn test_update_experience_increment() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        update_experience_increment(
            &pool,
            &exp.id,
            "jstat -> arthas thread -> jmap dump",
            "Check thread count first. Also check heap dump.",
            "2026-08-23T00:00:00Z",
        )
        .await
        .unwrap();

        let fetched = get_experience(&pool, &exp.id).await.unwrap().unwrap();
        assert_eq!(fetched.occurrence_count, 2);
        assert_eq!(fetched.last_seen_at, "2026-08-23T00:00:00Z");
        assert!(fetched.investigation_path.contains("jmap dump"));
        assert!(fetched.experience_lesson.contains("heap dump"));
    }

    #[tokio::test]
    async fn test_find_by_fields_positive_match() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_by_fields(
            &pool,
            "OOM",
            "java",
            "OrderService",
            Some("ThreadPool leak"),
        )
        .await
        .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exp.id);
    }

    #[tokio::test]
    async fn test_find_by_fields_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_by_fields(
            &pool,
            "OOM",
            "java",
            "OrderService",
            Some("Different root cause"),
        )
        .await
        .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_find_negative_by_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = Experience {
            root_cause: None,
            outcome: Outcome::Negative,
            ..make_test_experience()
        };
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_negative_by_fields(&pool, "OOM", "java", "OrderService")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exp.id);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_insert_and_get_experience -- --nocapture`
Expected: FAIL — functions don't exist

- [ ] **Step 3: Implement CRUD functions**

Add to `src-tauri/src/knowledge/experience.rs` (after the struct, before `#[cfg(test)]`):

```rust
use sqlx::{Row, SqlitePool};

pub async fn insert_experience(
    pool: &SqlitePool,
    exp: &Experience,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO experiences \
         (id, symptom, service, language, root_cause, investigation_path, \
          experience_lesson, outcome, occurrence_count, last_seen_at, created_at, query_text) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&exp.id)
    .bind(&exp.symptom)
    .bind(&exp.service)
    .bind(&exp.language)
    .bind(&exp.root_cause)
    .bind(&exp.investigation_path)
    .bind(&exp.experience_lesson)
    .bind(exp.outcome.as_str())
    .bind(exp.occurrence_count)
    .bind(&exp.last_seen_at)
    .bind(&exp.created_at)
    .bind(&exp.query_text)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_experience(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(
        String, String, String, String, Option<String>,
        String, String, String, i64, String, String, String,
    )> = sqlx::query_as(
        "SELECT id, symptom, service, language, root_cause, \
         investigation_path, experience_lesson, outcome, occurrence_count, \
         last_seen_at, created_at, query_text \
         FROM experiences WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Experience {
        id: r.0,
        symptom: r.1,
        service: r.2,
        language: r.3,
        root_cause: r.4,
        investigation_path: r.5,
        experience_lesson: r.6,
        outcome: Outcome::from_str(&r.7),
        occurrence_count: r.8,
        last_seen_at: r.9,
        created_at: r.10,
        query_text: r.11,
    }))
}

pub async fn update_experience_increment(
    pool: &SqlitePool,
    id: &str,
    new_investigation_path: &str,
    new_lesson: &str,
    last_seen_at: &str,
) -> Result<(), sqlx::Error> {
    let existing = get_experience(pool, id).await?;
    if let Some(exp) = existing {
        let combined_path = if exp.investigation_path.is_empty() {
            new_investigation_path.to_string()
        } else if !exp.investigation_path.contains(new_investigation_path) {
            format!("{}. {}", exp.investigation_path, new_investigation_path)
        } else {
            exp.investigation_path
        };
        let combined_lesson = if exp.experience_lesson.is_empty() {
            new_lesson.to_string()
        } else if !exp.experience_lesson.contains(new_lesson) {
            format!("{}. {}", exp.experience_lesson, new_lesson)
        } else {
            exp.experience_lesson
        };
        sqlx::query(
            "UPDATE experiences SET investigation_path = ?, experience_lesson = ?, \
             occurrence_count = ?, last_seen_at = ? WHERE id = ?",
        )
        .bind(&combined_path)
        .bind(&combined_lesson)
        .bind(exp.occurrence_count + 1)
        .bind(last_seen_at)
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn find_by_fields(
    pool: &SqlitePool,
    symptom: &str,
    language: &str,
    service: &str,
    root_cause: Option<&str>,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences \
         WHERE symptom = ? AND language = ? AND service = ? AND root_cause = ? \
         AND outcome = 'positive' LIMIT 1",
    )
    .bind(symptom)
    .bind(language)
    .bind(service)
    .bind(root_cause)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = row {
        return get_experience(pool, &id).await;
    }
    Ok(None)
}

pub async fn find_negative_by_fields(
    pool: &SqlitePool,
    symptom: &str,
    language: &str,
    service: &str,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences \
         WHERE symptom = ? AND language = ? AND service = ? AND root_cause IS NULL \
         AND outcome = 'negative' ORDER BY last_seen_at DESC LIMIT 1",
    )
    .bind(symptom)
    .bind(language)
    .bind(service)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = row {
        return get_experience(pool, &id).await;
    }
    Ok(None)
}

pub async fn replace_experience(
    pool: &SqlitePool,
    id: &str,
    exp: &Experience,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE experiences SET symptom = ?, service = ?, language = ?, \
         root_cause = ?, investigation_path = ?, experience_lesson = ?, \
         outcome = ?, last_seen_at = ? WHERE id = ?",
    )
    .bind(&exp.symptom)
    .bind(&exp.service)
    .bind(&exp.language)
    .bind(&exp.root_cause)
    .bind(&exp.investigation_path)
    .bind(&exp.experience_lesson)
    .bind(exp.outcome.as_str())
    .bind(&exp.last_seen_at)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_insert_and_get test_update_experience test_find_by_fields test_find_negative -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/knowledge/experience.rs
git commit -m "feat: experience DB CRUD — insert, get, update_increment, find_by_fields, replace"
```

---

## Task 6: Session Summary Types and DB CRUD

**Files:**
- Create: `src-tauri/src/knowledge/summary.rs`
- Modify: `src-tauri/src/knowledge/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/knowledge/summary.rs`:

```rust
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub summary_text: String,
    pub generated_at: String,
}

pub async fn insert_summary(
    pool: &SqlitePool,
    session_id: &str,
    summary_text: &str,
    generated_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO session_summaries (session_id, summary_text, generated_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(session_id) DO UPDATE SET summary_text = excluded.summary_text, generated_at = excluded.generated_at",
    )
    .bind(session_id)
    .bind(summary_text)
    .bind(generated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_summary(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<SessionSummary>, sqlx::Error> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT session_id, summary_text, generated_at FROM session_summaries WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| SessionSummary {
        session_id: r.0,
        summary_text: r.1,
        generated_at: r.2,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;
    use crate::app::session;

    #[tokio::test]
    async fn test_insert_and_get_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();

        // Need a session first (FK constraint)
        let session = session::create_session(&pool, "test message").await.unwrap();

        insert_summary(&pool, &session.id.0, "OOM caused by thread leak", "2026-08-22T00:00:00Z")
            .await
            .unwrap();

        let fetched = get_summary(&pool, &session.id.0).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().summary_text, "OOM caused by thread leak");
    }

    #[tokio::test]
    async fn test_insert_summary_upserts_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = session::create_session(&pool, "test message").await.unwrap();

        insert_summary(&pool, &session.id.0, "first summary", "2026-08-22T00:00:00Z")
            .await
            .unwrap();
        insert_summary(&pool, &session.id.0, "updated summary", "2026-08-22T01:00:00Z")
            .await
            .unwrap();

        let fetched = get_summary(&pool, &session.id.0).await.unwrap().unwrap();
        assert_eq!(fetched.summary_text, "updated summary");
        assert_eq!(fetched.generated_at, "2026-08-22T01:00:00Z");
    }
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src-tauri/src/knowledge/mod.rs`:

```rust
pub mod experience;
pub mod playbook;
pub mod summary;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_insert_and_get_summary test_insert_summary_upserts -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/summary.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: session summary types and DB CRUD with upsert"
```

---

## Task 7: Embedding Module

**Files:**
- Create: `src-tauri/src/knowledge/embedding.rs`
- Modify: `src-tauri/src/knowledge/mod.rs`

- [ ] **Step 1: Write the EmbeddingModel wrapper**

Create `src-tauri/src/knowledge/embedding.rs`:

```rust
use fastembed::{EmbeddingModel, InitOptions, Embedding};
use std::path::PathBuf;
use std::sync::Arc;

pub struct EmbeddingService {
    model: Embedding,
}

impl EmbeddingService {
    pub fn new(models_dir: PathBuf) -> Result<Self, String> {
        // fastembed downloads models to a cache dir on first use.
        // We point the cache to our models_dir so it can find pre-bundled models.
        let model = Embedding::try_new(InitOptions::new(EmbeddingModel::BGESmallZHV15)
            .with_cache_dir(models_dir))
            .map_err(|e| format!("failed to load embedding model: {e}"))?;
        Ok(Self { model })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let embeddings = self.model
            .embed(vec![text.to_string()], None)
            .map_err(|e| format!("embedding inference failed: {e}"))?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| "embedding returned no results".to_string())
    }
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src-tauri/src/knowledge/mod.rs`:

```rust
pub mod embedding;
pub mod experience;
pub mod playbook;
pub mod summary;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS

Note: We skip unit tests for embedding because they require downloading the model. Integration testing happens in Task 13 when the full flow is wired up.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/embedding.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: embedding service — bge-small-zh-v1.5 via fastembed"
```

---

## Task 8: Vector Store — sqlite-vec Integration

**Files:**
- Create: `src-tauri/src/knowledge/vec_store.rs`
- Modify: `src-tauri/src/knowledge/mod.rs`

- [ ] **Step 1: Write the VecStore struct and tests**

Create `src-tauri/src/knowledge/vec_store.rs`:

```rust
use rusqlite::Connection;
use sqlite_vec::sqlite3_vec_init;
use std::sync::Mutex;

pub struct VecStore {
    conn: Mutex<Connection>,
}

impl VecStore {
    pub fn new(db_path: &str) -> Result<Self, String> {
        let conn = Connection::open(db_path)
            .map_err(|e| format!("failed to open vec db: {e}"))?;

        // Register sqlite-vec extension
        unsafe {
            sqlite3_vec_init(
                conn.handle(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }

        // Create virtual table if not exists
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS experiences_vec USING vec0(\
                id TEXT PRIMARY KEY,\
                embedding FLOAT[512]\
            );",
        )
        .map_err(|e| format!("failed to create vec table: {e}"))?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn upsert_vector(&self, id: &str, embedding: &[f32]) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        conn.execute(
            "INSERT OR REPLACE INTO experiences_vec (id, embedding) VALUES (?, ?)",
            rusqlite::params![id, embedding_bytes],
        )
        .map_err(|e| format!("failed to upsert vector: {e}"))?;
        Ok(())
    }

    pub fn delete_vector(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        conn.execute(
            "DELETE FROM experiences_vec WHERE id = ?",
            rusqlite::params![id],
        )
        .map_err(|e| format!("failed to delete vector: {e}"))?;
        Ok(())
    }

    /// Query top-K nearest neighbors by embedding.
    /// Returns (experience_id, distance) pairs.
    pub fn query(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32)>, String> {
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let mut stmt = conn
            .prepare("SELECT id, distance FROM experiences_vec WHERE embedding MATCH ? ORDER BY distance ASC LIMIT ?")
            .map_err(|e| format!("failed to prepare query: {e}"))?;

        let rows = stmt
            .query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
                let id: String = row.get(0)?;
                let distance: f32 = row.get(1)?;
                Ok((id, distance))
            })
            .map_err(|e| format!("failed to query vectors: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("row mapping error: {e}"))?);
        }
        Ok(result)
    }

    /// Query top-K nearest neighbors, filtered by a set of experience IDs.
    /// This is used for outcome-filtered retrieval (positive-only, negative-only).
    pub fn query_filtered(
        &self,
        embedding: &[f32],
        limit: usize,
        allowed_ids: &[String],
    ) -> Result<Vec<(String, f32)>, String> {
        if allowed_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        // Build placeholder list for IN clause
        let placeholders: Vec<String> = (0..allowed_ids.len())
            .map(|_| "?".to_string())
            .collect();
        let sql = format!(
            "SELECT id, distance FROM experiences_vec WHERE embedding MATCH ? AND id IN ({}) ORDER BY distance ASC LIMIT ?",
            placeholders.join(", ")
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("failed to prepare filtered query: {e}"))?;

        // Bind embedding bytes first, then each allowed_id, then limit
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params_vec.push(Box::new(embedding_bytes));
        for id in allowed_ids {
            params_vec.push(Box::new(id.clone()));
        }
        params_vec.push(Box::new(limit as i64));

        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id: String = row.get(0)?;
                let distance: f32 = row.get(1)?;
                Ok((id, distance))
            })
            .map_err(|e| format!("failed to query filtered vectors: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("row mapping error: {e}"))?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dummy_vec(id: &str) -> (String, Vec<f32>) {
        // Create a 512-dim vector with a simple pattern
        let embedding: Vec<f32> = (0..512).map(|i| (i as f32) * 0.001).collect();
        (id.to_string(), embedding)
    }

    #[test]
    fn test_upsert_and_query_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id, embedding) = make_dummy_vec("exp-1");
        store.upsert_vector(&id, &embedding).unwrap();

        let results = store.query(&embedding, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-1");
        // distance to self should be very small
        assert!(results[0].1 < 0.01);
    }

    #[test]
    fn test_upsert_replaces_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (_, embedding1) = make_dummy_vec("exp-1");
        store.upsert_vector("exp-1", &embedding1).unwrap();

        // Different embedding, same id
        let embedding2: Vec<f32> = (0..512).map(|i| (i as f32) * 0.002).collect();
        store.upsert_vector("exp-1", &embedding2).unwrap();

        let results = store.query(&embedding2, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-1");
    }

    #[test]
    fn test_query_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id1, emb1) = make_dummy_vec("exp-1");
        let (id2, emb2) = make_dummy_vec("exp-2");
        store.upsert_vector(&id1, &emb1).unwrap();
        store.upsert_vector(&id2, &emb2).unwrap();

        // Query with filter — only allow exp-2
        let results = store.query_filtered(&emb1, 5, &["exp-2".to_string()]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-2");
    }

    #[test]
    fn test_query_filtered_empty_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (_, emb) = make_dummy_vec("exp-1");
        let results = store.query_filtered(&emb, 5, &[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_delete_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id, emb) = make_dummy_vec("exp-1");
        store.upsert_vector(&id, &emb).unwrap();
        store.delete_vector(&id).unwrap();

        let results = store.query(&emb, 1).unwrap();
        assert!(results.is_empty());
    }
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src-tauri/src/knowledge/mod.rs`:

```rust
pub mod embedding;
pub mod experience;
pub mod playbook;
pub mod summary;
pub mod vec_store;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_upsert test_query test_delete -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/vec_store.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: vector store — sqlite-vec integration with upsert, query, filtered query"
```

---

## Task 9: LLM Output Parsing — Layered Fallback

**Files:**
- Create: `src-tauri/src/knowledge/parsing.rs`
- Modify: `src-tauri/src/knowledge/mod.rs`

- [ ] **Step 1: Write the parsing tests and implementation**

Create `src-tauri/src/knowledge/parsing.rs`:

```rust
use serde::{Deserialize, Serialize};
use super::experience::Outcome;

/// The structured output expected from spawn_one_shot.
/// This is what the LLM is asked to produce as a JSON code block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmOutput {
    pub summary: String,
    pub symptom: String,
    pub service: String,
    pub language: String,
    pub root_cause: Option<String>,
    pub investigation_path: String,
    pub experience_lesson: String,
    pub outcome: String,
}

/// Result of parsing LLM output. Fields may be None if extraction fell back.
#[derive(Clone, Debug)]
pub struct ParsedOutput {
    pub summary: String,
    pub symptom: Option<String>,
    pub service: Option<String>,
    pub language: Option<String>,
    pub root_cause: Option<String>,
    pub investigation_path: Option<String>,
    pub experience_lesson: Option<String>,
    pub outcome: Outcome,
    pub extraction_succeeded: bool,
}

/// Parse LLM stdout with layered fallback:
/// 1. Extract JSON code block (```json ... ```)
/// 2. Try raw JSON line scanning
/// 3. Partial field degradation
/// 4. Rule-based fallback (take first 500 chars as summary)
pub fn parse_llm_output(stdout: &str, fallback_outcome: Outcome) -> ParsedOutput {
    // Step 1: Try JSON code block extraction
    if let Some(json_str) = extract_json_code_block(stdout) {
        if let Ok(parsed) = serde_json::from_str::<LlmOutput>(&json_str) {
            return build_parsed_output(parsed, true);
        }
    }

    // Step 2: Try raw JSON line scanning
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(parsed) = serde_json::from_str::<LlmOutput>(trimmed) {
                return build_parsed_output(parsed, true);
            }
        }
    }

    // Step 3: Fallback — take first 500 chars as summary, use fallback outcome
    let summary = if stdout.len() > 500 {
        stdout[..500].to_string()
    } else {
        stdout.to_string()
    };

    tracing::warn!("LLM output parsing failed, using fallback");
    ParsedOutput {
        summary,
        symptom: None,
        service: None,
        language: None,
        root_cause: None,
        investigation_path: None,
        experience_lesson: None,
        outcome: fallback_outcome,
        extraction_succeeded: false,
    }
}

fn extract_json_code_block(text: &str) -> Option<String> {
    // Match ```json ... ``` or ``` ... ```
    let json_start = text.find("```json").or_else(|| text.find("```"))?;
    let content_start = if text[..json_start + 7].contains("json") {
        json_start + 7  // skip "```json"
    } else {
        json_start + 3  // skip "```"
    };

    // Skip the newline after the opening fence
    let content_start = text[content_start..]
        .char_indices()
        .skip_while(|(_, c)| c.is_whitespace() && *c != '\n')
        .nth(1) // skip the newline itself
        .map(|(i, _)| content_start + i)
        .unwrap_or(content_start);

    let content_end = text[content_start..].find("```")?;
    Some(text[content_start..content_start + content_end].trim().to_string())
}

fn build_parsed_output(parsed: LlmOutput, succeeded: bool) -> ParsedOutput {
    let outcome = Outcome::from_str(&parsed.outcome);

    // Step 3 (partial degradation): if positive but no root_cause, degrade to uncertain
    let outcome = if outcome == Outcome::Positive && parsed.root_cause.is_none() {
        tracing::warn!("positive outcome but no root_cause, degrading to uncertain");
        Outcome::Uncertain
    } else {
        outcome
    };

    ParsedOutput {
        summary: parsed.summary,
        symptom: Some(parsed.symptom),
        service: Some(parsed.service),
        language: Some(if parsed.language.is_empty() {
            "unknown".to_string()
        } else {
            parsed.language
        }),
        root_cause: parsed.root_cause,
        investigation_path: Some(parsed.investigation_path),
        experience_lesson: Some(parsed.experience_lesson),
        outcome,
        extraction_succeeded: succeeded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_code_block() {
        let stdout = "Here is the summary:\n```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"thread leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"check threads\",\"outcome\":\"positive\"}\n```\nDone.";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
        assert_eq!(result.symptom.as_deref(), Some("OOM"));
        assert_eq!(result.outcome, Outcome::Positive);
        assert_eq!(result.root_cause.as_deref(), Some("thread leak"));
    }

    #[test]
    fn test_parse_raw_json_line() {
        let stdout = "{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"thread leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"check threads\",\"outcome\":\"positive\"}";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
    }

    #[test]
    fn test_parse_positive_without_root_cause_degrades_to_uncertain() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":null,\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.outcome, Outcome::Uncertain);
    }

    #[test]
    fn test_parse_missing_outcome_defaults_uncertain() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.outcome, Outcome::Uncertain);
    }

    #[test]
    fn test_parse_empty_language_defaults_unknown() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.language.as_deref(), Some("unknown"));
    }

    #[test]
    fn test_parse_fallback_takes_first_500_chars() {
        let long_text = "x".repeat(600);
        let result = parse_llm_output(&long_text, Outcome::Negative);
        assert!(!result.extraction_succeeded);
        assert_eq!(result.summary.len(), 500);
        assert_eq!(result.outcome, Outcome::Negative);
    }

    #[test]
    fn test_parse_fallback_short_text() {
        let result = parse_llm_output("some text", Outcome::Negative);
        assert!(!result.extraction_succeeded);
        assert_eq!(result.summary, "some text");
    }

    #[test]
    fn test_parse_json_block_without_json_tag() {
        // ``` without json specifier
        let stdout = "```\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
    }
}
```

- [ ] **Step 2: Add module to mod.rs**

In `src-tauri/src/knowledge/mod.rs`:

```rust
pub mod embedding;
pub mod experience;
pub mod parsing;
pub mod playbook;
pub mod summary;
pub mod vec_store;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_parse -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/parsing.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: LLM output parsing — layered fallback (JSON block, raw scan, partial degradation, rule fallback)"
```

---

## Task 10: Prompt — Experience Injection

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `src-tauri/src/agent/prompt.rs`:

```rust
    use crate::knowledge::experience::{Experience, Outcome};

    fn make_test_experience(outcome: Outcome, root_cause: Option<&str>) -> Experience {
        Experience {
            id: "test-id".to_string(),
            symptom: "OOM".to_string(),
            service: "OrderService".to_string(),
            language: "java".to_string(),
            root_cause: root_cause.map(|s| s.to_string()),
            investigation_path: "jstat -> arthas thread".to_string(),
            experience_lesson: "Check thread count first".to_string(),
            outcome,
            occurrence_count: 1,
            last_seen_at: "2026-08-22T00:00:00Z".to_string(),
            created_at: "2026-08-22T00:00:00Z".to_string(),
            query_text: "OrderService OOM".to_string(),
        }
    }

    #[test]
    fn test_build_prompt_with_experiences_injects_section() {
        let exps = vec![
            make_test_experience(Outcome::Positive, Some("ThreadPool leak")),
            make_test_experience(Outcome::Negative, None),
        ];
        let result = build_prompt_with_experiences("hello", None, &exps);

        assert!(result.contains("## 历史经验参考"));
        assert!(result.contains("成功"));
        assert!(result.contains("未成功"));
        assert!(result.contains("ThreadPool leak"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_build_prompt_with_empty_experiences_no_section() {
        let exps: Vec<Experience> = vec![];
        let result = build_prompt_with_experiences("hello", None, &exps);

        assert!(!result.contains("## 历史经验参考"));
        assert!(result.contains("hello"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_build_prompt_with_experiences -- --nocapture`
Expected: FAIL — `build_prompt_with_experiences` doesn't exist

- [ ] **Step 3: Implement build_prompt_with_experiences**

Add to `src-tauri/src/agent/prompt.rs` (after `build_prompt`, before the test module):

```rust
use crate::knowledge::experience::{Experience, Outcome};

pub fn build_prompt_with_experiences(
    message: &str,
    override_path: Option<&Path>,
    experiences: &[Experience],
) -> String {
    let system = build_system_prompt(override_path);

    if experiences.is_empty() {
        return format!("{system}\n\n---\n\n用户消息：{message}");
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

    format!("{system}\n\n---\n\n{exp_section}\n---\n\n用户消息：{message}")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_build_prompt_with_experiences test_build_prompt_with_empty -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: prompt experience injection — build_prompt_with_experiences injects history section"
```

---

## Task 11: spawn_one_shot

**Files:**
- Modify: `src-tauri/src/agent/spawn.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `src-tauri/src/agent/spawn.rs`:

```rust
    #[tokio::test]
    async fn test_spawn_one_shot_accepts_session_id_param() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_one_shot(&pool, "test prompt".to_string()).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_spawn_one_shot_accepts -- --nocapture`
Expected: FAIL — `spawn_one_shot` doesn't exist

- [ ] **Step 3: Implement spawn_one_shot**

Add to `src-tauri/src/agent/spawn.rs` (after `spawn_active`, before the test module):

```rust
/// One-shot LLM call via agent CLI. No --sessions, no stream parsing.
/// Writes prompt to stdin, reads full stdout, returns the text output.
/// Used for summary generation and experience extraction.
#[tracing::instrument(skip(pool))]
pub async fn spawn_one_shot(
    pool: &sqlx::SqlitePool,
    prompt: String,
) -> Result<String, SpawnError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str, provider) = row.ok_or(SpawnError::NoActiveAgent)?;
    let raw_path = PathBuf::from(&path_str);

    if !raw_path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let config = command_config_for(&provider);

    let exe_path = if config.needs_exe_resolution {
        resolve_native_exe(&raw_path)
    } else {
        raw_path.clone()
    };
    tracing::info!(
        exe_path = %exe_path.display(),
        provider = %provider,
        "spawn_one_shot resolved agent executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.args(config.mode_args)
        .args(config.format_args)
        .arg("--dangerously-skip-permissions");

    // No --sessions flag for one-shot calls

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(home) = dirs::home_dir() {
        cmd.env("PWD", &home);
        cmd.current_dir(&home);
    }

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    tracing::info!(pid, "spawn_one_shot agent process spawned");

    // Write prompt to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        let msg = prompt.clone();
        tokio::spawn(async move {
            if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                tracing::error!(?e, "spawn_one_shot: failed to write prompt to stdin");
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::error!(?e, "spawn_one_shot: failed to close stdin");
            }
        });
    }

    // Read stderr in background
    let stderr = child.stderr.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stderr not piped"),
    ))?;
    let stderr_handle = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(raw = %line, "spawn_one_shot stderr line");
        }
    });

    // Read full stdout
    let stdout = child.stdout.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stdout not piped"),
    ))?;

    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stdout).lines();
    let mut output = String::new();

    // For opencode (run --format json): extract text from NDJSON text events
    // For codeagentcli (-p --output-format stream-json): extract text from assistant events
    while let Ok(Some(line)) = lines.next_line().await {
        if provider == "opencode" {
            // Try to parse as JSON and extract text/reasoning content
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = v
                        .get("part")
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        output.push_str(text);
                    }
                }
            }
        } else {
            // codeagentcli: parse assistant events for text content
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                    if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                        output.push_str(result);
                    }
                } else if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
                    if let Some(content) = v
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in content {
                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    output.push_str(text);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = child.wait().await;
    let _ = stderr_handle.await;

    tracing::info!(output_len = output.len(), "spawn_one_shot completed");

    Ok(output)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_spawn_one_shot_accepts -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/spawn.rs
git commit -m "feat: spawn_one_shot — one-shot LLM call for summary/experience generation"
```

---

## Task 12: spawn_active — Add experiences Parameter

**Files:**
- Modify: `src-tauri/src/agent/spawn.rs`

- [ ] **Step 1: Update spawn_active signature and prompt building**

In `src-tauri/src/agent/spawn.rs`, change the `spawn_active` signature (around line 99-105) from:

```rust
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    agent_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
) -> Result<AgentProcess, SpawnError> {
```

to:

```rust
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    agent_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
    experiences: Option<&[crate::knowledge::experience::Experience]>,
) -> Result<AgentProcess, SpawnError> {
```

Then replace the prompt building line (around line 141):

```rust
    let prompt_text = prompt::build_prompt(&message, prompt_override_path.as_deref());
```

with:

```rust
    let prompt_text = if let Some(exps) = experiences {
        if !exps.is_empty() {
            prompt::build_prompt_with_experiences(&message, prompt_override_path.as_deref(), exps)
        } else {
            prompt::build_prompt(&message, prompt_override_path.as_deref())
        }
    } else {
        prompt::build_prompt(&message, prompt_override_path.as_deref())
    };
```

- [ ] **Step 2: Update the existing spawn_active tests to pass the new param**

In the test module of `src-tauri/src/agent/spawn.rs`, update the three calls to `spawn_active`. Change all three from:

```rust
spawn_active(&pool, "test-sid".to_string(), String::new(), None, None).await;
```

to:

```rust
spawn_active(&pool, "test-sid".to_string(), String::new(), None, None, None).await;
```

There are 3 test calls to update (in `test_spawn_active_accepts_session_id_param`, `test_spawn_active_returns_no_active_agent_when_db_empty`, `test_spawn_active_returns_binary_missing_when_path_invalid`).

- [ ] **Step 3: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS (lifecycle.rs still passes `None` — will be wired up in Task 14)

Note: lifecycle.rs currently calls `spawn_active` with 5 args. This will fail to compile. Temporarily add `None` as the 6th arg in lifecycle.rs to unblock compilation:

In `src-tauri/src/app/lifecycle.rs`, change the `spawn_active` call (around line 138-144) to add `, None` as the last argument:

```rust
    let agent_process = match spawn_active(
        &pool,
        friday_session_id.clone(),
        message,
        agent_session_id,
        Some(prompt_override_path),
        None,  // experiences — wired up in Task 14
    )
    .await
```

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 4: Run existing tests to verify no regressions**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_spawn_active -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/spawn.rs src-tauri/src/app/lifecycle.rs
git commit -m "feat: spawn_active accepts experiences parameter for prompt injection"
```

---

## Task 13: Memory Module — Recall, Upsert, Generate

**Files:**
- Modify: `src-tauri/src/knowledge/memory.rs`

This is the core orchestration module that ties together embedding, vec_store, experience CRUD, parsing, and spawn_one_shot.

- [ ] **Step 1: Implement the memory module**

Create `src-tauri/src/knowledge/memory.rs` with the following content. This is the core orchestration module — it ties together embedding, vec_store, experience CRUD, parsing, and spawn_one_shot.

Add to `src-tauri/src/knowledge/mod.rs`:
```rust
pub mod memory;
```

```rust
use crate::agent::spawn::spawn_one_shot;
use crate::app::session::get_session_messages;
use crate::knowledge::embedding::EmbeddingService;
use crate::knowledge::experience::{
    self, Experience, Outcome,
};
use crate::knowledge::parsing::parse_llm_output;
use crate::knowledge::summary;
use crate::knowledge::vec_store::VecStore;
use sqlx::SqlitePool;
use std::sync::Arc;

const SIMILARITY_THRESHOLD: f32 = 0.5;

/// Convert sqlite-vec distance to cosine similarity.
/// sqlite-vec returns L2 squared distance for FLOAT vectors.
/// cos_sim = 1 - (distance / 2) for normalized vectors.
/// We use a simple heuristic: similarity = 1.0 / (1.0 + distance).
fn distance_to_similarity(distance: f32) -> f32 {
    1.0 / (1.0 + distance)
}

/// Retrieve relevant experiences for prompt injection.
/// Called at spawn time for new sessions.
/// Returns top-2 positive + top-1 negative, filtered by similarity threshold.
/// This function requires AppState-level resources (embedding service + vec store),
/// so it's called via a wrapper that provides them.
pub async fn recall_experiences(
    pool: &SqlitePool,
    embedding: &EmbeddingService,
    vec_store: &VecStore,
    query_text: &str,
) -> Vec<Experience> {
    let query_vec = match embedding.embed(query_text) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(?e, "failed to embed query for experience recall");
            return Vec::new();
        }
    };

    // Get all positive experience IDs
    let positive_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences WHERE outcome = 'positive'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let positive_ids: Vec<String> = positive_ids.into_iter().map(|(id,)| id).collect();

    // Get all negative experience IDs
    let negative_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences WHERE outcome = 'negative'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let negative_ids: Vec<String> = negative_ids.into_iter().map(|(id,)| id).collect();

    // Query positive top-2
    let positive_results = if positive_ids.is_empty() {
        Vec::new()
    } else {
        vec_store.query_filtered(&query_vec, 2, &positive_ids).unwrap_or_default()
    };

    // Query negative top-1
    let negative_results = if negative_ids.is_empty() {
        Vec::new()
    } else {
        vec_store.query_filtered(&query_vec, 1, &negative_ids).unwrap_or_default()
    };

    let mut experiences = Vec::new();

    for (id, distance) in &positive_results {
        if distance_to_similarity(*distance) < SIMILARITY_THRESHOLD {
            continue;
        }
        if let Ok(Some(exp)) = experience::get_experience(pool, id).await {
            experiences.push(exp);
        }
    }

    for (id, distance) in &negative_results {
        if distance_to_similarity(*distance) < SIMILARITY_THRESHOLD {
            continue;
        }
        if let Ok(Some(exp)) = experience::get_experience(pool, id).await {
            experiences.push(exp);
        }
    }

    experiences
}
```

- [ ] **Step 2: Write the generate_memory function**

Add to `src-tauri/src/knowledge/memory.rs`:

```rust
/// Compress session data for spawn_one_shot input.
/// User messages: full. Agent text: full. Tool calls: name + args + output first 20 lines.
fn compress_session_data(
    messages: &[crate::app::session::MessageRow],
) -> String {
    let mut output = String::new();
    for msg in messages {
        let role_label = match msg.role.as_str() {
            "user" => "用户",
            "agent" => "Friday",
            other => other,
        };
        writeln!(output, "\n## {} (seq={})", role_label, msg.seq).ok();

        if let Some(content) = &msg.content {
            if !content.is_empty() {
                writeln!(output, "{}", content).ok();
            }
        }

        for part in &msg.parts {
            match part.part_type.as_str() {
                "text" => {
                    if let Some(text) = &part.text {
                        if !text.is_empty() {
                            writeln!(output, "{}", text).ok();
                        }
                    }
                }
                "tool" => {
                    writeln!(output, "\n[工具调用: {}]", part.tool_name.as_deref().unwrap_or("?")).ok();
                    if let Some(args) = &part.tool_args {
                        writeln!(output, "参数: {}", args).ok();
                    }
                    if let Some(tool_output) = &part.tool_output {
                        let lines: Vec<&str> = tool_output.lines().collect();
                        if lines.len() > 20 {
                            writeln!(output, "输出:\n{}\n... ({} 行已截断)", lines[..20].join("\n"), lines.len() - 20).ok();
                        } else {
                            writeln!(output, "输出:\n{}", tool_output).ok();
                        }
                    }
                }
                _ => {}
            }
        }
    }
    output
}

/// Build the prompt for spawn_one_shot — asks LLM to produce summary + experience fields.
fn build_one_shot_prompt(session_data: &str, fallback_outcome: &str) -> String {
    format!(
        r#"请分析以下诊断会话记录，提取结构化信息。

## 会话记录

{session_data}

## 输出要求

请输出一个 JSON 代码块，包含以下字段：

```json
{{
    "summary": "会话摘要（2-3句话概括诊断过程和结论）",
    "symptom": "症状关键词（如 OOM、CPU飙高、连接池耗尽）",
    "service": "服务名称",
    "language": "编程语言（java/cpp/go/python/unknown，从工具使用推断）",
    "root_cause": "根因（如果定位到，否则 null）",
    "investigation_path": "排查路径（自然语言描述，如 jstat显示GC频繁 → arthas thread发现2000+线程 → 定位ThreadPoolExecutor）",
    "experience_lesson": "经验提炼（可复用的经验，如 OOM先查线程数）",
    "outcome": "positive（成功定位根因）| negative（未定位根因）| uncertain（不确定）"
}}
```

注意：
- outcome 为 {fallback_outcome} 时，说明诊断未正常完成，请据此判断。
- 如果无法确定某个字段，使用空字符串或 null。
- 只输出 JSON 代码块，不要其他文字。"#
    )
}

/// Generate summary + experience after diagnosis completes.
/// Called as a background tokio::spawn task.
pub async fn generate_memory(
    pool: SqlitePool,
    session_id: String,
    fallback_outcome: Outcome,
    embedding: Arc<EmbeddingService>,
    vec_store: Arc<VecStore>,
) {
    tracing::info!(session_id = %session_id, "generate_memory started");

    // 1. Read session data
    let messages = match get_session_messages(&pool, &session_id).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::error!(?e, session_id = %session_id, "failed to read session messages");
            return;
        }
    };

    if messages.is_empty() {
        tracing::warn!(session_id = %session_id, "no messages to summarize");
        return;
    }

    // 2. Compress session data
    let session_data = compress_session_data(&messages);
    let fallback_str = fallback_outcome.as_str();
    let prompt = build_one_shot_prompt(&session_data, fallback_str);

    // 3. Call spawn_one_shot
    let stdout = match spawn_one_shot(&pool, prompt).await {
        Ok(text) => text,
        Err(e) => {
            tracing::error!(?e, session_id = %session_id, "spawn_one_shot failed in generate_memory");
            return;
        }
    };

    // 4. Parse output
    let parsed = parse_llm_output(&stdout, fallback_outcome);

    // 5. Store session summary
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = summary::insert_summary(&pool, &session_id, &parsed.summary, &now).await {
        tracing::error!(?e, session_id = %session_id, "failed to store session summary");
    }

    // 6. Backfill sessions table fields
    if let (Some(symptom), Some(service), Some(language)) =
        (&parsed.symptom, &parsed.service, &parsed.language)
    {
        let _ = sqlx::query(
            "UPDATE sessions SET symptom = ?, service = ?, language = ? WHERE id = ?",
        )
        .bind(symptom)
        .bind(service)
        .bind(language)
        .bind(&session_id)
        .execute(&pool)
        .await;
    }
    if let Some(root_cause) = &parsed.root_cause {
        // Store root_cause in the experience, not sessions (sessions has no root_cause column)
        // This is used for experience dedup
        let _ = root_cause;
    }

    // 7. Get the first user message for vectorization
    let query_text = messages
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    if query_text.is_empty() {
        tracing::warn!(session_id = %session_id, "no user message found for vectorization");
        return;
    }

    // 8. Build experience and upsert
    let exp = Experience {
        id: uuid::Uuid::new_v4().to_string(),
        symptom: parsed.symptom.clone().unwrap_or_default(),
        service: parsed.service.clone().unwrap_or_default(),
        language: parsed.language.clone().unwrap_or("unknown".to_string()),
        root_cause: parsed.root_cause.clone(),
        investigation_path: parsed.investigation_path.clone().unwrap_or_default(),
        experience_lesson: parsed.experience_lesson.clone().unwrap_or_default(),
        outcome: parsed.outcome.clone(),
        occurrence_count: 1,
        last_seen_at: now.clone(),
        created_at: now.clone(),
        query_text: query_text.clone(),
    };

    // 9. Upsert with dedup
    if let Err(e) = upsert_experience(&pool, &exp, &embedding, &vec_store).await {
        tracing::error!(?e, session_id = %session_id, "failed to upsert experience");
    }

    tracing::info!(session_id = %session_id, "generate_memory completed");
}

/// Upsert an experience with dedup/merge logic.
/// 1. Embed query_text
/// 2. Vector search top-5 candidates
/// 3. Exact field match for dedup
/// 4. Merge or insert
pub async fn upsert_experience(
    pool: &SqlitePool,
    exp: &Experience,
    embedding: &EmbeddingService,
    vec_store: &VecStore,
) -> Result<(), String> {
    // Embed the query text
    let query_vec = embedding.embed(&exp.query_text)?;

    // Search for candidates (top-5)
    let candidates = vec_store.query(&query_vec, 5)?;

    // Collect candidate experience IDs
    let candidate_ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();

    // Check for exact field match (dedup)
    if exp.outcome == Outcome::Positive {
        // Positive: check by symptom+language+service+root_cause
        let root_cause = exp.root_cause.as_deref();
        for cid in &candidate_ids {
            if let Ok(Some(existing)) = experience::get_experience(pool, cid).await {
                if existing.outcome == Outcome::Positive
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                    && existing.root_cause == exp.root_cause
                {
                    // Match found — incremental update
                    tracing::info!(existing_id = %existing.id, "dedup match, incrementing");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }

                // Cross-outcome: existing negative, new positive → replace
                if existing.outcome == Outcome::Negative
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    tracing::info!(existing_id = %existing.id, "replacing negative with positive");
                    // Update the existing experience with new positive data
                    experience::replace_experience(pool, &existing.id, exp)
                        .await
                        .map_err(|e| e.to_string())?;
                    // Update the vector (query_text may have changed)
                    vec_store.upsert_vector(&existing.id, &query_vec)?;
                    return Ok(());
                }
            }
        }
    } else if exp.outcome == Outcome::Negative {
        // Negative: check by symptom+language+service (no root_cause)
        for cid in &candidate_ids {
            if let Ok(Some(existing)) = experience::get_experience(pool, cid).await {
                if existing.outcome == Outcome::Negative
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    // Match found — merge lesson, keep existing
                    tracing::info!(existing_id = %existing.id, "negative dedup match, merging lesson");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }

                // Cross-outcome: existing positive, new negative → keep positive, append lesson
                if existing.outcome == Outcome::Positive
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    tracing::info!(existing_id = %existing.id, "keeping positive, appending negative lesson");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }
            }
        }
    }

    // No match found — insert new experience
    tracing::info!(exp_id = %exp.id, "inserting new experience");
    experience::insert_experience(pool, exp)
        .await
        .map_err(|e| e.to_string())?;
    vec_store.upsert_vector(&exp.id, &query_vec)?;

    Ok(())
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/memory.rs
git commit -m "feat: memory module — recall, generate_memory, upsert_experience with dedup/merge"
```

---

## Task 14: Wire Up AppState and Lifecycle

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/app/lifecycle.rs`
- Modify: `src-tauri/src/agent/spawn.rs`
- Modify: `src-tauri/src/agent/stream.rs`

- [ ] **Step 1: Add embedding + vec_store to AppState**

In `src-tauri/src/lib.rs`, update the `AppState` struct:

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
    pub embedding: Arc<crate::knowledge::embedding::EmbeddingService>,
    pub vec_store: Arc<crate::knowledge::vec_store::VecStore>,
}
```

Update the `run()` function setup to initialize them. After `let pool = ...` and before `app.manage(AppState {...})`:

```rust
            let embedding = match crate::knowledge::embedding::EmbeddingService::new(
                paths.models_dir(),
            ) {
                Ok(e) => {
                    tracing::info!("embedding model loaded");
                    Arc::new(e)
                }
                Err(e) => {
                    tracing::error!(?e, "failed to load embedding model, memory features disabled");
                    // We still create the AppState — memory features will fail at runtime
                    // but the app won't crash. A better approach would be Option<Arc<...>>,
                    // but for v1 we let it fail lazily.
                    Arc::new(unsafe { std::mem::zeroed() })
                }
            };
```

Wait — `std::mem::zeroed()` is unsafe and wrong. Let me use `Option` instead:

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
    pub embedding: Option<Arc<crate::knowledge::embedding::EmbeddingService>>,
    pub vec_store: Option<Arc<crate::knowledge::vec_store::VecStore>>,
}
```

And in setup:

```rust
            let embedding = match crate::knowledge::embedding::EmbeddingService::new(
                paths.models_dir(),
            ) {
                Ok(e) => {
                    tracing::info!("embedding model loaded");
                    Some(Arc::new(e))
                }
                Err(e) => {
                    tracing::error!(?e, "failed to load embedding model, memory features disabled");
                    None
                }
            };

            let vec_store = match crate::knowledge::vec_store::VecStore::new(
                paths.db_path().to_str().unwrap_or("friday.db"),
            ) {
                Ok(v) => {
                    tracing::info!("vec store initialized");
                    Some(Arc::new(v))
                }
                Err(e) => {
                    tracing::error!(?e, "failed to init vec store, memory features disabled");
                    None
                }
            };
```

Then update `app.manage`:

```rust
            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
                paths,
                embedding,
                vec_store,
            });
```

- [ ] **Step 2: Update lifecycle.rs to retrieve experiences and pass to spawn**

In `src-tauri/src/app/lifecycle.rs`, update the spawn section (around line 132-161). Replace the temporary `None` from Task 12 with actual experience retrieval:

```rust
    // Retrieve relevant experiences for new sessions
    let experiences: Vec<crate::knowledge::experience::Experience> = if session_id.is_none() {
        if let (Some(ref embedding), Some(ref vec_store)) = (state.embedding.as_ref(), state.vec_store.as_ref()) {
            crate::knowledge::memory::recall_experiences(&pool, embedding, vec_store, &message)
                .await
        } else {
            tracing::warn!("embedding or vec_store not available, skipping experience recall");
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Get prompt override path and spawn agent
    let prompt_override_path = state.paths.prompts_dir().join("friday.md");
    tracing::info!(
        session_id = %friday_session_id,
        experience_count = experiences.len(),
        "spawning agent"
    );
    let agent_process = match spawn_active(
        &pool,
        friday_session_id.clone(),
        message,
        agent_session_id,
        Some(prompt_override_path),
        Some(&experiences),
    )
    .await
    {
```

- [ ] **Step 4: Update stream.rs — unify exit paths and spawn background memory task**

In `src-tauri/src/agent/stream.rs`, update `consume_stream` signature to accept the memory resources:

```rust
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    agent_message_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
    embedding: Option<std::sync::Arc<crate::knowledge::embedding::EmbeddingService>>,
    vec_store: Option<std::sync::Arc<crate::knowledge::vec_store::VecStore>>,
) {
```

Add a new enum before the function for exit reason:

```rust
enum ExitReason {
    Normal,    // stdout EOF (then check exit code for done vs crashed)
    Cancelled, // cancellation
}
```

Replace the cancel branch (the `_ = cancel.cancelled()` arm) — change from early return to break:

```rust
            _ = cancel.cancelled() => {
                tracing::info!(session_id = %session_id, "cancellation received, killing child");
                child.kill().await.ok();
                bus.emit(&session_id, AppEvent::AgentStopped {
                    session_id: session_id.clone(),
                });
                exit_reason = ExitReason::Cancelled;
                break;
            }
```

Add `let mut exit_reason = ExitReason::Normal;` before the loop.

After the loop, unify the logic:

```rust
    let status = child.wait().await;
    let exit_ok = status.as_ref().map(|s| s.success()).unwrap_or(false);

    let _ = stderr_handle.await;

    // Determine exit reason and emit appropriate event
    let (final_status, fallback_outcome) = match exit_reason {
        ExitReason::Normal => {
            if exit_ok {
                tracing::info!(session_id = %session_id, exit_ok, "child process exited normally");
                bus.emit(&session_id, AppEvent::DiagnosisDone {
                    session_id: session_id.clone(),
                    conclusion: String::new(),
                });
                ("done", crate::knowledge::experience::Outcome::Uncertain)
            } else {
                let reason = match &status {
                    Ok(s) => format!("exit code: {}", s.code().unwrap_or(-1)),
                    Err(e) => format!("wait error: {}", e),
                };
                tracing::info!(session_id = %session_id, "child process crashed");
                bus.emit(&session_id, AppEvent::AgentCrashed {
                    session_id: session_id.clone(),
                    reason,
                });
                ("error", crate::knowledge::experience::Outcome::Negative)
            }
        }
        ExitReason::Cancelled => {
            ("stopped", crate::knowledge::experience::Outcome::Negative)
        }
    };

    // Flush accumulated message parts to DB
    accumulator.flush_to_db(&pool).await;
    if let Err(e) = crate::app::session::update_message_status(&pool, &agent_message_id, final_status).await {
        tracing::error!(?e, message_id = %agent_message_id, "failed to update message status");
    }

    // Remove from agents map
    {
        let mut map = agents.lock().await;
        map.remove(&session_id);
    }

    // Spawn background task for memory generation (summary + experience)
    if let (Some(embedding), Some(vec_store)) = (embedding, vec_store) {
        let pool_clone = pool.clone();
        let session_id_clone = session_id.clone();
        tokio::spawn(async move {
            crate::knowledge::memory::generate_memory(
                pool_clone,
                session_id_clone,
                fallback_outcome,
                embedding,
                vec_store,
            )
            .await;
        });
    } else {
        tracing::warn!(session_id = %session_id, "memory resources not available, skipping memory generation");
    }
```

Remove the old code that was after the loop (the old `if exit_ok { ... } else { ... }`, the old `let final_status = ...`, the old `accumulator.flush_to_db`, the old `update_message_status`, and the old `map.remove`).

- [ ] **Step 4: Update lifecycle.rs to pass embedding + vec_store to consume_stream**

In `src-tauri/src/app/lifecycle.rs`, update the `tokio::spawn` block (around line 185-196):

```rust
    let embedding_clone = state.embedding.clone();
    let vec_store_clone = state.vec_store.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            agent_message_id_clone,
            pool_clone,
            agents_clone,
            cancel_for_task,
            embedding_clone,
            vec_store_clone,
        )
        .await;
    });
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 6: Run all tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --nocapture`
Expected: PASS (all existing tests should pass with updated signatures)

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/app/lifecycle.rs src-tauri/src/agent/spawn.rs src-tauri/src/agent/stream.rs src-tauri/src/knowledge/memory.rs src-tauri/src/knowledge/mod.rs
git commit -m "feat: wire up memory system — AppState embedding/vec_store, lifecycle recall, stream background generation"
```

---

## Task 15: Session Summary IPC Command

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to the test module in `src-tauri/src/app/lifecycle.rs`:

```rust
    #[tokio::test]
    async fn test_get_session_summary_cmd_returns_none_when_no_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = session::create_session(&pool, "test").await.unwrap();

        let result = crate::knowledge::summary::get_summary(&pool, &session.id.0).await.unwrap();
        assert!(result.is_none());
    }
```

- [ ] **Step 2: Run test to verify it passes (it tests existing summary::get_summary)**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_get_session_summary_cmd_returns_none -- --nocapture`
Expected: PASS

- [ ] **Step 3: Add get_session_summary_cmd Tauri command**

In `src-tauri/src/app/lifecycle.rs`, add the command:

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_session_summary_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<Option<String>, String> {
    crate::knowledge::summary::get_summary(&state.db, &session_id)
        .await
        .map(|opt| opt.map(|s| s.summary_text))
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 4: Register the command in lib.rs**

In `src-tauri/src/lib.rs`, add to the `invoke_handler` list:

```rust
            app::lifecycle::get_session_summary_cmd,
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs src-tauri/src/lib.rs
git commit -m "feat: get_session_summary_cmd — expose session summary to frontend"
```

---

## Task 16: Final Integration Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --nocapture`
Expected: All tests PASS

- [ ] **Step 2: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS with no warnings (or only minor warnings)

- [ ] **Step 3: Run frontend typecheck**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 4: Manual smoke test (if possible)**

Run: `pnpm tauri dev`

1. Send a diagnostic message → verify agent responds normally
2. Wait for diagnosis to complete → check logs for "generate_memory started" / "generate_memory completed"
3. Send another similar diagnostic message → check logs for experience recall
4. Check SQLite: `sqlite3 friday.db "SELECT * FROM experiences;"`
5. Check SQLite: `sqlite3 friday.db "SELECT * FROM session_summaries;"`

- [ ] **Step 5: Final commit**

If any fixes were needed during smoke test:

```bash
git add -A
git commit -m "fix: integration fixes from smoke test"
```

---

## Self-Review Notes

**Spec coverage check:**

| Spec Section | Task |
|---|---|
| §3.1 Session Summary | Task 6 (CRUD), Task 13 (generation), Task 14 (wiring) |
| §3.2 Experience Index | Task 4-5 (types+CRUD), Task 7-8 (embedding+vec), Task 13 (recall+upsert) |
| §4.1 Unified cleanup | Task 14 (stream.rs refactor) |
| §4.2 spawn_one_shot | Task 11 |
| §4.3 Parsing fallback | Task 9 |
| §4.4 Vectorization | Task 7 (embedding), Task 8 (vec_store) |
| §4.5 Retrieval and injection | Task 10 (prompt), Task 13 (recall), Task 14 (lifecycle wiring) |
| §4.6 Dedup/merge | Task 5 (find_by_fields), Task 13 (upsert_experience) |
| §5 DB changes | Task 2 |
| §5.3 paths.models_dir | Task 3 |
| §6 Integration points | Tasks 10-15 |

**Placeholder scan:** No TBD/TODO. All steps have complete code.

**Type consistency:** `Experience` struct used consistently across experience.rs, memory.rs, prompt.rs. `Outcome` enum consistent. `ParsedOutput` used in parsing.rs and memory.rs.
