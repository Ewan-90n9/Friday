use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

pub struct DetectedAgent {
    pub provider: &'static str,
    pub display_name: &'static str,
    pub path: PathBuf,
    pub version: Option<String>,
}

pub async fn detect() -> Vec<DetectedAgent> {
    let mut found = Vec::new();
    for desc in super::registry::REGISTRY {
        match which::which_global(desc.command) {
            Ok(path) => {
                let version = detect_version(&path).await;
                found.push(DetectedAgent {
                    provider: desc.provider,
                    display_name: desc.display_name,
                    path,
                    version,
                });
            }
            Err(_) => continue,
        }
    }
    found
}

pub async fn detect_version(path: &Path) -> Option<String> {
    let result = timeout(
        Duration::from_secs(5),
        Command::new(path)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output(),
    )
    .await;
    match result {
        Ok(Ok(output)) => parse_version(&String::from_utf8_lossy(&output.stdout)),
        _ => None,
    }
}

pub fn parse_version(text: &str) -> Option<String> {
    let re = Regex::new(r"v?(\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(text)?;
    Some(format!("{}.{}.{}", &caps[1], &caps[2], &caps[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_extracts_semver_from_opencode_output() {
        assert_eq!(parse_version("opencode 0.2.15"), Some("0.2.15".to_string()));
    }

    #[test]
    fn parse_version_strips_v_prefix() {
        assert_eq!(parse_version("v1.0.0-beta"), Some("1.0.0".to_string()));
    }

    #[test]
    fn parse_version_returns_none_when_no_version() {
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn parse_version_returns_none_for_empty_string() {
        assert_eq!(parse_version(""), None);
    }

    #[tokio::test]
    async fn detect_returns_vec_without_panicking() {
        let result = detect().await;
        for agent in &result {
            assert_eq!(agent.provider, "opencode");
        }
    }
}
