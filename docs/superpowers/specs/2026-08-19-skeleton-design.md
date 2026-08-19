# Friday 框架骨架设计

- 日期：2026-08-19
- 状态：已确认，待转实现计划
- 范围：纯骨架——目录结构 + 依赖配置 + 接口/类型签名 + 前端布局壳 + 基础设施真实初始化代码。不含业务逻辑实现。

## 1. 目标

把 Friday 的代码框架搭起来，为后续六层实现落地提供可编译、可运行、结构清晰的地基。具体达到：

- `cargo tauri dev` 能启动，Tauri 窗口出现
- SQLite 数据库自动创建，schema 就位
- `tracing` 日志初始化，文件轮转就位
- 前端暗色三栏布局壳可渲染（顶栏 + 会话列表 + 主诊断区 + 输入框）
- 六层的核心 trait/类型/枚举签名已声明，实现留 `todo!()`
- 前后端类型对齐，IPC 调用入口就位
- AGENTS.md 开发命令补全

不在范围内：opencode 集成、MCP Server 实现、SSH/K8s 真实连接、真实工具实现、playbook 内容、shadcn 组件落地、图表、LLM 流式渲染逻辑。

## 2. 技术选型

| 项 | 选型 | 理由 |
|----|------|------|
| 桌面框架 | Tauri v2 | 架构文档既定；Tailwind v4 需要 v2 |
| 前端包管理器 | pnpm | 快、省磁盘、现代项目主流 |
| 前端框架 | React 19 + TypeScript + Vite 6 | 当前稳定主流 |
| 样式 | Tailwind v4（`@tailwindcss/vite` 插件） | 设计语言文档既定；v4 用 Vite 插件替代旧 PostCSS |
| 组件库 | shadcn/ui（基于 Radix UI） | 设计语言文档既定；骨架阶段不跑 `shadcn init`，实现具体组件时再加 |
| 图标 | `@phosphor-icons/react` | 设计语言文档既定 |
| 状态管理 | Zustand v5 | 轻量，适合流式事件 + 会话状态 |
| Rust async runtime | tokio（`features = ["full"]`） | 架构文档既定 |
| SSH | russh 0.45 | 架构文档既定 |
| 凭证 | keyring 3 | 跨平台 OS 密钥链，架构文档既定 |
| DB | sqlx 0.8（`runtime-tokio` + `sqlite`） | async，配合 tokio；SQLite 架构文档既定 |
| 日志 | tracing 0.1 + tracing-appender 0.2 + tracing-subscriber 0.3 | 架构文档既定 |
| 序列化 | serde 1 + serde_json 1 | Rust 标配 |
| 异步 trait | async-trait 0.1 | `ExecChannel` trait 需要 |

**暂不引入（v1 实现阶段再加）：**
- MCP SDK（集成 opencode 时引入）
- Recharts（图表实现时引入）
- shadcn CLI 初始化的组件源码

## 3. 项目目录结构

Rust 六层各成目录，子模块对齐架构文档子组件。前端按布局壳 + store + lib 分层。

