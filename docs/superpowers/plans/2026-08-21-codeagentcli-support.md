# codeagentcli 多 Provider 支持实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有 opencode 支持基础上新增 codeagentcli 作为可选 agent 后端，用户可在设置中切换。

**Architecture:** Provider enum + match dispatch 方案。registry 加 codeagentcli 条目；spawn.rs 新增 `CommandConfig` 结构 + `command_config_for()` 分发函数，按 provider 构建不同 CLI 参数；stream 解析器共享不变；DB 列 `opencode_session_id` 重命名为 `agent_session_id`；前端加下拉选项。

**Tech Stack:** Rust, Tauri 2, sqlx (SQLite), tokio, React + TypeScript

**Spec:** `docs/superpowers/specs/2026-08-21-codeagentcli-support-design.md`

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `src-tauri/src/infra/db.rs` | 修改 | 加 `rename_column_if_exists` 辅助函数，执行列重命名迁移，更新测试 |
| `src-tauri/migrations/0004_rename_session_column.sql` | 新增 | 文档性迁移文件 |
| `src-tauri/src/app/session.rs` | 修改 | `get_opencode_session_id` → `get_agent_session_id`，`update_opencode_session_id` → `update_agent_session_id`，SQL 列名更新，测试更新 |
| `src-tauri/src/agent/stream.rs` | 修改 | 调用 `update_agent_session_id`，日志文案 opencode → agent |
| `src-tauri/src/app/lifecycle.rs` | 修改 | 变量 `oc_session_id` → `agent_session_id`，调用 `get_agent_session_id` |
| `src-tauri/src/agent/registry.rs` | 修改 | 加 codeagentcli 条目，更新测试 |
| `src-tauri/src/agent/detect.rs` | 修改 | 测试断言泛化（检查 REGISTRY 而非硬编码 opencode） |
| `src-tauri/src/agent/spawn.rs` | 修改 | 加 `CommandConfig` + `command_config_for`，查询 provider，按 config 构建命令，参数重命名，日志泛化，新增测试 |
| `src/components/agents/AgentSettingsDialog.tsx` | 修改 | 加 codeagentcli 下拉选项 |

---

## Task 1: DB 迁移 — `rename_column_if_exists` 辅助函数

**Files:**
- Modify: `src-tauri/src/infra/db.rs`
- Create: `src-tauri/migrations/0004_rename_session_column.sql`

- [ ] **Step 1: Write failing tests for `rename_column_if_exists`**

Add these tests to the `#[cfg(test)] mod tests` block in `src-tauri/src/infra/db.rs`, after the existing tests:

```rust
    #[tokio::test]
    async fn test_rename_column_if_exists_renames_column() {
        let tmp = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", tmp.path().join("test.db").display());
        let pool = SqlitePoolOptions::new()
            .connect(&db_url)
            .await
            .unwrap();

        sqlx::query("CREATE TABLE test_table (old_col TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        rename_column_if_exists(&pool, "test_table", "old_col", "new_col")
            .await
            .unwrap();

        let new_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'new_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(new_count, 1);

        let old_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'old_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old_count, 0);
    }

    #[tokio::test]
    async fn test_rename_column_if_exists_noop_when_old_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", tmp.path().join("test.db").display());
        let pool = SqlitePoolOptions::new()
            .connect(&db_url)
            .await
            .unwrap();

        sqlx::query("CREATE TABLE test_table (new_col TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        // old_col doesn't exist, new_col already exists — should be no-op
        rename_column_if_exists(&pool, "test_table", "old_col", "new_col")
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'new_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
```

Also update the existing `test_db_init_adds_conversation_columns` test to check for `agent_session_id` instead of `opencode_session_id`:

```rust
    #[tokio::test]
    async fn test_db_init_adds_conversation_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let col_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name IN ('agent_session_id', 'title')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(col_count, 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_rename_column_if_exists -- --exact`
