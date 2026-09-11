use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdout, ChildStderr, Command};
use tokio::time::{timeout, Duration};

use crate::agent::prompt;

pub struct AgentProcess {
    pub pid: u32,
    pub child: Child,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("无可用 agent，请先检测或手动添加")]
    NoActiveAgent,
    #[error("agent 二进制不存在：{path}")]
    BinaryMissing { path: String },
    #[error("启动 agent 失败：{0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("DB 查询失败：{0}")]
    Db(#[from] sqlx::Error),
}

/// On Windows, resolve past the .cmd/.ps1 shim to the native opencode.exe
/// to avoid argv truncation through cmd.exe's %* forwarding.
/// Based on multica's resolveOpenCodeNativeFromShim.
fn resolve_native_exe(path: &PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        // The shim is typically at <prefix>/opencode.cmd or <prefix>/opencode (shell script)
        // The native exe is at <prefix>/node_modules/opencode-ai/node_modules/opencode-windows-x64/bin/opencode.exe
        if path.extension().and_then(|e| e.to_str()) == Some("exe") {
            return path.clone();
        }

        let parent = match path.parent() {
            Some(p) => p,
            None => return path.clone(),
        };

        let candidates = [
            parent.join("node_modules").join("opencode-ai").join("node_modules").join("opencode-windows-x64").join("bin").join("opencode.exe"),
            parent.join("node_modules").join("opencode-ai").join("node_modules").join("opencode-windows-x64-baseline").join("bin").join("opencode.exe"),
            parent.join("node_modules").join("opencode-ai").join("node_modules").join("opencode-windows-arm64").join("bin").join("opencode.exe"),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return candidate.clone();
            }
        }

        path.clone()
    }

    #[cfg(not(windows))]
    {
        path.clone()
    }
}

struct CommandConfig {
    mode_args: &'static [&'static str],
    format_args: &'static [&'static str],
    session_flag: &'static str,
    needs_exe_resolution: bool,
}

fn command_config_for(provider: &str) -> CommandConfig {
    match provider {
        "opencode" => CommandConfig {
            mode_args: &["run"],
            format_args: &["--format", "json"],
            session_flag: "--session",
            needs_exe_resolution: true,
        },
        "codeagentcli" => CommandConfig {
            mode_args: &["-p"],
            format_args: &["--output-format", "stream-json", "--verbose", "--skip-safe-check"],
            session_flag: "--sessions",
            needs_exe_resolution: false,
        },
        _ => {
            tracing::warn!(provider, "unknown provider, falling back to opencode config");
            CommandConfig {
                mode_args: &["run"],
                format_args: &["--format", "json"],
                session_flag: "--session",
                needs_exe_resolution: true,
            }
        }
    }
}

/// Pick the first session-resume flag from `--help` output that the CLI
/// actually advertises, in candidate (preference) order.
/// `--sessions` and `--session-id` are not substrings of each other, so a
/// plain `contains()` check is sufficient and unambiguous.
fn pick_session_flag_from_help(help_text: &str, candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|flag| help_text.contains(flag))
}

