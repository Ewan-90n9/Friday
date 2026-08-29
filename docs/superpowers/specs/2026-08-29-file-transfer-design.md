# Friday 独立文件上传下载工具设计

- 日期：2026-08-29
- 状态：已评审（各节均与用户逐节确认）
- 上游：[JDK 原生命令结构化工具设计](2026-08-28-jdk-native-tools-design.md) §5 heap_dump 三阶段流程的演进
- 前置依赖：[工具系统框架](2026-08-23-tool-system-design.md)、[SSH 执行通道](2026-08-26-phase1-ssh-run-command-design.md)、ExecChannel SFTP upload/download（已实现）

## 1. 背景与根因

用户场景：堆快照在远端生成成功，但拉不回本地。错误为 `hyper::Error code 10054 远程主机强迫关闭了一个现有的连接 addr 127.0.0.1:10184`。

**根因不在 SSH 链路，而在 MCP 同步调用模型**：`127.0.0.1:10184` 是 Agent CLI ↔ Friday 本地 MCP HTTP 服务的回环连接。`call_tool` 同步 await 工具 handler 完成才返回 HTTP 响应，GB 级传输挂几十分钟，Agent CLI 的 HTTP 客户端先超时掐断连接，工具结果丢失。即使换更好的网络，同步模型也撑不住大文件传输。

目标：把文件上传/下载做成**独立的 Agent 工具**，并改造 heap_dump 复用同一传输引擎：

- 每个 MCP 调用秒回，长传输在后台 task 进行，Agent 轮询获取结果；
- 断链自动重试 + 下载断点续传（SSH 层在 VPN/防火墙环境下也可能中途断，是 MCP 层之后的下一个故障点）；
- UI 展示传输进度条。

## 2. 决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 方案选型 | 进程内 TransferManager + 专用连接 + 轮询（方案 A）；SQLite 持久化任务表（方案 B）留作后续演进，接口不变可叠加；MCP progress notification 流式上报（方案 C）不解决客户端超时根因，否决 |
| 2 | 传输用连接 | **专用 SshTransport，不走 ExecChannelPool**：池连接的 conn 锁被长传输持有会阻塞同环境所有工具调用；池的 600s 空闲回收会掐断传输中的连接 |
| 3 | 传输状态 | 内存注册表（`Arc<Mutex<HashMap>>`），不落库；应用重启丢进行中任务，但远端文件与本地 `.part` 半成品保留，重调即断点续传 |
| 4 | 断链恢复 | 下载支持断点续传（SFTP offset seek + 本地 `.part` 追加）；上传整体重传（远端 create truncate 覆盖） |
| 5 | 下载落盘 | 固定会话 artifacts 目录（`artifact_dir_for`），文件名取远端 basename，防穿越校验 |
| 6 | 上传源路径 | Agent 可指定任意本地绝对路径；RiskLevel::High，每次调用走用户确认弹窗（确认卡片展示 local_path + remote_path） |
| 7 | heap_dump 改造 | 生成 + stat 校验两阶段保持同步；第三阶段改为启动 TransferManager 后台下载，返回 transfer_id 让 Agent 轮询 |
| 8 | 远端清理 | 独立 file_download 成功后**不删**远端文件；heap_dump 场景带 `cleanup_remote_on_success` 标志，rename 成功后才 `rm` |
| 9 | 进度展示 | 后端 TransferProgress/TransferFinished 事件 + 前端新 ChatPart 类型渲染进度条 |
| 10 | 防重复传输 | 同一 `session_id + direction + remote_path` 已有 active 传输时拒绝新请求，返回已有 transfer_id |

## 3. 总体架构

新增 `src-tauri/src/transfer/` 模块（基础设施层，同 `exec/` 定位）：

