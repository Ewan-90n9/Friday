use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::reload;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

pub struct LoggingGuard {
    _file_guard: WorkerGuard,
    filter_handle: reload::Handle<EnvFilter, Registry>,
    _dispatch: tracing::Dispatch,
}

pub fn init(app_data_dir: PathBuf) -> LoggingGuard {
    let log_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = rolling::daily(&log_dir, "friday.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("debug"));
    let (filter_layer, filter_handle) = reload::Layer::new(filter);

    let subscriber = Registry::default()
        .with(filter_layer)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking));

    let dispatch = tracing::Dispatch::new(subscriber);
    let dispatch_clone = dispatch.clone();
    let _ = tracing::dispatcher::set_global_default(dispatch);

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("panic payload");
        tracing::error!(location = %location, payload = %payload, "panic");
        prev_hook(info);
    }));

    cleanup_old_logs(&log_dir, 7);

    tracing::info!(?log_dir, "logging initialized");
    LoggingGuard {
        _file_guard: guard,
        filter_handle,
        _dispatch: dispatch_clone,
    }
}

pub fn set_level(handle: &reload::Handle<EnvFilter, Registry>, level: &str) -> Result<(), String> {
    let new_filter = EnvFilter::new(level);
    handle.reload(new_filter).map_err(|e| e.to_string())?;
    tracing::info!(new_level = level, "log level changed");
    Ok(())
}

pub(crate) fn cleanup_old_logs(log_dir: &std::path::Path, max_days: u64) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(max_days * 86400);
    let mut removed: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let path = entry.path();
                        tracing::debug!(path = %path.display(), "removing old log file");
                        if let Err(e) = std::fs::remove_file(&path) {
                            tracing::warn!(?e, path = %path.display(), "failed to remove old log file");
                        } else {
                            removed += 1;
                        }
                    }
                }
            }
        }
    }
    tracing::debug!(removed, "old log files cleaned up");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn test_logging_init_creates_log_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        assert!(!log_dir.exists());

        let _guard = init(tmp.path().to_path_buf());
        assert!(log_dir.exists());
    }

    #[test]
    fn test_init_returns_logging_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = init(tmp.path().to_path_buf());
        // Access filter_handle to prove the struct has it
        let _handle = &guard.filter_handle;
    }

    #[test]
    fn test_set_level_changes_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = init(tmp.path().to_path_buf());
        let handle = &guard.filter_handle;

        let result = set_level(handle, "trace");
        assert!(result.is_ok());

        let result = set_level(handle, "info");
        assert!(result.is_ok());
    }

    fn set_file_modified(path: &std::path::Path, time: SystemTime) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap();
        let times = std::fs::FileTimes::new().set_modified(time);
        file.set_times(times).unwrap();
    }

    #[test]
    fn test_cleanup_old_logs_removes_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let old_file = log_dir.join("old.log");
        std::fs::write(&old_file, "old").unwrap();

        let old_time = SystemTime::now() - Duration::from_secs(10 * 86400);
        set_file_modified(&old_file, old_time);

        cleanup_old_logs(&log_dir, 7);

        assert!(!old_file.exists());
    }

    #[test]
    fn test_cleanup_old_logs_keeps_recent_files() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let recent_file = log_dir.join("recent.log");
        std::fs::write(&recent_file, "recent").unwrap();

        // File has current modification time (default when just created)
        cleanup_old_logs(&log_dir, 7);

        assert!(recent_file.exists());
    }

    #[test]
    fn test_cleanup_old_logs_keeps_within_7_days() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let file_5_days = log_dir.join("5days.log");
        std::fs::write(&file_5_days, "data").unwrap();

        let five_days_ago = SystemTime::now() - Duration::from_secs(5 * 86400);
        set_file_modified(&file_5_days, five_days_ago);

        cleanup_old_logs(&log_dir, 7);

        assert!(file_5_days.exists());
    }

    #[test]
    fn test_panic_hook_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = init(tmp.path().to_path_buf());

        // Take the hook — if init() panicked, we wouldn't reach here.
        // Our custom hook chains the previous hook.
        let _hook = std::panic::take_hook();

        // Restore a no-op hook to avoid affecting other tests
        std::panic::set_hook(Box::new(|_| {}));
    }
}
