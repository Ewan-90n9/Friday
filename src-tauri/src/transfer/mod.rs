pub mod state;
pub mod worker;

use crate::app::events::{AppEvent, EventBus};
use state::{Direction, Status, TransferState};
use std::collections::HashMap;
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
            if status.is_terminal() {
                return; // 终态只能经 finish 流转
            }
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
        tracing::info!(transfer_id = %event.id, status = ?event.status, error = ?event.error, "transfer finished");
        self.evict_finished().await;
    }

    /// 请求取消。返回 false = 不存在、已终态或尚未 attach worker。
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
    async fn evict_finished(&self) {
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
    use std::path::PathBuf;

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
    async fn test_finish_sets_terminal_and_is_idempotent() {
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
    async fn test_update_progress_rejects_terminal_status() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = TransferManager::new(db, EventBus::disabled());
        let id = mgr
            .register(make_state(Direction::Download, "/tmp/a.hprof", "s1"))
            .await;
        mgr.update_progress(&id, Status::Completed, 1, 1, 0, 1).await;
        let st = mgr.get(&id).await.unwrap();
        assert_eq!(st.status, Status::Pending);
        assert!(st.completed_at.is_none());
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
