# 代码读取能力（code_* 工具集）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Friday 补齐读代码能力——agent 问询用户代码仓地址+分支（分支/tag/commitid），本机完整 clone + worktree 检出，提供 code_* 六个 MCP 工具（查映射/开仓/状态/读/搜/列），服务→仓地址映射持久化并在设置页可管理。

**Architecture:** 新建 `src-tauri/src/code/` 域（git 子进程封装 + CodeRepoManager 异步 open 管线 + 读/搜/列实现），`tools/builtin/code/` 注册 6 个 MCP 工具（全部 `needs_channel: false`、只读自主），SQLite `service_repos` 表存映射，设置弹窗加「代码仓」区块（映射列表 + 缓存仓库管理）。clone/fetch 走本机 `git` 命令（认证依赖本机 git 配置），仓按 `url_hash` 缓存在 `app_data_dir/repos/`，每个 (repo, ref) 一个 git worktree。

**Tech Stack:** Rust（tokio、sha2、regex、ignore(新依赖)、sqlx）、React/TS（既有 types/ipc/组件模式）。

**Spec:** `docs/superpowers/specs/2026-09-11-code-reading-design.md`

**验证命令:** `cargo check --manifest-path src-tauri/Cargo.toml` / `cargo test --manifest-path src-tauri/Cargo.toml` / `pnpm typecheck`

---

### Task 1: `Paths::repos_dir` + migration 0010 + `service_repos` 存储层

**Files:**
- Modify: `src-tauri/src/infra/paths.rs`
- Create: `src-tauri/migrations/0010_service_repos.sql`
- Modify: `src-tauri/src/infra/db.rs`（migration 装载）
- Create: `src-tauri/src/app/service_repos.rs`
- Modify: `src-tauri/src/app/mod.rs`

- [ ] **Step 1.1: 写 paths.rs 的失败测试**

在 `src-tauri/src/infra/paths.rs` 的 `mod tests` 中追加（同时改 `test_ensure_dirs_creates_all_seven_subdirs` 为八个目录）：

```rust
    #[test]
    fn test_repos_dir_returns_root_join_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.repos_dir(), tmp.path().join("repos"));
    }
```

并把现有测试 `test_ensure_dirs_creates_all_seven_subdirs` 改名 `test_ensure_dirs_creates_all_eight_subdirs`、追加 `assert!(tmp.path().join("repos").is_dir());`。

- [ ] **Step 1.2: 运行确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml repos_dir`
Expected: 编译失败 `no method named repos_dir`

- [ ] **Step 1.3: 实现 paths.rs**

在 `cache_dir()` 方法后加：

```rust
    pub fn repos_dir(&self) -> PathBuf {
        self.root.join("repos")
    }
```

`ensure_dirs()` 的数组中 `self.cache_dir(),` 后加一行 `self.repos_dir(),`。

- [ ] **Step 1.4: 建 migration 文件 `src-tauri/migrations/0010_service_repos.sql`**

```sql
-- 服务 → 代码仓映射记忆（code_* 工具）。
-- repo_url 权威；last_ref 仅提示（分支经常变，agent 每次仍须与用户确认）。
CREATE TABLE IF NOT EXISTS service_repos (
    service      TEXT PRIMARY KEY,
    repo_url     TEXT NOT NULL,
    last_ref     TEXT,
    updated_at   TEXT NOT NULL,
    last_used_at TEXT NOT NULL
);
```

- [ ] **Step 1.5: db.rs 装载 migration**

`src-tauri/src/infra/db.rs` 在 `let schema9 = ...; sqlx::query(schema9)...` 块之后（`transport_type` UPDATE 之前）插入：

```rust
    // Migration (code reading)：服务 → 代码仓映射记忆
    let schema10 = include_str!("../../migrations/0010_service_repos.sql");
    sqlx::query(schema10).execute(&pool).await?;
```

- [ ] **Step 1.6: 写 service_repos.rs 存储层（含测试）**

创建 `src-tauri/src/app/service_repos.rs`：

```rust
use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum ServiceRepoError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Serialize, Clone, Debug)]
pub struct ServiceRepoRow {
    pub service: String,
    pub repo_url: String,
    pub last_ref: Option<String>,
    pub updated_at: String,
    pub last_used_at: String,
}

const COLUMNS: &str = "service, repo_url, last_ref, updated_at, last_used_at";

fn row_to_service_repo(r: &sqlx::sqlite::SqliteRow) -> ServiceRepoRow {
    ServiceRepoRow {
        service: r.get("service"),
        repo_url: r.get("repo_url"),
        last_ref: r.get("last_ref"),
        updated_at: r.get("updated_at"),
        last_used_at: r.get("last_used_at"),
    }
}

/// upsert：repo_url 权威覆盖、last_ref 记本次
pub async fn upsert_service_repo(
    pool: &SqlitePool,
    service: &str,
    repo_url: &str,
    last_ref: Option<&str>,
) -> Result<(), ServiceRepoError> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO service_repos (service, repo_url, last_ref, updated_at, last_used_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(service) DO UPDATE SET \
           repo_url = excluded.repo_url, last_ref = excluded.last_ref, \
           updated_at = excluded.updated_at, last_used_at = excluded.last_used_at",
    )
    .bind(service)
    .bind(repo_url)
    .bind(last_ref)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查映射；命中时刷新 last_used_at
pub async fn get_service_repo(
    pool: &SqlitePool,
    service: &str,
) -> Result<Option<ServiceRepoRow>, ServiceRepoError> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM service_repos WHERE service = ?"))
        .bind(service)
        .fetch_optional(pool)
        .await?;
    if row.is_some() {
        sqlx::query("UPDATE service_repos SET last_used_at = ? WHERE service = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(service)
            .execute(pool)
            .await?;
    }
    Ok(row.map(|r| row_to_service_repo(&r)))
}

pub async fn list_service_repos(pool: &SqlitePool) -> Result<Vec<ServiceRepoRow>, ServiceRepoError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM service_repos ORDER BY last_used_at DESC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_service_repo).collect())
}

pub async fn delete_service_repo(pool: &SqlitePool, service: &str) -> Result<(), ServiceRepoError> {
    sqlx::query("DELETE FROM service_repos WHERE service = ?")
        .bind(service)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_upsert_then_get_roundtrip() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "BarService", "https://git.example.com/bar.git", Some("main"))
            .await
            .unwrap();
        let row = get_service_repo(&pool, "BarService").await.unwrap().unwrap();
        assert_eq!(row.repo_url, "https://git.example.com/bar.git");
        assert_eq!(row.last_ref.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn test_upsert_overwrites_url_and_ref() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "S", "https://a.git", Some("main")).await.unwrap();
        upsert_service_repo(&pool, "S", "https://b.git", Some("release-1.2")).await.unwrap();
        let row = get_service_repo(&pool, "S").await.unwrap().unwrap();
        assert_eq!(row.repo_url, "https://b.git");
        assert_eq!(row.last_ref.as_deref(), Some("release-1.2"));
    }

    #[tokio::test]
    async fn test_get_missing_returns_none() {
        let pool = test_pool().await;
        assert!(get_service_repo(&pool, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_refreshes_last_used_at() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "S", "https://a.git", None).await.unwrap();
        let before = get_service_repo(&pool, "S").await.unwrap().unwrap().last_used_at;
        // 时间戳至少非空且再次读取成功（rfc3339 精度内可能相等，断言可再读）
        let after = get_service_repo(&pool, "S").await.unwrap().unwrap().last_used_at;
        assert!(!before.is_empty());
        assert!(!after.is_empty());
    }

    #[tokio::test]
    async fn test_list_orders_by_last_used_desc_and_delete() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "A", "https://a.git", None).await.unwrap();
        upsert_service_repo(&pool, "B", "https://b.git", None).await.unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 2);
        delete_service_repo(&pool, "A").await.unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].service, "B");
    }
}
```

`src-tauri/src/app/mod.rs` 加 `pub mod service_repos;`（按字母序在 `pub mod settings;` 前）。

- [ ] **Step 1.7: 运行测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- paths:: service_repos`
Expected: 全部 PASS

- [ ] **Step 1.8: Commit**

```bash
git add src-tauri/src/infra/paths.rs src-tauri/src/infra/db.rs src-tauri/migrations/0010_service_repos.sql src-tauri/src/app/service_repos.rs src-tauri/src/app/mod.rs
git commit -m "feat: service-repo mapping storage and repos dir (code reading groundwork)"
```

---

### Task 2: `code::git` — git 子进程封装

**Files:**
- Modify: `src-tauri/Cargo.toml`（加 `ignore` 依赖，Task 4 用，一并加）
- Create: `src-tauri/src/code/mod.rs`
- Create: `src-tauri/src/code/git.rs`
- Modify: `src-tauri/src/lib.rs`（加 `mod code;`，模块声明序：`mod arthas;` 后、`mod exec;` 前）

- [ ] **Step 2.1: Cargo.toml 加依赖**

`[dependencies]` 中 `regex = "1"` 行后加：

```toml
ignore = "0.4"
```

- [ ] **Step 2.2: 创建 `src-tauri/src/code/mod.rs`（含测试 fixture 工具）**

