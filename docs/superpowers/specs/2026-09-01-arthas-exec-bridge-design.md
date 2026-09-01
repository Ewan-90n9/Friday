# arthas MCP exec 通道桥接设计（绕过 sshd TCP 转发限制）

日期：2026-09-01
状态：已评审通过
关联 issue：#7（第四次：v0.11.3 后隧道连接失败）

## 背景

v0.11.3 后错误推进到 `error sending request for url (http://127.0.0.1:4713/mcp)`（连接层失败）。目标机实测确认：**sshd 配置 `AllowTcpForwarding no`**（环境不可修改）。direct-tcpip channel 被 sshd 拒绝（administratively prohibited），现有隧道架构在该环境不可用。

前四轮修复（404 包下发 / --pid CLI / 系统代理 504 / 残留清理）均真实有效，但都死在 direct-tcpip 之前——该通路直到本轮才被首次触发。

### 可行性验证（本地 math-game + arthas 4.3.5 实测）

纯 POST（无 GET SSE 流）走 arthas MCP 全链路：

| 请求 | 响应 | 结论 |
|---|---|---|
| initialize | 200 + `application/json` + mcp-session-id | 一次性 JSON |
| notifications/initialized | 202 | 无 body |
| tools/list | SSE 格式，**单事件即完** | data 里是完整 JSON-RPC 响应 |
| tools/call dashboard | 同上 | 完整结果单事件返回 |

关键结论：arthas MCP 的 POST 响应虽是 SSE Content-Type，但每请求单事件一次性返回，**不依赖 GET 长连接流** → 可用 `ssh exec curl` 承载每个 POST，响应体原样回传。

## 方案：exec 通道 HTTP 桥（B 方案）

Friday 实现 rmcp 的 `StreamableHttpClient` trait（三个方法），HTTP 语义映射到 ssh exec curl：

```
rmcp StreamableHttpClientTransport::with_client(ExecHttpBridge, config)
                                        │
  post_message(uri, msg, session, auth) │ → ssh exec: curl -s -m N -X POST -H auth -H content-type
                                        │   -H accept -H session-id --data-binary @- url
                                        │   （stdin 喂 JSON-RPC 消息，stdout 收响应体）
  delete_session(uri, session, auth)    │ → ssh exec: curl -s -m N -X DELETE -H auth -H session-id url
  get_stream(...)                       │ → 返回空流（协议允许 server 不开 GET 流；arthas 不依赖）
```

### 与现有架构的关系

- **ArthasManager / attach 编排 / 残留清理 / 端口探测全部不变**——探活本来就走 exec
- **隧道不再是 arthas MCP 的必经通路**：attach 编排中删除 `tunnels.open`/`tunnels.close`（arthas 路径）；TunnelManager 保留（通用基础设施，未来 JMX 等用；但本环境不可用时同样受限——不在本次范围）
- `ProductionStopHandle` 的 stop 也改走 exec curl（原本就是 exec，只需 URL 从隧道端口概念改为远端端口——实际 stop_command 本来就是 `http://127.0.0.1:{远端端口}/api` 经 exec 执行，无需改）
- MCP 握手 URL 不再是 `http://127.0.0.1:{本地隧道端口}/mcp`，bridge 内部直接用 `http://127.0.0.1:{远端端口}/mcp`（在目标机本地视角）

### ExecHttpBridge 设计

```rust
/// arthas MCP 的 exec 通道 HTTP 桥：rmcp StreamableHttpClient 实现。
/// 每请求一条 ssh exec channel 跑 curl（目标机本地视角 127.0.0.1:{remote_port}），
/// 绕过 sshd AllowTcpForwarding 限制（direct-tcpip 被禁的环境唯一可用通路）。
pub struct ExecHttpBridge {
    channel: Arc<dyn ExecChannel>,
    remote_port: u16,
    timeout_secs: u64,
}
```

- `post_message`：构造 curl 命令（shell_quote 转义 stdin 数据——实际用 `--data-binary @-` + stdin 写入，无注入面），exec 执行，stdout 按响应解析：
  - 简化解析：响应头由 curl `-w` 输出 http_code 分隔（`-w "\n%{http_code}"`），body + code 拼接回传给 rmcp 需要的类型（StreamableHttpPostResponse）
  - SSE body（`event: message\ndata: {...}`）：rmcp 的 StreamableHttpPostResponse 期望 SSE 或 JSON——查 rmcp 类型定义后适配（SSE 事件提取 data 行转 JSON，或直接传 SSE body 让 rmcp 解析——以 rmcp 实际接口为准，实现时定）
- `delete_session`：curl -X DELETE，同上
- `get_stream`：`futures::stream::empty()`（BoxStream 空流）
- 超时：单请求 60s（dashboard 等工具可能秒级~十秒级）；exec 本身有 run 的裸调用（无超时参数）——需给 ExecChannel 加带超时的 run 或用 tokio::time::timeout 包裹

### 错误路径

- curl 不可用（极少见）→ 握手时报明确错误（"目标机无 curl，无法建立 MCP 通路"）
- exec 失败/超时 → rmcp Transport 错误 → 现有 invalidate 机制
- HTTP 401 → token 错误（理论不发生，token 同源生成）

## 不做（YAGNI）

- 不改 TunnelManager（保留通用能力，arthas 不再用）
- 不做 GET SSE 流桥接（arthas 不依赖；rmcp 容忍空流）
- 不做请求合并/连接复用（每请求一条 exec channel，MCP 频率低，开销毫秒级）
- analyzer（stdio）/ heap 传输（SFTP）不受影响，不动

## 测试策略（TDD）

- `ExecHttpBridge` 单测（RecordingChannel 模式）：
  - post_message 生成的 curl 命令正确（URL/Bearer/session-id/stdin data）
  - 响应解析：JSON body / SSE 单事件 body / 非 2xx → 错误
  - delete_session 命令形态
  - get_stream 空流
- attach.rs 编排：隧道段替换为 bridge 构造（无隧道 open/close）；AttachedSession.remote_port 语义不变（bridge 用远端端口）
- manager：无改动（client 抽象不变，McpArthasClient 换 bridge transport）
- 本地 math-game 全链路手动回归（实现后跑一次 initialize/tools/call via bridge——用 LocalExecChannel 直连本机模拟）
