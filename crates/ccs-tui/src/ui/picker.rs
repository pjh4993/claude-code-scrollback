//! Session picker screen — the app's landing page.
//!
//! Design goals (PJH-49):
//!
//! * **Instant cold launch** on any corpus size. No session JSONL is read at
//!   startup — [`PickerState::new`] only consumes the `fs::metadata` already
//!   collected by [`ccs_core::session::discover`].
//! * **Lazy preview population.** When the cursor lands on a row whose
//!   [`PickerRowData`] has not been loaded, the picker asks its injected
//!   [`SessionMetadataSource`] for the row's preview. Today the source is
//!   [`LazyFsSource`](ccs_core::metadata::LazyFsSource) which reads ~16 KiB
//!   of the target file; tomorrow (PJH-54) it becomes a SQLite lookup, with
//!   no state-machine changes required in this file.
//! * **Fuzzy search** over the project path and session id, using `nucleo`.
//!   Previews become searchable as they load — there is no pre-population
//!   pass because that would defeat the point of lazy loading.
//!
//! Filters (`--last-24h`, `by project`) and user-selectable sort orders are
//! **deliberately deferred** to a future iteration. v1 uses the defaults
//! called out in the ticket: sort by last-modified descending, and when
//! launched from inside a project cwd, auto-filter to that project — the
//! filtering is applied in `ccs-cli` before the `SessionFile` list reaches
//! this module.

use ccs_core::metadata::{PickerRowData, SessionMetadataSource};
use ccs_core::session::SessionFile;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One picker row — a session plus its (possibly empty) preview metadata.
pub struct PickerRow {
    pub session: SessionFile,
    pub meta: PickerRowData,
    /// True once the metadata source has been consulted for this row, so we
    /// never re-fetch after a miss. Misses stay missing until PJH-54 ships
    /// a source that can supply the data.
    meta_loaded: bool,
}

impl PickerRow {
    fn new(session: SessionFile) -> Self {
        Self {
            session,
            meta: PickerRowData::default(),
            meta_loaded: false,
        }
    }
}

/// Event-loop state for the picker. Owned by `Screen::Picker`.
pub struct PickerState {
    /// All rows in display order (default sort: last-modified desc).
    rows: Vec<PickerRow>,
    /// Indices into `rows`, narrowed by the current fuzzy-search query.
    filtered: Vec<usize>,
    /// Cursor position into `filtered`.
    cursor: usize,
    /// Current fuzzy-search query. Empty ⇒ `filtered == 0..rows.len()`.
    search_query: String,
    /// True while `/` search is active and key events route to the query
    /// buffer instead of navigation.
    pub search_mode: bool,
    /// Set when the user presses Enter on a row. The app consumes this and
    /// transitions to the viewer screen on the next tick.
    open_requested: bool,

    source: Box<dyn SessionMetadataSource + Send>,
    matcher: Matcher,
    char_buf: Vec<char>,
}

