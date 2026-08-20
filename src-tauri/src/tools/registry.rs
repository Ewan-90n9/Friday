use super::risk::RiskLevel;
use crate::exec::channel::ExecChannel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub data: serde_json::Value,
    pub raw_stdout: Option<String>,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(
        &self,
        args: serde_json::Value,
        channel: &dyn ExecChannel,
    ) -> ToolOutput;
}

pub struct ToolDef {
    pub name: String,
    pub schema: serde_json::Value,
    pub risk_level: RiskLevel,
    pub handler: Arc<dyn ToolHandler>,
}

pub struct ToolRegistry {
    tools: HashMap<String, ToolDef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, def: ToolDef) {
        self.tools.insert(def.name.clone(), def);
    }

    pub async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Value,
        _channel: &dyn ExecChannel,
    ) -> Option<ToolOutput> {
        let _def = self.tools.get(name)?;
        todo!()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
