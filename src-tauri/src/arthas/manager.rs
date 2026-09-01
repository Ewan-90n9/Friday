use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// 同时保持 attach 的 JVM 会话上限（LRU 逐出，对齐 heap analyzer 的 MAX_OPEN_DUMPS）
pub const MAX_SESSIONS: usize = 3;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("attach 失败：{0}")]
    Attach(String),
    #[error("该 JVM 尚未 attach arthas")]
    NotOpen { attaching: bool },
    #[error("arthas 调用超时（{0}s）")]
    Timeout(u64),
    #[error("{0}")]
    Upstream(String),
    #[error("arthas 通道传输错误：{0}")]
    Transport(String),
}

/// 一次上游工具调用结果
#[derive(Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

/// arthas MCP client 抽象（测试注入 mock 的 seam，对齐 HeapAnalyzerClient）
#[async_trait]
pub trait ArthasClient: Send + Sync {
    /// Err = 传输层错误（通道死亡，调用方 invalidate 会话）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    async fn shutdown(&self);
}

/// attach 资源释放句柄：HTTP stop arthas + 拆隧道（尽力而为）
#[async_trait]
pub trait ArthasStopHandle: Send + Sync {
    async fn stop(&self);
}

pub struct AttachedSession {
    pub client: Arc<dyn ArthasClient>,
    pub stop_handle: Arc<dyn ArthasStopHandle>,
    /// attach 成功的远端 arthas HTTP 端口
    pub remote_port: u16,
}

#[derive(Clone, Debug)]
pub struct AttachRequest {
    pub session_id: String,
    pub env_id: String,
    pub pid: i64,
    /// 目标机 java 可执行文件路径或 java 命令名（arthas-boot 运行需要；默认 "java"）
    pub java_bin: String,
}

pub type AttachFactory = Arc<
    dyn Fn(AttachRequest) -> Pin<Box<dyn Future<Output = Result<AttachedSession, ManagerError>> + Send>>
        + Send
        + Sync,
>;

/// 活跃端口查询句柄：返回 env_id 下 Ready 会话占用的远端 arthas 端口。
/// attach 编排（残留清理）用它排除活跃会话；以闭包形式共享 manager 内部状态，
/// 避免 manager↔factory 循环依赖（manager 持有 factory，factory 的 AttachDeps 持有本句柄）。
pub type ActivePortsFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Vec<u16>> + Send>> + Send + Sync,
>;

#[derive(Clone, Debug)]
pub struct ArthasConfig {
    /// 距最后调用超过该时长且无 inflight → 自动 stop
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for ArthasConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}

/// attach 任务内部硬超时（工厂 future 兜底；调用方超时只是不再等待）
const ATTACH_TASK_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub enum ArthasPhase {
    Attaching,
    Ready,
    Failed { error: ManagerError },
}

struct ArthasEntry {
    phase_tx: watch::Sender<ArthasPhase>,
    client: Option<Arc<dyn ArthasClient>>,
    stop_handle: Option<Arc<dyn ArthasStopHandle>>,
    last_active: Instant,
    inflight: u32,
    /// 代际令牌：条目创建时由 ManagerInner.next_task_id 分配，
    /// 仅创建它的 attach 任务在落定时可写入（防 stale 任务覆盖重开后的新条目）
    task_id: u64,
    /// attach 成功的远端 arthas HTTP 端口，残留清理时排除活跃会话
    remote_port: Option<u16>,
}

#[derive(Debug)]
pub struct OpenOutcome {
    pub env_id: String,
    pub pid: i64,
    pub summary: String,
}

pub struct ArthasManager {
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    attach_factory: AttachFactory,
    config: ArthasConfig,
}

struct ManagerInner {
    sessions: HashMap<(String, i64), ArthasEntry>,
    reaper_spawned: bool,
    next_task_id: u64,
}

