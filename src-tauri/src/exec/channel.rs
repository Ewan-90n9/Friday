use async_trait::async_trait;

pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[async_trait]
pub trait ExecChannel: Send + Sync {
    async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>>;
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn disconnect(&self);
    /// 连接池巡检用：连接是否仍然存活
    async fn is_alive(&self) -> bool;
}
