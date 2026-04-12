use ratatui::{
    layout::{Constraint, Layout},
    style::{Style, Stylize},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn render(frame: &mut Frame) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new("claude-code-scrollback — session picker").style(Style::new().bold()),
        header,
    );
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title("sessions (stub)"),
        body,
    );
    frame.render_widget(
        Paragraph::new("q/Esc quit  ·  / search (todo)  ·  ↵ open (todo)").dim(),
        footer,
    );
}
