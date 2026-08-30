// Task 5（manager 行为）接入前暂无构造方，避免 dead_code 告警
#[allow(dead_code)]
#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("{0}")]
    JavaMissing(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("分析调用超时（{0}s），工人进程保留未受影响")]
    Timeout(u64),
    #[error("该 dump 尚未打开")]
    NotOpen { warming: bool },
    #[error("{0}")]
    Upstream(String),
}
