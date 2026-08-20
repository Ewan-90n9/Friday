# Agent 自动识别 — 设计文档

- 日期：2026-08-20
- 状态：已批准（逐节通过）
- 参考：multica `server/internal/daemon/agents_probe.go` 的两阶段检测模式（PATH 发现 + 版本探测）

## 1. 背景与目标

Friday 架构决策 #13/#15：v1 集成本机 opencode agent CLI，后续扩展 claude code / codex。`agent/spawn.rs` 当前硬编码 `spawn_opencode` 且为 `todo!()`。

本功能实现**自动识别本机已安装的 agent CLI**——检测 PATH 上的二进制、解析版本、持久化到 SQLite、供 spawn 层读取 active agent、在 UI 展示并支持手动添加路径与切换 active。

**v1 范围**：注册表预置 opencode 一条；可扩展（后续加 claude/codex 仅追加常量）。检测与 spawn 解耦——检测可更广，spawn 逐步支持，但 v1 两者都只对 opencode。

**不做（YAGNI）**：
- 不做 multica 的登录 shell 回退（`$SHELL -ilc`）——Tauri 桌面应用用 `which_global` 覆盖 PATH + 常见目录；用户 nvm shim 缺失时走 manual 添加。
- 不做最低版本门禁——v1 opencode 无已知最低版本要求。
- 不做后台定时刷新循环——仅启动 + 按需检测。
- 不做 Codex macOS app bundle 路径回退——v1 不检测 codex。
- 不做 agent 认证状态探测。

## 2. 架构与模块归属

在现有 `agent/` 层内新增关注点，不扰动其他层。

```
agent/
  mod.rs        ← +pub mod registry; +pub mod detect;
  registry.rs   ← 新（静态描述符）
  detect.rs     ← 新（纯检测）
  spawn.rs      ← 改（从 DB 读 active agent）
  prompt.rs     ← 不变
  stream.rs     ← 不变
app/
  mod.rs        ← +pub mod agents;
  agents.rs     ← 新（command + 持久化）
  lifecycle.rs  ← 不变
  ...
migrations/
  0002_agents.sql ← 新
```

**`agent/registry.rs`** — 已知 agent 的静态目录：
```rust
pub struct AgentDescriptor {
    pub provider: &'static str,      // "opencode"
    pub command: &'static str,       // "opencode"（PATH 查找的命令名）
    pub display_name: &'static str,  // "OpenCode"
}
pub const REGISTRY: &[AgentDescriptor] = &[
    AgentDescriptor { provider: "opencode", command: "opencode", display_name: "OpenCode" },
];
```
无 trait、无插件机制。后续加 claude/codex = 追加常量。

**`agent/detect.rs`** — 纯检测逻辑，不碰 DB/IPC，无副作用，可单测。

**`app/agents.rs`** — 编排模块（与 `app::lifecycle` 同级）。持有 5 个 Tauri command + DB 持久化逻辑。桥接 `agent::detect`（纯）↔ SQLite ↔ 前端。

**集成点**：
- `lib.rs` `setup`：DB 初始化后、manage AppState 之前，调 `app::agents::detect_and_persist(&pool).await` 播种。
- `agent/spawn.rs`：改为读 DB active agent。
- `Cargo.toml`：加 `which = "7"`、`regex = "1"`、`thiserror = "2"`。

## 3. 数据模型

新增迁移 `migrations/0002_agents.sql`：

```sql
CREATE TABLE IF NOT EXISTS agents (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    display_name TEXT NOT NULL,
    path         TEXT NOT NULL,
    version      TEXT,
    source       TEXT NOT NULL,             -- 'auto' | 'manual'
    is_active    INTEGER NOT NULL DEFAULT 0,
    detected_at  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_agents_active ON agents(is_active) WHERE is_active = 1;
```

**不变式：恰好一条 `is_active = 1`。** 部分唯一索引保证至多一条。`ensure_active` 在无 active 时把首条 auto 行（按 `detected_at` 降序）置 active。

**两条数据来源**：
- **auto**：`detect_and_persist` 按 `provider` 且 `source='auto'` upsert（存在则更新 path/version/detected_at，不存在则插入），不触碰 `is_active`。
- **manual**：`add_agent_cmd` 插入（provider + 用户给定 path，同样跑 `--version` 取版本）。

允许同 provider 多条（manual 可加不同路径）。`provider` 不唯一，`id`（UUID）才是主键。

