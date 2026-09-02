use super::category::ToolCategory;
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

pub struct ToolContext {
    pub session_id: String,
    /// None for tools with `needs_channel: false` (echo, get_playbook, etc.)
    pub channel: Option<Arc<dyn ExecChannel>>,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput;
}

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub risk_level: RiskLevel,
    /// 面板分组归属（见 tools/category.rs；枚举声明序即分组展示序）
    pub category: ToolCategory,
    /// Whether the tool requires a remote ExecChannel. Local tools (echo,
    /// get_playbook) set this to false and run without an environment.
    pub needs_channel: bool,
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

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    pub fn list(&self) -> Vec<&ToolDef> {
        self.tools.values().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHandler;

    #[async_trait]
    impl ToolHandler for DummyHandler {
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput {
                success: true,
                data: serde_json::json!({"result": "ok"}),
                raw_stdout: None,
            }
        }
    }

    fn make_tool_def(name: &str, risk: RiskLevel) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: format!("Test tool {}", name),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                }
            }),
            risk_level: risk,
            category: ToolCategory::Environment,
            needs_channel: true,
            handler: Arc::new(DummyHandler),
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));

        assert!(registry.get("jstat").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_list_returns_all_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));
        registry.register(make_tool_def("arthas_trace", RiskLevel::Low));

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_empty_registry() {
        let registry = ToolRegistry::new();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_register_overwrites_same_name() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool_def("jstat", RiskLevel::ReadOnly));
        registry.register(make_tool_def("jstat", RiskLevel::High));

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(registry.get("jstat").unwrap().risk_level, RiskLevel::High);
    }
}