```
Agent (MCP tools/call)                     后台 tokio task（每次传输一个）
┌──────────────────┐   启动传输，秒回      ┌─────────────────────────┐
│ file_download    │ ───────────────────▶ │ TransferManager         │
│ file_upload      │ ◀─────────────────── │  · 状态注册表(内存)       │
│ transfer_status  │   transfer_id        │  · spawn 专用 SshTransport│
│ transfer_cancel  │                      │  · 循环: 连接→传→断线重试 │
└──────────────────┘                      │  · 断点续传(SFTP offset)  │
        ▲                                 └──────────┬──────────────┘
        │ transfer_status 轮询                        │ EventBus
        ▼                                            ▼
┌──────────────────┐                      ┌─────────────────────────┐
│ heap_dump        │ 生成+校验后也走       │ TransferProgress 事件     │
│ (改造)           │ TransferManager      │ → 前端进度条（新 part 类型）│
└──────────────────┘                      └─────────────────────────┘
```

模块定位：`transfer/` 不碰工具协议（参数校验、ToolOutput 组装在 `tools/builtin/file_transfer.rs` 工具层），工具层和 heap_dump 通过同一个 TransferManager 入口使用。

## 4. TransferManager 内部设计

### 4.1 状态机

```
Pending → Connecting → Transferring ⇄ Retrying → Completed | Failed | Cancelled
```

```rust
pub struct TransferState {
    pub id: String,              // UUID
    pub direction: Direction,    // Download | Upload
    pub session_id: String,
    pub env_id: String,
    pub remote_path: String,
    pub local_path: PathBuf,     // 下载: session artifacts 目录内; 上传: 任意本地路径
    pub status: Status,          // 上述状态机
    pub total_bytes: u64,        // 传输前 stat 获得
    pub transferred_bytes: u64,  // 断点续传时为已传字节数
    pub attempt: u32,            // 当前第几次尝试(1 起)
    pub error: Option<String>,   // Failed 时的终态错误
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

### 4.2 执行循环（后台 task 伪码）

```
loop (最多 MAX_ATTEMPTS=5 次):
  connect 专用 SshTransport          // 失败 → backoff 后重试
  stat 远端文件 → total_bytes         // 不存在 → Failed(终态)
  下载: 本地半成品文件大小 N → SFTP seek 到 N 续传
        (上传: 无续传，每次从头传；成功后远端校验大小)
  while 读写: transferred_bytes 更新 → 每秒节流发 TransferProgress 事件
  断线/出错 → attempt+1, backoff(5s,15s,45s,2m,6m) → 重连续传
  成功 → Completed, disconnect 专用连接
```

要点：

- **断点续传只做下载**：SFTP 读支持 offset seek，本地 `.part` 临时文件追加。上传断点续传需远端配合截断/校验，复杂度高收益低。
- **下载写 `.part` 临时文件**（`xxx.hprof.part`），完成后 rename 成正式名：本地半成品可续传 + UI 看到的正式文件永远完整。上传失败重传用 `create`（truncate）覆盖。
- **大小校验**：下载完成后本地大小 == stat 的 total_bytes 才算 Completed；不一致视作传输损坏，删 `.part` 从头重试。
- **重试预算**：attempt 用尽（5 次）或累计耗时超 2 小时 → Failed。error 说明"远端文件保留，可重新 file_download 断点续传"。
- **Cancellation**：每任务一个 `CancellationToken`；Cancelled 为终态，`.part` 保留，专用连接断开。
- **终态记录保留**：内存注册表不删终态记录（Agent 可查历史），LRU 上限 100 条防泄漏。

### 4.3 SshTransport 复用与扩展

TransferManager 直接复用 `exec/pool.rs` 的 `build_transport` + `fetch_environment`（提取为可共享）建立专用连接。现有 `ExecChannel::download/upload` 的锁结构（全程持 conn 锁）对专用连接无碍，但需要两点扩展：

1. **进度回调**：`download`/`upload` 增加可选进度上报（回调或 channel），每 N 字节/每秒更新 transferred_bytes；
2. **下载续传入口**：新增 `download_resume(remote_path, local, offset)` 或在 download 加 offset 参数（russh-sftp file 对象支持 seek）。

stat 远端大小走**专用连接自己的 exec 通道**（每次重连后先 `stat -c %s`，路径用 `shell_quote_single` 包裹），不借道 ExecChannelPool。

## 5. Agent 工具接口

四个新工具注册进 ToolRegistry。MCP 层自动加 `friday_` 前缀。参数统一含 `environment`（环境名）。四个工具 `needs_channel: false`——自建专用连接，不占池。

### 5.1 file_download

```jsonc
// 入参
{
  "environment": "prod",            // 必填，环境名
  "remote_path": "/tmp/friday-tools/heapdump-1234-xxx.hprof"  // 必填，绝对路径
  // session_id 由 MCP 层自动注入（现有机制）
}
// 返回（立即）
{ "transfer_id": "uuid", "status": "pending",
  "total_bytes": 123456789,         // 启动前 stat 已知则带
  "local_path": "<artifacts>/<sid>/heapdump-1234-xxx.hprof",
  "note": "传输已在后台启动，请轮询 transfer_status(transfer_id) 获取进度/结果" }
