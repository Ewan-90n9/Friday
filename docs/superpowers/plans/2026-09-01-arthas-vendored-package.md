# arthas 包 vendoring 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** arthas 工具包随应用分发（resources/arthas），运行时 SFTP 直传目标机，移除 artifactory 下载依赖（issue #7）。

**Architecture:** 对齐 analyzer JAR vendoring 模式：CI 下载 zip → tauri resources 打包 → lib.rs resource_dir 解析 → ProvisionContext 传递 → ArthasPackage::ensure 直接 upload + 解压（原 channel B 路径），删除 channel A 与本地下载。

**Tech Stack:** Rust (ssh2 SFTP upload) / PowerShell fetch 脚本 / Tauri resources / GitHub Actions。

**约定：**
- Rust 检查/测试：`cargo check --manifest-path src-tauri/Cargo.toml` / `cargo test --manifest-path src-tauri/Cargo.toml`
- 日志规范：错误路径 tracing::error!/warn!，进度事件 stage/detail 语义不变
- 工作区：main 分支直接干
- spec：docs/superpowers/specs/2026-09-01-arthas-vendored-package-design.md

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `scripts/fetch-arthas.ps1` | 新建 | 构建期下载 arthas zip 到 resources（幂等） |
| `src-tauri/tauri.conf.json` | 修改 | resources 加 `resources/arthas/*` |
| `.github/workflows/release.yml` | 修改 | 加 fetch-arthas 步骤 |
| `src-tauri/src/provision/package.rs` | 修改 | ProvisionContext 加 `arthas_zip: Option<PathBuf>` 字段 |
| `src-tauri/src/provision/arthas.rs` | 重写 ensure | vendored zip 校验 + SFTP 上传；删 download_url/channel A/B；测试重写 |
| `src-tauri/src/arthas/attach.rs` | 修改 | AttachDeps 加 `arthas_zip`；provision_context 传递 |
| `src-tauri/src/lib.rs` | 修改 | resource_dir 解析 arthas zip（analyzer 同款双候选） |
| `src-tauri/src/provision/jdk.rs`（仅测试） | 修改 | test_ctx 补 `arthas_zip: None` 字段 |
| `src-tauri/src/tools/builtin/ensure_tool.rs`（仅测试） | 修改 | 测试用 ProvisionContext 构造补字段 |
| `.gitignore` | 检查 | 确认 resources/analyzer 忽略模式是否同样适用 arthas（resources 不入库，只 CI 下载） |

---

### Task 1: ProvisionContext.arthas_zip 字段传递链（TDD）

**Files:**
- Modify: `src-tauri/src/provision/package.rs:23-31`
- Modify: `src-tauri/src/arthas/attach.rs:125-132, 300-322`
- Modify: `src-tauri/src/lib.rs:135-147`
- Modify（测试构造点）: `src-tauri/src/provision/jdk.rs:662-672`、`src-tauri/src/tools/builtin/ensure_tool.rs`（grep `ProvisionContext {` 找全）

- [ ] **Step 1.1: 加字段**（编译驱动——结构体字段缺省值无法构造，编译错误即 RED）

`package.rs` ProvisionContext 加：

```rust
pub struct ProvisionContext {
    pub session_id: String,
    pub env_id: String,
    pub channel: Arc<dyn ExecChannel>,
    pub cache_dir: std::path::PathBuf,
    pub artifactory_base_url: String,
    /// vendored arthas zip（随应用分发）；None = 未随包分发，arthas ensure 时报结构化错误
    pub arthas_zip: Option<std::path::PathBuf>,
    pub timeouts: StageTimeouts,
    pub bus: EventBus,
}
```

所有 `ProvisionContext {` 构造点补 `arthas_zip: None`（jdk.rs test_ctx、ensure_tool.rs 测试、attach.rs provision_context——先占位 None，Step 1.3 接真值）。

- [ ] **Step 1.2: 跑编译确认构造点全部补齐**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过（若有遗漏构造点会报 missing field）

- [ ] **Step 1.3: 接真值**

`attach.rs` AttachDeps 加字段：

```rust
pub struct AttachDeps {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    pub jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
    pub cache_dir: PathBuf,
    pub arthas_zip: Option<PathBuf>,
    pub bus: EventBus,
}
```

`provision_context()`：

```rust
Ok(crate::provision::package::ProvisionContext {
    session_id: req.session_id.clone(),
    env_id: req.env_id.clone(),
    channel,
    cache_dir: deps.cache_dir.clone(),
    artifactory_base_url: base,
    arthas_zip: deps.arthas_zip.clone(),
    timeouts: crate::provision::package::StageTimeouts::default(),
    bus: deps.bus.clone(),
})
```

