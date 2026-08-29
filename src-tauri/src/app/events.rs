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
    ProvisionProgress {
        session_id: String,
        tool: String,
        stage: String,
        detail: String,
    },
    TransferProgress {
        session_id: String,
        transfer_id: String,
        direction: crate::transfer::state::Direction,
        status: crate::transfer::state::Status,
        transferred_bytes: u64,
        total_bytes: u64,
        speed_bps: u64,
        attempt: u32,
    },
    TransferFinished {
        session_id: String,
        transfer_id: String,
        direction: crate::transfer::state::Direction,
        status: crate::transfer::state::Status,
        transferred_bytes: u64,
        total_bytes: u64,
        error: Option<String>,
        local_path: Option<String>,
        remote_path: String,
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

#[derive(Clone, Default)]
pub struct EventBus {
    handle: Option<AppHandle>,
}

impl EventBus {
    pub fn new(handle: AppHandle) -> Self {
        Self { handle: Some(handle) }
    }

    /// 无 AppHandle 的 EventBus（测试用）：emit 只走 tracing 日志
    pub fn disabled() -> Self {
        Self { handle: None }
    }

    pub fn emit(&self, session_id: &str, event: AppEvent) {
        tracing::debug!(
            session_id = %session_id,
            event_type = ?std::mem::discriminant(&event),
            "emitting event"
        );
        let Some(handle) = &self.handle else {
            tracing::debug!(session_id, "event bus disabled, event not emitted to frontend");
            return;
        };
        let payload = EventPayload {
            session_id: session_id.to_string(),
            event,
        };
        if let Err(e) = handle.emit("app_event", payload) {
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
    fn test_event_bus_disabled_does_not_panic() {
        let bus = EventBus::disabled();
        bus.emit(
            "s1",
            AppEvent::AgentStopped { session_id: "s1".to_string() },
        );
    }

    #[test]
    fn test_provision_progress_serialization() {
        let event = AppEvent::ProvisionProgress {
            session_id: "s1".to_string(),
            tool: "jdk".to_string(),
            stage: "download".to_string(),
            detail: "channel B".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("provision_progress"));
        assert!(json.contains("jdk"));
        assert!(json.contains("download"));
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

    #[test]
    fn test_transfer_progress_serialization() {
        let event = AppEvent::TransferProgress {
            session_id: "s1".to_string(),
            transfer_id: "t1".to_string(),
            direction: crate::transfer::state::Direction::Download,
            status: crate::transfer::state::Status::Transferring,
            transferred_bytes: 100,
            total_bytes: 200,
            speed_bps: 10,
            attempt: 1,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("transfer_progress"));
        assert!(json.contains("transferring"));
        assert!(json.contains("download"));
    }

    #[test]
    fn test_transfer_finished_serialization() {
        let event = AppEvent::TransferFinished {
            session_id: "s1".to_string(),
            transfer_id: "t1".to_string(),
            direction: crate::transfer::state::Direction::Upload,
            status: crate::transfer::state::Status::Failed,
            transferred_bytes: 1,
            total_bytes: 2,
            error: Some("boom".to_string()),
            local_path: None,
            remote_path: "/tmp/x".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("transfer_finished"));
        assert!(json.contains("upload"));
        assert!(json.contains("failed"));
        assert!(json.contains("boom"));
    }
}