/// Probe the agent CLI's `--help` output to determine which session-resume
/// flag it actually supports.
///
/// Different CodeAgentCLI versions use different flag names for resuming a
/// conversation: newer versions advertise `--sessions <id>`, while older ones
/// only know `--session-id`. Friday passes the session flag only when
/// resuming (`agent_session_id` is `Some`). If we hardcode `--sessions` and
/// the installed CLI is an older version, the CLI exits immediately with
/// `error: unknown option '--sessions'` and the user sees an error in the UI.
///
/// This runs `<exe> --help` (5s timeout), checks which candidate flag the
/// output advertises, and returns the first match. On probe failure (timeout,
/// IO error) or when no candidate is advertised, it falls back to the
/// preferred flag so behavior is no worse than before the probe existed.
async fn resolve_session_flag(exe_path: &Path, provider: &str, preferred: &'static str) -> &'static str {
    // Only codeagentcli has version-dependent flag names; opencode's
    // `--session` has been stable.
    if provider != "codeagentcli" {
        return preferred;
    }

    // Ordered fallback list. The preferred flag is tried first; if the CLI's
    // help output doesn't mention it, we fall through to the next candidate.
    let candidates: &[&'static str] = match preferred {
        "--sessions" => &["--sessions", "--session-id"],
        "--session-id" => &["--session-id", "--sessions"],
        _ => &[preferred],
    };

    let result = timeout(
        Duration::from_secs(5),
        Command::new(exe_path)
            .arg("--help")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output(),
    )
    .await;

    let output = match result {
        Ok(Ok(output)) => output,
        _ => {
            tracing::warn!(provider, "session-flag probe failed, using preferred flag");
            return preferred;
        }
    };
    let help_text = String::from_utf8_lossy(&output.stdout);

    match pick_session_flag_from_help(&help_text, candidates) {
        Some(flag) => {
            if flag != preferred {
                tracing::warn!(
                    provider,
                    preferred,
                    resolved = flag,
                    "session flag fallback: preferred flag not supported by installed CLI"
                );
            }
            flag
        }
        None => {
            tracing::warn!(
                provider,
                "session-flag probe found no supported candidate, using preferred flag"
            );
            preferred
        }
    }
}

/// 优先级：experiences（新会话记忆召回）> history（issue #21 全新会话重试的
/// 本地历史注入）> 普通 prompt。experiences 与 history 不会同时出现。
#[tracing::instrument(skip(pool))]
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    agent_session_id: Option<String>,
    prompt_override_path: Option<PathBuf>,
    experiences: Option<&[crate::knowledge::experience::Experience]>,
    history: Option<&str>,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str, provider) = row.ok_or(SpawnError::NoActiveAgent)?;
    let raw_path = PathBuf::from(&path_str);

    if !raw_path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let config = command_config_for(&provider);

    let exe_path = if config.needs_exe_resolution {
        resolve_native_exe(&raw_path)
    } else {
        raw_path.clone()
    };
    tracing::info!(
        raw_path = %raw_path.display(),
        exe_path = %exe_path.display(),
        provider = %provider,
        "resolved agent executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.args(config.mode_args)
        .args(config.format_args)
        .arg("--dangerously-skip-permissions");

    if let Some(ref id) = agent_session_id {
        let flag = resolve_session_flag(&exe_path, &provider, config.session_flag).await;
        cmd.arg(flag).arg(id);
    }

    let prompt_text = match (experiences.filter(|e| !e.is_empty()), history) {
        (Some(exps), _) => {
            prompt::build_prompt_with_experiences(&message, prompt_override_path.as_deref(), &session_id, exps)
        }
        (None, Some(history)) => {
            prompt::build_prompt_with_history(&message, prompt_override_path.as_deref(), &session_id, history)
        }
        (None, None) => {
            prompt::build_prompt(&message, prompt_override_path.as_deref(), &session_id)
        }
    };
    tracing::info!(prompt_len = prompt_text.len(), "prompt built");

    // Prompt is delivered via stdin, not as a positional argument.
    // This avoids Windows argv truncation (cmd.exe caps at 8191 chars)
    // and matches multica's approach.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Set PWD to the user's home directory so the agent doesn't pick up
    // the Friday project's AGENTS.md or .opencode/ config. Friday manages
    // the conversation context, not the host project's.
    if let Some(home) = dirs::home_dir() {
        cmd.env("PWD", &home);
        cmd.current_dir(&home);
    }

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    tracing::info!(pid, exe = %exe_path.display(), provider = %provider, "agent process spawned");

    // Write prompt to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        let msg = prompt_text.clone();
        tokio::spawn(async move {
            tracing::info!(msg_len = msg.len(), "writing prompt to stdin");
            if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                tracing::error!(?e, "failed to write prompt to stdin");
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::error!(?e, "failed to close stdin");
            }
            tracing::info!("stdin written and closed");
        });
    }

    let stdout = child.stdout.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stdout not piped"),
    ))?;

    let stderr = child.stderr.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stderr not piped"),
    ))?;

    Ok(AgentProcess { pid, child, stdout, stderr })
}

