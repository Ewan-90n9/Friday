# 远程工具装备机制（ensure_tool + JDK 包）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增通用远程工具装备机制：MCP 工具 `ensure_tool` 让 agent 在 JVM 诊断前自动探测目标环境 JRE 版本、从华为 Artifactory 下载匹配的 BiSheng JDK 到 `/tmp/friday-tools` 并返回工具全路径（双通道下载：目标自拉优先，Friday 下载 + SFTP 上传兜底）。

**Architecture:** 新增 `provision/` 模块（ToolPackage trait + JdkPackage 实现 + 本地下载缓存），`ExecChannel` trait 扩展 `upload()` 方法（russh SFTP），SQLite 新增全局设置表存 Artifactory base URL，EventBus 新增 ProvisionProgress 事件，前端 AgentSettingsDialog 加设置项。

**Tech Stack:** Tauri 2 / Rust（russh 0.45 SFTP、sqlx、tokio、sha2 0.10）、React + zustand + Tailwind v4。

**Spec:** [docs/superpowers/specs/2026-08-27-remote-tool-provisioning-design.md](../specs/2026-08-27-remote-tool-provisioning-design.md)

**约定（所有任务遵守）：**
- Rust 检查命令：`cargo check --manifest-path src-tauri/Cargo.toml`；测试：`cargo test --manifest-path src-tauri/Cargo.toml`（在仓库根目录跑）
- 前端类型检查：`pnpm typecheck`
- 日志规范：新增 Tauri command 一律 `#[tracing::instrument(skip(state))]`；错误路径 `tracing::error!`/`warn!`
- 文件路径统一走 `infra/paths.rs` 的 `Paths`，不内联 `.join()`
- 测试放同文件 `#[cfg(test)] mod tests`，用 `tempfile::tempdir()` + `crate::infra::db::init`
- russh 0.45 的 SFTP 客户端是独立 crate `russh-sftp`（`channel_open_session` 后发起 subsystem 请求）；若与 russh 0.45 版本不兼容，回退方案见 Task 4 备注

**任务依赖图：**

```
Task 1 (settings 表迁移) ──────────────┐
Task 2 (Paths::cache_dir) ────────────┤
Task 3 (ExecChannel::upload trait 方法) → Task 4 (SshTransport SFTP 实现)
Task 5 (BiSheng 解析纯函数)            │
Task 6 (transfer.rs 本地下载+缓存)      │
Task 7 (JdkPackage ensure 流程) ← 依赖 2,3,5,6
Task 8 (ProvisionProgress 事件) ← 依赖 7
Task 9 (ensure_tool MCP 工具) ← 依赖 1,7
Task 10 (prompt 引导) ← 依赖 9
Task 11 (前端 settings UI) ← 依赖 1
Task 12 (全量验证收口) ← 依赖全部
```

---

## Task 1: DB 迁移 — app_settings 全局设置表

**Files:**
- Create: `src-tauri/migrations/0008_app_settings.sql`
- Modify: `src-tauri/src/infra/db.rs`（init 函数）
- Modify: `src-tauri/src/app/settings.rs`（新建）

**关于 app/settings.rs：** 新文件，全局设置读写模块。key-value 表 + 两个函数（get_setting / set_setting）+ 两个 Tauri command。Artifactory base URL 是第一个设置项。

- [ ] **Step 1: 写失败测试**

新建 `src-tauri/src/app/settings.rs`，先只写测试部分（实现部分留空跑失败）：

```rust
use sqlx::SqlitePool;

pub const KEY_ARTIFACTORY_BASE_URL: &str = "artifactory_base_url";
pub const DEFAULT_ARTIFACTORY_BASE_URL: &str =
    "https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release";

/// 读取设置项，未设置时返回 None
pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    todo!()
}

/// 写入设置项（upsert）
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    todo!()
}

/// 读取 Artifactory base URL：未设置时返回默认值
pub async fn artifactory_base_url(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    Ok(get_setting(pool, KEY_ARTIFACTORY_BASE_URL)
        .await?
        .unwrap_or_else(|| DEFAULT_ARTIFACTORY_BASE_URL.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_setting_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert!(get_setting(&pool, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_set_then_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, "k1", "v1").await.unwrap();
        assert_eq!(get_setting(&pool, "k1").await.unwrap().as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn test_set_setting_upsert_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, "k1", "v1").await.unwrap();
        set_setting(&pool, "k1", "v2").await.unwrap();
        assert_eq!(get_setting(&pool, "k1").await.unwrap().as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn test_artifactory_base_url_defaults_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert_eq!(artifactory_base_url(&pool).await.unwrap(), DEFAULT_ARTIFACTORY_BASE_URL);
    }

    #[tokio::test]
    async fn test_artifactory_base_url_returns_custom() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_ARTIFACTORY_BASE_URL, "https://example.com/artifactory").await.unwrap();
        assert_eq!(
            artifactory_base_url(&pool).await.unwrap(),
            "https://example.com/artifactory"
        );
    }
}
```

在 `src-tauri/src/app/mod.rs` 加 `pub mod settings;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app::settings`
Expected: FAIL（`app_settings` 表不存在，SQL 报错 no such table；且 todo!() panic）

- [ ] **Step 3: 实现迁移文件**

新建 `src-tauri/migrations/0008_app_settings.sql`：

```sql
CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

- [ ] **Step 4: 在 db.rs init 中执行迁移**

`src-tauri/src/infra/db.rs` init 函数中，`add_column_if_not_exists(&pool, "environments", "private_key_path", "TEXT").await?;`（第 28 行）之后追加：

```rust
    // Migration (provisioning): global app settings (key-value)
    let schema8 = include_str!("../../migrations/0008_app_settings.sql");
    sqlx::query(schema8).execute(&pool).await?;
```

- [ ] **Step 5: 实现 get_setting / set_setting**

`src-tauri/src/app/settings.rs` 替换两个 `todo!()`：

```rust
pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app::settings`
Expected: 5 个测试全部 PASS

- [ ] **Step 7: 跑全量 DB 测试防回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml infra::db`
Expected: 全部 PASS

- [ ] **Step 8: Commit**

```bash
git add src-tauri/migrations/0008_app_settings.sql src-tauri/src/infra/db.rs src-tauri/src/app/settings.rs src-tauri/src/app/mod.rs
git commit -m "feat: app_settings table with get/set and artifactory base url default"
```

---

## Task 2: Paths 新增 cache_dir

**Files:**
- Modify: `src-tauri/src/infra/paths.rs`

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/infra/paths.rs` 的 `mod tests` 中追加（放在 `test_models_dir_returns_root_join_models` 之后）：

```rust
    #[test]
    fn test_cache_dir_returns_root_join_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.cache_dir(), tmp.path().join("cache"));
    }

    #[test]
    fn test_ensure_dirs_creates_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        assert!(tmp.path().join("cache").is_dir());
    }
```

同时把现有测试 `test_ensure_dirs_creates_all_six_subdirs` 改名 `test_ensure_dirs_creates_all_seven_subdirs` 并追加断言 `assert!(tmp.path().join("cache").is_dir());`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml infra::paths`
Expected: FAIL（`cache_dir` 方法不存在，编译错误）

- [ ] **Step 3: 实现**

在 `Paths` impl 中 `models_dir()` 之后加：

```rust
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }
```

`ensure_dirs()` 的目录数组中追加 `self.cache_dir(),`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml infra::paths`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/infra/paths.rs
git commit -m "feat: Paths::cache_dir for provisioning local download cache"
```

---

## Task 3: ExecChannel trait 新增 upload 方法

**Files:**
- Modify: `src-tauri/src/exec/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/exec/channel.rs` 底部追加测试模块：

```rust
#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use super::*;
    use std::path::Path;

    struct RecordingChannel {
        uploaded: tokio::sync::Mutex<Vec<(std::path::PathBuf, String)>>,
    }

    #[async_trait]
    impl ExecChannel for RecordingChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, local: &Path, remote_path: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.uploaded.lock().await.push((local.to_path_buf(), remote_path.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_upload_trait_method_dispatches() {
        let ch = RecordingChannel { uploaded: tokio::sync::Mutex::new(Vec::new()) };
        let dyn_ch: &dyn ExecChannel = &ch;
        dyn_ch.upload(Path::new("/tmp/f.tar.gz"), "/tmp/friday-tools/f.tar.gz").await.unwrap();
        assert_eq!(ch.uploaded.lock().await.len(), 1);
        assert_eq!(ch.uploaded.lock().await[0].1, "/tmp/friday-tools/f.tar.gz");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::channel`
Expected: FAIL（trait 无 `upload` 方法，编译错误）

- [ ] **Step 3: 实现**

`src-tauri/src/exec/channel.rs` 的 `ExecChannel` trait 中，`is_alive` 之后追加：

```rust
    /// 上传文件到远端路径（SFTP 或等价实现）。供工具装备（推 JDK 包）与后续
    /// artifacts 回拉复用。默认返回未实现错误——Mock/测试实现按需覆盖。
    async fn upload(&self, _local: &std::path::Path, _remote_path: &str)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("upload not implemented for this channel".into())
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec::channel`
Expected: PASS

