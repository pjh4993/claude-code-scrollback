//! Session discovery under `~/.claude/projects/`.
//!
//! Each subdirectory corresponds to an encoded CWD; each `*.jsonl` file inside
//! is one Claude Code session. This module owns path decoding, enumeration of
//! sessions (with metadata), and the "active session" heuristic used by
//! live-tail.
//!
//! # Project directory encoding
//!
//! Claude Code encodes a session's CWD into the project directory name by
//! replacing `/` with `-`. For example, `/Users/alice/src/app` becomes
//! `-Users-alice-src-app`. The encoding is **lossy** — a CWD containing a
//! literal `-` character is indistinguishable from a path separator — so
//! [`decode_project_dir`] returns a best-effort reconstruction, not a
//! round-trip guarantee.
//!
//! To recover the real CWD despite the lossy encoding, [`discover`] peeks at
//! the first few lines of one JSONL file per project dir and pulls the
//! top-level `cwd` field that Claude Code writes on every `user`/`assistant`
//! event. [`decode_project_dir`] is the fallback when no session in the dir
//! carries a usable `cwd`.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// Maximum lines scanned per JSONL when looking for a `cwd` field. The
/// authoritative `cwd` is on every `user`/`assistant` event, and Claude Code
/// usually emits one within the first few lines (typically after a
/// `permission-mode` and `file-history-snapshot` preamble). 32 is comfortably
/// above that without turning discovery into a full-file scan.
const CWD_PROBE_MAX_LINES: usize = 32;

/// Kind of session file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// Top-level session JSONL directly under the project directory.
    Primary,
    /// Subagent/sidechain JSONL under `<session-id>/subagents/`. The parent
    /// session id is preserved in [`SessionFile::parent_session_id`].
    Subagent,
}

/// Metadata about a single session JSONL file.
#[derive(Debug, Clone)]
pub struct SessionFile {
    /// Absolute path to the `.jsonl` file.
    pub path: PathBuf,
    /// Session id for primary sessions; agent id for subagent files.
    pub session_id: String,
    /// Parent session id when [`kind`](Self::kind) is [`SessionKind::Subagent`].
    pub parent_session_id: Option<String>,
    /// Primary vs. subagent side-channel.
    pub kind: SessionKind,
    /// Decoded CWD of the project this session belongs to. Best-effort —
    /// see the module docs for the lossy-encoding caveat.
    pub project_cwd: PathBuf,
    /// Last-modified time from the filesystem.
    pub modified: SystemTime,
    /// File size in bytes.
    pub size: u64,
}

/// Return `~/.claude/projects`, or `None` if the home directory cannot be
/// determined.
pub fn projects_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Decode a project directory name (`-Users-alice-src-app`) back to a best-
/// effort CWD (`/Users/alice/src/app`). Returns the name unchanged if it does
/// not start with `-`.
pub fn decode_project_dir(name: &str) -> PathBuf {
    if let Some(rest) = name.strip_prefix('-') {
        PathBuf::from(format!("/{}", rest.replace('-', "/")))
    } else {
        PathBuf::from(name)
    }
}

/// Recover the authoritative CWD for a project dir by peeking at its session
/// files. Returns `None` when no JSONL in the dir carries a top-level `cwd`
/// string within the first [`CWD_PROBE_MAX_LINES`] lines — callers should
/// fall back to [`decode_project_dir`].
///
/// Lines are parsed as loose [`serde_json::Value`] rather than the typed
/// [`crate::jsonl::Event`] enum so that an unknown or broken event kind can
/// still contribute its `cwd` field. The scan stops at the first hit.
pub fn read_cwd_from_project_dir(project_path: &Path) -> Option<PathBuf> {
    let rd = fs::read_dir(project_path).ok()?;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(cwd) = read_cwd_from_jsonl(&path) {
            return Some(cwd);
        }
    }
    None
}

fn read_cwd_from_jsonl(path: &Path) -> Option<PathBuf> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(CWD_PROBE_MAX_LINES) {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
            if !cwd.is_empty() {
                return Some(PathBuf::from(cwd));
            }
        }
    }
    None
}

/// Non-fatal outcomes from a discovery pass, surfaced to the picker so
/// the user sees *why* their session list might be incomplete.
///
/// Right now the only counted outcome is project directories that
/// could not be enumerated (typically `EACCES`). The struct is kept
/// separate from `Vec<SessionFile>` so we can add more kinds of
/// skip — malformed names, unreadable metadata — without churning
/// the `discover` signature again.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryStats {
    /// Number of project directories that `read_dir` refused. A
    /// non-zero count is shown in the picker footer and bumps the
    /// per-dir log to `warn!` so `RUST_LOG=warn` surfaces which ones.
    pub skipped_dirs: usize,
}

