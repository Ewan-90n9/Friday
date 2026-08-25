# 工具系统框架设计

## 概述

Friday 的工具系统是连接 Agent 与远程诊断能力的桥梁。本 spec 设计工具系统**框架**：MCP Server + Tool Registry + 风险拦截 + 会话路由 + ExecChannel 集成，为后续具体诊断工具（jstat、jcmd、arthas 等）定义接口规范。

不含具体诊断工具实现、SSH/K8s Transport 实现、Environment 管理 UI——这些是后续独立 spec。

## 核心决策

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | MCP 协议实现 | rmcp SDK（官方 Rust MCP SDK），`transport-streamable-http-server` feature |
| 2 | HTTP 传输 | hyper 直接 serve `StreamableHttpService`（rmcp 已传递依赖 hyper） |
| 3 | 部署模型 | 进程内，与 Tauri app 同进程，`setup()` 中启动 |
| 4 | 端口策略 | 动态绑定 `127.0.0.1:0`，每次启动写入 opencode 配置 |
| 5 | 会话路由 | `session_id` 作为工具参数（Server 自动注入 schema），Mcp-Session-Id 头映射为可选增强 |
| 6 | 风险拦截 | oneshot channel 阻塞 + 120s 超时 + agent 取消时清理 pending confirms |
| 7 | 工具事件 | v1 MCP Server 和 opencode stdout 都发事件（方便调试），已知重复问题，后续优化 |
| 8 | SSH/K8s Transport | `todo!()` 改为返回 `Err` 的占位实现，框架可独立测试 |
| 9 | opencode 配置 | 合并写入 `~/.config/opencode/opencode.jsonc`，v1 接受注释丢失 |
| 10 | 测试工具 | `friday_echo`（ReadOnly）验证端到端流程 |
| 11 | Agent 停止时 in-flight 工具 | 不处理，标注为已知限制 |

## 架构

### 进程内 MCP Server

Friday 的 Tauri 进程内托管 MCP Server 作为 tokio task。Server 绑定 `127.0.0.1` 动态端口，使用 rmcp 的 `StreamableHttpService` 提供 SSE 传输。

```
┌────────────────────────────────────────────────────────────────┐
│ Tauri App 进程                                                  │
│                                                                │
│  ┌──────────┐     ┌──────────────────────────────────────┐     │
│  │ Tauri    │     │ tokio runtime                         │     │
│  │ Frontend │     │                                       │     │
│  │ (React)  │     │  ┌─────────┐  ┌──────────────────┐   │     │
│  │          │◄───►│  │ MCP     │  │ Agent 进程        │   │     │
│  │          │ IPC │  │ Server  │◄─┤ (opencode)       │   │     │
│  │          │     │  │ (rmcp)  │  │   ↓ stdout NDJSON │   │     │
│  │          │     │  │    ↑    │  │   ↓ SSE/MCP       │   │     │
│  │          │     │  │ ToolReg │  └──────────────────┘   │     │
│  │          │     │  │ ExecPool│  ┌──────────────────┐   │     │
│  │          │     │  │ Confirm │  │ SSH/K8s (todo)   │   │     │
│  │          │     │  └────┬────┘  └──────────────────┘   │     │
│  └──────────┘     │       │                                │     │
│                   │  ┌────▼────┐                           │     │
│                   │  │ SQLite  │                           │     │
│                   │  └─────────┘                           │     │
│                   └──────────────────────────────────────┘     │
└────────────────────────────────────────────────────────────────┘
```

### 启动序列

`setup()` 中执行：

1. 初始化 Paths、DB、日志（已有流程）
2. 创建 `ToolRegistry`，注册 `friday_echo` 测试工具
3. 创建 `ExecChannelPool`（空 HashMap）
4. 创建 `ConfirmRegistry`（空 HashMap）
5. 创建 `SessionMapper`（空 HashMap，用于 Mcp-Session-Id → Friday session_id 映射）
6. TCP listener 绑定 `127.0.0.1:0`，获取分配的端口
7. 构建 `StreamableHttpService`：
   - `service_factory`：闭包，每次调用创建 `FridayMcpServer`（克隆 Arc 引用）
   - `session_manager`：`LocalSessionManager::default()`
   - `config`：`StreamableHttpServerConfig`（loopback-only allowed_hosts，SSE keepalive 30s，cancellation_token）
