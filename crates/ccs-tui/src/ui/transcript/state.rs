//! Owns the transcript, the pre-rendered line cache, and viewport/scroll state.

use std::collections::HashSet;
use std::path::PathBuf;

use ccs_core::checkpoints::{self, Mark, SessionMarks};
use ccs_core::jsonl::Event;
use ccs_core::transcript::{Block, Transcript};
use ratatui::text::Line;

use super::layout;
use super::search::{SearchIndex, SearchMatch, SearchMode};

/// Stable identifier for a block inside the transcript: `(msg_index, block_index)`.
/// Used by the collapse set and by `t` to hit-test the cursor line.
pub type BlockId = (usize, usize);

/// What kind of source line a [`RenderedLine`] came from. Used by the
/// renderer for status-line accounting and by the collapse/toggle
/// machinery to decide which lines are hit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// The `── user ──` / `── assistant ──` header line of a message.
    Header,
    /// Body text of an expanded block.
    Body,
    /// Blank spacer between messages.
    Separator,
    /// A single-line fold marker standing in for a collapsed block.
    Fold,
}

/// One pre-rendered terminal line. Owned (`'static`) so the cache outlives
/// any frame's borrow of the state.
#[derive(Debug, Clone)]
pub struct RenderedLine {
    pub line: Line<'static>,
    pub msg_index: usize,
    /// Source block index within the owning message. `None` for headers
    /// and separators; `Some` for `Body` and `Fold` lines.
    pub block_index: Option<usize>,
    pub kind: LineKind,
}

impl RenderedLine {
    pub fn block_id(&self) -> Option<BlockId> {
        self.block_index.map(|b| (self.msg_index, b))
    }
}

/// What gets toggled by a `T` "collapse all" cycle: everything that is a
/// tool call, a tool result, or a thinking block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapseAll {
    Off,
    ToolsAndThinking,
}

/// Viewer state: transcript, cache, viewport, scroll, collapse set, and
/// vim-chord state. The cache (`lines`) is rebuilt via [`relayout`] any
/// time the viewport width, collapse set, or `collapse_all` mode changes.
pub struct TranscriptState {
    transcript: Transcript,
    lines: Vec<RenderedLine>,
    /// Cursor line index → one per `user` message. Sorted ascending; used
    /// by `{` and `}` for prev/next-user-turn jumps.
    user_turn_line_starts: Vec<usize>,

    viewport_width: u16,
    viewport_height: u16,

    /// Top visible line index into `lines`.
    scroll: usize,
    /// Cursor line (for gg/G targeting, `t` hit-testing, and visible
    /// cursor in a later slice).
    cursor: usize,

    /// Individually collapsed blocks. Cleared whenever `collapse_all`
    /// changes so cycle behavior stays predictable.
    collapsed: HashSet<BlockId>,
    collapse_all: CollapseAll,

    /// Pending vim chord state: `g` seen, waiting for the second `g`.
    pending_g: bool,
    /// Pending `m<letter>` chord: `m` seen, waiting for the mark letter.
    pending_m: bool,
    /// Pending `'<letter>` chord: `'` seen, waiting for the jump target.
    pending_quote: bool,

    /// Manual marks for this session (letter → anchor). Loaded from
    /// `marks.json` on viewer entry; re-saved on every `m<letter>`.
    marks: SessionMarks,
    /// Path to the on-disk `marks.json`. `None` disables persistence —
    /// used by unit tests to avoid writing to the user's home directory.
    marks_path: Option<PathBuf>,

    /// Live-tail mode: the underlying session file is still being
    /// written, and the viewer wants to follow new events.
    live: bool,

    /// Follow-mode: when `true`, any new events appended via
    /// [`append_events`] re-pin the cursor to the bottom of the
    /// transcript so the user sees new content without scrolling.
    /// Manual scrolling (`j`/`k`/`Ctrl-d`/`Ctrl-u`/`gg`) disables
    /// follow-mode; `G` and `F` re-engage it.
    follow: bool,

    /// Latched request to snap to the bottom on the first non-empty
    /// relayout. `new_live` sets this because `hydrate` runs before
    /// the first viewport is known: at construction time `lines` is
    /// empty and `jump_bottom` would no-op. The flag is consumed the
    /// first time `relayout` produces a non-empty cache.
    needs_initial_bottom: bool,

