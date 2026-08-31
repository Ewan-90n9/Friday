use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum EnvCredentialError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("credential not found: {0}")]
    NotFound(String),
    #[error("keychain error: {0}")]
    Keychain(String),
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

pub async fn add_credential(
    pool: &SqlitePool,
    environment_id: &str,
    username: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    secret: Option<&str>,
    make_default: bool,
) -> Result<EnvCredentialRow, EnvCredentialError> {
    if username.trim().is_empty() {
        return Err(EnvCredentialError::Validation("username 不能为空".to_string()));
    }
    if !matches!(auth_type, "private_key" | "password") {
        return Err(EnvCredentialError::Validation(
            "auth_type 必须是 private_key 或 password".to_string(),
        ));
    }
    if auth_type == "private_key"
        && private_key_path.map(str::trim).filter(|p| !p.is_empty()).is_none()
    {
        return Err(EnvCredentialError::Validation("私钥认证需要填写私钥路径".to_string()));
    }
    if find_credential_by_username(pool, environment_id, username.trim()).await?.is_some() {
        return Err(EnvCredentialError::Validation(format!("用户 {username} 的凭证已存在")));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    if make_default {
        sqlx::query("UPDATE env_credentials SET is_default = 0 WHERE environment_id = ?")
            .bind(environment_id)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(environment_id)
    .bind(username.trim())
    .bind(auth_type)
    .bind(private_key_path)
    .bind(if make_default { 1 } else { 0 })
    .bind(&now)
    .execute(pool)
    .await?;

    // keychain 写入失败 → 回滚凭证行（DB 与 keychain 保持一致）
    if let Some(secret) = secret {
        if !secret.is_empty() {
            if let Err(e) = crate::app::credentials::store_cred_secret(environment_id, &id, secret).await {
                tracing::error!(environment_id, cred_id = %id, ?e, "keychain store failed, rolling back credential insert");
                if let Err(del_err) = sqlx::query("DELETE FROM env_credentials WHERE id = ?")
                    .bind(&id)
                    .execute(pool)
                    .await
                {
                    tracing::error!(environment_id, cred_id = %id, ?del_err, "rollback delete failed, orphaned credential row remains");
                }
                return Err(EnvCredentialError::Keychain(e.to_string()));
            }
        }
    }
    if make_default {
        // 与 set_default_credential 一致：镜像 user/auth_type/private_key_path 三列
        sqlx::query("UPDATE environments SET user = ?, auth_type = ?, private_key_path = ? WHERE id = ?")
            .bind(username.trim())
            .bind(auth_type)
            .bind(private_key_path)
            .bind(environment_id)
            .execute(pool)
            .await?;
    }

    let row = sqlx::query(&format!("SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ?"))
        .bind(&id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| EnvCredentialError::NotFound(id.clone()))?;
    Ok(row_to_cred(&row))
}

pub async fn delete_credential(
    pool: &SqlitePool,
    environment_id: &str,
    cred_id: &str,
) -> Result<(), EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ? AND environment_id = ?"
    ))
    .bind(cred_id)
    .bind(environment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| EnvCredentialError::NotFound(cred_id.to_string()))?;
    let cred = row_to_cred(&row);
    if cred.is_default {
        return Err(EnvCredentialError::Validation(
            "不能删除默认凭证；请先把其他凭证设为默认".to_string(),
        ));
    }
    sqlx::query("DELETE FROM env_credentials WHERE id = ? AND environment_id = ?")
        .bind(cred_id)
        .bind(environment_id)
        .execute(pool)
        .await?;
    if let Err(e) = crate::app::credentials::delete_cred_secret(environment_id, cred_id).await {
        // DB 已删，keychain 残留仅告警（无引用条目无害）
        tracing::warn!(environment_id, cred_id, ?e, "failed to delete credential keychain entry");
    }
    Ok(())
}

