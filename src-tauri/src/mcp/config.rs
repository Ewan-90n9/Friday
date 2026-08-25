use serde_json::Value;
use std::path::PathBuf;

pub fn merge_friday_mcp_config(config_path: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{port}/mcp");

    let existing = read_config(&config_path)?;
    let merged = inject_friday_entry(existing, &url);

    write_config(&config_path, &merged)?;

    tracing::info!(path = %config_path.display(), port, url = %url, "merged Friday MCP config into opencode");
    Ok(())
}

pub fn merge_codeagentcli_mcp_config(config_path: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{port}/mcp");

    let existing = read_config(&config_path)?;
    let merged = inject_friday_entry_codeagentcli(existing, &url);

    write_config(&config_path, &merged)?;

    tracing::info!(path = %config_path.display(), port, url = %url, "merged Friday MCP config into codeagentcli");
    Ok(())
}

fn read_config(path: &PathBuf) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return Ok(Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(path)?;

    match serde_json::from_str::<Value>(&content) {
        Ok(v) => Ok(v),
        Err(e) => {
            tracing::warn!(?e, path = %path.display(), "failed to parse config, backing up and starting fresh");
            let backup = path.with_extension(format!("{}.bak", path.extension().and_then(|e| e.to_str()).unwrap_or("json")));
            std::fs::rename(path, &backup)?;
            Ok(Value::Object(serde_json::Map::new()))
        }
    }
}

fn inject_friday_entry(mut config: Value, url: &str) -> Value {
    if config.get("mcp").is_none() {
        config["mcp"] = Value::Object(serde_json::Map::new());
    }

    let mcp = config.get_mut("mcp").unwrap();
    if mcp.get("friday").is_none() {
        mcp["friday"] = Value::Object(serde_json::Map::new());
    }

    let friday = mcp.get_mut("friday").unwrap();
    friday["type"] = Value::String("remote".to_string());
    friday["url"] = Value::String(url.to_string());
    friday["enabled"] = Value::Bool(true);
    friday["timeout"] = Value::Number(serde_json::Number::from(10000));

    config
}

fn write_config(path: &PathBuf, config: &Value) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pretty = serde_json::to_string_pretty(config)?;
    std::fs::write(path, pretty)?;
    Ok(())
}

pub fn default_opencode_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("opencode").join("opencode.jsonc"))
}

pub fn default_codeagentcli_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cac").join("settings.json"))
}