```rust
//! 代码仓读取域：本机 git clone 缓存 + worktree 检出 + 只读工具实现。

pub mod git;
pub mod manager;
pub mod search;

pub use manager::{CodeRepoManager, RepoCacheEntry, RepoPhase, RepoState};

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git binary required for tests");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// main 分支当前 HEAD 的 commit sha
    pub fn head_sha(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse");
        assert!(out.status.success(), "rev-parse failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// 本地源仓 fixture：main 分支 2 提交（首提交打 tag v1.0）+ feature-x 分支。
    /// 返回 file:// URL（仓需保留在 tempdir 中，删除即可模拟远端不可达）。
    pub fn make_source_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "friday@test.local"]);
        git(dir, &["config", "user.name", "friday-test"]);
        std::fs::write(dir.join("README.md"), "hello friday\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src").join("Bar.java"),
            "package com.foo;\npublic class Bar {\n    void baz() {}\n}\n",
        )
        .unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
        git(dir, &["tag", "v1.0"]);
        std::fs::write(dir.join("src").join("Baz.java"), "package com.foo;\npublic class Baz {}\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "add Baz"]);
        git(dir, &["checkout", "-b", "feature-x"]);
        std::fs::write(dir.join("src").join("Feature.java"), "package com.foo;\npublic class Feature {}\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "feature work"]);
        git(dir, &["checkout", "main"]);
        git::file_url(dir)
    }
}
```

注意：`pub mod manager; pub mod search;` 先注释掉（Task 3/4 才创建），否则编译不过。写成：

```rust
pub mod git;
// pub mod manager;   // Task 3
// pub mod search;    // Task 4
```

- [ ] **Step 2.3: 写 `src-tauri/src/code/git.rs`（含测试）**

```rust
//! git 子进程封装。认证完全依赖本机 git 配置（credential manager / .netrc /
//! URL 内嵌 token）；GIT_TERMINAL_PROMPT=0 防止终端交互挂死后台任务。
//! stderr 全量读取并记录（日志规范：不截断）。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// 目录名安全短哈希：sha256(url) 前 16 个十六进制字符
pub fn url_hash(repo_url: &str) -> String {
    let hex = format!("{:x}", Sha256::digest(repo_url.as_bytes()));
    hex[..16].to_string()
}

/// worktree 子目录名：sha256(ref) 前 16 个十六进制字符
pub fn ref_hash(git_ref: &str) -> String {
    let hex = format!("{:x}", Sha256::digest(git_ref.as_bytes()));
    hex[..16].to_string()
}

/// repo_id = hash(url + "\n" + ref)：同仓同 ref 幂等
pub fn repo_id(repo_url: &str, git_ref: &str) -> String {
    let mut h = Sha256::new();
    h.update(repo_url.as_bytes());
    h.update(b"\n");
    h.update(git_ref.as_bytes());
    let hex = format!("{:x}", h.finalize());
    hex[..16].to_string()
}

/// git 可用性检测（PATH）
pub fn git_available() -> bool {
    which::which("git").is_ok()
}

/// URL 策略：允许 https:// 与 file://（内网裸 git 服务 / 测试 fixture）；
/// ssh://、git@ 明确拒绝（公司强制 https）。
pub fn validate_repo_url(repo_url: &str) -> Result<(), String> {
    let url = repo_url.trim();
    if url.is_empty() {
        return Err("repo_url cannot be empty".to_string());
    }
    if url.starts_with("ssh://") || url.starts_with("git@") {
        return Err(
            "SSH protocol is not allowed (company policy: HTTPS only); convert to an https:// URL"
                .to_string(),
        );
    }
    if !(url.starts_with("https://") || url.starts_with("file://")) {
        return Err("repo_url must start with https:// or file://".to_string());
    }
    if url.chars().any(|c| c.is_whitespace() || c == ';') {
        return Err("repo_url contains unsupported characters (whitespace / semicolon)".to_string());
    }
    Ok(())
}

/// 本地路径 → file:// URL（Windows 反斜杠转正斜杠）
pub fn file_url(path: &Path) -> String {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
}

pub struct GitResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

fn base_command(args: &[&str], cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd
}

fn log_result(args: &[&str], res: &GitResult) {
    let cmd_line = args.join(" ");
    if !res.stderr.trim().is_empty() {
        // git 进度输出以 \r 刷新；规范化为 \n 后全量记录（不截断）
        tracing::debug!(cmd = %cmd_line, stderr = %res.stderr.replace('\r', "\n"), "git stderr");
    }
    tracing::info!(cmd = %cmd_line, success = res.success, "git finished");
}

/// 运行 git 命令，超时强杀
pub async fn run_git(args: &[&str], cwd: Option<&Path>, timeout: Duration) -> GitResult {
    let mut child = match base_command(args, cwd).spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "failed to spawn git");
            return GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("failed to spawn git: {e}"),
            };
        }
    };
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    // stdout/stderr 并发读，防单流管道缓冲写满死锁
    let out_task = tokio::spawn(async move {
        let mut s = String::new();
        stdout_pipe.read_to_string(&mut s).await.ok();
        s
    });
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        stderr_pipe.read_to_string(&mut s).await.ok();
        s
    });
    let result = tokio::time::timeout(timeout, child.wait()).await;
    let res = match result {
        Ok(Ok(status)) => GitResult {
            success: status.success(),
            stdout: out_task.await.unwrap_or_default(),
            stderr: err_task.await.unwrap_or_default(),
        },
        Ok(Err(e)) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "git wait failed");
            GitResult { success: false, stdout: String::new(), stderr: format!("git wait failed: {e}") }
        }
        Err(_) => {
            // kill_on_drop 兜底；显式 kill 加速退出
            let _ = child.start_kill();
            let _ = out_task.await;
            let _ = err_task.await;
            GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("git {} timed out after {}s", args.join(" "), timeout.as_secs()),
            }
        }
    };
    log_result(args, &res);
    res
}

/// 运行 git 命令并流式解析 stderr 进度（clone/fetch），进度经 channel 实时上报
pub async fn run_git_streaming(
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    progress_tx: UnboundedSender<String>,
) -> GitResult {
    let mut child = match base_command(args, cwd).spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "failed to spawn git");
            return GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("failed to spawn git: {e}"),
            };
        }
    };
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let out_task = tokio::spawn(async move {
        let mut s = String::new();
        stdout_pipe.read_to_string(&mut s).await.ok();
        s
    });
    let progress_tx = std::sync::Arc::new(progress_tx);
    let tx = progress_tx.clone();
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr_pipe.read_to_end(&mut buf).await.ok();
        // \r / \n 分隔的进度行流
        let text = String::from_utf8_lossy(&buf);
        for line in text.split(['\r', '\n']) {
            if let Some(p) = parse_progress(line) {
                let _ = tx.send(p);
            }
        }
        text.into_owned()
    });
    let result = tokio::time::timeout(timeout, child.wait()).await;
    let res = match result {
        Ok(Ok(status)) => GitResult {
            success: status.success(),
            stdout: out_task.await.unwrap_or_default(),
            stderr: err_task.await.unwrap_or_default(),
        },
        Ok(Err(e)) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "git wait failed");
            GitResult { success: false, stdout: String::new(), stderr: format!("git wait failed: {e}") }
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = out_task.await;
            let _ = err_task.await;
            GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("git {} timed out after {}s", args.join(" "), timeout.as_secs()),
            }
        }
    };
    log_result(args, &res);
    res
}

/// 解析 git 进度行，如 "Receiving objects:  45% (123/273), 1.2 MiB | 2.3 MiB/s"
pub fn parse_progress(line: &str) -> Option<String> {
    let trimmed = line.trim_start_matches("remote: ").trim();
    let (label, rest) = trimmed.split_once(':')?;
    if !matches!(
        label,
        "Enumerating objects"
            | "Counting objects"
            | "Compressing objects"
            | "Receiving objects"
            | "Resolving deltas"
            | "Updating files"
    ) {
        return None;
    }
    let rest = rest.trim();
    let pct: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '%').collect();
    if pct.ends_with('%') && !pct.starts_with('%') {
        Some(format!("{label}: {pct}"))
    } else {
        None
    }
}

/// ref → commit sha。分支名本地不存在时回退 origin/<ref>（clone 后非默认分支
/// 只有 remote-tracking 引用）。tag / commitid 直接命中第一轮。
pub async fn resolve_ref(clone_dir: &Path, git_ref: &str) -> Option<String> {
    for candidate in [git_ref.to_string(), format!("origin/{git_ref}")] {
        let peeled = format!("{candidate}^{{commit}}");
        let out = run_git(
            &["-C", &clone_dir.display().to_string(), "rev-parse", "--verify", &peeled],
            Some(clone_dir),
            Duration::from_secs(120),
        )
        .await;
        if out.success {
            return Some(out.stdout.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_hash_stable_16_hex() {
        let a = url_hash("https://git.example.com/bar.git");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, url_hash("https://git.example.com/bar.git"));
        assert_ne!(a, url_hash("https://git.example.com/baz.git"));
    }

    #[test]
    fn test_ref_hash_and_repo_id() {
        assert_eq!(ref_hash("main").len(), 16);
        let id1 = repo_id("https://a.git", "main");
        assert_eq!(id1, repo_id("https://a.git", "main"));
        assert_ne!(id1, repo_id("https://a.git", "v1.0"));
        assert_ne!(id1, repo_id("https://b.git", "main"));
    }

    #[test]
    fn test_validate_repo_url_rules() {
        assert!(validate_repo_url("https://gitlab.example.com/foo/bar.git").is_ok());
        assert!(validate_repo_url("file:///C:/some/repo").is_ok());
        assert!(validate_repo_url("ssh://git@example.com/foo.git").is_err());
        assert!(validate_repo_url("git@example.com:foo/bar.git").is_err());
        assert!(validate_repo_url("ftp://example.com/x").is_err());
        assert!(validate_repo_url("example.com/x").is_err());
        assert!(validate_repo_url("").is_err());
        assert!(validate_repo_url("https://x.com/a b").is_err());
    }

    #[test]
    fn test_parse_progress_variants() {
        assert_eq!(
            parse_progress("Receiving objects:  45% (123/273), 1.20 MiB | 2.30 MiB/s"),
            Some("Receiving objects: 45%".to_string())
        );
        assert_eq!(
            parse_progress("remote: Enumerating objects: 12, done."),
            None
        );
        assert_eq!(parse_progress("Resolving deltas: 100% (273/273)"), Some("Resolving deltas: 100%".to_string()));
        assert_eq!(parse_progress("Unpacking objects: 100%"), None); // 不在白名单
    }

    #[tokio::test]
    async fn test_file_url_clone_roundtrip() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        assert!(url.starts_with("file:///"));
        let dst = tempfile::tempdir().unwrap();
        let res = run_git(
            &["clone", &url, &dst.path().join("clone").display().to_string()],
            None,
            Duration::from_secs(120),
        )
        .await;
        assert!(res.success, "clone failed: {}", res.stderr);
        let sha = resolve_ref(&dst.path().join("clone"), "v1.0").await;
        assert!(sha.is_some());
        let feature = resolve_ref(&dst.path().join("clone"), "feature-x").await;
        assert!(feature.is_some(), "remote-tracking fallback for non-default branch");
    }
}
```

