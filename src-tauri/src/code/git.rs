//! git 子进程封装。认证完全依赖本机 git 配置（credential manager / .netrc /
//! URL 内嵌 token）；GIT_TERMINAL_PROMPT=0 防止终端交互挂死后台任务。
//! stderr 全量读取并记录（日志规范：不截断）。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// 目录名安全短哈希：sha256(url) 前 16 个十六进制字符
pub fn url_hash(repo_url: &str) -> String {
    let hex = format!("{:x}", Sha256::digest(repo_url.as_bytes()));
    hex[..16].to_string()
}

/// worktree 子目录名：sha256(ref) 前 16 个十六进制字符
pub fn ref_hash(git_ref: &str) -> String {
    let hex = format!("{:x}", Sha256::digest(git_ref.as_bytes()));
    hex[..16].to_string()
}

/// repo_id = hash(url + "\n" + ref)：同仓同 ref 幂等
pub fn repo_id(repo_url: &str, git_ref: &str) -> String {
    let mut h = Sha256::new();
    h.update(repo_url.as_bytes());
    h.update(b"\n");
    h.update(git_ref.as_bytes());
    let hex = format!("{:x}", h.finalize());
    hex[..16].to_string()
}

/// git 可用性检测（PATH）
pub fn git_available() -> bool {
    which::which("git").is_ok()
}

/// URL 策略：允许 https:// 与 file://（内网裸 git 服务 / 测试 fixture）；
/// ssh://、git@ 明确拒绝（公司强制 https）。
pub fn validate_repo_url(repo_url: &str) -> Result<(), String> {
    let url = repo_url.trim();
    if url.is_empty() {
        return Err("repo_url cannot be empty".to_string());
    }
    if url.starts_with("ssh://") || url.starts_with("git@") {
        return Err(
            "SSH protocol is not allowed (company policy: HTTPS only); convert to an https:// URL"
                .to_string(),
        );
    }
    if !(url.starts_with("https://") || url.starts_with("file://")) {
        return Err("repo_url must start with https:// or file://".to_string());
    }
    if url.chars().any(|c| c.is_whitespace() || c == ';') {
        return Err("repo_url contains unsupported characters (whitespace / semicolon)".to_string());
    }
    Ok(())
}

/// 本地路径 → file:// URL（Windows 反斜杠转正斜杠）
pub fn file_url(path: &Path) -> String {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
}

pub struct GitResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

fn base_command(args: &[&str], cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd
}

fn log_result(args: &[&str], res: &GitResult) {
    let cmd_line = args.join(" ");
    if !res.stderr.trim().is_empty() {
        // git 进度输出以 \r 刷新；规范化为 \n 后全量记录（不截断）；
        // 失败命令的 stderr 提级 warn（日志规范：错误路径可观测）
        if res.success {
            tracing::debug!(cmd = %cmd_line, stderr = %res.stderr.replace('\r', "\n"), "git stderr");
        } else {
            tracing::warn!(cmd = %cmd_line, stderr = %res.stderr.replace('\r', "\n"), "git stderr");
        }
    }
    tracing::info!(cmd = %cmd_line, success = res.success, "git finished");
}

/// 运行 git 命令，超时强杀
pub async fn run_git(args: &[&str], cwd: Option<&Path>, timeout: Duration) -> GitResult {
    let mut child = match base_command(args, cwd).spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "failed to spawn git");
            return GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("failed to spawn git: {e}"),
            };
        }
    };
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    // stdout/stderr 并发读，防单流管道缓冲写满死锁；lossy 解码防非 UTF-8 字节丢流
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout_pipe.read_to_end(&mut buf).await.ok();
        String::from_utf8_lossy(&buf).into_owned()
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr_pipe.read_to_end(&mut buf).await.ok();
        String::from_utf8_lossy(&buf).into_owned()
    });
    let result = tokio::time::timeout(timeout, child.wait()).await;
    let res = match result {
        Ok(Ok(status)) => GitResult {
            success: status.success(),
            stdout: out_task.await.unwrap_or_default(),
            stderr: err_task.await.unwrap_or_default(),
        },
        Ok(Err(e)) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "git wait failed");
            GitResult { success: false, stdout: String::new(), stderr: format!("git wait failed: {e}") }
        }
        Err(_) => {
            // kill_on_drop 兜底；显式 kill 加速退出
            let _ = child.start_kill();
            let partial_stderr = match tokio::time::timeout(Duration::from_secs(5), err_task).await {
                Ok(joined) => joined.unwrap_or_default(),
                Err(_) => {
                    tracing::warn!(cmd = %args.join(" "), "stderr drain did not finish after kill; discarding");
                    String::new()
                }
            };
            if tokio::time::timeout(Duration::from_secs(5), out_task).await.is_err() {
                tracing::warn!(cmd = %args.join(" "), "stdout drain did not finish after kill; discarding");
            }
            tracing::warn!(cmd = %args.join(" "), timeout_secs = timeout.as_secs(), stderr = %partial_stderr.replace('\r', "\n"), "git command timed out, killed");
            GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("git {} timed out after {}s\n{}", args.join(" "), timeout.as_secs(), partial_stderr),
            }
        }
    };
    log_result(args, &res);
    res
}

