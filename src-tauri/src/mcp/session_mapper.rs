use std::collections::HashMap;

pub struct SessionMapper {
    next_session: Option<String>,
    mapping: HashMap<String, String>,
}

impl SessionMapper {
    pub fn new() -> Self {
        Self {
            next_session: None,
            mapping: HashMap::new(),
        }
    }

    pub fn enqueue(&mut self, session_id: String) {
        self.next_session = Some(session_id);
    }

    pub fn dequeue_and_map(&mut self, mcp_session_id: String) {
        if let Some(friday_session_id) = self.next_session.take() {
            self.mapping.insert(mcp_session_id, friday_session_id);
        }
    }

    pub fn lookup(&self, mcp_session_id: &str) -> Option<String> {
        self.mapping.get(mcp_session_id).cloned()
    }

    pub fn pending_count(&self) -> usize {
        self.mapping.len()
    }

    pub fn has_queued(&self) -> bool {
        self.next_session.is_some()
    }
}

impl Default for SessionMapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_and_dequeue_creates_mapping() {
        let mut mapper = SessionMapper::new();
        mapper.enqueue("friday-s1".to_string());
        assert!(mapper.has_queued());

        mapper.dequeue_and_map("mcp-session-abc".to_string());
        assert!(!mapper.has_queued());

        assert_eq!(mapper.lookup("mcp-session-abc"), Some("friday-s1".to_string()));
    }

    #[test]
    fn test_dequeue_without_enqueue_is_noop() {
        let mut mapper = SessionMapper::new();
        mapper.dequeue_and_map("mcp-session-xyz".to_string());
        assert_eq!(mapper.lookup("mcp-session-xyz"), None);
    }

    #[test]
    fn test_lookup_nonexistent_returns_none() {
        let mapper = SessionMapper::new();
        assert_eq!(mapper.lookup("nonexistent"), None);
    }

    #[test]
    fn test_multiple_mappings() {
        let mut mapper = SessionMapper::new();
        mapper.enqueue("s1".to_string());
        mapper.dequeue_and_map("mcp1".to_string());
        mapper.enqueue("s2".to_string());
        mapper.dequeue_and_map("mcp2".to_string());

        assert_eq!(mapper.lookup("mcp1"), Some("s1".to_string()));
        assert_eq!(mapper.lookup("mcp2"), Some("s2".to_string()));
        assert_eq!(mapper.pending_count(), 2);
    }
}
