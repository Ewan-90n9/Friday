use std::path::PathBuf;

pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("friday.db")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn playbooks_dir(&self) -> PathBuf {
        self.root.join("playbooks")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn prompts_dir(&self) -> PathBuf {
        self.root.join("prompts")
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn session_artifacts_dir(&self, session_id: &str) -> PathBuf {
        self.artifacts_dir().join(session_id)
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.log_dir(),
            self.playbooks_dir(),
            self.skills_dir(),
            self.prompts_dir(),
            self.artifacts_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_path_returns_root_join_friday_db() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let db = paths.db_path();
        assert_eq!(db, tmp.path().join("friday.db"));
    }

    #[test]
    fn test_log_dir_returns_root_join_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.log_dir(), tmp.path().join("logs"));
    }

    #[test]
    fn test_playbooks_dir_returns_root_join_playbooks() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.playbooks_dir(), tmp.path().join("playbooks"));
    }

    #[test]
    fn test_skills_dir_returns_root_join_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.skills_dir(), tmp.path().join("skills"));
    }

    #[test]
    fn test_prompts_dir_returns_root_join_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.prompts_dir(), tmp.path().join("prompts"));
    }

    #[test]
    fn test_artifacts_dir_returns_root_join_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        assert_eq!(paths.artifacts_dir(), tmp.path().join("artifacts"));
    }

    #[test]
    fn test_session_artifacts_dir_joins_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        let dir = paths.session_artifacts_dir("abc-123");
        assert_eq!(dir, tmp.path().join("artifacts").join("abc-123"));
    }

    #[test]
    fn test_ensure_dirs_creates_all_five_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(tmp.path().join("logs").is_dir());
        assert!(tmp.path().join("playbooks").is_dir());
        assert!(tmp.path().join("skills").is_dir());
        assert!(tmp.path().join("prompts").is_dir());
        assert!(tmp.path().join("artifacts").is_dir());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_db_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("friday.db").exists());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_session_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("artifacts").join("some-session").exists());
    }

    #[test]
    fn test_ensure_dirs_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());

        paths.ensure_dirs().unwrap();
        paths.ensure_dirs().unwrap();

        assert!(tmp.path().join("logs").is_dir());
    }

    #[test]
    fn test_ensure_dirs_does_not_create_session_subdir_after_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().to_path_buf());

        paths.ensure_dirs().unwrap();
        paths.ensure_dirs().unwrap();

        assert!(!tmp.path().join("artifacts").join("some-session").exists());
    }
}
