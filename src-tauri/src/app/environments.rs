use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::State;

use crate::exec::channel::ExecChannel;

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("environment not found: {0}")]
    NotFound(String),
}

#[derive(Serialize)]
pub struct EnvironmentRow {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    pub created_at: String,
}

fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn row_to_env(r: &sqlx::sqlite::SqliteRow) -> EnvironmentRow {
    EnvironmentRow {
        id: r.get("id"),
        name: r.get("name"),
        host: r.get("host"),
        port: r.get("port"),
        user: r.get("user"),
        auth_type: r.get("auth_type"),
        private_key_path: r.get("private_key_path"),
        created_at: r.get("created_at"),
    }
}

const ENV_COLUMNS: &str = "id, name, host, port, user, auth_type, private_key_path, created_at";

pub async fn add_environment(
    pool: &SqlitePool,
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    password: Option<&str>,
) -> Result<EnvironmentRow, EnvironmentError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso8601();
    sqlx::query(
        "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, private_key_path, created_at) \
         VALUES (?, ?, ?, ?, ?, 'ssh', ?, ?, ?)",
    )
    .bind(&id).bind(name).bind(host).bind(port as i64).bind(user)
    .bind(auth_type).bind(private_key_path).bind(&now)
    .execute(pool).await?;

    // 创建默认凭证行（keychain 走 env/{id}/cred/{cred_id} 新路径）；
    // keychain 失败回滚（env 行 + 凭证行一起删，保持一致）
    if let Err(e) = crate::app::env_credentials::add_credential(
        pool, &id, user, auth_type, private_key_path, password, true,
    ).await {
        tracing::error!(env_id = %id, ?e, "default credential creation failed, rolling back environment insert");
        if let Err(del_err) = sqlx::query("DELETE FROM environments WHERE id = ?")
            .bind(&id).execute(pool).await
        {
            tracing::error!(env_id = %id, ?del_err, "rollback delete failed, orphaned environment row remains");
        }
        return Err(EnvironmentError::Keychain(e.to_string()));
    }

    get_environment(pool, &id).await?.ok_or(EnvironmentError::NotFound(id.clone()))
}

pub async fn get_environment(pool: &SqlitePool, id: &str) -> Result<Option<EnvironmentRow>, EnvironmentError> {
    let row = sqlx::query(&format!("SELECT {ENV_COLUMNS} FROM environments WHERE id = ?"))
        .bind(id).fetch_optional(pool).await?;
    Ok(row.map(|r| row_to_env(&r)))
}

pub async fn find_by_name(pool: &SqlitePool, name: &str) -> Result<Option<EnvironmentRow>, EnvironmentError> {
    let row = sqlx::query(&format!("SELECT {ENV_COLUMNS} FROM environments WHERE name = ?"))
        .bind(name).fetch_optional(pool).await?;
    Ok(row.map(|r| row_to_env(&r)))
}

/// 校验环境参数。update 时 exclude_id 用于排除自身（重命名场景）。
pub async fn validate_environment(
    pool: &SqlitePool,
    name: &str,
    host: &str,
    user: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    exclude_id: Option<&str>,
) -> Result<(), EnvironmentError> {
    if name.trim().is_empty() || host.trim().is_empty() || user.trim().is_empty() {
        return Err(EnvironmentError::Validation("name/host/user 不能为空".to_string()));
    }
    if !matches!(auth_type, "private_key" | "password") {
        return Err(EnvironmentError::Validation("auth_type 必须是 private_key 或 password".to_string()));
    }
    if auth_type == "private_key" && private_key_path.map(str::trim).filter(|p| !p.is_empty()).is_none() {
        return Err(EnvironmentError::Validation("私钥认证需要填写私钥路径".to_string()));
    }
    let dup = if let Some(exclude) = exclude_id {
        sqlx::query_as::<_, (String,)>("SELECT id FROM environments WHERE name = ? AND id != ?")
            .bind(name.trim()).bind(exclude)
            .fetch_optional(pool).await?
            .is_some()
    } else {
        find_by_name(pool, name.trim()).await?.is_some()
    };
    if dup {
        return Err(EnvironmentError::Validation("同名环境已存在".to_string()));
    }
    Ok(())
}

pub async fn list_environments(pool: &SqlitePool) -> Result<Vec<EnvironmentRow>, EnvironmentError> {
    let rows = sqlx::query(&format!("SELECT {ENV_COLUMNS} FROM environments ORDER BY created_at"))
        .fetch_all(pool).await?;
    Ok(rows.iter().map(row_to_env).collect())
}

