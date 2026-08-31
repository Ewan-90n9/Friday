use crate::provision::package::{
    emit_progress, ProvisionContext, ProvisionError, ProvisionResult, ToolPackage,
};
use crate::provision::jdk::{run_remote, try_remote_download, JvmProbe, REMOTE_TOOLS_DIR};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

/// arthas 版本（官方 arthas-bin.zip 对应版本；升级只改这里 + artifactory 放包）
pub const ARTHAS_VERSION: &str = "4.3.5";
/// 进度事件携带的工具名：与 MCP 工具名一致（前端按 tool.name 匹配工具卡片）
pub const ARTHAS_TOOL_NAME: &str = "arthas_open";

pub fn arthas_home() -> String {
    format!("{REMOTE_TOOLS_DIR}/arthas-{ARTHAS_VERSION}")
}

pub fn arthas_download_url(base: &str) -> String {
    format!("{}/arthas/arthas-bin-{ARTHAS_VERSION}.zip", base.trim_end_matches('/'))
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

        // 2. 下载（通道 A：目标自拉；通道 B：本地下载 + SFTP 上传）
        let url = arthas_download_url(&ctx.artifactory_base_url);
        let remote_zip = format!("{REMOTE_TOOLS_DIR}/arthas-bin-{ARTHAS_VERSION}.zip");
        emit_progress(ctx, ARTHAS_TOOL_NAME, "download", "channel A: remote curl/wget");
        if let Err(a_err) = try_remote_download(ctx, &url, &remote_zip).await {
            tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, error = %a_err, "channel A failed, falling back to channel B");
            emit_progress(ctx, ARTHAS_TOOL_NAME, "download", "channel B: local download + sftp upload");
            let local = crate::provision::transfer::download_to_cache(&url, &ctx.cache_dir)
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                })?;
            if let Err(e) = crate::provision::transfer::validate_download(&local, 5 * 1024 * 1024) {
                tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, path = %local.display(), error = %e, "local cached arthas zip failed validation, removing");
                let _ = std::fs::remove_file(&local);
                return Err(ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                });
            }
            ctx.channel
                .upload(&local, &remote_zip)
                .await
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "upload", e.to_string())
                })?;
        }

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
                format!("arthas-boot.jar missing after extract; check artifactory package layout ({url})"),
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

    #[test]
    fn test_arthas_home() {
        assert_eq!(arthas_home(), "/tmp/friday-tools/arthas-4.3.5");
    }

    #[test]
    fn test_download_url() {
        assert_eq!(
            arthas_download_url("https://artifactory.example.com/artifactory/tools"),
            "https://artifactory.example.com/artifactory/tools/arthas/arthas-bin-4.3.5.zip",
        );
        // 尾部斜杠容忍
        assert_eq!(
            arthas_download_url("https://a.example.com/b/"),
            "https://a.example.com/b/arthas/arthas-bin-4.3.5.zip",
        );
    }
}