```

校验：

- `remote_path` 必须以 `/` 开头（拒绝相对路径）；拼 stat 命令时 `shell_quote_single` 包裹；
- 落盘文件名取 basename，拒绝空/`.`/`..`（防穿越）；
- 同 `session_id + direction + remote_path` 已有 active 传输 → 拒绝并返回已有 transfer_id。

### 5.2 file_upload

```jsonc
{
  "environment": "prod",
  "local_path": "D:\\dumps\\tool.jar",       // 必填，本地绝对路径
  "remote_path": "/tmp/friday-tools/tool.jar" // 必填，远端绝对路径
}
// 返回同上：transfer_id + 后台已启动
```

- **RiskLevel::High**：走确认弹窗，确认卡片展示 local_path 与 remote_path，用户批准才传；
- 上传前本地 stat 获得大小（total_bytes），本地文件不存在 → 同步拒绝；
- 终态前远端 stat 校验大小一致才算 Completed。

### 5.3 transfer_status

```jsonc
{ "transfer_id": "uuid" }   // 缺省 → 返回该会话全部传输列表
// 单条返回
{ "transfer_id": "...", "direction": "download", "status": "transferring",
  "transferred_bytes": 524288000, "total_bytes": 2147483648,
  "speed_bps": 10485760, "attempt": 1,
  "error": null, "local_path": "...", "remote_path": "..." }
```

终态自描述：`completed` 带 local_path（下载场景提示交付用户）；`failed` 带 error + remote_path（远端文件保留语境）；`retrying` 带 attempt（提示等待后再轮询）。

### 5.4 transfer_cancel

```jsonc
{ "transfer_id": "uuid" }
// 返回 { "cancelled": true } 或 "not found / 已终态"
```

### 5.5 风险分级

| 工具 | 风险级 | 确认 |
|---|---|---|
| file_download | Low | 是（对齐现有机制：Low/High 均走确认流，如 class_histogram） |
| transfer_status | ReadOnly | 否 |
| transfer_cancel | ReadOnly | 否 |
| file_upload | High | 是（卡片展示本地→远端路径） |

工具描述文案明确引导：download 描述写"启动后台传输后立即返回，必须轮询 transfer_status 至终态"；upload 描述写"上传任意本地文件需用户确认"。

## 6. heap_dump 改造

生成、stat 校验两阶段不变（同步，分钟级）。第三阶段从"同步 SFTP 拉回"改为启动 TransferManager 后台任务：

```jsonc
// 返回 Agent
{ "transfer_id": "uuid", "remote_path": "...", "remote_size": 123456789,
  "dump_elapsed_ms": 45678,
  "local_path": "<artifacts>/<sid>/heapdump-1234-xxx.hprof",  // 预告最终路径
  "note": "dump 已生成，正在后台拉回。请轮询 transfer_status(transfer_id)；completed 后把 local_path 告知用户；failed 时远端文件保留，可用 file_download 重试（断点续传）。" }
