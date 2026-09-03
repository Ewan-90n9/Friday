use crate::provision::package::{
    emit_progress, ProvisionContext, ProvisionError, ProvisionResult, ToolPackage,
};
use crate::provision::jdk::{run_remote, JvmProbe, REMOTE_TOOLS_DIR};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

/// arthas 版本（官方 arthas-bin.zip 对应版本；升级只改这里 + 替换 vendored 包）
pub const ARTHAS_VERSION: &str = "4.3.5";
/// 进度事件携带的工具名：与 MCP 工具名一致（前端按 tool.name 匹配工具卡片）
pub const ARTHAS_TOOL_NAME: &str = "arthas_open";

pub fn arthas_home() -> String {
    format!("{REMOTE_TOOLS_DIR}/arthas-{ARTHAS_VERSION}")
}

pub struct ArthasPackage;

#[async_trait]
impl ToolPackage for ArthasPackage {
    fn name(&self) -> &str {
        "arthas"
    }

    /// arthas 包与目标 JVM 版本无关，无需探测
    async fn probe(
        &self,
        _ctx: &ProvisionContext,
        _java_bin: &str,
    ) -> Result<JvmProbe, ProvisionError> {
        Ok(JvmProbe {
            openjdk_version: String::new(),
            bisheng_version: String::new(),
            arch: String::new(),
        })
    }

