use super::session::{Session, SessionId};
use serde::{Deserialize, Serialize};
use tauri::State;

pub struct LifecycleManager;

impl LifecycleManager {
    pub fn new() -> Self {
        Self
    }

    pub async fn start_diagnosis(
        &self,
        _session: Session,
        _prompt: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        todo!()
    }

    pub async fn stop_agent(&self, _session_id: &SessionId) {
        todo!()
    }

    pub async fn close_session(&self, _session_id: &SessionId) {
        todo!()
    }

    pub async fn confirm_tool(&self, _session_id: &SessionId, _tool: &str) {
        todo!()
    }

    pub async fn cancel_diagnosis(&self, _session_id: &SessionId) {
        todo!()
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

#[tauri::command]
pub async fn start_diagnosis_cmd(
    _state: State<'_, crate::AppState>,
    env: String,
    service: String,
    symptom: String,
) -> Result<String, String> {
    let _params = StartDiagnosisParams { env, service, symptom };
    todo!()
}

#[tauri::command]
pub async fn stop_agent_cmd(
    _state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    let _ = session_id;
    todo!()
}

#[tauri::command]
pub async fn close_session_cmd(
    _state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    let _ = session_id;
    todo!()
}

#[tauri::command]
pub async fn confirm_tool_cmd(
    _state: State<'_, crate::AppState>,
    session_id: String,
    tool: String,
) -> Result<(), String> {
    let _ = (session_id, tool);
    todo!()
}

#[tauri::command]
pub async fn cancel_diagnosis_cmd(
    _state: State<'_, crate::AppState>,
    session_id: String,
) -> Result<(), String> {
    let _ = session_id;
    todo!()
}
