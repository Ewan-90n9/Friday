use super::spawn::AgentProcess;
use crate::app::events::{AppEvent, EventBus};
use crate::app::session::MessageRow;
use serde_json::Value;
use std::fmt::Write as _;
use tokio_util::sync::CancellationToken;

/// Tracks a running agent's cancellation token and background task handle.
/// Stored in AppState.agents map keyed by session_id.
pub struct RunningAgent {
    pub cancel: CancellationToken,
    pub handle: tokio::task::JoinHandle<()>,
}

/// Retry parameters for a resume spawn whose session id the CLI rejected
/// ("Session ID ... is already in use", issue #21): the stream consumer
/// respawns once WITHOUT the resume flag, with Friday's own record of the
/// conversation injected into the prompt (the CLI-side history is unreachable
/// behind the stale lock). The new agent session id captured from the retry
/// stream overwrites the stale one, so later turns resume normally again.
pub struct ResumeRetry {
    pub user_message: String,
    pub prompt_override_path: Option<std::path::PathBuf>,
}

/// codeagentcli (Claude-Code-style CLI) refuses to resume a session whose id
/// it still considers "in use" — a per-session lock left behind by a previous
/// run that exited without cleanup. The error goes to stderr and the process
/// exits non-zero before producing any stdout NDJSON.
/// Example (issue #21): `Error: Session ID e3afdf0c-... is already in use.`
fn is_session_in_use_error(line: &str) -> bool {
    line.contains("Session ID") && line.contains("already in use")
}

/// 单条消息注入重试 prompt 时的字符上限：用户消息 2000、agent 文本 1000。
/// 工具调用只保留「名称（状态）」一行，输出不注入（体积大且结论通常在文本里）。
const HISTORY_USER_MAX_CHARS: usize = 2000;
const HISTORY_AGENT_TEXT_MAX_CHARS: usize = 1000;
/// 整段历史的字符上限：超限时保留最近的部分（诊断结论通常在尾部）。
const HISTORY_TOTAL_MAX_CHARS: usize = 8000;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…（已截断）")
    }
}

