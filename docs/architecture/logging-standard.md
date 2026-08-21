# 日志规范

> 本文档是项目级强制约束。编写或修改任何 Rust 代码时必须遵从以下规则。

## 1. 框架与输出

- 使用 `tracing` 生态，不引入其他日志库（log4rs / env_logger / slog 等）。
- 双输出：stdout + 每日轮转文件（`logs/friday.log`），通过 `infra/logging.rs` 统一初始化。
- 格式维持 `fmt::layer()` 默认人类可读文本，不输出 JSON。
- 默认级别 `debug`，通过 `RUST_LOG` 环境变量覆盖。
- 日志文件保留 7 天，超期自动清理。

## 2. 级别约定

| 级别 | 用于 | 示例 |
|------|------|------|
| `ERROR` | 操作失败且无法自动恢复，需人为介入 | spawn 失败、IPC emit 失败、子进程非零退出、panic |
| `WARN` | 可恢复的异常或非预期状态 | opencode stderr 输出、SSH 重试、连接断开重连、工具超时取消 |
| `INFO` | 关键生命周期事件，能回答"发生了什么" | command 入口、session 创建/恢复、spawn 成功、进程退出、级别调整 |
| `DEBUG` | 排障细节 | NDJSON 原始行、event 类型、SQL 变更细节 |
| `TRACE` | 极细粒度数据流，默认关闭 | 单条 NDJSON 完整内容、span enter/exit |

**选择规则**：正常流程中也会触发的日志用 INFO；只在异常路径触发的用 WARN 或 ERROR。拿不准时问自己"这条日志在正常运行中出现是否意味着出了问题"——是则 WARN，否则 INFO。

## 3. Span 与 `#[instrument]`

在以下关键入口添加 `#[tracing::instrument]`，使 `session_id` 自动作为 span 字段传播，子节点日志无需手动重复：

| 函数 | instrument 签名 |
|------|----------------|
| `send_message_cmd` | `#[tracing::instrument(skip(state, session_id), fields(session_id))]` — `session_id` 是 `Option<String>`，需在函数内解析出实际 ID 后用 `Span::current().record("session_id", &tracing::field::display(&friday_session_id))` 记录 |
| `stop_agent_cmd` | `#[tracing::instrument(skip(state), fields(session_id))]` |
| `close_session_cmd` | `#[tracing::instrument(skip(state), fields(session_id))]` |
| `spawn_active` | `#[tracing::instrument(skip(pool))]` — `session_id` 是 `String`，自动捕获 |
| `consume_stream` | `#[tracing::instrument(skip(agent, bus, pool, agents, cancel))]` — `session_id` 自动捕获 |
| `detect_agents_cmd` | `#[tracing::instrument(skip(state))]` |

**skip 原则**：仅 skip 非业务数据——`state`（Tauri `State` 包装器）、`pool`（`SqlitePool`）、IO 句柄。业务参数（`message`、`prompt`）不 skip，完整记录。

**新增 Tauri command 时**：如果函数有 `session_id` 参数，加 `#[tracing::instrument(skip(state), fields(session_id))]`；如果没有，加 `#[tracing::instrument(skip(state))]`。

## 4. 结构化字段

### 会话级字段（span 自动传播）

| 字段 | 类型 | 说明 |
|------|------|------|
| `session_id` | `Display` | Friday 会话 ID，贯穿诊断全流程 |
| `oc_id` | `Display` | opencode 子进程会话 ID |

**未来扩展**（exec 层实现后加入 span）：`target_host`（`Display`）、`env_id`（`Display`）。

### 事件级字段（手动写入）

| 字段 | 类型 | 适用场景 |
|------|------|----------|
| `pid` | `Display` | 进程操作 |
| `line_count` | `Display` | 流处理 |
| `exit_ok` | `bool` | 进程退出 |
| `status` | `Debug` | 进程退出码 |
| `raw` | `Display` | NDJSON 行（完整，不截断） |
| `attempt` | `Display` | 重试 |
| `elapsed_ms` | `Display` | 耗时 |
| `tool_name` | `Display` | 工具调用 |
| `error` | `Debug` (`?`) | 错误对象 |

### 字段格式规范

- ID 类用 `Display`（`%session_id`），不输出 Debug 全结构
- 错误类用 `Debug`（`?e`），保留完整错误链
- 布尔值直接用 `bool`
- 路径类用 `Display`（`%path`）
- **不截断**——所有日志内容完整输出
- 字段命名：`snake_case`

## 5. 埋点要求

### 新增 Tauri command 时

每个 command 必须有入口日志。优先用 `#[instrument]`（自动记录函数名和参数）；如果函数无 `session_id` 参数或参数是 `Option`，加手动 `info!` 记录实际 session_id。

### 新增 async 函数时

如果函数是关键路径（会被 command 或 `consume_stream` 调用链触及），加 `#[instrument]`。叶子工具函数（纯计算、无 IO）不需要。

### 错误路径

每个 `map_err` / `?` 返回错误的位置，如果错误意味着操作失败（非预期），必须有 `tracing::error!(?e, ...)` 或 `tracing::warn!(?e, ...)`。

### 子进程 stderr

任何通过 `tokio::process::Command` 启动的子进程，其 stderr 必须被读取并记录（`warn!` 级别），不允许用 `..` 丢弃。

### panic hook

已通过 `infra/logging.rs` 的 `init()` 全局安装。panic 会自动写入 `tracing::error!`。新增代码无需额外处理 panic。

## 6. 动态级别

运行时可通过 `set_log_level_cmd` Tauri command 调整日志级别，无需重启。前端通过 `ipc.ts` 的 `setLogLevel(level)` 调用。级别变更本身会记录一条 INFO 日志（含旧级别和新级别）。

## 7. 不做的事

- **不做脱敏**：Friday 运行于内网环境，密码、私钥路径等可入日志。如未来部署场景变更，需重新评估此项。
- **不截断**：NDJSON 原始行、用户输入、prompt 内容均完整记录。
- **不引入前端日志转发**：前端 `console.error` 维持现状，不纳入 tracing。如需统一，另立规范。
- **不拆分 logging 模块**：`infra/logging.rs` 单文件维护，不过早抽象。

## 8. 与架构文档的关系

- `docs/architecture/infrastructure.md` 的"日志与可观测"章节是简要概述，本文档是详细规范。
- `docs/architecture/infrastructure.md:14` 提到"敏感信息不入日志"——本规范决定**不执行脱敏**（见第 7 节），以内网部署为前提。
- 设计决策记录见 `docs/superpowers/specs/2026-08-21-logging-standard-design.md`。
