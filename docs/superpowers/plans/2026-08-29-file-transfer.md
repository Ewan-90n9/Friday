# 文件上传下载工具实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现独立文件上传/下载 Agent 工具（TransferManager 后台异步传输 + 轮询 + 断点续传 + UI 进度条），并把 heap_dump 的同步拉回改为复用该引擎。

**Architecture:** 新增 `src-tauri/src/transfer/` 基础设施模块：内存状态注册表 + 每任务一个后台 tokio task + 专用 SSH 连接（不走 ExecChannelPool）。工具层 `tools/builtin/file_transfer.rs` 提供 file_download/file_upload/transfer_status/transfer_cancel 四个 MCP 工具，秒回 transfer_id。heap_dump 生成+校验后启动 TransferManager 下载任务。前端新增 transfer ChatPart 类型渲染进度条。

**Tech Stack:** Rust (tokio, russh, russh-sftp, sqlx), React + TypeScript + Zustand, Tauri IPC events。

**Spec:** `docs/superpowers/specs/2026-08-29-file-transfer-design.md`

**约定（全任务通用）：**
- 所有 Rust 命令都在仓库根目录跑：`cargo test --manifest-path src-tauri/Cargo.toml`（跑单测用 `cargo test --manifest-path src-tauri/Cargo.toml <测试名子串>`）
- 日志规范：每个入口 `tracing::info!`、错误路径 `tracing::warn!`/`error!`，字段命名对齐现有代码（session_id/env_id/transfer_id）
- 前端检查：`pnpm typecheck`
- 提交信息不用 Conventional Commits 前缀（对齐 git log 现状：`feat: xxx` 风格已存在，继续用）

---

## 文件结构总览

```
src-tauri/src/transfer/
├── mod.rs            # 模块入口；TransferManager（状态注册表 + 启动/查询/取消）
├── state.rs          # TransferState/Direction/Status 类型 + serde
└── worker.rs         # 后台执行循环：专用连接 + 重试 + 断点续传 + 进度事件
src-tauri/src/tools/builtin/file_transfer.rs   # 4 个工具 handler + ToolDef
src-tauri/src/exec/ssh.rs                      # download 加 offset + 进度回调参数
src-tauri/src/exec/pool.rs                     # build_transport/fetch_environment 提为 pub
src-tauri/src/app/events.rs                    # TransferProgress/TransferFinished 变体
src-tauri/src/tools/builtin/jvm/heap_dump.rs   # 三阶段改为启动后台下载
src-tauri/src/lib.rs                           # TransferManager 创建 + 工具注册
src/lib/types.ts                                # AppEvent 扩展 + TransferInfo + ChatPart.transfer
src/store/sessionStore.ts                       # 两个事件处理
src/components/chat/TransferProgressCard.tsx    # 进度条卡片
src/components/chat/AgentMessage.tsx            # 渲染 transfer part
```

---

### Task 1: TransferState 类型与状态机骨架

**Files:**
- Create: `src-tauri/src/transfer/mod.rs`
- Create: `src-tauri/src/transfer/state.rs`
- Modify: `src-tauri/src/lib.rs:1-8`（加 `mod transfer;`）
- Modify: `src-tauri/src/exec/pool.rs:136-187`（build_transport/fetch_environment 改 pub）
- Test: `src-tauri/src/transfer/state.rs` 内嵌 tests

- [ ] **Step 1: 写失败测试（state.rs 整个文件含 tests）**

`src-tauri/src/transfer/state.rs`：

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Connecting,
    Transferring,
    Retrying,
    Completed,
    Failed,
    Cancelled,
}

impl Status {
    /// 终态：completed / failed / cancelled
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Completed | Status::Failed | Status::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferState {
    pub id: String,
    pub direction: Direction,
    pub session_id: String,
    pub env_id: String,
    pub remote_path: String,
    pub local_path: PathBuf,
    pub status: Status,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub speed_bps: u64,
    pub attempt: u32,
    pub error: Option<String>,
    pub cleanup_remote_on_success: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TransferState {
    pub fn new(
        direction: Direction,
        session_id: &str,
        env_id: &str,
        remote_path: &str,
        local_path: PathBuf,
        cleanup_remote_on_success: bool,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            direction,
            session_id: session_id.to_string(),
            env_id: env_id.to_string(),
            remote_path: remote_path.to_string(),
            local_path,
            status: Status::Pending,
            total_bytes: 0,
            transferred_bytes: 0,
            speed_bps: 0,
            attempt: 0,
            error: None,
            cleanup_remote_on_success,
            created_at: chrono::Utc::now(),
            completed_at: None,
        }
    }
}

/// 下载场景本地落盘的临时文件路径：<local>.part
pub fn part_path_for(local: &std::path::Path) -> PathBuf {
    let mut s = local.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_is_terminal() {
        assert!(Status::Completed.is_terminal());
        assert!(Status::Failed.is_terminal());
        assert!(Status::Cancelled.is_terminal());
        assert!(!Status::Pending.is_terminal());
        assert!(!Status::Transferring.is_terminal());
        assert!(!Status::Retrying.is_terminal());
        assert!(!Status::Connecting.is_terminal());
    }

    #[test]
    fn test_new_state_defaults() {
        let s = TransferState::new(
            Direction::Download,
            "sess",
            "env",
            "/tmp/a.hprof",
            PathBuf::from("/local/a.hprof"),
            false,
        );
        assert_eq!(s.status, Status::Pending);
        assert_eq!(s.attempt, 0);
        assert_eq!(s.transferred_bytes, 0);
        assert!(!s.cleanup_remote_on_success);
        assert!(s.error.is_none());
        assert!(uuid::Uuid::parse_str(&s.id).is_ok());
    }

    #[test]
    fn test_part_path_appends_suffix() {
        assert_eq!(
            part_path_for(std::path::Path::new("/x/a.hprof")),
            PathBuf::from("/x/a.hprof.part")
        );
    }

    #[test]
    fn test_direction_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&Direction::Download).unwrap(),
            "\"download\""
        );
        assert_eq!(
            serde_json::to_string(&Status::Retrying).unwrap(),
            "\"retrying\""
        );
    }
}
```

`src-tauri/src/transfer/mod.rs`（初始骨架，Task 2 扩充）：

```rust
pub mod state;
pub mod worker;

pub use state::{Direction, Status, TransferState};
```

（worker.rs 本 task 先建空文件：`// Task 2 实现`）

- [ ] **Step 2: lib.rs 挂模块**

`src-tauri/src/lib.rs` 第 8 行 `mod tools;` 后加：

```rust
mod transfer;
```

- [ ] **Step 3: pool.rs 的 build_transport/fetch_environment 改 pub（含测试辅助）**

`src-tauri/src/exec/pool.rs`：

```rust
// 原 136 行 fn build_transport( → pub fn build_transport(
pub fn build_transport(
    environment_id: &str,
    env: &EnvironmentInfo,
) -> Result<super::ssh::SshTransport, PoolError> {

// 原 161 行 async fn fetch_environment( → pub async fn fetch_environment(
pub async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::state`
Expected: 4 个测试 PASS

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/transfer src-tauri/src/lib.rs src-tauri/src/exec/pool.rs
git commit -m "feat: transfer state types and skeleton"
```

---

### Task 2: TransferManager 状态注册表

**Files:**
- Modify: `src-tauri/src/transfer/mod.rs`
- Test: `src-tauri/src/transfer/mod.rs` 内嵌 tests

- [ ] **Step 1: 写失败测试（mod.rs 追加 tests，先只写注册表行为）**

`src-tauri/src/transfer/mod.rs` 完整替换为：

```rust
pub mod state;
pub mod worker;

use crate::app::events::{AppEvent, EventBus};
use state::{Direction, Status, TransferState};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 终态记录保留上限（LRU 淘汰防泄漏）
const MAX_FINISHED_RECORDS: usize = 100;

pub struct TransferManager {
    db: sqlx::SqlitePool,
    bus: EventBus,
    transfers: Mutex<HashMap<String, ManagedTransfer>>,
}

struct ManagedTransfer {
    state: TransferState,
    cancel: Option<CancellationToken>,
}

impl TransferManager {
    pub fn new(db: sqlx::SqlitePool, bus: EventBus) -> Self {
        Self {
            db,
            bus,
            transfers: Mutex::new(HashMap::new()),
        }
    }

    /// 是否已有同 session + direction + remote_path 的活跃传输
    pub async fn find_active(
        &self,
        session_id: &str,
        direction: Direction,
        remote_path: &str,
    ) -> Option<TransferState> {
        let transfers = self.transfers.lock().await;
        transfers
            .values()
            .find(|t| {
                !t.state.status.is_terminal()
                    && t.state.session_id == session_id
                    && t.state.direction == direction
                    && t.state.remote_path == remote_path
            })
            .map(|t| t.state.clone())
    }

