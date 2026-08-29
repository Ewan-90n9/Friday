use super::state::{Status, TransferState};
use super::TransferManager;
use crate::exec::channel::ExecChannel;
use crate::exec::pool::{build_transport, fetch_environment};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_ATTEMPTS: u32 = 5;
const MAX_TOTAL_SECS: u64 = 7200; // 2 小时重试预算
const BACKOFF_SECS: [u64; 5] = [5, 15, 45, 120, 360];

/// 建一条专用连接（不走 ExecChannelPool）
async fn dedicated_channel(
    db: &sqlx::SqlitePool,
    env_id: &str,
) -> Result<Arc<dyn ExecChannel>, String> {
    let env = fetch_environment(db, env_id).await.map_err(|e| e.to_string())?;
    let transport = build_transport(env_id, &env).map_err(|e| e.to_string())?;
    transport.connect().await.map_err(|e| e.to_string())?;
    Ok(Arc::new(transport))
}

/// stat 远端文件大小；None = 文件不存在或 stat 失败
async fn stat_remote(channel: &Arc<dyn ExecChannel>, remote_path: &str) -> Option<u64> {
    let cmd = format!("stat -c %s {}", crate::exec::ssh::shell_quote_single(remote_path));
    match channel.run(&cmd).await {
        Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().ok(),
        _ => None,
    }
}

/// 下载 worker：断点续传 + 重试。进度经 manager.update_progress 上报。
pub async fn run_download(mgr: Arc<TransferManager>, state: TransferState, cancel: CancellationToken) {
    let id = state.id.clone();
    let env_id = state.env_id.clone();
    let remote_path = state.remote_path.clone();
    let local = state.local_path.clone();
    let cleanup = state.cleanup_remote_on_success;
    let part = super::state::part_path_for(&local);
    let session_id = state.session_id.clone();
    let started = std::time::Instant::now();
    let mut attempt: u32 = 0;

    tracing::info!(transfer_id = %id, session_id = %session_id, env_id = %env_id, remote_path = %remote_path, "transfer worker: download starting");

    loop {
        if cancel.is_cancelled() {
            mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
            return;
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS || started.elapsed().as_secs() > MAX_TOTAL_SECS {
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!(
                    "传输失败：重试次数用尽（{remote_path}）。远端文件保留，可重新调用 file_download 断点续传。"
                )),
                0, 0,
            ).await;
            return;
        }

        // backoff（首次不等待）
        if attempt > 1 {
            let wait = BACKOFF_SECS[((attempt - 2) as usize).min(BACKOFF_SECS.len() - 1)];
            mgr.update_progress(&id, Status::Retrying, 0, 0, 0, attempt).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                _ = cancel.cancelled() => {
                    mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
                    return;
                }
            }
        }

        let channel = match dedicated_channel(mgr.db(), &env_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: connect failed");
                continue;
            }
        };
        mgr.update_progress(&id, Status::Connecting, 0, 0, 0, attempt).await;

        let Some(total) = stat_remote(&channel, &remote_path).await else {
            channel.disconnect().await;
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!("远端文件不存在或无法读取: {remote_path}")),
                0, 0,
            ).await;
            return; // 终态不重试
        };

        // 断点续传：本地 .part 已有字节数为偏移
        let offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0).min(total);
        let transferred_cell = Arc::new(std::sync::Mutex::new(offset));
        let speed_cell = Arc::new(std::sync::Mutex::new(0u64));

        // 实时进度 ticker：1s 轮询 transferred_cell 上报（download 回调是同步的，
        // 不能在里面 await manager，所以用 ticker task 转发）
        let mgr_t = mgr.clone();
        let id_t = id.clone();
        let tc = transferred_cell.clone();
        let sc = speed_cell.clone();
        let ticker = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let t = *tc.lock().unwrap();
                let s = *sc.lock().unwrap();
                mgr_t.update_progress(&id_t, Status::Transferring, t, total, s, attempt).await;
            }
        });

        let (tc2, sc2) = (transferred_cell.clone(), speed_cell.clone());
        let progress_cb = move |t: u64, s: u64| {
            *tc2.lock().unwrap() = t;
            *sc2.lock().unwrap() = s;
        };
        let result = tokio::select! {
            r = channel.download(&remote_path, &part, offset, &progress_cb) => r,
            _ = cancel.cancelled() => {
                ticker.abort();
                channel.disconnect().await;
                mgr.finish(&id, Status::Cancelled, None, 0, 0).await;
                return;
            }
        };
        ticker.abort();

        let transferred_now = *transferred_cell.lock().unwrap();
        let speed_now = *speed_cell.lock().unwrap();
        tracing::debug!(transfer_id = %id, transferred_now, speed_now, "transfer: download attempt ended");
        channel.disconnect().await;

        match result {
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: download interrupted");
                continue; // 重试（.part 保留，下次续传）
            }
            Ok(()) => {
                // 大小校验
                let local_size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if local_size != total {
                    tracing::warn!(transfer_id = %id, local_size, total, "transfer: size mismatch, restarting");
                    let _ = std::fs::remove_file(&part);
                    continue; // 损坏：删 .part 从头重试
                }
                // rename 成正式文件
                if let Err(e) = std::fs::rename(&part, &local) {
                    mgr.finish(&id, Status::Failed, Some(format!("本地文件落盘失败: {e}")), local_size, total).await;
                    return;
                }
                // heap_dump 场景：下载成功后清理远端（失败仅告警）
                if cleanup {
                    let rm = channel_cmd_after_disconnect(&mgr, &env_id, &remote_path).await;
                    if let Err(e) = rm {
                        tracing::warn!(transfer_id = %id, error = %e, "transfer: remote cleanup failed (kept)");
                    }
                }
                mgr.finish(&id, Status::Completed, None, total, total).await;
                return;
            }
        }
    }
}

