//! Benchmarks for [`ccs_core::tail::TailReader::poll`].
//!
//! Measures the **first `poll()` call on a freshly-constructed
//! [`TailReader`]** against a pre-populated JSONL file. This is the path a
//! picker preview takes when it opens a session, and the live-tail
//! catch-up path when a reader attaches to a session that has already
//! been running.
//!
//! Note that "first poll" is a *parser-path* cold state, not a *disk*
//! cold state: every iteration re-opens the same file, so the OS page
//! cache is warm after the first iter. The numbers below are therefore an
//! upper bound on what the parser can achieve when bytes are already in
//! memory — which is also the state the picker will be in on a warm
//! session. Actual disk-cold reads will be slower on spinning disks or
//! network filesystems; we don't try to simulate that here.
//!
//! We deliberately cap the largest size at 100k lines (not the 1M suggested
//! in the ticket): real Claude Code sessions rarely exceed a few thousand
//! lines, and 1M lines yields a ~500 MB corpus whose setup would dominate
//! the measurement without adding meaningful signal.
//!
//! # Usage
//!
//! ```text
//! cargo bench -p ccs-core --bench tail
//! ```
//!
//! Not wired into CI — see [`ccs_core`] issue #14.
//!
//! # Baseline — 2026-04-12 (UTC), Apple M4 Pro (12-core), macOS 26.3.1, APFS
//!
//! ```text
//! tail_cold/1000       1.39 ms   (median)  —  721 Kelem/s
//! tail_cold/10000    113.74 ms   (median)  —   88 Kelem/s
//! tail_cold/100000    11.11 s    (median)  —    9 Kelem/s   ⚠ super-linear
//! ```
//!
//! **Non-linear scaling surfaced.** 10× more data costs 100× more time at
//! the 10k→100k step. Root cause is [`TailReader::drain_lines`]: each
//! `self.pending.drain(..=idx).collect()` call rewrites the remaining
//! buffer in place, making the total drain work O(N²) in byte copies.
//! For 100k lines × ~180 bytes each, that's hundreds of gigabytes of
//! memmove work, which is what the wall clock is showing.
//!
//! This isn't a bench bug — it's the bench doing its job. See [issue #26]
//! for the follow-up that rewrites `drain_lines` with a cursor-based
//! approach (no per-line shifts), which should restore linear scaling.
//! Until that lands, the 100k case is an honest baseline of the current
//! implementation, and real Claude Code sessions in the wild rarely
//! exceed 10k lines so nobody has noticed in production.
//!
//! [issue #26]: https://github.com/pjh4993/claude-code-scrollback/issues/26
//!
//! When re-running on a different machine, **add** a dated row beside the
//! existing ones rather than overwriting — the baseline is a historical
//! record, not a moving target.

use std::io::{BufWriter, Write};
use std::time::Duration;

use ccs_core::tail::TailReader;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::NamedTempFile;

/// Write `n_lines` synthesized `user` events to a fresh temporary file.
/// Each line is ~180 bytes, so 100k lines → ~18 MB.
fn write_corpus(n_lines: usize) -> NamedTempFile {
    let file = NamedTempFile::new().expect("tempfile");
    {
        let mut w = BufWriter::new(file.reopen().expect("reopen"));
        for i in 0..n_lines {
            writeln!(
                w,
                r#"{{"type":"user","uuid":"u{i}","sessionId":"s","timestamp":"2026-04-13T00:00:00.000Z","message":{{"role":"user","content":"bench line {i}"}}}}"#,
            )
            .expect("write line");
        }
        w.flush().expect("flush");
    }
    file
}

fn bench_tail_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("tail_cold");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(3));

    for &n in &[1_000usize, 10_000, 100_000] {
        let file = write_corpus(n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let mut reader = TailReader::open(file.path());
                let result = reader.poll().expect("poll");
                std::hint::black_box(result);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_tail_cold);
criterion_main!(benches);