```

- 远端清理时机改为"下载完成后"：heap_dump 启动任务时带 `cleanup_remote_on_success: true` 标志（独立 file_download 默认 false），rename 成功后才 `rm`，清理失败仅告警不影响传输结果；
- 移除现有同步下载分支、`download_timeout_secs` 参数与 `DOWNLOAD_*` 常量；超时语义转移到 TransferManager 重试预算（5 次尝试 / 累计 2 小时）。

## 7. 事件与前端

### 7.1 Rust 事件（AppEvent 新增两个变体，snake_case 对齐）

```rust
TransferProgress {
    session_id: String, transfer_id: String, direction: Direction,
    status: Status,            // retrying 也走这个事件
    transferred_bytes: u64, total_bytes: u64, speed_bps: u64, attempt: u32,
},
TransferFinished {             // 终态一次性事件
    session_id: String, transfer_id: String, direction: Direction,
    status: Status,            // completed | failed | cancelled
    transferred_bytes: u64, total_bytes: u64,
    error: Option<String>, local_path: Option<String>, remote_path: String,
},
```

进度事件 1s 节流；speed_bps 由后端按相邻进度差计算。

### 7.2 前端

- `lib/types.ts`：AppEvent 联合类型加 `transfer_progress` / `transfer_finished`；`ChatPartType` 加 `"transfer"`，`ChatPart` 加 `transfer?: TransferInfo`（transfer_id、direction、status、transferred_bytes、total_bytes、speed_bps、attempt、error、文件名）。
- `sessionStore.ts`：处理两个事件，在最后一条 agent 消息上按 transfer_id 幂等挂载/更新 transfer part；无 agent 消息时兜底新建一条承载。
- `AgentMessage.tsx`：新增 `TransferProgressCard` 组件渲染 transfer part——进度条（百分比）、速率（MB/s）、状态标签（传输中/重试中(第 N 次)/已完成/失败/已取消）、文件名；样式对齐 ToolCallCard 暗色卡片；终态后保留。
- 会话删除流程不动：后台任务照常跑完，内存表 LRU 兜底。

## 8. 错误处理

传输全链路按[日志规范](../../architecture/logging-standard.md)打点。分类：

| 错误 | 处理 |
|---|---|
| 环境不存在 / 参数非法 | 工具入口同步拒绝（`environment_not_found` / `invalid_params`） |
| 远端文件不存在 / stat 失败 | Failed 终态（不重试），error 带 remote_path |
| 连接失败 / 传输中断 | 重试循环，backoff 5s/15s/45s/2m/6m；attempt 用尽 → Failed |
| 重试预算耗尽（累计 2h） | Failed，error 说明"远端文件保留，可重新 file_download 断点续传" |
| 本地磁盘写入失败 | Failed 终态（本地问题重试无意义），error 带目标路径 |
| 大小校验不一致 | 传输损坏：下载删 `.part` 从头重试 |
| MCP 层会话已结束 | 后台任务照常跑完（结果在内存表，无害） |

下载失败时远端文件**永远不删**（可手动取回/断点续传的保底）；上传失败远端半成品由下次重传覆盖，Failed 终态后残留（error 说明）。

## 9. 测试策略

对齐仓库 cargo test 惯例（mock ExecChannel 风格）。

**transfer/ 模块单测**（核心）：

- 状态机全路径：pending→…→completed / failed / cancelled；
- 断点续传：第一次传一半断（mock 到 N 字节后返回 Err），重试从 N 续传（mock 记录 seek offset）；
- 大小校验失败 → 重试从头；
- 取消：传输中 cancel → 终态 cancelled，`.part` 保留；
- 重试预算：mock 永远失败 → attempt 用尽 → Failed；
- LRU 淘汰、同 session+path 去重。

**工具层单测**：四工具参数校验（remote_path 相对路径拒绝、basename 穿越、local_path 必须绝对路径）、transfer_status 列表模式、file_upload High 风险确认流。

**heap_dump 回归**：现有 5 个测试改写——成功路径断言返回 transfer_id + note；下载失败保留远端的测试移到 transfer 层；mock channel 移除 download 分支。

**前端**：`pnpm typecheck` 过 + transfer part 事件处理逻辑人工冒烟。

**验收标准**：真实环境触发 heap_dump，dump > 1GB 时 MCP 调用秒回、Agent 轮询、传输断网重连续传、UI 进度条走完、完成后本地文件大小 == 远端 stat、远端已清理。

## 10. 不做的事（YAGNI）

- SQLite 持久化任务表 + 重启自动恢复（接口不变，后续可叠加）；
- 上传断点续传；
- 下载完成后自动删远端（独立工具不做副作用）；
- 并发多文件批量传输（单文件任务已覆盖场景）；
- 传输限速/带宽控制。
