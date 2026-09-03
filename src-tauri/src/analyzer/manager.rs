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

#[derive(Debug, Clone)]
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
    /// reaper 只在首个工人进程拉起时 spawn 一次（new() 无 runtime 上下文，禁止 tokio::spawn）
    reaper_spawned: bool,
}

/// 会话 phase 订阅者类型别名（open 等待用）
type PhaseRx = tokio::sync::watch::Receiver<EntryPhase>;

/// -Xmx 预算：dump 大小 × 1.5，向上取整 GB，clamp [4, 12]
pub fn xmx_gb_for(dump_size_bytes: u64) -> u32 {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let need = dump_size_bytes as f64 * 1.5;
    ((need / GB).ceil() as u32).clamp(4, 12)
}

/// 去除 Windows verbatim 路径前缀（`\\?\` 与 `\\?\UNC\`）。
/// Java/MAT 的文件 API 不支持 verbatim 前缀（`java -jar \\?\...` 会
/// ClassNotFoundException，见 issue #6）；Tauri 的 resource_dir() 在
/// Windows 上可能返回 verbatim 路径，任何要传给 JVM 工人进程的路径都须先归一化。
pub fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// 规范化 dump 路径作为会话主键：转绝对路径；Windows 下统一反斜杠分隔符与小写盘符。
/// 消除调用方复述路径时的变体（正/反斜杠、盘符大小写、verbatim 前缀）导致的会话 miss。
pub fn normalize_dump_path(p: &Path) -> PathBuf {
    let stripped = strip_verbatim_prefix(p);
    let abs = std::path::absolute(&stripped).unwrap_or(stripped);
    if cfg!(windows) {
        let s = abs.to_string_lossy().replace('/', "\\");
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let mut out = String::with_capacity(s.len());
            out.push((bytes[0] as char).to_ascii_lowercase());
            out.push_str(&s[1..]);
            return PathBuf::from(out);
        }
        PathBuf::from(s)
    } else {
        abs
    }
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
                reaper_spawned: false,
            })),
            spawn_lock: Arc::new(tokio::sync::Mutex::new(())),
            client_factory,
            bus,
            artifacts_dir,
            config: config.clone(),
        };
        mgr
    }

    /// 打开 dump（MAT 建索引）。Ready 命中秒回（缓存 summary）；Warming 合流等待；
    /// Failed 重试。检查与 begin 在同一锁内完成（并发安全去重）。
    /// 会话主键在入口统一规范化（工具层 / warm_up 传入的路径变体在此归一）。
    pub async fn open(
        &self,
        session_id: &str,
        path: &Path,
        timeout_secs: u64,
    ) -> Result<OpenOutcome, ManagerError> {
        let normalized = normalize_dump_path(path);
        let path: &Path = &normalized;
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
        let normalized = normalize_dump_path(path);
        let path: &Path = &normalized;
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

        debug_assert!(upstream_args.get("id").is_none(), "query injects analyzer session id");
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
        let normalized = normalize_dump_path(path);
        let path: &Path = &normalized;
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
        // 与 open() 的主键规范化保持一致：目录前缀匹配同样基于规范化路径
        let dir = normalize_dump_path(&crate::tools::builtin::run_command::artifact_dir_for(&self.artifacts_dir, session_id));
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
            for (path, analyzer_id) in removed {
                let res = tokio::time::timeout(
                    Duration::from_secs(CLOSE_TIMEOUT_SECS),
                    client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
                )
                .await;
                match res {
                    Err(_) => tracing::warn!(
                        session_id, dump = %path.display(),
                        "heap analyzer close timed out"
                    ),
                    Ok(Err(e)) => tracing::warn!(
                        session_id, dump = %path.display(), error = %e,
                        "heap analyzer close failed"
                    ),
                    Ok(Ok(o)) if o.is_error => tracing::warn!(
                        session_id, dump = %path.display(), text = %o.text,
                        "heap analyzer close upstream error"
                    ),
                    _ => {}
                }
            }
        }
    }

    // ── 内部 ──

    /// open 的后台任务：ensure client → 上游 open → 落定 phase。
    async fn run_open_task(&self, path: &Path, analyzer_id: String, dump_size: u64) {
        let xmx_gb = xmx_gb_for(dump_size);
        let phase = match self.ensure_client(xmx_gb).await {
            Err(e) => {
                tracing::warn!(dump = %path.display(), error = %e, "heap analyzer open: ensure client failed");
                EntryPhase::Failed { error: e }
            }
            Ok(client) => {
                let args = serde_json::json!({ "path": path.to_string_lossy(), "id": analyzer_id });
                let result = tokio::time::timeout(
                    Duration::from_secs(OPEN_TASK_TIMEOUT_SECS),
                    client.call_tool("open_heap_dump", &args),
                )
                .await;
                match result {
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
                }
            }
        };
        self.settle_open_phase(path, &analyzer_id, phase).await;
    }

    /// open 任务结果落定。条目已消失（close/逐出/被重试覆盖）时：
    /// Ready 结果对应的上游会话已成孤儿（对 LRU/reaper 不可见），必须主动释放；
    /// Failed 结果无需释放（上游会话未建立，或已随 invalidate/失败处理）。
    async fn settle_open_phase(&self, path: &Path, analyzer_id: &str, phase: EntryPhase) {
        let was_ready = matches!(phase, EntryPhase::Ready { .. });
        if self.finish_phase(path, analyzer_id, phase).await {
            return;
        }
        if was_ready {
            tracing::warn!(
                dump = %path.display(),
                "open task completed but session was closed meanwhile, releasing orphaned analyzer session"
            );
            self.close_upstream_quietly(analyzer_id).await;
        } else {
            tracing::debug!(
                dump = %path.display(),
                "open task failed but session was closed meanwhile, nothing to release"
            );
        }
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

    /// 确保 MAT 工人进程客户端存在（不存在则经工厂拉起）。
    /// 首次拉起时启动 idle reaper（此处必在 async 上下文中运行）。
    async fn ensure_client(&self, xmx_gb: u32) -> Result<Arc<dyn HeapAnalyzerClient>, ManagerError> {
        // 注意（Task 8）：工人 -Xmx 由首个 open 的 dump 大小定档，后续更大的 dump 复用同一进程时可能 OOM（表现为 Upstream 错误），若成为实际问题需支持按需重启/分档。
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
        let mut spawn_reaper = false;
        {
            let mut inner = self.inner.lock().await;
            inner.client = Some(client.clone());
            inner.last_active = Instant::now();
            if !inner.reaper_spawned {
                inner.reaper_spawned = true;
                spawn_reaper = true;
            }
        }
        if spawn_reaper {
            self.spawn_idle_reaper();
        }
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

    /// 落定 phase（见 DumpSessions::set_phase）。返回是否命中条目（false = 条目已被
    /// close/逐出，或已被重试覆盖，写入被丢弃）。
    async fn finish_phase(&self, path: &Path, analyzer_id: &str, phase: EntryPhase) -> bool {
        let mut inner = self.inner.lock().await;
        let matched = inner.sessions.set_phase(path, analyzer_id, phase);
        inner.last_active = Instant::now();
        matched
    }

    /// 尽力关闭上游会话（不传播错误，但按日志规范记录失败原因）
    async fn close_upstream_quietly(&self, analyzer_id: &str) {
        if let Some(client) = self.existing_client().await {
            let res = tokio::time::timeout(
                Duration::from_secs(CLOSE_TIMEOUT_SECS),
                client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
            )
            .await;
            match res {
                Err(_) => tracing::warn!(analyzer_id, "heap analyzer close timed out"),
                Ok(Err(e)) => tracing::warn!(analyzer_id, error = %e, "heap analyzer close failed"),
                Ok(Ok(o)) if o.is_error => {
                    tracing::warn!(analyzer_id, text = %o.text, "heap analyzer close upstream error")
                }
                _ => {}
            }
        }
    }

    /// 空闲巡检任务：无会话、无调用且超过 idle_timeout 后关闭工人进程。
    /// 由 ensure_client 在首个客户端拉起后启动（每份共享状态恰一次），
    /// new() 不再调用——Tauri setup 是同步上下文，无 tokio runtime 可用。
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

/// 传输完成钩子：下载的 .hprof 完成后触发自动预热（lib.rs 注入 TransferManager）。
/// 其余扩展名直接忽略；预热失败只记事件，不影响传输终态。
pub fn download_complete_hook(manager: &Arc<HeapAnalyzerManager>) -> crate::transfer::DownloadCompleteHook {
    let mgr = manager.clone();
    Arc::new(move |state: &crate::transfer::state::TransferState| {
        let is_hprof = state
            .local_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("hprof"))
            .unwrap_or(false);
        if !is_hprof {
            return;
        }
        tracing::debug!(transfer_id = %state.id, dump = %state.local_path.display(), session_id = %state.session_id, "heap dump download complete, warming up analysis");
        let mgr = mgr.clone();
        let session_id = state.session_id.clone();
        let path = state.local_path.clone();
        tokio::spawn(async move {
            mgr.warm_up(&session_id, &path).await;
        });
    })
}

/// vendored 分析器 JAR 文件名（scripts/fetch-analyzer-jar.ps1 下载）
pub const ANALYZER_JAR_NAME: &str = "jvm-heap-dump-mcp-0.2.0-all.jar";

/// 生产 client 工厂：Java 探测（Ok 结果进程内缓存）→ stdio 子进程 MCP client。
/// jar 缺失（未跑 fetch 脚本）→ Unavailable 引导。
pub fn production_client_factory(jar_path: Option<PathBuf>) -> ClientFactory {
    Arc::new(move |xmx_gb: u32| {
        let jar = jar_path.clone();
        Box::pin(async move {
            static JAVA_CACHE: std::sync::OnceLock<crate::analyzer::java::JavaInfo> = std::sync::OnceLock::new();
            let java = match JAVA_CACHE.get() {
                Some(j) => j.clone(),
                None => match crate::analyzer::java::detect_java().await {
                    Ok(info) => {
                        let _ = JAVA_CACHE.set(info.clone());
                        info
                    }
                    Err(e) => return Err(ManagerError::JavaMissing(e)),
                },
            };
            let jar = jar.ok_or_else(|| {
                ManagerError::Unavailable(
                    "分析器 JAR 缺失（resources/analyzer/）。请运行 scripts/fetch-analyzer-jar.ps1 后重启。"
                        .to_string(),
                )
            })?;
            match crate::analyzer::client::spawn_analyzer_client(&java, &jar, xmx_gb).await {
                Ok(c) => {
                    let c: Arc<dyn HeapAnalyzerClient> = Arc::new(c);
                    Ok(c)
                }
                Err(e) => Err(ManagerError::Unavailable(e)),
            }
        })
    })
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

    #[test]
    fn test_manager_new_outside_tokio_runtime_does_not_panic() {
        // 回归：lib.rs 的 Tauri setup 是同步上下文，new() 不得依赖运行时
        let factory: ClientFactory = Arc::new(|_xmx| {
            Box::pin(async { Err(ManagerError::Unavailable("x".into())) })
        });
        let tmp = tempfile::tempdir().unwrap();
        let _mgr = HeapAnalyzerManager::new(
            factory,
            EventBus::disabled(),
            tmp.path().to_path_buf(),
            ManagerConfig::default(),
        );
    }

    #[test]
    fn test_normalize_dump_path_forms() {
        #[cfg(windows)]
        {
            // 正/反斜杠与盘符大小写变体归一为同一主键形式（小写盘符 + 反斜杠）
            let a = normalize_dump_path(Path::new("C:/Foo/Bar.hprof"));
            assert_eq!(a.to_string_lossy(), "c:\\Foo\\Bar.hprof");
            assert_eq!(a, normalize_dump_path(Path::new("c:\\Foo\\Bar.hprof")));
            // verbatim 前缀（Tauri resource_dir 等 API 可能返回）必须剥掉：
            // Java/MAT 文件 API 不支持 \\?\ 前缀
            assert_eq!(
                normalize_dump_path(Path::new(r"\\?\C:\Foo\Bar.hprof")).to_string_lossy(),
                "c:\\Foo\\Bar.hprof"
            );
            assert_eq!(
                normalize_dump_path(Path::new(r"\\?\UNC\server\share\Bar.hprof")).to_string_lossy(),
                "\\\\server\\share\\Bar.hprof"
            );
        }
        #[cfg(not(windows))]
        {
            // 非 Windows：仅转绝对路径，分隔符不动
            assert!(normalize_dump_path(Path::new("foo/bar.hprof")).is_absolute());
        }
    }

    #[test]
    fn test_strip_verbatim_prefix_forms() {
        // 普通路径原样返回
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"C:\foo\bar.jar")),
            PathBuf::from(r"C:\foo\bar.jar")
        );
        // 盘符 verbatim → 去前缀
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\foo\bar.jar")),
            PathBuf::from(r"C:\foo\bar.jar")
        );
        // UNC verbatim → 还原为 \\server\share 形式
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\bar.jar")),
            PathBuf::from(r"\\server\share\bar.jar")
        );
        // 非 Windows 分隔符路径不受影响
        assert_eq!(
            strip_verbatim_prefix(Path::new("/foo/bar.jar")),
            PathBuf::from("/foo/bar.jar")
        );
    }

    #[tokio::test]
    async fn test_path_variants_share_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        // 分隔符风格变体（正/反斜杠互换）必须命中同一会话：第二次 open 走 Ready 缓存
        let variant = PathBuf::from(a.to_string_lossy().replace('\\', "/"));
        assert_eq!(open_ready(&mgr, &variant).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(
            calls.iter().filter(|(n, _)| *n == "open_heap_dump").count(),
            1,
            "path variant must hit the Ready cache instead of starting a second upstream open"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_path_drive_case_variant_shares_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        let s = a.to_string_lossy().to_string();
        let bytes = s.as_bytes();
        if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
            return; // 非「盘符:」形式（如 UNC 路径）无盘符大小写变体可测
        }
        let flipped = if bytes[0].is_ascii_uppercase() {
            bytes[0].to_ascii_lowercase()
        } else {
            bytes[0].to_ascii_uppercase()
        };
        let variant = format!("{}{}", flipped as char, &s[1..]);
        assert_ne!(variant, s, "variant must differ from the canonical path");
        assert_eq!(open_ready(&mgr, Path::new(&variant)).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(
            calls.iter().filter(|(n, _)| *n == "open_heap_dump").count(),
            1,
            "drive-case variant must hit the Ready cache instead of a second upstream open"
        );
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
            let _ = mgr2.open(SID, &a2, 30).await;
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

    /// 轮询 mock 调用记录直至谓词命中或超时（容忍调度抖动的确定性等待）
    async fn wait_for_calls(
        mock: &Arc<MockHeapAnalyzerClient>,
        pred: impl Fn(&[(String, serde_json::Value)]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let hit = {
                let calls = mock.calls.lock().await;
                pred(&calls)
            };
            if hit {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn test_open_timeout_then_attach_recovers() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    Ok(CallOutcome { text: "SUMMARY".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        // 第一次 open 1s 超时：后台任务继续跑（工人进程保留）
        assert!(matches!(
            mgr.open(SID, &a, 1).await,
            Err(ManagerError::Timeout(1))
        ));
        // 第二次 open 合流到仍在运行的 warming 任务，拿到最终结果
        let o = mgr.open(SID, &a, 30).await.expect("attach to in-flight open should recover");
        assert_eq!(o.summary, "SUMMARY");
        let calls = mock.calls.lock().await;
        assert_eq!(
            calls.iter().filter(|(n, _)| n == "open_heap_dump").count(),
            1,
            "recovery must attach to the still-running task, not start a new upstream open"
        );
    }

    #[tokio::test]
    async fn test_waiter_sees_channel_closed_on_invalidate() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        let mgr2 = mgr.clone();
        let a2 = a.clone();
        let h = tokio::spawn(async move { mgr2.open(SID, &a2, 30).await });
        assert!(
            wait_for_calls(&mock, |cs| cs.iter().any(|(n, _)| n == "open_heap_dump")).await,
            "upstream open should start"
        );
        // warming 期间 close：移除条目 → watch sender drop → 等待者看到通道关闭
        assert!(mgr.close(&a, 5).await.unwrap(), "dump was warming");
        let res = h.await.unwrap();
        assert!(
            matches!(res, Err(ManagerError::Unavailable(_))),
            "waiter must see channel closed after close during warming, got {res:?}"
        );
        // 孤儿释放（Issue 1）：任务最终 Ready 但条目已消失 → 主动 close_heap_dump。
        // 显式 close 已发一次（id 相同），孤儿释放补齐第二次。
        let first_open_id = {
            let calls = mock.calls.lock().await;
            calls
                .iter()
                .find(|(n, _)| n == "open_heap_dump")
                .map(|(_, args)| args["id"].as_str().unwrap().to_string())
                .expect("open call recorded")
        };
        assert!(
            wait_for_calls(&mock, |cs| {
                cs.iter()
                    .filter(|(n, args)| {
                        *n == "close_heap_dump" && args["id"].as_str() == Some(first_open_id.as_str())
                    })
                    .count()
                    >= 2
            })
            .await,
            "explicit close + orphan release must both fire upstream close for the open's id"
        );
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "open_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_warm_up_failure_emits_event_and_open_still_possible() {
        // EventBus 无测试捕获钩子（emit 仅走 tracing），ProvisionProgress 事件内容不做断言；
        // 此处验证 warm_up 失败不 wedge 状态：后续 open 仍可重试。
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
        // warm_up 吞掉错误（转事件/日志），不 panic
        mgr.warm_up(SID, &a).await;
        // 预热失败后 open 重试：begin 覆盖 Failed 条目，返回同一错误而非卡死/不可用
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
    }

    #[tokio::test]
    async fn test_close_during_warming_then_open_retries_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        let mgr2 = mgr.clone();
        let a2 = a.clone();
        let h = tokio::spawn(async move { mgr2.open(SID, &a2, 30).await });
        assert!(
            wait_for_calls(&mock, |cs| cs.iter().any(|(n, _)| n == "open_heap_dump")).await,
            "upstream open should start"
        );
        assert!(mgr.close(&a, 5).await.unwrap(), "dump was warming");
        let res = h.await.unwrap();
        assert!(
            matches!(res, Err(ManagerError::Unavailable(_))),
            "first open must fail with channel closed, got {res:?}"
        );
        // 重试：全新 begin + 新任务，正常完成
        let o = mgr.open(SID, &a, 30).await.expect("retry after close should succeed");
        assert_eq!(o.summary, "S");
        // 孤儿释放（Issue 1）：第一次 open 的上游会话必须被 close
        // （显式 close 一次 + 孤儿释放一次，均为第一次 open 的 id；重试会话不受影响）
        assert!(
            wait_for_calls(&mock, |cs| {
                cs.iter().filter(|(n, _)| *n == "close_heap_dump").count() >= 2
            })
            .await,
            "explicit close + orphan release should both fire upstream close"
        );
        let calls = mock.calls.lock().await;
        let opens: Vec<_> = calls.iter().filter(|(n, _)| *n == "open_heap_dump").collect();
        assert_eq!(opens.len(), 2, "exactly two upstream opens (orphaned + retry)");
        let first_id = opens[0].1["id"].as_str().unwrap();
        let second_id = opens[1].1["id"].as_str().unwrap();
        assert_ne!(first_id, second_id);
        let closes: Vec<_> = calls.iter().filter(|(n, _)| *n == "close_heap_dump").collect();
        assert_eq!(
            closes.len(),
            2,
            "explicit close + orphan release (retry session must NOT be closed)"
        );
        assert!(
            closes.iter().all(|(_, args)| args["id"].as_str().unwrap() == first_id),
            "all closes must target the first (orphaned) open's id"
        );
    }

    #[tokio::test]
    async fn test_download_complete_hook_warms_hprof_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let mgr = Arc::new(mgr);
        let hook = download_complete_hook(&mgr);

        let a = dump_file(tmp.path(), "a.hprof");
        let mut st = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            SID,
            "env-1",
            "/tmp/remote/a.hprof",
            a.clone(),
            false,
        );
        hook(&st);
        // 非 hprof 不触发
        let log = dump_file(tmp.path(), "b.log");
        st.local_path = log;
        st.id = "t2".into();
        hook(&st);

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let calls = mock.calls.lock().await;
        let opens: Vec<_> = calls.iter().filter(|(n, _)| n == "open_heap_dump").collect();
        assert_eq!(opens.len(), 1, "only the hprof must be warmed");
        assert!(opens[0].1["path"].as_str().unwrap().ends_with("a.hprof"));
    }

    /// vendoring 一致性守卫：scripts/vendor-versions.json 与 ANALYZER_JAR_NAME 必须一致。
    /// 版本升级必须同时改清单与常量，漏一处此测试即红。
    #[test]
    fn test_vendor_manifest_matches_analyzer_jar_name() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts")
            .join("vendor-versions.json");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest.display()));
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("vendor-versions.json must be valid JSON");
        let asset = v["analyzer"]["asset"].as_str().expect("analyzer.asset");
        assert_eq!(
            asset, ANALYZER_JAR_NAME,
            "scripts/vendor-versions.json 的 analyzer.asset 与 ANALYZER_JAR_NAME 漂移，二者必须同步修改"
        );
        let version = v["analyzer"]["version"].as_str().expect("analyzer.version");
        assert!(
            ANALYZER_JAR_NAME.contains(version),
            "ANALYZER_JAR_NAME 应内嵌版本 {version}"
        );
    }
}
