# AGENTS.md

## 项目概述

Friday 是面向软件开发人员的问题定位辅助工具。技术栈为 **Tauri**（Rust 后端 + React 前端）。

> 仓库当前为空（无提交、无文件）。下述结构约定随代码落地逐步核实与补充。

## 技术栈与结构

- **Tauri**：Rust 后端预期位于 `src-tauri/`，React 前端在其同级目录；两者通过 Tauri IPC（command / event）通信。
- 修改后端 command 或 event 时，同步检查前端调用侧绑定，避免两端脱节。
- 单体仓库，无多包（workspace）划分。

## 开发命令

> 暂未确定，待代码落地后补全。Tauri 项目通常使用 `cargo tauri dev` / `cargo tauri build`，但在仓库中核实前不要将其作为既定命令断言。

- 构建 / 运行：`TODO`
- 测试：`TODO`
- lint：`TODO`
- typecheck：`TODO`

## 约定

无特殊团队约定。分支、提交信息、CI 等按默认处理；不要强加未要求的规范（如 Conventional Commits）。
