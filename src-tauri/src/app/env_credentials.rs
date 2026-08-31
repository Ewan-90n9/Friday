use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum EnvCredentialError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Serialize, Clone, Debug)]
pub struct EnvCredentialRow {
    pub id: String,
    pub environment_id: String,
    pub username: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    pub is_default: bool,
    pub created_at: String,
}

const CRED_COLUMNS: &str = "id, environment_id, username, auth_type, private_key_path, is_default, created_at";

fn row_to_cred(r: &sqlx::sqlite::SqliteRow) -> EnvCredentialRow {
    EnvCredentialRow {
        id: r.get("id"),
        environment_id: r.get("environment_id"),
        username: r.get("username"),
        auth_type: r.get("auth_type"),
        private_key_path: r.get("private_key_path"),
        is_default: r.get::<i64, _>("is_default") != 0,
        created_at: r.get("created_at"),
    }
}

pub async fn list_credentials(
    pool: &SqlitePool,
    environment_id: &str,
) -> Result<Vec<EnvCredentialRow>, EnvCredentialError> {
    let rows = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? \
         ORDER BY is_default DESC, created_at"
    ))
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_cred).collect())
}

pub async fn default_credential(
    pool: &SqlitePool,
    environment_id: &str,
) -> Result<Option<EnvCredentialRow>, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? AND is_default = 1"
    ))
    .bind(environment_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| row_to_cred(&r)))
}

pub async fn find_credential_by_username(
    pool: &SqlitePool,
    environment_id: &str,
    username: &str,
) -> Result<Option<EnvCredentialRow>, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE environment_id = ? AND username = ? LIMIT 1"
    ))
    .bind(environment_id)
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| row_to_cred(&r)))
}

/// 一次性迁移：为没有凭证行的环境从 environments 列 + 旧 keychain 条目生成默认凭证。
/// 幂等（已有凭证行的环境跳过）。keychain 移动先于插入：读取/移动失败的环境跳过
/// （不插入凭证行），下次启动因行不存在而重试；插入成功后才删除旧条目，删除失败
/// 仅告警并保留（无引用，无害）。由 lib.rs setup 在 db init 后调用。
pub async fn migrate_legacy(pool: &SqlitePool) {
    let envs: Vec<(String, String, String, Option<String>)> = match sqlx::query_as(
        "SELECT e.id, e.user, e.auth_type, e.private_key_path FROM environments e \
         WHERE NOT EXISTS (SELECT 1 FROM env_credentials c WHERE c.environment_id = e.id)",
    )
    .fetch_all(pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(?e, "env_credentials legacy migration query failed");
            return;
        }
    };
    for (env_id, user, auth_type, key_path) in envs {
        // 1. 先读旧密钥：读取失败无法判断是否有密钥，跳过该环境，下次启动重试
        let legacy_secret = match crate::app::credentials::load_secret(&env_id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(env_id = %env_id, ?e, "legacy secret read failed, skipping env, will retry next start");
                continue;
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        // 2. 有密钥 → 先移动到新路径 friday/env/{id}/cred/{cred_id}；
        //    失败则跳过该环境（不插行，下次启动重试），避免密钥滞留旧路径
        if let Some(secret) = &legacy_secret {
            if let Err(e) = crate::app::credentials::store_cred_secret(&env_id, &id, secret).await {
                tracing::warn!(env_id = %env_id, ?e, "keychain move failed, skipping env, will retry next start");
                continue;
            }
        }
        // 3. 密钥已就位（或本无密钥）→ 插入凭证行
        let now = chrono::Utc::now().to_rfc3339();
        let inserted = sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(&id)
        .bind(&env_id)
        .bind(&user)
        .bind(&auth_type)
        .bind(&key_path)
        .bind(&now)
        .execute(pool)
        .await;
        if let Err(e) = inserted {
            tracing::error!(env_id = %env_id, ?e, "env_credentials legacy migration insert failed");
            continue;
        }
        // 4. 移动成功且行已插入 → 删除旧条目；失败仅告警（无引用，无害）
        if legacy_secret.is_some() {
            if let Err(e) = crate::app::credentials::delete_secret(&env_id).await {
                tracing::warn!(env_id = %env_id, ?e, "legacy secret cleanup failed, orphaned legacy entry remains");
            }
        }
        tracing::info!(env_id = %env_id, cred_id = %id, "migrated legacy environment credential");
    }
}

// ── Tauri commands ──

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_env_credentials_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
) -> Result<Vec<EnvCredentialRow>, String> {
    list_credentials(&state.db, &environment_id).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    // arthas attach 用户对齐依赖该查询，保留回归覆盖
    #[tokio::test]
    async fn test_find_by_username() {
        let (_tmp, pool) = setup().await;
        let outcome = crate::app::env_save::save_environment(
            &pool, None, "e", "10.0.0.1", 22,
            vec![
                crate::app::env_save::CredentialInput {
                    id: None, username: "opc".to_string(), auth_type: "password".to_string(),
                    private_key_path: None, secret: None, is_default: true,
                },
                crate::app::env_save::CredentialInput {
                    id: None, username: "svcapp".to_string(), auth_type: "password".to_string(),
                    private_key_path: None, secret: None, is_default: false,
                },
            ],
        ).await.unwrap();
        let env_id = &outcome.environment.id;
        let found = find_credential_by_username(&pool, env_id, "svcapp").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().username, "svcapp");
        assert!(find_credential_by_username(&pool, env_id, "nobody").await.unwrap().is_none());
    }
}