    /// 注册新传输（状态 Pending），返回 transfer_id。调用方随后 spawn worker 并
    /// 调 attach_worker 装上 cancel token。
    pub async fn register(&self, state: TransferState) -> String {
        let id = state.id.clone();
        let mut transfers = self.transfers.lock().await;
        transfers.insert(
            id.clone(),
            ManagedTransfer { state, cancel: None },
        );
        id
    }

    /// worker 启动后装上取消令牌
    pub async fn attach_worker(&self, transfer_id: &str, cancel: CancellationToken) {
        let mut transfers = self.transfers.lock().await;
        if let Some(t) = transfers.get_mut(transfer_id) {
            t.cancel = Some(cancel);
        }
    }

    pub async fn get(&self, transfer_id: &str) -> Option<TransferState> {
        self.transfers.lock().await.get(transfer_id).map(|t| t.state.clone())
    }

    /// 该会话全部传输（按创建时间倒序）
    pub async fn list_for_session(&self, session_id: &str) -> Vec<TransferState> {
        let transfers = self.transfers.lock().await;
        let mut list: Vec<TransferState> = transfers
            .values()
            .filter(|t| t.state.session_id == session_id)
            .map(|t| t.state.clone())
            .collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    /// worker 更新进度（非终态变更 + 进度数值）
    pub async fn update_progress(
        &self,
        transfer_id: &str,
        status: Status,
        transferred_bytes: u64,
        total_bytes: u64,
        speed_bps: u64,
        attempt: u32,
    ) {
        let event = {
            let mut transfers = self.transfers.lock().await;
            let Some(t) = transfers.get_mut(transfer_id) else { return };
            if t.state.status.is_terminal() {
                return; // 已终态，迟到进度丢弃
            }
            t.state.status = status;
            t.state.transferred_bytes = transferred_bytes;
            t.state.total_bytes = total_bytes;
            t.state.speed_bps = speed_bps;
            t.state.attempt = attempt;
            t.state.clone()
        };
        self.bus.emit(
            &event.session_id,
            AppEvent::TransferProgress {
                session_id: event.session_id.clone(),
                transfer_id: event.id.clone(),
                direction: event.direction,
                status: event.status,
                transferred_bytes: event.transferred_bytes,
                total_bytes: event.total_bytes,
                speed_bps: event.speed_bps,
                attempt: event.attempt,
            },
        );
        if event.status == Status::Retrying {
            tracing::debug!(transfer_id = %event.id, "transfer entering retry");
        }
    }

    /// worker 上报终态（completed/failed/cancelled）+ 发 TransferFinished 事件 + LRU 淘汰
    pub async fn finish(
        &self,
        transfer_id: &str,
        status: Status,
        error: Option<String>,
        transferred_bytes: u64,
        total_bytes: u64,
    ) {
        let event = {
            let mut transfers = self.transfers.lock().await;
            let Some(t) = transfers.get_mut(transfer_id) else { return };
            if t.state.status.is_terminal() {
                return;
            }
            t.state.status = status;
            t.state.error = error.clone();
            t.state.transferred_bytes = transferred_bytes;
            t.state.total_bytes = total_bytes;
            t.state.speed_bps = 0;
            t.state.completed_at = Some(chrono::Utc::now());
            t.cancel = None;
            t.state.clone()
        };
        self.bus.emit(
            &event.session_id,
            AppEvent::TransferFinished {
                session_id: event.session_id.clone(),
                transfer_id: event.id.clone(),
                direction: event.direction,
                status: event.status,
                transferred_bytes: event.transferred_bytes,
                total_bytes: event.total_bytes,
                error,
                local_path: if event.direction == Direction::Download {
                    Some(event.local_path.to_string_lossy().into_owned())
                } else {
                    None
                },
                remote_path: event.remote_path.clone(),
            },
        );
        self.evict_finished_locked().await;
    }

    /// 请求取消。返回 false = 不存在或已终态。
    pub async fn cancel(&self, transfer_id: &str) -> bool {
        let token = {
            let transfers = self.transfers.lock().await;
            match transfers.get(transfer_id) {
                Some(t) if !t.state.status.is_terminal() => {
                    t.cancel.as_ref().map(|c| c.clone())
                }
                _ => None,
            }
        };
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// 终态记录超上限时按 created_at 淘汰最旧的（仅终态可淘汰）
    async fn evict_finished_locked(&self) {
        let mut transfers = self.transfers.lock().await;
        let finished: Vec<(String, chrono::DateTime<chrono::Utc>)> = transfers
            .iter()
            .filter(|(_, t)| t.state.status.is_terminal())
            .map(|(id, t)| (id.clone(), t.state.created_at))
            .collect();
        if finished.len() > MAX_FINISHED_RECORDS {
            let mut finished = finished;
            finished.sort_by_key(|(_, created)| *created);
            let to_remove = finished.len() - MAX_FINISHED_RECORDS;
            for (id, _) in finished.into_iter().take(to_remove) {
                transfers.remove(&id);
                tracing::debug!(transfer_id = %id, "evicted finished transfer record");
            }
        }
    }

    pub fn db(&self) -> &sqlx::SqlitePool {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(direction: Direction, remote: &str, session: &str) -> TransferState {
        TransferState::new(
            direction,
            session,
            "env-1",
            remote,
            PathBuf::from("/local/a.hprof"),
            false,
        )
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let db = crate::infra::db::init(std::path::Path::new(":memory:").to_path_buf())
            .await
            .unwrap();
        // :memory: 每连接独立库，改用临时文件
        drop(db);
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let st = make_state(Direction::Download, "/tmp/a.hprof", "s1");
        let id = mgr.register(st).await;
        assert!(mgr.get(&id).await.is_some());
        assert!(mgr.get("nope").await.is_none());
    }

    #[tokio::test]
    async fn test_find_active_matches_active_only() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let id = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        assert!(mgr.find_active("s1", Direction::Download, "/tmp/a.hprof").await.is_some());
        // 不同 session / 不同路径不算
        assert!(mgr.find_active("s2", Direction::Download, "/tmp/a.hprof").await.is_none());
        assert!(mgr.find_active("s1", Direction::Download, "/tmp/b.hprof").await.is_none());
        // 终态后不算
        mgr.finish(&id, Status::Completed, None, 10, 10).await;
        assert!(mgr.find_active("s1", Direction::Download, "/tmp/a.hprof").await.is_none());
    }

    #[tokio::test]
    async fn test_finish_sets_terminal_and_emits() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let id = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        mgr.finish(&id, Status::Completed, None, 100, 100).await;
        let st = mgr.get(&id).await.unwrap();
        assert_eq!(st.status, Status::Completed);
        assert!(st.completed_at.is_some());
        // 重复 finish 幂等（第二次不覆盖）
        mgr.finish(&id, Status::Failed, Some("x".into()), 50, 100).await;
        assert_eq!(mgr.get(&id).await.unwrap().status, Status::Completed);
    }

    #[tokio::test]
    async fn test_update_progress_late_after_terminal_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let id = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        mgr.finish(&id, Status::Cancelled, None, 0, 100).await;
        mgr.update_progress(&id, Status::Transferring, 50, 100, 10, 1).await;
        assert_eq!(mgr.get(&id).await.unwrap().status, Status::Cancelled);
    }

    #[tokio::test]
    async fn test_cancel_requires_active_and_token() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let id = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        // 未 attach worker：无 token，取消失败
        assert!(!mgr.cancel(&id).await);
        let token = CancellationToken::new();
        mgr.attach_worker(&id, token.clone()).await;
        assert!(mgr.cancel(&id).await);
        assert!(token.is_cancelled());
        // 终态后再取消失败
        mgr.finish(&id, Status::Cancelled, None, 0, 100).await;
        assert!(!mgr.cancel(&id).await);
    }

    #[tokio::test]
    async fn test_list_for_session_orders_desc_and_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let _a = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _b = mgr
            .register(make_state(Direction::Download, "/tmp/b.hprof", "s1"))
            .await;
        let _c = mgr
            .register(make_state(Direction::Download, "/tmp/c.hprof", "s2"))
            .await;
        let list = mgr.list_for_session("s1").await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].remote_path, "/tmp/b.hprof");
    }

    #[tokio::test]
    async fn test_evict_finished_keeps_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let mut ids = Vec::new();
        for i in 0..(MAX_FINISHED_RECORDS + 10) {
            let id = mgr
                .register(make_state(Direction::Download, &format!("/tmp/{i}.hprof"), "s1"))
                .await;
            mgr.finish(&id, Status::Completed, None, 1, 1).await;
            ids.push(id);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let remaining = mgr.list_for_session("s1").await;
        assert_eq!(remaining.len(), MAX_FINISHED_RECORDS);
        // 最旧的被淘汰
        assert!(mgr.get(&ids[0]).await.is_none());
        assert!(mgr.get(ids.last().unwrap()).await.is_some());
    }
}
```

注意：`AppEvent::TransferProgress` / `TransferFinished` 变体本 task 还不存在——**本 task 同时要加事件定义**（Task 6 只做前端消费，Rust 侧定义放这里避免编译不过）。

- [ ] **Step 2: events.rs 加两个事件变体**

`src-tauri/src/app/events.rs` 的 `AppEvent` enum 中 `ProvisionProgress` 变体后加：

```rust
    TransferProgress {
        session_id: String,
        transfer_id: String,
        direction: crate::transfer::state::Direction,
        status: crate::transfer::state::Status,
        transferred_bytes: u64,
        total_bytes: u64,
        speed_bps: u64,
        attempt: u32,
    },
    TransferFinished {
        session_id: String,
        transfer_id: String,
        direction: crate::transfer::state::Direction,
        status: crate::transfer::state::Status,
        transferred_bytes: u64,
        total_bytes: u64,
        error: Option<String>,
        local_path: Option<String>,
        remote_path: String,
    },