- [ ] **Step 5: 跑全量测试防回归（现有 Mock 实现不受默认方法影响）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/exec/channel.rs
git commit -m "feat: ExecChannel::upload default method for file transfer"
```

---

## Task 4: SshTransport SFTP upload 实现

**Files:**
- Modify: `src-tauri/Cargo.toml`（dependencies）
- Modify: `src-tauri/src/exec/ssh.rs`

**备注：** russh 0.45 的 SFTP 走独立 crate `russh-sftp 2.x`：`channel_open_session` 打开 channel 后 `sftp = SftpSession::new(channel.into_stream())`，再 `sftp.write().create(remote_path)` 流式写。`russh-sftp` 与 russh 0.45 兼容（0.45 的 Channel 实现了 `Into<russh_sftp::channel::Channel>` 所需的 stream 转换）。若编译发现版本不兼容，回退方案：用 `russh-sftp 1.1`（russh 0.4x 系列配套）；再不行则 upload 内部降级为 `cat > remote_path` 的 exec 方案（base64 分块追加），并在日志中 warn 说明。

- [ ] **Step 1: 加依赖**

`src-tauri/Cargo.toml` `[dependencies]` 中 `russh = "0.45"` 之后追加：

```toml
russh-sftp = "2"
```

- [ ] **Step 2: 实现 upload**

在 `src-tauri/src/exec/ssh.rs` 的 `impl ExecChannel for SshTransport` 中（`disconnect` 之后）追加：

```rust
    async fn upload(&self, local: &std::path::Path, remote_path: &str)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.conn.lock().await;
        let Some(c) = conn.as_mut() else {
            return Err("ssh not connected (call connect first)".into());
        };

        let channel = c.handle.channel_open_session().await?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await?;

        let file = tokio::fs::File::open(local).await?;
        let mut reader = tokio::io::BufReader::with_capacity(256 * 1024, file);
        let mut remote_file = sftp
            .write()
            .create(true)
            .truncate(true)
            .open(remote_path)
            .await?;

        let mut buf = vec![0u8; 32 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await?;
            if n == 0 {
                break;
            }
            use tokio::io::AsyncWriteExt;
            remote_file.write_all(&buf[..n]).await?;
            total += n as u64;
        }
        use tokio::io::AsyncWriteExt;
        remote_file.shutdown().await?;
        sftp.close().await?;

        tracing::info!(
            env_id = %self.env_id,
            local = %local.display(),
            remote_path,
            bytes = total,
            "sftp upload complete"
        );
        Ok(())
    }
```

- [ ] **Step 3: 编译检查**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。若 `russh-sftp 2` 与 russh 0.45 的 stream API 不匹配，按本 task 备注调整版本或回退方案，调整后重新 `cargo check`。

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml exec`
Expected: 全部 PASS（upload 无真实网络测试，类型对齐由 check 保证）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/exec/ssh.rs
git commit -m "feat: SshTransport SFTP upload via russh-sftp"
```

---

## Task 5: BiSheng 版本解析纯函数（provision/jdk.rs 第一部分）

**Files:**
- Create: `src-tauri/src/provision/mod.rs`
- Create: `src-tauri/src/provision/jdk.rs`

- [ ] **Step 1: 写失败测试**

新建 `src-tauri/src/provision/mod.rs`：

```rust
pub mod jdk;
pub mod package;
pub mod transfer;
```

（`package.rs`、`transfer.rs` 在 Task 6/7 创建；本 task 先只写 `pub mod jdk;`，后两行随后续 task 加。）

新建 `src-tauri/src/provision/jdk.rs`，先写测试与类型骨架（函数体 `todo!()`）：

```rust
use serde::Serialize;

/// BiSheng 版本解析结果
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BishengVersion {
    /// 产品目录名：BiSheng_JDK_Enterprise → "BiSheng JDK Enterprise"
    pub product_dir: String,
    /// 大版本目录名："BiSheng JDK Enterprise 205"
    pub major_dir: String,
    /// 完整版本目录名（原串原样）
    pub full_dir: String,
}

/// java -version + uname -m 的探测结果
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JvmProbe {
    /// OpenJDK 版本号，如 "21.0.11"
    pub openjdk_version: String,
    /// BiSheng 版本串，如 "BiSheng_JDK_Enterprise_205.2.0.110.B001"
    pub bisheng_version: String,
    /// 归一化架构名：x64 / aarch64
    pub arch: String,
}

/// 从 java -version 与 uname -m 的合并输出解析探测信息
pub fn parse_probe_output(stdout: &str, stderr: &str) -> Result<JvmProbe, String> {
    todo!()
}

/// BiSheng 版本串 → 三段目录名
pub fn parse_bisheng_version(s: &str) -> Result<BishengVersion, String> {
    todo!()
}

/// 拼下载 URL（base 末尾无斜杠）
pub fn build_download_url(base: &str, probe: &JvmProbe) -> Result<String, String> {
    todo!()
}

/// uname -m 输出 → 产物 arch 名
pub fn normalize_arch(uname_m: &str) -> Result<String, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_STDOUT: &str = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
    const PROBE_STDERR: &str = "openjdk version \"21.0.11\" 2025-04-15\nOpenJDK Runtime Environment (build 21.0.11+9-LTS)\nOpenJDK 64-Bit Server VM (build 21.0.11+9-LTS, mixed mode)\n";

    #[test]
    fn test_parse_probe_output_standard() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        assert_eq!(probe.openjdk_version, "21.0.11");
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.arch, "x64");
    }

    #[test]
    fn test_parse_probe_output_bisheng_on_stderr() {
        // 部分版本 BiSheng 串在 stderr —— 两路都扫
        let stdout = "---\nx86_64\n";
        let stderr = &format!("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n");
        let probe = parse_probe_output(stdout, stderr).unwrap();
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.openjdk_version, "21.0.11");
    }

    #[test]
    fn test_parse_probe_output_no_bisheng_is_unsupported_vendor() {
        let stdout = "---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11\" 2025-04-15\n";
        let err = parse_probe_output(stdout, stderr).unwrap_err();
        assert!(err.contains("unsupported_vendor"), "err: {err}");
        assert!(err.contains("21.0.11"), "err should carry original output: {err}");
    }

    #[test]
    fn test_parse_probe_output_no_openjdk_version() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
        let err = parse_probe_output(stdout, "OpenJDK Runtime Environment (build 21.0.11+9-LTS)\n").unwrap_err();
        assert!(err.contains("parse_failed"), "err: {err}");
    }

    #[test]
    fn test_parse_probe_output_java_not_found() {
        // java 命令不存在的输出（bash: java: command not found）
        let err = parse_probe_output("---\n", "bash: java: command not found\n").unwrap_err();
        assert!(err.contains("probe_failed"), "err: {err}");
    }

    #[test]
    fn test_parse_probe_output_unknown_arch() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nriscv64\n";
        let err = parse_probe_output(stdout, PROBE_STDERR).unwrap_err();
        assert!(err.contains("parse_failed") || err.contains("arch"), "err: {err}");
    }

    #[test]
    fn test_parse_bisheng_version_standard() {
        let v = parse_bisheng_version("BiSheng_JDK_Enterprise_205.2.0.110.B001").unwrap();
        assert_eq!(v.product_dir, "BiSheng JDK Enterprise");
        assert_eq!(v.major_dir, "BiSheng JDK Enterprise 205");
        assert_eq!(v.full_dir, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
    }

    #[test]
    fn test_parse_bisheng_version_malformed() {
        assert!(parse_bisheng_version("OpenJDK").is_err());
        // 尾部无版本数字
        assert!(parse_bisheng_version("BiSheng_JDK_Enterprise_").is_err());
        // 尾部版本不是数字开头
        assert!(parse_bisheng_version("BiSheng_JDK_Enterprise_ABC").is_err());
        assert!(parse_bisheng_version("").is_err());
    }

    #[test]
    fn test_parse_bisheng_version_two_segment_product() {
        // 双段产品名变体：product 部分所有 _ 都还原为空格
        let v = parse_bisheng_version("BiSheng_JDK_Compact_105.1.0.B002").unwrap();
        assert_eq!(v.product_dir, "BiSheng JDK Compact");
        assert_eq!(v.major_dir, "BiSheng JDK Compact 105");
        assert_eq!(v.full_dir, "BiSheng_JDK_Compact_105.1.0.B002");
    }

    #[test]
    fn test_build_download_url_full() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release", &probe).unwrap();
        assert_eq!(
            url,
            "https://artifactory.example.com/artifactory/release/BiSheng%20JDK%20Enterprise/BiSheng%20JDK%20Enterprise%20205/BiSheng_JDK_Enterprise_205.2.0.110.B001/jdk-21.0.11-linux-x64.tar.gz"
        );
    }

    #[test]
    fn test_build_download_url_base_trailing_slash_normalized() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release/", &probe).unwrap();
        assert!(url.contains("release/BiSheng%20JDK%20Enterprise/"), "url: {url}");
        assert!(!url.contains("release//"), "url: {url}");
    }

    #[test]
    fn test_normalize_arch() {
        assert_eq!(normalize_arch("x86_64\n").unwrap(), "x64");
        assert_eq!(normalize_arch("aarch64\n").unwrap(), "aarch64");
        assert!(normalize_arch("riscv64").is_err());
    }
}
```

在 `src-tauri/src/lib.rs` 顶部 `mod knowledge;` 之后加 `mod provision;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provision::jdk`
Expected: FAIL（todo!() panic / 编译错误）

- [ ] **Step 3: 实现四个纯函数**

替换 `src-tauri/src/provision/jdk.rs` 中的 `todo!()`：

```rust
/// 从 java -version 与 uname -m 的合并输出解析探测信息。
/// 输出布局：stdout/stderr 混合扫描。分隔行 "---" 之后的 stdout 尾行是 uname -m。
pub fn parse_probe_output(stdout: &str, stderr: &str) -> Result<JvmProbe, String> {
    // java 不存在：bash 报 command not found
    if stdout.contains("command not found") || stderr.contains("command not found") {
        return Err(format!(
            "probe_failed: java not found on target. stdout: {stdout:?} stderr: {stderr:?}. \
             请先通过 run_command 确认目标服务的 java 可执行文件路径，再用 java_bin 参数指定"
        ));
    }

    // OpenJDK 版本行：openjdk version "21.0.11" —— stdout/stderr 两路都扫
    let combined = format!("{stdout}\n{stderr}");
    let openjdk_version = combined
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.starts_with("openjdk version").then(|| {
                l.split('"').nth(1).unwrap_or_default().to_string()
            })
        })
        .ok_or_else(|| {
            format!(
                "parse_failed: no `openjdk version` line found. stdout: {stdout:?} stderr: {stderr:?}"
            )
        })?;
    if openjdk_version.is_empty() {
        return Err(format!("parse_failed: empty openjdk version. stdout: {stdout:?} stderr: {stderr:?}"));
    }

    // BiSheng 版本串：形如 BiSheng_JDK_Enterprise_205.2.0.110.B001 的非空行
    let bisheng_version = combined
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("BiSheng") && parse_bisheng_version(l).is_ok())
        .ok_or_else(|| {
            format!(
                "unsupported_vendor: no BiSheng version string found (only BiSheng is supported in this version). stdout: {stdout:?} stderr: {stderr:?}"
            )
        })?
        .to_string();

    // arch：stdout 中 "---" 分隔行之后的最后一行非空内容
    let arch_raw = stdout
        .split("---")
        .nth(1)
        .and_then(|tail| tail.lines().map(str::trim).filter(|l| !l.is_empty()).last())
        .ok_or_else(|| format!("parse_failed: no uname -m output after --- separator. stdout: {stdout:?}"))?;
    let arch = normalize_arch(arch_raw)?;

    Ok(JvmProbe { openjdk_version, bisheng_version, arch })
}

