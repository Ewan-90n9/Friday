# Friday 文件管理规则实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将所有运行时文件路径收敛到集中式 `infra/paths.rs` 模块，统一目录布局，支持 prompt 覆盖层，对齐 playbook 签名，更新架构文档。

**Architecture:** 新增 `Paths` struct 存入 `AppState`，所有模块从 `State<AppState>` 取路径而非内联 `join`。`logging::init` 和 `db::init` 签名从接收 `app_data_dir` 改为接收具体子路径。`prompt.rs` 加 `override_path` 参数支持未来 GUI 编辑。`playbook.rs` 签名对齐 `playbooks_dir` 参数。

**Tech Stack:** Rust, Tauri 2, tracing, sqlx (SQLite), serde_yaml (playbook 已有依赖通过 serde)

**Spec:** `docs/superpowers/specs/2026-08-21-file-management-design.md`

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `src-tauri/src/infra/paths.rs` | 新增 | `Paths` struct — 单一事实源，解析所有运行时路径 |
| `src-tauri/src/infra/mod.rs` | 修改 | 注册 `pub mod paths;` |
| `src-tauri/src/lib.rs` | 修改 | setup 构造 `Paths`，`ensure_dirs()`，存入 `AppState`，分发路径给 init 函数 |
| `src-tauri/src/infra/logging.rs` | 修改 | `init` 签名改为接收 `log_dir: PathBuf`，去掉内部 join/create |
| `src-tauri/src/infra/db.rs` | 修改 | `init` 签名改为接收 `db_path: PathBuf`，去掉内部 join |
| `src-tauri/src/agent/prompt.rs` | 修改 | 加 `build_system_prompt` 函数 + `build_prompt` 加 `override_path` 参数 |
| `src-tauri/src/agent/spawn.rs` | 修改 | `spawn_active` 接收 `prompt_override_path: Option<PathBuf>`，传给 `build_prompt` |
| `src-tauri/src/app/lifecycle.rs` | 修改 | `send_message_cmd` 从 `state.paths` 取 prompt override path 传给 `spawn_active` |
| `src-tauri/src/knowledge/playbook.rs` | 修改 | `get_playbook` 签名加 `playbooks_dir: &Path` 参数 |
| `src-tauri/playbooks/` | 删除 | 源码树占位目录（仅 `.gitkeep`） |
| `docs/architecture/infrastructure.md` | 修改 | 补充"文件布局"章节 |
| `docs/architecture/playbook.md` | 修改 | 修订 playbook 位置 |
| `docs/architecture/overview.md` | 修改 | 修订决策 #11 + MCP config 描述 |
| `AGENTS.md` | 修改 | 补充文件管理约定 |

---

## Task 1: 创建 `infra/paths.rs` 模块

**Files:**
- Create: `src-tauri/src/infra/paths.rs`
- Modify: `src-tauri/src/infra/mod.rs`

- [ ] **Step 1: Write the failing tests for `Paths`**

Create `src-tauri/src/infra/paths.rs` with only the test module:

