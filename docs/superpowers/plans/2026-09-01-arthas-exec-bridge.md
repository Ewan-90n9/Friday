# arthas exec 通道 HTTP 桥 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** arthas MCP 流量改走 ssh exec + curl 桥（绕过 sshd AllowTcpForwarding no），替换 direct-tcpip 隧道路径（issue #7 第四轮）。

**Architecture:** 新 `ExecHttpBridge` 实现 rmcp `StreamableHttpClient` trait（post_message/delete_session → ssh exec curl；get_stream → 空流）；attach 编排删隧道段改构造 bridge；McpArthasClient 经 with_client 使用 bridge。

**Tech Stack:** Rust（rmcp 3.1.4 trait 契约已核实：StreamableHttpPostResponse::{Accepted, Json, Sse}、StreamableHttpError::{Client, UnexpectedServerResponse, Deserialize...}、Sse{sse_stream} 事件结构）。

**约定：** spec：docs/superpowers/specs/2026-09-01-arthas-exec-bridge-design.md；本地已实测 arthas MCP 纯 POST 全链路可行；main 分支直接干。

**已核实的 rmcp 契约（实现依据）：**
- `trait StreamableHttpClient: Clone + Send + 'static`，`type Error: std::error::Error + Send + Sync + 'static`
- `post_message(uri: Arc<str>, message: ClientJsonRpcMessage, session_id: Option<Arc<str>>, auth_header: Option<String>, custom_headers: HashMap<HeaderName, HeaderValue>) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>>> + Send + '_`
- `delete_session(uri, session_id: Arc<str>, auth_header, custom_headers) -> impl Future<Output = Result<(), StreamableHttpError<Self::Error>>> + Send + '_`
- `get_stream(...) -> impl Future<Output = Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>>> + Send + '_`
- `StreamableHttpPostResponse::{Accepted, Json(ServerJsonRpcMessage, Option<String>), Sse(BoxedSseStream, Option<String>)}`（`BoxedSseStream = BoxStream<'static, Result<Sse, SseError>>`）
- `Sse { event: Option<String>, data: Option<String>, id: Option<String>, retry: Option<u64> }`（sse-stream crate，字段 pub 可直接构造；builder 方法 `.data(...)` 等）
- `StreamableHttpError` non_exhaustive 但 `Client(E)` / `UnexpectedServerResponse(Cow<'static, str>)` / `Deserialize(#[from] serde_json::Error)` 可用
- `ClientJsonRpcMessage` / `ServerJsonRpcMessage`：serde 序列化（message → JSON 字符串喂 curl stdin；响应 data → from_str）
- custom_headers：bridge 忽略（我们不用自定义头；Bearer 走 auth_header）

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `src-tauri/src/arthas/bridge.rs` | 新建 | ExecHttpBridge：StreamableHttpClient 实现 + curl 命令构造 + 响应解析（纯函数拆分便于测试） |
| `src-tauri/src/arthas/client.rs` | 修改 | connect_arthas_client 改用 ExecHttpBridge（with_client）；签名变化 |
| `src-tauri/src/arthas/attach.rs` | 修改 | 编排删隧道段（tunnels.open/close/失败拆隧道）；构造 bridge 传入 client 工厂；stop 不变 |
| `src-tauri/src/arthas/manager.rs` | 检查 | AttachDeps.tunnels 字段是否仍被 attach 用（close_for_environment 等保留）——仅 attach 路径脱钩 |
| `src-tauri/src/arthas/mod.rs` | 修改 | 挂 bridge 模块 |

---

### Task 1: ExecHttpBridge 核心（TDD）

**Files:**
- Create: `src-tauri/src/arthas/bridge.rs`
- Modify: `src-tauri/src/arthas/mod.rs`

- [ ] **Step 1.1: 失败测试**

