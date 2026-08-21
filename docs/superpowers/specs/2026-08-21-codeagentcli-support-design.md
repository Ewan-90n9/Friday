# 多 Provider 支持 — codeagentcli 接入设计

- 日期：2026-08-21
- 状态：已批准（逐节通过）
- 关联 issue：#1「需要支持 codeagentcli」

## 1. 背景与目标

Friday 当前 agent 子系统硬编码绑定 opencode：registry 只有一条、spawn 硬编码 `opencode run --format json`、stream 解析器为 opencode 专属、DB 列名为 `opencode_session_id`。

issue #1 要求新增 codeagentcli 作为可选 agent 后端，用户可在设置中切换 opencode / codeagentcli。两者**同时支持**，不是替换。

### opencode vs codeagentcli CLI 差异

| 特性 | opencode | codeagentcli |
|------|----------|-------------|
| 非交互模式 | `run` 子命令 | `-p` / `--print` 标志 |
| JSON 流式输出 | `--format json` | `--output-format stream-json` |
| 跳过权限 | `--dangerously-skip-permissions` | `--dangerously-skip-permissions`（相同） |
| 会话恢复 | `--session <id>` | `--sessions <id>`（或 `-s <id>` / `--resume <id>`） |
| 版本探测 | `--version` | `--version`（也有 `-v`） |
| Windows 安装方式 | npm 包，需 shim → native exe 解析 | 不确定（跳过解析，直接用检测路径） |

### 假设与约束

- **输出格式假设相同**：codeagentcli 的 `--output-format stream-json` 假定与 opencode 的 `--format json` 输出相同的 NDJSON 事件结构（type 字段：text/reasoning/tool_use/error/step_start/step_finish/session.created）。raw NDJSON 行已在 debug 级别记录，用户测试发现格式不符时将日志贴入 issue 再做调整。
- **安装方式未知**：codeagentcli 的 Windows 安装方式不确定。设计上跳过 Windows npm shim 解析，直接使用 `which` 检测到的路径。若后续发现需要解析，再补充。
- **Prompt 传递**：两者均通过 stdin 传递 prompt（codeagentcli 的 `-p` 标注 "useful for pipes"）。不使用 codeagentcli 的 `--system-prompt` 标志，保持与 opencode 一致的 stdin 方式。

### 不做（YAGNI）

- 不做 trait 抽象（2 个 provider 不值得，match dispatch 足够）。
- 不做配置驱动的 registry（provider 差异用 match 处理更显式）。
- 不做 codeagentcli 专属的 `--system-prompt` / `--append-system-prompt` 集成。
- 不做 codeagentcli 专属的 `--include-partial-messages` / `--include-hook-events` 集成。
- 不做 codeagentcli 的 `--session-id <uuid>` 预设会话 ID 优化。

## 2. 架构与改动范围

```
agent/
  mod.rs        ← 不变
  registry.rs   ← 改（加 codeagentcli 条目）
  detect.rs     ← 微改（测试断言泛化，检测逻辑不变）
  prompt.rs     ← 不变
  spawn.rs      ← 改（provider 感知的命令构建 + 查询 provider + 跳过 exe 解析）
  stream.rs     ← 微改（日志文案 opencode → agent，解析逻辑不变）
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

`AgentDescriptor` 结构不变。`detect.rs` 的检测逻辑不需要改动——已遍历 `REGISTRY`，对每个 command 调 `which::which_global` + `--version`。但测试 `detect_returns_vec_without_panicking` 当前硬编码 `assert_eq!(agent.provider, "opencode")`，需泛化为检查所有返回的 provider 都在 REGISTRY 中。`app/agents.rs` 的 `add_agent_cmd` 通过 `REGISTRY.iter().any(|d| d.provider == provider)` 验证，codeagentcli 自动通过。

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
    /// 非交互模式标志（opencode: ["run"], codeagentcli: ["-p"]）
    print_args: &'static [&'static str],
    /// 输出格式标志（opencode: ["--format", "json"], codeagentcli: ["--output-format", "stream-json"]）
    format_args: &'static [&'static str],
    /// 会话恢复标志（opencode: "--session", codeagentcli: "--sessions"）
    session_flag: &'static str,
    /// 是否需要 Windows npm shim → native exe 解析（opencode: true, codeagentcli: false）
    needs_exe_resolution: bool,
}

fn command_config_for(provider: &str) -> CommandConfig {
    match provider {
        "opencode" => CommandConfig {
            print_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
        "codeagentcli" => CommandConfig {
            print_args: &["-p"],
            format_args: &["--output-format", "stream-json"],
            session_flag: "--sessions",
            needs_exe_resolution: false,
        },
        _ => CommandConfig {
            // 未知 provider 回退到 opencode 配置
            print_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
    }
}
```

