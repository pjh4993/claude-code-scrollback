//! Draw the transcript viewer: header, body, footer status line.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::search::{SearchMatch, SearchMode};
use super::state::TranscriptState;

/// Render the transcript viewer into `frame`. Mutably borrows `state` so
/// it can reflow the line cache on width changes.
pub fn render(frame: &mut Frame, state: &mut TranscriptState, live: bool) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Update state with the current body inner dimensions so the cache
    // matches the viewport. The bordered Block we draw below eats 2 cols
    // and 2 rows; account for that before we relayout.
    let inner_w = body.width.saturating_sub(2);
    let inner_h = body.height.saturating_sub(2);
    state.set_viewport(inner_w, inner_h);

    let title = build_title(state, live);
    frame.render_widget(Paragraph::new(title).style(Style::new().bold()), header);

    let visible = slice_visible(state, inner_h);
    let body_widget =
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title("messages"));
    frame.render_widget(body_widget, body);

    let status = build_status(state);
    frame.render_widget(Paragraph::new(status).dim(), footer);
}

fn build_title(state: &TranscriptState, live: bool) -> String {
    let t = state.transcript();
    if live {
        return "transcript viewer — live-tail".to_string();
    }
    if t.session_id.is_empty() {
        "transcript viewer".to_string()
    } else {
        format!("transcript viewer — {}", t.session_id)
    }
}

fn slice_visible(state: &TranscriptState, height: u16) -> Vec<Line<'static>> {
    let lines = state.lines();
    if lines.is_empty() {
        return vec![Line::from(Span::styled(
            "(empty transcript)",
            Style::new().add_modifier(Modifier::DIM),
        ))];
    }
    let start = state.scroll();
    let end = (start + height as usize).min(lines.len());
    let active_matches: &[SearchMatch] = match state.search_mode() {
        SearchMode::Active { matches, .. } => matches,
        _ => &[],
    };
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, rl)| {
            let absolute = start + i;
            highlight_matches(&rl.line, absolute, active_matches)
        })
        .collect()
}

/// Overlay search-match highlights on a single rendered line.
///
/// Walks `line`'s existing spans, splitting any that intersect a match
/// range into before/middle/after pieces. Match pieces get an inverted
/// `on_yellow / black` style; everything else preserves its original
/// styling. Runs over at most one viewport's worth of lines per frame,
/// so the per-line O(spans × matches) cost is fine.
fn highlight_matches(
    line: &Line<'static>,
    line_idx: usize,
    matches: &[SearchMatch],
) -> Line<'static> {
    let line_matches: Vec<&SearchMatch> = matches.iter().filter(|m| m.line == line_idx).collect();
    if line_matches.is_empty() {
        return line.clone();
    }
    let highlight = Style::new().bg(Color::Yellow).fg(Color::Black);
    let mut out_spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
    let mut cursor = 0usize;
    for span in &line.spans {
        let span_len = span.content.len();
        let span_start = cursor;
        let span_end = cursor + span_len;
        // Collect all matches that overlap this span.
        let mut slice_start = 0usize;
        let mut overlaps: Vec<(usize, usize)> = line_matches
            .iter()
            .filter_map(|m| {
                let s = m.byte_start.max(span_start);
                let e = m.byte_end.min(span_end);
                if s < e {
                    Some((s - span_start, e - span_start))
                } else {
                    None
                }
            })
            .collect();
        overlaps.sort_by_key(|o| o.0);
        if overlaps.is_empty() {
            out_spans.push(span.clone());
        } else {
            let text = span.content.as_ref();
            let base_style = span.style;
            for (ms, me) in overlaps {
                if ms > slice_start {
                    out_spans.push(Span::styled(text[slice_start..ms].to_string(), base_style));
                }
                out_spans.push(Span::styled(text[ms..me].to_string(), highlight));
                slice_start = me;
            }
            if slice_start < span_len {
                out_spans.push(Span::styled(
                    text[slice_start..span_len].to_string(),
                    base_style,
                ));
            }
        }
        cursor = span_end;
    }
    Line::from(out_spans)
}

fn build_status(state: &TranscriptState) -> Line<'static> {
    // Typing-mode search takes priority so the user sees the live query.
    if let SearchMode::Typing { query } = state.search_mode() {
        return Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Yellow).bold()),
            Span::raw(query.clone()),
        ]);
    }
    // Flash messages (toggled by `t`/`T`/`{`/`}`/search failures, etc.)
    // take precedence over the default session counters so the user
    // sees why nothing appeared to happen.
    if let Some(flash) = state.flash() {
        return Line::from(Span::styled(
            format!("⚠ {flash}"),
            Style::new().fg(Color::Yellow),
        ));
    }
    let t = state.transcript();
    let total_msgs = t.messages.len();
    let current_msg = if total_msgs == 0 {
        0
    } else {
        state.current_msg_index() + 1
    };
    let total_lines = state.lines().len();
    let current_line = if total_lines == 0 {
        0
    } else {
        state.cursor() + 1
    };
    let project = t.project.as_deref().unwrap_or("-");
    let session_short = short_session_id(&t.session_id);

    let parts = format!(
        "session {session_short}  ·  project {project}  ·  msg {current_msg}/{total_msgs}  ·  line {current_line}/{total_lines}  ·  q/Esc quit"
    );
    Line::from(Span::raw(parts))
}

fn short_session_id(id: &str) -> String {
    if id.is_empty() {
        "-".to_string()
    } else if id.len() <= 12 {
        id.to_string()
    } else {
        format!("{}…", &id[..12])
    }
}
