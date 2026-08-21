# 多 Provider 支持 — codeagentcli 接入设计

- 日期：2026-08-21
- 状态：已实现（v0.2.3 发布，issue #1 已关闭）
- 关联 issue：#1「需要支持 codeagentcli」

## 1. 背景与目标

Friday 当前 agent 子系统硬编码绑定 opencode：registry 只有一条、spawn 硬编码 `opencode run --format json`、stream 解析器为 opencode 专属、DB 列名为 `opencode_session_id`。

issue #1 要求新增 codeagentcli 作为可选 agent 后端，用户可在设置中切换 opencode / codeagentcli。两者**同时支持**，不是替换。

### opencode vs codeagentcli CLI 差异

| 特性 | opencode | codeagentcli |
|------|----------|-------------|
| 非交互模式 | `run` 子命令 | `-p` / `--print` 标志 |
| JSON 流式输出 | `--format json` | `--output-format stream-json --verbose --skip-safe-check` |
| 跳过权限 | `--dangerously-skip-permissions` | `--dangerously-skip-permissions`（相同） |
| 会话恢复 | `--session <id>` | `--sessions <id>` |
| 版本探测 | `--version` | `--version`（也有 `-v`） |
| Windows 安装方式 | npm 包，需 shim → native exe 解析 | 独立安装（`.bat`），跳过 exe 解析 |

> **实测发现的额外约束**：codeagentcli 的 `stream-json` 输出在 `--print` 模式下要求同时传 `--verbose`，否则报错退出。`--skip-safe-check` 用于跳过信任对话框（虽不阻塞但产生 stderr 噪音）。

### NDJSON 输出格式差异

设计阶段假设两端格式相同，**实测发现完全不同**：

| 事件 | opencode | codeagentcli |
|------|----------|-------------|
| 文本输出 | `{"type":"text","part":{"text":"..."}}` | `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}` |
| 推理过程 | `{"type":"reasoning","part":{"text":"..."}}` | `{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"..."}]}}` |
| 会话初始化 | `session.created` 事件 | `{"type":"system","subtype":"init",...}` |
| 完成信号 | 无（靠 stdout EOF） | `{"type":"result","subtype":"success","result":"..."}` |
| session ID 字段 | `sessionID`（camelCase） | `session_id`（snake_case） |

stream 解析器需要同时处理两种格式。

### 不做（YAGNI）

- 不做 trait 抽象（2 个 provider 不值得，match dispatch 足够）。
- 不做配置驱动的 registry（provider 差异用 match 处理更显式）。
- 不做 codeagentcli 专属的 `--system-prompt` / `--append-system-prompt` 集成。
- 不做 codeagentcli 专属的 `--include-partial-messages` / `--include-hook-events` 集成。
- 不做 codeagentcli 的 `--session-id <uuid>` 预设会话 ID 优化。
- 不展示 `thinking` 内容（内部推理，不显示给用户）。
- 不处理 `result` 事件文本（与 `assistant` text 重复，忽略避免重复显示）。

## 2. 架构与改动范围

```
agent/
  mod.rs        ← 不变
  registry.rs   ← 改（加 codeagentcli 条目）
  detect.rs     ← 微改（测试断言泛化，检测逻辑不变）
  prompt.rs     ← 不变
  spawn.rs      ← 改（CommandConfig + provider 感知命令构建 + 查询 provider + 跳过 exe 解析）
  stream.rs     ← 改（双格式解析：opencode part.* + codeagentcli message.content[]）
app/
  agents.rs     ← 不变（已通过 REGISTRY 验证 provider，自动支持 codeagentcli）
  lifecycle.rs  ← 改（opencode_session_id → agent_session_id 重命名）
  session.rs    ← 改（函数重命名 + SQL 列名更新）
infra/
  db.rs         ← 改（加 rename_column_if_exists，执行列重命名迁移）
migrations/
  0004_rename_session_column.sql  ← 新（文档性迁移文件）
src/
  components/agents/AgentSettingsDialog.tsx  ← 改（加下拉选项）
```

## 3. 详细设计

### 3.1 Registry — 新增 codeagentcli 条目