8. spawn tokio task：hyper serve `StreamableHttpService` on bound listener，路由 `/mcp`
9. 合并写入 opencode 配置（`~/.config/opencode/opencode.jsonc`）
10. 将 `McpServerHandle`（端口、CancellationToken、JoinHandle）和共享状态存入 `AppState`

### 关闭

`CancellationToken` 取消 → `StreamableHttpServerConfig.cancellation_token` 触发 → 所有 SSE 连接关闭 → server task 结束。Tauri app 关闭时发生。

### AppState 变更

```rust
pub struct AppState {
    // ... 已有字段 ...
    pub tool_registry: Arc<ToolRegistry>,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<SessionMapper>>,
    pub mcp_server: Option<McpServerHandle>,
}
```

### Cargo.toml 依赖变更

```toml
rmcp = { version = "3", features = ["server", "macros", "transport-streamable-http-server"] }
hyper = "1"
hyper-util = { version = "0.1", features = ["server", "http1", "tokio"] }
http = "1"
http-body-util = "0.1"
```

## MCP Server

### FridayMcpServer 结构体

实现 `rmcp::handler::server::ServerHandler` trait 的具体类型。`StreamableHttpService::new` 的工厂闭包为每个 SSE 连接创建一个新实例，通过 `Arc` 共享状态：

```rust
struct FridayMcpServer {
    tool_registry: Arc<ToolRegistry>,
    exec_pool: Arc<Mutex<ExecChannelPool>>,
    confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    session_mapper: Arc<Mutex<SessionMapper>>,
    bus: EventBus,
    pool: SqlitePool,
}
```

### ServerHandler 实现要点

手动实现 `call_tool`（不使用 `#[tool_handler]` 宏），以插入风险拦截和 session 路由逻辑。

**`list_tools`**：从 `tool_registry.list()` 构建 `ListToolsResult`。**自动注入 `session_id` 必填参数**到每个工具的 input_schema 中——工具定义本身不含 `session_id`，由 Server 在返回时动态注入。

**`get_tool(name)`**：从 `tool_registry.get(name)` 返回 `Option<Tool>`，同样注入 `session_id` 参数。

**`call_tool(request)`**：核心 dispatch 流程（见下节）。

**`get_info()`**：返回 `ServerInfo`，name="Friday"，version 从 Cargo.toml 读取。

**`initialize(request, context)`**：调用默认实现。在 `on_initialized` 中，从 `RequestContext` 的 HTTP headers 读取 `Mcp-Session-Id`，从 `SessionMapper` 的 next-connection 队列弹出 Friday session_id，注册映射。

### session_id 自动注入

`ToolDef` 的 `input_schema` 不含 `session_id`。`FridayMcpServer::list_tools` 和 `get_tool` 在返回前动态注入：

```rust
fn inject_session_id(schema: &mut serde_json::Value) {
    // 在 schema["properties"] 中添加 "session_id": {"type": "string"}
    // 在 schema["required"] 中添加 "session_id"
}
```

这样后续 spec 定义 jstat、arthas 等工具时，只需写业务参数（pid、interval 等），不需要关心 `session_id`。

## 工具调用 dispatch 流程

`FridayMcpServer::call_tool` 的完整流程：

### 步骤 1：解析参数，提取 session_id

```
session_id = args["session_id"].as_str()
```

优先从 Mcp-Session-Id 头映射获取（通过 `session_mapper`）。如果映射存在且与参数一致，正常继续。如果映射存在但参数不一致，warn 日志，以参数为准。如果参数缺失，尝试从映射兜底。都没有 → `CallToolResult::error("缺少 session_id 参数")`。

### 步骤 2：查找工具定义

```
def = tool_registry.get(tool_name)
if not found → Err(McpError::METHOD_NOT_FOUND)  // 协议错误，agent 不可见
```

### 步骤 3：风险拦截

```
match def.risk_level {
    ReadOnly → 直接继续（步骤 4）
    Low | High → 进入确认流程
}
```

确认流程：
1. `confirm_id = uuid::new_v4()`
2. `(tx, rx) = oneshot::channel()`
3. `confirm_registry.insert(confirm_id, session_id, tx)`
4. `bus.emit(session_id, ConfirmRequired { session_id, confirm_id, tool, args, risk_level })`
5. `tokio::time::timeout(Duration::from_secs(120), rx.await)` — 120s 超时
6. 超时 → `CallToolResult::error("确认超时，工具未执行")`
7. `ConfirmResult::Cancelled` → `CallToolResult::error("用户取消了工具执行")`
8. `ConfirmResult::Confirmed` → 继续

