use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init(app_data_dir: PathBuf) -> WorkerGuard {
    let log_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = rolling::daily(&log_dir, "friday.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking))
        .init();

    tracing::info!(?log_dir, "logging initialized");
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_init_creates_log_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        assert!(!log_dir.exists());

        let _guard = init(tmp.path().to_path_buf());
        assert!(log_dir.exists());
    }
}