/// ArthasManager 共享内部状态句柄：生产装配时先独立创建，
/// `active_ports_fn()` 交给 AttachDeps（残留清理排除活跃会话），
/// 再连同 attach factory 一起交给 `ArthasManager::with_shared_state` 构造 manager。
/// 构造顺序 shared → deps → factory → manager，解决 manager↔factory 循环依赖。
pub struct ArthasSharedState {
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
}

impl Default for ArthasSharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl ArthasSharedState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(ManagerInner {
                sessions: HashMap::new(),
                reaper_spawned: false,
                next_task_id: 0,
            })),
        }
    }

    /// 活跃端口查询句柄（与接管本状态的 manager 实时同源）
    pub fn active_ports_fn(&self) -> ActivePortsFn {
        let inner = self.inner.clone();
        Arc::new(move |env_id| {
            let inner = inner.clone();
            let env_id = env_id.to_string();
            Box::pin(async move {
                let inner = inner.lock().await;
                collect_active_ports(&inner, &env_id)
            })
        })
    }
}

/// 收集 env_id 下 Ready 会话占用的远端端口（active_remote_ports 与 ActivePortsFn 共用）
fn collect_active_ports(inner: &ManagerInner, env_id: &str) -> Vec<u16> {
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

/// 空闲回收判定（纯函数便于单测）
fn is_reapable(entry: &ArthasEntry, idle_timeout: Duration) -> bool {
    if entry.last_active.elapsed() <= idle_timeout {
        return false;
    }
    match *entry.phase_tx.borrow() {
        ArthasPhase::Ready => entry.inflight == 0,
        ArthasPhase::Failed { .. } => true, // 失败残留条目一并清理
        ArthasPhase::Attaching => false,    // attach 中的条目不回收（有 ATTACH_TASK_TIMEOUT 兜底）
    }
}

impl ArthasManager {
    pub fn new(attach_factory: AttachFactory, config: ArthasConfig) -> Self {
        Self::with_shared_state(attach_factory, config, ArthasSharedState::new())
    }

    /// 生产装配入口：接管 ArthasSharedState 的 inner（与它的 active_ports_fn 同源，
    /// attach 编排经 AttachDeps 查询的活跃端口即本 manager 的会话）
    pub fn with_shared_state(
        attach_factory: AttachFactory,
        config: ArthasConfig,
        shared: ArthasSharedState,
    ) -> Self {
        Self {
            inner: shared.inner,
            attach_factory,
            config,
        }
    }

    /// attach arthas 到 (env_id, pid)。幂等：Ready 秒回；Attaching 等待合流；
    /// 失败条目即时清除（下次 open 走全新 attach）。
    pub async fn open(
        &self,
        session_id: &str,
        env_id: &str,
        pid: i64,
        java_bin: &str,
        timeout_secs: u64,
    ) -> Result<OpenOutcome, ManagerError> {
        let mut rx = {
            let mut inner = self.inner.lock().await;
            self.ensure_reaper(&mut inner);
            let key = (env_id.to_string(), pid);
            if let Some(entry) = inner.sessions.get_mut(&key) {
                if matches!(*entry.phase_tx.borrow(), ArthasPhase::Ready) {
                    entry.last_active = Instant::now();
                    return Ok(OpenOutcome {
                        env_id: env_id.to_string(),
                        pid,
                        summary: "arthas 已就绪（复用现有 attach）".to_string(),
                    });
                }
                entry.phase_tx.subscribe()
            } else {
                // LRU：满员时逐出最久未用的 Ready 条目
                while inner.sessions.len() >= MAX_SESSIONS {
                    let victim = lru_ready_victim(&inner.sessions);
                    let Some(victim) = victim else { break };
                    if let Some(entry) = inner.sessions.remove(&victim) {
                        if let Some(stop) = entry.stop_handle {
                            tracing::info!(env_id = %victim.0, pid = victim.1, "arthas session evicted (LRU)");
                            tokio::spawn(async move { stop.stop().await; });
                        }
                    }
                }
                inner.next_task_id += 1;
                let task_id = inner.next_task_id;
                let (tx, rx) = watch::channel(ArthasPhase::Attaching);
                inner.sessions.insert(
                    key.clone(),
                    ArthasEntry {
                        phase_tx: tx,
                        client: None,
                        stop_handle: None,
                        last_active: Instant::now(),
                        inflight: 0,
                        task_id,
                        remote_port: None,
                    },
                );
                // spawn attach 任务（对齐 heap analyzer 的 run_open_task 模式）
                let inner_clone = self.inner.clone();
                let factory = self.attach_factory.clone();
                let req = AttachRequest {
                    session_id: session_id.to_string(),
                    env_id: env_id.to_string(),
                    pid,
                    java_bin: java_bin.to_string(),
                };
                tokio::spawn(async move {
                    run_attach_task(inner_clone, factory, req, task_id).await;
                });
                rx
            }
        };

        // 等待 phase 落定（调用方超时只是不再等待；任务继续跑满 ATTACH_TASK_TIMEOUT）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            // 先把 phase 克隆出本地变量：match 判别位置的 watch::Ref 临时值不能跨 await 存活
            let phase = rx.borrow().clone();
            match phase {
                ArthasPhase::Ready => {
                    return Ok(OpenOutcome {
                        env_id: env_id.to_string(),
                        pid,
                        summary: "arthas 已就绪".to_string(),
                    });
                }
                ArthasPhase::Failed { error } => {
                    // 清除失败条目，让下次 open 走全新 attach
                    self.inner.lock().await.sessions.remove(&(env_id.to_string(), pid));
                    return Err(error);
                }
                ArthasPhase::Attaching => {}
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(ManagerError::Timeout(timeout_secs));
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                // 超时
                Err(_) => return Err(ManagerError::Timeout(timeout_secs)),
                // 发送端随条目移除而 drop（等待期间被关闭/逐出）→ 引导重试
                Ok(Err(_)) => {
                    return Err(ManagerError::Attach(
                        "attach 会话已被回收，请重试 arthas_open".to_string(),
                    ));
                }
                // phase 变化，回到循环头重新读取
                Ok(Ok(())) => {}
            }
        }
    }

    /// 调用上游 arthas MCP 工具。传输错误 → invalidate 会话。
    pub async fn query(
        &self,
        env_id: &str,
        pid: i64,
        tool: &str,
        args: &Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        let client = {
            let mut inner = self.inner.lock().await;
            let key = (env_id.to_string(), pid);
            let Some(entry) = inner.sessions.get_mut(&key) else {
                return Err(ManagerError::NotOpen { attaching: false });
            };
            // 先克隆 phase，避免 watch::Ref 临时值活到块尾（E0597）
            let phase = entry.phase_tx.borrow().clone();
            match phase {
                ArthasPhase::Ready => {
                    let client = entry
                        .client
                        .clone()
                        .ok_or(ManagerError::NotOpen { attaching: false })?;
                    entry.inflight += 1;
                    entry.last_active = Instant::now();
                    client
                }
                ArthasPhase::Attaching => return Err(ManagerError::NotOpen { attaching: true }),
                ArthasPhase::Failed { .. } => return Err(ManagerError::NotOpen { attaching: false }),
            }
        };

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            client.call_tool(tool, args),
        )
        .await;

        // inflight 回落 + touch（会话可能已被并发关闭，忽略即可）
        {
            let mut inner = self.inner.lock().await;
            if let Some(entry) = inner.sessions.get_mut(&(env_id.to_string(), pid)) {
                entry.inflight = entry.inflight.saturating_sub(1);
                entry.last_active = Instant::now();
            }
        }

        match result {
            Err(_) => Err(ManagerError::Timeout(timeout_secs)),
            Ok(Err(transport)) => {
                tracing::warn!(env_id, pid, tool, error = %transport, "arthas transport error, invalidating session");
                self.invalidate(env_id, pid).await;
                Err(ManagerError::Transport(transport))
            }
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    /// 显式关闭（arthas_close 工具）。返回是否原本处于打开状态。
    pub async fn close(&self, env_id: &str, pid: i64) -> bool {
        let entry = { self.inner.lock().await.sessions.remove(&(env_id.to_string(), pid)) };
        match entry {
            Some(e) => {
                if let Some(stop) = e.stop_handle {
                    tokio::spawn(async move { stop.stop().await; });
                }
                if let Some(client) = e.client {
                    tokio::spawn(async move { client.shutdown().await; });
                }
                tracing::info!(env_id, pid, "arthas session closed");
                true
            }
            None => false,
        }
    }

    /// 关闭某环境全部会话（环境删除联动）
    pub async fn close_for_environment(&self, env_id: &str) {
        let entries: Vec<ArthasEntry> = {
            let mut inner = self.inner.lock().await;
            let keys: Vec<(String, i64)> = inner
                .sessions
                .keys()
                .filter(|(e, _)| e == env_id)
                .cloned()
                .collect();
            keys.iter().filter_map(|k| inner.sessions.remove(k)).collect()
        };
        let count = entries.len();
        for e in entries {
            if let Some(stop) = e.stop_handle {
                tokio::spawn(async move { stop.stop().await; });
            }
            if let Some(client) = e.client {
                tokio::spawn(async move { client.shutdown().await; });
            }
        }
        if count > 0 {
            tracing::info!(env_id, count, "arthas sessions closed for environment");
        }
    }

    /// 当前环境 Ready 会话占用的远端 arthas 端口（残留清理排除用）
    pub async fn active_remote_ports(&self, env_id: &str) -> Vec<u16> {
        let inner = self.inner.lock().await;
        collect_active_ports(&inner, env_id)
    }

    /// 传输错误 → 移除会话 + best-effort stop（下次 open 重新 attach）
    async fn invalidate(&self, env_id: &str, pid: i64) {
        let stop = {
            let mut inner = self.inner.lock().await;
            inner
                .sessions
                .remove(&(env_id.to_string(), pid))
                .and_then(|e| e.stop_handle)
        };
        if let Some(stop) = stop {
            tokio::spawn(async move { stop.stop().await; });
        }
    }

    /// reaper 只在首个 open 时 spawn 一次（构造在 async 上下文之外，不能 tokio::spawn）
    fn ensure_reaper(&self, inner: &mut ManagerInner) {
        if inner.reaper_spawned {
            return;
        }
        inner.reaper_spawned = true;
        let inner_clone = self.inner.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.idle_tick);
            loop {
                interval.tick().await;
                let stops: Vec<((String, i64), Arc<dyn ArthasStopHandle>)> = {
                    let mut inner = inner_clone.lock().await;
                    let keys: Vec<(String, i64)> = inner
                        .sessions
                        .iter()
                        .filter(|(_, e)| is_reapable(e, config.idle_timeout))
                        .map(|(k, _)| k.clone())
                        .collect();
                    keys.iter()
                        .filter_map(|k| inner.sessions.remove(k).map(|e| (k.clone(), e.stop_handle)))
                        .filter_map(|(k, stop)| stop.map(|s| (k, s)))
                        .collect()
                };
                for ((env_id, pid), stop) in stops {
                    tracing::info!(env_id = %env_id, pid = pid, "arthas session idle, stopping");
                    tokio::spawn(async move { stop.stop().await; });
                }
            }
        });
    }
}

