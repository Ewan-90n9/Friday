use super::channel::{ExecChannel, ExecOutput};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
}

#[async_trait]
impl ExecChannel for SshTransport {
    async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        Err("SSH transport not yet implemented".into())
    }

    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("SSH transport not yet implemented".into())
    }

    async fn disconnect(&self) {}
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
}