impl DiscoveryStats {
    pub fn is_clean(&self) -> bool {
        self.skipped_dirs == 0
    }
}

/// Enumerate every session JSONL under `root`, returning the
/// [`SessionFile`]s plus a [`DiscoveryStats`] summary. Non-JSONL files
/// are silently skipped as before; project directories that cannot
/// be read are counted and logged at `warn!` so the picker can
/// display "N dirs skipped" in its footer. A missing root returns
/// an empty list rather than an error — a fresh machine with no
/// Claude Code sessions is not a failure.
#[tracing::instrument(level = "debug", skip_all, fields(root = %root.display()))]
pub fn discover(root: &Path) -> anyhow::Result<(Vec<SessionFile>, DiscoveryStats)> {
    let mut out = Vec::new();
    let mut stats = DiscoveryStats::default();
    let project_dirs = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((out, stats)),
        Err(e) => return Err(e.into()),
    };

    for project in project_dirs.flatten() {
        match collect_project_dir(&project) {
            ProjectDirResult::Sessions(batch) => out.extend(batch),
            ProjectDirResult::Skipped => {
                stats.skipped_dirs += 1;
            }
            ProjectDirResult::Skip => {}
        }
    }
    tracing::debug!(
        count = out.len(),
        skipped_dirs = stats.skipped_dirs,
        "session discovery complete"
    );
    Ok((out, stats))
}

/// Message sent from a streaming discovery thread to the UI event loop.
pub enum DiscoveryMsg {
    /// Sessions from one project directory. May be empty if the dir
    /// contained no `.jsonl` files.
    Batch(Vec<SessionFile>),
    /// Discovery is complete. Carries final stats. The receiver should
    /// drop the channel after handling this.
    Done(DiscoveryStats),
}

/// Streaming variant of [`discover`]. Walks `root` and sends one
/// [`DiscoveryMsg::Batch`] per project directory as it goes, followed
/// by a final [`DiscoveryMsg::Done`]. Designed for
/// `std::thread::spawn` — the UI thread can `try_recv` batches on
/// each event-loop tick to populate the picker incrementally.
///
/// If the receiver is dropped mid-discovery (e.g. the user quits
/// before scanning finishes), sends will fail silently and the thread
/// exits.
#[tracing::instrument(level = "debug", skip_all, fields(root = %root.display()))]
pub fn discover_streaming(root: &Path, tx: mpsc::Sender<DiscoveryMsg>) -> anyhow::Result<()> {
    let mut stats = DiscoveryStats::default();
    let mut total = 0usize;
    let project_dirs = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let _ = tx.send(DiscoveryMsg::Done(stats));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    for project in project_dirs.flatten() {
        match collect_project_dir(&project) {
            ProjectDirResult::Sessions(batch) => {
                total += batch.len();
                // Silently stop if the receiver hung up.
                if tx.send(DiscoveryMsg::Batch(batch)).is_err() {
                    return Ok(());
                }
            }
            ProjectDirResult::Skipped => {
                stats.skipped_dirs += 1;
            }
            ProjectDirResult::Skip => {}
        }
    }
    tracing::debug!(
        count = total,
        skipped_dirs = stats.skipped_dirs,
        "streaming session discovery complete"
    );
    let _ = tx.send(DiscoveryMsg::Done(stats));
    Ok(())
}

/// Outcome of processing a single top-level project directory.
enum ProjectDirResult {
    /// Successfully collected sessions (may be empty if dir has no JSONL).
    Sessions(Vec<SessionFile>),
    /// Directory could not be read (permission-denied, etc.) — caller
    /// increments `stats.skipped_dirs`.
    Skipped,
    /// Entry was not a directory or had an unparseable name — skip silently.
    Skip,
}

