use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub summary_text: String,
    pub generated_at: String,
}

pub async fn insert_summary(
    pool: &SqlitePool,
    session_id: &str,
    summary_text: &str,
    generated_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO session_summaries (session_id, summary_text, generated_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(session_id) DO UPDATE SET summary_text = excluded.summary_text, generated_at = excluded.generated_at",
    )
    .bind(session_id)
    .bind(summary_text)
    .bind(generated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_summary(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<SessionSummary>, sqlx::Error> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT session_id, summary_text, generated_at FROM session_summaries WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| SessionSummary {
        session_id: r.0,
        summary_text: r.1,
        generated_at: r.2,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;
    use crate::app::session;

    #[tokio::test]
    async fn test_insert_and_get_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();

        // Need a session first (FK constraint)
        let session = session::create_session(&pool, "test message").await.unwrap();

        insert_summary(&pool, &session.id.0, "OOM caused by thread leak", "2026-08-22T00:00:00Z")
            .await
            .unwrap();

        let fetched = get_summary(&pool, &session.id.0).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().summary_text, "OOM caused by thread leak");
    }

    #[tokio::test]
    async fn test_insert_summary_upserts_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let session = session::create_session(&pool, "test message").await.unwrap();

        insert_summary(&pool, &session.id.0, "first summary", "2026-08-22T00:00:00Z")
            .await
            .unwrap();
        insert_summary(&pool, &session.id.0, "updated summary", "2026-08-22T01:00:00Z")
            .await
            .unwrap();

        let fetched = get_summary(&pool, &session.id.0).await.unwrap().unwrap();
        assert_eq!(fetched.summary_text, "updated summary");
        assert_eq!(fetched.generated_at, "2026-08-22T01:00:00Z");
    }
}
