//! Core primitives for claude-code-scrollback.
//!
//! This crate is I/O-light and has no TUI dependencies. It owns:
//! - [`jsonl`]  — streaming parser + Event model for Claude Code session files
//! - [`session`] — discovery of `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - [`tail`] — incremental reader for live-tail and bulk ingest
//! - [`metadata`] — pluggable picker row metadata (lazy FS today, SQLite later)
//! - [`checkpoints`] — manual marks + auto-checkpoint detection and persistence
//! - [`transcript`] — viewer-friendly lowering of parsed events

pub mod checkpoints;
pub mod jsonl;
pub mod metadata;
pub mod session;
pub mod tail;
pub mod transcript;
