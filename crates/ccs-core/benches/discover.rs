//! Benchmarks for [`ccs_core::session::discover`].
//!
//! Measures the cost of walking `~/.claude/projects`-shaped trees of varying
//! sizes using pure `fs::read_dir` + `fs::metadata` — no file contents are
//! read, since [`discover`](ccs_core::session::discover) never parses JSONL.
//! These numbers are the uncached baseline that PJH-54 (SQLite metadata
//! cache) should be benchmarked against.
//!
//! # Usage
//!
//! ```text
//! cargo bench -p ccs-core --bench discover
//! ```
//!
//! Not wired into CI — run manually or on a weekly cadence. See [`ccs_core`]
//! issue #14 for the policy rationale.
//!
//! # Baseline — 2026-04-12 (UTC), Apple M4 Pro (12-core), macOS 26.3.1, APFS
//!
//! ```text
//! discover/1000       3.67 ms  (median)   — 272 Kelem/s
//! discover/10000     42.62 ms  (median)   — 234 Kelem/s
//! discover/50000    225.18 ms  (median)   — 222 Kelem/s
//! ```
//!
//! **Linear scaling holds** across three orders of magnitude — each 10×
//! increase in session count costs roughly 5–6× more wall time, reflecting
//! the per-dir `read_dir` overhead amortizing nicely. PJH-54's 500 ms
//! target on a 10k-session corpus is ~12× headroom over the uncached
//! baseline, so the cache's value is almost entirely at >50k corpora or on
//! slower filesystems (spinning disks, network mounts, FUSE).
//!
//! When re-running on a different machine, **add** a dated row beside the
//! existing ones rather than overwriting — the baseline is a historical
//! record, not a moving target.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ccs_core::session;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;

/// Populate `root` with exactly `session_count` empty session JSONLs,
/// spread across ~`session_count / 10` project dirs so both the outer and
/// inner `read_dir` paths in [`session::discover`] get exercised. The loop
/// breaks out the moment it hits `session_count`, so non-multiple-of-10
/// inputs don't overshoot.
fn populate(root: &Path, session_count: usize) {
    let project_count = (session_count / 10).max(1);
    let per_project = session_count.div_ceil(project_count);
    let mut created = 0usize;
    for p in 0..project_count {
        if created == session_count {
            break;
        }
        let proj = root.join(format!("-tmp-bench-project-{p:06}"));
        fs::create_dir_all(&proj).expect("mkdir project");
        for s in 0..per_project {
            if created == session_count {
                break;
            }
            let path = proj.join(format!("{p:06}-{s:04}.jsonl"));
            fs::write(path, b"").expect("write session stub");
            created += 1;
        }
    }
    debug_assert_eq!(created, session_count);
}

fn bench_discover(c: &mut Criterion) {
    let mut group = c.benchmark_group("discover");
    // Generating 50k files + 5k dirs per iter would dwarf the measurement.
    // Cap sampling on the largest sizes so a full run stays under ~90s.
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(3));

    for &n in &[1_000usize, 10_000, 50_000] {
        // Fresh corpus per size, held alive for the group's entire iter loop.
        let tmp = TempDir::new().expect("tempdir");
        populate(tmp.path(), n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let sessions = session::discover(tmp.path()).expect("discover");
                std::hint::black_box(sessions);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_discover);
criterion_main!(benches);
