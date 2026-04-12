//! Session discovery under `~/.claude/projects/`.
//!
//! Each subdirectory corresponds to an encoded CWD; each `*.jsonl` file inside
//! is one Claude Code session. This module will own path decoding, metadata
//! enumeration, and the "active session" heuristic used by live-tail.

use std::path::PathBuf;

pub fn projects_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}
