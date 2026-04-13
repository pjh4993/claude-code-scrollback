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

use fs2::FileExt;
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

/// Atomically update one session's marks in `path` under an exclusive
/// advisory lock. Callers provide an `update` closure that mutates the
/// slot for `session_id`; the function takes care of load, merge, save,
/// and lock release. Without the lock, two concurrent viewers could
/// both load the same snapshot, each apply their own session's update,
/// and the later writer would drop the earlier writer's session entry.
///
/// A sibling `.lock` file is used as the lock target so the lock
/// release cannot race against the `tmp → real` rename that [`save`]
/// performs: the lock file has a stable identity across rewrites.
///
/// Setting `marks` to `None` removes that session's entry — used when
/// every mark has been cleared.
pub fn update_session<F>(path: &Path, session_id: &str, update: F) -> io::Result<()>
where
    F: FnOnce(Option<&SessionMarks>) -> Option<SessionMarks>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("json.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    let mut file = load(path);
    let next = update(file.sessions.get(session_id));
    match next {
        Some(marks) if marks.is_empty() => {
            file.sessions.remove(session_id);
        }
        Some(marks) => {
            file.sessions.insert(session_id.to_string(), marks);
        }
        None => {
            file.sessions.remove(session_id);
        }
    }
    let result = save(path, &file);

    // `unlock` is best-effort; the lock also drops when the file handle
    // is closed on scope exit, so an error here is non-fatal.
    let _ = FileExt::unlock(&lock_file);
    result
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
    fn update_session_preserves_other_sessions() {
        // Two sessions' updates must not clobber each other even when
        // callers only touch their own slot — the locked merge step in
        // update_session is what makes that safe.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("marks.json");

        update_session(&path, "sess-a", |_| {
            let mut m = SessionMarks::new();
            m.insert(
                'a',
                Mark {
                    msg_index: 1,
                    block_index: None,
                },
            );
            Some(m)
        })
        .unwrap();
        update_session(&path, "sess-b", |_| {
            let mut m = SessionMarks::new();
            m.insert(
                'b',
                Mark {
                    msg_index: 2,
                    block_index: None,
                },
            );
            Some(m)
        })
        .unwrap();

        let loaded = load(&path);
        assert!(loaded.sessions.get("sess-a").unwrap().contains_key(&'a'));
        assert!(loaded.sessions.get("sess-b").unwrap().contains_key(&'b'));
    }

    #[test]
    fn update_session_empty_marks_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("marks.json");
        update_session(&path, "sess", |_| {
            let mut m = SessionMarks::new();
            m.insert(
                'a',
                Mark {
                    msg_index: 0,
                    block_index: None,
                },
            );
            Some(m)
        })
        .unwrap();
        assert!(load(&path).sessions.contains_key("sess"));

        update_session(&path, "sess", |_| Some(SessionMarks::new())).unwrap();
        assert!(!load(&path).sessions.contains_key("sess"));
    }

    #[test]
    fn update_session_concurrent_writers_do_not_clobber() {
        // Two threads hammering update_session on disjoint session ids
        // must each end up with their mark preserved. Without the
        // advisory lock, a read/modify/write race drops one of them.
        use std::sync::Arc;
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let path = Arc::new(tmp.path().join("marks.json"));

        let mut handles = Vec::new();
        for i in 0..8 {
            let path = Arc::clone(&path);
            handles.push(thread::spawn(move || {
                let sess = format!("sess-{i}");
                for j in 0..5 {
                    update_session(&path, &sess, |_| {
                        let mut m = SessionMarks::new();
                        m.insert(
                            (b'a' + j as u8) as char,
                            Mark {
                                msg_index: j,
                                block_index: None,
                            },
                        );
                        Some(m)
                    })
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let loaded = load(&path);
        for i in 0..8 {
            let sess = format!("sess-{i}");
            let marks = loaded
                .sessions
                .get(&sess)
                .unwrap_or_else(|| panic!("{sess} dropped by concurrent writer"));
            // Each writer ends on letter `e` at msg_index 4.
            assert_eq!(
                marks.get(&'e'),
                Some(&Mark {
                    msg_index: 4,
                    block_index: None,
                }),
            );
        }
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
