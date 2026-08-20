mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod tools;

use app::events::EventBus;
use tauri::Manager;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
}

pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();

            let guard = infra::logging::init(data_dir.clone());
            let pool = tauri::async_runtime::block_on(infra::db::init(data_dir))?;
            tauri::async_runtime::block_on(app::agents::detect_and_persist(&pool))?;

            app.manage(AppState {
                db: pool,
                bus: EventBus::new(handle),
            });
            app.manage(guard);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::lifecycle::start_diagnosis_cmd,
            app::lifecycle::stop_agent_cmd,
            app::lifecycle::close_session_cmd,
            app::lifecycle::confirm_tool_cmd,
            app::lifecycle::cancel_diagnosis_cmd,
            app::agents::detect_agents_cmd,
            app::agents::list_agents_cmd,
            app::agents::add_agent_cmd,
            app::agents::set_active_agent_cmd,
            app::agents::remove_agent_cmd,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
