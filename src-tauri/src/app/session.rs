use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionId(pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub env: String,
    pub service: String,
    pub symptom: String,
    pub status: SessionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Closed,
}

#[derive(Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub archived_at: Option<String>,
}

#[derive(Serialize)]
pub struct MessagePartRow {
    pub part_type: String,
    pub seq: i64,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub tool_status: Option<String>,
    pub tool_output: Option<String>,
    pub tool_elapsed_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct MessageRow {
    pub id: String,
    pub role: String,
    pub content: Option<String>,
    pub status: Option<String>,
    pub seq: i64,
    pub parts: Vec<MessagePartRow>,
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn truncate_title(message: &str) -> String {
    let chars: Vec<char> = message.trim().chars().take(40).collect();
    chars.into_iter().collect()
}

pub async fn create_session(
    pool: &SqlitePool,
    message: &str,
) -> Result<Session, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    let title = truncate_title(message);

    sqlx::query(
        "INSERT INTO sessions (id, env, service, symptom, status, created_at) \
         VALUES (?, '', '', '', 'active', ?)",
    )
    .bind(&id)
    .bind(&now)
    .execute(pool)
    .await?;

    sqlx::query("UPDATE sessions SET title = ? WHERE id = ?")
        .bind(&title)
        .bind(&id)
        .execute(pool)
        .await?;

    Ok(Session {
        id: SessionId(id),
        env: String::new(),
        service: String::new(),
        symptom: String::new(),
        status: SessionStatus::Active,
    })
}

pub async fn close_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = now_iso8601();
    sqlx::query("UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_sessions(
    pool: &SqlitePool,
    include_archived: bool,
) -> Result<Vec<SessionRow>, sqlx::Error> {
    let rows = if include_archived {
        sqlx::query(
            "SELECT id, title, status, created_at, archived_at \
             FROM sessions WHERE status = 'archived' \
             ORDER BY archived_at DESC",
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id, title, status, created_at, archived_at \
             FROM sessions WHERE status IN ('active', 'closed') \
             ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await?
    };

    rows.into_iter()
        .map(|row| {
            Ok(SessionRow {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                status: row.try_get("status")?,
                created_at: row.try_get("created_at")?,
                archived_at: row.try_get("archived_at")?,
            })
        })
        .collect()
}

pub async fn get_session(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, title, status, created_at, archived_at FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    row.map(|row| {
        Ok(SessionRow {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            status: row.try_get("status")?,
            created_at: row.try_get("created_at")?,
            archived_at: row.try_get("archived_at")?,
        })
    })
    .transpose()
}

pub async fn get_agent_session_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT agent_session_id FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(oc_id,)| oc_id))
}

pub async fn update_agent_session_id(
    pool: &SqlitePool,
    id: &str,
    agent_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sessions SET agent_session_id = ? WHERE id = ?")
        .bind(agent_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    async fn setup() -> SqlitePool {
        let tmp = tempfile::tempdir().unwrap();
        db::init(tmp.path().join("friday.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_create_session_inserts_row() {
        let pool = setup().await;
        let session = create_session(&pool, "OOMService 频繁 OOM").await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_create_session_sets_title_to_first_40_chars() {
        let pool = setup().await;
        let short_msg = "OOMService OOM 了";
        let session = create_session(&pool, short_msg).await.unwrap();

        let title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, short_msg);
    }

    #[tokio::test]
    async fn test_create_session_truncates_long_title() {
        let pool = setup().await;
        let long_msg = "这是一条非常非常非常非常非常非常非常非常非常非常非常非常非常非常长的消息要超过四十个字符";
        let session = create_session(&pool, long_msg).await.unwrap();

        let title: String =
            sqlx::query_scalar("SELECT title FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        let title_chars: Vec<char> = title.chars().collect();
        assert_eq!(title_chars.len(), 40);
    }

    #[tokio::test]
    async fn test_close_session_updates_status() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        close_session(&pool, &session.id.0).await.unwrap();

        let status: String =
            sqlx::query_scalar("SELECT status FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "closed");
    }

    #[tokio::test]
    async fn test_list_sessions_returns_descending_by_created_at() {
        let pool = setup().await;
        let s1 = create_session(&pool, "first").await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let s2 = create_session(&pool, "second").await.unwrap();

        let rows = list_sessions(&pool, false).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, s2.id.0);
        assert_eq!(rows[1].id, s1.id.0);
    }

    #[tokio::test]
    async fn test_get_session_returns_none_for_nonexistent() {
        let pool = setup().await;
        let result = get_session(&pool, "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_session_returns_row() {
        let pool = setup().await;
        let session = create_session(&pool, "test message").await.unwrap();

        let row = get_session(&pool, &session.id.0).await.unwrap().unwrap();
        assert_eq!(row.id, session.id.0);
        assert_eq!(row.title, Some("test message".to_string()));
        assert_eq!(row.status, "active");
    }

    #[tokio::test]
    async fn test_get_agent_session_id_returns_none_initially() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        let result = get_agent_session_id(&pool, &session.id.0).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_update_agent_session_id_persists() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        update_agent_session_id(&pool, &session.id.0, "agent-123").await.unwrap();

        let result = get_agent_session_id(&pool, &session.id.0).await.unwrap();
        assert_eq!(result, Some("agent-123".to_string()));
    }
}
