use super::channel::ExecChannel;
use super::pool::{build_transport, fetch_environment, PoolError};
use super::ssh::SshTransport;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("environment not found: {0}")]
    EnvironmentNotFound(String),
    #[error("database error: {0}")]
    Db(String),
    #[error("ssh connection failed: {0}")]
    Connection(String),
    #[error("invalid transport config: {0}")]
    Config(String),
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

/// TunnelManager 与 accept_loop 转发任务共享的状态内核。
/// 独立成 struct：accept_loop 的转发任务需要在转发失败时调用 mark_failed
/// 自愈死连接，但 TunnelManager 无法把自身 Arc 交给它 spawn 的 accept_loop
/// （构造期自引用问题），故把可变状态抽到 Arc<TunnelShared> 里两边共用。
struct TunnelShared {
    db: sqlx::SqlitePool,
    inner: Mutex<HashMap<String, TunnelEntry>>,
}

/// SSH 本地端口转发管理器（russh direct-tcpip）。
/// 按 (env_id, remote_host, remote_port) 复用隧道，引用计数，归零即拆除。
/// 隧道独享一条 SSH 连接（不与 exec 池混用 channel），
/// 避免 russh 多路复用下 exec 大输出阻塞转发数据通道。
pub struct TunnelManager {
    shared: Arc<TunnelShared>,
}

fn tunnel_key(env_id: &str, remote_host: &str, remote_port: u16) -> String {
    format!("{env_id}/{remote_host}/{remote_port}")
}