/// Compact conversation-history block for the fresh-session retry prompt,
/// built from Friday's own message store (the CLI-side history is unreachable
/// when its session lock is stale). Excludes the in-flight exchange (the
/// streaming agent message and the user message right before it — the retry
/// prompt carries the current user message itself). Returns None when there
/// is no prior exchange to inject.
fn format_recent_history(messages: &[MessageRow], agent_message_id: &str) -> Option<String> {
    let inflight_pos = messages.iter().position(|m| m.id == agent_message_id)?;
    // 在途 agent 消息的前一条是本轮用户消息（由重试 prompt 自带），再之前的才是历史
    let history_end = inflight_pos.checked_sub(1)?;
    if history_end == 0 {
        return None;
    }

    let mut out = String::new();
    for msg in &messages[..history_end] {
        match msg.role.as_str() {
            "user" => {
                let Some(content) = msg.content.as_deref() else { continue };
                if content.trim().is_empty() {
                    continue;
                }
                let _ = writeln!(out, "[用户] {}", truncate_chars(content, HISTORY_USER_MAX_CHARS));
            }
            "agent" => {
                if msg.parts.is_empty() {
                    if let Some(content) = msg.content.as_deref() {
                        if !content.trim().is_empty() {
                            let _ = writeln!(
                                out,
                                "[助手] {}",
                                truncate_chars(content, HISTORY_AGENT_TEXT_MAX_CHARS)
                            );
                        }
                    }
                    continue;
                }
                for part in &msg.parts {
                    match part.part_type.as_str() {
                        "text" => {
                            if let Some(text) = part.text.as_deref() {
                                if !text.trim().is_empty() {
                                    let _ = writeln!(
                                        out,
                                        "[助手] {}",
                                        truncate_chars(text, HISTORY_AGENT_TEXT_MAX_CHARS)
                                    );
                                }
                            }
                        }
                        "tool" => {
                            let name = part.tool_name.as_deref().unwrap_or("tool");
                            let status = part.tool_status.as_deref().unwrap_or("unknown");
                            let _ = writeln!(out, "[工具] {name}（{status}）");
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if out.is_empty() {
        return None;
    }

    // 整体截断：保留最近的历史（结尾部分），前缀标注省略
    let total = out.chars().count();
    if total > HISTORY_TOTAL_MAX_CHARS {
        let tail: String = out
            .chars()
            .skip(total - HISTORY_TOTAL_MAX_CHARS)
            .collect();
        return Some(format!("（更早的对话已省略）\n{tail}"));
    }
    Some(out)
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
/// Returns the number of lines read and whether any line matched the
/// "Session ID ... is already in use" pattern (issue #21).
async fn read_stderr_lines<R: tokio::io::AsyncRead + Unpin>(reader: R, session_id: &str) -> (u64, bool) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(reader).lines();
    let mut count = 0u64;
    let mut session_conflict = false;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::warn!(session_id = %session_id, raw = %line, "stderr line");
                if is_session_in_use_error(&line) {
                    session_conflict = true;
                }
                count += 1;
            }
            Ok(None) => break,
            Err(e) => {
                tracing::error!(?e, count, session_id = %session_id, "error reading stderr");
                break;
            }
        }
    }
    (count, session_conflict)
}

/// One consume attempt's outcome: terminal, or the CLI rejected the resume
/// session id and the caller should retry with a fresh agent session.
enum AttemptOutcome {
    Finished,
    RetryWithFreshSession(AgentProcess),
}

enum ExitReason {
    Normal,
    Cancelled,
}

/// Consume the stdout stream of an agent process, parse NDJSON lines,
/// and emit AppEvents via the EventBus. Handles process lifecycle:
/// - stdout EOF + exit 0 → DiagnosisDone
/// - stdout EOF + exit ≠0 → AgentCrashed
/// - cancellation → AgentStopped
/// - resume spawn rejected with "Session ID ... already in use" (issue #21,
///   每次最多一次) → retry once with a fresh agent session (no resume flag),
///   Friday 本地对话历史注入 prompt 补偿 CLI 侧上下文丢失。
#[tracing::instrument(skip(agent, bus, pool, agents, cancel, embedding, vec_store, resume_retry))]
pub async fn consume_stream(
    agent: AgentProcess,
    bus: EventBus,
    session_id: String,
    agent_message_id: String,
    pool: sqlx::SqlitePool,
    agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
    embedding: Option<std::sync::Arc<crate::knowledge::embedding::EmbeddingService>>,
    vec_store: Option<std::sync::Arc<crate::knowledge::vec_store::VecStore>>,
    resume_retry: Option<ResumeRetry>,
) {
    let mut agent = agent;
    let mut resume_retry = resume_retry;
    loop {
        let outcome = consume_attempt(
            agent,
            &bus,
            &session_id,
            &agent_message_id,
            &pool,
            &agents,
            cancel.clone(),
            embedding.clone(),
            vec_store.clone(),
            resume_retry.take(),
        )
        .await;
        match outcome {
            AttemptOutcome::Finished => return,
            AttemptOutcome::RetryWithFreshSession(next) => {
                tracing::warn!(
                    session_id = %session_id,
                    "retrying with fresh agent session after session-id-in-use rejection"
                );
                agent = next;
            }
        }
    }
}

#[tracing::instrument(skip(agent, bus, pool, agents, cancel, embedding, vec_store, resume_retry))]
async fn consume_attempt(
    agent: AgentProcess,
    bus: &EventBus,
    session_id: &str,
    agent_message_id: &str,
    pool: &sqlx::SqlitePool,
    agents: &std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>>,
    cancel: CancellationToken,
    embedding: Option<std::sync::Arc<crate::knowledge::embedding::EmbeddingService>>,
    vec_store: Option<std::sync::Arc<crate::knowledge::vec_store::VecStore>>,
    resume_retry: Option<ResumeRetry>,
) -> AttemptOutcome {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let AgentProcess { mut child, stdout, stderr, .. } = agent;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut agent_session_captured = false;
    let mut line_count = 0u64;
    let mut accumulator = MessageAccumulator::new(agent_message_id.to_string());

    let stderr_sid = session_id.to_string();
    let stderr_handle = tokio::spawn(async move {
        read_stderr_lines(stderr, &stderr_sid).await
    });

    tracing::info!(session_id = %session_id, "consume_stream started");

    let mut exit_reason = ExitReason::Normal;

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
                                    pool, session_id, &agent_id,
                                ).await;
                                agent_session_captured = true;
                            }
                        }

                        let events = parse_event(&line, session_id);
                        for event in &events {
                            accumulator.handle_event(event);
                        }
                        for event in events {
                            tracing::debug!(event_type = ?std::mem::discriminant(&event), "emitting event");
                            bus.emit(session_id, event);
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
                bus.emit(session_id, AppEvent::AgentStopped {
                    session_id: session_id.to_string(),
                });
                exit_reason = ExitReason::Cancelled;
                break;
            }
        }
    }

    let status = child.wait().await;
    let exit_ok = status.as_ref().map(|s| s.success()).unwrap_or(false);

    let (_, session_conflict) = stderr_handle.await.unwrap_or((0, false));

    // issue #21：复用的 agent_session_id 被 CLI 以「already in use」拒绝，
    // 且未产生任何 stdout——说明旧会话锁死，resume 不可达。带重试配置时
    // 自动降级为全新会话（不传 resume flag），本地历史注入 prompt 补偿上下文。
    // 新会话 id 会在重试流中被捕获并覆盖存量值，后续轮次恢复正常 resume。
    if matches!(exit_reason, ExitReason::Normal)
        && session_conflict
        && line_count == 0
    {
        match resume_retry {
            Some(retry) => {
                tracing::warn!(
                    session_id = %session_id,
                    "agent session resume rejected (session id in use), falling back to fresh agent session"
                );
                let history = match crate::app::session::get_session_messages(pool, session_id).await {
                    Ok(messages) => format_recent_history(&messages, agent_message_id),
                    Err(e) => {
                        tracing::warn!(?e, session_id = %session_id, "failed to load session history for retry prompt");
                        None
                    }
                };
                match super::spawn::spawn_active(
                    pool,
                    session_id.to_string(),
                    retry.user_message.clone(),
                    None,
                    retry.prompt_override_path.clone(),
                    None,
                    history.as_deref(),
                )
                .await
                {
                    Ok(next) => return AttemptOutcome::RetryWithFreshSession(next),
                    Err(e) => {
                        tracing::error!(?e, session_id = %session_id, "fresh-session respawn failed after session-id conflict");
                        // 落到下方通用崩溃处理
                    }
                }
            }
            None => {
                tracing::warn!(
                    session_id = %session_id,
                    "session-id-in-use crash detected but no retry configured"
                );
            }
        }
    }

    let (final_status, fallback_outcome) = match exit_reason {
        ExitReason::Normal => {
            if exit_ok {
                tracing::info!(session_id = %session_id, exit_ok, "child process exited normally");
                bus.emit(session_id, AppEvent::DiagnosisDone {
                    session_id: session_id.to_string(),
                    conclusion: String::new(),
                });
                ("done", crate::knowledge::experience::Outcome::Uncertain)
            } else {
                let reason = match &status {
                    Ok(s) => format!("exit code: {}", s.code().unwrap_or(-1)),
                    Err(e) => format!("wait error: {}", e),
                };
                tracing::info!(session_id = %session_id, "child process crashed");
                bus.emit(session_id, AppEvent::AgentCrashed {
                    session_id: session_id.to_string(),
                    reason,
                });
                ("error", crate::knowledge::experience::Outcome::Negative)
            }
        }
        ExitReason::Cancelled => {
            ("stopped", crate::knowledge::experience::Outcome::Negative)
        }
    };

    accumulator.flush_to_db(pool).await;
    if let Err(e) = crate::app::session::update_message_status(pool, agent_message_id, final_status).await {
        tracing::error!(?e, message_id = %agent_message_id, "failed to update message status");
    }

    {
        let mut map = agents.lock().await;
        map.remove(session_id);
    }

    if let (Some(embedding), Some(vec_store)) = (embedding, vec_store) {
        let pool_clone = pool.clone();
        let session_id_clone = session_id.to_string();
        tokio::spawn(async move {
            crate::knowledge::memory::generate_memory(
                pool_clone,
                session_id_clone,
                fallback_outcome,
                embedding,
                vec_store,
            )
            .await;
        });
    } else {
        tracing::warn!(session_id = %session_id, "memory resources not available, skipping memory generation");
    }

    AttemptOutcome::Finished
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

        let (count, conflict) = read_stderr_lines(reader, "test-session").await;

        assert_eq!(count, 3);
        assert!(!conflict);
    }

    #[tokio::test]
    async fn test_read_stderr_lines_detects_session_conflict() {
        use tokio::io::{duplex, AsyncWriteExt};

        let (mut writer, reader) = duplex(1024);
        writer
            .write_all(b"some noise\nError: Session ID abc-123 is already in use.\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let (count, conflict) = read_stderr_lines(reader, "test-session").await;

        assert_eq!(count, 2);
        assert!(conflict, "already in use 行应被识别为会话冲突");
    }

    #[tokio::test]
    async fn test_read_stderr_lines_empty() {
        use tokio::io::duplex;

        let (_, reader) = duplex(1024);
        let (count, conflict) = read_stderr_lines(reader, "test-session").await;
        assert_eq!(count, 0);
        assert!(!conflict);
    }

    // ---- issue #21: session-id-in-use 检测与全新会话重试 ----

    use crate::app::session::MessagePartRow;

    #[test]
    fn test_is_session_in_use_error_matches_incident_line() {
        let line = "Error: Session ID e3afdf0c-951e-4299-acad-51974107ea08 is already in use.";
        assert!(is_session_in_use_error(line));
    }

    #[test]
    fn test_is_session_in_use_error_matches_without_error_prefix() {
        let line = "Session ID abc is already in use";
        assert!(is_session_in_use_error(line));
    }

    #[test]
    fn test_is_session_in_use_error_rejects_other_lines() {
        assert!(!is_session_in_use_error("Error: API key invalid"));
        assert!(!is_session_in_use_error("Error: Session not found"));
        assert!(!is_session_in_use_error(""));
    }

    fn msg(id: &str, role: &str, content: Option<&str>, seq: i64) -> MessageRow {
        MessageRow {
            id: id.to_string(),
            role: role.to_string(),
            content: content.map(|s| s.to_string()),
            status: Some("done".to_string()),
            seq,
            parts: vec![],
        }
    }

    fn text_part(text: &str) -> MessagePartRow {
        MessagePartRow {
            part_type: "text".to_string(),
            seq: 0,
            text: Some(text.to_string()),
            tool_name: None,
            tool_args: None,
            tool_status: None,
            tool_output: None,
            tool_elapsed_ms: None,
        }
    }

    fn tool_part(name: &str, status: &str) -> MessagePartRow {
        MessagePartRow {
            part_type: "tool".to_string(),
            seq: 1,
            text: None,
            tool_name: Some(name.to_string()),
            tool_args: None,
            tool_status: Some(status.to_string()),
            tool_output: None,
            tool_elapsed_ms: None,
        }
    }

    #[test]
    fn test_format_recent_history_includes_prior_exchange_only() {
        let mut prior_agent = msg("m2", "agent", None, 1);
        prior_agent.parts = vec![text_part("已定位到内存泄漏")];
        let messages = vec![
            msg("m1", "user", Some("OOM 排查"), 0),
            prior_agent,
            msg("m3", "user", Some("继续排查"), 2),
            msg("m4", "agent", None, 3),
        ];
        let history = format_recent_history(&messages, "m4").unwrap();
        assert!(history.contains("OOM 排查"));
        assert!(history.contains("已定位到内存泄漏"));
        assert!(!history.contains("继续排查"), "当前在途用户消息不应进入历史");
    }

    #[test]
    fn test_format_recent_history_renders_tool_part_compactly() {
        let mut prior_agent = msg("m2", "agent", None, 1);
        prior_agent.parts = vec![tool_part("jvm_gc_stats", "completed"), text_part("结论")];
        let messages = vec![
            msg("m1", "user", Some("问题"), 0),
            prior_agent,
            msg("m3", "user", Some("现在"), 2),
            msg("m4", "agent", None, 3),
        ];
        let history = format_recent_history(&messages, "m4").unwrap();
        assert!(history.contains("jvm_gc_stats"));
        assert!(history.contains("completed"));
        assert!(history.contains("结论"));
    }

    #[test]
    fn test_format_recent_history_none_when_only_inflight_exchange() {
        let messages = vec![
            msg("m1", "user", Some("第一问"), 0),
            msg("m2", "agent", None, 1),
        ];
        assert!(format_recent_history(&messages, "m2").is_none());
    }

    #[test]
    fn test_format_recent_history_none_when_agent_message_not_found() {
        let messages = vec![msg("m1", "user", Some("问"), 0)];
        assert!(format_recent_history(&messages, "missing-id").is_none());
    }

    #[test]
    fn test_format_recent_history_truncates_long_messages() {
        let long: String = "长".repeat(5000);
        let messages = vec![
            msg("m1", "user", Some(&long), 0),
            msg("m2", "user", Some("继续"), 1),
            msg("m3", "agent", None, 2),
        ];
        let history = format_recent_history(&messages, "m3").unwrap();
        let total: usize = history.chars().count();
        assert!(total < 3000, "超长历史应被截断，实际 {total} 字符");
    }

    /// 构造一个假 agent CLI（Windows .cmd）：
    /// - `--help`：输出含 `--sessions` 的帮助（session flag 探测命中）；
    /// - 带 `--sessions`：复现 issue #21 —— stderr 打 `Error: Session ID ... is
    ///   already in use.` 并以退出码 1 崩溃，不产生任何 stdout；
    /// - 不带（全新会话）：把 stdin prompt 原样字节落盘到 dump_path（用
    ///   PowerShell 流拷贝——`more` 会按代码页转码破坏 UTF-8），输出正常 NDJSON。
    #[cfg(windows)]
    fn write_fake_agent_cmd(dir: &std::path::Path, dump_path: &std::path::Path) -> std::path::PathBuf {
        let script = format!(
            "@echo off\r\n\
             echo %* | findstr /C:\"--help\" >nul 2>&1\r\n\
             if %errorlevel%==0 goto :help\r\n\
             echo %* | findstr /C:\"--sessions\" >nul 2>&1\r\n\
             if %errorlevel%==0 goto :conflict\r\n\
             goto :run\r\n\
             :help\r\n\
             echo usage: fake-agent [options]\r\n\
             echo   --sessions ^<id^>  resume session\r\n\
             exit /b 0\r\n\
             :conflict\r\n\
             echo Error: Session ID e3afdf0c-stale is already in use. 1>&2\r\n\
             exit /b 1\r\n\
             :run\r\n\
             powershell -NoProfile -Command \"$s=[Console]::OpenStandardInput();$f=[System.IO.File]::Create('{dump}');$s.CopyTo($f);$f.Close()\"\r\n\
             echo {{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fresh-agent-session\"}}\r\n\
             echo {{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"retry answer\"}}]}}}}\r\n\
             echo {{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"retry answer\"}}\r\n\
             exit /b 0\r\n",
            dump = dump_path.display(),
        );
        let path = dir.join("fake-agent.cmd");
        std::fs::write(&path, script).unwrap();
        path
    }

    /// 复现 issue #21 场景的完整数据库 + 会话状态：
    /// 历史一轮对话 → 存量 agent_session_id（已被 CLI 锁死）→ 用户发新消息。
    #[cfg(windows)]
    async fn setup_conflict_scenario(
        pool: &sqlx::SqlitePool,
        session_id: &str,
        agent_path: &str,
    ) -> String {
        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES ('test-agent-id', 'codeagentcli', 'FakeAgent', ?, NULL, 'manual', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(agent_path)
        .execute(pool)
        .await
        .unwrap();

        // 历史一轮：用户问 + agent 答（带文本 part）
        let u1 = crate::app::session::insert_message(pool, session_id, "user", Some("OOM 排查一下"), Some("done"), 0).await.unwrap();
        let _ = u1;
        let a1 = crate::app::session::insert_message(pool, session_id, "agent", None, Some("done"), 1).await.unwrap();
        crate::app::session::insert_text_part(pool, &a1, 0, "已定位到内存泄漏").await.unwrap();

        // 存量 agent_session_id（CLI 侧锁未释放）
        crate::app::session::update_agent_session_id(pool, session_id, "e3afdf0c-stale").await.unwrap();

        // 当前在途：用户新消息 + streaming agent 消息
        crate::app::session::insert_message(pool, session_id, "user", Some("继续排查"), Some("done"), 2).await.unwrap();
        crate::app::session::insert_message(pool, session_id, "agent", None, Some("streaming"), 3).await.unwrap()
    }

    #[cfg(windows)]
    async fn last_agent_message_status(
        pool: &sqlx::SqlitePool,
        session_id: &str,
    ) -> (String, Vec<String>) {
        let messages = crate::app::session::get_session_messages(pool, session_id).await.unwrap();
        let last = messages.last().unwrap();
        let texts: Vec<String> = last
            .parts
            .iter()
            .filter(|p| p.part_type == "text")
            .filter_map(|p| p.text.clone())
            .collect();
        (last.status.clone().unwrap_or_default(), texts)
    }

    /// issue #21：复用被锁死的 agent_session_id spawn 后，CLI 立即崩溃
    /// （stderr 报 already in use、无任何 stdout）。带 ResumeRetry 时必须自动
    /// 以全新会话重试：消息状态 done、回答落库、agent_session_id 更新为新值、
    /// 重试 prompt 注入本地历史记录。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_consume_stream_retries_with_fresh_session_on_session_id_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let sid = session.id.0;

        let dump_path = tmp.path().join("prompt-dump.txt");
        let fake = write_fake_agent_cmd(tmp.path(), &dump_path);
        let agent_message_id = setup_conflict_scenario(&pool, &sid, &fake.to_string_lossy()).await;

        // 先以存量 session_id spawn（复现冲突）
        let process = crate::agent::spawn::spawn_active(
            &pool,
            sid.clone(),
            "继续排查".to_string(),
            Some("e3afdf0c-stale".to_string()),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let bus = EventBus::disabled();

        consume_stream(
            process,
            bus,
            sid.clone(),
            agent_message_id,
            pool.clone(),
            agents,
            CancellationToken::new(),
            None,
            None,
            Some(ResumeRetry {
                user_message: "继续排查".to_string(),
                prompt_override_path: None,
            }),
        )
        .await;

        // 重试后消息正常完成
        let (status, texts) = last_agent_message_status(&pool, &sid).await;
        assert_eq!(status, "done", "重试后 agent 消息应为 done");
        assert!(texts.iter().any(|t| t.contains("retry answer")));

        // agent_session_id 已被新会话覆盖（后续轮次可正常 resume）
        let new_agent_session = crate::app::session::get_agent_session_id(&pool, &sid).await.unwrap();
        assert_eq!(new_agent_session.as_deref(), Some("fresh-agent-session"));

        // 重试 prompt 注入了本地历史与当前消息
        let prompt = std::fs::read_to_string(&dump_path).unwrap();
        assert!(prompt.contains("此前对话记录"), "重试 prompt 应包含历史记录段");
        assert!(prompt.contains("已定位到内存泄漏"));
        assert!(prompt.contains("继续排查"));
    }

    /// 对照组：不带 ResumeRetry 时维持现状 —— 冲突崩溃直接落 error，
    /// 存量 agent_session_id 不变。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_consume_stream_without_retry_reports_crash_on_session_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&pool, "test").await.unwrap();
        let sid = session.id.0;

        let dump_path = tmp.path().join("prompt-dump.txt");
        let fake = write_fake_agent_cmd(tmp.path(), &dump_path);
        let agent_message_id = setup_conflict_scenario(&pool, &sid, &fake.to_string_lossy()).await;

        let process = crate::agent::spawn::spawn_active(
            &pool,
            sid.clone(),
            "继续排查".to_string(),
            Some("e3afdf0c-stale".to_string()),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let agents: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RunningAgent>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        consume_stream(
            process,
            EventBus::disabled(),
            sid.clone(),
            agent_message_id,
            pool.clone(),
            agents,
            CancellationToken::new(),
            None,
            None,
            None,
        )
        .await;

        let (status, _) = last_agent_message_status(&pool, &sid).await;
        assert_eq!(status, "error");

        let agent_session = crate::app::session::get_agent_session_id(&pool, &sid).await.unwrap();
        assert_eq!(agent_session.as_deref(), Some("e3afdf0c-stale"));
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
