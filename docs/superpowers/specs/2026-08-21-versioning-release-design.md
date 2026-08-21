# Friday 版本号规则与自动化发布设计

> 日期：2026-08-21
> 状态：已确认，待实现

## 1. 背景与动机

Friday 已落地骨架功能（SQLite、tracing 日志、agent 检测、对话管道），源码版本号统一为 `0.1.0`。当前仓库无 `.github/` 目录，无 CI/CD 配置，无 tag，无 Release。

需要建立版本号规则并实现全自动发布流程：开发者只需在 GitHub 上打一个 tag，CI 自动构建产物并创建 Release。首版仅覆盖 Windows 平台，不做代码签名。

## 2. 决策汇总

| 项 | 决策 | 说明 |
|----|------|------|
| 版本规则 | SemVer + 0.x.y 预发布阶段 | 标准语义化版本，0.x 阶段 minor 位可含破坏性变更 |
| Tag 格式 | `v` 前缀 + 版本号 | 如 `v0.1.0`、`v0.2.1`（GitHub 惯例） |
| 版本同步 | tag 驱动，CI 自动写入 | 源码中版本号不变（保持 `0.1.0`），CI 从 tag 提取版本号写入 3 个文件 |
| 目标平台 | 仅 Windows | 单 runner，`.msi` + `.exe`（NSIS） |
| Release Notes | GitHub 自动生成 | 从 PR 标题/标签自动生成 |
| 代码签名 | 不签名 | 用户安装时见 SmartScreen 警告，点击"仍要运行"即可 |
| CI 工具 | `tauri-apps/tauri-action` | Tauri 官方 Action，封装构建+发布全流程 |
| 首个发布版本 | `v0.1.0` | 与当前源码版本号一致 |

## 3. 版本号规则

### 3.1 格式

标准 SemVer：`MAJOR.MINOR.PATCH`

### 3.2 0.x.y 预发布阶段（当前 → v1.0.0 之前）

- `MINOR` 位 = 功能性变更（可含破坏性变更，如 API/配置格式调整）
- `PATCH` 位 = 纯 bug 修复，不引入新功能

示例：
- `v0.1.0`（首发）→ `v0.1.1`（修 bug）→ `v0.2.0`（新功能）

### 3.3 1.0.0 之后（未来）

- `MAJOR` = 破坏性变更
- `MINOR` = 新功能（向后兼容）
- `PATCH` = bug 修复

### 3.4 版本文件

三处版本号保持一致：
- `package.json`（`version` 字段）
- `src-tauri/Cargo.toml`（`version` 字段）
- `src-tauri/tauri.conf.json`（`version` 字段）

源码中保持 `0.1.0`，CI 构建时从 tag 注入实际版本号。源码版本号不随发版变更，避免每次发版产生无意义的版本号 bump 提交。

## 4. CI/CD Pipeline（GitHub Actions Workflow）

### 4.1 触发条件

推送匹配 `v*` 的 tag（如 `v0.1.0`、`v0.2.1`）。

### 4.2 Runner

`windows-latest`（单平台）。

### 4.3 执行步骤

| 步骤 | 动作 | 说明 |
|------|------|------|
| 1. Checkout | `actions/checkout@v4` | 拉取代码 |
| 2. 安装 Node + pnpm | `actions/setup-node@v4` + `pnpm/action-setup@v2` | Node 22, pnpm 11 |
| 3. 安装 Rust | `dtolnay/rust-toolchain@stable` | 稳定版工具链 |
| 4. 缓存 Rust | `Swatinem/rust-cache@v2` | 缓存 `src-tauri/target`，加速后续构建 |
| 5. 安装前端依赖 | `pnpm install` | |
| 6. 版本注入 | PowerShell 脚本 | 从 tag 提取版本号，写入 3 个文件 |
| 7. 构建并发布 | `tauri-apps/tauri-action@v0` | 构建 + 创建 Release + 上传产物 |

### 4.4 版本注入脚本（Step 6 详解）

从 `GITHUB_REF_NAME` 环境变量提取 tag 名，去掉 `v` 前缀得到版本号（如 `v0.2.1` → `0.2.1`）。用 PowerShell 正则替换三个文件中的 version 字段：

