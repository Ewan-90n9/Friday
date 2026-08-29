# AGENTS.md

## 项目概述

Friday 是面向软件开发人员的**远程环境运行时故障诊断 Agent**。用户输入"环境+服务+症状"（如"xx.xx.xx.xx 环境 OOMService OOM 了，帮我定位"），Agent 自动连接目标环境、调用诊断工具（jstat、jcmd、arthas、读日志、读 dump 等）、分析根因并给出结论。

技术栈为 **Tauri**（Rust 后端 + React 前端）。

> 仓库已落地骨架代码（会话管理、agent 检测、对话管道）。下述结构约定随代码持续演进。

## 技术栈与结构

- **Tauri**：Rust 后端位于 `src-tauri/`，React 前端位于 `src/`；两者通过 Tauri IPC（command / event）通信。
- 修改后端 command 或 event 时，同步检查前端调用侧绑定（`src/lib/ipc.ts`），避免两端脱节。
- 单体仓库，无多包（workspace）划分。

## 架构设计

- [总览（决策表 + 分层图）](docs/architecture/overview.md)
- [运行时模型（通信 + 并发 + 取消）](docs/architecture/runtime.md)
- [错误处理与安全边界](docs/architecture/error-handling.md)
- [基础设施（凭证 + 日志）](docs/architecture/infrastructure.md)
- [日志规范（强制约束）](docs/architecture/logging-standard.md)
- [知识层（Playbook）](docs/architecture/playbook.md)
- [v1 范围与演进](docs/architecture/roadmap.md)

## 设计语言

- [Friday 设计语言](docs/design/design-language.md)

## 已实现功能

- **骨架层**：SQLite 初始化、tracing 日志、Tauri IPC 命令注册、三栏暗色布局
- **Agent 自动识别**：检测 PATH 上的 opencode / codeagentcli 二进制、版本探测、持久化到 SQLite、UI 设置弹窗、手动添加路径、切换 active agent
- **多 Provider 支持**：opencode 和 codeagentcli 同时支持，用户可在设置中切换。spawn 通过 `CommandConfig` 按 provider 分发不同 CLI 参数，stream 解析器同时处理两种 NDJSON 格式（opencode 的 `part.*` 和 codeagentcli 的 Claude API 风格 `message.content[]`）
- **对话管道**：多轮对话、NDJSON 流式解析、Friday 人格 system prompt、会话列表、流式渲染（文本 + 工具卡片）
- **文件上传下载**：独立 Agent 工具（file_download / file_upload / transfer_status / transfer_cancel），TransferManager 后台异步传输（专用 SSH 连接、断点续传、5 次重试/2h 预算、1s 进度事件），heap_dump 生成后自动后台拉回，前端聊天流内进度条卡片

## 开发命令

- 构建 / 运行：`pnpm tauri dev`（开发）/ `pnpm tauri build`（打包）
- 前端单独运行：`pnpm dev`
- 前端类型检查：`pnpm typecheck`
- Rust 检查：`cargo check --manifest-path src-tauri/Cargo.toml`
- Rust 测试：`cargo test --manifest-path src-tauri/Cargo.toml`
- lint：TODO（待定 clippy + eslint 配置后再补）

## 发版流程

- **版本规则**：SemVer + 0.x.y 预发布阶段。Tag 格式 `vX.Y.Z`（如 `v0.1.0`）。详见 [版本号规则与自动化发布设计](docs/superpowers/specs/2026-08-21-versioning-release-design.md)。
- **发版步骤**：
  1. 确认主分支代码就绪，本地跑 `pnpm typecheck` + `cargo check --manifest-path src-tauri/Cargo.toml`。
  2. 打 tag：`git tag vX.Y.Z && git push origin vX.Y.Z`（或在 GitHub Releases 页面创建）。
  3. CI 自动构建并发布（`.msi` + `.exe`），无需人工干预。
  4. 几分钟后在 Releases 页面验证产物和 release notes。
- **版本同步**：源码中版本号保持 `0.1.0` 不变，CI 从 tag 提取版本号自动注入 `package.json`、`Cargo.toml`、`tauri.conf.json`。不要手动改版本号。
- **CI 配置**：`.github/workflows/release.yml`，版本注入脚本 `scripts/set-version.ps1`。

## 约定

- **日志规范**：编写或修改 Rust 代码时必须遵从 [docs/architecture/logging-standard.md](docs/architecture/logging-standard.md)。核心要求：每个 Tauri command 有 `#[instrument]` 或入口 `info!`；错误路径有 `tracing::error!`/`warn!`；子进程 stderr 必须读取记录；日志不截断、不脱敏。
- **文件管理**：所有运行时文件路径通过 `infra/paths.rs` 的 `Paths` struct 统一解析，不内联 `.join()`。`Paths` 存入 `AppState`，各模块从 `State<AppState>` 取路径。新增文件类别时，在 `Paths` 加方法 + `ensure_dirs()` 加目录，不散落到各模块。详见 [文件管理设计](docs/superpowers/specs/2026-08-21-file-management-design.md)。
- 无特殊团队约定。分支、提交信息、CI 等按默认处理；不要强加未要求的规范（如 Conventional Commits）。
