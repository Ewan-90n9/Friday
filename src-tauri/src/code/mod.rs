//! 代码仓读取域：本机 git clone 缓存 + worktree 检出 + 只读工具实现。

pub mod git;
pub mod manager;       // Task 3
// pub mod search;    // Task 4

pub use manager::{CodeRepoManager, RepoCacheEntry, RepoPhase, RepoState};  // Task 3

#[cfg(test)]
pub(crate) mod testutil {
    use crate::code::git;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git binary required for tests");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// main 分支当前 HEAD 的 commit sha
    pub fn head_sha(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse");
        assert!(out.status.success(), "rev-parse failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// 本地源仓 fixture：main 分支 2 提交（首提交打 tag v1.0）+ feature-x 分支。
    /// 返回 file:// URL（仓需保留在 tempdir 中，删除即可模拟远端不可达）。
    pub fn make_source_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "friday@test.local"]);
        git(dir, &["config", "user.name", "friday-test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        git(dir, &["config", "tag.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), "hello friday\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src").join("Bar.java"),
            "package com.foo;\npublic class Bar {\n    void baz() {}\n}\n",
        )
        .unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
        git(dir, &["tag", "v1.0"]);
        std::fs::write(dir.join("src").join("Baz.java"), "package com.foo;\npublic class Baz {}\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "add Baz"]);
        git(dir, &["checkout", "-b", "feature-x"]);
        std::fs::write(dir.join("src").join("Feature.java"), "package com.foo;\npublic class Feature {}\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "feature work"]);
        git(dir, &["checkout", "main"]);
        git::file_url(dir)
    }
}
