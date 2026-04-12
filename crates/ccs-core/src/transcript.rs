//! Viewer-friendly transcript model.
//!
//! Lowers raw [`jsonl::Event`]s into a flat, rendering-oriented structure:
//! one [`Message`] per displayable turn, each carrying a [`Vec<Block>`] of
//! typed content. Opaque telemetry events and sidechain messages are dropped;
//! `MessageContent::Text` is normalized to a single [`Block::Text`];
//! polymorphic `tool_result` bodies are flattened to display strings.
//!
//! The UI (`ccs-tui`) consumes this module; the CLI's `ccs print` will too.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::jsonl::{
    self, AttachmentEvent, ContentBlock, Event, MessageContent, MessageEvent, SystemEvent,
};

/// A full session prepared for rendering.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub session_id: String,
    pub project: Option<String>,
    pub messages: Vec<Message>,
}

/// One displayable turn in the transcript.
#[derive(Debug, Clone)]
pub struct Message {
    pub index: usize,
    pub role: Role,
    pub uuid: String,
    pub timestamp: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
}

/// A single renderable unit inside a message.
///
/// `ToolCall` and `ToolResult` are kept as siblings on the message that
/// produced them; the viewer pairs them across messages via `tool_use_id`
/// to drive collapse behavior.
#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolCall {
        id: String,
        name: String,
        input_json: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    Attachment(String),
    Unknown,
}

/// Read a JSONL session file from disk and lower it into a [`Transcript`].
///
/// Lines that fail to parse are skipped and logged at `warn`; a single
/// corrupt line never prevents the viewer from opening. If *every*
/// non-empty line fails to parse, this returns an error so the viewer
/// can surface "this file is not a Claude Code session" instead of
/// silently rendering an empty transcript.
pub fn load_from_path(path: &Path) -> anyhow::Result<Transcript> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events: Vec<Event> = Vec::new();
    let mut saw_non_empty = false;
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        saw_non_empty = true;
        match jsonl::parse_line(&line) {
            Ok(ev) => events.push(ev),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    lineno = idx + 1,
                    error = %err,
                    "skipping malformed JSONL line",
                );
            }
        }
    }
    if saw_non_empty && events.is_empty() {
        anyhow::bail!("no valid JSONL events recovered from {}", path.display());
    }
    Ok(from_events(events))
}

/// Build a [`Transcript`] from a stream of parsed events.
///
/// * Drops sidechain messages (primary-only for v1).
/// * Drops opaque telemetry (`progress`, `queue-operation`, `last-prompt`,
///   `file-history-snapshot`, `pr-link`, `permission-mode`, `custom-title`,
///   `agent-name`, `unknown`).
/// * Infers `session_id` and `project` (`cwd`) from the first event that
///   carries them.
pub fn from_events(events: impl IntoIterator<Item = Event>) -> Transcript {
    let mut out = Transcript::default();
    let mut next_index = 0usize;

    for ev in events {
        match ev {
            Event::User(me) => {
                capture_meta(&mut out, &me);
                if me.is_sidechain {
                    continue;
                }
                let blocks = lower_message_content(me.message.content);
                if blocks.is_empty() {
                    continue;
                }
                out.messages.push(Message {
                    index: next_index,
                    role: Role::User,
                    uuid: me.uuid,
                    timestamp: me.timestamp,
                    blocks,
                });
                next_index += 1;
            }
            Event::Assistant(me) => {
                capture_meta(&mut out, &me);
                if me.is_sidechain {
                    continue;
                }
                let blocks = lower_message_content(me.message.content);
                if blocks.is_empty() {
                    continue;
                }
                out.messages.push(Message {
                    index: next_index,
                    role: Role::Assistant,
                    uuid: me.uuid,
                    timestamp: me.timestamp,
                    blocks,
                });
                next_index += 1;
            }
            Event::System(se) => {
                if out.session_id.is_empty() {
                    out.session_id = se.session_id.clone();
                }
                if let Some(msg) = lower_system_event(se, next_index) {
                    out.messages.push(msg);
                    next_index += 1;
                }
            }
            Event::Attachment(ae) => {
                if let Some(msg) = lower_attachment_event(ae, next_index, &mut out.session_id) {
                    out.messages.push(msg);
                    next_index += 1;
                }
            }
            // Opaque telemetry — intentionally dropped.
            Event::Progress(_)
            | Event::QueueOperation(_)
            | Event::LastPrompt(_)
            | Event::FileHistorySnapshot(_)
            | Event::PrLink(_)
            | Event::PermissionMode(_)
            | Event::CustomTitle(_)
            | Event::AgentName(_)
            | Event::Unknown => {}
        }
    }

    out
}

