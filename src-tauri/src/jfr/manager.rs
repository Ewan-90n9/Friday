use crate::analyzer::client::CallOutcome;
use crate::app::events::{AppEvent, EventBus};
use crate::jfr::client::JmcClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 预热（jfr_overview 触发上游建缓存）的内部硬超时，对齐 heap open 上限
const WARMUP_TASK_TIMEOUT_SECS: u64 = 1800;

/// vendored JMC JAR 文件名（scripts/fetch-jmc-jar.ps1 下载）
pub const JMC_JAR_NAME: &str = "jmc-mcp-1.0.0.jar";
/// JMC 工人进程堆预算（v1 常量起步，spec §2 决策 7）
pub const JMC_XMX_GB: u32 = 4;

#[derive(Debug, Clone, thiserror::Error)]
pub enum JmcError {
    #[error("{0}")]
    JavaMissing(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("JMC 调用超时（{0}s），工人进程保留未受影响")]
    Timeout(u64),
    #[error("{0}")]
    Upstream(String),
}

pub type ClientFactory = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Arc<dyn JmcClient>, JmcError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct JmcConfig {
    /// 无进行中调用持续该时长后退出工人进程
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for JmcConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}

/// JMC 工人进程管理器（全局单例，无会话层：上游 jmc-mcp-server 自带 TTL 录制缓存，
/// 所有工具直接接收 jfr_file_path；Friday 只管进程生命周期）。
#[derive(Clone)]
pub struct JmcManager {
    inner: Arc<tokio::sync::Mutex<JmcInner>>,
    spawn_lock: Arc<tokio::sync::Mutex<()>>,
    client_factory: ClientFactory,
    bus: EventBus,
    config: JmcConfig,
}

struct JmcInner {
    client: Option<Arc<dyn JmcClient>>,
    inflight: u32,
    last_active: Instant,
    /// reaper 只在首个工人进程拉起时 spawn 一次（new() 无 runtime 上下文，禁止 tokio::spawn）
    reaper_spawned: bool,
}

impl JmcManager {
    pub fn new(client_factory: ClientFactory, bus: EventBus, config: JmcConfig) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(JmcInner {
                client: None,
                inflight: 0,
                last_active: Instant::now(),
                reaper_spawned: false,
            })),
            spawn_lock: Arc::new(tokio::sync::Mutex::new(())),
            client_factory,
            bus,
            config,
        }
    }

    /// 透传调用上游工具。传输错误 → invalidate + 懒重建（下次调用经工厂重拉）。
    pub async fn query(
        &self,
        upstream_tool: &str,
        upstream_args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, JmcError> {
        let client = self.ensure_client().await?;
        match self.guarded_call(&client, upstream_tool, upstream_args, timeout_secs).await {
            Err(JmcError::Unavailable(e)) => {
                tracing::error!(tool = %upstream_tool, error = %e, "jmc worker unavailable during query, invalidating");
                self.invalidate().await;
                Err(JmcError::Unavailable(e))
            }
            other => other,
        }
    }

    /// .jfr 拉回完成后的自动预热：后台调 jfr_overview 触发上游缓存加载 +
    /// provision_progress 事件。失败只记事件，不影响传输终态与后续调用。
    pub async fn warm_up(&self, session_id: &str, path: &Path) {
        let progress = |detail: String| AppEvent::ProvisionProgress {
            session_id: session_id.to_string(),
            tool: "jfr_record".to_string(),
            stage: "analyze".to_string(),
            detail,
        };
        self.bus.emit(
            session_id,
            progress(format!(
                "JFR 拉回完成，后台分析预热开始（JMC 解析建缓存）：{}",
                path.display()
            )),
        );
        let args = serde_json::json!({ "jfr_file_path": path.to_string_lossy(), "async": false });
        match self.query("jfrOverview", &args, WARMUP_TASK_TIMEOUT_SECS).await {
            Ok(_) => self.bus.emit(
                session_id,
                progress(format!("分析就绪，jfr_* 工具可直接查询：{}", path.display())),
            ),
            Err(e) => {
                tracing::warn!(session_id, error = %e, jfr = %path.display(), "jfr warm_up failed");
                self.bus.emit(
                    session_id,
                    progress(format!("JFR 分析预热失败（不影响对话，可直接用 jfr_overview 重试）：{e}")),
                )
            }
        }
    }

    /// 显式停机（测试清理用；平时靠 idle reaper）
    pub async fn shutdown(&self) {
        let client = self.inner.lock().await.client.take();
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    // ── 内部 ──

    /// 带超时 + inflight 计数的上游调用
    async fn guarded_call(
        &self,
        client: &Arc<dyn JmcClient>,
        tool: &str,
        args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, JmcError> {
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
            Err(_) => Err(JmcError::Timeout(timeout_secs)),
            Ok(Err(e)) => Err(JmcError::Unavailable(e)),
            Ok(Ok(outcome)) if outcome.is_error => Err(JmcError::Upstream(outcome.text)),
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    /// 确保工人进程客户端存在（不存在则经工厂拉起）。
    /// 首次拉起时启动 idle reaper（此处必在 async 上下文中运行）。
    async fn ensure_client(&self) -> Result<Arc<dyn JmcClient>, JmcError> {
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
        let client = match (self.client_factory)().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "jmc worker spawn failed (factory)");
                return Err(e);
            }
        };
        tracing::info!("jmc worker process started");
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

    /// 工人进程失效：摘除客户端 + 尽力 shutdown（无会话表可清）
    async fn invalidate(&self) {
        let client = {
            let mut inner = self.inner.lock().await;
            let client = inner.client.take();
            inner.last_active = Instant::now();
            client
        };
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    /// 空闲巡检任务：无进行中调用且超过 idle_timeout 后关闭工人进程。
    /// 由 ensure_client 在首个客户端拉起后启动（每份共享状态恰一次）。
    fn spawn_idle_reaper(&self) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(mgr.config.idle_tick);
            loop {
                ticker.tick().await;
                let client = {
                    let mut inner = mgr.inner.lock().await;
                    let should = inner.client.is_some()
                        && inner.inflight == 0
                        && inner.last_active.elapsed() >= mgr.config.idle_timeout;
                    if should { inner.client.take() } else { None }
                };
                if let Some(client) = client {
                    tracing::info!("jmc worker idle (no inflight calls), shutting down");
                    client.shutdown().await;
                }
            }
        });
    }
}

