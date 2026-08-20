# Friday 日志规范设计

> 日期：2026-08-21
> 状态：已确认，待实现

## 1. 背景与动机

Friday 是远程环境运行时故障诊断 Agent。随着功能膨胀（SSH 执行层、工具调用、凭证管理即将实现），当前日志能力不足以支撑问题定位：

- 无 `#[instrument]` / span 层次，多会话并发时只能靠 `grep session_id` 手动串联
- opencode 子进程 **stderr 被丢弃**（`stream.rs:190` 解构时用 `..` 忽略）
- 多数 Tauri command **无埋点**（仅 `send_message_cmd` 有）
- 无 panic hook——panic 不写日志文件
- 无日志保留策略——文件无限累积
- 无运行时级别调整——排障需重启

本规范针对 **tracing 运行日志**（`logs/friday.log`），不覆盖诊断数据持久化（`diagnosis_steps` / `tool_calls` 表，另立规范）。

## 2. 决策汇总

| 维度 | 决策 | 理由 |
|------|------|------|
| 范围 | 仅 tracing 运行日志 | 诊断数据持久化另立规范 |
| 格式 | 人类可读（维持 `fmt::layer()` 默认） | 开发期直接可读，后续接 ELK 时再加 JSON 层 |
| Span | 关键入口 `#[instrument]` | 平衡可观测深度与噪音 |
| 保留 | 7 天自动清理 | 开发期磁盘可控 |
| 脱敏 | **不做** | Friday 运行在内网，无信息安全需求 |
| 动态级别 | 支持运行时调整 | 用户反馈问题时无需重启即可抓详细日志 |
| 截断 | **不截断** | 日志尽量详细，完整记录所有内容 |

### 与架构文档的偏差

`docs/architecture/infrastructure.md:14` 约定"敏感信息（密码、私钥路径）不入日志"。本规范决定**不执行脱敏**，理由：Friday 运行于内网环境，无信息安全需求。如未来部署场景变更，需重新评估此项。

## 3. 架构

在现有 `infra/logging.rs`（23 行）基础上就地扩展，不拆分模块。

```
infra/logging.rs
  ├─ init()           — 现有：双 layer（stdout + 文件）、EnvFilter、WorkerGuard
  │    + reload::Handle<EnvFilter>  — 新增：返回 Handle，供运行时动态调级
  │    + 7 天文件清理               — 新增：init 后清理旧文件
  │    + panic hook                 — 新增：panic 写入 tracing::error!
  │
  └─ init() 返回值
       旧: WorkerGuard
       新: LoggingGuard { _file_guard: WorkerGuard, filter_handle: reload::Handle<EnvFilter> }
```

**stderr 捕获**不属于 logging 模块本身，而是 `agent/stream.rs` 的改动——`consume_stream` 改为读取 stderr 并以 `warn!` 记录。

**不改动**：日志格式（`fmt::layer()` 默认）、输出目标（stdout + 每日轮转 `logs/friday.log`）、`RUST_LOG` 启动配置优先级。

## 4. 日志级别约定

| 级别 | 语义 | 典型内容 |
|------|------|----------|
| **ERROR** | 需人为介入的故障。操作失败且无法自动恢复。 | spawn 失败、IPC emit 失败、子进程非零退出、panic |
| **WARN** | 可恢复的异常或非预期状态。操作降级但未中断。 | opencode stderr 输出、SSH 重试、连接断开重连、工具超时取消 |
| **INFO** | 关键生命周期事件。一条 INFO 应能回答"发生了什么"。 | command 入口、session 创建/恢复、spawn 成功、stdout EOF、进程退出、级别调整 |
| **DEBUG** | 排障细节。开发期默认级别。 | NDJSON 原始行（完整）、event 类型、SQL 变更细节 |
| **TRACE** | 极细粒度数据流。默认关闭，临时开启。 | 单条 NDJSON 完整内容、stdin 写入字节、span enter/exit |

**默认级别**：`debug`（与当前实现一致）。生产环境通过 `RUST_LOG=info` 降级。

**级别选择规则**：如果一条日志在正常流程中也会触发（如"session created"），用 INFO；如果只在异常路径触发，用 WARN 或 ERROR。如果拿不准——"这条日志在正常运行中出现是否意味着出了问题"——是则 WARN，否则 INFO。