/// BiSheng 版本串 → 三段目录名。
/// BiSheng_JDK_Enterprise_205.2.0.110.B001
///   product = "BiSheng JDK Enterprise"（字母段 _ → 空格）
///   major   = product + " " + 205
///   full    = 原串
pub fn parse_bisheng_version(s: &str) -> Result<BishengVersion, String> {
    let s = s.trim();
    // 格式：<字母段(_分隔)>_<数字 major>.<数字串>
    // 用正则捕获：BiSheng 打头，中段允许字母/下划线，尾部是 205.2.0.110.B001 式版本
    let re = regex::Regex::new(
        r"^(?P<product>BiSheng(?:_[A-Za-z0-9]+)*?)_(?P<version>\d+\.\d+(?:\.\d+)*(?:\.?[AB]\d+)?)$",
    )
    .map_err(|e| format!("parse_failed: regex build error: {e}"))?;
    let caps = re.captures(s).ok_or_else(|| format!("parse_failed: not a BiSheng version string: {s:?}"))?;

    let product_raw = caps.name("product").unwrap().as_str();
    let version = caps.name("version").unwrap().as_str();
    let product = product_raw.replace('_', " ");

    // major = 版本串第一段数字（205）
    let major = version.split('.').next().unwrap_or_default();
    if major.is_empty() {
        return Err(format!("parse_failed: no major version in {s:?}"));
    }

    Ok(BishengVersion {
        product_dir: product.clone(),
        major_dir: format!("{product} {major}"),
        full_dir: s.to_string(),
    })
}

/// 拼下载 URL。目录段 URL encode（空格 → %20），文件名不 encode（只含安全字符）。
pub fn build_download_url(base: &str, probe: &JvmProbe) -> Result<String, String> {
    let v = parse_bisheng_version(&probe.bisheng_version)
        .map_err(|e| format!("parse_failed: {e}"))?;
    let base = base.trim_end_matches('/');
    Ok(format!(
        "{base}/{}/{}/{}/jdk-{}-linux-{}.tar.gz",
        url_encode_path_segment(&v.product_dir),
        url_encode_path_segment(&v.major_dir),
        url_encode_path_segment(&v.full_dir),
        probe.openjdk_version,
        probe.arch,
    ))
}

/// uname -m 输出 → 产物 arch 名
pub fn normalize_arch(uname_m: &str) -> Result<String, String> {
    match uname_m.trim() {
        "x86_64" | "amd64" => Ok("x64".to_string()),
        "aarch64" | "arm64" => Ok("aarch64".to_string()),
        other => Err(format!("parse_failed: unsupported arch: {other:?} (supported: x86_64, aarch64)")),
    }
}

/// URL path segment 编码：RFC 3986 非保留字符之外全部 percent-encode
fn url_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
```

注意：`test_build_download_url_full` 期望 URL 中 `full_dir` 里的 `_` 保留（`_` 是非保留字符不编码），空格编码为 `%20`——上面的 `url_encode_path_segment` 行为与此一致。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provision::jdk`
Expected: 11 个测试全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/provision/mod.rs src-tauri/src/provision/jdk.rs src-tauri/src/lib.rs
git commit -m "feat: BiSheng version parsing and artifactory URL building"
```

---

## Task 6: transfer.rs — 本地下载与缓存

**Files:**
- Create: `src-tauri/src/provision/transfer.rs`
- Modify: `src-tauri/Cargo.toml`（加 sha2 依赖）

- [ ] **Step 1: 加依赖**

`src-tauri/Cargo.toml` `[dependencies]` 中 `uuid` 之后追加：

```toml
sha2 = "0.10"
```

- [ ] **Step 2: 写失败测试**

新建 `src-tauri/src/provision/transfer.rs`：

```rust
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// URL → 本地缓存路径：<cache_dir>/<sha256(url) 前缀>.tar.gz
pub fn cache_path_for(cache_dir: &Path, url: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    let name = format!("{:x}.tar.gz", digest);
    cache_dir.join(name)
}

/// 下载 URL 到缓存路径（已存在且非空则复用）。curl.exe 模式对齐 embedding.rs。
/// 返回本地文件路径。
pub fn download_to_cache(url: &str, cache_dir: &Path) -> Result<PathBuf, String> {
    let _ = (url, cache_dir);
    todo!()
}

/// 校验下载产物：存在且大于 min_bytes
pub fn validate_download(path: &Path, min_bytes: u64) -> Result<(), String> {
    let _ = (path, min_bytes);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_path_for_is_deterministic_and_named_by_url_hash() {
        let dir = Path::new("/tmp/cache");
        let p1 = cache_path_for(dir, "https://example.com/jdk-21.tar.gz");
        let p2 = cache_path_for(dir, "https://example.com/jdk-21.tar.gz");
        let p3 = cache_path_for(dir, "https://example.com/jdk-22.tar.gz");
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        assert!(p1.file_name().unwrap().to_string_lossy().ends_with(".tar.gz"));
        assert!(p1.file_name().unwrap().to_string_lossy().len() >= 16);
    }

    #[test]
    fn test_validate_download_rejects_missing() {
        assert!(validate_download(Path::new("/nonexistent/x.tar.gz"), 1).is_err());
    }

    #[test]
    fn test_validate_download_rejects_too_small() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("small.tar.gz");
        std::fs::write(&f, vec![0u8; 1024]).unwrap();
        assert!(validate_download(&f, 50 * 1024 * 1024).is_err());
    }

    #[test]
    fn test_validate_download_accepts_large() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("big.tar.gz");
        std::fs::write(&f, vec![0u8; 51 * 1024 * 1024]).unwrap();
        assert!(validate_download(&f, 50 * 1024 * 1024).is_ok());
        // 空文件（0 字节）也拒绝
        let empty = tmp.path().join("empty.tar.gz");
        std::fs::write(&empty, b"").unwrap();
        assert!(validate_download(&empty, 1).is_err());
    }

    #[test]
    fn test_download_to_cache_reuses_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let dest = cache_path_for(&cache, "https://example.com/jdk.tar.gz");
        std::fs::write(&dest, vec![1u8; 60 * 1024 * 1024]).unwrap();
        // 缓存命中：不发起下载，直接返回路径
        let path = download_to_cache("https://example.com/jdk.tar.gz", &cache).unwrap();
        assert_eq!(path, dest);
    }

    #[test]
    fn test_download_to_cache_missing_file_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        // curl.exe 对不可达 URL 立即失败（connect-timeout 30s 内），错误信息包含 download
        // Windows CI 上该测试最多等 ~30s；本地通常 <1s
        let err = download_to_cache("http://127.0.0.1:1/never-reachable.tar.gz", &cache).unwrap_err();
        assert!(err.to_lowercase().contains("download") || err.to_lowercase().contains("curl"), "err: {err}");
        // 失败后不残留半截文件
        assert!(!cache_path_for(&cache, "http://127.0.0.1:1/never-reachable.tar.gz").exists());
    }
}
```

`src-tauri/src/provision/mod.rs` 中加 `pub mod transfer;`。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provision::transfer`
Expected: FAIL（todo!() panic / 编译错误）