### 步骤 4：获取/创建 ExecChannel（仅 needs_channel 工具）

```
if def.needs_channel:
  channel = exec_pool.get_or_create(session_id, &pool)
    ├─ 已有连接 → Arc clone 返回
    ├─ 无连接 → 从 DB 查 session 关联的 environment
    │   ├─ session 无 environment_id → ToolOutput { success: false, data: {"error": "no_environment"} }
    │   ├─ environment 记录不存在 → ToolOutput { success: false, data: {"error": "environment_not_found"} }
    │   ├─ 创建 SshTransport / K8sTransport
    │   ├─ connect() → Err("SSH transport not yet implemented")  // 占位
    │   └─ 存入 pool，返回 Arc clone
    └─ 连接失败 → ToolOutput { success: false, data: {"error": "connection_error"} }
else:
  channel = None  // 本地工具（echo、get_playbook）不需要远程环境
```

### 步骤 5：执行工具

```
ctx = ToolContext { session_id, channel }
bus.emit(session_id, ToolExecuting { session_id, tool, args })
output = def.handler.execute(args, &ctx).await
bus.emit(session_id, ToolResult { session_id, tool, output, elapsed_ms })
```

**注意**：v1 MCP Server 和 opencode stdout 都会发出 `ToolExecuting`/`ToolResult` 事件。前端会收到重复事件。这是 v1 的有意设计——方便调试，后续优化去重。

### 步骤 6：持久化到 tool_calls 表

```
INSERT INTO tool_calls (id, session_id, tool_name, args, risk_level, status, output, raw_stdout, elapsed_ms, error, created_at, completed_at)
VALUES (...)
```

持久化失败不影响工具结果返回——log error 并继续。

### 步骤 7：返回结果

```
ToolOutput → CallToolResult 转换
  ├─ success: true → CallToolResult::success(content)  // content 包含结构化 data 和 raw_stdout
  └─ success: false → CallToolResult::error(content)    // agent 可见错误
```

## Tool Registry

### ToolDef

```rust
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,  // JSON Schema，不含 session_id
    pub risk_level: RiskLevel,
    pub needs_channel: bool,              // 本地工具（echo、get_playbook）为 false，跳过 ExecChannel 获取
    pub handler: Arc<dyn ToolHandler>,
}
```

### ToolHandler trait

```rust
pub struct ToolContext {
    pub session_id: String,
    pub channel: Option<Arc<dyn ExecChannel>>,  // needs_channel=false 时为 None
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput;
}
```

### ToolOutput

```rust
pub struct ToolOutput {
    pub success: bool,
    pub data: serde_json::Value,       // 结构化数据
    pub raw_stdout: Option<String>,     // 原始命令输出
}
```

### ToolRegistry

```rust
pub struct ToolRegistry {
    tools: HashMap<String, ToolDef>,
}

impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, def: ToolDef);
    pub fn get(&self, name: &str) -> Option<&ToolDef>;
    pub fn list(&self) -> Vec<&ToolDef>;
}
```

在 `setup()` 中构建，注册到 `Arc` 后不可变。后续如需动态注册（playbook 生成工具），扩展为 `RwLock`。

### friday_echo 测试工具

```rust
pub struct EchoHandler;

// schema: { "message": {"type": "string"} }  (不含 session_id，由 Server 注入)
// risk_level: ReadOnly, needs_channel: false
// 执行: 返回 { echo: args, session_id: ctx.session_id }
```

## 风险拦截

### ConfirmRegistry

```rust
struct PendingConfirm {
    session_id: String,
    tx: oneshot::Sender<ConfirmResult>,
}

pub enum ConfirmResult {
    Confirmed,
    Cancelled,
}

pub struct ConfirmRegistry {
    pending: HashMap<String, PendingConfirm>,  // key = confirm_id
}

impl ConfirmRegistry {
    pub fn insert(&mut self, confirm_id: String, session_id: String, tx: oneshot::Sender<ConfirmResult>);
    pub fn resolve(&mut self, confirm_id: &str) -> Option<oneshot::Sender<ConfirmResult>>;
    pub fn cancel_for_session(&mut self, session_id: &str) -> usize;  // 遍历，发送 Cancelled，返回取消数量
}
```

