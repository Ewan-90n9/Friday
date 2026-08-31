use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// 一次上游工具调用结果（上游输出为 markdown 文本）
#[derive(Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

#[async_trait]
pub trait HeapAnalyzerClient: Send + Sync {
    /// 调用上游 MCP 工具。Err = 传输/进程层错误（进程疑似死亡）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    /// 终止工人进程
    async fn shutdown(&self);
}

/// 从 CallToolResult 提取全部 text 内容块（拼接）
pub fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// rmcp stdio 子进程实现：java -Xmx<n>g -jar <jar>，MCP client 角色。
/// rmcp 3.1.4 适配：`RunningService::cancel(self)` 消费所有权，故 service 存于
/// `Mutex<Option<..>>` 供 shutdown 取出取消；工具调用走克隆的 `Peer`。
pub struct McpHeapAnalyzerClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 构造工人进程 JVM 参数（纯函数，单独可测）。
/// - `-Dfile.encoding=UTF-8`：zh-CN Windows 上 JVM 默认 GBK 输出（stderr 里的中文
///   被 Friday 按 UTF-8 lossy 读取后永久丢失，见 issue #6 日志乱码），强制 UTF-8
///   使 stderr 与 MCP stdio 协议编码一致
/// - `-Dstdout.encoding/stderr.encoding=UTF-8`：JDK 19+ 的流编码属性，
///   覆盖 Windows 控制台 codepage 跟随行为（JDK 21 认识；早于 19 的 JVM 会
///   警告未知系统属性但不影响启动——本项目要求 Java 21+，安全）
pub fn analyzer_jvm_args(jar_path: &Path, xmx_gb: u32) -> Vec<String> {
    vec![
        format!("-Xmx{xmx_gb}g"),
        "-Dfile.encoding=UTF-8".to_string(),
        "-Dstdout.encoding=UTF-8".to_string(),
        "-Dstderr.encoding=UTF-8".to_string(),
        "-jar".to_string(),
        jar_path.to_string_lossy().into_owned(),
    ]
}

/// 启动工人进程并完成 MCP 握手（60s 超时）
pub async fn spawn_analyzer_client(
    java: &crate::analyzer::java::JavaInfo,
    jar_path: &Path,
    xmx_gb: u32,
) -> Result<McpHeapAnalyzerClient, String> {
    use rmcp::ServiceExt;

    // verbatim 前缀（\\?\）会导致 java -jar ClassNotFound（issue #6），传入前必须剥掉
    let jar_path = crate::analyzer::manager::strip_verbatim_prefix(jar_path);

    let mut cmd = tokio::process::Command::new(&java.path);
    cmd.args(analyzer_jvm_args(&jar_path, xmx_gb));
    let (transport, stderr) =
        rmcp::transport::child_process::TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动分析器进程失败: {e}"))?;

    // 日志规范：子进程 stderr 必须读取记录（同时防止管道写满阻塞 JVM）。
    // 不能用 lines()：GBK 等非 UTF-8 字节会使其报错并静默退出 drain 循环，
    // 导致 stderr 无人读取、管道写满、JVM 阻塞。改用 read_until + from_utf8_lossy。
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
                            tracing::info!(target: "heap_analyzer", "worker: {line}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "heap analyzer stderr drain ended with error");
                        break;
                    }
                }
            }
        });
    }

    tracing::info!(java = %java.path.display(), jar = %jar_path.display(), xmx_gb, pid = ?transport.id(), "heap analyzer worker spawning");
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ().serve(transport),
    )
    .await
    .map_err(|_| "分析器进程初始化超时（60s）".to_string())?
    .map_err(|e| format!("分析器 MCP 握手失败: {e}"))?;

    let peer = service.peer().clone();
    Ok(McpHeapAnalyzerClient {
        peer,
        service: tokio::sync::Mutex::new(Some(service)),
    })
}

