use crate::exec::channel::ExecChannel;
use crate::app::events::EventBus;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// 阶段超时配置（秒）
pub struct StageTimeouts {
    pub probe: u64,
    pub download: u64,
    pub extract: u64,
    pub verify: u64,
}

impl Default for StageTimeouts {
    fn default() -> Self {
        Self { probe: 15, download: 600, extract: 120, verify: 15 }
    }
}

/// 装备上下文：由 MCP 工具 handler 构造传入
pub struct ProvisionContext {
    pub session_id: String,
    pub env_id: String,
    pub channel: Arc<dyn ExecChannel>,
    pub cache_dir: std::path::PathBuf,
    pub artifactory_base_url: String,
    /// vendored arthas zip（随应用分发）；None = 未随包分发，arthas ensure 时报结构化错误
    pub arthas_zip: Option<std::path::PathBuf>,
    pub timeouts: StageTimeouts,
    pub bus: EventBus,
}

/// 装备结果
#[derive(Clone, Debug, Serialize)]
pub struct ProvisionResult {
    pub tool: String,
    pub cached: bool,
    pub java_version: String,
    pub bisheng_version: String,
    pub arch: String,
    pub tool_home: String,
    pub bins: HashMap<String, String>,
    pub elapsed_ms: u64,
}

/// 装备错误：code 用于结构化返回，stage 标记失败阶段
#[derive(Debug)]
pub struct ProvisionError {
    pub code: String,
    pub stage: String,
    pub message: String,
    pub url: Option<String>,
}

impl ProvisionError {
    pub fn new(code: &str, stage: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_string(), stage: stage.to_string(), message: message.into(), url: None }
    }
}

/// 进度事件：阶段级
pub fn emit_progress(ctx: &ProvisionContext, tool: &str, stage: &str, detail: &str) {
    tracing::info!(session_id = %ctx.session_id, env_id = %ctx.env_id, tool, stage, detail, "provision progress");
    ctx.bus.emit(
        &ctx.session_id,
        crate::app::events::AppEvent::ProvisionProgress {
            session_id: ctx.session_id.clone(),
            tool: tool.to_string(),
            stage: stage.to_string(),
            detail: detail.to_string(),
        },
    );
}

/// 远程工具包：探测 + 装备。JDK 是第一个实现，arthas 等后续复用。
#[async_trait]
pub trait ToolPackage: Send + Sync {
    fn name(&self) -> &str;

    /// 探测目标环境 JVM 信息
    async fn probe(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<crate::provision::jdk::JvmProbe, ProvisionError>;

    /// 确保 package 已装备（幂等：已装备直接返回 cached=true）
    async fn ensure(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<ProvisionResult, ProvisionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_timeouts_defaults() {
        let t = StageTimeouts::default();
        assert_eq!(t.probe, 15);
        assert_eq!(t.download, 600);
        assert_eq!(t.extract, 120);
        assert_eq!(t.verify, 15);
    }

    #[test]
    fn test_provision_error_fields() {
        let e = ProvisionError::new("provision_failed", "extract", "disk full");
        assert_eq!(e.code, "provision_failed");
        assert_eq!(e.stage, "extract");
        assert_eq!(e.message, "disk full");
        assert!(e.url.is_none());
    }
}
