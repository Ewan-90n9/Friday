use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse};

use crate::exec::channel::ExecChannel;
use crate::exec::ssh::shell_quote_single;

/// bridge 层错误：ssh exec 失败 / 目标机 curl 执行失败
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("ssh exec 失败: {0}")]
    Exec(String),
    #[error("目标机 curl 执行失败: {0}")]
    Curl(String),
}

/// 非预期响应错误消息里 body 的截断长度（完整 body 见日志）
const ERROR_BODY_PREFIX_CHARS: usize = 200;

/// curl -w 格式串：末行追加 http code（curl 把 \n 解释为换行）
const HTTP_CODE_SUFFIX: &str = "\\n%{http_code}";

/// 构造目标机 curl POST 命令：-D - 把响应头 dump 进 stdout（抓 mcp-session-id），
/// body 经 shell_quote_single 内嵌 --data（ExecChannel::run 无 stdin 通道，
/// ARG_MAX ~128KB 对 MCP 消息足够）
pub fn build_post_command(
    uri: &str,
    token: Option<&str>,
    session_id: Option<&str>,
    body: &str,
    timeout_secs: u64,
) -> String {
    let mut cmd = format!("curl -s -S -m {timeout_secs} -D - -X POST");
    if let Some(token) = token {
        cmd.push_str(" -H ");
        cmd.push_str(&shell_quote_single(&format!("Authorization: Bearer {token}")));
    }
    cmd.push_str(" -H ");
    cmd.push_str(&shell_quote_single("Content-Type: application/json"));
    cmd.push_str(" -H ");
    cmd.push_str(&shell_quote_single("Accept: application/json, text/event-stream"));
    // 空 Expect 头抑制 curl 对 >1KB body 自动加的 Expect: 100-continue
    // （避免 -D - dump 出 1xx 中间响应块 + 每请求 1s 等待）
    cmd.push_str(" -H ");
    cmd.push_str(&shell_quote_single("Expect:"));
    if let Some(session) = session_id {
        cmd.push_str(" -H ");
        cmd.push_str(&shell_quote_single(&format!("mcp-session-id: {session}")));
    }
    cmd.push_str(&format!(" --data {} {uri} -w {}", shell_quote_single(body), shell_quote_single(HTTP_CODE_SUFFIX)));
    cmd
}

/// 构造目标机 curl DELETE 命令（会话销毁），与 POST 同样的 -D/-w 结构
pub fn build_delete_command(
    uri: &str,
    token: Option<&str>,
    session_id: &str,
    timeout_secs: u64,
) -> String {
    let mut cmd = format!("curl -s -S -m {timeout_secs} -D - -X DELETE");
    if let Some(token) = token {
        cmd.push_str(" -H ");
        cmd.push_str(&shell_quote_single(&format!("Authorization: Bearer {token}")));
    }
    cmd.push_str(" -H ");
    cmd.push_str(&shell_quote_single(&format!("mcp-session-id: {session_id}")));
    cmd.push_str(&format!(" {uri} -w {}", shell_quote_single(HTTP_CODE_SUFFIX)));
    cmd
}

/// 解析 curl stdout：末行是 http code；其上是可选的响应头块（以 HTTP/ 状态行开头、
/// 空行分隔）——提取 mcp-session-id（大小写不敏感）；剩余为 body。
/// 存在多个响应头块时（1xx 中间响应）取最后一块。无响应头块时（lenient）整个前缀视为 body。
pub fn parse_curl_output(stdout: &str) -> Result<(u16, Option<String>, String), String> {
    let trimmed = stdout.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err("curl 输出为空（无 http code）".to_string());
    }
    let (rest, code_line) = match trimmed.rsplit_once('\n') {
        Some((rest, code)) => (rest, code),
        None => ("", trimmed),
    };
    let code: u16 = code_line
        .trim()
        .parse()
        .map_err(|_| format!("curl 输出末行不是 http code: {code_line:?}"))?;

    let rest_lines: Vec<&str> = rest.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let body = if rest_lines.first().is_some_and(|l| l.starts_with("HTTP/")) {
        // 新块 = 空行后的 HTTP/ 状态行；取最后一块为最终响应
        let last_block = rest_lines
            .iter()
            .enumerate()
            .filter(|(i, l)| l.starts_with("HTTP/") && (*i == 0 || rest_lines[i - 1].is_empty()))
            .map(|(i, _)| i)
            .last()
            .unwrap_or(0);
        match rest_lines.iter().skip(last_block).position(|l| l.is_empty()) {
            Some(offset) => {
                let sep = last_block + offset;
                let session_id = extract_session_id(&rest_lines[last_block..sep]);
                (session_id, rest_lines[sep + 1..].join("\n"))
            }
            None => (extract_session_id(&rest_lines[last_block..]), String::new()),
        }
    } else {
        (None, rest.to_string())
    };
    Ok((code, body.0, body.1))
}

