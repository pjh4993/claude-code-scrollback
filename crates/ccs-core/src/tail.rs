//! Streaming tail reader for a session JSONL file.
//!
//! Designed for live-tail (PJH-51) and bulk ingest from the same code path.
//! Responsibilities:
//!
//! 1. **Partial-line safety.** A read that ends mid-line must not produce a
//!    truncated event; the unfinished bytes are buffered until the trailing
//!    newline arrives on a later read.
//! 2. **Truncation / rewrite recovery.** Claude Code rewrites JSONL files on
//!    compaction. The reader detects this (current file length < last read
//!    offset) and re-reads from byte 0, discarding any in-flight partial
//!    buffer, so compaction cannot desync the view.
//!
//! The reader itself is synchronous and poll-driven — call [`TailReader::poll`]
//! from an event loop or on a notify signal. It does not spawn threads.

use crate::jsonl::{self, Event};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// An event emitted by [`TailReader::poll`], paired with its raw JSONL line.
#[derive(Debug, Clone)]
pub struct TailEvent {
    pub event: Event,
    pub raw: String,
}

/// Outcome of a single poll tick.
#[derive(Debug, Default)]
pub struct PollResult {
    /// Events parsed from any new complete lines.
    pub events: Vec<TailEvent>,
    /// Lines that failed to decode, paired with the raw text. Parse errors do
    /// not abort the tail — callers decide whether to surface or log them.
    pub errors: Vec<(String, anyhow::Error)>,
    /// True if this poll observed a file truncation or rewrite and re-read
    /// from byte 0. Useful for viewers that need to reset scroll state.
    pub reset: bool,
}

/// Incremental reader over a JSONL file.
pub struct TailReader {
    path: PathBuf,
    offset: u64,
    /// Bytes read past the last `\n`, buffered until the line completes.
    pending: String,
}

impl TailReader {
    /// Open a tail reader at offset 0. The first [`poll`](Self::poll) call
    /// will return every existing line in the file.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            pending: String::new(),
        }
    }

    /// Read any new data since the last poll and return parsed events.
    ///
    /// If the file is shorter than our last offset (Claude Code rewrote it on
    /// compaction) the reader resets to byte 0, drops the partial-line
    /// buffer, and re-reads from the top. The returned [`PollResult::reset`]
    /// flag signals this to the caller.
    #[tracing::instrument(level = "trace", skip(self), fields(path = %self.path.display(), offset = self.offset))]
    pub fn poll(&mut self) -> anyhow::Result<PollResult> {
        let mut result = PollResult::default();
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(result),
            Err(e) => return Err(e.into()),
        };
        let len = file.metadata()?.len();

        if len < self.offset {
            // Truncation or rewrite — discard state and re-read from the top.
            self.offset = 0;
            self.pending.clear();
            result.reset = true;
        }
        if len == self.offset {
            return Ok(result);
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = String::new();
        let read_bytes = file.read_to_string(&mut buf)? as u64;
        self.offset += read_bytes;

        self.pending.push_str(&buf);
        self.drain_lines(&mut result);
        Ok(result)
    }

    fn drain_lines(&mut self, result: &mut PollResult) {
        while let Some(idx) = self.pending.find('\n') {
            let mut line: String = self.pending.drain(..=idx).collect();
            // Strip the trailing `\n` (and `\r` for CRLF files).
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            if line.trim().is_empty() {
                continue;
            }
            match jsonl::parse_line(&line) {
                Ok(event) => result.events.push(TailEvent { event, raw: line }),
                Err(e) => result.errors.push((line, e)),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn write(path: &Path, s: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    fn append(path: &Path, s: &str) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    const U1: &str = r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"t","message":{"role":"user","content":"one"}}"#;
    const U2: &str = r#"{"type":"user","uuid":"u2","sessionId":"s","timestamp":"t","message":{"role":"user","content":"two"}}"#;
    const U3: &str = r#"{"type":"user","uuid":"u3","sessionId":"s","timestamp":"t","message":{"role":"user","content":"three"}}"#;

    #[test]
    fn reads_existing_lines_on_first_poll() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write(tmp.path(), &format!("{U1}\n{U2}\n"));
        let mut r = TailReader::open(tmp.path());
        let out = r.poll().unwrap();
        assert_eq!(out.events.len(), 2);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn appended_lines_picked_up_on_next_poll() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write(tmp.path(), &format!("{U1}\n"));
        let mut r = TailReader::open(tmp.path());
        assert_eq!(r.poll().unwrap().events.len(), 1);
        append(tmp.path(), &format!("{U2}\n{U3}\n"));
        let out = r.poll().unwrap();
        assert_eq!(out.events.len(), 2);
    }

    #[test]
    fn partial_trailing_line_is_buffered_until_newline() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Write a full line then half of the next — no trailing newline.
        let half = &U2[..U2.len() / 2];
        write(tmp.path(), &format!("{U1}\n{half}"));
        let mut r = TailReader::open(tmp.path());
        let out = r.poll().unwrap();
        assert_eq!(out.events.len(), 1, "should only see the one complete line");
        assert!(!r.pending.is_empty(), "partial line held in buffer");

        // Finish writing the half-line plus a new one.
        append(tmp.path(), &format!("{}\n{U3}\n", &U2[U2.len() / 2..]));
        let out = r.poll().unwrap();
        assert_eq!(out.events.len(), 2);
        assert!(
            out.errors.is_empty(),
            "buffered half must reassemble cleanly"
        );
    }

    #[test]
    fn rewrite_smaller_than_offset_triggers_reset() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write(tmp.path(), &format!("{U1}\n{U2}\n{U3}\n"));
        let mut r = TailReader::open(tmp.path());
        let _ = r.poll().unwrap();

        // Compaction: rewrite with a single new line, smaller than before.
        write(tmp.path(), &format!("{U1}\n"));
        let out = r.poll().unwrap();
        assert!(out.reset, "rewrite must be flagged as reset");
        assert_eq!(out.events.len(), 1);
        assert_eq!(r.offset(), (U1.len() + 1) as u64);
    }

    #[test]
    fn malformed_line_yields_error_without_halting_stream() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write(tmp.path(), &format!("{U1}\nnot json\n{U2}\n"));
        let mut r = TailReader::open(tmp.path());
        let out = r.poll().unwrap();
        assert_eq!(out.events.len(), 2);
        assert_eq!(out.errors.len(), 1);
    }

    #[test]
    fn missing_file_poll_is_noop() {
        let mut r = TailReader::open("/tmp/definitely-not-there-xyz-999.jsonl");
        let out = r.poll().unwrap();
        assert!(out.events.is_empty());
    }
}