```rust
use std::path::PathBuf;

pub struct Paths {
    root: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_path_returns_root_join_friday_db() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let db = paths.db_path();
        assert_eq!(db, tmp.path().join("friday.db"));
    }

    #[test]
    fn test_log_dir_returns_root_join_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.log_dir(), tmp.path().join("logs"));
    }

    #[test]
    fn test_playbooks_dir_returns_root_join_playbooks() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.playbooks_dir(), tmp.path().join("playbooks"));
    }

    #[test]
    fn test_skills_dir_returns_root_join_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.skills_dir(), tmp.path().join("skills"));
    }

    #[test]
    fn test_prompts_dir_returns_root_join_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.prompts_dir(), tmp.path().join("prompts"));
    }

    #[test]
    fn test_artifacts_dir_returns_root_join_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.artifacts_dir(), tmp.path().join("artifacts"));
    }

    #[test]
    fn test_session_artifacts_dir_joins_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let dir = paths.session_artifacts_dir("abc-123");
        assert_eq!(dir, tmp.path().join("artifacts").join("abc-123"));
    }

    #[test]
    fn test_ensure_dirs_creates_all_five_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(tmp.path().join("logs").is_dir());
        assert!(tmp.path().join("playbooks").is_dir());
        assert!(tmp.path().join("skills").is_dir());
        assert!(tmp.path().join("prompts").is_dir());
        assert!(tmp.path().join("artifacts").is_dir());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_db_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("friday.db").exists());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_session_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("artifacts").join("some-session").exists());
    }

    #[test]
    fn test_ensure_dirs_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());

        paths.ensure_dirs().unwrap();
        // Second call should not error
        paths.ensure_dirs().unwrap();

        assert!(tmp.path().join("logs").is_dir());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_session_subdir_after_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());

        paths.ensure_dirs().unwrap();
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("artifacts").join("some-session").exists());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib infra::paths`
Expected: FAIL — methods on `Paths` not defined (compile errors: `db_path`, `log_dir`, etc. not found)

- [ ] **Step 3: Write the implementation**

Add the `impl Paths` block above the test module in `src-tauri/src/infra/paths.rs`:

```rust
use std::path::PathBuf;

pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("friday.db")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn playbooks_dir(&self) -> PathBuf {
        self.root.join("playbooks")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn prompts_dir(&self) -> PathBuf {
        self.root.join("prompts")
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn session_artifacts_dir(&self, session_id: &str) -> PathBuf {
        self.artifacts_dir().join(session_id)
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.log_dir(),
            self.playbooks_dir(),
            self.skills_dir(),
            self.prompts_dir(),
            self.artifacts_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1 stay here
}
```

- [ ] **Step 4: Register the module in `infra/mod.rs`**

Modify `src-tauri/src/infra/mod.rs` — add `pub mod paths;`:

