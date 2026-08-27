use serde::Serialize;

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

    let bisheng_version = combined
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("BiSheng") && parse_bisheng_version(l).is_ok())
        .ok_or_else(|| {
            format!(
                "unsupported_vendor: no BiSheng version string found (only BiSheng is supported in this version). stdout: {stdout:?} stderr: {stderr:?}"
            )
        })?
        .to_string();

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
        full_dir: s.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_STDOUT: &str = "BiSheng_JDK_Enterprise_205.2.0.110.B001\n---\nx86_64\n";
    const PROBE_STDERR: &str = "openjdk version \"21.0.11\" 2025-04-15\nOpenJDK Runtime Environment (build 21.0.11+9-LTS)\nOpenJDK 64-Bit Server VM (build 21.0.11+9-LTS, mixed mode)\n";

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
    fn test_parse_bisheng_version_standard() {
        let v = parse_bisheng_version("BiSheng_JDK_Enterprise_205.2.0.110.B001").unwrap();
        assert_eq!(v.product_dir, "BiSheng JDK Enterprise");
        assert_eq!(v.major_dir, "BiSheng JDK Enterprise 205");
        assert_eq!(v.full_dir, "BiSheng_JDK_Enterprise_205.2.0.110.B001");
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
        assert_eq!(v.full_dir, "BiSheng_JDK_Compact_105.1.0.B002");
    }

    #[test]
    fn test_build_download_url_full() {
        let probe = parse_probe_output(PROBE_STDOUT, PROBE_STDERR).unwrap();
        let url = build_download_url("https://artifactory.example.com/artifactory/release", &probe).unwrap();
        assert_eq!(
            url,
            "https://artifactory.example.com/artifactory/release/BiSheng%20JDK%20Enterprise/BiSheng%20JDK%20Enterprise%20205/BiSheng_JDK_Enterprise_205.2.0.110.B001/jdk-21.0.11-linux-x64.tar.gz"
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
}
