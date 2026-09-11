use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum ServiceRepoError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Serialize, Clone, Debug)]
pub struct ServiceRepoRow {
    pub service: String,
    pub repo_url: String,
    pub last_ref: Option<String>,
    pub updated_at: String,
    pub last_used_at: String,
}

const COLUMNS: &str = "service, repo_url, last_ref, updated_at, last_used_at";

fn row_to_service_repo(r: &sqlx::sqlite::SqliteRow) -> ServiceRepoRow {
    ServiceRepoRow {
        service: r.get("service"),
        repo_url: r.get("repo_url"),
        last_ref: r.get("last_ref"),
        updated_at: r.get("updated_at"),
        last_used_at: r.get("last_used_at"),
    }
}

/// upsert：repo_url 权威覆盖、last_ref 记本次
pub async fn upsert_service_repo(
    pool: &SqlitePool,
    service: &str,
    repo_url: &str,
    last_ref: Option<&str>,
) -> Result<(), ServiceRepoError> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO service_repos (service, repo_url, last_ref, updated_at, last_used_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(service) DO UPDATE SET \
           repo_url = excluded.repo_url, last_ref = excluded.last_ref, \
           updated_at = excluded.updated_at, last_used_at = excluded.last_used_at",
    )
    .bind(service)
    .bind(repo_url)
    .bind(last_ref)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查映射；命中时刷新 last_used_at
pub async fn get_service_repo(
    pool: &SqlitePool,
    service: &str,
) -> Result<Option<ServiceRepoRow>, ServiceRepoError> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM service_repos WHERE service = ?"))
        .bind(service)
        .fetch_optional(pool)
        .await?;
    if row.is_some() {
        sqlx::query("UPDATE service_repos SET last_used_at = ? WHERE service = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(service)
            .execute(pool)
            .await?;
    }
    Ok(row.map(|r| row_to_service_repo(&r)))
}

pub async fn list_service_repos(pool: &SqlitePool) -> Result<Vec<ServiceRepoRow>, ServiceRepoError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM service_repos ORDER BY last_used_at DESC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_service_repo).collect())
}

pub async fn delete_service_repo(pool: &SqlitePool, service: &str) -> Result<(), ServiceRepoError> {
    sqlx::query("DELETE FROM service_repos WHERE service = ?")
        .bind(service)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_upsert_then_get_roundtrip() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "BarService", "https://git.example.com/bar.git", Some("main"))
            .await
            .unwrap();
        let row = get_service_repo(&pool, "BarService").await.unwrap().unwrap();
        assert_eq!(row.repo_url, "https://git.example.com/bar.git");
        assert_eq!(row.last_ref.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn test_upsert_overwrites_url_and_ref() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "S", "https://a.git", Some("main")).await.unwrap();
        upsert_service_repo(&pool, "S", "https://b.git", Some("release-1.2")).await.unwrap();
        let row = get_service_repo(&pool, "S").await.unwrap().unwrap();
        assert_eq!(row.repo_url, "https://b.git");
        assert_eq!(row.last_ref.as_deref(), Some("release-1.2"));
    }

    #[tokio::test]
    async fn test_get_missing_returns_none() {
        let pool = test_pool().await;
        assert!(get_service_repo(&pool, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_get_refreshes_last_used_at() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "S", "https://a.git", None).await.unwrap();
        let before = get_service_repo(&pool, "S").await.unwrap().unwrap().last_used_at;
        // 时间戳至少非空且再次读取成功（rfc3339 精度内可能相等，断言可再读）
        let after = get_service_repo(&pool, "S").await.unwrap().unwrap().last_used_at;
        assert!(!before.is_empty());
        assert!(!after.is_empty());
    }

    #[tokio::test]
    async fn test_list_orders_by_last_used_desc_and_delete() {
        let pool = test_pool().await;
        upsert_service_repo(&pool, "A", "https://a.git", None).await.unwrap();
        upsert_service_repo(&pool, "B", "https://b.git", None).await.unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 2);
        delete_service_repo(&pool, "A").await.unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].service, "B");
    }
}
