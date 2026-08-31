# Arthas MCP Server 对接设计

- 日期：2026-08-31
- 状态：已评审（设计三节均经用户确认）
- 范围批次：诊断工具层 arthas 批次（overview.md 决策表 #5 工具层演进；error-handling.md arthas 风险分级预案的落地）

## 背景与目标

Arthas 4.3.x 官方内置实验性 MCP Server（`arthas.mcpEndpoint=/mcp`）：arthas attach 到目标 JVM 后，随 arthas HTTP 服务在目标机 `127.0.0.1:8563` 暴露 Streamable HTTP 端点，提供 29 个结构化诊断工具（dashboard / thread / watch / trace / sc / jad / ognl / redefine 等），支持 Bearer 认证。

Friday 是远程环境故障诊断 Agent：诊断目标是 SSH 直连远程主机上的 JVM，agent CLI（opencode / codeagentcli）在本地运行。本设计让 Friday 对接官方 arthas 4.x 内置 MCP Server，使 agent 能以结构化工具调用方式使用 arthas 诊断能力，并完整复用 Friday 现有的风险拦截、审计与前端可视化机制。

### 目标

1. agent 通过 Friday Tool Registry 中的 `arthas_*` 系列工具诊断远程 JVM（工具调用经 SSH 隧道代理到目标机上的 arthas MCP Server）
2. arthas 由 Friday 经 artifactory 统一下发到目标机（`ensure_tool` 机制），无需目标机外网
3. attach 生命周期自动管理：按需 attach、空闲回收、显式关闭
4. v1 范围：仅 SSH 直连主机场景；K8s pod 场景列 roadmap

### 非目标

- 不支持热更新字节码类工具（mc / redefine / retransform 剔除）——Friday 定位是诊断而非热修复
- 不做 agent 直连 arthas（绕过风险拦截与审计，与架构决策 9 冲突）
- 不做 sudo / su 提权（用户决策：失败时提示补录环境用户凭证）
- 不做 K8s pod 内 JVM attach（roadmap）
- jvm_* 系列工具接入用户对齐/多凭证能力（roadmap，本批次仅 arthas 使用）

## 总体架构

```
agent CLI (opencode/codeagentcli)
   │ MCP (Friday 自有 server, 127.0.0.1:port/mcp)
   ▼
Friday MCP Server / Tool Registry
   ├─ arthas_open / arthas_close                ← 生命周期工具
   ├─ arthas_dashboard / arthas_thread / arthas_watch / ... (25 个代理工具)
   │    │
   │    ▼
   │  ArthasManager (移植 HeapAnalyzerManager 模式, src-tauri/src/analyzer/)
   │    ├─ ArthasSession { (env_id, pid) → phase: Attaching|Ready|Failed }
   │    ├─ ClientFactory seam（测试注入 mock）
   │    ├─ 空闲回收 15min / LRU 上限 3 / 传输错误 invalidate
   │    │
   │    ├─ McpArthasClient（rmcp streamable-http-client + Bearer token）
   │    │      │ http://127.0.0.1:<本地临时端口>/mcp
   │    │      ▼
   │    ├─ TunnelManager（exec 层新能力：russh direct-tcpip 本地端口转发）
   │    │      │ 127.0.0.1:<local> ⇄ SSH ⇄ 目标机 127.0.0.1:<http_port>
   │    │      ▼
   │    └─ attach 编排：
   │         ensure_tool("arthas") → ArthasPackage（artifactory 下载 arthas-bin.zip
   │         → SFTP 上传 → 解压 /tmp/friday-tools/arthas-<ver>/）
   │         → 预写 arthas.properties（mcpEndpoint=/mcp、telnetPort=-1、
   │            httpPort=分配端口、password=随机生成）
   │         → 用户对齐检查（见下节）
   │         → SSH 后台执行 <jdk>/bin/java -jar arthas-boot.jar --pid <pid>
   │         → HTTP 探活 → 建隧道 → MCP 握手
   ▼
SSH ExecChannel（现有执行层，连接池不变）
```

### 关键决策

