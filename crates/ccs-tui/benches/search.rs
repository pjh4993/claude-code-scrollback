//! Benchmarks for [`ccs_tui::ui::transcript::search::SearchIndex`].
//!
//! Measures the cost of (a) building a search index from a rendered line
//! cache produced by `layout::build` and (b) finding every match of a
//! needle in that index. The target corpus is a synthetic 10k-message
//! transcript — roughly the high-end of a typical `~/.claude/projects`
//! session.
//!
//! # Usage
//!
//! ```text
//! cargo bench -p ccs-tui --bench search
//! ```
//!
//! Not wired into CI — run manually. Compare future runs against the
//! baseline below by **adding** a dated row, not overwriting, so the
//! baseline stays a historical record.
//!
//! # Baseline — 2026-04-13 (UTC), Apple M4 Pro (12-core), macOS 26.3.1
//!
//! ```text
//! search/index_build/10000     926 µs  (median)
//! search/find_all/10000        118 µs  (median)
//! ```
//!
//! Corpus: 10002 messages → 53343 rendered body lines at width 120.
//! PJH-50's "first match < 100 ms on a 10k-message session" target has
//! **~850× headroom** over the current implementation — the perf risk
//! here is nonexistent. The bench exists to catch the next order-of-
//! magnitude regression, not to enforce the target.
//!
//! When re-running on a different machine, **add** a dated row beside
//! the existing one rather than overwriting — the baseline is a
//! historical record, not a moving target.

use std::collections::HashSet;

use ccs_core::jsonl::parse_line;
use ccs_core::transcript::{from_events, Transcript};
use ccs_tui::ui::transcript::layout::{self, CollapseContext};
use ccs_tui::ui::transcript::search::SearchIndex;
use ccs_tui::ui::transcript::state::{BlockId, CollapseAll};
use criterion::{criterion_group, criterion_main, Criterion};

/// Build a synthetic transcript with `n` alternating user/assistant
/// messages. Each assistant turn contains a short text block plus a
/// tool_use + tool_result pair so the resulting line cache has a
/// realistic mix of block kinds.
fn synthetic_transcript(n: usize) -> Transcript {
    let mut lines: Vec<String> = Vec::with_capacity(n * 3);
    for i in 0..n {
        lines.push(format!(
            r#"{{"type":"user","uuid":"u{i}","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":"question {i} about rust ownership and lifetimes"}}}}"#
        ));
        lines.push(format!(
            r#"{{"type":"assistant","uuid":"a{i}","sessionId":"s1","timestamp":"t","message":{{"role":"assistant","content":[{{"type":"text","text":"answer {i}: ownership transfers on move, references borrow"}},{{"type":"tool_use","id":"t{i}","name":"Read","input":{{"path":"file{i}.rs"}}}}]}}}}"#
        ));
        lines.push(format!(
            r#"{{"type":"user","uuid":"r{i}","sessionId":"s1","timestamp":"t","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{i}","content":"fn main() {{ println!(\"hello\"); }}","is_error":false}}]}}}}"#
        ));
    }
    from_events(lines.iter().map(|l| parse_line(l).unwrap()))
}

fn bench_search(c: &mut Criterion) {
    let transcript = synthetic_transcript(3334); // ≈10k messages after lowering
    let collapsed: HashSet<BlockId> = HashSet::new();
    let ctx = CollapseContext {
        collapsed: &collapsed,
        collapse_all: CollapseAll::Off,
    };
    let layout_out = layout::build(&transcript, 120, &ctx);
    let lines = layout_out.lines;
    let total_msgs = transcript.messages.len();
    let total_lines = lines.len();
    eprintln!("synthetic corpus: {total_msgs} msgs, {total_lines} rendered lines");

    let mut group = c.benchmark_group("search");
    group.bench_function("index_build/10000", |b| {
        b.iter(|| {
            let idx = SearchIndex::build(&lines);
            criterion::black_box(idx);
        });
    });

    let index = SearchIndex::build(&lines);
    group.bench_function("find_all/10000", |b| {
        b.iter(|| {
            let matches = index.find_all("ownership");
            criterion::black_box(matches);
        });
    });
    group.finish();
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