`agent/registry.rs`：

```rust
pub const REGISTRY: &[AgentDescriptor] = &[
    AgentDescriptor { provider: "opencode", command: "opencode", display_name: "OpenCode" },
    AgentDescriptor { provider: "codeagentcli", command: "codeagentcli", display_name: "CodeAgentCLI" },
];
```

`AgentDescriptor` 结构不变。`detect.rs` 已遍历 `REGISTRY`，对每个 command 调 `which::which_global` + `--version`。测试泛化为检查所有返回的 provider 都在 REGISTRY 中。

### 3.2 Spawn — Provider 感知的命令构建

#### 3.2.1 查询 provider

`spawn_active` 的 DB 查询从 `SELECT path` 改为 `SELECT path, provider`：

```rust
let row: Option<(String, String)> =
    sqlx::query_as("SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
        .fetch_optional(pool)
        .await?;
let (path_str, provider) = row.ok_or(SpawnError::NoActiveAgent)?;
```

#### 3.2.2 CommandConfig 结构

```rust
struct CommandConfig {
    mode_args: &'static [&'static str],
    format_args: &'static [&'static str],
    session_flag: &'static str,
    needs_exe_resolution: bool,
}

fn command_config_for(provider: &str) -> CommandConfig {
    match provider {
        "opencode" => CommandConfig {
            mode_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
        "codeagentcli" => CommandConfig {
            mode_args: &["-p"],
            format_args: &["--output-format", "stream-json", "--verbose", "--skip-safe-check"],
            session_flag: "--sessions",
            needs_exe_resolution: false,
        },
        _ => {
            tracing::warn!(provider, "unknown provider, falling back to opencode config");
            CommandConfig { /* ... opencode config ... */ }
        }
    }
}
```

> **字段命名**：原设计用 `print_args`，实现中改为 `mode_args`——`print_args` 容易与输出/打印混淆，`mode_args` 更准确表达"非交互模式标志"的含义。

> **codeagentcli format_args**：`--verbose` 是 `stream-json` 在 `--print` 模式下的硬性要求（实测发现），`--skip-safe-check` 消除信任对话框的 stderr 噪音。

#### 3.2.3 命令构建

```rust
let config = command_config_for(&provider);

let exe_path = if config.needs_exe_resolution {
    resolve_native_exe(&raw_path)
} else {
    raw_path.clone()
};

let mut cmd = tokio::process::Command::new(&exe_path);
cmd.args(config.mode_args)
   .args(config.format_args)
   .arg("--dangerously-skip-permissions");

if let Some(ref id) = agent_session_id {
    cmd.arg(config.session_flag).arg(id);
}
```

#### 3.2.4 参数重命名

`spawn_active` 的参数 `opencode_session_id` → `agent_session_id`。调用链同步更新：
- `lifecycle.rs`：`oc_session_id` → `agent_session_id`，`get_opencode_session_id` → `get_agent_session_id`
- `stream.rs`：`update_opencode_session_id` → `update_agent_session_id`，`oc_session_captured` → `agent_session_captured`

### 3.3 Stream 解析 — 双格式支持

`stream.rs` 的 `parse_event` 和 `extract_session_id` 同时处理两种 NDJSON 格式。

#### 3.3.1 parse_event

原有 opencode 格式的 match arm 保留不变（`text`、`reasoning`、`tool_use`、`error`），新增 codeagentcli 格式的 arm：

- **`"assistant"`** → 调用 `parse_assistant_event()`，遍历 `message.content[]` 数组：
  - `"text"` 类型 → emit `LlmThinking`（文本输出）
  - `"thinking"` 类型 → **跳过**（内部推理，不显示给用户）
- **`"result"`** → **返回空 vec**（文本已通过 `assistant` 事件输出，`result` 会重复）
- **`"system"`** → **返回空 vec**（init 事件，无需处理）

#### 3.3.2 extract_session_id

按优先级检查三种来源：
1. `session.created` 事件的 `properties.info.id`（opencode）
2. 顶层 `sessionID` 字段（opencode 各事件）
3. 顶层 `session_id` 字段（codeagentcli 各事件）