- [ ] **Step 2.4: lib.rs 加模块声明**

`src-tauri/src/lib.rs` 模块声明区 `mod arthas;` 后加 `mod code;`。

- [ ] **Step 2.5: 运行测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- code::git`
Expected: 全部 PASS（`test_file_url_clone_roundtrip` 需要本机 git，CI/开发机均有）

- [ ] **Step 2.6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/code/mod.rs src-tauri/src/code/git.rs src-tauri/src/lib.rs
git commit -m "feat: git subprocess wrapper with url policy and progress streaming"
```

---

### Task 3: `CodeRepoManager` — open 管线与状态注册表

**Files:**
- Create: `src-tauri/src/code/manager.rs`
- Modify: `src-tauri/src/code/mod.rs`（放开 `pub mod manager;` 与 `pub use`）

- [ ] **Step 3.1: 写 manager.rs**

```rust
//! CodeRepoManager：clone 缓存（完整克隆）+ 每 (repo, ref) 一个 worktree +
//! 异步 open 管线（同 jfr_record 模式：立即返回、后台任务、轮询状态）。
//!
//! 生命周期不做定时回收：clone/worktree 落盘持久、跨会话复用（重开秒级），
//! 磁盘增长出口在设置页缓存管理（delete_cache）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::git;

pub const CLONE_TIMEOUT_SECS: u64 = 1800;
pub const FETCH_TIMEOUT_SECS: u64 = 300;
pub const GIT_TIMEOUT_SECS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoPhase {
    Cloning,
    Fetching,
    Ready,
    Failed,
}

impl RepoPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoPhase::Cloning => "cloning",
            RepoPhase::Fetching => "fetching",
            RepoPhase::Ready => "ready",
            RepoPhase::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, RepoPhase::Ready | RepoPhase::Failed)
    }
}

#[derive(Clone, Debug)]
pub struct RepoState {
    pub repo_id: String,
    pub url_hash: String,
    pub repo_url: String,
    pub git_ref: String,
    pub phase: RepoPhase,
    /// git 进度（如 "Receiving objects: 45%"）
    pub progress: Option<String>,
    pub resolved_commit: Option<String>,
    /// fetch 失败但本地有该 ref：降级用本地缓存
    pub stale_warning: Option<String>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    /// Some = worktree 已检出（读工具可用的最低条件）
    pub worktree_path: Option<PathBuf>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RepoCacheEntry {
    pub url_hash: String,
    pub repo_url: String,
    pub disk_bytes: u64,
    pub worktrees: usize,
}

struct ManagerInner {
    repos_dir: PathBuf,
    states: Mutex<HashMap<String, RepoState>>,
}

#[derive(Clone)]
pub struct CodeRepoManager {
    inner: Arc<ManagerInner>,
}

impl CodeRepoManager {
    pub fn new(repos_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(ManagerInner { repos_dir, states: Mutex::new(HashMap::new()) }),
        }
    }

    fn repo_root(&self, url_hash: &str) -> PathBuf {
        self.inner.repos_dir.join(url_hash)
    }

    fn clone_dir(&self, url_hash: &str) -> PathBuf {
        self.repo_root(url_hash).join("clone")
    }

    fn worktree_dir(&self, url_hash: &str, git_ref: &str) -> PathBuf {
        self.repo_root(url_hash).join("wt").join(git::ref_hash(git_ref))
    }

    /// 打开仓（幂等）：进行中直接返回同 id；已就绪仍触发一次 fetch 刷新。
    /// 返回 repo_id，状态经 status() 轮询。
    pub async fn open(&self, repo_url: &str, git_ref: &str) -> String {
        let id = git::repo_id(repo_url, git_ref);
        let uh = git::url_hash(repo_url);
        {
            let states = self.inner.states.lock().await;
            if let Some(st) = states.get(&id) {
                if st.phase == RepoPhase::Cloning || st.phase == RepoPhase::Fetching {
                    return id; // in-flight 去重
                }
            }
        }
        self.inner.states.lock().await.insert(
            id.clone(),
            RepoState {
                repo_id: id.clone(),
                url_hash: uh.clone(),
                repo_url: repo_url.to_string(),
                git_ref: git_ref.to_string(),
                phase: RepoPhase::Cloning,
                progress: None,
                resolved_commit: None,
                stale_warning: None,
                error_code: None,
                error: None,
                worktree_path: Some(self.worktree_dir(&uh, git_ref)).filter(|p| p.exists()),
                created_at: chrono::Utc::now(),
            },
        );
        let mgr = self.clone();
        let rid = id.clone();
        tokio::spawn(async move {
            run_pipeline(mgr, rid).await;
        });
        id
    }

    pub async fn status(&self, repo_id: &str) -> Option<RepoState> {
        self.inner.states.lock().await.get(repo_id).cloned()
    }

    /// 读工具入口：worktree 已检出即可读（fetch 期间内容稳定，不阻塞并发读）
    pub async fn worktree(&self, repo_id: &str) -> Result<PathBuf, String> {
        let st = self.inner.states.lock().await.get(repo_id).cloned();
        match st {
            None => Err("repo_not_found".to_string()),
            Some(st) => match st.worktree_path {
                Some(p) if p.exists() => Ok(p),
                _ => Err(format!("repo_not_ready: {}", st.phase.as_str())),
            },
        }
    }

    /// 缓存仓库列表（设置页）：目录扫描 + url 标记 + 磁盘占用
    pub async fn list_cache(&self) -> Vec<RepoCacheEntry> {
        let mut entries = Vec::new();
        let Ok(dir) = std::fs::read_dir(&self.inner.repos_dir) else {
            return entries;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let url_hash = entry.file_name().to_string_lossy().to_string();
            let repo_url = std::fs::read_to_string(path.join("url.txt"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| format!("<unknown {}>", url_hash));
            let worktrees = std::fs::read_dir(path.join("wt"))
                .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0);
            entries.push(RepoCacheEntry {
                url_hash,
                repo_url,
                disk_bytes: dir_size(&path),
                worktrees,
            });
        }
        entries.sort_by(|a, b| b.disk_bytes.cmp(&a.disk_bytes));
        entries
    }

    /// 删除某仓全部缓存（主 clone + worktrees）。有进行中任务时拒绝。
    pub async fn delete_cache(&self, url_hash: &str) -> Result<(), String> {
        {
            let states = self.inner.states.lock().await;
            let busy: Vec<String> = states
                .values()
                .filter(|st| st.url_hash == url_hash && !st.phase.is_terminal())
                .map(|st| st.repo_id.clone())
                .collect();
            if !busy.is_empty() {
                return Err("repo busy (cloning/fetching in progress); retry later".to_string());
            }
            states.retain(|_, st| st.url_hash != url_hash);
        }
        let root = self.repo_root(url_hash);
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|e| format!("failed to delete cache: {e}"))?;
            tracing::info!(url_hash, ?root, "repo cache deleted");
        }
        Ok(())
    }

    async fn set_phase(&self, repo_id: &str, phase: RepoPhase) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = phase;
            st.progress = None;
        }
    }

    async fn set_progress(&self, repo_id: &str, progress: String) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.progress = Some(progress);
        }
    }

    async fn fail(&self, repo_id: &str, error_code: &str, error: String) {
        tracing::warn!(repo_id, error_code, error = %error, "code repo open failed");
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = RepoPhase::Failed;
            st.error_code = Some(error_code.to_string());
            st.error = Some(error);
            st.progress = None;
        }
    }

    async fn set_ready(
        &self,
        repo_id: &str,
        resolved_commit: String,
        worktree: PathBuf,
        stale_warning: Option<String>,
    ) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = RepoPhase::Ready;
            st.resolved_commit = Some(resolved_commit);
            st.worktree_path = Some(worktree);
            st.stale_warning = stale_warning;
            st.progress = None;
            st.error_code = None;
            st.error = None;
        }
    }
}

/// open 三分支管线：①无主 clone → 后台 clone；②有 clone → fetch 刷新（best
/// effort，失败降级本地缓存）；③ref 解析（分支/tag/commitid）→ worktree 建/检出。
async fn run_pipeline(mgr: CodeRepoManager, repo_id: String) {
    let st = mgr.inner.states.lock().await.get(&repo_id).cloned();
    let Some(st) = st else { return };
    let url = st.repo_url;
    let git_ref = st.git_ref;
    let uh = st.url_hash;
    let root = mgr.repo_root(&uh);
    let clone_dir = mgr.clone_dir(&uh);
    let wt_dir = mgr.worktree_dir(&uh, &git_ref);
    let _ = std::fs::create_dir_all(root.join("wt"));
    // url 标记（缓存列表反查仓地址；clone 前写，半截 clone 也能识别）
    let _ = std::fs::write(root.join("url.txt"), &url);
    let clone_str = clone_dir.display().to_string();
    let wt_str = wt_dir.display().to_string();

    // ① 主 clone 不存在 → 完整克隆
    if !clone_dir.join(".git").exists() {
        mgr.set_phase(&repo_id, RepoPhase::Cloning).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let fwd = {
            let mgr = mgr.clone();
            let rid = repo_id.clone();
            tokio::spawn(async move {
                while let Some(p) = rx.recv().await {
                    mgr.set_progress(&rid, p).await;
                }
            })
        };
        let res = git::run_git_streaming(
            &["clone", &url, &clone_str],
            None,
            Duration::from_secs(CLONE_TIMEOUT_SECS),
            tx,
        )
        .await;
        drop(fwd);
        if !res.success {
            mgr.fail(&repo_id, "clone_failed", res.stderr.trim().to_string()).await;
            return;
        }
    }

    // ② fetch 刷新（分支经常变；best effort）
    mgr.set_phase(&repo_id, RepoPhase::Fetching).await;
    let fetch = git::run_git(
        &["-C", &clone_str, "fetch", "--force", "origin", &git_ref],
        Some(&clone_dir),
        Duration::from_secs(FETCH_TIMEOUT_SECS),
    )
    .await;
    let stale_warning = if fetch.success {
        None
    } else {
        Some(format!(
            "fetch failed ({}); using locally cached code which may be stale",
            fetch.stderr.trim().lines().last().unwrap_or("network error")
        ))
    };

    // ③ ref 解析：分支（本地或 origin/）→ tag → commitid
    let Some(sha) = git::resolve_ref(&clone_dir, &git_ref).await else {
        // fetch 对「远端没有该 ref」也会失败（couldn't find remote ref），需按
        // stderr 语义区分：ref 缺失 → invalid_ref；网络/认证 → fetch_failed
        let ref_missing = fetch.stderr.to_lowercase().contains("couldn't find remote ref");
        let code = if ref_missing { "invalid_ref" } else { "fetch_failed" };
        mgr.fail(
            &repo_id,
            code,
            format!(
                "ref '{git_ref}' not found ({}); \
                 confirm branch/tag/commitid with the user, or check network/credentials",
                if ref_missing { "absent on remote".to_string() } else { format!("fetch error: {}", fetch.stderr.trim()) }
            ),
        )
        .await;
        return;
    };

    // ④ worktree：残损缓存自动重建
    let ok = if wt_dir.exists() {
        let res = git::run_git(
            &["-C", &wt_str, "checkout", "--force", &sha],
            Some(&wt_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await;
        if res.success {
            true
        } else {
            tracing::warn!(repo_id, ?wt_dir, "worktree checkout failed, rebuilding");
            let _ = std::fs::remove_dir_all(&wt_dir);
            let _ = git::run_git(
                &["-C", &clone_str, "worktree", "prune"],
                Some(&clone_dir),
                Duration::from_secs(GIT_TIMEOUT_SECS),
            )
            .await;
            git::run_git(
                &["-C", &clone_str, "worktree", "add", "--detach", &wt_str, &sha],
                Some(&clone_dir),
                Duration::from_secs(GIT_TIMEOUT_SECS),
            )
            .await
            .success
        }
    } else {
        let _ = git::run_git(
            &["-C", &clone_str, "worktree", "prune"],
            Some(&clone_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await;
        git::run_git(
            &["-C", &clone_str, "worktree", "add", "--detach", &wt_str, &sha],
            Some(&clone_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await
        .success
    };
    if !ok {
        mgr.fail(&repo_id, "worktree_failed", format!("failed to prepare worktree at {wt_str}")).await;
        return;
    }

    mgr.set_ready(&repo_id, sha, wt_dir, stale_warning).await;
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(dir) = std::fs::read_dir(path) {
        for entry in dir.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_terminal(mgr: &CodeRepoManager, repo_id: &str) -> RepoState {
        for _ in 0..300 {
            if let Some(st) = mgr.status(repo_id).await {
                if st.phase.is_terminal() {
                    return st;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("pipeline did not reach terminal state in 15s");
    }

    #[tokio::test]
    async fn test_open_main_ready_and_worktree_content() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_none());
        assert!(st.resolved_commit.as_deref().unwrap().len() >= 7);
        let wt = mgr.worktree(&rid).await.unwrap();
        assert!(wt.join("src").join("Bar.java").exists());
        assert!(wt.join("src").join("Baz.java").exists(), "main HEAD has Baz");
    }

    #[tokio::test]
    async fn test_open_tag_and_branch_isolated_worktrees() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid_main = mgr.open(&url, "main").await;
        let rid_tag = mgr.open(&url, "v1.0").await;
        let rid_feat = mgr.open(&url, "feature-x").await;
        assert_ne!(rid_main, rid_tag);
        assert_ne!(rid_main, rid_feat);

        let st = wait_terminal(&mgr, &rid_tag).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        let wt_tag = mgr.worktree(&rid_tag).await.unwrap();
        assert!(!wt_tag.join("src").join("Baz.java").exists(), "v1.0 predates Baz");

        let st = wait_terminal(&mgr, &rid_feat).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        let wt_feat = mgr.worktree(&rid_feat).await.unwrap();
        assert!(wt_feat.join("src").join("Feature.java").exists());

        wait_terminal(&mgr, &rid_main).await;
    }

    #[tokio::test]
    async fn test_open_commitid() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let sha = crate::code::testutil::head_sha(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, &sha).await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert_eq!(st.resolved_commit.as_deref(), Some(sha.as_str()));
    }

    #[tokio::test]
    async fn test_invalid_ref_fails() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "no-such-branch").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Failed);
        assert_eq!(st.error_code.as_deref(), Some("invalid_ref"));
        assert!(mgr.worktree(&rid).await.is_err());
    }

    #[tokio::test]
    async fn test_fetch_failure_degrades_to_stale_ready() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_none());

        // 源仓消失 → 重开（Ready 触发 fetch 刷新）→ fetch 失败但本地有 ref
        std::fs::remove_dir_all(src.path()).unwrap();
        let rid2 = mgr.open(&url, "main").await;
        assert_eq!(rid, rid2, "same (url, ref) is idempotent");
        let st = wait_terminal(&mgr, &rid2).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_some(), "degraded to local cache with warning");
    }

    #[tokio::test]
    async fn test_list_cache_and_delete() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        wait_terminal(&mgr, &rid).await;

        let entries = mgr.list_cache().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].repo_url, url);
        assert!(entries[0].disk_bytes > 0);
        assert!(entries[0].worktrees >= 1);

        mgr.delete_cache(&entries[0].url_hash).await.unwrap();
        assert!(mgr.list_cache().await.is_empty());
        // 重开后可再次 clone
        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
    }

    #[tokio::test]
    async fn test_delete_cache_rejects_inflight() {
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());
        // 直接注入一个进行中状态（不真跑 clone）
        mgr.inner.states.lock().await.insert(
            "fake-rid".to_string(),
            RepoState {
                repo_id: "fake-rid".to_string(),
                url_hash: "fakehash00000000".to_string(),
                repo_url: "https://example.com/x.git".to_string(),
                git_ref: "main".to_string(),
                phase: RepoPhase::Cloning,
                progress: None,
                resolved_commit: None,
                stale_warning: None,
                error_code: None,
                error: None,
                worktree_path: None,
                created_at: chrono::Utc::now(),
            },
        );
        let err = mgr.delete_cache("fakehash00000000").await.unwrap_err();
        assert!(err.contains("busy"));
    }
}
```

