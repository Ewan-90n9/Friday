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
/// Handles two output formats:
///
/// opencode `run --format json`:
///   {"type":"text", "sessionID":"...", "part":{"type":"text", "text":"hello"}}
///   {"type":"tool_use", "sessionID":"...", "part":{"tool":"bash", "state":{"status":"completed", ...}}}
///   {"type":"step_start", ...}
///   {"type":"step_finish", ...}
///   {"type":"error", "error":{"data":{"message":"..."}}}
///
/// codeagentcli `-p --output-format stream-json`:
///   {"type":"system","subtype":"init","session_id":"...",...}
///   {"type":"assistant","message":{"content":[{"type":"thinking","thinking":"..."}]}}
///   {"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}
///   {"type":"result","subtype":"success","result":"..."}
pub fn parse_event(line: &str, session_id: &str) -> Vec<AppEvent> {
    let json: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "text" => {
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
        "assistant" => parse_assistant_event(&json, session_id),
        "result" => vec![],
        "tool_use" => {
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
        _ => vec![],
    }
}

/// Extract agent session ID from various event formats.
/// Checks: session.created (opencode), sessionID (opencode), session_id (codeagentcli).
pub fn extract_session_id(line: &str) -> Option<String> {
    let json: Value = serde_json::from_str(line).ok()?;

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

    if let Some(id) = json.get("sessionID").and_then(|s| s.as_str()) {
        return Some(id.to_string());
    }

    json.get("session_id")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

/// Parse codeagentcli assistant event: message.content[] array contains
/// thinking and text items.
fn parse_assistant_event(json: &Value, session_id: &str) -> Vec<AppEvent> {
    let content = match json
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(arr) => arr,
        None => return vec![],
    };

    let mut events = vec![];
    for item in content {
        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match item_type {
            "text" => {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        events.push(AppEvent::LlmThinking {
                            session_id: session_id.to_string(),
                            token: text.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    events
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

struct MessageAccumulator {
    message_id: String,
    parts: Vec<AccumulatedPart>,
    current_text: String,
    pending_tool_args: Option<String>,
}

enum AccumulatedPart {
    Text(String),
    Tool {
        name: String,
        args: String,
        status: String,
        output: String,
        elapsed_ms: i64,
    },
}

impl MessageAccumulator {
    fn new(message_id: String) -> Self {
        Self {
            message_id,
            parts: Vec::new(),
            current_text: String::new(),
            pending_tool_args: None,
        }
    }

    fn handle_event(&mut self, event: &AppEvent) {
        match event {
            AppEvent::LlmThinking { token, .. } => {
                self.current_text.push_str(token);
            }
            AppEvent::ToolExecuting { args, .. } => {
                self.flush_current_text();
                self.pending_tool_args = Some(serde_json::to_string(args).unwrap_or_default());
            }
            AppEvent::ToolResult { tool, output, elapsed_ms, .. } => {
                self.flush_current_text();
                let args = self.pending_tool_args.take().unwrap_or_default();
                let output_str = match output {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                self.parts.push(AccumulatedPart::Tool {
                    name: tool.clone(),
                    args,
                    status: "completed".to_string(),
                    output: output_str,
                    elapsed_ms: *elapsed_ms as i64,
                });
            }
            _ => {}
        }
    }

    fn flush_current_text(&mut self) {
        if !self.current_text.is_empty() {
            self.parts.push(AccumulatedPart::Text(std::mem::take(&mut self.current_text)));
        }
    }

    async fn flush_to_db(&mut self, pool: &sqlx::SqlitePool) {
        self.flush_current_text();
        for (seq, part) in self.parts.drain(..).enumerate() {
            let seq = seq as i64;
            match part {
                AccumulatedPart::Text(text) => {
                    if let Err(e) = crate::app::session::insert_text_part(
                        pool, &self.message_id, seq, &text,
                    ).await {
                        tracing::error!(?e, message_id = %self.message_id, seq, "failed to persist text part");
                    }
                }
                AccumulatedPart::Tool { name, args, status, output, elapsed_ms } => {
                    if let Err(e) = crate::app::session::insert_tool_part(
                        pool, &self.message_id, seq, &name, &args, &status, &output, elapsed_ms,
                    ).await {
                        tracing::error!(?e, message_id = %self.message_id, seq, tool = %name, "failed to persist tool part");
                    }
                }
            }
        }
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

/// Consume the stdout stream of an agent process, parse NDJSON lines,
/// and emit AppEvents via the EventBus. Handles process lifecycle:
/// - stdout EOF + exit 0 → DiagnosisDone
/// - stdout EOF + exit ≠0 → AgentCrashed
/// - cancellation → AgentStopped
#[tracing::instrument(skip(agent, bus, pool, agents, cancel))]
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    agent_message_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let AgentProcess { mut child, stdout, stderr, .. } = agent;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut agent_session_captured = false;
    let mut line_count = 0u64;
    let mut accumulator = MessageAccumulator::new(agent_message_id.clone());

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

                        // Extract agent session ID from any event that has it
                        if !agent_session_captured {
                            if let Some(agent_id) = extract_session_id(&line) {
                                tracing::info!(agent_id = %agent_id, "captured agent session id");
                                let _ = crate::app::session::update_agent_session_id(
                                    &pool, &session_id, &agent_id,
                                ).await;
                                agent_session_captured = true;
                            }
                        }

                        let events = parse_event(&line, &session_id);
                        for event in &events {
                            accumulator.handle_event(event);
                        }
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
                accumulator.flush_to_db(&pool).await;
                if let Err(e) = crate::app::session::update_message_status(&pool, &agent_message_id, "stopped").await {
                    tracing::error!(?e, message_id = %agent_message_id, "failed to update message status on stop");
                }
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

    let final_status = if exit_ok { "done" } else { "error" };
    accumulator.flush_to_db(&pool).await;
    if let Err(e) = crate::app::session::update_message_status(&pool, &agent_message_id, final_status).await {
        tracing::error!(?e, message_id = %agent_message_id, "failed to update message status");
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
    fn test_parse_assistant_thinking_skipped() {
        let line = r#"{"type":"assistant","message":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"The user is greeting me.","signature":"123"}],"model":"Glm-5.1"},"session_id":"c7c8d0d3"}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_assistant_text_emits_llm_thinking() {
        let line = r#"{"type":"assistant","message":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"你好，我是 Friday。"}],"model":"Glm-5.1"},"session_id":"c7c8d0d3"}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::LlmThinking { session_id, token } => {
                assert_eq!(session_id, "s1");
                assert_eq!(token, "你好，我是 Friday。");
            }
            _ => panic!("expected LlmThinking"),
        }
    }

    #[test]
    fn test_parse_assistant_multiple_content_items() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"analyzing"},{"type":"text","text":"here is the answer"}]},"session_id":"c7c8d0d3"}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AppEvent::LlmThinking { token, .. } if token == "here is the answer"));
    }

    #[test]
    fn test_parse_result_event_returns_empty() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"诊断完成","session_id":"c7c8d0d3"}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_result_event_empty_result_returns_empty() {
        let line = r#"{"type":"result","subtype":"success","result":""}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_system_init_returns_empty() {
        let line = r#"{"type":"system","subtype":"init","session_id":"c7c8d0d3","tools":["Bash","Edit"]}"#;
        let events = parse_event(line, "s1");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_extract_session_id_from_codeagentcli_snake_case() {
        let line = r#"{"type":"system","subtype":"init","session_id":"c7c8d0d3-abc"}"#;
        let result = extract_session_id(line);
        assert_eq!(result, Some("c7c8d0d3-abc".to_string()));
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

    use crate::infra::db;

    fn make_llm_thinking(token: &str) -> AppEvent {
        AppEvent::LlmThinking {
            session_id: "s1".to_string(),
            token: token.to_string(),
        }
    }

    fn make_tool_executing(name: &str) -> AppEvent {
        AppEvent::ToolExecuting {
            session_id: "s1".to_string(),
            tool: name.to_string(),
            args: serde_json::Value::Null,
        }
    }

    fn make_tool_result(name: &str, output: &str, elapsed: u64) -> AppEvent {
        AppEvent::ToolResult {
            session_id: "s1".to_string(),
            tool: name.to_string(),
            output: serde_json::Value::String(output.to_string()),
            elapsed_ms: elapsed,
        }
    }

    #[tokio::test]
    async fn test_accumulator_text_accumulation() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        acc.handle_event(&make_llm_thinking("Hello "));
        acc.handle_event(&make_llm_thinking("world!"));

        acc.flush_to_db(&pool).await;
        crate::app::session::update_message_status(&pool, &msg_id, "done").await.unwrap();

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].parts.len(), 1);
        assert_eq!(messages[0].parts[0].part_type, "text");
        assert_eq!(messages[0].parts[0].text, Some("Hello world!".to_string()));
    }

    #[tokio::test]
    async fn test_accumulator_tool_result_persists_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        acc.handle_event(&make_tool_executing("bash"));
        acc.handle_event(&make_tool_result("bash", "file1\nfile2", 500));
        acc.handle_event(&make_llm_thinking("Done."));
        acc.flush_to_db(&pool).await;

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages[0].parts.len(), 2);
        assert_eq!(messages[0].parts[0].part_type, "tool");
        assert_eq!(messages[0].parts[0].tool_name, Some("bash".to_string()));
        assert_eq!(messages[0].parts[0].tool_status, Some("completed".to_string()));
        assert_eq!(messages[0].parts[1].part_type, "text");
        assert_eq!(messages[0].parts[1].text, Some("Done.".to_string()));
    }

    #[tokio::test]
    async fn test_accumulator_multiple_text_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let msg_id = crate::app::session::insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();

        let mut acc = MessageAccumulator::new(msg_id.clone());
        acc.handle_event(&make_llm_thinking("First text"));
        acc.handle_event(&make_tool_executing("bash"));
        acc.handle_event(&make_tool_result("bash", "output", 100));
        acc.handle_event(&make_llm_thinking("Second text"));
        acc.flush_to_db(&pool).await;

        let messages = crate::app::session::get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages[0].parts.len(), 3);
        assert_eq!(messages[0].parts[0].part_type, "text");
        assert_eq!(messages[0].parts[0].text, Some("First text".to_string()));
        assert_eq!(messages[0].parts[1].part_type, "tool");
        assert_eq!(messages[0].parts[2].part_type, "text");
        assert_eq!(messages[0].parts[2].text, Some("Second text".to_string()));
    }
}
