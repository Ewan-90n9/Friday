use std::path::PathBuf;
use tokio::process::Child;

pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
}

pub async fn spawn_opencode(
    _prompt: String,
    _mcp_config_path: PathBuf,
) -> Result<AgentProcess, Box<dyn std::error::Error + Send + Sync>> {
    todo!()
}