/// 编辑环境时是否需要清除密钥链条目：
/// 认证方式切换且未提供新密钥 → true（旧密钥不能跨认证模式残留）
pub fn should_clear_secret_on_update(old_auth_type: &str, new_auth_type: &str, new_secret_provided: bool) -> bool {
    old_auth_type != new_auth_type && !new_secret_provided
}

pub async fn update_environment(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    auth_type: &str,
    private_key_path: Option<&str>,
    password: Option<&str>,
) -> Result<(), EnvironmentError> {
    // 读取旧行以检测认证方式切换（切认证方式且未提供新密钥时，清除旧密钥，避免跨认证模式残留）
    let old = get_environment(pool, id).await?.ok_or(EnvironmentError::NotFound(id.to_string()))?;
    let new_secret_provided = password.map(|p| !p.is_empty()).unwrap_or(false);

    let result = sqlx::query(
        "UPDATE environments SET name = ?, host = ?, port = ?, user = ?, auth_type = ?, private_key_path = ? \
         WHERE id = ?",
    )
    .bind(name).bind(host).bind(port as i64).bind(user)
    .bind(auth_type).bind(private_key_path).bind(id)
    .execute(pool).await?;
    if result.rows_affected() == 0 {
        return Err(EnvironmentError::NotFound(id.to_string()));
    }

    // 默认凭证行同步（environments.user/auth 与默认凭证保持一致）
    match crate::app::env_credentials::default_credential(pool, id).await {
        Ok(Some(cred)) => {
            // 认证切换且未提供新密钥 → 清除该凭证密钥（旧密钥不能跨认证模式残留）
            if should_clear_secret_on_update(&old.auth_type, auth_type, new_secret_provided) {
                tracing::info!(env_id = %id, cred_id = %cred.id, "auth_type switched without new secret, clearing cred keychain entry");
                if let Err(e) = crate::app::credentials::delete_cred_secret(id, &cred.id).await {
                    tracing::warn!(env_id = %id, ?e, "failed to clear cred secret");
                }
            }
            sqlx::query("UPDATE env_credentials SET username = ?, auth_type = ?, private_key_path = ? WHERE id = ?")
                .bind(user).bind(auth_type).bind(private_key_path).bind(&cred.id)
                .execute(pool).await?;
            if let Some(secret) = password {
                if !secret.is_empty() {
                    crate::app::credentials::store_cred_secret(id, &cred.id, secret).await
                        .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
                }
            }
        }
        Ok(None) => {
            // 无凭证行（迁移未跑）：退回旧路径行为
            if should_clear_secret_on_update(&old.auth_type, auth_type, new_secret_provided) {
                tracing::info!(env_id = %id, "auth_type switched without new secret, clearing legacy keychain entry");
                crate::app::credentials::delete_secret(id).await
                    .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
            }
            if let Some(secret) = password {
                if !secret.is_empty() {
                    crate::app::credentials::store_secret(id, secret).await
                        .map_err(|e| EnvironmentError::Keychain(e.to_string()))?;
                }
            }
        }
        Err(e) => {
            tracing::warn!(env_id = %id, ?e, "default credential lookup failed, keychain not updated");
        }
    }
    Ok(())
}

