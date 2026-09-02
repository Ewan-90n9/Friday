pub mod arthas;
pub mod ensure_tool;
pub mod file_transfer;
pub mod heap;
pub mod jvm;
pub mod list_environments;
pub mod run_command;

use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub struct EchoHandler;

#[async_trait]
impl ToolHandler for EchoHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        ToolOutput {
            success: true,
            data: serde_json::json!({
                "echo": args,
                "session_id": ctx.session_id,
            }),
            raw_stdout: None,
        }
    }
}

pub fn echo_tool_def() -> ToolDef {
    ToolDef {
        // Registered as "echo"; opencode prefixes MCP tools with the server
        // name ("friday_"), so the agent sees "friday_echo" — not a double prefix.
        name: "echo".to_string(),
        description: "Echo test tool. Returns the arguments and session_id. Used for verifying tool system connectivity.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Message to echo back"
                }
            },
            "required": ["message"]
        }),
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Builtin,
        needs_channel: false,
        handler: Arc::new(EchoHandler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo_handler_returns_args_and_session_id() {
        let handler = EchoHandler;
        let ctx = ToolContext {
            session_id: "test-session-123".to_string(),
            channel: None,
        };
        let args = serde_json::json!({"message": "hello", "session_id": "test-session-123"});

        let output = handler.execute(args, &ctx).await;

        assert!(output.success);
        assert_eq!(output.data["session_id"], "test-session-123");
        assert_eq!(output.data["echo"]["message"], "hello");
    }

    #[test]
    fn test_echo_tool_def_has_correct_metadata() {
        let def = echo_tool_def();

        assert_eq!(def.name, "echo");
        assert_eq!(def.risk_level, RiskLevel::ReadOnly);
        assert_eq!(def.category, ToolCategory::Builtin);
        assert!(!def.needs_channel);
        assert!(def.description.to_lowercase().contains("echo"));
    }
}
