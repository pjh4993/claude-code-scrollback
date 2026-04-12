//! Live-tail watcher for active Claude Code sessions.
//!
//! Combines `notify` filesystem events with a truncation-aware line reader so
//! the viewer can follow the active session's JSONL as Claude Code writes to
//! it — including the case where the file is rewritten on compaction.