/// 运行 git 命令并流式解析 stderr 进度（clone/fetch），进度经 channel 实时上报
pub async fn run_git_streaming(
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    progress_tx: UnboundedSender<String>,
) -> GitResult {
    let mut child = match base_command(args, cwd).spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "failed to spawn git");
            return GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("failed to spawn git: {e}"),
            };
        }
    };
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let out_task = tokio::spawn(async move {
        let mut s = String::new();
        stdout_pipe.read_to_string(&mut s).await.ok();
        s
    });
    let err_task = tokio::spawn(async move {
        drain_stderr_with_progress(stderr_pipe, progress_tx).await
    });
    let result = tokio::time::timeout(timeout, child.wait()).await;
    let res = match result {
        Ok(Ok(status)) => GitResult {
            success: status.success(),
            stdout: out_task.await.unwrap_or_default(),
            stderr: err_task.await.unwrap_or_default(),
        },
        Ok(Err(e)) => {
            tracing::error!(cmd = %args.join(" "), error = ?e, "git wait failed");
            GitResult { success: false, stdout: String::new(), stderr: format!("git wait failed: {e}") }
        }
        Err(_) => {
            let _ = child.start_kill();
            let partial_stderr = match tokio::time::timeout(Duration::from_secs(5), err_task).await {
                Ok(joined) => joined.unwrap_or_default(),
                Err(_) => {
                    tracing::warn!(cmd = %args.join(" "), "stderr drain did not finish after kill; discarding");
                    String::new()
                }
            };
            if tokio::time::timeout(Duration::from_secs(5), out_task).await.is_err() {
                tracing::warn!(cmd = %args.join(" "), "stdout drain did not finish after kill; discarding");
            }
            tracing::warn!(cmd = %args.join(" "), timeout_secs = timeout.as_secs(), stderr = %partial_stderr.replace('\r', "\n"), "git command timed out, killed");
            GitResult {
                success: false,
                stdout: String::new(),
                stderr: format!("git {} timed out after {}s\n{}", args.join(" "), timeout.as_secs(), partial_stderr),
            }
        }
    };
    log_result(args, &res);
    res
}

/// 增量读取 stderr：每读完一个 \r / \n 分隔段立即解析转发进度（长 clone 实时
/// 上报），同时保留全量原始字节，最终以 lossy UTF-8 返回完整文本。
async fn drain_stderr_with_progress<R: AsyncRead + Unpin>(
    reader: R,
    tx: UnboundedSender<String>,
) -> String {
    let mut reader = BufReader::new(reader);
    let mut full = Vec::new();
    let mut seg = Vec::new();
    loop {
        let available = match reader.fill_buf().await {
            Ok(buf) => buf,
            Err(_) => break,
        };
        if available.is_empty() {
            break;
        }
        for &b in available {
            full.push(b);
            if b == b'\r' || b == b'\n' {
                let text = String::from_utf8_lossy(&seg);
                if let Some(p) = parse_progress(&text) {
                    let _ = tx.send(p);
                }
                seg.clear();
            } else {
                seg.push(b);
            }
        }
        let len = available.len();
        reader.consume(len);
    }
    if !seg.is_empty() {
        let text = String::from_utf8_lossy(&seg);
        if let Some(p) = parse_progress(&text) {
            let _ = tx.send(p);
        }
    }
    String::from_utf8_lossy(&full).into_owned()
}