### confirm_tool_cmd 重构

现有 no-op stub 改为实际逻辑，新增 `approved` 参数区分确认和取消：

```rust
#[tauri::command]
pub async fn confirm_tool_cmd(
    state: State<'_, crate::AppState>,
    confirm_id: String,   // 从 (session_id, tool) 改为 confirm_id
    approved: bool,        // true=确认, false=取消
) -> Result<(), String> {
    let mut registry = state.confirm_registry.lock().await;
    match registry.resolve(&confirm_id) {
        Some(tx) => {
            let result = if approved { ConfirmResult::Confirmed } else { ConfirmResult::Cancelled };
            tx.send(result).ok();
            Ok(())
        }
        None => Err("确认请求不存在或已过期".to_string())
    }
}
```

### ConfirmRequired 事件变更

`AppEvent::ConfirmRequired` 新增 `confirm_id` 字段，前端需要它来调用 `confirm_tool_cmd`：

```rust
ConfirmRequired {
    session_id: String,
    confirm_id: String,    // 新增
    tool: String,
    args: serde_json::Value,
    risk_level: RiskLevel,
},
```

### Agent 取消时清理

`stop_agent_cmd` 和 `close_session_cmd` 中调用 `confirm_registry.cancel_for_session(&session_id)`，发送 `Cancelled` 给所有 pending confirms。

## ExecChannelPool

```rust
pub struct ExecChannelPool {
    connections: HashMap<String, Arc<dyn ExecChannel>>,
}

impl ExecChannelPool {
    pub async fn get_or_create(
        &mut self,
        session_id: &str,
        pool: &SqlitePool,
    ) -> Result<Arc<dyn ExecChannel>, ToolError>;

    pub async fn disconnect(&mut self, session_id: &str);
    pub async fn disconnect_all(&mut self);
}
```

### 连接生命周期

| 用户动作 | agent | SSH 连接 | ExecChannelPool 行为 |
|---------|-------|---------|---------------------|
| 停 agent | kill，保留会话 | 保留 | 不动连接 |
| 关闭会话 | kill | 断开 | `disconnect(session_id)` |
| 应用关闭 | kill all | 断开 all | `disconnect_all()` |

### 懒连接

连接在第一次工具调用时创建，不在 session 创建时预连接。

### Session ↔ Environment 关联

`sessions` 表新增 `environment_id` 列（migration `0007_environment_link.sql`）：

```sql
ALTER TABLE sessions ADD COLUMN environment_id TEXT REFERENCES environments(id);
```

`get_or_create` 查询：
```sql
SELECT e.host, e.port, e.user, e.transport_type, e.k8s_namespace, e.k8s_pod
FROM sessions s
JOIN environments e ON s.environment_id = e.id
WHERE s.id = ?
```

Environment 管理 UI/CRUD 不在本 spec 范围内。

### SSH/K8s Transport 占位实现

`todo!()` 改为返回 `Err`：

```rust
async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
    Err("SSH transport not yet implemented".into())
}
async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err("SSH transport not yet implemented".into())
}
```

## SessionMapper（可选增强）

Mcp-Session-Id → Friday session_id 的映射，用于校验和兜底。存储在 `Arc<Mutex<SessionMapper>>` 中，方法取 `&mut self`：

```rust
pub struct SessionMapper {
    /// next-connection 队列：spawn_active 推入，initialize 弹出
    next_session: Option<String>,
    /// Mcp-Session-Id → Friday session_id 映射
    mapping: HashMap<String, String>,
}

impl SessionMapper {
    pub fn enqueue(&mut self, session_id: String);
    pub fn dequeue_and_map(&mut self, mcp_session_id: String);
    pub fn lookup(&self, mcp_session_id: &str) -> Option<String>;
}
```

`spawn_active` 调用 `session_mapper.enqueue(friday_session_id)`。`FridayMcpServer::on_initialized` 读取 `Mcp-Session-Id` 头，调用 `dequeue_and_map`。`call_tool` 中优先用参数里的 `session_id`，映射做校验和兜底。