fn capture_meta(out: &mut Transcript, me: &MessageEvent) {
    if out.session_id.is_empty() {
        out.session_id = me.session_id.clone();
    }
    if out.project.is_none() {
        if let Some(cwd) = &me.cwd {
            out.project = Some(cwd.clone());
        }
    }
}

fn lower_message_content(content: MessageContent) -> Vec<Block> {
    match content {
        MessageContent::Text(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![Block::Text(s)]
            }
        }
        MessageContent::Blocks(blocks) => blocks.into_iter().map(lower_block).collect(),
    }
}

fn lower_block(block: ContentBlock) -> Block {
    match block {
        ContentBlock::Text { text, .. } => Block::Text(text),
        ContentBlock::Thinking { thinking, .. } => Block::Thinking(thinking.unwrap_or_default()),
        ContentBlock::ToolUse {
            id, name, input, ..
        } => Block::ToolCall {
            id,
            name,
            input_json: pretty_json(&input),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => Block::ToolResult {
            tool_use_id,
            content: flatten_tool_result_content(content),
            is_error,
        },
        ContentBlock::Unknown => Block::Unknown,
    }
}

fn lower_system_event(se: SystemEvent, index: usize) -> Option<Message> {
    // Synthesize a short text line. System events carry no body of their own;
    // their useful fields are `subtype`, `level`, `stop_reason`.
    let mut parts: Vec<String> = Vec::new();
    if let Some(sub) = &se.subtype {
        parts.push(sub.clone());
    }
    if let Some(level) = &se.level {
        parts.push(format!("[{level}]"));
    }
    if let Some(reason) = &se.stop_reason {
        parts.push(format!("stop_reason={reason}"));
    }
    let text = if parts.is_empty() {
        return None;
    } else {
        parts.join(" ")
    };
    Some(Message {
        index,
        role: Role::System,
        uuid: se.uuid,
        timestamp: se.timestamp,
        blocks: vec![Block::Text(text)],
    })
}

fn lower_attachment_event(
    ae: AttachmentEvent,
    index: usize,
    session_id: &mut String,
) -> Option<Message> {
    if session_id.is_empty() {
        *session_id = ae.session_id.clone();
    }
    let summary = summarize_attachment(&ae.attachment);
    Some(Message {
        index,
        role: Role::User,
        uuid: ae.uuid,
        timestamp: ae.timestamp,
        blocks: vec![Block::Attachment(summary)],
    })
}

fn summarize_attachment(v: &Value) -> String {
    // Common shapes: `{"type":"file","path":"..."}`, `{"type":"image",...}`.
    if let Some(obj) = v.as_object() {
        let ty = obj
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("attachment");
        if let Some(path) = obj.get("path").and_then(|p| p.as_str()) {
            return format!("{ty}: {path}");
        }
        if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
            return format!("{ty}: {name}");
        }
        return ty.to_string();
    }
    v.to_string()
}

/// Flatten a `tool_result.content` field into a display string.
///
/// The schema is polymorphic:
/// * a bare string → return as-is
/// * an array of `{type:"text", text:"..."}` blocks → concatenate texts
/// * anything else → pretty JSON
fn flatten_tool_result_content(v: Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s,
        Value::Array(items) => {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                // Only treat `{type:"text", text:"..."}` as body text; any other
                // block shape (e.g. `{type:"image", ...}`) falls back to JSON.
                let is_text_block = item.get("type").and_then(|t| t.as_str()) == Some("text");
                if is_text_block {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        out.push_str(text);
                        continue;
                    }
                }
                out.push_str(&pretty_json(item));
            }
            out
        }
        other => pretty_json(&other),
    }
}

