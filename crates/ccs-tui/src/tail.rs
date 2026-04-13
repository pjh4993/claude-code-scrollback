//! Live-tail driver for the transcript viewer.
//!
//! Wraps [`ccs_core::tail::TailReader`] with the book-keeping the TUI
//! needs: tracking what path is being tailed, translating the reader's
//! `PollResult` into a viewer-facing [`Update`], and logging malformed
//! lines at `warn` instead of surfacing them per-line.
//!
//! The driver is synchronous and poll-driven. The event loop in
//! `app.rs` calls [`LiveTail::poll`] once per tick; the `TailReader`
//! underneath handles partial lines and compaction rewrites.

use std::path::{Path, PathBuf};

use ccs_core::jsonl::Event;
use ccs_core::tail::TailReader;

/// What happened during one [`LiveTail::poll`] tick.
#[derive(Debug, Default)]
pub struct Update {
    /// New events parsed from bytes appended since the last poll.
    pub new_events: Vec<Event>,
    /// True if the underlying file was rewritten (compaction); the
    /// caller must reset its transcript before feeding `new_events`.
    pub reset: bool,
    /// Number of malformed JSONL lines skipped during this tick. Also
    /// logged at `warn`, but exposed here so the viewer can surface a
    /// one-line flash when schema drift starts dropping events.
    pub errors_skipped: usize,
}

impl Update {
    pub fn is_empty(&self) -> bool {
        self.new_events.is_empty() && !self.reset && self.errors_skipped == 0
    }
}

/// Live-tail driver bound to a single session file.
pub struct LiveTail {
    reader: TailReader,
}

impl LiveTail {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            reader: TailReader::open(path),
        }
    }

    /// Open a tail driver already advanced past `offset` bytes — used
    /// after [`ccs_core::transcript::load_from_path_with_offset`] so
    /// the initial snapshot and the tail reader see the file as a
    /// single consistent view.
    pub fn new_at(path: impl Into<PathBuf>, offset: u64) -> Self {
        Self {
            reader: TailReader::open_at(path, offset),
        }
    }

    pub fn path(&self) -> &Path {
        self.reader.path()
    }

    /// Read whatever is new in the file and return parsed events.
    /// Malformed lines are logged and counted in `errors_skipped` but
    /// don't abort the stream — the viewer should keep running through
    /// schema drift.
    pub fn poll(&mut self) -> anyhow::Result<Update> {
        let poll = self.reader.poll()?;
        let errors_skipped = poll.errors.len();
        for (line, err) in &poll.errors {
            tracing::warn!(
                path = %self.reader.path().display(),
                error = %err,
                line_preview = %&line.chars().take(80).collect::<String>(),
                "skipping malformed JSONL line in live tail",
            );
        }
        Ok(Update {
            new_events: poll.events.into_iter().map(|te| te.event).collect(),
            reset: poll.reset,
            errors_skipped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    const U1: &str = r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"t","message":{"role":"user","content":"one"}}"#;
    const U2: &str = r#"{"type":"user","uuid":"u2","sessionId":"s","timestamp":"t","message":{"role":"user","content":"two"}}"#;

    #[test]
    fn poll_returns_new_events_after_append() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(tmp.path())
            .unwrap();
        writeln!(f, "{U1}").unwrap();
        drop(f);

        let mut tail = LiveTail::new(tmp.path());
        let first = tail.poll().unwrap();
        assert_eq!(first.new_events.len(), 1);
        assert!(!first.reset);

        let mut f = OpenOptions::new().append(true).open(tmp.path()).unwrap();
        writeln!(f, "{U2}").unwrap();
        drop(f);

        let second = tail.poll().unwrap();
        assert_eq!(second.new_events.len(), 1);
        assert!(!second.reset);

        let idle = tail.poll().unwrap();
        assert!(idle.is_empty());
    }

    #[test]
    fn poll_reports_reset_on_rewrite() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(tmp.path())
            .unwrap();
        writeln!(f, "{U1}").unwrap();
        writeln!(f, "{U2}").unwrap();
        drop(f);

        let mut tail = LiveTail::new(tmp.path());
        let _ = tail.poll().unwrap();

        // Compaction: rewrite with a single line (smaller than before).
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(tmp.path())
            .unwrap();
        writeln!(f, "{U1}").unwrap();
        drop(f);

        let after = tail.poll().unwrap();
        assert!(after.reset);
        assert_eq!(after.new_events.len(), 1);
    }
}
