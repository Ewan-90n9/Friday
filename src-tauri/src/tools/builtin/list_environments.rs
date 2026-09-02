use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::sync::Arc;

pub struct ListEnvironmentsHandler {
    pub db: sqlx::SqlitePool,
}

#[async_trait]
impl ToolHandler for ListEnvironmentsHandler {
    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        match crate::app::environments::list_environments(&self.db).await {
            Ok(envs) => {
                let list: Vec<serde_json::Value> = envs
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name,
                            "host": e.host,
                            "port": e.port,
                            "user": e.user,
                            "auth_type": e.auth_type,
                        })
                    })
                    .collect();
                ToolOutput {
                    success: true,
                    data: serde_json::json!({ "environments": list }),
                    raw_stdout: None,
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "list_environments: database query failed");
                ToolOutput {
                    success: false,
                    data: serde_json::json!({ "error": "lookup_failed", "message": format!("failed to list environments: {e}") }),
                    raw_stdout: None,
                }
            }
        }
    }
}

pub fn list_environments_tool_def(db: sqlx::SqlitePool) -> ToolDef {
    ToolDef {
        name: "list_environments".to_string(),
        description: "列出所有已配置的远程诊断环境（名称、host、端口、用户、认证方式）。诊断远程环境前先调用本工具，把用户提到的环境名或 IP 与列表匹配；若无匹配环境，请用户提供环境信息并引导用户在 Friday 右侧「环境」面板添加，不要猜测 host。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Environment,
        needs_channel: false,
        handler: Arc::new(ListEnvironmentsHandler { db }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolContext;

    #[tokio::test]
    async fn test_list_environments_returns_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::env_save::save_environment(
            &db, None, "prod", "10.0.0.1", 22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        ).await.unwrap();

        let handler = ListEnvironmentsHandler { db };
        let ctx = ToolContext { session_id: "s1".to_string(), channel: None };
        let output = handler.execute(serde_json::json!({}), &ctx).await;

        assert!(output.success);
        let envs = output.data["environments"].as_array().unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0]["name"], "prod");
        assert_eq!(envs[0]["host"], "10.0.0.1");
        assert_eq!(envs[0]["user"], "root");
        assert_eq!(envs[0]["auth_type"], "password");
    }

    #[tokio::test]
    async fn test_list_environments_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();

        let handler = ListEnvironmentsHandler { db };
        let ctx = ToolContext { session_id: "s1".to_string(), channel: None };
        let output = handler.execute(serde_json::json!({}), &ctx).await;

        assert!(output.success);
        assert_eq!(output.data["environments"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let def = list_environments_tool_def(dummy_db());
        assert_eq!(def.name, "list_environments");
        assert_eq!(def.risk_level, crate::tools::risk::RiskLevel::ReadOnly);
        assert_eq!(def.category, ToolCategory::Environment);
        assert!(!def.needs_channel);
        assert!(def.description.contains("list_environments") || def.description.len() > 20);
    }

    fn dummy_db() -> sqlx::SqlitePool {
        // connect_lazy to nonexistent path — def construction doesn't touch DB
        sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap()
    }
}