Expected: COMPILE ERROR — `rename_column_if_exists` not found (function doesn't exist yet), and `test_db_init_adds_conversation_columns` FAIL (column is still `opencode_session_id`).

- [ ] **Step 3: Implement `rename_column_if_exists`**

Add this function in `src-tauri/src/infra/db.rs`, after the existing `add_column_if_not_exists` function (after line 45):

```rust
/// Rename a column only if the old column exists and the new one doesn't.
/// SQLite supports ALTER TABLE RENAME COLUMN since 3.25.0.
async fn rename_column_if_exists(
    pool: &SqlitePool,
    table: &str,
    old_name: &str,
    new_name: &str,
) -> Result<(), sqlx::Error> {
    let old_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, old_name
        ),
    )
    .fetch_one(pool)
    .await?;

    let new_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, new_name
        ),
    )
    .fetch_one(pool)
    .await?;

    if old_exists > 0 && new_exists == 0 {
        let sql = format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            table, old_name, new_name
        );
        sqlx::query(&sql).execute(pool).await?;
        tracing::info!(table, old_name, new_name, "renamed column");
    }

    Ok(())
}
```

- [ ] **Step 4: Update `init` to use the new migration flow**

In `src-tauri/src/infra/db.rs`, replace the `init` function's migration section (lines 14-16):

Old:
```rust
    // Migration 0003: add conversation columns (idempotent — safe to re-run)
    add_column_if_not_exists(&pool, "sessions", "opencode_session_id", "TEXT").await?;
    add_column_if_not_exists(&pool, "sessions", "title", "TEXT").await?;
```

New:
```rust
    // Migration 0003/0004: rename opencode_session_id → agent_session_id, add title
    rename_column_if_exists(&pool, "sessions", "opencode_session_id", "agent_session_id").await?;
    add_column_if_not_exists(&pool, "sessions", "agent_session_id", "TEXT").await?;
    add_column_if_not_exists(&pool, "sessions", "title", "TEXT").await?;
```

- [ ] **Step 5: Create migration file**

Create `src-tauri/migrations/0004_rename_session_column.sql`:

```sql
-- Rename opencode_session_id to agent_session_id for multi-provider support.
-- Executed in db.rs::init via rename_column_if_exists (SQLite ALTER TABLE RENAME COLUMN).
-- This file documents the migration; the actual execution is in Rust for idempotency.
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- infra::db`
Expected: ALL PASS (5 tests: 3 existing + 2 new)

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/infra/db.rs src-tauri/migrations/0004_rename_session_column.sql
git commit -m "feat: add rename_column_if_exists and migrate opencode_session_id to agent_session_id"
```

---

## Task 2: 重命名 `opencode_session_id` → `agent_session_id`（session.rs + stream.rs + lifecycle.rs）

This is a refactor across 3 files. All changes must be done together for compilation.

**Files:**
- Modify: `src-tauri/src/app/session.rs`
- Modify: `src-tauri/src/agent/stream.rs`
- Modify: `src-tauri/src/app/lifecycle.rs`

- [ ] **Step 1: Rename functions and SQL in `session.rs`**

In `src-tauri/src/app/session.rs`:

1. Rename function `get_opencode_session_id` → `get_agent_session_id` (line 123):
   - Change `pub async fn get_opencode_session_id(` to `pub async fn get_agent_session_id(`
   - Change SQL `"SELECT opencode_session_id FROM sessions WHERE id = ?"` to `"SELECT agent_session_id FROM sessions WHERE id = ?"`

2. Rename function `update_opencode_session_id` → `update_agent_session_id` (line 135):
   - Change `pub async fn update_opencode_session_id(` to `pub async fn update_agent_session_id(`
   - Change SQL `"UPDATE sessions SET opencode_session_id = ? WHERE id = ?"` to `"UPDATE sessions SET agent_session_id = ? WHERE id = ?"`

3. Update test function names (lines 250, 259):
   - `test_get_opencode_session_id_returns_none_initially` → `test_get_agent_session_id_returns_none_initially`
   - `test_update_opencode_session_id_persists` → `test_update_agent_session_id_persists`
   - Inside these tests, update calls: `get_opencode_session_id` → `get_agent_session_id`, `update_opencode_session_id` → `update_agent_session_id`

- [ ] **Step 2: Update `stream.rs` call sites and log messages**

In `src-tauri/src/agent/stream.rs`:

1. Line 238: Change `crate::app::session::update_opencode_session_id` to `crate::app::session::update_agent_session_id`

2. Line 237: Change log message `"captured opencode session id"` to `"captured agent session id"`

3. Line 239 (comment): Change `// Extract opencode session ID from any event that has it` to `// Extract agent session ID from any event that has it`

4. Line 197 (doc comment): Change `/// Consume the stdout stream of an opencode process` to `/// Consume the stdout stream of an agent process`

- [ ] **Step 3: Update `lifecycle.rs` variable names and call sites**

In `src-tauri/src/app/lifecycle.rs`:

1. Line 62: Change `let (friday_session_id, oc_session_id) = match session_id {` to `let (friday_session_id, agent_session_id) = match session_id {`

2. Line 71: Change `(session.id.0, None)` — no change needed (already None)

3. Line 85: Change `let oc_id = session::get_opencode_session_id(&pool, &id)` to `let agent_id = session::get_agent_session_id(&pool, &id)`

4. Line 88: Change `tracing::info!(?oc_id, "found opencode session id")` to `tracing::info!(?agent_id, "found agent session id")`

5. Line 89: Change `(id, oc_id)` to `(id, agent_id)`

6. Line 109: Change the `spawn_active` call — the 4th argument `oc_session_id` to `agent_session_id`:
   ```rust
   let agent_process = spawn_active(
       &pool,
       friday_session_id.clone(),
       message,
       agent_session_id,
       Some(prompt_override_path),
   )
   ```

7. Line 108: Change `"spawning opencode"` to `"spawning agent"`

8. Line 123: Change `"opencode spawned"` to `"agent spawned"`

- [ ] **Step 4: Run cargo check to verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: COMPILES with no errors

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- app::session`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/session.rs src-tauri/src/agent/stream.rs src-tauri/src/app/lifecycle.rs
git commit -m "refactor: rename opencode_session_id to agent_session_id across codebase"
```

---

## Task 3: Registry — 新增 codeagentcli 条目

**Files:**
- Modify: `src-tauri/src/agent/registry.rs`

- [ ] **Step 1: Update tests to expect 2 entries**

In `src-tauri/src/agent/registry.rs`, replace the existing test (lines 15-27):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_opencode_and_codeagentcli() {
        assert_eq!(REGISTRY.len(), 2);

        let opencode = REGISTRY.iter().find(|d| d.provider == "opencode").unwrap();
        assert_eq!(opencode.command, "opencode");
        assert_eq!(opencode.display_name, "OpenCode");

        let codeagent = REGISTRY.iter().find(|d| d.provider == "codeagentcli").unwrap();
        assert_eq!(codeagent.command, "codeagentcli");
        assert_eq!(codeagent.display_name, "CodeAgentCLI");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::registry`
Expected: FAIL — `REGISTRY.len()` is 1, not 2

- [ ] **Step 3: Add codeagentcli entry to REGISTRY**

In `src-tauri/src/agent/registry.rs`, replace the REGISTRY constant (lines 7-13):

```rust
pub const REGISTRY: &[AgentDescriptor] = &[
    AgentDescriptor {
        provider: "opencode",
        command: "opencode",
        display_name: "OpenCode",
    },
    AgentDescriptor {
        provider: "codeagentcli",
        command: "codeagentcli",
        display_name: "CodeAgentCLI",
    },
];
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::registry`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/registry.rs
git commit -m "feat: add codeagentcli to agent registry"
```

---

## Task 4: Spawn — CommandConfig + provider 感知命令构建

**Files:**
- Modify: `src-tauri/src/agent/spawn.rs`

- [ ] **Step 1: Write failing tests for `command_config_for`**

Add these tests to the `#[cfg(test)] mod tests` block in `src-tauri/src/agent/spawn.rs`, after the existing tests:

```rust
    #[test]
    fn test_command_config_for_opencode() {
        let config = command_config_for("opencode");
        assert_eq!(config.print_args, &["run"]);
        assert_eq!(config.format_args, &["--format", "json"]);
        assert_eq!(config.session_flag, "--session");
        assert!(config.needs_exe_resolution);
    }

    #[test]
    fn test_command_config_for_codeagentcli() {
        let config = command_config_for("codeagentcli");
        assert_eq!(config.print_args, &["-p"]);
        assert_eq!(config.format_args, &["--output-format", "stream-json"]);
        assert_eq!(config.session_flag, "--sessions");
        assert!(!config.needs_exe_resolution);
    }

    #[test]
    fn test_command_config_for_unknown_falls_back_to_opencode() {
        let config = command_config_for("unknown");
        assert_eq!(config.print_args, &["run"]);
        assert_eq!(config.format_args, &["--format", "json"]);
        assert_eq!(config.session_flag, "--session");
        assert!(config.needs_exe_resolution);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::spawn::tests::test_command_config`
Expected: COMPILE ERROR — `command_config_for` and `CommandConfig` not found

- [ ] **Step 3: Implement `CommandConfig` struct and `command_config_for` function**

In `src-tauri/src/agent/spawn.rs`, add this after the `resolve_native_exe` function (after line 63, before `spawn_active`):

```rust
struct CommandConfig {
    print_args: &'static [&'static str],
    format_args: &'static [&'static str],
    session_flag: &'static str,
    needs_exe_resolution: bool,
}

fn command_config_for(provider: &str) -> CommandConfig {
    match provider {
        "opencode" => CommandConfig {
            print_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
        "codeagentcli" => CommandConfig {
            print_args: &["-p"],
            format_args: &["--output-format", "stream-json"],
            session_flag: "--sessions",
            needs_exe_resolution: false,
        },
        _ => CommandConfig {
            print_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::spawn::tests::test_command_config`
Expected: PASS (3 tests)

- [ ] **Step 5: Update `spawn_active` to query provider and use CommandConfig**

In `src-tauri/src/agent/spawn.rs`, update the `spawn_active` function. Replace the function signature and body from line 66 to line 101:

Old (lines 66-101):
```rust
#[tracing::instrument(skip(pool))]
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    opencode_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str,) = row.ok_or(SpawnError::NoActiveAgent)?;
    let raw_path = PathBuf::from(&path_str);

    if !raw_path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    // On Windows, resolve to native .exe to avoid cmd.exe shim issues
    let exe_path = resolve_native_exe(&raw_path);
    tracing::info!(
        raw_path = %raw_path.display(),
        exe_path = %exe_path.display(),
        "resolved opencode executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.arg("run")
        .arg("--format")
        .arg("json")
        .arg("--dangerously-skip-permissions");

    if let Some(ref oc_id) = opencode_session_id {
        cmd.arg("--session").arg(oc_id);
    }
```

New:
```rust
#[tracing::instrument(skip(pool))]
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    agent_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
) -> Result<AgentProcess, SpawnError> {
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
        raw_path = %raw_path.display(),
        exe_path = %exe_path.display(),
        provider = %provider,
        "resolved agent executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.args(config.print_args)
        .args(config.format_args)
        .arg("--dangerously-skip-permissions");

    if let Some(ref id) = agent_session_id {
        cmd.arg(config.session_flag).arg(id);
    }
```

- [ ] **Step 6: Update remaining log messages and comments in `spawn_active`**

In `src-tauri/src/agent/spawn.rs`, search for and update these strings (line numbers will have shifted after Step 5):

1. Comment: Change `// Set PWD to the user's home directory so opencode doesn't pick up` to `// Set PWD to the user's home directory so the agent doesn't pick up`

2. Log message: Change `tracing::info!(pid, exe = %exe_path.display(), "opencode process spawned")` to `tracing::info!(pid, exe = %exe_path.display(), provider = %provider, "agent process spawned")`

- [ ] **Step 7: Run cargo check to verify compilation**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: COMPILES with no errors

- [ ] **Step 8: Run all spawn tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::spawn`
Expected: ALL PASS (existing 4 tests + 3 new command_config tests = 7 tests)

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/agent/spawn.rs
git commit -m "feat: add CommandConfig for provider-aware command building in spawn"
```

---

## Task 5: Detect — 测试断言泛化

**Files:**
- Modify: `src-tauri/src/agent/detect.rs`

- [ ] **Step 1: Update the test to check REGISTRY instead of hardcoding "opencode"**

In `src-tauri/src/agent/detect.rs`, replace the test `detect_returns_vec_without_panicking` (lines 79-85):

Old:
```rust
    #[tokio::test]
    async fn detect_returns_vec_without_panicking() {
        let result = detect().await;
        for agent in &result {
            assert_eq!(agent.provider, "opencode");
        }
    }
```

New:
```rust
    #[tokio::test]
    async fn detect_returns_vec_without_panicking() {
        let result = detect().await;
        for agent in &result {
            assert!(
                super::registry::REGISTRY.iter().any(|d| d.provider == agent.provider),
                "detected provider {} not in registry",
                agent.provider
            );
        }
    }
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- agent::detect`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/agent/detect.rs
git commit -m "test: generalize detect test to check registry instead of hardcoded opencode"
```

---

## Task 6: 前端 — 加 codeagentcli 下拉选项

**Files:**
- Modify: `src/components/agents/AgentSettingsDialog.tsx`

- [ ] **Step 1: Add codeagentcli option to the provider dropdown**

In `src/components/agents/AgentSettingsDialog.tsx`, find the `<select>` element (around line 141-148) and add the codeagentcli option:

Old:
```tsx
                <select
                  value={provider}
                  onChange={(e) => setProvider(e.target.value)}
                  className="bg-muted border border-border rounded-md text-sm text-foreground px-2 py-1.5 cursor-pointer"
                  aria-label="Provider"
                >
                  <option value="opencode">opencode</option>
                </select>
```

New:
```tsx
                <select
                  value={provider}
                  onChange={(e) => setProvider(e.target.value)}
                  className="bg-muted border border-border rounded-md text-sm text-foreground px-2 py-1.5 cursor-pointer"
                  aria-label="Provider"
                >
                  <option value="opencode">opencode</option>
                  <option value="codeagentcli">codeagentcli</option>
                </select>
```

- [ ] **Step 2: Run typecheck to verify**

Run: `pnpm typecheck`
Expected: PASS with no errors

- [ ] **Step 3: Commit**

```bash
git add src/components/agents/AgentSettingsDialog.tsx
git commit -m "feat: add codeagentcli option to agent provider dropdown"
```

---

## Task 7: 最终验证

- [ ] **Step 1: Run full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: ALL TESTS PASS

- [ ] **Step 2: Run cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: COMPILES with no errors

- [ ] **Step 3: Run frontend typecheck**

Run: `pnpm typecheck`
Expected: PASS with no errors

- [ ] **Step 4: Verify no stale references to "opencode_session_id"**

Run: `grep -r "opencode_session_id" src-tauri/src/ src/`
Expected: NO MATCHES (all references should be renamed to `agent_session_id`)

- [ ] **Step 5: Final commit if any cleanup needed**

If the verification steps revealed any issues, fix and commit. Otherwise, no commit needed.
