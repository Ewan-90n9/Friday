use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;

pub async fn init(app_data_dir: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_path = app_data_dir.join("friday.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    let schema = include_str!("../../migrations/0001_init.sql");
    sqlx::query(schema).execute(&pool).await?;
    tracing::info!(?db_path, "SQLite initialized");
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_db_init_creates_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().to_path_buf()).await.unwrap();

        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions', 'diagnosis_steps', 'tool_calls', 'environments')"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(table_count, 4);
    }

    #[tokio::test]
    async fn test_db_init_creates_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().to_path_buf()).await.unwrap();

        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('idx_diagnosis_steps_session', 'idx_tool_calls_session')"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(index_count, 2);
    }
}