/// 从响应头块提取 mcp-session-id（大小写不敏感）
fn extract_session_id(headers: &[&str]) -> Option<String> {
    headers.iter().find_map(|header| {
        let (name, value) = header.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("mcp-session-id")
            .then(|| value.trim().to_string())
    })
}

/// 解析 SSE body 为逐事件 data 负载：按空行分块，每块内 `data:` 行以 \n 连接
/// （SSE 规范：去一个可选前导空格；`id:`/`event:`/注释行忽略）。
/// arthas MCP 携带 progressToken 的请求会返回多事件（progress 通知 + 最终结果），
/// 无 data 的块跳过；无任何 data 返回空 Vec。
pub fn sse_body_to_events(body: &str) -> Vec<String> {
    let mut events: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !current.is_empty() {
                events.push(current.join("\n"));
                current.clear();
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            current.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if !current.is_empty() {
        events.push(current.join("\n"));
    }
    events
}

/// 错误消息用的 body 前缀（超长截断，完整内容走日志）
fn body_prefix(body: &str) -> String {
    let prefix: String = body.chars().take(ERROR_BODY_PREFIX_CHARS).collect();
    if body.chars().count() > ERROR_BODY_PREFIX_CHARS {
        format!("{prefix}...")
    } else {
        prefix
    }
}

/// MCP-over-exec HTTP 桥：rmcp StreamableHttpClient 的 ssh exec + curl 实现。
/// 目标机 curl 直接打 http://127.0.0.1:{port}/mcp，绕过 sshd AllowTcpForwarding 限制。
/// uri/token/session_id 均由 rmcp 参数流入（auth_header 为裸 token，Bearer 前缀在此拼接）；
/// bridge 自身只持有 exec 通道与超时预算。
#[derive(Clone)]
pub struct ExecHttpBridge {
    channel: Arc<dyn ExecChannel>,
    timeout_secs: u64,
}

impl ExecHttpBridge {
    pub fn new(channel: Arc<dyn ExecChannel>, timeout_secs: u64) -> Self {
        Self { channel, timeout_secs }
    }

    /// 执行 curl 命令并解析 (http_code, mcp-session-id, body)
    async fn exec_curl(
        &self,
        cmd: &str,
    ) -> Result<(u16, Option<String>, String), StreamableHttpError<BridgeError>> {
        let timed = tokio::time::timeout(Duration::from_secs(self.timeout_secs), self.channel.run(cmd)).await;
        let output = match timed {
            Ok(result) => result.map_err(|e| {
                tracing::warn!(error = %e, "arthas mcp bridge exec 失败");
                StreamableHttpError::Client(BridgeError::Exec(format!("ssh exec 失败: {e}")))
            })?,
            Err(_) => {
                tracing::warn!(timeout_secs = self.timeout_secs, "arthas mcp bridge exec 超时");
                return Err(StreamableHttpError::Client(BridgeError::Exec(format!(
                    "exec 超时（{}s）",
                    self.timeout_secs
                ))));
            }
        };
        if output.exit_code != 0 {
            tracing::warn!(exit_code = output.exit_code, stderr = %output.stderr, "arthas mcp bridge curl 非零退出");
            return Err(StreamableHttpError::Client(BridgeError::Curl(format!(
                "curl 退出码 {}（目标 curl 不可用或连接失败）: {}",
                output.exit_code,
                output.stderr.trim()
            ))));
        }
        parse_curl_output(&output.stdout).map_err(|e| {
            tracing::warn!(error = %e, "arthas mcp bridge 响应解析失败");
            StreamableHttpError::Client(BridgeError::Curl(e))
        })
    }
}

impl StreamableHttpClient for ExecHttpBridge {
    type Error = BridgeError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let body =
            serde_json::to_string(&message).map_err(StreamableHttpError::Deserialize)?;
        let cmd = build_post_command(
            uri.as_ref(),
            auth_header.as_deref(),
            session_id.as_deref(),
            &body,
            self.timeout_secs,
        );
        tracing::debug!(cmd = %cmd, "arthas mcp bridge post");
        let (code, session_from_header, resp_body) = self.exec_curl(&cmd).await?;
        tracing::debug!(code, session = ?session_from_header, "arthas mcp bridge post 响应");
        match code {
            202 => Ok(StreamableHttpPostResponse::Accepted),
            200 => {
                let trimmed = resp_body.trim();
                if trimmed.starts_with('{') {
                    let msg: ServerJsonRpcMessage = serde_json::from_str(trimmed)?;
                    Ok(StreamableHttpPostResponse::Json(msg, session_from_header))
                } else {
                    let events = sse_body_to_events(&resp_body);
                    if events.is_empty() {
                        tracing::warn!(code, body = %resp_body, "arthas mcp bridge 收到无法识别的 200 响应");
                        return Err(StreamableHttpError::UnexpectedServerResponse(
                            format!("HTTP 200 无法识别的响应体: {}", body_prefix(&resp_body)).into(),
                        ));
                    }
                    // arthas MCP 实测：SSE 形态 body，progress 通知 + 最终结果逐事件返回
                    // （rmcp 发 progressToken 时多事件，无 GET 流依赖）。最终事件即请求
                    // 响应，提前校验可解析以给出清晰错误。
                    let last = events.last().expect("checked non-empty");
                    let _msg: ServerJsonRpcMessage = serde_json::from_str(last)?;
                    let stream = futures::stream::iter(
                        events.into_iter().map(|data| {
                            Ok::<Sse, SseError>(Sse { event: None, data: Some(data), id: None, retry: None })
                        }),
                    )
                    .boxed();
                    Ok(StreamableHttpPostResponse::Sse(stream, session_from_header))
                }
            }
            401 => {
                tracing::warn!(body = %resp_body, "arthas mcp bridge 收到 401（token 不匹配?）");
                Err(StreamableHttpError::UnexpectedServerResponse(
                    format!("HTTP 401: 未授权（token 不匹配?）: {}", body_prefix(&resp_body)).into(),
                ))
            }
            code => {
                tracing::warn!(code, body = %resp_body, "arthas mcp bridge 收到非 2xx 响应");
                Err(StreamableHttpError::UnexpectedServerResponse(
                    format!("HTTP {code}: {}", body_prefix(&resp_body)).into(),
                ))
            }
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let cmd = build_delete_command(
            uri.as_ref(),
            auth_header.as_deref(),
            session_id.as_ref(),
            self.timeout_secs,
        );
        tracing::debug!(cmd = %cmd, "arthas mcp bridge delete session");
        let (code, _, body) = self.exec_curl(&cmd).await?;
        if (200..300).contains(&code) {
            Ok(())
        } else {
            tracing::warn!(code, body = %body, "arthas mcp bridge delete session 非 2xx");
            Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {code}: {}", body_prefix(&body)).into(),
            ))
        }
    }

    /// arthas MCP 单事件响应模型不需要 GET 流（控制器已实测验证），恒返回空流
    async fn get_stream(
        &self,
        _uri: Arc<str>,
        _session_id: Option<Arc<str>>,
        _last_event_id: Option<String>,
        _auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        Ok(futures::stream::empty().boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::ExecOutput;
    use futures::StreamExt;
    use std::collections::VecDeque;

    struct RecordingChannel {
        calls: tokio::sync::Mutex<Vec<String>>,
        responses: tokio::sync::Mutex<VecDeque<ExecOutput>>,
    }

    impl RecordingChannel {
        fn new(responses: Vec<(&str, i32)>) -> Arc<Self> {
            let dq = responses
                .into_iter()
                .map(|(o, c)| ExecOutput { stdout: o.to_string(), stderr: String::new(), exit_code: c })
                .collect();
            Arc::new(Self {
                calls: tokio::sync::Mutex::new(Vec::new()),
                responses: tokio::sync::Mutex::new(dq),
            })
        }

        async fn calls(&self) -> Vec<String> {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl ExecChannel for RecordingChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            Ok(self.responses.lock().await.pop_front().unwrap_or(ExecOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
            }))
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    struct FailingChannel;

    #[async_trait::async_trait]
    impl ExecChannel for FailingChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Err("ssh broken".into())
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    struct SlowChannel;

    #[async_trait::async_trait]
    impl ExecChannel for SlowChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    fn bridge(channel: Arc<dyn ExecChannel>) -> ExecHttpBridge {
        ExecHttpBridge::new(channel, 30)
    }

    fn client_message() -> ClientJsonRpcMessage {
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap()
    }

    fn server_response_json() -> String {
        let msg = ServerJsonRpcMessage::response(
            rmcp::model::ServerResult::ListToolsResult(rmcp::model::ListToolsResult::default()),
            rmcp::model::NumberOrString::Number(1),
        );
        serde_json::to_string(&msg).unwrap()
    }

    // ── build_post_command ──

    #[test]
    fn test_build_post_command_full_shape() {
        let cmd = build_post_command(
            "http://127.0.0.1:18563/mcp",
            Some("tok123"),
            Some("sess-9"),
            r#"{"jsonrpc":"2.0"}"#,
            60,
        );
        assert!(cmd.starts_with("curl -s -S -m 60 -D - -X POST "), "cmd: {cmd}");
        assert!(cmd.contains("-H 'Authorization: Bearer tok123'"), "cmd: {cmd}");
        assert!(cmd.contains("-H 'Content-Type: application/json'"), "cmd: {cmd}");
        assert!(cmd.contains("-H 'Accept: application/json, text/event-stream'"), "cmd: {cmd}");
        assert!(cmd.contains("-H 'Expect:'"), "cmd: {cmd}");
        assert!(cmd.contains("-H 'mcp-session-id: sess-9'"), "cmd: {cmd}");
        assert!(cmd.contains(r#"--data '{"jsonrpc":"2.0"}'"#), "cmd: {cmd}");
        assert!(
            cmd.ends_with(r#"http://127.0.0.1:18563/mcp -w '\n%{http_code}'"#),
            "cmd: {cmd}"
        );
    }

    #[test]
    fn test_build_post_command_quotes_body_and_omits_optional_headers() {
        let cmd = build_post_command("http://127.0.0.1:18563/mcp", None, None, r#"{"expr":"it's"}"#, 10);
        assert!(!cmd.contains("Authorization"), "cmd: {cmd}");
        assert!(!cmd.contains("mcp-session-id"), "cmd: {cmd}");
        assert!(cmd.contains(r#"--data '{"expr":"it'\''s"}'"#), "cmd: {cmd}");
        assert!(cmd.contains("-m 10 "), "cmd: {cmd}");
    }

    #[test]
    fn test_build_delete_command_shape() {
        let cmd = build_delete_command("http://127.0.0.1:18563/mcp", Some("tok123"), "sess-9", 30);
        assert!(cmd.starts_with("curl -s -S -m 30 -D - -X DELETE "), "cmd: {cmd}");
        assert!(cmd.contains("-H 'Authorization: Bearer tok123'"), "cmd: {cmd}");
        assert!(cmd.contains("-H 'mcp-session-id: sess-9'"), "cmd: {cmd}");
        assert!(cmd.ends_with(r#"http://127.0.0.1:18563/mcp -w '\n%{http_code}'"#), "cmd: {cmd}");
    }

    // ── parse_curl_output ──

    #[test]
    fn test_parse_curl_output_headers_body_code() {
        let stdout = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nmcp-session-id: abc-123\r\n\r\n{}\n200",
            server_response_json()
        );
        let (code, session, body) = parse_curl_output(&stdout).unwrap();
        assert_eq!(code, 200);
        assert_eq!(session.as_deref(), Some("abc-123"));
        assert_eq!(body, server_response_json());
    }

    #[test]
    fn test_parse_curl_output_header_case_insensitive() {
        let stdout = "HTTP/1.1 200\r\nMCP-SESSION-ID: xyz\r\n\r\n{\"jsonrpc\":\"2.0\"}\n200";
        let (code, session, _) = parse_curl_output(stdout).unwrap();
        assert_eq!(code, 200);
        assert_eq!(session.as_deref(), Some("xyz"));
    }

    #[test]
    fn test_parse_curl_output_no_headers() {
        let stdout = format!("{}\n200", server_response_json());
        let (code, session, body) = parse_curl_output(&stdout).unwrap();
        assert_eq!(code, 200);
        assert_eq!(session, None);
        assert_eq!(body, server_response_json());
    }

    #[test]
    fn test_parse_curl_output_skips_100_continue_interim_block() {
        let stdout = format!(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nmcp-session-id: abc\r\n\r\n{}\n200",
            server_response_json()
        );
        let (code, session, body) = parse_curl_output(&stdout).unwrap();
        assert_eq!(code, 200);
        assert_eq!(session.as_deref(), Some("abc"));
        assert_eq!(body, server_response_json());
    }

    #[test]
    fn test_parse_curl_output_empty_body_202() {
        let stdout = "HTTP/1.1 202 Accepted\r\n\r\n\n202";
        let (code, session, body) = parse_curl_output(stdout).unwrap();
        assert_eq!(code, 202);
        assert_eq!(session, None);
        assert_eq!(body, "");
    }

    #[test]
    fn test_parse_curl_output_rejects_missing_code() {
        assert!(parse_curl_output("").is_err());
        assert!(parse_curl_output("garbage without code line").is_err());
    }

    // ── sse_body_to_events ──

    #[test]
    fn test_sse_body_to_events_single_event() {
        let json = server_response_json();
        let body = format!("id: abc\r\nevent: message\r\ndata: {json}\r\n\r\n");
        assert_eq!(sse_body_to_events(&body), vec![json]);
    }

    #[test]
    fn test_sse_body_to_events_progress_then_final() {
        // arthas 实测形态：携带 progressToken 的 tools/call 返回多个事件
        // （progress 通知 × N + 最终结果；每事件带 id:/event: 行）
        let progress1 = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"9","progress":1.0,"total":3.0}}"#;
        let progress2 = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"9","progress":2.0,"total":3.0}}"#;
        let final_resp = server_response_json();
        let body = format!(
            "id: 1\nevent: message\ndata: {progress1}\n\n\
             id: 2\nevent: message\ndata: {progress2}\n\n\
             id: 3\nevent: message\ndata: {final_resp}\n\n"
        );
        assert_eq!(
            sse_body_to_events(&body),
            vec![progress1.to_string(), progress2.to_string(), final_resp]
        );
    }

    #[test]
    fn test_sse_body_to_events_multiline_data_joined() {
        assert_eq!(
            sse_body_to_events("data: {\"a\":\ndata: 1}"),
            vec!["{\"a\":\n1}".to_string()]
        );
    }

    #[test]
    fn test_sse_body_to_events_no_space_after_colon() {
        assert_eq!(sse_body_to_events("data:{\"x\":1}"), vec!["{\"x\":1}".to_string()]);
    }

    #[test]
    fn test_sse_body_to_events_empty_when_no_data() {
        assert!(sse_body_to_events("event: message\n").is_empty());
        assert!(sse_body_to_events("").is_empty());
    }

    // ── post_message（StreamableHttpClient trait）──

    #[tokio::test]
    async fn test_post_message_json_response() {
        let json = server_response_json();
        let stdout = format!("HTTP/1.1 200 OK\r\nmcp-session-id: sess-9\r\n\r\n{json}\n200");
        let channel = RecordingChannel::new(vec![(&stdout, 0)]);
        let b = bridge(channel.clone());
        let message = client_message();
        let expected: serde_json::Value = serde_json::from_str(&json).unwrap();
        let sent_body = serde_json::to_string(&message).unwrap();

        let resp = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                message,
                Some(Arc::from("sess-old")),
                Some("tok123".to_string()),
                HashMap::new(),
            )
            .await
            .unwrap();

        match resp {
            StreamableHttpPostResponse::Json(msg, session) => {
                assert_eq!(session.as_deref(), Some("sess-9"));
                assert_eq!(serde_json::to_value(&msg).unwrap(), expected);
            }
            other => panic!("expected Json, got {other:?}"),
        }

        let calls = channel.calls().await;
        assert_eq!(calls.len(), 1, "calls: {calls:?}");
        assert!(calls[0].contains("-X POST"), "cmd: {}", calls[0]);
        assert!(calls[0].contains("-H 'Authorization: Bearer tok123'"), "cmd: {}", calls[0]);
        assert!(calls[0].contains("-H 'mcp-session-id: sess-old'"), "cmd: {}", calls[0]);
        assert!(calls[0].contains(&format!("--data '{sent_body}'")), "cmd: {}", calls[0]);
    }

    #[tokio::test]
    async fn test_post_message_sse_response_single_event() {
        let json = server_response_json();
        let stdout = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nmcp-session-id: sess-1\r\n\r\nevent: message\ndata: {json}\n\n200"
        );
        let channel = RecordingChannel::new(vec![(&stdout, 0)]);
        let b = bridge(channel.clone());

        let resp = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap();

        match resp {
            StreamableHttpPostResponse::Sse(mut stream, session) => {
                assert_eq!(session.as_deref(), Some("sess-1"));
                let mut events = 0;
                while let Some(event) = stream.next().await {
                    let event = event.unwrap();
                    assert_eq!(event.data.as_deref(), Some(json.as_str()));
                    events += 1;
                }
                assert_eq!(events, 1, "expected exactly one sse event");
            }
            other => panic!("expected Sse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_post_message_sse_progress_then_final() {
        // arthas 实测：携带 progressToken 的请求返回 progress 通知 + 最终结果多事件流
        let progress = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"9","progress":1.0,"total":3.0}}"#;
        let final_resp = server_response_json();
        let stdout = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nmcp-session-id: sess-7\r\n\r\n\
             id: 1\nevent: message\ndata: {progress}\n\n\
             id: 2\nevent: message\ndata: {final_resp}\n\n\
             200"
        );
        let channel = RecordingChannel::new(vec![(&stdout, 0)]);
        let b = bridge(channel);

        let resp = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                Some(Arc::from("sess-7")),
                Some("tok123".to_string()),
                HashMap::new(),
            )
            .await
            .unwrap();

        match resp {
            StreamableHttpPostResponse::Sse(mut stream, session) => {
                assert_eq!(session.as_deref(), Some("sess-7"));
                let first = stream.next().await.unwrap().unwrap();
                assert_eq!(first.data.as_deref(), Some(progress));
                let second = stream.next().await.unwrap().unwrap();
                assert_eq!(second.data.as_deref(), Some(final_resp.as_str()));
                assert!(stream.next().await.is_none(), "stream should end after final event");
            }
            other => panic!("expected Sse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_post_message_202_accepted() {
        let channel = RecordingChannel::new(vec![("HTTP/1.1 202 Accepted\r\n\r\n\n202", 0)]);
        let b = bridge(channel.clone());

        let resp = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap();

        assert!(matches!(resp, StreamableHttpPostResponse::Accepted), "resp: {resp:?}");
    }

    #[tokio::test]
    async fn test_post_message_401_unauthorized() {
        let stdout = "HTTP/1.1 401\r\nContent-Type: application/json\r\n\r\n{\"error\":\"unauthorized\"}\n401";
        let channel = RecordingChannel::new(vec![(stdout, 0)]);
        let b = bridge(channel);

        let err = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                Some("wrong-token".to_string()),
                HashMap::new(),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("HTTP 401"), "err: {err}");
    }

    #[tokio::test]
    async fn test_post_message_non_2xx_error() {
        let stdout = "HTTP/1.1 500\r\n\r\nboom\n500";
        let channel = RecordingChannel::new(vec![(stdout, 0)]);
        let b = bridge(channel);

        let err = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("HTTP 500"), "err: {err}");
        assert!(err.to_string().contains("boom"), "err: {err}");
    }

    #[tokio::test]
    async fn test_post_message_exec_failure_maps_to_client_error() {
        let b = bridge(Arc::new(FailingChannel));
        let err = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StreamableHttpError::Client(_)), "err: {err}");
        assert!(err.to_string().contains("exec"), "err: {err}");
    }

    #[tokio::test]
    async fn test_post_message_timeout_maps_to_client_error() {
        let b = ExecHttpBridge::new(Arc::new(SlowChannel), 0);
        let err = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StreamableHttpError::Client(_)), "err: {err}");
        assert!(err.to_string().contains("超时") || err.to_string().contains("timeout"), "err: {err}");
    }

    #[tokio::test]
    async fn test_post_message_curl_exit_nonzero_maps_to_client_error() {
        let channel = RecordingChannel::new(vec![("", 7)]);
        let b = bridge(channel);
        let err = b
            .post_message(
                Arc::from("http://127.0.0.1:18563/mcp"),
                client_message(),
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StreamableHttpError::Client(_)), "err: {err}");
        assert!(err.to_string().contains("curl"), "err: {err}");
    }

    // ── delete_session ──

    #[tokio::test]
    async fn test_delete_session_ok() {
        let channel = RecordingChannel::new(vec![("HTTP/1.1 200 OK\r\n\r\n\n200", 0)]);
        let b = bridge(channel.clone());

        b.delete_session(
            Arc::from("http://127.0.0.1:18563/mcp"),
            Arc::from("sess-9"),
            Some("tok123".to_string()),
            HashMap::new(),
        )
        .await
        .unwrap();

        let calls = channel.calls().await;
        assert_eq!(calls.len(), 1, "calls: {calls:?}");
        assert!(calls[0].contains("-X DELETE"), "cmd: {}", calls[0]);
        assert!(calls[0].contains("-H 'mcp-session-id: sess-9'"), "cmd: {}", calls[0]);
        assert!(calls[0].contains("-H 'Authorization: Bearer tok123'"), "cmd: {}", calls[0]);
    }

    #[tokio::test]
    async fn test_delete_session_404_errors() {
        let channel = RecordingChannel::new(vec![("HTTP/1.1 404\r\n\r\n\n404", 0)]);
        let b = bridge(channel);

        let err = b
            .delete_session(
                Arc::from("http://127.0.0.1:18563/mcp"),
                Arc::from("sess-9"),
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("404"), "err: {err}");
    }

    // ── get_stream ──

    #[tokio::test]
    async fn test_get_stream_returns_empty() {
        let b = bridge(Arc::new(FailingChannel));
        let stream = b
            .get_stream(
                Arc::from("http://127.0.0.1:18563/mcp"),
                None,
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        let events: Vec<Result<Sse, SseError>> = stream.collect().await;
        assert!(events.is_empty(), "events: {events:?}");
    }

    // ── 本地全链路回归（真 arthas + 真 curl；#[ignore]，手动 --ignored 运行）──

    /// exec 通道的本地替身：命令经 Git Bash 执行——bridge 生成的 POSIX 命令
    /// （单引号转义、curl 参数）原样可跑，语义与目标机 Linux bash 一致。
    struct LocalExecChannel {
        bash: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl ExecChannel for LocalExecChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            let out = tokio::process::Command::new(&self.bash)
                .arg("-c")
                .arg(cmd)
                .output()
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;
            Ok(ExecOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                exit_code: out.status.code().unwrap_or(-1),
            })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    /// e2e 固定口令（arthas.password，仅本地 throwaway 实例用）
    const E2E_PASSWORD: &str = "testtoken123";

    /// 本地全链路回归（真 arthas + 真 curl）：math-game JVM（arthas 官方 demo，
    /// 随 bin 包分发）+ arthas-boot attach + exec HTTP 桥 MCP 握手 + dashboard /
    /// thread 工具调用 + shutdown（DELETE session）。
    /// 依赖本机 JDK（PATH java）、Git Bash（POSIX 命令语义）、resources/arthas 包；
    /// 不进 CI（#[ignore]），环境缺失时早退跳过。
    #[tokio::test]
    #[ignore]
    async fn local_e2e_bridge_against_real_arthas() {
        use crate::arthas::manager::ArthasClient;

        // 前置：Git Bash
        let bash = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
        .or_else(|| which::which("bash").ok());
        let Some(bash) = bash else {
            eprintln!("skip: Git Bash not found (provides POSIX command semantics like the target host)");
            return;
        };
        // 前置：JDK（java + attach API）
        let Ok(java) = which::which("java") else {
            eprintln!("skip: java not on PATH (JDK required for arthas-boot)");
            return;
        };
        // 前置：vendored arthas 包
        let zip = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("arthas")
            .join("arthas-bin-4.3.5.zip");
        if !zip.exists() {
            eprintln!("skip: arthas package not found at {}", zip.display());
            return;
        }

        // 解压到 %TEMP% 缓存（复用避免每次解 17MB）
        let dir = std::env::temp_dir().join("friday-arthas-e2e-4.3.5");
        if !dir.join("arthas-boot.jar").exists() {
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let to_slash = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
            let cmd = format!("unzip -o -q '{}' -d '{}'", to_slash(&zip), to_slash(&dir));
            let out = tokio::process::Command::new(&bash)
                .arg("-c")
                .arg(&cmd)
                .output()
                .await
                .expect("run unzip");
            assert!(
                out.status.success() && dir.join("arthas-boot.jar").exists(),
                "unzip arthas package failed: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }

        // 端口：18599 起避开生产常用段，探测取空闲的 http + telnet 预检两个
        let mut free_ports: Vec<u16> = Vec::new();
        for port in 18599..18620u16 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
                free_ports.push(port);
                if free_ports.len() == 2 {
                    break;
                }
            }
        }
        if free_ports.len() < 2 {
            eprintln!("skip: no free ports in 18599..18619 for e2e test");
            return;
        }
        let (http_port, telnet_det_port) = (free_ports[0], free_ports[1]);

        let token = E2E_PASSWORD;
        std::fs::write(
            dir.join("arthas.properties"),
            crate::arthas::attach::arthas_properties_content(http_port, token),
        )
        .expect("write arthas.properties");

        // 目标 JVM：math-game（长驻）。stdin 持有保活（EOF 会退出）；stdout/stderr 丢弃防管道写满
        let mut game = tokio::process::Command::new(&java)
            .args(["-jar", "math-game.jar"])
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn math-game");
        let _game_stdin = game.stdin.take();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await; // 等 JVM 启动
        let pid = game.id().expect("math-game pid");

        // attach（与生产 attach_command 同构；--attach-only 后 boot 自行退出）
        let attach = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            tokio::process::Command::new(&java)
                .args([
                    "-jar",
                    "arthas-boot.jar",
                    "--attach-only",
                    "--http-port",
                    &http_port.to_string(),
                    "--telnet-port",
                    &telnet_det_port.to_string(),
                    &pid.to_string(),
                ])
                .current_dir(&dir)
                .output(),
        )
        .await
        .expect("arthas-boot attach timed out (90s)")
        .expect("run arthas-boot");
        assert!(
            attach.status.success(),
            "arthas-boot attach failed (exit {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            attach.status.code(),
            String::from_utf8_lossy(&attach.stdout),
            String::from_utf8_lossy(&attach.stderr),
        );

        // 等 arthas HTTP server 就绪（端口可连）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", http_port)).await.is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "arthas http server not ready on port {http_port} within 60s",
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // THE 焦点：exec HTTP 桥 + 真 curl 的 MCP 全链路
        let channel: Arc<dyn ExecChannel> = Arc::new(LocalExecChannel { bash: bash.clone() });
        let bridge = ExecHttpBridge::new(channel.clone(), 60);
        let url = format!("http://127.0.0.1:{http_port}/mcp");
        let client = crate::arthas::client::connect_arthas_client(bridge, &url, token)
            .await
            .expect("mcp handshake via exec bridge (real curl)");

        // tools/call dashboard（POST 经真 curl，SSE 单事件响应解析）
        let dashboard = client
            .call_tool("dashboard", &serde_json::json!({}))
            .await
            .expect("dashboard call");
        assert!(!dashboard.is_error, "dashboard returned error: {}", dashboard.text);
        assert!(!dashboard.text.trim().is_empty(), "dashboard returned empty text");

        // 第二次调用（验证 mcp-session-id 经 -D - 解析后在后续请求正确回传）
        let thread = client
            .call_tool("thread", &serde_json::json!({}))
            .await
            .expect("thread call");
        assert!(!thread.is_error, "thread returned error: {}", thread.text);
        assert!(!thread.text.trim().is_empty(), "thread returned empty text");

        // shutdown（DELETE session 经真 curl）
        client.shutdown().await;

        // 清理：best-effort 停 arthas agent + 杀 math-game（kill_on_drop 兜底）
        let _ = channel
            .run(&crate::arthas::attach::stop_command(http_port, token))
            .await;
        let _ = game.kill().await;
    }
}
