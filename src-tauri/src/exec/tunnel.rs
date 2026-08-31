use super::channel::ExecChannel;
use super::pool::{build_transport, fetch_environment};
use super::ssh::SshTransport;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("environment not found: {0}")]
    EnvironmentNotFound(String),
    #[error("ssh connection failed: {0}")]
    Connection(String),
    #[error("local listen failed: {0}")]
    Listen(String),
}

/// 一条已建立隧道的描述（值类型，调用方只读）
#[derive(Clone, Debug)]
pub struct TunnelLease {
    pub env_id: String,
    pub remote_host: String,
    pub remote_port: u16,
    pub local_port: u16,
}

struct TunnelEntry {
    local_port: u16,
    transport: Arc<SshTransport>,
    accept_task: tokio::task::JoinHandle<()>,
    refs: u32,
}

/// SSH 本地端口转发管理器（russh direct-tcpip）。
/// 按 (env_id, remote_host, remote_port) 复用隧道，引用计数，归零即拆除。
/// 隧道独享一条 SSH 连接（不与 exec 池混用 channel），
/// 避免 russh 多路复用下 exec 大输出阻塞转发数据通道。
pub struct TunnelManager {
    db: sqlx::SqlitePool,
    inner: Mutex<HashMap<String, TunnelEntry>>,
}

fn tunnel_key(env_id: &str, remote_host: &str, remote_port: u16) -> String {
    format!("{env_id}/{remote_host}/{remote_port}")
}

impl TunnelManager {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self { db, inner: Mutex::new(HashMap::new()) }
    }

    /// 打开（或复用）一条到目标机 remote_host:remote_port 的隧道。
    /// 本地监听 127.0.0.1 临时端口（OS 分配），返回 TunnelLease。
    #[tracing::instrument(skip(self))]
    pub async fn open(
        &self,
        env_id: &str,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<TunnelLease, TunnelError> {
        let key = tunnel_key(env_id, remote_host, remote_port);
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.get_mut(&key) {
            entry.refs += 1;
            return Ok(TunnelLease {
                env_id: env_id.to_string(),
                remote_host: remote_host.to_string(),
                remote_port,
                local_port: entry.local_port,
            });
        }

        let env = fetch_environment(&self.db, env_id)
            .await
            .map_err(|e| TunnelError::EnvironmentNotFound(e.to_string()))?;
        let transport = build_transport(env_id, &env)
            .map_err(|e| TunnelError::Connection(e.to_string()))?;
        transport
            .connect()
            .await
            .map_err(|e| TunnelError::Connection(e.to_string()))?;
        let transport = Arc::new(transport);

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| TunnelError::Listen(e.to_string()))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| TunnelError::Listen(e.to_string()))?
            .port();

        let accept_task = tokio::spawn(accept_loop(
            listener,
            transport.clone(),
            remote_host.to_string(),
            remote_port,
        ));
        inner.insert(key, TunnelEntry { local_port, transport, accept_task, refs: 1 });
        tracing::info!(env_id, remote_host, remote_port, local_port, "ssh tunnel opened");
        Ok(TunnelLease {
            env_id: env_id.to_string(),
            remote_host: remote_host.to_string(),
            remote_port,
            local_port,
        })
    }

    /// 引用计数减一；归零时拆除隧道（停 accept + 断开隧道专属 SSH 连接）。
    /// 已建立的转发连接随 SSH 连接断开而终止。幂等。
    pub async fn close(&self, env_id: &str, remote_host: &str, remote_port: u16) {
        let key = tunnel_key(env_id, remote_host, remote_port);
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get_mut(&key) else { return };
        entry.refs = entry.refs.saturating_sub(1);
        if entry.refs == 0 {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                let env_id_owned = env_id.to_string();
                tokio::spawn(async move {
                    transport.disconnect().await;
                    tracing::info!(env_id = %env_id_owned, remote_port, "ssh tunnel closed");
                });
            }
        }
    }

    /// 拆除某环境全部隧道（环境删除联动）。幂等。
    pub async fn close_all_for_env(&self, env_id: &str) {
        let mut inner = self.inner.lock().await;
        let prefix = format!("{env_id}/");
        let keys: Vec<String> =
            inner.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for key in keys {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                tokio::spawn(async move { transport.disconnect().await; });
            }
        }
    }

    pub async fn tunnel_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

async fn accept_loop(
    listener: TcpListener,
    transport: Arc<SshTransport>,
    remote_host: String,
    remote_port: u16,
) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else { break };
        let transport = transport.clone();
        let remote_host = remote_host.clone();
        tokio::spawn(async move {
            if let Err(e) = forward(stream, transport, &remote_host, remote_port).await {
                tracing::warn!(remote_host = %remote_host, remote_port, error = %e, "tunnel forward ended with error");
            }
        });
    }
}

/// 单条本地 TCP 连接的双向转发：local TCP ⇄ direct-tcpip channel
async fn forward(
    stream: tokio::net::TcpStream,
    transport: Arc<SshTransport>,
    remote_host: &str,
    remote_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let channel = transport.open_direct_tcpip(remote_host, remote_port).await?;
    // russh 0.45: Channel 自身不实现 AsyncRead/AsyncWrite，需转成 ChannelStream
    let channel = channel.into_stream();
    let (mut tcp_read, mut tcp_write) = tokio::io::split(stream);
    let (mut ch_read, mut ch_write) = tokio::io::split(channel);
    // 任一方向结束即结束（HTTP 客户端按需开新连接）
    tokio::select! {
        r = tokio::io::copy(&mut tcp_read, &mut ch_write) => { r?; }
        r = tokio::io::copy(&mut ch_read, &mut tcp_write) => { r?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    #[test]
    fn test_tunnel_key_format() {
        assert_eq!(tunnel_key("env-1", "127.0.0.1", 8563), "env-1/127.0.0.1/8563");
    }

    #[tokio::test]
    async fn test_open_unknown_environment_errors() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool);
        let r = mgr.open("no-such-env", "127.0.0.1", 8563).await;
        assert!(matches!(r, Err(TunnelError::EnvironmentNotFound(_))));
    }

    #[tokio::test]
    async fn test_close_nonexistent_is_noop() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool);
        mgr.close("env-1", "127.0.0.1", 8563).await;
        assert_eq!(mgr.tunnel_count().await, 0);
    }
}
