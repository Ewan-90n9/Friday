# Friday 文件管理规则设计

> 日期：2026-08-21
> 状态：已确认，待实现

## 1. 背景与动机

Friday 是远程环境运行时故障诊断 Agent。当前已有日志、SQLite、agent 检测等骨架功能落地，但文件存放位置散落在各模块内联 `join`，且即将引入的 playbooks（agent 运行时生成）、skills、远程产物缓存等新类别尚无位置约定。

若不在现在统一规则，后续功能实现时各模块自行决定路径，会导致：
- 路径散落难审计，新增类别时位置不一致
- 测试时路径 fixture 无法复用
- 日后整改成本随功能膨胀增长

本设计在功能尚未膨胀前，一次性约定所有运行时文件的位置与解析方式。

## 2. 决策汇总

| 类别 | 策略 | 位置 | 说明 |
|------|------|------|------|
| SQLite | 持久 | `<app_data>/friday.db` | sessions/agents/diagnosis_steps/tool_calls/environments |
| 运行日志 | 每日轮转 + 7 天清理 | `<app_data>/logs/` | tracing-appender，已实现 |
| Playbook | agent 运行时生成 | `<app_data>/playbooks/` | 诊断知识 YAML，用户可编辑 |
| Friday skill | agent 运行时生成 | `<app_data>/skills/` | 能力包，用户可编辑 |
| Prompt | 代码内 const（默认） + 覆盖层 | `<app_data>/prompts/` | v1 为空；有 friday.md 则完全覆盖内置默认 |
| 远程产物 | 持久保留可回看 | `<app_data>/artifacts/<session_id>/` | heap dump、远端日志等 |
| 诊断过程数据 | SQLite | `diagnosis_steps`/`tool_calls` 表 | 已实现 |
| 凭证 | OS 密钥链 | 不落文件 | 已实现 |
| 路径解析 | 集中式 `infra/paths.rs` | 统一从 `app_data_dir` 解析 | 新增 |

## 3. 明确不纳入 Friday 文件管理的边界

| 项 | 归属 | 说明 |
|----|------|------|
| opencode 工作环境/skill | 用户 `~/.opencode/` | Friday 不管理，spawn 已设 PWD=home 复用 |
| SSH 私钥 | 用户 `~/.ssh/` | 引用不复制 |
| 凭证（密码/API key） | OS 密钥链 | 不落文件 |
| migrations | 编译进二进制 `include_str!` | 非运行时文件 |
| 真正临时文件 | `std::env::temp_dir()` | 若出现纯临时需求，不持久化 |

## 4. 总体目录布局

**根目录**：Tauri `app_data_dir()`（identifier `com.friday.app`）。所有 Friday 管理的文件都在这一个根下，无随包只读资源。

- Windows: `%APPDATA%\com.friday.app`
- macOS: `~/Library/Application Support/com.friday.app`
- Linux: `~/.local/share/com.friday.app`

```
<app_data>/                          # = handle.path().app_data_dir()
├── friday.db                        # SQLite
├── logs/
│   └── friday.log.{date}            # tracing 每日轮转, 7 天自动清理 [已实现]
├── playbooks/                       # agent 运行时生成的诊断知识 (YAML), 用户可编辑
├── skills/                          # Friday 自有 skill (agent 生成, 能力包)
├── prompts/                         # 预留: 未来 GUI 编辑人格 prompt 的覆盖层
│                                    #   v1 为空 — 代码内 const 作默认; 有 friday.md 则覆盖
└── artifacts/                       # 从目标机拉取的产物, 按会话隔离, 持久保留
    └── <session_id>/
        ├── heapdump-<ts>.hprof
        ├── remotelog-<ts>.log
        └── ...
```

**源码树清理**：`src-tauri/playbooks/`（当前仅 `.gitkeep`）删除——playbooks 已改为运行时生成于 `app_data`，源码树不再保留占位目录。

## 5. 路径解析模块 `infra/paths.rs`

