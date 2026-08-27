# 阶段 1：SSH 通道 + run_command 设计

- 日期：2026-08-26
- 状态：已评审（各节均已与用户逐节确认）
- 来源：[知识库与工具库伞形总纲设计](2026-08-26-knowledge-tool-umbrella-design.md) §8 阶段 1；[TODO.md](../../../TODO.md) 阶段 1
- 交付价值：Friday 第一次真正能诊断远程环境——从演示品变产品

## 1. 核心决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 环境与会话关系 | **解耦**。环境是独立一等实体，不绑定 session；`sessions.environment_id` 列保留但不读写 |
| 2 | 环境发现 | agent 通过 `list_environments` 工具自主发现；用户提到的环境由 agent 匹配；无匹配时 agent 引导用户添加 |
| 3 | SSH 认证 | 私钥优先（默认）；密码认证为备选（用户配置时二选一，运行时不自动降级） |
| 4 | Host key | 自动接受（内网工具定位），日志记录 sha256 指纹备查 |
| 5 | 执行 shell | 登录 shell（`bash -lc`），PATH 完整 |
| 6 | 超时/输出 | `timeout_secs` 默认 120s、上限 600s；stdout/stderr 各截断 64KB（保头部），完整输出落 artifacts |
| 7 | 连接生命周期 | 按 environment_id 池化（跨会话共享），空闲 10 分钟自动断开 |
| 8 | 建连失败 | 重试 2 次（1s/2s 递增），仍失败返回 `connection_error` |
| 9 | 中途断开 | 自动重连 1 次并重试当前命令；再失败 `connection_error` |
| 10 | 凭证存储 | 私钥路径/host/user 明文 SQLite；密码与私钥 passphrase 存 OS 密钥链（keyring，key=`friday/env/{env_id}/secret`） |
| 11 | run_command 风险级 | High——走现有确认拦截（120s 超时自动拒绝） |
| 12 | 确认卡片 UI | 消息流内联卡片（agent 消息的 confirm part，按请求位置渲染）；批准后由执行卡片原位接管，拒绝/超时卡片留在原位作记录（2026-08-27 修订：不再固定渲染在消息列表尾部） |
| 13 | 环境 UI | 右侧面板上下分区：上「环境」下「工具」；卡片列表 + 弹窗编辑（沿用 AgentSettingsDialog 模式） |
| 14 | K8s | 删除 `exec/k8s.rs`；K8s 场景 = SSH 到宿主机跑 kubectl（playbook 内容，阶段 2） |

## 2. 总体架构

```
用户: "10.0.1.23 环境 OOMService OOM 了"
   │
   ▼
Agent（看到用户提到的环境标识）
   │  ① list_environments()          ← 新增 ReadOnly 工具
   │ ② 匹配用户说的环境（host/name）
   │
   ├─ 匹配到 → run_command(environment: "prod-jvm-01", command: "jstat ...")
   │              │
   │              ▼
   │        MCP Server 拦截（High 风险）→ 前端确认卡片 → 批准
   │              │
   │              ▼
   │        ExecPool.get(environment_id) ──有连接──► 复用
   │              │无连接
   │              ▼
   │        russh 建连（按环境认证配置：私钥或密码）→ 缓存
   │              │
   │              ▼
   │        登录 shell 执行 → stdout/stderr/exit_code 返回
   │
   └─ 没匹配 → agent 让用户去右侧「环境」面板添加
```

## 3. 数据模型

```sql
-- environments 表（现有基础上扩展）
ALTER TABLE environments ADD COLUMN auth_type TEXT NOT NULL DEFAULT 'private_key';
-- 'private_key' | 'password'
ALTER TABLE environments ADD COLUMN private_key_path TEXT;     -- 引用 ~/.ssh/ 下的路径，不复制
-- 密钥链引用键不单独存列：由 env_id 推导（friday/env/{env_id}/secret）
```

- `host`、`port`、`user`、`name`、`transport_type` 保留（transport_type 恒为 `'ssh'`）
- `k8s_namespace` / `k8s_pod` 列废弃：表结构不动（迁移兼容），代码不再读写
- `sessions.environment_id`：列保留（兼容），阶段 1 不读写——环境选择完全由 agent 工具调用驱动
- 迁移方式：沿用 `infra/db.rs` 的 `add_column_if_not_exists` 模式

## 4. SSH Transport（exec/ssh.rs 重写）

### 4.1 结构

```rust
pub struct SshTransport {
    host: String,
    port: u16,
    user: String,
    auth: SshAuth,                                    // PrivateKey{path} | Password
    conn: tokio::sync::Mutex<Option<ConnState>>,      // interior mutability（trait 是 &self）
}
```

### 4.2 认证

