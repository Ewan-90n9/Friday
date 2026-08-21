# Friday 版本号规则与自动化发布实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立版本号规则并实现全自动发布流程 — 开发者只需在 GitHub 上打一个 `v*` tag，CI 自动构建 Windows 安装包并创建 Release。

**Architecture:** 一个 GitHub Actions workflow 在 tag 推送时触发，先运行 PowerShell 版本注入脚本将 tag 版本号写入 3 个文件（`package.json`、`Cargo.toml`、`tauri.conf.json`），再用 `tauri-apps/tauri-action` 构建并发布。版本注入不提交到仓库，只在 CI 构建环境中生效。

**Tech Stack:** GitHub Actions, PowerShell, `tauri-apps/tauri-action@v0`, Tauri 2, pnpm, Rust

**Spec:** `docs/superpowers/specs/2026-08-21-versioning-release-design.md`

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `scripts/set-version.ps1` | 新增 | 版本注入脚本 — 从 tag 提取版本号，正则替换 3 个文件中的 version 字段 |
| `.github/workflows/release.yml` | 新增 | GitHub Actions workflow — tag 触发，构建并发布 Windows 安装包 |
| `AGENTS.md` | 修改 | 补充"发版流程"章节，链接到 spec |

---

## Task 1: 创建版本注入脚本 `scripts/set-version.ps1`

**Files:**
- Create: `scripts/set-version.ps1`

- [ ] **Step 1: 创建 `scripts/` 目录**

```powershell
New-Item -ItemType Directory -Path "scripts"
```

- [ ] **Step 2: 编写 `scripts/set-version.ps1`**

```powershell
param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

$Version = $Version -replace '^v', ''

if ($Version -notmatch '^\d+\.\d+\.\d+') {
    Write-Error "Invalid version format: $Version. Expected: X.Y.Z (e.g., 0.1.0)"
    exit 1
}

Write-Host "Injecting version $Version into 3 files..."

$packageJson = Get-Content "package.json" -Raw
$packageJson = $packageJson -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
Set-Content -Path "package.json" -Value $packageJson -NoNewline
Write-Host "  Updated package.json"

$cargoToml = Get-Content "src-tauri/Cargo.toml" -Raw
$cargoToml = $cargoToml -replace '(?m)^version\s*=\s*"[^"]*"', "version = `"$Version`""
Set-Content -Path "src-tauri/Cargo.toml" -Value $cargoToml -NoNewline
Write-Host "  Updated src-tauri/Cargo.toml"

$tauriConf = Get-Content "src-tauri/tauri.conf.json" -Raw
$tauriConf = $tauriConf -replace '"version"\s*:\s*"[^"]*"', "`"version`": `"$Version`""
Set-Content -Path "src-tauri/tauri.conf.json" -Value $tauriConf -NoNewline
Write-Host "  Updated src-tauri/tauri.conf.json"

Write-Host "Done. All 3 files updated to version $Version"
```

**脚本说明：**
- 接收 `-Version` 参数（如 `v0.2.1` 或 `0.2.1`），去掉 `v` 前缀
- 用正则 `^\d+\.\d+\.\d+` 校验 SemVer 格式，不匹配则报错退出
- `package.json` 和 `tauri.conf.json`：匹配 `"version": "..."` 模式替换
- `Cargo.toml`：用 `(?m)^version\s*=\s*"[^"]*"` 匹配行首的 `version = "..."`（排除依赖中的 `version = "2"`）
- `Set-Content -NoNewline` 避免添加额外尾部换行

- [ ] **Step 3: 本地测试脚本**

运行脚本注入一个测试版本号：

```powershell
./scripts/set-version.ps1 -Version "v0.9.9"
```

Expected output:
```
Injecting version 0.9.9 into 3 files...
  Updated package.json
  Updated src-tauri/Cargo.toml
  Updated src-tauri/tauri.conf.json
Done. All 3 files updated to version 0.9.9
```

- [ ] **Step 4: 验证 3 个文件已正确更新**

```powershell
Select-String -Path "package.json" -Pattern '"version"'
Select-String -Path "src-tauri/Cargo.toml" -Pattern '^version'
Select-String -Path "src-tauri/tauri.conf.json" -Pattern '"version"'
```

Expected: 3 个文件中的 version 字段均为 `0.9.9`

- [ ] **Step 5: 恢复文件（撤销测试改动）**

```powershell
git checkout -- package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
```

- [ ] **Step 6: 测试无效版本号报错**

```powershell
./scripts/set-version.ps1 -Version "invalid"
```

Expected: 报错退出，输出 `Invalid version format: invalid. Expected: X.Y.Z (e.g., 0.1.0)`，3 个文件未被修改

