use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    ErrorData as McpError,
    RoleServer,
    ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerInfo, Tool,
    },
    service::{MaybeSendFuture, RequestContext},
};
use tokio::sync::Mutex;

use crate::app::events::{AppEvent, EventBus};
use crate::exec::pool::ExecChannelPool;
use crate::mcp::session_mapper::SessionMapper;
use crate::tools::confirm::{ConfirmRegistry, ConfirmResult};
use crate::tools::registry::{ToolContext, ToolDef, ToolOutput, ToolRegistry};
use crate::tools::risk::RiskLevel;

pub struct FridayMcpServer {
    pub tool_registry: Arc<ToolRegistry>,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<SessionMapper>>,
    pub bus: EventBus,
    pub pool: sqlx::SqlitePool,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Inject a `session_id` string parameter into a JSON Schema's `properties`
/// and `required` arrays. Idempotent — calling twice produces the same result.
pub fn inject_session_id_param(mut schema: serde_json::Value) -> serde_json::Value {
    if schema.is_null() {
        schema = serde_json::json!({"type": "object"});
    }
    if let Some(obj) = schema.as_object_mut() {
        if !obj.contains_key("properties") {
            obj.insert("properties".to_string(), serde_json::json!({}));
        }
        if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
            if !props.contains_key("session_id") {
                props.insert(
                    "session_id".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Friday session ID for routing tool execution"
                    }),
                );
            }
        }
        if !obj.contains_key("required") {
            obj.insert("required".to_string(), serde_json::json!([]));
        }
        if let Some(required) = obj.get_mut("required").and_then(|r| r.as_array_mut()) {
            let has_session_id = required.iter().any(|v| v.as_str() == Some("session_id"));
            if !has_session_id {
                required.push(serde_json::json!("session_id"));
            }
        }
    }
    schema
}

