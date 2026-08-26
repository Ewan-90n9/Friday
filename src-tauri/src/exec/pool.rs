use super::channel::ExecChannel;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("session {session_id} has no associated environment")]
    NoEnvironment { session_id: String },
    #[error("environment {env_id} not found")]
    EnvironmentNotFound { env_id: String },
    #[error("transport not yet implemented: {0}")]
    TransportNotImplemented(String),
}

pub struct ExecChannelPool {
    connections: HashMap<String, Arc<dyn ExecChannel>>,
}

impl ExecChannelPool {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    pub async fn get_or_create(
        &mut self,
        session_id: &str,
        pool: &sqlx::SqlitePool,
    ) -> Result<Arc<dyn ExecChannel>, PoolError> {
        if let Some(channel) = self.connections.get(session_id) {
            return Ok(channel.clone());
        }

        let (env_id, env) = fetch_environment(pool, session_id).await?;

        let channel: Arc<dyn ExecChannel> = match env.transport_type.as_str() {
            "ssh" => {
                let auth = super::ssh::SshAuth::from_row(
                    env.auth_type.as_deref().unwrap_or("private_key"),
                    env.private_key_path.as_deref(),
                )
                .ok_or_else(|| PoolError::TransportNotImplemented(format!(
                    "invalid auth config for environment {env_id}"
                )))?;
                Arc::new(super::ssh::SshTransport::new(
                    &env_id,
                    env.host.as_deref().unwrap_or_default(),
                    env.port.unwrap_or(22),
                    env.user.as_deref().unwrap_or_default(),
                    auth,
                ))
            }
            "k8s" => Arc::new(super::k8s::K8sTransport {
                namespace: env.k8s_namespace.unwrap_or_default(),
                pod: env.k8s_pod.unwrap_or_default(),
                container: String::new(),
            }),
            other => return Err(PoolError::TransportNotImplemented(other.to_string())),
        };

        channel
            .connect()
            .await
            .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

        self.connections.insert(session_id.to_string(), channel.clone());
        Ok(channel)
    }

    pub async fn disconnect(&mut self, session_id: &str) {
        if let Some(channel) = self.connections.remove(session_id) {
            channel.disconnect().await;
        }
    }

    pub async fn disconnect_all(&mut self) {
        let channels: Vec<_> = self.connections.drain().collect();
        for (_, channel) in channels {
            channel.disconnect().await;
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

impl Default for ExecChannelPool {
    fn default() -> Self {
        Self::new()
    }
}

struct EnvironmentInfo {
    transport_type: String,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    k8s_namespace: Option<String>,
    k8s_pod: Option<String>,
    auth_type: Option<String>,
    private_key_path: Option<String>,
}

async fn fetch_environment(
    pool: &sqlx::SqlitePool,
    session_id: &str,
) -> Result<(String, EnvironmentInfo), PoolError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT environment_id FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

    let env_id = row
        .and_then(|(id,)| id)
        .ok_or(PoolError::NoEnvironment {
            session_id: session_id.to_string(),
        })?;

    let env_row: Option<(Option<String>, Option<i64>, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT host, port, user, transport_type, k8s_namespace, k8s_pod, auth_type, private_key_path \
             FROM environments WHERE id = ?",
        )
        .bind(&env_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PoolError::TransportNotImplemented(e.to_string()))?;

    let env_row = env_row.ok_or_else(|| PoolError::EnvironmentNotFound {
        env_id: env_id.clone(),
    })?;

    Ok((
        env_id,
        EnvironmentInfo {
            transport_type: env_row.3,
            host: env_row.0,
            port: env_row.1.map(|p| p as u16),
            user: env_row.2,
            k8s_namespace: env_row.4,
            k8s_pod: env_row.5,
            auth_type: env_row.6,
            private_key_path: env_row.7,
        },
    ))
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
    }

    #[tokio::test]
    async fn test_disconnect_removes_connection() {
        let mut pool = ExecChannelPool::new();
        pool.connections.insert("s1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);

        pool.disconnect("s1").await;
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
        pool.connections.insert("s1".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);
        pool.connections.insert("s2".to_string(), Arc::new(MockChannel) as Arc<dyn ExecChannel>);

        pool.disconnect_all().await;
        assert_eq!(pool.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_get_or_create_no_environment_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let db_pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = crate::app::session::create_session(&db_pool, "test").await.unwrap();

        let mut pool = ExecChannelPool::new();
        let result = pool.get_or_create(&session.id.0, &db_pool).await;

        assert!(matches!(result, Err(PoolError::NoEnvironment { .. })));
    }
}