- [ ] **Step 7: 再次恢复文件（如有改动）**

```powershell
git checkout -- package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
```

- [ ] **Step 8: 提交**

```bash
git add scripts/set-version.ps1
git commit -m "feat: add version injection script for CI releases"
```

---

## Task 2: 创建 GitHub Actions workflow `.github/workflows/release.yml`

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: 创建 `.github/workflows/` 目录**

```powershell
New-Item -ItemType Directory -Path ".github/workflows" -Force
```

- [ ] **Step 2: 编写 `.github/workflows/release.yml`**

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write

jobs:
  release:
    runs-on: windows-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Setup pnpm
        uses: pnpm/action-setup@v2

      - name: Setup Node
        uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: 'pnpm'

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Cache Rust
        uses: Swatinem/rust-cache@v2
        with:
          workspaces: src-tauri

      - name: Install frontend dependencies
        run: pnpm install

      - name: Inject version from tag
        shell: pwsh
        run: ./scripts/set-version.ps1 -Version ${{ github.ref_name }}

      - name: Build and publish
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          tagName: ${{ github.ref_name }}
          releaseName: 'Friday v__VERSION__'
          releaseDraft: false
          generateReleaseNotes: true
          args: --bundles msi,nsis
```

**workflow 说明：**
- `on.push.tags: 'v*'` — 推送 `v` 开头的 tag 时触发（如 `v0.1.0`）
- `permissions: contents: write` — 允许 `GITHUB_TOKEN` 创建 Release
- `pnpm/action-setup@v2` — 从 `package.json` 的 `packageManager` 字段读取 pnpm 版本（11.22.0）
- `actions/setup-node@v4` — Node 22，启用 pnpm 缓存
- `Swatinem/rust-cache@v2` — `workspaces: src-tauri` 缓存 `src-tauri/target/`
- 版本注入步骤用 `shell: pwsh`（GitHub Actions windows-latest 默认 PowerShell 7）
- `tauri-action` 配置：
  - `tagName` = 触发的 tag 名（如 `v0.1.0`）
  - `releaseName` = `Friday v__VERSION__`（`__VERSION__` 被 tauri-action 从 `tauri.conf.json` 读取替换）
  - `releaseDraft: false` — 直接发布，不存为草稿
  - `generateReleaseNotes: true` — GitHub 自动从 PR 标题生成 release notes
  - `args: --bundles msi,nsis` — 生成 `.msi` 和 `.exe` 两个安装包

- [ ] **Step 3: 验证 YAML 语法**

```powershell
try {
    $null = Get-Content ".github/workflows/release.yml" -Raw | ConvertFrom-Yaml
    Write-Host "YAML syntax valid"
} catch {
    Write-Host "YAML syntax check skipped (no ConvertFrom-Yaml module) — verify manually"
}
```

> 注：如果 PowerShell 没有安装 `powershell-yaml` 模块，此步跳过。YAML 语法在 CI 首次运行时由 GitHub Actions 自动验证。

- [ ] **Step 4: 提交**

```bash
git add .github/workflows/release.yml
git commit -m "feat: add automated release workflow for Windows builds"
```

---

## Task 3: 更新 AGENTS.md 补充发版流程

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: 在 AGENTS.md "开发命令" 之后添加"发版流程"章节**

在 `AGENTS.md` 中找到：

```markdown
- lint：TODO（待定 clippy + eslint 配置后再补）
```

在其后添加：

```markdown

## 发版流程

- **版本规则**：SemVer + 0.x.y 预发布阶段。Tag 格式 `vX.Y.Z`（如 `v0.1.0`）。详见 [版本号规则与自动化发布设计](docs/superpowers/specs/2026-08-21-versioning-release-design.md)。
- **发版步骤**：
  1. 确认主分支代码就绪，本地跑 `pnpm typecheck` + `cargo check --manifest-path src-tauri/Cargo.toml`。
  2. 打 tag：`git tag vX.Y.Z && git push origin vX.Y.Z`（或在 GitHub Releases 页面创建）。
  3. CI 自动构建并发布（`.msi` + `.exe`），无需人工干预。
  4. 几分钟后在 Releases 页面验证产物和 release notes。
