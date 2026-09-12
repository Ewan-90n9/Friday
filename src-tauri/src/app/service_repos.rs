use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::State;

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

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_service_repos_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<ServiceRepoRow>, String> {
    tracing::info!("list_service_repos_cmd called");
    list_service_repos(&state.db).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_service_repo_cmd(
    state: State<'_, crate::AppState>,
    service: String,
) -> Result<(), String> {
    tracing::info!(service = %service, "delete_service_repo_cmd called");
    delete_service_repo(&state.db, &service)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_repo_cache_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<crate::code::RepoCacheEntry>, String> {
    tracing::info!("list_repo_cache_cmd called");
    Ok(state.code_repos.list_cache().await)
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_repo_cache_cmd(
    state: State<'_, crate::AppState>,
    url_hash: String,
) -> Result<(), String> {
    tracing::info!(url_hash = %url_hash, "delete_repo_cache_cmd called");
    state.code_repos.delete_cache(&url_hash).await
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
        // raw SQL 钉死旧 last_used_at，get 命中后应刷新（不依赖时钟精度，读库验证而非返回值）
        sqlx::query(
            "INSERT INTO service_repos (service, repo_url, last_ref, updated_at, last_used_at) \
             VALUES ('S', 'https://a.git', NULL, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        get_service_repo(&pool, "S").await.unwrap().unwrap();
        let refreshed: String =
            sqlx::query_scalar("SELECT last_used_at FROM service_repos WHERE service = 'S'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(refreshed, "2020-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn test_list_orders_by_last_used_desc_and_delete() {
        let pool = test_pool().await;
        // raw SQL 钉死已知时间戳：A 较新、B 较旧，DESC 则 A 在前
        sqlx::query(
            "INSERT INTO service_repos (service, repo_url, last_ref, updated_at, last_used_at) \
             VALUES ('A', 'https://a.git', NULL, '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO service_repos (service, repo_url, last_ref, updated_at, last_used_at) \
             VALUES ('B', 'https://b.git', NULL, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].service, "A");
        assert_eq!(list[1].service, "B");
        // get 刷新 B 的 last_used_at 至当前时间，B 应跃升队首
        get_service_repo(&pool, "B").await.unwrap().unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list[0].service, "B");
        assert_eq!(list[1].service, "A");
        delete_service_repo(&pool, "A").await.unwrap();
        let list = list_service_repos(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].service, "B");
    }
}
