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

pub async fn next_message_seq(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<i64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_messages WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

pub async fn insert_message(
    pool: &SqlitePool,
    session_id: &str,
    role: &str,
    content: Option<&str>,
    status: Option<&str>,
    seq: i64,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    sqlx::query(
        "INSERT INTO session_messages (id, session_id, role, content, status, seq, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(status)
    .bind(seq)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn update_message_status(
    pool: &SqlitePool,
    message_id: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE session_messages SET status = ? WHERE id = ?")
        .bind(status)
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_text_part(
    pool: &SqlitePool,
    message_id: &str,
    seq: i64,
    text: &str,
) -> Result<(), sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO session_message_parts (id, message_id, part_type, seq, text) \
         VALUES (?, ?, 'text', ?, ?)",
    )
    .bind(&id)
    .bind(message_id)
    .bind(seq)
    .bind(text)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_tool_part(
    pool: &SqlitePool,
    message_id: &str,
    seq: i64,
    tool_name: &str,
    tool_args: &str,
    tool_status: &str,
    tool_output: &str,
    tool_elapsed_ms: i64,
) -> Result<(), sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO session_message_parts \
         (id, message_id, part_type, seq, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms) \
         VALUES (?, ?, 'tool', ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(message_id)
    .bind(seq)
    .bind(tool_name)
    .bind(tool_args)
    .bind(tool_status)
    .bind(tool_output)
    .bind(tool_elapsed_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_session_messages(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<MessageRow>, sqlx::Error> {
    let messages: Vec<(String, String, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, role, content, status, seq FROM session_messages \
         WHERE session_id = ? ORDER BY seq ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(messages.len());
    for (msg_id, role, content, status, seq) in messages {
        let parts: Vec<(String, String, i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT id, part_type, seq, text, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms \
             FROM session_message_parts WHERE message_id = ? ORDER BY seq ASC",
        )
        .bind(&msg_id)
        .fetch_all(pool)
        .await?;

        let part_rows: Vec<MessagePartRow> = parts
            .into_iter()
            .map(|(_, part_type, seq, text, tool_name, tool_args, tool_status, tool_output, tool_elapsed_ms)| MessagePartRow {
                part_type,
                seq,
                text,
                tool_name,
                tool_args,
                tool_status,
                tool_output,
                tool_elapsed_ms,
            })
            .collect();

        result.push(MessageRow {
            id: msg_id,
            role,
            content,
            status,
            seq,
            parts: part_rows,
        });
    }
    Ok(result)
}

pub async fn archive_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = now_iso8601();
    sqlx::query("UPDATE sessions SET status = 'archived', archived_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn unarchive_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sessions SET status = 'closed', archived_at = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let message_ids: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM session_messages WHERE session_id = ?")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;

    for (msg_id,) in &message_ids {
        sqlx::query("DELETE FROM session_message_parts WHERE message_id = ?")
            .bind(msg_id)
            .execute(&mut *tx)
            .await?;
    }

    sqlx::query("DELETE FROM session_messages WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM diagnosis_steps WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM tool_calls WHERE session_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
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

    #[tokio::test]
    async fn test_next_message_seq_starts_at_zero() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let seq = next_message_seq(&pool, &session.id.0).await.unwrap();
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn test_next_message_seq_increments() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 1).await.unwrap();
        let seq = next_message_seq(&pool, &session.id.0).await.unwrap();
        assert_eq!(seq, 2);
    }

    #[tokio::test]
    async fn test_insert_message_returns_id() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let id = insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn test_update_message_status() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        update_message_status(&pool, &msg_id, "done").await.unwrap();

        let status: String = sqlx::query_scalar("SELECT status FROM session_messages WHERE id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "done");
    }

    #[tokio::test]
    async fn test_insert_text_part() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        insert_text_part(&pool, &msg_id, 0, "Hello world").await.unwrap();

        let text: String = sqlx::query_scalar("SELECT text FROM session_message_parts WHERE message_id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(text, "Hello world");
    }

    #[tokio::test]
    async fn test_insert_tool_part() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "agent", None, Some("streaming"), 0).await.unwrap();
        insert_tool_part(&pool, &msg_id, 0, "bash", r#"{"command":"ls"}"#, "completed", "file1\nfile2", 800).await.unwrap();

        let (name, status, output, elapsed): (String, String, String, i64) =
            sqlx::query_as("SELECT tool_name, tool_status, tool_output, tool_elapsed_ms FROM session_message_parts WHERE message_id = ?")
                .bind(&msg_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "bash");
        assert_eq!(status, "completed");
        assert_eq!(output, "file1\nfile2");
        assert_eq!(elapsed, 800);
    }

    #[tokio::test]
    async fn test_get_session_messages_returns_messages_with_parts() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();

        let user_id = insert_message(&pool, &session.id.0, "user", Some("diagnose OOM"), Some("done"), 0).await.unwrap();
        let agent_id = insert_message(&pool, &session.id.0, "agent", None, Some("done"), 1).await.unwrap();
        insert_text_part(&pool, &agent_id, 0, "Analysis complete").await.unwrap();
        insert_tool_part(&pool, &agent_id, 1, "jstat", "{}", "completed", "output", 100).await.unwrap();

        let messages = get_session_messages(&pool, &session.id.0).await.unwrap();
        assert_eq!(messages.len(), 2);

        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, Some("diagnose OOM".to_string()));
        assert_eq!(messages[0].seq, 0);
        assert_eq!(messages[0].parts.len(), 0);

        assert_eq!(messages[1].role, "agent");
        assert_eq!(messages[1].seq, 1);
        assert_eq!(messages[1].parts.len(), 2);
        assert_eq!(messages[1].parts[0].part_type, "text");
        assert_eq!(messages[1].parts[0].text, Some("Analysis complete".to_string()));
        assert_eq!(messages[1].parts[1].part_type, "tool");
        assert_eq!(messages[1].parts[1].tool_name, Some("jstat".to_string()));
    }

    #[tokio::test]
    async fn test_get_session_messages_empty_for_nonexistent() {
        let pool = setup().await;
        let messages = get_session_messages(&pool, "nonexistent").await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn test_archive_session_sets_status_and_timestamp() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        archive_session(&pool, &session.id.0).await.unwrap();

        let (status, archived_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, archived_at FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "archived");
        assert!(archived_at.is_some());
    }

    #[tokio::test]
    async fn test_unarchive_session_resets_status_and_clears_timestamp() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        archive_session(&pool, &session.id.0).await.unwrap();
        unarchive_session(&pool, &session.id.0).await.unwrap();

        let (status, archived_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, archived_at FROM sessions WHERE id = ?")
                .bind(&session.id.0)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "closed");
        assert!(archived_at.is_none());
    }

    #[tokio::test]
    async fn test_delete_session_removes_row() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        delete_session(&pool, &session.id.0).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_delete_session_cascades_messages_and_parts() {
        let pool = setup().await;
        let session = create_session(&pool, "test").await.unwrap();
        let msg_id = insert_message(&pool, &session.id.0, "user", Some("hello"), Some("done"), 0).await.unwrap();
        insert_text_part(&pool, &msg_id, 0, "text content").await.unwrap();

        delete_session(&pool, &session.id.0).await.unwrap();

        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = ?")
            .bind(&session.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(msg_count, 0);

        let part_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_message_parts WHERE message_id = ?")
            .bind(&msg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(part_count, 0);
    }

    #[tokio::test]
    async fn test_list_sessions_excludes_archived() {
        let pool = setup().await;
        let s1 = create_session(&pool, "active session").await.unwrap();
        let s2 = create_session(&pool, "will be archived").await.unwrap();
        archive_session(&pool, &s2.id.0).await.unwrap();

        let rows = list_sessions(&pool, false).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, s1.id.0);
    }

    #[tokio::test]
    async fn test_list_sessions_archived_only() {
        let pool = setup().await;
        let s1 = create_session(&pool, "active session").await.unwrap();
        let s2 = create_session(&pool, "archived session").await.unwrap();
        archive_session(&pool, &s2.id.0).await.unwrap();

        let rows = list_sessions(&pool, true).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, s2.id.0);
        assert_eq!(rows[0].status, "archived");
        assert!(rows[0].archived_at.is_some());
    }
}