```
Friday/
├── src-tauri/                      # Rust 后端
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── src/
│   │   ├── main.rs                 # 薄二进制壳，只调 lib::run()
│   │   ├── lib.rs                  # 入口：setup 钩子初始化 infra + 启动 tauri
│   │   ├── app/                    # 应用层
│   │   │   ├── mod.rs
│   │   │   ├── session.rs          # 会话管理（创建/恢复/关闭，持久化）
│   │   │   ├── credentials.rs      # 凭证管理（keyring + SQLite 标识）
│   │   │   ├── events.rs           # 事件总线（session_id 归属，推前端）
│   │   │   └── lifecycle.rs        # 生命周期编排（spawn/kill agent，连接池）
│   │   ├── agent/                  # Agent 编排层
│   │   │   ├── mod.rs
│   │   │   ├── spawn.rs            # spawn opencode CLI 子进程
│   │   │   ├── stream.rs           # 捕获流式 JSON 输出 → 推 event
│   │   │   └── prompt.rs           # prompt 拼装（含 playbook 索引）
│   │   ├── tools/                  # 诊断工具层（MCP Server）
│   │   │   ├── mod.rs
│   │   │   ├── registry.rs         # Tool Registry（name/schema/handler/risk_level）
│   │   │   ├── risk.rs             # RiskLevel 枚举 + 拦截逻辑
│   │   │   └── builtin/            # 内置工具（v1: 仅占位 mod，实现留空）
│   │   │       └── mod.rs
│   │   ├── exec/                   # 执行层
│   │   │   ├── mod.rs
│   │   │   ├── channel.rs          # ExecChannel trait
│   │   │   ├── ssh.rs              # SSH Transport（russh）
│   │   │   └── k8s.rs              # K8s Transport（kubectl exec）
│   │   ├── knowledge/              # 知识层
│   │   │   ├── mod.rs
│   │   │   └── playbook.rs         # get_playbook(symptom) → YAML 内容
│   │   └── infra/                  # 基础设施
│   │       ├── mod.rs
│   │       ├── db.rs               # SQLite 初始化 + schema（真实可运行）
│   │       └── logging.rs          # tracing + 文件轮转初始化（真实可运行）
│   ├── playbooks/                  # 知识层 YAML（独立维护）
│   │   └── .gitkeep
│   └── migrations/                 # SQLite schema 迁移
│       └── 0001_init.sql
├── src/                            # React 前端
│   ├── main.tsx                    # 入口
│   ├── App.tsx                     # 根组件（布局壳）
│   ├── pages/
│   │   └── DiagnosisPage.tsx       # 主诊断页（侧边栏 + 主区 + 输入框）
│   ├── components/
│   │   ├── layout/
│   │   │   ├── TopBar.tsx          # 顶栏（logo + 会话标题 + 状态指示）
│   │   │   ├── SessionSidebar.tsx  # 会话列表
│   │   │   └── MainDiagnosisArea.tsx
│   │   └── diagnosis/              # 占位（后续填充）
│   │       └── .gitkeep
│   ├── store/
│   │   └── sessionStore.ts         # Zustand store（会话 + 事件流状态）
│   ├── lib/
│   │   ├── ipc.ts                  # Tauri command/event 绑定
│   │   └── types.ts                # 与 Rust 共享的类型（Event/Command 枚举）
│   ├── styles/
│   │   └── globals.css             # 设计 token CSS 变量 + Tailwind 入口
│   └── assets/
├── index.html
├── package.json
├── pnpm-lock.yaml
├── tsconfig.json
├── vite.config.ts
├── AGENTS.md                       # 补全开发命令
└── docs/                           # 现有架构文档
```

## 4. Rust 各层接口定义

骨架阶段声明所有核心类型和 trait 签名，实现留 `todo!()`。唯一例外是 `infra/`（真实可运行）和 `lib.rs` 的 setup 钩子。

> 下方签名中 `Result<T>` 的错误类型在实现阶段确定（预计统一用 `anyhow::Error` 或自定义 `FridayError`），骨架阶段返回 `todo!()` 即可编译。

### 4.1 `infra/`（真实初始化）

详见第 6 节。

### 4.2 `app/`

```rust
// session.rs
pub struct SessionId(pub String);
pub struct Session {
    pub id: SessionId,
    pub env: String,
    pub service: String,
    pub symptom: String,
    pub status: SessionStatus,
}
pub enum SessionStatus { Active, Closed }

pub async fn create_session(env: &str, service: &str, symptom: &str) -> Result<Session>  // todo!()
pub async fn close_session(id: &SessionId) -> Result<()>  // todo!()

// credentials.rs
pub async fn store_secret(env_id: &str, key: &str, value: &str) -> Result<()>  // keyring, todo!()
pub async fn load_secret(env_id: &str, key: &str) -> Result<Option<String>>  // todo!()

// events.rs
pub enum AppEvent {
    AgentStarted { session_id: String, agent_pid: u32 },
    ToolExecuting { session_id: String, tool: String, args: serde_json::Value },
    ToolResult { session_id: String, tool: String, output: serde_json::Value, elapsed_ms: u64 },
    LlmThinking { session_id: String, token: String },
    ConfirmRequired { session_id: String, tool: String, args: serde_json::Value, risk_level: RiskLevel },
    AgentStopped { session_id: String },
    AgentCrashed { session_id: String, reason: String },
    DiagnosisDone { session_id: String, conclusion: String },
    SessionClosed { session_id: String },
}

pub struct EventBus { /* Tauri AppHandle sender */ }
impl EventBus {
    pub fn new(handle: tauri::AppHandle) -> Self  // 真实
    pub async fn emit(&self, session_id: &str, event: AppEvent)  // 通过 Tauri event 推前端, todo!()
}

// lifecycle.rs
pub struct LifecycleManager { /* 持有 agent 进程 + 连接池句柄 */ }
impl LifecycleManager {
    pub async fn start_diagnosis(&self, session: Session, prompt: String) -> Result<()>  // todo!()
    pub async fn stop_agent(&self, session_id: &SessionId)  // SIGTERM→SIGKILL, todo!()
    pub async fn close_session(&self, session_id: &SessionId)  // 停 agent + 断连接, todo!()
    pub async fn confirm_tool(&self, session_id: &SessionId, tool: &str)  // todo!()
    pub async fn cancel_diagnosis(&self, session_id: &SessionId)  // todo!()
}

// Tauri command handler（薄壳，调 LifecycleManager）
#[tauri::command]
pub async fn start_diagnosis_cmd(state: State<'_, AppState>, env: String, service: String, symptom: String) -> Result<String, String>  // 返回 session_id, 内部 todo!()
#[tauri::command]
pub async fn stop_agent_cmd(state: State<'_, AppState>, session_id: String) -> Result<(), String>
#[tauri::command]
pub async fn close_session_cmd(state: State<'_, AppState>, session_id: String) -> Result<(), String>
#[tauri::command]
pub async fn confirm_tool_cmd(state: State<'_, AppState>, session_id: String, tool: String) -> Result<(), String>
#[tauri::command]
pub async fn cancel_diagnosis_cmd(state: State<'_, AppState>, session_id: String) -> Result<(), String>
```