```

events.rs 顶部需要 `pub use crate::transfer::state::{Direction, Status};` 不需要——用全路径即可。同时给 events.rs tests 追加序列化测试：

```rust
    #[test]
    fn test_transfer_progress_serialization() {
        let event = AppEvent::TransferProgress {
            session_id: "s1".to_string(),
            transfer_id: "t1".to_string(),
            direction: crate::transfer::state::Direction::Download,
            status: crate::transfer::state::Status::Transferring,
            transferred_bytes: 100,
            total_bytes: 200,
            speed_bps: 10,
            attempt: 1,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("transfer_progress"));
        assert!(json.contains("transferring"));
        assert!(json.contains("download"));
    }

    #[test]
    fn test_transfer_finished_serialization() {
        let event = AppEvent::TransferFinished {
            session_id: "s1".to_string(),
            transfer_id: "t1".to_string(),
            direction: crate::transfer::state::Direction::Upload,
            status: crate::transfer::state::Status::Failed,
            transferred_bytes: 1,
            total_bytes: 2,
            error: Some("boom".to_string()),
            local_path: None,
            remote_path: "/tmp/x".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("transfer_finished"));
        assert!(json.contains("upload"));
        assert!(json.contains("failed"));
        assert!(json.contains("boom"));
    }
```

- [ ] **Step 3: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::`
Expected: state 4 个 + manager 7 个 + events 2 个全 PASS（evict 测试约 1s）

- [ ] **Step 4: 提交**

```bash
git add src-tauri/src/transfer src-tauri/src/app/events.rs
git commit -m "feat: TransferManager registry with progress/finish/cancel and events"
```

---

### Task 3: SshTransport 下载续传 + 进度回调

**Files:**
- Modify: `src-tauri/src/exec/channel.rs:23-27`（download 签名加参数）
- Modify: `src-tauri/src/exec/ssh.rs:401-446`（实现 offset + 进度）
- Test: 两文件内嵌 tests

- [ ] **Step 1: 扩 ExecChannel trait download 签名**

`src-tauri/src/exec/channel.rs` 的 download 默认实现改为：

```rust
    /// 从远端下载文件到本地路径（SFTP 或等价实现）。offset 为续传起点
    /// （0 = 从头）。progress 每 1s 节流回调 (transferred, total)。
    async fn download(
        &self,
        _remote_path: &str,
        _local: &std::path::Path,
        _offset: u64,
        _progress: &dyn Fn(u64, u64),
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("download not implemented for this channel".into())
    }
```

同步更新 channel.rs tests 里 `RecordingDownloadChannel`/`DefaultDownloadChannel` 的 download 实现签名（`_offset: u64, _progress: &dyn Fn(u64, u64)`，调用侧补 `0, &|_, _| {}`）。

- [ ] **Step 2: SshTransport::download 实现 offset + 进度**

`src-tauri/src/exec/ssh.rs` 的 download 替换为：

```rust
    async fn download(
        &self,
        remote_path: &str,
        local: &std::path::Path,
        offset: u64,
        progress: &dyn Fn(u64, u64),
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

        let mut conn = self.conn.lock().await;
        let Some(c) = conn.as_mut() else {
            return Err("ssh not connected (call connect first)".into());
        };

        let channel = c.handle.channel_open_session().await?;
        // 对齐 upload：慢速链路传 GB 级 dump 时 10s 默认超时不够
        let sftp_cfg = russh_sftp::client::Config {
            request_timeout_secs: 600,
            max_concurrent_writes: 16,
            ..Default::default()
        };
        let sftp = russh_sftp::client::SftpSession::new_with_config(channel.into_stream(), sftp_cfg).await?;

        let mut remote_file = sftp.open(remote_path).await?;
        let total = remote_file.metadata().await.map(|m| m.len.unwrap_or(0)).unwrap_or(0);
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 续传：append 模式打开本地；offset=0 时 truncate
        let mut local_file = if offset > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(local)
                .await?
        } else {
            tokio::fs::File::create(local).await?
        };
        if offset > 0 {
            remote_file.seek(std::io::SeekFrom::Start(offset)).await?;
        }

        let mut buf = vec![0u8; 32 * 1024];
        let mut transferred: u64 = offset;
        let mut last_report = std::time::Instant::now();
        let mut last_bytes = transferred;
        loop {
            let n = remote_file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            local_file.write_all(&buf[..n]).await?;
            transferred += n as u64;
            // 1s 节流进度回调（回调只做同步轻量更新）
            if last_report.elapsed() >= std::time::Duration::from_secs(1) {
                let elapsed = last_report.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 {
                    ((transferred - last_bytes) as f64 / elapsed) as u64
                } else {
                    0
                };
                progress(transferred, speed);
                last_report = std::time::Instant::now();
                last_bytes = transferred;
            }
        }
        local_file.flush().await?;
        sftp.close().await?;

        tracing::info!(
            env_id = %self.env_id,
            remote_path,
            local = %local.display(),
            offset,
            bytes = transferred - offset,
            "sftp download complete"
        );
        Ok(())
    }
```

注意：

- russh-sftp 的 `File` 实现了 `AsyncSeek`（`remote_file.seek(...)`），metadata 的 `len` 字段是 `Option<u64>`。如果 `remote_file.metadata()` 拿不到大小，total 传 0（前端按 0 显示"未知大小"，不阻塞传输）。
- progress 回调签名是 `(transferred_bytes, speed_bps)`——total 由调用方（worker）自己持有，避免闭包再捕获。
- 速率计算在最后一次 report 后 `last_bytes` 会重置，循环结束时不补报——worker 的 ticker 会兜底读取 transferred_cell，无丢失。

- [ ] **Step 3: 更新 heap_dump.rs / channel.rs tests 里 download 的调用点**

`src-tauri/src/tools/builtin/jvm/heap_dump.rs:166` 的调用改为（本 task 临时保持同步下载可用，Task 5 会整体删掉这段）：

```rust
        let download_result = tokio::time::timeout(
            std::time::Duration::from_secs(download_timeout),
            channel.download(&remote_path, &local_path, 0, &|_, _| {}),
        )
        .await;
```

heap_dump.rs tests 的 DumpChannel::download 签名同步加 `_offset: u64, _progress: &dyn Fn(u64, u64)`。

- [ ] **Step 4: 跑全量测试确认无回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全 PASS（含 heap_dump 现有 5 测试）

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/exec src-tauri/src/tools/builtin/jvm/heap_dump.rs
git commit -m "feat: ssh download with resume offset and progress callback"
```

---

### Task 4: transfer worker（重试 + 断点续传执行循环）

**Files:**
- Create: `src-tauri/src/transfer/worker.rs`
- Modify: `src-tauri/src/transfer/mod.rs`（加 spawn_download/spawn_upload 入口）
- Test: `src-tauri/src/transfer/worker.rs` 内嵌 tests（mock ExecChannel 风格）

- [ ] **Step 1: 写 worker.rs（实现 + tests 一起，TDD 粒度上测试先行写在本文件底部）**

`src-tauri/src/transfer/worker.rs`：

```rust
use super::state::{Direction, Status, TransferState};
use super::TransferManager;
use crate::exec::channel::ExecChannel;
use crate::exec::pool::{build_transport, fetch_environment};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_ATTEMPTS: u32 = 5;
const MAX_TOTAL_SECS: u64 = 7200; // 2 小时重试预算
const BACKOFF_SECS: [u64; 5] = [5, 15, 45, 120, 360];

/// 建一条专用连接（不走 ExecChannelPool）
async fn dedicated_channel(
    db: &sqlx::SqlitePool,
    env_id: &str,
) -> Result<Arc<dyn ExecChannel>, String> {
    let env = fetch_environment(db, env_id).await.map_err(|e| e.to_string())?;
    let transport = build_transport(env_id, &env).map_err(|e| e.to_string())?;
    transport.connect().await.map_err(|e| e.to_string())?;
    Ok(Arc::new(transport))
}

/// stat 远端文件大小；None = 文件不存在或 stat 失败
async fn stat_remote(channel: &Arc<dyn ExecChannel>, remote_path: &str) -> Option<u64> {
    let cmd = format!("stat -c %s {}", crate::exec::ssh::shell_quote_single(remote_path));
    match channel.run(&cmd).await {
        Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().ok(),
        _ => None,
    }
}