| # | 决策 | 理由 |
|---|------|------|
| A1 | Friday 代理模式：Friday 作为 MCP client 经 SSH 隧道连远端 arthas，在 Tool Registry 注册 `arthas_*` 代理工具 | 风险拦截 / 确认弹窗 / tool_calls 审计 / 前端工具卡片全部复用现有机制；与 heap analyzer 托管模式（Friday 作为 MCP client）同构 |
| A2 | 工具调用走 arthas MCP 结构化工具（参数 schema 校验、结构化返回）；生命周期（stop / 探活）走 arthas HTTP API（`POST :port/api`） | 29 个 MCP 工具不含 stop；探活用轻量 HTTP 请求避免 MCP 会话状态干扰 |
| A3 | 隧道（TunnelManager）作为 exec 层通用基础设施，与 arthas 解耦 | 后续 JMX、日志 tail 等可复用；russh 0.45 支持 `channel_open_direct_tcpip` |
| A4 | arthas 配置统一走 Friday 预写的 arthas.properties，不依赖 CLI 参数 | arthas 配置优先级为命令行 > properties；统一走 properties 消除旗标映射的不确定性 |
| A5 | 端口由 Friday 在目标机探测分配（18563 起顺序向上），结果缓存复用 | 避免固定 8563 端口冲突（同机多 arthas 实例） |
| A6 | Bearer token（`arthas.password`）随 properties 下发随机值，存 OS keychain | 共享主机上防同机其他用户直连冒用；零成本加固 |
| A7 | 依赖变化：Cargo.toml rmcp 增加 `transport-streamable-http-client` feature | rmcp 3.1.4 已支持，仅 feature 开关 |

## attach 用户对齐与环境多用户凭证

JVM attach 要求发起进程与目标 JVM 同 UID（或 root）。生产环境 SSH 登录用户与 JVM 进程属主（服务账号）常不一致。方案：环境多用户凭证 + 失败时引导补录，**不做提权**。

### attach 流程

```
arthas_open(environment, pid)
  ├─ ① pre-flight：ps -o user= -p <pid> → jvm_user；当前连接 id -un → ssh_user
  ├─ ② ssh_user == jvm_user（或 ssh_user 为 root）→ 直接 attach（走默认连接）
  └─ ③ 不一致 → 查该环境的用户凭证表，按 jvm_user 匹配
        ├─ 命中 → 用 jvm_user 凭证建临时 SSH 连接执行 attach
        │         （attach 命令一次性执行完即退出，临时连接用后即弃；
        │           隧道 / MCP 调用 / stop 走默认连接，无用户限制）
        └─ 未命中 → 结构化错误返回 agent：
           「目标 JVM 运行用户为 X，当前 SSH 用户为 Y 且未录入 X 的凭证，
             请在环境管理中为该环境添加用户 X 的凭证后重试」
```

### 环境多用户管理（新功能）

- 数据模型：新表 `env_credentials`（id、environment_id、username、auth_type、is_default）；密钥仍入 OS keychain，路径 `friday/env/{env_id}/cred/{cred_uuid}`；环境删除时联动清除
- 一个环境可录多个用户凭证（密码或私钥），其中一个标记默认；默认用户 = Friday 日常连接（连接池、run_command、jvm_* 工具）使用的 SSH 用户
- 迁移：现有环境单 username + secret 自动转为一条默认凭证，对外行为不变
- UI：环境编辑弹窗内凭证列表（增 / 删 / 改、设默认）；多用户纯可选，单用户场景无感
- 前端 `src/lib/ipc.ts` 同步新增 command 绑定（AGENTS.md 两端同步约定）
- 用户对齐检查 + 按 jvm_user 查凭证做成通用工具函数（jvm_* 工具接入列 roadmap）

### attach 时的 arthas home 传递

sudo / 用户切换场景不存在，但 jvm_user 临时连接的 HOME 与默认用户不同：attach 命令显式指定 arthas home 指向 Friday 下发目录（`/tmp/friday-tools/arthas-<ver>/`，权限 755 保证各用户可读），避免 arthas-boot 自行去 `~/.arthas` 下载。确切旗标（`--arthas-home` 或等价系统属性）为实现期验证点。

## ArthasManager 生命周期

移植 HeapAnalyzerManager 模式（`src-tauri/src/analyzer/manager.rs`）：

