# arthas MCP 代理修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 MCP 客户端被系统代理截走（504）、attach 失败 arthas 残留、残留实例挡死重试三个问题（issue #7 第三轮）。

**Architecture:** A：client.rs 用自建 `reqwest::Client`（`.no_proxy()`）经 rmcp `with_client` 连接；B：attach.rs 失败路径统一收尾（先 stop 再报错）；C：端口分配前清理段内残留实例（探测 → HTTP stop → 等释放），活跃端口从 manager 收集避免误杀。

**Tech Stack:** Rust（rmcp 3.1.4 reqwest 0.13 / 现有 SequentialChannel 测试模式）。

**约定：**
- 测试/检查命令同前；spec：docs/superpowers/specs/2026-09-01-arthas-mcp-proxy-fix-design.md
- main 分支直接干

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `src-tauri/Cargo.toml` | 修改 | 加 reqwest 0.13 直接依赖（对齐 rmcp 特性：default-features = false, features = ["json"]） |
| `src-tauri/src/arthas/client.rs` | 修改 | no_proxy http client + with_client 连接；构造器 seam |
| `src-tauri/src/arthas/attach.rs` | 修改 | 失败收尾 cleanup_partial_attach；残留清理（探测/stop/等释放）；ArthasManager::active_remote_ports 调用 |
| `src-tauri/src/arthas/manager.rs` | 修改 | ArthasEntry.remote_port + active_remote_ports()；run_attach_task 落定回写 |

---

### Task 1: MCP 客户端 no_proxy（修根因 A）

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/arthas/client.rs`
- Modify: `src-tauri/src/lib.rs`（若有 connect 工厂注入点——先 grep 确认 client.rs 直接被 attach.rs 调用，无 lib.rs 工厂则不动）

- [ ] **Step 1.1: Cargo.toml 依赖**

```toml
reqwest = { version = "0.13", default-features = false, features = ["json"] }
```

（与 rmcp 的 reqwest 同 semver 段，lockfile 收敛为一个版本。）

- [ ] **Step 1.2: client.rs 改造**

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use super::manager::{ArthasClient, CallOutcome};

pub struct McpArthasClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 构建直连 HTTP 客户端：MCP 流量走 SSH 隧道（127.0.0.1 本地端口），
/// 必须绕过一切系统/环境代理——企业代理截走 localhost 请求时回 504（issue #7）。
fn direct_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("构建 MCP http 客户端失败: {e}"))
}

/// 连接 + MCP 握手（30s 超时）
pub async fn connect_arthas_client(url: &str, token: &str) -> Result<McpArthasClient, String> {
    use rmcp::ServiceExt;

    let config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url)
        .auth_header(token);
    let transport = rmcp::transport::StreamableHttpClientTransport::with_client(
        direct_http_client()?,
        config,
    );

    let service = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .map_err(|_| format!("arthas MCP 握手超时（30s）: {url}"))?
        .map_err(|e| format!("arthas MCP 连接失败: {e}"))?;
    // ...其余不变
}
```

call_tool/shutdown 不变。`StreamableHttpClientTransport::with_client` 接受任何实现 `StreamableHttpClient` 的类型；reqwest::Client 有该 impl（`transport-streamable-http-client-reqwest` feature 已启用）。

- [ ] **Step 1.3: 编译 + 现有测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml` → 通过（注意类型标注：`with_client` 的泛型由参数推断，`StreamableHttpClientTransport` 别名是 generic 的——若需要显式类型，用 `rmcp::transport::StreamableHttpClientTransport::<reqwest::Client>` 或让推断处理）
Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas` → 全过

- [ ] **Step 1.4: 提交**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/arthas/client.rs
git commit -m "fix: arthas mcp client bypasses system proxy (504 root cause)"
```

---

### Task 2: manager 记录活跃端口（C 的依赖）

**Files:**
- Modify: `src-tauri/src/arthas/manager.rs`

- [ ] **Step 2.1: 失败测试（TDD）**

tests 模块加：

```rust
    #[tokio::test]
    async fn test_active_remote_ports_lists_ready_sessions_only() {
        let (manager, _factory_rx) = test_manager().await; // 现有测试基建：找类似 helper；没有就照 manager 现有测试模式造
        // 造一个 Ready 条目（remote_port 18563）和一个 Attaching 条目（无端口）
        // 断言 active_remote_ports("env-1") == vec![18563]
        // 断言 active_remote_ports("other-env") 为空
    }
