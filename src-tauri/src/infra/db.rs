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
    // Migration 0003: add conversation columns (idempotent — safe to re-run)
    add_column_if_not_exists(&pool, "sessions", "opencode_session_id", "TEXT").await?;
    add_column_if_not_exists(&pool, "sessions", "title", "TEXT").await?;
    tracing::info!(?db_path, "SQLite initialized");
    Ok(pool)
}

/// Add a column to a table only if it doesn't already exist.
/// SQLite's ALTER TABLE ADD COLUMN lacks IF NOT EXISTS support.
async fn add_column_if_not_exists(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<(), sqlx::Error> {
    let exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, column
        ),
    )
    .fetch_one(pool)
    .await?;

    if exists == 0 {
        let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type);
        sqlx::query(&sql).execute(pool).await?;
        tracing::info!(table, column, "added column");
    }

    Ok(())
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
