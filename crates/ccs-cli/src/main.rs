mod logging;

use anyhow::Result;
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
            session: Some(id),
        },
        (false, None) => Screen::Picker,
    };

    let mut terminal = ccs_tui::init();
    let result = App::new(initial).run(&mut terminal);
    ccs_tui::restore();
    result
}