- [ ] **Step 3.2: mod.rs 放开 manager**

`src-tauri/src/code/mod.rs` 中 `// pub mod manager;` 改为 `pub mod manager;`，放开 `pub use manager::{CodeRepoManager, RepoCacheEntry, RepoPhase, RepoState};`（`search` 仍注释）。

- [ ] **Step 3.3: 运行测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- code::manager`
Expected: 全部 PASS（真实 git 子进程 + file:// fixture）

- [ ] **Step 3.4: Commit**

```bash
git add src-tauri/src/code/manager.rs src-tauri/src/code/mod.rs
git commit -m "feat: CodeRepoManager with clone cache, worktree per ref and async open pipeline"
```

---

### Task 4: `code::search` — 读/搜/列三件套

**Files:**
- Create: `src-tauri/src/code/search.rs`
- Modify: `src-tauri/src/code/mod.rs`（放开 `pub mod search;`）

- [ ] **Step 4.1: 写 search.rs（含测试）**

```rust
//! 仓内只读三件套：read_file（行号分页）/ search（正则 + .gitignore）/
//! list_files（glob / 子目录）。路径安全：canonicalize + worktree 前缀校验。

use std::path::{Path, PathBuf};

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use regex::Regex;

pub const DEFAULT_READ_LINES: usize = 2000;
pub const MAX_READ_LINES: usize = 5000;
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_MAX_RESULTS: usize = 100;
pub const MAX_RESULTS_CAP: usize = 500;
pub const DEFAULT_CONTEXT_LINES: usize = 2;
pub const MAX_CONTEXT_LINES: usize = 10;
pub const LIST_MAX: usize = 500;
const BINARY_SNIFF_BYTES: usize = 8192;

