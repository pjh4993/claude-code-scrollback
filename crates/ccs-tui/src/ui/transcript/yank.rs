//! Format a [`Message`] as a copyable plain-text string for the `y`
//! yank key.
//!
//! Per the PJH-50 plan: text blocks copy as their raw markdown source
//! (preserves code fences for pasting into another tool), tool calls
//! as pretty JSON, tool results as their plain content, attachments
//! and unknown blocks as a short placeholder line. Blocks inside one
//! message are joined with a blank line so a multi-block assistant
//! turn pastes cleanly.

use ccs_core::transcript::{Block, Message, Role};

pub fn format_message(message: &Message) -> String {
    let role_label = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    };
    let mut out = String::new();
    out.push_str(role_label);
    if !message.timestamp.is_empty() {
        out.push_str(" @ ");
        out.push_str(&message.timestamp);
    }
    out.push('\n');
    let mut first = true;
    for block in &message.blocks {
        if !first {
            out.push_str("\n\n");
        }
        first = false;
        match block {
            Block::Text(s) => out.push_str(s),
            Block::Thinking(s) => {
                out.push_str("[thinking]\n");
                out.push_str(s);
            }
            Block::ToolCall {
                name, input_json, ..
            } => {
                out.push_str(&format!("[tool call: {name}]\n"));
                out.push_str(input_json);
            }
            Block::ToolResult {
                content, is_error, ..
            } => {
                if *is_error {
                    out.push_str("[tool result — error]\n");
                } else {
                    out.push_str("[tool result]\n");
                }
                out.push_str(content);
            }
            Block::Attachment(s) => {
                out.push_str("[attachment] ");
                out.push_str(s);
            }
            Block::Unknown => out.push_str("[unknown block]"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs_core::transcript::{Block, Message, Role};

    fn msg(blocks: Vec<Block>) -> Message {
        Message {
            index: 0,
            role: Role::Assistant,
            uuid: "u1".to_string(),
            timestamp: "2026-04-12T00:00:00Z".to_string(),
            blocks,
        }
    }

    #[test]
    fn text_block_is_pasted_verbatim() {
        let m = msg(vec![Block::Text("**bold** markdown".to_string())]);
        let s = format_message(&m);
        assert!(s.contains("**bold** markdown"));
        assert!(s.starts_with("assistant @ "));
    }

    #[test]
    fn tool_call_includes_name_and_json() {
        let m = msg(vec![Block::ToolCall {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input_json: "{\n  \"path\": \"x\"\n}".to_string(),
        }]);
        let s = format_message(&m);
        assert!(s.contains("[tool call: Read]"));
        assert!(s.contains("\"path\": \"x\""));
    }

    #[test]
    fn tool_result_error_is_labeled() {
        let m = msg(vec![Block::ToolResult {
            tool_use_id: "t1".to_string(),
            content: "boom".to_string(),
            is_error: true,
        }]);
        let s = format_message(&m);
        assert!(s.contains("[tool result — error]"));
        assert!(s.contains("boom"));
    }

    #[test]
    fn blocks_are_joined_with_blank_line() {
        let m = msg(vec![
            Block::Thinking("hm".to_string()),
            Block::Text("answer".to_string()),
        ]);
        let s = format_message(&m);
        assert!(s.contains("[thinking]\nhm"));
        assert!(s.contains("\n\nanswer"));
    }
}
