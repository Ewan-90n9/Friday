mod agent;
mod analyzer;
mod app;
mod arthas;
mod exec;
mod infra;
mod knowledge;
mod mcp;
mod provision;
mod tools;
mod transfer;

use app::events::EventBus;
use infra::paths::Paths;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Registry};

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
    pub embedding: Option<Arc<crate::knowledge::embedding::EmbeddingService>>,
    pub vec_store: Option<Arc<crate::knowledge::vec_store::VecStore>>,
    pub tool_registry: Arc<crate::tools::registry::ToolRegistry>,
    pub analyzer: Arc<crate::analyzer::HeapAnalyzerManager>,
    pub arthas: Arc<crate::arthas::manager::ArthasManager>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<crate::tools::confirm::ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<crate::mcp::session_mapper::SessionMapper>>,
    pub mcp_server: Option<crate::mcp::transport::McpServerHandle>,
}

pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let paths = Paths::new(data_dir.clone());
            paths.ensure_dirs()?;

            let guard = infra::logging::init(paths.log_dir());
            let filter_handle = guard.filter_handle();
            let pool = tauri::async_runtime::block_on(infra::db::init(paths.db_path()))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            // 环境多用户凭证：旧单用户数据迁移为默认凭证行（幂等）
            tauri::async_runtime::block_on(app::env_credentials::migrate_legacy(&pool));

            let resource_dir = handle.path().resource_dir().ok();

            let embedding = match crate::knowledge::embedding::EmbeddingService::new(
                paths.models_dir(),
                resource_dir.clone(),
            ) {
                Ok(e) => {
                    tracing::info!("embedding model loaded");
                    Some(Arc::new(e))
                }
                Err(e) => {
                    tracing::error!(?e, "failed to load embedding model, memory features disabled");
                    None
                }
            };

            let vec_store = match crate::knowledge::vec_store::VecStore::new(
                paths.db_path().to_str().unwrap_or("friday.db"),
            ) {
                Ok(v) => {
                    tracing::info!("vec store initialized");
                    Some(Arc::new(v))
                }
                Err(e) => {
                    tracing::error!(?e, "failed to init vec store, memory features disabled");
                    None
                }
            };

            // 堆快照分析：vendored MAT 工人进程（resources/analyzer JAR + 本机 Java 21+）
            let analyzer_jar = resource_dir.as_ref().and_then(|r| {
                let candidates = [
                    r.join("resources").join("analyzer").join(crate::analyzer::ANALYZER_JAR_NAME),
                    r.join("analyzer").join(crate::analyzer::ANALYZER_JAR_NAME),
                ];
                candidates.into_iter().find(|p| p.exists())
            });
            if analyzer_jar.is_none() {
                tracing::warn!(
                    "heap analyzer JAR missing (resources/analyzer/{}); heap_* tools will report analyzer_unavailable",
                    crate::analyzer::ANALYZER_JAR_NAME
                );
            }
            let analyzer_manager = Arc::new(crate::analyzer::HeapAnalyzerManager::new(
                crate::analyzer::production_client_factory(analyzer_jar),
                EventBus::new(handle.clone()),
                paths.artifacts_dir(),
                crate::analyzer::ManagerConfig::default(),
            ));

            // Create shared state for MCP server
            let exec_pool = Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new()));

            // SSH 隧道（direct-tcpip 本地转发）：通用基础设施（环境删除时统一清理）；
            // arthas MCP 已改走 exec HTTP 桥，后续 JMX 等复用
            let tunnels = Arc::new(crate::exec::tunnel::TunnelManager::new(pool.clone()));

            // 文件传输：TransferManager（后台异步传输引擎）+ 4 个工具；
            // heap dump 拉回完成 → 自动预热分析（钩子须在 Arc 包装前注入）
            let mut transfer_manager = crate::transfer::TransferManager::new(
                pool.clone(),
                EventBus::new(handle.clone()),
            );
            transfer_manager
                .set_download_complete_hook(crate::analyzer::download_complete_hook(&analyzer_manager));
            let transfer_manager = Arc::new(transfer_manager);

            // Build tool registry
            // JVM 语义工具共享内核与 JDK 路径缓存（ensure_tool 与 jvm_* 必须共享同一实例）
            let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
            let jvm_core = Arc::new(crate::tools::builtin::jvm::core::JvmExecCore {
                db: pool.clone(),
                exec_pool: exec_pool.clone(),
                jdk_cache: jdk_cache.clone(),
                artifacts_dir: paths.artifacts_dir(),
            });

            // arthas 工具包（vendored zip 随包分发，attach 时 SFTP 直传目标机）
            let arthas_zip = resource_dir.as_ref().and_then(|r| {
                let candidates = [
                    r.join("resources").join("arthas").join(format!("arthas-bin-{}.zip", crate::provision::arthas::ARTHAS_VERSION)),
                    r.join("arthas").join(format!("arthas-bin-{}.zip", crate::provision::arthas::ARTHAS_VERSION)),
                ];
                candidates.into_iter().find(|p| p.exists())
            });
            if arthas_zip.is_none() {
                tracing::warn!(
                    "arthas package missing (resources/arthas/arthas-bin-{}.zip); arthas attach will report vendored_package_missing",
                    crate::provision::arthas::ARTHAS_VERSION
                );
            }

            // arthas 动态诊断：共享状态先行（active_ports_fn 进 AttachDeps，manager 后接管同一份 inner；
            // 构造顺序 shared → deps → factory → manager，避免 manager↔factory 循环依赖）
            let arthas_shared = crate::arthas::manager::ArthasSharedState::new();
            let attach_deps = crate::arthas::attach::AttachDeps {
                db: pool.clone(),
                exec_pool: exec_pool.clone(),
                jdk_cache: jdk_cache.clone(),
                cache_dir: paths.cache_dir(),
                arthas_zip,
                bus: EventBus::new(handle.clone()),
                active_ports_fn: arthas_shared.active_ports_fn(),
            };
            let arthas_manager = Arc::new(crate::arthas::manager::ArthasManager::with_shared_state(
                crate::arthas::attach::production_attach_factory(attach_deps),
                crate::arthas::manager::ArthasConfig::default(),
                arthas_shared,
            ));

            let mut tool_registry = crate::tools::registry::ToolRegistry::new();
            tool_registry.register(crate::tools::builtin::echo_tool_def());
            tool_registry.register(crate::tools::builtin::run_command::run_command_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.artifacts_dir(),
            ));
            tool_registry.register(crate::tools::builtin::list_environments::list_environments_tool_def(
                pool.clone(),
            ));
            tool_registry.register(crate::tools::builtin::ensure_tool::ensure_tool_tool_def(
                pool.clone(),
                exec_pool.clone(),
                paths.cache_dir(),
                EventBus::new(handle.clone()),
                jdk_cache,
            ));
            for def in crate::tools::builtin::file_transfer::file_transfer_tool_defs(
                transfer_manager.clone(),
                paths.artifacts_dir(),
            ) {
                tool_registry.register(def);
            }
            crate::tools::builtin::jvm::register_all(
                &mut tool_registry,
                jvm_core,
                EventBus::new(handle.clone()),
                transfer_manager.clone(),
            );
            crate::tools::builtin::heap::register_all(
                &mut tool_registry,
                analyzer_manager.clone(),
                paths.artifacts_dir(),
            );
            crate::tools::builtin::arthas::register_all(
                &mut tool_registry,
                arthas_manager.clone(),
                pool.clone(),
                paths.artifacts_dir(),
            );
            let tool_registry = Arc::new(tool_registry);

            // SSH 连接池空闲清理巡检：每 60s 清理空闲超 10min 的连接。
            // 每轮清理单独 spawn：panic 只杀掉当轮任务，巡检循环继续存活。
            {
                let exec_pool_for_cleanup = exec_pool.clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        let pool = exec_pool_for_cleanup.clone();
                        tokio::spawn(async move {
                            let removed = pool.lock().await.cleanup_idle(std::time::Duration::from_secs(600)).await;
                            if removed > 0 {
                                tracing::info!(removed, "idle ssh connections cleaned");
                            }
                        });
                    }
                });
            }

            let confirm_registry = Arc::new(Mutex::new(crate::tools::confirm::ConfirmRegistry::new()));
            let session_mapper = Arc::new(Mutex::new(crate::mcp::session_mapper::SessionMapper::new()));

            // Start MCP server
            let mcp_server = match tauri::async_runtime::block_on(crate::mcp::transport::start_mcp_server(
                tool_registry.clone(),
                exec_pool.clone(),
                confirm_registry.clone(),
                session_mapper.clone(),
                EventBus::new(handle.clone()),
                pool.clone(),
            )) {
                Ok(handle) => {
                    tracing::info!(port = handle.port, "MCP server started");

                    // Merge Friday MCP config into opencode
                    if let Some(config_path) = crate::mcp::config::default_opencode_config_path() {
                        if let Err(e) = crate::mcp::config::merge_friday_mcp_config(config_path, handle.port) {
                            tracing::warn!(?e, "failed to merge opencode config");
                        }
                    }

                    // Merge Friday MCP config into codeagentcli
                    if let Some(config_path) = crate::mcp::config::default_codeagentcli_config_path() {
                        if let Err(e) = crate::mcp::config::merge_codeagentcli_mcp_config(config_path, handle.port) {
                            tracing::warn!(?e, "failed to merge codeagentcli config");
                        }
                    }

                    Some(handle)
                }
                Err(e) => {
                    tracing::error!(?e, "failed to start MCP server");
                    None
                }
            };

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle.clone()),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
                paths,
                embedding,
                vec_store,
                tool_registry,
                analyzer: analyzer_manager,
                arthas: arthas_manager,
                tunnels,
                exec_pool,
                confirm_registry,
                session_mapper,
                mcp_server,
            });
            app.manage(guard);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::lifecycle::send_message_cmd,
            app::lifecycle::stop_agent_cmd,
            app::lifecycle::close_session_cmd,
            app::lifecycle::confirm_tool_cmd,
            app::lifecycle::list_sessions_cmd,
            app::lifecycle::set_log_level_cmd,
            app::lifecycle::get_session_messages_cmd,
            app::lifecycle::archive_session_cmd,
            app::lifecycle::unarchive_session_cmd,
            app::lifecycle::delete_session_cmd,
            app::lifecycle::get_session_summary_cmd,
            app::lifecycle::list_tools_cmd,
            app::agents::detect_agents_cmd,
            app::agents::list_agents_cmd,
            app::agents::add_agent_cmd,
            app::agents::set_active_agent_cmd,
            app::agents::remove_agent_cmd,
            app::environments::list_environments_cmd,
            app::env_save::save_environment_cmd,
            app::environments::delete_environment_cmd,
            app::env_credentials::list_env_credentials_cmd,
            app::environments::test_connection_params_cmd,
            app::settings::get_artifactory_base_url_cmd,
            app::settings::set_artifactory_base_url_cmd,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::infra::logging;

    #[test]
    fn test_filter_handle_cloneable_and_usable() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let guard = logging::init(log_dir);
        let handle = guard.filter_handle();
        let result = logging::set_level(&handle, "info");
        assert!(result.is_ok());
    }
}