```

（实现时先读 manager.rs 现有测试怎么构造条目——grep `sessions.insert` in tests。若现有测试不直接操作 inner，走 factory mock 让 attach 成功/进行中。）

- [ ] **Step 2.2: 实现**

```rust
struct ArthasEntry {
    // ...现有字段
    /// attach 成功的远端 arthas HTTP 端口（残留清理时排除活跃会话）
    remote_port: Option<u16>,
}
```

- `run_attach_task` Ok 分支：需要从 AttachedSession 拿到端口 → `AttachedSession` 增加 `pub remote_port: u16`（attach.rs 构造处填充 `port`）；`entry.remote_port = Some(attached.remote_port)`
- `ArthasManager::active_remote_ports`：

```rust
    /// 当前环境 Ready 会话占用的远端 arthas 端口（残留清理排除用）
    pub async fn active_remote_ports(&self, env_id: &str) -> Vec<u16> {
        let inner = self.inner.lock().await;
        inner
            .sessions
            .iter()
            .filter(|((e, _), _)| e == env_id)
            .filter_map(|(_, entry)| {
                if matches!(*entry.phase_tx.borrow(), ArthasPhase::Ready) {
                    entry.remote_port
                } else {
                    None
                }
            })
            .collect()
    }
```

- attach.rs 中所有 `AttachedSession { client, stop_handle }` 构造点补 `remote_port: port`（仅生产构造点 1 处；测试 mock 若构造 AttachedSession 需同步补字段）。

- [ ] **Step 2.3: 测试 + 提交**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas` → 全过

```bash
git add src-tauri/src/arthas/manager.rs src-tauri/src/arthas/attach.rs
git commit -m "feat: manager tracks active remote ports for stale-instance cleanup"
```

---

### Task 3: attach 失败收尾 + 残留清理（修 B/C）

**Files:**
- Modify: `src-tauri/src/arthas/attach.rs`

- [ ] **Step 3.1: 纯函数（探测/清理命令）**

复用现有 `port_probe_command(port)`（探测单个）与 `stop_command(port, token)`（清理时传占位 token `""`——localConnectionNonAuth 下本地 stop 免密；Bearer 为空时 curl 发 `Authorization: Bearer `，无害）。新增编排函数（带 SSH 通道参数，非纯字符串函数，走 SequentialChannel 测试）：

```rust
/// 残留 arthas 实例清理：探测段内端口，被占且非活跃 → HTTP stop + 等释放。
/// v0.11.2+ 实例（localConnectionNonAuth）本地 stop 免密；更早残留会 401 → 报错指路重启目标服务。
async fn cleanup_stale_instances(
    channel: &dyn ExecChannel,
    active_ports: &[u16],
) -> Result<(), ManagerError> {
    for port in ARTHAS_PORT_START..ARTHAS_PORT_START + ARTHAS_PORT_CANDIDATES {
        if active_ports.contains(&port) {
            continue;
        }
        let probe = run_with_timeout(channel, &port_probe_command(port), 15)
            .await
            .map_err(|e| ManagerError::Attach(format!("残留端口探测失败: {e}")))?;
        if probe.stdout.trim() != "busy" {
            continue;
        }
        tracing::info!(port, "stale arthas instance detected, stopping");
        run_with_timeout(channel, &stop_command(port, ""), 15).await?;
        // 等端口释放（最多 15s）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let check = run_with_timeout(channel, &port_probe_command(port), 15).await?;
            if check.stdout.trim() != "busy" {
                tracing::info!(port, "stale arthas instance stopped");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ManagerError::Attach(format!(
                    "端口 {port} 被残留 arthas 占用且无法停止（可能是旧版本实例，stop 被拒绝）。请重启目标服务后重试"
                )));
            }
        }
    }
    Ok(())
}
```

`attach_arthas` 步骤 3.5（check_user 之后、allocate_port 之前）插入：

```rust
    // 3.5 残留实例清理：上次失败 attach 留下的 arthas 会占住端口并因 already-bind 守卫挡死重试
    progress("cleanup", "清理残留 arthas 实例".to_string());
    let active_ports = deps.active_remote_ports(&req.env_id).await; // 见下：AttachDeps 增加回调
    cleanup_stale_instances(channel.as_ref(), &active_ports).await?;
```

**AttachDeps 增加字段**：`pub active_remote_ports: Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Vec<u16>> + Send>> + Send + Sync>`（manager 的引用不能直接持有——manager 持有 attach factory，会成环）。lib.rs 构造：

```rust
    let arthas_manager_for_ports = arthas_manager.clone(); // 需在 manager 创建后构造 deps？
```

——注意依赖顺序：现有代码 AttachDeps 在 manager 创建**之前**构造。重构：把 AttachDeps 构造挪到 manager 之后不可行（factory 进 manager）。改用 `Arc<tokio::sync::OnceCell<Weak<ArthasManager>>>` 或更简单：**ManagerInner 不需要 manager 引用——给 AttachDeps 传 manager 的内部状态共享**：

最简方案：`ArthasManager` 拆出 `ports_view: Arc<tokio::sync::Mutex<ManagerInner>>` 共享克隆——manager.inner 本来就是 `Arc<Mutex<ManagerInner>>`。AttachDeps 增加：