impl PickerState {
    /// Build a picker over the given sessions.
    ///
    /// Sort order:
    ///
    /// * Primary — **cwd affinity**: sessions whose `project_cwd` shares a
    ///   deeper common path prefix with `launch_cwd` rank higher. This puts
    ///   the sessions most relevant to where the user launched from at the
    ///   top of the list, without hiding other sessions via a hard filter.
    ///   `launch_cwd = None` skips this key entirely and falls back to pure
    ///   mtime-desc ordering.
    /// * Secondary — **last modified descending**: within the same affinity
    ///   bucket, the freshest session comes first.
    pub fn new(
        mut sessions: Vec<SessionFile>,
        source: Box<dyn SessionMetadataSource + Send>,
        launch_cwd: Option<&Path>,
    ) -> Self {
        let launch_cwd: Option<PathBuf> = launch_cwd.map(|p| p.to_path_buf());
        sessions.sort_by(|a, b| {
            let a_affinity = launch_cwd
                .as_deref()
                .map(|cwd| cwd_affinity(&a.project_cwd, cwd))
                .unwrap_or(0);
            let b_affinity = launch_cwd
                .as_deref()
                .map(|cwd| cwd_affinity(&b.project_cwd, cwd))
                .unwrap_or(0);
            b_affinity
                .cmp(&a_affinity)
                .then_with(|| b.modified.cmp(&a.modified))
        });
        let rows: Vec<_> = sessions.into_iter().map(PickerRow::new).collect();
        let filtered: Vec<usize> = (0..rows.len()).collect();
        let mut state = Self {
            rows,
            filtered,
            cursor: 0,
            search_query: String::new(),
            search_mode: false,
            open_requested: false,
            source,
            matcher: Matcher::new(Config::DEFAULT),
            char_buf: Vec::new(),
        };
        state.ensure_preview_for_cursor();
        state
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn visible_len(&self) -> usize {
        self.filtered.len()
    }

    pub fn total_len(&self) -> usize {
        self.rows.len()
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Consume a pending open request, returning the selected session if
    /// one was staged by [`request_open`](Self::request_open).
    pub fn take_open_request(&mut self) -> Option<SessionFile> {
        if !self.open_requested {
            return None;
        }
        self.open_requested = false;
        self.filtered
            .get(self.cursor)
            .and_then(|&idx| self.rows.get(idx))
            .map(|row| row.session.clone())
    }

    pub fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        if self.cursor + 1 < self.filtered.len() {
            self.cursor += 1;
            self.ensure_preview_for_cursor();
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.ensure_preview_for_cursor();
        }
    }

    pub fn jump_top(&mut self) {
        self.cursor = 0;
        self.ensure_preview_for_cursor();
    }

    pub fn jump_bottom(&mut self) {
        if !self.filtered.is_empty() {
            self.cursor = self.filtered.len() - 1;
            self.ensure_preview_for_cursor();
        }
    }

    pub fn request_open(&mut self) {
        if !self.filtered.is_empty() {
            self.open_requested = true;
        }
    }

    pub fn enter_search(&mut self) {
        self.search_mode = true;
    }

    pub fn exit_search(&mut self) {
        self.search_mode = false;
    }

    pub fn push_search_char(&mut self, c: char) {
        self.search_query.push(c);
        self.recompute_filter();
    }

    pub fn pop_search_char(&mut self) {
        if self.search_query.pop().is_some() {
            self.recompute_filter();
        }
    }

    pub fn clear_search(&mut self) {
        if !self.search_query.is_empty() {
            self.search_query.clear();
            self.recompute_filter();
        }
    }

    fn ensure_preview_for_cursor(&mut self) {
        let Some(&row_idx) = self.filtered.get(self.cursor) else {
            return;
        };
        let row = &mut self.rows[row_idx];
        if row.meta_loaded {
            return;
        }
        row.meta = self.source.fetch(&row.session);
        row.meta_loaded = true;
    }

    /// Rescore every row against the current query. Empty query resets to
    /// the natural (last-modified desc) order. With a query, the result is
    /// sorted by match score desc then by modified desc as a tiebreaker.
    fn recompute_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.rows.len()).collect();
            self.cursor = 0;
            self.ensure_preview_for_cursor();
            return;
        }

        let pattern = Pattern::parse(
            &self.search_query,
            CaseMatching::Smart,
            Normalization::Smart,
        );

        let mut scored: Vec<(u32, usize)> = Vec::new();
        for (idx, row) in self.rows.iter().enumerate() {
            let haystack = searchable_text(row);
            self.char_buf.clear();
            let utf = nucleo::Utf32Str::new(&haystack, &mut self.char_buf);
            if let Some(score) = pattern.score(utf, &mut self.matcher) {
                scored.push((score, idx));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.filtered = scored.into_iter().map(|(_, idx)| idx).collect();
        self.cursor = 0;
        self.ensure_preview_for_cursor();
    }
}