**动态调整**：通过 `reload::Handle` 在运行时替换 `EnvFilter`。提供 Tauri command `set_log_level(level: String)`，前端设置页可调用。调整时发一条 INFO 日志记录变更（"log level changed: debug → trace"），确保级别变更本身可审计。

## 5. 结构化字段约定

### 会话级字段（span 自动传播）

通过 `#[instrument]` 在关键入口注入，子节点自动继承：

| 字段 | 类型 | 来源 | 说明 |
|------|------|------|------|
| `session_id` | `Display` | `send_message_cmd` / `consume_stream` / `stop_agent_cmd` / `close_session_cmd` | Friday 会话 ID，贯穿整个诊断流程 |
| `oc_id` | `Display` | `consume_stream` | opencode 子进程会话 ID |

**未来扩展**（exec 层实现后加入 span）：

| 字段 | 类型 | 来源 | 说明 |
|------|------|------|------|
| `target_host` | `Display` | SSH exec 入口 | 目标主机 IP/hostname |
| `env_id` | `Display` | 环境管理入口 | 环境标识 |

### 事件级字段（手动写入）

不通过 span 传播，按需写入：

| 字段 | 类型 | 适用场景 |
|------|------|----------|
| `pid` | `Display` | 进程操作 |
| `line_count` | `Display` | 流处理 |
| `exit_ok` | `bool` | 进程退出 |
| `status` | `Debug` | 进程退出码 |
| `raw` | `Display` | NDJSON 行（完整，不截断） |
| `attempt` | `Display` | 重试（未来） |
| `elapsed_ms` | `Display` | 耗时（未来） |
| `tool_name` | `Display` | 工具调用（未来） |
| `error` | `Debug` (`?`) | 错误对象 |

### 字段格式规范

- **ID 类**用 `Display`（`%session_id`）——输出短形式，不输出 Debug 全结构
- **错误类**用 `Debug`（`?e`）——保留完整错误链
- **布尔值**直接用 `bool`——输出 `true`/`false`
- **路径类**用 `Display`（`%path`）——输出路径字符串
- **不截断**——所有日志内容完整输出，包括 NDJSON 原始行、用户输入、prompt 内容
- **字段命名**：`snake_case`，与 tracing 惯例一致

### `#[instrument]` 放置规则

业务参数（`message`、`prompt`）不 skip，完整记录。仅 `state`（Tauri `State` 包装器，非业务数据）和 `stdout`（IO reader 句柄）保留 skip。

| 函数 | instrument 签名 | 备注 |
|------|----------------|------|
| `send_message_cmd` | `#[instrument(skip(state), fields(session_id))]` | `message` 不 skip |
| `stop_agent_cmd` | `#[instrument(skip(state), fields(session_id))]` | 新增 |
| `close_session_cmd` | `#[instrument(skip(state), fields(session_id))]` | 新增 |
| `spawn_opencode` | `#[instrument(fields(session_id))]` | `prompt` 不 skip |
| `consume_stream` | `#[instrument(skip(stdout), fields(session_id))]` | `stdout` 是 BufReader 句柄 |
| `detect_agents_cmd` | `#[instrument(skip(state))]` | 无 session 上下文 |

## 6. 各层埋点清单

### 基础设施层 `infra/`

| 位置 | 级别 | 内容 | 状态 |
|------|------|------|------|
| `logging.rs` init | INFO | `log_dir`、初始化完成 | ✅ 已有 |
| `logging.rs` panic hook | ERROR | panic 消息、location、backtrace | ➕ 新增 |
| `logging.rs` 级别调整 | INFO | 旧级别→新级别 | ➕ 新增 |
| `logging.rs` 文件清理 | DEBUG | 删除了哪些旧文件、保留数量 | ➕ 新增 |
| `db.rs` init | INFO | `db_path` | ✅ 已有 |
| `db.rs` migration | DEBUG | `table`、`column` | ✅ 已有 |

### 应用层 `app/`

