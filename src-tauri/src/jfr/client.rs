use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// 一次上游工具调用结果（复用 analyzer 的形状；上游输出为 markdown 文本）
pub use crate::analyzer::client::CallOutcome;

#[async_trait]
pub trait JmcClient: Send + Sync {
    /// 调用上游 JMC MCP 工具。Err = 传输/进程层错误（进程疑似死亡）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    /// 终止工人进程
    async fn shutdown(&self);
}

/// 构造 JMC 工人进程 JVM 参数（纯函数，单独可测）。
/// - `--enable-preview`：JAR 由 jmc-jar.yml 以 release 21 + preview 构建（上游用
///   unnamed variable `_ ->`，Java 22 转正、21 预览），运行时必须带此 flag；
///   若降级回退到 25 非预览产物，此 flag 无害保留。
/// - UTF-8 强制三件套同 analyzer（issue #6：zh-CN Windows JVM 默认 GBK 输出）。
pub fn jmc_jvm_args(jar_path: &Path, xmx_gb: u32) -> Vec<String> {
    vec![
        "--enable-preview".to_string(),
        format!("-Xmx{xmx_gb}g"),
        "-Dfile.encoding=UTF-8".to_string(),
        "-Dstdout.encoding=UTF-8".to_string(),
        "-Dstderr.encoding=UTF-8".to_string(),
        "-jar".to_string(),
        jar_path.to_string_lossy().into_owned(),
    ]
}

/// rmcp stdio 子进程实现：java --enable-preview -Xmx<n>g -jar <jar>，MCP client 角色。
/// rmcp 3.1.4 适配：`RunningService::cancel(self)` 消费所有权，故 service 存于
/// `Mutex<Option<..>>` 供 shutdown 取出取消；工具调用走克隆的 `Peer`。
pub struct McpJmcClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 启动 JMC 工人进程并完成 MCP 握手（60s 超时）
pub async fn spawn_jmc_client(
    java: &crate::analyzer::java::JavaInfo,
    jar_path: &Path,
    xmx_gb: u32,
) -> Result<McpJmcClient, String> {
    use rmcp::ServiceExt;

    // verbatim 前缀（\\?\）会导致 java -jar ClassNotFound（issue #6），传入前必须剥掉
    let jar_path = crate::analyzer::manager::strip_verbatim_prefix(jar_path);

    let mut cmd = tokio::process::Command::new(&java.path);
    cmd.args(jmc_jvm_args(&jar_path, xmx_gb));
    // 上游默认只允许访问其工作目录内的 JFR 文件（Access denied）。Friday 本地 spawn、
    // stdio 单客户端、路径已由工具层 resolve_existing_file 校验——禁用校验对齐
    // heap analyzer（MAT）的同信任模型；local_path 本身就是唯一路径来源。
    cmd.env("JMC_MCP_DISABLE_PATH_VALIDATION", "true");
    let (transport, stderr) =
        rmcp::transport::child_process::TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 JMC 工人进程失败: {e}"))?;

    // 日志规范：子进程 stderr 必须读取记录（同时防止管道写满阻塞 JVM）。
    // read_until + from_utf8_lossy（GBK 安全），与 analyzer 相同。
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::with_capacity(256);
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF：进程退出
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']);
                        if !line.is_empty() {
                            tracing::info!(target: "jmc_worker", "worker: {line}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "jmc worker stderr drain ended with error");
                        break;
                    }
                }
            }
        });
    }

    tracing::info!(java = %java.path.display(), jar = %jar_path.display(), xmx_gb, pid = ?transport.id(), "jmc worker spawning");
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ().serve(transport),
    )
    .await
    .map_err(|_| "JMC 工人进程初始化超时（60s）".to_string())?
    .map_err(|e| format!("JMC MCP 握手失败: {e}"))?;

    let peer = service.peer().clone();
    Ok(McpJmcClient {
        peer,
        service: tokio::sync::Mutex::new(Some(service)),
    })
}