fn pretty_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonl::parse_line;

    fn events(lines: &[&str]) -> Vec<Event> {
        lines.iter().map(|l| parse_line(l).unwrap()).collect()
    }

    #[test]
    fn empty_input_yields_empty_transcript() {
        let t = from_events(std::iter::empty());
        assert_eq!(t.session_id, "");
        assert!(t.project.is_none());
        assert!(t.messages.is_empty());
    }

    #[test]
    fn user_string_content_becomes_text_block() {
        let t = from_events(events(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","cwd":"/proj","message":{"role":"user","content":"hello"}}"#,
        ]));
        assert_eq!(t.session_id, "s1");
        assert_eq!(t.project.as_deref(), Some("/proj"));
        assert_eq!(t.messages.len(), 1);
        let m = &t.messages[0];
        assert_eq!(m.index, 0);
        assert_eq!(m.role, Role::User);
        assert!(matches!(m.blocks[0], Block::Text(ref s) if s == "hello"));
    }

    #[test]
    fn assistant_mixed_blocks_preserve_order() {
        let line = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"x"}}]}}"#;
        let t = from_events(events(&[line]));
        assert_eq!(t.messages.len(), 1);
        let m = &t.messages[0];
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.blocks.len(), 3);
        assert!(matches!(m.blocks[0], Block::Thinking(ref s) if s == "hm"));
        assert!(matches!(m.blocks[1], Block::Text(ref s) if s == "hi"));
        match &m.blocks[2] {
            Block::ToolCall {
                id,
                name,
                input_json,
            } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "Read");
                assert!(input_json.contains("\"path\""));
                assert!(input_json.contains("\"x\""));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_string_content_passes_through() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok","is_error":false}]}}"#;
        let t = from_events(events(&[line]));
        match &t.messages[0].blocks[0] {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "tu1");
                assert_eq!(content, "ok");
                assert!(!*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_array_of_text_blocks_is_concatenated() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"line1"},{"type":"text","text":"line2"}]}]}}"#;
        let t = from_events(events(&[line]));
        match &t.messages[0].blocks[0] {
            Block::ToolResult { content, .. } => assert_eq!(content, "line1\nline2"),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_array_non_text_block_falls_back_to_json() {
        // Regression: only `{type:"text", text:...}` entries should be
        // concatenated as body text; other block shapes must fall through to
        // the pretty-JSON representation, even if they happen to have a `text`
        // field (e.g. an image block with alt text).
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"hello"},{"type":"image","text":"alt","source":{"data":"..."}}]}]}}"#;
        let t = from_events(events(&[line]));
        match &t.messages[0].blocks[0] {
            Block::ToolResult { content, .. } => {
                assert!(content.starts_with("hello\n"));
                // The image block must NOT be rendered as just "alt"; it must
                // appear as JSON containing its `type` field.
                assert!(content.contains("\"image\""));
                assert!(content.contains("\"source\""));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_object_content_is_pretty_json() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":{"ok":true,"n":3}}]}}"#;
        let t = from_events(events(&[line]));
        match &t.messages[0].blocks[0] {
            Block::ToolResult { content, .. } => {
                assert!(content.contains("\"ok\""));
                assert!(content.contains("true"));
                assert!(content.contains("\"n\""));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn sidechain_messages_are_dropped() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","isSidechain":true,"message":{"role":"user","content":"sub"}}"#;
        let t = from_events(events(&[line]));
        assert!(t.messages.is_empty());
    }

    #[test]
    fn telemetry_events_are_dropped_but_session_id_still_picks_up() {
        let lines = &[
            r#"{"type":"progress","phase":"running","pct":42}"#,
            r#"{"type":"queue-operation","op":"push"}"#,
            r#"{"type":"file-history-snapshot","messageId":"m","isSnapshotUpdate":false,"snapshot":{}}"#,
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ];
        let t = from_events(events(lines));
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.session_id, "s1");
    }

    #[test]
    fn thinking_block_with_null_body_becomes_empty_string() {
        let line = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking"}]}}"#;
        let t = from_events(events(&[line]));
        assert!(matches!(&t.messages[0].blocks[0], Block::Thinking(s) if s.is_empty()));
    }

    #[test]
    fn unknown_content_blocks_survive_as_unknown() {
        let line = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"future_block","foo":"bar"}]}}"#;
        let t = from_events(events(&[line]));
        assert!(matches!(&t.messages[0].blocks[0], Block::Unknown));
    }

    #[test]
    fn indices_are_sequential_across_kept_messages() {
        let lines = &[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"a"}}"#,
            r#"{"type":"progress","pct":1}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"b"}]}}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"t","isSidechain":true,"message":{"role":"user","content":"skip"}}"#,
            r#"{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"t","message":{"role":"user","content":"c"}}"#,
        ];
        let t = from_events(events(lines));
        let indices: Vec<usize> = t.messages.iter().map(|m| m.index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn sidechain_envelopes_still_contribute_metadata() {
        // Regression: sidechain messages are dropped from `messages`, but their
        // `sessionId` / `cwd` should still populate the transcript metadata.
        let lines = &[
            r#"{"type":"user","uuid":"u1","sessionId":"s-sidechain","timestamp":"t","cwd":"/proj","isSidechain":true,"message":{"role":"user","content":"sub"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s-sidechain","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
        ];
        let t = from_events(events(lines));
        assert_eq!(t.session_id, "s-sidechain");
        assert_eq!(t.project.as_deref(), Some("/proj"));
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.messages[0].role, Role::Assistant);
    }

    #[test]
    fn system_first_uses_session_id_not_message_uuid() {
        // Regression: `out.session_id` must come from `SystemEvent.session_id`,
        // never from the message `uuid` (a different identifier class).
        let lines = &[
            r#"{"type":"system","uuid":"sys-uuid-1","sessionId":"s-real","timestamp":"t","subtype":"tool_error","level":"warn"}"#,
            r#"{"type":"user","uuid":"u1","sessionId":"s-real","timestamp":"t","message":{"role":"user","content":"hi"}}"#,
        ];
        let t = from_events(events(lines));
        assert_eq!(t.session_id, "s-real");
        assert_ne!(t.session_id, "sys-uuid-1");
    }

    #[test]
    fn empty_user_text_produces_no_message() {
        let line = r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","message":{"role":"user","content":""}}"#;
        let t = from_events(events(&[line]));
        assert!(t.messages.is_empty());
    }

    #[test]
    fn load_from_path_reads_jsonl_and_skips_malformed() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"t","cwd":"/proj","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();
        writeln!(f, "this is not json").unwrap();
        writeln!(f).unwrap(); // blank line
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"t","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}]}}}}"#
        )
        .unwrap();
        drop(f);

        let t = load_from_path(&path).unwrap();
        assert_eq!(t.session_id, "s1");
        assert_eq!(t.project.as_deref(), Some("/proj"));
        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.messages[0].role, Role::User);
        assert_eq!(t.messages[1].role, Role::Assistant);
    }

    #[test]
    fn load_from_path_missing_file_errors() {
        let err = load_from_path(Path::new("/nonexistent/path/does/not/exist.jsonl"));
        assert!(err.is_err());
    }

    #[test]
    fn load_from_path_fully_corrupt_file_errors() {
        // Regression: a non-empty file whose every line fails to parse must
        // surface an error rather than quietly returning an empty transcript.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "not json").unwrap();
        writeln!(f, "also not json").unwrap();
        drop(f);

        let err = load_from_path(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no valid JSONL events"), "got: {msg}");
    }

    #[test]
    fn load_from_path_empty_file_is_ok() {
        // Distinct from fully-corrupt: a truly empty file produces an empty
        // transcript without erroring (nothing failed to parse).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        File::create(&path).unwrap();
        let t = load_from_path(&path).unwrap();
        assert!(t.messages.is_empty());
    }
}