/// 下载 worker：断点续传 + 重试。进度经 manager.update_progress 上报。
pub async fn run_download(mgr: Arc<TransferManager>, state: TransferState, cancel: CancellationToken) {
    let id = state.id.clone();
    let env_id = state.env_id.clone();
    let remote_path = state.remote_path.clone();
    let local = state.local_path.clone();
    let cleanup = state.cleanup_remote_on_success;
    let part = super::state::part_path_for(&local);
    let session_id = state.session_id.clone();
    let started = std::time::Instant::now();
    let mut attempt: u32 = 0;

    tracing::info!(transfer_id = %id, session_id = %session_id, env_id = %env_id, remote_path = %remote_path, "transfer worker: download starting");

    loop {
        if cancel.is_cancelled() {
            mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
            return;
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS || started.elapsed().as_secs() > MAX_TOTAL_SECS {
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!(
                    "传输失败：重试次数用尽（{remote_path}）。远端文件保留，可重新调用 file_download 断点续传。"
                )),
                0, 0,
            ).await;
            return;
        }

        // backoff（首次不等待）
        if attempt > 1 {
            let wait = BACKOFF_SECS[((attempt - 2) as usize).min(BACKOFF_SECS.len() - 1)];
            mgr.update_progress(&id, Status::Retrying, 0, 0, 0, attempt).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                _ = cancel.cancelled() => {
                    mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
                    return;
                }
            }
        }

        let channel = match dedicated_channel(mgr.db(), &env_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: connect failed");
                continue;
            }
        };
        mgr.update_progress(&id, Status::Connecting, 0, 0, 0, attempt).await;

        let Some(total) = stat_remote(&channel, &remote_path).await else {
            channel.disconnect().await;
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!("远端文件不存在或无法读取: {remote_path}")),
                0, 0,
            ).await;
            return; // 终态不重试
        };

        // 断点续传：本地 .part 已有字节数为偏移
        let offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0).min(total);
        let transferred_cell = Arc::new(std::sync::Mutex::new(offset));
        let speed_cell = Arc::new(std::sync::Mutex::new(0u64));

        // 实时进度 ticker：1s 轮询 transferred_cell 上报（download 回调是同步的，
        // 不能在里面 await manager，所以用 ticker task 转发）
        let mgr_t = mgr.clone();
        let id_t = id.clone();
        let tc = transferred_cell.clone();
        let sc = speed_cell.clone();
        let ticker = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let t = *tc.lock().unwrap();
                let s = *sc.lock().unwrap();
                mgr_t.update_progress(&id_t, Status::Transferring, t, total, s, attempt).await;
            }
        });

        let (tc2, sc2) = (transferred_cell.clone(), speed_cell.clone());
        let result = tokio::select! {
            r = channel.download(&remote_path, &part, offset, &move |t, s| {
                *tc2.lock().unwrap() = t;
                *sc2.lock().unwrap() = s;
            }) => r,
            _ = cancel.cancelled() => {
                ticker.abort();
                channel.disconnect().await;
                mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
                return;
            }
        };
        ticker.abort();

        let transferred_now = *transferred_cell.lock().unwrap();
        let speed_now = *speed_cell.lock().unwrap();
        tracing::debug!(transfer_id = %id, transferred_now, speed_now, "transfer: download attempt ended");
        channel.disconnect().await;

        match result {
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: download interrupted");
                continue; // 重试（.part 保留，下次续传）
            }
            Ok(()) => {
                // 大小校验
                let local_size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if local_size != total {
                    tracing::warn!(transfer_id = %id, local_size, total, "transfer: size mismatch, restarting");
                    let _ = std::fs::remove_file(&part);
                    continue; // 损坏：删 .part 从头重试
                }
                // rename 成正式文件
                if let Err(e) = std::fs::rename(&part, &local) {
                    mgr.finish(&id, Status::Failed, Some(format!("本地文件落盘失败: {e}")), local_size, total).await;
                    return;
                }
                // heap_dump 场景：下载成功后清理远端（失败仅告警）
                if cleanup {
                    let rm = channel_cmd_after_disconnect(&mgr, &env_id, &remote_path).await;
                    if let Err(e) = rm {
                        tracing::warn!(transfer_id = %id, error = %e, "transfer: remote cleanup failed (kept)");
                    }
                }
                mgr.finish(&id, Status::Completed, None, total, total).await;
                return;
            }
        }
    }
}

/// 清理远端文件：再开一条短连接执行 rm（原连接已断开）
async fn channel_cmd_after_disconnect(
    mgr: &TransferManager,
    env_id: &str,
    remote_path: &str,
) -> Result<(), String> {
    let channel = dedicated_channel(mgr.db(), env_id).await?;
    let cmd = format!("rm -f {}", crate::exec::ssh::shell_quote_single(remote_path));
    let out = channel.run(&cmd).await.map_err(|e| e.to_string())?;
    channel.disconnect().await;
    if out.exit_code == 0 {
        Ok(())
    } else {
        Err(format!("rm exit {}: {}", out.exit_code, out.stderr))
    }
}

/// 上传 worker：整体重传 + 大小校验
pub async fn run_upload(mgr: Arc<TransferManager>, state: TransferState, cancel: CancellationToken) {
    let id = state.id.clone();
    let env_id = state.env_id.clone();
    let remote_path = state.remote_path.clone();
    let local = state.local_path.clone();
    let session_id = state.session_id.clone();
    let started = std::time::Instant::now();
    let mut attempt: u32 = 0;

    let Ok(meta) = std::fs::metadata(&local) else {
        mgr.finish(&id, Status::Failed, Some(format!("本地文件不存在: {}", local.display())), 0, 0).await;
        return;
    };
    let total = meta.len();

    tracing::info!(transfer_id = %id, session_id = %session_id, env_id = %env_id, remote_path = %remote_path, total, "transfer worker: upload starting");

    loop {
        if cancel.is_cancelled() {
            mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
            return;
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS || started.elapsed().as_secs() > MAX_TOTAL_SECS {
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!("上传失败：重试次数用尽（远端 {remote_path} 可能有残留半成品，重新上传会覆盖）。")),
                0, total,
            ).await;
            return;
        }
        if attempt > 1 {
            let wait = BACKOFF_SECS[((attempt - 2) as usize).min(BACKOFF_SECS.len() - 1)];
            mgr.update_progress(&id, Status::Retrying, 0, total, 0, attempt).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                _ = cancel.cancelled() => {
                    mgr.finish(&id, Status::Cancelled, None, 0, total).await;
                    return;
                }
            }
        }

        let channel = match dedicated_channel(mgr.db(), &env_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: connect failed");
                continue;
            }
        };
        mgr.update_progress(&id, Status::Connecting, 0, total, 0, attempt).await;

        let result = tokio::select! {
            r = channel.upload(&local, &remote_path) => r,
            _ = cancel.cancelled() => {
                channel.disconnect().await;
                mgr.finish(&id, Status::Cancelled, None, 0, total).await;
                return;
            }
        };
        channel.disconnect().await;

        match result {
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: upload interrupted");
                continue;
            }
            Ok(()) => {
                // 远端大小校验：再开短连接 stat
                let check = dedicated_channel(mgr.db(), &env_id).await.and_then(|c| async {
                    let size = stat_remote(&c, &remote_path).await;
                    c.disconnect().await;
                    size.ok_or_else(|| "stat failed".to_string())
                });
                match check {
                    Ok(Some(remote_size)) if remote_size == total => {
                        mgr.finish(&id, Status::Completed, None, total, total).await;
                        return;
                    }
                    Ok(_) => {
                        tracing::warn!(transfer_id = %id, "transfer: remote size mismatch after upload, retrying");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(transfer_id = %id, error = %e, "transfer: remote stat failed after upload, retrying");
                        continue;
                    }
                }
            }
        }
    }
}
```

实现说明：

1. 进度回调是同步闭包（`&dyn Fn(u64, u64)`），进度值经 `Arc<Mutex<u64>>` cell 传递；worker 用 1s ticker task 轮询 cell 调 `mgr.update_progress`（上面的主代码已含 ticker，download 前 spawn、结束/取消后 abort）。
2. upload 没有 ticker（upload trait 方法无进度回调）——上传进度只体现在状态（Connecting/Retrying），终态给总量。后续若需要，可给 upload 加同样的回调，本计划不做（YAGNI）。
3. `ticker` 闭包捕获的 `total`/`attempt` 是 Copy 类型，直接捕获值。

- [ ] **Step 2: worker.rs 纯逻辑测试（mock channel 只测 stat_remote 与常量）**

worker 的专用连接在函数内自建（`dedicated_channel`），无法用 mock channel 注入跑全流程——集成行为靠 Task 5/6 的工具层测试覆盖（注册表与启动逻辑），worker 全流程靠人工验收。worker.rs tests 只测纯函数与 stat_remote：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_sequence() {
        assert_eq!(BACKOFF_SECS, [5, 15, 45, 120, 360]);
        assert_eq!(MAX_ATTEMPTS, 5);
    }

    #[test]
    fn test_offset_clamped_to_total() {
        // offset = min(.part 大小, total)
        let offset = 300u64.min(200);
        assert_eq!(offset, 200);
    }

    #[tokio::test]
    async fn test_stat_remote_quotes_path() {
        // mock：记录收到的命令，验证路径被单引号包裹
        use crate::exec::channel::{ExecChannel, ExecOutput};
        use async_trait::async_trait;
        struct StatChan(tokio::sync::Mutex<Vec<String>>);
        #[async_trait]
        impl ExecChannel for StatChan {
            async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
                self.0.lock().await.push(cmd.to_string());
                Ok(ExecOutput { stdout: "12345\n".into(), stderr: String::new(), exit_code: 0 })
            }
            async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
            async fn disconnect(&self) {}
            async fn is_alive(&self) -> bool { true }
        }
        let ch = Arc::new(StatChan(tokio::sync::Mutex::new(Vec::new())));
        let dyn_ch: Arc<dyn ExecChannel> = ch.clone();
        let size = stat_remote(&dyn_ch, "/tmp/a b.hprof").await;
        assert_eq!(size, Some(12345));
        let calls = ch.0.lock().await;
        assert!(calls[0].contains("'/tmp/a b.hprof'"), "cmd: {}", calls[0]);
    }

    #[tokio::test]
    async fn test_stat_remote_missing_returns_none() {
        use crate::exec::channel::{ExecChannel, ExecOutput};
        use async_trait::async_trait;
        struct NoFileChan;
        #[async_trait]
        impl ExecChannel for NoFileChan {
            async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
                Ok(ExecOutput { stdout: String::new(), stderr: "No such file".into(), exit_code: 1 })
            }
            async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
            async fn disconnect(&self) {}
            async fn is_alive(&self) -> bool { true }
        }
        let ch: Arc<dyn ExecChannel> = Arc::new(NoFileChan);
        assert_eq!(stat_remote(&ch, "/tmp/gone").await, None);
    }
}
```