    /// In-viewer search UI mode. Owns the query buffer, committed
    /// matches, and the current match cursor.
    search: SearchMode,

    /// Ephemeral one-line message shown in the status bar until the next
    /// keypress clears it. Used for "not collapsible", "no more user
    /// turns", etc.
    flash: Option<String>,

    dirty: bool,
}

impl TranscriptState {
    pub fn new(transcript: Transcript) -> Self {
        Self {
            transcript,
            lines: Vec::new(),
            user_turn_line_starts: Vec::new(),
            viewport_width: 0,
            viewport_height: 0,
            scroll: 0,
            cursor: 0,
            collapsed: HashSet::new(),
            collapse_all: CollapseAll::Off,
            pending_g: false,
            pending_m: false,
            pending_quote: false,
            marks: SessionMarks::new(),
            marks_path: None,
            live: false,
            follow: false,
            needs_initial_bottom: false,
            search: SearchMode::new(),
            flash: None,
            dirty: true,
        }
    }

    /// Construct a state for a live-tail viewer: the same as [`new`]
    /// but `live` and `follow` default to `true` so new events pin the
    /// cursor to the bottom until the user scrolls up manually. Also
    /// latches `needs_initial_bottom` so the first non-empty relayout
    /// pins the cursor to the tail — which is what a live-tail user
    /// actually wants to see on open, rather than the top of the
    /// backlog.
    pub fn new_live(transcript: Transcript) -> Self {
        let mut s = Self::new(transcript);
        s.live = true;
        s.follow = true;
        s.needs_initial_bottom = true;
        s
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Explicitly toggle live follow-mode. No-op when not live-tailing.
    pub fn toggle_follow(&mut self) {
        if !self.live {
            return;
        }
        self.follow = !self.follow;
        if self.follow {
            self.jump_bottom();
            self.set_flash("follow: on");
        } else {
            self.set_flash("follow: off");
        }
    }

    /// Append newly-observed events to the transcript and refresh the
    /// line cache. If follow-mode is on, re-pins the cursor to the
    /// bottom so the user sees the new events immediately. No-op when
    /// `events` is empty.
    pub fn append_events(&mut self, events: impl IntoIterator<Item = Event>) {
        let before = self.transcript.messages.len();
        self.transcript.append_events(events);
        if self.transcript.messages.len() == before {
            return;
        }
        self.dirty = true;
        self.relayout();
        if self.follow {
            self.jump_bottom();
        }
    }

    /// Replace the entire transcript — used after a `TailReader`
    /// rewrite (compaction) where the on-disk line order has changed.
    /// Cursor / scroll state is preserved when possible but follow
    /// mode snaps to the bottom to keep the user at the new head.
    ///
    /// Clears the collapse set and marks because `BlockId` and
    /// `Mark` are positional (`msg_index`, `block_index`): after
    /// compaction those coordinates refer to different blocks (or
    /// none), so keeping the old entries would quietly decorate
    /// random unrelated blocks and land jumps on the wrong turn.
    pub fn reset_transcript(&mut self, transcript: Transcript) {
        self.transcript = transcript;
        self.collapsed.clear();
        self.marks.clear();
        self.dirty = true;
        self.relayout();
        if self.follow {
            self.jump_bottom();
        }
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn lines(&self) -> &[RenderedLine] {
        &self.lines
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn viewport_height(&self) -> u16 {
        self.viewport_height
    }

    /// Current message index under the cursor, or 0 if the transcript is empty.
    pub fn current_msg_index(&self) -> usize {
        self.lines
            .get(self.cursor)
            .map(|l| l.msg_index)
            .unwrap_or(0)
    }

    pub fn collapsed(&self) -> &HashSet<BlockId> {
        &self.collapsed
    }

    pub fn collapse_all(&self) -> CollapseAll {
        self.collapse_all
    }

    pub fn flash(&self) -> Option<&str> {
        self.flash.as_deref()
    }

    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some(msg.into());
    }

    pub fn clear_flash(&mut self) {
        self.flash = None;
    }

    /// Inform the state of the current body area size. Triggers a relayout
    /// when the width changes (wrapping depends on width; height doesn't).
    pub fn set_viewport(&mut self, width: u16, height: u16) {
        if width != self.viewport_width {
            self.viewport_width = width;
            self.dirty = true;
        }
        self.viewport_height = height;
        if self.dirty {
            self.relayout();
        }
        self.clamp_scroll();
    }

    fn relayout(&mut self) {
        let ctx = layout::CollapseContext {
            collapsed: &self.collapsed,
            collapse_all: self.collapse_all,
        };
        let out = layout::build(&self.transcript, self.viewport_width, &ctx);
        self.lines = out.lines;
        self.user_turn_line_starts = out.user_turn_line_starts;
        self.dirty = false;
        if self.cursor >= self.lines.len() {
            self.cursor = self.lines.len().saturating_sub(1);
        }
        // Consume the latched "jump to bottom on first real layout"
        // flag set by `new_live`. Without this, a live-tail viewer
        // opened against an existing backlog would display the top
        // of the backlog until a new tail event arrived, because
        // `new_live` runs before the first viewport is known.
        if self.needs_initial_bottom && !self.lines.is_empty() {
            self.cursor = self.lines.len() - 1;
            self.needs_initial_bottom = false;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let total = self.lines.len();
        let height = self.viewport_height as usize;
        let max_scroll = total.saturating_sub(height);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height && height > 0 {
            self.scroll = self.cursor + 1 - height;
        }
    }

    // --- navigation -------------------------------------------------------

    /// Manual scroll keys break follow-mode: if the user is steering
    /// the cursor themselves, auto-scrolling on new events would yank
    /// them away from what they're reading.
    fn disable_follow_on_manual_scroll(&mut self) {
        if self.follow {
            self.follow = false;
            self.set_flash("paused (press F or G to follow)");
        }
    }

    pub fn move_down(&mut self, n: usize) {
        self.disable_follow_on_manual_scroll();
        let max = self.lines.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
        self.clamp_scroll();
    }

    pub fn move_up(&mut self, n: usize) {
        self.disable_follow_on_manual_scroll();
        self.cursor = self.cursor.saturating_sub(n);
        self.clamp_scroll();
    }

    pub fn half_page_down(&mut self) {
        let n = (self.viewport_height as usize / 2).max(1);
        self.move_down(n);
    }

    pub fn half_page_up(&mut self) {
        let n = (self.viewport_height as usize / 2).max(1);
        self.move_up(n);
    }

    pub fn jump_top(&mut self) {
        self.disable_follow_on_manual_scroll();
        self.cursor = 0;
        self.scroll = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.cursor = self.lines.len().saturating_sub(1);
        self.clamp_scroll();
        // G always re-engages follow mode in live sessions — it's how
        // the user explicitly asks for "take me to the head again".
        if self.live {
            self.follow = true;
        }
    }

    /// Jump the cursor to the next user-turn line (`}`).
    pub fn jump_next_user_turn(&mut self) {
        self.disable_follow_on_manual_scroll();
        let next = self
            .user_turn_line_starts
            .iter()
            .copied()
            .find(|&start| start > self.cursor);
        match next {
            Some(target) => {
                self.cursor = target;
                self.clamp_scroll();
            }
            None => self.set_flash("no more user turns"),
        }
    }

    /// Jump the cursor to the previous user-turn line (`{`).
    pub fn jump_prev_user_turn(&mut self) {
        self.disable_follow_on_manual_scroll();
        let prev = self
            .user_turn_line_starts
            .iter()
            .copied()
            .rev()
            .find(|&start| start < self.cursor);
        match prev {
            Some(target) => {
                self.cursor = target;
                self.clamp_scroll();
            }
            None => self.set_flash("no previous user turns"),
        }
    }

    // --- collapse ---------------------------------------------------------

    /// Toggle collapse on the block under the cursor (`t`). Only thinking,
    /// tool calls, and tool results are collapsible — other kinds flash
    /// "not collapsible".
    pub fn toggle_current_block(&mut self) {
        let Some(block_id) = self.lines.get(self.cursor).and_then(|l| l.block_id()) else {
            self.set_flash("no block under cursor");
            return;
        };
        let (msg_idx, block_idx) = block_id;
        let Some(block) = self
            .transcript
            .messages
            .get(msg_idx)
            .and_then(|m| m.blocks.get(block_idx))
        else {
            self.set_flash("no block under cursor");
            return;
        };
        if !is_collapsible(block) {
            self.set_flash("not collapsible");
            return;
        }
        if !self.collapsed.insert(block_id) {
            self.collapsed.remove(&block_id);
        }
        self.dirty = true;
        self.relayout();
    }

    /// Cycle the `T` collapse-all state between off and tools+thinking.
    /// Clears any individual collapse entries so the cycle is predictable.
    pub fn toggle_collapse_all(&mut self) {
        self.collapsed.clear();
        self.collapse_all = match self.collapse_all {
            CollapseAll::Off => CollapseAll::ToolsAndThinking,
            CollapseAll::ToolsAndThinking => CollapseAll::Off,
        };
        self.dirty = true;
        self.relayout();
        match self.collapse_all {
            CollapseAll::Off => self.set_flash("expanded all"),
            CollapseAll::ToolsAndThinking => self.set_flash("collapsed tools & thinking"),
        }
    }

    pub fn pending_g(&self) -> bool {
        self.pending_g
    }

    pub fn set_pending_g(&mut self, pending: bool) {
        self.pending_g = pending;
    }

    pub fn pending_m(&self) -> bool {
        self.pending_m
    }

    pub fn set_pending_m(&mut self, pending: bool) {
        self.pending_m = pending;
    }

    pub fn pending_quote(&self) -> bool {
        self.pending_quote
    }

    pub fn set_pending_quote(&mut self, pending: bool) {
        self.pending_quote = pending;
    }

    // --- marks ------------------------------------------------------------

    /// Point the state at a marks.json file and preload this session's
    /// marks from it. Called by `app.rs` after constructing the state;
    /// tests that don't want to touch disk simply skip this.
    pub fn attach_marks_file(&mut self, path: PathBuf) {
        let file = checkpoints::load(&path);
        if let Some(marks) = file.sessions.get(&self.transcript.session_id) {
            self.marks = marks.clone();
        }
        self.marks_path = Some(path);
    }

    #[cfg(test)]
    pub fn marks(&self) -> &SessionMarks {
        &self.marks
    }

    /// Set the mark `letter` to the current cursor position and persist.
    /// Letters outside `a..=z` are rejected with a flash — matching vim's
    /// lowercase-only local marks.
    pub fn set_mark(&mut self, letter: char) {
        if !letter.is_ascii_lowercase() {
            self.set_flash(format!("bad mark '{letter}'"));
            return;
        }
        let Some(line) = self.lines.get(self.cursor) else {
            self.set_flash("nothing to mark");
            return;
        };
        let mark = Mark {
            msg_index: line.msg_index,
            block_index: line.block_index,
        };
        self.marks.insert(letter, mark);
        self.set_flash(format!("mark {letter} set"));
        self.save_marks();
    }

    /// Jump the cursor to the position saved under `letter`. Resolves the
    /// anchor against the current line cache — if the target block has
    /// since been collapsed, we fall back to the first line whose
    /// `msg_index` matches, so the jump always lands somewhere reasonable.
    pub fn jump_to_mark(&mut self, letter: char) {
        if !letter.is_ascii_lowercase() {
            self.set_flash(format!("bad mark '{letter}'"));
            return;
        }
        let Some(&mark) = self.marks.get(&letter) else {
            self.set_flash(format!("no mark {letter}"));
            return;
        };
        let exact = self
            .lines
            .iter()
            .position(|l| l.msg_index == mark.msg_index && l.block_index == mark.block_index);
        let fallback = exact.or_else(|| {
            self.lines
                .iter()
                .position(|l| l.msg_index == mark.msg_index)
        });
        match fallback {
            Some(line) => {
                self.cursor = line;
                self.clamp_scroll();
                self.set_flash(format!("'{letter}"));
            }
            None => self.set_flash(format!("mark {letter} out of range")),
        }
    }

    /// Persist the current session's marks back to `marks.json`. Uses
    /// [`checkpoints::update_session`] so concurrent viewers cannot
    /// clobber each other's session entries via an interleaved
    /// read/modify/write.
    ///
    /// Merges against `current` rather than overwriting: if the same
    /// session is open in two viewers, both sides see the union of
    /// marks — last-write-wins per letter, but letters the other
    /// viewer set are preserved. Failures are logged but not surfaced;
    /// a broken home directory should not break the viewer mid-session.
    fn save_marks(&self) {
        let Some(path) = self.marks_path.as_ref() else {
            return;
        };
        let local = self.marks.clone();
        let result = checkpoints::update_session(path, &self.transcript.session_id, |current| {
            let mut merged = current.cloned().unwrap_or_default();
            for (letter, mark) in local {
                merged.insert(letter, mark);
            }
            Some(merged)
        });
        if let Err(e) = result {
            tracing::warn!(error = %e, "failed to persist marks.json");
        }
    }

    // --- search -----------------------------------------------------------

    pub fn search_mode(&self) -> &SearchMode {
        &self.search
    }

    /// Enter search-typing mode with an empty query (response to `/`).
    pub fn begin_search(&mut self) {
        self.search = SearchMode::Typing {
            query: String::new(),
        };
    }

    /// Cancel any in-flight search and clear highlights (response to
    /// `Esc` while searching).
    pub fn cancel_search(&mut self) {
        self.search = SearchMode::Idle;
    }

    /// Append one character to the search query buffer. Live-rebuilds
    /// the match list so the user sees highlights update as they type.
    pub fn push_search_char(&mut self, ch: char) {
        if let SearchMode::Typing { query } = &mut self.search {
            query.push(ch);
        }
    }

    /// Pop the last character from the search query buffer (backspace
    /// while in typing mode).
    pub fn pop_search_char(&mut self) {
        if let SearchMode::Typing { query } = &mut self.search {
            query.pop();
        }
    }

    /// Commit the current typing-mode query to an active search. Jumps
    /// the cursor to the first match (if any) and flashes a summary.
    pub fn commit_search(&mut self) {
        let SearchMode::Typing { query } = &self.search else {
            return;
        };
        let query = query.clone();
        if query.is_empty() {
            self.search = SearchMode::Idle;
            return;
        }
        let index = SearchIndex::build(&self.lines);
        let matches = index.find_all(&query);
        if matches.is_empty() {
            self.search = SearchMode::Idle;
            self.set_flash(format!("no match for \"{query}\""));
            return;
        }
        let first = matches[0];
        let total = matches.len();
        self.search = SearchMode::Active {
            query,
            matches,
            cursor: 0,
        };
        self.jump_to_match_line(first);
        self.set_flash(format!("1/{total}"));
    }

    /// `n` — advance to the next match; wraps around at the end.
    pub fn next_match(&mut self) {
        let target = match &mut self.search {
            SearchMode::Active {
                matches, cursor, ..
            } if !matches.is_empty() => {
                *cursor = (*cursor + 1) % matches.len();
                Some((matches[*cursor], *cursor, matches.len()))
            }
            _ => None,
        };
        if let Some((m, i, total)) = target {
            self.jump_to_match_line(m);
            self.set_flash(format!("{}/{}", i + 1, total));
        }
    }

    /// `N` — step to the previous match; wraps around at the start.
    pub fn prev_match(&mut self) {
        let target = match &mut self.search {
            SearchMode::Active {
                matches, cursor, ..
            } if !matches.is_empty() => {
                *cursor = if *cursor == 0 {
                    matches.len() - 1
                } else {
                    *cursor - 1
                };
                Some((matches[*cursor], *cursor, matches.len()))
            }
            _ => None,
        };
        if let Some((m, i, total)) = target {
            self.jump_to_match_line(m);
            self.set_flash(format!("{}/{}", i + 1, total));
        }
    }

    fn jump_to_match_line(&mut self, m: SearchMatch) {
        if m.line < self.lines.len() {
            self.cursor = m.line;
            self.clamp_scroll();
        }
    }
}

/// A block is collapsible via `t` if it has any body worth hiding:
/// thinking, tool calls, tool results. Text/attachment/unknown are not.
fn is_collapsible(block: &Block) -> bool {
    matches!(
        block,
        Block::Thinking(_) | Block::ToolCall { .. } | Block::ToolResult { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::jsonl::parse_line;
    use ccs_core::transcript::from_events;

    fn t_with(lines: &[&str]) -> Transcript {
        from_events(lines.iter().map(|l| parse_line(l).unwrap()))
    }

    fn event(line: &str) -> Event {
        parse_line(line).unwrap()
    }

    #[test]
    fn new_live_starts_in_follow_mode() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        assert!(s.is_live());
        assert!(s.is_following());
    }

    #[test]
    fn new_live_with_existing_backlog_lands_cursor_at_bottom() {
        // Regression: hydrate() no longer calls jump_bottom() because
        // lines is empty at construction time; the first relayout has
        // to consume the `needs_initial_bottom` flag instead.
        let lines: Vec<String> = (0..10)
            .map(|i| {
                format!(
                    r#"{{"type":"user","uuid":"u{i}","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"msg-{i}"}}}}"#
                )
            })
            .collect();
        let t: Transcript = from_events(lines.iter().map(|l| parse_line(l).unwrap()));
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        assert!(
            !s.lines().is_empty(),
            "layout should have produced lines by now"
        );
        assert_eq!(
            s.cursor(),
            s.lines().len() - 1,
            "live viewer with existing backlog must open at the tail, not the top"
        );
    }

    #[test]
    fn reset_transcript_clears_collapsed_and_marks() {
        // Regression: BlockId and Mark are positional, so compaction
        // makes old coordinates meaningless. reset_transcript must
        // wipe them rather than decorate random new blocks.
        let t = t_with(&[
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"}]}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        // Land cursor on the thinking body, collapse it, and plant a mark.
        for _ in 0..20 {
            if let Some(rl) = s.lines().get(s.cursor()) {
                if rl.block_index == Some(0) && rl.kind == LineKind::Body {
                    break;
                }
            }
            s.move_down(1);
        }
        s.toggle_current_block();
        assert!(!s.collapsed().is_empty());
        s.set_mark('a');
        assert!(s.marks().contains_key(&'a'));

        // Fresh transcript after compaction.
        let fresh = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"new"}}"#,
        ]);
        s.reset_transcript(fresh);
        assert!(s.collapsed().is_empty());
        assert!(s.marks().is_empty());
    }

    #[test]
    fn append_events_jumps_cursor_to_bottom_when_following() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"one"}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        let initial_lines = s.lines().len();

        s.append_events(vec![
            event(r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"two"}}"#),
            event(r#"{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"three"}}"#),
        ]);
        assert!(s.lines().len() > initial_lines);
        // Cursor should be pinned to the last rendered line.
        assert_eq!(s.cursor(), s.lines().len() - 1);
    }