#### 3.2.3 命令构建

```rust
let config = command_config_for(&provider);

let exe_path = if config.needs_exe_resolution {
    resolve_native_exe(&raw_path)
} else {
    raw_path.clone()
};

let mut cmd = tokio::process::Command::new(&exe_path);
cmd.args(config.print_args)
   .args(config.format_args)
   .arg("--dangerously-skip-permissions");

if let Some(ref id) = agent_session_id {
    cmd.arg(config.session_flag).arg(id);
}
```

其余部分（stdin 写入 prompt、stdout/stderr 管道、PWD 设置）不变。

#### 3.2.4 参数重命名

`spawn_active` 的参数 `opencode_session_id: Option<String>` 重命名为 `agent_session_id: Option<String>`。调用链同步更新：
- `lifecycle.rs`：`oc_session_id` → `agent_session_id`，`get_opencode_session_id` → `get_agent_session_id`
- `stream.rs`：`update_opencode_session_id` → `update_agent_session_id`，日志文案 "opencode session id" → "agent session id"

#### 3.2.5 `--sessions` 标志注意事项

codeagentcli 的 `-s/--sessions` 接受可选值（`[value]`）。某些 CLI 解析器（如 Commander.js）对可选值标志要求 `--sessions=<id>` 而非 `--sessions <id>`。如果空格分隔形式不工作，改为 `--sessions=<id>` 拼接方式。此问题在测试阶段通过日志验证后调整。

### 3.3 Stream 解析 — 共享，不变

`stream.rs` 的 `parse_event` 和 `extract_session_id` 函数不改动。NDJSON 事件格式假定两端相同。

唯一变更：日志文案中的 "opencode" 引用泛化为 "agent"：
- `"captured opencode session id"` → `"captured agent session id"`
- `"consume_stream started"` 等已通用的不变

### 3.4 DB 迁移 — 列重命名

#### 3.4.1 `rename_column_if_exists` 辅助函数

在 `infra/db.rs` 新增，镜像现有 `add_column_if_not_exists` 模式：

```rust
async fn rename_column_if_exists(
    pool: &SqlitePool,
    table: &str,
    old_name: &str,
    new_name: &str,
) -> Result<(), sqlx::Error> {
    let old_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, old_name
        ),
    )
    .fetch_one(pool)
    .await?;

    let new_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, new_name
        ),
    )
    .fetch_one(pool)
    .await?;

    if old_exists > 0 && new_exists == 0 {
        let sql = format!("ALTER TABLE {} RENAME COLUMN {} TO {}", table, old_name, new_name);
        sqlx::query(&sql).execute(pool).await?;
        tracing::info!(table, old_name, new_name, "renamed column");
    }

    Ok(())
}
```

#### 3.4.2 迁移执行

在 `db.rs` 的 `init` 函数中，现有 `add_column_if_not_exists` 调用之后添加：

```rust
rename_column_if_exists(&pool, "sessions", "opencode_session_id", "agent_session_id").await?;
```

对于全新数据库：`add_column_if_not_exists` 添加 `opencode_session_id`，然后 `rename_column_if_exists` 立即重命名为 `agent_session_id`。两步操作均幂等，无副作用。

对于已有数据库（含 `opencode_session_id`）：直接重命名，数据保留。

对于已迁移的数据库（含 `agent_session_id`）：`old_exists == 0`，跳过。

#### 3.4.3 迁移文件

