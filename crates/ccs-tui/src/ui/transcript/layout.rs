//! Pre-render a [`Transcript`] into a flat, terminal-ready line cache.
//!
//! PR 2 scope: plain-text only. Role-colored header per message, body lines
//! per block with a short prefix indicating block kind, blank separator
//! between messages. PR 3 will swap the body path for a pulldown-cmark
//! event walker that emits styled spans; the shape of this function stays
//! the same.

use ccs_core::transcript::{Block, Message, Role, Transcript};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::state::{LineKind, RenderedLine};

/// Fallback wrap width used only when the caller has not yet set a real
/// viewport (first draw, width == 0). A real narrow terminal — even
/// something ridiculous like 8 columns — is rendered as-is so the user
/// sees what their terminal is actually showing.
const UNKNOWN_WIDTH_FALLBACK: usize = 80;

/// Build the full line cache for `transcript` at the given body width.
///
/// `width` is the inner width of the viewer body (already has borders /
/// padding subtracted by the caller). A `width` of 0 is treated as
/// "unknown" and falls back to a conservative wrap width so lines still
/// render during the very first draw before the real viewport size is
/// known.
pub fn build(transcript: &Transcript, width: u16) -> Vec<RenderedLine> {
    let wrap_at = effective_wrap_width(width);
    let mut out: Vec<RenderedLine> = Vec::new();
    for (i, msg) in transcript.messages.iter().enumerate() {
        if i > 0 {
            out.push(separator_line(msg.index));
        }
        out.push(header_line(msg));
        for block in &msg.blocks {
            append_block(&mut out, msg.index, block, wrap_at);
        }
    }
    out
}

fn effective_wrap_width(width: u16) -> usize {
    if width == 0 {
        UNKNOWN_WIDTH_FALLBACK
    } else {
        width as usize
    }
}