/// 解析 git 进度行，如 "Receiving objects:  45% (123/273), 1.2 MiB | 2.3 MiB/s"
pub fn parse_progress(line: &str) -> Option<String> {
    let trimmed = line.trim_start_matches("remote: ").trim();
    let (label, rest) = trimmed.split_once(':')?;
    if !matches!(
        label,
        "Enumerating objects"
            | "Counting objects"
            | "Compressing objects"
            | "Receiving objects"
            | "Resolving deltas"
            | "Updating files"
    ) {
        return None;
    }
    let rest = rest.trim();
    let pct: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '%').collect();
    if pct.ends_with('%') && !pct.starts_with('%') {
        Some(format!("{label}: {pct}"))
    } else {
        None
    }
}

/// ref → commit sha。分支名本地不存在时回退 origin/<ref>（clone 后非默认分支
/// 只有 remote-tracking 引用）。tag / commitid 直接命中第一轮。
pub async fn resolve_ref(clone_dir: &Path, git_ref: &str) -> Option<String> {
    for candidate in [git_ref.to_string(), format!("origin/{git_ref}")] {
        let peeled = format!("{candidate}^{{commit}}");
        let out = run_git(
            &["-C", &clone_dir.display().to_string(), "rev-parse", "--verify", &peeled],
            Some(clone_dir),
            Duration::from_secs(120),
        )
        .await;
        if out.success {
            return Some(out.stdout.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_hash_stable_16_hex() {
        let a = url_hash("https://git.example.com/bar.git");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, url_hash("https://git.example.com/bar.git"));
        assert_ne!(a, url_hash("https://git.example.com/baz.git"));
    }

    #[test]
    fn test_ref_hash_and_repo_id() {
        assert_eq!(ref_hash("main").len(), 16);
        let id1 = repo_id("https://a.git", "main");
        assert_eq!(id1, repo_id("https://a.git", "main"));
        assert_ne!(id1, repo_id("https://a.git", "v1.0"));
        assert_ne!(id1, repo_id("https://b.git", "main"));
    }

    #[test]
    fn test_validate_repo_url_rules() {
        assert!(validate_repo_url("https://gitlab.example.com/foo/bar.git").is_ok());
        assert!(validate_repo_url("file:///C:/some/repo").is_ok());
        assert!(validate_repo_url("ssh://git@example.com/foo.git").is_err());
        assert!(validate_repo_url("git@example.com:foo/bar.git").is_err());
        assert!(validate_repo_url("ftp://example.com/x").is_err());
        assert!(validate_repo_url("example.com/x").is_err());
        assert!(validate_repo_url("").is_err());
        assert!(validate_repo_url("https://x.com/a b").is_err());
    }

    #[test]
    fn test_parse_progress_variants() {
        assert_eq!(
            parse_progress("Receiving objects:  45% (123/273), 1.20 MiB | 2.30 MiB/s"),
            Some("Receiving objects: 45%".to_string())
        );
        assert_eq!(
            parse_progress("remote: Enumerating objects: 12, done."),
            None
        );
        assert_eq!(parse_progress("Resolving deltas: 100% (273/273)"), Some("Resolving deltas: 100%".to_string()));
        assert_eq!(parse_progress("Unpacking objects: 100%"), None); // 不在白名单
    }

    #[tokio::test]
    async fn test_drain_stderr_with_progress_incremental() {
        use tokio::io::{duplex, AsyncWriteExt};
        let (mut writer, reader) = duplex(1024);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let drain = tokio::spawn(drain_stderr_with_progress(reader, tx));
        writer
            .write_all(b"Receiving objects:  10% (1/10)\r")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(rx.try_recv().unwrap(), "Receiving objects: 10%");
        writer
            .write_all(b"remote: Enumerating objects: 12, done.\n")
            .await
            .unwrap();
        writer
            .write_all(b"Receiving objects:  50% (5/10)\r")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(rx.try_recv().unwrap(), "Receiving objects: 50%");
        drop(writer);
        let text = drain.await.unwrap();
        assert_eq!(
            text,
            "Receiving objects:  10% (1/10)\rremote: Enumerating objects: 12, done.\nReceiving objects:  50% (5/10)\r"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_file_url_clone_roundtrip() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        assert!(url.starts_with("file:///"));
        let dst = tempfile::tempdir().unwrap();
        let res = run_git(
            &["clone", &url, &dst.path().join("clone").display().to_string()],
            None,
            Duration::from_secs(120),
        )
        .await;
        assert!(res.success, "clone failed: {}", res.stderr);
        let sha = resolve_ref(&dst.path().join("clone"), "v1.0").await;
        assert!(sha.is_some());
        let feature = resolve_ref(&dst.path().join("clone"), "feature-x").await;
        assert!(feature.is_some(), "remote-tracking fallback for non-default branch");
    }
}
