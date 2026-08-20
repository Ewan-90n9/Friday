use std::path::PathBuf;
use tokio::process::Child;

pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
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
    _prompt: String,
    _mcp_config_path: PathBuf,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str, _provider) = row.ok_or(SpawnError::NoActiveAgent)?;
    let path = std::path::PathBuf::from(&path_str);

    if !path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let mut cmd = tokio::process::Command::new(&path);
    let child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    Ok(AgentProcess { pid, child })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().to_path_buf())
            .await
            .unwrap();
        let result = spawn_active(&pool, String::new(), std::path::PathBuf::new()).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }
}