| 位置 | 级别 | 内容 | 状态 |
|------|------|------|------|
| `lifecycle.rs` send_message_cmd | INFO | `session_id`、`message`（完整） | ✅ 有埋点，➕ 改 instrument |
| `lifecycle.rs` create session | INFO | 新建 session | ✅ 已有 |
| `lifecycle.rs` resume session | INFO | `session_id` | ✅ 已有 |
| `lifecycle.rs` spawn opencode | INFO | `session_id`、`prompt_len` | ✅ 已有 |
| `lifecycle.rs` spawn failed | ERROR | `?e` | ✅ 已有 |
| `lifecycle.rs` oc_id captured | INFO | `oc_id` | ✅ 已有 |
| `lifecycle.rs` stop_agent_cmd | INFO | `session_id` | ➕ 新增 instrument |
| `lifecycle.rs` close_session_cmd | INFO | `session_id` | ➕ 新增 instrument |
| `lifecycle.rs` list_sessions_cmd | INFO | — | ➕ 新增 |
| `lifecycle.rs` confirm_tool_cmd | INFO | `session_id`、tool 信息 | ➕ 新增 |
| `lifecycle.rs` detect_agents_cmd | INFO | — | ➕ 新增 instrument |
| `lifecycle.rs` list_agents_cmd | INFO | — | ➕ 新增 |
| `lifecycle.rs` add_agent_cmd | INFO | agent 信息 | ➕ 新增 |
| `lifecycle.rs` set_active_agent_cmd | INFO | agent 信息 | ➕ 新增 |
| `lifecycle.rs` remove_agent_cmd | INFO | agent 信息 | ➕ 新增 |
| `events.rs` emit failed | ERROR | `?e` | ✅ 已有 |
| `events.rs` emit 事件 | DEBUG | 事件类型、`session_id` | ➕ 新增 |
| `credentials.rs` | — | — | 🔜 凭证读取/刷新 |

### Agent 层 `agent/`

| 位置 | 级别 | 内容 | 状态 |
|------|------|------|------|
| `spawn.rs` exe resolved | INFO | `raw_path`、`exe_path` | ✅ 已有 |
| `spawn.rs` process spawned | INFO | `pid`、`exe` | ✅ 已有 |
| `spawn.rs` prompt written | INFO | `msg`（完整） | ✅ 已有 |
| `spawn.rs` write failed | ERROR | `?e` | ✅ 已有 |
| `spawn.rs` stdin closed | INFO | — | ✅ 已有 |
| `stream.rs` consume started | INFO | `session_id` | ✅ 已有 |
| `stream.rs` stdout line | DEBUG | `line_count`、`raw`（完整） | ✅ 有，➕ 去截断 |
| `stream.rs` oc_id captured | INFO | `oc_id` | ✅ 已有 |
| `stream.rs` event emitting | DEBUG | `event_type` | ✅ 已有 |
| `stream.rs` stdout EOF | INFO | `line_count`、`session_id` | ✅ 已有 |
| `stream.rs` read error | ERROR | `?e`、`line_count` | ✅ 已有 |
| `stream.rs` cancellation | INFO | `session_id` | ✅ 已有 |
| `stream.rs` process exited | INFO | `session_id`、`exit_ok`、`status` | ✅ 已有 |
| `stream.rs` **stderr 读取** | WARN | `session_id`、stderr 完整内容 | ➕ 新增 |

### 执行层 `exec/`（🔜 未来实现）

| 位置 | 级别 | 内容 |
|------|------|------|
| SSH connect | INFO | `target_host`、`port` |
| SSH connect retry | WARN | `target_host`、`attempt`、`?e` |
| SSH reconnect | WARN | `target_host`、`attempt` |
| SSH command exec | INFO | `target_host`、命令、`elapsed_ms` |
| SSH command stderr | WARN | `target_host`、stderr 内容 |
| K8s exec | INFO | `pod`、`namespace`、命令 |

### 工具层 `tools/`（🔜 未来实现）

| 位置 | 级别 | 内容 |
|------|------|------|
| 工具调用开始 | INFO | `tool_name`、`session_id`、参数 |
| 工具调用完成 | INFO | `tool_name`、`elapsed_ms`、`exit_ok` |
| 工具调用超时 | WARN | `tool_name`、`elapsed_ms` |
| 高风险工具执行 | WARN | `tool_name`、参数（审计用） |

### 前端

不纳入本规范。前端 `console.error` 维持现状。如需统一，未来另立前端日志规范。

## 7. 实现改动清单

### `src-tauri/src/infra/logging.rs`（核心改动，23 行 → ~100 行）

1. **返回值变化**：`init()` 返回 `LoggingGuard` 而非裸 `WorkerGuard`
   ```rust
   pub struct LoggingGuard {
       _file_guard: WorkerGuard,
       filter_handle: reload::Handle<EnvFilter>,
   }
   ```