**删除策略**：`remove_agent_cmd(id)` 删指定行；若删的是 active 行 → `ensure_active` 补位；无 auto 行则 `is_active` 全空，`start_diagnosis` 返回"无可用 agent"错误。

时间字段存 ISO 8601 字符串，与现有 `sessions.created_at` 一致。

## 4. 检测机制（`agent::detect`）

两阶段，对齐 multica 的 discovery + qualification：

**阶段 1 — PATH 发现**：遍历 `REGISTRY`，对每个 `command` 调 `which::which_global(command)`（查用户 PATH + 常见安装目录，Windows 原生补 `.exe`）。找到返回绝对路径，找不到跳过。

**阶段 2 — 版本探测**：对已找到的 agent 跑 `Command::new(&path).arg("--version")`，取 stdout，用正则 `v?(\d+)\.(\d+)\.(\d+)` 提取首个 semver。失败（命令不支持、超时、无 semver）则 `version = None`——agent 仍记为已安装，不阻塞检测。

```rust
pub struct DetectedAgent {
    pub provider: &'static str,
    pub display_name: &'static str,
    pub path: PathBuf,
    pub version: Option<String>,
}

pub async fn detect() -> Vec<DetectedAgent> {
    let mut found = Vec::new();
    for desc in registry::REGISTRY {
        if let Ok(path) = which::which_global(desc.command) {
            let version = detect_version(&path).await;
            found.push(DetectedAgent { provider: desc.provider, display_name: desc.display_name, path, version });
        }
    }
    found
}

async fn detect_version(path: &Path) -> Option<String> {
    let out = tokio::time::timeout(Duration::from_secs(5),
        tokio::process::Command::new(path).arg("--version").output())
        .await.ok()?.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let re = regex::Regex::new(r"v?(\d+)\.(\d+)\.(\d+)").ok()?;
    re.captures(&text).map(|c| format!("{}.{}.{}", &c[1], &c[2], &c[3]))
}
```

**超时**：版本探测 5s 超时 → `version = None`，agent 保留。

**错误隔离**：单个 agent 失败不影响其他；`detect()` 只收集成功项，永远不返回 `Err`。

## 5. 持久化与编排（`app::agents`）

**内部函数：**

```rust
pub async fn detect_and_persist(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let detected = agent::detect::detect().await;
    for d in &detected {
        upsert_auto_agent(pool, d).await?;
    }
    ensure_active(pool).await?;
    Ok(())
}
```

- `upsert_auto_agent`：按 `provider` 且 `source='auto'` 查找——存在则更新 `path/version/detected_at`，不存在则插入。不触碰 `is_active`。
- `ensure_active`：当前无 active 行 → 把首条 auto 行（按 `detected_at` 降序）置 active；已有 active → 不动。

**5 个 Tauri command（`Result<_, String>`）：**

| command | 入参 | 出参 | 行为 |
|---|---|---|---|
| `detect_agents_cmd` | 无 | `()` | 调 `detect_and_persist` |
| `list_agents_cmd` | 无 | `Vec<AgentRow>` | 读全表，active 在前、auto 在前排序 |
| `add_agent_cmd` | `provider, path` | `AgentRow` | 跑 `detect_version(path)` → 插 manual 行，返回该行 |
| `set_active_agent_cmd` | `id` | `()` | 事务：全置 0 → 目标置 1 |
| `remove_agent_cmd` | `id` | `()` | 删行；若删的是 active → `ensure_active` 补位 |

**`provider` 取值约束**：v1 限定为 registry 内已注册的 provider（即 `"opencode"`）。原因——`spawn_active` 的子进程参数是 provider 特定的（v1 只有 opencode 的参数），手动添加 registry 外的 provider（如 `"claude"`）会注册成功但 spawn 时因参数不匹配而失败。前端 provider 下拉从 `registry::REGISTRY` 渲染，v1 只有一项。后续支持 claude/codex 时，先在 registry 加条目 + 在 spawn.rs 加对应参数分支，再开放下拉。

**`AgentRow`（serde，snake_case 对齐前端）：**
```rust
#[derive(Serialize)]
pub struct AgentRow {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    pub path: String,
    pub version: Option<String>,
    pub source: String,
    pub is_active: bool,
    pub detected_at: String,
}
```