/// rmcp 调用结果 → JmcClient 语义映射（纯函数可测）：
/// - `McpError`（JSON-RPC 错误响应，如上游工具内 NPE 的 -32603）：传输层完好、
///   工人进程存活 → 工具级错误（is_error=true，业务透传，不 invalidate）
/// - 其余 ServiceError（TransportClosed 等）→ 传输层错误（进程疑似死亡 → invalidate）
/// - 非 Complete 响应 → 传输层错误（JMC 工具为一次性请求/响应）
/// issue #10：此前 JSON-RPC 错误响应被误判为传输错误，一次业务失败即
/// invalidate 整个工人进程并丢失上游录制缓存。
fn map_call_result(
    result: Result<rmcp::model::CallToolResponse, rmcp::ServiceError>,
) -> Result<CallOutcome, String> {
    match result {
        Err(rmcp::ServiceError::McpError(err)) => Ok(CallOutcome {
            text: format!("MCP error: {err}"),
            is_error: true,
        }),
        Err(e) => Err(format!("MCP 调用失败: {e}")),
        Ok(rmcp::model::CallToolResponse::Complete(result)) => Ok(CallOutcome {
            text: crate::analyzer::client::extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        }),
        Ok(other) => Err(format!("MCP 调用返回非最终结果: {other:?}")),
    }
}

#[async_trait]
impl JmcClient for McpJmcClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        // rmcp 3.1.4：Peer 侧入口为 call_tool_once；JMC 工具为一次性请求/响应，
        // 非 Complete 响应一律按传输层错误处理（对齐 analyzer client 适配写法）。
        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        } else {
            tracing::warn!(tool = %name, "non-object args passed to jmc client, treated as empty");
        }
        // rmcp 3.1.4：CallToolRequestParams 为 non_exhaustive，只能经 Default 构造
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = Some(arguments);
        map_call_result(self.peer.call_tool_once(params).await)
    }

    async fn shutdown(&self) {
        // cancel 消费 RunningService：取出后优雅关闭传输（关 stdin → 等 3s → kill）
        if let Some(service) = self.service.lock().await.take() {
            match service.cancel().await {
                Ok(reason) => {
                    tracing::info!(reason = ?reason, "jmc worker shut down");
                }
                Err(e) => {
                    tracing::warn!(?e, "jmc worker service cancel failed");
                }
            }
        }
    }
}

// ── 测试 mock（全 crate 测试可用）──

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
pub struct MockJmcClient {
    pub calls: Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
    pub shutdown_count: Arc<std::sync::atomic::AtomicUsize>,
    handler: Arc<
        dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallOutcome, String>> + Send>>
            + Send
            + Sync,
    >,
}

