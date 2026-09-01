use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use super::manager::{ArthasClient, CallOutcome};

/// rmcp Streamable HTTP 实现：连接远端 arthas MCP Server（经 SSH 隧道到达 127.0.0.1:local_port）。
/// Bearer token 即 arthas.password（Friday 生成、随 arthas.properties 下发）。
pub struct McpArthasClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 构建直连 HTTP 客户端：MCP 流量走 SSH 隧道（127.0.0.1 本地端口），
/// 必须绕过一切系统/环境代理——企业代理截走 localhost 请求时回 504（issue #7）。
fn direct_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("构建 MCP http 客户端失败: {e}"))
}

/// 连接 + MCP 握手（30s 超时）
pub async fn connect_arthas_client(url: &str, token: &str) -> Result<McpArthasClient, String> {
    use rmcp::ServiceExt;

    let config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url)
        .auth_header(token);
    let transport = rmcp::transport::StreamableHttpClientTransport::with_client(
        direct_http_client()?,
        config,
    );

    let service = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .map_err(|_| format!("arthas MCP 握手超时（30s）: {url}"))?
        .map_err(|e| format!("arthas MCP 连接失败: {e}"))?;

    let peer = service.peer().clone();
    tracing::info!(url, "arthas mcp client connected");
    Ok(McpArthasClient {
        peer,
        service: tokio::sync::Mutex::new(Some(service)),
    })
}

#[async_trait]
impl ArthasClient for McpArthasClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        // rmcp 3.1.4：CallToolRequestParams 为 non_exhaustive，只能经 Default 构造
        // （对齐 analyzer client 的适配写法）
        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        } else {
            tracing::warn!(tool = %name, "non-object args passed to arthas client, treated as empty");
        }
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = Some(arguments);

        let result = self
            .peer
            .call_tool_once(params)
            .await
            .map_err(|e| format!("arthas MCP 调用失败: {e}"))?;
        // 一次性请求/响应：非 Complete 一律按传输层错误处理（调用方 invalidate 会话）
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            other => return Err(format!("arthas MCP 调用返回非最终结果: {other:?}")),
        };
        Ok(CallOutcome {
            text: crate::analyzer::client::extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn shutdown(&self) {
        if let Some(service) = self.service.lock().await.take() {
            match service.cancel().await {
                Ok(reason) => tracing::info!(reason = ?reason, "arthas mcp client shut down"),
                Err(e) => tracing::warn!(?e, "arthas mcp service cancel failed"),
            }
        }
    }
}