- [ ] **Step 4: 实现**

```rust
/// 下载 URL 到缓存路径（已存在且非空则复用）。curl.exe 模式对齐 embedding.rs。
pub fn download_to_cache(url: &str, cache_dir: &Path) -> Result<PathBuf, String> {
    let dest = cache_path_for(cache_dir, url);
    if dest.exists() {
        let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if len > 0 {
            tracing::info!(url, path = %dest.display(), "provision: local cache hit");
            return Ok(dest);
        }
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("create cache dir: {e}"))?;

    tracing::info!(url, path = %dest.display(), "provision: downloading to local cache");
    let dest_str = dest.to_string_lossy();
    let output = std::process::Command::new("curl.exe")
        .args([
            "-L", "-o", &dest_str,
            "--connect-timeout", "30",
            "--max-time", "600",
            "--retry", "2",
            "-s", "-S",
            "-w", "%{http_code}",
            url,
        ])
        .output()
        .map_err(|e| format!("download failed: failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_file(&dest);
        return Err(format!("download failed: {}", stderr.trim()));
    }
    let http_code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !http_code.starts_with('2') {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("download failed: HTTP {http_code}"));
    }
    if !dest.exists() {
        return Err("download failed: file not created".to_string());
    }
    Ok(dest)
}

/// 校验下载产物：存在且大于 min_bytes
pub fn validate_download(path: &Path, min_bytes: u64) -> Result<(), String> {
    let len = std::fs::metadata(path).map(|m| m.len()).map_err(|e| format!("download incomplete: {e}"))?;
    if len < min_bytes {
        return Err(format!(
            "download incomplete: file is {} bytes, expected at least {min_bytes} bytes"
        ));
    }
    Ok(())
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provision::transfer`
Expected: 6 个测试全部 PASS（`test_download_to_cache_missing_file_fails_cleanly` 依赖 curl.exe 对 127.0.0.1:1 快速连接失败，Windows 上 curl.exe 是系统自带；若环境无 curl.exe 该测试会以 "failed to run curl" 失败——错误信息含 download，断言仍成立的前提是 curl.exe 存在，Windows CI 满足）

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/provision/transfer.rs src-tauri/src/provision/mod.rs
git commit -m "feat: local download cache with sha256 naming and size validation"
```

---

## Task 7: package.rs + JdkPackage ensure 流程

**Files:**
- Create: `src-tauri/src/provision/package.rs`
- Modify: `src-tauri/src/provision/jdk.rs`（追加 JdkPackage 实现）
- Modify: `src-tauri/src/provision/mod.rs`

- [ ] **Step 1: 写失败测试**

新建 `src-tauri/src/provision/package.rs`，先写类型与测试骨架：

```rust
use crate::exec::channel::ExecChannel;
use crate::app::events::EventBus;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// 阶段超时配置（秒）
pub struct StageTimeouts {
    pub probe: u64,
    pub download: u64,
    pub extract: u64,
    pub verify: u64,
}

impl Default for StageTimeouts {
    fn default() -> Self {
        Self { probe: 15, download: 600, extract: 120, verify: 15 }
    }
}

/// 装备上下文：由 MCP 工具 handler 构造传入
pub struct ProvisionContext {
    pub session_id: String,
    pub env_id: String,
    pub channel: Arc<dyn ExecChannel>,
    pub cache_dir: std::path::PathBuf,
    pub artifactory_base_url: String,
    pub timeouts: StageTimeouts,
    pub bus: EventBus,
}

/// 装备结果
#[derive(Clone, Debug, Serialize)]
pub struct ProvisionResult {
    pub tool: String,
    pub cached: bool,
    pub java_version: String,
    pub bisheng_version: String,
    pub arch: String,
    pub tool_home: String,
    pub bins: HashMap<String, String>,
    pub elapsed_ms: u64,
}

/// 装备错误：code 用于结构化返回（provision_failed / probe_failed / ...），stage 标记失败阶段
#[derive(Debug)]
pub struct ProvisionError {
    pub code: String,
    pub stage: String,
    pub message: String,
    pub url: Option<String>,
}

impl ProvisionError {
    pub fn new(code: &str, stage: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_string(), stage: stage.to_string(), message: message.into(), url: None }
    }
}

