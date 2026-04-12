//! Viewer screen: either an empty-state (live-tail placeholder / no session)
//! or a full transcript viewer driven by [`TranscriptState`].

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::transcript::{self, TranscriptState};

/// Render the viewer. When `state` is `Some`, delegates to the transcript
/// renderer. When `None`, draws a neutral placeholder (used by the
/// `--live` entry point before live-tail lands in PJH-51, and as the
/// empty-state fallback when a requested session id was not found).
pub fn render(frame: &mut Frame, live: bool, state: Option<&mut TranscriptState>) {
    if let Some(state) = state {
        transcript::render(frame, state, live);
        return;
    }
    render_placeholder(frame, live);
}

fn render_placeholder(frame: &mut Frame, live: bool) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = if live {
        "transcript viewer — live-tail"
    } else {
        "transcript viewer"
    };
    frame.render_widget(Paragraph::new(title).style(Style::new().bold()), header);

    let body_label = if live {
        "live-tail (stub — PJH-51)"
    } else {
        "no session loaded"
    };
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(body_label),
        body,
    );
    frame.render_widget(Paragraph::new("q/Esc quit").dim(), footer);
}
