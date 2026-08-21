# 会话管理功能增强 — 设计文档

- 日期：2026-08-22
- 状态：待实现
- 前置：[Agent 对话管道设计](2026-08-20-agent-conversation-design.md)（已实现）

## 1. 背景与目标

当前对话管道已落地，但会话管理存在三个缺口：

1. **无法查看历史会话** — 消息不持久化，仅存于前端内存（`messagesBySession`）。重启应用或切换会话后消息丢失。原 v1 设计明确排除了消息持久化，现在需要补上。
2. **无归档能力** — 会话只有 `active` | `closed` 两种状态，所有会话平铺在侧边栏列表中，无法将已完成的会话归档隐藏。
3. **无删除能力** — 没有 `delete_session` 命令，会话数据永久留存，无法清理。

本功能实现：消息全量持久化、会话归档/取消归档、会话硬删除。

### 1.1 范围

- **消息持久化**：用户消息、Agent 文本输出、工具调用（含参数/输出/状态/耗时）全量存入 SQLite，重新打开会话时完整恢复
- **会话归档**：三态生命周期 `active` → `closed` → `archived`，侧边栏 toggle 切换主列表与归档列表
- **会话删除**：硬删除，手动级联删除消息和消息部分，确认弹窗防误删
- **取消归档**：从 `archived` 恢复到 `closed`

### 1.2 不做（YAGNI）

- 不做消息搜索（Ctrl+K 快速搜索）— 后续独立功能
- 不做会话重命名 — 后续功能
- 不做软删除 / 回收站 — 硬删除 + 确认弹窗已足够
- 不做消息编辑 / 重新生成 — 与 Agent 对话模型无关
- 不做批量操作（批量归档/批量删除）— 后续按需加

## 2. 持久化方案

采用 **后端在流式边界持久化** 方案：`consume_stream` 在自然完成点将消息写入 SQLite。

| 消息类型 | 持久化时机 | 写入策略 |
|---------|-----------|---------|
| 用户消息 | `send_message_cmd` 发送时立即写入 | 一次 INSERT |
| Agent 消息记录 | 流开始时创建 | INSERT status='streaming' |
| Agent 文本部分 | 流结束时一次性写入 | 内存累积 → 一次 INSERT per text part |
| 工具部分 | 工具完成时写入 | INSERT on `tool_result` event |
| Agent 消息状态 | 流结束时更新 | UPDATE status → done/stopped/error |

文本部分不在 per-token 级别持久化（避免数百次 DB 写入），而是在内存中累积，流结束时一次写入。工具部分有明确的完成边界，在 `tool_result` 事件时立即写入。

每条 Agent 消息最多 `1 + N_tools` 次写入 + 1 次状态更新 — 高效且完整。

## 3. 数据模型

### 3.1 迁移 `0005_session_messages.sql`

```sql
CREATE TABLE IF NOT EXISTS session_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,           -- 'user' | 'agent'
    content TEXT,                 -- user: 消息原文; agent: NULL
    status TEXT,                  -- agent: 'streaming'|'done'|'stopped'|'error'; user: 'done'
    seq INTEGER NOT NULL,         -- 会话内排序
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    part_type TEXT NOT NULL,      -- 'text' | 'tool'
    seq INTEGER NOT NULL,         -- 消息内排序
    text TEXT,                    -- 累积文本（text part）
    tool_name TEXT,               -- 工具名（tool part）
    tool_args TEXT,               -- JSON 字符串
    tool_status TEXT,             -- 'running'|'completed'|'error'
    tool_output TEXT,
    tool_elapsed_ms INTEGER,
    FOREIGN KEY (message_id) REFERENCES session_messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_messages_session ON session_messages(session_id);
CREATE INDEX IF NOT EXISTS idx_session_message_parts_message ON session_message_parts(message_id);
```

> **注意**：SQLite 默认不启用外键约束（`PRAGMA foreign_keys = OFF`）。当前 `db.rs::init` 未开启该 PRAGMA。因此 `ON DELETE CASCADE` 不会自动生效 — `delete_session` 函数需**手动级联删除**子表数据（先删 parts，再删 messages，最后删 session）。FK 约束保留在 schema 中用于数据完整性声明，但运行时不依赖其级联行为。

### 3.2 sessions 表新增 `archived_at` 列

在 `db.rs::init` 中通过现有 `add_column_if_not_exists` 添加：

```rust
add_column_if_not_exists(&pool, "sessions", "archived_at", "TEXT").await?;
```