/// worktree 内路径安全解析：canonicalize + 前缀校验，拒绝 `..` 逃逸与绝对路径替换
pub fn resolve_in_worktree(worktree: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.contains('\0') {
        return Err("path contains NUL".to_string());
    }
    let wt_canon = worktree
        .canonicalize()
        .map_err(|e| format!("worktree not accessible: {e}"))?;
    let joined = wt_canon.join(rel);
    let canon = joined
        .canonicalize()
        .map_err(|_| format!("path not found in repo: {rel}"))?;
    if canon.starts_with(&wt_canon) {
        Ok(canon)
    } else {
        Err(format!("path escapes repo worktree: {rel}"))
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

fn rel_display(path: &Path, worktree: &Path) -> String {
    path.strip_prefix(worktree)
        .map(|p| p.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

pub struct ReadFileOutput {
    pub total_lines: usize,
    pub offset: usize,
    pub limit: usize,
    pub lines: Vec<(usize, String)>,
    pub truncated: bool,
}

/// 读文件（1-based 行号）；offset 默认 1、limit 默认 2000 上限 5000（调用方 clamp）
pub fn read_file(worktree: &Path, rel: &str, offset: usize, limit: usize) -> Result<ReadFileOutput, String> {
    let path = resolve_in_worktree(worktree, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot stat {rel}: {e}"))?;
    if meta.is_dir() {
        return Err(format!("{rel} is a directory; use code_list_files"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file too large ({} bytes > {}); use code_search with a pattern to locate content",
            meta.len(),
            MAX_FILE_BYTES
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    if is_binary(&bytes) {
        return Err(format!("{rel} is a binary file"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let offset = offset.max(1);
    let lines: Vec<(usize, String)> = all
        .iter()
        .enumerate()
        .skip(offset - 1)
        .take(limit)
        .map(|(i, l)| (i + 1, (*l).to_string()))
        .collect();
    let truncated = offset - 1 + lines.len() < total;
    Ok(ReadFileOutput { total_lines: total, offset, limit: lines.len(), lines, truncated })
}

pub struct SearchMatch {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub context_before: Vec<(usize, String)>,
    pub context_after: Vec<(usize, String)>,
}

pub struct SearchOutput {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

/// 正则搜索（尊重 .gitignore；跳过二进制与 >5MB 文件）
pub fn search(
    worktree: &Path,
    pattern: &str,
    glob: Option<&str>,
    context: usize,
    max_results: usize,
) -> Result<SearchOutput, String> {
    let re = Regex::new(pattern).map_err(|e| format!("invalid regex pattern: {e}"))?;
    let mut builder = WalkBuilder::new(worktree);
    if let Some(g) = glob {
        let ob = OverrideBuilder::new(worktree);
        let ob = ob.add(g).map_err(|e| format!("invalid glob '{g}': {e}"))?;
        builder.overrides(ob.build().map_err(|e| format!("invalid glob '{g}': {e}"))?);
    }
    let mut matches = Vec::new();
    let mut truncated = false;
    'outer: for entry in builder.build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                if matches.len() >= max_results {
                    truncated = true;
                    break 'outer;
                }
                let before = lines[i.saturating_sub(context)..i]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| (i - context + j + 1, (*l).to_string()))
                    .collect();
                let after_end = (i + 1 + context).min(lines.len());
                let after = lines[i + 1..after_end]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| (i + 2 + j, (*l).to_string()))
                    .collect();
                matches.push(SearchMatch {
                    file: rel_display(path, worktree),
                    line: i + 1,
                    text: (*line).to_string(),
                    context_before: before,
                    context_after: after,
                });
            }
        }
    }
    Ok(SearchOutput { matches, truncated })
}

/// 列文件（尊重 .gitignore；glob 白名单或 path 子目录二选一或全仓）
pub fn list_files(
    worktree: &Path,
    subpath: Option<&str>,
    glob: Option<&str>,
) -> Result<(Vec<String>, bool), String> {
    let root = match subpath {
        Some(p) => resolve_in_worktree(worktree, p)?,
        None => worktree.canonicalize().map_err(|e| format!("worktree not accessible: {e}"))?,
    };
    if root.is_file() {
        return Ok((vec![rel_display(&root, worktree)], false));
    }
    let mut builder = WalkBuilder::new(&root);
    if let Some(g) = glob {
        let ob = OverrideBuilder::new(&root);
        let ob = ob.add(g).map_err(|e| format!("invalid glob '{g}': {e}"))?;
        builder.overrides(ob.build().map_err(|e| format!("invalid glob '{g}': {e}"))?);
    }
    let mut files = Vec::new();
    let mut truncated = false;
    for entry in builder.build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if files.len() >= LIST_MAX {
            truncated = true;
            break;
        }
        files.push(rel_display(entry.path(), worktree));
    }
    files.sort();
    Ok((files, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_worktree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        std::fs::write(wt.join("README.md"), "line1\nline2 target\nline3\n").unwrap();
        std::fs::create_dir_all(wt.join("src").join("main")).unwrap();
        std::fs::write(
            wt.join("src").join("main").join("Bar.java"),
            "package com.foo;\npublic class Bar {\n    void baz() {}\n}\n",
        )
        .unwrap();
        std::fs::write(wt.join("src").join("bin.dat"), "bin\0ary\n").unwrap();
        std::fs::create_dir_all(wt.join("target").join("classes")).unwrap();
        std::fs::write(wt.join("target").join("classes").join("Gen.java"), "generated target line\n").unwrap();
        std::fs::write(wt.join(".gitignore"), "target/\n").unwrap();
        tmp
    }

    #[test]
    fn test_read_file_line_numbers_and_pagination() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let out = read_file(wt, "src/main/Bar.java", 1, 5000).unwrap();
        assert_eq!(out.total_lines, 4);
        assert_eq!(out.lines[0], (1, "package com.foo;".to_string()));
        assert_eq!(out.lines[1].0, 2);
        assert!(!out.truncated);

        let out = read_file(wt, "src/main/Bar.java", 2, 1).unwrap();
        assert_eq!(out.lines, vec![(2, "public class Bar {".to_string())]);
        assert!(out.truncated, "more lines after the window");
    }

    #[test]
    fn test_read_file_rejects_escape_and_missing() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        assert!(read_file(wt, "../outside.txt", 1, 10).is_err());
        assert!(read_file(wt, "no-such.txt", 1, 10).is_err());
        assert!(read_file(wt, "src/bin.dat", 1, 10).is_err(), "binary rejected");
    }

    #[test]
    fn test_search_respects_gitignore_and_context() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let out = search(wt, "target", None, 1, 100).unwrap();
        // .gitignore 的 target/ 不搜；README 里的 target 命中
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].file, "README.md");
        assert_eq!(out.matches[0].line, 2);
        assert_eq!(out.matches[0].context_before.len(), 1);
        assert!(!out.truncated);

        let out = search(wt, "class", Some("*.java"), 0, 100).unwrap();
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].file, "src/main/Bar.java");

        assert!(search(wt, "class(", None, 1, 10).is_err(), "invalid regex rejected");
    }

    #[test]
    fn test_search_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        for i in 0..10 {
            std::fs::write(tmp.path().join("src").join(format!("F{i}.txt")), "hit\n").unwrap();
        }
        let out = search(tmp.path(), "hit", None, 0, 3).unwrap();
        assert_eq!(out.matches.len(), 3);
        assert!(out.truncated);
    }

    #[test]
    fn test_list_files_glob_and_path() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let (files, truncated) = list_files(wt, None, Some("**/*.java")).unwrap();
        assert!(!truncated);
        assert_eq!(files, vec!["src/main/Bar.java".to_string()], "glob filter + gitignore");

        let (files, _) = list_files(wt, Some("src/main"), None).unwrap();
        assert_eq!(files, vec!["src/main/Bar.java".to_string()]);

        let (_, truncated) = list_files(wt, None, None).unwrap();
        assert!(!truncated);
        assert!(list_files(wt, Some("../"), None).is_err(), "escape rejected");
    }
}
```

- [ ] **Step 4.2: mod.rs 放开 search**

`// pub mod search;` 改为 `pub mod search;`。

- [ ] **Step 4.3: 运行测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- code::search`
Expected: 全部 PASS

- [ ] **Step 4.4: Commit**

```bash
git add src-tauri/src/code/search.rs src-tauri/src/code/mod.rs
git commit -m "feat: code search/read/list with gitignore respect and path safety"
```

---

### Task 5: `ToolCategory::Code` + 6 个 code_* MCP 工具 + lib.rs 装配

**Files:**
- Modify: `src-tauri/src/tools/category.rs`
- Create: `src-tauri/src/tools/builtin/code/mod.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`
- Modify: `src-tauri/src/lib.rs`（manager 构造 + register_all + AppState 字段）

- [ ] **Step 5.1: category.rs 加 Code 变体（声明序 = Arthas 后、FileTransfer 前）**

```rust
pub enum ToolCategory {
    Environment,
    K8s,
    Jvm,
    Heap,
    Jfr,
    Arthas,
    Code,
    FileTransfer,
    Builtin,
}
```

测试 `test_serde_names_match_frontend_union` 的 `names` 数组在 `(ToolCategory::Arthas, "arthas")` 后插入 `(ToolCategory::Code, "code"),`；`test_declaration_order_k8s_after_environment` 追加：

```rust
        assert!(ToolCategory::Arthas < ToolCategory::Code);
        assert!(ToolCategory::Code < ToolCategory::FileTransfer);
```

- [ ] **Step 5.2: 运行 category 测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- tools::category`
Expected: PASS

- [ ] **Step 5.3: 写 tools/builtin/code/mod.rs（工具定义 + handler + 测试）**

