use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, ChildStdout, ChildStderr};
use tokio_util::sync::CancellationToken;

pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("无可用 agent，请先检测或手动添加")]
    NoActiveAgent,
    #[error("agent 二进制不存在：{path}")]
    BinaryMissing { path: String },
    #[error("启动 agent 失败：{0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("DB 查询失败：{0}")]
    Db(#[from] sqlx::Error),
}

pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    message: String,
    opencode_session_id: Option<String>,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str,) = row.ok_or(SpawnError::NoActiveAgent)?;
    let path = PathBuf::from(&path_str);

    if !path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let mut cmd = tokio::process::Command::new(&path);
    cmd.arg("run")
        .arg("--format")
        .arg("json")
        .arg("--auto")
        .arg("--thinking");

    if let Some(ref oc_id) = opencode_session_id {
        cmd.arg("-s").arg(oc_id);
    }

    cmd.arg(&message);

    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    let stdout = child.stdout.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stdout not piped"),
    ))?;

    let stderr = child.stderr.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stderr not piped"),
    ))?;

    Ok(AgentProcess { pid, child, stdout, stderr })
}

/// RunningAgent lives in stream.rs but we need CancellationToken here for the type.
/// Re-exported from stream module. This is a placeholder re-export.
pub type RunningAgentCancel = CancellationToken;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();
        let result = spawn_active(&pool, String::new(), None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_binary_missing_when_path_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().to_path_buf()).await.unwrap();

        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES ('test-id', 'opencode', 'OpenCode', '/nonexistent/path/opencode', NULL, 'manual', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = spawn_active(&pool, "test message".to_string(), None).await;
        assert!(matches!(result, Err(SpawnError::BinaryMissing { .. })));
    }
}