pub async fn delete_environment(pool: &SqlitePool, id: &str) -> Result<(), EnvironmentError> {
    let result = sqlx::query("DELETE FROM environments WHERE id = ?").bind(id).execute(pool).await?;
    if result.rows_affected() == 0 {
        return Err(EnvironmentError::NotFound(id.to_string()));
    }
    Ok(())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn list_environments_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<EnvironmentRow>, String> {
    list_environments(&state.db).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn add_environment_cmd(
    state: State<'_, crate::AppState>,
    name: String,
    host: String,
    port: Option<u16>,
    user: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
) -> Result<EnvironmentRow, String> {
    validate_environment(
        &state.db, name.trim(), host.trim(), user.trim(), &auth_type,
        private_key_path.as_deref(), None,
    ).await.map_err(|e| e.to_string())?;
    add_environment(
        &state.db, name.trim(), host.trim(), port.unwrap_or(22), user.trim(),
        &auth_type, private_key_path.as_deref(), password.as_deref(),
    ).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn update_environment_cmd(
    state: State<'_, crate::AppState>,
    id: String,
    name: String,
    host: String,
    port: Option<u16>,
    user: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
) -> Result<(), String> {
    validate_environment(
        &state.db, name.trim(), host.trim(), user.trim(), &auth_type,
        private_key_path.as_deref(), Some(&id),
    ).await.map_err(|e| e.to_string())?;
    update_environment(
        &state.db, &id, name.trim(), host.trim(), port.unwrap_or(22), user.trim(),
        &auth_type, private_key_path.as_deref(), password.as_deref(),
    ).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn delete_environment_cmd(
    state: State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    // 断开池中连接（环境没了连接必须断）
    {
        let mut exec_pool = state.exec_pool.lock().await;
        exec_pool.disconnect(&id).await;
    }
    // 删除该环境全部凭证（keychain 条目 + DB 行）与环境级 keychain（失败仅告警，不阻塞删除）
    match crate::app::env_credentials::list_credentials(&state.db, &id).await {
        Ok(creds) => {
            for cred in creds {
                if let Err(e) = crate::app::credentials::delete_cred_secret(&id, &cred.id).await {
                    tracing::warn!(env_id = %id, cred_id = %cred.id, ?e, "failed to delete credential secret");
                }
            }
            if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?")
                .bind(&id).execute(&state.db).await
            {
                tracing::warn!(env_id = %id, ?e, "failed to delete credential rows");
            }
        }
        Err(e) => {
            tracing::warn!(env_id = %id, ?e, "failed to list credentials during environment deletion; credential keychain entries may be orphaned");
        }
    }
    if let Err(e) = crate::app::credentials::delete_secret(&id).await {
        tracing::warn!(env_id = %id, ?e, "failed to delete legacy keychain secret");
    }
    delete_environment(&state.db, &id).await.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct TestConnectionResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// 测试连接时的密钥来源决策
#[derive(Debug, PartialEq)]
pub enum TestSecret {
    /// 使用表单提供的密钥（None = 无密钥，适用于无口令私钥）
    Provided(Option<String>),
    /// 编辑已有环境且未填新密钥 → 从密钥链按环境 id 读取
    FromKeychain(String),
}

pub fn resolve_test_secret(environment_id: Option<&str>, password: Option<&str>) -> TestSecret {
    match password {
        Some(p) if !p.trim().is_empty() => TestSecret::Provided(Some(p.to_string())),
        _ => match environment_id {
            Some(id) => TestSecret::FromKeychain(id.to_string()),
            None => TestSecret::Provided(None),
        },
    }
}

/// 按表单参数测试连接（无需先保存环境）。
/// 编辑已有环境且密码留空时回退读取密钥链中已存的密钥。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn test_connection_params_cmd(
    state: State<'_, crate::AppState>,
    environment_id: Option<String>,
    host: String,
    port: Option<u16>,
    user: String,
    auth_type: String,
    private_key_path: Option<String>,
    password: Option<String>,
) -> Result<TestConnectionResult, String> {
    if host.trim().is_empty() || user.trim().is_empty() {
        return Err("主机 / 用户名不能为空".to_string());
    }
    let auth = crate::exec::ssh::SshAuth::from_row(&auth_type, private_key_path.as_deref())
        .ok_or("认证配置无效（私钥认证需要私钥路径）".to_string())?;

    let secret_override = match resolve_test_secret(environment_id.as_deref(), password.as_deref())
    {
        TestSecret::Provided(s) => s,
        TestSecret::FromKeychain(env_id) => {
            get_environment(&state.db, &env_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("环境不存在".to_string())?;
            match crate::app::env_credentials::default_credential(&state.db, &env_id).await {
                Ok(Some(cred)) => crate::app::credentials::load_cred_secret(&env_id, &cred.id)
                    .await
                    .map_err(|e| e.to_string())?
                    .or(crate::app::credentials::load_secret(&env_id).await.map_err(|e| e.to_string())?),
                Ok(None) => crate::app::credentials::load_secret(&env_id).await.map_err(|e| e.to_string())?,
                Err(e) => {
                    tracing::warn!(env_id = %env_id, ?e, "default credential lookup failed, falling back to legacy secret");
                    crate::app::credentials::load_secret(&env_id).await.map_err(|e| e.to_string())?
                }
            }
        }
    };

    // 密码认证但没有任何可用密钥 → 不发起连接，直接给出明确错误
    if matches!(auth, crate::exec::ssh::SshAuth::Password)
        && secret_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return Ok(TestConnectionResult {
            ok: false,
            latency_ms: 0,
            error: Some("密码认证需要填写密码".to_string()),
        });
    }

    let log_env_id = environment_id.clone().unwrap_or_else(|| "test-connection".to_string());
    let transport = crate::exec::ssh::SshTransport::with_secret(
        &log_env_id,
        host.trim(),
        port.unwrap_or(22),
        user.trim(),
        auth,
        secret_override.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    );
    let start = std::time::Instant::now();
    let test_future = async {
        match transport.connect().await {
            Ok(()) => match transport.run("echo friday-ok").await {
                Ok(output) if output.stdout.trim() == "friday-ok" => Ok(()),
                Ok(output) => Err(format!("unexpected echo output: {}", output.stdout.trim())),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    };
    let result = match tokio::time::timeout(std::time::Duration::from_secs(30), test_future).await {
        Ok(r) => r,
        Err(_) => Err("connection test timed out after 30s".to_string()),
    };
    transport.disconnect().await;

    Ok(TestConnectionResult {
        ok: result.is_ok(),
        latency_ms: start.elapsed().as_millis() as u64,
        error: result.err(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    #[test]
    fn test_resolve_test_secret_uses_form_password_when_provided() {
        assert_eq!(
            resolve_test_secret(None, Some("form-pass")),
            TestSecret::Provided(Some("form-pass".to_string()))
        );
        assert_eq!(
            resolve_test_secret(Some("env-1"), Some("form-pass")),
            TestSecret::Provided(Some("form-pass".to_string()))
        );
    }

    #[test]
    fn test_resolve_test_secret_edit_blank_password_falls_back_to_keychain() {
        assert_eq!(
            resolve_test_secret(Some("env-1"), None),
            TestSecret::FromKeychain("env-1".to_string())
        );
        assert_eq!(
            resolve_test_secret(Some("env-1"), Some("  ")),
            TestSecret::FromKeychain("env-1".to_string())
        );
    }

    #[test]
    fn test_resolve_test_secret_new_blank_password_is_none() {
        assert_eq!(resolve_test_secret(None, None), TestSecret::Provided(None));
        assert_eq!(resolve_test_secret(None, Some("")), TestSecret::Provided(None));
    }

    #[tokio::test]
    async fn test_add_and_list_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        assert_eq!(env.name, "prod");
        assert_eq!(env.host, "10.0.0.1");
        assert_eq!(env.auth_type, "password");

        let list = list_environments(&pool).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn test_find_by_name() {
        let (_tmp, pool) = setup().await;
        add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();

        let found = find_by_name(&pool, "prod").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().host, "10.0.0.1");

        let missing = find_by_name(&pool, "staging").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_update_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();

        update_environment(&pool, &env.id, "prod", "10.0.0.2", 2222, "opc", "private_key", Some("~/.ssh/id_ed25519"), None).await.unwrap();
        let updated = get_environment(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(updated.host, "10.0.0.2");
        assert_eq!(updated.port, 2222);
        assert_eq!(updated.auth_type, "private_key");
    }

    #[tokio::test]
    async fn test_update_nonexistent_returns_error() {
        let (_tmp, pool) = setup().await;
        let result = update_environment(&pool, "no-such", "n", "h", 22, "u", "password", None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_environment() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        delete_environment(&pool, &env.id).await.unwrap();
        let gone = get_environment(&pool, &env.id).await.unwrap();
        assert!(gone.is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_returns_not_found() {
        let (_tmp, pool) = setup().await;
        let err = delete_environment(&pool, "no-such-id").await.unwrap_err();
        assert!(matches!(err, EnvironmentError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_validate_environment_valid_password() {
        let (_tmp, pool) = setup().await;
        validate_environment(&pool, "prod", "10.0.0.1", "root", "password", None, None).await.unwrap();
    }

    #[tokio::test]
    async fn test_validate_environment_empty_name_fails() {
        let (_tmp, pool) = setup().await;
        let err = validate_environment(&pool, "  ", "10.0.0.1", "root", "password", None, None).await.unwrap_err();
        assert!(matches!(err, EnvironmentError::Validation(_)));
    }

    #[tokio::test]
    async fn test_validate_environment_bad_auth_type_fails() {
        let (_tmp, pool) = setup().await;
        let err = validate_environment(&pool, "prod", "10.0.0.1", "root", "telnet", None, None).await.unwrap_err();
        assert!(matches!(err, EnvironmentError::Validation(_)));
    }

    #[tokio::test]
    async fn test_validate_environment_private_key_without_path_fails() {
        let (_tmp, pool) = setup().await;
        let err = validate_environment(&pool, "prod", "10.0.0.1", "root", "private_key", None, None).await.unwrap_err();
        assert!(matches!(err, EnvironmentError::Validation(_)));
    }

    #[tokio::test]
    async fn test_validate_environment_private_key_with_path_passes() {
        let (_tmp, pool) = setup().await;
        validate_environment(&pool, "prod", "10.0.0.1", "root", "private_key", Some("~/.ssh/id_ed25519"), None).await.unwrap();
    }

    #[tokio::test]
    async fn test_validate_environment_duplicate_name_fails() {
        let (_tmp, pool) = setup().await;
        add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        let err = validate_environment(&pool, "prod", "10.0.0.2", "root", "password", None, None).await.unwrap_err();
        assert!(matches!(err, EnvironmentError::Validation(_)));
    }

    #[tokio::test]
    async fn test_validate_environment_rename_to_own_name_passes() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        validate_environment(&pool, "prod", "10.0.0.2", "root", "password", None, Some(&env.id)).await.unwrap();
    }

    #[test]
    fn test_should_clear_secret_when_auth_type_switches_without_new_secret() {
        assert!(should_clear_secret_on_update("password", "private_key", false));
        assert!(should_clear_secret_on_update("private_key", "password", false));
    }

    #[test]
    fn test_should_not_clear_secret_when_same_auth_type() {
        assert!(!should_clear_secret_on_update("password", "password", false));
        assert!(!should_clear_secret_on_update("private_key", "private_key", false));
    }

    #[test]
    fn test_should_not_clear_secret_when_new_secret_provided() {
        assert!(!should_clear_secret_on_update("password", "private_key", true));
        assert!(!should_clear_secret_on_update("private_key", "password", true));
    }

    #[tokio::test]
    async fn test_validate_environment_rename_to_other_env_name_fails() {
        let (_tmp, pool) = setup().await;
        let a = add_environment(&pool, "a", "10.0.0.1", 22, "root", "password", None, None).await.unwrap();
        add_environment(&pool, "b", "10.0.0.2", 22, "root", "password", None, None).await.unwrap();
        let err = validate_environment(&pool, "b", "10.0.0.1", "root", "password", None, Some(&a.id)).await.unwrap_err();
        assert!(matches!(err, EnvironmentError::Validation(_)));
    }

    #[tokio::test]
    async fn test_add_environment_creates_default_credential_row() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "opc", "password", None, None).await.unwrap();
        let cred = crate::app::env_credentials::default_credential(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(cred.username, "opc");
        assert_eq!(cred.auth_type, "password");
    }

    #[tokio::test]
    async fn test_update_environment_syncs_default_credential() {
        let (_tmp, pool) = setup().await;
        let env = add_environment(&pool, "prod", "10.0.0.1", 22, "opc", "password", None, None).await.unwrap();
        update_environment(&pool, &env.id, "prod", "10.0.0.1", 22, "deploy", "private_key", Some("~/.ssh/deploy"), None).await.unwrap();
        let cred = crate::app::env_credentials::default_credential(&pool, &env.id).await.unwrap().unwrap();
        assert_eq!(cred.username, "deploy");
        assert_eq!(cred.auth_type, "private_key");
        assert_eq!(cred.private_key_path.as_deref(), Some("~/.ssh/deploy"));
        // 额外凭证不受影响
        crate::app::env_credentials::add_credential(&pool, &env.id, "svcapp", "password", None, None, false).await.unwrap();
        update_environment(&pool, &env.id, "prod2", "10.0.0.1", 22, "deploy", "private_key", Some("~/.ssh/deploy"), None).await.unwrap();
        assert!(crate::app::env_credentials::find_credential_by_username(&pool, &env.id, "svcapp").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_migrate_legacy_creates_default_credential() {
        let (_tmp, pool) = setup().await;
        // 直接走 SQL 模拟一个未迁移的旧环境（绕过新的 add_environment）
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, created_at) \
             VALUES ('old-1', 'old', '10.0.0.1', 22, 'opc', 'ssh', 'password', '2026-01-01T00:00:00Z')",
        ).execute(&pool).await.unwrap();

        crate::app::env_credentials::migrate_legacy(&pool).await;
        let cred = crate::app::env_credentials::default_credential(&pool, "old-1").await.unwrap().unwrap();
        assert_eq!(cred.username, "opc");

        // 幂等：再跑一次不重复插入
        crate::app::env_credentials::migrate_legacy(&pool).await;
        let all = crate::app::env_credentials::list_credentials(&pool, "old-1").await.unwrap();
        assert_eq!(all.len(), 1);
    }
}