```rust
pub mod db;
pub mod logging;
pub mod paths;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib infra::paths`
Expected: PASS — all 12 tests pass

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/infra/paths.rs src-tauri/src/infra/mod.rs
git commit -m "feat: add infra/paths.rs centralized path resolution module"
```

---

## Task 2: 修改 `logging.rs` 签名 — 接收 `log_dir` 而非 `app_data_dir`

**Files:**
- Modify: `src-tauri/src/infra/logging.rs:19-21`
- Test: existing tests in `logging.rs` (`test_logging_init_creates_log_dir`, `test_init_returns_logging_guard`, `test_set_level_changes_filter`, `test_panic_hook_installed`)

- [ ] **Step 1: Update `init` signature and body**

Modify `src-tauri/src/infra/logging.rs`. Change the `init` function signature from `init(app_data_dir: PathBuf)` to `init(log_dir: PathBuf)`, and remove the internal `.join("logs")` + `create_dir_all`:

Current code (lines 19-22):
```rust
pub fn init(app_data_dir: PathBuf) -> LoggingGuard {
    let log_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = rolling::daily(&log_dir, "friday.log");
```

Replace with:
```rust
pub fn init(log_dir: PathBuf) -> LoggingGuard {
    let file_appender = rolling::daily(&log_dir, "friday.log");
```

The rest of `init` (from line 23 onward) stays unchanged — it already uses `log_dir`.

- [ ] **Step 2: Update logging tests to pass `log_dir` instead of `app_data_dir`**

The existing tests call `init(tmp.path().to_path_buf())` expecting `init` to create `logs/` internally. Now `init` receives the `log_dir` directly, so tests must pass `tmp.path().join("logs")` and pre-create the directory (since `ensure_dirs` is called in `lib.rs` setup, not in `init`).

In the `#[cfg(test)] mod tests` block of `logging.rs`, update all `init` calls. There are 4 test functions that call `init`:

`test_logging_init_creates_log_dir`:
```rust
#[test]
fn test_logging_init_creates_log_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let log_dir = tmp.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    assert!(log_dir.exists());

    let _guard = init(log_dir);
}
```

`test_init_returns_logging_guard`:
```rust
#[test]
fn test_init_returns_logging_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let log_dir = tmp.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let guard = init(log_dir);
    let _handle = &guard.filter_handle;
}
```

`test_set_level_changes_filter`:
```rust
#[test]
fn test_set_level_changes_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let log_dir = tmp.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let guard = init(log_dir);
    let handle = &guard.filter_handle;

    let result = set_level(handle, "trace");
    assert!(result.is_ok());

    let result = set_level(handle, "info");
    assert!(result.is_ok());
}
```

`test_panic_hook_installed`:
```rust
#[test]
fn test_panic_hook_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let log_dir = tmp.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let _guard = init(log_dir);

    let _hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
}
```

- [ ] **Step 3: Run logging tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib infra::logging`
Expected: PASS — all 7 logging tests pass (4 init-related + 3 cleanup tests which don't call init)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/infra/logging.rs
git commit -m "refactor: logging::init receives log_dir instead of app_data_dir"
```

---

## Task 3: 修改 `db.rs` 签名 — 接收 `db_path` 而非 `app_data_dir`

**Files:**
- Modify: `src-tauri/src/infra/db.rs:4-6`
- Test: existing tests in `db.rs` + tests in `agents.rs`, `session.rs`, `lifecycle.rs`, `spawn.rs` that call `db::init`

- [ ] **Step 1: Update `init` signature and body**

Modify `src-tauri/src/infra/db.rs`. Change `init(app_data_dir: PathBuf)` to `init(db_path: PathBuf)`, and remove the internal `.join("friday.db")`:

Current code (lines 4-6):
```rust
pub async fn init(app_data_dir: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_path = app_data_dir.join("friday.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
```

Replace with:
```rust
pub async fn init(db_path: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
```

- [ ] **Step 2: Update db.rs tests**

In the `#[cfg(test)] mod tests` block of `db.rs`, update all `db::init` calls to pass `tmp.path().join("friday.db")`:

`test_db_init_creates_tables`:
```rust
let pool = init(tmp.path().join("friday.db")).await.unwrap();
```

`test_db_init_creates_agents_index`:
```rust
let pool = init(tmp.path().join("friday.db")).await.unwrap();
```

`test_db_init_creates_indexes`:
```rust
let pool = init(tmp.path().join("friday.db")).await.unwrap();
```

`test_db_init_adds_conversation_columns`:
```rust
let pool = init(tmp.path().join("friday.db")).await.unwrap();
```

- [ ] **Step 3: Update all other test files that call `db::init`**

Search for all `db::init(tmp.path()` calls across the codebase and update them to `db::init(tmp.path().join("friday.db"))`.

Files to update (each has a `setup()` helper or direct call):
- `src-tauri/src/app/agents.rs` — `setup()` function at line ~256: `db::init(tmp.path().to_path_buf())` → `db::init(tmp.path().join("friday.db"))`
- `src-tauri/src/app/session.rs` — `setup()` function at line ~153: `db::init(tmp.path().to_path_buf())` → `db::init(tmp.path().join("friday.db"))`
- `src-tauri/src/app/lifecycle.rs` — `test_close_session_updates_status` at line ~241: `db::init(tmp.path().to_path_buf())` → `db::init(tmp.path().join("friday.db"))`
- `src-tauri/src/agent/spawn.rs` — 3 test functions:
  - `test_spawn_active_accepts_session_id_param`: `db::init(tmp.path().to_path_buf())` → `db::init(tmp.path().join("friday.db"))`
  - `test_spawn_active_returns_no_active_agent_when_db_empty`: same
  - `test_spawn_active_returns_binary_missing_when_path_invalid`: same

- [ ] **Step 4: Run all tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests pass. If `lib.rs` doesn't compile yet because it still calls `db::init(data_dir)`, that's expected — we fix `lib.rs` in Task 5. For now, only run the library tests that don't depend on `lib.rs` compiling:

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib infra::db`
Expected: PASS — 4 db tests pass

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib app`
Expected: PASS — all app tests pass (agents, session, lifecycle)

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent`
Expected: PASS — all agent tests pass

Note: `cargo check` will fail at this point because `lib.rs` still calls `init(data_dir)` and `init(data_dir.clone())` with the old signatures. This is fixed in Task 5. The per-module tests pass because they call `db::init` / `logging::init` directly with the new signatures.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/infra/db.rs src-tauri/src/app/agents.rs src-tauri/src/app/session.rs src-tauri/src/app/lifecycle.rs src-tauri/src/agent/spawn.rs
git commit -m "refactor: db::init receives db_path instead of app_data_dir"
```

---

## Task 4: 修改 `prompt.rs` — 加 `build_system_prompt` + `override_path` 参数

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`
- Test: new tests in `prompt.rs`

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)] mod tests` block to `src-tauri/src/agent/prompt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_system_prompt_uses_default_when_no_override() {
        let result = build_system_prompt(None);
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_system_prompt_uses_default_when_file_not_found() {
        let path = PathBuf::from("/nonexistent/path/friday.md");
        let result = build_system_prompt(Some(&path));
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_system_prompt_uses_override_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "You are a custom assistant.").unwrap();

        let result = build_system_prompt(Some(&path));
        assert_eq!(result, "You are a custom assistant.");
    }

    #[test]
    fn test_build_system_prompt_falls_back_when_file_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "   \n  ").unwrap();

        let result = build_system_prompt(Some(&path));
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_prompt_includes_system_and_message() {
        let result = build_prompt("hello world", None);
        assert!(result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_build_prompt_uses_override_system_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "Custom system.").unwrap();

        let result = build_prompt("hello", Some(&path));
        assert!(result.contains("Custom system."));
        assert!(!result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent::prompt`
Expected: FAIL — `build_system_prompt` not defined; `build_prompt` doesn't accept `override_path` parameter

- [ ] **Step 3: Implement `build_system_prompt` and update `build_prompt`**

Modify `src-tauri/src/agent/prompt.rs`. Keep `FRIDAY_SYSTEM_PROMPT` const unchanged. Replace the `build_prompt` function and add `build_system_prompt`:

```rust
use std::path::Path;

const FRIDAY_SYSTEM_PROMPT: &str = r#"你是 Friday，一个面向软件开发人员的远程环境运行时故障诊断助手。

## 身份
- 你的名字是 Friday，不是 opencode，不是其他任何名字。
- 当用户问"你是谁"时，回答你是 Friday。
- 不要提及底层的模型名称（如 glm、claude 等）或实现工具。

## 能力
- 帮助开发人员诊断远程环境中的运行时故障（OOM、CPU 飙高、连接池耗尽等）。
- 当前版本你的工具能力有限，主要依靠对话和分析。后续会集成 jstat、jcmd、arthas 等诊断工具。
- 诚实告知能力边界：做不到的事情直接说，不要编造。

## 风格
- 简洁直接，不啰嗦。开发者要的是答案，不是寒暄。
- 中文交流，技术术语可以保留英文。
- 代码和命令用代码块包裹。
- 长回答分段，用列表和标题组织结构。

## 限制
- 你不是通用聊天机器人。话题应围绕软件诊断、系统排查、开发效率。
- 不做与诊断无关的事情（写诗、聊天、讲笑话等）。
- 不确定的事情先说不确定，不要瞎猜。
"#;

pub fn build_system_prompt(override_path: Option<&Path>) -> String {
    if let Some(path) = override_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                return content;
            }
        }
    }
    FRIDAY_SYSTEM_PROMPT.to_string()
}

pub fn build_prompt(message: &str, override_path: Option<&Path>) -> String {
    let system = build_system_prompt(override_path);
    format!("{system}\n\n---\n\n用户消息：{message}")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent::prompt`
Expected: PASS — all 6 prompt tests pass

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: add prompt override layer via build_system_prompt"
```

---

## Task 5: 修改 `spawn.rs` — `spawn_active` 接收 prompt override path

**Files:**
- Modify: `src-tauri/src/agent/spawn.rs:64-69` (function signature)
- Modify: `src-tauri/src/agent/spawn.rs` tests (signature update)
- Test: existing tests in `spawn.rs`

- [ ] **Step 1: Update `spawn_active` signature and move prompt building inside**

The current flow: `lifecycle.rs` calls `prompt::build_prompt(&message)` and passes the built prompt string to `spawn_active` as the `message` parameter. `spawn_active` writes it to stdin.

New flow: `lifecycle.rs` passes the raw user `message` + `prompt_override_path` to `spawn_active`. `spawn_active` calls `prompt::build_prompt(&message, override_path)` internally.

Modify `src-tauri/src/agent/spawn.rs`:

1. Add `use crate::agent::prompt;` import at the top of the file (after the existing `use` statements, before `pub struct AgentProcess`).

2. Update the `spawn_active` signature — add `prompt_override_path: Option<PathBuf>` parameter:

```rust
#[tracing::instrument(skip(pool))]
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    opencode_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
) -> Result<AgentProcess, SpawnError> {
```

3. After the agent path is resolved (after the `tracing::info!(raw_path = ..., exe_path = ..., "resolved opencode executable")` call, currently around line 88), add the prompt building:

```rust
    let prompt_text = prompt::build_prompt(&message, prompt_override_path.as_deref());
    tracing::info!(prompt_len = prompt_text.len(), "prompt built");
```

4. In the stdin writing block (currently around line 126-137), change `msg` from `message.clone()` to `prompt_text.clone()`:

```rust
    if let Some(mut stdin) = child.stdin.take() {
        let msg = prompt_text.clone();
        tokio::spawn(async move {
            tracing::info!(msg_len = msg.len(), "writing prompt to stdin");
            if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                tracing::error!(?e, "failed to write prompt to stdin");
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::error!(?e, "failed to close stdin");
            }
            tracing::info!("stdin written and closed");
        });
    }
```

- [ ] **Step 2: Update `spawn.rs` tests to pass the new parameter**

All 3 test functions that call `spawn_active` need the extra `None` argument:

`test_spawn_active_accepts_session_id_param`:
```rust
let result = spawn_active(&pool, "test-sid".to_string(), String::new(), None, None).await;
```

`test_spawn_active_returns_no_active_agent_when_db_empty`:
```rust
let result = spawn_active(&pool, "test-session".to_string(), String::new(), None, None).await;
```

`test_spawn_active_returns_binary_missing_when_path_invalid`:
```rust
let result = spawn_active(&pool, "test-session".to_string(), "test message".to_string(), None, None).await;
```

- [ ] **Step 3: Run spawn tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent::spawn`
Expected: PASS — all 5 spawn tests pass (3 that call spawn_active + 2 that test resolve_native_exe)

Note: `cargo check` on the full crate will still fail because `lifecycle.rs` still calls `spawn_active` with the old 4-argument signature. This is fixed in Task 6.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/agent/spawn.rs
git commit -m "refactor: spawn_active receives prompt_override_path and builds prompt internally"
```

---

## Task 6: 修改 `lifecycle.rs` — 从 `state.paths` 取 override path 传给 `spawn_active`

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs:104-116`

- [ ] **Step 1: Update `send_message_cmd` to pass prompt override path**

In `src-tauri/src/app/lifecycle.rs`, the `send_message_cmd` function currently:
1. Builds the prompt at line 105: `let prompt_text = prompt::build_prompt(&message);`
2. Calls `spawn_active` at line 111: `spawn_active(&pool, friday_session_id.clone(), prompt_text, oc_session_id)`

We need to:
1. Remove the `prompt::build_prompt` call (moved into `spawn_active`)
2. Get the prompt override path from `state.paths`
3. Pass raw `message` and override path to `spawn_active`

Also remove the `use crate::agent::prompt;` import at line 2 (no longer used in lifecycle.rs).

Current code (lines 1-2):
```rust
use super::session;
use crate::agent::prompt;
```

Replace with:
```rust
use super::session;
```

Current code (lines 104-116):
```rust
    // Build prompt and spawn opencode
    let prompt_text = prompt::build_prompt(&message);
    tracing::info!(
        session_id = %friday_session_id,
        prompt_len = prompt_text.len(),
        "spawning opencode"
    );
    let agent_process = spawn_active(&pool, friday_session_id.clone(), prompt_text, oc_session_id)
        .await
        .map_err(|e| {
            tracing::error!(?e, "failed to spawn opencode");
            e.to_string()
        })?;
```

Replace with:
```rust
    // Get prompt override path and spawn opencode
    let prompt_override_path = state.paths.prompts_dir().join("friday.md");
    tracing::info!(
        session_id = %friday_session_id,
        "spawning opencode"
    );
    let agent_process = spawn_active(
        &pool,
        friday_session_id.clone(),
        message,
        oc_session_id,
        Some(prompt_override_path),
    )
    .await
    .map_err(|e| {
        tracing::error!(?e, "failed to spawn opencode");
        e.to_string()
    })?;
```

- [ ] **Step 2: Run lifecycle tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib app::lifecycle`
Expected: PASS — 2 lifecycle tests pass

Note: Full `cargo check` will still fail because `lib.rs` doesn't have `paths` field on `AppState` yet. Fixed in Task 7.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs
git commit -m "refactor: send_message_cmd passes prompt override path from state.paths"
```

---

## Task 7: 修改 `lib.rs` — 构造 `Paths`，存入 `AppState`，分发路径给 init 函数

**Files:**
- Modify: `src-tauri/src/lib.rs:16-21` (AppState struct)
- Modify: `src-tauri/src/lib.rs:26-44` (setup block)

- [ ] **Step 1: Update `AppState` struct to include `paths`**

Modify `src-tauri/src/lib.rs`. Add `paths: Paths` field and import:

Current (lines 1-21):
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
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Registry};

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
}
```

Replace with:
```rust
mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod tools;

