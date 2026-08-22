use super::session;
use crate::agent::spawn::spawn_active;
use crate::agent::stream::{self, RunningAgent};
use crate::app::events::AppEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct LifecycleManager;

impl LifecycleManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartDiagnosisParams {
    pub env: String,
    pub service: String,
    pub symptom: String,
}

/// Stop a running agent for a session. Returns Ok(()) if no agent was running.
pub async fn stop_agent_for_session(
    agents: &Arc<Mutex<HashMap<String, RunningAgent>>>,
    session_id: &str,
) -> Result<(), String> {
    let entry = {
        let mut map = agents.lock().await;
        map.remove(session_id)
    };

    if let Some(running) = entry {
        running.cancel.cancel();
        let _ = running.handle.await;
    }
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip(state, session_id), fields(session_id))]
pub async fn send_message_cmd(
    state: State<'_, crate::AppState>,
    session_id: Option<String>,
    message: String,
) -> Result<String, String> {
    let pool = state.db.clone();
    let bus = state.bus.clone();
    let agents = state.agents.clone();

    // Determine session ID and agent session ID
    let (friday_session_id, agent_session_id) = match session_id {
        None => {
            tracing::info!("creating new session");
            let session = session::create_session(&pool, &message)
                .await
                .map_err(|e| {
                    tracing::error!(?e, "failed to create session");
                    e.to_string()
                })?;
            (session.id.0, None)
        }
        Some(id) => {
            tracing::info!(session_id = %id, "resuming existing session");
            let row = session::get_session(&pool, &id)
                .await
                .map_err(|e| e.to_string())?;
            match row {
                None => return Err("会话不存在".to_string()),
                Some(row) if row.status == "closed" => {
                    return Err("会话已关闭".to_string())
                }
                Some(_) => {}
            }
            let agent_id = session::get_agent_session_id(&pool, &id)
                .await
                .map_err(|e| e.to_string())?;
            tracing::info!(?agent_id, "found agent session id");
            (id, agent_id)
        }
    };

    tracing::Span::current().record("session_id", &tracing::field::display(&friday_session_id));

    // Check if agent is already running for this session
    {
        let map = agents.lock().await;
        if map.contains_key(&friday_session_id) {
            return Err("agent 正在运行".to_string());
        }
    }

    // Get prompt override path and spawn agent
    let prompt_override_path = state.paths.prompts_dir().join("friday.md");
    tracing::info!(
        session_id = %friday_session_id,
        "spawning agent"
    );
    let agent_process = spawn_active(
        &pool,
        friday_session_id.clone(),
        message,
        agent_session_id,
        Some(prompt_override_path),
    )
    .await
    .map_err(|e| {
        tracing::error!(?e, "failed to spawn agent");
        e.to_string()
    })?;

    let pid = agent_process.pid;
    tracing::info!(pid, session_id = %friday_session_id, "agent spawned");

    // Emit AgentStarted
    bus.emit(
        &friday_session_id,
        AppEvent::AgentStarted {
            session_id: friday_session_id.clone(),
            agent_pid: pid,
        },
    );

    // Set up cancellation and background task
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();

    let session_id_clone = friday_session_id.clone();
    let bus_clone = bus.clone();
    let pool_clone = pool.clone();
    let agents_clone = agents.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            pool_clone,
            agents_clone,
            cancel_for_task,
        )
        .await;
    });

    // Store RunningAgent
    {
        let mut map = agents.lock().await;
        map.insert(
            friday_session_id.clone(),
            RunningAgent { cancel, handle },
        );
    }

    Ok(friday_session_id)
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn stop_agent_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    stop_agent_for_session(&state.agents, &session_id).await
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn close_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    // Stop agent if running
    stop_agent_for_session(&state.agents, &session_id).await?;

    // Mark session as closed
    session::close_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())?;

    // Emit SessionClosed
    state.bus.emit(
        &session_id,
        AppEvent::SessionClosed {
            session_id: session_id.clone(),
        },
    );

    Ok(())
}

#[tauri::command]
pub async fn list_sessions_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<session::SessionRow>, String> {
    tracing::info!("list_sessions_cmd called");
    session::list_sessions(&state.db, false)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn confirm_tool_cmd(
    _state: State<'_, crate::AppState>,
    session_id: String,
    tool: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, tool = %tool, "confirm_tool_cmd called");
    Ok(())
}

#[tauri::command]
pub async fn set_log_level_cmd(
    state: State<'_, crate::AppState>,
    level: String,
) -> Result<(), String> {
    crate::infra::logging::set_level(&state.filter_handle, &level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_stop_agent_when_no_agent_running() {
        let agents: Arc<Mutex<HashMap<String, RunningAgent>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let result = stop_agent_for_session(&agents, "s1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_close_session_updates_status() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let s = session::create_session(&pool, "test").await.unwrap();
        session::close_session(&pool, &s.id.0).await.unwrap();

        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "closed");
    }
}