/// Score how relevant `project_cwd` is to `launch_cwd` for default-sort
/// purposes.
///
/// The comparison runs on the **encoded** form of both paths (`/` → `-`)
/// rather than the decoded form. This sidesteps the lossy decoding done by
/// [`ccs_core::session::decode_project_dir`], which splits any literal `-`
/// in a path segment into two path components — so a real project at
/// `/Users/alice/claude-code-scrollback` decodes to the nonsense path
/// `/Users/alice/claude/code/scrollback`, which in turn fails to prefix-
/// match against the user's actual launch directory. Comparing the encoded
/// strings avoids that entirely: encode-then-decode-then-re-encode is
/// always identity, so the encoded project directory name on disk is a
/// reliable key for prefix comparisons against the encoded launch cwd.
///
/// Returned score = shared-component count + a large `1000` bonus when one
/// encoded path is a strict ancestor of the other.
pub fn cwd_affinity(project_cwd: &Path, launch_cwd: &Path) -> i32 {
    let p_enc = encode_path(project_cwd);
    let l_enc = encode_path(launch_cwd);

    let shared = p_enc
        .split('-')
        .zip(l_enc.split('-'))
        .take_while(|(a, b)| a == b)
        .count() as i32;

    let prefix_bonus = if is_dash_prefix(&p_enc, &l_enc) || is_dash_prefix(&l_enc, &p_enc) {
        1000
    } else {
        0
    };

    shared + prefix_bonus
}

fn encode_path(p: &Path) -> String {
    p.to_string_lossy().replace('/', "-")
}

/// Is `shorter` an ancestor-or-equal prefix of `longer` in the encoded form?
/// A match requires that either the strings are equal, or `longer` starts
/// with `shorter` followed immediately by a `-` boundary — so
/// `-Users-pyler` is a prefix of `-Users-pyler-workspace` but not of
/// `-Users-pylerbogus`.
fn is_dash_prefix(shorter: &str, longer: &str) -> bool {
    if shorter == longer {
        return true;
    }
    if longer.len() <= shorter.len() {
        return false;
    }
    longer.starts_with(shorter) && longer.as_bytes()[shorter.len()] == b'-'
}

/// Render a project path for the picker's project column. Replaces the
/// user's home directory prefix with `~` so rows stay narrow on long
/// absolute paths like `/Users/<user>/workspace/...`.
///
/// The full path remains the canonical value in [`SessionFile::project_cwd`]
/// and the search index — this is display-layer only.
fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn searchable_text(row: &PickerRow) -> String {
    // Match on project cwd + session id today; previews join the search
    // surface as they're loaded (on cursor visit). The SQLite cache (PJH-54)
    // will make every row fully searchable at launch.
    let mut s = String::new();
    s.push_str(&row.session.project_cwd.display().to_string());
    s.push(' ');
    s.push_str(&row.session.session_id);
    if let Some(p) = &row.meta.first_prompt {
        s.push(' ');
        s.push_str(p);
    }
    s
}