/// 清理远端文件：再开一条短连接执行 rm（原连接已断开）
async fn channel_cmd_after_disconnect(
    mgr: &TransferManager,
    env_id: &str,
    remote_path: &str,
) -> Result<(), String> {
    let channel = dedicated_channel(mgr.db(), env_id).await?;
    let cmd = format!("rm -f {}", crate::exec::ssh::shell_quote_single(remote_path));
    let out = channel.run(&cmd).await.map_err(|e| e.to_string())?;
    channel.disconnect().await;
    if out.exit_code == 0 {
        Ok(())
    } else {
        Err(format!("rm exit {}: {}", out.exit_code, out.stderr))
    }
}

/// 上传 worker：整体重传 + 大小校验
pub async fn run_upload(mgr: Arc<TransferManager>, state: TransferState, cancel: CancellationToken) {
    let id = state.id.clone();
    let env_id = state.env_id.clone();
    let remote_path = state.remote_path.clone();
    let local = state.local_path.clone();
    let session_id = state.session_id.clone();
    let started = std::time::Instant::now();
    let mut attempt: u32 = 0;

    let Ok(meta) = std::fs::metadata(&local) else {
        mgr.finish(&id, Status::Failed, Some(format!("本地文件不存在: {}", local.display())), 0, 0).await;
        return;
    };
    let total = meta.len();

    tracing::info!(transfer_id = %id, session_id = %session_id, env_id = %env_id, remote_path = %remote_path, total, "transfer worker: upload starting");

    loop {
        if cancel.is_cancelled() {
            mgr.finish(&id, Status::Cancelled, None, 0, total).await;
            return;
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS || started.elapsed().as_secs() > MAX_TOTAL_SECS {
            mgr.finish(
                &id,
                Status::Failed,
                Some(format!("上传失败：重试次数用尽（远端 {remote_path} 可能有残留半成品，重新上传会覆盖）。")),
                0, total,
            ).await;
            return;
        }
        if attempt > 1 {
            let wait = BACKOFF_SECS[((attempt - 2) as usize).min(BACKOFF_SECS.len() - 1)];
            mgr.update_progress(&id, Status::Retrying, 0, total, 0, attempt).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                _ = cancel.cancelled() => {
                    mgr.finish(&id, Status::Cancelled, None, 0, total).await;
                    return;
                }
            }
        }

        let channel = match dedicated_channel(mgr.db(), &env_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: connect failed");
                continue;
            }
        };
        mgr.update_progress(&id, Status::Connecting, 0, total, 0, attempt).await;

        let result = tokio::select! {
            r = channel.upload(&local, &remote_path) => r,
            _ = cancel.cancelled() => {
                channel.disconnect().await;
                mgr.finish(&id, Status::Cancelled, None, 0, total).await;
                return;
            }
        };
        channel.disconnect().await;

        match result {
            Err(e) => {
                tracing::warn!(transfer_id = %id, env_id = %env_id, attempt, error = %e, "transfer: upload interrupted");
                continue;
            }
            Ok(()) => {
                // 远端大小校验：再开短连接 stat
                let check = match dedicated_channel(mgr.db(), &env_id).await {
                    Ok(c) => {
                        let size = stat_remote(&c, &remote_path).await;
                        c.disconnect().await;
                        Ok(size)
                    }
                    Err(e) => Err(e),
                };
                match check {
                    Ok(Some(remote_size)) if remote_size == total => {
                        mgr.finish(&id, Status::Completed, None, total, total).await;
                        return;
                    }
                    Ok(_) => {
                        tracing::warn!(transfer_id = %id, "transfer: remote size mismatch after upload, retrying");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(transfer_id = %id, error = %e, "transfer: remote stat failed after upload, retrying");
                        continue;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_sequence() {
        assert_eq!(BACKOFF_SECS, [5, 15, 45, 120, 360]);
        assert_eq!(MAX_ATTEMPTS, 5);
    }

    #[test]
    fn test_offset_clamped_to_total() {
        // offset = min(.part 大小, total)
        let offset = 300u64.min(200);
        assert_eq!(offset, 200);
    }

    #[tokio::test]
    async fn test_stat_remote_quotes_path() {
        // mock：记录收到的命令，验证路径被单引号包裹
        use crate::exec::channel::{ExecChannel, ExecOutput};
        use async_trait::async_trait;
        struct StatChan(tokio::sync::Mutex<Vec<String>>);
        #[async_trait]
        impl ExecChannel for StatChan {
            async fn run(&self, cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
                self.0.lock().await.push(cmd.to_string());
                Ok(ExecOutput { stdout: "12345\n".into(), stderr: String::new(), exit_code: 0 })
            }
            async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
            async fn disconnect(&self) {}
            async fn is_alive(&self) -> bool { true }
        }
        let ch = Arc::new(StatChan(tokio::sync::Mutex::new(Vec::new())));
        let dyn_ch: Arc<dyn ExecChannel> = ch.clone();
        let size = stat_remote(&dyn_ch, "/tmp/a b.hprof").await;
        assert_eq!(size, Some(12345));
        let calls = ch.0.lock().await;
        assert!(calls[0].contains("'/tmp/a b.hprof'"), "cmd: {}", calls[0]);
    }

    #[tokio::test]
    async fn test_stat_remote_missing_returns_none() {
        use crate::exec::channel::{ExecChannel, ExecOutput};
        use async_trait::async_trait;
        struct NoFileChan;
        #[async_trait]
        impl ExecChannel for NoFileChan {
            async fn run(&self, _cmd: &str) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
                Ok(ExecOutput { stdout: String::new(), stderr: "No such file".into(), exit_code: 1 })
            }
            async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }
            async fn disconnect(&self) {}
            async fn is_alive(&self) -> bool { true }
        }
        let ch: Arc<dyn ExecChannel> = Arc::new(NoFileChan);
        assert_eq!(stat_remote(&ch, "/tmp/gone").await, None);
    }
}
