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
- [知识层（Playbook）](docs/architecture/playbook.md)
- [v1 范围与演进](docs/architecture/roadmap.md)

## 设计语言

- [Friday 设计语言](docs/design/design-language.md)

## 已实现功能

- **骨架层**：SQLite 初始化、tracing 日志、Tauri IPC 命令注册、三栏暗色布局
- **Agent 自动识别**：检测 PATH 上的 opencode 二进制、版本探测、持久化到 SQLite、UI 设置弹窗
- **对话管道**：多轮对话（`opencode run --format json --dangerously-skip-permissions`）、NDJSON 流式解析、Friday 人格 system prompt、会话列表、流式渲染（文本 + 工具卡片）

## 开发命令

- 构建 / 运行：`pnpm tauri dev`（开发）/ `pnpm tauri build`（打包）
- 前端单独运行：`pnpm dev`
- 前端类型检查：`pnpm typecheck`
- Rust 检查：`cargo check --manifest-path src-tauri/Cargo.toml`
- Rust 测试：`cargo test --manifest-path src-tauri/Cargo.toml`
- lint：TODO（待定 clippy + eslint 配置后再补）

## 约定

无特殊团队约定。分支、提交信息、CI 等按默认处理；不要强加未要求的规范（如 Conventional Commits）。
