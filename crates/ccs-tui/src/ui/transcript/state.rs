//! Owns the transcript, the pre-rendered line cache, and viewport/scroll state.

use ccs_core::transcript::Transcript;
use ratatui::text::Line;

use super::layout;

/// What kind of source line a [`RenderedLine`] came from. Used by the
/// renderer for status-line accounting and by future slices (PJH-50 PR 3)
/// for collapse hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// The `── user ──` / `── assistant ──` header line of a message.
    Header,
    /// Body text of any block (text, thinking, tool call, tool result, ...).
    Body,
    /// Blank spacer between messages.
    Separator,
}

/// One pre-rendered terminal line. Owned (`'static`) so the cache outlives
/// any frame's borrow of the state.
#[derive(Debug, Clone)]
pub struct RenderedLine {
    pub line: Line<'static>,
    pub msg_index: usize,
    pub kind: LineKind,
}

/// Viewer state: transcript, cache, viewport, scroll, and vim-chord state.
///
/// The cache (`lines`) is rebuilt via [`relayout`] whenever the viewport
/// width changes or `dirty` is set. Height is only used for scroll math;
/// it does not invalidate the cache.
pub struct TranscriptState {
    transcript: Transcript,
    lines: Vec<RenderedLine>,

    viewport_width: u16,
    viewport_height: u16,

    /// Top visible line index into `lines`.
    scroll: usize,
    /// Cursor line (for gg/G targeting now, visible cursor in a later slice).
    cursor: usize,

    /// Pending vim chord state: `g` seen, waiting for the second `g`.
    pending_g: bool,

    dirty: bool,
}

impl TranscriptState {
    pub fn new(transcript: Transcript) -> Self {
        Self {
            transcript,
            lines: Vec::new(),
            viewport_width: 0,
            viewport_height: 0,
            scroll: 0,
            cursor: 0,
            pending_g: false,
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
        self.lines = layout::build(&self.transcript, self.viewport_width);
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
        // Keep cursor inside the viewport.
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

    pub fn pending_g(&self) -> bool {
        self.pending_g
    }

    pub fn set_pending_g(&mut self, pending: bool) {
        self.pending_g = pending;
    }
}