fn role_style(role: Role) -> Style {
    match role {
        Role::User => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        Role::Assistant => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        Role::System => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn header_line(msg: &Message) -> RenderedLine {
    let label = format!("── {} ──", role_label(msg.role));
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    spans.push(Span::styled(label, role_style(msg.role)));
    if !msg.timestamp.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            msg.timestamp.clone(),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    RenderedLine {
        line: Line::from(spans),
        msg_index: msg.index,
        kind: LineKind::Header,
    }
}

fn separator_line(next_msg_index: usize) -> RenderedLine {
    RenderedLine {
        line: Line::from(Span::raw("")),
        // Attribute the separator to the message that *follows* it, so
        // scrolling to the top of a message lands the cursor on its
        // separator+header pair.
        msg_index: next_msg_index,
        kind: LineKind::Separator,
    }
}

fn append_block(out: &mut Vec<RenderedLine>, msg_index: usize, block: &Block, wrap_at: usize) {
    match block {
        Block::Text(s) => {
            for chunk in wrap_plain(s, wrap_at) {
                out.push(body_line(msg_index, Line::from(Span::raw(chunk))));
            }
        }
        Block::Thinking(s) => {
            let prefix = "  ";
            let style = Style::new()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM | Modifier::ITALIC);
            let first = format!("{prefix}[thinking]");
            out.push(body_line(msg_index, Line::from(Span::styled(first, style))));
            for chunk in wrap_plain(s, wrap_at.saturating_sub(prefix.len())) {
                let text = format!("{prefix}{chunk}");
                out.push(body_line(msg_index, Line::from(Span::styled(text, style))));
            }
        }
        Block::ToolCall {
            name, input_json, ..
        } => {
            let head = format!("→ {name}");
            out.push(body_line(
                msg_index,
                Line::from(Span::styled(
                    head,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
            ));
            for chunk in wrap_plain(input_json, wrap_at.saturating_sub(2)) {
                let text = format!("  {chunk}");
                out.push(body_line(
                    msg_index,
                    Line::from(Span::styled(text, Style::new().fg(Color::Yellow))),
                ));
            }
        }
        Block::ToolResult {
            content, is_error, ..
        } => {
            let head_style = if *is_error {
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            };
            let head = if *is_error {
                "← tool_result [error]"
            } else {
                "← tool_result"
            };
            out.push(body_line(
                msg_index,
                Line::from(Span::styled(head.to_string(), head_style)),
            ));
            let body_style = Style::new().add_modifier(Modifier::DIM);
            for chunk in wrap_plain(content, wrap_at.saturating_sub(2)) {
                let text = format!("  {chunk}");
                out.push(body_line(
                    msg_index,
                    Line::from(Span::styled(text, body_style)),
                ));
            }
        }
        Block::Attachment(s) => {
            let style = Style::new().fg(Color::Blue);
            let text = format!("[attachment] {s}");
            for chunk in wrap_plain(&text, wrap_at) {
                out.push(body_line(msg_index, Line::from(Span::styled(chunk, style))));
            }
        }
        Block::Unknown => {
            out.push(body_line(
                msg_index,
                Line::from(Span::styled(
                    "[unknown block]".to_string(),
                    Style::new().add_modifier(Modifier::DIM),
                )),
            ));
        }
    }
}

fn body_line(msg_index: usize, line: Line<'static>) -> RenderedLine {
    RenderedLine {
        line,
        msg_index,
        kind: LineKind::Body,
    }
}

/// Split `text` on newlines, then break each line at `wrap_at` char
/// boundaries. Char-count based — fine for plain ASCII / BMP text; PR 3
/// swaps this for `unicode-width` grapheme-aware wrapping when the
/// markdown renderer lands.
fn wrap_plain(text: &str, wrap_at: usize) -> Vec<String> {
    let wrap_at = wrap_at.max(1);
    let mut out: Vec<String> = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for ch in raw_line.chars() {
            if current_len >= wrap_at {
                out.push(std::mem::take(&mut current));
                current_len = 0;
            }
            current.push(ch);
            current_len += 1;
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::jsonl::parse_line;
    use ccs_core::transcript::from_events;

    fn tx(lines: &[&str]) -> Transcript {
        from_events(lines.iter().map(|l| parse_line(l).unwrap()))
    }

    #[test]
    fn empty_transcript_produces_no_lines() {
        let t = Transcript::default();
        assert!(build(&t, 80).is_empty());
    }

    #[test]
    fn single_message_has_header_and_body_no_leading_separator() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ]);
        let lines = build(&t, 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].kind, LineKind::Header);
        assert_eq!(lines[1].kind, LineKind::Body);
    }

    #[test]
    fn two_messages_have_a_separator_between() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ]);
        let lines = build(&t, 80);
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                LineKind::Header,
                LineKind::Body,
                LineKind::Separator,
                LineKind::Header,
                LineKind::Body,
            ]
        );
    }

    #[test]
    fn wraps_long_text_at_width() {
        let long = "x".repeat(90);
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let t = tx(&[&line]);
        let lines = build(&t, 40);
        // header + wrapped body (90 chars / 40 width = 3 body lines)
        let body_count = lines.iter().filter(|l| l.kind == LineKind::Body).count();
        assert_eq!(body_count, 3);
    }

    #[test]
    fn unknown_viewport_falls_back_to_80() {
        // width = 0 happens on the first draw before set_viewport runs.
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hello world"}}"#,
        ]);
        let lines = build(&t, 0);
        assert!(lines.iter().any(|l| l.kind == LineKind::Body));
    }

    #[test]
    fn real_narrow_viewport_wraps_at_its_own_width() {
        // Regression: previously any width < 20 was treated as "unknown" and
        // force-fallen-back to 80 cols, so a real 10-col terminal rendered
        // overflow. Now a nonzero width is honored, however small.
        let long = "x".repeat(30);
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let t = tx(&[&line]);
        let lines = build(&t, 10);
        // 30 chars / 10 cols = 3 wrapped body lines
        let body_count = lines.iter().filter(|l| l.kind == LineKind::Body).count();
        assert_eq!(body_count, 3);
    }

    #[test]
    fn attachment_wraps_long_paths() {
        // Regression: attachments used to push a single unwrapped line.
        let long_path = format!("/very/long/{}", "x".repeat(200));
        let t = tx(&[&format!(
            r#"{{"type":"attachment","uuid":"a1","sessionId":"s1","timestamp":"t","attachment":{{"type":"file","path":"{long_path}"}}}}"#
        )]);
        let lines = build(&t, 40);
        let body_count = lines.iter().filter(|l| l.kind == LineKind::Body).count();
        // Long attachment summary must span multiple wrapped body lines.
        assert!(
            body_count >= 4,
            "expected attachment to wrap across >=4 lines, got {body_count}"
        );
    }

    #[test]
    fn tool_call_and_result_render_with_prefix_glyphs() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"contents","is_error":false}]}}"#,
        ]);
        let lines = build(&t, 80);
        let rendered: Vec<String> = lines
            .iter()
            .flat_map(|l| l.line.spans.iter().map(|s| s.content.to_string()))
            .collect();
        let joined = rendered.join("|");
        assert!(
            joined.contains("→ Read"),
            "missing tool call head: {joined}"
        );
        assert!(
            joined.contains("← tool_result"),
            "missing tool result head: {joined}"
        );
    }

    #[test]
    fn each_rendered_line_carries_its_source_msg_index() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"a"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"b"}]}}"#,
        ]);
        let lines = build(&t, 80);
        // 0: header(msg0), 1: body(msg0), 2: sep(msg1), 3: header(msg1), 4: body(msg1)
        assert_eq!(lines[0].msg_index, 0);
        assert_eq!(lines[1].msg_index, 0);
        assert_eq!(lines[2].msg_index, 1);
        assert_eq!(lines[3].msg_index, 1);
        assert_eq!(lines[4].msg_index, 1);
    }
}