```rust
    /// 共享 manager 内部状态（残留清理读活跃端口；避免 manager↔factory 循环依赖）
    pub sessions_view: Option<Arc<tokio::sync::Mutex<ManagerInner>>>,
```

ManagerInner 需 pub(crate)/字段可见性调整；`ArthasManager::inner()` 不暴露，直接在 new 时克隆 Arc 传入 deps。构造顺序改为：

```rust
    // lib.rs：先建共享 inner，再建 manager 和 deps
    let manager_inner: Arc<tokio::sync::Mutex<ManagerInner>> = Arc::new(tokio::sync::Mutex::new(ManagerInner::new()));
    // ArthasManager::with_inner(...) 或改 new 签名返回 (manager, inner_view)
```

（实现时选最小改动路径：`ArthasManager::new` 改为 `pub fn with_shared_inner(...) -> (Self, Arc<Mutex<ManagerInner>>)`，或给 ArthasManager 加 `pub fn sessions_view(&self) -> Arc<Mutex<ManagerInner>>`——后者最简单，零构造顺序变化：lib.rs 先建 manager（factory 里 deps 不含 view），再 `let view = manager.sessions_view()`……但 deps 在 factory 里已捕获。→ **最终方案**：deps 构造挪到 manager 之前不行，就改 factory 捕获：lib.rs 现在的顺序是 deps → factory → manager。改为：inner → deps(含 inner view) → factory → manager::with_inner(inner)。允许实现者按此重构，保持外部 API 不变。）

- [ ] **Step 3.2: 失败收尾（修 B）**

`attach_arthas` 中隧道/握手/探活失败分支统一：

```rust
    // 6. 探活失败 → 先停可能已起的 arthas 再报错（防 already-bind 残留）
    if let Err(e) = wait_http_ready(channel.as_ref(), port, std::time::Duration::from_secs(60)).await {
        cleanup_partial_attach(channel.as_ref(), port, &token).await;
        return Err(e);
    }
    // 7. 隧道失败 / 握手失败 → 同样先 stop
```

```rust
/// attach 中途失败的收尾：best-effort 停掉可能已启动的 arthas（stop 失败不掩盖原错误）
async fn cleanup_partial_attach(channel: &dyn ExecChannel, port: u16, token: &str) {
    match run_with_timeout(channel, &stop_command(port, token), 15).await {
        Ok(_) => tracing::info!(port, "partial attach cleaned up (arthas stopped)"),
        Err(e) => tracing::warn!(port, error = %e, "cleanup stop failed, arthas agent may remain on target"),
    }
}
```

隧道失败分支：`deps.tunnels.open` Err → cleanup_partial_attach + 报错；握手失败分支（connect_arthas_client Err）→ 先拆隧道（现有逻辑）再 cleanup_partial_attach + 报错。

- [ ] **Step 3.3: TDD 测试（SequentialChannel 模式）**

tests（attach.rs 现有 RecordingChannel/SequentialChannel 基建——manager tests 用 factory mock，attach 编排测试较少；按现状选可测层）：

1. `cleanup_stale_instances`：busy 端口 → stop 被调用（命令序列断言 stop_command 内容）→ 释放 → Ok；活跃端口跳过（无 stop 命令）；停不掉 → 报错含"重启目标服务"
2. `cleanup_partial_attach`：stop 命令发出（best-effort，失败仅 warn 不 Err）
3. 纯函数层无新增（stop_command/port_probe_command 复用）

（编排全链路（attach_arthas）无现成注入框架的，不强求；manager.active_remote_ports 在 Task 2 已测。）

- [ ] **Step 3.4: 全量 + 提交**

Run: `cargo test --manifest-path src-tauri/Cargo.toml` → 全绿

```bash
git add src-tauri/src/arthas/attach.rs src-tauri/src/arthas/manager.rs src-tauri/src/lib.rs
git commit -m "fix: stop arthas on attach failure and clean stale instances before port allocation"
```

---

### Task 4: 回归 + 发布

- [ ] `cargo check` / `cargo test` 全绿 / `pnpm typecheck`
- [ ] AGENTS.md：Arthas 段落追加一句（"attach 前自动清理残留实例；MCP 客户端绕过系统代理"）
- [ ] 提交、推送、tag v0.11.3、回复 issue #7（说明 504 根因 + 残留自动恢复，无需重启目标服务）

## Self-Review 记录

- Spec 覆盖：A（Task 1）/ B（Task 3.2）/ C（Task 3.1 + Task 2 端口追踪）✓
- 依赖顺序难题（manager↔factory 环）已给出 inner 共享方案，允许实现者选最小改动路径 ✓
- 占位符：Task 2 测试 helper 依赖现有基建（已注明查证方式）；lib.rs 构造顺序重构给出方向但留实现自由度 ✓
