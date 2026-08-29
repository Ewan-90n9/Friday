use std::path::PathBuf;

// 字段由 Task 3（analyzer manager）读取
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct JavaInfo {
    pub path: PathBuf,
    pub major: u32,
}

/// 解析 `java -version` 输出主版本号。处理两种格式：
/// - 现代格式 `openjdk version "21.0.3"` → 21
/// - 旧格式 `java version "1.8.0_391"` → 8（主版本 1 时取次版本）
pub fn parse_java_version(output: &str) -> Option<u32> {
    let rest = output.split("version").nth(1)?;
    let quoted = rest.split('"').nth(1)?;
    if quoted.is_empty() {
        return None;
    }
    let mut parts = quoted.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    match first {
        1 => {
            let second = parts.next()?;
            let digits: String = second.chars().take_while(|c| c.is_ascii_digit()).collect();
            let v: u32 = digits.parse().ok()?;
            if v == 0 { None } else { Some(v) }
        }
        v => Some(v),
    }
}

/// 候选 java 路径：JAVA_HOME/bin/java 优先（文件存在才入列），其次 PATH（which 解析）
pub fn java_candidates(java_home: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = java_home {
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        let p = PathBuf::from(home).join("bin").join(exe);
        if p.is_file() {
            out.push(p);
        }
    }
    if let Ok(p) = which::which("java") {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// 探测 Java 21+：逐候选执行 `java -version`。Err 附带可读原因（含探测到的版本号）。
// Task 3（analyzer manager）接入前暂无调用方，避免 dead_code 告警
#[allow(dead_code)]
pub async fn detect_java() -> Result<JavaInfo, String> {
    let candidates = java_candidates(std::env::var("JAVA_HOME").ok().as_deref());
    let mut last_err = String::from("未找到 java 可执行文件（已检查 JAVA_HOME 与 PATH）");
    for path in candidates {
        match probe_version(&path).await {
            Ok(Some(v)) if v >= 21 => return Ok(JavaInfo { path, major: v }),
            Ok(Some(v)) => last_err = format!("找到 {} 但为 Java {v}，需要 21+", path.display()),
            Ok(None) => last_err = format!("无法解析 {} 的版本输出", path.display()),
            Err(e) => last_err = format!("执行 {} -version 失败: {e}", path.display()),
        }
    }
    Err(last_err)
}

async fn probe_version(java_path: &std::path::Path) -> Result<Option<u32>, String> {
    let out = tokio::process::Command::new(java_path)
        .arg("-version")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    // `java -version` 惯例输出到 stderr，stdout 兜底
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    Ok(parse_java_version(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_modern_versions() {
        assert_eq!(parse_java_version(r#"openjdk version "21.0.3" 2024-04-16"#), Some(21));
        assert_eq!(parse_java_version(r#"openjdk version "17.0.2" 2022-01-18"#), Some(17));
        assert_eq!(parse_java_version(r#"openjdk version "25" 2025-09-16"#), Some(25));
        // BiSheng JDK 打印标准格式
        assert_eq!(parse_java_version(r#"openjdk version "21.0.11" 2025-01-21"#), Some(21));
    }

    #[test]
    fn test_parse_legacy_1_8_format() {
        assert_eq!(parse_java_version(r#"java version "1.8.0_391""#), Some(8));
        assert_eq!(parse_java_version(r#"java version "1.8.0_391" Java(TM) SE"#), Some(8));
    }

    #[test]
    fn test_parse_garbage_returns_none() {
        assert_eq!(parse_java_version(""), None);
        assert_eq!(parse_java_version("xyz"), None);
        assert_eq!(parse_java_version("Runtime Environment (build 25+36)"), None);
    }

    #[test]
    fn test_java_candidates_prefers_java_home_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        std::fs::write(bin.join(exe), "").unwrap();

        let cands = java_candidates(Some(tmp.path().to_str().unwrap()));
        assert_eq!(cands.first().unwrap(), &bin.join(exe));

        // JAVA_HOME 不存在 → 不在候选里
        let cands = java_candidates(Some("C:/definitely/not/here"));
        assert!(cands.iter().all(|p| !p.to_string_lossy().contains("not/here")));
    }
}