```
ArthasManager
├─ sessions: HashMap<(env_id, pid), ArthasEntry>
│    ArthasEntry { phase: Attaching | Ready | Failed,
│                  last_active, inflight }   // watch channel 通知等待者
├─ spawn_lock 双检锁：同一 (env,pid) 并发 open 去重为一次 attach
├─ ClientFactory seam（注入 mock 测试，同 heap analyzer）
└─ 空闲回收任务：30s tick，无引用且无 inflight 超过 15min
     → HTTP API stop arthas + 拆隧道 + 移除会话
```

| 事件 | 行为 |
|---|---|
| `arthas_open` | 编排：ensure_tool("arthas") → 用户对齐 → 后台 attach → 探活 → 建隧道 → MCP 握手；全程 Attaching 状态，总超时 120s |
| 工具调用 | Requires Ready；`guarded_call`（inflight 计数 + per-tool 超时）；按 (env, pid) 路由到对应会话 |
| 传输错误 | invalidate 该会话：尽力 HTTP stop → 拆隧道 → 移除；下次 open 重新 attach |
| 空闲 15min | 自动 stop + 拆隧道（跨 Friday 会话复用，环境级资源，同 SSH 连接池决策 17b） |
| Friday 会话关闭 | 不联动 arthas |
| 环境删除 | stop 该环境全部 arthas 会话 + 拆隧道 + 清凭证 keychain |
| LRU 上限 | 3 个 Ready 会话，超出逐出最久未用（同 heap dump 上限） |
| 卸载式 stop 后再次使用 | arthas 4.x stop 卸载 agent；重新 open = 完整 attach 流程（含用户凭证检查） |

## 工具面：27 个（25 代理 + 2 生命周期）

### ReadOnly（自动执行，16 个）

`arthas_close`、`arthas_dashboard`、`arthas_jvm`、`arthas_memory`、`arthas_sysenv`、`arthas_perfcounter`、`arthas_sc`、`arthas_sm`、`arthas_jad`、`arthas_classloader`、`arthas_getstatic`、`arthas_mbean`、`arthas_dump`、`arthas_thread`*、`arthas_viewfile`、`arthas_options`

### Low（确认后执行，11 个）

`arthas_open`（加载 agent 侵入 JVM）、`arthas_watch`、`arthas_trace`、`arthas_stack`、`arthas_monitor`、`arthas_tt`（字节码增强）、`arthas_ognl`（可调用方法）、`arthas_vmtool`*、`arthas_sysprop`、`arthas_vmoption`（可修改运行时状态）、`arthas_profiler`（采样开销）

### 子操作过滤

带 \* 工具在 proxy mapping 层拒绝危险子操作：`thread` 拒绝 `--interrupt`；`vmtool` 拒绝 `interrupt`。返回结构化错误说明允许的操作。mapping 纯函数校验，可单测。

### 剔除（4 个）

- `mc` / `redefine` / `retransform`：热更新字节码，超出诊断定位
- `heapdump`：与 `jvm_heap_dump`（High + 自动拉回 + MAT 预热）重复；agent guidance 指引用 `jvm_heap_dump`

### 输出处理与超时

- 复用 heap 工具模式：64KB 截断返回 agent，全文落 `artifacts/<session>/arthas-<uuid>.md`
- per-tool `(default_secs, max_secs)` 元组：dashboard 类 30/60；watch / trace / monitor 流式类 120/600；profiler 更长（实现期定标）

### TOOL_GUIDANCE 更新（agent/prompt.rs）

新增 arthas 工作流：`list_processes` 拿 pid → `arthas_open` → 诊断工具 → 堆快照走 `jvm_heap_dump` → 完成后 `arthas_close` 或留给空闲回收。

## SSH 隧道（exec 层新基础设施）

- `TunnelManager`：`open_tunnel(env_id, remote_host, remote_port) -> LocalTunnel { local_port }`；russh `channel_open_direct_tcpip` 实现，双向 copy 任务；按 `(env_id, remote_host, remote_port)` 复用，引用计数，空闲释放
- 连接池协同：隧道独享一条从池里借出的 SSH 连接（不与 exec 混用 channel），避免 russh 多路复用下 exec 大输出阻塞隧道数据通道；该连接标记 tunnel 专用
- 生命周期：隧道随 arthas 会话开 / 关；ArthasManager 是首个消费者，TunnelManager 与 arthas 解耦（后续 JMX 等复用）

