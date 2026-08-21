use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdout, ChildStderr};

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

#[tracing::instrument(skip(pool))]
pub async fn spawn_active(
    pool: &sqlx::SqlitePool,
    session_id: String,
    message: String,
    opencode_session_id: Option<String>,
) -> Result<AgentProcess, SpawnError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM agents WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let (path_str,) = row.ok_or(SpawnError::NoActiveAgent)?;
    let raw_path = PathBuf::from(&path_str);

    if !raw_path.exists() {
        return Err(SpawnError::BinaryMissing { path: path_str });
    }

    // On Windows, resolve to native .exe to avoid cmd.exe shim issues
    let exe_path = resolve_native_exe(&raw_path);
    tracing::info!(
        raw_path = %raw_path.display(),
        exe_path = %exe_path.display(),
        "resolved opencode executable"
    );

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.arg("run")
        .arg("--format")
        .arg("json")
        .arg("--dangerously-skip-permissions");

    if let Some(ref oc_id) = opencode_session_id {
        cmd.arg("--session").arg(oc_id);
    }

    // Prompt is delivered via stdin, not as a positional argument.
    // This avoids Windows argv truncation (cmd.exe caps at 8191 chars)
    // and matches multica's approach.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Set PWD to the user's home directory so opencode doesn't pick up
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

    tracing::info!(pid, exe = %exe_path.display(), "opencode process spawned");

    // Write prompt to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        let msg = message.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::db;

    #[tokio::test]
    async fn test_spawn_active_accepts_session_id_param() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_active(&pool, "test-sid".to_string(), String::new(), None).await;
        assert!(matches!(result, Err(SpawnError::NoActiveAgent)));
    }

    #[tokio::test]
    async fn test_spawn_active_returns_no_active_agent_when_db_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::init(tmp.path().join("friday.db")).await.unwrap();
        let result = spawn_active(&pool, "test-session".to_string(), String::new(), None).await;
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

        let result = spawn_active(&pool, "test-session".to_string(), "test message".to_string(), None).await;
        assert!(matches!(result, Err(SpawnError::BinaryMissing { .. })));
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
}