/// One-shot LLM call via agent CLI. No --sessions, no stream parsing.
/// Writes prompt to stdin, reads full stdout, returns the text output.
/// Used for summary generation and experience extraction.
#[tracing::instrument(skip(pool))]
pub async fn spawn_one_shot(
    pool: &sqlx::SqlitePool,
    prompt: String,
) -> Result<String, SpawnError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT path, provider FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str, provider) = row.ok_or(SpawnError::NoActiveAgent)?;
    let raw_path = PathBuf::from(&path_str);

    if !raw_path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    let config = command_config_for(&provider);

    let exe_path = if config.needs_exe_resolution {
        resolve_native_exe(&raw_path)
    } else {
        raw_path.clone()
    };
    tracing::info!(
        exe_path = %exe_path.display(),
        provider = %provider,
        "spawn_one_shot resolved agent executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.args(config.mode_args)
        .args(config.format_args)
        .arg("--dangerously-skip-permissions");

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(home) = dirs::home_dir() {
        cmd.env("PWD", &home);
        cmd.current_dir(&home);
    }

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or(SpawnError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no pid",
        )))?;

    tracing::info!(pid, "spawn_one_shot agent process spawned");

    if let Some(mut stdin) = child.stdin.take() {
        let msg = prompt.clone();
        tokio::spawn(async move {
            if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                tracing::error!(?e, "spawn_one_shot: failed to write prompt to stdin");
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::error!(?e, "spawn_one_shot: failed to close stdin");
            }
        });
    }

    let stderr = child.stderr.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stderr not piped"),
    ))?;
    let stderr_handle = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(raw = %line, "spawn_one_shot stderr line");
        }
    });

    let stdout = child.stdout.take().ok_or(SpawnError::SpawnFailed(
        std::io::Error::new(std::io::ErrorKind::Other, "stdout not piped"),
    ))?;

    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stdout).lines();
    let mut output = String::new();

    while let Ok(Some(line)) = lines.next_line().await {
        if provider == "opencode" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = v
                        .get("part")
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        output.push_str(text);
                    }
                }
            }
        } else {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                    if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                        output.push_str(result);
                    }
                } else if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
                    if let Some(content) = v
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in content {
                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    output.push_str(text);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = child.wait().await;
    let _ = stderr_handle.await;

    tracing::info!(output_len = output.len(), "spawn_one_shot completed");

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_spawn_active_accepts_session_id_param() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_active(&pool, "test-sid".to_string(), String::new(), None, None, None, None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_active(&pool, "test-session".to_string(), String::new(), None, None, None, None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_binary_missing_when_path_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();

        sqlx::query(
            "INSERT INTO agents (id, provider, display_name, path, version, source, is_active, detected_at, created_at) \
             VALUES ('test-id', 'opencode', 'OpenCode', '/nonexistent/path/opencode', NULL, 'manual', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = spawn_active(&pool, "test-session".to_string(), "test message".to_string(), None, None, None, None).await;
        assert!(matches!(result, Err(SpawnError::BinaryMissing { .. })));
    }

    #[tokio::test]
    async fn test_spawn_one_shot_accepts_session_id_param() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_one_shot(&pool, "test prompt".to_string()).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[test]
    fn test_resolve_native_exe_returns_exe_unchanged() {
        let path = PathBuf::from("/usr/bin/opencode");
        let resolved = resolve_native_exe(&path);
        assert_eq!(resolved, path);
    }

    #[cfg(windows)]
    #[test]
    fn test_resolve_native_exe_finds_native_on_windows() {
        // The opencode shim is at:
        // C:\Users\g00609569\AppData\Local\Microsoft\WinGet\Packages\OpenJS.NodeJS.23_Microsoft.Winget.Source_8wekyb3d8bbwe\node-v23.11.0-win-x64\opencode.cmd
        // The native exe is at:
        // <same prefix>\node_modules\opencode-ai\node_modules\opencode-windows-x64\bin\opencode.exe
        let shim_dir = std::env::var("LOCALAPPDATA")
            .map(|p| PathBuf::from(p)
                .join("Microsoft")
                .join("WinGet")
                .join("Packages")
                .join("OpenJS.NodeJS.23_Microsoft.Winget.Source_8wekyb3d8b")
                .join("node-v23.11.0-win-x64"))
            .unwrap();

        if shim_dir.exists() {
            let shim = shim_dir.join("opencode.cmd");
            if shim.exists() {
                let resolved = resolve_native_exe(&shim);
                assert_eq!(resolved.extension().unwrap(), "exe");
                assert!(resolved.exists());
            }
        }
    }

    #[test]
    fn test_command_config_for_opencode() {
        let config = command_config_for("opencode");
        assert_eq!(config.mode_args, &["run"]);
        assert_eq!(config.format_args, &["--format", "json"]);
        assert_eq!(config.session_flag, "--session");
        assert!(config.needs_exe_resolution);
    }

    #[test]
    fn test_command_config_for_codeagentcli() {
        let config = command_config_for("codeagentcli");
        assert_eq!(config.mode_args, &["-p"]);
        assert_eq!(config.format_args, &["--output-format", "stream-json", "--verbose", "--skip-safe-check"]);
        assert_eq!(config.session_flag, "--sessions");
        assert!(!config.needs_exe_resolution);
    }

    #[test]
    fn test_command_config_for_unknown_falls_back_to_opencode() {
        let config = command_config_for("unknown");
        assert_eq!(config.mode_args, &["run"]);
        assert_eq!(config.format_args, &["--format", "json"]);
        assert_eq!(config.session_flag, "--session");
        assert!(config.needs_exe_resolution);
    }

    #[test]
    fn test_pick_session_flag_matches_old_cli_session_id() {
        // Older CodeAgentCLI only advertises --session-id for resuming.
        let help = "usage: codeagentcli [options]\n  --session-id <uuid>  session to resume";
        let candidates = ["--sessions", "--session-id"];
        assert_eq!(
            pick_session_flag_from_help(help, &candidates),
            Some("--session-id")
        );
    }

    #[test]
    fn test_pick_session_flag_prefers_preferred_when_both_supported() {
        let help = "  --sessions <id>   resume session\n  --session-id <uuid>  set id for new conversation";
        let candidates = ["--sessions", "--session-id"];
        assert_eq!(
            pick_session_flag_from_help(help, &candidates),
            Some("--sessions")
        );
    }

    #[test]
    fn test_pick_session_flag_returns_none_when_no_candidate_supported() {
        let help = "usage: codeagentcli [options]\n  --version  print version";
        let candidates = ["--sessions", "--session-id"];
        assert_eq!(pick_session_flag_from_help(help, &candidates), None);
    }

    #[tokio::test]
    async fn test_resolve_session_flag_returns_preferred_for_opencode() {
        // opencode's `--session` has been stable — no probing, so a
        // nonexistent path must not matter and no subprocess is launched.
        let path = PathBuf::from("/nonexistent/opencode");
        let flag = resolve_session_flag(&path, "opencode", "--session").await;
        assert_eq!(flag, "--session");
    }

    #[tokio::test]
    async fn test_resolve_session_flag_falls_back_when_probe_fails() {
        // Nonexistent binary -> --help probe fails -> preferred flag returned.
        let path = PathBuf::from("/nonexistent/codeagentcli");
        let flag = resolve_session_flag(&path, "codeagentcli", "--sessions").await;
        assert_eq!(flag, "--sessions");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_resolve_session_flag_picks_old_cli_flag_from_help() {
        // A fake CLI whose --help only advertises --session-id (old
        // CodeAgentCLI). The probe must pick --session-id over the preferred
        // --sessions so resume works against old CLI versions.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-codeagentcli.cmd");
        std::fs::write(
            &fake,
            "@echo usage: fake-codeagentcli [options]\r\n@echo   --session-id ^<uuid^>  session to resume\r\n",
        )
        .unwrap();
        let flag = resolve_session_flag(&fake, "codeagentcli", "--sessions").await;
        assert_eq!(flag, "--session-id");
    }
}
