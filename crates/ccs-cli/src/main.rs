use anyhow::Result;
use ccs_tui::{App, Screen};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "claude-code-scrollback", version, about)]
struct Cli {
    /// Open the active session directly in live-tail mode, skipping the picker.
    #[arg(long)]
    live: bool,

    /// Open a specific session by id (or id prefix).
    session: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let initial = match (cli.live, cli.session) {
        (true, _) => Screen::Viewer { live: true, session: None },
        (false, Some(id)) => Screen::Viewer { live: false, session: Some(id) },
        (false, None) => Screen::Picker,
    };

    let mut terminal = ccs_tui::init();
    let result = App::new(initial).run(&mut terminal);
    ccs_tui::restore();
    result
}