2. **动态级别**：用 `reload::Layer` 包装 `EnvFilter`，返回 `Handle`。新增公开方法 `set_level(handle: &reload::Handle<EnvFilter>, level: &str)`——解析 level 字符串，构造新 `EnvFilter`，调用 `handle.reload()`，成功后 `info!` 记录变更。

3. **7 天文件清理**：`init()` 末尾调用 `cleanup_old_logs(&log_dir, 7)`。函数遍历 `log_dir`，删除修改时间超过 7 天的文件，`debug!` 记录删除了哪些。同步执行（文件少，无需异步），失败时 `warn!` 但不阻断启动。

4. **panic hook**：`init()` 中调用 `std::panic::set_hook`，将 panic message + location + backtrace 写入 `tracing::error!`，再调用原 hook（保持默认 stderr 输出）。

### `src-tauri/src/lib.rs`

`setup` 钩子中 `init()` 返回值从 `WorkerGuard` 改为 `LoggingGuard`，`app.manage(guard)` 不变（保活 `_file_guard`）。`filter_handle` 存入 `AppState`，供 command 通过 `State<AppState>` 访问。

### `src-tauri/src/app/lifecycle.rs`

1. `send_message_cmd`：加 `#[instrument(skip(state), fields(session_id))]`，移除入口处的手动 `info!`（`"send_message_cmd called"`，instrument 自动记录函数名和参数）。函数内更深层的手动 `info!`/`error!`（create session、resume、spawn opencode 等）全部保留，与 instrument 互补。
2. `stop_agent_cmd`：加 `#[instrument(skip(state), fields(session_id))]`
3. `close_session_cmd`：加 `#[instrument(skip(state), fields(session_id))]`
4. `detect_agents_cmd`：加 `#[instrument(skip(state))]`
5. `list_sessions_cmd`、`confirm_tool_cmd`、`list_agents_cmd`、`add_agent_cmd`、`set_active_agent_cmd`、`remove_agent_cmd`：各加一条 INFO 级别入口日志（参数完整记录）

### `src-tauri/src/app/events.rs`

emit 事件处加一条 `debug!`，记录事件类型和 `session_id`。

### `src-tauri/src/agent/spawn.rs`

加 `#[instrument(fields(session_id))]`（`prompt` 不 skip）。现有手动 `info!` 全部保留。instrument 自动记录函数入口/出口，手动 `info!` 记录函数内关键节点（exe resolved、process spawned 等），两者互补。

### `src-tauri/src/agent/stream.rs`

1. `consume_stream`：加 `#[instrument(skip(stdout), fields(session_id))]`
2. `stdout line` 的 `debug!`：去掉 `.min(200)` 截断，`raw = %line` 完整记录
3. **stderr 捕获**：`AgentProcess` 的 `stderr: ChildStderr` 不再被 `..` 忽略。`consume_stream` 中启动一个独立线程读取 stderr，每行 `warn!` 记录（带 `session_id`），EOF 后线程结束。stderr 线程与 stdout 循环并行。

### `src-tauri/Cargo.toml`

无需新增依赖。`tracing-subscriber` 已启用 `env-filter` feature，`reload` 是其内置模块，无需额外 feature flag。

### 新增 Tauri command

`set_log_level_cmd(level: String) -> Result<(), String>`：从 managed state 取 `filter_handle`，调用 `logging::set_level()`。注册到 `invoke_handler`。前端 `src/lib/ipc.ts` 同步加绑定。

### 改动量估算

| 文件 | 改动类型 | 规模 |
|------|----------|------|
| `infra/logging.rs` | 重写扩展 | ~100 行（现 23 行） |
| `lib.rs` | 返回值适配 | ~5 行 |
| `app/lifecycle.rs` | instrument + 埋点 | ~15 行 |
| `app/events.rs` | 加 debug | ~2 行 |
| `agent/spawn.rs` | instrument | ~1 行 |
| `agent/stream.rs` | 去截断 + stderr 线程 | ~20 行 |
| `ipc.ts` + command 注册 | 新增 set_log_level | ~10 行 |

## 8. 测试

- `logging.rs`：单元测试覆盖 init 成功、`set_level` 生效、`cleanup_old_logs` 正确删除旧文件且保留近 7 天
- `stream.rs`：stderr 捕获测试——构造带 stderr 输出的子进程，验证 `warn!` 日志产生
- instrument：手动验证 `logs/friday.log` 中 span 层次正确出现（`session_id` 自动传播）
