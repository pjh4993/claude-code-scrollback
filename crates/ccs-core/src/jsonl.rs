//! Streaming parser for Claude Code session JSONL files.
//!
//! Source path: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//! The schema is undocumented and drifts across Claude Code versions, so the
//! parser preserves unknown fields and degrades to [`Event::Unknown`] for
//! unrecognised event types rather than failing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    UserMessage(serde_json::Value),
    AssistantMessage(serde_json::Value),
    ToolUse(serde_json::Value),
    ToolResult(serde_json::Value),
    Thinking(serde_json::Value),
    System(serde_json::Value),
    #[serde(other)]
    Unknown,
}

pub fn parse_line(line: &str) -> anyhow::Result<Event> {
    Ok(serde_json::from_str(line)?)
}
