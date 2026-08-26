use std::path::PathBuf;

/// 展开 `~` 前缀路径（Windows 上 ~ = %USERPROFILE%）
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home_prefix() {
        let p = expand_tilde("~/.ssh/id_ed25519");
        assert!(p.components().count() > 1);
        assert!(!p.starts_with("~"));
    }

    #[test]
    fn test_expand_tilde_absolute_untouched() {
        let p = expand_tilde("C:/keys/id_rsa");
        assert_eq!(p, PathBuf::from("C:/keys/id_rsa"));
    }

    #[test]
    fn test_expand_tilde_bare_home_is_home_dir() {
        let p = expand_tilde("~/");
        assert_eq!(p, dirs::home_dir().unwrap());
    }
}
