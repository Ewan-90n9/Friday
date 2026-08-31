use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// 同时保持 attach 的 JVM 会话上限（LRU 逐出，对齐 heap analyzer 的 MAX_OPEN_DUMPS）
pub const MAX_SESSIONS: usize = 3;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("attach 失败：{0}")]
    Attach(String),
    #[error("该 JVM 尚未 attach arthas")]
    NotOpen { attaching: bool },
    #[error("arthas 调用超时（{0}s）")]
    Timeout(u64),
    #[error("{0}")]
    Upstream(String),
    #[error("arthas 通道传输错误：{0}")]
    Transport(String),
}

/// 一次上游工具调用结果
#[derive(Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

/// arthas MCP client 抽象（测试注入 mock 的 seam，对齐 HeapAnalyzerClient）
#[async_trait]
pub trait ArthasClient: Send + Sync {
    /// Err = 传输层错误（通道死亡，调用方 invalidate 会话）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    async fn shutdown(&self);
}

/// attach 资源释放句柄：HTTP stop arthas + 拆隧道（尽力而为）
#[async_trait]
pub trait ArthasStopHandle: Send + Sync {
    async fn stop(&self);
}

pub struct AttachedSession {
    pub client: Arc<dyn ArthasClient>,
    pub stop_handle: Arc<dyn ArthasStopHandle>,
}

#[derive(Clone, Debug)]
pub struct AttachRequest {
    pub session_id: String,
    pub env_id: String,
    pub pid: i64,
    /// 目标机 java 可执行文件路径或 java 命令名（arthas-boot 运行需要；默认 "java"）
    pub java_bin: String,
}

pub type AttachFactory = Arc<
    dyn Fn(AttachRequest) -> Pin<Box<dyn Future<Output = Result<AttachedSession, ManagerError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ArthasConfig {
    /// 距最后调用超过该时长且无 inflight → 自动 stop
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for ArthasConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}
