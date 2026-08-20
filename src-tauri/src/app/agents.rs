use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::State;

use crate::agent;
use crate::agent::detect::DetectedAgent;

#[derive(Serialize)]
pub struct AgentRow {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    pub path: String,
    pub version: Option<String>,
    pub source: String,
    pub is_active: bool,
    pub detected_at: String,
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub async fn detect_and_persist(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let detected = agent::detect::detect().await;
    for d in &detected {
        upsert_auto_agent(pool, d).await?;
    }
    ensure_active(pool).await?;
    Ok(())
}

pub async fn upsert_auto_agent(pool: &SqlitePool, d: &DetectedAgent) -> Result<(), sqlx::Error> {
    let now = now_iso8601();
    let path_str = d.path.display().to_string();

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agents WHERE provider = ? AND source = 'auto'",
    )
    .bind(d.provider)
    .fetch_one(pool)
    .await?;

    if count > 0 {
        sqlx::query(
            "UPDATE agents SET path = ?, version = ?, detected_at = ? \
             WHERE provider = ? AND source = 'auto'",
        )
        .bind(&path_str)
        .bind(&d.version)
        .bind(&now)
        .bind(d.provider)
        .execute(pool)
        .await?;
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES (?, ?, ?, ?, ?, 'auto', 0, ?, ?)",
        )
        .bind(&id)
        .bind(d.provider)
        .bind(d.display_name)
        .bind(&path_str)
        .bind(&d.version)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn ensure_active(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let active_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents WHERE is_active = 1")
        .fetch_one(pool)
        .await?;

    if active_count == 0 {
        let first_auto: Option<(String,)> =
            sqlx::query_as("SELECT id FROM agents WHERE source = 'auto' ORDER BY detected_at DESC LIMIT 1")
                .fetch_optional(pool)
                .await?;

        if let Some((id,)) = first_auto {
            sqlx::query("UPDATE agents SET is_active = 1 WHERE id = ?")
                .bind(&id)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

pub async fn set_active(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE agents SET is_active = 0")
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("UPDATE agents SET is_active = 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    tx.commit().await?;
    Ok(())
}

pub async fn remove_agent(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let was_active: Option<i64> =
        sqlx::query_scalar("SELECT is_active FROM agents WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;

    sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if was_active == Some(1) {
        ensure_active(pool).await?;
    }
    Ok(())
}

pub async fn list_agents(pool: &SqlitePool) -> Result<Vec<AgentRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, provider, display_name, path, version, source, is_active, detected_at \
         FROM agents \
         ORDER BY is_active DESC, CASE source WHEN 'auto' THEN 0 ELSE 1 END, detected_at DESC",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let is_active: i64 = row.try_get("is_active")?;
            Ok(AgentRow {
                id: row.try_get("id")?,
                provider: row.try_get("provider")?,
                display_name: row.try_get("display_name")?,
                path: row.try_get("path")?,
                version: row.try_get("version")?,
                source: row.try_get("source")?,
                is_active: is_active != 0,
                detected_at: row.try_get("detected_at")?,
            })
        })
        .collect()
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn detect_agents_cmd(state: State<'_, crate::AppState>) -> Result<(), String> {
    detect_and_persist(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_agents_cmd(state: State<'_, crate::AppState>) -> Result<Vec<AgentRow>, String> {
    tracing::info!("list_agents_cmd called");
    list_agents(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_agent_cmd(
    state: State<'_, crate::AppState>,
    provider: String,
    path: String,
) -> Result<AgentRow, String> {
    tracing::info!(provider = %provider, path = %path, "add_agent_cmd called");
    let valid = agent::registry::REGISTRY
        .iter()
        .any(|d| d.provider == provider);
    if !valid {
        return Err(format!("Unknown provider: {}", provider));
    }

    let version = agent::detect::detect_version(std::path::Path::new(&path)).await;

    let display_name = agent::registry::REGISTRY
        .iter()
        .find(|d| d.provider == provider)
        .map(|d| d.display_name.to_string())
        .unwrap_or_else(|| provider.clone());

    let now = now_iso8601();
    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
         VALUES (?, ?, ?, ?, ?, 'manual', 0, ?, ?)",
    )
    .bind(&id)
    .bind(&provider)
    .bind(&display_name)
    .bind(&path)
    .bind(&version)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| e.to_string())?;

    Ok(AgentRow {
        id,
        provider,
        display_name,
        path,
        version,
        source: "manual".to_string(),
        is_active: false,
        detected_at: now,
    })
}

#[tauri::command]
pub async fn set_active_agent_cmd(
    state: State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    tracing::info!(id = %id, "set_active_agent_cmd called");
    set_active(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_agent_cmd(
    state: State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    tracing::info!(id = %id, "remove_agent_cmd called");
    remove_agent(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::detect::DetectedAgent;
    use crate::infra::db;
    use sqlx::SqlitePool;
    use std::path::PathBuf;

    async fn setup() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        db::init(tmp.path().to_path_buf()).await.unwrap()
    }

    fn make_detected() -> DetectedAgent {
        DetectedAgent {
            provider: "opencode",
            display_name: "OpenCode",
            path: PathBuf::from("/usr/bin/opencode"),
            version: Some("0.2.15".to_string()),
        }
    }

    async fn insert_test_agent(
        pool: &SqlitePool,
        id: &str,
        provider: &str,
        source: &str,
        is_active: i64,
        detected_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES (?, ?, 'Test', '/test/path', NULL, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(provider)
        .bind(source)
        .bind(is_active)
        .bind(detected_at)
        .bind(detected_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn count_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agents")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn count_active(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agents WHERE is_active = 1")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn get_is_active(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT is_active FROM agents WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_upsert_auto_agent_inserts_new_row() {
        let pool = setup().await;
        upsert_auto_agent(&pool, &make_detected()).await.unwrap();

        assert_eq!(count_rows(&pool).await, 1);
        let (source, is_active): (String, i64) =
            sqlx::query_as("SELECT source, is_active FROM agents WHERE provider = 'opencode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(source, "auto");
        assert_eq!(is_active, 0);
    }

    #[tokio::test]
    async fn test_upsert_auto_agent_updates_existing() {
        let pool = setup().await;
        upsert_auto_agent(&pool, &make_detected()).await.unwrap();

        let mut updated = make_detected();
        updated.path = PathBuf::from("/new/path");
        updated.version = Some("0.3.0".to_string());
        upsert_auto_agent(&pool, &updated).await.unwrap();

        assert_eq!(count_rows(&pool).await, 1);
        let (path, version): (String, Option<String>) =
            sqlx::query_as("SELECT path, version FROM agents WHERE provider = 'opencode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(path, "/new/path");
        assert_eq!(version, Some("0.3.0".to_string()));
    }

    #[tokio::test]
    async fn test_ensure_active_promotes_first_auto() {
        let pool = setup().await;
        upsert_auto_agent(&pool, &make_detected()).await.unwrap();
        assert_eq!(count_active(&pool).await, 0);

        ensure_active(&pool).await.unwrap();

        assert_eq!(count_active(&pool).await, 1);
    }

    #[tokio::test]
    async fn test_ensure_active_does_not_change_existing_active() {
        let pool = setup().await;
        insert_test_agent(&pool, "a", "opencode", "manual", 1, "100").await;
        insert_test_agent(&pool, "b", "opencode", "auto", 0, "200").await;

        ensure_active(&pool).await.unwrap();

        assert_eq!(count_active(&pool).await, 1);
        assert_eq!(get_is_active(&pool, "a").await, 1);
        assert_eq!(get_is_active(&pool, "b").await, 0);
    }

    #[tokio::test]
    async fn test_set_active_switches_active() {
        let pool = setup().await;
        insert_test_agent(&pool, "a", "opencode", "manual", 1, "100").await;
        insert_test_agent(&pool, "b", "opencode", "auto", 0, "200").await;

        set_active(&pool, "b").await.unwrap();

        assert_eq!(count_active(&pool).await, 1);
        assert_eq!(get_is_active(&pool, "a").await, 0);
        assert_eq!(get_is_active(&pool, "b").await, 1);
    }

    #[tokio::test]
    async fn test_set_active_rejects_nonexistent_id() {
        let pool = setup().await;
        insert_test_agent(&pool, "a", "opencode", "auto", 1, "100").await;

        let result = set_active(&pool, "nonexistent").await;

        assert!(result.is_err());
        assert_eq!(get_is_active(&pool, "a").await, 1);
    }

    #[tokio::test]
    async fn test_remove_active_triggers_ensure_active() {
        let pool = setup().await;
        insert_test_agent(&pool, "a", "opencode", "auto", 1, "100").await;
        insert_test_agent(&pool, "b", "opencode", "auto", 0, "200").await;

        remove_agent(&pool, "a").await.unwrap();

        assert_eq!(count_rows(&pool).await, 1);
        assert_eq!(get_is_active(&pool, "b").await, 1);
    }
}
