# Agent 对话管道 — 设计文档

- 日期：2026-08-20
- 状态：已实现并验证
- 前置：[Agent 自动识别设计](2026-08-20-agent-detection-design.md)（已完成，spawn_active 可读 DB active agent）

## 1. 背景与目标

Agent 自动识别功能已落地——检测 opencode、持久化到 SQLite、UI 可切换。当前从"用户发消息"到"agent 在 UI 中回复"的整条链路均为 `todo!()`：`spawn_active` 不传参不读输出、`consume_stream` 为空、`build_prompt` 为空、`start_diagnosis_cmd` 及其余 4 个 lifecycle command 均未实现、前端 `MainDiagnosisArea` 是静态占位。

本功能实现**端到端对话管道**：用户输入消息 → spawn opencode 子进程 → 流式解析 JSON 输出 → 推 AppEvent 到前端 → 渲染对话。支持多轮对话（续聊同一会话）。

### v1 范围

- 对话管道本身：会话管理、prompt 组装、spawn opencode `run --format json`、流式解析、前端对话 UI
- opencode 使用自带工具（bash/read/edit），用 `--dangerously-skip-permissions` 自动批准权限
- 多轮对话：首条消息创建 opencode session，后续消息用 `-s <session_id>` 续聊

### 不做（YAGNI）

- 不做 Friday MCP 诊断工具层（jstat/jcmd/arthas 等）——后续独立功能
- 不做 SSH/K8s 执行层——同上
- 不做 playbook/知识层——同上
- 不做 ConfirmRequired 交互流程——v1 用 `--auto`
- 不做对话消息持久化到 SQLite——消息流式推送即丢弃，会话只存元数据
- 不做会话历史恢复（通过 `opencode export` 恢复消息）——后续演进
- 不做 `cancel_diagnosis_cmd`——与 `stop_agent_cmd` 重复，移除

## 2. 架构与模块归属

在现有 `agent/` 和 `app/` 层内实现，不新增层级。

```
agent/
  mod.rs        ← 不变
  registry.rs   ← 不变
  detect.rs     ← 不变
  spawn.rs      ← 改（构造 opencode run 参数，pipe stdout/stderr）
  stream.rs     ← 改（NDJSON 解析 → AppEvent 映射 + 进程生命周期）
  prompt.rs     ← 改（v1 直接返回消息，预留扩展）
app/
  mod.rs        ← 不变
  session.rs    ← 改（实现 create/list/close/get/update_oc_session_id）
  lifecycle.rs  ← 改（实现 send_message_cmd / stop_agent_cmd / close_session_cmd / list_sessions_cmd）
  events.rs     ← 不变（AppEvent 类型已定义，无需新增）
  agents.rs     ← 不变
  credentials.rs← 不变
infra/
  db.rs         ← 改（加载 0003 迁移）
migrations/
  0003_conversation.sql ← 新（sessions 表加列）
```

**集成点：**
- `lib.rs`：AppState 新增 `agents` 字段（进程管理 map），handler 注册变更
- 前端：IPC 绑定、types、sessionStore、组件全面改造

## 3. IPC 契约变更

### 3.1 命令变更

| command | 入参 | 出参 | 行为 |
|---|---|---|---|
| `send_message_cmd` | `session_id: Option<String>, message: String` | `String`（session_id） | session_id=None → 创建新会话；Some → 续聊。spawn opencode，立即返回 session_id |
| `stop_agent_cmd` | `session_id: String` | `()` | kill opencode 子进程，会话保持 active |
| `close_session_cmd` | `session_id: String` | `()` | stop agent（如有）+ 标记 closed |
| `list_sessions_cmd` | 无 | `Vec<SessionRow>` | 读 sessions 表，供侧边栏展示 |
| `confirm_tool_cmd` | `session_id, tool` | `()` | v1 不实现，返回 `Ok(())`（保留注册，后续启用） |

**移除**：`cancel_diagnosis_cmd`（与 `stop_agent_cmd` 重复）、`start_diagnosis_cmd`（被 `send_message_cmd` 替代）。

### 3.2 前端 IPC 绑定

```ts
// 替换 startDiagnosis
export async function sendMessage(sessionId: string | null, message: string): Promise<string> {
  return invoke<string>("send_message_cmd", { sessionId, message });
}
// 新增
export async function listSessions(): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions_cmd");
}
// stopAgent / closeSession 保持不变
// 移除 cancelDiagnosis（命令已删除）
```

## 4. 数据模型

### 4.1 迁移 `0003_conversation.sql`