impl TunnelManager {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self {
            shared: Arc::new(TunnelShared { db, inner: Mutex::new(HashMap::new()) }),
        }
    }

    /// 打开（或复用）一条到目标机 remote_host:remote_port 的隧道。
    /// 本地监听 127.0.0.1 临时端口（OS 分配），返回 TunnelLease。
    ///
    /// 锁纪律：connect 可能阻塞数十秒（黑洞主机 3 次重试 + 退避），
    /// 绝不能持锁 await——否则所有 open/close/close_all_for_env 全局串行。
    /// 因此慢路径走「锁内查无 → 释放锁建连 → 重取锁双检」：
    /// 并发同 key open 只有一个赢家插入，输家拆除自己的重复建连并复用赢家的。
    #[tracing::instrument(skip(self))]
    pub async fn open(
        &self,
        env_id: &str,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<TunnelLease, TunnelError> {
        let key = tunnel_key(env_id, remote_host, remote_port);
        // 快路径：命中即复用（锁内只有内存操作）
        {
            let mut inner = self.shared.inner.lock().await;
            if let Some(entry) = inner.get_mut(&key) {
                entry.refs += 1;
                return Ok(TunnelLease {
                    env_id: env_id.to_string(),
                    remote_host: remote_host.to_string(),
                    remote_port,
                    local_port: entry.local_port,
                });
            }
        } // 锁在此释放，慢路径建连期间不阻塞 close 等操作

        let env = fetch_environment(&self.shared.db, env_id).await.map_err(|e| match e {
            PoolError::EnvironmentNotFound { .. } => {
                TunnelError::EnvironmentNotFound(e.to_string())
            }
            other => TunnelError::Db(other.to_string()),
        })?;
        let transport =
            build_transport(env_id, &env).map_err(|e| TunnelError::Config(e.to_string()))?;
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

        // 双检：建连期间并发 open 可能已插入同 key 隧道 → 复用它并拆除自己的重复建连。
        // 此时 accept_loop 尚未 spawn，无 task 需清理，drop listener 即停止监听；
        // disconnect 可能阻塞（transport 内部锁），必须后台执行，不能持锁 await。
        let mut inner = self.shared.inner.lock().await;
        if let Some(entry) = inner.get_mut(&key) {
            entry.refs += 1;
            let reused_port = entry.local_port;
            drop(listener);
            let duplicate = transport.clone();
            let env_id_owned = env_id.to_string();
            let remote_host_owned = remote_host.to_string();
            tokio::spawn(async move {
                duplicate.disconnect().await;
                tracing::info!(
                    env_id = %env_id_owned,
                    remote_host = %remote_host_owned,
                    remote_port,
                    "discarded duplicate tunnel connection (concurrent open raced)"
                );
            });
            return Ok(TunnelLease {
                env_id: env_id.to_string(),
                remote_host: remote_host.to_string(),
                remote_port,
                local_port: reused_port,
            });
        }
        let accept_task = tokio::spawn(accept_loop(
            listener,
            self.shared.clone(),
            transport.clone(),
            env_id.to_string(),
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
        let mut inner = self.shared.inner.lock().await;
        let Some(entry) = inner.get_mut(&key) else { return };
        entry.refs = entry.refs.saturating_sub(1);
        if entry.refs == 0 {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                let env_id_owned = env_id.to_string();
                let remote_host_owned = remote_host.to_string();
                let local_port = entry.local_port;
                tokio::spawn(async move {
                    transport.disconnect().await;
                    tracing::info!(
                        env_id = %env_id_owned,
                        remote_host = %remote_host_owned,
                        remote_port,
                        local_port,
                        "ssh tunnel closed"
                    );
                });
            }
        }
    }

    /// 拆除某环境全部隧道（环境删除联动）。幂等。
    pub async fn close_all_for_env(&self, env_id: &str) {
        let mut inner = self.shared.inner.lock().await;
        let prefix = format!("{env_id}/");
        let keys: Vec<String> =
            inner.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        let mut removed = 0usize;
        for key in keys {
            if let Some(entry) = inner.remove(&key) {
                entry.accept_task.abort();
                let transport = entry.transport.clone();
                tokio::spawn(async move { transport.disconnect().await; });
                removed += 1;
            }
        }
        tracing::info!(env_id, count = removed, "closed all ssh tunnels for environment");
    }

    pub async fn tunnel_count(&self) -> usize {
        self.shared.inner.lock().await.len()
    }

    /// 标记隧道失败：转发遇到传输/通道级错误（连接可能已死——典型场景：
    /// russh inactivity_timeout 600s 静默断开隧道专属连接，accept_loop 仍在
    /// 收新连接但每个 open_direct_tcpip 都失败，形成永不清理的僵尸隧道）。
    /// 移除条目使下一次 open() 重新建连。幂等；条目不存在或已被并发重建
    /// （transport 不是失败的同一条）时不动，避免误杀新隧道。
    /// 注意：引用计数随条目一并丢弃——遗留 lease 之后的 close() 会作用于
    /// 不存在的 key（no-op）或新建条目（引用计数偏少一次），重建代价可接受。
    pub(crate) async fn mark_failed(
        &self,
        env_id: &str,
        remote_host: &str,
        remote_port: u16,
        dead: &Arc<SshTransport>,
    ) {
        self.shared.mark_failed(env_id, remote_host, remote_port, dead).await;
    }
}

impl TunnelShared {
    /// mark_failed 的实际实现（见 TunnelManager::mark_failed 文档）。
    async fn mark_failed(
        &self,
        env_id: &str,
        remote_host: &str,
        remote_port: u16,
        dead: &Arc<SshTransport>,
    ) {
        let key = tunnel_key(env_id, remote_host, remote_port);
        let mut inner = self.inner.lock().await;
        let is_dead_entry = match inner.get(&key) {
            Some(entry) => Arc::ptr_eq(&entry.transport, dead),
            None => false,
        };
        if !is_dead_entry {
            return;
        }
        if let Some(entry) = inner.remove(&key) {
            entry.accept_task.abort();
            let transport = entry.transport.clone();
            let env_id_owned = env_id.to_string();
            let remote_host_owned = remote_host.to_string();
            let local_port = entry.local_port;
            tokio::spawn(async move {
                transport.disconnect().await;
                tracing::warn!(
                    env_id = %env_id_owned,
                    remote_host = %remote_host_owned,
                    remote_port,
                    local_port,
                    "ssh tunnel marked failed and torn down (next open will reconnect)"
                );
            });
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<TunnelShared>,
    transport: Arc<SshTransport>,
    env_id: String,
    remote_host: String,
    remote_port: u16,
) {
    loop {
        // accept 错误（EMFILE/ENOMEM 等瞬时资源耗尽）只告警 + 100ms 退避后重试：
        // 直接 break 会让隧道静默死亡。正常终止路径是 close/mark_failed abort
        // 本任务（abort 在 await 点生效，listener 随任务结束 Drop 而关闭）。
        let (stream, _peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::warn!(
                    env_id = %env_id,
                    remote_host = %remote_host,
                    remote_port,
                    error = %e,
                    "tunnel accept error; retrying in 100ms"
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let shared = shared.clone();
        let transport = transport.clone();
        let remote_host = remote_host.clone();
        let env_id = env_id.clone();
        tokio::spawn(async move {
            // forward 返回 Err = 传输/通道级故障（连接死、channel 打开被拒等），
            // 正常 EOF 收尾是 Ok。此处标记失败让下一次 open() 重建隧道。
            // 代价权衡：远端拒绝（channel open rejected）或本地客户端异常 RST
            // 也会走到这里而拆除可能健康的隧道——但重建幂等且便宜，远好于
            // 保留 russh inactivity_timeout 死连接形成的僵尸隧道。
            if let Err(e) = forward(stream, transport.clone(), &remote_host, remote_port).await {
                tracing::warn!(
                    env_id = %env_id,
                    remote_host = %remote_host,
                    remote_port,
                    error = %e,
                    "tunnel forward failed; marking tunnel failed for reconnect"
                );
                shared.mark_failed(&env_id, &remote_host, remote_port, &transport).await;
            }
        });
    }
}

/// 单条本地 TCP 连接的双向转发：local TCP ⇄ direct-tcpip channel。
/// copy_bidirectional 正确处理半关闭：单方向 EOF 只关闭对端写方向，
/// 另一方向继续搬运直到双向都结束（HTTP 客户端按需开新连接，正常
/// 收尾返回 Ok）。任一方向 I/O 错误返回 Err（交由调用方 mark_failed）。
async fn forward(
    mut stream: tokio::net::TcpStream,
    transport: Arc<SshTransport>,
    remote_host: &str,
    remote_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let channel = transport.open_direct_tcpip(remote_host, remote_port).await?;
    // russh 0.45: Channel 自身不实现 AsyncRead/AsyncWrite，需转成 ChannelStream
    let mut channel = channel.into_stream();
    tokio::io::copy_bidirectional(&mut stream, &mut channel).await?;
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

    #[tokio::test]
    async fn test_open_connection_failure_leaves_no_entry() {
        let (_tmp, pool) = setup().await;
        // 环境存在但认证必然失败：password 认证 + keychain 无密码 → connect 报错（快失败）
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES ('env-x', 'x', '127.0.0.1', 1, 'u', 'ssh', 'password', '2026-01-01T00:00:00Z')",
        ).execute(&pool).await.unwrap();
        let mgr = TunnelManager::new(pool);
        let r = mgr.open("env-x", "127.0.0.1", 8563).await;
        assert!(r.is_err());
        assert_eq!(mgr.tunnel_count().await, 0, "failed open must not leave a map entry");
    }

    #[tokio::test]
    async fn test_open_unsupported_transport_maps_to_config_error() {
        let (_tmp, pool) = setup().await;
        // 非 ssh transport_type：build_transport 报 TransportNotImplemented → Config
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES ('env-d', 'd', '10.0.0.1', 22, 'root', 'local', 'password', '2026-01-01T00:00:00Z')",
        ).execute(&pool).await.unwrap();
        let mgr = TunnelManager::new(pool);
        let r = mgr.open("env-d", "127.0.0.1", 8563).await;
        assert!(matches!(r, Err(TunnelError::Config(_))), "expected Config error, got: {r:?}");
    }

    #[tokio::test]
    async fn test_open_db_error_maps_to_db_error() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool.clone());
        pool.close().await; // 后续查询必失败 → fetch_environment 的 DB 错误 → Db
        let r = mgr.open("any-env", "127.0.0.1", 8563).await;
        assert!(matches!(r, Err(TunnelError::Db(_))), "expected Db error, got: {r:?}");
    }

    #[tokio::test]
    async fn test_mark_failed_nonexistent_key_is_noop() {
        let (_tmp, pool) = setup().await;
        let mgr = TunnelManager::new(pool);
        let dead = Arc::new(SshTransport::new(
            "env-1",
            "10.0.0.1",
            22,
            "root",
            crate::exec::ssh::SshAuth::Password,
        ));
        // key 不存在：幂等 no-op，不 panic、计数不变
        mgr.mark_failed("env-1", "127.0.0.1", 8563, &dead).await;
        assert_eq!(mgr.tunnel_count().await, 0);
    }
}