- [ ] **Step 3: mod.rs 加 start 入口**

`src-tauri/src/transfer/mod.rs` 追加（impl TransferManager 内）：

```rust
    /// 启动后台传输任务（spawn worker）。返回 transfer_id。
    pub async fn start(
        self: &Arc<Self>,
        state: TransferState,
    ) -> String {
        // 去重：已有同 session+direction+remote_path 活跃传输 → 返回已有 id
        if let Some(existing) = self
            .find_active(&state.session_id, state.direction, &state.remote_path)
            .await
        {
            return existing.id;
        }
        let id = self.register(state.clone()).await;
        let cancel = CancellationToken::new();
        self.attach_worker(&id, cancel.clone()).await;
        let mgr = self.clone();
        match state.direction {
            Direction::Download => {
                tokio::spawn(async move {
                    worker::run_download(mgr, state, cancel).await;
                });
            }
            Direction::Upload => {
                tokio::spawn(async move {
                    worker::run_upload(mgr, state, cancel).await;
                });
            }
        }
        id
    }
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::`
Expected: 全 PASS

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/transfer
git commit -m "feat: transfer worker with retry, resume and progress ticker"
```

---

### Task 5: file_transfer 四个 Agent 工具

**Files:**
- Create: `src-tauri/src/tools/builtin/file_transfer.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`（加 `pub mod file_transfer;`）
- Modify: `src-tauri/src/lib.rs`（创建 TransferManager + 注册工具）
- Test: `src-tauri/src/tools/builtin/file_transfer.rs` 内嵌 tests

- [ ] **Step 1: 写 file_transfer.rs（handler + ToolDef + tests）**

```rust
use crate::tools::builtin::run_command::artifact_dir_for;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use crate::transfer::state::{Direction, Status};
use crate::transfer::TransferManager;
use async_trait::async_trait;
use std::sync::Arc;

fn err_invalid(msg: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": "invalid_params", "message": msg }),
        raw_stdout: None,
    }
}

fn err_env_not_found(environment: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({
            "error": "environment_not_found",
            "message": format!(
                "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
            ),
        }),
        raw_stdout: None,
    }
}

/// 远端路径校验：必须以 / 开头
fn validate_remote_path(p: &str) -> Result<(), String> {
    if !p.starts_with('/') {
        Err(format!("remote_path 必须是绝对路径（以 / 开头）: {p}"))
    } else {
        Ok(())
    }
}

/// 远端 basename 校验（防穿越）：非空且不是 . / ..
fn remote_basename(p: &str) -> Result<String, String> {
    let name = p.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        Err(format!("remote_path 文件名非法: {p}"))
    } else {
        Ok(name.to_string())
    }
}

pub struct FileTransferTools {
    pub core: Arc<TransferManager>,
    pub artifacts_dir: std::path::PathBuf,
}