```sql
ALTER TABLE sessions ADD COLUMN opencode_session_id TEXT;
ALTER TABLE sessions ADD COLUMN title TEXT;
```

- `opencode_session_id`：首条消息时从 opencode `session.created` 事件提取并回写，后续消息用 `-s <oc_session_id>` 续聊
- `title`：取首条消息前 40 字符，供侧边栏显示
- `env`/`service`/`symptom`：保留列但 v1 存空字符串（不破坏 NOT NULL 约束，后续诊断工具层再用）
- `diagnosis_steps`/`tool_calls` 表：v1 不使用，保留不动

### 4.2 SessionRow（serde，前端用）

```rust
#[derive(Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub status: String,           // "active" | "closed"
    pub created_at: String,
}
```

### 4.3 会话消息不持久化

v1 不把对话历史存入 SQLite。消息流式推送——事件推到前端渲染，进程退出即结束。重新打开会话时消息列表为空（后续可通过 `opencode export` 恢复历史，不在 v1 范围）。

## 5. opencode 调用（`agent/spawn.rs`）

### 5.1 签名变更

```rust
pub async fn spawn_active(
    pool: &SqlitePool,
    message: String,
    opencode_session_id: Option<String>,
) -> Result<AgentProcess, SpawnError>
```

### 5.2 命令构造

```
opencode run --format json --dangerously-skip-permissions [--session <opencode_session_id>]
```

prompt 通过 **stdin** 传递（非命令行参数，避免 Windows argv 截断），stdin 写完后关闭。

- `--format json`：stdout 输出 NDJSON（每行一个事件对象）
- `--dangerously-skip-permissions`：自动批准工具权限（`--auto` 不是有效参数）
- `--session`：仅续聊时传（注意是 `--session` 不是 `-s`）
- stdout/stderr/stdin 均 `Stdio::piped()`
- Windows 上解析到 native `opencode.exe`（绕过 `.cmd`/`.ps1` shim，避免 argv 截断）
- 工作目录设为用户 home 目录（避免加载宿主项目的 AGENTS.md / .opencode 配置）
- 不传 MCP config（v1 无 MCP 工具）

### 5.3 AgentProcess 改造

```rust
pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
    pub stdout: ChildStdout,
}
```

`spawn_active` 在 spawn 后 `child.stdout.take()` 取出 stdout handle，与 child 一并返回。`consume_stream` 消费 stdout，`stop_agent_cmd` 通过 CancellationToken 操作 child。

## 6. 流式解析（`agent/stream.rs`）

### 6.1 NDJSON → AppEvent 映射

opencode `run --format json` 输出**扁平格式**的事件（非 server SDK 的嵌套格式），每行一个 JSON 对象：

```json
{"type":"step_start", "sessionID":"ses_abc", "part":{...}}
{"type":"text", "sessionID":"ses_abc", "part":{"type":"text", "text":"回复内容"}}
{"type":"tool_use", "sessionID":"ses_abc", "part":{"tool":"bash", "state":{"status":"completed", ...}}}
{"type":"step_finish", "sessionID":"ses_abc", "part":{"reason":"stop", ...}}
```

用 `serde_json::Value` 灵活解析，按顶层 `type` 分发：

| opencode 事件 | Friday AppEvent | 提取字段 |
|---|---|---|
| `text` | `LlmThinking { token }` | `part.text` |
| `reasoning` | `LlmThinking { token }` | `part.text` |
| `tool_use` (state.status=running) | `ToolExecuting { tool, args }` | `part.tool`, `part.state.input` |
| `tool_use` (state.status=completed) | `ToolResult { tool, output, elapsed_ms }` | `part.tool`, `part.state.output`, `part.state.time` |
| `tool_use` (state.status=error) | `ToolResult { tool, output: error, elapsed_ms }` | `part.tool`, `part.state.error` |
| `error` | `AgentCrashed { reason }` | `error.data.message` |
| `step_start` | 忽略 | — |
| `step_finish` | 忽略 | — |
| stdout EOF + exit 0 | `DiagnosisDone { conclusion: "" }` | 结论已通过 text 事件流式推送 |
| stdout EOF + exit ≠0 | `AgentCrashed { reason }` | exit code |

**session ID 提取**：每个事件都有顶层 `sessionID` 字段，从中提取 opencode session ID 回写 DB。

**参考**：对齐 multica 的 `opencodeBackend.processEvents` 实现。

### 6.2 进程管理与取消模型

用 `tokio_util::sync::CancellationToken` 管理进程生命周期：

```rust
pub struct RunningAgent {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}
```