```rust
//! code_* 工具：全部只读自主、needs_channel = false（纯本地，无环境也可用）。

use crate::app::service_repos;
use crate::code::{self, CodeRepoManager, RepoState};
use crate::tools::builtin::jvm::core::error_output;
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;

pub struct CodeToolDeps {
    pub manager: Arc<CodeRepoManager>,
    pub db: SqlitePool,
}

#[derive(Clone, Copy, Debug)]
enum CodeToolKind {
    GetRepo,
    OpenRepo,
    RepoStatus,
    ReadFile,
    Search,
    ListFiles,
}

struct CodeToolHandler {
    deps: Arc<CodeToolDeps>,
    kind: CodeToolKind,
}

fn state_json(st: &RepoState) -> serde_json::Value {
    serde_json::json!({
        "repo_id": st.repo_id,
        "repo_url": st.repo_url,
        "ref": st.git_ref,
        "status": st.phase.as_str(),
        "progress": st.progress,
        "resolved_commit": st.resolved_commit,
        "stale_warning": st.stale_warning,
        "error_code": st.error_code,
        "error": st.error,
    })
}

fn require_str(args: &serde_json::Value, key: &str) -> Result<String, ToolOutput> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error_output("invalid_params", &format!("missing required parameter: {key}")))
}

fn clamp_usize(v: Option<i64>, default: usize, max: usize) -> usize {
    match v {
        Some(n) if n >= 1 => (n as usize).min(max),
        _ => default,
    }
}

#[async_trait]
impl ToolHandler for CodeToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, "code tool executing");
        match self.kind {
            CodeToolKind::GetRepo => self.get_repo(&args).await,
            CodeToolKind::OpenRepo => self.open_repo(&args).await,
            CodeToolKind::RepoStatus => self.repo_status(&args).await,
            CodeToolKind::ReadFile => self.read_file(&args).await,
            CodeToolKind::Search => self.search(&args).await,
            CodeToolKind::ListFiles => self.list_files(&args).await,
        }
    }
}

impl CodeToolHandler {
    /// search.rs 的路径错误 → 结构化错误码（逃逸/未找到区分）
    fn path_error(e: String) -> ToolOutput {
        if e.contains("escapes") {
            error_output("path_outside_repo", &e)
        } else {
            error_output("path_not_found", &e)
        }
    }

    async fn get_repo(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(service) = require_str(args, "service") else {
            return error_output("invalid_params", "missing required parameter: service");
        };
        match service_repos::get_service_repo(&self.deps.db, &service).await {
            Ok(Some(row)) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "service": row.service,
                    "found": true,
                    "repo_url": row.repo_url,
                    "last_ref": row.last_ref,
                    "hint": "repo_url is the remembered address; always confirm the ref (branch changes often, last_ref is only a hint)",
                }),
                raw_stdout: None,
            },
            Ok(None) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "service": service,
                    "found": false,
                    "hint": "ask the user for the repo URL and the ref (branch/tag/commit id) deployed on the target",
                }),
                raw_stdout: None,
            },
            Err(e) => error_output("db_error", &e.to_string()),
        }
    }

    async fn open_repo(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_url) = require_str(args, "repo_url") else {
            return error_output("invalid_params", "missing required parameter: repo_url");
        };
        let Ok(git_ref) = require_str(args, "ref") else {
            return error_output("invalid_params", "missing required parameter: ref (branch/tag/commit id)");
        };
        if let Err(e) = code::git::validate_repo_url(&repo_url) {
            return error_output("invalid_repo_url", &e);
        }
        if !code::git::git_available() {
            return error_output(
                "git_not_found",
                "git binary not found on PATH; install git (https://git-scm.com) and retry",
            );
        }
        let service = args.get("service").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let repo_id = self.deps.manager.open(&repo_url, &git_ref).await;
        if let Some(svc) = &service {
            if let Err(e) = service_repos::upsert_service_repo(&self.deps.db, svc, &repo_url, Some(&git_ref)).await {
                tracing::warn!(service = %svc, error = ?e, "failed to persist service repo mapping");
            }
        }
        let st = self.deps.manager.status(&repo_id).await;
        let mut data = st.as_ref().map(state_json).unwrap_or_else(|| serde_json::json!({"repo_id": repo_id, "status": "cloning"}));
        if service.is_some() {
            data["service"] = serde_json::json!(service);
        }
        ToolOutput { success: true, data, raw_stdout: None }
    }

    async fn repo_status(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_id) = require_str(args, "repo_id") else {
            return error_output("invalid_params", "missing required parameter: repo_id");
        };
        match self.deps.manager.status(&repo_id).await {
            Some(st) => ToolOutput { success: true, data: state_json(&st), raw_stdout: None },
            None => error_output("repo_not_found", "unknown repo_id; call code_open_repo first"),
        }
    }

    async fn resolve_worktree(&self, repo_id: &str) -> Result<std::path::PathBuf, ToolOutput> {
        match self.deps.manager.worktree(repo_id).await {
            Ok(p) => Ok(p),
            Err(code) if code == "repo_not_found" => {
                Err(error_output("repo_not_found", "unknown repo_id; call code_open_repo first"))
            }
            Err(not_ready) => Err(error_output(
                "repo_not_ready",
                &format!("repo is {not_ready}; poll code_repo_status until ready, do NOT re-open"),
            )),
        }
    }

    async fn read_file(&self, args: &serde_json::Value) -> ToolOutput {
        let (Ok(repo_id), Ok(path)) = (require_str(args, "repo_id"), require_str(args, "path")) else {
            return error_output("invalid_params", "missing required parameter: repo_id / path");
        };
        let offset = clamp_usize(args.get("offset").and_then(|v| v.as_i64()), 1, usize::MAX);
        let limit = clamp_usize(args.get("limit").and_then(|v| v.as_i64()), code::search::DEFAULT_READ_LINES, code::search::MAX_READ_LINES);
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::read_file(&wt, &path, offset, limit) {
            Ok(out) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "path": path,
                    "total_lines": out.total_lines,
                    "offset": out.offset,
                    "limit": out.limit,
                    "truncated": out.truncated,
                    "lines": out.lines.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                }),
                raw_stdout: None,
            },
            Err(e) => Self::path_error(e),
        }
    }

    async fn search(&self, args: &serde_json::Value) -> ToolOutput {
        let (Ok(repo_id), Ok(pattern)) = (require_str(args, "repo_id"), require_str(args, "pattern")) else {
            return error_output("invalid_params", "missing required parameter: repo_id / pattern");
        };
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let context = clamp_usize(args.get("context").and_then(|v| v.as_i64()), code::search::DEFAULT_CONTEXT_LINES, code::search::MAX_CONTEXT_LINES);
        let max_results = clamp_usize(args.get("max_results").and_then(|v| v.as_i64()), code::search::DEFAULT_MAX_RESULTS, code::search::MAX_RESULTS_CAP);
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::search(&wt, &pattern, glob.as_deref(), context, max_results) {
            Ok(out) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "pattern": pattern,
                    "truncated": out.truncated,
                    "matches": out.matches.iter().map(|m| serde_json::json!({
                        "file": m.file,
                        "line": m.line,
                        "text": m.text,
                        "context_before": m.context_before.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                        "context_after": m.context_after.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                }),
                raw_stdout: None,
            },
            Err(e) => error_output("pattern_invalid", &e),
        }
    }

    async fn list_files(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_id) = require_str(args, "repo_id") else {
            return error_output("invalid_params", "missing required parameter: repo_id");
        };
        let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::list_files(&wt, path.as_deref(), glob.as_deref()) {
            Ok((files, truncated)) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "files": files,
                    "truncated": truncated,
                }),
                raw_stdout: None,
            },
            Err(e) => Self::path_error(e),
        }
    }
}

fn code_tool_def(name: &str, description: &str, schema: serde_json::Value, kind: CodeToolKind, deps: &Arc<CodeToolDeps>) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Code,
        needs_channel: false,
        handler: Arc::new(CodeToolHandler { deps: deps.clone(), kind }),
    }
}

pub fn register_all(registry: &mut crate::tools::registry::ToolRegistry, deps: Arc<CodeToolDeps>) {
    registry.register(code_tool_def(
        "code_get_repo",
        "查询某服务记住的代码仓地址与上次使用的 ref（分支/tag/commitid）。需要读源码时先调本工具：命中后仍须向用户确认仓地址仍有效、并确认本次部署的 ref（分支经常变，last_ref 仅作提示）；未命中则直接问用户仓地址和 ref。",
        serde_json::json!({
            "type": "object",
            "properties": { "service": { "type": "string", "description": "服务名（与用户输入/进程关键词一致）" } },
            "required": ["service"]
        }),
        CodeToolKind::GetRepo,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_open_repo",
        "打开代码仓用于读取（幂等）：后台完整 clone / fetch + 检出到指定 ref（分支/tag/commitid 均可），立即返回 repo_id 与状态。大仓首次 clone 需数分钟，轮询 code_repo_status 直到 ready；cloning/fetching 期间勿重复调用（同 url+ref 幂等返回同 id）。带 service 参数会记住 服务→仓 映射。认证依赖本机 git 配置（credential manager / .netrc / URL 内嵌 token），公司仓走 https。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_url": { "type": "string", "description": "git 仓地址（https:// 或 file://，公司强制 https）" },
                "ref": { "type": "string", "description": "分支名 / tag 名 / commitid" },
                "service": { "type": "string", "description": "服务名（可选，提供则记住 服务→仓 映射，下次免问仓地址）" }
            },
            "required": ["repo_url", "ref"]
        }),
        CodeToolKind::OpenRepo,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_repo_status",
        "轮询 code_open_repo 进度：status = cloning / fetching / ready / failed。progress 为 git 进度百分比；ready 的 resolved_commit 为检出 commit，stale_warning 非空表示 fetch 失败降级用本地缓存（要告知用户代码可能非最新）。failed 看 error_code：clone_failed / fetch_failed / invalid_ref / git_not_found。",
        serde_json::json!({
            "type": "object",
            "properties": { "repo_id": { "type": "string", "description": "code_open_repo 返回的 repo_id" } },
            "required": ["repo_id"]
        }),
        CodeToolKind::RepoStatus,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_read_file",
        "读仓内源码文件，输出 1-based 行号。线程栈/堆栈帧行号可直接对上。大文件分页：offset 起始行（默认 1）、limit 行数（默认 2000 上限 5000）。定位类文件先用 code_list_files(glob) 或 code_search。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "path": { "type": "string", "description": "仓内相对路径（如 src/main/java/com/foo/Bar.java）" },
                "offset": { "type": "number", "description": "起始行（1-based，默认 1）" },
                "limit": { "type": "number", "description": "读取行数（默认 2000，上限 5000）" }
            },
            "required": ["repo_id", "path"]
        }),
        CodeToolKind::ReadFile,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_search",
        "仓内正则搜索（尊重 .gitignore，跳过二进制/超大文件），返回 文件+行号+匹配行+上下文。定位类定义用 pattern=\"class Foo\"；glob 过滤文件类型如 \"*.java\"。context 上下文行数默认 2，max_results 默认 100。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "pattern": { "type": "string", "description": "正则表达式" },
                "glob": { "type": "string", "description": "文件白名单 glob（如 *.java、**/Bar.java）" },
                "context": { "type": "number", "description": "上下文行数（默认 2，上限 10）" },
                "max_results": { "type": "number", "description": "匹配上限（默认 100，上限 500）" }
            },
            "required": ["repo_id", "pattern"]
        }),
        CodeToolKind::Search,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_list_files",
        "列仓内文件（尊重 .gitignore）：glob 匹配（如 **/Bar.java，线程栈类名定位）或 path 列子目录。上限 500 条，超出置 truncated。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "path": { "type": "string", "description": "子目录前缀（可选）" },
                "glob": { "type": "string", "description": "文件 glob 白名单（可选）" }
            },
            "required": ["repo_id"]
        }),
        CodeToolKind::ListFiles,
        &deps,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolHandler;

    async fn test_deps() -> (Arc<CodeToolDeps>, tempfile::TempDir, tempfile::TempDir, String) {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        (deps, src, repos, url)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "s-test".to_string(), channel: None }
    }

    async fn open_until_ready(deps: &CodeToolDeps, url: &str, service: Option<&str>) -> String {
        let mut args = serde_json::json!({"repo_url": url, "ref": "main"});
        if let Some(s) = service {
            args["service"] = serde_json::json!(s);
        }
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }.execute(args, &ctx()).await;
        assert!(out.success, "open failed: {:?}", out.data);
        let repo_id = out.data["repo_id"].as_str().unwrap().to_string();
        for _ in 0..300 {
            let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::RepoStatus }
                .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
                .await;
            if out.data["status"] == "ready" {
                return repo_id;
            }
            if out.data["status"] == "failed" {
                panic!("open failed: {:?}", out.data);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("not ready in 15s");
    }

    #[tokio::test]
    async fn test_open_get_read_search_list_roundtrip() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, Some("BarService")).await;

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "src/Bar.java"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["total_lines"], 4);
        assert_eq!(out.data["lines"][1]["line"], 2);
        assert_eq!(out.data["lines"][1]["text"], "public class Bar {");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id, "pattern": "class Bar", "glob": "*.java"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["matches"][0]["file"], "src/Bar.java");
        assert_eq!(out.data["matches"][0]["line"], 2);

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ListFiles }
            .execute(serde_json::json!({"repo_id": repo_id, "glob": "**/B*.java"}), &ctx())
            .await;
        assert!(out.success);
        let files: Vec<&str> = out.data["files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(files.contains(&"src/Bar.java"));

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::GetRepo }
            .execute(serde_json::json!({"service": "BarService"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["found"], true);
        assert_eq!(out.data["repo_url"], url);
        assert_eq!(out.data["last_ref"], "main");
    }

    #[tokio::test]
    async fn test_read_unknown_repo_errors() {
        let (deps, _src, _repos, _url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": "nope", "path": "x"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "repo_not_found");
    }

    #[tokio::test]
    async fn test_open_rejects_ssh_url_and_missing_params() {
        let (deps, _src, _repos, _url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": "git@example.com:a/b.git", "ref": "main"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_repo_url");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": "https://x.git"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
    }

    #[tokio::test]
    async fn test_register_all_six_tools() {
        let mut registry = crate::tools::registry::ToolRegistry::new();
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        register_all(&mut registry, deps);
        let names: Vec<&str> = registry.list().iter().map(|d| d.name.as_str()).collect();
        for expected in ["code_get_repo", "code_open_repo", "code_repo_status", "code_read_file", "code_search", "code_list_files"] {
            assert!(names.contains(&expected), "missing {expected}");
            let def = registry.get(expected).unwrap();
            assert_eq!(def.category, ToolCategory::Code);
            assert_eq!(def.risk_level, RiskLevel::ReadOnly);
            assert!(!def.needs_channel);
        }
    }
}
```