impl FileTransferTools {
    async fn file_download(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: environment");
        };
        let Some(remote_path) = args.get("remote_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: remote_path");
        };
        if let Err(e) = validate_remote_path(remote_path) {
            return err_invalid(&e);
        }
        let Ok(file_name) = remote_basename(remote_path) else {
            return err_invalid(&format!("remote_path 文件名非法: {remote_path}"));
        };
        let env = match crate::app::environments::find_by_name(self.core.db(), environment).await {
            Ok(Some(env)) => env,
            Ok(None) => return err_env_not_found(environment),
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "file_download: env lookup failed");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({ "error": "lookup_failed", "message": format!("查询环境失败: {e}") }),
                    raw_stdout: None,
                };
            }
        };

        // 去重提示（start 内部也会去重，这里先查一次给 Agent 明确信号）
        if let Some(existing) = self
            .core
            .find_active(&ctx.session_id, Direction::Download, remote_path)
            .await
        {
            return ToolOutput {
                success: false,
                data: serde_json::json!({
                    "error": "duplicate_transfer",
                    "message": "该文件已有进行中的下载任务。",
                    "transfer_id": existing.id,
                    "note": "请轮询 transfer_status(transfer_id) 获取结果。",
                }),
                raw_stdout: None,
            };
        }

        let session_dir = artifact_dir_for(&self.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(&file_name);

        let state = crate::transfer::state::TransferState::new(
            Direction::Download,
            &ctx.session_id,
            &env.id,
            remote_path,
            local_path.clone(),
            false, // 独立下载不清理远端
        );
        let transfer_id = self.core.start(state).await;

        tracing::info!(session_id = %ctx.session_id, transfer_id = %transfer_id, env_id = %env.id, remote_path, "file_download: background transfer started");

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "status": "pending",
                "local_path": local_path.to_string_lossy(),
                "note": "传输已在后台启动，请轮询 transfer_status(transfer_id) 获取进度/结果。",
            }),
            raw_stdout: None,
        }
    }

    async fn file_upload(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: environment");
        };
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: local_path");
        };
        let Some(remote_path) = args.get("remote_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: remote_path");
        };
        let local = std::path::PathBuf::from(local_path);
        if !local.is_absolute() {
            return err_invalid(&format!("local_path 必须是绝对路径: {local_path}"));
        }
        if let Err(e) = validate_remote_path(remote_path) {
            return err_invalid(&e);
        }
        if !local.exists() {
            return err_invalid(&format!("本地文件不存在: {local_path}"));
        }
        let env = match crate::app::environments::find_by_name(self.core.db(), environment).await {
            Ok(Some(env)) => env,
            Ok(None) => return err_env_not_found(environment),
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "file_upload: env lookup failed");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({ "error": "lookup_failed", "message": format!("查询环境失败: {e}") }),
                    raw_stdout: None,
                };
            }
        };
        if let Some(existing) = self
            .core
            .find_active(&ctx.session_id, Direction::Upload, remote_path)
            .await
        {
            return ToolOutput {
                success: false,
                data: serde_json::json!({
                    "error": "duplicate_transfer",
                    "message": "该文件已有进行中的上传任务。",
                    "transfer_id": existing.id,
                    "note": "请轮询 transfer_status(transfer_id) 获取结果。",
                }),
                raw_stdout: None,
            };
        }

        let state = crate::transfer::state::TransferState::new(
            Direction::Upload,
            &ctx.session_id,
            &env.id,
            remote_path,
            local.clone(),
            false,
        );
        let transfer_id = self.core.start(state).await;

        tracing::info!(session_id = %ctx.session_id, transfer_id = %transfer_id, env_id = %env.id, local_path, remote_path, "file_upload: background transfer started");

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "status": "pending",
                "note": "上传已在后台启动，请轮询 transfer_status(transfer_id) 获取进度/结果。",
            }),
            raw_stdout: None,
        }
    }

    async fn transfer_status(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let transfer_id = args.get("transfer_id").and_then(|v| v.as_str());
        let state_to_json = |s: &crate::transfer::state::TransferState| {
            let mut j = serde_json::json!({
                "transfer_id": s.id,
                "direction": s.direction,
                "status": s.status,
                "transferred_bytes": s.transferred_bytes,
                "total_bytes": s.total_bytes,
                "speed_bps": s.speed_bps,
                "attempt": s.attempt,
                "error": s.error,
                "local_path": s.local_path.to_string_lossy(),
                "remote_path": s.remote_path,
            });
            match s.status {
                Status::Completed => {
                    j["note"] = serde_json::json!("传输完成。下载场景请把 local_path 告知用户。");
                }
                Status::Failed => {
                    j["note"] = serde_json::json!("传输失败。远端文件保留（下载场景），可用 file_download 重试（断点续传）。");
                }
                Status::Retrying => {
                    j["note"] = serde_json::json!("传输中断，正在自动重试。请稍后再轮询。");
                }
                _ => {}
            }
            j
        };
        match transfer_id {
            Some(id) => match self.core.get(id).await {
                Some(s) => ToolOutput {
                    success: true,
                    data: state_to_json(&s),
                    raw_stdout: None,
                },
                None => err_invalid(&format!("transfer_id 不存在: {id}")),
            },
            None => {
                let list = self.core.list_for_session(&ctx.session_id).await;
                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "transfers": list.iter().map(state_to_json).collect::<Vec<_>>(),
                    }),
                    raw_stdout: None,
                }
            }
        }
    }

    async fn transfer_cancel(&self, args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(transfer_id) = args.get("transfer_id").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: transfer_id");
        };
        let ok = self.core.cancel(transfer_id).await;
        ToolOutput {
            success: ok,
            data: if ok {
                serde_json::json!({ "cancelled": true, "transfer_id": transfer_id })
            } else {
                serde_json::json!({
                    "cancelled": false,
                    "transfer_id": transfer_id,
                    "message": "任务不存在或已结束",
                })
            },
            raw_stdout: None,
        }
    }
}
```

每个工具一个薄 handler struct（直接调 FileTransferTools 的对应方法，不用 args 注入分发）：

```rust
pub struct FileDownloadHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for FileDownloadHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.file_download(args, ctx).await
    }
}
pub struct FileUploadHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for FileUploadHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.file_upload(args, ctx).await
    }
}
pub struct TransferStatusHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for TransferStatusHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.transfer_status(args, ctx).await
    }
}
pub struct TransferCancelHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for TransferCancelHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.transfer_cancel(args, ctx).await
    }
}
```

ToolDef 注册函数：

```rust
pub fn file_transfer_tool_defs(
    mgr: Arc<TransferManager>,
    artifacts_dir: std::path::PathBuf,
) -> Vec<ToolDef> {
    let tools = Arc::new(FileTransferTools { core: mgr, artifacts_dir });
    vec![
        ToolDef {
            name: "file_download".to_string(),
            description: "从远端环境下载文件到本地（后台异步传输，支持断点续传）。启动后立即返回 transfer_id，必须轮询 transfer_status(transfer_id) 至终态。下载完成后文件在本机会话 artifacts 目录（返回 local_path），请把路径告知用户。远端文件不会被删除。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                    "remote_path": { "type": "string", "description": "远端文件绝对路径" }
                },
                "required": ["environment", "remote_path"]
            }),
            risk_level: RiskLevel::Low,
            needs_channel: false,
            handler: Arc::new(FileDownloadHandler(tools.clone())),
        },
        ToolDef {
            name: "file_upload".to_string(),
            description: "上传本地文件到远端环境（后台异步传输）。⚠ 上传任意本地文件需用户确认。启动后立即返回 transfer_id，必须轮询 transfer_status(transfer_id) 至终态。上传失败重试会整体重传覆盖远端半成品。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "environment": { "type": "string", "description": "目标环境名称" },
                    "local_path": { "type": "string", "description": "本地文件绝对路径" },
                    "remote_path": { "type": "string", "description": "远端目标绝对路径" }
                },
                "required": ["environment", "local_path", "remote_path"]
            }),
            risk_level: RiskLevel::High,
            needs_channel: false,
            handler: Arc::new(FileUploadHandler(tools.clone())),
        },
        ToolDef {
            name: "transfer_status".to_string(),
            description: "查询后台传输任务状态（file_download/file_upload/jvm_heap_dump 的拉回均产生传输任务）。传 transfer_id 查单条；不传则列出本会话全部传输。终态：completed（下载场景带 local_path 可交付用户）/ failed（远端文件保留，可重试）/ cancelled；retrying 表示自动重试中，请稍后再查。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "transfer_id": { "type": "string", "description": "传输任务 ID（可选，缺省列出全部）" }
                }
            }),
            risk_level: RiskLevel::ReadOnly,
            needs_channel: false,
            handler: Arc::new(TransferStatusHandler(tools.clone())),
        },
        ToolDef {
            name: "transfer_cancel".to_string(),
            description: "取消进行中的后台传输任务。已下载的部分保留（下次 file_download 同文件可断点续传）。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "transfer_id": { "type": "string", "description": "要取消的传输任务 ID" }
                },
                "required": ["transfer_id"]
            }),
            risk_level: RiskLevel::ReadOnly,
            needs_channel: false,
            handler: Arc::new(TransferCancelHandler(tools)),
        },
    ]
}
```

tests（同文件底部）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, Arc<FileTransferTools>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(
            &db, "prod", "10.0.0.1", 22, "root", "password", None, None,
        ).await.unwrap();
        let mgr = Arc::new(TransferManager::new(db, crate::app::events::EventBus::disabled()));
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        (tmp, Arc::new(FileTransferTools { core: mgr, artifacts_dir: artifacts }))
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    #[tokio::test]
    async fn test_download_rejects_relative_remote_path() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "remote_path": "tmp/a.hprof"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_rejects_bad_basename() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        for p in ["/tmp/", "/tmp/..", "/tmp/."] {
            let out = h.execute(
                serde_json::json!({"environment": "prod", "remote_path": p}),
                &ctx(),
            ).await;
            assert!(!out.success, "path {p} must be rejected");
            assert_eq!(out.data["error"], "invalid_params");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upload_requires_absolute_local_path() {
        let (tmp, tools) = setup().await;
        let h = FileUploadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "local_path": "relative.jar", "remote_path": "/tmp/x.jar"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upload_rejects_missing_local_file() {
        let (tmp, tools) = setup().await;
        let h = FileUploadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "local_path": "Z:/no/such/file.jar", "remote_path": "/tmp/x.jar"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert!(out.data["message"].as_str().unwrap().contains("不存在"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "ghost", "remote_path": "/tmp/a.hprof"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_status_unknown_transfer_id() {
        let (tmp, tools) = setup().await;
        let h = TransferStatusHandler(tools);
        let out = h.execute(
            serde_json::json!({"transfer_id": "nope"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_status_empty_lists_session_transfers() {
        let (tmp, tools) = setup().await;
        let h = TransferStatusHandler(tools);
        let out = h.execute(serde_json::json!({}), &ctx()).await;
        assert!(out.success);
        assert_eq!(out.data["transfers"].as_array().unwrap().len(), 0);
        drop(tmp);
    }

    #[test]
    fn test_tool_def_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let db = futures::executor::block_on(
            crate::infra::db::init(tmp.path().join("t.db"))
        ).unwrap();
        let mgr = Arc::new(TransferManager::new(db, crate::app::events::EventBus::disabled()));
        let defs = file_transfer_tool_defs(mgr, tmp.path().join("artifacts"));
        assert_eq!(defs.len(), 4);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"file_download"));
        assert!(names.contains(&"file_upload"));
        assert!(names.contains(&"transfer_status"));
        assert!(names.contains(&"transfer_cancel"));
        let upload = defs.iter().find(|d| d.name == "file_upload").unwrap();
        assert_eq!(upload.risk_level, RiskLevel::High);
        let status = defs.iter().find(|d| d.name == "transfer_status").unwrap();
        assert_eq!(status.risk_level, RiskLevel::ReadOnly);
    }
}
```

注意：`test_download_rejects_*` 等测试不真正启动传输（校验在 start 之前失败），无需 SSH。`test_unknown_environment` 同理。

- [ ] **Step 2: builtin/mod.rs 挂模块**

`src-tauri/src/tools/builtin/mod.rs` 顶部加：

```rust
pub mod file_transfer;
```

- [ ] **Step 3: lib.rs 创建 TransferManager 并注册**

`src-tauri/src/lib.rs`：在 `let mut tool_registry = ...` 之后、`jvm::register_all` 之前加：

```rust
            // 文件传输：TransferManager（后台异步传输引擎）+ 4 个工具
            let transfer_manager = Arc::new(crate::transfer::TransferManager::new(
                pool.clone(),
                EventBus::new(handle.clone()),
            ));
            for def in crate::tools::builtin::file_transfer::file_transfer_tool_defs(
                transfer_manager.clone(),
                paths.artifacts_dir(),
            ) {
                tool_registry.register(def);
            }
```

heap_dump 需要 TransferManager——但 jvm_core 已在前面构造。**调整顺序**：把 transfer_manager 的创建挪到 jvm_core 之前，`register_all` 签名加参数（Task 6 处理 heap_dump 时改）。本 task 先只注册 4 工具；lib.rs 中 transfer_manager 创建放在 jvm_core 之前即可（Task 6 复用）。