DB 里 `is_active` 是 `INTEGER 0/1`，sqlx `query_as` 自动转 `bool`。

**启动时序（`lib.rs` setup）：**
```
infra::logging::init
infra::db::init
app::agents::detect_and_persist(&pool)   ← 新增
app.manage(AppState { db, bus })
```
播种在 manage AppState 之前完成。

## 6. 前端

### 6.1 IPC / 类型 / Store

**`src/lib/ipc.ts`** 新增 5 个绑定（与现有 `startDiagnosis` 同风格，`invoke` + snake_case 参数）：
```ts
export async function detectAgents(): Promise<void> { return invoke<void>("detect_agents_cmd"); }
export async function listAgents(): Promise<AgentRow[]> { return invoke<AgentRow[]>("list_agents_cmd"); }
export async function addAgent(provider: string, path: string): Promise<AgentRow> { return invoke<AgentRow>("add_agent_cmd", { provider, path }); }
export async function setActiveAgent(id: string): Promise<void> { return invoke<void>("set_active_agent_cmd", { id }); }
export async function removeAgent(id: string): Promise<void> { return invoke<void>("remove_agent_cmd", { id }); }
```

**`src/lib/types.ts`** 新增 `AgentRow`（字段与 Rust `AgentRow` snake_case 对齐）：
```ts
export interface AgentRow {
  id: string; provider: string; display_name: string; path: string;
  version: string | null; source: "auto" | "manual"; is_active: boolean; detected_at: string;
}
```

**`src/store/agentStore.ts`（新）** — zustand store，与 `sessionStore` 同模式。`refresh` = `detectAgents()` 后再 `load()`；`setActive`/`remove` 在 IPC 成功后 `load()` 刷新本地状态：
```ts
interface AgentStore {
  agents: AgentRow[];
  activeAgent: AgentRow | null;     // agents.find(a => a.is_active) ?? null
  loading: boolean;                 // detect/list 进行中
  error: string | null;             // 最近一次操作错误（展示后清空）
  refresh: () => Promise<void>;
  load: () => Promise<void>;
  addManual: (provider: string, path: string) => Promise<void>;
  setActive: (id: string) => Promise<void>;
  remove: (id: string) => Promise<void>;
}
```

### 6.2 新增/修改前端文件

| 文件 | 动作 | 内容 |
|------|------|------|
| `src/lib/ipc.ts` | 改 | +5 个 agent IPC 绑定 |
| `src/lib/types.ts` | 改 | +`AgentRow` 接口 |
| `src/store/agentStore.ts` | 新 | zustand store |
| `src/components/agents/AgentSettingsDialog.tsx` | 新 | `<dialog>` 弹窗主组件 |
| `src/components/agents/AgentListItem.tsx` | 新 | 列表行（含设为当前/移除确认） |
| `src/components/layout/TopBar.tsx` | 改 | 状态药丸接 `agentStore` + 打开弹窗 |
| `src/pages/DiagnosisPage.tsx` | 改 | 挂载时 `agentStore.refresh()` |

### 6.3 TopBar 状态药丸

复用设计语言 §5.6 状态指示器模式 + §2 语义色 token。替换现有 `TopBar.tsx` 里"待机"灰点药丸，改为读 `agentStore.activeAgent`：

| 状态 | 视觉 | 文本 | 点色 token | 文本色 |
|------|------|------|-----------|--------|
| 有 active + 有版本 | 静态点 + 文本 | `opencode v0.2.15` | `--success` | `--foreground` |
| 有 active + 无版本 | 静态点 + 文本 | `opencode 版本未知` | `--success` | `--foreground` |
| 检测中 | 脉冲点 + 文本 | `检测中…` | `--accent` | `--muted-foreground` |
| 无 active | 静态点 + 文本 | `未检测到 Agent` | `--destructive` | `--muted-foreground` |
| 检测失败 | 静态点 + 文本 | `检测失败` | `--warning` | `--muted-foreground` |

- 点尺寸 6px（`w-1.5 h-1.5 rounded-full`），脉冲动画用 §8 `--duration-slow` + `opacity 0.5↔1.0`，尊重 `prefers-reduced-motion`。
- 药丸容器：`bg-muted/50` + `px-2.5 py-1 rounded-md`，点击打开设置弹窗（药丸整体可点，`<button>` 语义）。
- 文本用 `--text-xs` / `JetBrains Mono`（与品牌一致），`version` 部分用 `--muted-foreground` 降低层级。
- 齿轮按钮保留，行为与药丸一致（都打开弹窗），避免两个入口混淆——齿轮 `aria-label="Agent 设置"`。

