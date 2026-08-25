use super::server::FridayMcpServer;
use crate::app::events::EventBus;
use crate::exec::pool::ExecChannelPool;
use crate::mcp::session_mapper::SessionMapper;
use crate::tools::confirm::ConfirmRegistry;
use crate::tools::registry::ToolRegistry;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct McpServerHandle {
    pub port: u16,
    pub cancel_token: CancellationToken,
    pub join_handle: tokio::task::JoinHandle<()>,
}

pub async fn start_mcp_server(
    tool_registry: Arc<ToolRegistry>,
    exec_pool: Arc<Mutex<ExecChannelPool>>,
    confirm_registry: Arc<Mutex<ConfirmRegistry>>,
    session_mapper: Arc<Mutex<SessionMapper>>,
    bus: EventBus,
    pool: sqlx::SqlitePool,
) -> Result<McpServerHandle, Box<dyn std::error::Error + Send + Sync>> {
    let cancel_token = CancellationToken::new();
    let server_cancel = cancel_token.clone();
    let loop_cancel = cancel_token.clone();

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    tracing::info!(port, "MCP server binding to 127.0.0.1");

    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(std::time::Duration::from_secs(30)))
        .with_cancellation_token(server_cancel);

    let session_manager = Arc::new(LocalSessionManager::default());

    let service_factory = move || {
        Ok::<_, std::io::Error>(FridayMcpServer {
            tool_registry: tool_registry.clone(),
            exec_pool: exec_pool.clone(),
            confirm_registry: confirm_registry.clone(),
            session_mapper: session_mapper.clone(),
            bus: bus.clone(),
            pool: pool.clone(),
        })
    };

    let service = StreamableHttpService::new(service_factory, session_manager, config);

    listener.set_nonblocking(true)?;
    let tokio_listener = tokio::net::TcpListener::from_std(listener)?;

    let join_handle = tokio::spawn(async move {
        let service = Arc::new(service);

        loop {
            tokio::select! {
                accept_result = tokio_listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            let service = service.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, addr, service).await {
                                    tracing::error!(?e, %addr, "connection error");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!(?e, "accept error");
                        }
                    }
                }
                _ = loop_cancel.cancelled() => {
                    tracing::info!("MCP server listener shutting down");
                    break;
                }
            }
        }
    });

    Ok(McpServerHandle {
        port,
        cancel_token,
        join_handle,
    })
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
    service: Arc<StreamableHttpService<FridayMcpServer, LocalSessionManager>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(%addr, "MCP connection opened");
    let io = hyper_util::rt::TokioIo::new(stream);

    let service_clone = service.clone();
    let svc = hyper::service::service_fn(move |req| {
        let service = service_clone.clone();
        async move {
            let method = req.method().as_str().to_owned();
            let path = req.uri().path().to_owned();
            let user_agent = req
                .headers()
                .get(http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_owned();

            let response = service.handle(req).await;

            tracing::info!(
                method = %method,
                path = %path,
                user_agent = %user_agent,
                status = %response.status().as_u16(),
                "MCP HTTP request"
            );

            Ok::<_, std::convert::Infallible>(response)
        }
    });

    match hyper::server::conn::http1::Builder::new()
        .serve_connection(io, svc)
        .await
    {
        Ok(()) => {
            tracing::info!(%addr, "MCP connection closed");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(%addr, ?e, "MCP connection ended with error");
            Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        }
    }
}
