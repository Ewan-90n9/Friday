use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// SSH 认证配置（用户添加环境时选定，运行时不自动降级）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SshAuth {
    /// 私钥认证。passphrase 从 OS 密钥链取（friday/env/{env_id}/secret）。
    PrivateKey { key_path: String },
    /// 密码认证。密码从 OS 密钥链取（friday/env/{env_id}/secret）。
    Password,
}

impl SshAuth {
    /// 从 DB 行的 auth_type / private_key_path 构造认证配置。
    /// 未知 auth_type 或私钥认证缺路径返回 None。
    pub fn from_row(auth_type: &str, private_key_path: Option<&str>) -> Option<Self> {
        match auth_type {
            "private_key" => Some(SshAuth::PrivateKey {
                key_path: private_key_path?.to_string(),
            }),
            "password" => Some(SshAuth::Password),
            _ => None,
        }
    }

    /// run_command 的命令包装：登录 shell（PATH 完整，jstat/jcmd 直接可用）
    pub fn wrap_login_shell(command: &str) -> String {
        format!("bash -lc {}", shell_quote_single(command))
    }
}

/// POSIX 单引号转义：'...' 内的 ' 替换为 '\''。用于把任意命令安全嵌入 bash -lc '...'。
pub fn shell_quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub struct SshTransport {
    pub env_id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// interior mutability: ExecChannel trait 方法都是 &self
    conn: Mutex<Option<SshConn>>,
}

struct SshConn {
    handle: russh::client::Handle<SshHandler>,
}

/// 加载私钥；passphrase 为 None 或空串时按无密码私钥加载
pub fn load_key_pair(
    key_path: &str,
    passphrase: Option<&str>,
) -> Result<russh::keys::key::KeyPair, Box<dyn std::error::Error + Send + Sync>> {
    let expanded = crate::infra::ssh_paths::expand_tilde(key_path);
    if !expanded.exists() {
        return Err(format!("private key not found: {}", expanded.display()).into());
    }
    let passphrase = match passphrase {
        Some(p) if !p.is_empty() => Some(p),
        _ => None,
    };
    russh::keys::load_secret_key(expanded.to_string_lossy().as_ref(), passphrase)
        .map_err(|e| format!("failed to load private key {}: {e}", expanded.display()).into())
}

struct SshHandler {
    env_id: String,
    host: String,
}

#[async_trait]
impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = format!("SHA256:{}", server_public_key.fingerprint());
        tracing::info!(
            env_id = %self.env_id,
            host = %self.host,
            fingerprint = %fingerprint,
            "accepted server host key"
        );
        Ok(true)
    }
}

impl SshTransport {
    pub fn new(env_id: &str, host: &str, port: u16, user: &str, auth: SshAuth) -> Self {
        Self {
            env_id: env_id.to_string(),
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth,
            conn: Mutex::new(None),
        }
    }

    /// Task 4 会把 is_alive 提升为 ExecChannel trait 方法。
    pub async fn is_alive(&self) -> bool {
        self.conn.lock().await.is_some()
    }

    /// 建连 + 认证（不含重试）。每次调用新建一条连接。
    async fn connect_once(
        &self,
    ) -> Result<russh::client::Handle<SshHandler>, Box<dyn std::error::Error + Send + Sync>> {
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(600)),
            ..Default::default()
        });
        let handler = SshHandler {
            env_id: self.env_id.clone(),
            host: self.host.clone(),
        };
        let mut handle = russh::client::connect(config, (self.host.as_str(), self.port), handler)
            .await
            .map_err(|e| format!("ssh connect to {}:{} failed: {e}", self.host, self.port))?;

        let authed = match &self.auth {
            SshAuth::PrivateKey { key_path } => {
                let passphrase = crate::app::credentials::load_secret(&self.env_id).await?;
                let key_pair = load_key_pair(key_path, passphrase.as_deref())?;
                handle
                    .authenticate_publickey(self.user.clone(), Arc::new(key_pair))
                    .await?
            }
            SshAuth::Password => {
                let secret = crate::app::credentials::load_secret(&self.env_id)
                    .await?
                    .ok_or("password not found in keychain")?;
                handle.authenticate_password(self.user.clone(), secret).await?
            }
        };
        if !authed {
            return Err(format!("SSH authentication failed for {}@{}", self.user, self.host).into());
        }
        Ok(handle)
    }
}

