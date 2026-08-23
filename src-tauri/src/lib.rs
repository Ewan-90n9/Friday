mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod mcp;
mod tools;

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

            let resource_dir = handle.path().resource_dir().ok();

            let embedding = match crate::knowledge::embedding::EmbeddingService::new(
                paths.models_dir(),
                resource_dir,
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

            // Build tool registry
            let mut tool_registry = crate::tools::registry::ToolRegistry::new();
            tool_registry.register(crate::tools::builtin::echo_tool_def());
            let tool_registry = Arc::new(tool_registry);

            // Create shared state for MCP server
            let exec_pool = Arc::new(Mutex::new(crate::exec::pool::ExecChannelPool::new()));
            let confirm_registry = Arc::new(Mutex::new(crate::tools::confirm::ConfirmRegistry::new()));
            let session_mapper = Arc::new(Mutex::new(crate::mcp::session_mapper::SessionMapper::new()));

            // Start MCP server
            let mcp_server = match crate::mcp::transport::start_mcp_server(
                tool_registry.clone(),
                exec_pool.clone(),
                confirm_registry.clone(),
                session_mapper.clone(),
                EventBus::new(handle.clone()),
                pool.clone(),
            ) {
                Ok(handle) => {
                    tracing::info!(port = handle.port, "MCP server started");

                    // Merge Friday MCP config into opencode
                    if let Some(config_path) = crate::mcp::config::default_opencode_config_path() {
                        if let Err(e) = crate::mcp::config::merge_friday_mcp_config(config_path, handle.port) {
                            tracing::warn!(?e, "failed to merge opencode config");
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