新增 `migrations/0004_rename_session_column.sql`（文档性，实际执行在 Rust 代码中）：

```sql
-- Rename opencode_session_id to agent_session_id for multi-provider support.
-- Executed in db.rs::init via rename_column_if_exists (SQLite ALTER TABLE RENAME COLUMN).
-- This file documents the migration; the actual execution is in Rust for idempotency.
```

#### 3.4.4 代码引用更新

| 文件 | 旧 | 新 |
|------|----|----|
| `session.rs` | `get_opencode_session_id` | `get_agent_session_id` |
| `session.rs` | `update_opencode_session_id` | `update_agent_session_id` |
| `session.rs` | `SELECT opencode_session_id` | `SELECT agent_session_id` |
| `session.rs` | `UPDATE sessions SET opencode_session_id` | `UPDATE sessions SET agent_session_id` |
| `lifecycle.rs` | `oc_id` / `oc_session_id` | `agent_id` / `agent_session_id` |
| `stream.rs` | `update_opencode_session_id` 调用 | `update_agent_session_id` 调用 |
| `db.rs` 测试 | `opencode_session_id` 断言 | `agent_session_id` 断言 |

### 3.5 前端 — 下拉选项

`src/components/agents/AgentSettingsDialog.tsx`：

```tsx
<select value={provider} onChange={(e) => setProvider(e.target.value)} ...>
  <option value="opencode">opencode</option>
  <option value="codeagentcli">codeagentcli</option>
</select>
```

`provider` state 的默认值保持 `"opencode"`。无其他前端改动——`AgentRow`、`agentStore`、IPC 绑定已 provider 无关（`provider` 作为 string 传递）。

## 4. 测试策略

### 4.1 单元测试

- `registry.rs`：更新测试断言 REGISTRY 包含 2 条（opencode + codeagentcli）。
- `spawn.rs`：新增 `command_config_for` 测试——验证每个 provider 返回正确配置；验证未知 provider 回退到 opencode 配置。
- `db.rs`：更新 `test_db_init_adds_conversation_columns` 断言列为 `agent_session_id`（而非 `opencode_session_id`）。
- `session.rs`：函数重命名后测试同步更新。
- `detect.rs`：`detect_returns_vec_without_panicking` 测试不再硬编码 `assert_eq!(agent.provider, "opencode")`，改为检查所有返回的 provider 都在 REGISTRY 中。

### 4.2 集成验证

- `pnpm typecheck`：前端类型检查通过。
- `cargo check --manifest-path src-tauri/Cargo.toml`：Rust 编译通过。
- `cargo test --manifest-path src-tauri/Cargo.toml`：所有测试通过。

### 4.3 手动验证（用户测试）

安装 codeagentcli 后在 Friday 中：
1. 设置弹窗 → 重新检测 → 应出现 codeagentcli 条目。
2. 切换 codeagentcli 为 active agent。
3. 发送消息 → 观察是否正常流式输出。
4. 如果无输出或报错，查看 debug 日志中的 raw NDJSON 行，贴入 issue #1 做格式调整。

## 5. 涉及文件清单

| 文件 | 改动类型 |
|------|---------|
| `src-tauri/src/agent/registry.rs` | 改：加 codeagentcli 条目 |
| `src-tauri/src/agent/spawn.rs` | 改：CommandConfig + provider 查询 + 参数重命名 |
| `src-tauri/src/agent/stream.rs` | 微改：日志文案 + 函数调用重命名 |
| `src-tauri/src/agent/detect.rs` | 微改：测试断言泛化 |
| `src-tauri/src/app/session.rs` | 改：函数重命名 + SQL 列名 |
| `src-tauri/src/app/lifecycle.rs` | 改：变量重命名 + 函数调用更新 |
| `src-tauri/src/infra/db.rs` | 改：加 rename_column_if_exists + 执行迁移 + 测试更新 |
| `src-tauri/migrations/0004_rename_session_column.sql` | 新：文档性迁移文件 |
| `src/components/agents/AgentSettingsDialog.tsx` | 改：加下拉选项 |