### 4.3 `agent/`

```rust
// spawn.rs
pub struct AgentProcess { pub pid: u32, child: tokio::process::Child }
pub async fn spawn_opencode(prompt: String, mcp_config_path: PathBuf) -> Result<AgentProcess>  // todo!()

// stream.rs
pub async fn consume_stream(child: AgentProcess, bus: &EventBus, session_id: &str)  // 解析流式 JSON → event, todo!()

// prompt.rs
pub fn build_prompt(env: &str, service: &str, symptom: &str, playbook_index: &str) -> String  // todo!()
```

### 4.4 `tools/`

```rust
// risk.rs
pub enum RiskLevel { ReadOnly, Low, High }

// registry.rs
pub struct ToolDef {
    pub name: String,
    pub schema: serde_json::Value,
    pub risk_level: RiskLevel,
    pub handler: Box<dyn ToolHandler>,
}
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: serde_json::Value, channel: &dyn ExecChannel) -> ToolOutput;
}
pub struct ToolOutput {
    pub success: bool,
    pub data: serde_json::Value,
    pub raw_stdout: Option<String>,
}
pub struct ToolRegistry { tools: HashMap<String, ToolDef> }
impl ToolRegistry {
    pub fn new() -> Self  // 真实
    pub fn register(&mut self, def: ToolDef)  // 真实
    pub async fn dispatch(&self, name: &str, args: serde_json::Value, channel: &dyn ExecChannel) -> ToolOutput  // 检查 risk_level + 调 handler, todo!()
}
```

### 4.5 `exec/`

```rust
// channel.rs
#[async_trait]
pub trait ExecChannel: Send + Sync {
    async fn run(&self, cmd: &str) -> Result<ExecOutput>;
    async fn connect(&self) -> Result<()>;
    async fn disconnect(&self);
}
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

// ssh.rs
pub struct SshTransport { /* russh session 字段 */ }  // todo!()
#[async_trait]
impl ExecChannel for SshTransport { /* todo!() */ }

// k8s.rs
pub struct K8sTransport { /* kubeconfig, namespace, pod */ }  // todo!()
#[async_trait]
impl ExecChannel for K8sTransport { /* todo!() */ }
```

### 4.6 `knowledge/`

```rust
// playbook.rs
pub struct Playbook {
    pub symptom: String,
    pub steps: Vec<PlaybookStep>,
    pub notes: String,
}
pub struct PlaybookStep {
    pub tool: String,
    pub args: serde_json::Value,
    pub interpret: String,
}
pub async fn get_playbook(symptom: &str) -> Option<Playbook>  // 读 playbooks/*.yaml, todo!()
```

## 5. 前端壳 + 设计 token + Store

### 5.1 `styles/globals.css`（真实，对齐设计语言文档）

- `@import` Google Fonts（JetBrains Mono 400/500/600/700 + IBM Plex Sans 300/400/500/600/700）
- `@import "tailwindcss"`（Tailwind v4 入口）
- `@theme inline` 映射所有语义色 token 到 CSS 变量，值来自设计语言文档第 2 节：
  - `--background: #0F172A`、`--foreground: #F8FAFC`、`--card: #1B2336`、`--card-foreground: #F8FAFC`
  - `--primary: #1E293B`、`--primary-foreground: #FFFFFF`、`--secondary: #334155`、`--secondary-foreground: #FFFFFF`
  - `--muted: #272F42`、`--muted-foreground: #94A3B8`、`--border: #475569`
  - `--accent: #22C55E`、`--accent-foreground: #0F172A`、`--destructive: #EF4444`
  - `--ring: #FFFFFF`
  - 自定义补充：`--warning: #EAB308`（黄）、`--info: #3B82F6`（蓝）
