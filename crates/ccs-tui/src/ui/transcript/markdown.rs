//! Minimal terminal-safe markdown renderer.
//!
//! Walks a [`pulldown_cmark`] event stream and emits `ratatui::text::Line`
//! values with appropriate styling. Intentionally minimal:
//!
//! * **Supported**: paragraphs, bold, italic, inline code, fenced code
//!   blocks (dim background, no syntax highlighting), bulleted & numbered
//!   lists (1 level deep renders clean, deeper nests indent), headings,
//!   hard/soft breaks.
//! * **Dropped for v1**: tables, blockquotes (rendered as indented dim
//!   text), images, autolinks, HTML blocks, footnotes.
//!
//! Wrapping is width-aware via `unicode-width::UnicodeWidthChar` so CJK
//! and emoji account for their visual width rather than `char` count.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Render a markdown source string into a list of wrapped, styled lines.
///
/// `width` is the usable body width in columns; 0 is treated as "unknown"
/// and falls back to a conservative 80 so the very first draw (before
/// `set_viewport` runs) still produces content.
pub fn render_markdown(src: &str, width: usize) -> Vec<Line<'static>> {
    let wrap_at = if width == 0 { 80 } else { width.max(1) };
    let mut walker = Walker::new(wrap_at);
    let parser = Parser::new_ext(src, Options::empty());
    for event in parser {
        walker.event(event);
    }
    walker.finish()
}

struct Walker {
    wrap_at: usize,
    out: Vec<Line<'static>>,

    /// Spans accumulated for the current line (before wrapping).
    cur_spans: Vec<Span<'static>>,
    /// Visible column width of `cur_spans`.
    cur_width: usize,

    /// Active inline style modifiers (bold, italic, code).
    style_stack: Vec<Style>,

    /// `Some(indent_prefix)` while inside a code block; we render code
    /// blocks line-for-line without re-wrapping to preserve code shape.
    in_code_block: bool,

    /// List state: a stack where each entry is `Some(next_number)` for
    /// an ordered list or `None` for a bulleted list.
    list_stack: Vec<Option<u64>>,

    /// Left-indent applied to every wrapped line (set by lists, headings).
    indent: String,
    /// Prefix for just the first line of the *next* block-level container
    /// (the bullet or ordinal for a list item).
    pending_item_prefix: Option<String>,
}

