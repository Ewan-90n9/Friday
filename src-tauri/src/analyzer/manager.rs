use crate::analyzer::client::{CallOutcome, HeapAnalyzerClient};
use crate::analyzer::session::{DumpSessions, EntryPhase};
use crate::app::events::{AppEvent, EventBus};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// open 任务（预热/显式 open）的内部硬超时，对齐 heap_open 工具超时上限
const OPEN_TASK_TIMEOUT_SECS: u64 = 1800;
/// upstream close 调用的固定超时
const CLOSE_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("{0}")]
    JavaMissing(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("分析调用超时（{0}s），工人进程保留未受影响")]
    Timeout(u64),
    #[error("该 dump 尚未打开")]
    NotOpen { warming: bool },
    #[error("{0}")]
    Upstream(String),
}

pub type ClientFactory = Arc<
    dyn Fn(
            u32,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Arc<dyn HeapAnalyzerClient>, ManagerError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ManagerConfig {
    /// 无会话且无调用持续该时长后退出工人进程
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}

pub struct OpenOutcome {
    pub summary: String,
    pub evicted: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct HeapAnalyzerManager {
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    spawn_lock: Arc<tokio::sync::Mutex<()>>,
    client_factory: ClientFactory,
    bus: EventBus,
    artifacts_dir: PathBuf,
    config: ManagerConfig,
}

struct ManagerInner {
    client: Option<Arc<dyn HeapAnalyzerClient>>,
    sessions: DumpSessions,
    inflight: u32,
    last_active: Instant,
}

/// 会话 phase 订阅者类型别名（open 等待用）
type PhaseRx = tokio::sync::watch::Receiver<EntryPhase>;

/// -Xmx 预算：dump 大小 × 1.5，向上取整 GB，clamp [4, 12]
pub fn xmx_gb_for(dump_size_bytes: u64) -> u32 {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let need = dump_size_bytes as f64 * 1.5;
    ((need / GB).ceil() as u32).clamp(4, 12)
}

impl HeapAnalyzerManager {
    pub fn new(
        client_factory: ClientFactory,
        bus: EventBus,
        artifacts_dir: PathBuf,
        config: ManagerConfig,
    ) -> Self {
        let mgr = Self {
            inner: Arc::new(tokio::sync::Mutex::new(ManagerInner {
                client: None,
                sessions: DumpSessions::new(),
                inflight: 0,
                last_active: Instant::now(),
            })),
            spawn_lock: Arc::new(tokio::sync::Mutex::new(())),
            client_factory,
            bus,
            artifacts_dir,
            config: config.clone(),
        };
        mgr.spawn_idle_reaper();
        mgr
    }

    /// 打开 dump（MAT 建索引）。Ready 命中秒回（缓存 summary）；Warming 合流等待；
    /// Failed 重试。检查与 begin 在同一锁内完成（并发安全去重）。
    pub async fn open(
        &self,
        session_id: &str,
        path: &Path,
        timeout_secs: u64,
    ) -> Result<OpenOutcome, ManagerError> {
        tracing::info!(session_id, dump = %path.display(), timeout_secs, "heap analyzer open");

        enum Step {
            Cached(String),
            Attach(PhaseRx),
            Begin { analyzer_id: String, rx: PhaseRx, victims: Vec<(PathBuf, String)> },
        }

        let step = {
            let mut inner = self.inner.lock().await;
            match inner.sessions.phase(path) {
                Some(EntryPhase::Ready { summary }) => {
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    Step::Cached(summary)
                }
                Some(EntryPhase::Warming) => {
                    Step::Attach(inner.sessions.receiver(path).expect("warming entry has receiver"))
                }
                Some(EntryPhase::Failed { .. }) | None => {
                    let analyzer_id = uuid::Uuid::new_v4().to_string();
                    let (rx, victims) = inner.sessions.begin(path.to_path_buf(), analyzer_id.clone());
                    Step::Begin { analyzer_id, rx, victims }
                }
            }
        };

        let mut evicted = Vec::new();
        let mut rx = match step {
            Step::Cached(summary) => return Ok(OpenOutcome { summary, evicted }),
            Step::Attach(rx) => rx,
            Step::Begin { analyzer_id, rx, victims } => {
                for (victim_path, victim_id) in victims {
                    tracing::info!(victim = %victim_path.display(), "evicting lru dump session");
                    self.close_upstream_quietly(&victim_id).await;
                    evicted.push(victim_path);
                }
                let dump_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let mgr = self.clone();
                let p = path.to_path_buf();
                tokio::spawn(async move {
                    mgr.run_open_task(&p, analyzer_id, dump_size).await;
                });
                rx
            }
        };

        // 等待完成（先查当前值，避免与已完成的 open 竞态）。
        // 注意先绑定再 match：watch::Ref 非 Send，不能作为 match scrutinee 临时值跨 await 存活。
        loop {
            let phase = rx.borrow().clone();
            match phase {
                EntryPhase::Ready { summary } => {
                    let mut inner = self.inner.lock().await;
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    return Ok(OpenOutcome { summary, evicted });
                }
                EntryPhase::Failed { error } => return Err(error),
                EntryPhase::Warming => {}
            }
            match tokio::time::timeout(Duration::from_secs(timeout_secs), rx.changed()).await {
                Err(_) => return Err(ManagerError::Timeout(timeout_secs)),
                Ok(Err(_)) => {
                    return Err(ManagerError::Unavailable(
                        "分析会话已失效（工人进程可能已崩溃），请重试 heap_open".into(),
                    ))
                }
                Ok(Ok(())) => {}
            }
        }
    }

    /// 查询类工具：要求 dump 已 Ready，注入 analyzer session id 后路由到上游工具。
    pub async fn query(
        &self,
        path: &Path,
        upstream_tool: &str,
        upstream_args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        let (analyzer_id, client) = {
            let mut inner = self.inner.lock().await;
            match inner.sessions.phase(path) {
                Some(EntryPhase::Ready { .. }) => {
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    let id = inner.sessions.analyzer_id(path).expect("ready entry has id");
                    (id, inner.client.clone())
                }
                Some(EntryPhase::Warming) => return Err(ManagerError::NotOpen { warming: true }),
                Some(EntryPhase::Failed { .. }) | None => {
                    return Err(ManagerError::NotOpen { warming: false })
                }
            }
        };
        let client = client.ok_or_else(|| ManagerError::Unavailable("工人进程不在运行".into()))?;

        let mut args = upstream_args.clone();
        if let Some(map) = args.as_object_mut() {
            map.insert("id".to_string(), serde_json::json!(analyzer_id));
        }

        match self.guarded_call(&client, upstream_tool, &args, timeout_secs).await {
            Err(ManagerError::Unavailable(e)) => {
                tracing::error!(error = %e, "analyzer worker unavailable during query, invalidating");
                self.invalidate().await;
                Err(ManagerError::Unavailable(e))
            }
            other => other,
        }
    }

    /// 关闭 dump 会话（幂等，上游错误仅告警）。返回是否原本处于打开（含预热中）状态。
    pub async fn close(&self, path: &Path, timeout_secs: u64) -> Result<bool, ManagerError> {
        let analyzer_id = {
            let mut inner = self.inner.lock().await;
            inner.last_active = Instant::now();
            inner.sessions.remove(path)
        };
        let Some(analyzer_id) = analyzer_id else {
            return Ok(false);
        };
        if let Some(client) = self.existing_client().await {
            let res = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
            )
            .await;
            match res {
                Err(_) => tracing::warn!(dump = %path.display(), "heap analyzer close timed out"),
                Ok(Err(e)) => tracing::warn!(dump = %path.display(), error = %e, "heap analyzer close failed"),
                Ok(Ok(o)) if o.is_error => {
                    tracing::warn!(dump = %path.display(), text = %o.text, "heap analyzer close upstream error")
                }
                _ => {}
            }
        }
        Ok(true)
    }

    /// heap dump 拉回完成后的自动预热：open（建索引，硬超时 1800s）+ provision_progress 事件。
    pub async fn warm_up(&self, session_id: &str, path: &Path) {
        let progress = |detail: String| AppEvent::ProvisionProgress {
            session_id: session_id.to_string(),
            tool: "jvm_heap_dump".to_string(),
            stage: "analyze".to_string(),
            detail,
        };
        self.bus.emit(session_id, progress(format!(
            "拉回完成，后台分析预热开始（MAT 建索引）：{}",
            path.display()
        )));
        match self.open(session_id, path, OPEN_TASK_TIMEOUT_SECS).await {
            Ok(_) => self.bus.emit(
                session_id,
                progress(format!("分析就绪，heap_* 工具可直接查询：{}", path.display())),
            ),
            Err(e) => self.bus.emit(
                session_id,
                progress(format!("分析预热失败（不影响对话，可手动 heap_open 重试）：{e}")),
            ),
        }
    }

    /// Friday 会话关闭联动：关闭该会话 artifacts 目录下全部 dump 会话（不主动拉起工人进程）。
    pub async fn close_for_friday_session(&self, session_id: &str) {
        let dir = crate::tools::builtin::run_command::artifact_dir_for(&self.artifacts_dir, session_id);
        let removed = {
            let mut inner = self.inner.lock().await;
            inner.last_active = Instant::now();
            inner.sessions.remove_under_dir(&dir)
        };
        if removed.is_empty() {
            return;
        }
        tracing::info!(session_id, count = removed.len(), "closing dump sessions for friday session");
        if let Some(client) = self.existing_client().await {
            for (_path, analyzer_id) in removed {
                let _ = tokio::time::timeout(
                    Duration::from_secs(CLOSE_TIMEOUT_SECS),
                    client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
                )
                .await;
            }
        }
    }

    // ── 内部 ──

    /// open 的后台任务：ensure client → 上游 open → 落定 phase。
    async fn run_open_task(&self, path: &Path, analyzer_id: String, dump_size: u64) {
        let xmx_gb = xmx_gb_for(dump_size);
        let client = match self.ensure_client(xmx_gb).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(dump = %path.display(), error = %e, "heap analyzer open: ensure client failed");
                self.finish_phase(path, &analyzer_id, EntryPhase::Failed { error: e }).await;
                return;
            }
        };
        let args = serde_json::json!({ "path": path.to_string_lossy(), "id": analyzer_id });
        let result = tokio::time::timeout(
            Duration::from_secs(OPEN_TASK_TIMEOUT_SECS),
            client.call_tool("open_heap_dump", &args),
        )
        .await;
        let phase = match result {
            Err(_) => EntryPhase::Failed {
                error: ManagerError::Timeout(OPEN_TASK_TIMEOUT_SECS),
            },
            Ok(Err(e)) => {
                // 传输层错误 = 工人进程疑似死亡：先失效全部，再落定 Failed
                tracing::error!(dump = %path.display(), error = %e, "heap analyzer open: transport error");
                self.invalidate().await;
                EntryPhase::Failed {
                    error: ManagerError::Unavailable(e),
                }
            }
            Ok(Ok(outcome)) if outcome.is_error => EntryPhase::Failed {
                error: ManagerError::Upstream(outcome.text),
            },
            Ok(Ok(outcome)) => EntryPhase::Ready { summary: outcome.text },
        };
        self.finish_phase(path, &analyzer_id, phase).await;
    }

    /// 带超时 + inflight 计数的上游调用
    async fn guarded_call(
        &self,
        client: &Arc<dyn HeapAnalyzerClient>,
        tool: &str,
        args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        {
            let mut inner = self.inner.lock().await;
            inner.inflight += 1;
        }
        let result = tokio::time::timeout(Duration::from_secs(timeout_secs), client.call_tool(tool, args)).await;
        {
            let mut inner = self.inner.lock().await;
            inner.inflight -= 1;
            inner.last_active = Instant::now();
        }
        match result {
            Err(_) => Err(ManagerError::Timeout(timeout_secs)),
            Ok(Err(e)) => Err(ManagerError::Unavailable(e)),
            Ok(Ok(outcome)) if outcome.is_error => Err(ManagerError::Upstream(outcome.text)),
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    async fn ensure_client(&self, xmx_gb: u32) -> Result<Arc<dyn HeapAnalyzerClient>, ManagerError> {
        {
            let inner = self.inner.lock().await;
            if let Some(c) = &inner.client {
                return Ok(c.clone());
            }
        }
        let _g = self.spawn_lock.lock().await;
        {
            let inner = self.inner.lock().await;
            if let Some(c) = &inner.client {
                return Ok(c.clone());
            }
        }
        let client = (self.client_factory)(xmx_gb).await?;
        tracing::info!(xmx_gb, "heap analyzer worker process started");
        let mut inner = self.inner.lock().await;
        inner.client = Some(client.clone());
        inner.last_active = Instant::now();
        Ok(client)
    }

    async fn existing_client(&self) -> Option<Arc<dyn HeapAnalyzerClient>> {
        self.inner.lock().await.client.clone()
    }

    /// 工人进程失效：摘除客户端 + 清空全部会话（等待者经 watch sender drop 感知错误）+ 尽力 shutdown
    async fn invalidate(&self) {
        let client = {
            let mut inner = self.inner.lock().await;
            let client = inner.client.take();
            inner.sessions = DumpSessions::new();
            inner.last_active = Instant::now();
            client
        };
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    async fn finish_phase(&self, path: &Path, analyzer_id: &str, phase: EntryPhase) {
        let mut inner = self.inner.lock().await;
        inner.sessions.set_phase(path, analyzer_id, phase);
        inner.last_active = Instant::now();
    }

    async fn close_upstream_quietly(&self, analyzer_id: &str) {
        if let Some(client) = self.existing_client().await {
            let _ = tokio::time::timeout(
                Duration::from_secs(CLOSE_TIMEOUT_SECS),
                client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
            )
            .await;
        }
    }

    fn spawn_idle_reaper(&self) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(mgr.config.idle_tick);
            loop {
                ticker.tick().await;
                let client = {
                    let mut inner = mgr.inner.lock().await;
                    let should = inner.client.is_some()
                        && inner.sessions.is_empty()
                        && inner.inflight == 0
                        && inner.last_active.elapsed() >= mgr.config.idle_timeout;
                    if should { inner.client.take() } else { None }
                };
                if let Some(client) = client {
                    tracing::info!("heap analyzer worker idle (no sessions, no calls), shutting down");
                    client.shutdown().await;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::client::{CallOutcome, HeapAnalyzerClient, MockHeapAnalyzerClient};
    use crate::app::events::EventBus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const GB: u64 = 1024 * 1024 * 1024;

    fn manager_with(
        mock: &Arc<MockHeapAnalyzerClient>,
        artifacts: &std::path::Path,
        config: ManagerConfig,
    ) -> (HeapAnalyzerManager, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let mock2 = mock.clone();
        let factory: ClientFactory = Arc::new(move |_xmx| {
            let mock = mock2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn HeapAnalyzerClient> = mock;
                Ok(c)
            })
        });
        (
            HeapAnalyzerManager::new(factory, EventBus::disabled(), artifacts.to_path_buf(), config),
            spawns,
        )
    }

    fn dump_file(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "fake hprof").unwrap();
        p
    }

    async fn open_ready(mgr: &HeapAnalyzerManager, path: &std::path::Path) -> OpenOutcome {
        mgr.open(SID, path, 30).await.expect("open should succeed")
    }

    #[test]
    fn test_xmx_gb_for_matrix() {
        assert_eq!(xmx_gb_for(0), 4);
        assert_eq!(xmx_gb_for(GB), 4); // 1.5GB → ceil 2 → clamp 4
        assert_eq!(xmx_gb_for(3 * GB), 5); // 4.5 → 5
        assert_eq!(xmx_gb_for(6 * GB), 9);
        assert_eq!(xmx_gb_for(9 * GB), 12); // 13.5 → 14 → clamp 12
        assert_eq!(xmx_gb_for(100 * GB), 12);
    }

    #[tokio::test]
    async fn test_open_caches_summary_and_reuses_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("SUMMARY"));
        let (mgr, spawns) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        assert_eq!(open_ready(&mgr, &a).await.summary, "SUMMARY");
        assert_eq!(open_ready(&mgr, &a).await.summary, "SUMMARY");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1, "second open must hit Ready cache");
        drop(calls);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_concurrent_open_dedups_to_single_upstream_call() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    Ok(CallOutcome { text: "SUMMARY".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        let (r1, r2) = tokio::join!(mgr.open(SID, &a, 30), mgr.open(SID, &a, 30));
        assert_eq!(r1.unwrap().summary, "SUMMARY");
        assert_eq!(r2.unwrap().summary, "SUMMARY");
        let calls = mock.calls.lock().await;
        let opens = calls.iter().filter(|(n, _)| n == "open_heap_dump").count();
        assert_eq!(opens, 1, "concurrent opens must dedup to one upstream call");
    }

    #[tokio::test]
    async fn test_open_evicts_lru_when_exceeding_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let paths: Vec<PathBuf> = ["a.hprof", "b.hprof", "c.hprof", "d.hprof"]
            .iter()
            .map(|n| dump_file(tmp.path(), n))
            .collect();
        for p in &paths[..3] {
            open_ready(&mgr, p).await;
        }
        let o = open_ready(&mgr, &paths[3]).await;
        assert_eq!(o.evicted, vec![paths[0].clone()], "oldest ready session must be evicted");
        {
            let calls = mock.calls.lock().await;
            let a_open_id = calls
                .iter()
                .find(|(n, args)| n == "open_heap_dump" && args["path"].as_str().unwrap().ends_with("a.hprof"))
                .map(|(_, args)| args["id"].as_str().unwrap().to_string())
                .expect("a.hprof open call recorded");
            let closes: Vec<_> = calls.iter().filter(|(n, _)| n == "close_heap_dump").collect();
            assert_eq!(closes.len(), 1, "evicted session closed upstream");
            assert_eq!(closes[0].1["id"].as_str().unwrap(), a_open_id);
        }
        assert!(matches!(
            mgr.query(&paths[0], "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
    }

    #[tokio::test]
    async fn test_query_requires_ready_and_reports_warming() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        // 未打开
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
        // 预热中
        let mgr2 = mgr.clone();
        let a2 = a.clone();
        let h = tokio::spawn(async move {
            mgr2.open(SID, &a2, 30).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: true })
        ));
        h.await.unwrap();
    }

    #[tokio::test]
    async fn test_query_routes_and_injects_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        mgr.query(&a, "get_class_histogram", &serde_json::json!({"limit": 5}), 5)
            .await
            .unwrap();
        let calls = mock.calls.lock().await;
        let open_id = calls[0].1["id"].as_str().unwrap();
        assert_eq!(calls[1].0, "get_class_histogram");
        assert_eq!(calls[1].1["id"].as_str().unwrap(), open_id);
        assert_eq!(calls[1].1["limit"], 5);
    }

    #[tokio::test]
    async fn test_query_upstream_tool_error_keeps_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "MAT error: bad query".into(), is_error: true })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        match mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await {
            Err(ManagerError::Upstream(text)) => assert!(text.contains("MAT error")),
            other => panic!("expected Upstream, got {other:?}"),
        }
        // 会话仍有效：open 命中缓存（无新增上游 open 调用）
        assert_eq!(open_ready(&mgr, &a).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "open_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_query_transport_error_invalidates_and_respawns() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Err("transport closed".to_string())
                }
            }
        }));
        let (mgr, spawns) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::Unavailable(_))
        ));
        // 会话已全部失效 → 再查是 NotOpen 而非 Unavailable
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "dead worker shut down");
        // 重新 open → 工厂重新拉起
        open_ready(&mgr, &a).await;
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_open_task_factory_failure_surfaces_error() {
        let tmp = tempfile::tempdir().unwrap();
        let factory: ClientFactory = Arc::new(|_xmx| {
            Box::pin(async { Err(ManagerError::JavaMissing("no java".into())) })
        });
        let mgr = HeapAnalyzerManager::new(
            factory,
            EventBus::disabled(),
            tmp.path().to_path_buf(),
            ManagerConfig::default(),
        );
        let a = dump_file(tmp.path(), "a.hprof");
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
        // Failed 条目可重试（再次 open 仍是同错误，不死循环）
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
    }

    #[tokio::test]
    async fn test_timeout_does_not_kill_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let hist_calls = Arc::new(AtomicUsize::new(0));
        let hc = hist_calls.clone();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(move |name, _args| {
            let name = name.to_string();
            let hc = hc.clone();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else if name == "get_class_histogram" && hc.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    Ok(CallOutcome { text: "hist".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "hist".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(matches!(
            mgr.query(&a, "get_class_histogram", &serde_json::json!({}), 1).await,
            Err(ManagerError::Timeout(1))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 0, "timeout must NOT kill worker");
        // 会话未被破坏：再次查询（快速路径）成功
        mgr.query(&a, "get_class_histogram", &serde_json::json!({}), 5)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_close_removes_and_calls_upstream_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(mgr.close(&a, 5).await.unwrap());
        {
            let calls = mock.calls.lock().await;
            assert_eq!(calls.iter().filter(|(n, _)| n == "close_heap_dump").count(), 1);
        }
        // 幂等：再次 close 返回 false 且不再上游调用
        assert!(!mgr.close(&a, 5).await.unwrap());
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "close_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_idle_exit_shuts_down_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, spawns) = manager_with(
            &mock,
            tmp.path(),
            ManagerConfig {
                idle_timeout: std::time::Duration::from_millis(150),
                idle_tick: std::time::Duration::from_millis(20),
            },
        );
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        mgr.close(&a, 5).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "idle worker must exit");
        // 空闲期间有会话则不退出
        let b = dump_file(tmp.path(), "b.hprof");
        open_ready(&mgr, &b).await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "worker with open session must stay");
        mgr.close(&b, 5).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 2);
        // 退出后再 open → 工厂重新拉起
        open_ready(&mgr, &a).await;
        assert_eq!(spawns.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_warm_up_opens_in_background() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        mgr.warm_up(SID, &a).await;
        // warm_up 完成后 open 命中缓存（无新增上游调用）
        assert_eq!(open_ready(&mgr, &a).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "open_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_close_for_friday_session_scoped_to_artifacts_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join("artifacts");
        let sid1 = "11111111-1111-1111-1111-111111111111";
        let sid2 = "22222222-2222-2222-2222-222222222222";
        let dir1 = crate::tools::builtin::run_command::artifact_dir_for(&artifacts, sid1);
        let dir2 = crate::tools::builtin::run_command::artifact_dir_for(&artifacts, sid2);
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir1.join("a.hprof"), "fake").unwrap();
        std::fs::write(dir2.join("b.hprof"), "fake").unwrap();

        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, &artifacts, ManagerConfig::default());
        open_ready(&mgr, &dir1.join("a.hprof")).await;
        open_ready(&mgr, &dir2.join("b.hprof")).await;

        mgr.close_for_friday_session(sid1).await;
        {
            let calls = mock.calls.lock().await;
            let closes: Vec<_> = calls.iter().filter(|(n, _)| n == "close_heap_dump").collect();
            assert_eq!(closes.len(), 1, "only sid1's dump closed");
            let closed_id = closes[0].1["id"].as_str().unwrap();
            let a_open_id = calls
                .iter()
                .find(|(n, args)| n == "open_heap_dump" && args["path"].as_str().unwrap().contains("a.hprof"))
                .map(|(_, args)| args["id"].as_str().unwrap().to_string())
                .unwrap();
            assert_eq!(closed_id, a_open_id);
        }
        // sid2 的 dump 仍可查询
        mgr.query(&dir2.join("b.hprof"), "get_leak_suspects", &serde_json::json!({}), 5)
            .await
            .unwrap();
        // sid1 的不可
        assert!(matches!(
            mgr.query(&dir1.join("a.hprof"), "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
    }
}