- `auth_type = private_key`：russh-keys 加载私钥；私钥带 passphrase 时从 keyring 取 `friday/env/{env_id}/secret`
- 密钥文件不存在/坏格式 → 认证失败并明确报错（含路径），**不回退密码**（配置错误应显式暴露）
- `auth_type = password`：密码从 keyring 取，直接密码认证
- "私钥优先"是**用户配置层面**的默认（添加环境时默认私钥方式），不是运行时自动降级

### 4.3 执行与重连

- 每条命令 `channel.request_exec()`，包装为 `bash -lc '<command>'`（登录 shell，PATH 完整，jstat/jcmd 直接可用）
- 建连失败：重试 2 次（间隔 1s / 2s 递增），仍失败返回 `connection_error`
- 中途断开：自动重连 1 次（重新走认证），重连后重试当前命令一次；再失败返回 `connection_error`
- 命令超时由工具层 `tokio::time::timeout` 包裹（见 §5），transport 层不感知

### 4.4 Host key 与 trait

- `check_server_key` 恒接受；`info!` 记录指纹（sha256）备审计
- `ExecChannel` trait：`run()` 签名不变；新增 `is_alive(&self) -> bool` 供连接池巡检
- trait 保持 `&self`——连接句柄放 `Mutex<Option<ConnState>>` 内部

### 4.5 日志（遵从 logging-standard.md）

- 连接建立/断开/重试：`info!`/`warn!`，含 env_id、host、指纹、耗时
- 命令执行：`info!`，含 command + exit_code + elapsed
- 错误路径一律 `tracing::error!`/`warn!`

## 5. 工具层

### 5.1 run_command（tools/builtin/ 新文件）

```jsonc
// input_schema（session_id 由 MCP Server 自动注入）
{
  "environment": "string  — 目标环境名称（list_environments 返回的 name）",
  "command":     "string  — 要执行的 shell 命令",
  "timeout_secs": "number? — 默认 120，上限 600"
}
// risk_level: High, needs_channel: true
```

执行流程：

1. 解析 `environment` 参数 → 按 name 查 environments 表
2. `exec_pool.get_or_create(environment_id)` 拿连接
3. `tokio::time::timeout(timeout_secs, channel.run(command))` 包裹执行
4. 输出处理：stdout/stderr 各截断 64KB（保头部，标注 `[truncated, full output: <artifacts path>]`）；完整输出写 `artifacts/tool-outputs/{session_id}/{tool_call_id}.log`
5. 返回结构化 JSON：`{ stdout, stderr, exit_code, elapsed_ms, truncated: bool }`
6. 超时：杀掉远端进程（channel 关闭即终止该 session 的执行），返回 `timeout_error`（不重试）

错误语义（agent 可读）：

- 环境名不存在 → "环境不存在，先用 list_environments 查看可用环境，或让用户在右侧环境面板添加"
- 连接失败 → `connection_error`，含 host 与原因

### 5.2 list_environments（ReadOnly，免确认）

```jsonc
{ "environments": [ { "name", "host", "port", "user", "auth_type" } ] }
```

工具描述引导 agent：诊断远程环境前先调用本工具匹配用户提到的环境；无匹配时请用户提供环境信息并引导其在右侧「环境」面板添加。

### 5.3 MCP Server 改动（mcp/server.rs）

- `needs_channel` 工具的 channel 获取逻辑改为：从 args 解析 `environment` 参数（run_command 专有字段，不进 ExecChannel trait）→ `exec_pool.get_or_create(environment_id)`
- session_id 仍用于 tool_calls 落库与会话关联

### 5.4 连接池（exec/pool.rs 重写）

- `HashMap<session_id, Channel>` → `HashMap<environment_id, PooledConnection>`
- `PooledConnection { channel: Arc<dyn ExecChannel>, last_used: Instant }`
- 后台巡检任务每分钟清理空闲 >10min 的连接（`is_alive` 辅助判断）
- `get_or_create`：缓存命中即复用（刷新 last_used）；未命中建连入池

### 5.5 prompt 引导（agent/prompt.rs）

工具使用 section 补充：

- 远程命令一律走 run_command 并指定 environment
- 优先结构化工具，run_command 是兜底
- 环境没匹配到就引导用户添加，不要瞎猜 host

## 6. Environment CRUD + 凭证模块

### 6.1 后端命令（app/environments.rs，模式照抄 app/agents.rs）

```rust
list_environments_cmd() -> Vec<EnvironmentRow>     // name/host/port/user/auth_type/private_key_path
add_environment_cmd(name, host, port, user, auth_type, private_key_path?, password?)
update_environment_cmd(env_id, ...)                // 密码/私钥变更时同步更新 keychain
delete_environment_cmd(env_id)                     // 断开池中连接 + 删 keychain 条目 + 删行
test_connection_cmd(env_id) -> { ok, latency_ms, error? }   // 手动验证（建连跑 echo ok）
```

所有 command 加 `#[instrument]`（logging-standard 强制）。

