# arthas MCP 连接代理修复设计（504 + 失败清理 + 残留实例恢复）

日期：2026-09-01
状态：已评审通过
关联 issue：#7（第三次：v0.11.2 后 MCP 握手 504）

## 背景

v0.11.2 后 attach 命令本身已正确（目标机日志两次 `Attach process 3241191 success`），但 Windows → 目标主机的 MCP 连接失败。用户日志两条错误：

```
07:32  arthas MCP 握手失败: ... unexpected server response: HTTP 504 Gateway Timeout
07:36  arthas HTTP 服务在 60s 内未就绪（端口 18564）
```

### 根因分析

**A（根因）：MCP 客户端请求被系统代理截走。**

- 504 Gateway Timeout 只可能来自 HTTP 中间层：arthas 只回 200/400/401/404；SSH 隧道故障表现为连接拒绝/重置而非 504
- MCP 客户端走 rmcp 3.1.4 `StreamableHttpClientTransport`（内部 reqwest 0.13）。rmcp 的 `default_http_client()` 只设了 pool/redirect，**没有禁代理** → reqwest 默认继承系统/环境代理 → `http://127.0.0.1:{本地隧道端口}/mcp` 被企业代理截走 → 代理回连用户自己机器的 localhost 失败 → 504
- Friday 本来就要求 SSH 可达目标机，MCP 流量走隧道不需要任何代理

**B（连锁）：失败后 arthas agent 残留 → 重试被 "already bind" 守卫挡死。**

1. 07:32 attach 成功、agent 在 18563 监听 → MCP 握手 504 → Friday 报错退出，**未停 arthas** → agent 残留
2. 07:36 重试：18563 被占 → 端口分配跳到 18564；但目标 JVM 里 agent 已加载，`AgentBootstrap` 有 `Arthas server already bind` 守卫（agent jar 反编译证实）→ 新 attach 被跳过，18564 永远不监听 → 探活 60s 超时
3. 用户必须重启目标服务才能恢复

两个 bug 互相放大：A 造成首次失败，B 把失败变成持久性卡死。

## 修复设计

### A. MCP 客户端禁代理（`arthas/client.rs`）

friday 直接依赖 reqwest 0.13（与 rmcp 同版本，Cargo.toml 加 `reqwest = { version = "0.13", default-features = false, features = ["json"] }`——对齐 rmcp 的依赖特征，避免特性漂移）：

```rust
let http = reqwest::Client::builder()
    .no_proxy()                        // MCP 走 SSH 隧道，绝不经任何系统/环境代理（504 根因）
    .pool_max_idle_per_host(0)         // 对齐 rmcp 默认（避免 delayed-ack 复用停顿）
    .redirect(reqwest::redirect::Policy::none())
    .build()?;
let transport = rmcp::transport::StreamableHttpClientTransport::with_client(http, config);
```

rmcp 3.1.4 已有 `StreamableHttpClientTransport::with_client(client, config)` 公开 API（generic 版本，reqwest::Client 实现了 `StreamableHttpClient` trait）。

### B. attach 失败路径停 arthas（`arthas/attach.rs`）

`attach_arthas` 编排中，隧道建立/握手失败的错误分支：先对远端 18563 发 HTTP stop（best-effort，15s 超时，失败仅 warn），再返回错误。实现为错误路径统一收尾：`cleanup_partial_attach(channel, port, token, reason) -> ManagerError`（stop 失败不掩盖原错误）。

探活超时分支（wait_http_ready 失败）同样收尾——此场景 agent 多半没起来，stop 会 404/连接失败，无害；但 B 场景（agent 起了、隧道/握手失败）必须停。

### C. attach 前清理残留实例（`arthas/attach.rs` 编排 + manager 协作）

端口分配（步骤 4）前插入清理步骤：

1. **活跃端口收集**：manager 提供 `active_remote_ports(&self, env_id) -> Vec<u16>`（遍历 Ready 会话的 remote_port；ArthasEntry 增加记录 remote_port 的字段，attach 成功落定回写）
2. **探测段内端口**：`18563..18572` 逐个探活（复用 port_probe_command）。被占且 ∉ 活跃端口 → 判定为残留实例
3. **逐个 stop**：`stop_command(port, token)`——注意 token：残留实例的 token 是上次 attach 生成的，本次会话不知道。v0.11.2+ 的实例有 `arthas.localConnectionNonAuth=true`，stop 请求从目标机本地发起（SSH exec curl 127.0.0.1）→ 免密通过，token 随便填。对更早版本（无 localConnectionNonAuth）的残留，stop 会 401——此时报错提示用户重启目标服务
4. **等端口释放**：stop 后轮询探测（最多 15s，500ms 间隔），释放后继续
5. **停不掉**（仍在监听）：报结构化错误（"端口 X 被残留 arthas 占用且无法停止，请重启目标服务"），中止 attach

清理只针对**同环境**：跨环境/其他用途占用 18563-18572 的进程，stop 请求对非 arthas 服务是无效 POST，无副作用；探测范围严格限定在 Friday 自己的端口段。

### 不变的

- 包下发、attach 命令形态、properties（v0.11.1/v0.11.2 已修好）
- stop_command 构造（Bearer + curl/wget 兜底）；C 场景传占位 token，靠 localConnectionNonAuth 免密
- 隧道/MCP 客户端接口（ArthasClient trait）
- 前端无改动

## 测试策略（TDD）

- `client.rs`：连接工厂改造后可注入 http client 构造器（沿用 analyzer 的 production_client_factory 模式）；单测断言构造器调用 no_proxy 配置（Builder 无法内省，改为断言自定义构造函数被调用 + 集成冒烟）
- `attach.rs` 纯函数：
  - 残留清理命令序列（探测 → stop → 等待释放）构造正确性
  - `parse_free_port` 之后端口占用判定
- 编排层（SequentialChannel 模式）：残留端口被 stop 后重试成功；活跃端口不被 stop；stop 401 场景报结构化错误
- manager：`active_remote_ports` 返回正确（含/不含 Ready 会话）
- 手动：用户环境验证（发布后）

## 产物影响

无资源变更；仅 Rust 逻辑。