/// Enumerate one project directory and collect its session files.
fn collect_project_dir(project: &fs::DirEntry) -> ProjectDirResult {
    let project_path = project.path();
    if !project_path.is_dir() {
        return ProjectDirResult::Skip;
    }
    let name = match project.file_name().into_string() {
        Ok(n) => n,
        Err(_) => return ProjectDirResult::Skip,
    };
    let project_cwd =
        read_cwd_from_project_dir(&project_path).unwrap_or_else(|| decode_project_dir(&name));

    let rd = match fs::read_dir(&project_path) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(path=?project_path, error=%e, "skipping unreadable project dir");
            return ProjectDirResult::Skipped;
        }
    };

    let mut out = Vec::new();
    for session in rd.flatten() {
        let path = session.path();
        if path.is_dir() {
            if let Some(parent_id) = path.file_name().and_then(|s| s.to_str()) {
                collect_subagents(&path.join("subagents"), parent_id, &project_cwd, &mut out);
            }
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = session.metadata() else {
            continue;
        };
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(SessionFile {
            session_id: stem.to_string(),
            parent_session_id: None,
            kind: SessionKind::Primary,
            path,
            project_cwd: project_cwd.clone(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        });
    }
    ProjectDirResult::Sessions(out)
}

fn collect_subagents(
    dir: &Path,
    parent_session_id: &str,
    project_cwd: &Path,
    out: &mut Vec<SessionFile>,
) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(SessionFile {
            session_id: stem.to_string(),
            parent_session_id: Some(parent_session_id.to_string()),
            kind: SessionKind::Subagent,
            path,
            project_cwd: project_cwd.to_path_buf(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        });
    }
}

/// Return sessions whose `project_cwd` overlaps with `cwd` — either path may
/// be an ancestor of the other, or they may be equal. The bidirectional
/// prefix match is deliberate: a picker launched from a sub-directory of a
/// tracked project should still surface that project's sessions, and a
/// picker launched from an ancestor directory should surface every nested
/// project's sessions.
///
/// Because [`decode_project_dir`] is lossy, this is a prefix match on the
/// decoded path rather than a strict equality check.
pub fn filter_by_cwd(sessions: &[SessionFile], cwd: &Path) -> Vec<SessionFile> {
    sessions
        .iter()
        .filter(|s| cwd.starts_with(&s.project_cwd) || s.project_cwd.starts_with(cwd))
        .cloned()
        .collect()
}

/// Pick the most recently modified session in the slice, if any. Used as a
/// proxy for "the active session" when live-tail is invoked without an
/// explicit session id.
pub fn most_recent(sessions: &[SessionFile]) -> Option<&SessionFile> {
    sessions.iter().max_by_key(|s| s.modified)
}

/// Default freshness window for [`active_session`]: a session whose JSONL
/// has not been touched for longer than this is assumed dormant, not
/// actively being written. Five minutes is long enough to survive a
/// multi-turn assistant response without a heartbeat and short enough
/// that an abandoned session doesn't look live the next morning.
pub const DEFAULT_ACTIVE_WITHIN: Duration = Duration::from_secs(5 * 60);

