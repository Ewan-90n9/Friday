use super::spawn::AgentProcess;
use crate::app::events::{AppEvent, EventBus};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Tracks a running agent's cancellation token and background task handle.
/// Stored in AppState.agents map keyed by session_id.
pub struct RunningAgent {
    pub cancel: CancellationToken,
    pub handle: tokio::task::JoinHandle<()>,
}

/// Parse a single NDJSON line and return the corresponding AppEvent(s).
/// Returns empty vec for events that should be ignored.
pub fn parse_event(line: &str, session_id: &str) -> Vec<AppEvent> {
    let json: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "message.part.updated" => parse_message_part_updated(&json, session_id),
        "session.error" => vec![AppEvent::AgentCrashed {
            session_id: session_id.to_string(),
            reason: json
                .get("properties")
                .and_then(|p| p.get("error"))
                .and_then(|e| e.get("data"))
                .and_then(|d| d.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string(),
        }],
        _ => vec![],
    }
}

/// Extract opencode session ID from a session.created event.
pub fn extract_session_id(line: &str) -> Option<String> {
    let json: Value = serde_json::from_str(line).ok()?;
    if json.get("type").and_then(|t| t.as_str()) != Some("session.created") {
        return None;
    }
    json.get("properties")
        .and_then(|p| p.get("info"))
        .and_then(|i| i.get("id"))
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
}

fn parse_message_part_updated(json: &Value, session_id: &str) -> Vec<AppEvent> {
    let properties = match json.get("properties") {
        Some(p) => p,
        None => return vec![],
    };

    let part = match properties.get("part") {
        Some(p) => p,
        None => return vec![],
    };

    let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match part_type {
        "text" => {
            if let Some(delta) = properties.get("delta").and_then(|d| d.as_str()) {
                if !delta.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: delta.to_string(),
                    }];
                }
            }
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: text.to_string(),
                    }];
                }
            }
            vec![]
        }
        "reasoning" => {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    return vec![AppEvent::LlmThinking {
                        session_id: session_id.to_string(),
                        token: text.to_string(),
                    }];
                }
            }
            vec![]
        }
        "tool" => parse_tool_event(part, session_id),
        _ => vec![],
    }
}

fn parse_tool_event(part: &Value, session_id: &str) -> Vec<AppEvent> {
    let tool_name = part
        .get("tool")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");

    let state = match part.get("state") {
        Some(s) => s,
        None => return vec![],
    };

    let status = state.get("status").and_then(|s| s.as_str()).unwrap_or("");

    match status {
        "running" => {
            let input = state.get("input").cloned().unwrap_or(Value::Null);
            vec![AppEvent::ToolExecuting {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                args: input,
            }]
        }
        "completed" => {
            let output = state
                .get("output")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();
            let elapsed_ms = compute_elapsed_ms(state);
            vec![AppEvent::ToolResult {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                output: serde_json::Value::String(output),
                elapsed_ms,
            }]
        }
        "error" => {
            let error = state
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
                .to_string();
            let elapsed_ms = compute_elapsed_ms(state);
            vec![AppEvent::ToolResult {
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                output: serde_json::Value::String(error),
                elapsed_ms,
            }]
        }
        _ => vec![],
    }
}