impl Walker {
    fn new(wrap_at: usize) -> Self {
        Self {
            wrap_at,
            out: Vec::new(),
            cur_spans: Vec::new(),
            cur_width: 0,
            style_stack: vec![Style::default()],
            in_code_block: false,
            list_stack: Vec::new(),
            indent: String::new(),
            pending_item_prefix: None,
        }
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default())
    }

    fn push_style<F: FnOnce(Style) -> Style>(&mut self, f: F) {
        let new = f(self.current_style());
        self.style_stack.push(new);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.out
    }

    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(t) => self.emit_text(&t),
            Event::Code(c) => {
                self.push_style(|_| {
                    Style::new()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM | Modifier::ITALIC)
                });
                self.emit_text(&c);
                self.pop_style();
            }
            Event::SoftBreak => self.emit_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                let rule = "─".repeat(self.wrap_at.min(40));
                self.out.push(Line::from(Span::styled(
                    rule,
                    Style::new().add_modifier(Modifier::DIM),
                )));
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                // Strip HTML entirely for v1 — terminal-unfriendly.
            }
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {
                // Out of scope for v1.
            }
            Event::InlineMath(s) | Event::DisplayMath(s) => {
                // Render math as-is; no TeX support in v1.
                self.emit_text(&s);
            }
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                // Nothing special; text flows into the current line.
            }
            Tag::Heading { level, .. } => {
                self.flush_line();
                self.push_style(|s| s.fg(Color::Magenta).add_modifier(Modifier::BOLD));
                let hashes = "#".repeat(level as usize);
                self.emit_raw(&format!("{hashes} "));
            }
            Tag::BlockQuote(_) => {
                // Render as a dim indented paragraph — no true blockquote glyph.
                self.flush_line();
                self.push_indent("│ ");
                self.push_style(|s| s.add_modifier(Modifier::DIM | Modifier::ITALIC));
            }
            Tag::CodeBlock(_) => {
                self.flush_line();
                self.in_code_block = true;
            }
            Tag::List(start) => {
                self.flush_line();
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.flush_line();
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.pending_item_prefix = Some(marker);
            }
            Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { .. } => {
                self.push_style(|s| s.fg(Color::Blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { .. } => {
                // Render alt text inline for v1; image loading is out of scope.
                self.push_style(|s| s.add_modifier(Modifier::DIM));
            }
            Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::HtmlBlock
            | Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {
                // Out of scope; containing text still flows.
            }
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
                self.out.push(Line::from(""));
            }
            TagEnd::Heading(_) => {
                self.flush_line();
                self.pop_style();
                self.out.push(Line::from(""));
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.pop_indent("│ ");
                self.pop_style();
                self.out.push(Line::from(""));
            }
            TagEnd::CodeBlock => {
                self.flush_line();
                self.in_code_block = false;
                self.out.push(Line::from(""));
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.out.push(Line::from(""));
                }
            }
            TagEnd::Item => {
                self.flush_line();
                // Clear any pending prefix that never got used (empty item).
                self.pending_item_prefix = None;
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => {
                self.pop_style();
            }
            _ => {}
        }
        // Drop trailing blank line from accumulating past the very last block.
        while matches!(self.out.last(), Some(l) if l.spans.is_empty())
            && self.out.len() > 1
            && matches!(self.out.get(self.out.len() - 2), Some(l) if l.spans.is_empty())
        {
            self.out.pop();
        }
    }

    // --- text emission ---------------------------------------------------

    fn emit_text(&mut self, text: &str) {
        if self.in_code_block {
            self.emit_code_block_text(text);
            return;
        }
        for ch in text.chars() {
            if ch == '\n' {
                self.flush_line();
                continue;
            }
            self.push_char(ch);
        }
    }

    fn emit_raw(&mut self, text: &str) {
        for ch in text.chars() {
            self.push_char(ch);
        }
    }

    fn emit_code_block_text(&mut self, text: &str) {
        // Preserve code block shape — one output line per source line,
        // no re-wrapping, dim style, indented like any other block.
        let style = self.current_style().add_modifier(Modifier::DIM);
        for raw_line in text.split_inclusive('\n') {
            let trimmed = raw_line.trim_end_matches('\n');
            let mut line_spans: Vec<Span<'static>> = Vec::new();
            if !self.indent.is_empty() {
                line_spans.push(Span::raw(self.indent.clone()));
            }
            line_spans.push(Span::styled(trimmed.to_string(), style));
            self.out.push(Line::from(line_spans));
            if !raw_line.ends_with('\n') {
                // Partial final segment — keep it in cur_spans for flush.
                self.out.pop();
                self.cur_spans
                    .push(Span::styled(trimmed.to_string(), style));
                self.cur_width += str_width(trimmed);
            }
        }
    }

    fn push_char(&mut self, ch: char) {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        let budget = self.line_budget();
        if self.cur_width + w > budget && self.cur_width > 0 {
            self.wrap_line_at_last_space();
        }
        let style = self.current_style();
        self.append_span(Span::styled(ch.to_string(), style), w);
    }

    fn append_span(&mut self, span: Span<'static>, width: usize) {
        // Merge with previous span if the style matches; keeps Line.spans
        // tiny (1–3 spans per line in the common case) and avoids
        // megabyte-sized Vec<Span> on long paragraphs.
        if let Some(last) = self.cur_spans.last_mut() {
            if last.style == span.style {
                let mut merged = last.content.to_string();
                merged.push_str(&span.content);
                last.content = merged.into();
                self.cur_width += width;
                return;
            }
        }
        self.cur_spans.push(span);
        self.cur_width += width;
    }

    fn line_budget(&self) -> usize {
        let indent_w = self
            .indent
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>();
        let prefix_w = self
            .pending_item_prefix
            .as_deref()
            .map(str_width)
            .unwrap_or(0);
        self.wrap_at.saturating_sub(indent_w + prefix_w).max(1)
    }

    fn wrap_line_at_last_space(&mut self) {
        // Try to break at the last ASCII space in the current spans for a
        // nicer visual wrap; fall back to a hard break if no space.
        if let Some((span_idx, char_idx)) = self.last_space_position() {
            let mut tail_spans: Vec<Span<'static>> = Vec::new();
            let tail_src = &self.cur_spans[span_idx];
            let split_point = tail_src
                .content
                .char_indices()
                .nth(char_idx + 1)
                .map(|(i, _)| i)
                .unwrap_or(tail_src.content.len());
            let before = tail_src.content[..split_point]
                .trim_end_matches(' ')
                .to_string();
            let after = tail_src.content[split_point..].to_string();
            let style = tail_src.style;
            let drop_idx = if before.is_empty() {
                span_idx
            } else {
                span_idx + 1
            };
            // Take spans after `span_idx` wholesale as tail.
            let mut trailing: Vec<Span<'static>> = self.cur_spans.drain(span_idx + 1..).collect();
            // Replace `span_idx` span with its "before" half (or drop).
            if before.is_empty() {
                self.cur_spans.pop();
            } else {
                self.cur_spans[span_idx] = Span::styled(before, style);
            }
            if !after.is_empty() {
                tail_spans.push(Span::styled(after, style));
            }
            tail_spans.append(&mut trailing);
            self.flush_line();
            self.cur_spans = tail_spans;
            self.cur_width = self.cur_spans.iter().map(|s| str_width(&s.content)).sum();
            let _ = drop_idx;
        } else {
            // No space in the current line — hard break.
            self.flush_line();
        }
    }

    fn last_space_position(&self) -> Option<(usize, usize)> {
        for (si, span) in self.cur_spans.iter().enumerate().rev() {
            if let Some(ci) = span.content.rfind(' ') {
                let char_idx = span.content[..ci].chars().count();
                return Some((si, char_idx));
            }
        }
        None
    }

    fn flush_line(&mut self) {
        if self.cur_spans.is_empty() && self.pending_item_prefix.is_none() && self.indent.is_empty()
        {
            return;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !self.indent.is_empty() {
            spans.push(Span::styled(
                self.indent.clone(),
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        if let Some(prefix) = self.pending_item_prefix.take() {
            spans.push(Span::raw(prefix));
        }
        spans.append(&mut self.cur_spans);
        self.out.push(Line::from(spans));
        self.cur_width = 0;
    }

    fn push_indent(&mut self, s: &str) {
        self.indent.push_str(s);
    }

    fn pop_indent(&mut self, s: &str) {
        if self.indent.ends_with(s) {
            let new_len = self.indent.len() - s.len();
            self.indent.truncate(new_len);
        }
    }
}

fn str_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str, width: usize) -> Vec<String> {
        render_markdown(src, width)
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn plain_paragraph_passes_through() {
        let out = render("hello world", 80);
        assert!(out.iter().any(|l| l.contains("hello world")));
    }

    #[test]
    fn bold_and_italic_retain_their_text() {
        let out = render("a **bold** c *italic* d", 80);
        let joined = out.join("\n");
        assert!(joined.contains("bold"));
        assert!(joined.contains("italic"));
    }

    #[test]
    fn fenced_code_block_preserves_line_shape() {
        let src = "```\nlet x = 1;\nlet y = 2;\n```";
        let out = render(src, 80);
        assert!(out.iter().any(|l| l.contains("let x = 1;")));
        assert!(out.iter().any(|l| l.contains("let y = 2;")));
    }

    #[test]
    fn bulleted_list_renders_with_bullets() {
        let out = render("- one\n- two\n- three", 80);
        let bullet_lines: Vec<_> = out.iter().filter(|l| l.contains("•")).collect();
        assert_eq!(bullet_lines.len(), 3);
    }

    #[test]
    fn numbered_list_renders_with_ordinals() {
        let out = render("1. first\n2. second\n3. third", 80);
        assert!(out.iter().any(|l| l.contains("1.") && l.contains("first")));
        assert!(out.iter().any(|l| l.contains("2.") && l.contains("second")));
        assert!(out.iter().any(|l| l.contains("3.") && l.contains("third")));
    }

    #[test]
    fn long_paragraph_wraps_at_width() {
        let src = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt";
        let out = render(src, 30);
        assert!(out.len() >= 3, "expected multi-line wrap, got: {out:?}");
        for line in &out {
            if !line.is_empty() {
                assert!(
                    line.chars().count() <= 32,
                    "line exceeds wrap width: {line:?}"
                );
            }
        }
    }

    #[test]
    fn inline_code_is_preserved() {
        let out = render("use `Vec::new()` please", 80);
        assert!(out.iter().any(|l| l.contains("Vec::new()")));
    }

    #[test]
    fn heading_renders_with_hashes() {
        let out = render("# Title\n\nbody", 80);
        assert!(out.iter().any(|l| l.contains("# Title")));
        assert!(out.iter().any(|l| l.contains("body")));
    }

    #[test]
    fn unknown_width_falls_back_to_80() {
        let out = render_markdown("hello", 0);
        assert!(!out.is_empty());
    }
}