## 错误处理

对齐 error-handling.md 分级：

| 场景 | 策略 |
|---|---|
| SSH 连接失败 / 中断 | 复用基础设施重试（连 2 次 / 重连 1 次）；隧道数据通道断开 → invalidate 该 arthas 会话，下次调用返回结构化错误引导重新 open |
| MCP 传输错误 | invalidate 会话（尽力 stop → 拆隧道），下次 open 重 attach |
| attach 失败（探活超时 / 用户不对齐 / 无凭证） | 不重试（error-handling.md「attach 失败不重试」）；结构化错误返回 agent，含 pre-flight 明确原因（用户不匹配 + 缺哪个凭证）+ 建议动作 |
| arthas MCP 工具业务错误 | 原样透传（is_error + 文本），agent 自行决策 |
| arthas stop 失败 | 仅 warn 日志；会话照常移除、隧道照拆；残留 agent 由用户 `arthas_close` 重试或目标机重启解决 |
| 隧道本地端口耗尽 | 顺序分配失败 → 结构化错误，提示减少并发 attach 的 JVM 数 |

## 前端改动（最小面）

- 凭证管理 UI：环境编辑弹窗内多凭证列表（增删改、设默认），`ipc.ts` 新增 command 绑定
- 确认弹窗：现有 ConfirmRequired 机制零改动，Low / High 工具自动走弹窗（文案带 arthas 工具名和目标 env / pid）
- 工具卡片：现有 ToolExecuting / ToolResult 事件照常渲染 arthas_* 调用
- attach 过程可视化：复用 ProvisionProgress 事件显示「attach 中…建隧道…握手」（同 heap analyzer 预热进度条）
- 无新页面、无新布局

## 日志与文件管理约定

- 遵循 logging-standard.md：ArthasManager / TunnelManager / attach 编排入口 `#[instrument]`；错误路径 `error!` / `warn!`；attach 子进程 stderr 必须读取记录；日志不截断、不脱敏
- 运行时文件路径经 `infra/paths.rs` 的 `Paths` 统一解析，不内联 `.join()`；artifacts 落 `artifacts/<session>/`（现有约定）

## 测试策略

- 纯函数单测：proxy mapping（参数翻译 + 子操作过滤）、arthas.properties 生成、端口分配
- ArthasManager 单测：MockArthasClient（ClientFactory seam）覆盖——open 去重、phase 状态机、传输错误 invalidate、空闲回收、LRU 逐出、stop 失败容错
- attach 编排单测：mock ExecChannel 验证命令拼装、用户对齐分支、凭证缺失错误文案
- TunnelManager：本地 sshd / russh mock 较重，v1 以集成冒烟为主（真连测试机验证 direct-tcpip + 端口转发）
- 验证命令：`cargo test --manifest-path src-tauri/Cargo.toml`、`cargo check --manifest-path src-tauri/Cargo.toml`、`pnpm typecheck`

## 实现期验证点（不确定项，实现时先验证）

1. 批处理模式 attach 后进程是否驻留（决定 attach 命令形态：`--batch` 一条命令 vs nohup 驻留 + 记录远端进程号）
2. properties 优先级是否覆盖 CLI 默认（文档称命令行 > properties，故统一走 properties、不传端口 CLI 参数）
3. arthas home 显式指定的确切旗标（`--arthas-home` 或等价系统属性）
4. arthas MCP STREAMABLE 模式与 rmcp streamable-http-client 的会话兼容性（SSE keep-alive、会话头）
5. arthas-boot.jar 对 JDK 版本要求（复用已下发的 JDK，版本下限实现期确认）

## Roadmap（本批次外）

- K8s pod 场景（kubectl exec attach + port-forward 双跳链路）
- jvm_* 系列工具接入用户对齐 / 多凭证能力
- TunnelManager 第二消费者（JMX、日志 tail 等）
- 社区第三方 arthas MCP server 适配（如需要）