AppState 新增字段：
```rust
pub agents: Arc<Mutex<HashMap<String, RunningAgent>>>,  // session_id → RunningAgent
```

**`consume_stream` 函数签名：**

```rust
async fn consume_stream(
    child: Child,                    // 拥有子进程（用于 wait/kill）
    stdout: ChildStdout,             // NDJSON 输出流
    bus: EventBus,                   // 事件推送
    pool: SqlitePool,                // 回写 opencode_session_id
    session_id: String,
    agents: Arc<Mutex<HashMap<String, RunningAgent>>>,  // 退出时移除自身
    cancel: CancellationToken,        // stop_agent_cmd 触发
)
```

**`consume_stream` 核心循环：**

```rust
loop {
    tokio::select! {
        line = reader.next_line() => {
            match line {
                Ok(Some(line)) => parse_and_emit(line, &bus, &session_id, &pool).await,
                Ok(None) => break,  // EOF → 自然结束
                Err(e) => break,    // 读取错误
            }
        }
        _ = cancel.cancelled() => {
            child.kill().await.ok();
            bus.emit(&session_id, AppEvent::AgentStopped { session_id: session_id.clone() });
            // 从 agents map 移除自身条目
            remove_from_map(&agents, &session_id).await;
            return;
        }
    }
}
// 自然结束
let status = child.wait().await?;
if status.success() {
    bus.emit(&session_id, AppEvent::DiagnosisDone { session_id: session_id.clone(), conclusion: String::new() });
} else {
    bus.emit(&session_id, AppEvent::AgentCrashed { session_id: session_id.clone(), reason: format!("exit code {}", status.code().unwrap_or(-1)) });
}
remove_from_map(&agents, &session_id).await;
```

**并发安全**：`Arc<Mutex<HashMap>>` 保护进程 map；`SqlitePool` 多连接并发安全；`EventBus` 内部 `AppHandle` 可安全 clone。

## 7. 会话管理（`app/session.rs`）

```rust
pub async fn create_session(pool: &SqlitePool, message: &str) -> Result<Session, sqlx::Error>
// id = UUID, title = message 前 40 字符
// env/service/symptom = "" (保 NOT NULL), status = "active"
// opencode_session_id = NULL

pub async fn close_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error>
// UPDATE sessions SET status='closed', closed_at=<ISO8601> WHERE id=?

pub async fn list_sessions(pool: &SqlitePool) -> Result<Vec<SessionRow>, sqlx::Error>
// SELECT id, title, status, created_at FROM sessions ORDER BY created_at DESC

pub async fn get_session(pool: &SqlitePool, id: &str) -> Result<Option<SessionRow>, sqlx::Error>

pub async fn update_opencode_session_id(pool: &SqlitePool, id: &str, oc_id: &str) -> Result<(), sqlx::Error>
// UPDATE sessions SET opencode_session_id=? WHERE id=?
```

时间字段存 ISO 8601 字符串，与现有 `sessions.created_at` 及 `agents.detected_at` 一致。

## 8. 生命周期命令（`app/lifecycle.rs`）

### 8.1 `send_message_cmd`

```
1. session_id=None → create_session(message) → 新 Friday session
   session_id=Some → get_session(id) → 取 opencode_session_id
   └ 会话不存在 → 返回错误 "会话不存在"
   └ 会话 closed → 返回错误 "会话已关闭"
2. 查 agents map → 该 session 已有运行中 agent → 返回错误 "agent 正在运行"
3. spawn_active(pool, message, oc_session_id) → AgentProcess
4. 从 AgentProcess 取 stdout
5. emit AgentStarted { session_id, agent_pid }
6. 创建 CancellationToken，spawn consume_stream 后台任务
7. 存 RunningAgent { cancel, handle } 到 agents map
8. 立即返回 session_id
```

### 8.2 `stop_agent_cmd`

1. 锁 agents map，取 `RunningAgent`（不存在 → agent 已结束，返回 Ok）
2. `cancel.cancel()` + `handle.await()`
3. 返回 Ok

### 8.3 `close_session_cmd`

1. 如有运行中 agent → stop（同 `stop_agent_cmd` 逻辑）
2. `close_session()` → DB 标记 closed
3. emit `SessionClosed`
4. 返回 Ok

### 8.4 `list_sessions_cmd`

调 `session::list_sessions(pool)`，返回 `Vec<SessionRow>`。