### 5.1 职责

单一事实源，集中解析所有运行时路径；启动时一次性建好目录。

### 5.2 接口

```rust
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// 从 Tauri app_data_dir 构造。
    pub fn new(root: PathBuf) -> Self { ... }

    // —— 文件（单文件，非目录） ——
    pub fn db_path(&self) -> PathBuf              // <root>/friday.db

    // —— 目录（返回路径，不负责创建） ——
    pub fn log_dir(&self) -> PathBuf              // <root>/logs
    pub fn playbooks_dir(&self) -> PathBuf        // <root>/playbooks
    pub fn skills_dir(&self) -> PathBuf           // <root>/skills
    pub fn prompts_dir(&self) -> PathBuf          // <root>/prompts
    pub fn artifacts_dir(&self) -> PathBuf        // <root>/artifacts

    /// 会话级产物目录：artifacts/<session_id>/
    pub fn session_artifacts_dir(&self, session_id: &str) -> PathBuf

    /// 启动时调用：创建所有需要的子目录（logs/playbooks/skills/prompts/artifacts）。
    /// 目录已存在则跳过，不报错。不创建 friday.db 文件（db.rs 负责）。
    pub fn ensure_dirs(&self) -> std::io::Result<()>
}
```

### 5.3 设计要点

1. **`Paths` 存入 `AppState`**——与现有 `db`/`bus`/`agents`/`filter_handle` 并列为 managed state。`Paths` 是轻量 `PathBuf` 包装，无连接池开销。调用侧统一从 `State<AppState>` 取路径，避免路径在调用链手工传递。

2. **`ensure_dirs()` 幂等**——`create_dir_all` 天然幂等，重复调用安全。当前 `logging.rs:21` 和 `lib.rs:29` 各自 `create_dir_all` 的散落逻辑收敛到这里。

3. **session_artifacts_dir 延迟创建**——`ensure_dirs()` 只建 `artifacts/` 顶层；`<session_id>/` 子目录在工具层首次写入产物时按需 `create_dir_all`，避免空会话目录。

4. **路径安全**——`session_id` 来自内部生成（UUID），不含用户输入，拼路径无注入风险。若未来 session_id 来源变化，加校验。

5. **测试友好**——`Paths::new(tmp.path())` 可注入临时目录，替代当前每个测试各自 `tempfile::tempdir()` + 手动 join 的散乱模式。

### 5.4 AppState 变更

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,    // 新增
}
```

## 6. Prompt 覆盖层

### 6.1 加载规则

启动时检查 `<app_data>/prompts/friday.md` 是否存在：
- 不存在 → 用代码内 `const FRIDAY_SYSTEM_PROMPT`（当前行为，零影响）
- 存在 → 读文件内容作 system prompt，**完全覆盖**内置默认

**为什么是"完全覆盖"而非"追加/合并"**：人格 prompt 是一个整体语义单元，部分合并语义不可控；用户选择放文件就是想完整自定义。

### 6.2 接口

```rust
// agent/prompt.rs

const FRIDAY_SYSTEM_PROMPT: &str = r#"..."#;  // 内置默认, 保留

/// 构建 system prompt。
/// 若 override_path 指向的文件存在且非空，用其内容完全覆盖内置默认。
pub fn build_system_prompt(override_path: Option<&Path>) -> String {
    if let Some(path) = override_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                return content;
            }
        }
    }
    FRIDAY_SYSTEM_PROMPT.to_string()
}

