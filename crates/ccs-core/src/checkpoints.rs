//! Manual marks and auto-checkpoint detection.
//!
//! Manual marks (`m<letter>` / `'<letter>`) are persisted to
//! `~/.claude/claude-code-scrollback/marks.json` keyed by session id.
//! Auto-checkpoints are derived at load time from user-turn boundaries and
//! compaction markers in the JSONL stream.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A saved position inside a transcript.
///
/// Stored as a `(msg_index, block_index)` anchor rather than a raw line
/// index because the viewer's line cache reflows on collapse and width
/// changes — line indices are not stable across relayouts, but the
/// message/block pair always resolves back to a real position.
///
/// `block_index == None` points at the header line of a message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mark {
    pub msg_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_index: Option<usize>,
}

/// Per-session map of mark letter → anchor. `BTreeMap` for stable on-disk
/// ordering (and therefore human-readable diffs of `marks.json`).
pub type SessionMarks = BTreeMap<char, Mark>;

/// On-disk shape of `marks.json`: sessions keyed by session id, each a
/// map of letter → [`Mark`]. JSON for human readability and hand-editing.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MarksFile {
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionMarks>,
}

/// Return `~/.claude/claude-code-scrollback/marks.json`, or `None` if the
/// home directory cannot be determined.
pub fn marks_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".claude")
            .join("claude-code-scrollback")
            .join("marks.json")
    })
}

/// Load the marks file at `path`. A missing file yields an empty
/// [`MarksFile`] rather than an error — a fresh install has no marks yet.
/// A malformed file is logged and also yields empty marks so a corrupted
/// file never blocks the viewer from opening.
pub fn load(path: &Path) -> MarksFile {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return MarksFile::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read marks.json");
            return MarksFile::default();
        }
    };
    match serde_json::from_slice::<MarksFile>(&bytes) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "malformed marks.json; ignoring");
            MarksFile::default()
        }
    }
}

/// Atomically write `file` to `path`, creating parent directories as
/// needed. Writes to `<path>.tmp` then renames, so a crash mid-write
/// never leaves a half-truncated marks.json on disk.
pub fn save(path: &Path, file: &MarksFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_yields_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("no-such.json");
        let file = load(&path);
        assert!(file.sessions.is_empty());
    }

    #[test]
    fn load_malformed_file_yields_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("marks.json");
        fs::write(&path, b"not json at all").unwrap();
        let file = load(&path);
        assert!(file.sessions.is_empty());
    }

    #[test]
    fn save_creates_parent_dirs_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("dir").join("marks.json");

        let mut file = MarksFile::default();
        let mut marks = SessionMarks::new();
        marks.insert(
            'a',
            Mark {
                msg_index: 3,
                block_index: Some(1),
            },
        );
        marks.insert(
            'b',
            Mark {
                msg_index: 7,
                block_index: None,
            },
        );
        file.sessions.insert("sess-1".to_string(), marks);

        save(&path, &file).unwrap();
        assert!(path.exists());

        let loaded = load(&path);
        let sess = loaded.sessions.get("sess-1").unwrap();
        assert_eq!(
            sess.get(&'a'),
            Some(&Mark {
                msg_index: 3,
                block_index: Some(1),
            }),
        );
        assert_eq!(
            sess.get(&'b'),
            Some(&Mark {
                msg_index: 7,
                block_index: None,
            }),
        );
    }

    #[test]
    fn save_is_atomic_across_rewrites() {
        // Writing to the same path twice must leave a single valid file,
        // not a lingering `.tmp` sibling.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("marks.json");

        let mut f1 = MarksFile::default();
        f1.sessions.insert("s".into(), SessionMarks::new());
        save(&path, &f1).unwrap();

        let mut f2 = MarksFile::default();
        let mut marks = SessionMarks::new();
        marks.insert(
            'z',
            Mark {
                msg_index: 0,
                block_index: None,
            },
        );
        f2.sessions.insert("s".into(), marks);
        save(&path, &f2).unwrap();

        let loaded = load(&path);
        assert!(loaded.sessions.get("s").and_then(|m| m.get(&'z')).is_some());
        assert!(!path.with_extension("json.tmp").exists());
    }
}
