use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::sync::watch;

/// 同时打开的 dump 会话上限（LRU 逐出）
pub const MAX_OPEN_DUMPS: usize = 3;

/// 单个 dump 的会话状态。watch 通道广播状态变迁（多等待者合流）。
// Task 5（manager）接入前 Ready/Failed 无构造方，避免 dead_code 告警
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum EntryPhase {
    Warming,
    Ready { summary: String },
    Failed { error: crate::analyzer::manager::ManagerError },
}

#[derive(Debug)]
pub struct DumpEntry {
    pub analyzer_session_id: String,
    phase_tx: watch::Sender<EntryPhase>,
    pub last_touched: Instant,
}

#[derive(Debug, Default)]
pub struct DumpSessions {
    entries: HashMap<PathBuf, DumpEntry>,
}

// Task 5（manager）接入前暂无调用方，避免 dead_code 告警
#[allow(dead_code)]
impl DumpSessions {
    pub fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 新建 warming 条目（覆盖同路径旧条目，Failed 重试路径）。返回 (phase 订阅者, LRU 逐出受害者列表)。
    /// 超过上限时循环逐出最久未访问的 Ready 条目直至回到上限；无 Ready 可逐出时停止（允许暂时超限）。
    pub fn begin(
        &mut self,
        path: PathBuf,
        analyzer_session_id: String,
    ) -> (watch::Receiver<EntryPhase>, Vec<(PathBuf, String)>) {
        let (tx, rx) = watch::channel(EntryPhase::Warming);
        self.entries.insert(
            path,
            DumpEntry {
                analyzer_session_id,
                phase_tx: tx,
                last_touched: Instant::now(),
            },
        );
        let mut victims = Vec::new();
        while self.entries.len() > MAX_OPEN_DUMPS {
            match self.evict_lru() {
                Some(victim) => victims.push(victim),
                None => break,
            }
        }
        (rx, victims)
    }

    pub fn phase(&self, path: &Path) -> Option<EntryPhase> {
        self.entries.get(path).map(|e| e.phase_tx.borrow().clone())
    }

    pub fn receiver(&self, path: &Path) -> Option<watch::Receiver<EntryPhase>> {
        self.entries.get(path).map(|e| e.phase_tx.subscribe())
    }

    pub fn analyzer_id(&self, path: &Path) -> Option<String> {
        self.entries.get(path).map(|e| e.analyzer_session_id.clone())
    }

    /// 落定 phase（Warming → Ready/Failed）并刷新 LRU 时间；条目不存在（已被 close/逐出）或
    /// analyzer_session_id 不匹配（过期任务写入已被重试覆盖的新条目）→ false，写入被静默丢弃。
    pub fn set_phase(&mut self, path: &Path, analyzer_session_id: &str, phase: EntryPhase) -> bool {
        match self.entries.get_mut(path) {
            Some(e) if e.analyzer_session_id == analyzer_session_id => {
                e.phase_tx.send_replace(phase);
                e.last_touched = Instant::now();
                true
            }
            _ => false,
        }
    }

    pub fn touch(&mut self, path: &Path) {
        if let Some(e) = self.entries.get_mut(path) {
            e.last_touched = Instant::now();
        }
    }

    /// 移除条目，返回 analyzer_session_id（供上游 close）
    pub fn remove(&mut self, path: &Path) -> Option<String> {
        self.entries.remove(path).map(|e| e.analyzer_session_id)
    }