bridge.rs 先写测试（RecordingChannel 模式，参照 attach.rs tests 的 RecordingChannel——拷贝或提取共用；先拷贝保持独立）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // RecordingChannel: 记录 run 命令 + 队列响应（拷自 attach.rs tests，含 stdin 数据记录——
    // ExecChannel::run 只有 cmd 参数，curl --data-binary @- 的 stdin 怎么进？
    // → 关键设计决策：不用 stdin。改为命令行内嵌：--data <shell_quote(json)>。
    //    JSON-RPC 消息可能较大（tools/call 参数），内嵌命令行长度上限 ~128KB（Linux ARG_MAX 足够）；
    //    shell_quote_single 转义无注入面。ExecChannel::run(cmd) 单参数即可。
    // 测试断言命令含 --data '...json...' 转义正确。
}
```

**设计修正（实现前定案）**：`ExecChannel::run(cmd)` 单参数无 stdin 通道 → curl 用 `--data`（命令内嵌 JSON，`shell_quote_single` 转义）。JSON 含单引号会被转义为 `'\''`，安全。ARG_MAX 128KB 上限对 MCP 消息足够（arthas 工具参数最长 watch/trace 表达式，远小于此）。

测试用例：
1. `build_post_command(url, token, session, body)` 纯函数：断言含 `-X POST`、`-H 'Authorization: Bearer tok'`、`-H 'Content-Type: application/json'`、`-H 'Accept: application/json, text/event-stream'`、session 存在时 `-H 'mcp-session-id: sid'`、`--data '<escaped>'`、URL
2. `build_delete_command(url, token, session)`：`-X DELETE` + 头
3. `parse_response(stdout) -> Result<(u16 http_code, String body), String>`：curl `-w "\n%{http_code}"` 分隔 body 与 code（多行 body：code 是最后一行）
4. `parse_body_to_post_response(body, session_from_header)` 纯函数：
   - JSON body（`{"jsonrpc"...}`）→ Json(from_str, session)
   - SSE body（`event: message\ndata: {...}`）→ Sse(单事件流, session)
   - 空且 202 → Accepted（由调用方按 code 分派）
5. `post_message` 集成（RecordingChannel）：stub 返回 `{"jsonrpc":"2.0","id":1,"result":{...}}\n200` → 断言 Json 变体 + 命令正确
6. `post_message` SSE 响应：stub 返回 `event: message\ndata: {"jsonrpc"...}\n\n200` → Sse 变体，流中一条事件 data 正确
7. `delete_session`：stub `200` → Ok；`404` → Err（UnexpectedServerResponse 或映射）
8. `get_stream`：空流（stream pending/empty，返回 Ok 空流）
9. 非 2xx（401）→ StreamableHttpError（含 HTTP 401 字样）

- [ ] **Step 1.2: 跑测试确认 RED**（函数不存在编译错即 RED）

- [ ] **Step 1.3: 实现**

```rust
use async_trait::async_trait; // 注意：trait 用 impl Future 语法不是 #[async_trait]——按 rmcp 契约写
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use sse_stream::Sse;
use std::{collections::HashMap, sync::Arc};

use crate::exec::channel::{ExecChannel, ExecOutput};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};

/// bridge 自身错误（exec/curl 层）
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("exec 失败: {0}")]
    Exec(String),
    #[error("目标机 curl 不可用或执行失败: {0}")]
    Curl(String),
}

#[derive(Clone)]
pub struct ExecHttpBridge {
    channel: Arc<dyn ExecChannel>,
    remote_port: u16,
    timeout_secs: u64,
}
```

实现要点：
- `post_message`：serialize message → `build_post_command` → exec（tokio::time::timeout 包裹）→ `parse_response` → 按 code：202 → Accepted；200 且 body JSON → Json；200 且 SSE → Sse（构造单事件流：`futures::stream::once(async move { Ok(sse_event) }).boxed()`）；401/其他 → UnexpectedServerResponse(format!("HTTP {code}: {body前200字}"))
- SSE 单事件构造：`Sse { event: None, data: Some(json_str), id: None, retry: None }`——`data` 是 data: 行后的原始字符串
- `delete_session`：同上，2xx → Ok
- `get_stream`：`Ok(futures::stream::empty().boxed())`
- `build_post_command` 用 `crate::exec::ssh::shell_quote_single` 转义 body
- curl 超时 `-m {timeout}`；`-s -S`；`--max-time`
- URL 形态：`http://127.0.0.1:{remote_port}/mcp`（bridge 内部构造，uri 参数忽略或仅日志）
- trait 的 `impl Future + '_` 语法：直接 async fn 不行（trait 无 async）——按 rmcp 写法 `fn post_message(&self, ...) -> impl Future<...> + Send + '_ { async move { ... } }`

- [ ] **Step 1.4: GREEN + 提交**

```bash
git add src-tauri/src/arthas/bridge.rs src-tauri/src/arthas/mod.rs
git commit -m "feat: exec-channel http bridge for arthas mcp (bypasses tcp forwarding)"
```

---

### Task 2: 接线——client/attach 改用 bridge

**Files:**
- Modify: `src-tauri/src/arthas/client.rs`
- Modify: `src-tauri/src/arthas/attach.rs`

- [ ] **Step 2.1: client.rs**

`connect_arthas_client` 签名改为接收 bridge：

