//! Picker row metadata and pluggable metadata sources.
//!
//! The session picker needs more than what `fs::metadata` gives us — it wants
//! a first-prompt preview, message count, and duration. Computing any of
//! those requires reading the JSONL file itself, which is cheap per file but
//! catastrophic at picker cold-launch time on a corpus with thousands of
//! sessions.
//!
//! To keep the picker launch instant *and* leave a seam for the future
//! SQLite cache (PJH-54), we hide the data source behind a trait:
//!
//! * [`LazyFsSource`] — reads the first few kilobytes of the file, parses
//!   the first [`jsonl::Event::User`] line, and returns the text as
//!   [`PickerRowData::first_prompt`]. `message_count` and `duration` stay
//!   `None` because those require a full file read.
//! * [`NullSource`] — returns empty metadata. Useful in tests and as the
//!   default when the picker has not yet lazy-loaded a row.
//!
//! PJH-54 will add a `SqliteSource` that returns all three fields from an
//! mtime-keyed index, dropping into the same trait slot without any UI
//! changes.

use crate::jsonl::{self, ContentBlock, Event, MessageContent};
use crate::session::SessionFile;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Columns the picker wants beyond what `fs::metadata` provides. Every field
/// is optional — the picker renders missing fields as `…` so a slow or
/// unavailable source never blocks rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerRowData {
    /// First user-authored prompt, trimmed to a single line preview.
    pub first_prompt: Option<String>,
    /// Total event count (user + assistant + tool calls).
    pub message_count: Option<usize>,
    /// Wall-clock duration between first and last event.
    pub duration: Option<Duration>,
}

/// A pluggable source of picker row metadata. Implementors may read directly
/// from disk, from a cache, or synthesize for tests.
pub trait SessionMetadataSource {
    /// Look up metadata for the given session. Implementations should be
    /// cheap; the picker calls this lazily as rows become visible.
    fn fetch(&self, session: &SessionFile) -> PickerRowData;
}

/// Always returns empty metadata. The picker uses this as the default source
/// before anything has been loaded, and tests use it to short-circuit I/O.
pub struct NullSource;

impl SessionMetadataSource for NullSource {
    fn fetch(&self, _session: &SessionFile) -> PickerRowData {
        PickerRowData::default()
    }
}

/// Reads the first `HEAD_READ_BYTES` of a session JSONL and extracts the
/// first user prompt. Sufficient for the picker's preview column without
/// triggering a full-file parse per session.
///
/// Does **not** populate `message_count` or `duration` — those require
/// scanning the entire file. The PJH-54 SQLite cache will fill them.
pub struct LazyFsSource;

/// Header read budget per session. 16 KiB is enough to cover the several
/// small system/attachment preamble events plus the first user message in
/// the vast majority of sessions observed in real corpora.
pub const HEAD_READ_BYTES: usize = 16 * 1024;

/// Max characters kept in the preview string. Long prompts get truncated.
pub const PREVIEW_CHAR_LIMIT: usize = 120;

impl SessionMetadataSource for LazyFsSource {
    fn fetch(&self, session: &SessionFile) -> PickerRowData {
        PickerRowData {
            first_prompt: first_user_prompt(&session.path),
            message_count: None,
            duration: None,
        }
    }
}

/// Extract the first user-authored prompt from a session JSONL, reading at
/// most [`HEAD_READ_BYTES`] from disk. Returns `None` if the file cannot be
/// opened, contains no user event in its header, or the first user event has
/// no renderable text content.
pub fn first_user_prompt(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; HEAD_READ_BYTES];
    let n = File::open(path).ok()?.read(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf[..n]).ok()?;

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = jsonl::parse_line(line) else {
            continue;
        };
        if let Event::User(msg) = event {
            if let Some(preview) = preview_from_content(&msg.message.content) {
                return Some(truncate(&preview, PREVIEW_CHAR_LIMIT));
            }
        }
    }
    None
}

fn preview_from_content(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(s) => Some(single_line(s)),
        MessageContent::Blocks(blocks) => {
            // A `user` event with array-content is almost always a
            // tool-result echo from the model's previous turn, not a
            // human-authored prompt. Skip those and keep looking.
            for block in blocks {
                if let ContentBlock::Text { text, .. } = block {
                    return Some(single_line(text));
                }
            }
            None
        }
    }
}

