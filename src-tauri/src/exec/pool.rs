use super::channel::ExecChannel;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("environment {env_id} not found")]
    EnvironmentNotFound { env_id: String },
    #[error("connection error: {0}")]
    Connection(String),
    #[error("transport not implemented: {0}")]
    TransportNotImplemented(String),
}

struct PooledConnection {
    channel: Arc<dyn ExecChannel>,
    last_used: Instant,
}

/// 后台执行 disconnect（fire-and-forget）。
/// disconnect 可能等待 transport 内部连接锁（长命令持有时无界阻塞），
/// 绝不能在持有 pool 锁的路径上 await。TCP 连接短暂残留可接受（russh Drop 也会清理）。
fn spawn_disconnect(env_id: String, channel: Arc<dyn ExecChannel>) {
    tokio::spawn(async move {
        tracing::debug!(env_id = %env_id, "disconnecting ssh connection in background");
        channel.disconnect().await;
        tracing::debug!(env_id = %env_id, "ssh connection disconnected");
    });
}

pub struct ExecChannelPool {
    connections: HashMap<String, PooledConnection>,
}

impl ExecChannelPool {
    pub fn new() -> Self {
        Self { connections: HashMap::new() }
    }

    /// 按环境获取或建连。缓存命中即复用（刷新 last_used）。
    pub async fn get_or_create(
        &mut self,
        environment_id: &str,
        pool: &sqlx::SqlitePool,
    ) -> Result<Arc<dyn ExecChannel>, PoolError> {
        if let Some(conn) = self.connections.get_mut(environment_id) {
            conn.last_used = Instant::now();
            return Ok(conn.channel.clone());
        }

        let env = fetch_environment(pool, environment_id).await?;
        let transport = build_transport(environment_id, &env)?;

        transport
            .connect()
            .await
            .map_err(|e| PoolError::Connection(e.to_string()))?;

        let channel: Arc<dyn ExecChannel> = Arc::from(transport);
        self.connections.insert(
            environment_id.to_string(),
            PooledConnection { channel: channel.clone(), last_used: Instant::now() },
        );
        Ok(channel)
    }

    /// 测试与内部注入用：直接放入一条已建好的 channel
    pub async fn insert_channel(&mut self, environment_id: String, channel: Arc<dyn ExecChannel>) {
        self.connections.insert(environment_id, PooledConnection { channel, last_used: Instant::now() });
    }

    /// 清理空闲超时连接。返回清理数量。
    /// disconnect 在后台 task 中执行（fire-and-forget）：transport 内部锁可能被长命令持有，
    /// 若在持有 pool 锁时 await disconnect，会阻塞所有环境的连接获取。
    pub async fn cleanup_idle(&mut self, idle_timeout: Duration) -> usize {
        let stale: Vec<String> = self
            .connections
            .iter()
            .filter(|(_, c)| c.last_used.elapsed() > idle_timeout)
            .map(|(k, _)| k.clone())
            .collect();
        for env_id in &stale {
            if let Some(conn) = self.connections.remove(env_id) {
                tracing::info!(env_id = %env_id, idle_secs = conn.last_used.elapsed().as_secs(), "closing idle ssh connection");
                spawn_disconnect(env_id.clone(), conn.channel);
            }
        }
        stale.len()
    }

    pub async fn disconnect(&mut self, environment_id: &str) {
        if let Some(conn) = self.connections.remove(environment_id) {
            spawn_disconnect(environment_id.to_string(), conn.channel);
        }
    }