- 间距 token（`--space-1`=4px ~ `--space-12`=48px）
- 圆角 token（`--radius-sm`=4px / `--radius-md`=6px / `--radius-lg`=8px / `--radius-full`=9999px）
- 字号 token（`--text-xs` ~ `--text-2xl`，值与行高对齐文档）
- 动画时长 token（`--duration-fast`=150ms / `--duration-normal`=250ms / `--duration-slow`=400ms）
- 缓动函数（`--ease-out: cubic-bezier(0.16, 1, 0.3, 1)`）

### 5.2 `App.tsx` + 布局壳（真实可渲染空壳）

三栏布局，对齐设计语言文档第 4 节主布局图：
- `TopBar`（48px 高）：左侧 Friday logo 占位 + 会话标题，右侧状态指示器（静态灰点 "已停止"）
- `SessionSidebar`（240px 宽）：空会话列表 + 底部 `[+ 新会话]` 按钮（非功能占位）
- `MainDiagnosisArea`：上方空诊断区（flex-1）+ 底部输入框（多行可展开，`Shift+Enter` 换行、`Enter` 发送的键盘绑定先不接逻辑，只留 UI）
- 全部用 token 类（`bg-background`、`text-muted-foreground`、`border-border` 等），不硬编码颜色
- `pages/DiagnosisPage.tsx` 组合上述三者，订阅 `sessionStore` 但事件流接通逻辑留空

### 5.3 `store/sessionStore.ts`（Zustand，接口形状）

```typescript
interface SessionStore {
  sessions: Session[]
  currentSessionId: string | null
  eventsBySession: Record<string, AppEvent[]>
  // actions（骨架阶段实现为空操作）
  selectSession: (id: string) => void
  appendEvent: (sessionId: string, event: AppEvent) => void
  clearEvents: (sessionId: string) => void
}
```

### 5.4 `lib/types.ts`（与 Rust AppEvent 对齐的 TS 镜像）

- `AppEvent` discriminated union，variant 名与 Rust 枚举对齐：
  - `agent_started` / `tool_executing` / `tool_result` / `llm_thinking` / `confirm_required` / `agent_stopped` / `agent_crashed` / `diagnosis_done` / `session_closed`
- `Session`、`SessionStatus` 接口（`active` | `closed`）
- `RiskLevel`（`read_only` | `low` | `high`）

### 5.5 `lib/ipc.ts`（Tauri 绑定薄封装）

- `invoke('start_diagnosis', { env, service, symptom })` → `Promise<string>`（session_id）
- `invoke('stop_agent', { sessionId })`、`invoke('close_session', ...)`、`invoke('confirm_tool', ...)`、`invoke('cancel_diagnosis', ...)`
- `listen('app_event', handler)` 订阅事件流
- 骨架阶段只导出函数，不实际调用

## 6. 基础设施初始化代码（真实可运行）

### 6.1 `migrations/0001_init.sql`

```sql
-- 会话表
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    env TEXT NOT NULL,
    service TEXT NOT NULL,
    symptom TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    closed_at TEXT
);

-- 诊断步骤表（agent 思考/工具调用的流水）
CREATE TABLE IF NOT EXISTS diagnosis_steps (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    step_type TEXT NOT NULL,    -- llm_thinking / tool_call / conclusion
    content TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

-- 工具调用记录表
CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    args TEXT,
    risk_level TEXT NOT NULL,   -- read_only / low / high
    status TEXT NOT NULL,       -- executing / success / failed / confirmed
    output TEXT,
    raw_stdout TEXT,
    elapsed_ms INTEGER,
    error TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

-- 环境配置表（明文配置，凭证走 keyring）
CREATE TABLE IF NOT EXISTS environments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT,
    port INTEGER,
    user TEXT,
    transport_type TEXT NOT NULL,  -- ssh / k8s
    k8s_namespace TEXT,
    k8s_pod TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_diagnosis_steps_session ON diagnosis_steps(session_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);
```

### 6.2 `infra/db.rs`

```rust
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;

pub async fn init(app_data_dir: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_path = app_data_dir.join("friday.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    let schema = include_str!("../../migrations/0001_init.sql");
    sqlx::query(schema).execute(&pool).await?;
    tracing::info!(?db_path, "SQLite initialized");
    Ok(pool)
}
```

### 6.3 `infra/logging.rs`