状态值从 `active` | `closed` 扩展为 `active` | `closed` | `archived`。`archived_at` 记录归档时间，与 `closed_at` 模式一致。

### 3.3 SessionRow 变更

```rust
#[derive(Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub status: String,               // "active" | "closed" | "archived"
    pub created_at: String,
    pub archived_at: Option<String>,  // NEW
}
```

### 3.4 MessageRow + MessagePartRow（加载历史用）

```rust
#[derive(Serialize)]
pub struct MessagePartRow {
    pub part_type: String,            // "text" | "tool"
    pub seq: i64,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,    // JSON 字符串
    pub tool_status: Option<String>,
    pub tool_output: Option<String>,
    pub tool_elapsed_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct MessageRow {
    pub id: String,
    pub role: String,                 // "user" | "agent"
    pub content: Option<String>,
    pub status: Option<String>,
    pub seq: i64,
    pub parts: Vec<MessagePartRow>,
}
```

`ON DELETE CASCADE` 在 schema 中声明，但 SQLite 默认不启用外键约束，因此 `delete_session` 手动级联删除（见 4.5）。

## 4. 后端持久化逻辑

### 4.1 用户消息 — 发送时写入

在 `send_message_cmd` 中，session 解析后立即插入用户消息：

```rust
let seq = session::next_message_seq(&pool, &friday_session_id).await?;
session::insert_message(&pool, &friday_session_id, "user", Some(&message), "done", seq).await?;
```

`next_message_seq` 通过 `SELECT COUNT(*) FROM session_messages WHERE session_id = ?` 获取序号。

### 4.2 Agent 消息 — 流开始创建，流结束定稿

**流开始时**（`send_message_cmd` 中，spawn 前）：

```rust
let agent_seq = session::next_message_seq(&pool, &friday_session_id).await?;
let agent_message_id = session::insert_message(
    &pool, &friday_session_id, "agent", None, "streaming", agent_seq,
).await?;
```

将 `agent_message_id` 传入 `consume_stream`。

**流过程中** — `MessageAccumulator` 在内存中累积：

```rust
struct MessageAccumulator {
    message_id: String,
    text_parts: Vec<(i64, String)>,   // (seq, 累积文本)
    tool_parts_written: i64,           // 已写入的工具部分计数
    next_seq: i64,
}
```

- `LlmThinking` 事件 → 累积到当前文本部分（无新 part 时创建）
- `ToolExecuting` 事件 → 不写入（等完成）
- `ToolResult` 事件 → 立即写入 `session_message_parts` 行（part_type='tool'，含完整 args/output/status/elapsed_ms）

**流结束时**：

1. 将所有累积的文本部分写入 DB（每个 text part 一行 INSERT）
2. 更新 agent 消息 status → `done` | `stopped` | `error`
3. 单次 UPDATE

### 4.3 consume_stream 签名变更

```rust
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    agent_message_id: String,  // NEW
    pool: SqlitePool,
    agents: Arc<Mutex<HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
)
```

在 `consume_stream` 的事件循环中，`parse_event` 的结果同时：
- emit 到 bus（供前端实时渲染，现有逻辑不变）
- feed 到 `MessageAccumulator`（供持久化，新增逻辑）

### 4.4 持久化失败处理

`consume_stream` 中的 DB 写入使用 `let _ =` / `.ok()` — 持久化失败通过 `tracing::error!` 记录，但不中断流。实时 UI 仍正常工作（事件通过 bus 推送）。最坏情况是消息部分持久化不完整 — 可接受，因为用户正在观看实时流。

### 4.5 新增 `session.rs` 函数

```rust
pub async fn next_message_seq(pool, session_id) -> Result<i64>
pub async fn insert_message(pool, session_id, role, content, status, seq) -> Result<String>
pub async fn update_message_status(pool, message_id, status) -> Result<()>
pub async fn insert_text_part(pool, message_id, seq, text) -> Result<()>
pub async fn insert_tool_part(pool, message_id, seq, name, args, status, output, elapsed_ms) -> Result<()>
pub async fn get_session_messages(pool, session_id) -> Result<Vec<MessageRow>>
pub async fn archive_session(pool, id) -> Result<()>     // status='archived', archived_at=now
pub async fn unarchive_session(pool, id) -> Result<()>   // status='closed', archived_at=NULL
pub async fn delete_session(pool, id) -> Result<()>      // 手动级联：先删 parts → 再删 messages → 最后删 session
pub async fn list_sessions(pool, include_archived: bool) -> Result<Vec<SessionRow>>
```

`list_sessions` 查询逻辑：

