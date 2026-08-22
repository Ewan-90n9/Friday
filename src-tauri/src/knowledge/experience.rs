use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Outcome {
    Positive,
    Negative,
    Uncertain,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Positive => "positive",
            Outcome::Negative => "negative",
            Outcome::Uncertain => "uncertain",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "positive" => Outcome::Positive,
            "negative" => Outcome::Negative,
            _ => Outcome::Uncertain,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experience {
    pub id: String,
    pub symptom: String,
    pub service: String,
    pub language: String,
    pub root_cause: Option<String>,
    pub investigation_path: String,
    pub experience_lesson: String,
    pub outcome: Outcome,
    pub occurrence_count: i64,
    pub last_seen_at: String,
    pub created_at: String,
    pub query_text: String,
}

use sqlx::SqlitePool;

pub async fn insert_experience(
    pool: &SqlitePool,
    exp: &Experience,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO experiences \
         (id, symptom, service, language, root_cause, investigation_path, \
          experience_lesson, outcome, occurrence_count, last_seen_at, created_at, query_text) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&exp.id)
    .bind(&exp.symptom)
    .bind(&exp.service)
    .bind(&exp.language)
    .bind(&exp.root_cause)
    .bind(&exp.investigation_path)
    .bind(&exp.experience_lesson)
    .bind(exp.outcome.as_str())
    .bind(exp.occurrence_count)
    .bind(&exp.last_seen_at)
    .bind(&exp.created_at)
    .bind(&exp.query_text)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_experience(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(
        String, String, String, String, Option<String>,
        String, String, String, i64, String, String, String,
    )> = sqlx::query_as(
        "SELECT id, symptom, service, language, root_cause, \
         investigation_path, experience_lesson, outcome, occurrence_count, \
         last_seen_at, created_at, query_text \
         FROM experiences WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Experience {
        id: r.0,
        symptom: r.1,
        service: r.2,
        language: r.3,
        root_cause: r.4,
        investigation_path: r.5,
        experience_lesson: r.6,
        outcome: Outcome::from_str(&r.7),
        occurrence_count: r.8,
        last_seen_at: r.9,
        created_at: r.10,
        query_text: r.11,
    }))
}

pub async fn update_experience_increment(
    pool: &SqlitePool,
    id: &str,
    new_investigation_path: &str,
    new_lesson: &str,
    last_seen_at: &str,
) -> Result<(), sqlx::Error> {
    let existing = get_experience(pool, id).await?;
    if let Some(exp) = existing {
        let combined_path = if exp.investigation_path.is_empty() {
            new_investigation_path.to_string()
        } else if !exp.investigation_path.contains(new_investigation_path) {
            format!("{}. {}", exp.investigation_path, new_investigation_path)
        } else {
            exp.investigation_path
        };
        let combined_lesson = if exp.experience_lesson.is_empty() {
            new_lesson.to_string()
        } else if !exp.experience_lesson.contains(new_lesson) {
            format!("{}. {}", exp.experience_lesson, new_lesson)
        } else {
            exp.experience_lesson
        };
        sqlx::query(
            "UPDATE experiences SET investigation_path = ?, experience_lesson = ?, \
             occurrence_count = ?, last_seen_at = ? WHERE id = ?",
        )
        .bind(&combined_path)
        .bind(&combined_lesson)
        .bind(exp.occurrence_count + 1)
        .bind(last_seen_at)
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn find_by_fields(
    pool: &SqlitePool,
    symptom: &str,
    language: &str,
    service: &str,
    root_cause: Option<&str>,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences \
         WHERE symptom = ? AND language = ? AND service = ? AND root_cause = ? \
         AND outcome = 'positive' LIMIT 1",
    )
    .bind(symptom)
    .bind(language)
    .bind(service)
    .bind(root_cause)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = row {
        return get_experience(pool, &id).await;
    }
    Ok(None)
}

pub async fn find_negative_by_fields(
    pool: &SqlitePool,
    symptom: &str,
    language: &str,
    service: &str,
) -> Result<Option<Experience>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences \
         WHERE symptom = ? AND language = ? AND service = ? AND root_cause IS NULL \
         AND outcome = 'negative' ORDER BY last_seen_at DESC LIMIT 1",
    )
    .bind(symptom)
    .bind(language)
    .bind(service)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = row {
        return get_experience(pool, &id).await;
    }
    Ok(None)
}