### 6.4 AgentSettingsDialog

**弹窗基座——原生 `<dialog>`**（不引 Radix，`package.json` 无依赖增加）：
- `<dialog ref={ref}>` + `ref.showModal()` 打开 / `ref.close()` 关闭。`showModal()` 原生提供：焦点陷阱、ESC 关闭、顶层层叠（自动置于所有内容之上）。
- **scrim**：`dialog::backdrop { background: rgba(0,0,0,0.6); backdrop-filter: blur(2px); }`——纯黑基底上用 60% 黑 + 轻模糊隔离前景，保证前景文本 ≥4.5:1。
- **层叠层级**：`z-50`（html-tailwind 验证：用 `z-*` 量级，不用 `z-[9999]`）。原生 `<dialog>` 本身在顶层，但显式 `z-50` 保持与未来 fixed 元素的量级一致。
- **尺寸**：`w-[480px] max-w-[90vw]`，`rounded-xl`（`--radius-xl` 16px），`bg-card` + `border border-border`。
- **进入动画**：`scale(0.96→1) + opacity(0→1)`，200ms `--ease-out`（§8.3）；`prefers-reduced-motion` 时直接显示。

**布局：**
```
┌─────────────────────────────────────────────┐
│ Agent 设置                            [✕]    │  ← 标题 + 关闭
├─────────────────────────────────────────────┤
│ [重新检测]                                   │  ← 工具栏
├─────────────────────────────────────────────┤
│ ● OpenCode                    v0.2.15 [auto] │  ← active 行（左 2px 绿边条）
│   /usr/local/bin/opencode                    │
│   [设为当前]  [移除]                          │
│ ─────────────────────────────────────────── │
│ ○ OpenCode                    v0.1.0 [manual]│  ← 非 active 行
│   /home/u/.local/bin/opencode                │
│   [设为当前]  [移除]                          │
├─────────────────────────────────────────────┤
│ 手动添加                                     │  ← 折叠区
│   Provider [opencode ▾]   路径 [__________]  │
│   [添加]                                     │
└─────────────────────────────────────────────┘
```

**agent 列表行（`AgentRow` → `AgentListItem`）：**
- active 行：左侧 2px `--success` 边条（对齐 §5.1 选中态约定）+ 点用 `--success`；非 active 行：点用 `--muted-foreground`。
- `display_name`：`--text-sm` / `--foreground` / `IBM Plex Sans`；`version`：`--text-xs` / `JetBrains Mono` / `--muted-foreground`，`null` 显示"版本未知"。
- `path`：`--text-xs` / `JetBrains Mono` / `--muted-foreground`，`truncate` + `title={path}`（hover 看全路径）。
- source 标签：`auto` / `manual` badge，`--text-xs` / `--radius-sm` / `bg-muted/50` + `text-muted-foreground`。
- 行内动作（右侧，`--text-xs` 文字按钮，非 active 才显示"设为当前"）：
  - **设为当前** → `setActive(id)`；成功后行变为 active 态（即时视觉反馈，§UX 验证：success feedback）。
  - **移除** → **二次确认**（§UX 验证：destructive action 必须 confirm）：行内展开 `确认移除？[确认][取消]`，或小型 `confirm` 子弹窗；确认后 `remove(id)`。
- 行 hover：`bg-surface-3`（§2 token），`--duration-fast` 过渡。

**手动添加（折叠区，默认收起）：**
- provider 下拉：v1 只一项 `opencode`（从后端 `REGISTRY` 渲染；前端可硬编码为 `["opencode"]`，v1 无需新增 IPC 拉 registry）。
- 路径输入：`<input type="text">`，`bg-muted` + `border-border` + `--text-sm` / `JetBrains Mono`，占位"可执行文件绝对路径"。
- 「添加」按钮：`addManual(provider, path)`；成功后新行出现在列表顶部（manual 排序在 auto 之后？——见下排序），失败在输入框下显示 `--text-xs` / `--destructive` 错误。

**列表排序**：active 行置顶 → 其余按 `source`（auto 优先）→ `detected_at` 降序。与后端 `list_agents_cmd` 排序一致。