/// 传输完成钩子：下载的 .jfr 完成后触发 JMC 预热（lib.rs 注入 TransferManager）。
/// 其余扩展名直接忽略；预热失败只记事件，不影响传输终态。
pub fn download_complete_hook(manager: &Arc<JmcManager>) -> crate::transfer::DownloadCompleteHook {
    let mgr = manager.clone();
    Arc::new(move |state: &crate::transfer::state::TransferState| {
        let is_jfr = state
            .local_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jfr"))
            .unwrap_or(false);
        if !is_jfr {
            return;
        }
        tracing::debug!(transfer_id = %state.id, jfr = %state.local_path.display(), session_id = %state.session_id, "jfr download complete, warming up analysis");
        let mgr = mgr.clone();
        let session_id = state.session_id.clone();
        let path = state.local_path.clone();
        tokio::spawn(async move {
            mgr.warm_up(&session_id, &path).await;
        });
    })
}

/// 生产 client 工厂：Java 探测（Ok 结果进程内缓存）→ stdio 子进程 MCP client。
/// jar 缺失（未跑 fetch 脚本）→ Unavailable 引导。
/// Java 阈值 21（jmc-jar.yml 已降级；若降级失败回退，需将 detect_java 的
/// 版本判断参数化并在此要求 25，spec §9）。
pub fn production_client_factory(jar_path: Option<PathBuf>) -> ClientFactory {
    Arc::new(move || {
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
                    Err(e) => return Err(JmcError::JavaMissing(e)),
                },
            };
            let jar = jar.ok_or_else(|| {
                JmcError::Unavailable(
                    "JMC JAR 缺失（resources/jmc/）。请运行 scripts/fetch-jmc-jar.ps1 后重启。"
                        .to_string(),
                )
            })?;
            match crate::jfr::client::spawn_jmc_client(&java, &jar, JMC_XMX_GB).await {
                Ok(c) => {
                    let c: Arc<dyn JmcClient> = Arc::new(c);
                    Ok(c)
                }
                Err(e) => Err(JmcError::Unavailable(e)),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jfr::client::MockJmcClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn manager_with(mock: &Arc<MockJmcClient>, config: JmcConfig) -> (JmcManager, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let mock2 = mock.clone();
        let factory: ClientFactory = Arc::new(move || {
            let mock = mock2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn JmcClient> = mock;
                Ok(c)
            })
        });
        (JmcManager::new(factory, EventBus::disabled(), config), spawns)
    }

    #[tokio::test]
    async fn test_query_lazy_spawns_once() {
        let mock = Arc::new(MockJmcClient::ok("OVERVIEW"));
        let (mgr, spawns) = manager_with(&mock, JmcConfig::default());
        let out = mgr
            .query("jfrOverview", &serde_json::json!({"jfr_file_path": "a.jfr"}), 5)
            .await
            .expect("query should succeed");
        assert_eq!(out.text, "OVERVIEW");
        mgr.query("jfrRules", &serde_json::json!({"jfr_file_path": "a.jfr"}), 5)
            .await
            .unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "worker must spawn exactly once");
    }

    #[tokio::test]
    async fn test_query_upstream_error_kept_as_upstream() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Ok(CallOutcome { text: "bad jfr file".into(), is_error: true })
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        match mgr.query("jfrOverview", &serde_json::json!({}), 5).await {
            Err(JmcError::Upstream(text)) => assert!(text.contains("bad jfr file")),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_transport_error_invalidates_and_respawns() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Err("transport closed".to_string())
        }));
        let (mgr, spawns) = manager_with(&mock, JmcConfig::default());
        assert!(matches!(
            mgr.query("jfrOverview", &serde_json::json!({}), 5).await,
            Err(JmcError::Unavailable(_))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "dead worker shut down");
        // 下次调用懒重建（再失败但工厂已再次拉起）
        assert!(matches!(
            mgr.query("jfrOverview", &serde_json::json!({}), 5).await,
            Err(JmcError::Unavailable(_))
        ));
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_timeout_does_not_kill_worker() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let mock = Arc::new(MockJmcClient::with_fn(move |_name, _args| {
            let c2 = c2.clone();
            async move {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ok(CallOutcome { text: "ok".into(), is_error: false })
            }
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        assert!(matches!(
            mgr.query("jfrOverview", &serde_json::json!({}), 1).await,
            Err(JmcError::Timeout(1))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 0, "timeout must NOT kill worker");
        mgr.query("jfrOverview", &serde_json::json!({}), 5).await.unwrap();
    }

    #[tokio::test]
    async fn test_idle_exit_shuts_down_worker() {
        let mock = Arc::new(MockJmcClient::ok("S"));
        let (mgr, spawns) = manager_with(
            &mock,
            JmcConfig {
                idle_timeout: Duration::from_millis(150),
                idle_tick: Duration::from_millis(20),
            },
        );
        mgr.query("jfrOverview", &serde_json::json!({}), 5).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "idle worker must exit");
        // 退出后再调用 → 工厂重新拉起
        mgr.query("jfrOverview", &serde_json::json!({}), 5).await.unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_warm_up_calls_overview_in_background() {
        let mock = Arc::new(MockJmcClient::ok("SUMMARY"));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        mgr.warm_up("sid-1", Path::new("/tmp/a.jfr")).await;
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1, "warm_up issues exactly one jfr_overview");
        assert_eq!(calls[0].0, "jfrOverview");
        assert_eq!(calls[0].1["jfr_file_path"], "/tmp/a.jfr");
        assert_eq!(calls[0].1["async"], false);
    }

    #[tokio::test]
    async fn test_warm_up_failure_does_not_break_next_query() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Ok(CallOutcome { text: "corrupt".into(), is_error: true })
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        mgr.warm_up("sid-1", Path::new("/tmp/a.jfr")).await;
        // 预热失败不阻断：后续 query 照常透传上游错误
        assert!(matches!(
            mgr.query("jfrOverview", &serde_json::json!({"jfr_file_path": "/tmp/a.jfr"}), 5).await,
            Err(JmcError::Upstream(_))
        ));
    }

    #[test]
    fn test_jmc_manager_new_outside_tokio_runtime_does_not_panic() {
        // 回归：lib.rs 的 Tauri setup 是同步上下文，new() 不得依赖运行时
        let factory: ClientFactory = Arc::new(|| {
            Box::pin(async { Err(JmcError::Unavailable("x".into())) })
        });
        let _mgr = JmcManager::new(factory, EventBus::disabled(), JmcConfig::default());
    }

    /// hook 扩展名分发：.jfr 触发预热，.hprof/其他不触发
    #[tokio::test]
    async fn test_download_complete_hook_only_fires_for_jfr() {
        let mock = Arc::new(MockJmcClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        let hook = download_complete_hook(&Arc::new(mgr.clone()));
        let mk = |name: &str| {
            crate::transfer::state::TransferState::new(
                crate::transfer::state::Direction::Download,
                "sid-1",
                "env-1",
                "/tmp/r.jfr",
                PathBuf::from(format!("C:/tmp/{name}")),
                false,
            )
        };
        hook(&mk("a.jfr"));
        hook(&mk("b.hprof"));
        hook(&mk("c.txt"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let calls = mock.calls.lock().await;
        let overviews: Vec<_> = calls.iter().filter(|(n, _)| *n == "jfrOverview").collect();
        assert_eq!(overviews.len(), 1, "only .jfr triggers warm_up, calls: {calls:?}");
    }

    /// vendoring 一致性守卫（同 analyzer/arthas）：清单与 JMC_JAR_NAME 必须一致。
    #[test]
    fn test_vendor_manifest_matches_jmc_jar_name() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts")
            .join("vendor-versions.json");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest.display()));
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("vendor-versions.json must be valid JSON");
        let asset = v["jmc"]["asset"].as_str().expect("jmc.asset");
        assert_eq!(
            asset, JMC_JAR_NAME,
            "scripts/vendor-versions.json 的 jmc.asset 与 JMC_JAR_NAME 漂移，二者必须同步修改"
        );
    }

    /// vendoring 一致性守卫：jmc-jar.yml 的 pinned SHA（env.JMC_SHA，自动构建的
    /// 触发锚点）与清单 jmc.upstream_sha 必须一致——升级上游两处同步改，
    /// 漏改 workflow 则不触发自动重建，漏改清单则巡检误报。
    #[test]
    fn test_workflow_sha_matches_vendor_manifest() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts")
            .join("vendor-versions.json");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest.display()));
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("vendor-versions.json must be valid JSON");
        let upstream_sha = v["jmc"]["upstream_sha"].as_str().expect("jmc.upstream_sha");
        let workflow = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(".github")
            .join("workflows")
            .join("jmc-jar.yml");
        let wf = std::fs::read_to_string(&workflow)
            .unwrap_or_else(|e| panic!("read workflow {}: {e}", workflow.display()));
        assert!(
            wf.contains(upstream_sha),
            ".github/workflows/jmc-jar.yml 未包含清单 pin 的 upstream_sha {upstream_sha}——\
             升级上游时 workflow env.JMC_SHA 与 vendor-versions.json 必须同步修改"
        );
    }

    /// 端到端集成（spec §7.5）：真实 spawn → jfr_overview → jfr_rules。
    /// 样例 .jfr 由 jcmd 对本机 JVM 录制生成。需要本机 Java 21（不是 25——
    /// 这才是降级闸门的真实验证）、jcmd、以及 fetch 脚本已下载的 JAR。
    #[tokio::test]
    #[ignore = "requires local Java 21, jcmd and vendored JAR"]
    async fn test_real_worker_overview_and_rules() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        assert_eq!(java.major, 21, "run with Java 21 to validate the downgrade gate (found {})", java.major);
        let jcmd = java.path.parent().unwrap().join(if cfg!(windows) { "jcmd.exe" } else { "jcmd" });
        assert!(jcmd.is_file(), "jcmd not found next to java: {}", jcmd.display());
        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/jmc")
            .join(JMC_JAR_NAME);
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-jmc-jar.ps1)", jar.display());
        let tmp = tempfile::tempdir().unwrap();
        let jfr = tmp.path().join("sample.jfr");
        // 拉起一个真实 JVM 作为录制目标（-jar JMC server 阻塞等 stdin，天然长驻）
        let mut jvm = tokio::process::Command::new(&java.path)
            .arg("-jar")
            .arg(&jar)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("target JVM must spawn");
        let target_pid = jvm.id().expect("target JVM must have a pid");
        let out = std::process::Command::new(&jcmd)
            .args([
                format!("{}", target_pid),
                "JFR.start".to_string(),
                "name=friday-it".to_string(),
                "settings=default".to_string(),
                "duration=5s".to_string(),
                format!("filename={}", jfr.display()),
            ])
            .output()
            .expect("jcmd JFR.start must run");
        assert!(
            out.status.success(),
            "JFR.start failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !jfr.is_file() || std::fs::metadata(&jfr).map(|m| m.len()).unwrap_or(0) == 0 {
            assert!(std::time::Instant::now() < deadline, "recording file never materialized");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let java_f = java.clone();
        let jar_f = jar.clone();
        let factory: ClientFactory = Arc::new(move || {
            let java = java_f.clone();
            let jar = jar_f.clone();
            Box::pin(async move {
                match crate::jfr::client::spawn_jmc_client(&java, &jar, JMC_XMX_GB).await {
                    Ok(c) => Ok(Arc::new(c) as Arc<dyn JmcClient>),
                    Err(e) => Err(JmcError::Unavailable(e)),
                }
            })
        });
        let mgr = JmcManager::new(factory, EventBus::disabled(), JmcConfig::default());
        let args = serde_json::json!({ "jfr_file_path": jfr.to_string_lossy(), "async": false });
        let out = mgr.query("jfrOverview", &args, 300).await.expect("jfrOverview");
        assert!(!out.text.trim().is_empty(), "overview output should not be empty");
        let out = mgr.query("jfrRules", &args, 300).await.expect("jfrRules");
        assert!(!out.text.trim().is_empty(), "rules output should not be empty");
        mgr.shutdown().await;
        let _ = jvm.kill().await;
    }
}