/// Find the "active" session rooted at `cwd`: the most-recently-modified
/// session under the current project whose mtime is within `within` of
/// now. Returns `Ok(None)` when no session matches (project has never
/// been used, or the last session is stale).
///
/// This is the `--live` entry point and also used by live-tail auto-
/// detect when opening a session that happens to still be being written.
pub fn active_session(cwd: &Path, within: Duration) -> anyhow::Result<Option<SessionFile>> {
    let Some(root) = projects_root() else {
        return Ok(None);
    };
    let (all, _stats) = discover(&root)?;
    let candidates = filter_by_cwd(&all, cwd);
    let Some(newest) = most_recent(&candidates) else {
        return Ok(None);
    };
    let now = SystemTime::now();
    match now.duration_since(newest.modified) {
        Ok(age) if age <= within => Ok(Some(newest.clone())),
        // File mtime in the future (clock skew, NFS) — accept it.
        Err(_) => Ok(Some(newest.clone())),
        Ok(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn decode_project_dir_basic() {
        assert_eq!(
            decode_project_dir("-Users-alice-src-app"),
            PathBuf::from("/Users/alice/src/app")
        );
        assert_eq!(
            decode_project_dir("not-encoded"),
            PathBuf::from("not-encoded")
        );
    }

    #[test]
    fn discover_enumerates_jsonl_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let proj = root.join("-tmp-project-a");
        fs::create_dir_all(&proj).unwrap();
        let mut f = File::create(proj.join("abc.jsonl")).unwrap();
        writeln!(f, "{{\"type\":\"user\"}}").unwrap();
        File::create(proj.join("ignore.txt")).unwrap();
        fs::create_dir_all(proj.join("nested")).unwrap();

        let (sessions, stats) = discover(root).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "abc");
        // No jsonl line carries `cwd`, so we fall back to the lossy decode.
        assert_eq!(sessions[0].project_cwd, PathBuf::from("/tmp/project/a"));
        assert!(sessions[0].size > 0);
        assert!(stats.is_clean());
    }

    #[test]
    fn discover_prefers_jsonl_cwd_over_lossy_decode() {
        // Project dir name `-Users-alice-claude-code-scrollback` would lossy-
        // decode to `/Users/alice/claude/code/scrollback`. A real session
        // line's `cwd` field is authoritative and should win.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let proj = root.join("-Users-alice-claude-code-scrollback");
        fs::create_dir_all(&proj).unwrap();
        let mut f = File::create(proj.join("sess.jsonl")).unwrap();
        writeln!(
            f,
            r#"{{"type":"permission-mode","permissionMode":"default"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u","sessionId":"s","timestamp":"t","cwd":"/Users/alice/claude-code-scrollback","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();

        let (sessions, _stats) = discover(root).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].project_cwd,
            PathBuf::from("/Users/alice/claude-code-scrollback"),
        );
    }

    #[test]
    fn read_cwd_probes_multiple_lines_before_giving_up() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path();
        let mut f = File::create(proj.join("a.jsonl")).unwrap();
        // Preamble without `cwd` — parser must scan past it.
        for _ in 0..5 {
            writeln!(f, r#"{{"type":"permission-mode"}}"#).unwrap();
        }
        writeln!(
            f,
            r#"{{"type":"assistant","cwd":"/some/real-path","uuid":"u","sessionId":"s","timestamp":"t","message":{{"role":"assistant","content":[]}}}}"#
        )
        .unwrap();
        let got = read_cwd_from_project_dir(proj);
        assert_eq!(got, Some(PathBuf::from("/some/real-path")));
    }

    #[test]
    fn discover_missing_root_returns_empty() {
        let (sessions, stats) = discover(Path::new("/definitely/does/not/exist/xyz123")).unwrap();
        assert!(sessions.is_empty());
        assert!(stats.is_clean());
    }

    #[test]
    fn most_recent_picks_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let proj = root.join("-p");
        fs::create_dir_all(&proj).unwrap();
        File::create(proj.join("old.jsonl")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        File::create(proj.join("new.jsonl")).unwrap();
        let (sessions, _stats) = discover(root).unwrap();
        let newest = most_recent(&sessions).unwrap();
        assert_eq!(newest.session_id, "new");
    }

    #[test]
    fn discover_enumerates_subagents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let proj = root.join("-p");
        let sess_dir = proj.join("aaaa-bbbb");
        let agents = sess_dir.join("subagents");
        fs::create_dir_all(&agents).unwrap();
        File::create(proj.join("aaaa-bbbb.jsonl")).unwrap();
        File::create(agents.join("agent-1.jsonl")).unwrap();
        File::create(agents.join("agent-2.jsonl")).unwrap();

        let (sessions, _stats) = discover(root).unwrap();
        let primary: Vec<_> = sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Primary)
            .collect();
        let sub: Vec<_> = sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Subagent)
            .collect();
        assert_eq!(primary.len(), 1);
        assert_eq!(sub.len(), 2);
        assert!(sub
            .iter()
            .all(|s| s.parent_session_id.as_deref() == Some("aaaa-bbbb")));
    }

    #[cfg(unix)]
    #[test]
    fn discover_counts_unreadable_project_dir_as_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // A readable project dir with one session — baseline.
        let readable = root.join("-readable");
        fs::create_dir_all(&readable).unwrap();
        File::create(readable.join("s1.jsonl")).unwrap();

        // An unreadable project dir — `read_dir` should fail with
        // EACCES, and `discover` should count it as skipped rather
        // than crashing or silently losing it.
        let locked = root.join("-locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let result = discover(root);

        // Restore permissions so tempfile can clean up, no matter
        // what the assertion below does.
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        let (sessions, stats) = result.unwrap();
        assert_eq!(sessions.len(), 1, "readable session should still surface");
        assert_eq!(stats.skipped_dirs, 1, "locked dir must be counted");
        assert!(!stats.is_clean());
    }

    #[test]
    fn discovery_stats_default_is_clean() {
        assert!(DiscoveryStats::default().is_clean());
        assert!(!DiscoveryStats { skipped_dirs: 1 }.is_clean());
    }

    #[test]
    fn filter_by_cwd_matches_parent_and_child_paths() {
        let s = SessionFile {
            path: PathBuf::new(),
            session_id: "s".into(),
            parent_session_id: None,
            kind: SessionKind::Primary,
            project_cwd: PathBuf::from("/Users/alice/repo"),
            modified: SystemTime::UNIX_EPOCH,
            size: 0,
        };
        let sessions = vec![s];
        assert_eq!(
            filter_by_cwd(&sessions, Path::new("/Users/alice/repo/sub")).len(),
            1
        );
        assert_eq!(filter_by_cwd(&sessions, Path::new("/Users/alice")).len(), 1);
        assert_eq!(filter_by_cwd(&sessions, Path::new("/etc")).len(), 0);
    }
}