```sql
-- include_archived=false（主列表）:
SELECT ... FROM sessions WHERE status IN ('active', 'closed') ORDER BY created_at DESC

-- include_archived=true（归档列表）:
SELECT ... FROM sessions WHERE status = 'archived' ORDER BY archived_at DESC
```

## 5. IPC 契约变更

### 5.1 新增命令

| command | 入参 | 出参 | 行为 |
|---|---|---|---|
| `get_session_messages_cmd` | `session_id: String` | `Vec<MessageRow>` | 加载会话完整消息历史（messages + parts） |
| `archive_session_cmd` | `session_id: String` | `()` | status → `archived`，设置 `archived_at` |
| `unarchive_session_cmd` | `session_id: String` | `()` | status → `closed`，清除 `archived_at` |
| `delete_session_cmd` | `session_id: String` | `()` | 停止运行中 agent（如有）→ 手动级联删除 session + messages + parts → emit `SessionDeleted` |

### 5.2 修改命令

**`list_sessions_cmd`** — 新增 `include_archived` 参数：

```rust
#[tauri::command]
pub async fn list_sessions_cmd(
    state: State<'_, AppState>,
    include_archived: bool,
) -> Result<Vec<SessionRow>, String>
```

- `false` → 返回 `active` + `closed` 会话（主列表）
- `true` → 返回 `archived` 会话（归档视图）

**`send_message_cmd`** — 新增用户消息和 agent 消息的持久化步骤：

```
1. 解析 session（现有逻辑）
2. 插入用户消息行（NEW）
3. 插入 agent 消息行 status='streaming'（NEW）
4. spawn_active → consume_stream（传入 agent_message_id）（现有逻辑 + 新参数）
5. 返回 session_id
```

### 5.3 不变命令

`stop_agent_cmd`、`close_session_cmd`、`confirm_tool_cmd`、`set_log_level_cmd` — 契约不变。

### 5.4 新增 AppEvent

```rust
SessionDeleted { session_id: String }
```

在 `app/events.rs` 的 `AppEvent` enum 中新增。前端 `handleEvent` 处理方式：清除 `messagesBySession[id]`，从 `sessions` 列表移除，若为 `currentSessionId` 则重置为新会话状态。

### 5.5 前端 IPC 绑定 (`src/lib/ipc.ts`)

```ts
export async function listSessions(includeArchived: boolean): Promise<SessionRow[]>
export async function getSessionMessages(sessionId: string): Promise<MessageRow[]>
export async function archiveSession(sessionId: string): Promise<void>
export async function unarchiveSession(sessionId: string): Promise<void>
export async function deleteSession(sessionId: string): Promise<void>
```

`listSessions` 签名变更 — 现有调用方需传 `false`。

## 6. 前端

### 6.1 类型定义 (`src/lib/types.ts`)

```ts
export interface SessionRow {
  id: string;
  title: string | null;
  status: "active" | "closed" | "archived";
  created_at: string;
  archived_at: string | null;
}

export interface MessagePartRow {
  part_type: "text" | "tool";
  seq: number;
  text: string | null;
  tool_name: string | null;
  tool_args: string | null;
  tool_status: string | null;
  tool_output: string | null;
  tool_elapsed_ms: number | null;
}

export interface MessageRow {
  id: string;
  role: "user" | "agent";
  content: string | null;
  status: string | null;
  seq: number;
  parts: MessagePartRow[];
}
```

AppEvent 联合类型新增 `session_deleted` 变体。

### 6.2 sessionStore 变更

新增字段：

```ts
sidebarView: "sessions" | "archived";
```

新增 action：

```ts
loadArchivedSessions: () => Promise<void>;   // listSessions(true)
selectSession: (id: string) => Promise<void>; // 加载消息历史（如不在内存中）
archiveSession: (id: string) => Promise<void>;
unarchiveSession: (id: string) => Promise<void>;
deleteSession: (id: string) => Promise<void>;
setSidebarView: (view: "sessions" | "archived") => void;
```

`selectSession` 消息加载逻辑：

```ts
selectSession: async (id) => {
  set({ currentSessionId: id });
  const { messagesBySession } = get();
  if (!messagesBySession[id]) {
    const rows = await getSessionMessages(id);
    const messages = convertMessages(rows);
    set({ messagesBySession: { ...get().messagesBySession, [id]: messages } });
  }
}
```

`convertMessages` 将 `MessageRow[]`（DB 结构）映射为 `ChatMessage[]`（UI 结构），解析 `tool_args` JSON 字符串，构建 `ChatPart[]`。