- [ ] **Step 5.4: builtin/mod.rs 加模块**

`src-tauri/src/tools/builtin/mod.rs` 声明列表 `pub mod arthas;` 后加 `pub mod code;`。

- [ ] **Step 5.5: lib.rs 装配**

`src-tauri/src/lib.rs`：

1. `pub struct AppState` 字段 `pub arthas: Arc<crate::arthas::manager::ArthasManager>,` 后加：

```rust
    pub code_repos: Arc<crate::code::CodeRepoManager>,
```

2. setup 中 `let arthas_manager = ...` 块后（`let mut tool_registry` 前）加：

```rust
            // 代码仓读取：本机 git clone 缓存 + worktree（code_* 工具，全部本地只读）
            let code_manager = Arc::new(crate::code::CodeRepoManager::new(paths.repos_dir()));
            let code_deps = Arc::new(crate::tools::builtin::code::CodeToolDeps {
                manager: code_manager.clone(),
                db: pool.clone(),
            });
```

3. `crate::tools::builtin::arthas::register_all(...)` 调用后加：

```rust
            crate::tools::builtin::code::register_all(&mut tool_registry, code_deps);
```

4. `app.manage(AppState { ... })` 中 `arthas: arthas_manager,` 后加 `code_repos: code_manager,`。

- [ ] **Step 5.6: 运行全部 code 测试 + cargo check**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- code builtin::code tools::category`
Expected: 全部 PASS；`cargo check --manifest-path src-tauri/Cargo.toml` 无错误

- [ ] **Step 5.7: Commit**

```bash
git add src-tauri/src/tools/category.rs src-tauri/src/tools/builtin/code/mod.rs src-tauri/src/tools/builtin/mod.rs src-tauri/src/lib.rs
git commit -m "feat: six code_* MCP tools with Code category and lib wiring"
```

---

### Task 6: 设置 IPC commands（映射列表 + 缓存管理）

**Files:**
- Modify: `src-tauri/src/app/service_repos.rs`（追加 command）
- Modify: `src-tauri/src/lib.rs`（invoke_handler 注册）

- [ ] **Step 6.1: service_repos.rs 追加 4 个 command**

文件头部 `use tauri::State;`，文件尾部（tests 模块前）加：

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_service_repos_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<ServiceRepoRow>, String> {
    tracing::info!("list_service_repos_cmd called");
    list_service_repos(&state.db).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_service_repo_cmd(
    state: State<'_, crate::AppState>,
    service: String,
) -> Result<(), String> {
    tracing::info!(service = %service, "delete_service_repo_cmd called");
    delete_service_repo(&state.db, &service)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_repo_cache_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<crate::code::RepoCacheEntry>, String> {
    tracing::info!("list_repo_cache_cmd called");
    Ok(state.code_repos.list_cache().await)
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_repo_cache_cmd(
    state: State<'_, crate::AppState>,
    url_hash: String,
) -> Result<(), String> {
    tracing::info!(url_hash = %url_hash, "delete_repo_cache_cmd called");
    state.code_repos.delete_cache(&url_hash).await
}
```

- [ ] **Step 6.2: lib.rs invoke_handler 注册**

`.invoke_handler(tauri::generate_handler![...])` 列表中 `app::settings::set_auto_approve_tools_cmd,` 后加：

```rust
            app::service_repos::list_service_repos_cmd,
            app::service_repos::delete_service_repo_cmd,
            app::service_repos::list_repo_cache_cmd,
            app::service_repos::delete_repo_cache_cmd,
```

- [ ] **Step 6.3: 验证编译 + 测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml; cargo test --manifest-path src-tauri/Cargo.toml -- service_repos`
Expected: 编译通过，测试 PASS

- [ ] **Step 6.4: Commit**

```bash
git add src-tauri/src/app/service_repos.rs src-tauri/src/lib.rs
git commit -m "feat: settings ipc commands for service repo mappings and cache"
```

---

### Task 7: 前端 — 类型 / IPC / 工具面板 / 设置区块

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`
- Modify: `src/components/tools/ToolsPanel.tsx`
- Create: `src/components/settings/CodeRepoSection.tsx`
- Modify: `src/components/agents/AgentSettingsDialog.tsx`

- [ ] **Step 7.1: types.ts**

`ToolCategory` 联合类型 `"arthas"` 后插一行 `| "code"`；文件尾部追加：

```ts
export interface ServiceRepoRow {
  service: string;
  repo_url: string;
  last_ref: string | null;
  updated_at: string;
  last_used_at: string;
}

export interface RepoCacheEntry {
  url_hash: string;
  repo_url: string;
  disk_bytes: number;
  worktrees: number;
}
```

