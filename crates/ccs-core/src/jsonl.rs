//! Streaming parser for Claude Code session JSONL files.
//!
//! Source path: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
//!
//! The schema is undocumented and drifts across Claude Code versions. The
//! parser is forward-compatible by design:
//!
//! * unknown top-level `type` values decode to [`Event::Unknown`]
//! * unknown fields on typed structs are preserved in an `extra` map
//! * unknown content-block `type` values decode to [`ContentBlock::Unknown`]
//!
//! Event shapes pinned from real JSONLs observed in `~/.claude/projects`:
//! `user`, `assistant`, `system`, `attachment`, `progress`, `queue-operation`,
//! `last-prompt`, `file-history-snapshot`, `pr-link`, `permission-mode`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A single line in a Claude Code session JSONL file.
///
/// Only the events we actually render (`user`, `assistant`, `system`,
/// `attachment`) are decoded into typed structs. The rest are retained
/// opaquely as [`Value`] so the parser never drops data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    User(MessageEvent),
    Assistant(MessageEvent),
    System(SystemEvent),
    Attachment(AttachmentEvent),
    Progress(Value),
    QueueOperation(Value),
    LastPrompt(Value),
    FileHistorySnapshot(Value),
    PrLink(Value),
    PermissionMode(Value),
    CustomTitle(Value),
    AgentName(Value),
    #[serde(other)]
    Unknown,
}

/// Envelope shared by `user` and `assistant` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEvent {
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    pub session_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub message: Message,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Present on normal sessions; absent on `remote-bridge` variant where
    /// the role is implied by the parent event's `type`.
    #[serde(default)]
    pub role: Option<String>,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `user` events may carry `content` as a bare string; `assistant` events
/// always carry a typed content-block array. Both shapes decode here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    Thinking {
        #[serde(default)]
        thinking: Option<String>,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: bool,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemEvent {
    pub uuid: String,
    pub session_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentEvent {
    pub uuid: String,
    pub session_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    pub attachment: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Parse one JSONL line into an [`Event`]. Blank lines are rejected so callers
/// can distinguish them from parse failures.
pub fn parse_line(line: &str) -> anyhow::Result<Event> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty line");
    }
    Ok(serde_json::from_str(trimmed)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_user_string_content() {
        let line = r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"hello"},"uuid":"u1","timestamp":"2026-04-12T08:20:42.076Z","sessionId":"s1"}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::User(m) => {
                assert_eq!(m.uuid, "u1");
                assert_eq!(m.session_id, "s1");
                assert_eq!(m.message.role.as_deref(), Some("user"));
                assert!(matches!(m.message.content, MessageContent::Text(ref s) if s == "hello"));
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn decodes_user_tool_result_blocks() {
        let line = r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok","is_error":false}]}}"#;
        let ev = parse_line(line).unwrap();
        let Event::User(m) = ev else {
            panic!("expected User")
        };
        let MessageContent::Blocks(blocks) = m.message.content else {
            panic!("expected blocks")
        };
        assert!(matches!(
            blocks[0],
            ContentBlock::ToolResult { ref tool_use_id, .. } if tool_use_id == "tu1"
        ));
    }

    #[test]
    fn decodes_assistant_mixed_blocks() {
        let line = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#;
        let ev = parse_line(line).unwrap();
        let Event::Assistant(m) = ev else {
            panic!("expected Assistant")
        };
        let MessageContent::Blocks(blocks) = m.message.content else {
            panic!("expected blocks")
        };
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(blocks[1], ContentBlock::Text { .. }));
        assert!(matches!(blocks[2], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn unknown_top_level_type_becomes_unknown() {
        let line = r#"{"type":"future-event","foo":"bar"}"#;
        let ev = parse_line(line).unwrap();
        assert!(matches!(ev, Event::Unknown));
    }

    #[test]
    fn unknown_content_block_becomes_unknown() {
        let line = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"future_block","foo":"bar"}]}}"#;
        let ev = parse_line(line).unwrap();
        let Event::Assistant(m) = ev else {
            panic!("expected Assistant")
        };
        let MessageContent::Blocks(blocks) = m.message.content else {
            panic!("expected blocks")
        };
        assert!(matches!(blocks[0], ContentBlock::Unknown));
    }

    #[test]
    fn untyped_events_are_opaque_but_preserved() {
        let line = r#"{"type":"progress","phase":"running","pct":42}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::Progress(v) => assert_eq!(v["pct"], 42),
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn kebab_case_types_decode() {
        let cases: &[(&str, &str)] = &[
            (
                r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s"}"#,
                "PermissionMode",
            ),
            (
                r#"{"type":"pr-link","prNumber":1,"prUrl":"u","sessionId":"s","timestamp":"t","prRepository":"r"}"#,
                "PrLink",
            ),
            (
                r#"{"type":"file-history-snapshot","messageId":"m","isSnapshotUpdate":false,"snapshot":{}}"#,
                "FileHistorySnapshot",
            ),
            (
                r#"{"type":"queue-operation","op":"push"}"#,
                "QueueOperation",
            ),
            (r#"{"type":"last-prompt","text":"hi"}"#, "LastPrompt"),
        ];
        for (line, want) in cases {
            let ev = parse_line(line).unwrap();
            let got = match ev {
                Event::PermissionMode(_) => "PermissionMode",
                Event::PrLink(_) => "PrLink",
                Event::FileHistorySnapshot(_) => "FileHistorySnapshot",
                Event::QueueOperation(_) => "QueueOperation",
                Event::LastPrompt(_) => "LastPrompt",
                _ => "other",
            };
            assert_eq!(got, *want, "line: {line}");
        }
    }

    #[test]
    fn unknown_fields_preserved_in_extra() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"},"futureField":"value"}"#;
        let ev = parse_line(line).unwrap();
        let Event::User(m) = ev else {
            panic!("expected User")
        };
        assert_eq!(m.extra.get("futureField").unwrap(), "value");
    }

    #[test]
    fn remote_bridge_message_without_role_decodes() {
        // Real-world: `remote-bridge` variant emits message.content with no role.
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"t","version":"remote-bridge","message":{"content":"(remote session)"}}"#;
        let ev = parse_line(line).unwrap();
        let Event::User(m) = ev else {
            panic!("expected User")
        };
        assert!(m.message.role.is_none());
        assert!(matches!(m.message.content, MessageContent::Text(_)));
    }

    #[test]
    fn empty_line_rejected() {
        assert!(parse_line("").is_err());
        assert!(parse_line("   \n").is_err());
    }
}