/// exec 结果是否触发重连重试：exec 报错，或 channel 未返回退出码就关闭（exit_code == -1，连接可能已死）。
/// 注意 exit_status 255 是合法退出码；-1 只是"未拿到退出码"的哨兵值。
fn should_reconnect(exit_code: i32, exec_err: Option<&str>) -> bool {
    exec_err.is_some() || exit_code == -1
}

/// 在已有 handle 上开 channel 执行命令，收集 stdout/stderr/exit_code。env_id/label 仅用于日志上下文（label 为用户原始命令）。
async fn exec_on_handle(
    handle: &russh::client::Handle<SshHandler>,
    wrapped_cmd: &str,
    env_id: &str,
    label: &str,
) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
    let mut channel = handle.channel_open_session().await?;
    channel.exec(true, wrapped_cmd).await?;

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut exit_code = -1i32;

    loop {
        let Some(msg) = channel.wait().await else { break };
        match msg {
            russh::ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
            russh::ChannelMsg::ExtendedData { ref data, .. } => stderr.extend_from_slice(data),
            russh::ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status as i32;
                let _ = channel.eof().await;
            }
            russh::ChannelMsg::ExitSignal { signal_name, core_dumped, error_message, .. } => {
                tracing::warn!(
                    env_id = %env_id,
                    command = %label,
                    signal = ?signal_name,
                    core_dumped,
                    error_message = %error_message,
                    "ssh process killed by signal"
                );
                break;
            }
            russh::ChannelMsg::Eof | russh::ChannelMsg::Close => {}
            _ => {}
        }
    }

    if exit_code == -1 {
        tracing::warn!(
            env_id = %env_id,
            command = %label,
            "ssh channel closed without exit status"
        );
    }

    Ok(ExecOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code,
    })
}

