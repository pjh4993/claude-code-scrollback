//! Logging layer initialization.
//!
//! The TUI owns the terminal via an alternate screen buffer, so writing logs
//! to stderr corrupts the UI. By default we emit rotating daily files under
//! the user's cache directory; `RUST_LOG` and the CLI flags decide verbosity
//! and format.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::ValueEnum;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

/// Return the directory where log files live. Falls back to a temp-dir path
/// if the OS cache dir cannot be resolved.
pub fn log_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("claude-code-scrollback")
        .join("logs")
}

/// Initialize the global tracing subscriber. Returns a [`WorkerGuard`] that
/// must be held for the lifetime of the process — dropping it flushes and
/// stops the background writer thread.
///
/// Resolution order for the filter:
/// 1. `--log-level` CLI flag, if provided.
/// 2. `RUST_LOG` environment variable.
/// 3. `info` as the default.
pub fn init(level: Option<&str>, format: LogFormat) -> Result<WorkerGuard> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create log directory at {}", dir.display()))?;

    let file_appender = rolling::daily(&dir, "ccs.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = match level {
        Some(l) => {
            EnvFilter::try_new(l).with_context(|| format!("invalid --log-level value: {l}"))?
        }
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };

    // Stderr is best-effort: only emit WARN+ so it doesn't mangle the TUI,
    // and only when stderr is not a tty (e.g. the user redirected 2> file).
    let stderr_writer = std::io::stderr.with_max_level(tracing::Level::WARN);

    let registry = tracing_subscriber::registry().with(filter);

    match format {
        LogFormat::Text => {
            let file_layer = fmt::layer().with_ansi(false).with_writer(file_writer);
            let stderr_layer = fmt::layer().with_ansi(false).with_writer(stderr_writer);
            registry.with(file_layer).with(stderr_layer).init();
        }
        LogFormat::Json => {
            let file_layer = fmt::layer().json().with_writer(file_writer);
            let stderr_layer = fmt::layer().json().with_writer(stderr_writer);
            registry.with(file_layer).with(stderr_layer).init();
        }
    }

    tracing::info!(dir = %dir.display(), "logging initialized");
    Ok(guard)
}