/// 进度事件：阶段级
pub fn emit_progress(ctx: &ProvisionContext, tool: &str, stage: &str, detail: &str) {
    tracing::info!(session_id = %ctx.session_id, env_id = %ctx.env_id, tool, stage, detail, "provision progress");
    ctx.bus.emit(
        &ctx.session_id,
        crate::app::events::AppEvent::ProvisionProgress {
            session_id: ctx.session_id.clone(),
            tool: tool.to_string(),
            stage: stage.to_string(),
            detail: detail.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_timeouts_defaults() {
        let t = StageTimeouts::default();
        assert_eq!(t.probe, 15);
        assert_eq!(t.download, 600);
        assert_eq!(t.extract, 120);
        assert_eq!(t.verify, 15);
    }

    #[test]
    fn test_provision_error_fields() {
        let e = ProvisionError::new("provision_failed", "extract", "disk full");
        assert_eq!(e.code, "provision_failed");
        assert_eq!(e.stage, "extract");
        assert_eq!(e.message, "disk full");
        assert!(e.url.is_none());
    }
}
```

**注意：** `AppEvent::ProvisionProgress` 变体在 Task 8 才加入 events.rs。为了让本 task 编译通过，先在 `src-tauri/src/app/events.rs` 的 `AppEvent` enum 中（`SessionDeleted` 之前）追加：

```rust
    ProvisionProgress {
        session_id: String,
        tool: String,
        stage: String,
        detail: String,
    },
```

（events.rs 的序列化测试在 Task 8 补。）

- [ ] **Step 2: EventBus 支持 disabled 模式（测试前置）**

`EventBus::new` 需要 `AppHandle`，测试环境没有真实 Tauri app。先修改 `src-tauri/src/app/events.rs`：`EventBus` 的 `handle` 改为 `Option<AppHandle>`，`emit` 在 `None` 时只走 tracing（测试/无窗口场景）：

```rust
#[derive(Clone, Default)]
pub struct EventBus {
    handle: Option<AppHandle>,
}

impl EventBus {
    pub fn new(handle: AppHandle) -> Self {
        Self { handle: Some(handle) }
    }

    /// 无 AppHandle 的 EventBus（测试用）：emit 只走 tracing 日志
    pub fn disabled() -> Self {
        Self { handle: None }
    }

    pub fn emit(&self, session_id: &str, event: AppEvent) {
        tracing::debug!(
            session_id = %session_id,
            event_type = ?std::mem::discriminant(&event),
            "emitting event"
        );
        let Some(handle) = &self.handle else {
            tracing::debug!(session_id, "event bus disabled, event not emitted to frontend");
            return;
        };
        let payload = EventPayload {
            session_id: session_id.to_string(),
            event,
        };
        if let Err(e) = handle.emit("app_event", payload) {
            tracing::error!(?e, "failed to emit event");
        }
    }
}
```

（`lib.rs` 与 `mcp/transport.rs` 中现有 `EventBus::new(handle.clone())` 调用无需改动；`EventBus` 增加 `Default` derive 不影响现有构造。）

跑 `cargo test --manifest-path src-tauri/Cargo.toml app::events` 确认现有测试 PASS。

- [ ] **Step 3: 写 JdkPackage 类型骨架与集成测试（Mock channel 驱动全流程）**

在 `src-tauri/src/provision/jdk.rs` 追加类型骨架与测试。测试统一使用 `SequentialChannel`（按调用顺序返回预置输出）：

```rust
// ---- JdkPackage 实现（追加在纯函数部分之后）----

use crate::provision::package::{
    emit_progress, ProvisionContext, ProvisionError, ProvisionResult, StageTimeouts,
};
use async_trait::async_trait;

pub const REMOTE_TOOLS_DIR: &str = "/tmp/friday-tools";
pub const JDK_BINS: [&str; 4] = ["jcmd", "jstat", "jstack", "jmap"];

/// 按探测到的 OpenJDK 版本命名的安装目录
pub fn jdk_home_for(openjdk_version: &str) -> String {
    format!("{REMOTE_TOOLS_DIR}/jdk-{openjdk_version}")
}

pub struct JdkPackage;

#[async_trait]
impl crate::provision::package::ToolPackage for JdkPackage {
    fn name(&self) -> &str {
        "jdk"
    }

    async fn probe(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<JvmProbe, ProvisionError> {
        todo!()
    }

    async fn ensure(
        &self,
        ctx: &ProvisionContext,
        java_bin: &str,
    ) -> Result<ProvisionResult, ProvisionError> {
        todo!()
    }
}
```

（`ToolPackage` trait 定义在 Step 4 的 package.rs 中，包含 `probe` 与 `ensure` 两个方法——ensure 依赖 probe。trait 比 spec 里多了 `java_bin` 参数：JDK 包需要知道用哪个 java 探测。）

测试（追加在 jdk.rs 的 `mod tests` 中）：

```rust
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::provision::package::{ProvisionContext, StageTimeouts};
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// 顺序消费 Mock：第 n 次 run 返回第 n 条预置输出。
    /// 统一用它而非 contains 匹配——缓存检查与验证命令都以 `test -x` 开头，contains 会混淆。
    struct SequentialChannel {
        responses: TokioMutex<std::collections::VecDeque<ExecOutput>>,
        calls: TokioMutex<Vec<String>>,
    }

    impl SequentialChannel {
        fn new(responses: Vec<(&str, i32)>) -> Self {
            Self {
                responses: TokioMutex::new(
                    responses
                        .into_iter()
                        .map(|(out, code)| ExecOutput {
                            stdout: out.to_string(),
                            stderr: String::new(),
                            exit_code: code,
                        })
                        .collect(),
                ),
                calls: TokioMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl ExecChannel for SequentialChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            Ok(self.responses.lock().await.pop_front().unwrap_or(ExecOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
            }))
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, _local: &std::path::Path, _remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
    }

    fn test_ctx(channel: Arc<SequentialChannel>) -> ProvisionContext {
        ProvisionContext {
            session_id: "s1".into(),
            env_id: "env-1".into(),
            channel,
            cache_dir: std::path::PathBuf::from("/tmp/unused-cache"),
            artifactory_base_url: "https://artifactory.example.com/artifactory/release".into(),
            timeouts: StageTimeouts::default(),
            bus: crate::app::events::EventBus::disabled(),
        }
    }

    #[tokio::test]
    async fn test_ensure_cache_hit_returns_without_download() {
        // 顺序响应：probe → 缓存检查命中（exit 0）
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n", 0),
            ("", 0), // test -x 缓存命中
        ]));
        let ctx = test_ctx(channel.clone());
        let result = JdkPackage.ensure(&ctx, "java").await.unwrap();
        assert!(result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/jdk-21.0.11");
        assert_eq!(result.bins["jcmd"], "/tmp/friday-tools/jdk-21.0.11/bin/jcmd");
        assert_eq!(result.arch, "x64");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().all(|c| !c.contains("curl") && !c.contains("wget")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_download_and_extract() {
        // 完整通道 A 流程：probe → 缓存未命中 → command -v curl → curl 下载 → tar 解压 → 验证
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n", 0),
            ("", 1),             // 缓存未命中
            ("/usr/bin/curl\n", 0), // command -v curl
            ("", 0),             // curl 下载成功
            ("", 0),             // tar 解压成功
            ("", 0),             // 验证成功
        ]));
        let ctx = test_ctx(channel.clone());
        let result = JdkPackage.ensure(&ctx, "java").await.unwrap();
        assert!(!result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/jdk-21.0.11");
        let calls = channel.calls.lock().await;
        // 下载命令用 URL encode 后的目录
        assert!(calls.iter().any(|c| c.contains("BiSheng%20JDK%20Enterprise")), "calls: {calls:?}");
        // 解压命令含目录规范化 mv 与 tar 包清理
        assert!(calls.iter().any(|c| c.contains("tar -xzf")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_failure_falls_back_to_channel_b() {
        // 通道 A 失败（curl exit 1）→ 通道 B：本地下载（URL 不可达，curl.exe 失败）→ provision_failed/download_local
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n", 0),
            ("", 1),             // 缓存未命中
            ("/usr/bin/curl\n", 0), // command -v curl
            ("", 1),             // curl 下载失败
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut ctx = test_ctx(channel);
        ctx.cache_dir = cache;
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "download_local");
    }

    #[tokio::test]
    async fn test_ensure_verify_failure_reports_verify_stage() {
        // 顺序响应：probe → 缓存检查未命中 → command -v → curl 下载 → tar 解压 → 验证失败
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n", 0),
            ("", 1),
            ("/usr/bin/curl\n", 0),
            ("", 0),
            ("", 0),
            ("", 1), // 验证失败
        ]));
        let ctx = test_ctx(channel);
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "verify");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_no_curl_wget_reports_download_a() {
        // 顺序响应：probe → 缓存检查未命中 → command -v 失败（无下载器）
        // → 通道 B 本地下载（URL 不可达，curl.exe 失败）→ provision_failed/download_local
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n", 0),
            ("", 1),
            ("", 1), // 无 curl/wget
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut ctx = test_ctx(channel);
        ctx.cache_dir = cache;
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "download_local");
    }
```

- [ ] **Step 4: 实现 ToolPackage trait + JdkPackage**

`src-tauri/src/provision/package.rs` 追加 trait 定义：

```rust
use async_trait::async_trait;

/// 远程工具包：探测 + 装备。JDK 是第一个实现，arthas 等后续复用。
#[async_trait]
pub trait ToolPackage: Send + Sync {
    fn name(&self) -> &str;

    /// 探测目标环境 JVM 信息（也供外部查询）
    async fn probe(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<crate::provision::jdk::JvmProbe, ProvisionError>;

    /// 确保 package 已装备（幂等：已装备直接返回 cached=true）
    async fn ensure(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<ProvisionResult, ProvisionError>;
}
```

`src-tauri/src/provision/jdk.rs` 实现两个 `todo!()`：

```rust
#[async_trait]
impl crate::provision::package::ToolPackage for JdkPackage {
    fn name(&self) -> &str {
        "jdk"
    }

    async fn probe(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<JvmProbe, ProvisionError> {
        emit_progress(ctx, "jdk", "probe", &format!("running `{java_bin} -version`"));
        let cmd = format!("{java_bin} -version 2>&1 ; echo '---' ; uname -m");
        let out = run_remote(ctx, &cmd, Duration::from_secs(ctx.timeouts.probe), "probe").await?;
        let probe = parse_probe_output(&out.stdout, &out.stderr).map_err(|e| {
            // 解析错误分类：unsupported_vendor / parse_failed / probe_failed 前缀已含
            let code = e.split(':').next().unwrap_or("parse_failed").to_string();
            ProvisionError::new(&code, "probe", e)
        })?;
        Ok(probe)
    }

    async fn ensure(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<ProvisionResult, ProvisionError> {
        let start = std::time::Instant::now();
        let probe = self.probe(ctx, java_bin).await?;
        let home = jdk_home_for(&probe.openjdk_version);
        let tarball = format!("{REMOTE_TOOLS_DIR}/jdk-{}.tar.gz", probe.openjdk_version);

        // 1. 远端缓存检查
        emit_progress(ctx, "jdk", "check_cache", &format!("checking {home}/bin/jcmd"));
        let check = run_remote(ctx, &format!("test -x {home}/bin/jcmd"), Duration::from_secs(ctx.timeouts.probe), "check_cache").await?;
        if check.exit_code == 0 {
            return Ok(ProvisionResult {
                cached: true,
                tool_home: home,
                bins: bins_for(&home),
                elapsed_ms: start.elapsed().as_millis() as u64,
                java_version: probe.openjdk_version.clone(),
                bisheng_version: probe.bisheng_version.clone(),
                arch: probe.arch.clone(),
                tool: "jdk".to_string(),
            });
        }

        // 2. 解析 URL
        let url = build_download_url(&ctx.artifactory_base_url, &probe)
            .map_err(|e| ProvisionError::new("parse_failed", "resolve_url", e))?;

        // 3. 通道 A：目标自拉
        emit_progress(ctx, "jdk", "download", "channel A: remote curl/wget");
        let dl_ok = self.try_remote_download(ctx, &url, &tarball).await;
        if let Err(a_err) = dl_ok {
            tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, error = %a_err, "channel A failed, falling back to channel B");
            emit_progress(ctx, "jdk", "download", "channel B: local download + sftp upload");
            // 通道 B：本地下载 + 上传
            let local = crate::provision::transfer::download_to_cache(&url, &ctx.cache_dir)
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                })?;
            crate::provision::transfer::validate_download(&local, 50 * 1024 * 1024)
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                })?;
            ctx.channel.upload(&local, &tarball).await.map_err(|e| {
                // 清理远端半截文件
                let ch = ctx.channel.clone();
                let cleanup = tarball.clone();
                tokio::spawn(async move {
                    let _ = ch.run(&format!("rm -f {cleanup}")).await;
                });
                ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "upload", e.to_string())
                }
            })?;
        }

        // 4. 解压 + 目录规范化 + 清理 tar 包
        emit_progress(ctx, "jdk", "extract", &format!("extracting {tarball}"));
        let extract_cmd = format!(
            "mkdir -p {REMOTE_TOOLS_DIR} && cd {REMOTE_TOOLS_DIR} && \
             tar -xzf jdk-{v}.tar.gz && \
             topdir=$(tar -tzf jdk-{v}.tar.gz | head -1 | cut -f1 -d'/') && \
             if [ \"$topdir\" != \"jdk-{v}\" ] && [ -d \"$topdir\" ]; then mv \"$topdir\" jdk-{v}; fi && \
             rm -f jdk-{v}.tar.gz",
            v = probe.openjdk_version,
        );
        let extract = run_remote(ctx, &extract_cmd, Duration::from_secs(ctx.timeouts.extract), "extract").await?;
        if extract.exit_code != 0 {
            // 清理残留
            let ch = ctx.channel.clone();
            let cleanup_home = home.clone();
            tokio::spawn(async move {
                let _ = ch.run(&format!("rm -rf {cleanup_home}")).await;
            });
            return Err(ProvisionError {
                url: Some(url.clone()),
                ..ProvisionError::new(
                    "provision_failed",
                    "extract",
                    format!("tar failed (exit {}): {}", extract.exit_code, extract.stderr),
                )
            });
        }

        // 5. 验证
        emit_progress(ctx, "jdk", "verify", &format!("verifying {home}/bin/jcmd"));
        let verify = run_remote(
            ctx,
            &format!("test -x {home}/bin/jcmd && test -x {home}/bin/jstat"),
            Duration::from_secs(ctx.timeouts.verify),
            "verify",
        )
        .await?;
        if verify.exit_code != 0 {
            return Err(ProvisionError {
                url: Some(url.clone()),
                ..ProvisionError::new(
                    "provision_failed",
                    "verify",
                    format!("jdk binaries missing after extract; check artifactory base url setting ({})", ctx.artifactory_base_url),
                )
            });
        }

        Ok(ProvisionResult {
            cached: false,
            tool_home: home,
            bins: bins_for(&home),
            elapsed_ms: start.elapsed().as_millis() as u64,
            java_version: probe.openjdk_version,
            bisheng_version: probe.bisheng_version,
            arch: probe.arch,
            tool: "jdk".to_string(),
        })
    }
}