- `package.json`: `"version": "0.1.0"` → `"version": "0.2.1"`
- `src-tauri/Cargo.toml`: `version = "0.1.0"` → `version = "0.2.1"`
- `src-tauri/tauri.conf.json`: `"version": "0.1.0"` → `"version": "0.2.1"`

此步骤不提交到仓库，只在 CI 构建环境中生效。

### 4.5 `tauri-action` 配置

- `tagName`: `${{ github.ref_name }}`（触发的 tag 名）
- `releaseName`: `Friday v__VERSION__`（`__VERSION__` 被 tauri-action 自动替换为版本号）
- `generateReleaseNotes`: `true`（GitHub 自动从 PR 标题生成 release notes）
- `args`: `--bundles msi,nsis`（生成 `.msi` 安装包和 `.exe` NSIS 安装器）

### 4.6 产物

上传到 GitHub Release 页面，用户可选 `.msi` 或 `.exe` 下载安装。

### 4.7 不签名的后果

用户首次安装时 Windows SmartScreen 会弹出警告，点击"仍要运行"即可。首版可接受，后续版本可按需添加代码签名。

## 5. 发版操作步骤

### 5.1 前提

代码已合并到主分支，所有功能/修复已就绪。

### 5.2 步骤

1. **确定版本号**：根据变更类型决定
   - 纯 bug 修复 → patch +1（如 `v0.1.0` → `v0.1.1`）
   - 新功能 → minor +1（如 `v0.1.0` → `v0.2.0`）

2. **在 GitHub 上打 tag**：
   - 进入仓库页面 → **Releases** → **Create a new release** → **Choose a tag**
   - 输入 tag 名（如 `v0.1.1`），选择 **Create new tag: v0.1.1 on publish**
   - 选择目标分支（默认 main）
   - **Publish release**（标题和说明留空，CI 会自动生成）

   或用命令行（等效）：
   ```bash
   git tag v0.1.1
   git push origin v0.1.1
   ```

3. **CI 自动执行**：tag 推送后，GitHub Actions workflow 自动触发 — 构建 → 创建/更新 Release → 上传产物 → 生成 release notes。无需人工干预。

4. **验证**：几分钟后进入 Releases 页面，确认：
   - Release 已创建，标题为 `Friday vX.X.X`
   - Release notes 已自动生成（列出 PR 列表）
   - 产物已上传（`.msi` 和 `.exe` 各一个）

5. **（可选）发布后检查**：从 Release 页面下载 `.msi`，本地安装验证能否正常启动。

### 5.3 首个版本

当前源码版本号为 `0.1.0`，首个发布的 tag 就是 `v0.1.0`。操作步骤与常规发版完全相同。

## 6. 失败处理与验证

### 6.1 CI 失败场景与应对

| 场景 | 现象 | 处理 |
|------|------|------|
| 版本注入失败 | workflow Step 6 报红 | 检查 tag 格式是否为 `vX.Y.Z`（正则不匹配会报错退出） |
| 前端构建失败 | `pnpm build` 报错 | 检查 TypeScript 编译错误，本地先跑 `pnpm typecheck` |
| Rust 构建失败 | `tauri-action` 报错 | 检查 `cargo check`，本地先跑验证 |
| Release 创建失败 | 通常因 tag 已存在对应 Release | 删除旧 Release 后重新触发（重新 push tag 或手动跑 workflow） |

### 6.2 发版前本地验证清单

推荐在打 tag 前跑一遍：

```bash
pnpm typecheck
cargo check --manifest-path src-tauri/Cargo.toml
pnpm tauri build  # 本地完整构建一次，确认产物正常
```

### 6.3 不做的事

- 不加自动化测试门禁（项目当前无测试框架，后续按 roadmap 演进再加）
- 不加多平台验证（首版仅 Windows）
- 不加回滚机制（Release 页面可手动删除，tag 可手动删除后重新打）

## 7. 演进路径

| 阶段 | 演进项 | 触发条件 |
|------|--------|----------|
| 后续版本 | 添加 macOS 构建 | 需要支持 Mac 用户 |
| 后续版本 | 添加代码签名 | SmartScreen 警告影响用户体验 |
| 1.0.0 | 版本规则切换到严格 SemVer | 功能稳定，可对外承诺 API 稳定性 |
| 后续版本 | 启用 Tauri 内置更新器 | 需要应用内自动更新功能 |