pub async fn set_default_credential(
    pool: &SqlitePool,
    environment_id: &str,
    cred_id: &str,
) -> Result<EnvCredentialRow, EnvCredentialError> {
    let row = sqlx::query(&format!(
        "SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ? AND environment_id = ?"
    ))
    .bind(cred_id)
    .bind(environment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| EnvCredentialError::NotFound(cred_id.to_string()))?;
    let cred = row_to_cred(&row);
    sqlx::query("UPDATE env_credentials SET is_default = 0 WHERE environment_id = ?")
        .bind(environment_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE env_credentials SET is_default = 1 WHERE id = ?")
        .bind(cred_id)
        .execute(pool)
        .await?;
    // environments 行镜像默认凭证（user/auth_type/private_key_path），旧路径消费者保持一致
    sqlx::query("UPDATE environments SET user = ?, auth_type = ?, private_key_path = ? WHERE id = ?")
        .bind(&cred.username)
        .bind(&cred.auth_type)
        .bind(&cred.private_key_path)
        .bind(environment_id)
        .execute(pool)
        .await?;
    // 重新读取行，返回新默认状态（同 add_credential 模式）
    let fresh = sqlx::query(&format!("SELECT {CRED_COLUMNS} FROM env_credentials WHERE id = ?"))
        .bind(cred_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| EnvCredentialError::NotFound(cred_id.to_string()))?;
    Ok(row_to_cred(&fresh))
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

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn add_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    username: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
    make_default: Option<bool>,
) -> Result<EnvCredentialRow, String> {
    add_credential(
        &state.db,
        &environment_id,
        username.trim(),
        &auth_type,
        private_key_path.as_deref(),
        password.as_deref(),
        make_default.unwrap_or(false),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    credential_id: String,
) -> Result<(), String> {
    delete_credential(&state.db, &environment_id, &credential_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_default_env_credential_cmd(
    state: tauri::State<'_, crate::AppState>,
    environment_id: String,
    credential_id: String,
) -> Result<EnvCredentialRow, String> {
    set_default_credential(&state.db, &environment_id, &credential_id)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    async fn add_env(pool: &SqlitePool, id: &str, user: &str) {
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES (?, 'e', '10.0.0.1', 22, ?, 'ssh', 'password', '2026-01-01T00:00:00Z')",
        )
        .bind(id).bind(user).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn test_add_list_and_default() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        let first = add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        assert!(first.is_default);
        let second = add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        assert!(!second.is_default);

        let list = list_credentials(&pool, "env-1").await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].is_default); // default 排前

        let def = default_credential(&pool, "env-1").await.unwrap().unwrap();
        assert_eq!(def.username, "opc");
    }

    #[tokio::test]
    async fn test_add_duplicate_username_rejected() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let err = add_credential(&pool, "env-1", "opc", "password", None, None, false).await.unwrap_err();
        assert!(matches!(err, EnvCredentialError::Validation(_)));
    }

    #[tokio::test]
    async fn test_find_by_username() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        let found = find_credential_by_username(&pool, "env-1", "svcapp").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().username, "svcapp");
        assert!(find_credential_by_username(&pool, "env-1", "nobody").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_delete_default_rejected() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        let cred = add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let err = delete_credential(&pool, "env-1", &cred.id).await.unwrap_err();
        assert!(matches!(err, EnvCredentialError::Validation(_)));
    }

    #[tokio::test]
    async fn test_delete_non_default_ok() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let extra = add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        delete_credential(&pool, "env-1", &extra.id).await.unwrap();
        assert_eq!(list_credentials(&pool, "env-1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_set_default_syncs_environments_user() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let svc = add_credential(&pool, "env-1", "svcapp", "private_key", Some("~/.ssh/svc"), None, false).await.unwrap();

        set_default_credential(&pool, "env-1", &svc.id).await.unwrap();
        let def = default_credential(&pool, "env-1").await.unwrap().unwrap();
        assert_eq!(def.username, "svcapp");
        // environments.user 镜像默认凭证用户名
        let (user,): (String,) = sqlx::query_as("SELECT user FROM environments WHERE id = 'env-1'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(user, "svcapp");
        let (auth, key_path): (String, Option<String>) =
            sqlx::query_as("SELECT auth_type, private_key_path FROM environments WHERE id = 'env-1'")
                .fetch_one(&pool).await.unwrap();
        assert_eq!(auth, "private_key");
        assert_eq!(key_path.as_deref(), Some("~/.ssh/svc"));
    }

    #[tokio::test]
    async fn test_set_default_returns_fresh_row() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        let svc = add_credential(&pool, "env-1", "svcapp", "password", None, None, false).await.unwrap();
        assert!(!svc.is_default);
        let updated = set_default_credential(&pool, "env-1", &svc.id).await.unwrap();
        assert!(updated.is_default, "returned row must reflect the new default state");
        assert_eq!(updated.username, "svcapp");
    }

    #[tokio::test]
    async fn test_add_default_credential_mirrors_auth_columns() {
        let (_tmp, pool) = setup().await;
        add_env(&pool, "env-1", "opc").await;
        add_credential(&pool, "env-1", "opc", "password", None, None, true).await.unwrap();
        // 新默认凭证是 private_key：environments 行必须镜像 auth_type + private_key_path
        add_credential(&pool, "env-1", "svcapp", "private_key", Some("~/.ssh/svc"), None, true).await.unwrap();
        let (auth, key_path): (String, Option<String>) =
            sqlx::query_as("SELECT auth_type, private_key_path FROM environments WHERE id = 'env-1'")
                .fetch_one(&pool).await.unwrap();
        assert_eq!(auth, "private_key");
        assert_eq!(key_path.as_deref(), Some("~/.ssh/svc"));
        // 只剩一个默认
        let defaults: Vec<EnvCredentialRow> = list_credentials(&pool, "env-1").await.unwrap()
            .into_iter().filter(|c| c.is_default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].username, "svcapp");
    }
}