**空状态**：列表为空时显示居中提示"未检测到 Agent，点击上方[重新检测]或手动添加路径"，`--muted-foreground` / `--text-sm`，配 `Robot`（Phosphor）图标 32px。

**检测中态**：`refresh()` 期间「重新检测」按钮禁用 + `CircleNotch` spinner 旋转（`--duration-normal`），列表保持旧数据（不闪空），完成后替换。

**焦点管理（§UX 验证）：**
- 打开时焦点落到弹窗内首个可交互控件（「重新检测」按钮）。
- 所有控件可见焦点环：`focus-visible:outline 2px solid var(--color-ring)` + `outline-offset: 2px`（≥2px 周长 + 3:1 对比，§UX Result 1/2）。
- ESC 关闭（`<dialog>` 原生）；关闭后焦点回到触发器（药丸/齿轮）——`ref.close()` 后手动 `triggerRef.current?.focus()`。
- 焦点不被 sticky 元素遮挡（§UX Result 3）——弹窗内无 sticky 头/底，内容区可滚动。

### 6.5 加载时机

- `DiagnosisPage` 挂载时调 `agentStore.refresh()` 一次（启动已在后端播种，此处刷新拉取给 UI）。
- 弹窗打开时不重复 `refresh()`（避免每次打开都跑 `--version` 探测，5s 超时累积）——展示 `load()` 缓存数据，用户点「重新检测」才主动刷新。

### 6.6 无障碍

- 药丸 `<button aria-label>` 含当前状态（"Agent: opencode v0.2.15，点击打开设置"）。
- 弹窗 `<dialog aria-label="Agent 设置">`。
- 「设为当前」按钮 `aria-pressed` 反映 active 状态。
- 状态点为装饰性，`aria-hidden="true"`（信息已由文本承载，不依赖颜色 alone——§7.1 无障碍约定）。
- 移除确认不依赖颜色，用明确文字"确认移除？"。

## 7. spawn 集成与错误处理

**`agent/spawn.rs`** — 从硬编码改为读 DB active agent：
```rust
pub async fn spawn_active(
    pool: &SqlitePool,
    prompt: String,
    mcp_config_path: PathBuf,
) -> Result<AgentProcess, SpawnError> {
    let agent = sqlx::query_as!(ActiveAgent, "SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
        .fetch_optional(pool).await?
        .ok_or(SpawnError::NoActiveAgent)?;
    let child = tokio::process::Command::new(&agent.path)
        .arg(/* opencode 子进程参数 */)
        .spawn()?;
    Ok(AgentProcess { pid: child.id().unwrap(), child })
}
```

**`SpawnError`（新增，thiserror）：**
```rust
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("无可用 agent，请先检测或手动添加")]
    NoActiveAgent,
    #[error("agent 二进制不存在：{path}")]
    BinaryMissing { path: String },
    #[error("启动 agent 失败：{0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("DB 查询失败：{0}")]
    Db(#[from] sqlx::Error),
}
```

**范围边界**：本功能只打通"读 active agent → spawn"这一段。`start_diagnosis_cmd` 的完整逻辑（prompt 组装、流式推送、MCP config 生成）仍 `todo!()`。`SpawnError` 在 `start_diagnosis_cmd` 里转 `String` 返回前端。

**并发安全**：`detect_and_persist` 与 `spawn_active` 走 `&SqlitePool`（多连接 + 事务）；`set_active` 单事务保证原子性。

## 8. 测试策略

- **`agent/detect.rs`**：单测 `detect_version` 的正则解析——喂字符串样本（`"opencode 0.2.15"`、`"v1.0.0-beta"`、无版本行），不测 `which` 真实 PATH 查找。与 PATH 解耦。
- **`app/agents.rs`**：用 `tempfile` + in-memory SQLite（沿用 `db::init` 测试模式）测：
  - `upsert_auto_agent`：同 provider 二次检测更新而非插入。
  - `ensure_active`：空表 → 无 active；插入 auto 后 → 首条置 active。
  - `set_active`：切换后恰好一条 active。
  - `remove_agent_cmd`：删 active 行后 `ensure_active` 补位。
- **`agent/spawn.rs`**：测 `NoActiveAgent` 错误路径（空表查询），不 spawn 真实 opencode。

## 9. 新增依赖

```toml
which = "7"        # PATH 查找
regex = "1"        # semver 解析
thiserror = "2"    # SpawnError 派生
```
