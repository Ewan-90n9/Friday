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
                conn.channel.disconnect().await;
            }
        }
        stale.len()
    }

    pub async fn disconnect(&mut self, environment_id: &str) {
        if let Some(conn) = self.connections.remove(environment_id) {
            conn.channel.disconnect().await;
        }
    }

    pub async fn disconnect_all(&mut self) {
        let conns: Vec<_> = self.connections.drain().collect();
        for (_, conn) in conns {
            conn.channel.disconnect().await;
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
}

fn build_transport(
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
            Ok(super::ssh::SshTransport::new(
                environment_id,
                env.host.as_deref().unwrap_or_default(),
                env.port.unwrap_or(22),
                env.user.as_deref().unwrap_or_default(),
                auth,
            ))
        }
        other => Err(PoolError::TransportNotImplemented(other.to_string())),
    }
}

async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<EnvironmentInfo, PoolError> {
    let row: Option<(Option<String>, Option<i64>, Option<String>, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT host, port, user, transport_type, auth_type, private_key_path \
             FROM environments WHERE id = ?",
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
        user: row.2,
        auth_type: row.4,
        private_key_path: row.5,
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
}