    async fn ensure(
        &self,
        ctx: &ProvisionContext,
        _java_bin: &str,
    ) -> Result<ProvisionResult, ProvisionError> {
        let start = std::time::Instant::now();
        let home = arthas_home();

        // 1. 远端缓存检查
        emit_progress(ctx, ARTHAS_TOOL_NAME, "check_cache", &format!("checking {home}/arthas-boot.jar"));
        let check = run_remote(
            ctx,
            &format!("mkdir -p {REMOTE_TOOLS_DIR} && test -f {home}/arthas-boot.jar"),
            Duration::from_secs(ctx.timeouts.probe),
            "check_cache",
        )
        .await?;
        if check.exit_code == 0 {
            return Ok(ProvisionResult {
                tool: "arthas".to_string(),
                cached: true,
                java_version: String::new(),
                bisheng_version: String::new(),
                arch: String::new(),
                tool_home: home,
                bins: HashMap::new(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }

        // 2. vendored zip：随应用分发的包 SFTP 直传目标机（不再依赖 artifactory）
        let zip = ctx.arthas_zip.as_ref().ok_or_else(|| ProvisionError::new(
            "vendored_package_missing",
            "vendored_package",
            format!("arthas 工具包未随应用分发（resources/arthas/arthas-bin-{ARTHAS_VERSION}.zip），请重新安装 Friday"),
        ))?;
        if let Err(e) = crate::provision::transfer::validate_download(zip, 5 * 1024 * 1024) {
            return Err(ProvisionError::new("vendored_package_corrupt", "vendored_package", e));
        }
        let remote_zip = format!("{REMOTE_TOOLS_DIR}/arthas-bin-{ARTHAS_VERSION}.zip");
        emit_progress(ctx, ARTHAS_TOOL_NAME, "upload", &format!("uploading arthas-bin-{ARTHAS_VERSION}.zip via sftp"));
        ctx.channel.upload(zip, &remote_zip).await.map_err(|e| ProvisionError::new("provision_failed", "upload", e.to_string()))?;

        // 3. 解压（unzip → python3 兜底）+ 顶层目录扁平化 + 清理
        //    find arthas-boot.jar 所在目录作为包根，兼容 zip 内有无顶层目录两种布局
        emit_progress(ctx, ARTHAS_TOOL_NAME, "extract", &format!("extracting arthas-bin-{ARTHAS_VERSION}.zip"));
        let extract_cmd = format!(
            "cd {REMOTE_TOOLS_DIR} && rm -rf arthas-tmp-{ARTHAS_VERSION} arthas-{ARTHAS_VERSION} && \
             mkdir arthas-tmp-{ARTHAS_VERSION} && \
             if command -v unzip >/dev/null 2>&1; then \
               unzip -q -o arthas-bin-{ARTHAS_VERSION}.zip -d arthas-tmp-{ARTHAS_VERSION}/; \
             elif command -v python3 >/dev/null 2>&1; then \
               python3 -m zipfile -e arthas-bin-{ARTHAS_VERSION}.zip arthas-tmp-{ARTHAS_VERSION}/; \
             else \
               echo 'neither unzip nor python3 available' >&2; exit 3; \
             fi && \
             d=$(dirname \"$(find arthas-tmp-{ARTHAS_VERSION} -name arthas-boot.jar | head -1)\") && \
             [ -n \"$d\" ] && mv \"$d\" arthas-{ARTHAS_VERSION} && \
             rm -rf arthas-tmp-{ARTHAS_VERSION} arthas-bin-{ARTHAS_VERSION}.zip && \
             chmod -R 755 arthas-{ARTHAS_VERSION}"
        );
        let extract = run_remote(ctx, &extract_cmd, Duration::from_secs(ctx.timeouts.extract), "extract").await?;
        if extract.exit_code != 0 {
            // 失败清理半成品（后台执行）
            let ch = ctx.channel.clone();
            let cleanup = format!(
                "rm -rf {REMOTE_TOOLS_DIR}/arthas-tmp-{ARTHAS_VERSION} {REMOTE_TOOLS_DIR}/arthas-{ARTHAS_VERSION}"
            );
            tokio::spawn(async move {
                let _ = ch.run(&cleanup).await;
            });
            return Err(ProvisionError::new(
                "provision_failed",
                "extract",
                format!(
                    "unzip failed (exit {}): {} —— 目标机需要 unzip 或 python3 之一",
                    extract.exit_code, extract.stderr
                ),
            ));
        }

        // 4. 验证
        emit_progress(ctx, ARTHAS_TOOL_NAME, "verify", &format!("verifying {home}/arthas-boot.jar"));
        let verify = run_remote(
            ctx,
            &format!("test -f {home}/arthas-boot.jar"),
            Duration::from_secs(ctx.timeouts.verify),
            "verify",
        )
        .await?;
        if verify.exit_code != 0 {
            return Err(ProvisionError::new(
                "provision_failed",
                "verify",
                format!("arthas-boot.jar missing after extract; check vendored arthas-bin-{ARTHAS_VERSION}.zip package layout"),
            ));
        }

        Ok(ProvisionResult {
            tool: "arthas".to_string(),
            cached: false,
            java_version: String::new(),
            bisheng_version: String::new(),
            arch: String::new(),
            tool_home: home,
            bins: HashMap::new(),
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::provision::package::{ProvisionContext, StageTimeouts};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    #[test]
    fn test_arthas_home() {
        assert_eq!(arthas_home(), "/tmp/friday-tools/arthas-4.3.5");
    }

    /// 记录 run + upload 调用的 ExecChannel stub
    #[derive(Default)]
    struct RecordingChannel {
        calls: TokioMutex<Vec<String>>,
        uploads: TokioMutex<Vec<(String, String)>>, // (local, remote)
        responses: TokioMutex<VecDeque<ExecOutput>>,
    }

    impl RecordingChannel {
        fn new(responses: Vec<(&str, i32)>) -> Arc<Self> {
            let dq = responses
                .into_iter()
                .map(|(o, c)| ExecOutput { stdout: o.to_string(), stderr: String::new(), exit_code: c })
                .collect();
            Arc::new(Self {
                calls: TokioMutex::new(Vec::new()),
                uploads: TokioMutex::new(Vec::new()),
                responses: TokioMutex::new(dq),
            })
        }
    }

    #[async_trait::async_trait]
    impl ExecChannel for RecordingChannel {
        async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            Ok(self.responses.lock().await.pop_front().unwrap_or(ExecOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
            }))
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool { true }
        async fn upload(&self, local: &std::path::Path, remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.uploads.lock().await.push((local.display().to_string(), remote.to_string()));
            Ok(())
        }
    }

    fn test_ctx(channel: Arc<RecordingChannel>, arthas_zip: Option<PathBuf>) -> ProvisionContext {
        ProvisionContext {
            session_id: "s1".into(),
            env_id: "env-1".into(),
            channel,
            cache_dir: PathBuf::from("/tmp/unused-cache"),
            artifactory_base_url: "https://artifactory.example.com/artifactory/release".into(),
            arthas_zip,
            timeouts: StageTimeouts::default(),
            bus: crate::app::events::EventBus::disabled(),
        }
    }

    fn make_zip(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("arthas-bin-4.3.5.zip");
        std::fs::write(&p, vec![0u8; 6 * 1024 * 1024]).unwrap();
        p
    }

    #[tokio::test]
    async fn test_ensure_cache_hit_skips_upload() {
        let channel = RecordingChannel::new(vec![
            ("", 0), // test -f arthas-boot.jar 缓存命中
        ]);
        let ctx = test_ctx(channel.clone(), None);
        let result = ArthasPackage.ensure(&ctx, "java").await.unwrap();
        assert!(result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/arthas-4.3.5");
        assert!(channel.uploads.lock().await.is_empty(), "cache hit must not upload");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().all(|c| !c.contains("unzip") && !c.contains("python3")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_missing_zip_reports_structured_error() {
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
        ]);
        let ctx = test_ctx(channel.clone(), None);
        let err = ArthasPackage.ensure(&ctx, "java").await.unwrap_err();
        assert!(err.stage == "vendored_package" || err.message.contains("未随应用分发"), "err: {err:?}");
        assert!(channel.uploads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_ensure_corrupt_zip_reports_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("arthas-bin-4.3.5.zip");
        std::fs::write(&bad, vec![0u8; 1024]).unwrap(); // 太小
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
        ]);
        let ctx = test_ctx(channel.clone(), Some(bad));
        let err = ArthasPackage.ensure(&ctx, "java").await.unwrap_err();
        assert!(err.message.contains("arthas") || err.stage == "vendored_package", "err: {err:?}");
        assert!(channel.uploads.lock().await.is_empty(), "corrupt zip must not upload");
    }

    #[tokio::test]
    async fn test_ensure_uploads_and_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = make_zip(tmp.path());
        let channel = RecordingChannel::new(vec![
            ("", 1), // 缓存未命中
            ("", 0), // 解压成功
            ("", 0), // 验证成功
        ]);
        let ctx = test_ctx(channel.clone(), Some(zip.clone()));
        let result = ArthasPackage.ensure(&ctx, "java").await.unwrap();
        assert!(!result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/arthas-4.3.5");
        let uploads = channel.uploads.lock().await;
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, zip.display().to_string());
        assert_eq!(uploads[0].1, "/tmp/friday-tools/arthas-bin-4.3.5.zip");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().any(|c| c.contains("unzip -q -o arthas-bin-4.3.5.zip")), "calls: {calls:?}");
        assert!(calls.iter().any(|c| c.contains("arthas-boot.jar")), "find arthas-boot.jar: {calls:?}");
        // 不再有任何 artifactory 下载
        assert!(calls.iter().all(|c| !c.contains("curl") && !c.contains("wget")), "calls: {calls:?}");
    }

    /// vendoring 一致性守卫：scripts/vendor-versions.json 与 ARTHAS_VERSION 必须一致。
    #[test]
    fn test_vendor_manifest_matches_arthas_version() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts")
            .join("vendor-versions.json");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest.display()));
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("vendor-versions.json must be valid JSON");
        let version = v["arthas"]["version"].as_str().expect("arthas.version");
        assert_eq!(
            version, ARTHAS_VERSION,
            "scripts/vendor-versions.json 的 arthas.version 与 ARTHAS_VERSION 漂移，二者必须同步修改"
        );
        let asset = v["arthas"]["asset"].as_str().expect("arthas.asset");
        assert_eq!(asset, format!("arthas-bin-{ARTHAS_VERSION}.zip"));
    }
}