## 9. lib.rs 变更

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, RunningAgent>>>,  // 新增
}
```

handler 注册变更：
- `send_message_cmd` 替换 `start_diagnosis_cmd`
- 新增 `list_sessions_cmd`
- 移除 `cancel_diagnosis_cmd`
- `confirm_tool_cmd` 保留（改为返回 `Ok(())`）
- `stop_agent_cmd`、`close_session_cmd` 保留（实现）

`setup` 初始化：`detect_and_persist` 之后，`app.manage(AppState)` 时传入 `Arc::new(Mutex::new(HashMap::new()))`。

## 10. prompt 组装（`agent/prompt.rs`）

注入 Friday 专属人格 system prompt，定义身份、能力、风格、限制：

```rust
pub fn build_prompt(message: &str) -> String {
    format!("{system}\n\n---\n\n用户消息：{message}", system = FRIDAY_SYSTEM_PROMPT, message = message)
}
```

**Friday 人格要点：**
- 名字是 Friday，不提底层模型名称
- 面向运行时故障诊断，不是通用聊天
- 简洁直接，中文交流
- 诚实告知能力边界

后续诊断工具层启用时扩展为 `build_prompt(env, service, symptom, playbook_index)`，注入诊断上下文。

## 11. 前端

### 11.1 类型定义（`src/lib/types.ts`）

```ts
export interface SessionRow {
  id: string;
  title: string | null;
  status: "active" | "closed";
  created_at: string;
}

export interface ChatPart {
  type: "text" | "reasoning" | "tool";
  text?: string;           // text/reasoning
  tool?: {
    name: string;
    args: unknown;
    status: "running" | "completed" | "error";
    output?: string;
    elapsedMs?: number;
  };
}

export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  content: string;          // user 消息原文
  parts: ChatPart[];        // agent 消息的组成部分
  status: "streaming" | "done" | "stopped" | "error";
}
```

### 11.2 sessionStore 改造

```ts
interface SessionStore {
  sessions: SessionRow[];
  currentSessionId: string | null;
  messagesBySession: Record<string, ChatMessage[]>;
  agentRunning: Record<string, boolean>;

