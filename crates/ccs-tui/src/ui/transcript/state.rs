//! Owns the transcript, the pre-rendered line cache, and viewport/scroll state.

use std::collections::HashSet;

use ccs_core::transcript::{Block, Transcript};
use ratatui::text::Line;

use super::layout;

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
            flash: None,
            dirty: true,
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

    pub fn move_down(&mut self, n: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
        self.clamp_scroll();
    }

    pub fn move_up(&mut self, n: usize) {
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
        self.cursor = 0;
        self.scroll = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.cursor = self.lines.len().saturating_sub(1);
        self.clamp_scroll();
    }

    /// Jump the cursor to the next user-turn line (`}`).
    pub fn jump_next_user_turn(&mut self) {
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
}

/// A block is collapsible via `t` if it has any body worth hiding:
/// thinking, tool calls, tool results. Text/attachment/unknown are not.
fn is_collapsible(block: &Block) -> bool {
    matches!(
        block,
        Block::Thinking(_) | Block::ToolCall { .. } | Block::ToolResult { .. }
    )
}
