//! Integration tests that parse committed JSONL fixtures end-to-end.
//!
//! Unlike the unit tests in `jsonl.rs`, these exercise the full
//! `TailReader::poll` path against realistic multi-line files, and verify the
//! two streaming corner cases (partial-line resume, compaction reset) that
//! can't be checked by parsing single lines in isolation.
//!
//! Fixtures live next to this file under `tests/fixtures/` and are pulled in
//! at compile time via `include_str!`, so the tests run on a fresh machine
//! with no `~/.claude/projects/` present — PJH-48's success criterion moved
//! from "developer corpus" to "committed corpus."

use std::fs::OpenOptions;
use std::io::Write;

use ccs_core::jsonl::Event;
use ccs_core::tail::TailReader;

const BASIC: &str = include_str!("fixtures/basic.jsonl");
const COMPACTED_BEFORE: &str = include_str!("fixtures/compacted_before.jsonl");
const COMPACTED_AFTER: &str = include_str!("fixtures/compacted_after.jsonl");

fn write(path: &std::path::Path, contents: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

fn append(path: &std::path::Path, contents: &str) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

/// Count each `Event` variant in a slice so tests can assert on per-type
/// populations without matching every line individually.
#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    user: usize,
    assistant: usize,
    system: usize,
    attachment: usize,
    progress: usize,
    queue_operation: usize,
    last_prompt: usize,
    file_history_snapshot: usize,
    pr_link: usize,
    permission_mode: usize,
    custom_title: usize,
    agent_name: usize,
    unknown: usize,
}

fn count(events: &[ccs_core::tail::TailEvent]) -> Counts {
    let mut c = Counts::default();
    for ev in events {
        match &ev.event {
            Event::User(_) => c.user += 1,
            Event::Assistant(_) => c.assistant += 1,
            Event::System(_) => c.system += 1,
            Event::Attachment(_) => c.attachment += 1,
            Event::Progress(_) => c.progress += 1,
            Event::QueueOperation(_) => c.queue_operation += 1,
            Event::LastPrompt(_) => c.last_prompt += 1,
            Event::FileHistorySnapshot(_) => c.file_history_snapshot += 1,
            Event::PrLink(_) => c.pr_link += 1,
            Event::PermissionMode(_) => c.permission_mode += 1,
            Event::CustomTitle(_) => c.custom_title += 1,
            Event::AgentName(_) => c.agent_name += 1,
            Event::Unknown => c.unknown += 1,
        }
    }
    c
}

#[test]
fn basic_fixture_parses_every_event_type_without_errors_or_unknowns() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    write(tmp.path(), BASIC);

    let mut reader = TailReader::open(tmp.path());
    let result = reader.poll().unwrap();

    assert!(
        result.errors.is_empty(),
        "fixture must parse cleanly, got errors: {:?}",
        result.errors
    );
    assert!(
        !result.reset,
        "first poll on a fresh file must not signal reset"
    );

    let c = count(&result.events);
    assert_eq!(
        c,
        Counts {
            user: 3, // normal user, tool_result user, remote-bridge user
            assistant: 1,
            system: 1,
            attachment: 1,
            progress: 1,
            queue_operation: 1,
            last_prompt: 1,
            file_history_snapshot: 1,
            pr_link: 1,
            permission_mode: 1,
            custom_title: 1,
            agent_name: 1,
            unknown: 0,
        },
        "per-type counts drifted — update basic.jsonl or the assertion together"
    );
}

#[test]
fn partial_trailing_line_is_buffered_across_polls() {
    // Split `basic.jsonl` mid-way through a line and feed it in two chunks.
    // The first chunk must not produce a truncated event; the suffix on the
    // second chunk reassembles cleanly.
    let split = BASIC.len() / 2;
    // Back up to the middle of a line (not on a `\n` boundary) so we
    // deliberately exercise the partial-line buffer.
    let split = (split..BASIC.len())
        .find(|&i| BASIC.as_bytes()[i] != b'\n' && BASIC.as_bytes()[i - 1] != b'\n')
        .expect("basic.jsonl has an in-line byte past the midpoint");

    let tmp = tempfile::NamedTempFile::new().unwrap();
    write(tmp.path(), &BASIC[..split]);

    let mut reader = TailReader::open(tmp.path());
    let first = reader.poll().unwrap();
    assert!(first.errors.is_empty(), "partial chunk must not error");

    append(tmp.path(), &BASIC[split..]);
    let second = reader.poll().unwrap();
    assert!(
        second.errors.is_empty(),
        "resumed chunk must not error: {:?}",
        second.errors
    );
    assert!(!second.reset);

    let mut combined = first.events;
    combined.extend(second.events);
    let c = count(&combined);
    assert_eq!(c.unknown, 0);
    assert_eq!(
        c.user + c.assistant + c.system + c.attachment,
        6,
        "must see every typed event across the split"
    );
}

#[test]
fn compaction_shrink_triggers_reset_and_re_reads_from_top() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    write(tmp.path(), COMPACTED_BEFORE);

    let mut reader = TailReader::open(tmp.path());
    let before = reader.poll().unwrap();
    assert_eq!(before.events.len(), 4);
    assert!(!before.reset);

    // Simulate Claude Code compaction: rewrite the file smaller than our
    // current offset. TailReader should notice, reset to 0, and re-read.
    write(tmp.path(), COMPACTED_AFTER);
    let after = reader.poll().unwrap();
    assert!(after.reset, "shrink must be flagged as reset");
    assert_eq!(after.events.len(), 1);
    assert!(after.errors.is_empty());

    if let Event::User(m) = &after.events[0].event {
        assert_eq!(m.uuid, "c1");
    } else {
        panic!("expected User event after compaction");
    }
}
