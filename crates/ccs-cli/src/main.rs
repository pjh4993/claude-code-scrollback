mod logging;

use anyhow::Result;
use ccs_core::metadata::LazyFsSource;
use ccs_core::session::{self, SessionFile, SessionKind};
use ccs_tui::ui::picker::PickerState;
use ccs_tui::{App, Screen};
use clap::Parser;
use std::io::IsTerminal;
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
        (true, explicit) => {
            // `--live` with an explicit session id/path opens that one
            // and follows it; `--live` alone auto-detects the active
            // session under the current cwd's project. An explicit
            // target that resolves to nothing is a hard error — live
            // mode has no useful empty state and a silent fall-through
            // to an empty viewer would bury the typo.
            let session = match explicit {
                Some(target) => match resolve_session_target(&target)? {
                    Some(s) => Some(s),
                    None => anyhow::bail!("unknown session id or path: {target}"),
                },
                None => resolve_active_session()?,
            };
            Screen::viewer(true, session)
        }
        (false, Some(target)) => {
            // Non-live mode: a bad positional arg is a hard error too.
            // Without this the empty `Screen::viewer(false, None)`
            // opens, which initialises the TUI just to show "no
            // session loaded" — worse than a one-line shell error.
            let session = match resolve_session_target(&target)? {
                Some(s) => Some(s),
                None => anyhow::bail!("unknown session id or path: {target}"),
            };
            Screen::viewer(false, session)
        }
        (false, None) => Screen::Picker(build_picker()?),
    };

    // TUI requires a real terminal. Bail cleanly if we were piped or
    // redirected — otherwise ratatui's init panics inside the
    // alternate-screen-buffer sequence, which leaves the user's
    // terminal in a bad state. First-run experience is one of
    // PJH-53's success criteria; this keeps it clean.
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "claude-code-scrollback requires an interactive terminal (stdout is not a TTY)"
        );
    }

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
    let (sessions, stats, root) = discover_sessions()?;
    let cwd = std::env::current_dir().ok();
    let mut picker = PickerState::new(sessions, Box::new(LazyFsSource), cwd.as_deref());
    picker.set_discovery_info(stats.skipped_dirs, root);
    Ok(picker)
}

/// Returns the discovered sessions, discovery stats, and the actual
/// projects root used — the picker surfaces the root in its empty
/// state so the user sees *which* directory came back empty, and the
/// stats so we can tell them "3 dirs skipped" when permissions bite.
fn discover_sessions() -> Result<(Vec<SessionFile>, session::DiscoveryStats, Option<PathBuf>)> {
    let Some(root) = session::projects_root() else {
        return Ok((Vec::new(), session::DiscoveryStats::default(), None));
    };
    let (sessions, stats) = session::discover(&root)?;
    Ok((sessions, stats, Some(root)))
}

/// Resolve the `session` positional arg into an optional [`SessionFile`].
///
/// Tries in order:
/// 1. If `target` points at an existing file on disk, open it directly —
///    this is how `claude-code-scrollback path/to/session.jsonl` works,
///    including files outside `~/.claude/projects/`.
/// 2. Otherwise, if `target` *looks* path-like (contains a path separator
///    or starts with `.`/`/`), fail fast with a clear file-not-found
///    error instead of silently falling through to id resolution. This
///    catches typos like `./fixtures/missing.jsonl` early.
/// 3. Otherwise, treat `target` as a session id prefix and resolve it
///    against the project directory via [`discover_sessions`].
fn resolve_session_target(target: &str) -> Result<Option<SessionFile>> {
    let path = Path::new(target);
    if path.is_file() {
        return Ok(Some(session_file_from_path(path)?));
    }
    if looks_like_path(target) {
        anyhow::bail!(
            "session file not found: {target} (path does not exist or is not a regular file)"
        );
    }
    let (sessions, _stats, _root) = discover_sessions()?;
    Ok(sessions
        .into_iter()
        .find(|s| s.session_id.starts_with(target)))
}

/// Heuristic: does `target` look like a filesystem path rather than a
/// session id? Session ids are UUIDs with no separators; anything
/// containing `/`, `\`, or starting with `.`/`/` is treated as a path.
fn looks_like_path(target: &str) -> bool {
    target.contains(std::path::MAIN_SEPARATOR)
        || target.contains('/')
        || target.contains('\\')
        || target.starts_with('.')
}

/// Resolve the active session for `--live` with no explicit target.
/// Finds the newest JSONL under the current cwd's project whose mtime
/// is within [`session::DEFAULT_ACTIVE_WITHIN`]. Errors out with a
/// clear message if nothing matches — live-tail has no fallback other
/// than silently dropping the user back at an empty viewer.
fn resolve_active_session() -> Result<Option<SessionFile>> {
    let cwd = std::env::current_dir()?;
    match session::active_session(&cwd, session::DEFAULT_ACTIVE_WITHIN)? {
        Some(s) => {
            tracing::info!(
                path = %s.path.display(),
                session_id = %s.session_id,
                "resolved active session for --live",
            );
            Ok(Some(s))
        }
        None => {
            anyhow::bail!(
                "no active session found under {} (no JSONL modified in the last {}s)",
                cwd.display(),
                session::DEFAULT_ACTIVE_WITHIN.as_secs(),
            );
        }
    }
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