**已知限制**：
- 并发 spawn 时 next-connection 队列可能竞态（agent 连接顺序 ≠ spawn 顺序）
- SSE 断线重连后映射丢失
- MCP 2026-07-28 协议取消了 session，无 Mcp-Session-Id

因此映射是可选增强，`session_id` 参数始终是主路由机制。

## Opencode 配置自动合并

### 配置文件

路径：`~/.config/opencode/opencode.jsonc`

### 合并策略（read-merge-write）

1. 读取 `~/.config/opencode/opencode.jsonc`
   - 文件不存在 → 创建 `~/.config/opencode/` 目录，初始 config = `{}`
   - 文件存在但 JSON 解析失败 → warn 日志，备份原文件为 `.bak`，初始 config = `{}`
   - 文件存在且解析成功 → config = 解析结果
2. 合并 Friday MCP 条目：
   ```json
   "mcp": {
     "friday": {
       "type": "remote",
       "url": "http://127.0.0.1:PORT/mcp",
       "enabled": true,
       "timeout": 10000
     }
   }
   ```
   保留 config 中其他所有内容。
3. 格式化 JSON 写回。
4. **v1 接受注释丢失**（serde_json 不保留 JSONC 注释），warn 日志告知用户。

### 每次启动都执行

端口是动态的，每次启动可能不同。合并是幂等的。

### 仅处理 opencode

codeagentcli 的 MCP 配置如果格式不同，后续 spec 处理。

## System Prompt 注入 session_id

`build_prompt` 新增 `session_id` 参数，注入为独立段落（在 system prompt 之后、用户消息之前）：

```rust
pub fn build_prompt(message: &str, override_path: Option<&Path>, session_id: &str) -> String {
    let system = build_system_prompt(override_path);
    format!(
        "{system}\n\n---\n\n## 工具使用\n- 调用诊断工具时，必须传入 session_id 参数。\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
    )
}
```

`build_prompt_with_experiences` 同理新增 `session_id` 参数。

`spawn_active` 已有 `session_id` 参数，透传即可。`spawn_one_shot` 不需要 session_id（用于 summary/experience 生成，不调用工具）。

session_id 注入为独立段落，即使用户有自定义 `friday.md` 也会注入。

## 前端适配

### 工具展示

新增 `list_tools_cmd` Tauri command，返回工具列表（只读，基本信息）：

```rust
#[derive(Clone, Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub risk_level: RiskLevel,
}

#[tauri::command]
pub async fn list_tools_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<ToolInfo>, String>;
```

前端调用后展示工具名称、描述、风险级别。工具列表在运行时不变（注册于启动时），可获取后缓存。

### confirm_tool_cmd 参数变更

`src/lib/ipc.ts` 中 `confirm_tool_cmd` 参数从 `(session_id, tool)` 改为 `(confirm_id, approved)`。

### ConfirmRequired 事件

`ConfirmRequired` 事件新增 `confirm_id` 字段（`AppEvent::ConfirmRequired`），前端实现确认弹窗：
- `risk_level: Low` → 简单确认对话框
- `risk_level: High` → 醒目警告 + 确认
- 用户确认 → 调用 `confirm_tool_cmd(confirm_id, true)`
- 用户取消 → 调用 `confirm_tool_cmd(confirm_id, false)`
- 120s 未操作 → 自动超时，MCP Server 返回 error 给 agent

## 模块结构

```
src-tauri/src/
├── tools/
│   ├── mod.rs              # 不变
│   ├── registry.rs         # 重构：ToolDef, ToolHandler, ToolContext, ToolOutput, ToolRegistry
│   ├── risk.rs             # 不变
│   ├── confirm.rs          # 新增：ConfirmRegistry, PendingConfirm, ConfirmResult
│   └── builtin/
│       └── mod.rs          # EchoHandler
├── mcp/                    # 新增模块
│   ├── mod.rs
│   ├── server.rs           # FridayMcpServer (impl ServerHandler), session_id 注入
│   ├── config.rs           # opencode 配置合并
│   └── transport.rs        # hyper serve StreamableHttpService
├── exec/
│   ├── channel.rs          # 不变
│   ├── pool.rs             # 新增：ExecChannelPool
│   ├── ssh.rs              # todo!() → Err 占位
│   ├── k8s.rs              # todo!() → Err 占位
│   └── mod.rs              # 新增 pub mod pool
├── app/
│   ├── lifecycle.rs        # confirm_tool_cmd 实现、stop_agent 清理、list_tools_cmd
│   ├── session.rs          # 不变（environment_id 由后续 spec 设置）
│   └── ...
├── agent/
│   ├── prompt.rs           # build_prompt / build_prompt_with_experiences 新增 session_id
│   ├── spawn.rs            # 透传 session_id 到 build_prompt
│   └── ...
└── lib.rs                  # AppState 新增字段、setup() 启动 MCP server、注册 list_tools_cmd
```

