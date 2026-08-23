use crate::tools::risk::RiskLevel;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    AgentStarted {
        session_id: String,
        agent_pid: u32,
    },
    ToolExecuting {
        session_id: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        session_id: String,
        tool: String,
        output: serde_json::Value,
        elapsed_ms: u64,
    },
    LlmThinking {
        session_id: String,
        token: String,
    },
    ConfirmRequired {
        session_id: String,
        confirm_id: String,
        tool: String,
        args: serde_json::Value,
        risk_level: RiskLevel,
    },
    AgentStopped {
        session_id: String,
    },
    AgentCrashed {
        session_id: String,
        reason: String,
    },
    DiagnosisDone {
        session_id: String,
        conclusion: String,
    },
    SessionClosed {
        session_id: String,
    },
    SessionDeleted {
        session_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventPayload {
    pub session_id: String,
    pub event: AppEvent,
}

#[derive(Clone)]
pub struct EventBus {
    handle: AppHandle,
}

impl EventBus {
    pub fn new(handle: AppHandle) -> Self {
        Self { handle }
    }

    pub fn emit(&self, session_id: &str, event: AppEvent) {
        tracing::debug!(
            session_id = %session_id,
            event_type = ?std::mem::discriminant(&event),
            "emitting event"
        );
        let payload = EventPayload {
            session_id: session_id.to_string(),
            event,
        };
        if let Err(e) = self.handle.emit("app_event", payload) {
            tracing::error!(?e, "failed to emit event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_event_serialization() {
        let event = AppEvent::AgentStarted {
            session_id: "s1".to_string(),
            agent_pid: 123,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("agent_started"));
        assert!(json.contains("s1"));
        assert!(json.contains("123"));
    }

    #[test]
    fn test_agent_stopped_serialization() {
        let event = AppEvent::AgentStopped {
            session_id: "s42".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("agent_stopped"));
        assert!(json.contains("s42"));
    }

    #[test]
    fn test_confirm_required_serialization() {
        let event = AppEvent::ConfirmRequired {
            session_id: "s1".to_string(),
            confirm_id: "c1".to_string(),
            tool: "arthas trace".to_string(),
            args: serde_json::json!({"class": "com.example.Foo"}),
            risk_level: RiskLevel::Low,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("confirm_required"));
        assert!(json.contains("low"));
        assert!(json.contains("c1"));
    }

    #[test]
    fn test_session_deleted_serialization() {
        let event = AppEvent::SessionDeleted {
            session_id: "s99".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("session_deleted"));
        assert!(json.contains("s99"));
    }
}
