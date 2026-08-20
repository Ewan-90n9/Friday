# 任务计划：Friday 日志规范实现

## 目标
在现有 tracing 日志基础设施上落地日志规范：动态级别调整、7 天文件清理、panic hook、stderr 捕获、关键入口 `#[instrument]`、去截断、全 command 埋点。

## 当前阶段
阶段 1

## 各阶段

### 阶段 1：logging.rs 核心重写
- [ ] 新增 `LoggingGuard` 结构体（持有 `WorkerGuard` + `reload::Handle<EnvFilter>`）
- [ ] 用 `reload::Layer` 包装 `EnvFilter`，`init()` 返回 `LoggingGuard`
- [ ] 新增 `cleanup_old_logs(log_dir, max_days: u64)` — 删除 7 天前的日志文件
- [ ] 新增 `set_level(handle, level: &str)` — 动态替换 EnvFilter，成功后 info! 记录变更
- [ ] 新增 panic hook — `std::panic::set_hook`，写 `tracing::error!` + 调用原 hook
- [ ] 更新单元测试：init 返回类型变化、set_level 生效、cleanup_old_logs 删除旧文件
- **状态：** pending

### 阶段 2：lib.rs + AppState 集成
- [ ] `AppState` 新增 `filter_handle: reload::Handle<EnvFilter>` 字段
- [ ] `setup` 中 `init()` 返回 `LoggingGuard`，拆分：`_file_guard` 经 `app.manage()` 保活，`filter_handle` 存入 `AppState`
- [ ] 新增 `set_log_level_cmd(level: String) -> Result<(), String>` Tauri command（放在 `app/lifecycle.rs`）
- [ ] 注册 `set_log_level_cmd` 到 `invoke_handler`
- **状态：** pending

### 阶段 3：agent/stream.rs — stderr 捕获 + instrument + 去截断
- [ ] `consume_stream` 加 `#[instrument(skip(agent, bus, pool, agents, cancel), fields(session_id))]`
- [ ] 解构 `AgentProcess` 时不再用 `..` 忽略 `stderr`，改为 `let AgentProcess { mut child, stdout, stderr } = agent;`
- [ ] 在 stdout 循环前 spawn 一个独立 tokio task 读取 stderr，每行 `warn!`（带 `session_id`）
- [ ] 函数末尾 await stderr task handle，确保 stderr 读完再返回
- [ ] `debug!` 行去掉 `.min(200)` 截断，改为 `raw = %line`
- **状态：** pending

### 阶段 4：agent/spawn.rs — instrument + session_id 参数
- [ ] `spawn_active` 函数签名新增 `session_id: String` 参数（位于 `pool` 之后、`message` 之前）
- [ ] 加 `#[instrument(skip(pool), fields(session_id))]`（`message` 不 skip，完整记录）
- [ ] **注意**：调用方 `lifecycle.rs:114` 需同步传入 `friday_session_id.clone()`
- [ ] `#[cfg(test)]` 中的 `spawn_active` 调用需同步加参数
- **状态：** pending

### 阶段 5：app/lifecycle.rs — instruments + entry logs + spawn_active 调用适配
- [ ] `send_message_cmd` 加 `#[instrument(skip(state))]`（不声明 fields(session_id)，因为参数是 Option<String>；内部 info! 保留 session_id=%friday_session_id）
- [ ] 移除 `send_message_cmd` 入口处手动 `info!("send_message_cmd called")`（instrument 自动记录）
- [ ] `stop_agent_cmd` 加 `#[instrument(skip(state), fields(session_id))]`
- [ ] `close_session_cmd` 加 `#[instrument(skip(state), fields(session_id))]`
- [ ] `list_sessions_cmd` 加 INFO 入口日志
- [ ] `confirm_tool_cmd` 加 INFO 入口日志（含 session_id、tool）
- [ ] `spawn_active` 调用处传入 `friday_session_id.clone()`
- [ ] 新增 `set_log_level_cmd` command
- **状态：** pending

### 阶段 6：app/agents.rs — instrument + entry logs
- [ ] `detect_agents_cmd` 加 `#[instrument(skip(state))]`
- [ ] `list_agents_cmd` 加 INFO 入口日志
- [ ] `add_agent_cmd` 加 INFO 入口日志（含 provider、path）
- [ ] `set_active_agent_cmd` 加 INFO 入口日志（含 id）
- [ ] `remove_agent_cmd` 加 INFO 入口日志（含 id）
- **状态：** pending

### 阶段 7：app/events.rs — emit debug 日志
- [ ] `EventBus::emit` 成功路径加 `debug!`（事件类型 + session_id）
- **状态：** pending

### 阶段 8：前端 ipc.ts — setLogLevel 绑定
- [ ] 新增 `setLogLevel(level: string): Promise<void>` 函数
- **状态：** pending

### 阶段 9：验证
- [ ] `cargo check --manifest-path src-tauri/Cargo.toml`
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] `pnpm typecheck`
- **状态：** pending

## 关键问题
1. `spawn_active` 无 `session_id` 参数 → 新增参数，调用方同步适配（阶段 4+5）
2. `consume_stream` 的 `agent/bus/pool/agents/cancel` 都不实现 Debug → instrument 需 skip 全部，spec 原文 `skip(stdout)` 不够
3. `send_message_cmd` 的 `session_id` 是 `Option<String>` → instrument 不声明 fields(session_id)，靠内部 info! 记录实际 ID
4. `reload::Handle<EnvFilter>` 是否 Send+Sync → 是（内部 Arc），可存入 AppState

## 已做决策
| 决策 | 理由 |
|------|------|
| 不拆分 logging.rs 模块 | 23 行→~100 行，不值得过早抽象 |
| 不截断日志 | 用户要求日志尽量详细 |
| 不做脱敏 | 内网环境，无信息安全需求 |
| spawn_active 新增 session_id 参数 | 让 span 能传播 session_id |
| set_log_level_cmd 放在 lifecycle.rs | 与其他 app 级 command 一致 |

## 遇到的错误
| 错误 | 尝试次数 | 解决方案 |
|------|---------|---------|
|      | 1       |         |

## 备注
- 随着进度更新阶段状态：pending → in_progress → complete
- 做重大决策前重新读取此计划（注意力操纵）
- 记录所有错误，避免重复
- spec 文件：`docs/superpowers/specs/2026-08-21-logging-standard-design.md`
