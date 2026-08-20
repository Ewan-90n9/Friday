use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;

pub struct SshTransport {
    pub host: String,
    pub port: u16,
    pub user: String,
}

#[async_trait]
impl ExecChannel for SshTransport {
    async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        todo!()
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        todo!()
    }

    async fn disconnect(&self) {
        todo!()
    }
}
