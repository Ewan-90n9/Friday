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
    /// 从远端下载文件到本地路径（SFTP 或等价实现）。offset 为续传起点
    /// （0 = 从头）。progress 每 1s 节流回调 (transferred_bytes, speed_bps)。
    /// 供 heap dump 回拉等 artifacts 下载复用。默认返回未实现错误——
    /// Mock/测试实现按需覆盖。
    async fn download(
        &self,
        _remote_path: &str,
        _local: &std::path::Path,
        _offset: u64,
        _progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("download not implemented for this channel".into())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

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

    struct RecordingDownloadChannel {
        downloaded: tokio::sync::Mutex<Vec<(String, std::path::PathBuf)>>,
    }

    #[async_trait]
    impl ExecChannel for RecordingDownloadChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn download(
            &self,
            remote_path: &str,
            local: &Path,
            _offset: u64,
            _progress: &(dyn Fn(u64, u64) + Sync),
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.downloaded.lock().await.push((remote_path.to_string(), local.to_path_buf()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_download_trait_method_dispatches() {
        let ch = RecordingDownloadChannel { downloaded: tokio::sync::Mutex::new(Vec::new()) };
        let dyn_ch: &dyn ExecChannel = &ch;
        dyn_ch.download("/tmp/friday-tools/dump.hprof", Path::new("/local/dump.hprof"), 0, &|_, _| {}).await.unwrap();
        let recorded = ch.downloaded.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "/tmp/friday-tools/dump.hprof");
        assert_eq!(recorded[0].1, Path::new("/local/dump.hprof"));
    }

    struct DefaultDownloadChannel;

    #[async_trait]
    impl ExecChannel for DefaultDownloadChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_download_default_returns_not_implemented() {
        let ch = DefaultDownloadChannel;
        let dyn_ch: &dyn ExecChannel = &ch;
        let err = dyn_ch.download("/tmp/x.hprof", Path::new("/tmp/local.hprof"), 0, &|_, _| {}).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"), "err: {err}");
    }

    // 记录 offset 并模拟断点续传的 mock：验证 trait 分发携带 offset
    struct OffsetRecordingChannel {
        offsets: tokio::sync::Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl ExecChannel for OffsetRecordingChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn download(&self, _remote: &str, _local: &Path, offset: u64, _progress: &(dyn Fn(u64, u64) + Sync))
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.offsets.lock().await.push(offset);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_download_dispatches_offset() {
        let ch = Arc::new(OffsetRecordingChannel { offsets: tokio::sync::Mutex::new(Vec::new()) });
        let dyn_ch: Arc<dyn ExecChannel> = ch.clone();
        dyn_ch.download("/tmp/x.hprof", Path::new("/local/x.hprof"), 4096, &|_, _| {}).await.unwrap();
        let offsets = ch.offsets.lock().await;
        assert_eq!(*offsets, vec![4096]);
    }

    // 进度回调 mock：验证 progress 被调用
    struct ProgressReportingChannel;

    #[async_trait]
    impl ExecChannel for ProgressReportingChannel {
        async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn download(&self, _remote: &str, _local: &Path, offset: u64, progress: &(dyn Fn(u64, u64) + Sync))
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            progress(offset + 100, 1024);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_download_progress_callback_invoked() {
        let ch: Arc<dyn ExecChannel> = Arc::new(ProgressReportingChannel);
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
        let seen_clone = seen.clone();
        ch.download("/tmp/x.hprof", Path::new("/local/x.hprof"), 500, &move |t, s| {
            seen_clone.lock().unwrap().push((t, s));
        })
        .await
        .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(*seen, vec![(600, 1024)]);
    }
}