## DB Migration

`migrations/0007_environment_link.sql`：

```sql
ALTER TABLE sessions ADD COLUMN environment_id TEXT REFERENCES environments(id);
```

## 测试策略

| 层次 | 测试内容 | 方式 |
|------|---------|------|
| ToolRegistry | 注册、查询、list | 纯单元测试 |
| ConfirmRegistry | insert/resolve/cancel_for_session | 单元测试，oneshot channel |
| ExecChannelPool | get_or_create 缓存命中/未命中 | 单元测试，mock ExecChannel |
| 配置合并 | 文件不存在/已存在/JSON 损坏 | tempdir + 文件 IO |
| session_id 注入 | schema 注入正确性 | 单元测试 |
| FridayMcpServer::call_tool | 完整 dispatch 流程 | 集成测试：mock tool handler + mock channel |
| 风险拦截 | ReadOnly 直通 / Low 确认 / High 确认 / 取消 / 超时 | 集成测试 |
| Session 路由 | session_id 提取、missing session_id 错误 | 单元测试 |

不测试：实际 SSH 连接、实际 opencode 连接 MCP Server、K8s exec（依赖外部环境，后续 spec）。

## 错误处理

| 错误类型 | 处理 | 返回给 agent |
|---------|------|-------------|
| session_id 缺失 | Server 尝试 Mcp-Session-Id 映射兜底 | 失败 → `CallToolResult::error("缺少 session_id")` |
| 工具不存在 | 协议错误 | `Err(McpError::METHOD_NOT_FOUND)` |
| 确认超时 | 120s 后自动返回 | `CallToolResult::error("确认超时")` |
| 用户取消确认 | `cancel_for_session` 发送 Cancelled | `CallToolResult::error("用户取消")` |
| 无 environment | `get_or_create` 查询失败 | `CallToolResult::error("未关联目标环境")` |
| SSH 未实现 | 占位 Err | `CallToolResult::error("SSH transport not yet implemented")` |
| 工具命令超时 | 不重试 | `ToolOutput { success: false }` → `CallToolResult::error` |
| 工具输出解析失败 | 不重试，返回原始 stdout | `ToolOutput { success: true, raw_stdout }` → `CallToolResult::success` |
| tool_calls 持久化失败 | log error，不影响结果 | 正常返回 |

## 已知限制

1. **工具事件重复**：MCP Server 和 opencode stdout 都发 `ToolExecuting`/`ToolResult`。v1 有意保留方便调试，后续优化。
2. **Agent 停止时 in-flight 工具不取消**：`call_tool` future 自然完成（SSH 命令几秒内结束，确认等待已被 `cancel_for_session` 清理）。后续如支持长时间运行的诊断命令，再引入 per-session cancel。
3. **SessionMapper 竞态**：并发 spawn 时 next-connection 队列可能映射错误。`session_id` 参数始终是主路由机制，映射仅做校验和兜底。
4. **SSE 断线重连丢失映射**：重连后 Mcp-Session-Id 变化，映射失效。`session_id` 参数兜底。
5. **opencode 配置注释丢失**：serde_json 不保留 JSONC 注释。后续可换 `jsonc-parser` crate。
6. **仅 opencode 配置**：codeagentcli 的 MCP 配置后续处理。
7. **无 Environment 管理 UI**：`environment_id` 列已加，但设置它的 UI/CRUD 是后续 spec。

## 不在本 spec 范围内

- 具体 builtin 工具实现（jstat、jcmd、arthas、read_log、read_dump）→ 后续 spec
- SSH/K8s Transport 实现（russh/kubectl exec）→ 后续 spec
- Environment 管理 UI（CRUD、凭证存储）→ 后续 spec
- Playbook `get_playbook` 工具 → 后续 spec
- 工具事件去重优化 → 后续优化
- codeagentcli MCP 配置 → 后续 spec