```rust
use tracing_appender::rolling;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};
use std::path::PathBuf;

pub fn init(app_data_dir: PathBuf) -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = rolling::daily(&log_dir, "friday.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking))
        .init();
    tracing::info!(?log_dir, "logging initialized");
    guard
}
```

### 6.4 `lib.rs` 串联

```rust
mod app; mod agent; mod tools; mod exec; mod knowledge; mod infra;

use tauri::Manager;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: app::events::EventBus,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();
            let pool = tauri::async_runtime::block_on(infra::db::init(data_dir.clone()))?;
            let guard = infra::logging::init(data_dir);
            app.manage(AppState { db: pool, bus: app::events::EventBus::new(handle) });
            app.manage(guard);  // 保活 WorkerGuard，否则日志丢
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::lifecycle::start_diagnosis_cmd,
            app::lifecycle::stop_agent_cmd,
            app::lifecycle::close_session_cmd,
            app::lifecycle::confirm_tool_cmd,
            app::lifecycle::cancel_diagnosis_cmd,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
```

`main.rs` 只调 `friday_lib::run()`。

## 7. 依赖配置

### 7.1 `src-tauri/Cargo.toml`

```toml
[package]
name = "friday"
version = "0.1.0"
edition = "2021"

[lib]
name = "friday_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-shell = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-appender = "0.2"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
russh = "0.45"
keyring = "3"
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }
async-trait = "0.1"
dirs = "5"

[build-dependencies]
tauri-build = { version = "2", features = [] }
```

### 7.2 `package.json`

```json
{
  "name": "friday",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "preview": "vite preview",
    "typecheck": "tsc --noEmit"
  },
  "dependencies": {
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-shell": "^2",
    "@phosphor-icons/react": "^2",
    "react": "^19",
    "react-dom": "^19",
    "zustand": "^5"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2",
    "@types/react": "^19",
    "@types/react-dom": "^19",
    "@vitejs/plugin-react": "^4",
    "tailwindcss": "^4",
    "@tailwindcss/vite": "^4",
    "typescript": "^5",
    "vite": "^6"
  }
}
```

### 7.3 `tauri.conf.json` 关键项

- `productName: "Friday"`
- `identifier: "com.friday.app"`
- 前端 dev URL `http://localhost:1420`
- `beforeDevCommand: "pnpm dev"`、`beforeBuildCommand: "pnpm build"`

### 7.4 `vite.config.ts`

- `@vitejs/plugin-react`
- `@tailwindcss/vite`（Tailwind v4 的 Vite 插件）
- `server.port: 1420`、`clearScreen: false`（Tauri 要求）
- `envPrefix: ['VITE_', 'TAURI_']`
- alias `@` → `./src`

### 7.5 `tsconfig.json`

- `strict: true`、`target: ES2022`、`module: ESNext`、`jsx: react-jsx`
- `paths: { "@/*": ["./src/*"] }`

## 8. AGENTS.md 开发命令补全

在现有 AGENTS.md 的"开发命令"节补全：

```
- 构建 / 运行：cargo tauri dev（开发）/ cargo tauri build（打包）
- 前端单独运行：pnpm dev
- 前端类型检查：pnpm typecheck
- Rust 检查：cargo check --manifest-path src-tauri/Cargo.toml
- Rust 测试：cargo test --manifest-path src-tauri/Cargo.toml
- lint：TODO（待定 clippy + eslint 配置后再补）
```

## 9. 验收标准

骨架完成后需满足：

1. `cargo tauri dev` 启动成功，Tauri 窗口出现
2. 窗口显示暗色三栏布局（顶栏 + 空会话列表 + 空主诊断区 + 输入框）
3. Tauri app data dir 下生成 `friday.db`，包含 4 张表
4. Tauri app data dir 下生成 `logs/friday.log`，含 "logging initialized" 和 "SQLite initialized" 记录
5. `cargo check --manifest-path src-tauri/Cargo.toml` 无错误（`todo!()` 可编译）
6. `pnpm typecheck` 无错误
7. 前端调用 `start_diagnosis` command 能拿到 session_id（即使内部 todo，handler 要能返回值或明确 panic 而非编译失败）

## 10. 不在范围内（显式排除）

- opencode CLI 集成与 spawn 逻辑
- MCP Server 协议实现
- SSH（russh）真实连接
- K8s（kubectl exec）真实连接
- 任何诊断工具的真实实现（jstat/jcmd/arthas/读日志/读 dump）
- 风险分级拦截的真实调度
- playbook YAML 内容
- shadcn 组件源码落地
- 图表（Recharts）
- LLM 流式渲染逻辑
- 会话持久化的真实读写（仅 schema 就位，`create_session` 等 todo）