fn single_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(limit).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionFile, SessionKind};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_session(path: PathBuf) -> SessionFile {
        SessionFile {
            session_id: "s".into(),
            parent_session_id: None,
            kind: SessionKind::Primary,
            project_cwd: PathBuf::from("/tmp"),
            modified: SystemTime::UNIX_EPOCH,
            size: 0,
            path,
        }
    }

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        p
    }

    #[test]
    fn null_source_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "a.jsonl", "");
        let data = NullSource.fetch(&make_session(p));
        assert_eq!(data, PickerRowData::default());
    }

    #[test]
    fn lazy_fs_source_extracts_first_user_prompt_from_string_content() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "a.jsonl",
            r#"{"type":"system","uuid":"s1","sessionId":"x","timestamp":"t"}
{"type":"user","uuid":"u1","sessionId":"x","timestamp":"t","message":{"role":"user","content":"hello world"}}
{"type":"user","uuid":"u2","sessionId":"x","timestamp":"t","message":{"role":"user","content":"second prompt"}}
"#,
        );
        let data = LazyFsSource.fetch(&make_session(p));
        assert_eq!(data.first_prompt.as_deref(), Some("hello world"));
        assert!(data.message_count.is_none());
        assert!(data.duration.is_none());
    }

    #[test]
    fn lazy_fs_source_skips_tool_result_users_finds_first_text_user() {
        // A `user` event with array-content carrying a tool_result should NOT
        // be treated as the first prompt — that's a tool echo, not a human
        // typing. The parser needs to keep scanning for a real text event.
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "a.jsonl",
            r#"{"type":"user","uuid":"u1","sessionId":"x","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]}}
{"type":"user","uuid":"u2","sessionId":"x","timestamp":"t","message":{"role":"user","content":"the real prompt"}}
"#,
        );
        let data = LazyFsSource.fetch(&make_session(p));
        assert_eq!(data.first_prompt.as_deref(), Some("the real prompt"));
    }

    #[test]
    fn lazy_fs_source_truncates_long_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let long: String = "x".repeat(500);
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"x","timestamp":"t","message":{{"role":"user","content":"{long}"}}}}"#
        );
        let p = write_file(tmp.path(), "a.jsonl", &format!("{line}\n"));
        let data = LazyFsSource.fetch(&make_session(p));
        let preview = data.first_prompt.unwrap();
        assert!(
            preview.chars().count() == PREVIEW_CHAR_LIMIT + 1,
            "{preview}"
        );
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn lazy_fs_source_collapses_multiline_prompt_to_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "a.jsonl",
            r#"{"type":"user","uuid":"u1","sessionId":"x","timestamp":"t","message":{"role":"user","content":"line one\nline two"}}
"#,
        );
        let data = LazyFsSource.fetch(&make_session(p));
        assert_eq!(data.first_prompt.as_deref(), Some("line one"));
    }

    #[test]
    fn lazy_fs_source_returns_none_when_no_user_event_in_header() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "a.jsonl",
            r#"{"type":"system","uuid":"s1","sessionId":"x","timestamp":"t"}
{"type":"permission-mode","permissionMode":"default","sessionId":"x"}
"#,
        );
        let data = LazyFsSource.fetch(&make_session(p));
        assert!(data.first_prompt.is_none());
    }

    #[test]
    fn lazy_fs_source_missing_file_returns_empty() {
        let data = LazyFsSource.fetch(&make_session(PathBuf::from("/nonexistent/path/nope.jsonl")));
        assert_eq!(data, PickerRowData::default());
    }

    #[test]
    fn lazy_fs_source_skips_malformed_lines_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "a.jsonl",
            r#"not valid json
{"type":"user","uuid":"u1","sessionId":"x","timestamp":"t","message":{"role":"user","content":"survived"}}
"#,
        );
        let data = LazyFsSource.fetch(&make_session(p));
        assert_eq!(data.first_prompt.as_deref(), Some("survived"));
    }
}