    #[test]
    fn manual_scroll_disables_follow_and_append_no_longer_jumps() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"one"}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"two"}}"#,
            r#"{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"three"}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        s.jump_bottom();
        assert!(s.is_following());

        // User scrolls up manually → follow disengages.
        s.move_up(2);
        assert!(!s.is_following());
        let paused_cursor = s.cursor();

        // A new event should not yank the cursor away.
        s.append_events(vec![event(
            r#"{"type":"user","uuid":"u4","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"four"}}"#,
        )]);
        assert_eq!(s.cursor(), paused_cursor);
    }

    #[test]
    fn capital_g_reengages_follow_in_live_mode() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"one"}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"two"}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        s.move_up(5);
        assert!(!s.is_following());
        s.jump_bottom();
        assert!(s.is_following());
    }

    #[test]
    fn toggle_follow_is_noop_outside_live_mode() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"one"}}"#,
        ]);
        let mut s = TranscriptState::new(t);
        s.set_viewport(80, 20);
        assert!(!s.is_live());
        s.toggle_follow();
        assert!(!s.is_following());
    }

    #[test]
    fn append_empty_slice_is_noop() {
        let t = t_with(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ]);
        let mut s = TranscriptState::new_live(t);
        s.set_viewport(80, 20);
        let before_cursor = s.cursor();
        let before_lines = s.lines().len();
        s.append_events(Vec::<Event>::new());
        assert_eq!(s.cursor(), before_cursor);
        assert_eq!(s.lines().len(), before_lines);
    }
}
