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
    let is_new_session = session_id.is_none();
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
                Some(row) if row.status == "archived" => {
                    return Err("会话已归档".to_string())
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

    let user_seq = session::next_message_seq(&pool, &friday_session_id)
        .await
        .map_err(|e| {
            tracing::error!(?e, "failed to get next message seq");
            e.to_string()
        })?;
    session::insert_message(&pool, &friday_session_id, "user", Some(&message), Some("done"), user_seq)
        .await
        .map_err(|e| {
            tracing::error!(?e, "failed to persist user message");
            e.to_string()
        })?;

    let agent_seq = session::next_message_seq(&pool, &friday_session_id)
        .await
        .map_err(|e| e.to_string())?;
    let agent_message_id = session::insert_message(
        &pool, &friday_session_id, "agent", None, Some("streaming"), agent_seq,
    )
    .await
    .map_err(|e| {
        tracing::error!(?e, "failed to create agent message record");
        e.to_string()
    })?;
    tracing::info!(agent_message_id = %agent_message_id, "created agent message record");

    // Retrieve relevant experiences for new sessions
    let experiences: Vec<crate::knowledge::experience::Experience> = if is_new_session {
        if let (Some(ref embedding), Some(ref vec_store)) = (state.embedding.as_ref(), state.vec_store.as_ref()) {
            crate::knowledge::memory::recall_experiences(&pool, embedding, vec_store, &message)
                .await
        } else {
            tracing::warn!("embedding or vec_store not available, skipping experience recall");
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Get prompt override path and spawn agent
    let prompt_override_path = state.paths.prompts_dir().join("friday.md");
    tracing::info!(
        session_id = %friday_session_id,
        experience_count = experiences.len(),
        "spawning agent"
    );
    let agent_process = match spawn_active(
        &pool,
        friday_session_id.clone(),
        message,
        agent_session_id,
        Some(prompt_override_path),
        Some(&experiences),
    )
    .await
    {
        Ok(process) => process,
        Err(e) => {
            tracing::error!(?e, "failed to spawn agent");
            if let Err(update_err) =
                crate::app::session::update_message_status(&pool, &agent_message_id, "error").await
            {
                tracing::error!(
                    ?update_err,
                    message_id = %agent_message_id,
                    "failed to update orphaned agent message status"
                );
            }
            return Err(e.to_string());
        }
    };

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
    let agent_message_id_clone = agent_message_id.clone();
    let embedding_clone = state.embedding.clone();
    let vec_store_clone = state.vec_store.clone();

    let handle = tokio::spawn(async move {
        stream::consume_stream(
            agent_process,
            bus_clone,
            session_id_clone,
            agent_message_id_clone,
            pool_clone,
            agents_clone,
            cancel_for_task,
            embedding_clone,
            vec_store_clone,
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
    include_archived: bool,
) -> Result<Vec<session::SessionRow>, String> {
    tracing::info!(include_archived, "list_sessions_cmd called");
    session::list_sessions(&state.db, include_archived)
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

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_session_messages_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<Vec<session::MessageRow>, String> {
    tracing::info!(session_id = %session_id, "get_session_messages_cmd called");
    session::get_session_messages(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn archive_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "archive_session_cmd called");
    session::archive_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn unarchive_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "unarchive_session_cmd called");
    session::unarchive_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_session_cmd(
    state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    tracing::info!(session_id = %session_id, "delete_session_cmd called");
    stop_agent_for_session(&state.agents, &session_id).await?;

    session::delete_session(&state.db, &session_id)
        .await
        .map_err(|e| e.to_string())?;

    state.bus.emit(
        &session_id,
        AppEvent::SessionDeleted {
            session_id: session_id.clone(),
        },
    );

    Ok(())
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

    #[tokio::test]
    async fn test_delete_session_removes_session() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let s = session::create_session(&pool, "to delete").await.unwrap();

        session::delete_session(&pool, &s.id.0).await.unwrap();

        let row = session::get_session(&pool, &s.id.0).await.unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn test_archive_then_unarchive_session() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let s = session::create_session(&pool, "test").await.unwrap();

        session::archive_session(&pool, &s.id.0).await.unwrap();
        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "archived");

        session::unarchive_session(&pool, &s.id.0).await.unwrap();
        let row = session::get_session(&pool, &s.id.0).await.unwrap().unwrap();
        assert_eq!(row.status, "closed");
    }
}