`lib.rs`（analyzer JAR 解析之后、AttachDeps 构造之前）：

```rust
// arthas 工具包（vendored zip 随包分发，attach 时 SFTP 直传目标机）
let arthas_zip = resource_dir.as_ref().and_then(|r| {
    let candidates = [
        r.join("resources").join("arthas").join(format!("arthas-bin-{}.zip", crate::provision::arthas::ARTHAS_VERSION)),
        r.join("arthas").join(format!("arthas-bin-{}.zip", crate::provision::arthas::ARTHAS_VERSION)),
    ];
    candidates.into_iter().find(|p| p.exists())
});
if arthas_zip.is_none() {
    tracing::warn!(
        "arthas package missing (resources/arthas/arthas-bin-{}.zip); arthas attach will report vendored_package_missing",
        crate::provision::arthas::ARTHAS_VERSION
    );
}
```

AttachDeps 构造（lib.rs:136-143）加 `arthas_zip,`。

- [ ] **Step 1.4: 编译 + 测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml` → 通过
Run: `cargo test --manifest-path src-tauri/Cargo.toml` → 507 passed（attach.rs 若有 AttachDeps 测试构造点同样补字段——grep 确认）

- [ ] **Step 1.5: 提交**

```bash
git add src-tauri/src/provision/package.rs src-tauri/src/arthas/attach.rs src-tauri/src/lib.rs src-tauri/src/provision/jdk.rs src-tauri/src/tools/builtin/ensure_tool.rs
git commit -m "feat: provision context carries vendored arthas zip path"
```

---

### Task 2: ArthasPackage::ensure 改造为 vendored 直传（TDD）

**Files:**
- Rewrite: `src-tauri/src/provision/arthas.rs`（ensure 主体 + 测试模块）

- [ ] **Step 2.1: 写失败测试**

替换 arthas.rs 测试模块为（SequentialChannel 需记录 upload 调用——jdk.rs 的 stub 不记录，本文件自带增强版 stub）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::provision::package::{ProvisionContext, StageTimeouts};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// 记录 run + upload 调用的 ExecChannel stub
    #[derive(Default)]
    struct RecordingChannel {
        calls: TokioMutex<Vec<String>>,
        uploads: TokioMutex<Vec<(String, String)>>, // (local, remote)
        responses: TokioMutex<VecDeque<ExecOutput>>,
    }

    impl RecordingChannel {
        fn new(responses: Vec<(&str, i32)>) -> Arc<Self> {
            let dq = responses
                .into_iter()
                .map(|(o, c)| ExecOutput { stdout: o.to_string(), stderr: String::new(), exit_code: c })
                .collect();
            Arc::new(Self {
                calls: TokioMutex::new(Vec::new()),
                uploads: TokioMutex::new(Vec::new()),
                responses: TokioMutex::new(dq),
            })
        }
    }

    #[async_trait::async_trait]
    impl ExecChannel for RecordingChannel {
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
        async fn upload(&self, local: &std::path::Path, remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.uploads.lock().await.push((local.display().to_string(), remote.to_string()));
            Ok(())
        }
    }

    fn test_ctx(channel: Arc<RecordingChannel>, arthas_zip: Option<PathBuf>) -> ProvisionContext {
        ProvisionContext {
            session_id: "s1".into(),
            env_id: "env-1".into(),
            channel,
            cache_dir: PathBuf::from("/tmp/unused-cache"),
            artifactory_base_url: "https://artifactory.example.com/artifactory/release".into(),
            arthas_zip,
            timeouts: StageTimeouts::default(),
            bus: crate::app::events::EventBus::disabled(),
        }
    }

    fn make_zip(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("arthas-bin-4.3.5.zip");
        std::fs::write(&p, vec![0u8; 6 * 1024 * 1024]).unwrap();
        p
    }

    #[tokio::test]
    async fn test_ensure_cache_hit_skips_upload() {
        let channel = RecordingChannel::new(vec![
            ("", 0), // test -f arthas-boot.jar 缓存命中
        ]);
        let ctx = test_ctx(channel.clone(), None);
        let result = ArthasPackage.ensure(&ctx, "java").await.unwrap();
        assert!(result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/arthas-4.3.5");
        assert!(channel.uploads.lock().await.is_empty(), "cache hit must not upload");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().all(|c| !c.contains("unzip") && !c.contains("python3")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_missing_zip_reports_structured_error() {
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
        ]);
        let ctx = test_ctx(channel.clone(), None);
        let err = ArthasPackage.ensure(&ctx, "java").await.unwrap_err();
        assert!(err.stage == "vendored_package" || err.to_string().contains("未随应用分发"), "err: {err}");
        assert!(channel.uploads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_ensure_corrupt_zip_reports_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("arthas-bin-4.3.5.zip");
        std::fs::write(&bad, vec![0u8; 1024]).unwrap(); // 太小
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
        ]);
        let ctx = test_ctx(channel.clone(), Some(bad));
        let err = ArthasPackage.ensure(&ctx, "java").await.unwrap_err();
        assert!(err.to_string().contains("arthas") || err.stage == "vendored_package", "err: {err}");
        assert!(channel.uploads.lock().await.is_empty(), "corrupt zip must not upload");
    }

    #[tokio::test]
    async fn test_ensure_uploads_and_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = make_zip(tmp.path());
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
            ("", 0), // 解压成功
            ("", 0), // 验证成功
        ]);
        let ctx = test_ctx(channel.clone(), Some(zip.clone()));
        let result = ArthasPackage.ensure(&ctx, "java").await.unwrap();
        assert!(!result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/arthas-4.3.5");
        let uploads = channel.uploads.lock().await;
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, zip.display().to_string());
        assert_eq!(uploads[0].1, "/tmp/friday-tools/arthas-bin-4.3.5.zip");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().any(|c| c.contains("unzip -q -o arthas-bin-4.3.5.zip")), "calls: {calls:?}");
        assert!(calls.iter().any(|c| c.contains("arthas-boot.jar")), "find arthas-boot.jar: {calls:?}");
        // 不再有任何 artifactory 下载
        assert!(calls.iter().all(|c| !c.contains("curl") && !c.contains("wget")), "calls: {calls:?}");
    }
}
```