- [ ] **Step 7.2: ipc.ts**

import 类型行追加 `ServiceRepoRow, RepoCacheEntry`；文件尾部追加：

```ts
export async function listServiceRepos(): Promise<ServiceRepoRow[]> {
  return invoke<ServiceRepoRow[]>("list_service_repos_cmd");
}

export async function deleteServiceRepo(service: string): Promise<void> {
  return invoke<void>("delete_service_repo_cmd", { service });
}

export async function listRepoCache(): Promise<RepoCacheEntry[]> {
  return invoke<RepoCacheEntry[]>("list_repo_cache_cmd");
}

export async function deleteRepoCache(urlHash: string): Promise<void> {
  return invoke<void>("delete_repo_cache_cmd", { urlHash });
}
```

- [ ] **Step 7.3: ToolsPanel.tsx**

import 区 `Stack` 后加 `FileCode`；`CATEGORY_META` 数组 arthas 行后插：

```ts
  { key: "code", label: "代码仓", icon: FileCode },
```

`collapsed` 初始对象 `arthas: true,` 后插 `code: true,`。

- [ ] **Step 7.4: 创建 CodeRepoSection.tsx**

```tsx
import { useCallback, useEffect, useState } from "react";
import { Trash, CircleNotch, FileCode } from "@phosphor-icons/react";
import { listRepoCache, listServiceRepos, deleteRepoCache, deleteServiceRepo } from "@/lib/ipc";
import type { ServiceRepoRow, RepoCacheEntry } from "@/lib/types";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

export function CodeRepoSection() {
  const [mappings, setMappings] = useState<ServiceRepoRow[] | null>(null);
  const [cache, setCache] = useState<RepoCacheEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, c] = await Promise.all([listServiceRepos(), listRepoCache()]);
      setMappings(m);
      setCache(c);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleDeleteMapping = async (service: string) => {
    setBusy(`map:${service}`);
    try {
      await deleteServiceRepo(service);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleDeleteCache = async (urlHash: string) => {
    setBusy(`cache:${urlHash}`);
    try {
      await deleteRepoCache(urlHash);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="px-5 py-3 space-y-3">
      <label className="text-sm text-foreground">代码仓</label>
      <p className="text-xs text-muted-foreground">
        agent 诊断时问询的服务→代码仓记忆；缓存为完整 clone（本机 repos 目录），删除后下次 clone 重建
      </p>

      {/* 服务映射 */}
      <div className="space-y-1">
        <p className="text-xs text-muted-foreground">服务映射（{mappings?.length ?? 0}）</p>
        {mappings?.length === 0 && (
          <p className="text-xs text-muted-foreground/70">暂无记忆（agent 问询仓地址后自动记录）</p>
        )}
        {mappings?.map((m) => (
          <div
            key={m.service}
            className="flex items-center gap-2 px-2 py-1.5 rounded-md border border-border bg-surface-2"
          >
            <div className="flex-1 min-w-0">
              <p className="text-xs text-foreground truncate">{m.service}</p>
              <p
                className="text-[11px] text-muted-foreground truncate"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                {m.repo_url}
                {m.last_ref ? ` @ ${m.last_ref}` : ""}
              </p>
            </div>
            <button
              onClick={() => handleDeleteMapping(m.service)}
              disabled={busy === `map:${m.service}`}
              aria-label={`删除 ${m.service} 映射`}
              className="shrink-0 text-muted-foreground hover:text-destructive transition-colors cursor-pointer disabled:opacity-50"
            >
              {busy === `map:${m.service}` ? (
                <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Trash size={14} weight="regular" aria-hidden="true" />
              )}
            </button>
          </div>
        ))}
      </div>

      {/* 缓存仓库 */}
      <div className="space-y-1">
        <p className="text-xs text-muted-foreground">缓存仓库（{cache?.length ?? 0}）</p>
        {cache?.length === 0 && (
          <p className="text-xs text-muted-foreground/70 flex items-center gap-1">
            <FileCode size={12} aria-hidden="true" /> 暂无缓存
          </p>
        )}
        {cache?.map((c) => (
          <div
            key={c.url_hash}
            className="flex items-center gap-2 px-2 py-1.5 rounded-md border border-border bg-surface-2"
          >
            <div className="flex-1 min-w-0">
              <p
                className="text-[11px] text-foreground truncate"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                {c.repo_url}
              </p>
              <p className="text-[11px] text-muted-foreground">
                {formatBytes(c.disk_bytes)} · {c.worktrees} 个检出
              </p>
            </div>
            <button
              onClick={() => handleDeleteCache(c.url_hash)}
              disabled={busy === `cache:${c.url_hash}`}
              aria-label="删除缓存仓库"
              className="shrink-0 text-muted-foreground hover:text-destructive transition-colors cursor-pointer disabled:opacity-50"
            >
              {busy === `cache:${c.url_hash}` ? (
                <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Trash size={14} weight="regular" aria-hidden="true" />
              )}
            </button>
          </div>
        ))}
      </div>

      {error && <p className="text-xs text-destructive break-words">{error}</p>}
    </div>
  );
}
```

- [ ] **Step 7.5: AgentSettingsDialog.tsx 挂载区块**

import 区加：

```tsx
import { CodeRepoSection } from "@/components/settings/CodeRepoSection";
```

「Runtime logs」section（`{/* Runtime logs */}` 注释块）之前插入：

```tsx
        {/* Code repos (service mappings + clone cache) */}
        <div className="border-t border-border shrink-0 max-h-[240px] overflow-y-auto">
          <CodeRepoSection />
        </div>
```

- [ ] **Step 7.6: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 7.7: Commit**

```bash
git add src/lib/types.ts src/lib/ipc.ts src/components/tools/ToolsPanel.tsx src/components/settings/CodeRepoSection.tsx src/components/agents/AgentSettingsDialog.tsx
git commit -m "feat: code repo settings section and tools panel code category"
```

---

### Task 8: TOOL_GUIDANCE 注入 + 全量验证

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`

- [ ] **Step 8.1: 写失败测试**

`src-tauri/src/agent/prompt.rs` tests 模块追加：

```rust
    #[test]
    fn test_tool_guidance_mentions_code_tools() {
        assert!(TOOL_GUIDANCE.contains("code_get_repo"));
        assert!(TOOL_GUIDANCE.contains("code_open_repo"));
        assert!(TOOL_GUIDANCE.contains("code_repo_status"));
        assert!(TOOL_GUIDANCE.contains("code_read_file"));
        assert!(TOOL_GUIDANCE.contains("code_search"));
        assert!(TOOL_GUIDANCE.contains("code_list_files"));
        assert!(TOOL_GUIDANCE.contains("commitid"));
        assert!(TOOL_GUIDANCE.contains("jad"), "cross-check with arthas jad guidance");
    }
```

- [ ] **Step 8.2: 运行确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- test_tool_guidance_mentions_code_tools`
Expected: FAIL（TOOL_GUIDANCE 无 code_* 内容）

- [ ] **Step 8.3: 在 TOOL_GUIDANCE 追加读代码指引**

`const TOOL_GUIDANCE` 中 JFR 那一行之后、「用户提到的环境先与 list_environments…」行之前插入：

```text
- 读源码（栈帧回源/业务逻辑确认）：先 code_get_repo(服务名) 查记忆的代码仓；有记忆 → 向用户确认仓地址仍对 + 确认本次部署的 ref（分支经常变，last_ref 仅作提示）；无记忆 → 问用户仓地址和 ref（分支/tag/commitid）。然后 code_open_repo(repo_url, ref, service) → 轮询 code_repo_status 到 ready（cloning/fetching 稍候再查、勿重复 open；同 url+ref 幂等）→ code_read_file / code_search / code_list_files 读码：线程栈类名用 code_list_files(glob="**/类名.java") 或 code_search("class 类名") 定位，栈帧行号直接对 code_read_file 的行号。发现代码与部署不匹配（栈帧行号对不上、jad 反编译与源码不一致、搜不到预期符号）→ 问用户部署的 commitid（部署系统/镜像 label/META-INF/MANIFEST 的 Implementation-Build）重新 code_open_repo。stale_warning 要告知用户代码可能非最新；源码与部署版本可能不一致，可用 arthas jad 交叉验证。
```

- [ ] **Step 8.4: 运行测试通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- test_tool_guidance_mentions_code_tools`
Expected: PASS

- [ ] **Step 8.5: 全量验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（既有测试不受影响——category 名单测试已在 Task 5 更新）

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 8.6: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: tool guidance for code reading workflow"
```

---

## 完成核对（对照 spec）

- [ ] 6 个工具 + Code 分类（第 9 组，arthas 后 file_transfer 前）——Task 5
- [ ] repo_id = hash(url+ref) 幂等 + in-flight 去重——Task 3 `open`
- [ ] 完整 clone + 每 (repo, ref) worktree + fetch 刷新 + stale 降级——Task 3 `run_pipeline`
- [ ] ref 泛化（分支/tag/commitid）——Task 3 `resolve_ref` + 测试
- [ ] 读/搜/列（行号、.gitignore、路径逃逸拦截、截断）——Task 4
- [ ] service_repos 映射（仓地址权威、last_ref 提示、last_used_at 刷新）——Task 1
- [ ] 设置页两列表（映射 + 缓存磁盘占用/删除、进行中拒绝删除）——Task 6/7
- [ ] TOOL_GUIDANCE 问询/确认分支/commitid 追问/jad 交叉验证——Task 8
- [ ] 日志规范：git stderr 全量记录、工具执行入口 info!——Task 2/5