**避免重复加载**：消息已在内存中（来自实时流或 DB 加载）时，后续 select 跳过 fetch。

### 6.3 持久化策略

前端**不负责持久化**。现有 `messagesBySession` 内存状态仅用于实时流渲染。用户重新打开会话时通过 IPC 加载。实时会话通过事件累积到内存（现有逻辑），后端并行持久化。

### 6.4 组件变更

| 文件 | 动作 | 变更 |
|------|------|------|
| `src/lib/types.ts` | 改 | +`archived_at`、+`MessageRow`、+`MessagePartRow`、status +"archived"、+`session_deleted` event |
| `src/lib/ipc.ts` | 改 | `listSessions(includeArchived)`，+`getSessionMessages`、+`archiveSession`、+`unarchiveSession`、+`deleteSession` |
| `src/store/sessionStore.ts` | 改 | +`sidebarView`、+加载/归档/取消归档/删除 action、`selectSession` 加载历史 |
| `src/components/layout/SessionSidebar.tsx` | 改 | toggle 栏、上下文菜单、归档视图渲染 |
| `src/components/chat/DeleteConfirmDialog.tsx` | 新 | 删除确认弹窗 |
| `src/components/chat/MessageList.tsx` | 不变 | 已渲染 `ChatMessage[]` — 加载的历史输入相同结构 |

### 6.5 侧边栏 UI

**Toggle 栏**：替换现有 "会话" 标题，两个 tab — "会话"（主列表）和 "归档"（归档列表）。选中 tab 底部 2px `--success` 边条。

**主列表**：显示 `active` + `closed` 会话。`active` 会话状态点绿色脉冲（运行中）或灰色静态。`closed` 会话半透明（opacity 0.6）。

**归档列表**：显示 `archived` 会话，全部半透明。每个会话显示 "归档于 YYYY-MM-DD"（`archived_at`）。顶部显示归档计数。

**上下文菜单**：右键或 `⋯` 按钮触发。主列表菜单项： "归档会话"、"删除会话"（红色）。归档列表菜单项："取消归档"、"删除会话"（红色）。点击外部关闭。不引入外部库 — 轻量定位 `div`。

**新建会话按钮**：仅主列表显示，归档列表不显示。

### 6.6 删除确认弹窗

`DeleteConfirmDialog` 组件：

- 文案："确定删除该会话？删除后不可恢复。"
- 按钮：取消（默认） / 确认删除（`--destructive`）
- 确认后：调用 `deleteSession` → 从列表移除 → 清除 `messagesBySession[id]` → 若为 `currentSessionId` 则重置为新会话状态

### 6.7 渲染约定

- 归档会话：`opacity: 0.5` + `--muted-foreground` 文字
- Toggle 栏：`bg-surface-1` + `border-border`，tab 高 40px
- 上下文菜单：`bg-surface-2` + `border-border-strong` + `rounded-lg` + `shadow`，`--text-sm`
- 删除项：`text-destructive`，hover `bg-destructive/10`
- 删除弹窗：`bg-card` + `border-border` + `rounded-xl`，居中遮罩

## 7. 错误处理与边界情况

### 7.1 持久化失败

`consume_stream` 中 DB 写入失败时 `tracing::error!` 记录，不中断流。实时 UI 正常工作。最坏情况：消息部分持久化不完整。

### 7.2 删除运行中 agent 的会话

`delete_session_cmd` 先停止运行中 agent（同 `close_session_cmd` 模式），再 DELETE，再 emit `SessionDeleted`。

### 7.3 归档运行中 agent 的会话

允许归档有运行中 agent 的会话。Agent 继续运行，归档仅改变列表可见性。会话仍通过 `currentSessionId` 可访问。Agent 完成后会话保持归档状态。

### 7.4 加载有运行中 agent 的会话历史

用户点击归档/已关闭会话时，另一个会话的 agent 可能正在运行。消息从 DB 正常加载。运行中 agent 的事件仍推送到其自己的 `session_id` — `handleEvent` 按 `session_id` 分发，无冲突。

### 7.5 list_sessions 查询

主列表按 `created_at` 降序（现有行为）。归档列表按 `archived_at` 降序（最近归档在前）。

### 7.6 迁移安全

`0005_session_messages.sql` 使用 `CREATE TABLE IF NOT EXISTS` — 对现有 DB 安全。`archived_at` 列通过现有 `add_column_if_not_exists` 添加。

## 8. 架构与模块归属

