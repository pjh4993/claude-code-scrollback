mod logging;

use anyhow::Result;
use ccs_core::metadata::LazyFsSource;
use ccs_core::session::{self, SessionFile};
use ccs_tui::ui::picker::PickerState;
use ccs_tui::{App, Screen};
use clap::Parser;

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

    /// Open a specific session by id (or id prefix).
    session: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let _log_guard = logging::init(cli.log_level.as_deref(), cli.log_format)?;

    let initial = match (cli.live, cli.session) {
        (true, _) => Screen::Viewer {
            live: true,
            session: None,
        },
        (false, Some(id)) => Screen::Viewer {
            live: false,
            session: resolve_session_by_id(&id)?,
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

/// Find a session by id prefix for the `claude-code-scrollback <id>` form.
///
/// Returns the **first** session whose `session_id` starts with the prefix,
/// or `None` if there was no match (the viewer renders its empty state).
/// Ambiguous prefixes that match multiple sessions currently resolve to
/// whichever session `discover` yielded first — when the picker is
/// self-hosted via `claude-code-scrollback <prefix>`, the caller should
/// supply a long enough prefix to disambiguate.
fn resolve_session_by_id(prefix: &str) -> Result<Option<SessionFile>> {
    let sessions = discover_sessions()?;
    Ok(sessions
        .into_iter()
        .find(|s| s.session_id.starts_with(prefix)))
}
