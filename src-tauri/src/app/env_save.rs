use serde::Deserialize;
use sqlx::SqlitePool;

use crate::app::env_credentials::EnvCredentialRow;

/// save_environment 入参：单条凭证（全量列表中的一项）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInput {
    /// None = 新增的凭证
    pub id: Option<String>,
    pub username: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    /// None/空 = 不修改已有 secret（新凭证时表示无口令私钥/无密码由校验兜底）
    pub secret: Option<String>,
    /// 恰好一条为 true
    pub is_default: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("environment not found: {0}")]
    NotFound(String),
}

/// 保存结果：环境行 + 全量凭证列表
#[derive(Debug)]
pub struct SaveOutcome {
    pub environment: crate::app::environments::EnvironmentRow,
    pub credentials: Vec<EnvCredentialRow>,
}

/// 校验凭证列表：至少 1 条、恰好 1 个默认、用户名非空不重复、认证合法、私钥必有路径
pub fn validate_credentials(creds: &[CredentialInput]) -> Result<(), SaveError> {
    if creds.is_empty() {
        return Err(SaveError::Validation("至少需要一条凭证".to_string()));
    }
    let defaults = creds.iter().filter(|c| c.is_default).count();
    if defaults != 1 {
        return Err(SaveError::Validation(format!(
            "必须恰好指定一个默认凭证（当前 {defaults} 个）"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for c in creds {
        if c.username.trim().is_empty() {
            return Err(SaveError::Validation("凭证用户名不能为空".to_string()));
        }
        if !seen.insert(c.username.trim().to_string()) {
            return Err(SaveError::Validation(format!(
                "凭证用户名重复：{}",
                c.username.trim()
            )));
        }
        if !matches!(c.auth_type.as_str(), "private_key" | "password") {
            return Err(SaveError::Validation(
                "auth_type 必须是 private_key 或 password".to_string(),
            ));
        }
        if c.auth_type == "private_key"
            && c.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()).is_none()
        {
            return Err(SaveError::Validation("私钥认证需要填写私钥路径".to_string()));
        }
    }
    Ok(())
}

impl From<crate::app::env_credentials::EnvCredentialError> for SaveError {
    fn from(e: crate::app::env_credentials::EnvCredentialError) -> Self {
        use crate::app::env_credentials::EnvCredentialError as E;
        match e {
            E::Db(e) => SaveError::Db(e),
            E::Validation(s) => SaveError::Validation(s),
            E::NotFound(s) => SaveError::NotFound(s),
            E::Keychain(s) => SaveError::Keychain(s),
        }
    }
}

impl From<crate::app::environments::EnvironmentError> for SaveError {
    fn from(e: crate::app::environments::EnvironmentError) -> Self {
        use crate::app::environments::EnvironmentError as E;
        match e {
            E::Db(e) => SaveError::Db(e),
            E::Validation(s) => SaveError::Validation(s),
            E::NotFound(s) => SaveError::NotFound(s),
            E::Keychain(s) => SaveError::Keychain(s),
        }
    }
}

/// keychain 操作（事务提交后执行；owned 避免 clone 生命周期问题）
#[derive(Clone)]
enum KeychainOp {
    Write { cred_id: String, secret: String },
    Delete { cred_id: String },
}

/// 私钥路径绑定规则：private_key 时取 trim 后非空的路径，否则 None
fn bound_key_path<'a>(auth_type: &str, path: Option<&'a str>) -> Option<&'a str> {
    if auth_type == "private_key" {
        path.map(str::trim).filter(|p| !p.is_empty())
    } else {
        None
    }
}

/// 原子保存环境 + 全量凭证（新增 + 编辑统一入口）
pub async fn save_environment(
    pool: &SqlitePool,
    environment_id: Option<&str>,
    name: &str,
    host: &str,
    port: u16,
    credentials: Vec<CredentialInput>,
) -> Result<SaveOutcome, SaveError> {
    // 环境级校验：名称/host 非空
    if name.trim().is_empty() || host.trim().is_empty() {
        return Err(SaveError::Validation("名称 / 主机不能为空".to_string()));
    }
    validate_credentials(&credentials)?;
    // 名称查重（编辑时排除自身）
    let dup = match environment_id {
        Some(id) => {
            sqlx::query_as::<_, (String,)>("SELECT id FROM environments WHERE name = ? AND id != ?")
                .bind(name.trim())
                .bind(id)
                .fetch_optional(pool)
                .await?
                .is_some()
        }
        None => {
            sqlx::query_as::<_, (String,)>("SELECT id FROM environments WHERE name = ?")
                .bind(name.trim())
                .fetch_optional(pool)
                .await?
                .is_some()
        }
    };
    if dup {
        return Err(SaveError::Validation("同名环境已存在".to_string()));
    }

    // 现有凭证行（编辑 diff 用；新增路径为空集）
    let existing: Vec<EnvCredentialRow> = match environment_id {
        Some(id) => crate::app::env_credentials::list_credentials(pool, id).await?,
        None => Vec::new(),
    };

    // ── diff ──
    let input_ids: std::collections::HashSet<&str> = credentials
        .iter()
        .filter_map(|c| c.id.as_deref())
        .collect();
    let to_delete: Vec<&EnvCredentialRow> = existing
        .iter()
        .filter(|e| !input_ids.contains(e.id.as_str()))
        .collect();

    // keychain 操作清单（事务提交后执行）
    let mut keychain_ops: Vec<KeychainOp> = Vec::new();

    let env_id = match environment_id {
        Some(id) => id.to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let now = chrono::Utc::now().to_rfc3339();

    // ── DB 事务 ──
    let mut tx = pool.begin().await?;
    if environment_id.is_none() {
        // 默认凭证镜像进 environments 行
        let def = credentials.iter().find(|c| c.is_default).unwrap();
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, private_key_path, created_at) \
             VALUES (?, ?, ?, ?, ?, 'ssh', ?, ?, ?)",
        )
        .bind(&env_id)
        .bind(name.trim())
        .bind(host.trim())
        .bind(port as i64)
        .bind(def.username.trim())
        .bind(&def.auth_type)
        .bind(bound_key_path(&def.auth_type, def.private_key_path.as_deref()))
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    } else {
        // 编辑：环境行基本信息 + 默认凭证镜像
        let def = credentials.iter().find(|c| c.is_default).unwrap();
        let updated = sqlx::query(
            "UPDATE environments SET name = ?, host = ?, port = ?, user = ?, auth_type = ?, private_key_path = ? WHERE id = ?",
        )
        .bind(name.trim())
        .bind(host.trim())
        .bind(port as i64)
        .bind(def.username.trim())
        .bind(&def.auth_type)
        .bind(bound_key_path(&def.auth_type, def.private_key_path.as_deref()))
        .bind(&env_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(SaveError::NotFound(env_id.clone()));
        }
    }

    // 删除集：DB 行 + keychain 条目
    for cred in &to_delete {
        sqlx::query("DELETE FROM env_credentials WHERE id = ?")
            .bind(&cred.id)
            .execute(&mut *tx)
            .await?;
        keychain_ops.push(KeychainOp::Delete {
            cred_id: cred.id.clone(),
        });
    }

    for input in &credentials {
        let secret_provided = input
            .secret
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        match input.id.as_deref() {
            None => {
                // 新增凭证
                let cred_id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&cred_id)
                .bind(&env_id)
                .bind(input.username.trim())
                .bind(&input.auth_type)
                .bind(bound_key_path(&input.auth_type, input.private_key_path.as_deref()))
                .bind(if input.is_default { 1 } else { 0 })
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                if secret_provided {
                    keychain_ops.push(KeychainOp::Write {
                        cred_id,
                        secret: input.secret.as_deref().unwrap().trim().to_string(),
                    });
                }
            }
            Some(cred_id) => {
                // 更新凭证（认证切换且未提供新 secret → 清旧条目）
                let old = existing.iter().find(|e| e.id == cred_id);
                sqlx::query(
                    "UPDATE env_credentials SET username = ?, auth_type = ?, private_key_path = ?, is_default = ? WHERE id = ? AND environment_id = ?",
                )
                .bind(input.username.trim())
                .bind(&input.auth_type)
                .bind(bound_key_path(&input.auth_type, input.private_key_path.as_deref()))
                .bind(if input.is_default { 1 } else { 0 })
                .bind(cred_id)
                .bind(&env_id)
                .execute(&mut *tx)
                .await?;
                if secret_provided {
                    keychain_ops.push(KeychainOp::Write {
                        cred_id: cred_id.to_string(),
                        secret: input.secret.as_deref().unwrap().trim().to_string(),
                    });
                } else if let Some(old) = old {
                    // 认证方式切换且未提供新 secret → 清旧条目（旧密钥不能跨认证模式残留）
                    if crate::app::environments::should_clear_secret_on_update(
                        &old.auth_type,
                        &input.auth_type,
                        false,
                    ) {
                        keychain_ops.push(KeychainOp::Delete {
                            cred_id: cred_id.to_string(),
                        });
                    }
                }
            }
        }
    }

    tx.commit().await?;

    // ── keychain（事务提交后；失败补偿回滚 DB）──
    let mut done: Vec<KeychainOp> = Vec::new();
    for op in keychain_ops {
        let result = match &op {
            KeychainOp::Write { cred_id, secret } => {
                crate::app::credentials::store_cred_secret(&env_id, cred_id, secret).await
            }
            KeychainOp::Delete { cred_id } => {
                crate::app::credentials::delete_cred_secret(&env_id, cred_id).await
            }
        };
        if let Err(e) = result {
            tracing::error!(env_id = %env_id, ?e, "keychain op failed, rolling back save");
            // 补偿：删除本次已写条目
            for d in &done {
                if let KeychainOp::Write { cred_id, .. } = d {
                    let _ = crate::app::credentials::delete_cred_secret(&env_id, cred_id).await;
                }
            }
            rollback_saved_state(pool, &env_id, environment_id.is_none(), &existing).await;
            return Err(SaveError::Keychain(e.to_string()));
        }
        done.push(op);
    }

    let environment = crate::app::environments::get_environment(pool, &env_id)
        .await?
        .ok_or(SaveError::NotFound(env_id.clone()))?;
    let saved_creds = crate::app::env_credentials::list_credentials(pool, &env_id).await?;
    Ok(SaveOutcome {
        environment,
        credentials: saved_creds,
    })
}