    pub async fn disconnect_all(&mut self) {
        for (env_id, conn) in self.connections.drain() {
            spawn_disconnect(env_id, conn.channel);
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    #[cfg(test)]
    pub fn mark_last_used_for_test(&mut self, environment_id: &str, at: Instant) {
        if let Some(conn) = self.connections.get_mut(environment_id) {
            conn.last_used = at;
        }
    }

    #[cfg(test)]
    pub async fn get_or_create_unchecked_for_test(&mut self, environment_id: &str) -> Arc<dyn ExecChannel> {
        self.connections.get(environment_id).map(|c| c.channel.clone()).unwrap()
    }
}

impl Default for ExecChannelPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EnvironmentInfo {
    pub transport_type: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub auth_type: Option<String>,
    pub private_key_path: Option<String>,
    /// 默认凭证 id（env_credentials.id）；无凭证行时 None（退回 environments 列）
    pub default_cred_id: Option<String>,
}

pub fn build_transport(
    environment_id: &str,
    env: &EnvironmentInfo,
) -> Result<super::ssh::SshTransport, PoolError> {
    match env.transport_type.as_str() {
        "ssh" => {
            let auth = super::ssh::SshAuth::from_row(
                env.auth_type.as_deref().unwrap_or("private_key"),
                env.private_key_path.as_deref(),
            )
            .ok_or_else(|| PoolError::TransportNotImplemented(format!(
                "invalid auth config for environment {environment_id}"
            )))?;
            let transport = match &env.default_cred_id {
                Some(cred_id) => super::ssh::SshTransport::with_cred(
                    environment_id,
                    env.host.as_deref().unwrap_or_default(),
                    env.port.unwrap_or(22),
                    env.user.as_deref().unwrap_or_default(),
                    auth,
                    cred_id,
                ),
                None => super::ssh::SshTransport::new(
                    environment_id,
                    env.host.as_deref().unwrap_or_default(),
                    env.port.unwrap_or(22),
                    env.user.as_deref().unwrap_or_default(),
                    auth,
                ),
            };
            Ok(transport)
        }
        other => Err(PoolError::TransportNotImplemented(other.to_string())),
    }
}

pub async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
    let row: Option<(
        Option<String>,
        Option<i64>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT e.host, e.port, e.user, e.transport_type, \
                COALESCE(c.auth_type, e.auth_type), \
                COALESCE(c.private_key_path, e.private_key_path), \
                COALESCE(c.username, e.user), \
                c.id \
         FROM environments e \
         LEFT JOIN env_credentials c ON c.environment_id = e.id AND c.is_default = 1 \
         WHERE e.id = ?",
    )
    .bind(environment_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PoolError::Connection(e.to_string()))?;

    let row = row.ok_or(PoolError::EnvironmentNotFound {
        env_id: environment_id.to_string(),
    })?;

    Ok(EnvironmentInfo {
        transport_type: row.3,
        host: row.0,
        port: row.1.map(|p| p as u16),
        user: row.6,
        auth_type: row.4,
        private_key_path: row.5,
        default_cred_id: row.7,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use async_trait::async_trait;

    struct MockChannel;

    #[async_trait]
    impl ExecChannel for MockChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    async fn insert_test_environment(pool: &sqlx::SqlitePool, id: &str, name: &str) {
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES (?, ?, '10.0.0.1', 22, 'root', 'ssh', 'password', '2026-01-01T00:00:00Z')",
        )
        .bind(id).bind(name).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn test_disconnect_removes_connection() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;

        pool.disconnect("env-1").await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_disconnect_nonexistent_is_noop() {
        let mut pool = ExecChannelPool::new();
        pool.disconnect("nonexistent").await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_disconnect_all_removes_all() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        pool.insert_channel("env-2".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;

        pool.disconnect_all().await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_channel_trait_exposes_is_alive() {
        let ch: Arc<dyn ExecChannel> = Arc::new(MockChannel);
        assert!(ch.is_alive().await);
    }

    #[tokio::test]
    async fn test_get_or_create_caches_by_environment_id() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;

        let mut pool = ExecChannelPool::new();
        // 第一次：缓存未命中 → 注入 channel 后复用
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        let ch = pool.get_or_create("env-1", &db_pool).await.unwrap();
        assert!(ch.run("echo").await.is_ok());
        // 第二次：命中同一缓存（同一 Arc）
        let ch2 = pool.get_or_create("env-1", &db_pool).await.unwrap();
        assert_eq!(pool.connection_count(), 1);
        assert!(std::sync::Arc::ptr_eq(&ch, &ch2));
    }

    #[tokio::test]
    async fn test_get_or_create_unknown_environment_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();

        let mut pool = ExecChannelPool::new();
        let result = pool.get_or_create("no-such-env", &db_pool).await;
        assert!(matches!(result, Err(PoolError::EnvironmentNotFound { .. })));
    }

    struct SlowDisconnectChannel;

    #[async_trait]
    impl ExecChannel for SlowDisconnectChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        async fn is_alive(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_cleanup_idle_returns_promptly_when_disconnect_blocks() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-slow".to_string(), Arc::new(SlowDisconnectChannel) as Arc<dyn ExecChannel>).await;
        pool.mark_last_used_for_test("env-slow", std::time::Instant::now() - std::time::Duration::from_secs(660));

        let start = std::time::Instant::now();
        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
        assert_eq!(removed, 1);
        // cleanup must return well before the 3s disconnect completes
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "cleanup_idle blocked on disconnect: {:?}", start.elapsed());
        // entry must be gone from the pool immediately
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_idle_cleanup_removes_stale_connections() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;
        assert_eq!(pool.connection_count(), 1);

        pool.mark_last_used_for_test("env-1", std::time::Instant::now() - std::time::Duration::from_secs(660));
        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
        assert_eq!(removed, 1);
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_idle_cleanup_keeps_recent_connections() {
        let mut pool = ExecChannelPool::new();
        pool.insert_channel("env-1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>).await;

        let removed = pool.cleanup_idle(std::time::Duration::from_secs(600)).await;
        assert_eq!(removed, 0);
        assert_eq!(pool.connection_count(), 1);
    }

    #[tokio::test]
    async fn test_fetch_environment_prefers_default_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;
        // 环境行：user=root/password；凭证行：svcapp/private_key
        sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES ('c1', 'env-1', 'svcapp', 'private_key', '~/.ssh/svc', 1, '2026-01-01T00:00:00Z')",
        )
        .execute(&db_pool).await.unwrap();

        let info = fetch_environment(&db_pool, "env-1").await.unwrap();
        assert_eq!(info.user.as_deref(), Some("svcapp"));
        assert_eq!(info.auth_type.as_deref(), Some("private_key"));
        assert_eq!(info.private_key_path.as_deref(), Some("~/.ssh/svc"));
        assert_eq!(info.default_cred_id.as_deref(), Some("c1"));

        let transport = build_transport("env-1", &info).unwrap();
        assert_eq!(transport.user, "svcapp");
        assert_eq!(transport.cred_id_as_ref(), Some("c1"));
    }

    #[tokio::test]
    async fn test_fetch_environment_falls_back_to_env_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        insert_test_environment(&db_pool, "env-1", "prod").await;

        let info = fetch_environment(&db_pool, "env-1").await.unwrap();
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.auth_type.as_deref(), Some("password"));
        assert!(info.default_cred_id.is_none());
    }
}