fn inject_friday_entry_codeagentcli(mut config: Value, url: &str) -> Value {
    if config.get("mcpServers").is_none() {
        config["mcpServers"] = Value::Object(serde_json::Map::new());
    }

    let mcp = config.get_mut("mcpServers").unwrap();
    if mcp.get("friday").is_none() {
        mcp["friday"] = Value::Object(serde_json::Map::new());
    }

    let friday = mcp.get_mut("friday").unwrap();
    // Streamable HTTP transport (Claude Code convention: "http" = streamable,
    // "sse" = legacy HTTP+SSE). Friday's MCP server is rmcp StreamableHttpService.
    friday["type"] = Value::String("http".to_string());
    friday["url"] = Value::String(url.to_string());
    friday["enabled"] = Value::Bool(true);

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_friday_entry_into_empty_config() {
        let config = Value::Object(serde_json::Map::new());
        let result = inject_friday_entry(config, "http://127.0.0.1:12345/mcp");

        assert_eq!(result["mcp"]["friday"]["type"], "remote");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
        assert_eq!(result["mcp"]["friday"]["enabled"], true);
        assert_eq!(result["mcp"]["friday"]["timeout"], 10000);
    }

    #[test]
    fn test_inject_friday_entry_preserves_existing_config() {
        let config = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "disabled_providers": ["zhipu"],
            "provider": {
                "zhipu": { "name": "Zhipu AI" }
            },
            "mcp": {
                "other_server": {
                    "type": "local",
                    "command": ["npx", "other"]
                }
            }
        });

        let result = inject_friday_entry(config, "http://127.0.0.1:9999/mcp");

        assert_eq!(result["disabled_providers"][0], "zhipu");
        assert_eq!(result["mcp"]["other_server"]["type"], "local");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:9999/mcp");
    }

    #[test]
    fn test_inject_friday_entry_updates_existing_friday() {
        let config = serde_json::json!({
            "mcp": {
                "friday": {
                    "type": "remote",
                    "url": "http://127.0.0.1:OLD/mcp"
                }
            }
        });

        let result = inject_friday_entry(config, "http://127.0.0.1:NEW/mcp");
        assert_eq!(result["mcp"]["friday"]["url"], "http://127.0.0.1:NEW/mcp");
    }

    #[tokio::test]
    async fn test_merge_friday_mcp_config_creates_file_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");

        merge_friday_mcp_config(config_path.clone(), 12345).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
    }

    #[tokio::test]
    async fn test_merge_friday_mcp_config_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");
        std::fs::write(&config_path, r#"{"$schema":"https://opencode.ai/config.json","disabled_providers":["zhipu"]}"#).unwrap();

        merge_friday_mcp_config(config_path.clone(), 54321).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["disabled_providers"][0], "zhipu");
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:54321/mcp");
    }
    #[tokio::test]
    async fn test_merge_friday_mcp_config_backs_up_corrupted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("opencode.jsonc");
        std::fs::write(&config_path, "not valid json {{{").unwrap();

        merge_friday_mcp_config(config_path.clone(), 11111).unwrap();

        assert!(config_path.with_extension("jsonc.bak").exists());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["friday"]["url"], "http://127.0.0.1:11111/mcp");
    }

    #[test]
    fn test_inject_friday_entry_codeagentcli_into_empty_config() {
        let config = Value::Object(serde_json::Map::new());
        let result = inject_friday_entry_codeagentcli(config, "http://127.0.0.1:12345/mcp");

        assert_eq!(result["mcpServers"]["friday"]["type"], "http");
        assert_eq!(result["mcpServers"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
        assert_eq!(result["mcpServers"]["friday"]["enabled"], true);
    }

    #[test]
    fn test_inject_friday_entry_codeagentcli_preserves_existing_config() {
        let config = serde_json::json!({
            "mcpServers": {
                "other_server": {
                    "type": "stdio",
                    "command": "other"
                }
            }
        });

        let result = inject_friday_entry_codeagentcli(config, "http://127.0.0.1:9999/mcp");

        assert_eq!(result["mcpServers"]["other_server"]["type"], "stdio");
        assert_eq!(result["mcpServers"]["friday"]["url"], "http://127.0.0.1:9999/mcp");
    }

    #[test]
    fn test_inject_friday_entry_codeagentcli_updates_existing_friday() {
        let config = serde_json::json!({
            "mcpServers": {
                "friday": {
                    "type": "http",
                    "url": "http://127.0.0.1:OLD/mcp"
                }
            }
        });

        let result = inject_friday_entry_codeagentcli(config, "http://127.0.0.1:NEW/mcp");
        assert_eq!(result["mcpServers"]["friday"]["url"], "http://127.0.0.1:NEW/mcp");
    }

    #[tokio::test]
    async fn test_merge_codeagentcli_mcp_config_creates_file_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("settings.json");

        merge_codeagentcli_mcp_config(config_path.clone(), 12345).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["friday"]["url"], "http://127.0.0.1:12345/mcp");
    }

    #[tokio::test]
    async fn test_merge_codeagentcli_mcp_config_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("settings.json");
        std::fs::write(&config_path, r#"{"someSetting":true}"#).unwrap();

        merge_codeagentcli_mcp_config(config_path.clone(), 54321).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["someSetting"], true);
        assert_eq!(parsed["mcpServers"]["friday"]["url"], "http://127.0.0.1:54321/mcp");
    }

    #[tokio::test]
    async fn test_merge_codeagentcli_mcp_config_backs_up_corrupted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("settings.json");
        std::fs::write(&config_path, "not valid json {{{").unwrap();

        merge_codeagentcli_mcp_config(config_path.clone(), 11111).unwrap();

        assert!(config_path.with_extension("json.bak").exists());
        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["friday"]["url"], "http://127.0.0.1:11111/mcp");
    }
}