use app::events::EventBus;
use infra::paths::Paths;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Registry};

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
}
```

- [ ] **Step 2: Update setup block to construct `Paths` and distribute paths**

Modify the setup block in `lib.rs`. Current (lines 26-44):
```rust
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let guard = infra::logging::init(data_dir.clone());
            let filter_handle = guard.filter_handle();
            let pool = tauri::async_runtime::block_on(infra::db::init(data_dir))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
            });
            app.manage(guard);

            Ok(())
        })
```

Replace with:
```rust
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let paths = Paths::new(data_dir.clone());
            paths.ensure_dirs()?;

            let guard = infra::logging::init(paths.log_dir());
            let filter_handle = guard.filter_handle();
            let pool = tauri::async_runtime::block_on(infra::db::init(paths.db_path()))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
                paths,
            });
            app.manage(guard);

            Ok(())
        })
```

- [ ] **Step 3: Update `lib.rs` test to include `paths` field**

The test `test_filter_handle_cloneable_and_usable` at line 67-74 doesn't construct `AppState` directly, so no change needed there. But verify there are no other `AppState` constructions in tests.

Check: `grep -r "AppState {" src-tauri/src/` — only in `lib.rs:36`. No test constructs `AppState` directly. Good.

- [ ] **Step 4: Run `cargo check` to verify the full crate compiles**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS — no compile errors

- [ ] **Step 5: Run all tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests pass

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: construct Paths in setup, store in AppState, distribute to init functions"
```