- [ ] **Step 4: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml file_transfer`
Expected: 全 PASS

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/tools src-tauri/src/lib.rs
git commit -m "feat: file_download/file_upload/transfer_status/transfer_cancel tools"
```

---

### Task 6: heap_dump 改造为后台拉回

**Files:**
- Modify: `src-tauri/src/tools/builtin/jvm/heap_dump.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/mod.rs`（register_all 加 transfer_manager 参数）
- Modify: `src-tauri/src/lib.rs`（传参）
- Test: `src-tauri/src/tools/builtin/jvm/heap_dump.rs` tests 重写

- [ ] **Step 1: 重写 heap_dump.rs 三阶段**

改动点：

1. 删除 `DOWNLOAD_DEFAULT_TIMEOUT_SECS` / `DOWNLOAD_MAX_TIMEOUT_SECS` 常量、`download_timeout_secs` 参数解析、`download_timeout` 相关逻辑；
2. 删除 import 里不再使用的 `clamp_or`（如果只有它在用）——检查：dump_timeout 还在用 `clamp_or`，保留 import；
3. `HeapDumpHandler` 加 `transfer: Arc<TransferManager>` 字段；
4. 第三阶段替换为：

```rust
        // ③ 后台拉回：TransferManager（MCP 同步调用秒回，Agent 轮询 transfer_status）
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(format!("heapdump-{pid}-{ts}.hprof"));
        let state = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            &ctx.session_id,
            &env.id,
            &remote_path,
            local_path.clone(),
            true, // 下载成功后清理远端（Friday 自己生成的文件）
        );
        let transfer_id = self.transfer.start(state).await;

        self.emit_progress(&ctx.session_id, "dump 生成完成，后台拉回已启动（轮询 transfer_status 获取进度）");

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            transfer_id = %transfer_id,
            remote_path, remote_size,
            dump_elapsed_ms, "heap dump generated, background download started"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "remote_path": remote_path,
                "remote_size": remote_size,
                "dump_elapsed_ms": dump_elapsed_ms,
                "local_path": local_path.to_string_lossy(),
                "note": "dump 已生成，正在后台拉回。请轮询 transfer_status(transfer_id)；completed 后把 local_path 告知用户；failed 时远端文件保留，可用 file_download 重试（断点续传）。",
            }),
            raw_stdout: Some(dump_output.stdout),
        }
```

5. `jvm_heap_dump_tool_def` 签名加 `transfer: Arc<TransferManager>` 参数，handler 构造带上；
6. tool description 更新（去掉"自动拉回本地"同步语义）：

```rust
        description: "对目标 JVM 生成堆转储并后台拉回本地（jcmd GC.heap_dump）。⚠ 高风险：触发 Full GC（STW），大堆可能停顿数十秒；dump 文件可达 GB 级。生成后自动启动后台下载（返回 transfer_id），请轮询 transfer_status(transfer_id)，completed 后 local_path 在本机会话 artifacts 目录，请告知用户用 MAT 等工具分析。需先 ensure_tool 装备 JDK。".to_string(),
```

input_schema 里删掉 `download_timeout_secs` 属性。

- [ ] **Step 2: mod.rs / lib.rs 更新签名**

`src-tauri/src/tools/builtin/jvm/mod.rs`：

```rust
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    core: Arc<core::JvmExecCore>,
    bus: EventBus,
    transfer: Arc<crate::transfer::TransferManager>,
) {
    // ... 前面 6 个不变 ...
    registry.register(heap_dump::jvm_heap_dump_tool_def(core, bus, transfer));
}
```

`src-tauri/src/lib.rs` 调用处：

```rust
            crate::tools::builtin::jvm::register_all(
                &mut tool_registry,
                jvm_core,
                EventBus::new(handle.clone()),
                transfer_manager.clone(),
            );
```

- [ ] **Step 3: 重写 heap_dump tests**