    /// LRU 逐出：移除最久未访问的 Ready 条目（Warming 不逐出）
    pub fn evict_lru(&mut self) -> Option<(PathBuf, String)> {
        let victim = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(*e.phase_tx.borrow(), EntryPhase::Ready { .. }))
            .min_by_key(|(_, e)| e.last_touched)
            .map(|(p, _)| p.clone())?;
        let entry = self.entries.remove(&victim)?;
        Some((victim, entry.analyzer_session_id))
    }

    /// 移除 base 目录下全部条目（Friday 会话关闭联动）
    pub fn remove_under_dir(&mut self, base: &Path) -> Vec<(PathBuf, String)> {
        let victims: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|p| p.starts_with(base))
            .cloned()
            .collect();
        victims
            .into_iter()
            .filter_map(|p| self.remove(&p).map(|id| (p, id)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed() -> EntryPhase {
        EntryPhase::Failed { error: crate::analyzer::manager::ManagerError::Unavailable("boom".into()) }
    }

    #[test]
    fn test_begin_starts_warming_and_returns_receiver() {
        let mut s = DumpSessions::new();
        let (rx, victims) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert!(victims.is_empty());
        assert_eq!(s.len(), 1);
        assert!(matches!(*rx.borrow(), EntryPhase::Warming));
        assert!(matches!(s.phase(Path::new("/a.hprof")), Some(EntryPhase::Warming)));
    }

    #[test]
    fn test_set_phase_ready_notifies_receiver() {
        let mut s = DumpSessions::new();
        let (rx, _) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert!(s.set_phase(Path::new("/a.hprof"), "id-1", EntryPhase::Ready { summary: "SUM".into() }));
        assert!(matches!(*rx.borrow(), EntryPhase::Ready { .. }));
        assert!(!s.set_phase(Path::new("/nope"), "id-1", EntryPhase::Ready { summary: String::new() }));
    }

    #[test]
    fn test_set_phase_failed_keeps_entry_for_waiters() {
        let mut s = DumpSessions::new();
        let (rx, _) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        s.set_phase(Path::new("/a.hprof"), "id-1", failed());
        assert!(matches!(*rx.borrow(), EntryPhase::Failed { .. }));
        assert_eq!(s.len(), 1, "failed entry kept so waiters can read the error");
    }

    #[test]
    fn test_remove_returns_analyzer_id() {
        let mut s = DumpSessions::new();
        s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert_eq!(s.remove(Path::new("/a.hprof")).as_deref(), Some("id-1"));
        assert_eq!(s.remove(Path::new("/a.hprof")), None);
        assert!(s.is_empty());
    }

    #[test]
    fn test_evict_lru_picks_oldest_ready_and_skips_warming() {
        let mut s = DumpSessions::new();
        for (p, id) in [("/a.hprof", "a"), ("/b.hprof", "b"), ("/c.hprof", "c")] {
            s.begin(PathBuf::from(p), id.into());
            s.set_phase(Path::new(p), id, EntryPhase::Ready { summary: "S".into() });
        }
        // b 重新 begin（转 Warming，不可逐出）
        s.begin(PathBuf::from("/b.hprof"), "b2".into());
        let victim = s.evict_lru();
        assert_eq!(
            victim.map(|(p, id)| (p.display().to_string(), id)),
            Some(("/a.hprof".to_string(), "a".to_string()))
        );
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn test_touch_affects_lru_order() {
        let mut s = DumpSessions::new();
        for (p, id) in [("/a.hprof", "a"), ("/b.hprof", "b")] {
            s.begin(PathBuf::from(p), id.into());
            s.set_phase(Path::new(p), id, EntryPhase::Ready { summary: "S".into() });
        }
        s.touch(Path::new("/a.hprof")); // a 最新 → 逐出 b
        let (p, id) = s.evict_lru().unwrap();
        assert_eq!((p.display().to_string(), id.as_str()), ("/b.hprof".to_string(), "b"));
    }

    #[test]
    fn test_begin_evicts_until_cap_reached() {
        let mut s = DumpSessions::new();
        for (p, id) in [("/a.hprof", "a"), ("/b.hprof", "b"), ("/c.hprof", "c")] {
            s.begin(PathBuf::from(p), id.into());
            s.set_phase(Path::new(p), id, EntryPhase::Ready { summary: "S".into() });
        }
        // 4th：超上限 → 逐出最老的 Ready（a），len 回到 3
        let (_rx, victims) = s.begin(PathBuf::from("/d.hprof"), "d".into());
        assert_eq!(victims.len(), 1);
        assert!(victims[0].0.ends_with("a.hprof"));
        assert_eq!(s.len(), 3);
        // 5th：此时 b,c Ready、d Warming → 逐出 b，len 仍 3
        let (_rx, victims) = s.begin(PathBuf::from("/e.hprof"), "e".into());
        assert_eq!(victims.len(), 1);
        assert!(victims[0].0.ends_with("b.hprof"));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn test_set_phase_rejects_stale_session_id() {
        let mut s = DumpSessions::new();
        s.begin(PathBuf::from("/a.hprof"), "old".into());
        // 重试路径：begin 覆盖为 new
        let (new_rx, _) = s.begin(PathBuf::from("/a.hprof"), "new".into());
        // 过期任务的写入必须被丢弃
        assert!(!s.set_phase(Path::new("/a.hprof"), "old", failed()));
        assert!(matches!(*new_rx.borrow(), EntryPhase::Warming));
        // 正确写入者生效
        assert!(s.set_phase(Path::new("/a.hprof"), "new", EntryPhase::Ready { summary: "S".into() }));
        assert!(matches!(*new_rx.borrow(), EntryPhase::Ready { .. }));
    }

    #[tokio::test]
    async fn test_begin_replace_closes_old_receiver() {
        let mut s = DumpSessions::new();
        let (mut old_rx, _) = s.begin(PathBuf::from("/a.hprof"), "a".into());
        s.begin(PathBuf::from("/a.hprof"), "a2".into());
        // 旧订阅者看到通道关闭（changed 返回 Err），而非虚假状态
        assert!(old_rx.changed().await.is_err());
    }

    #[test]
    fn test_remove_under_dir_sibling_prefix_not_matched() {
        let mut s = DumpSessions::new();
        let base = Path::new("/artifacts/sess-1");
        for (p, id) in [
            ("/artifacts/sess-1/a.hprof", "a"),
            ("/artifacts/sess-12/b.hprof", "b"),
        ] {
            s.begin(PathBuf::from(p), id.into());
        }
        let removed = s.remove_under_dir(base);
        assert_eq!(removed.len(), 1);
        assert!(removed[0].0.ends_with("a.hprof"));
        assert_eq!(s.len(), 1);
        assert_eq!(s.analyzer_id(Path::new("/artifacts/sess-12/b.hprof")).as_deref(), Some("b"));
    }

    #[test]
    fn test_remove_under_dir_scopes_by_prefix() {
        let mut s = DumpSessions::new();
        let base = Path::new("/artifacts/sess-1");
        for (p, id) in [
            ("/artifacts/sess-1/a.hprof", "a"),
            ("/artifacts/sess-1/b.hprof", "b"),
            ("/artifacts/sess-2/c.hprof", "c"),
        ] {
            s.begin(PathBuf::from(p), id.into());
        }
        let removed = s.remove_under_dir(base);
        assert_eq!(removed.len(), 2);
        assert!(removed.iter().all(|(p, _)| p.starts_with(base)));
        assert_eq!(s.len(), 1);
        assert_eq!(s.analyzer_id(Path::new("/artifacts/sess-2/c.hprof")).as_deref(), Some("c"));
    }
}
