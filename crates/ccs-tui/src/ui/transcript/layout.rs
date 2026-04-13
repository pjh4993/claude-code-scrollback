//! Pre-render a [`Transcript`] into a flat, terminal-ready line cache.
//!
//! Text blocks are rendered through the minimal markdown walker in
//! [`super::markdown`]; other block kinds (thinking, tool call, tool
//! result, attachment, unknown) stay on the plain char-wrap path.
//! Collapsed blocks are replaced by a single [`LineKind::Fold`] marker.

use std::collections::HashSet;

use ccs_core::transcript::{Block, Message, Role, Transcript};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown::render_markdown;
use super::state::{BlockId, Checkpoint, CheckpointKind, CollapseAll, LineKind, RenderedLine};

/// Fallback wrap width used only when the caller has not yet set a real
/// viewport (first draw, width == 0). A real narrow terminal — even
/// something ridiculous like 8 columns — is rendered as-is so the user
/// sees what their terminal is actually showing.
const UNKNOWN_WIDTH_FALLBACK: usize = 80;

/// Everything a `build` call needs to know about the current collapse
/// state. Lives in [`super::state`] but is passed in by reference so the
/// layout pass stays a pure function of transcript + collapse snapshot.
pub struct CollapseContext<'a> {
    pub collapsed: &'a HashSet<BlockId>,
    pub collapse_all: CollapseAll,
}

/// Output of a layout pass: the line cache plus the line indices of each
/// user-turn header (for `{` and `}` jumps) and the per-relayout list of
/// auto-checkpoints driving the sidebar.
pub struct LayoutOutput {
    pub lines: Vec<RenderedLine>,
    pub user_turn_line_starts: Vec<usize>,
    pub checkpoints: Vec<Checkpoint>,
}

/// Build the full line cache for `transcript` at the given body width.
///
/// `width` is the inner width of the viewer body (already has borders /
/// padding subtracted by the caller). A `width` of 0 falls back to 80
/// columns so the very first draw (before `set_viewport` runs) still
/// produces content.
pub fn build(transcript: &Transcript, width: u16, ctx: &CollapseContext<'_>) -> LayoutOutput {
    let wrap_at = effective_wrap_width(width);
    let mut lines: Vec<RenderedLine> = Vec::new();
    let mut user_turn_line_starts: Vec<usize> = Vec::new();
    let mut checkpoints: Vec<Checkpoint> = Vec::new();

    for (i, msg) in transcript.messages.iter().enumerate() {
        if i > 0 {
            lines.push(separator_line(msg.index));
        }
        let header_idx = lines.len();
        lines.push(header_line(msg));
        if msg.role == Role::User {
            user_turn_line_starts.push(header_idx);
            checkpoints.push(Checkpoint {
                line: header_idx,
                msg_index: msg.index,
                kind: CheckpointKind::UserTurn,
                preview: message_preview(msg),
            });
        }
        if let Some(reason) = msg.stop_reason.as_ref() {
            // System messages carrying a stopReason become Stop checkpoints
            // at their own header line — that's where the user sees the
            // `stop_reason=…` text today. `preview` holds only the raw
            // reason; the sidebar render adds the "stop" label up front
            // so we don't end up rendering "stop  stop: end_turn".
            checkpoints.push(Checkpoint {
                line: header_idx,
                msg_index: msg.index,
                kind: CheckpointKind::Stop(reason.clone()),
                preview: reason.clone(),
            });
        }
        for (block_idx, block) in msg.blocks.iter().enumerate() {
            if is_collapsed(msg.index, block_idx, block, ctx) {
                lines.push(fold_line(msg.index, block_idx, block));
            } else {
                append_block(&mut lines, msg.index, block_idx, block, wrap_at);
            }
        }
    }

    LayoutOutput {
        lines,
        user_turn_line_starts,
        checkpoints,
    }
}