#[async_trait]
impl ExecChannel for SshTransport {
    async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        let wrapped = SshAuth::wrap_login_shell(cmd);
        let mut retried = false;
        loop {
            let result = {
                let mut conn = self.conn.lock().await;
                match conn.as_mut() {
                    Some(c) => exec_on_handle(&c.handle, &wrapped, &self.env_id, cmd).await,
                    None => return Err("ssh not connected (call connect first)".into()),
                }
            };
            let exit_code = match &result {
                Ok(output) => output.exit_code,
                Err(_) => -1,
            };
            let exec_err = result.as_ref().err().map(|e| e.to_string());
            if !should_reconnect(exit_code, exec_err.as_deref()) {
                let Ok(output) = result else { unreachable!() };
                tracing::info!(
                    env_id = %self.env_id,
                    command = %cmd,
                    exit_code = output.exit_code,
                    "ssh command executed"
                );
                return Ok(output);
            }
            if retried {
                return match result {
                    Ok(_) => {
                        tracing::warn!(
                            env_id = %self.env_id,
                            command = %cmd,
                            "ssh channel closed without exit status after reconnect"
                        );
                        Err("ssh channel closed without exit status (connection dropped)".into())
                    }
                    Err(e) => {
                        tracing::warn!(
                            env_id = %self.env_id,
                            command = %cmd,
                            error = %e,
                            "ssh command failed after reconnect"
                        );
                        Err(format!("ssh command failed after reconnect: {e}").into())
                    }
                };
            }
            retried = true;
            tracing::warn!(
                env_id = %self.env_id,
                command = %cmd,
                error = exec_err.as_deref().unwrap_or("channel closed without exit status"),
                "ssh channel broke, reconnecting once"
            );
            match self.connect_once().await {
                Ok(new_handle) => {
                    *self.conn.lock().await = Some(SshConn { handle: new_handle });
                }
                Err(reconnect_err) => {
                    tracing::warn!(
                        env_id = %self.env_id,
                        host = %self.host,
                        error = %reconnect_err,
                        "ssh reconnect failed"
                    );
                    *self.conn.lock().await = None;
                    return Err(format!("ssh reconnect failed: {reconnect_err}").into());
                }
            }
        }
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs(attempt as u64);
                tracing::warn!(env_id = %self.env_id, host = %self.host, attempt, "ssh connect retry");
                tokio::time::sleep(delay).await;
            }
            match self.connect_once().await {
                Ok(handle) => {
                    tracing::info!(env_id = %self.env_id, host = %self.host, attempt, "ssh connected");
                    *self.conn.lock().await = Some(SshConn { handle });
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        env_id = %self.env_id,
                        host = %self.host,
                        attempt,
                        error = %e,
                        "ssh connect attempt failed"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| "ssh connect failed".into()))
    }

    async fn disconnect(&self) {
        let mut conn = self.conn.lock().await;
        if let Some(c) = conn.take() {
            if let Err(e) = c
                .handle
                .disconnect(russh::Disconnect::ByApplication, "friday idle", "en")
                .await
            {
                tracing::warn!(env_id = %self.env_id, error = %e, "ssh disconnect error");
            } else {
                tracing::info!(env_id = %self.env_id, "ssh disconnected");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_auth_from_row_private_key() {
        let auth = SshAuth::from_row("private_key", Some("/home/u/.ssh/id_ed25519")).unwrap();
        match auth {
            SshAuth::PrivateKey { key_path } => {
                assert_eq!(key_path, "/home/u/.ssh/id_ed25519");
            }
            _ => panic!("expected PrivateKey"),
        }
    }

    #[test]
    fn test_ssh_auth_from_row_private_key_missing_path_is_none() {
        assert!(SshAuth::from_row("private_key", None).is_none());
    }

    #[test]
    fn test_ssh_auth_from_row_password() {
        assert!(matches!(SshAuth::from_row("password", None), Some(SshAuth::Password)));
    }

    #[test]
    fn test_ssh_auth_from_row_unknown_is_none() {
        assert!(SshAuth::from_row("kerberos", None).is_none());
    }

    #[test]
    fn test_wrap_login_shell_plain() {
        assert_eq!(SshAuth::wrap_login_shell("jstat -gcutil 1234"), "bash -lc 'jstat -gcutil 1234'");
    }

    #[test]
    fn test_wrap_login_shell_with_single_quote() {
        assert_eq!(
            SshAuth::wrap_login_shell("echo 'hi'"),
            "bash -lc 'echo '\\''hi'\\'''"
        );
    }

    #[test]
    fn test_shell_quote_single_roundtrip_via_bash_semantics() {
        let q = shell_quote_single("it's a 'test'");
        assert_eq!(q, "'it'\\''s a '\\''test'\\'''");
    }

    #[test]
    fn test_load_key_pair_nonexistent_path() {
        let err = load_key_pair("Z:/definitely/not/a/key/path", None).unwrap_err();
        assert!(err.to_string().contains("private key not found"));
    }

    #[test]
    fn test_load_key_pair_invalid_content() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("id_garbage");
        std::fs::write(&key_path, "not a valid private key").unwrap();
        let err = load_key_pair(key_path.to_str().unwrap(), None).unwrap_err();
        assert!(err.to_string().contains("failed to load private key"));
    }

    #[tokio::test]
    async fn test_new_transport_starts_disconnected() {
        let t = SshTransport::new("env1", "h", 22, "u", SshAuth::Password);
        assert!(!t.is_alive().await);
    }

    #[tokio::test]
    async fn test_run_without_connect_errors() {
        let t = SshTransport::new("env1", "h", 22, "u", SshAuth::Password);
        let err = match t.run("echo hi").await {
            Err(e) => e,
            Ok(_) => panic!("expected error when not connected"),
        };
        assert!(err.to_string().contains("ssh not connected"));
    }

    #[tokio::test]
    async fn test_disconnect_without_connection_is_noop() {
        let t = SshTransport::new("env1", "h", 22, "u", SshAuth::Password);
        t.disconnect().await;
        assert!(!t.is_alive().await);
    }

    #[test]
    fn test_should_reconnect_on_exec_error() {
        assert!(should_reconnect(-1, Some("channel open failed")));
        assert!(should_reconnect(0, Some("channel broke")));
    }

    #[test]
    fn test_should_reconnect_on_missing_exit_status() {
        assert!(should_reconnect(-1, None));
    }

    #[test]
    fn test_no_reconnect_on_zero_exit() {
        assert!(!should_reconnect(0, None));
    }

    #[test]
    fn test_no_reconnect_on_nonzero_exit() {
        assert!(!should_reconnect(1, None));
        assert!(!should_reconnect(127, None));
    }

    #[test]
    fn test_no_reconnect_on_exit_255() {
        assert!(!should_reconnect(255, None));
    }
}