  loadSessions: () => Promise<void>;
  selectSession: (id: string) => void;
  newSession: () => void;
  sendMessage: (message: string) => Promise<void>;
  stopAgent: () => Promise<void>;
  handleEvent: (payload: EventPayload) => void;
}
```

**`handleEvent` 逻辑：**
- `agent_started` → `agentRunning[sessionId] = true`，创建新 AgentMessage（status: streaming）
- `llm_thinking` → 追加 `token` 到当前 AgentMessage 的末尾 text part（或创建新 text part）
- `tool_executing` → 在当前 AgentMessage 添加 ToolCallPart（status: running）
- `tool_result` → 更新对应 ToolCallPart（status + output + elapsedMs）
- `diagnosis_done` → `agentRunning[sessionId] = false`，AgentMessage status = done
- `agent_stopped` → `agentRunning[sessionId] = false`，AgentMessage status = stopped
- `agent_crashed` → `agentRunning[sessionId] = false`，AgentMessage status = error
- `session_closed` → 更新 session status

### 11.3 组件结构

| 文件 | 动作 | 内容 |
|------|------|------|
| `src/lib/types.ts` | 改 | +`SessionRow`、+`ChatMessage`、+`ChatPart` |
| `src/lib/ipc.ts` | 改 | `sendMessage` 替换 `startDiagnosis`，+`listSessions` |
| `src/store/sessionStore.ts` | 改 | 会话列表、消息累积、agent 运行状态、事件处理 |
| `src/components/layout/SessionSidebar.tsx` | 改 | 会话列表渲染 + 切换 + 新建 |
| `src/components/layout/MainDiagnosisArea.tsx` | 改 | 挂载 MessageList + InputArea |
| `src/components/chat/MessageList.tsx` | 新 | 消息列表，自动滚动到底 |
| `src/components/chat/UserMessage.tsx` | 新 | 用户消息气泡（右对齐） |
| `src/components/chat/AgentMessage.tsx` | 新 | Agent 消息容器（推理 + 文本 + 工具卡片） |
| `src/components/chat/ToolCallCard.tsx` | 新 | 工具卡片（折叠/展开 + 状态 badge + 输出区） |
| `src/components/chat/InputArea.tsx` | 新 | 输入区（textarea + 停止按钮 + 发送按钮） |

### 11.4 渲染约定（对齐设计语言）

- **用户消息**：右对齐气泡，`bg-surface-2` + `border-border` + `rounded-xl`（右下角小圆角），`--text-sm` / `--font-sans`
- **Agent 推理**：可折叠块，`bg-surface-1` + `--font-mono` + `--muted-foreground` / `--text-xs`
- **Agent 文本**：`--font-sans` + `--foreground` / `--text-base`，流式渲染（逐 token 追加）
- **Agent 思考中**（未输出文本时）：`--font-mono` + `--muted-foreground` + 光标闪烁
- **工具卡片**：`bg-card` + `border-border` + `rounded-lg`
  - 工具名 badge：`--font-mono` / `--text-xs` / `bg-success/10` + `text-success`
  - 运行中：spinner + "执行中..."（`--accent`）
  - 完成：✓ + 耗时（`--success`）
  - 失败：✗ + 错误摘要（`--destructive`）
  - 输出区：可折叠，`--font-mono` / `--text-xs` / `--muted-foreground`
- **输入区**：底部固定，`bg-surface-1` + `border-border` + `rounded-xl`
  - Agent 运行时：显示"停止"按钮（`--destructive`），输入框 placeholder "补充信息..."
  - Agent 空闲时：仅发送按钮，placeholder "描述环境、服务和症状…"
  - Enter 发送 / Shift+Enter 换行
- **会话列表项**：`--text-sm` / `--font-sans`，状态点（`--success` 脉冲 / `--muted-foreground` 静态），选中项左侧 2px `--success` 边条 + `bg-surface-2`
- **自动滚动**：新消息进入时滚动到底部；用户手动上滚时不强制下拉（检测 scroll position）
- **动画**：新消息 `translateY(8px→0) + opacity(0→1)`，250ms `--ease-out`；卡片展开 `height + opacity`，200ms；尊重 `prefers-reduced-motion`

### 11.5 事件监听挂载

`DiagnosisPage` 挂载时：
1. `agentStore.refresh()`（已有）
2. `sessionStore.loadSessions()`（新）
3. `onAppEvent(handler)` → `sessionStore.handleEvent(payload)`（已有 IPC 绑定，接入 store）

## 12. 测试策略

### 12.1 后端单测

- **`agent/spawn.rs`**：测 `NoActiveAgent` 错误路径（已有，保持通过）；不 spawn 真实 opencode
- **`agent/stream.rs`**：
  - 喂构造的 NDJSON 字符串样本（模拟 `session.created`、`message.part.updated` 各类型、`session.idle`），验证 AppEvent 映射正确
  - 测 `session.created` 事件提取 opencode session ID
  - 测 EOF + exit 0 → `DiagnosisDone`，EOF + exit ≠0 → `AgentCrashed`
  - 测 cancel → `AgentStopped`
  - 不依赖真实 opencode 进程
- **`app/session.rs`**：用 `tempfile` + in-memory SQLite（沿用 `db::init` 测试模式）测：
  - `create_session`：插入成功，title 正确截断
  - `close_session`：status 变 closed，closed_at 非空
  - `list_sessions`：按 created_at 降序
  - `update_opencode_session_id`：字段正确更新

### 12.2 前端

- 手动验证：`pnpm tauri dev` → 发消息 → 观察流式渲染、工具卡片、多轮续聊、停止/关闭
- `pnpm typecheck` 通过

### 12.3 集成验证

- 首条消息 → opencode 创建 session → `session.created` 事件 → opencode_session_id 回写 DB
- 第二条消息 → `-s <oc_session_id>` 续聊 → opencode 保留上下文
- 停止 → `AgentStopped` 事件 → 前端停止渲染 → 再发消息 → 新 spawn
- 关闭会话 → `SessionClosed` → 侧边栏状态更新

## 13. 新增依赖

```toml
tokio-util = "0.7"    # CancellationToken
uuid = "1"            # session ID 生成
```

`serde_json` 已在依赖中（用于流式解析）。`tokio` 已有（async runtime）。

## 14. 实现顺序建议

1. 迁移 `0003_conversation.sql` + `db.rs` 加载
2. `app/session.rs` 实现 + 单测
3. `agent/spawn.rs` 改造（构造 opencode run 参数）
4. `agent/stream.rs` 实现 NDJSON 解析 + 单测
5. `app/lifecycle.rs` 实现 `send_message_cmd` / `stop_agent_cmd` / `close_session_cmd` / `list_sessions_cmd`
6. `lib.rs` 更新 AppState + handler 注册
7. `cargo check` + `cargo test` 通过
8. 前端 types + ipc + sessionStore
9. 前端组件（MessageList / UserMessage / AgentMessage / ToolCallCard / InputArea）
10. SessionSidebar + MainDiagnosisArea 改造
11. `pnpm typecheck` 通过
12. `pnpm tauri dev` 端到端验证
