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
///
/// opencode `run --format json` outputs flat events like:
///   {"type":"text", "sessionID":"...", "part":{"type":"text", "text":"hello"}}
///   {"type":"tool_use", "sessionID":"...", "part":{"tool":"bash", "state":{"status":"completed", ...}}}
///   {"type":"step_start", ...}
///   {"type":"step_finish", ...}
///   {"type":"error", "error":{"data":{"message":"..."}}}
pub fn parse_event(line: &str, session_id: &str) -> Vec<AppEvent> {
    let json: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "text" => {
            // Text event: part.text contains the output text
            let part = json.get("part").unwrap_or(&json);
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
            // Reasoning event: part.text contains the reasoning text
            let part = json.get("part").unwrap_or(&json);
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
        "tool_use" => {
            // Tool use event: part.tool, part.state
            let part = json.get("part").unwrap_or(&json);
            parse_tool_event(part, session_id)
        }
        "error" => {
            let reason = json
                .get("error")
                .and_then(|e| e.get("data"))
                .and_then(|d| d.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();
            vec![AppEvent::AgentCrashed {
                session_id: session_id.to_string(),
                reason,
            }]
        }
        // step_start, step_finish, and other events are ignored
        _ => vec![],
    }
}

/// Extract opencode session ID from a session.created event, or from the
/// sessionID field present on any event (fallback).
pub fn extract_session_id(line: &str) -> Option<String> {
    let json: Value = serde_json::from_str(line).ok()?;

    // Primary: session.created event has properties.info.id
    if json.get("type").and_then(|t| t.as_str()) == Some("session.created") {
        if let Some(id) = json
            .get("properties")
            .and_then(|p| p.get("info"))
            .and_then(|i| i.get("id"))
            .and_then(|id| id.as_str())
        {
            return Some(id.to_string());
        }
    }

    // Fallback: many events carry a top-level sessionID field
    json.get("sessionID")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
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

/// Read all lines from a reader, logging each as warn!.
/// Returns the number of lines read.
async fn read_stderr_lines<R: tokio::io::AsyncRead + Unpin>(reader: R, session_id: &str) -> u64 {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(reader).lines();
    let mut count = 0u64;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::warn!(session_id = %session_id, raw = %line, "stderr line");
                count += 1;
            }
            Ok(None) => break,
            Err(e) => {
                tracing::error!(?e, count, session_id = %session_id, "error reading stderr");
                break;
            }
        }
    }
    count
}

/// Consume the stdout stream of an opencode process, parse NDJSON lines,
/// and emit AppEvents via the EventBus. Handles process lifecycle:
/// - stdout EOF + exit 0 → DiagnosisDone
/// - stdout EOF + exit ≠0 → AgentCrashed
/// - cancellation → AgentStopped
#[tracing::instrument(skip(agent, bus, pool, agents, cancel))]
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let AgentProcess { mut child, stdout, stderr, .. } = agent;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut oc_session_captured = false;
    let mut line_count = 0u64;

    let stderr_sid = session_id.clone();
    let stderr_handle = tokio::spawn(async move {
        read_stderr_lines(stderr, &stderr_sid).await
    });

    tracing::info!(session_id = %session_id, "consume_stream started");

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        line_count += 1;
                        tracing::debug!(line_count, raw = %line, "stdout line");

                        // Extract opencode session ID from any event that has it
                        if !oc_session_captured {
                            if let Some(oc_id) = extract_session_id(&line) {
                                tracing::info!(oc_id = %oc_id, "captured opencode session id");
                                let _ = crate::app::session::update_opencode_session_id(
                                    &pool, &session_id, &oc_id,
                                ).await;
                                oc_session_captured = true;
                            }
                        }

                        let events = parse_event(&line, &session_id);
                        for event in events {
                            tracing::debug!(event_type = ?std::mem::discriminant(&event), "emitting event");
                            bus.emit(&session_id, event);
                        }
                    }
                    Ok(None) => {
                        tracing::info!(line_count, session_id = %session_id, "stdout EOF");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(?e, line_count, "error reading stdout line");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                tracing::info!(session_id = %session_id, "cancellation received, killing child");
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

    let _ = stderr_handle.await;

    tracing::info!(
        session_id = %session_id,
        exit_ok,
        status = ?status.as_ref().map(|s| s.code()),
        "child process exited"
    );

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
    fn test_parse_text_event_emits_llm_thinking() {
        let line = r#"{"type":"text","timestamp":1787242656024,"sessionID":"ses_abc","part":{"type":"text","text":"你好！我是 Friday Agent。","id":"prt_123"}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "你好！我是 Friday Agent。");
            }
            _ => panic!("expected LlmThinking, got {:?}", events[0]),
        }
    }

    #[test]
    fn test_parse_text_event_empty_text_returns_empty() {
        let line = r#"{"type":"text","sessionID":"ses_abc","part":{"type":"text","text":""}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_reasoning_event_emits_llm_thinking() {
        let line = r#"{"type":"reasoning","sessionID":"ses_abc","part":{"type":"reasoning","text":"analyzing the issue"}}"#;
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
    fn test_parse_tool_use_running_emits_tool_executing() {
        let line = r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"bash","state":{"status":"running","input":{"command":"ls -la"}}}}"#;
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
    fn test_parse_tool_use_completed_emits_tool_result() {
        let line = r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"bash","state":{"status":"completed","output":"file1\nfile2","time":{"start":1000,"end":1800}}}}"#;
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
    fn test_parse_tool_use_error_emits_tool_result_with_error() {
        let line = r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"bash","state":{"status":"error","error":"command failed","time":{"start":1000,"end":1001}}}}"#;
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
    fn test_parse_error_event_emits_agent_crashed() {
        let line = r#"{"type":"error","error":{"name":"APIError","data":{"message":"rate limited"}}}"#;
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
    fn test_parse_step_start_returns_empty() {
        let line = r#"{"type":"step_start","sessionID":"ses_abc","part":{"id":"prt_1"}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_step_finish_returns_empty() {
        let line = r#"{"type":"step_finish","sessionID":"ses_abc","part":{"id":"prt_2","reason":"stop"}}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_invalid_json_returns_empty() {
        let events = parse_event("not valid json", "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_extract_session_id_from_top_level_field() {
        let line = r#"{"type":"step_start","timestamp":1787242655298,"sessionID":"ses_fe0096356ffeqwSFqjLhPeA72b","part":{"id":"prt_123"}}"#;
        let result = extract_session_id(line);
        assert_eq!(result, Some("ses_fe0096356ffeqwSFqjLhPeA72b".to_string()));
    }

    #[test]
    fn test_extract_session_id_returns_none_when_absent() {
        let line = r#"{"type":"unknown"}"#;
        let result = extract_session_id(line);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_session_id_returns_none_for_invalid_json() {
        let result = extract_session_id("not json");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_read_stderr_lines_captures_all_lines() {
        use tokio::io::{duplex, AsyncWriteExt};

        let (mut writer, reader) = duplex(1024);

        writer
            .write_all(b"error line 1\nerror line 2\nerror line 3\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let count = read_stderr_lines(reader, "test-session").await;

        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_read_stderr_lines_empty() {
        use tokio::io::duplex;

        let (_, reader) = duplex(1024);
        let count = read_stderr_lines(reader, "test-session").await;
        assert_eq!(count, 0);
    }
}