/// 通道 A：目标环境自拉。返回 Ok(()) 或错误描述。
async fn try_remote_download(
    ctx: &ProvisionContext,
    url: &str,
    tarball: &str,
) -> Result<(), String> {
    let which = ctx
        .channel
        .run("command -v curl || command -v wget")
        .await
        .map_err(|e| format!("probe downloader failed: {e}"))?;
    if which.exit_code != 0 {
        return Err("no curl/wget on target".to_string());
    }
    let has_curl = which.stdout.trim().contains("curl");
    let cmd = if has_curl {
        format!(
            "curl -fL --connect-timeout 15 --max-time {t} -o {tarball} {url}",
            t = ctx.timeouts.download,
        )
    } else {
        format!(
            "wget -T 15 -t 2 -O {tarball} {url}",
        )
    };
    let out = ctx
        .channel
        .run(&cmd)
        .await
        .map_err(|e| format!("remote download exec failed: {e}"))?;
    if out.exit_code != 0 {
        return Err(format!("remote download exit {}: {}", out.exit_code, out.stderr));
    }
    Ok(())
}

/// 带超时执行远端命令；超时/失败映射 ProvisionError。
async fn run_remote(
    ctx: &ProvisionContext,
    cmd: &str,
    timeout: Duration,
    stage: &str,
) -> Result<crate::exec::channel::ExecOutput, ProvisionError> {
    match tokio::time::timeout(timeout, ctx.channel.run(cmd)).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(ProvisionError::new("provision_failed", stage, format!("remote exec failed: {e}"))),
        Err(_) => Err(ProvisionError::new("provision_failed", stage, format!("remote command timed out after {}s", timeout.as_secs()))),
    }
}

fn bins_for(home: &str) -> std::collections::HashMap<String, String> {
    JDK_BINS
        .iter()
        .map(|b| (b.to_string(), format!("{home}/bin/{b}")))
        .collect()
}
```

`src-tauri/src/provision/mod.rs` 更新为：

```rust
pub mod jdk;
pub mod package;
pub mod transfer;

/// 包注册表：name → package。JDK 是第一个；arthas 等后续追加。
pub fn builtin_packages() -> Vec<std::sync::Arc<dyn package::ToolPackage>> {
    vec![std::sync::Arc::new(jdk::JdkPackage)]
}
```

**实现细节（执行者注意）：**
- probe 的命令 `{java_bin} -version 2>&1 ; echo '---' ; uname -m` —— `2>&1` 把 stderr 合并进 stdout，`parse_probe_output` 的 combined 扫描逻辑两路都能覆盖。**但 `test_parse_probe_output_bisheng_on_stderr` 单测传的是分立的 stdout/stderr**——`parse_probe_output` 本身就支持分立输入（参数就是两路），远端命令只是保证实际拿到时两路都有内容。
- 通道 A 的 curl 命令 URL 不加引号（URL 无空格，含 `%` 在 bash 中无引号也安全——`%` 不是 bash 特殊字符）。wget 同理。
- 通道 A 下载的 curl 命令**没有包超时**（`try_remote_download` 内直接 `ctx.channel.run`）——`--max-time {t}` 参数本身就是远端 curl 的超时；wget 用 `-T 15 -t 2`。SSH channel 层有 russh 600s inactivity timeout 兜底。
- SequentialChannel 测试中，通道 A 失败用例（`test_ensure_channel_a_failure_falls_back_to_channel_b`）的预期命令序列为：probe(0) → 缓存检查(1) → command -v(0) → curl(1 失败) → 【通道 B 本地下载失败】→ 错误返回。响应序列给 4 条即可，剩余响应不够时默认 exit 1。
- `test_ensure_channel_a_failure_falls_back_to_channel_b` 与 `test_ensure_channel_a_no_curl_wget_reports_download_a` 中本地下载 URL 是 `https://artifactory.example.com/...`（不可达），curl.exe 会失败——`download_to_cache` 返回 Err → 映射为 `provision_failed`/`download_local`。测试预期成立。`test_ensure_cache_hit_returns_without_download` 与 `test_ensure_channel_a_download_and_extract` 的 `cache_dir` 是 `/tmp/unused-cache`（不会走到本地下载）。
- jdk.rs 顶部需要 `use std::time::Duration;`（probe/ensure 的 `Duration::from_secs`）——骨架代码里的 import 区补上。
- `emit_progress` 在 `ProvisionContext.bus` 为 disabled 时只走 tracing 日志（Task 7 Step 2 的 EventBus 改造），测试不依赖事件副作用。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provision`
Expected: jdk + transfer + package 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/provision src-tauri/src/app/events.rs
git commit -m "feat: ToolPackage trait and JdkPackage ensure flow (dual-channel download)"
```

---

## Task 8: ProvisionProgress 事件前端消费 + events 序列化测试

**Files:**
- Modify: `src-tauri/src/app/events.rs`（测试）
- Modify: `src/lib/types.ts`
- Modify: `src/store/sessionStore.ts`

（`ProvisionProgress` 变体本体在 Task 7 已加入 events.rs。）

- [ ] **Step 1: 写失败测试（Rust 序列化）**

`src-tauri/src/app/events.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn test_provision_progress_serialization() {
        let event = AppEvent::ProvisionProgress {
            session_id: "s1".to_string(),
            tool: "jdk".to_string(),
            stage: "download".to_string(),
            detail: "channel B".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("provision_progress"));
        assert!(json.contains("jdk"));
        assert!(json.contains("download"));
    }
```

- [ ] **Step 2: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml app::events`
Expected: PASS（Task 7 已加变体；若失败说明变体没加对，修正）

- [ ] **Step 3: 前端类型 + store 分支**

`src/lib/types.ts` 的 `AppEvent` union 中 `session_deleted` 之前追加：

```ts
  | { type: "provision_progress"; session_id: string; tool: string; stage: string; detail: string }