### 6.2 凭证模块（app/credentials.rs，替换 todo!() 存根）

- keyring 实现 `store_secret(env_id, value)` / `load_secret(env_id)`，key 格式 `friday/env/{env_id}/secret`
- Windows 走 Credential Manager；失败错误上抛给 UI 提示

## 7. 前端

1. **右侧面板改造**（ToolsPanel → 上下分区）：上「环境」（环境卡片：name、host·user、认证类型徽标；+ 新增按钮；卡片 hover 显示编辑/删除），下「工具」保持现状
2. **EnvironmentDialog（弹窗编辑）**：表单 = 名称/host/port（默认 22）/user/认证方式（私钥 | 密码二选一）；私钥模式下 private_key_path 输入（placeholder `~/.ssh/id_ed25519`，默认探测 home 下常见密钥名）；密码模式下密码输入框（写 keychain，不落库）；编辑与新增共用弹窗；「测试连接」按钮调 test_connection_cmd 内联显示结果
3. **确认卡片（ConfirmCard 组件）**：
   - sessionStore 新增 `confirm_required` 事件分支（当前实现缺失此分支）+ pending confirmations 状态
   - 卡片内联渲染在消息流尾部：命令、风险徽标、120s 倒计时、批准/拒绝按钮 → 调 `confirmTool(confirmId, approved)`
   - 解决后卡片转终态（已批准/已拒绝/超时），保留在消息历史中作审计记录
4. **IPC 绑定**（lib/ipc.ts）：新增 §6.1 四个命令的绑定

## 8. 错误处理汇总

| 场景 | 行为 |
|---|---|
| 建连失败（认证错/网络不可达） | 重试 2 次（1s/2s 递增），仍失败返回 `connection_error`（含 host、原因）；确认卡片流程中表现为工具结果错误，agent 转告用户 |
| 中途断开 | 自动重连 1 次并重试当前命令；再失败 `connection_error` |
| 命令超时 | 不重试，杀远端进程，返回 `timeout_error` |
| 环境名不存在 | 明确错误信息引导 agent 调 list_environments 或让用户添加 |
| keyring 不可用 | add/update 环境时即报错（不让用户存一半）；连接时密码取不到 → `connection_error` |
| 私钥文件缺失/坏格式 | 建连即失败，错误信息含路径 |

## 9. 测试

沿用现有 in-module `#[cfg(test)]` + tempdir + 内存 DB 惯例：

- `exec/ssh.rs`：认证选择逻辑、命令包装（`bash -lc`）、重试策略单元测试（russh 真实连接由 test_connection_cmd 手测）
- `exec/pool.rs`：按 environment_id 缓存命中/未命中、空闲超时清理（注入假时钟或短超时）、MockChannel 复用现有
- `tools/builtin/run_command.rs`：参数校验（缺 environment/command、timeout 缺省 120 / 上限 600 钳制）、输出截断 64KB、超时路径、环境不存在错误信息
- `app/environments.rs`：CRUD 往返、密码不落库（DB 无明文）、删除级联（连接断开 + keychain 清理 mock）
- 前端无测试基建（无 vitest），不新增
- 手工验收：真机连一台 Linux 跳板机跑通"用户提问 → agent list_environments → run_command → 确认卡片 → 结果返回"全链路

## 10. 验收标准

1. 配置一个真实环境后，agent 能自主发现并对其执行命令，全程有确认拦截
2. 会话与环境无耦合——换会话不换环境、多会话可共用同一环境连接
3. 空闲 10 分钟连接自动断开，无泄漏
4. `exec/k8s.rs` 及相关引用全部移除，`cargo check`/`cargo test` 无死代码警告
5. `pnpm typecheck` + `cargo check` + `cargo test` 全绿

## 11. 对现有系统的影响

| 模块 | 变更 |
|---|---|
| `exec/ssh.rs` | russh 真实现（重写） |
| `exec/k8s.rs` | 删除；`exec/mod.rs` mod 声明、pool k8s 分支同步移除 |
| `exec/pool.rs` | session 维度 → environment 维度池化 + 空闲清理 |
| `exec/channel.rs` | trait 新增 `is_alive` |
| `tools/builtin/` | 新增 run_command、list_environments |
| `mcp/server.rs` | needs_channel 工具的 channel 获取改为按 environment 参数 |
| `app/credentials.rs` | keyring 实现（替换 todo!()） |
| `app/environments.rs` | 新文件：CRUD + test_connection 命令 |
| `agent/prompt.rs` | 工具使用 section 补环境引导 |
| `infra/db.rs` | environments 表 auth 列迁移 |
| 前端 | 右侧面板分区、EnvironmentDialog、ConfirmCard、ipc.ts 绑定、sessionStore confirm_required 分支 |
| `docs/architecture/*` | overview/runtime/error-handling 对齐环境解耦模型（实现完成后顺手更新） |
