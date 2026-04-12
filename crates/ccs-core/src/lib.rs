//! Core primitives for claude-code-scrollback.
//!
//! This crate is I/O-light and has no TUI dependencies. It owns:
//! - [`jsonl`]  — streaming parser + Event model for Claude Code session files
//! - [`session`] — discovery of `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - [`checkpoints`] — manual marks + auto-checkpoint detection and persistence

pub mod checkpoints;
pub mod jsonl;
pub mod session;