/// Extract the `session_id` string from the optional `JsonObject` arguments
/// of a `CallToolRequestParams`.
pub fn extract_session_id(args: &Option<JsonObject>) -> Option<String> {
    args.as_ref()
        .and_then(|map| map.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Convert a `ToolOutput` (Friday's internal tool result) into an rmcp
/// `CallToolResult` with text content.
pub fn tool_output_to_result(output: ToolOutput) -> CallToolResult {
    let text =
        serde_json::to_string_pretty(&output.data).unwrap_or_else(|_| "{}".to_string());
    if output.success {
        CallToolResult::success(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::error(vec![ContentBlock::text(text)])
    }
}

/// Convert a Friday `ToolDef` into an rmcp `Tool`, injecting the
/// `session_id` parameter into the input schema.
fn tool_def_to_rmcp_tool(def: &ToolDef) -> Tool {
    let schema = inject_session_id_param(def.input_schema.clone());
    let json_object: JsonObject = match schema {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    Tool::new(
        def.name.clone(),
        def.description.clone(),
        Arc::new(json_object),
    )
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

impl ServerHandler for FridayMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info.name = "Friday".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.capabilities.tools = Some(Default::default());
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        let tools: Vec<Tool> = self
            .tool_registry
            .list()
            .into_iter()
            .map(tool_def_to_rmcp_tool)
            .collect();
        async move { Ok(ListToolsResult::with_all_items(tools)) }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_registry.get(name).map(tool_def_to_rmcp_tool)
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + MaybeSendFuture + '_ {
        async move {
            let tool_name = request.name.as_ref();

            // Extract session_id from arguments
            let session_id = extract_session_id(&request.arguments).ok_or_else(|| {
                McpError::invalid_params("session_id is required in tool arguments", None)
            })?;

            // Look up tool in registry
            let tool_def = self.tool_registry.get(tool_name).ok_or_else(|| {
                McpError::new(
                    rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                    format!("unknown tool: {}", tool_name),
                    None,
                )
            })?;

            let risk_level = tool_def.risk_level;
            let args_value = request
                .arguments
                .clone()
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            // Confirmation flow for Low/High risk tools
            if matches!(risk_level, RiskLevel::Low | RiskLevel::High) {
                let confirm_id = uuid::Uuid::new_v4().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel();

                {
                    let mut confirm_registry = self.confirm_registry.lock().await;
                    confirm_registry.insert(confirm_id.clone(), session_id.clone(), tx);
                }

                tracing::info!(
                    session_id = %session_id,
                    tool = %tool_name,
                    confirm_id = %confirm_id,
                    ?risk_level,
                    "tool call requires confirmation"
                );

                self.bus.emit(
                    &session_id,
                    AppEvent::ConfirmRequired {
                        session_id: session_id.clone(),
                        confirm_id: confirm_id.clone(),
                        tool: tool_name.to_string(),
                        args: args_value.clone(),
                        risk_level,
                    },
                );

                match tokio::time::timeout(Duration::from_secs(120), rx).await {
                    Ok(Ok(ConfirmResult::Confirmed)) => {
                        tracing::info!(session_id = %session_id, tool = %tool_name, "tool call confirmed");
                    }
                    Ok(Ok(ConfirmResult::Cancelled)) => {
                        tracing::info!(session_id = %session_id, tool = %tool_name, "tool call cancelled");
                        return Ok(CallToolResult::error(vec![ContentBlock::text(
                            "tool execution cancelled by user",
                        )])
                        .into());
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(session_id = %session_id, tool = %tool_name, "confirmation channel closed");
                        return Ok(CallToolResult::error(vec![ContentBlock::text(
                            "confirmation channel closed unexpectedly",
                        )])
                        .into());
                    }
                    Err(_) => {
                        tracing::warn!(session_id = %session_id, tool = %tool_name, "tool confirmation timed out");
                        return Ok(CallToolResult::error(vec![ContentBlock::text(
                            "tool confirmation timed out after 120 seconds",
                        )])
                        .into());
                    }
                }
            }

            // Get or create exec channel
            let channel = {
                let mut exec_pool = self.exec_pool.lock().await;
                match exec_pool.get_or_create(&session_id, &self.pool).await {
                    Ok(ch) => ch,
                    Err(e) => {
                        tracing::error!(session_id = %session_id, tool = %tool_name, error = %e, "failed to get exec channel");
                        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "failed to establish execution channel: {}",
                            e
                        ))])
                        .into());
                    }
                }
            };

            // Emit ToolExecuting event
            self.bus.emit(
                &session_id,
                AppEvent::ToolExecuting {
                    session_id: session_id.clone(),
                    tool: tool_name.to_string(),
                    args: args_value.clone(),
                },
            );

            // Execute the tool handler
            let ctx = ToolContext {
                session_id: session_id.clone(),
                channel,
            };
            let start = std::time::Instant::now();
            let output = tool_def.handler.execute(args_value.clone(), &ctx).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;

            // Emit ToolResult event
            self.bus.emit(
                &session_id,
                AppEvent::ToolResult {
                    session_id: session_id.clone(),
                    tool: tool_name.to_string(),
                    output: output.data.clone(),
                    elapsed_ms,
                },
            );

            // Persist to tool_calls table
            let call_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let status = if output.success { "success" } else { "failure" };
            let risk_str = match risk_level {
                RiskLevel::ReadOnly => "read_only",
                RiskLevel::Low => "low",
                RiskLevel::High => "high",
            };
            let args_json = serde_json::to_string(&args_value).unwrap_or_default();
            let output_json = serde_json::to_string(&output.data).unwrap_or_default();
            let error_msg: Option<String> = if output.success {
                None
            } else {
                Some("tool execution failed".to_string())
            };

            if let Err(e) = sqlx::query(
                "INSERT INTO tool_calls \
                 (id, session_id, tool_name, args, risk_level, status, output, raw_stdout, elapsed_ms, error, created_at, completed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&call_id)
            .bind(&session_id)
            .bind(tool_name)
            .bind(&args_json)
            .bind(risk_str)
            .bind(status)
            .bind(&output_json)
            .bind(&output.raw_stdout)
            .bind(elapsed_ms as i64)
            .bind(&error_msg)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
            {
                tracing::error!(session_id = %session_id, tool = %tool_name, error = %e, "failed to persist tool call");
            }

            Ok(tool_output_to_result(output).into())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_session_id_param_into_empty_schema() {
        let schema = serde_json::json!({});
        let result = inject_session_id_param(schema);

        assert_eq!(result["properties"]["session_id"]["type"], "string");
        let required = result["required"].as_array().unwrap();
        assert!(
            required.contains(&serde_json::json!("session_id")),
            "required array should contain session_id"
        );
    }

    #[test]
    fn test_inject_session_id_param_preserves_existing_properties() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            },
            "required": ["message"]
        });
        let result = inject_session_id_param(schema);

        assert_eq!(result["properties"]["message"]["type"], "string");
        assert_eq!(result["properties"]["session_id"]["type"], "string");
        let required = result["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("message")));
        assert!(required.contains(&serde_json::json!("session_id")));
    }

    #[test]
    fn test_inject_session_id_param_idempotent() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            }
        });
        let first = inject_session_id_param(schema.clone());
        let second = inject_session_id_param(first.clone());

        assert_eq!(first, second);
    }

    #[test]
    fn test_extract_session_id_present() {
        let mut args = serde_json::Map::new();
        args.insert(
            "session_id".to_string(),
            serde_json::json!("sess-123"),
        );
        let result = extract_session_id(&Some(args));
        assert_eq!(result, Some("sess-123".to_string()));
    }

    #[test]
    fn test_extract_session_id_missing() {
        let mut args = serde_json::Map::new();
        args.insert("other".to_string(), serde_json::json!("value"));
        let result = extract_session_id(&Some(args));
        assert_eq!(result, None);
    }

    #[test]
    fn test_tool_output_to_result_success() {
        let output = ToolOutput {
            success: true,
            data: serde_json::json!({"result": "ok"}),
            raw_stdout: None,
        };
        let result = tool_output_to_result(output);

        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn test_tool_output_to_result_failure() {
        let output = ToolOutput {
            success: false,
            data: serde_json::json!({"error": "something went wrong"}),
            raw_stdout: None,
        };
        let result = tool_output_to_result(output);

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.len(), 1);
    }
}