pub fn build_prompt(message: &str, override_path: Option<&Path>) -> String {
    let system = build_system_prompt(override_path);
    format!("{system}\n\n---\n\n用户消息：{message}")
}
```

调用侧（`spawn.rs` 或 `lifecycle.rs`）从 `State<AppState>` 取 `paths.prompts_dir().join("friday.md")` 传入。

## 7. Artifacts（远程产物）管理

### 7.1 目录结构

```
<app_data>/artifacts/<session_id>/
├── heapdump-<timestamp>.hprof
├── remotelog-<timestamp>.log
└── ...
```

### 7.2 命名约定

`<类型>-<时间戳>.<扩展>`。时间戳用 UTC `yyyyMMddHHmmss`（如 `heapdump-20260821143052.hprof`）。类型和扩展由工具层定义，本设计只定位置和命名模式。

### 7.3 写入时机

工具层从远端拉取产物后，写入 `paths.session_artifacts_dir(session_id)`。首次写入时 `create_dir_all`（延迟创建，见 §5.3）。

### 7.4 回看访问

产物文件的**路径引用**记入 SQLite `tool_calls` 结果（在结果 JSON 里带 `artifact_path` 字段），前端从 DB 查历史会话时能定位到文件。本设计不定 DB schema 细节，只约定"路径引用入 DB"原则。

### 7.5 清理策略

持久保留，不做自动清理。理由：
- 用户明确要"持久保留可回看"
- dump 文件大，但内网桌面机磁盘非瓶颈
- 清理会丢失诊断证据
- v2 可加"清理历史产物"UI 功能；v1 不做

### 7.6 安全

产物可能含敏感信息（堆内对象、日志含业务数据）。与日志规范一致（不脱敏），内网环境不特殊处理。文件权限继承 `app_data_dir` 默认（OS 用户级）。

## 8. Playbook 与 Skill 约定

### 8.1 Playbook

**位置**：`<app_data>/playbooks/`

**格式**：YAML（与 `knowledge/playbook.rs` 现有 struct 对齐）

```yaml
# 文件名: <symptom-key>.yaml  (如 oom.yaml, cpu-spike.yaml)
symptom: "OOM"
steps:
  - tool: jstat
    args: { pid: "{target_pid}", option: "gcutil" }
    interpret: "如果 OU 接近 Old max，老年代接近打满"
  - tool: read_heap_dump
    args: { path: "{artifact_path}" }
    interpret: "找大对象占用的 GC root"
