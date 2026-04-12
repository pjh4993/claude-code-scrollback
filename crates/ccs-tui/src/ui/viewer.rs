use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Style, Stylize},
    widgets::{Block, Borders, Paragraph},
};

pub fn render(frame: &mut Frame, live: bool, session: Option<&str>) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = match (live, session) {
        (true, _) => "transcript viewer — live-tail".to_string(),
        (false, Some(id)) => format!("transcript viewer — {id}"),
        (false, None) => "transcript viewer".to_string(),
    };

    frame.render_widget(
        Paragraph::new(title).style(Style::new().bold()),
        header,
    );
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title("messages (stub)"),
        body,
    );
    frame.render_widget(
        Paragraph::new("q/Esc quit  ·  j/k scroll (todo)  ·  / search (todo)").dim(),
        footer,
    );
}