- **版本同步**：源码中版本号保持 `0.1.0` 不变，CI 从 tag 提取版本号自动注入 `package.json`、`Cargo.toml`、`tauri.conf.json`。不要手动改版本号。
- **CI 配置**：`.github/workflows/release.yml`，版本注入脚本 `scripts/set-version.ps1`。
```

- [ ] **Step 2: 验证文件内容**

读取 `AGENTS.md`，确认"发版流程"章节已正确插入在"开发命令"和"约定"之间。

- [ ] **Step 3: 提交**

```bash
git add AGENTS.md
git commit -m "docs: add release process to AGENTS.md"
```

---

## Task 4: 发版前本地验证

**Files:**
- 无文件修改

此任务验证当前代码可以成功构建，确保首个 tag 推送后 CI 不会因代码问题失败。

- [ ] **Step 1: 前端类型检查**

```bash
pnpm typecheck
```

Expected: 无错误退出（exit code 0）

- [ ] **Step 2: Rust 检查**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
```

Expected: 无错误退出（exit code 0）

- [ ] **Step 3: 完整构建**

```bash
pnpm tauri build
```

Expected: 构建成功，产物在 `src-tauri/target/release/bundle/` 下生成。

- [ ] **Step 4: 验证构建产物**

```powershell
Get-ChildItem -Path "src-tauri/target/release/bundle" -Recurse -Include "*.msi","*.exe"
```

Expected: 至少有一个 `.msi` 文件和一个 `.exe`（NSIS）文件

> 如果构建失败，修复问题后再继续。不要在构建失败的情况下打 tag 发版。

---

## Task 5: 首次发布 v0.1.0（用户操作）

**Files:**
- 无文件修改

此任务为用户在 GitHub 上的操作步骤，由用户手动执行。

- [ ] **Step 1: 推送所有提交到 GitHub**

```bash
git push origin main
```

确认 Task 1-3 的 3 个提交已推送到远程 `main` 分支。

- [ ] **Step 2: 打首个 tag**

方式 A（命令行）：
```bash
git tag v0.1.0
git push origin v0.1.0
```

方式 B（GitHub 网页）：
1. 进入仓库页面 → **Releases** → **Create a new release** → **Choose a tag**
2. 输入 `v0.1.0`，选择 **Create new tag: v0.1.0 on publish**
3. 目标分支选 `main`
4. 标题和说明留空（CI 自动生成）
5. 点击 **Publish release**

- [ ] **Step 3: 监控 CI**

进入仓库 **Actions** 标签页，确认 `Release` workflow 已触发并正在运行。

Expected:
- workflow 名称：`Release`
- 触发事件：`push` tag `v0.1.0`
- 状态：in progress → completed（绿色 ✓）

- [ ] **Step 4: 验证 Release**

进入仓库 **Releases** 页面，确认：
- Release 标题为 `Friday v0.1.0`
- Release notes 已自动生成
- 产物已上传：`.msi` 和 `.exe` 各一个

- [ ] **Step 5: （可选）下载安装验证**

从 Release 页面下载 `.msi`，本地安装，启动 Friday 验证功能正常。

> 如果 CI 失败，查看 Actions 日志定位问题。常见原因：版本注入脚本路径错误、`tauri-action` 配置问题、代码编译错误（Task 4 应已排除后者）。

---

## Spec Coverage Checklist

| Spec 章节 | 对应 Task | 状态 |
|-----------|-----------|------|
| §3 版本号规则 | Task 3（写入 AGENTS.md） | ✅ |
| §3.4 版本文件（3 处同步） | Task 1（脚本注入） | ✅ |
| §4.1 触发条件（`v*` tag） | Task 2（workflow `on.push.tags`） | ✅ |
| §4.2 Runner（windows-latest） | Task 2（workflow `runs-on`） | ✅ |
| §4.3 执行步骤 1-5（checkout/pnpm/rust/cache/install） | Task 2（workflow steps） | ✅ |
| §4.3 执行步骤 6（版本注入） | Task 1 + Task 2（脚本 + workflow 调用） | ✅ |
| §4.3 执行步骤 7（构建并发布） | Task 2（tauri-action） | ✅ |
| §4.4 版本注入脚本详解 | Task 1 | ✅ |
| §4.5 tauri-action 配置 | Task 2 | ✅ |
| §4.6 产物（msi + exe） | Task 2（`args: --bundles msi,nsis`） | ✅ |
| §4.7 不签名 | Task 2（无签名配置） | ✅ |
| §5 发版操作步骤 | Task 3（AGENTS.md）+ Task 5（首次发布） | ✅ |
| §5.3 首个版本 v0.1.0 | Task 5 | ✅ |
| §6.1 CI 失败处理 | Task 5 Step 5（失败排查说明） | ✅ |
| §6.2 发版前本地验证清单 | Task 4 | ✅ |
| §6.3 不做的事 | Task 2（无测试门禁/多平台/回滚） | ✅ |
| §7 演进路径 | 已在 spec 中记录，无需实现 | ✅（文档） |