pub async fn replace_experience(
    pool: &SqlitePool,
    id: &str,
    exp: &Experience,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE experiences SET symptom = ?, service = ?, language = ?, \
         root_cause = ?, investigation_path = ?, experience_lesson = ?, \
         outcome = ?, last_seen_at = ? WHERE id = ?",
    )
    .bind(&exp.symptom)
    .bind(&exp.service)
    .bind(&exp.language)
    .bind(&exp.root_cause)
    .bind(&exp.investigation_path)
    .bind(&exp.experience_lesson)
    .bind(exp.outcome.as_str())
    .bind(&exp.last_seen_at)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outcome_round_trip() {
        for outcome in [Outcome::Positive, Outcome::Negative, Outcome::Uncertain] {
            let s = outcome.as_str();
            assert_eq!(Outcome::from_str(s), outcome);
        }
    }

    #[test]
    fn test_outcome_from_str_unknown_defaults_uncertain() {
        assert_eq!(Outcome::from_str("garbage"), Outcome::Uncertain);
    }

    use crate::infra::db;

    fn make_test_experience() -> Experience {
        Experience {
            id: uuid::Uuid::new_v4().to_string(),
            symptom: "OOM".to_string(),
            service: "OrderService".to_string(),
            language: "java".to_string(),
            root_cause: Some("ThreadPool leak".to_string()),
            investigation_path: "jstat -> arthas thread".to_string(),
            experience_lesson: "Check thread count first".to_string(),
            outcome: Outcome::Positive,
            occurrence_count: 1,
            last_seen_at: "2026-08-22T00:00:00Z".to_string(),
            created_at: "2026-08-22T00:00:00Z".to_string(),
            query_text: "OrderService OOM".to_string(),
        }
    }

    #[tokio::test]
    async fn test_insert_and_get_experience() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();

        insert_experience(&pool, &exp).await.unwrap();

        let fetched = get_experience(&pool, &exp.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.symptom, "OOM");
        assert_eq!(fetched.service, "OrderService");
        assert_eq!(fetched.outcome, Outcome::Positive);
        assert_eq!(fetched.root_cause.as_deref(), Some("ThreadPool leak"));
    }

    #[tokio::test]
    async fn test_update_experience_increment() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        update_experience_increment(
            &pool,
            &exp.id,
            "jstat -> arthas thread -> jmap dump",
            "Check thread count first. Also check heap dump.",
            "2026-08-23T00:00:00Z",
        )
        .await
        .unwrap();

        let fetched = get_experience(&pool, &exp.id).await.unwrap().unwrap();
        assert_eq!(fetched.occurrence_count, 2);
        assert_eq!(fetched.last_seen_at, "2026-08-23T00:00:00Z");
        assert!(fetched.investigation_path.contains("jmap dump"));
        assert!(fetched.experience_lesson.contains("heap dump"));
    }

    #[tokio::test]
    async fn test_find_by_fields_positive_match() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_by_fields(
            &pool,
            "OOM",
            "java",
            "OrderService",
            Some("ThreadPool leak"),
        )
        .await
        .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exp.id);
    }

    #[tokio::test]
    async fn test_find_by_fields_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = make_test_experience();
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_by_fields(
            &pool,
            "OOM",
            "java",
            "OrderService",
            Some("Different root cause"),
        )
        .await
        .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_find_negative_by_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let exp = Experience {
            root_cause: None,
            outcome: Outcome::Negative,
            ..make_test_experience()
        };
        insert_experience(&pool, &exp).await.unwrap();

        let found = find_negative_by_fields(&pool, "OOM", "java", "OrderService")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exp.id);
    }
}
