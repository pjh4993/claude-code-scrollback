//! Smoke test against a real `~/.claude/projects/` tree.
//!
//! Not a unit test — it runs against whatever is on the developer's machine
//! and reports statistics. Run with `cargo run -p ccs-core --example parse_all`.

use ccs_core::{jsonl, session, tail};

fn main() -> anyhow::Result<()> {
    let root = session::projects_root().expect("no home dir");
    let (sessions, stats) = session::discover(&root)?;
    println!(
        "discovered {} sessions under {} ({} dirs skipped)",
        sessions.len(),
        root.display(),
        stats.skipped_dirs,
    );

    let mut total_lines = 0usize;
    let mut total_events = 0usize;
    let mut unknown = 0usize;
    let mut errors = 0usize;
    let mut type_counts = std::collections::BTreeMap::new();

    for s in &sessions {
        let mut reader = tail::TailReader::open(&s.path);
        let result = reader.poll()?;
        total_lines += result.events.len() + result.errors.len();
        total_events += result.events.len();
        errors += result.errors.len();
        if errors < 20 && !result.errors.is_empty() {
            for (raw, e) in result.errors.iter().take(3) {
                let preview: String = raw.chars().take(160).collect();
                eprintln!("  ERR in {}: {e} -- {preview}", s.session_id);
            }
        }
        for ev in &result.events {
            let name = variant_name(&ev.event);
            *type_counts.entry(name).or_insert(0usize) += 1;
            if matches!(ev.event, jsonl::Event::Unknown) {
                unknown += 1;
            }
        }
    }

    println!("lines:  {total_lines}");
    println!("events: {total_events}");
    println!("errors: {errors}");
    println!("unknown top-level types: {unknown}");
    println!("per-type counts:");
    for (k, v) in type_counts {
        println!("  {k:30} {v}");
    }
    Ok(())
}

fn variant_name(e: &jsonl::Event) -> &'static str {
    match e {
        jsonl::Event::User(_) => "user",
        jsonl::Event::Assistant(_) => "assistant",
        jsonl::Event::System(_) => "system",
        jsonl::Event::Attachment(_) => "attachment",
        jsonl::Event::Progress(_) => "progress",
        jsonl::Event::QueueOperation(_) => "queue-operation",
        jsonl::Event::LastPrompt(_) => "last-prompt",
        jsonl::Event::FileHistorySnapshot(_) => "file-history-snapshot",
        jsonl::Event::PrLink(_) => "pr-link",
        jsonl::Event::PermissionMode(_) => "permission-mode",
        jsonl::Event::CustomTitle(_) => "custom-title",
        jsonl::Event::AgentName(_) => "agent-name",
        jsonl::Event::Unknown => "unknown",
    }
}