---

## Task 8: 对齐 `playbook.rs` 签名

**Files:**
- Modify: `src-tauri/src/knowledge/playbook.rs:17-19`

- [ ] **Step 1: Update `get_playbook` signature**

Modify `src-tauri/src/knowledge/playbook.rs`. Add `playbooks_dir: &Path` parameter and `use std::path::Path;` import. Function body stays `todo!()` per spec §10.2.

Current (lines 1-19):
```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playbook {
    pub symptom: String,
    pub steps: Vec<PlaybookStep>,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybookStep {
    pub tool: String,
    pub args: serde_json::Value,
    pub interpret: String,
}

pub async fn get_playbook(_symptom: &str) -> Option<Playbook> {
    todo!()
}
```

Replace with:
```rust
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playbook {
    pub symptom: String,
    pub steps: Vec<PlaybookStep>,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybookStep {
    pub tool: String,
    pub args: serde_json::Value,
    pub interpret: String,
}

pub async fn get_playbook(_playbooks_dir: &Path, _symptom: &str) -> Option<Playbook> {
    todo!()
}
```

- [ ] **Step 2: Run `cargo check` to verify no callers break**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS — `get_playbook` has no callers yet (it's `todo!()`), so no breakage

- [ ] **Step 3: Run all tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests pass

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/knowledge/playbook.rs
git commit -m "refactor: align get_playbook signature with playbooks_dir parameter"
```

---

## Task 9: 删除 `src-tauri/playbooks/` 占位目录

**Files:**
- Delete: `src-tauri/playbooks/` (contains only `.gitkeep`)

- [ ] **Step 1: Delete the directory**

```bash
git rm -r src-tauri/playbooks/
```

- [ ] **Step 2: Run `cargo check` to verify nothing references it**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS — no references to `src-tauri/playbooks/` in code (it was just a placeholder)

- [ ] **Step 3: Commit**

```bash
git commit -m "chore: remove src-tauri/playbooks/ placeholder (runtime-generated in app_data)"
```

---

## Task 10: 更新架构文档

**Files:**
- Modify: `docs/architecture/infrastructure.md`
- Modify: `docs/architecture/playbook.md`
- Modify: `docs/architecture/overview.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Update `docs/architecture/infrastructure.md`**

Add a "文件布局" section after the existing "日志与可观测" section. The full new file:

```markdown
# 基础设施

## 凭证管理

- **SSH 私钥**：引用用户已有的 `~/.ssh/` 路径，不复制不存储；Friday 自管理的 key 路径记录在 SQLite，key 本身仍在用户 ssh 目录。
- **LLM API key / 目标机密码**：存 OS 密钥链（Windows Credential Manager / macOS Keychain / Linux Secret Service），通过 Rust `keyring` crate 跨平台封装。SQLite 里只存环境标识。
- **其余配置**（环境信息、连接参数等）：明文入 SQLite。内网工具，安全要求从简。

## 文件布局

所有运行时文件统一在 Tauri `app_data_dir()`（identifier `com.friday.app`）下，通过 `infra/paths.rs` 集中解析，不内联 `join`。

```
<app_data>/
├── friday.db                        # SQLite: sessions/agents/diagnosis_steps/tool_calls/environments
├── logs/
│   └── friday.log.{date}            # tracing 每日轮转, 7 天自动清理
├── playbooks/                       # agent 运行时生成的诊断知识 (YAML), 用户可编辑
├── skills/                          # Friday 自有 skill (agent 生成, 能力包)
├── prompts/                         # 预留: 未来 GUI 编辑人格 prompt 的覆盖层
│                                    #   v1 为空 — 代码内 const 作默认; 有 friday.md 则覆盖
└── artifacts/                       # 从目标机拉取的产物, 按会话隔离, 持久保留
    └── <session_id>/
```

**不纳入 Friday 文件管理的边界**：

| 项 | 归属 | 说明 |
|----|------|------|
| opencode 工作环境/skill | 用户 `~/.opencode/` | Friday 不管理，spawn 设 PWD=home 复用 |
| SSH 私钥 | 用户 `~/.ssh/` | 引用不复制 |
| 凭证（密码/API key） | OS 密钥链 | 不落文件 |
| migrations | 编译进二进制 `include_str!` | 非运行时文件 |
| 真正临时文件 | `std::env::temp_dir()` | 若出现纯临时需求，不持久化 |

## 日志与可观测

- **Friday 运行日志**：`tracing` + `tracing-appender` 文件轮转，写入 `Paths::log_dir()`（即 `<app_data>/logs/`）。INFO 为主，关键路径 DEBUG。
- **诊断过程数据**：会话/步骤/工具调用/结果持久化到 SQLite，供用户回看历史诊断。
- 两者分离，互不污染。
- 详细规范见 [日志规范（强制约束）](logging-standard.md)。
```

- [ ] **Step 2: Update `docs/architecture/playbook.md`**

```markdown
# 知识层（Playbook）

- 形态：结构化 YAML/TOML（故障模式 → 推荐工具序列 + 指标判读）+ 自然语言说明。
- 注入方式：prompt 精简索引 + MCP 工具 `get_playbook(symptom)` 按需获取完整内容。
- agent 不调 `get_playbook` 也能直接调诊断工具——playbook 是辅助不是强制。
- 位置：`<app_data>/playbooks/`，agent 运行时生成，用户可编辑。加 playbook 不改 Rust 代码。
```

- [ ] **Step 3: Update `docs/architecture/overview.md`**

Two changes:

1. Decision #11 storage row — add file layout reference. Current row:
```
| 11 | 存储 | SQLite |
```
Replace with:
```
| 11 | 存储 | SQLite；文件布局统一在 `app_data_dir`，见 [基础设施](infrastructure.md#文件布局) |
```

2. Agent 编排层 MCP config description. Current text in the Agent 编排层 box:
```
│ - 命令行传 prompt + 临时 MCP config 文件（用完删除）       │
```
Replace with:
```
│ - 命令行传 prompt；MCP config 注入走 opencode 自身配置机制  │
│   Friday 不单独管理临时配置文件                             │
```

- [ ] **Step 4: Update `AGENTS.md`**

Add a file management convention to the "约定" section. After the existing logging convention bullet, add:

```markdown
- **文件管理**：所有运行时文件路径通过 `infra/paths.rs` 的 `Paths` struct 统一解析，不内联 `.join()`。`Paths` 存入 `AppState`，各模块从 `State<AppState>` 取路径。新增文件类别时，在 `Paths` 加方法 + `ensure_dirs()` 加目录，不散落到各模块。详见 [文件管理设计](docs/superpowers/specs/2026-08-21-file-management-design.md)。
```

- [ ] **Step 5: Commit**

```bash
git add docs/architecture/infrastructure.md docs/architecture/playbook.md docs/architecture/overview.md AGENTS.md
git commit -m "docs: update architecture docs with file management layout"
```

---

## Task 11: 最终验证

- [ ] **Step 1: Run `cargo check`**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS — no errors, no warnings related to our changes

- [ ] **Step 2: Run all tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — all tests pass

- [ ] **Step 3: Run frontend typecheck (verify no IPC breakage)**

Run: `pnpm typecheck`
Expected: PASS — no type errors (we didn't change any IPC bindings)

- [ ] **Step 4: Verify git status is clean**

Run: `git status`
Expected: "nothing to commit, working tree clean"

- [ ] **Step 5: Review the full diff**

Run: `git diff main~10 --stat` (or appropriate range)
Expected: changes match the spec's §10 implementation checklist