/// Render the picker to `frame`, using `state` as the display source.
pub fn render(frame: &mut Frame, state: &PickerState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = if state.search_query.is_empty() {
        format!("claude-code-scrollback — {} sessions", state.rows.len())
    } else {
        format!(
            "claude-code-scrollback — {}/{} sessions · filter “{}”",
            state.filtered.len(),
            state.rows.len(),
            state.search_query
        )
    };
    frame.render_widget(Paragraph::new(title).style(Style::new().bold()), header);

    if state.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("no sessions found under ~/.claude/projects").style(Style::new().dim()),
            body,
        );
    } else {
        let widths = [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(40),
            Constraint::Min(20),
        ];
        let header_row = Row::new(vec![
            Cell::from("modified"),
            Cell::from("size"),
            Cell::from("project"),
            Cell::from("first prompt"),
        ])
        .style(
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        let rows: Vec<Row> = state
            .filtered
            .iter()
            .map(|&idx| {
                let row = &state.rows[idx];
                Row::new(vec![
                    Cell::from(format_relative_mtime(row.session.modified)),
                    Cell::from(format_size(row.session.size)),
                    Cell::from(display_path(&row.session.project_cwd)),
                    Cell::from(
                        row.meta
                            .first_prompt
                            .clone()
                            .unwrap_or_else(|| "…".to_string()),
                    ),
                ])
            })
            .collect();

        let table = Table::new(rows, widths)
            .header(header_row)
            .row_highlight_style(
                Style::new()
                    .bg(Color::Indexed(238))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌ ")
            .block(Block::default().borders(Borders::ALL));

        let mut table_state = TableState::default();
        if !state.filtered.is_empty() {
            table_state.select(Some(state.cursor));
        }
        frame.render_stateful_widget(table, body, &mut table_state);
    }

    let footer_text = if state.search_mode {
        format!("/{}  (Esc cancel · ↵ confirm)", state.search_query)
    } else {
        "j/k move  ·  / search  ·  ↵ open  ·  q quit".to_string()
    };
    frame.render_widget(Paragraph::new(footer_text).dim(), footer);
}

fn format_relative_mtime(t: SystemTime) -> String {
    let Ok(dur) = SystemTime::now().duration_since(t) else {
        return "future".into();
    };
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86400 * 30 {
        format!("{}d ago", secs / 86400)
    } else {
        format!("{}mo ago", secs / (86400 * 30))
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes < KB {
        format!("{bytes}B")
    } else if bytes < MB {
        format!("{}K", bytes / KB)
    } else {
        format!("{:.1}M", bytes as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::metadata::NullSource;
    use ccs_core::session::{SessionFile, SessionKind};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn session(id: &str, project: &str, mtime_offset_secs: u64, size: u64) -> SessionFile {
        SessionFile {
            session_id: id.into(),
            parent_session_id: None,
            kind: SessionKind::Primary,
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            project_cwd: PathBuf::from(project),
            modified: UNIX_EPOCH + Duration::from_secs(mtime_offset_secs),
            size,
        }
    }

    fn state(sessions: Vec<SessionFile>) -> PickerState {
        PickerState::new(sessions, Box::new(NullSource), None)
    }

    #[test]
    fn display_path_replaces_home_with_tilde() {
        // Synthesise a home-rooted path rather than using the real $HOME, so
        // the test is hermetic regardless of where it runs.
        let home = dirs::home_dir().expect("home dir available in test env");
        let under = home.join("workspace/claude-code-scrollback");
        assert_eq!(display_path(&under), "~/workspace/claude-code-scrollback");

        let exact_home = home.clone();
        assert_eq!(display_path(&exact_home), "~");

        let outside = PathBuf::from("/etc/hosts");
        assert_eq!(display_path(&outside), "/etc/hosts");
    }

    #[test]
    fn new_sorts_by_modified_desc() {
        let s = state(vec![
            session("old", "/a", 100, 0),
            session("new", "/b", 300, 0),
            session("mid", "/c", 200, 0),
        ]);
        let ids: Vec<_> = s
            .filtered
            .iter()
            .map(|&i| s.rows[i].session.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["new", "mid", "old"]);
    }

    #[test]
    fn cursor_navigation_respects_bounds() {
        let mut s = state(vec![session("a", "/a", 100, 0), session("b", "/b", 200, 0)]);
        assert_eq!(s.cursor, 0);
        s.move_up();
        assert_eq!(s.cursor, 0);
        s.move_down();
        assert_eq!(s.cursor, 1);
        s.move_down();
        assert_eq!(s.cursor, 1, "does not advance past last row");
        s.jump_top();
        assert_eq!(s.cursor, 0);
        s.jump_bottom();
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn search_narrows_to_matches_and_resets_cursor() {
        let mut s = state(vec![
            session("a", "/Users/alice/repo", 100, 0),
            session("b", "/Users/bob/work", 200, 0),
            session("c", "/Users/alice/notes", 300, 0),
        ]);
        s.move_down();
        s.move_down();
        assert_eq!(s.cursor, 2);

        for ch in "alice".chars() {
            s.push_search_char(ch);
        }

        assert_eq!(s.cursor, 0, "cursor resets to top after search changes");
        let matched_ids: Vec<_> = s
            .filtered
            .iter()
            .map(|&i| s.rows[i].session.session_id.clone())
            .collect();
        assert!(matched_ids.contains(&"a".to_string()));
        assert!(matched_ids.contains(&"c".to_string()));
        assert!(!matched_ids.contains(&"b".to_string()));
    }

    #[test]
    fn empty_search_restores_full_list() {
        let mut s = state(vec![
            session("a", "/alice", 100, 0),
            session("b", "/bob", 200, 0),
        ]);
        s.push_search_char('a');
        s.push_search_char('l');
        s.push_search_char('i');
        assert_eq!(s.filtered.len(), 1);
        s.pop_search_char();
        s.pop_search_char();
        s.pop_search_char();
        assert_eq!(s.filtered.len(), 2);
    }

    #[test]
    fn clear_search_resets_to_all_rows() {
        let mut s = state(vec![
            session("a", "/alice", 100, 0),
            session("b", "/bob", 200, 0),
        ]);
        s.push_search_char('a');
        s.clear_search();
        assert_eq!(s.filtered.len(), 2);
        assert_eq!(s.search_query, "");
    }

    #[test]
    fn request_open_emits_session_once() {
        let mut s = state(vec![session("a", "/a", 100, 0)]);
        assert!(s.take_open_request().is_none());
        s.request_open();
        let first = s.take_open_request();
        assert!(first.is_some());
        assert_eq!(first.unwrap().session_id, "a");
        assert!(
            s.take_open_request().is_none(),
            "open request is consumed on take"
        );
    }

    #[test]
    fn launch_cwd_ranks_closest_project_first() {
        // Fresh mtimes must NOT override cwd affinity — the session in the
        // user's actual directory should rank above a more recent session
        // from an unrelated project.
        let sessions = vec![
            session("unrelated-new", "/Users/alice/other-project", 500, 0),
            session("exact", "/Users/alice/workspace/repo/sub", 100, 0),
            session("ancestor", "/Users/alice/workspace/repo", 200, 0),
            session("sibling", "/Users/alice/workspace/other-repo", 300, 0),
            session("home-root", "/Users/alice", 400, 0),
        ];
        let cwd = PathBuf::from("/Users/alice/workspace/repo/sub");
        let s = PickerState::new(sessions, Box::new(NullSource), Some(&cwd));
        let order: Vec<_> = s
            .filtered
            .iter()
            .map(|&i| s.rows[i].session.session_id.clone())
            .collect();
        // exact (1000+6) > ancestor (1000+5) > home-root (1000+3)
        //   > sibling (shares /Users/alice/workspace = 4, no prefix bonus)
        //   > unrelated-new (shares /Users/alice = 3, no prefix bonus)
        assert_eq!(
            order,
            vec!["exact", "ancestor", "home-root", "sibling", "unrelated-new"],
        );
    }

    #[test]
    fn launch_cwd_none_falls_back_to_mtime_sort() {
        let s = PickerState::new(
            vec![
                session("old", "/a", 100, 0),
                session("new", "/b", 300, 0),
                session("mid", "/c", 200, 0),
            ],
            Box::new(NullSource),
            None,
        );
        let order: Vec<_> = s
            .filtered
            .iter()
            .map(|&i| s.rows[i].session.session_id.clone())
            .collect();
        assert_eq!(order, vec!["new", "mid", "old"]);
    }

    #[test]
    fn launch_cwd_beats_lossy_decode_collision() {
        // Real-world regression: when launched from a worktree under
        // /Users/pyler/workspace/claude-code-scrollback/..., the scrollback
        // session must rank above sessions at /Users/pyler. The trap is
        // that decode_project_dir turns the encoded dir
        // `-Users-pyler-workspace-claude-code-scrollback` into the lossy
        // path `/Users/pyler/workspace/claude/code/scrollback`, which no
        // longer prefix-matches the real launch cwd. The affinity function
        // must work around this by re-encoding.
        let sessions = vec![
            session(
                "home-recent",
                // project_cwd for /Users/pyler — freshest mtime by far.
                "/Users/pyler",
                10_000,
                0,
            ),
            session(
                "scrollback-old",
                // Lossy-decoded form of -Users-pyler-workspace-claude-code-scrollback
                "/Users/pyler/workspace/claude/code/scrollback",
                1_000,
                0,
            ),
        ];
        let cwd = PathBuf::from(
            "/Users/pyler/workspace/claude-code-scrollback/feat/tui-add-session-picker-screen",
        );
        let s = PickerState::new(sessions, Box::new(NullSource), Some(&cwd));
        let order: Vec<_> = s
            .filtered
            .iter()
            .map(|&i| s.rows[i].session.session_id.clone())
            .collect();
        assert_eq!(
            order,
            vec!["scrollback-old", "home-recent"],
            "scrollback session must outrank /Users/pyler even with older mtime"
        );
    }

    #[test]
    fn cwd_affinity_scoring() {
        use std::path::Path;
        let cwd = Path::new("/Users/alice/workspace/repo/sub");
        // Exact match beats ancestor.
        assert!(
            cwd_affinity(Path::new("/Users/alice/workspace/repo/sub"), cwd)
                > cwd_affinity(Path::new("/Users/alice/workspace/repo"), cwd)
        );
        // Ancestor beats sibling at same depth.
        assert!(
            cwd_affinity(Path::new("/Users/alice/workspace"), cwd)
                > cwd_affinity(Path::new("/Users/alice/other"), cwd)
        );
        // Sibling beats distant cousin.
        assert!(
            cwd_affinity(Path::new("/Users/alice/other"), cwd)
                > cwd_affinity(Path::new("/Users/bob"), cwd)
        );
        // Unrelated still gets shared `/` component count (1).
        assert!(cwd_affinity(Path::new("/etc"), cwd) >= 1);
    }

    #[test]
    fn empty_picker_does_not_panic_on_navigation() {
        let mut s = state(vec![]);
        s.move_up();
        s.move_down();
        s.request_open();
        assert!(s.take_open_request().is_none());
        assert!(s.is_empty());
    }

    struct CountingSource {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl SessionMetadataSource for CountingSource {
        fn fetch(&self, _session: &SessionFile) -> PickerRowData {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            PickerRowData {
                first_prompt: Some("loaded".into()),
                ..Default::default()
            }
        }
    }

    #[test]
    fn preview_loaded_once_per_row() {
        use std::sync::atomic::Ordering;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = CountingSource {
            calls: calls.clone(),
        };
        let mut picker = PickerState::new(
            vec![
                session("a", "/a", 100, 0),
                session("b", "/b", 200, 0),
                session("c", "/c", 300, 0),
            ],
            Box::new(source),
            None,
        );
        // new() loaded row 0 (freshest, so "c"). Two more move_down's
        // visit "b" and "a" — three unique rows total.
        picker.move_down();
        picker.move_down();
        // Revisiting must not re-fetch: cursor bounces across already-
        // loaded rows and the counter must stay pinned at 3.
        picker.move_up();
        picker.move_up();
        picker.move_down();

        let Some(&idx) = picker.filtered.get(picker.cursor) else {
            panic!()
        };
        assert!(picker.rows[idx].meta_loaded);
        assert_eq!(
            picker.rows[idx].meta.first_prompt.as_deref(),
            Some("loaded")
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "each row should be fetched exactly once, regardless of revisits"
        );
    }
}