```rust
pub async fn connect_arthas_client(bridge: ExecHttpBridge) -> Result<McpArthasClient, String> {
    use rmcp::ServiceExt;
    // uri 仅用于 rmcp config（session 管理用任意稳定值）
    let config = StreamableHttpClientTransportConfig::with_uri("http://127.0.0.1/mcp")
        .auth_header(token); // token 移入 bridge 的命令构造，config 的 auth_header 不再使用？
```

——注意：`auth_header` 在 config 上由 rmcp 传给 post_message 的 `auth_header` 参数——**bridge 从该参数拿 token**，不是自己存！重构：ExecHttpBridge 不存 token；token 走 rmcp 的 config.auth_header → post_message(auth_header: Option<String>) 参数 → build_post_command。删掉 Step 1 设计里 bridge 的 token 字段（保留 channel/remote_port/timeout）。uri 同理：rmcp 传 config.uri → post_message(uri)——bridge 忽略 uri 用 remote_port 构造（或直接用 uri？uri 是 config 的——把它构造成 `http://127.0.0.1:{remote_port}/mcp` 放 config，bridge 直接用 uri 参数！更干净：**bridge 完全用 rmcp 传入的 uri + auth_header，自身只存 channel + timeout**）

修订 ExecHttpBridge 字段：`{ channel: Arc<dyn ExecChannel>, timeout_secs: u64 }`；URL 由 attach 构造 config 时给 `http://127.0.0.1:{port}/mcp`。

- [ ] **Step 2.2: attach.rs 编排**

- 删隧道段：`deps.tunnels.open(...)`、握手失败 `deps.tunnels.close(...)`、stop_handle 里的 tunnels 引用（ProductionStopHandle 去掉 tunnels 字段与 close 调用——arthas 会话不再占用隧道）
- 构造：`let url = format!("http://127.0.0.1:{port}/mcp"); let bridge = ExecHttpBridge { channel: channel.clone(), timeout_secs: 60 }; connect_arthas_client(&url, &token, bridge).await`（签名按实现定：token 仍传 rmcp config.auth_header）
- 进度事件 stage："tunnel" → "bridge"（detail: "MCP 通路（exec HTTP 桥）"）
- AttachDeps.tunnels 字段：attach 不再用；但 AttachDeps 由 lib.rs 构造，字段删除会动 lib.rs + ProductionStopHandle。**保留字段但 attach 不用**（最小改动）或删掉（干净）。选删——grep tunnels 在 attach.rs 的全部引用一并清理；lib.rs AttachDeps 构造去字段。
- AttachedSession.remote_port 语义不变（bridge 的 URL 端口）

- [ ] **Step 2.3: 编译 + 测试 + 手动本地全链路**

- `cargo test --manifest-path src-tauri/Cargo.toml` 全绿
- **本地 math-game 全链路**（bridge 无法对真 SSH 测，但可用 LocalExecChannel：实现一个 ExecChannel 直接本地 tokio::process::Command 跑 curl.exe——测试专用，放 bridge.rs tests）：
  - 起 math-game + arthas（18563 + token）
  - LocalExecChannel + ExecHttpBridge → connect → initialize/tools/list/tools/call dashboard 全通
  - 这一步是关键回归（验证 SSE 解析/命令转义在真 curl 下正确）

- [ ] **Step 2.4: 提交**

```bash
git add src-tauri/src/arthas/client.rs src-tauri/src/arthas/attach.rs src-tauri/src/lib.rs
git commit -m "feat: arthas mcp over exec bridge, drop tunnel dependency"
```

---

### Task 3: 回归 + 发布 v0.11.4

- [ ] `cargo test` / `cargo check` / `pnpm typecheck` 全绿
- [ ] AGENTS.md：Arthas 段落更新（"经 SSH direct-tcpip 隧道" → "经 SSH exec 通道 HTTP 桥（不依赖 sshd TCP 转发，`arthas/bridge.rs`）"；MCP client 绕过代理表述保留）
- [ ] 推送 + tag v0.11.4 + issue 回复（说明 AllowTcpForwarding no 根因 + exec 桥方案 + 无需改环境）

## Self-Review 记录

- rmcp 契约（uri/auth_header 经参数传递、trait impl Future 语法、Sse 构造）已从源码核实并体现在 Task 1/2 ✓
- stdin 不可用 → --data 内嵌 + shell_quote（ARG_MAX 128KB 足够）✓
- LocalExecChannel 本地全链路回归覆盖"真 curl 下转义/解析"风险 ✓
- 隧道脱钩：attach 不再用 tunnels；TunnelManager 保留给未来（spec 明确）✓