#[async_trait]
impl HeapAnalyzerClient for McpHeapAnalyzerClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        // rmcp 3.1.4：Peer 侧入口为 call_tool_once（返回 CallToolResponse 枚举），
        // 高层 call_tool（自动驱动 MRTR 轮次）挂在 RunningService 上；
        // 分析器工具为一次性请求/响应，非 Complete 响应一律按传输层错误处理。
        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        } else {
            tracing::warn!(tool = %name, "non-object args passed to analyzer client, treated as empty");
        }
        // rmcp 3.1.4：CallToolRequestParams 为 non_exhaustive，只能经 Default 构造
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = Some(arguments);
        let result = self
            .peer
            .call_tool_once(params)
            .await
            .map_err(|e| format!("MCP 调用失败: {e}"))?;
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            other => return Err(format!("MCP 调用返回非最终结果: {other:?}")),
        };
        Ok(CallOutcome {
            text: extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn shutdown(&self) {
        // cancel 消费 RunningService：取出后优雅关闭传输（关 stdin → 等 3s → kill）
        if let Some(service) = self.service.lock().await.take() {
            match service.cancel().await {
                Ok(reason) => {
                    tracing::info!(reason = ?reason, "heap analyzer worker shut down");
                }
                Err(e) => {
                    tracing::warn!(?e, "heap analyzer service cancel failed");
                }
            }
        }
    }
}

// ── 测试 mock（全 crate 测试可用）──

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
pub struct MockHeapAnalyzerClient {
    pub calls: Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
    pub shutdown_count: Arc<std::sync::atomic::AtomicUsize>,
    handler: Arc<
        dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallOutcome, String>> + Send>>
            + Send
            + Sync,
    >,
}

#[cfg(test)]
impl MockHeapAnalyzerClient {
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
impl HeapAnalyzerClient for MockHeapAnalyzerClient {
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
    fn test_analyzer_jvm_args_force_utf8() {
        // issue #6：zh-CN Windows JVM 默认 GBK 输出 → stderr 中文被 UTF-8 lossy
        // 读取后永久丢失。JVM 参数必须强制 UTF-8。
        let args = analyzer_jvm_args(Path::new(r"C:\opt\analyzer.jar"), 4);
        assert!(args.contains(&"-Dfile.encoding=UTF-8".to_string()), "args: {args:?}");
        assert!(args.contains(&"-Dstdout.encoding=UTF-8".to_string()));
        assert!(args.contains(&"-Dstderr.encoding=UTF-8".to_string()));
        assert_eq!(args.first().unwrap(), "-Xmx4g");
        assert_eq!(args.last().unwrap(), r"C:\opt\analyzer.jar");
    }

    #[test]
    fn test_extract_text_joins_text_blocks() {
        let result = rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text("hello"),
            rmcp::model::ContentBlock::text("world"),
        ]);
        assert_eq!(extract_text(&result), "hello\nworld");
    }

    #[test]
    fn test_extract_text_empty_content() {
        let result = rmcp::model::CallToolResult::success(vec![]);
        assert_eq!(extract_text(&result), "");
    }

    #[test]
    fn test_extract_text_from_error_result() {
        let result = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("bad")]);
        assert_eq!(extract_text(&result), "bad");
        assert!(result.is_error.unwrap_or(false));
    }

    #[tokio::test]
    async fn test_mock_client_records_calls() {
        let mock = MockHeapAnalyzerClient::ok("S");
        let out = mock.call_tool("open_heap_dump", &serde_json::json!({"path": "x"})).await;
        assert!(out.is_ok());
        assert_eq!(out.unwrap().text, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "open_heap_dump");
    }

    #[tokio::test]
    async fn test_mock_client_error_and_shutdown_count() {
        let mock = MockHeapAnalyzerClient::with_fn(|_name, _args| async { Err("boom".to_string()) });
        let out = mock.call_tool("open_heap_dump", &serde_json::json!({"path": "x"})).await;
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
        let calls = mock.calls.lock().await;
        assert_eq!(calls[0].1["path"], "x");
    }

    /// issue #6 回归：verbatim（\\?\）前缀的 JAR 路径必须仍能完成 MCP 握手。
    /// 需要本机 Java 21+ 与已下载的 JAR（scripts/fetch-analyzer-jar.ps1），
    /// 不进常规测试（CI 无 java），显式 `--ignored` 运行。
    #[tokio::test]
    #[ignore = "requires local Java 21+ and vendored JAR"]
    async fn test_spawn_analyzer_client_with_verbatim_jar_path() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/analyzer/jvm-heap-dump-mcp-0.2.0-all.jar");
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-analyzer-jar.ps1)", jar.display());
        // 复现 Tauri resource_dir() 返回的 verbatim 形式
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", jar.display()));
        let client = spawn_analyzer_client(&java, &verbatim, 4)
            .await
            .expect("MCP handshake must succeed with verbatim jar path");
        let out = client
            .call_tool("open_heap_dump", &serde_json::json!({"path": "nonexistent.hprof", "id": "regress"}))
            .await
            .expect("tools/call must work");
        // 文件不存在 → 上游工具级错误（is_error=true），但传输层正常
        assert!(out.is_error, "expected tool-level error for nonexistent dump, got: {}", out.text);
        client.shutdown().await;
    }

    /// issue #6 日志乱码回归：工人 JVM 必须以 UTF-8 输出 stderr。
    /// 用真实工人进程（MAT bootstrap 自身就会输出「信息:」中文日志）验证：
    /// 读取其 stderr 必须 strict UTF-8 可解码且不含 U+FFFD 替换字符。
    /// 需要本机 Java 21+ 与已下载的 JAR；CI 无 java，显式 `--ignored` 运行。
    #[tokio::test]
    #[ignore = "requires local Java 21+ and vendored JAR"]
    async fn test_worker_stderr_is_utf8() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/analyzer/jvm-heap-dump-mcp-0.2.0-all.jar");
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-analyzer-jar.ps1)", jar.display());

        // 与 spawn_analyzer_client 完全一致的命令行，但 stderr 由本测试直接消费
        let mut child = tokio::process::Command::new(&java.path)
            .args(analyzer_jvm_args(&jar, 4))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("worker must spawn");
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // MCP initialize 请求触发工具注册与 MAT bootstrap（输出中文日志）
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"regress\",\"version\":\"0\"}}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        // 读 stderr 到进程结束（或读够内容）
        let mut stderr_bytes = Vec::new();
        let mut buf = [0u8; 4096];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if stderr_bytes.len() > 200 || std::time::Instant::now() > deadline {
                break;
            }
            tokio::select! {
                n = child.stderr.as_mut().unwrap().read(&mut buf) => {
                    match n {
                        Ok(0) | Err(_) => break,
                        Ok(n) => stderr_bytes.extend_from_slice(&buf[..n]),
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                    if stderr_bytes.len() > 50 { break; }
                }
            }
        }
        let _ = child.kill().await;
        let text = String::from_utf8(stderr_bytes.clone())
            .unwrap_or_else(|_| panic!("stderr must be valid UTF-8, bytes: {stderr_bytes:?}"));
        assert!(
            !text.contains('\u{FFFD}'),
            "stderr must not contain U+FFFD replacement chars, text: {text:?}"
        );
        // zh-CN 环境下 MAT bootstrap 输出「信息:」前缀；非中文环境无中文也无妨（编码断言已过）
        if text.contains('\u{4fe1}') && text.contains('\u{606f}') {
            // 包含「信息」且 strict UTF-8 解码成功——乱码修复确认
        }
    }
}
