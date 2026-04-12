//! Manual marks and auto-checkpoint detection.
//!
//! Manual marks (`m<letter>` / `'<letter>`) are persisted to
//! `~/.claude/claude-code-scrollback/marks.json` keyed by session id.
//! Auto-checkpoints are derived at load time from user-turn boundaries and
//! compaction markers in the JSONL stream.
