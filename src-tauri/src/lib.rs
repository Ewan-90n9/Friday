mod agent;
mod app;
mod exec;
mod infra;
mod knowledge;
mod tools;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: app::events::EventBus,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