/// 找最久未访问的 Ready 条目 key（Attaching/Failed 不参与逐出）
fn lru_ready_victim(sessions: &HashMap<(String, i64), ArthasEntry>) -> Option<(String, i64)> {
    sessions
        .iter()
        .filter(|(_, e)| matches!(*e.phase_tx.borrow(), ArthasPhase::Ready))
        .min_by_key(|(_, e)| e.last_active)
        .map(|(k, _)| k.clone())
}

/// attach 任务：调工厂 → 落定 phase（attach 任务自身有硬超时兜底）。
/// 落定时按 task_id 代际校验：若条目已被移除或替换（close/invalidate 后重新 open），
/// 本任务即为 stale —— 不得写入新条目，成功构建的会话资源需自行释放（孤儿回收）。
async fn run_attach_task(
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    factory: AttachFactory,
    req: AttachRequest,
    task_id: u64,
) {
    let key = (req.env_id.clone(), req.pid);
    let result = tokio::time::timeout(
        Duration::from_secs(ATTACH_TASK_TIMEOUT_SECS),
        factory(req.clone()),
    )
    .await;
    let mut inner = inner.lock().await;
    let Some(entry) = inner.sessions.get_mut(&key) else {
        // 条目已被整体移除（close/invalidate 后未重新 open）
        release_stale_attach(result, &req);
        return;
    };
    if entry.task_id != task_id {
        // 条目已被替换（close 后重新 open 生成了新条目）
        release_stale_attach(result, &req);
        return;
    }
    match result {
        Ok(Ok(attached)) => {
            entry.client = Some(attached.client);
            entry.stop_handle = Some(attached.stop_handle);
            entry.remote_port = Some(attached.remote_port);
            entry.last_active = Instant::now();
            entry.phase_tx.send_replace(ArthasPhase::Ready);
        }
        Ok(Err(e)) => {
            tracing::warn!(env_id = %req.env_id, pid = req.pid, error = %e, "arthas attach failed");
            entry.phase_tx.send_replace(ArthasPhase::Failed { error: e });
        }
        Err(_) => {
            tracing::error!(env_id = %req.env_id, pid = req.pid, "arthas attach task timed out");
            entry.phase_tx.send_replace(ArthasPhase::Failed {
                error: ManagerError::Attach(format!(
                    "attach 超时（{ATTACH_TASK_TIMEOUT_SECS}s 硬超时）"
                )),
            });
        }
    }
}

