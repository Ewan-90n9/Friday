use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;

pub async fn init(app_data_dir: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_path = app_data_dir.join("friday.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    let schema1 = include_str!("../../migrations/0001_init.sql");
    sqlx::query(schema1).execute(&pool).await?;
    let schema2 = include_str!("../../migrations/0002_agents.sql");
    sqlx::query(schema2).execute(&pool).await?;
    let schema3 = include_str!("../../migrations/0003_conversation.sql");
    sqlx::query(schema3).execute(&pool).await?;
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
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions', 'diagnosis_steps', 'tool_calls', 'environments', 'agents')"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(table_count, 5);
    }

    #[tokio::test]
    async fn test_db_init_creates_agents_index() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().to_path_buf()).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_agents_active'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 1);
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

    #[tokio::test]
    async fn test_db_init_adds_conversation_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().to_path_buf()).await.unwrap();

        let col_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name IN ('opencode_session_id', 'title')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(col_count, 2);
    }
}
