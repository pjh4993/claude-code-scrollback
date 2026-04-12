//! Incremental case-insensitive substring search across the visible line
//! cache.
//!
//! Build strategy: concatenate the plain-text content of every
//! [`RenderedLine`] into a single lowercase `haystack` buffer, remembering
//! each line's start offset. Query with [`memchr::memmem::Finder`] and
//! map match byte offsets back to `(line_idx, col_start, col_end)` via
//! binary search on the boundary table.
//!
//! **Current limitation:** the index is built from the already-rendered
//! line cache, so matches inside collapsed blocks are invisible until
//! the user expands them (manually or via `T`). Per-block full-transcript
//! search with auto-expand on jump is tracked as a follow-up.

use memchr::memmem::Finder;

use super::state::RenderedLine;

/// Precomputed, case-insensitive substring index over a slice of
/// [`RenderedLine`]s. Rebuilt whenever the underlying cache changes.
pub struct SearchIndex {
    /// Lowercase concatenation of every visible line's plain text,
    /// joined with `\n` so match offsets can't accidentally span lines.
    haystack: String,
    /// `line_starts[i]` is the byte offset in `haystack` where the
    /// plain-text content of rendered line `i` begins. There is always
    /// exactly one entry per input line and one trailing sentinel equal
    /// to `haystack.len()` so `line_starts.windows(2)` yields per-line
    /// slices.
    line_starts: Vec<usize>,
}

/// One match in the search index, resolved back to the rendered-line
/// coordinate system the viewer uses for highlighting and cursor jumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Index into the [`TranscriptState`](super::state::TranscriptState)
    /// line cache.
    pub line: usize,
    /// Byte offset of the match start **within** the plain text of
    /// `line`. Relative to the line, not the full haystack.
    pub byte_start: usize,
    /// Byte offset one past the last matched byte, within the line.
    pub byte_end: usize,
}

impl SearchIndex {
    /// Build an index over `lines`. Empty `lines` produces an index that
    /// never matches anything, which is what the viewer wants during the
    /// very first draw.
    pub fn build(lines: &[RenderedLine]) -> Self {
        let mut haystack = String::new();
        let mut line_starts = Vec::with_capacity(lines.len() + 1);
        for line in lines {
            line_starts.push(haystack.len());
            for span in &line.line.spans {
                for ch in span.content.chars() {
                    // Cheap ASCII fold; keeps byte offsets stable for
                    // the common case without a separate normalization
                    // pass. Non-ASCII is matched as-is via memmem.
                    if ch.is_ascii_uppercase() {
                        haystack.push(ch.to_ascii_lowercase());
                    } else {
                        haystack.push(ch);
                    }
                }
            }
            haystack.push('\n');
        }
        line_starts.push(haystack.len());
        Self {
            haystack,
            line_starts,
        }
    }

    /// Find every (non-overlapping) match of `query` in the index.
    /// Empty queries and queries that contain a newline return no matches.
    pub fn find_all(&self, query: &str) -> Vec<SearchMatch> {
        if query.is_empty() || query.contains('\n') || self.haystack.is_empty() {
            return Vec::new();
        }
        let needle: String = query
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c
                }
            })
            .collect();
        let finder = Finder::new(needle.as_bytes());
        let mut out: Vec<SearchMatch> = Vec::new();
        for byte_offset in finder.find_iter(self.haystack.as_bytes()) {
            let line = self.line_of(byte_offset);
            // Skip matches that straddle the synthetic newline — they
            // can't happen because we rejected query with '\n' above,
            // but this keeps the invariant explicit.
            let line_start = self.line_starts[line];
            let line_end = self.line_starts[line + 1].saturating_sub(1);
            let byte_start = byte_offset - line_start;
            let byte_end = (byte_offset + needle.len()) - line_start;
            if line_start + byte_end > line_end {
                continue;
            }
            out.push(SearchMatch {
                line,
                byte_start,
                byte_end,
            });
        }
        out
    }

    fn line_of(&self, byte_offset: usize) -> usize {
        // Binary search for the line whose range contains `byte_offset`.
        // `line_starts` has `lines.len() + 1` entries with the last one
        // equal to `haystack.len()`, so `partition_point` returns a
        // valid line index.
        self.line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1)
    }
}