```

`src/store/sessionStore.ts` `handleEvent` 中，`if (event.type === "tool_result")` 分支之前追加：

```ts
    if (event.type === "provision_progress") {
      // 装备进度：附加到最近一个 running 的同名工具卡片（状态行文本）
      const messages = state.messagesBySession[session_id] ?? [];
      if (messages.length === 0) return;
      const lastIdx = messages.length - 1;
      const lastMsg = messages[lastIdx];
      if (lastMsg.role !== "agent") return;

      const updatedParts = [...lastMsg.parts];
      for (let i = updatedParts.length - 1; i >= 0; i--) {
        const part = updatedParts[i];
        if (part.type === "tool" && part.tool && part.tool.name === event.tool && part.tool.status === "running") {
          updatedParts[i] = {
            ...part,
            tool: {
              ...part.tool,
              // 复用 output 字段展示当前阶段（tool_result 到达后会被覆盖）
              output: `${event.stage}: ${event.detail}`,
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
```

- [ ] **Step 4: 前端类型检查**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/app/events.rs src/lib/types.ts src/store/sessionStore.ts
git commit -m "feat: provision_progress event consumed by tool card status line"
```

---

## Task 9: ensure_tool MCP 工具

**Files:**
- Create: `src-tauri/src/tools/builtin/ensure_tool.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`
- Modify: `src-tauri/src/lib.rs`（注册工具）

- [ ] **Step 1: 写失败测试**

新建 `src-tauri/src/tools/builtin/ensure_tool.rs`：

```rust
use crate::provision::package::{ProvisionContext, StageTimeouts};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct EnsureToolHandler {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub cache_dir: std::path::PathBuf,
    pub bus: crate::app::events::EventBus,
    /// (env_id, package_name) → 串行化锁
    pub inflight: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[async_trait]
impl ToolHandler for EnsureToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        todo!()
    }
}

pub fn ensure_tool_tool_def(
    db: sqlx::SqlitePool,
    exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    cache_dir: std::path::PathBuf,
    bus: crate::app::events::EventBus,
) -> ToolDef {
    ToolDef {
        name: "ensure_tool".to_string(),
        description: "确保目标环境已装备指定诊断工具包（当前支持 jdk）。生产环境通常只有 JRE，缺少 jstat/jcmd 等诊断工具；本工具探测目标 JVM 版本并下载匹配的 JDK 到 /tmp/friday-tools（不影响系统 Java）。返回 tool_home 及各工具完整路径，后续请用全路径调用（如 /tmp/friday-tools/jdk-21.0.11/bin/jcmd <pid> GC.heap_info）。重复调用安全：已装备时直接返回。在 JVM 诊断前调用一次。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "tool": { "type": "string", "enum": ["jdk"], "description": "要装备的工具包名" },
                "java_bin": { "type": "string", "description": "目标服务使用的 java 可执行文件路径，默认 java（多版本共存时从服务进程命令行确认后传入）" }
            },
            "required": ["environment", "tool"]
        }),
        risk_level: RiskLevel::Low,
        needs_channel: false,
        handler: Arc::new(EnsureToolHandler {
            db,
            exec_pool,
            cache_dir,
            bus,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }),
    }
}

fn error_output(error: &str, message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": error, "message": message }),
        raw_stdout: None,
    }
}
```

测试（同文件 `mod tests`）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};

    struct ProbeOkChannel;

    #[async_trait]
    impl ExecChannel for ProbeOkChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput {
                stdout: "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n".into(),
                stderr: "openjdk version \"21.0.11\" 2025-04-15\n".into(),
                exit_code: 0,
            })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, _local: &std::path::Path, _remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
    }

    async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool, Arc<Mutex<crate::exec::pool::ExecChannelPool>>, std::path::PathBuf, crate::app::events::EventBus) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None)
            .await.unwrap();
        let exec_pool = Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        (tmp, db, exec_pool, cache, crate::app::events::EventBus::disabled())
    }

    fn make_handler(
        db: sqlx::SqlitePool,
        exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
        cache: std::path::PathBuf,
        bus: crate::app::events::EventBus,
    ) -> EnsureToolHandler {
        EnsureToolHandler {
            db,
            exec_pool,
            cache_dir: cache,
            bus,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn test_missing_environment_param() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"tool": "jdk"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_tool_package() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "tool": "arthas"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        assert!(out.data["message"].as_str().unwrap().contains("jdk"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment_guides_agent() {
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "nope", "tool": "jdk"}), &ctx).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        assert!(out.data["message"].as_str().unwrap().contains("list_environments"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_probe_only_cache_hit_flow() {
        // 探测成功 + 缓存检查命中 → 返回 cached:true（ProbeOkChannel 对所有命令返回探测输出，
        // test -x 命令也会拿到同一输出且 exit 0 → 缓存命中分支）
        let (tmp, db, exec_pool, cache, bus) = setup().await;
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        exec_pool.lock().await.insert_channel(env_id, Arc::new(ProbeOkChannel) as Arc<dyn ExecChannel>).await;
        let handler = make_handler(db, exec_pool, cache, bus);
        let ctx = ToolContext { session_id: "s1".into(), channel: None };
        let out = handler.execute(serde_json::json!({"environment": "prod", "tool": "jdk"}), &ctx).await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["cached"], true);
        assert_eq!(out.data["tool_home"], "/tmp/friday-tools/jdk-21.0.11");
        assert_eq!(out.data["bins"]["jcmd"], "/tmp/friday-tools/jdk-21.0.11/bin/jcmd");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let def = ensure_tool_tool_def(
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new())),
            std::path::PathBuf::from("/tmp/x"),
            crate::app::events::EventBus::disabled(),
        );
        assert_eq!(def.name, "ensure_tool");
        assert_eq!(def.risk_level, RiskLevel::Low);
        assert!(!def.needs_channel);
    }
}
```

在 `src-tauri/src/tools/builtin/mod.rs` 加 `pub mod ensure_tool;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::ensure_tool`
Expected: FAIL（todo!() panic）

- [ ] **Step 3: 实现 handler**

```rust
#[async_trait]
impl ToolHandler for EnsureToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: environment");
        };
        let Some(tool) = args.get("tool").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: tool");
        };
        let java_bin = args.get("java_bin").and_then(|v| v.as_str()).unwrap_or("java");

        if tool != "jdk" {
            return error_output(
                "invalid_params",
                &format!("unknown tool package: {tool:?}. supported packages: jdk"),
            );
        }

        // 按名称查环境
        let env = match crate::app::environments::find_by_name(&self.db, environment).await {
            Ok(Some(env)) => env,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。",
                );
            }
            Err(e) => return error_output("lookup_failed", &format!("查询环境失败: {e}")),
        };

        // 获取 channel
        let channel = {
            let mut pool = self.exec_pool.lock().await;
            match pool.get_or_create(&env.id, &self.db).await {
                Ok(ch) => ch,
                Err(e) => {
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "ensure_tool: failed to get exec channel");
                    return error_output("connection_error", &format!("{e} (host: {})", env.host));
                }
            }
        };

        let base_url = match crate::app::settings::artifactory_base_url(&self.db).await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "ensure_tool: read artifactory base url failed");
                return error_output("internal_error", &format!("读取 Artifactory 设置失败: {e}"));
            }
        };

        let bus = self.bus.clone();
        let pctx = ProvisionContext {
            session_id: ctx.session_id.clone(),
            env_id: env.id.clone(),
            channel,
            cache_dir: self.cache_dir.clone(),
            artifactory_base_url: base_url,
            timeouts: StageTimeouts::default(),
            bus,
        };

        // (env_id, package) 串行化：并发请求排队，后者进锁后重新查缓存
        let lock_key = format!("{}/{}", env.id, tool);
        let per_key = {
            let mut inflight = self.inflight.lock().await;
            inflight
                .entry(lock_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = per_key.lock().await;

        let package = crate::provision::jdk::JdkPackage;
        match package.ensure(&pctx, java_bin).await {
            Ok(result) => {
                tracing::info!(session_id = %ctx.session_id, env_id = %env.id, tool, cached = result.cached, elapsed_ms = result.elapsed_ms, "ensure_tool succeeded");
                ToolOutput {
                    success: true,
                    data: serde_json::to_value(&result).unwrap_or_default(),
                    raw_stdout: None,
                }
            }
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, tool, code = %e.code, stage = %e.stage, error = %e.message, "ensure_tool failed");
                let mut data = serde_json::json!({
                    "error": e.code,
                    "stage": e.stage,
                    "message": e.message,
                });
                if let Some(url) = &e.url {
                    data["url"] = serde_json::json!(url);
                }
                ToolOutput { success: false, data, raw_stdout: None }
            }
        }
    }
}
```

（bus 已在 handler struct 中，`ensure_tool_tool_def` 构造时注入；测试中传 `EventBus::disabled()`。）

- [ ] **Step 4: 注册进 lib.rs**

`src-tauri/src/lib.rs` 中，`tool_registry.register(crate::tools::builtin::list_environments::list_environments_tool_def(pool.clone()));` 之后追加：

```rust
            tool_registry.register(crate::tools::builtin::ensure_tool::ensure_tool_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.cache_dir(),
                EventBus::new(handle.clone()),
            ));
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::ensure_tool`
Expected: 5 个测试全部 PASS

- [ ] **Step 6: 跑全量测试防回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/tools/builtin/ensure_tool.rs src-tauri/src/tools/builtin/mod.rs src-tauri/src/lib.rs
git commit -m "feat: ensure_tool MCP tool with jdk package and per-env serialization"
```