/// stale 任务落定：成功构建的会话未接入任何条目（无人接管），后台释放资源；
/// 失败/超时不持有资源，记日志后静默丢弃
fn release_stale_attach(
    result: Result<Result<AttachedSession, ManagerError>, tokio::time::error::Elapsed>,
    req: &AttachRequest,
) {
    match result {
        Ok(Ok(attached)) => {
            tracing::warn!(
                env_id = %req.env_id, pid = req.pid,
                "stale arthas attach task settled after entry removal/replacement, releasing orphaned session"
            );
            tokio::spawn(async move {
                attached.stop_handle.stop().await;
                attached.client.shutdown().await;
            });
        }
        Ok(Err(e)) => {
            tracing::warn!(
                env_id = %req.env_id, pid = req.pid, error = %e,
                "stale arthas attach task failed, discarding result"
            );
        }
        Err(_) => {
            tracing::warn!(
                env_id = %req.env_id, pid = req.pid,
                "stale arthas attach task timed out, discarding result"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockClient {
        behavior: Arc<dyn Fn(&str) -> Result<CallOutcome, String> + Send + Sync>,
    }

    #[async_trait]
    impl ArthasClient for MockClient {
        async fn call_tool(&self, name: &str, _args: &Value) -> Result<CallOutcome, String> {
            (self.behavior)(name)
        }
        async fn shutdown(&self) {}
    }

    struct MockStop {
        stops: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ArthasStopHandle for MockStop {
        async fn stop(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn ok_client() -> Arc<dyn ArthasClient> {
        Arc::new(MockClient {
            behavior: Arc::new(|_| Ok(CallOutcome { text: "ok".into(), is_error: false })),
        })
    }

    /// 计数工厂：前 fail_first 次返回 Err，之后成功；记录总调用数
    struct CountingFactory {
        calls: Arc<AtomicUsize>,
        fail_first: usize,
    }

    impl CountingFactory {
        fn into_factory(self: Arc<Self>) -> AttachFactory {
            Arc::new(move |req| {
                let f = self.clone();
                Box::pin(async move {
                    let _ = &req; // 引用 req 避免未使用告警
                    let n = f.calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if n <= f.fail_first {
                        return Err(ManagerError::Attach(format!("mock attach failure #{n}")));
                    }
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18563,
                    })
                })
            })
        }
    }

    fn always_ok_factory() -> AttachFactory {
        Arc::new(|_req| {
            Box::pin(async move {
                Ok(AttachedSession {
                    client: ok_client(),
                    stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                    remote_port: 18563,
                })
            })
        })
    }

    #[tokio::test]
    async fn test_open_then_query_roundtrip() {
        let factory = Arc::new(CountingFactory { calls: Arc::new(AtomicUsize::new(0)), fail_first: 0 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let out = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap();
        assert_eq!(out.text, "ok");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn test_open_dedupes_concurrent_opens() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_factory = calls.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let calls = calls_for_factory.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        );
        let (a, b) = tokio::join!(
            mgr.open("sess-1", "env-1", 123, "java", 30),
            mgr.open("sess-2", "env-1", 123, "java", 30),
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "concurrent opens must dedupe to one attach");
    }

    #[tokio::test]
    async fn test_query_without_open_errors() {
        let factory = Arc::new(CountingFactory { calls: Arc::new(AtomicUsize::new(0)), fail_first: 0 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: false }));
    }

    #[tokio::test]
    async fn test_query_while_attaching_reports_attaching() {
        // 工厂被信号门控：open 进入 Attaching 后挂起，期间并发查询应报 attaching
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let gate_for_factory = gate.clone();
        let mgr = Arc::new(ArthasManager::new(
            Arc::new(move |_req| {
                let gate = gate_for_factory.clone();
                Box::pin(async move {
                    gate.acquire().await.unwrap();
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        ));
        let mgr_for_task = mgr.clone();
        let open_task = tokio::spawn(async move {
            mgr_for_task.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        });
        // 等 attach 条目进入 Attaching
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: true }));
        gate.add_permits(1);
        open_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_transport_error_invalidates_session() {
        let failing_client: Arc<dyn ArthasClient> = Arc::new(MockClient {
            behavior: Arc::new(|_| Err("connection reset".to_string())),
        });
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let client = failing_client.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client,
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::Transport(_)));
        // 会话已移除：再查报 NotOpen
        let err2 = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err2, ManagerError::NotOpen { attaching: false }));
    }

    #[tokio::test]
    async fn test_open_failure_then_retry_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(CountingFactory { calls: calls.clone(), fail_first: 1 });
        let mgr = ArthasManager::new(factory.into_factory(), ArthasConfig::default());
        let err = mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap_err();
        assert!(matches!(err, ManagerError::Attach(_)));
        // 失败条目已清除：重试成功
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_close_stops_and_is_idempotent() {
        let stops = Arc::new(AtomicUsize::new(0));
        let stops_for_factory = stops.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let stops = stops_for_factory.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        assert!(mgr.close("env-1", 123).await);
        assert!(!mgr.close("env-1", 123).await); // 幂等
        // stop 由后台 spawn：轮询断言最终恰好 1 次
        let mut waited = 0;
        while stops.load(Ordering::SeqCst) == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waited += 1;
        }
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_lru_eviction_at_capacity() {
        let mgr = ArthasManager::new(always_ok_factory(), ArthasConfig::default());
        mgr.open("s", "env-1", 1, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        mgr.open("s", "env-1", 2, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        mgr.open("s", "env-1", 3, "java", 30).await.unwrap();
        mgr.open("s", "env-1", 4, "java", 30).await.unwrap(); // 逐出 pid=1
        let err = mgr.query("env-1", 1, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { .. }));
        // 其余仍在
        mgr.query("env-1", 2, "dashboard", &json!({}), 10).await.unwrap();
        mgr.query("env-1", 3, "dashboard", &json!({}), 10).await.unwrap();
        mgr.query("env-1", 4, "dashboard", &json!({}), 10).await.unwrap();
    }

    #[tokio::test]
    async fn test_idle_reaper_stops_session() {
        let config = ArthasConfig {
            idle_timeout: std::time::Duration::from_millis(80),
            idle_tick: std::time::Duration::from_millis(30),
        };
        let stops = Arc::new(AtomicUsize::new(0));
        let stops_for_factory = stops.clone();
        let mgr = ArthasManager::new(
            Arc::new(move |_req| {
                let stops = stops_for_factory.clone();
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops }),
                        remote_port: 18563,
                    })
                })
            }),
            config,
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 10).await.unwrap_err();
        assert!(matches!(err, ManagerError::NotOpen { attaching: false }));
        // stop 由 reaper 后台 spawn：轮询断言恰好 1 次
        let mut waited = 0;
        while stops.load(Ordering::SeqCst) == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waited += 1;
        }
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_query_timeout_returns_timeout() {
        struct SlowClient;
        #[async_trait]
        impl ArthasClient for SlowClient {
            async fn call_tool(&self, _n: &str, _a: &Value) -> Result<CallOutcome, String> {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Ok(CallOutcome { text: "late".into(), is_error: false })
            }
            async fn shutdown(&self) {}
        }
        let mgr = ArthasManager::new(
            Arc::new(|_req| {
                Box::pin(async move {
                    Ok(AttachedSession {
                        client: Arc::new(SlowClient),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        );
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        let err = mgr.query("env-1", 123, "watch", &json!({}), 1).await.unwrap_err();
        assert!(matches!(err, ManagerError::Timeout(_)));
    }

    #[tokio::test]
    async fn test_close_for_environment_removes_all() {
        let mgr = ArthasManager::new(always_ok_factory(), ArthasConfig::default());
        mgr.open("s", "env-1", 1, "java", 30).await.unwrap();
        mgr.open("s", "env-1", 2, "java", 30).await.unwrap();
        mgr.open("s", "env-2", 3, "java", 30).await.unwrap();
        mgr.close_for_environment("env-1").await;
        assert!(mgr.query("env-1", 1, "d", &json!({}), 5).await.is_err());
        assert!(mgr.query("env-1", 2, "d", &json!({}), 5).await.is_err());
        // 其他环境不受影响
        mgr.query("env-2", 3, "d", &json!({}), 5).await.unwrap();
    }

    #[tokio::test]
    async fn test_stale_attach_task_cannot_overwrite_reopened_entry() {
        // 场景：open#1 挂起（gate1）→ close 移除条目 → open#2（新条目，挂起在 gate2）
        // → 释放 gate1（task#1 成功）：stale 任务不得把新条目置 Ready，其资源必须被释放
        let gate1 = Arc::new(tokio::sync::Semaphore::new(0));
        let gate2 = Arc::new(tokio::sync::Semaphore::new(0));
        let gate1_for_factory = gate1.clone();
        let gate2_for_factory = gate2.clone();
        let stale_stops = Arc::new(AtomicUsize::new(0));
        let stale_stops_for_factory = stale_stops.clone();
        // 调用计数器在闭包外创建，跨 factory 调用共享：第 1 次等 gate1，第 2 次等 gate2
        let calls = Arc::new(AtomicUsize::new(0));

        let mgr = Arc::new(ArthasManager::new(
            Arc::new(move |_req| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                let gate = if n == 0 { gate1_for_factory.clone() } else { gate2_for_factory.clone() };
                let stops = stale_stops_for_factory.clone();
                Box::pin(async move {
                    gate.acquire().await.unwrap();
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops }),
                        remote_port: 18563,
                    })
                })
            }),
            ArthasConfig::default(),
        ));

        // open#1（挂起在 gate1）
        let mgr1 = mgr.clone();
        let t1 = tokio::spawn(async move { mgr1.open("s1", "env-1", 123, "java", 30).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // close 移除 Attaching 条目（open#1 的 waiter 随后因 sender drop 得到 Err）
        mgr.close("env-1", 123).await;

        // open#2（新条目，挂起在 gate2）
        let mgr2 = mgr.clone();
        let t2 = tokio::spawn(async move { mgr2.open("s2", "env-1", 123, "java", 30).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 释放 gate1：task#1 成功返回 —— 但它是 stale 的，不得把新条目置 Ready
        gate1.add_permits(1);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // t1 因条目移除而失败（Attach 错误：已回收）
        assert!(t1.await.unwrap().is_err());
        // 新条目不得被 stale 任务置 Ready：仍应 Attaching
        let err = mgr.query("env-1", 123, "dashboard", &json!({}), 1).await.unwrap_err();
        assert!(
            matches!(err, ManagerError::NotOpen { attaching: true }),
            "stale task must not mark the new entry Ready, got: {err:?}"
        );
        // stale 任务的资源必须被释放（stop 由后台 spawn，轮询等待）
        let mut waited = 0;
        while stale_stops.load(Ordering::SeqCst) == 0 && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waited += 1;
        }
        assert_eq!(stale_stops.load(Ordering::SeqCst), 1, "stale session resources must be released");

        // 释放 gate2：task#2 正常落定
        gate2.add_permits(1);
        t2.await.unwrap().unwrap();
        mgr.query("env-1", 123, "dashboard", &json!({}), 5).await.unwrap();
    }

    #[tokio::test]
    async fn test_active_remote_ports_lists_ready_sessions_only() {
        // pid 123 → 立即 attach 成功（remote_port 18563，Ready）；
        // 其他 pid → 挂在 gate 上保持 Attaching（remote_port 不得计入活跃端口）
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let gate_for_factory = gate.clone();
        let mgr = Arc::new(ArthasManager::new(
            Arc::new(move |req| {
                let gate = gate_for_factory.clone();
                Box::pin(async move {
                    if req.pid == 123 {
                        return Ok(AttachedSession {
                            client: ok_client(),
                            stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                            remote_port: 18563,
                        });
                    }
                    gate.acquire().await.unwrap();
                    Ok(AttachedSession {
                        client: ok_client(),
                        stop_handle: Arc::new(MockStop { stops: Arc::new(AtomicUsize::new(0)) }),
                        remote_port: 18564,
                    })
                })
            }),
            ArthasConfig::default(),
        ));

        // pid 123 → Ready
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        // pid 456 → Attaching（挂起在 gate 上）
        let mgr_for_task = mgr.clone();
        let attaching_task =
            tokio::spawn(async move { mgr_for_task.open("sess-2", "env-1", 456, "java", 30).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 只有 Ready 会话的端口计入
        assert_eq!(mgr.active_remote_ports("env-1").await, vec![18563]);
        // 其他环境为空
        assert!(mgr.active_remote_ports("other-env").await.is_empty());

        // 收尾：释放 gate 让挂起的 open 完成
        gate.add_permits(1);
        attaching_task.await.unwrap().unwrap();
        // 两个会话都 Ready 后，两个端口都在
        let mut ports = mgr.active_remote_ports("env-1").await;
        ports.sort();
        assert_eq!(ports, vec![18563, 18564]);
    }

    #[tokio::test]
    async fn test_shared_state_ports_fn_sees_manager_sessions() {
        // 生产装配路径：shared → active_ports_fn（进 AttachDeps）→ factory → manager 接管 shared。
        // ports_fn 必须与 manager 看到同一份会话状态（循环依赖解法：共享 inner）
        let shared = ArthasSharedState::new();
        let ports_fn = shared.active_ports_fn();
        let mgr = ArthasManager::with_shared_state(always_ok_factory(), ArthasConfig::default(), shared);

        assert!(ports_fn("env-1").await.is_empty());
        mgr.open("sess-1", "env-1", 123, "java", 30).await.unwrap();
        assert_eq!(ports_fn("env-1").await, vec![18563]);
        assert_eq!(mgr.active_remote_ports("env-1").await, vec![18563]);
        assert!(ports_fn("other-env").await.is_empty());

        // close 后端口即释放（残留清理不再排除它）
        mgr.close("env-1", 123).await;
        assert!(ports_fn("env-1").await.is_empty());
    }
}