notes: "JDK 8+ 适用；JDK 11 以上可先用 jcmd GC.heap_info 替代 jstat"
```

**命名约定**：`<symptom-key>.yaml`，kebab-case，如 `oom.yaml`、`cpu-spike.yaml`、`connection-pool-exhausted.yaml`。一个文件一个 playbook。

**`get_playbook` 加载逻辑**：

```rust
pub async fn get_playbook(playbooks_dir: &Path, symptom: &str) -> Option<Playbook> {
    let key = symptom.to_lowercase().replace(' ', "-");
    let path = playbooks_dir.join(format!("{key}.yaml"));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&content).ok()
}
```

**用户编辑**：YAML 文本文件，用户可直接编辑。格式错误时 `get_playbook` 返回 `None`，并 `tracing::warn!` 记录解析失败。

### 8.2 Friday Skill

**位置**：`<app_data>/skills/`

**格式**：本设计**不定具体格式**。当前 Friday 没有 skill struct 或加载逻辑，过早定格式是 YAGNI。只约定：
- 一个 skill 一个子目录：`<app_data>/skills/<skill-name>/`
- 子目录内容（prompt、脚本、配置）由未来 skill 机制定义
- 目录存在即表示 skill 可用，加载逻辑未来实现时再定

## 9. 与现有架构文档的关系

### 9.1 需更新的文档

| 文档 | 改动 | 内容 |
|------|------|------|
| `docs/architecture/infrastructure.md` | 补充"文件布局"章节 | 完整目录树 + 各类别位置 + 边界（不纳入的项） |
| `docs/architecture/playbook.md` | 修订 playbook 位置 | 当前"独立成 `playbooks/` 目录"→ 改为 "`<app_data>/playbooks/`，agent 运行时生成，用户可编辑" |
| `docs/architecture/overview.md` | 修订决策 #11 存储 | 当前"SQLite"→ 补充"文件布局统一在 `app_data_dir`，见 infrastructure.md" |
| `docs/architecture/overview.md` | 修订"临时 MCP config 文件"描述 | 改为"MCP config 注入走 opencode 自身配置机制，Friday 不单独管理临时配置文件" |
| `AGENTS.md` | 补充文件管理约定 | "修改/新增运行时文件时，路径通过 `infra/paths.rs` 统一解析，不内联 join" |

### 9.2 不改动

- `docs/architecture/logging-standard.md` — 日志位置不变，只是路径来源从内联 join 改为 `Paths::log_dir()`。
- `docs/architecture/runtime.md` / `error-handling.md` — 不涉及文件布局。

## 10. 实现改动清单

### 10.1 新增文件

| 文件 | 内容 | 规模 |
|------|------|------|
| `src-tauri/src/infra/paths.rs` | `Paths` struct + 方法 + `ensure_dirs()` + 单元测试 | ~80 行 |

### 10.2 修改文件

| 文件 | 改动 | 规模 |
|------|------|------|
| `src-tauri/src/infra/mod.rs` | 加 `pub mod paths;`（现有 2 行 → 3 行） | 1 行 |
| `src-tauri/src/lib.rs` | setup 中构造 `Paths`，`ensure_dirs()`，存入 `AppState`；`logging::init` / `db::init` 改为接收具体路径 | ~10 行 |
| `src-tauri/src/infra/logging.rs` | `init(app_data_dir: PathBuf)` → `init(log_dir: PathBuf)`，去掉内部 `.join("logs")` 和 `create_dir_all` | ~3 行 |
| `src-tauri/src/infra/db.rs` | `init(app_data_dir: PathBuf)` → `init(db_path: PathBuf)`，去掉内部 `.join("friday.db")` | ~3 行 |
| `src-tauri/src/agent/prompt.rs` | `build_prompt` 加 `override_path: Option<&Path>` 参数 + `build_system_prompt` 函数 | ~15 行 |
| `src-tauri/src/agent/spawn.rs` | 调用 `build_prompt` 时传入 `paths.prompts_dir().join("friday.md")` | ~5 行 |
| `src-tauri/src/app/lifecycle.rs` | `send_message_cmd` 调用链传 `Paths`（从 `State<AppState>` 取）到 spawn | ~3 行 |
| `src-tauri/src/knowledge/playbook.rs` | `get_playbook` 签名加 `playbooks_dir: &Path` 参数。§8.1 的加载逻辑是目标设计，但当前 agent 尚未实现 playbook 生成，无文件可加载，因此函数体保持 `todo!()`，仅对齐签名 | ~2 行 |

### 10.3 删除

| 项 | 说明 |
|----|------|
| `src-tauri/playbooks/` 目录 | 仅含 `.gitkeep`，playbooks 已改为运行时生成于 `app_data` |

### 10.4 文档更新

见 §9.1。

### 10.5 测试

| 测试 | 位置 | 验证 |
|------|------|------|
| `Paths::ensure_dirs` 创建所有目录 | `paths.rs` `#[cfg(test)]` | 建后 5 个子目录存在 |
| `Paths::ensure_dirs` 幂等 | 同上 | 重复调用不报错 |
| `Paths::session_artifacts_dir` 拼接正确 | 同上 | 路径含 session_id |
| `build_system_prompt` 无覆盖文件时用默认 | `prompt.rs` 测试 | 返回内置 const |
| `build_system_prompt` 有覆盖文件时用文件内容 | 同上 | 返回文件内容 |
| `build_system_prompt` 覆盖文件为空时 fallback | 同上 | 空文件 → 用默认 |
| 现有 `logging.rs` / `db.rs` 测试 | 适配新签名 | 传入 `Paths` 或具体 `PathBuf` |

### 10.6 不做（YAGNI）

- artifacts 清理 / 配额
- skill 加载逻辑与格式定义
- playbook 生成逻辑（agent 实现）
- prompt GUI 编辑器
- MCP config 文件管理
