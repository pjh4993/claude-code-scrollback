//! Schema-drift detection against version-pinned Claude Code JSONL snapshots.
//!
//! The JSONL schema is undocumented and drifts between Claude Code releases.
//! [`ccs_core::jsonl`]'s typed model is forward-compatible by design
//! (`#[serde(flatten)] extra` maps, `Event::Unknown` fallback, opaque
//! `serde_json::Value` on metadata-only event types), but that graceful
//! degradation *hides* drift: a v2.1.200 release could silently start
//! dropping every `assistant` message into `Unknown` and we wouldn't notice
//! until someone saw blank messages in the viewer.
//!
//! This test file inverts the signal. For each pinned fixture it asserts:
//!
//! 1. **No `Event::Unknown`.** Every top-level line decodes to a typed
//!    variant. A new top-level `type` in the fixture → test fails with the
//!    offending `type` string.
//! 2. **No `ContentBlock::Unknown`.** Every content block inside
//!    `user`/`assistant` messages decodes to a typed variant. A new block
//!    type in the fixture → test fails.
//! 3. **Every typed-struct `extra` map is empty.** Any unknown field that
//!    `#[serde(flatten)] extra` would have captured on a typed struct fails
//!    the test with the path to the offending field. Opaque `Value`
//!    payloads (`Progress`, `QueueOperation`, etc.) are deliberately not
//!    checked — they're already "we gave up on typing this."
//!
//! # When this test fails
//!
//! A failure means the Claude Code schema moved and the typed model in
//! [`ccs_core::jsonl`] no longer covers it. To refresh:
//!
//! 1. Run `cargo run -p ccs-core --example parse_all` against a real
//!    `~/.claude/projects/` populated by the new Claude Code version.
//!    Inspect any `unknown top-level types` or new fields landing in the
//!    per-type counts.
//! 2. Update the typed model in `crates/ccs-core/src/jsonl.rs`: add the new
//!    `Event` variant and/or new fields on the relevant typed struct.
//! 3. Add a new pinned fixture under `tests/schema_snapshots/` named for
//!    the Claude Code version (e.g. `v2.1.200.jsonl`) and register it in
//!    [`PINNED_VERSIONS`] below. Existing fixtures stay frozen — they
//!    represent the schema at the time their version was cut.
//! 4. `cargo test -p ccs-core --test schema_snapshots` — should pass.
//!
//! # Version pinning policy
//!
//! Start with one pinned version (v2.1.104). As schemas drift, more
//! versions get added. A future tracking issue on GitHub will decide how
//! many historical versions we keep before pruning — for now, append-only.

use ccs_core::jsonl::{self, ContentBlock, Event, Message, MessageContent, MessageEvent};
use serde_json::{Map, Value};

const V2_1_104: &str = include_str!("schema_snapshots/v2.1.104.jsonl");

/// All pinned snapshot versions. Adding a version is as simple as dropping a
/// fixture file in `tests/schema_snapshots/`, wiring an `include_str!` const
/// for it above, and appending a tuple here.
const PINNED_VERSIONS: &[(&str, &str)] = &[("2.1.104", V2_1_104)];

#[test]
fn pinned_versions_decode_without_unknowns_or_extras() {
    for (version, body) in PINNED_VERSIONS {
        for (lineno, line) in body.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event = jsonl::parse_line(line)
                .unwrap_or_else(|e| panic!("{version} line {}: parse error: {e}", lineno + 1));
            if let Err(path) = check_event(&event) {
                panic!(
                    "{version} line {}: schema drift — {path}\n  line: {line}",
                    lineno + 1
                );
            }
        }
    }
}

/// Walk a decoded [`Event`] and return `Err(path)` describing the first
/// location where an `Unknown` variant or non-empty `extra` map was found.
/// `Ok(())` means the typed model fully covers the line.
fn check_event(event: &Event) -> Result<(), String> {
    match event {
        Event::User(m) => check_message_event(m, "Event::User"),
        Event::Assistant(m) => check_message_event(m, "Event::Assistant"),
        Event::System(s) => check_extra(&s.extra, "Event::System.extra"),
        Event::Attachment(a) => check_extra(&a.extra, "Event::Attachment.extra"),
        // Intentionally opaque payloads — no typed struct, so no extras to
        // check. These variants exist to preserve the raw line; they are
        // not covered by the "no-extra" guarantee.
        Event::Progress(_)
        | Event::QueueOperation(_)
        | Event::LastPrompt(_)
        | Event::FileHistorySnapshot(_)
        | Event::PrLink(_)
        | Event::PermissionMode(_)
        | Event::CustomTitle(_)
        | Event::AgentName(_) => Ok(()),
        Event::Unknown => Err("top-level Event::Unknown — new `type` in the schema".to_string()),
    }
}

fn check_message_event(m: &MessageEvent, tag: &str) -> Result<(), String> {
    check_extra(&m.extra, &format!("{tag}.extra"))?;
    check_message(&m.message, &format!("{tag}.message"))
}

fn check_message(msg: &Message, tag: &str) -> Result<(), String> {
    check_extra(&msg.extra, &format!("{tag}.extra"))?;
    if let MessageContent::Blocks(blocks) = &msg.content {
        for (i, block) in blocks.iter().enumerate() {
            check_block(block, &format!("{tag}.content[{i}]"))?;
        }
    }
    Ok(())
}

fn check_block(block: &ContentBlock, tag: &str) -> Result<(), String> {
    match block {
        ContentBlock::Text { extra, .. } => check_extra(extra, &format!("{tag}(Text).extra")),
        ContentBlock::Thinking { extra, .. } => {
            check_extra(extra, &format!("{tag}(Thinking).extra"))
        }
        ContentBlock::ToolUse { extra, .. } => check_extra(extra, &format!("{tag}(ToolUse).extra")),
        ContentBlock::ToolResult { extra, .. } => {
            check_extra(extra, &format!("{tag}(ToolResult).extra"))
        }
        ContentBlock::Unknown => Err(format!("{tag}: ContentBlock::Unknown — new block type")),
    }
}

fn check_extra(extra: &Map<String, Value>, tag: &str) -> Result<(), String> {
    if extra.is_empty() {
        Ok(())
    } else {
        let keys: Vec<_> = extra.keys().cloned().collect();
        Err(format!(
            "{tag} populated with unknown field(s): {keys:?} — typed model needs widening"
        ))
    }
}