```
app/
  mod.rs         ← 不变
  session.rs     ← 改（+消息 CRUD、+archive/unarchive/delete、+list_sessions 参数）
  lifecycle.rs   ← 改（+4 新 command、send_message_cmd 加持久化步骤、list_sessions_cmd 加参数）
  events.rs      ← 改（+SessionDeleted variant）
  agents.rs      ← 不变
  credentials.rs ← 不变
agent/
  spawn.rs       ← 不变
  stream.rs      ← 改（+MessageAccumulator、consume_stream 加 agent_message_id 参数 + 持久化逻辑）
  prompt.rs      ← 不变
  registry.rs    ← 不变
  detect.rs      ← 不变
infra/
  db.rs          ← 改（加载 0005 迁移 + archived_at 列）
migrations/
  0005_session_messages.sql ← 新（session_messages + session_message_parts 表）
```

**集成点：**
- `lib.rs`：handler 注册新增 `get_session_messages_cmd`、`archive_session_cmd`、`unarchive_session_cmd`、`delete_session_cmd`
- 前端：types、ipc、sessionStore、SessionSidebar 全面改造

## 9. 测试策略

### 9.1 后端单测 (`session.rs`)

沿用现有 `tempfile` + in-memory SQLite 模式：

- `insert_message` — 插入 user/agent 消息，验证行存在、role/status/seq 正确
- `insert_text_part` — 插入文本部分，验证内容
- `insert_tool_part` — 插入工具部分，验证完整字段
- `update_message_status` — 验证状态转换
- `get_session_messages` — 插入消息 + 部分，验证 `MessageRow` 含嵌套 `parts` 且顺序正确
- `get_session_messages_empty` — 不存在的 session 返回空 vec
- `archive_session` — status → archived，`archived_at` 非空
- `unarchive_session` — status → closed，`archived_at` null
- `delete_session` — session 行删除，messages + parts 手动级联删除（先 parts → 再 messages → 最后 session）
- `list_sessions_exclude_archived` — 归档会话不在主列表
- `list_sessions_include_archived` — 仅返回归档会话
- `seq_ordering` — 多条消息 seq 递增

### 9.2 后端单测 (`lifecycle.rs`)

- `delete_session_cmd` — 验证 agent 停止 + session 删除 + `SessionDeleted` 事件
- `archive_session_cmd` — 验证状态变更
- 现有 `close_session_cmd` 测试仍通过

### 9.3 后端单测 (`stream.rs`)

- `MessageAccumulator` 文本累积 — 多个 token 累积为单个 text part
- `MessageAccumulator` 工具完成 — tool result 触发即时写入
- `MessageAccumulator` flush on done — 流结束时所有累积文本写入

### 9.4 前端

- `pnpm typecheck` 通过
- 手动：发消息 → 关闭会话 → 重新打开 → 验证完整历史加载
- 手动：归档会话 → 切换归档视图 → 可见 → 取消归档 → 回到主列表
- 手动：删除会话 → 确认弹窗 → 从列表移除 + 消息清除
- 手动：删除运行中 agent 的会话 → agent 停止 + 会话移除

### 9.5 集成验证

- 发消息 → 流完成 → 重新打开会话 → 所有 user + agent 消息 + 工具部分可见
- 归档运行中 agent 的会话 → agent 继续 → 完成 → 会话保持归档 + 历史完整
- 删除会话 → `SessionDeleted` 事件 → 前端清除状态

## 10. 实现顺序建议

1. 迁移 `0005_session_messages.sql` + `db.rs` 加载（含 `archived_at` 列）
2. `app/session.rs` 新增消息 CRUD + archive/unarchive/delete + list_sessions 参数 + 单测
3. `agent/stream.rs` 实现 `MessageAccumulator` + `consume_stream` 持久化逻辑 + 单测
4. `app/lifecycle.rs` 实现 4 新 command + `send_message_cmd` 加持久化步骤 + `list_sessions_cmd` 加参数
5. `app/events.rs` 新增 `SessionDeleted` variant
6. `lib.rs` 注册新 handler
7. `cargo check` + `cargo test` 通过
8. 前端 types + ipc 变更
9. 前端 sessionStore 变更（sidebarView、加载历史、归档/删除 action）
10. 前端 SessionSidebar 改造（toggle、上下文菜单、归档视图）
11. 前端 DeleteConfirmDialog 组件
12. `pnpm typecheck` 通过
13. `pnpm tauri dev` 端到端验证

## 11. 新增依赖

无新增依赖。`serde_json`（已有，用于 tool_args JSON 序列化）、`uuid`（已有，用于消息 ID 生成）、`chrono`（已有，用于时间戳）。
