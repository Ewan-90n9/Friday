use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// URL → 本地缓存路径：<cache_dir>/<sha256(url)>.tar.gz
pub fn cache_path_for(cache_dir: &Path, url: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    let name = format!("{:x}.tar.gz", digest);
    cache_dir.join(name)
}

/// 下载 URL 到缓存路径（已存在且非空则复用）。curl.exe 模式对齐 embedding.rs。
pub fn download_to_cache(url: &str, cache_dir: &Path) -> Result<PathBuf, String> {
    let dest = cache_path_for(cache_dir, url);
    if dest.exists() {
        let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if len > 0 {
            tracing::info!(url, path = %dest.display(), "provision: local cache hit");
            return Ok(dest);
        }
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("download failed: create cache dir: {e}"))?;

    tracing::info!(url, path = %dest.display(), "provision: downloading to local cache");
    let dest_str = dest.to_string_lossy();
    let output = std::process::Command::new("curl.exe")
        .args([
            "-k", "-L", "-o", &dest_str,
            "--connect-timeout", "30",
            "--max-time", "600",
            "--retry", "2",
            "-s", "-S",
            "-w", "%{http_code}",
            url,
        ])
        .output()
        .map_err(|e| format!("download failed: failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_file(&dest);
        return Err(format!("download failed: {}", stderr.trim()));
    }
    let http_code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !http_code.starts_with('2') {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("download failed: HTTP {http_code}"));
    }
    if !dest.exists() {
        return Err("download failed: file not created".to_string());
    }
    Ok(dest)
}

/// 校验下载产物：存在且大小不小于 min_bytes
pub fn validate_download(path: &Path, min_bytes: u64) -> Result<(), String> {
    let len = std::fs::metadata(path).map(|m| m.len()).map_err(|e| format!("download incomplete: {e}"))?;
    if len < min_bytes {
        return Err(format!(
            "download incomplete: file is {len} bytes, expected at least {min_bytes} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_path_for_is_deterministic_and_named_by_url_hash() {
        let dir = Path::new("/tmp/cache");
        let p1 = cache_path_for(dir, "https://example.com/jdk-21.tar.gz");
        let p2 = cache_path_for(dir, "https://example.com/jdk-21.tar.gz");
        let p3 = cache_path_for(dir, "https://example.com/jdk-22.tar.gz");
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        assert!(p1.file_name().unwrap().to_string_lossy().ends_with(".tar.gz"));
        assert!(p1.file_name().unwrap().to_string_lossy().len() >= 16);
    }

    #[test]
    fn test_validate_download_rejects_missing() {
        assert!(validate_download(Path::new("/nonexistent/x.tar.gz"), 1).is_err());
    }

    #[test]
    fn test_validate_download_rejects_too_small() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("small.tar.gz");
        std::fs::write(&f, vec![0u8; 1024]).unwrap();
        assert!(validate_download(&f, 50 * 1024 * 1024).is_err());
    }

    #[test]
    fn test_validate_download_accepts_large() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("big.tar.gz");
        std::fs::write(&f, vec![0u8; 51 * 1024 * 1024]).unwrap();
        assert!(validate_download(&f, 50 * 1024 * 1024).is_ok());
        let empty = tmp.path().join("empty.tar.gz");
        std::fs::write(&empty, b"").unwrap();
        assert!(validate_download(&empty, 1).is_err());
    }

    #[test]
    fn test_download_to_cache_reuses_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let dest = cache_path_for(&cache, "https://example.com/jdk.tar.gz");
        std::fs::write(&dest, vec![1u8; 60 * 1024 * 1024]).unwrap();
        let path = download_to_cache("https://example.com/jdk.tar.gz", &cache).unwrap();
        assert_eq!(path, dest);
    }

    #[test]
    fn test_download_to_cache_unreachable_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let err = download_to_cache("http://127.0.0.1:1/never-reachable.tar.gz", &cache).unwrap_err();
        assert!(err.to_lowercase().contains("download") || err.to_lowercase().contains("curl"), "err: {err}");
        assert!(!cache_path_for(&cache, "http://127.0.0.1:1/never-reachable.tar.gz").exists());
    }
}
