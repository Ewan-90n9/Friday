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
    /// 密钥直供（测试连接等未落库场景）；None 时从 OS 密钥链按 env_id 读取
    secret_override: Option<String>,
    /// interior mutability: ExecChannel trait 方法都是 &self
    conn: Mutex<Option<SshConn>>,
}

struct SshConn {
    handle: russh::client::Handle<SshHandler>,
}

/// 解析实际使用的密钥：非空 override 优先，否则回退密钥链结果
fn effective_secret(secret_override: &Option<String>, keychain: Option<String>) -> Option<String> {
    match secret_override {
        Some(s) if !s.is_empty() => Some(s.clone()),
        _ => keychain,
    }
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
            secret_override: None,
            conn: Mutex::new(None),
        }
    }

    /// 带直供密钥的构造器（测试连接未保存的环境时使用，不读密钥链）
    pub fn with_secret(
        env_id: &str,
        host: &str,
        port: u16,
        user: &str,
        auth: SshAuth,
        secret: Option<String>,
    ) -> Self {
        Self {
            secret_override: secret,
            ..Self::new(env_id, host, port, user, auth)
        }
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
                let keychain = crate::app::credentials::load_secret(&self.env_id).await?;
                let passphrase = effective_secret(&self.secret_override, keychain);
                let key_pair = load_key_pair(key_path, passphrase.as_deref())?;
                handle
                    .authenticate_publickey(self.user.clone(), Arc::new(key_pair))
                    .await?
            }
            SshAuth::Password => {
                let keychain = crate::app::credentials::load_secret(&self.env_id).await?;
                let secret = effective_secret(&self.secret_override, keychain)
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

/// 续传前置校验（纯函数，便于单测）：本地文件已有字节数必须等于 offset，
/// 否则 append 续传会把两段不连续的数据拼成损坏的文件。
fn verify_resume_offset(actual_len: u64, offset: u64) -> Result<(), String> {
    if actual_len == offset {
        Ok(())
    } else {
        Err(format!(
            "resume offset mismatch: local file has {actual_len} bytes but offset is {offset}"
        ))
    }
}

/// 本地文件当前长度；文件不存在视作 0（续传场景下同样属于 offset mismatch）
async fn local_len_or_zero(path: &std::path::Path) -> std::io::Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// 远端 I/O 错误标记：russh-sftp 的 File 走 tokio AsyncRead/AsyncWrite，错误类型是
/// std::io::Error，与本地磁盘错误无法区分。TransferWorker 靠「错误是否为 io::Error」
/// 判定本地磁盘故障（终态、不重试），因此远端读写/seek/shutdown 的错误必须包装成
/// 非 io 类型，保证 io::Error 只代表本地文件操作失败。
fn remote_io_err(op: &str, e: std::io::Error) -> Box<dyn std::error::Error + Send + Sync> {
    format!("远端 I/O 失败（{op}）: {e}").into()
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
    async fn is_alive(&self) -> bool {
        self.conn.lock().await.is_some()
    }

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

    async fn upload(&self, local: &std::path::Path, remote_path: &str)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut conn = self.conn.lock().await;
        let Some(c) = conn.as_mut() else {
            return Err("ssh not connected (call connect first)".into());
        };

        let channel = c.handle.channel_open_session().await?;
        // 默认 request_timeout_secs=10：慢速链路传大文件时单个 write 请求超时（issue #4
        // 第三轮反馈的 "Timeout"）。放宽到 600s 与下载阶段超时对齐，并发写提升到 16 提高吞吐
        let sftp_cfg = russh_sftp::client::Config {
            request_timeout_secs: 600,
            max_concurrent_writes: 16,
            ..Default::default()
        };
        let sftp = russh_sftp::client::SftpSession::new_with_config(channel.into_stream(), sftp_cfg).await?;

        let file = tokio::fs::File::open(local).await?;
        let mut reader = tokio::io::BufReader::with_capacity(256 * 1024, file);
        let mut remote_file = sftp.create(remote_path).await?;

        let mut buf = vec![0u8; 32 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            remote_file
                .write_all(&buf[..n])
                .await
                .map_err(|e| remote_io_err("write", e))?;
            total += n as u64;
        }
        remote_file
            .shutdown()
            .await
            .map_err(|e| remote_io_err("shutdown", e))?;
        sftp.close().await?;

        tracing::info!(
            env_id = %self.env_id,
            local = %local.display(),
            remote_path,
            bytes = total,
            "sftp upload complete"
        );
        Ok(())
    }

    async fn download(
        &self,
        remote_path: &str,
        local: &std::path::Path,
        offset: u64,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

        let mut conn = self.conn.lock().await;
        let Some(c) = conn.as_mut() else {
            return Err("ssh not connected (call connect first)".into());
        };

        tracing::info!(
            env_id = %self.env_id,
            remote_path,
            local = %local.display(),
            offset,
            "sftp download starting"
        );

        // 续传前置校验：本地文件长度必须等于 offset；文件不存在视作 0 字节。
        // 在任何 SFTP 操作之前失败返回，避免白开 channel 后才拼出损坏文件。
        if offset > 0 {
            let actual_len = local_len_or_zero(local).await?;
            if let Err(msg) = verify_resume_offset(actual_len, offset) {
                tracing::warn!(
                    env_id = %self.env_id,
                    local = %local.display(),
                    offset,
                    actual_len,
                    "{}", msg
                );
                return Err(msg.into());
            }
        }

        let channel = c.handle.channel_open_session().await?;
        // 对齐 upload：慢速链路传 GB 级 dump 时 10s 默认超时不够
        let sftp_cfg = russh_sftp::client::Config {
            request_timeout_secs: 600,
            max_concurrent_writes: 16,
            ..Default::default()
        };
        let sftp = russh_sftp::client::SftpSession::new_with_config(channel.into_stream(), sftp_cfg).await?;

        let mut remote_file = sftp.open(remote_path).await?;
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 续传：append 模式打开本地；offset=0 时 truncate
        let mut local_file = if offset > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(local)
                .await?
        } else {
            tokio::fs::File::create(local).await?
        };
        if offset > 0 {
            remote_file
                .seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| remote_io_err("seek", e))?;
        }

        let mut buf = vec![0u8; 256 * 1024];
        let mut transferred: u64 = offset;
        let mut last_report = std::time::Instant::now();
        let mut last_bytes = transferred;
        loop {
            let n = remote_file
                .read(&mut buf)
                .await
                .map_err(|e| remote_io_err("read", e))?;
            if n == 0 {
                break;
            }
            local_file.write_all(&buf[..n]).await?;
            transferred += n as u64;
            // 1s 节流进度回调（回调只做同步轻量更新）
            if last_report.elapsed() >= std::time::Duration::from_secs(1) {
                let elapsed = last_report.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 {
                    ((transferred - last_bytes) as f64 / elapsed) as u64
                } else {
                    0
                };
                progress(transferred, speed);
                last_report = std::time::Instant::now();
                last_bytes = transferred;
            }
        }
        local_file.flush().await?;
        sftp.close().await?;

        tracing::info!(
            env_id = %self.env_id,
            remote_path,
            local = %local.display(),
            offset,
            bytes = transferred - offset,
            "sftp download complete"
        );
        Ok(())
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
    fn test_effective_secret_override_wins() {
        assert_eq!(
            effective_secret(&Some("form-pass".to_string()), Some("keychain-pass".to_string())),
            Some("form-pass".to_string())
        );
    }

    #[test]
    fn test_effective_secret_empty_override_falls_back_to_keychain() {
        assert_eq!(
            effective_secret(&Some(String::new()), Some("keychain-pass".to_string())),
            Some("keychain-pass".to_string())
        );
    }

    #[test]
    fn test_effective_secret_no_override_uses_keychain() {
        assert_eq!(
            effective_secret(&None, Some("keychain-pass".to_string())),
            Some("keychain-pass".to_string())
        );
        assert_eq!(effective_secret(&None, None), None);
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

    #[test]
    fn test_verify_resume_offset_match_is_ok() {
        assert!(verify_resume_offset(0, 0).is_ok());
        assert!(verify_resume_offset(4096, 4096).is_ok());
    }

    #[test]
    fn test_verify_resume_offset_mismatch_message() {
        let err = verify_resume_offset(100, 200).unwrap_err();
        assert!(err.contains("resume offset mismatch"), "err: {err}");
        assert!(err.contains("local file has 100 bytes but offset is 200"), "err: {err}");
    }

    #[tokio::test]
    async fn test_local_len_or_zero_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("partial.bin");
        std::fs::write(&p, vec![0u8; 1234]).unwrap();
        assert_eq!(local_len_or_zero(&p).await.unwrap(), 1234);
    }

    #[tokio::test]
    async fn test_local_len_or_zero_missing_file_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("no-such-file.bin");
        assert_eq!(local_len_or_zero(&p).await.unwrap(), 0);
    }
}