/// One-line role-aware preview of a message for the sidebar list.
/// Pulls the first text block (if any), takes only its first
/// non-empty line, and truncates. Falls back to the block kind when
/// the message has no textual content (e.g. a pure tool-call turn).
fn message_preview(msg: &Message) -> String {
    const MAX: usize = 48;
    let raw = msg
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Text(s) => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or_else(|| match msg.blocks.first() {
            Some(Block::Thinking(_)) => "(thinking)",
            Some(Block::ToolCall { name, .. }) => name.as_str(),
            Some(Block::ToolResult { .. }) => "(tool result)",
            Some(Block::Attachment(_)) => "(attachment)",
            _ => "",
        });
    // Take only the first non-empty line so a multi-paragraph message
    // doesn't get its remaining lines flattened into the preview.
    let first_line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let collapsed: String = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX {
        let truncated: String = collapsed.chars().take(MAX - 1).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

fn effective_wrap_width(width: u16) -> usize {
    if width == 0 {
        UNKNOWN_WIDTH_FALLBACK
    } else {
        width as usize
    }
}

fn is_collapsed(
    msg_idx: usize,
    block_idx: usize,
    block: &Block,
    ctx: &CollapseContext<'_>,
) -> bool {
    if ctx.collapsed.contains(&(msg_idx, block_idx)) {
        return true;
    }
    match ctx.collapse_all {
        CollapseAll::Off => false,
        CollapseAll::ToolsAndThinking => matches!(
            block,
            Block::Thinking(_) | Block::ToolCall { .. } | Block::ToolResult { .. }
        ),
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
        block_index: None,
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
        block_index: None,
        kind: LineKind::Separator,
    }
}

fn fold_line(msg_index: usize, block_index: usize, block: &Block) -> RenderedLine {
    let (label, style) = match block {
        Block::Thinking(s) => {
            let n = count_lines(s);
            (
                format!("▸ [thinking] ({n} lines hidden)"),
                Style::new()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC),
            )
        }
        Block::ToolCall { name, .. } => (
            format!("▸ → {name} (collapsed)"),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Block::ToolResult {
            content, is_error, ..
        } => {
            let n = count_lines(content);
            let head = if *is_error {
                format!("▸ ← tool_result [error] ({n} lines hidden)")
            } else {
                format!("▸ ← tool_result ({n} lines hidden)")
            };
            let style = if *is_error {
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            };
            (head, style)
        }
        _ => (
            "▸ (collapsed)".to_string(),
            Style::new().add_modifier(Modifier::DIM),
        ),
    };
    RenderedLine {
        line: Line::from(Span::styled(label, style)),
        msg_index,
        block_index: Some(block_index),
        kind: LineKind::Fold,
    }
}

fn count_lines(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.lines().count().max(1)
    }
}

fn append_block(
    out: &mut Vec<RenderedLine>,
    msg_index: usize,
    block_index: usize,
    block: &Block,
    wrap_at: usize,
) {
    match block {
        Block::Text(s) => {
            for line in render_markdown(s, wrap_at) {
                out.push(body_line(msg_index, block_index, line));
            }
        }
        Block::Thinking(s) => {
            let prefix = "  ";
            let style = Style::new()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM | Modifier::ITALIC);
            let first = format!("{prefix}[thinking]");
            out.push(body_line(
                msg_index,
                block_index,
                Line::from(Span::styled(first, style)),
            ));
            for chunk in wrap_plain(s, wrap_at.saturating_sub(prefix.len())) {
                let text = format!("{prefix}{chunk}");
                out.push(body_line(
                    msg_index,
                    block_index,
                    Line::from(Span::styled(text, style)),
                ));
            }
        }
        Block::ToolCall {
            name, input_json, ..
        } => {
            let head = format!("→ {name}");
            out.push(body_line(
                msg_index,
                block_index,
                Line::from(Span::styled(
                    head,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
            ));
            for chunk in wrap_plain(input_json, wrap_at.saturating_sub(2)) {
                let text = format!("  {chunk}");
                out.push(body_line(
                    msg_index,
                    block_index,
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
                block_index,
                Line::from(Span::styled(head.to_string(), head_style)),
            ));
            let body_style = Style::new().add_modifier(Modifier::DIM);
            for chunk in wrap_plain(content, wrap_at.saturating_sub(2)) {
                let text = format!("  {chunk}");
                out.push(body_line(
                    msg_index,
                    block_index,
                    Line::from(Span::styled(text, body_style)),
                ));
            }
        }
        Block::Attachment(s) => {
            let style = Style::new().fg(Color::Blue);
            let text = format!("[attachment] {s}");
            for chunk in wrap_plain(&text, wrap_at) {
                out.push(body_line(
                    msg_index,
                    block_index,
                    Line::from(Span::styled(chunk, style)),
                ));
            }
        }
        Block::Unknown => {
            out.push(body_line(
                msg_index,
                block_index,
                Line::from(Span::styled(
                    "[unknown block]".to_string(),
                    Style::new().add_modifier(Modifier::DIM),
                )),
            ));
        }
    }
}

fn body_line(msg_index: usize, block_index: usize, line: Line<'static>) -> RenderedLine {
    RenderedLine {
        line,
        msg_index,
        block_index: Some(block_index),
        kind: LineKind::Body,
    }
}

/// Plain char-count wrap used by non-text blocks (tool calls, tool
/// results, attachments, thinking). Text blocks go through the markdown
/// renderer in [`super::markdown`] which is width-aware.
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

    fn build_default(t: &Transcript, width: u16) -> LayoutOutput {
        let collapsed: HashSet<BlockId> = HashSet::new();
        let ctx = CollapseContext {
            collapsed: &collapsed,
            collapse_all: CollapseAll::Off,
        };
        build(t, width, &ctx)
    }

    #[test]
    fn empty_transcript_produces_no_lines() {
        let t = Transcript::default();
        assert!(build_default(&t, 80).lines.is_empty());
    }

    #[test]
    fn single_message_has_header_and_body_no_leading_separator() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ]);
        let out = build_default(&t, 80);
        assert_eq!(out.lines[0].kind, LineKind::Header);
        assert!(out.lines.iter().skip(1).any(|l| l.kind == LineKind::Body));
    }

    #[test]
    fn two_messages_have_a_separator_between() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ]);
        let out = build_default(&t, 80);
        let sep_count = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Separator)
            .count();
        assert_eq!(sep_count, 1);
    }

    #[test]
    fn user_turn_line_starts_index_the_user_headers() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"second"}}"#,
        ]);
        let out = build_default(&t, 80);
        assert_eq!(out.user_turn_line_starts.len(), 2);
        for &i in &out.user_turn_line_starts {
            assert_eq!(out.lines[i].kind, LineKind::Header);
            assert_eq!(out.lines[i].msg_index % 2, 0); // user turns at even msg indices
        }
    }

    #[test]
    fn wraps_long_text_at_width() {
        let long = "x".repeat(90);
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let t = tx(&[&line]);
        let out = build_default(&t, 40);
        let body_count = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Body)
            .count();
        assert!(body_count >= 3);
    }

    #[test]
    fn unknown_viewport_falls_back_to_80() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hello world"}}"#,
        ]);
        let out = build_default(&t, 0);
        assert!(out.lines.iter().any(|l| l.kind == LineKind::Body));
    }

    #[test]
    fn attachment_wraps_long_paths() {
        let long_path = format!("/very/long/{}", "x".repeat(200));
        let t = tx(&[&format!(
            r#"{{"type":"attachment","uuid":"a1","sessionId":"s1","timestamp":"t","attachment":{{"type":"file","path":"{long_path}"}}}}"#
        )]);
        let out = build_default(&t, 40);
        let body_count = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Body)
            .count();
        assert!(body_count >= 4);
    }

    #[test]
    fn collapse_all_replaces_tools_and_thinking_with_fold_lines() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"long\nreasoning\nblock"},{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#,
        ]);
        let mut collapsed: HashSet<BlockId> = HashSet::new();
        collapsed.clear();
        let ctx = CollapseContext {
            collapsed: &collapsed,
            collapse_all: CollapseAll::ToolsAndThinking,
        };
        let out = build(&t, 80, &ctx);
        let fold_count = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Fold)
            .count();
        // thinking + tool_use = 2 folds; text stays expanded.
        assert_eq!(fold_count, 2);
        assert!(out.lines.iter().any(|l| l.kind == LineKind::Body));
    }

    #[test]
    fn individual_collapse_via_set_replaces_single_block() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"one"},{"type":"text","text":"two"}]}}"#,
        ]);
        let mut collapsed: HashSet<BlockId> = HashSet::new();
        collapsed.insert((0, 0)); // collapse the thinking block
        let ctx = CollapseContext {
            collapsed: &collapsed,
            collapse_all: CollapseAll::Off,
        };
        let out = build(&t, 80, &ctx);
        let fold_count = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Fold)
            .count();
        assert_eq!(fold_count, 1);
        assert_eq!(
            out.lines
                .iter()
                .find(|l| l.kind == LineKind::Fold)
                .unwrap()
                .block_index,
            Some(0)
        );
    }

    #[test]
    fn markdown_bold_in_text_block_retains_text() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"a **bold** word"}]}}"#,
        ]);
        let out = build_default(&t, 80);
        let joined: String = out
            .lines
            .iter()
            .flat_map(|l| l.line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("bold"));
        assert!(joined.contains("word"));
    }

    #[test]
    fn tool_call_and_result_render_with_prefix_glyphs() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"contents","is_error":false}]}}"#,
        ]);
        let out = build_default(&t, 80);
        let rendered: Vec<String> = out
            .lines
            .iter()
            .flat_map(|l| l.line.spans.iter().map(|s| s.content.to_string()))
            .collect();
        let joined = rendered.join("|");
        assert!(joined.contains("→ Read"));
        assert!(joined.contains("← tool_result"));
    }

    #[test]
    fn checkpoints_emit_one_user_turn_per_user_message() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"second"}}"#,
        ]);
        let out = build_default(&t, 80);
        let user_turns: Vec<_> = out
            .checkpoints
            .iter()
            .filter(|c| matches!(c.kind, CheckpointKind::UserTurn))
            .collect();
        assert_eq!(user_turns.len(), 2);
        assert_eq!(user_turns[0].preview, "first");
        assert_eq!(user_turns[1].preview, "second");
        // Every checkpoint line points at a real header row.
        for cp in &out.checkpoints {
            assert_eq!(out.lines[cp.line].kind, LineKind::Header);
        }
    }

    #[test]
    fn stop_reason_system_event_emits_stop_checkpoint() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"ask"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}"#,
            r#"{"type":"system","uuid":"sys1","sessionId":"s1","timestamp":"t","stopReason":"end_turn"}"#,
        ]);
        let out = build_default(&t, 80);
        let stops: Vec<_> = out
            .checkpoints
            .iter()
            .filter_map(|c| match &c.kind {
                CheckpointKind::Stop(r) => Some((c.line, r.clone(), c.preview.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].1, "end_turn");
        // Preview is just the raw reason; the sidebar render adds the
        // "stop" label so we don't double-prefix the row.
        assert_eq!(stops[0].2, "end_turn");
        assert_eq!(out.lines[stops[0].0].kind, LineKind::Header);
    }

    #[test]
    fn user_turn_preview_uses_only_first_line() {
        // A multi-line user message must preview as just its first
        // line, not a whitespace-collapsed join of every line.
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"first line\nsecond line\nthird"}}"#,
        ]);
        let out = build_default(&t, 80);
        let preview = &out.checkpoints[0].preview;
        assert_eq!(preview, "first line");
    }

    #[test]
    fn checkpoints_are_sorted_by_line_so_prev_next_jumps_are_monotonic() {
        let t = tx(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"q1"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"a1"}]}}"#,
            r#"{"type":"system","uuid":"sys1","sessionId":"s1","timestamp":"t","stopReason":"end_turn"}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"q2"}}"#,
        ]);
        let out = build_default(&t, 80);
        assert!(
            out.checkpoints.windows(2).all(|w| w[0].line < w[1].line),
            "checkpoints not strictly increasing: {:?}",
            out.checkpoints.iter().map(|c| c.line).collect::<Vec<_>>()
        );
    }

    #[test]
    fn body_and_fold_lines_carry_block_index() {
        let t = tx(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}"#,
        ]);
        let out = build_default(&t, 80);
        let body_indices: Vec<_> = out
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Body)
            .filter_map(|l| l.block_index)
            .collect();
        assert!(body_indices.contains(&0));
        assert!(body_indices.contains(&1));
    }
}
