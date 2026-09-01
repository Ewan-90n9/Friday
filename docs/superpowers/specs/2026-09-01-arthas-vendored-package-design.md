# arthas 包 vendoring 设计（随应用分发，SFTP 直传目标机）

日期：2026-09-01
状态：已评审通过
关联 issue：#7（arthas 功能不可用：artifactory 404）

## 背景与问题

`arthas_open` 下发工具包时两个通道全部失败：

- **Channel A**（目标机 curl/wget 直接从 artifactory 拉）→ HTTP 404
- **Channel B**（Windows 本地下载 + SFTP 上传兜底）→ 从**同一个 URL** 下载 → 同样 404

根因：artifactory 的 `cmc-software-release/arthas/arthas-bin-4.3.5.zip` 路径未放包。A/B 共用 URL 意味着 artifactory 缺包时必双失败——下发链路对 artifactory 的强依赖是设计缺陷。

Issue 作者要求：**arthas 包跟随 Friday 打包发布，使用时从 Windows 上传到目标机，不依赖仓库下载。**

## 方案

对齐项目已有的 vendoring 模式（heap analyzer JAR：`scripts/fetch-analyzer-jar.ps1` → `resources/analyzer/*` → resource_dir 解析）：

### 构建期（CI）

- 新增 `scripts/fetch-arthas.ps1`：从 arthas 官方 GitHub Releases 下载 `arthas-bin-4.3.5.zip` 到 `src-tauri/resources/arthas/`（幂等：已存在即跳过）
- `src-tauri/tauri.conf.json` resources 增加 `"resources/arthas/*"`
- `.github/workflows/release.yml` 在 "Download heap analyzer JAR" 后加一步 `./scripts/fetch-arthas.ps1`

### 运行时

1. **解析**（`lib.rs`）：`resource_dir` 候选路径 `[r/resources/arthas/<zip>, r/arthas/<zip>]`（analyzer JAR 同款双候选，兼容 dev/打包两种布局）；存入 `AttachDeps.arthas_zip: Option<PathBuf>`。缺失时 `tracing::warn!`（attach 时报结构化错误，不阻断启动——与 analyzer JAR 缺失同策略）
2. **传递**：`AttachDeps` → `provision_context()` → `ProvisionContext.arthas_zip: Option<PathBuf>`
3. **下发**（`ArthasPackage::ensure`）：
   - 远端缓存检查（不变：`/tmp/friday-tools/arthas-4.3.5/arthas-boot.jar` 存在直接返回 cached）
   - vendored zip 校验：`arthas_zip` 缺失 → 错误 "arthas 工具包未随应用分发（resources/arthas/arthas-bin-4.3.5.zip），请重新安装 Friday"；存在但 < 5MB → 同款损坏错误
   - `ctx.channel.upload(zip, /tmp/friday-tools/arthas-bin-4.3.5.zip)`（原 channel B 的 SFTP 上传）
   - 解压（unzip → python3 兜底）+ 扁平化 + 清理 + 验证（**全部不变**）

### 删除

- Channel A（远端 curl/wget：`try_remote_download` 调用点）
- Channel B 的本地下载（`download_to_cache` 调用点；transfer.rs 本身保留，JDK 下发仍用）
- `arthas_download_url()` 函数及其测试
- ensure 里对 `ctx.artifactory_base_url` 的依赖（字段保留，JDK 仍用）

### 不做（YAGNI）

- 不动 JDK 下发链路（仍走 artifactory 双通道）
- 不做 zip 的 sha256 校验（与 analyzer JAR 一致，大小校验足够；官方 GitHub Releases 源可信）
- 不做 arthas 版本自动升级（仍硬编码 `ARTHAS_VERSION`，升级 = 改常量 + 换包）
- 前端无改动（进度事件 stage/detail 语义不变）

## 产物影响

arthas-bin.zip ≈ 15MB；setup.exe 约 80MB → 95MB，msi 相应增大。可接受。

## 测试策略

- Rust 单测（SequentialChannel 模式，对齐 jdk.rs 现有测试）：
  - vendored zip 缺失 → 明确错误信息
  - zip 存在 → upload 被调用且目标是 `/tmp/friday-tools/arthas-bin-4.3.5.zip`，解压命令序列正确
  - 远端缓存命中 → 不上传不解压
- fetch-arthas.ps1 由 CI 运行验证（本地不重复测试）
- 手动冒烟：`pnpm tauri dev` + arthas_open 走通（用户执行）