#[cfg(test)]
impl MockJmcClient {
    pub fn with_fn<F, Fut>(f: F) -> Self
    where
        F: Fn(&str, &Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<CallOutcome, String>> + Send + 'static,
    {
        Self {
            calls: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            shutdown_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            handler: Arc::new(move |name, args| Box::pin(f(name, args))),
        }
    }

    /// 所有调用成功返回固定文本
    pub fn ok(text: &str) -> Self {
        let text = text.to_string();
        Self::with_fn(move |_name, _args| {
            let text = text.clone();
            async move { Ok(CallOutcome { text, is_error: false }) }
        })
    }
}

#[cfg(test)]
#[async_trait]
impl JmcClient for MockJmcClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        self.calls.lock().await.push((name.to_string(), args.clone()));
        (self.handler)(name, args).await
    }

    async fn shutdown(&self) {
        self.shutdown_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jmc_jvm_args_force_utf8_and_preview() {
        let args = jmc_jvm_args(Path::new(r"C:\opt\jmc.jar"), 4);
        assert!(args.contains(&"-Dfile.encoding=UTF-8".to_string()), "args: {args:?}");
        assert!(args.contains(&"-Dstdout.encoding=UTF-8".to_string()));
        assert!(args.contains(&"-Dstderr.encoding=UTF-8".to_string()));
        assert_eq!(args.first().unwrap(), "--enable-preview");
        assert_eq!(args[1], "-Xmx4g");
        assert_eq!(args.last().unwrap(), r"C:\opt\jmc.jar");
    }

    #[tokio::test]
    async fn test_mock_client_records_calls() {
        let mock = MockJmcClient::ok("S");
        let out = mock.call_tool("jfr_overview", &serde_json::json!({"jfr_file_path": "x"})).await;
        assert!(out.is_ok());
        assert_eq!(out.unwrap().text, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "jfr_overview");
        assert_eq!(calls[0].1["jfr_file_path"], "x", "args must be cloned into calls record");
    }

    #[tokio::test]
    async fn test_mock_client_error_and_shutdown_count() {
        let mock = MockJmcClient::with_fn(|_name, _args| async { Err("boom".to_string()) });
        let out = mock.call_tool("jfr_overview", &serde_json::json!({})).await;
        match out {
            Err(e) => assert_eq!(e, "boom"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
        mock.shutdown().await;
        mock.shutdown().await;
        assert_eq!(
            mock.shutdown_count.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    /// issue #10 回归：JSON-RPC 错误响应（McpError，如上游工具内 NPE 的 -32603）
    /// 是服务器正常应答——必须归类为工具级错误（is_error=true）而非传输错误，
    /// 否则 manager 会 invalidate 存活的工人进程并丢失上游录制缓存。
    #[test]
    fn test_map_call_result_mcp_error_is_tool_level_error() {
        let err = rmcp::model::ErrorData::internal_error("Internal error", None);
        let out = map_call_result(Err(rmcp::ServiceError::McpError(err))).unwrap();
        assert!(out.is_error, "McpError must map to tool-level error");
        assert!(out.text.contains("-32603"), "text should carry the JSON-RPC code: {}", out.text);
        assert!(out.text.contains("Internal error"), "text should carry the message: {}", out.text);
    }

    /// issue #10 回归：真传输错误（TransportClosed）仍走 Err → Unavailable → invalidate
    #[test]
    fn test_map_call_result_transport_error_still_err() {
        assert!(map_call_result(Err(rmcp::ServiceError::TransportClosed)).is_err());
    }

    #[test]
    fn test_map_call_result_complete_and_non_complete() {
        // Complete + is_error=false → Ok
        let ok_result = rmcp::model::CallToolResult::success(Vec::new());
        let out = map_call_result(Ok(rmcp::model::CallToolResponse::Complete(ok_result))).unwrap();
        assert!(!out.is_error);
        // Complete + is_error=true → 工具级错误透传
        let mut err_result = rmcp::model::CallToolResult::default();
        err_result.is_error = Some(true);
        let out = map_call_result(Ok(rmcp::model::CallToolResponse::Complete(err_result))).unwrap();
        assert!(out.is_error);
        // 非 Complete 响应 → 传输层错误
        let input_required = rmcp::model::CallToolResponse::InputRequired(
            rmcp::model::InputRequiredResult::new(None, None),
        );
        assert!(map_call_result(Ok(input_required)).is_err());
    }

    /// verbatim（\\?\）前缀的 JAR 路径必须仍能完成 MCP 握手（issue #6 回归，对齐 analyzer 同名测试）。
    /// 需要本机 Java 21+ 与已下载的 JAR（scripts/fetch-jmc-jar.ps1），
    /// 不进常规测试（CI 无 java），显式 `--ignored` 运行。
    /// ⚠ Java 21 降级验证闸门：本机 java -version 应为 21.x 才真正验证降级成功。
    #[tokio::test]
    #[ignore = "requires local Java 21 and vendored JAR (run scripts/fetch-jmc-jar.ps1)"]
    async fn test_spawn_jmc_client_with_verbatim_jar_path() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/jmc/jmc-mcp-1.0.0.jar");
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-jmc-jar.ps1)", jar.display());
        // 复现 Tauri resource_dir() 返回的 verbatim 形式
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", jar.display()));
        let client = spawn_jmc_client(&java, &verbatim, 4)
            .await
            .expect("MCP handshake must succeed with verbatim jar path");
        let out = client
            .call_tool("jfrOverview", &serde_json::json!({"jfr_file_path": "nonexistent.jfr", "async": false}))
            .await
            .expect("tools/call must work");
        // 文件不存在 → 上游工具级错误（is_error=true），但传输层正常
        assert!(out.is_error, "expected tool-level error for nonexistent jfr, got: {}", out.text);
        client.shutdown().await;
    }
}