/// Operational mode of the in-viewer search UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    /// No active search. `/` transitions into [`Typing`](Self::Typing).
    Idle,
    /// User pressed `/` and is building up a query. `Enter` commits,
    /// `Esc` cancels.
    Typing { query: String },
    /// A query has been committed. `n`/`N` step through `matches`.
    Active {
        query: String,
        matches: Vec<SearchMatch>,
        cursor: usize,
    },
}

impl SearchMode {
    pub fn new() -> Self {
        Self::Idle
    }

    pub fn is_typing(&self) -> bool {
        matches!(self, Self::Typing { .. })
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    pub fn query(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Typing { query } | Self::Active { query, .. } => Some(query),
        }
    }

    pub fn current_match(&self) -> Option<SearchMatch> {
        match self {
            Self::Active {
                matches, cursor, ..
            } if !matches.is_empty() => Some(matches[*cursor]),
            _ => None,
        }
    }
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::transcript::state::{LineKind, RenderedLine};
    use ratatui::text::{Line, Span};

    fn line(text: &str, msg_index: usize, block_index: Option<usize>) -> RenderedLine {
        RenderedLine {
            line: Line::from(Span::raw(text.to_string())),
            msg_index,
            block_index,
            kind: if block_index.is_some() {
                LineKind::Body
            } else {
                LineKind::Header
            },
        }
    }

    #[test]
    fn empty_index_matches_nothing() {
        let idx = SearchIndex::build(&[]);
        assert!(idx.find_all("anything").is_empty());
    }

    #[test]
    fn finds_single_match_case_insensitively() {
        let lines = vec![line("Hello World", 0, Some(0))];
        let idx = SearchIndex::build(&lines);
        let m = idx.find_all("hello");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 0);
        assert_eq!(m[0].byte_start, 0);
        assert_eq!(m[0].byte_end, 5);
    }

    #[test]
    fn finds_multiple_matches_across_lines() {
        let lines = vec![
            line("alpha bravo", 0, Some(0)),
            line("bravo charlie", 0, Some(0)),
            line("delta", 0, Some(0)),
        ];
        let idx = SearchIndex::build(&lines);
        let m = idx.find_all("bravo");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].line, 0);
        assert_eq!(m[0].byte_start, 6);
        assert_eq!(m[0].byte_end, 11);
        assert_eq!(m[1].line, 1);
        assert_eq!(m[1].byte_start, 0);
        assert_eq!(m[1].byte_end, 5);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let lines = vec![line("hello", 0, Some(0))];
        let idx = SearchIndex::build(&lines);
        assert!(idx.find_all("").is_empty());
    }

    #[test]
    fn query_with_newline_returns_nothing() {
        let lines = vec![line("hello", 0, Some(0))];
        let idx = SearchIndex::build(&lines);
        assert!(idx.find_all("he\nllo").is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let lines = vec![line("hello world", 0, Some(0))];
        let idx = SearchIndex::build(&lines);
        assert!(idx.find_all("zzz").is_empty());
    }

    #[test]
    fn match_inside_multi_span_line() {
        // A line made of two spans: search must see the joined text.
        let rl = RenderedLine {
            line: Line::from(vec![Span::raw("foo "), Span::raw("bar baz")]),
            msg_index: 0,
            block_index: Some(0),
            kind: LineKind::Body,
        };
        let idx = SearchIndex::build(&[rl]);
        let m = idx.find_all("bar");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 0);
        assert_eq!(m[0].byte_start, 4);
        assert_eq!(m[0].byte_end, 7);
    }

    #[test]
    fn mode_transitions() {
        let mut m = SearchMode::new();
        assert!(matches!(m, SearchMode::Idle));
        m = SearchMode::Typing {
            query: "ab".to_string(),
        };
        assert!(m.is_typing());
        assert_eq!(m.query(), Some("ab"));
        m = SearchMode::Active {
            query: "ab".to_string(),
            matches: vec![SearchMatch {
                line: 0,
                byte_start: 0,
                byte_end: 2,
            }],
            cursor: 0,
        };
        assert!(m.is_active());
        assert_eq!(m.current_match().unwrap().line, 0);
    }
}