fn compute_elapsed_ms(state: &Value) -> u64 {
    let start = state
        .get("time")
        .and_then(|t| t.get("start"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    let end = state
        .get("time")
        .and_then(|t| t.get("end"))
        .and_then(|e| e.as_u64())
        .unwrap_or(start);
    if end >= start {
        end - start
    } else {
        0
    }
}

/// Consume the stdout stream of an opencode process, parse NDJSON lines,
/// and emit AppEvents via the EventBus. Handles process lifecycle:
/// - stdout EOF + exit 0 → DiagnosisDone
/// - stdout EOF + exit ≠0 → AgentCrashed
/// - cancellation → AgentStopped
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let AgentProcess { mut child, stdout, .. } = agent;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(oc_id) = extract_session_id(&line) {
                            let _ = crate::app::session::update_opencode_session_id(
                                &pool, &session_id, &oc_id,
                            ).await;
                        }

                        let events = parse_event(&line, &session_id);
                        for event in events {
                            bus.emit(&session_id, event);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!(?e, "error reading stdout line");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                child.kill().await.ok();
                bus.emit(&session_id, AppEvent::AgentStopped {
                    session_id: session_id.clone(),
                });
                let mut map = agents.lock().await;
                map.remove(&session_id);
                return;
            }
        }
    }

    let status = child.wait().await;
    let exit_ok = status.as_ref().map(|s| s.success()).unwrap_or(false);

    if exit_ok {
        bus.emit(&session_id, AppEvent::DiagnosisDone {
            session_id: session_id.clone(),
            conclusion: String::new(),
        });
    } else {
        let reason = match &status {
            Ok(s) => format!("exit code: {}", s.code().unwrap_or(-1)),
            Err(e) => format!("wait error: {}", e),
        };
        bus.emit(&session_id, AppEvent::AgentCrashed {
            session_id: session_id.clone(),
            reason,
        });
    }

    let mut map = agents.lock().await;
    map.remove(&session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_delta_emits_llm_thinking() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"text","text":"Hello"},"delta":"Hel"}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "Hel");
            }
            _ => panic!("expected LlmThinking, got {:?}", events[0]),
        }
    }

    #[test]
    fn test_parse_reasoning_emits_llm_thinking() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"reasoning","text":"analyzing the issue"}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "analyzing the issue");
            }
            _ => panic!("expected LlmThinking"),
        }
    }

    #[test]
    fn test_parse_tool_running_emits_tool_executing() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"running","input":{"command":"ls -la"}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolExecuting { session_id, tool, args } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(args["command"], "ls -la");
            }
            _ => panic!("expected ToolExecuting"),
        }
    }

    #[test]
    fn test_parse_tool_completed_emits_tool_result() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"completed","output":"file1\nfile2","time":{"start":1000,"end":1800}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolResult { session_id, tool, output, elapsed_ms } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(output, "file1\nfile2");
                assert_eq!(*elapsed_ms, 800);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn test_parse_tool_error_emits_tool_result_with_error() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"bash","state":{"status":"error","error":"command failed","time":{"start":1000,"end":1001}}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::ToolResult { session_id, tool, output, .. } => {
                assert_eq!(session_id, "s1");
                assert_eq!(tool, "bash");
                assert_eq!(output, "command failed");
            }
            _ => panic!("expected ToolResult with error"),
        }
    }

    #[test]
    fn test_parse_session_error_emits_agent_crashed() {
        let line = r#"{"type":"session.error","properties":{"error":{"name":"APIError","data":{"message":"rate limited"}}}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::AgentCrashed { session_id, reason } => {
                assert_eq!(session_id, "s1");
                assert_eq!(reason, "rate limited");
            }
            _ => panic!("expected AgentCrashed"),
        }
    }

    #[test]
    fn test_parse_unmapped_event_returns_empty() {
        let line = r#"{"type":"session.updated","properties":{}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_invalid_json_returns_empty() {
        let events = parse_event("not valid json", "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_empty_delta_returns_empty() {
        let line = r#"{"type":"message.part.updated","properties":{"part":{"type":"text","text":""},"delta":""}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_extract_session_id_from_session_created() {
        let line = r#"{"type":"session.created","properties":{"info":{"id":"oc-session-abc","title":"test"}}}"#;
        let result = extract_session_id(line);
        assert_eq!(result, Some("oc-session-abc".to_string()));
    }

    #[test]
    fn test_extract_session_id_returns_none_for_other_events() {
        let line = r#"{"type":"message.updated","properties":{}}"#;
        let result = extract_session_id(line);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_session_id_returns_none_for_invalid_json() {
        let result = extract_session_id("not json");
        assert!(result.is_none());
    }
}
