use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::PathBuf;

pub async fn init(db_path: PathBuf) -> Result<SqlitePool, sqlx::Error> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    let schema1 = include_str!("../../migrations/0001_init.sql");
    sqlx::query(schema1).execute(&pool).await?;
    let schema2 = include_str!("../../migrations/0002_agents.sql");
    sqlx::query(schema2).execute(&pool).await?;
    // Migration 0003/0004: rename opencode_session_id → agent_session_id, add title
    rename_column_if_exists(&pool, "sessions", "opencode_session_id", "agent_session_id").await?;
    add_column_if_not_exists(&pool, "sessions", "agent_session_id", "TEXT").await?;
    add_column_if_not_exists(&pool, "sessions", "title", "TEXT").await?;
    add_column_if_not_exists(&pool, "sessions", "archived_at", "TEXT").await?;
    let schema5 = include_str!("../../migrations/0005_session_messages.sql");
    sqlx::query(schema5).execute(&pool).await?;
    let schema6 = include_str!("../../migrations/0006_memory.sql");
    sqlx::query(schema6).execute(&pool).await?;
    add_column_if_not_exists(&pool, "sessions", "language", "TEXT").await?;
    let _schema7 = include_str!("../../migrations/0007_environment_link.sql");
    add_column_if_not_exists(&pool, "sessions", "environment_id", "TEXT").await?;
    // Migration (phase 1): environments auth columns
    add_column_if_not_exists(&pool, "environments", "auth_type", "TEXT NOT NULL DEFAULT 'private_key'").await?;
    add_column_if_not_exists(&pool, "environments", "private_key_path", "TEXT").await?;
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

/// Rename a column only if the old column exists and the new one doesn't.
/// SQLite supports ALTER TABLE RENAME COLUMN since 3.25.0.
async fn rename_column_if_exists(
    pool: &SqlitePool,
    table: &str,
    old_name: &str,
    new_name: &str,
) -> Result<(), sqlx::Error> {
    let old_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, old_name
        ),
    )
    .fetch_one(pool)
    .await?;

    let new_exists: i64 = sqlx::query_scalar(
        &format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
            table, new_name
        ),
    )
    .fetch_one(pool)
    .await?;

    if old_exists > 0 && new_exists == 0 {
        let sql = format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            table, old_name, new_name
        );
        sqlx::query(&sql).execute(pool).await?;
        tracing::info!(table, old_name, new_name, "renamed column");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_db_init_creates_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

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
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

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
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

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
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let col_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name IN ('agent_session_id', 'title')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(col_count, 2);
    }

    #[tokio::test]
    async fn test_rename_column_if_exists_renames_column() {
        let tmp = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", tmp.path().join("test.db").display());
        let pool = SqlitePoolOptions::new()
            .connect(&db_url)
            .await
            .unwrap();

        sqlx::query("CREATE TABLE test_table (old_col TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        rename_column_if_exists(&pool, "test_table", "old_col", "new_col")
            .await
            .unwrap();

        let new_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'new_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(new_count, 1);

        let old_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'old_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old_count, 0);
    }

    #[tokio::test]
    async fn test_rename_column_if_exists_noop_when_old_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", tmp.path().join("test.db").display());
        let pool = SqlitePoolOptions::new()
            .connect(&db_url)
            .await
            .unwrap();

        sqlx::query("CREATE TABLE test_table (new_col TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        // old_col doesn't exist, new_col already exists — should be no-op
        rename_column_if_exists(&pool, "test_table", "old_col", "new_col")
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('test_table') WHERE name = 'new_col'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_db_init_creates_message_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('session_messages', 'session_message_parts')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_db_init_creates_message_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('idx_session_messages_session', 'idx_session_message_parts_message')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_db_init_adds_archived_at_column() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'archived_at'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_db_init_adds_environment_id_column() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='environment_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_db_init_adds_environment_auth_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        // 列存在性直接验证
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('environments') WHERE name = 'auth_type'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('environments') WHERE name = 'private_key_path'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // 新建环境默认 auth_type = 'private_key'
        sqlx::query(
            "INSERT INTO environments (id, name, transport_type, created_at) VALUES ('e1', 'test', 'ssh', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let auth_type: String = sqlx::query_scalar("SELECT auth_type FROM environments WHERE id = 'e1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(auth_type, "private_key");
    }

    #[tokio::test]
    async fn test_migration_0006_creates_memory_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = init(tmp.path().join("friday.db")).await.unwrap();

        // session_summaries table exists
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_summaries'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // experiences table exists
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='experiences'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // sessions.language column exists
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='language'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
}