注意 `ProvisionError` 需有 `stage` 公开字段（package.rs 现有定义——确认；若字段名不同以实际为准调整断言）。

- [ ] **Step 2.2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas`
Expected: 新测试 FAIL（当前 ensure 走 channel A/B 下载路径，行为不符）；旧测试 `test_download_url` 编译失败（函数将被删）。观察具体失败。

- [ ] **Step 2.3: 重写 ensure**

ensure 主体改为（保留缓存检查/解压/验证三段不变，替换下载段）：

```rust
async fn ensure(&self, ctx: &ProvisionContext, _java_bin: &str) -> Result<ProvisionResult, ProvisionError> {
    let start = std::time::Instant::now();
    let home = arthas_home();

    // 1. 远端缓存检查（不变）
    emit_progress(ctx, ARTHAS_TOOL_NAME, "check_cache", &format!("checking {home}/arthas-boot.jar"));
    let check = run_remote(ctx, &format!("mkdir -p {REMOTE_TOOLS_DIR} && test -f {home}/arthas-boot.jar"), Duration::from_secs(ctx.timeouts.probe), "check_cache").await?;
    if check.exit_code == 0 { /* ...原 cached 返回不变... */ }

    // 2. vendored zip：随应用分发的包 SFTP 直传目标机（不再依赖 artifactory）
    let zip = ctx.arthas_zip.as_ref().ok_or_else(|| ProvisionError::new(
        "vendored_package_missing",
        "vendored_package",
        format!("arthas 工具包未随应用分发（resources/arthas/arthas-bin-{ARTHAS_VERSION}.zip），请重新安装 Friday"),
    ))?;
    if let Err(e) = crate::provision::transfer::validate_download(zip, 5 * 1024 * 1024) {
        return Err(ProvisionError::new("vendored_package_corrupt", "vendored_package", e));
    }
    let remote_zip = format!("{REMOTE_TOOLS_DIR}/arthas-bin-{ARTHAS_VERSION}.zip");
    emit_progress(ctx, ARTHAS_TOOL_NAME, "upload", &format!("uploading arthas-bin-{ARTHAS_VERSION}.zip via sftp"));
    ctx.channel.upload(zip, &remote_zip).await.map_err(|e| ProvisionError::new("provision_failed", "upload", e.to_string()))?;

    // 3./4. 解压 + 验证（原逻辑逐字保留：extract_cmd、失败清理 spawn、verify）
    // ...
}
```

删除：`arthas_download_url()`、channel A（try_remote_download 调用）、channel B 本地下载段、`use crate::provision::jdk::try_remote_download` import（`run_remote`/`JvmProbe`/`REMOTE_TOOLS_DIR` 保留）。删除旧测试 `test_download_url`。

- [ ] **Step 2.4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas` → 4 个新测试 PASS
Run: `cargo test --manifest-path src-tauri/Cargo.toml` → 全绿