现有 5 个测试改造 + DumpChannel 精简：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tokio::sync::Mutex as TokioMutex;

    /// 可编程 mock：按命令内容路由（dump/stat）
    struct DumpChannel {
        dump_exit: i32,
        stat_size: &'static str,
        calls: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for DumpChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            if cmd.contains("GC.heap_dump") {
                return Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: self.dump_exit });
            }
            if cmd.starts_with("stat -c %s") {
                return Ok(ExecOutput { stdout: self.stat_size.to_string(), stderr: String::new(), exit_code: 0 });
            }
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        // upload/download 不再被 heap_dump 使用
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>, Arc<crate::transfer::TransferManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(&db, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let env_id = crate::app::environments::find_by_name(&db, "prod").await.unwrap().unwrap().id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache.set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins }).await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore { db: db.clone(), exec_pool, jdk_cache, artifacts_dir: artifacts });
        let mgr = Arc::new(crate::transfer::TransferManager::new(db, crate::app::events::EventBus::disabled()));
        (tmp, core, mgr)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    fn handler(core: Arc<JvmExecCore>, mgr: Arc<crate::transfer::TransferManager>) -> HeapDumpHandler {
        HeapDumpHandler { core, transfer: mgr, bus: crate::app::events::EventBus::disabled() }
    }

    #[tokio::test]
    async fn test_full_flow_starts_background_download() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "12345", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch.clone()).await;
        let out = handler(core, mgr.clone())
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(out.success, "out: {}", out.data);
        // 返回 transfer_id（后台任务已注册）而非同步下载结果
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["local_path"].as_str().unwrap().ends_with(".hprof"));
        assert!(out.data["note"].as_str().unwrap().contains("轮询"));
        // 注册表里能查到（状态至少是 pending）
        assert!(mgr.get(tid).await.is_some());
        // 调用序列：dump → stat（无 rm/download——rm 移到 worker 完成后）
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("GC.heap_dump"));
        assert!(calls[1].starts_with("stat -c %s"));
        assert_eq!(calls.len(), 2);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_dump_cmd_failure_passthrough() {
        let ch = Arc::new(DumpChannel { dump_exit: 1, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_stat_empty_fails() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_failed");
        assert!(out.data["message"].as_str().unwrap().contains("不存在或为空"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_pid_injection_rejected() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let out = handler(core, mgr)
            .execute(serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jdk_not_provisioned() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let env_id = crate::app::environments::find_by_name(&core.db, "prod").await.unwrap().unwrap().id;
        core.jdk_cache.clear(&env_id).await;
        let out = handler(core, mgr).execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jdk_not_provisioned");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled(), mgr);
        assert_eq!(def.name, "jvm_heap_dump");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert!(!def.needs_channel);
        // schema 不再含 download_timeout_secs
        let schema_str = serde_json::to_string(&def.input_schema).unwrap();
        assert!(!schema_str.contains("download_timeout_secs"));
        drop(tmp);
    }
}
```

注意 `test_full_flow_starts_background_download` 中 `mgr.start()` 会 spawn 真 worker（连 10.0.0.1 失败进入重试循环）——测试断言只查注册表状态，worker 后台自旋不影响断言；TempDir drop 时 worker 可能仍在重试，`.part` 文件操作在 tmp 目录外无害（worker 用 state.local_path 在 tmp 内，重试中连接失败根本到不了写文件）。为避免测试结束后的孤儿 task 干扰（cargo test 进程退出即终止），可接受。

- [ ] **Step 4: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml heap_dump`
Expected: 6 个测试 PASS

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/tools/builtin/jvm src-tauri/src/lib.rs
git commit -m "feat: heap dump pulls back via background TransferManager"
```

---

### Task 7: 前端类型 + store 事件处理

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/store/sessionStore.ts`
- Test: `pnpm typecheck`

- [ ] **Step 1: types.ts 扩展**

AppEvent 联合类型（`session_deleted` 行前）加：

```typescript
  | { type: "transfer_progress"; session_id: string; transfer_id: string; direction: "download" | "upload"; status: "pending" | "connecting" | "transferring" | "retrying" | "completed" | "failed" | "cancelled"; transferred_bytes: number; total_bytes: number; speed_bps: number; attempt: number }
  | { type: "transfer_finished"; session_id: string; transfer_id: string; direction: "download" | "upload"; status: "completed" | "failed" | "cancelled"; transferred_bytes: number; total_bytes: number; error: string | null; local_path: string | null; remote_path: string }
```

ChatPartType 与 ChatPart：

```typescript
export type ChatPartType = "text" | "reasoning" | "tool" | "confirm" | "transfer";

export interface TransferInfo {
  transfer_id: string;
  direction: "download" | "upload";
  status: "pending" | "connecting" | "transferring" | "retrying" | "completed" | "failed" | "cancelled";
  transferred_bytes: number;
  total_bytes: number;
  speed_bps: number;
  attempt: number;
  error: string | null;
  file_name: string;
}

export interface ChatPart {
  type: ChatPartType;
  text?: string;
  tool?: ToolCallInfo;
  confirm?: ConfirmRequest;
  transfer?: TransferInfo;
}
```

- [ ] **Step 2: sessionStore.ts 处理两个事件**

`handleEvent` 里（`provision_progress` 分支后）加。注意 `transfer_finished` 事件没有 `speed_bps`/`attempt` 字段，联合类型上不能直接 `event.speed_bps ?? 0`——先做条件取值：

```typescript
    if (event.type === "transfer_progress" || event.type === "transfer_finished") {
      const messages = state.messagesBySession[session_id] ?? [];
      const speed = event.type === "transfer_progress" ? event.speed_bps : 0;
      const attempt = event.type === "transfer_progress" ? event.attempt : 0;
      const info: TransferInfo = {
        transfer_id: event.transfer_id,
        direction: event.direction,
        status: event.status,
        transferred_bytes: event.transferred_bytes,
        total_bytes: event.total_bytes,
        speed_bps: speed,
        attempt,
        error: "error" in event ? event.error : null,
        file_name: event.remote_path.split("/").pop() ?? event.remote_path,
      };

      let messages2 = messages;
      // 无 agent 消息时兜底新建一条承载（heap_dump 场景 Agent 可能已结束本轮回复）
      if (messages2.length === 0 || messages2[messages2.length - 1].role !== "agent") {
        messages2 = [
          ...messages2,
          {
            id: `agent-${agentMessageCounter++}`,
            role: "agent" as const,
            content: "",
            parts: [],
            status: "done" as const,
          },
        ];
      }

      const lastIdx = messages2.length - 1;
      const lastMsg = messages2[lastIdx];
      const updatedParts = [...lastMsg.parts];
      const existingIdx = updatedParts.findIndex(
        (p) => p.type === "transfer" && p.transfer?.transfer_id === event.transfer_id,
      );
      if (existingIdx >= 0) {
        updatedParts[existingIdx] = { ...updatedParts[existingIdx], transfer: info };
      } else {
        updatedParts.push({ type: "transfer", transfer: info });
      }

      const updatedMessages = [...messages2];
      updatedMessages[lastIdx] = { ...lastMsg, parts: updatedParts };
      set({
        messagesBySession: {
          ...state.messagesBySession,
          [session_id]: updatedMessages,
        },
      });
      return;
    }
```

sessionStore.ts 顶部 import 补 `TransferInfo`（从 `@/lib/types`）。

- [ ] **Step 3: typecheck**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: 提交**

```bash
git add src/lib/types.ts src/store/sessionStore.ts
git commit -m "feat: frontend transfer event handling and types"
```

---

### Task 8: TransferProgressCard 组件

**Files:**
- Create: `src/components/chat/TransferProgressCard.tsx`
- Modify: `src/components/chat/AgentMessage.tsx`
- Test: `pnpm typecheck`

- [ ] **Step 1: 写组件**

```tsx
import { CheckCircle, XCircle, Spinner, ArrowDown, ArrowUp, Warning } from "@phosphor-icons/react";
import type { TransferInfo } from "@/lib/types";

interface TransferProgressCardProps {
  transfer: TransferInfo;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
}

export function TransferProgressCard({ transfer }: TransferProgressCardProps) {
  const isDownload = transfer.direction === "download";
  const pct =
    transfer.total_bytes > 0
      ? Math.min(100, (transfer.transferred_bytes / transfer.total_bytes) * 100)
      : 0;
  const isTerminal = ["completed", "failed", "cancelled"].includes(transfer.status);

  const statusLabel = (() => {
    switch (transfer.status) {
      case "pending":
      case "connecting":
        return "连接中...";
      case "transferring":
        return `${formatBytes(transfer.transferred_bytes)} / ${formatBytes(transfer.total_bytes)} · ${formatBytes(transfer.speed_bps)}/s`;
      case "retrying":
        return `重试中（第 ${transfer.attempt} 次）`;
      case "completed":
        return `完成 · ${formatBytes(transfer.total_bytes)}`;
      case "failed":
        return "失败";
      case "cancelled":
        return "已取消";
    }
  })();

  const statusColor = (() => {
    switch (transfer.status) {
      case "completed":
        return "text-success";
      case "failed":
        return "text-destructive";
      case "cancelled":
        return "text-muted-foreground";
      default:
        return "text-accent";
    }
  })();

  return (
    <div className="bg-card border border-border rounded-lg overflow-hidden mb-3">
      <div className="flex items-center gap-2 px-3 py-2">
        {isDownload ? (
          <ArrowDown size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        ) : (
          <ArrowUp size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        )}
        <span
          className="text-xs font-semibold text-accent bg-accent/10 px-1.5 py-0.5 rounded shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {isDownload ? "下载" : "上传"}
        </span>
        <span
          className="text-xs text-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
          title={transfer.file_name}
        >
          {transfer.file_name}
        </span>
        <span
          className={`text-xs shrink-0 flex items-center gap-1 ${statusColor}`}
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {!isTerminal && transfer.status !== "retrying" && (
            <Spinner size={12} className="animate-spin" aria-hidden="true" />
          )}
          {transfer.status === "retrying" && (
            <Warning size={12} weight="fill" aria-hidden="true" />
          )}
          {transfer.status === "completed" && (
            <CheckCircle size={12} weight="fill" aria-hidden="true" />
          )}
          {transfer.status === "failed" && (
            <XCircle size={12} weight="fill" aria-hidden="true" />
          )}
          {statusLabel}
        </span>
      </div>
      {/* 进度条：未知大小(total=0)或终态失败时不显示 */}
      {(transfer.total_bytes > 0 || transfer.status === "completed") && transfer.status !== "failed" && (
        <div className="px-3 pb-2">
          <div className="h-1 bg-surface-2 rounded-full overflow-hidden" role="progressbar" aria-valuenow={Math.round(pct)} aria-valuemin={0} aria-valuemax={100} aria-label="传输进度">
            <div
              className={`h-full rounded-full transition-all ${
                transfer.status === "failed"
                  ? "bg-destructive"
                  : transfer.status === "completed"
                    ? "bg-success"
                    : "bg-accent"
              }`}
              style={{ width: `${transfer.status === "completed" ? 100 : pct}%` }}
            />
          </div>
        </div>
      )}
      {transfer.error && (
        <div className="border-t border-border px-3 py-2 bg-background">
          <p
            className="text-xs text-destructive whitespace-pre-wrap break-all"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {transfer.error}
          </p>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: AgentMessage.tsx 渲染 transfer part**

`src/components/chat/AgentMessage.tsx`：

import 加：

```tsx
import { TransferProgressCard } from "./TransferProgressCard";
```

`message.parts.map` 渲染分支加（confirm 分支后）：

```tsx
        if (part.type === "transfer" && part.transfer) {
          return <TransferProgressCard key={i} transfer={part.transfer} />;
        }
```

- [ ] **Step 3: typecheck**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: 提交**

```bash
git add src/components/chat
git commit -m "feat: transfer progress card in chat"
```

---

### Task 9: 全量验证 + agent prompt 提示

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs`（Friday 人格 prompt 补传输工具指引，看现有内容酌情加一小节）
- Test: 全量 cargo test + pnpm typecheck + cargo check

- [ ] **Step 1: prompt.rs 的 TOOL_GUIDANCE 补传输流程指引**

`src-tauri/src/agent/prompt.rs` 的 `TOOL_GUIDANCE` 常量（30-36 行），在 `run_command 是兜底` 那条之后追加：

```text
- 文件传输：拉取/推送大文件（堆快照、日志包、工具包）必须用 file_download / file_upload 后台传输工具。启动后立即返回 transfer_id，轮询 transfer_status(transfer_id) 直到终态：completed（下载场景把 local_path 告知用户，artifacts 目录可用 MAT 等分析）；failed（远端文件保留，file_download 同一文件可断点续传，不要放弃）；retrying（自动重试中，稍等再查，不要重复启动新任务）。不要用 run_command + cat/base64 拉大文件。
```

同步在 prompt.rs tests 里加断言（找到现有 TOOL_GUIDANCE 相关测试，追加）：

```rust
    #[test]
    fn test_tool_guidance_mentions_transfer_tools() {
        assert!(TOOL_GUIDANCE.contains("file_download"));
        assert!(TOOL_GUIDANCE.contains("transfer_status"));
    }
```

- [ ] **Step 2: 全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全 PASS（transfer:: + file_transfer + heap_dump + 既有测试无回归）

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无 warning 新增（特别是未使用 import）

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: agent prompt guidance for background file transfer"
```

---

## 验收（人工，计划执行完后）

真实环境（或本地 SSH 目标）：

1. 触发 heap_dump（大堆），确认 MCP 调用秒回 transfer_id；
2. Agent 轮询 transfer_status，UI 出现进度条并推进；
3. 传输中掐断 VPN/网络 10s 再恢复，确认状态变 retrying 后续传完成（transferred_bytes 从断点继续涨）；
4. 完成后本地文件大小 == 远端原大小，远端文件已清理；
5. `file_download` 拉一个远端文件 → 完成后远端文件仍在；
6. `file_upload` 上传本地文件 → 弹确认卡片 → 批准后传输完成，远端大小一致；
7. `transfer_cancel` 取消进行中任务 → 状态 cancelled，本地 .part 保留。