---

## Task 10: System prompt 引导

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/agent/prompt.rs` 的 `mod tests` 中追加：

```rust
    #[test]
    fn test_tool_guidance_mentions_ensure_tool() {
        assert!(TOOL_GUIDANCE.contains("ensure_tool"));
        assert!(TOOL_GUIDANCE.contains("jstat"));
    }

    #[test]
    fn test_build_prompt_contains_ensure_tool_guidance() {
        let prompt = build_prompt("帮我看看 OOM", None, "s1");
        assert!(prompt.contains("ensure_tool"));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt`
Expected: FAIL（TOOL_GUIDANCE 无 ensure_tool）

- [ ] **Step 3: 实现**

`TOOL_GUIDANCE` 常量改为：

```rust
const TOOL_GUIDANCE: &str = "## 工具使用
- 调用诊断工具时，必须传入 session_id 参数。
- 远程命令一律通过 run_command 工具执行，并用 environment 参数指定目标环境（name 来自 list_environments）。
- 优先使用结构化诊断工具，run_command 是兜底。
- 诊断 JVM 相关问题（OOM、GC、线程、CPU 飙高等）时，先调用 ensure_tool 装备 JDK，再用返回的 bins 全路径通过 run_command 执行 jstat/jcmd 等工具（目标环境通常只有 JRE，直接执行 jstat 会失败）。
- 用户提到的环境先与 list_environments 的结果匹配；没有匹配时引导用户在右侧「环境」面板添加，不要瞎猜 host。";
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: system prompt guidance for ensure_tool before JVM diagnostics"
```

---

## Task 11: 前端设置项 — Artifactory base URL

**Files:**
- Modify: `src-tauri/src/app/settings.rs`（Tauri commands）
- Modify: `src-tauri/src/lib.rs`（命令注册）
- Modify: `src/lib/types.ts`、`src/lib/ipc.ts`
- Create: `src/store/settingsStore.ts`
- Modify: `src/components/agents/AgentSettingsDialog.tsx`

- [ ] **Step 1: 后端 commands**

`src-tauri/src/app/settings.rs` 追加（测试之后、`mod tests` 之前）：

```rust
use tauri::State;

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_artifactory_base_url_cmd(state: State<'_, crate::AppState>) -> Result<String, String> {
    tracing::info!("get_artifactory_base_url_cmd called");
    artifactory_base_url(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_artifactory_base_url_cmd(
    state: State<'_, crate::AppState>,
    url: String,
) -> Result<(), String> {
    tracing::info!(url = %url, "set_artifactory_base_url_cmd called");
    let url = url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err("base url cannot be empty".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("base url must start with http:// or https://".to_string());
    }
    set_setting(&state.db, KEY_ARTIFACTORY_BASE_URL, &url)
        .await
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 2: 注册命令**

`src-tauri/src/lib.rs` 的 `generate_handler![...]` 中，`app::environments::test_connection_params_cmd,` 之后追加：

```rust
            app::settings::get_artifactory_base_url_cmd,
            app::settings::set_artifactory_base_url_cmd,
```

- [ ] **Step 3: cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过

- [ ] **Step 4: 前端 ipc + store**

`src/lib/ipc.ts` 末尾追加：

```ts
export async function getArtifactoryBaseUrl(): Promise<string> {
  return invoke<string>("get_artifactory_base_url_cmd");
}

export async function setArtifactoryBaseUrl(url: string): Promise<void> {
  return invoke<void>("set_artifactory_base_url_cmd", { url });
}
```

新建 `src/store/settingsStore.ts`：

```ts
import { create } from "zustand";
import { getArtifactoryBaseUrl, setArtifactoryBaseUrl } from "@/lib/ipc";

interface SettingsStore {
  artifactoryBaseUrl: string;
  loading: boolean;
  saving: boolean;
  error: string | null;
  load: () => Promise<void>;
  saveBaseUrl: (url: string) => Promise<boolean>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
  artifactoryBaseUrl: "",
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const url = await getArtifactoryBaseUrl();
      set({ artifactoryBaseUrl: url });
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  saveBaseUrl: async (url) => {
    set({ saving: true, error: null });
    try {
      await setArtifactoryBaseUrl(url);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },
}));
```

- [ ] **Step 5: AgentSettingsDialog 加设置区**

`src/components/agents/AgentSettingsDialog.tsx` 中，"Manual add (collapsible)" 区块（`{/* Manual add (collapsible) */}`）之前插入新区块：

```tsx
        {/* Artifactory base URL (JDK provisioning) */}
        <div className="border-t border-border shrink-0">
          <div className="px-5 py-3 space-y-2">
            <label htmlFor="artifactory-url" className="text-sm text-foreground">
              Artifactory 仓库地址
            </label>
            <p className="text-xs text-muted-foreground">
              用于 ensure_tool 下载 JDK 诊断工具包到目标环境（/tmp/friday-tools）
            </p>
            <div className="flex items-center gap-2">
              <input
                id="artifactory-url"
                type="text"
                value={urlDraft}
                onChange={(e) => setUrlDraft(e.target.value)}
                placeholder="https://…/artifactory/cmc-software-release"
                className="flex-1 bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none"
                style={{ fontFamily: "var(--font-mono)" }}
              />
              <button
                onClick={handleSaveUrl}
                disabled={savingUrl || urlDraft.trim() === artifactoryBaseUrl}
                className="px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
              >
                {savingUrl ? "保存中..." : "保存"}
              </button>
            </div>
            {settingsError && (
              <p className="text-xs text-destructive break-words">{settingsError}</p>
            )}
          </div>
        </div>
```

组件顶部追加状态与逻辑：

```tsx
  const artifactoryBaseUrl = useSettingsStore((s) => s.artifactoryBaseUrl);
  const settingsError = useSettingsStore((s) => s.error);
  const saveBaseUrl = useSettingsStore((s) => s.saveBaseUrl);
  const loadSettings = useSettingsStore((s) => s.load);

  const [urlDraft, setUrlDraft] = useState("");
  const [savingUrl, setSavingUrl] = useState(false);

  useEffect(() => {
    if (open) {
      loadSettings().then(() => {
        setUrlDraft(useSettingsStore.getState().artifactoryBaseUrl);
      });
    }
  }, [open, loadSettings]);

  const handleSaveUrl = async () => {
    const trimmed = urlDraft.trim();
    if (!trimmed || savingUrl) return;
    setSavingUrl(true);
    try {
      await saveBaseUrl(trimmed);
    } finally {
      setSavingUrl(false);
    }
  };
```

import 部分追加 `import { useSettingsStore } from "@/store/settingsStore";`。

- [ ] **Step 6: 前端类型检查**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/app/settings.rs src-tauri/src/lib.rs src/lib/ipc.ts src/store/settingsStore.ts src/components/agents/AgentSettingsDialog.tsx
git commit -m "feat: artifactory base url setting in agent settings dialog"
```

---

## Task 12: 全量验证收口

**Files:** 无新文件

- [ ] **Step 1: Rust 全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（无 ignore/skip）

- [ ] **Step 2: cargo check**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无 warning 新增（重点看未使用 import）

- [ ] **Step 3: 前端类型检查**

Run: `pnpm typecheck`
Expected: PASS

- [ ] **Step 4: spec 对照检查（人工）**

对照 [spec](../specs/2026-08-27-remote-tool-provisioning-design.md) 核对：
- 决策表 11 条逐一有对应实现（1→Task 7 trait；2→Task 7 双通道；3/4→Task 5 解析；5/6→Task 7 远端缓存+Task 6 本地缓存；7→Task 9 ensure_tool；8→Task 9 RiskLevel::Low；9→无状态表；10→Task 3/4 upload；11→Task 5 unsupported_vendor）
- 错误处理表 9 行的 code/stage 与实现一致
- 返回结构 JSON 字段与 ProvisionResult 序列化一致（tool/cached/java_version/bisheng_version/arch/tool_home/bins/elapsed_ms）

- [ ] **Step 5: 手工冒烟（可选，需真实环境）**

`pnpm tauri dev` 启动后：
1. 设置弹窗确认 Artifactory 地址正确显示默认值
2. 添加一个可达的测试环境，发起诊断"帮我确认这个环境的 JVM 版本"
3. agent 调用 ensure_tool → 确认 Low 风险卡片出现 → 批准 → ToolCallCard 显示阶段推进
4. 二次调用 ensure_tool 应显示 cached: true

- [ ] **Step 6: 最终 Commit（如有收口改动）**

```bash
git add -A
git commit -m "chore: provisioning feature final verification pass"
```

---

## 备注：测试中的 EventBus

`EventBus::disabled()`（Task 7 引入）让 provision 全链路可以在无 Tauri 窗口的单测中跑通。`lib.rs` / `mcp/transport.rs` 的 `EventBus::new(...)` 调用点不受影响（`new` 签名未变）。
