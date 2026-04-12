mod logging;

use anyhow::Result;
use ccs_core::metadata::LazyFsSource;
use ccs_core::session::{self, SessionFile, SessionKind};
use ccs_tui::ui::picker::PickerState;
use ccs_tui::{App, Screen};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::logging::LogFormat;

#[derive(Parser, Debug)]
#[command(name = "claude-code-scrollback", version, about)]
struct Cli {
    /// Open the active session directly in live-tail mode, skipping the picker.
    #[arg(long)]
    live: bool,

    /// Log filter directive (overrides `RUST_LOG`). Accepts any
    /// `tracing_subscriber::EnvFilter` value, e.g. `debug` or
    /// `ccs_core=trace,info`.
    #[arg(long, value_name = "FILTER")]
    log_level: Option<String>,

    /// Log output format.
    #[arg(long, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,

    /// Open a specific session. Accepts either:
    ///
    /// * a session id (or id prefix), resolved against `~/.claude/projects/`, or
    /// * a direct path to a `.jsonl` session file.
    session: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let _log_guard = logging::init(cli.log_level.as_deref(), cli.log_format)?;

    let initial = match (cli.live, cli.session) {
        (true, _) => Screen::Viewer {
            live: true,
            session: None,
            state: None,
        },
        (false, Some(target)) => Screen::Viewer {
            live: false,
            session: resolve_session_target(&target)?,
            state: None,
        },
        (false, None) => Screen::Picker(build_picker()?),
    };

    let mut terminal = ccs_tui::init();
    let result = App::new(initial).run(&mut terminal);
    ccs_tui::restore();
    result
}

/// Walk `~/.claude/projects/` and build the picker state. All discovered
/// sessions are kept so the user can always reach them, but they are ranked
/// by [`ccs_tui::ui::picker::cwd_affinity`] against the current working
/// directory so sessions for the project you launched from bubble to the
/// top. This replaces a hard filter-by-cwd, which was too aggressive and
/// hid relevant sessions from nearby directories.
fn build_picker() -> Result<PickerState> {
    let sessions = discover_sessions()?;
    let cwd = std::env::current_dir().ok();
    Ok(PickerState::new(
        sessions,
        Box::new(LazyFsSource),
        cwd.as_deref(),
    ))
}

fn discover_sessions() -> Result<Vec<SessionFile>> {
    let Some(root) = session::projects_root() else {
        return Ok(Vec::new());
    };
    session::discover(&root)
}

/// Resolve the `session` positional arg into an optional [`SessionFile`].
///
/// Tries in order:
/// 1. If `target` points at an existing file on disk, open it directly —
///    this is how `claude-code-scrollback path/to/session.jsonl` works,
///    including files outside `~/.claude/projects/`.
/// 2. Otherwise, treat `target` as a session id prefix and resolve it
///    against the project directory via [`discover_sessions`].
fn resolve_session_target(target: &str) -> Result<Option<SessionFile>> {
    let path = Path::new(target);
    if path.is_file() {
        return Ok(Some(session_file_from_path(path)?));
    }
    Ok(discover_sessions()?
        .into_iter()
        .find(|s| s.session_id.starts_with(target)))
}

/// Synthesize a [`SessionFile`] for an arbitrary path on disk. Used by the
/// `<path>` positional form, which intentionally bypasses project-root
/// discovery so users can open JSONLs from anywhere (fixtures, copies,
/// etc.). `session_id` is derived from the file stem, `project_cwd` from
/// the parent directory.
fn session_file_from_path(path: &Path) -> Result<SessionFile> {
    let abs: PathBuf = path.canonicalize()?;
    let metadata = std::fs::metadata(&abs)?;
    let session_id = abs
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();
    let project_cwd = abs
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(SessionFile {
        path: abs,
        session_id,
        parent_session_id: None,
        kind: SessionKind::Primary,
        project_cwd,
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        size: metadata.len(),
    })
}
