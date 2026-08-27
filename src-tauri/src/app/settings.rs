use sqlx::SqlitePool;

pub const KEY_ARTIFACTORY_BASE_URL: &str = "artifactory_base_url";
pub const DEFAULT_ARTIFACTORY_BASE_URL: &str =
    "https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release";

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
}