#### 3.3.3 日志文案

所有 "opencode" 引用泛化为 "agent"。

### 3.4 DB 迁移 — 列重命名

#### 3.4.1 `rename_column_if_exists` 辅助函数

在 `infra/db.rs` 新增，镜像 `add_column_if_not_exists` 模式：检查 `pragma_table_info` 中旧列存在且新列不存在时，执行 `ALTER TABLE RENAME COLUMN`。

#### 3.4.2 迁移执行

在 `db.rs` 的 `init` 函数中：

```rust
rename_column_if_exists(&pool, "sessions", "opencode_session_id", "agent_session_id").await?;
add_column_if_not_exists(&pool, "sessions", "agent_session_id", "TEXT").await?;
add_column_if_not_exists(&pool, "sessions", "title", "TEXT").await?;
```

- 全新数据库：rename no-op（旧列不存在），add_column 创建 `agent_session_id`。
- 已有数据库（含 `opencode_session_id`）：rename 保留数据。
- 已迁移数据库（含 `agent_session_id`）：两者均 no-op。

#### 3.4.3 迁移文件

`migrations/0004_rename_session_column.sql`（文档性，实际执行在 Rust 代码中）。

### 3.5 前端 — 下拉选项

`AgentSettingsDialog.tsx` 加 `<option value="codeagentcli">codeagentcli</option>`。无其他前端改动。

## 4. 测试策略

### 4.1 单元测试

- `registry.rs`：断言 REGISTRY 包含 2 条。
- `spawn.rs`：`command_config_for` 测试——验证每个 provider 返回正确配置；验证未知 provider 回退到 opencode。
- `stream.rs`：新增 codeagentcli 格式测试——`assistant` text/thinking/multiple content items、`result` 事件跳过、`system` init 跳过、`session_id` snake_case 提取。
- `db.rs`：`rename_column_if_exists` 测试（rename + no-op）、`agent_session_id` 列断言。
- `detect.rs`：检查所有返回的 provider 都在 REGISTRY 中。

### 4.2 集成验证

- `pnpm typecheck`：前端类型检查通过。
- `cargo check --manifest-path src-tauri/Cargo.toml`：Rust 编译通过。
- `cargo test --manifest-path src-tauri/Cargo.toml`：所有测试通过（89 个）。

## 5. 涉及文件清单

| 文件 | 改动类型 |
|------|---------|
| `src-tauri/src/agent/registry.rs` | 改：加 codeagentcli 条目 |
| `src-tauri/src/agent/spawn.rs` | 改：CommandConfig + provider 查询 + 参数重命名 |
| `src-tauri/src/agent/stream.rs` | 改：双格式解析（assistant/result/system）+ session_id snake_case |
| `src-tauri/src/agent/detect.rs` | 微改：测试断言泛化 |
| `src-tauri/src/app/session.rs` | 改：函数重命名 + SQL 列名 |
| `src-tauri/src/app/lifecycle.rs` | 改：变量重命名 + 函数调用更新 |
| `src-tauri/src/infra/db.rs` | 改：加 rename_column_if_exists + 执行迁移 + 测试更新 |
| `src-tauri/migrations/0004_rename_session_column.sql` | 新：文档性迁移文件 |
| `src/components/agents/AgentSettingsDialog.tsx` | 改：加下拉选项 |

## 6. 实现历程

设计阶段假设 codeagentcli 的 `stream-json` 输出与 opencode 的 `json` 格式相同。实测发现完全不同，分三个版本迭代修复：

1. **v0.2.0** — 初始实现：registry + CommandConfig + DB 迁移。codeagentcli 启动失败（缺 `--verbose`）。
2. **v0.2.1** — 修复 CLI 参数：加 `--verbose --skip-safe-check`。codeagentcli 启动成功但 UI 无内容（格式不匹配）。
3. **v0.2.2** — 添加 codeagentcli 格式解析：`assistant`/`result`/`system` 事件 + `session_id` snake_case。UI 显示但内容重复（thinking + result）。
4. **v0.2.3** — 跳过 thinking 内容和 result 事件，消除重复显示。issue #1 关闭。
