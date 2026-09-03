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
- **堆快照分析**：heap_* 系列 9 个 MCP 工具（MAT 内核，leak suspects/支配树/GC root 链/对象下钻/线程分析）。Friday 作为 MCP client 托管 vendored jvm-heap-dump-mcp JAR 工人进程（stdio，需本机 Java 21+，JAR 由 `scripts/fetch-analyzer-jar.ps1` 构建时获取、随安装包分发）；dump 拉回完成自动预热（MAT 建索引，provision_progress 事件）；会话 LRU（上限 3）、空闲 15min 自动退出、崩溃自动重启、会话关闭联动释放
- **JFR 飞行记录分析**：`jfr_record`（jcmd JFR.start 一次性定时录制 → 自动拉回 → `.jfr` 下载完成自动预热）+ 21 个 `jfr_*` 分析工具（JMC 内核：规则引擎/一键诊断/GC/热点方法/锁竞争/IO/异常/泄漏/相关性/A-B 对比）。Friday 作为 MCP client 托管 JMC JAR 工人进程（stdio，本机 Java 21+ + `--enable-preview`；JAR 由 `.github/workflows/jmc-jar.yml` 从上游 pinned SHA 降级构建、发布到本仓库 Releases，`scripts/fetch-jmc-jar.ps1` 构建时获取、随安装包分发）；无会话层（上游自带缓存），空闲 15min 自动退出、传输错误 invalidate 懒重建；TransferManager 下载完成钩子按扩展名分发（.hprof → MAT 预热 / .jfr → JMC 预热）。vendoring 依赖统一管理见 `scripts/vendor-versions.json`（checksum + 一致性单测 + `vendor-update-check.yml` 周期巡检上游）
- **Arthas 动态诊断**：arthas_open / arthas_close + 25 个 arthas_* 代理工具（dashboard / thread / watch / trace / sc / jad / ognl 等，精选诊断集，剔除 redefine 热更新类）。Friday 作为 MCP client（rmcp streamable-http + Bearer）经 SSH exec 通道 HTTP 桥（`arthas/bridge.rs`，每请求一条 exec curl；不依赖 sshd TCP 转发，适配 AllowTcpForwarding no 环境）连目标机 arthas 4.x 内置 MCP Server；arthas 包随应用分发（resources/arthas，scripts/fetch-arthas.ps1 构建期下载），attach 时 SFTP 直传目标机（`provision/arthas.rs`）；ArthasManager 管理会话（并发去重、LRU 3、空闲 15min 回收、传输错误 invalidate、stale attach 任务防护）；attach 前自动清理残留实例、失败路径停 arthas（`arthas/attach.rs`）；attach 用户对齐（SSH 用户 ≠ JVM 用户时用对应用户凭证建临时连接）；环境多用户凭证管理（`env_credentials` 表 + 新增/编辑统一弹窗：凭证增删改/星标设默认、本地暂存、`save_environment_cmd` 原子提交；默认凭证即日常 SSH 用户；逐凭证测试连接）
- **诊断工具面板分组**：右侧工具面板按 `ToolCategory`（tools/category.rs，声明序即分组展示序）分组折叠展示（7 组 73 项，默认全折叠，两行式列表项）。category 在工具注册时声明（单一事实来源，新增工具必须在 ToolDef 构造点补 category）；`list_tools_cmd` 稳定排序（分类序 → 名称序）；前端展示名统一 `friday_` 前缀与聊天流一致

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
