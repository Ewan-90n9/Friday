use keyring::Entry;

/// 环境密钥链条目的 service 名（Windows Credential Manager / macOS Keychain / Linux secret service）
const SERVICE: &str = "friday";

fn entry(env_id: &str) -> Result<Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, &format!("env/{env_id}/secret"))
}

/// 读取环境密钥。无条目返回 None。
pub async fn load_secret(
    env_id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let entry = entry(env_id).map_err(|e| {
        tracing::error!(?e, env_id = %env_id, "failed to create keyring entry");
        e
    })?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => {
            tracing::error!(?e, env_id = %env_id, "failed to load secret from keychain");
            Err(e.into())
        }
    }
}

/// 删除环境密钥（环境删除时级联）。无条目时静默成功。
pub async fn delete_secret(
    env_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = entry(env_id).map_err(|e| {
        tracing::error!(?e, env_id = %env_id, "failed to create keyring entry");
        e
    })?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => {
            tracing::error!(?e, env_id = %env_id, "failed to delete secret from keychain");
            Err(e.into())
        }
    }
}

/// 凭证维度条目（环境多用户）：friday/env/{env_id}/cred/{cred_id}
fn cred_entry(env_id: &str, cred_id: &str) -> Result<Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, &format!("env/{env_id}/cred/{cred_id}"))
}

/// 存储用户凭证密钥（密码或私钥 passphrase）。空值时删除条目。
pub async fn store_cred_secret(
    env_id: &str,
    cred_id: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to delete stale cred secret"),
        }
        return Ok(());
    }
    entry.set_password(value).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to store cred secret in keychain");
        e
    })?;
    tracing::info!(env_id = %env_id, cred_id = %cred_id, "cred secret stored in keychain");
    Ok(())
}

/// 读取用户凭证密钥。无条目返回 None。
pub async fn load_cred_secret(
    env_id: &str,
    cred_id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => {
            tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to load cred secret from keychain");
            Err(e.into())
        }
    }
}

/// 删除用户凭证密钥。无条目时静默成功。
pub async fn delete_cred_secret(
    env_id: &str,
    cred_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = cred_entry(env_id, cred_id).map_err(|e| {
        tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to create cred keyring entry");
        e
    })?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => {
            tracing::error!(env_id = %env_id, cred_id = %cred_id, ?e, "failed to delete cred secret from keychain");
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 keychain 往返验证：store → load → delete → load 为 None。
    /// 触碰真实 OS 凭证库（Windows Credential Manager），CI 不跑；
    /// 本地验证：`cargo test --manifest-path src-tauri/Cargo.toml test_cred_keyring_roundtrip -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn test_cred_keyring_roundtrip() {
        let env_id = "test-env-roundtrip";
        let cred_id = "test-cred-roundtrip";
        // 清理历史残留（若上次测试中断）
        let _ = delete_cred_secret(env_id, cred_id).await;

        store_cred_secret(env_id, cred_id, "s3cret-value").await.unwrap();
        assert_eq!(
            load_cred_secret(env_id, cred_id).await.unwrap().as_deref(),
            Some("s3cret-value")
        );

        delete_cred_secret(env_id, cred_id).await.unwrap();
        assert_eq!(load_cred_secret(env_id, cred_id).await.unwrap(), None);
    }
}
