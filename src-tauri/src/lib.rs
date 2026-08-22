mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
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

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
                paths,
                embedding,
                vec_store,
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
