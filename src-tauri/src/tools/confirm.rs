use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmResult {
    Confirmed,
    Cancelled,
}

struct PendingConfirm {
    session_id: String,
    tx: oneshot::Sender<ConfirmResult>,
}

pub struct ConfirmRegistry {
    pending: std::collections::HashMap<String, PendingConfirm>,
}

impl ConfirmRegistry {
    pub fn new() -> Self {
        Self {
            pending: std::collections::HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        confirm_id: String,
        session_id: String,
        tx: oneshot::Sender<ConfirmResult>,
    ) {
        self.pending.insert(
            confirm_id,
            PendingConfirm {
                session_id,
                tx,
            },
        );
    }

    pub fn resolve(&mut self, confirm_id: &str) -> Option<oneshot::Sender<ConfirmResult>> {
        self.pending.remove(confirm_id).map(|pc| pc.tx)
    }

    pub fn cancel_for_session(&mut self, session_id: &str) -> usize {
        let mut to_remove = Vec::new();
        for (id, pc) in &self.pending {
            if pc.session_id == session_id {
                to_remove.push(id.clone());
            }
        }
        let count = to_remove.len();
        for id in to_remove {
            if let Some(pc) = self.pending.remove(&id) {
                let _ = pc.tx.send(ConfirmResult::Cancelled);
            }
        }
        count
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

impl Default for ConfirmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_resolve() {
        let mut registry = ConfirmRegistry::new();
        let (tx, rx) = oneshot::channel();
        registry.insert("c1".to_string(), "s1".to_string(), tx);

        let resolved_tx = registry.resolve("c1");
        assert!(resolved_tx.is_some());

        resolved_tx.unwrap().send(ConfirmResult::Confirmed).unwrap();
        let result = rx.await.unwrap();
        assert_eq!(result, ConfirmResult::Confirmed);
    }

    #[tokio::test]
    async fn test_resolve_nonexistent_returns_none() {
        let mut registry = ConfirmRegistry::new();
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_cancel_for_session_sends_cancelled() {
        let mut registry = ConfirmRegistry::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        registry.insert("c1".to_string(), "s1".to_string(), tx1);
        registry.insert("c2".to_string(), "s2".to_string(), tx2);

        let count = registry.cancel_for_session("s1");
        assert_eq!(count, 1);

        let result = rx1.await.unwrap();
        assert_eq!(result, ConfirmResult::Cancelled);

        // s2 should still be pending
        assert_eq!(registry.pending_count(), 1);
    }

    #[tokio::test]
    async fn test_cancel_for_session_multiple_pending() {
        let mut registry = ConfirmRegistry::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        registry.insert("c1".to_string(), "s1".to_string(), tx1);
        registry.insert("c2".to_string(), "s1".to_string(), tx2);

        let count = registry.cancel_for_session("s1");
        assert_eq!(count, 2);

        assert_eq!(rx1.await.unwrap(), ConfirmResult::Cancelled);
        assert_eq!(rx2.await.unwrap(), ConfirmResult::Cancelled);
        assert_eq!(registry.pending_count(), 0);
    }

    #[tokio::test]
    async fn test_cancel_for_nonexistent_session_returns_zero() {
        let mut registry = ConfirmRegistry::new();
        let (tx, _rx) = oneshot::channel();
        registry.insert("c1".to_string(), "s1".to_string(), tx);

        let count = registry.cancel_for_session("nonexistent");
        assert_eq!(count, 0);
    }
}
