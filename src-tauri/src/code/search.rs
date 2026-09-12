//! 仓内只读三件套：read_file（行号分页）/ search（正则 + .gitignore）/
//! list_files（glob / 子目录）。路径安全：canonicalize + worktree 前缀校验。

use std::path::{Path, PathBuf};

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use regex::Regex;

pub const DEFAULT_READ_LINES: usize = 2000;
pub const MAX_READ_LINES: usize = 5000;
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_MAX_RESULTS: usize = 100;
pub const MAX_RESULTS_CAP: usize = 500;
pub const DEFAULT_CONTEXT_LINES: usize = 2;
pub const MAX_CONTEXT_LINES: usize = 10;
pub const LIST_MAX: usize = 500;
const BINARY_SNIFF_BYTES: usize = 8192;

/// worktree 内路径安全解析：canonicalize + 前缀校验，拒绝 `..` 逃逸与绝对路径替换
pub fn resolve_in_worktree(worktree: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.contains('\0') {
        return Err("path contains NUL".to_string());
    }
    let wt_canon = worktree
        .canonicalize()
        .map_err(|e| format!("worktree not accessible: {e}"))?;
    let joined = wt_canon.join(rel);
    let canon = joined
        .canonicalize()
        .map_err(|e| format!("path not found in repo: {rel}: {e}"))?;
    if canon.starts_with(&wt_canon) {
        Ok(canon)
    } else {
        Err(format!("path escapes repo worktree: {rel}"))
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

fn rel_display(path: &Path, worktree: &Path) -> String {
    path.strip_prefix(worktree)
        .map(|p| p.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

pub struct ReadFileOutput {
    pub total_lines: usize,
    pub offset: usize,
    pub limit: usize,
    pub lines: Vec<(usize, String)>,
    pub truncated: bool,
}

/// 读文件（1-based 行号）；offset 默认 1、limit 默认 2000 上限 5000（调用方 clamp）
pub fn read_file(worktree: &Path, rel: &str, offset: usize, limit: usize) -> Result<ReadFileOutput, String> {
    let path = resolve_in_worktree(worktree, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot stat {rel}: {e}"))?;
    if meta.is_dir() {
        return Err(format!("{rel} is a directory; use code_list_files"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file too large ({} bytes > {}); use code_search with a pattern to locate content",
            meta.len(),
            MAX_FILE_BYTES
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    if is_binary(&bytes) {
        return Err(format!("{rel} is a binary file"));
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let offset = offset.max(1);
    let lines: Vec<(usize, String)> = all
        .iter()
        .enumerate()
        .skip(offset - 1)
        .take(limit)
        .map(|(i, l)| (i + 1, (*l).to_string()))
        .collect();
    let truncated = offset - 1 + lines.len() < total;
    Ok(ReadFileOutput { total_lines: total, offset, limit: lines.len(), lines, truncated })
}

pub struct SearchMatch {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub context_before: Vec<(usize, String)>,
    pub context_after: Vec<(usize, String)>,
}

pub struct SearchOutput {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

/// 正则搜索（尊重 .gitignore；跳过二进制与 >5MB 文件）；walk 错误（如权限拒绝）静默跳过（只读工具可接受）
pub fn search(
    worktree: &Path,
    pattern: &str,
    glob: Option<&str>,
    context: usize,
    max_results: usize,
) -> Result<SearchOutput, String> {
    let re = Regex::new(pattern).map_err(|e| format!("invalid regex pattern: {e}"))?;
    let context = context.min(MAX_CONTEXT_LINES);
    let max_results = max_results.min(MAX_RESULTS_CAP);
    let mut builder = WalkBuilder::new(worktree);
    builder.require_git(false);
    if let Some(g) = glob {
        let mut ob = OverrideBuilder::new(worktree);
        ob.add(g).map_err(|e| format!("invalid glob '{g}': {e}"))?;
        builder.overrides(ob.build().map_err(|e| format!("invalid glob '{g}': {e}"))?);
    }
    let mut matches = Vec::new();
    let mut truncated = false;
    'outer: for entry in builder.build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                if matches.len() >= max_results {
                    truncated = true;
                    break 'outer;
                }
                let before = lines[i.saturating_sub(context)..i]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| (i.saturating_sub(context) + j + 1, (*l).to_string()))
                    .collect();
                let after_end = (i + 1 + context).min(lines.len());
                let after = lines[i + 1..after_end]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| (i + 2 + j, (*l).to_string()))
                    .collect();
                matches.push(SearchMatch {
                    file: rel_display(path, worktree),
                    line: i + 1,
                    text: (*line).to_string(),
                    context_before: before,
                    context_after: after,
                });
            }
        }
    }
    Ok(SearchOutput { matches, truncated })
}

/// 列文件（尊重 .gitignore；glob 白名单或 path 子目录二选一或全仓）；walk 错误（如权限拒绝）静默跳过（只读工具可接受）
pub fn list_files(
    worktree: &Path,
    subpath: Option<&str>,
    glob: Option<&str>,
) -> Result<(Vec<String>, bool), String> {
    let wt_canon = worktree
        .canonicalize()
        .map_err(|e| format!("worktree not accessible: {e}"))?;
    let root = match subpath {
        Some(p) => resolve_in_worktree(worktree, p)?,
        None => wt_canon.clone(),
    };
    if root.is_file() {
        return Ok((vec![rel_display(&root, &wt_canon)], false));
    }
    let mut builder = WalkBuilder::new(&root);
    builder.require_git(false);
    // walk 顺序确定化：截断保留字典序最小者，而非 FS 枚举顺序碰到的
    builder.sort_by_file_name(Ord::cmp);
    if let Some(g) = glob {
        let mut ob = OverrideBuilder::new(&root);
        ob.add(g).map_err(|e| format!("invalid glob '{g}': {e}"))?;
        builder.overrides(ob.build().map_err(|e| format!("invalid glob '{g}': {e}"))?);
    }
    let mut files = Vec::new();
    let mut truncated = false;
    for entry in builder.build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if files.len() >= LIST_MAX {
            truncated = true;
            break;
        }
        files.push(rel_display(entry.path(), &wt_canon));
    }
    files.sort();
    Ok((files, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_worktree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        std::fs::write(wt.join("README.md"), "line1\nline2 target\nline3\n").unwrap();
        std::fs::create_dir_all(wt.join("src").join("main")).unwrap();
        std::fs::write(
            wt.join("src").join("main").join("Bar.java"),
            "package com.foo;\npublic class Bar {\n    void baz() {}\n}\n",
        )
        .unwrap();
        std::fs::write(wt.join("src").join("bin.dat"), "bin\0ary\n").unwrap();
        std::fs::create_dir_all(wt.join("target").join("classes")).unwrap();
        std::fs::write(wt.join("target").join("classes").join("Gen.java"), "generated target line\n").unwrap();
        std::fs::write(wt.join(".gitignore"), "target/\n").unwrap();
        tmp
    }

    #[test]
    fn test_read_file_line_numbers_and_pagination() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let out = read_file(wt, "src/main/Bar.java", 1, 5000).unwrap();
        assert_eq!(out.total_lines, 4);
        assert_eq!(out.lines[0], (1, "package com.foo;".to_string()));
        assert_eq!(out.lines[1].0, 2);
        assert!(!out.truncated);

        let out = read_file(wt, "src/main/Bar.java", 2, 1).unwrap();
        assert_eq!(out.lines, vec![(2, "public class Bar {".to_string())]);
        assert!(out.truncated, "more lines after the window");
    }

    #[test]
    fn test_read_file_rejects_escape_and_missing() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        // worktree 外真实存在的文件：canonicalize 成功，必须由前缀校验拒绝
        let outside = tmp.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "outside\n").unwrap();
        assert!(read_file(wt, "../outside.txt", 1, 10).is_err(), "escape via prefix check");
        let _ = std::fs::remove_file(&outside);
        assert!(read_file(wt, "no-such.txt", 1, 10).is_err());
        assert!(read_file(wt, "src/bin.dat", 1, 10).is_err(), "binary rejected");
    }

    #[test]
    fn test_search_respects_gitignore_and_context() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let out = search(wt, "target", None, 1, 100).unwrap();
        // .gitignore 的 target/ 不搜；README 里的 target 命中
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].file, "README.md");
        assert_eq!(out.matches[0].line, 2);
        assert_eq!(out.matches[0].context_before.len(), 1);
        assert!(!out.truncated);

        let out = search(wt, "class", Some("*.java"), 0, 100).unwrap();
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].file, "src/main/Bar.java");

        assert!(search(wt, "class(", None, 1, 10).is_err(), "invalid regex rejected");
    }

    #[test]
    fn test_search_context_before_clamped_at_file_start() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.txt"),
            "alpha\nbeta match\ngamma\n",
        )
        .unwrap();
        let out = search(tmp.path(), "match", None, 2, 10).unwrap();
        assert_eq!(out.matches.len(), 1);
        let m = &out.matches[0];
        assert_eq!(m.line, 2);
        assert_eq!(m.context_before, vec![(1, "alpha".to_string())]);
    }

    #[test]
    fn test_search_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        for i in 0..10 {
            std::fs::write(tmp.path().join("src").join(format!("F{i}.txt")), "hit\n").unwrap();
        }
        let out = search(tmp.path(), "hit", None, 0, 3).unwrap();
        assert_eq!(out.matches.len(), 3);
        assert!(out.truncated);
    }

    #[test]
    fn test_search_extreme_context_and_max_results_clamped() {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (0..20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), body).unwrap();
        // context=usize::MAX：内部 clamp 到 MAX_CONTEXT_LINES，不 panic、上下文不越界
        let out = search(tmp.path(), "line15", None, usize::MAX, usize::MAX).unwrap();
        assert_eq!(out.matches.len(), 1);
        let m = &out.matches[0];
        assert_eq!(m.line, 16);
        assert_eq!(m.context_before.len(), MAX_CONTEXT_LINES);
        assert_eq!(m.context_before[0].0, 16 - MAX_CONTEXT_LINES);
        assert_eq!(m.context_after.len(), 20 - 16, "clamped by file end");
        // max_results=0：首命中即截断
        let out = search(tmp.path(), "line", None, 0, 0).unwrap();
        assert_eq!(out.matches.len(), 0);
        assert!(out.truncated);
    }

    #[test]
    fn test_list_files_cap_is_deterministic_lexicographic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("src");
        std::fs::create_dir_all(&dir).unwrap();
        // 大小写混用：NTFS 目录枚举按 upcase 表排序（a* 在 B* 前），二进制
        // Ord::cmp 则 B* 在 a* 前——两种顺序的前 LIST_MAX 名不同，可判别截断
        // 是否确定性保留字典序最小者
        for i in 0..600 {
            let name = if i < 300 { format!("a{i:03}.txt") } else { format!("B{i:03}.txt") };
            std::fs::write(dir.join(name), "x\n").unwrap();
        }
        let (files, truncated) = list_files(tmp.path(), Some("src"), None).unwrap();
        assert!(truncated);
        assert_eq!(files.len(), LIST_MAX);
        assert_eq!(files[0], "src/B300.txt", "binary-smallest name kept");
        assert!(files.contains(&"src/B599.txt".to_string()), "keeps lexicographically smallest 500");
        assert!(!files.contains(&"src/a299.txt".to_string()), "not walk-order 500");
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
    }

    #[test]
    fn test_list_files_glob_and_path() {
        let tmp = fixture_worktree();
        let wt = tmp.path();
        let (files, truncated) = list_files(wt, None, Some("**/*.java")).unwrap();
        assert!(!truncated);
        assert_eq!(files, vec!["src/main/Bar.java".to_string()], "glob filter + gitignore");

        let (files, _) = list_files(wt, Some("src/main"), None).unwrap();
        assert_eq!(files, vec!["src/main/Bar.java".to_string()]);

        let (_, truncated) = list_files(wt, None, None).unwrap();
        assert!(!truncated);
        assert!(list_files(wt, Some("../"), None).is_err(), "escape rejected");
    }
}
