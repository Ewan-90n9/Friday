mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod tools;

use app::events::EventBus;
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
}

pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let guard = infra::logging::init(data_dir.clone());
            let filter_handle = guard.filter_handle();
            let pool = tauri::async_runtime::block_on(infra::db::init(data_dir))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
                agents: Arc::new(Mutex::new(HashMap::new())),
                filter_handle,
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
        let guard = logging::init(tmp.path().to_path_buf());
        let handle = guard.filter_handle();
        let result = logging::set_level(&handle, "info");
        assert!(result.is_ok());
    }
}