- [ ] **Step 2.5: 提交**

```bash
git add src-tauri/src/provision/arthas.rs
git commit -m "feat: arthas provision uploads vendored zip via sftp, drops artifactory download"
```

---

### Task 3: 构建链——fetch 脚本 + tauri resources + CI

**Files:**
- Create: `scripts/fetch-arthas.ps1`
- Modify: `src-tauri/tauri.conf.json`（resources 数组）
- Modify: `.github/workflows/release.yml`（analyzer 步骤后加一步）
- 检查: `.gitignore`

- [ ] **Step 3.1: fetch-arthas.ps1**（对齐 fetch-analyzer-jar.ps1 结构）

```powershell
param(
    [string]$Version = "4.3.5"
)
$ErrorActionPreference = "Stop"
$url = "https://github.com/alibaba/arthas/releases/download/arthas-spring-boot-starter-$Version/arthas-bin.zip"
```

注意：官方 release tag 命名随版本变化，实现时先 `gh release list -R alibaba/arthas --limit 10` 或查 https://github.com/alibaba/arthas/releases 确认 4.3.5 的准确 asset URL（可能是 `https://github.com/alibaba/arthas/releases/download/v4.3.5/arthas-bin.zip` 之类）。下载到 `src-tauri/resources/arthas/arthas-bin-4.3.5.zip`，幂等（存在即跳过），.downloading 临时文件 + Move。若 4.3.5 无对应 bin asset，选最接近的官方版本并同步改 `ARTHAS_VERSION` 常量（保持 zip 文件名与常量一致）。

- [ ] **Step 3.2: tauri.conf.json resources** 加 `"resources/arthas/*"`（紧邻 `"resources/analyzer/*"`）。

- [ ] **Step 3.3: release.yml** 在 "Download heap analyzer JAR" 步骤后加：

```yaml
      - name: Download arthas package
        shell: pwsh
        run: ./scripts/fetch-arthas.ps1
```

- [ ] **Step 3.4: .gitignore 检查**：确认 `src-tauri/resources/analyzer` 的忽略模式（若有）同样覆盖 `src-tauri/resources/arthas`（resources 产物不入库，CI 每次下载）。若无现有忽略规则则 resources 目录本来就没被跟踪，无需改动——用 `git status` 验证下载后不出现 untracked。

- [ ] **Step 3.5: 本地验证脚本可用**

Run: `./scripts/fetch-arthas.ps1`
Expected: 下载成功且 `src-tauri/resources/arthas/arthas-bin-4.3.5.zip` 存在（约 15MB）；再跑一次 → "already present" 跳过。

- [ ] **Step 3.6: 提交**

```bash
git add scripts/fetch-arthas.ps1 src-tauri/tauri.conf.json .github/workflows/release.yml
git commit -m "build: vendor arthas package into installer resources"
```

---

### Task 4: 回归 + 文档收尾

- [ ] **Step 4.1: 全量验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml` → 通过（无新增 warning）
Run: `cargo test --manifest-path src-tauri/Cargo.toml` → 全绿
Run: `pnpm typecheck` → 通过（前端无改动，防意外）

- [ ] **Step 4.2: AGENTS.md 更新**

Arthas 段落 "arthas 包经 artifactory 统一下发（`provision/arthas.rs`）" 改为 "arthas 包随应用分发（resources/arthas，`scripts/fetch-arthas.ps1` 构建期下载），attach 时 SFTP 直传目标机（`provision/arthas.rs`）"。

- [ ] **Step 4.3: 提交**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md for vendored arthas provisioning"
```

- [ ] **Step 4.4: issue #7 回复**（发布验证后）：说明修复方案 + 版本号。留给控制器（主会话）在发布完成后执行。

---

## Self-Review 记录

- Spec 覆盖：构建期（Task 3）+ 运行时解析/传递（Task 1）+ ensure 直传（Task 2）+ 回归/文档（Task 4）✓
- 类型一致性：`arthas_zip: Option<PathBuf>` 贯穿 ProvisionContext/AttachDeps；zip 文件名 `arthas-bin-{ARTHAS_VERSION}.zip` 三处一致（lib.rs 解析/ensure 上传/脚本产物）✓
- 占位符：无 TBD；Task 3 Step 3.1 的 URL 需现场确认官方 asset 命名（已写明确认方法）✓
