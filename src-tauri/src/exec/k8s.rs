use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;

pub struct K8sTransport {
    pub namespace: String,
    pub pod: String,
    pub container: String,
}

#[async_trait]
impl ExecChannel for K8sTransport {
    async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        Err("K8s transport not yet implemented".into())
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("K8s transport not yet implemented".into())
    }

    async fn disconnect(&self) {}
}