/// keychain 失败后的 DB 补偿：新增路径删环境；编辑路径还原旧凭证全量。
/// 简化策略：编辑路径凭证行还原旧全量；环境行 name/host/port 残留为已保存值可接受——
/// keychain 失败极罕见，且 UI 报错后用户重试会用同一表单值覆盖。
async fn rollback_saved_state(
    pool: &SqlitePool,
    env_id: &str,
    is_new: bool,
    old_creds: &[EnvCredentialRow],
) {
    if is_new {
        if let Err(e) = sqlx::query("DELETE FROM environments WHERE id = ?")
            .bind(env_id)
            .execute(pool)
            .await
        {
            tracing::error!(env_id = %env_id, ?e, "rollback env delete failed, orphaned row remains");
        }
        if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?")
            .bind(env_id)
            .execute(pool)
            .await
        {
            tracing::error!(env_id = %env_id, ?e, "rollback cred delete failed, orphaned rows remain");
        }
        return;
    }
    if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?")
        .bind(env_id)
        .execute(pool)
        .await
    {
        tracing::error!(env_id = %env_id, ?e, "rollback cred delete failed");
    }
    for c in old_creds {
        if let Err(e) = sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&c.id)
        .bind(env_id)
        .bind(&c.username)
        .bind(&c.auth_type)
        .bind(&c.private_key_path)
        .bind(if c.is_default { 1 } else { 0 })
        .bind(&c.created_at)
        .execute(pool)
        .await
        {
            tracing::error!(env_id = %env_id, cred_id = %c.id, ?e, "rollback cred re-insert failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    fn cred(username: &str, is_default: bool) -> CredentialInput {
        CredentialInput {
            id: None,
            username: username.to_string(),
            auth_type: "password".to_string(),
            private_key_path: None,
            secret: Some("pass-1".to_string()),
            is_default,
        }
    }

    #[tokio::test]
    async fn test_save_new_environment_with_multiple_credentials() {
        let (_tmp, pool) = setup().await;
        let outcome = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("opc", true), cred("svcapp", false)],
        ).await.unwrap();

        assert_eq!(outcome.environment.name, "prod");
        assert_eq!(outcome.credentials.len(), 2);
        let def = outcome.credentials.iter().find(|c| c.is_default).unwrap();
        assert_eq!(def.username, "opc");
        // environments 三列镜像默认凭证
        assert_eq!(outcome.environment.user, "opc");
        assert_eq!(outcome.environment.auth_type, "password");
    }

    #[tokio::test]
    async fn test_save_validates_duplicate_username() {
        let (_tmp, pool) = setup().await;
        let err = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("opc", true), cred("opc", false)],
        ).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));
    }

    #[tokio::test]
    async fn test_save_validates_zero_and_multi_default() {
        let (_tmp, pool) = setup().await;
        let err = save_environment(&pool, None, "prod", "10.0.0.1", 22, vec![]).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));

        let err = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("a", true), cred("b", true)],
        ).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));
    }
}
