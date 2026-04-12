//! TUI layer for claude-code-scrollback.
//!
//! Owns the ratatui event loop, screen state machine, live-tail watcher, and
//! (eventually) the SQLite metadata cache. Depends on [`ccs_core`] for the
//! data model and file discovery.

pub mod app;
pub mod clipboard;
pub mod tail;
pub mod ui;

pub use app::{App, Screen};
pub use ratatui::{init, restore};
