use async_trait::async_trait;

pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[async_trait]
pub trait ExecChannel: Send + Sync {
    async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>>;
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn disconnect(&self);
    /// 连接池巡检用：连接是否仍然存活
    async fn is_alive(&self) -> bool;
    /// 上传文件到远端路径（SFTP 或等价实现）。供工具装备（推 JDK 包）与后续
    /// artifacts 回拉复用。默认返回未实现错误——Mock/测试实现按需覆盖。
    async fn upload(&self, _local: &std::path::Path, _remote_path: &str)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("upload not implemented for this channel".into())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use super::*;
    use std::path::Path;

    struct RecordingChannel {
        uploaded: tokio::sync::Mutex<Vec<(std::path::PathBuf, String)>>,
    }

    #[async_trait]
    impl ExecChannel for RecordingChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, local: &Path, remote_path: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.uploaded.lock().await.push((local.to_path_buf(), remote_path.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_upload_trait_method_dispatches() {
        let ch = RecordingChannel { uploaded: tokio::sync::Mutex::new(Vec::new()) };
        let dyn_ch: &dyn ExecChannel = &ch;
        dyn_ch.upload(Path::new("/tmp/f.tar.gz"), "/tmp/friday-tools/f.tar.gz").await.unwrap();
        assert_eq!(ch.uploaded.lock().await.len(), 1);
        assert_eq!(ch.uploaded.lock().await[0].1, "/tmp/friday-tools/f.tar.gz");
    }

    struct DefaultUploadChannel;

    #[async_trait]
    impl ExecChannel for DefaultUploadChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_upload_default_returns_not_implemented() {
        let ch = DefaultUploadChannel;
        let dyn_ch: &dyn ExecChannel = &ch;
        let err = dyn_ch.upload(Path::new("/tmp/f.tar.gz"), "/tmp/x.tar.gz").await.unwrap_err();
        assert!(err.to_string().contains("not implemented"), "err: {err}");
    }
}
