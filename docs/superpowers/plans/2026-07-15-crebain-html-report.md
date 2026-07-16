# crebain HTML Benchmark Report Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `--report <PATH>` flag to crebain that writes a self-contained HTML file charting requests/sec, success/fail records (cumulative and per-second), and — in `--isolated` mode — the target ingest server's CPU (cores) and RAM (RSS) over time.

**Architecture:** The existing per-second metrics aggregator retains one `TimePoint` per tick and, when it knows the spawned ingest's PID, samples that process from Linux `/proc`. The timeline rides on the final `Summary`. A new pure-Rust module renders it to a single HTML file with hand-drawn inline SVG line charts (no JS, no CDN, no new crates).

**Tech Stack:** Rust, tokio, clap, chrono (already deps). Linux `/proc` for resource sampling. Inline SVG for charts.

## Global Constraints

- **No new crate dependencies.** `backend/bins/crebain/Cargo.toml` must not gain entries. Resource sampling reads `/proc` via `std::fs`; CPU-core count via `std::thread::available_parallelism`; timestamp via the existing `chrono` dep.
- **CPU unit is cores** (1.0 = one full core), never a raw percent. Formula is USER_HZ-independent: `Δproc_jiffies / Δtotal_jiffies × ncpus`.
- **Resource sampling is best-effort:** any `/proc` read/parse failure yields `None` and omits the resource charts; it must never fail the run. Non-Linux platforms have no `/proc`, so resource charts are simply absent there.
- **Report is opt-in:** without `--report`, output is byte-for-byte the current text summary. A report *write* failure is a warning, not a run failure (the run already completed).
- **The HTML file is fully self-contained:** all CSS inline, all charts inline SVG, zero external/network requests.
- **Do NOT commit** unless the user explicitly authorizes it (standing project rule: work stays on the local branch, commits are held). Each task still ends at a green, independently-testable checkpoint. Treat every "Checkpoint" step as "reach green and stop" — do not `git commit` unless told.
- **Working directory for all commands:** `/home/splimter/projects/freelance/sauron/.claude/worktrees/crebain-benchmark/backend` (the Cargo workspace root). The crate is `crebain`; run tests with `cargo test -p crebain`.

---

### Task 1: `procstat` — Linux `/proc` sampling

**Files:**
- Create: `bins/crebain/src/procstat.rs`
- Modify: `bins/crebain/src/main.rs` (add `mod procstat;` to the module list)

**Interfaces:**
- Consumes: nothing (std only).
- Produces:
  - `pub fn parse_pid_stat_cpu_jiffies(contents: &str) -> Option<u64>`
  - `pub fn parse_stat_total_jiffies(contents: &str) -> Option<u64>`
  - `pub fn parse_status_vmrss_bytes(contents: &str) -> Option<u64>`
  - `pub fn cores_from_deltas(dproc: u64, dtotal: u64, ncpus: f64) -> f64`
  - `pub struct RawSample { pub proc_jiffies: u64, pub total_jiffies: u64, pub rss_bytes: u64 }`
  - `pub struct Sampler { /* private */ }` with `pub fn new(pid: u32) -> Self`, `pub fn ncpus(&self) -> f64`, `pub fn sample(&self) -> Option<RawSample>`

- [ ] **Step 1: Add the module declaration**

In `bins/crebain/src/main.rs`, the `mod` list currently reads:
```rust
mod cli;
mod client;
mod db_url;
mod dsn;
mod engine;
mod generator;
mod harness;
mod metrics;
mod report;
mod user;
```
Add `mod procstat;` (alphabetical, after `metrics;`):
```rust
mod metrics;
mod procstat;
mod report;
```

- [ ] **Step 2: Write the failing tests**

Create `bins/crebain/src/procstat.rs` with only the tests first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // comm field intentionally contains spaces and a ')': the parser must key
    // off the LAST ')'. Fields after it: state ppid pgrp session tty tpgid flags
    // minflt cminflt majflt cmajflt utime stime ... → utime is the 12th token.
    const PID_STAT: &str =
        "1234 (in gest (x)) S 1 1234 1234 0 -1 4194560 100 0 0 0 4200 1800 0 0 20 0 8 0 999";

    #[test]
    fn parses_utime_plus_stime() {
        assert_eq!(parse_pid_stat_cpu_jiffies(PID_STAT), Some(4200 + 1800));
    }

    #[test]
    fn parses_total_jiffies_from_cpu_line() {
        let stat = "cpu  100 20 30 400 5 0 6 0 0 0\ncpu0 50 10 15 200 2 0 3 0 0 0\n";
        assert_eq!(parse_stat_total_jiffies(stat), Some(100 + 20 + 30 + 400 + 5 + 0 + 6));
    }

    #[test]
    fn parses_vmrss_kb_to_bytes() {
        let status = "Name:\tingest\nState:\tS\nVmRSS:\t  20480 kB\nThreads:\t8\n";
        assert_eq!(parse_status_vmrss_bytes(status), Some(20480 * 1024));
    }

    #[test]
    fn malformed_inputs_return_none() {
        assert_eq!(parse_pid_stat_cpu_jiffies("garbage no paren"), None);
        assert_eq!(parse_stat_total_jiffies("no cpu line here"), None);
        assert_eq!(parse_status_vmrss_bytes("no vmrss"), None);
    }

    #[test]
    fn cores_formula_is_userhz_independent() {
        // proc used 400 of 4000 total jiffies over the interval on an 8-core box
        // → 0.1 of all-core time × 8 = 0.8 cores.
        assert!((cores_from_deltas(400, 4000, 8.0) - 0.8).abs() < 1e-9);
        // zero elapsed total → 0, never a divide-by-zero.
        assert_eq!(cores_from_deltas(5, 0, 8.0), 0.0);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p crebain procstat`
Expected: FAIL — `cannot find function parse_pid_stat_cpu_jiffies` etc.

- [ ] **Step 4: Write the implementation**

Prepend to `bins/crebain/src/procstat.rs` (above the `#[cfg(test)] mod tests`):
```rust
//! Best-effort Linux `/proc` sampling of a target process: CPU time (jiffies)
//! and resident memory (RSS). Everything is `Option`-returning — a missing or
//! malformed `/proc` (including on non-Linux, where it does not exist) yields
//! `None` so the caller simply omits the resource charts, never failing a run.

/// utime + stime (fields 14 and 15) from `/proc/<pid>/stat`, in clock ticks.
/// The `comm` field (field 2) may contain spaces and parentheses, so we split
/// on the LAST ')': everything after it starts at field 3 (`state`), which puts
/// utime at token index 11 and stime at index 12.
pub fn parse_pid_stat_cpu_jiffies(contents: &str) -> Option<u64> {
    let rparen = contents.rfind(')')?;
    let rest = contents.get(rparen + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Sum of every field on the aggregate `cpu ` line of `/proc/stat` (total system
/// jiffies across all cores). The trailing space avoids matching `cpu0`, `cpu1`.
pub fn parse_stat_total_jiffies(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|l| l.starts_with("cpu "))?;
    let mut total: u64 = 0;
    for tok in line.split_whitespace().skip(1) {
        total = total.checked_add(tok.parse::<u64>().ok()?)?;
    }
    Some(total)
}

/// `VmRSS` from `/proc/<pid>/status` (reported in kB) converted to bytes.
pub fn parse_status_vmrss_bytes(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Cores used over an interval from CPU-jiffie deltas. USER_HZ cancels because
/// numerator and denominator share the same jiffie unit, so no `libc` is needed.
pub fn cores_from_deltas(dproc: u64, dtotal: u64, ncpus: f64) -> f64 {
    if dtotal == 0 {
        0.0
    } else {
        dproc as f64 / dtotal as f64 * ncpus
    }
}

/// One raw reading of the target process and the system CPU counter.
pub struct RawSample {
    pub proc_jiffies: u64,
    pub total_jiffies: u64,
    pub rss_bytes: u64,
}

/// Reads `/proc` for a fixed pid. Holds the (fixed) core count so callers can
/// convert jiffie deltas to cores.
pub struct Sampler {
    pid: u32,
    ncpus: f64,
}

impl Sampler {
    pub fn new(pid: u32) -> Self {
        let ncpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        Sampler { pid, ncpus }
    }

    pub fn ncpus(&self) -> f64 {
        self.ncpus
    }

    /// Read `/proc/<pid>/stat`, `/proc/stat`, and `/proc/<pid>/status`. `None` if
    /// any read or parse fails (process gone, non-Linux, unexpected format).
    pub fn sample(&self) -> Option<RawSample> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let total = std::fs::read_to_string("/proc/stat").ok()?;
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.pid)).ok()?;
        Some(RawSample {
            proc_jiffies: parse_pid_stat_cpu_jiffies(&stat)?,
            total_jiffies: parse_stat_total_jiffies(&total)?,
            rss_bytes: parse_status_vmrss_bytes(&status)?,
        })
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p crebain procstat`
Expected: PASS (5 tests).

- [ ] **Step 6: Checkpoint**

Run: `cargo build -p crebain`
Expected: builds clean (a `dead_code` warning on `Sampler`/`RawSample` is fine — they're wired up in Task 2/3). Do not commit (see Global Constraints).

---

### Task 2: metrics timeline + resource sampling

**Files:**
- Modify: `bins/crebain/src/metrics.rs`

**Interfaces:**
- Consumes (from Task 1): `procstat::{Sampler, RawSample, cores_from_deltas}`.
- Produces:
  - `pub struct TimePoint { pub t_secs: f64, pub cum_requests: u64, pub cum_accepted: u64, pub cum_failed: u64, pub interval_rate: f64, pub interval_accepted: u64, pub interval_failed: u64, pub cpu_cores: Option<f64>, pub rss_bytes: Option<u64> }`
  - `Summary` gains `pub timeline: Vec<TimePoint>`.
  - `pub async fn aggregate(rx, users, start, target_pid: Option<u32>) -> Summary` (new final param).

- [ ] **Step 1: Write the failing test for `make_timepoint`**

Add to the `#[cfg(test)] mod tests` block in `bins/crebain/src/metrics.rs`:
```rust
    #[test]
    fn timepoint_computes_interval_deltas_and_resources() {
        // cumulative: 500 req / 480 ok / 20 fail; previous tick was 300/290/10.
        let tp = make_timepoint(
            Duration::from_secs(2),
            500, 480, 20,
            300, 290, 10,
            Duration::from_secs(1),
            Some(0.8),
            Some(64 * 1024 * 1024),
        );
        assert_eq!(tp.t_secs, 2.0);
        assert_eq!(tp.cum_requests, 500);
        assert_eq!(tp.interval_rate, 200.0);      // (500-300)/1s
        assert_eq!(tp.interval_accepted, 190);    // 480-290
        assert_eq!(tp.interval_failed, 10);       // 20-10
        assert_eq!(tp.cpu_cores, Some(0.8));
        assert_eq!(tp.rss_bytes, Some(64 * 1024 * 1024));
    }

    #[test]
    fn timepoint_without_resources_is_none() {
        let tp = make_timepoint(
            Duration::from_secs(1), 100, 100, 0, 0, 0, 0,
            Duration::from_secs(1), None, None,
        );
        assert_eq!(tp.cpu_cores, None);
        assert_eq!(tp.rss_bytes, None);
        assert_eq!(tp.interval_rate, 100.0);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p crebain metrics`
Expected: FAIL — `cannot find function make_timepoint`, and the existing `finalize()` calls will still compile for now.

- [ ] **Step 3: Add `TimePoint`, the `Summary` field, and `make_timepoint`**

In `bins/crebain/src/metrics.rs`, add the imports near the top (after the existing `use` lines):
```rust
use crate::procstat::{self, Sampler};
```

Add the `TimePoint` struct just above `pub struct Summary`:
```rust
/// One per-second row of the run timeline, retained for the HTML report.
pub struct TimePoint {
    pub t_secs: f64,
    pub cum_requests: u64,
    pub cum_accepted: u64,
    pub cum_failed: u64,
    pub interval_rate: f64,
    pub interval_accepted: u64,
    pub interval_failed: u64,
    /// `None` on the first tick (no prior CPU sample) or when no PID is sampled.
    pub cpu_cores: Option<f64>,
    /// `None` when no PID is sampled (non-Linux / direct mode).
    pub rss_bytes: Option<u64>,
}
```

Add the `timeline` field to `Summary` (append after `pub latency_truncated: bool,`):
```rust
    pub latency_truncated: bool,
    pub timeline: Vec<TimePoint>,
```

Add the pure builder as a free function (place it just above `pub async fn aggregate`):
```rust
/// Build one timeline row from current cumulative counters and the prior tick's
/// counters. Pure and total (no divide-by-zero: `interval` is always ≥ the 1s
/// tick). Resource values are passed in already-resolved so this stays testable.
pub(crate) fn make_timepoint(
    elapsed: Duration,
    cum_requests: u64,
    cum_accepted: u64,
    cum_failed: u64,
    prev_requests: u64,
    prev_accepted: u64,
    prev_failed: u64,
    interval: Duration,
    cpu_cores: Option<f64>,
    rss_bytes: Option<u64>,
) -> TimePoint {
    let secs = interval.as_secs_f64().max(1e-9);
    TimePoint {
        t_secs: elapsed.as_secs_f64(),
        cum_requests,
        cum_accepted,
        cum_failed,
        interval_rate: (cum_requests - prev_requests) as f64 / secs,
        interval_accepted: cum_accepted - prev_accepted,
        interval_failed: cum_failed - prev_failed,
        cpu_cores,
        rss_bytes,
    }
}
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p crebain metrics::tests::timepoint`
Expected: PASS (2 tests). The crate will NOT fully build yet — `finalize` and `aggregate` are updated next; that's expected mid-task.

- [ ] **Step 5: Thread the timeline through `finalize` and `aggregate`**

Change `finalize` to accept the collected timeline. Its signature line becomes:
```rust
    fn finalize(mut self, timeline: Vec<TimePoint>) -> Summary {
```
and add `timeline,` to the returned `Summary { ... }` literal (after `accepted_items: self.accepted_items.into_counts(),`):
```rust
            accepted_items: self.accepted_items.into_counts(),
            timeline,
        }
```

Replace the whole `pub async fn aggregate(...)` body with the resource-sampling version:
```rust
pub async fn aggregate(
    mut rx: UnboundedReceiver<Sample>,
    users: usize,
    start: Instant,
    target_pid: Option<u32>,
) -> Summary {
    let interval_dur = Duration::from_secs(1);
    let mut metrics = Metrics::new(users, start);
    let mut ticker = tokio::time::interval(interval_dur);
    ticker.tick().await; // consume the immediate first tick
    let mut prev_requests = 0u64;
    let mut prev_accepted = 0u64;
    let mut prev_failed = 0u64;

    let sampler = target_pid.map(Sampler::new);
    let mut prev_proc: Option<u64> = None;
    let mut prev_total: Option<u64> = None;
    let mut timeline: Vec<TimePoint> = Vec::new();

    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(sample) => metrics.record(sample),
                None => break, // all user tasks finished
            },
            _ = ticker.tick() => {
                let snap = metrics.snapshot(prev_requests, interval_dur);
                crate::report::live_line(&snap);

                // Resolve this tick's resource sample (best-effort).
                let (cpu_cores, rss_bytes) = match sampler.as_ref().and_then(|s| s.sample().map(|r| (s, r))) {
                    Some((s, raw)) => {
                        let cpu = match (prev_proc, prev_total) {
                            (Some(pp), Some(pt)) => Some(procstat::cores_from_deltas(
                                raw.proc_jiffies.saturating_sub(pp),
                                raw.total_jiffies.saturating_sub(pt),
                                s.ncpus(),
                            )),
                            _ => None, // first sample: no prior to delta against
                        };
                        prev_proc = Some(raw.proc_jiffies);
                        prev_total = Some(raw.total_jiffies);
                        (cpu, Some(raw.rss_bytes))
                    }
                    None => (None, None),
                };

                timeline.push(make_timepoint(
                    start.elapsed(),
                    metrics.requests, metrics.accepted, metrics.failed(),
                    prev_requests, prev_accepted, prev_failed,
                    interval_dur, cpu_cores, rss_bytes,
                ));
                prev_requests = metrics.requests;
                prev_accepted = metrics.accepted;
                prev_failed = metrics.failed();
            }
        }
    }
    crate::report::clear_live_line();
    metrics.finalize(timeline)
}
```

- [ ] **Step 6: Fix the existing `finalize()` call sites in tests**

Two existing tests call `m.finalize()`. Update both to `m.finalize(vec![])`:
- `percentiles_are_nearest_rank` does not call finalize — skip.
- `records_outcomes_and_items`: change `let s = m.finalize();` to `let s = m.finalize(vec![]);`.

- [ ] **Step 7: Run the full crate tests**

Run: `cargo test -p crebain`
Expected: metrics tests PASS. NOTE: the crate will fail to build at the `engine.rs` call to `metrics::aggregate` (arity changed). That is fixed in Task 3 — if you are running tasks strictly in order, it is acceptable for `cargo build` to fail here on the `engine.rs` call site only. To keep this task self-contained and green, apply the one-line `engine.rs` edit from Task 3 Step 1 now, then re-run.

- [ ] **Step 8: Checkpoint**

After applying Task 3 Step 1's one-liner, `cargo test -p crebain` is green. Do not commit.

---

### Task 3: PID plumbing (harness → engine → aggregator)

**Files:**
- Modify: `bins/crebain/src/engine.rs`
- Modify: `bins/crebain/src/harness.rs`
- Modify: `bins/crebain/src/main.rs`

**Interfaces:**
- Consumes: `metrics::aggregate(rx, users, start, target_pid)` (Task 2).
- Produces:
  - `pub async fn run(cfg: &RunConfig, target: &Target, target_pid: Option<u32>) -> anyhow::Result<Summary>`
  - `HarnessGuard::child_pid(&self) -> Option<u32>`

- [ ] **Step 1: Update `engine::run` to accept and forward the PID**

In `bins/crebain/src/engine.rs`, change the signature:
```rust
pub async fn run(cfg: &RunConfig, target: &Target, target_pid: Option<u32>) -> anyhow::Result<Summary> {
```
and the aggregator spawn line:
```rust
    let aggregator = tokio::spawn(metrics::aggregate(rx, cfg.users, start, target_pid));
```

- [ ] **Step 2: Add the `child_pid` accessor to the harness guard**

In `bins/crebain/src/harness.rs`, add a method inside `impl HarnessGuard` (just above `pub async fn teardown`):
```rust
    /// PID of the spawned ingest child, if it is running. Used to sample the
    /// server's CPU/RAM during the run. `None` before spawn or after teardown.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }
```

- [ ] **Step 3: Pass the PID at both call sites in `main.rs`**

In `bins/crebain/src/main.rs`, the direct-mode call in `run()` becomes:
```rust
            finish(run_with_signals(engine::run(&cfg, &target, None)).await, &cfg, "direct")
```
(the `mode_label` third arg is added in Task 5; if implementing strictly in order, temporarily call `finish(run_with_signals(engine::run(&cfg, &target, None)).await, &cfg)` and add the label in Task 5.)

In `isolated_body()`, capture the PID after provisioning succeeds and pass it in. Replace the `Ok(Some(target)) => { ... }` arm's inner block:
```rust
        Ok(Some(target)) => {
            print_target(&target);
            let target_pid = guard.child_pid();
            // engine::run is safe to cancel mid-flight (it just aborts user tasks).
            tokio::select! {
                r = engine::run(cfg, &target, target_pid) => Some(r),
                _ = cancel.changed() => None,
            }
        }
```

- [ ] **Step 4: Build and test**

Run: `cargo test -p crebain`
Expected: PASS, crate builds. (If Task 5 is not yet done, keep `finish(..., &cfg)` two-arg form.)

- [ ] **Step 5: Checkpoint**

Run: `cargo build -p crebain`
Expected: clean build. Do not commit.

---

### Task 4: `report_html` — inline-SVG HTML rendering

**Files:**
- Create: `bins/crebain/src/report_html.rs`
- Modify: `bins/crebain/src/main.rs` (add `mod report_html;`)

**Interfaces:**
- Consumes: `metrics::{Summary, TimePoint}`, `cli::Expected`.
- Produces:
  - `pub struct ReportMeta { pub mode_label: String, pub users: usize, pub duration_secs: u64, pub events_per_min: u32, pub issues_per_min: u32, pub gzip: bool, pub generated_at: String, pub ncpus: usize }`
  - `pub fn render(summary: &Summary, expected: &Expected, meta: &ReportMeta) -> String`
  - `pub fn write(path: &std::path::Path, summary: &Summary, expected: &Expected, meta: &ReportMeta) -> anyhow::Result<()>`

- [ ] **Step 1: Add the module declaration**

In `bins/crebain/src/main.rs` module list, add `mod report_html;` (after `mod report;`):
```rust
mod report;
mod report_html;
mod user;
```

- [ ] **Step 2: Write the failing tests**

Create `bins/crebain/src/report_html.rs` with the tests first:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::ItemCounts;
    use crate::metrics::{Summary, TimePoint};
    use std::time::Duration;

    fn tp(t: f64, req: u64, ok: u64, fail: u64, cpu: Option<f64>, rss: Option<u64>) -> TimePoint {
        TimePoint {
            t_secs: t, cum_requests: req, cum_accepted: ok, cum_failed: fail,
            interval_rate: req as f64, interval_accepted: ok, interval_failed: fail,
            cpu_cores: cpu, rss_bytes: rss,
        }
    }

    fn summary(timeline: Vec<TimePoint>) -> Summary {
        Summary {
            elapsed: Duration::from_secs(3), users: 10, requests: 900, accepted: 880,
            rate_limited: 5, http_errors: 10, transport: 5,
            status_counts: vec![(202, 880), (429, 5)],
            attempted: ItemCounts { events: 500, errors: 400, ..Default::default() },
            accepted_items: ItemCounts { events: 490, errors: 390, ..Default::default() },
            p50_us: 1200, p90_us: 4500, p99_us: 9000, max_us: 25000,
            latency_samples: 900, latency_truncated: false, timeline,
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            mode_label: "isolated".into(), users: 10, duration_secs: 3,
            events_per_min: 10, issues_per_min: 10, gzip: true,
            generated_at: "2026-07-15 00:00:00 UTC".into(), ncpus: 8,
        }
    }

    #[test]
    fn maps_values_into_plot_box() {
        // y: 0 → bottom(100), max → top(0)
        assert!((map_y(0.0, 10.0, 0.0, 100.0) - 100.0).abs() < 1e-9);
        assert!((map_y(10.0, 10.0, 0.0, 100.0) - 0.0).abs() < 1e-9);
        // degenerate y_max → clamp to bottom (no NaN)
        assert!((map_y(5.0, 0.0, 0.0, 100.0) - 100.0).abs() < 1e-9);
        // x: 0 → left, x_max → right
        assert!((map_x(0.0, 4.0, 10.0, 90.0) - 10.0).abs() < 1e-9);
        assert!((map_x(4.0, 4.0, 10.0, 90.0) - 90.0).abs() < 1e-9);
        assert!((map_x(1.0, 0.0, 10.0, 90.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn renders_headline_numbers_and_all_charts_when_resources_present() {
        let s = summary(vec![
            tp(1.0, 300, 290, 10, Some(0.5), Some(50 * 1024 * 1024)),
            tp(2.0, 600, 585, 15, Some(0.9), Some(60 * 1024 * 1024)),
            tp(3.0, 900, 880, 20, Some(1.1), Some(64 * 1024 * 1024)),
        ]);
        let html = render(&s, &crate::cli::Expected { requests: 1000.0, duration_secs: 3.0 }, &meta());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("crebain"));
        assert!(html.contains("Requests / sec"));
        assert!(html.contains("CPU (cores)"));
        assert!(html.contains("Memory (RSS)"));
        assert!(html.contains("880")); // accepted total appears
        assert!(html.contains("<svg"));
    }

    #[test]
    fn omits_resource_charts_without_samples() {
        let s = summary(vec![
            tp(1.0, 300, 295, 5, None, None),
            tp(2.0, 600, 590, 10, None, None),
        ]);
        let html = render(&s, &crate::cli::Expected { requests: 1000.0, duration_secs: 3.0 }, &meta());
        assert!(html.contains("Requests / sec"));
        assert!(!html.contains("CPU (cores)"));
        assert!(!html.contains("Memory (RSS)"));
    }

    #[test]
    fn empty_timeline_still_renders() {
        let s = summary(vec![]);
        let html = render(&s, &crate::cli::Expected { requests: 0.0, duration_secs: 3.0 }, &meta());
        assert!(html.contains("no data"));
        assert!(html.starts_with("<!doctype html>"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p crebain report_html`
Expected: FAIL — `map_y`, `map_x`, `render`, `ReportMeta` not found.

- [ ] **Step 4: Implement the renderer**

Prepend to `bins/crebain/src/report_html.rs` (above the tests):
```rust
//! Renders a run's [`Summary`] + timeline to a single self-contained HTML file:
//! inline CSS, hand-drawn inline SVG line charts, native `<title>` hover
//! tooltips, zero JS and zero network requests.

use std::path::Path;

use crate::cli::Expected;
use crate::generator::ItemCounts;
use crate::metrics::{Summary, TimePoint};

/// Run context for the report header (everything not on `Summary`).
pub struct ReportMeta {
    pub mode_label: String,
    pub users: usize,
    pub duration_secs: u64,
    pub events_per_min: u32,
    pub issues_per_min: u32,
    pub gzip: bool,
    pub generated_at: String,
    pub ncpus: usize,
}

// Plot geometry (SVG user units). viewBox is 0 0 720 260; the plot area is inset
// for axis labels.
const VB_W: f64 = 720.0;
const VB_H: f64 = 260.0;
const PLOT_LEFT: f64 = 56.0;
const PLOT_RIGHT: f64 = 704.0;
const PLOT_TOP: f64 = 16.0;
const PLOT_BOTTOM: f64 = 220.0;

/// Map a time value (0..=x_max) to an x pixel; x_max ≤ 0 pins to the left edge.
fn map_x(t: f64, x_max: f64, left: f64, right: f64) -> f64 {
    if x_max <= 0.0 {
        left
    } else {
        left + (t / x_max) * (right - left)
    }
}

/// Map a value (0..=y_max) to a y pixel (0 → bottom, y_max → top); y_max ≤ 0
/// pins to the bottom edge (avoids NaN on a flat-zero series).
fn map_y(v: f64, y_max: f64, top: f64, bottom: f64) -> f64 {
    if y_max <= 0.0 {
        bottom
    } else {
        bottom - (v / y_max) * (bottom - top)
    }
}

struct Series<'a> {
    name: &'a str,
    color: &'a str,
    /// (t_secs, value)
    points: Vec<(f64, f64)>,
}

/// A labelled, gridded SVG line chart. `fmt_y` formats a y value for the axis
/// labels and tooltips (e.g. cores, MB, req/s). Returns a `<figure>…</figure>`.
fn line_chart(title: &str, x_max: f64, series: &[Series], fmt_y: &dyn Fn(f64) -> String) -> String {
    let has_points = series.iter().any(|s| !s.points.is_empty());
    if !has_points {
        return format!(
            "<figure class=\"chart\"><figcaption>{title}</figcaption>\
             <div class=\"nodata\">no data</div></figure>"
        );
    }
    let y_max_raw = series
        .iter()
        .flat_map(|s| s.points.iter().map(|&(_, v)| v))
        .fold(0.0_f64, f64::max);
    let y_max = if y_max_raw <= 0.0 { 1.0 } else { y_max_raw * 1.1 };

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg viewBox=\"0 0 {VB_W} {VB_H}\" preserveAspectRatio=\"xMidYMid meet\" \
         role=\"img\" aria-label=\"{title}\">"
    ));
    // Horizontal gridlines + y labels (5 rows).
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = PLOT_BOTTOM - frac * (PLOT_BOTTOM - PLOT_TOP);
        let val = frac * y_max;
        svg.push_str(&format!(
            "<line class=\"grid\" x1=\"{PLOT_LEFT}\" y1=\"{y:.1}\" x2=\"{PLOT_RIGHT}\" y2=\"{y:.1}\"/>\
             <text class=\"ylab\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
            PLOT_LEFT - 6.0,
            y + 3.0,
            fmt_y(val),
        ));
    }
    // X axis labels: 0 and x_max.
    svg.push_str(&format!(
        "<text class=\"xlab\" x=\"{PLOT_LEFT}\" y=\"{:.1}\">0s</text>\
         <text class=\"xlab xend\" x=\"{PLOT_RIGHT}\" y=\"{:.1}\">{:.0}s</text>",
        PLOT_BOTTOM + 18.0,
        PLOT_BOTTOM + 18.0,
        x_max,
    ));
    // One polyline + hover dots per series.
    for s in series {
        if s.points.is_empty() {
            continue;
        }
        let pts = s
            .points
            .iter()
            .map(|&(t, v)| {
                format!(
                    "{:.1},{:.1}",
                    map_x(t, x_max, PLOT_LEFT, PLOT_RIGHT),
                    map_y(v, y_max, PLOT_TOP, PLOT_BOTTOM)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"2\" points=\"{pts}\"/>",
            s.color
        ));
        for &(t, v) in &s.points {
            svg.push_str(&format!(
                "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.5\" fill=\"{}\">\
                 <title>t={:.0}s · {}: {}</title></circle>",
                map_x(t, x_max, PLOT_LEFT, PLOT_RIGHT),
                map_y(v, y_max, PLOT_TOP, PLOT_BOTTOM),
                s.color,
                t,
                s.name,
                fmt_y(v),
            ));
        }
    }
    svg.push_str("</svg>");

    let legend = series
        .iter()
        .map(|s| {
            format!(
                "<span class=\"lg\"><i style=\"background:{}\"></i>{}</span>",
                s.color,
                esc(s.name)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        "<figure class=\"chart\"><figcaption>{title}</figcaption>{svg}\
         <div class=\"legend\">{legend}</div></figure>"
    )
}

/// Minimal HTML text escaping for the few dynamic strings we interpolate.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

fn fmt_us(us: u64) -> String {
    if us < 1000 {
        format!("{us}us")
    } else {
        format!("{:.2}ms", us as f64 / 1000.0)
    }
}

fn item_total(c: &ItemCounts) -> u64 {
    c.errors + c.events + c.identifies + c.transactions + c.breadcrumbs
}

fn stat_card(label: &str, value: &str) -> String {
    format!("<div class=\"card\"><div class=\"v\">{value}</div><div class=\"l\">{label}</div></div>")
}

/// Build the complete HTML document.
pub fn render(summary: &Summary, expected: &Expected, meta: &ReportMeta) -> String {
    let s = summary;
    let secs = s.elapsed.as_secs_f64().max(1e-9);
    let achieved_rps = s.requests as f64 / secs;
    let target_rps = expected.requests / expected.duration_secs.max(1e-9);
    let accept_pct = if s.requests == 0 {
        "—".to_string()
    } else {
        format!("{:.1}%", 100.0 * s.accepted as f64 / s.requests as f64)
    };
    let failed = s.rate_limited + s.http_errors + s.transport;

    let has_resources = s.timeline.iter().any(|t| t.cpu_cores.is_some() || t.rss_bytes.is_some());
    let peak_cpu = s
        .timeline
        .iter()
        .filter_map(|t| t.cpu_cores)
        .fold(0.0_f64, f64::max);
    let peak_rss = s.timeline.iter().filter_map(|t| t.rss_bytes).max().unwrap_or(0);
    let x_max = s.timeline.last().map(|t| t.t_secs).unwrap_or(0.0);

    // ---- charts ----
    let rps_chart = line_chart(
        "Requests / sec",
        x_max,
        &[Series {
            name: "req/s",
            color: "#4f8cff",
            points: s.timeline.iter().map(|t| (t.t_secs, t.interval_rate)).collect(),
        }],
        &|v| format!("{v:.0}"),
    );
    let cum_chart = line_chart(
        "Records — cumulative (success vs fail)",
        x_max,
        &[
            Series {
                name: "accepted",
                color: "#2ecc71",
                points: s.timeline.iter().map(|t| (t.t_secs, t.cum_accepted as f64)).collect(),
            },
            Series {
                name: "failed",
                color: "#e74c3c",
                points: s.timeline.iter().map(|t| (t.t_secs, t.cum_failed as f64)).collect(),
            },
        ],
        &|v| group(v as u64),
    );
    let interval_chart = line_chart(
        "Records / sec (success vs fail)",
        x_max,
        &[
            Series {
                name: "accepted/s",
                color: "#2ecc71",
                points: s.timeline.iter().map(|t| (t.t_secs, t.interval_accepted as f64)).collect(),
            },
            Series {
                name: "failed/s",
                color: "#e74c3c",
                points: s.timeline.iter().map(|t| (t.t_secs, t.interval_failed as f64)).collect(),
            },
        ],
        &|v| format!("{v:.0}"),
    );

    let mut resource_charts = String::new();
    if has_resources {
        resource_charts.push_str(&line_chart(
            "CPU (cores)",
            x_max,
            &[Series {
                name: "cores",
                color: "#f39c12",
                points: s
                    .timeline
                    .iter()
                    .filter_map(|t| t.cpu_cores.map(|c| (t.t_secs, c)))
                    .collect(),
            }],
            &|v| format!("{v:.2}"),
        ));
        resource_charts.push_str(&line_chart(
            "Memory (RSS)",
            x_max,
            &[Series {
                name: "RSS MB",
                color: "#9b59b6",
                points: s
                    .timeline
                    .iter()
                    .filter_map(|t| t.rss_bytes.map(|b| (t.t_secs, b as f64 / 1_048_576.0)))
                    .collect(),
            }],
            &|v| format!("{v:.0}"),
        ));
    }

    // ---- stat cards ----
    let mut cards = String::new();
    cards.push_str(&stat_card("total requests", &group(s.requests)));
    cards.push_str(&stat_card(
        "req/s achieved",
        &format!("{}<span class=\"sub\"> / {} target</span>", group(achieved_rps.round() as u64), group(target_rps.round() as u64)),
    ));
    cards.push_str(&stat_card("accepted", &group(s.accepted)));
    cards.push_str(&stat_card("failed", &group(failed)));
    cards.push_str(&stat_card("accept rate", &accept_pct));
    cards.push_str(&stat_card("latency p50", &fmt_us(s.p50_us)));
    cards.push_str(&stat_card("latency p90", &fmt_us(s.p90_us)));
    cards.push_str(&stat_card("latency p99", &fmt_us(s.p99_us)));
    cards.push_str(&stat_card("latency max", &fmt_us(s.max_us)));
    if has_resources {
        cards.push_str(&stat_card(
            "peak CPU",
            &format!("{peak_cpu:.2}<span class=\"sub\"> / {} cores</span>", meta.ncpus),
        ));
        cards.push_str(&stat_card("peak RSS", &format!("{} MB", peak_rss / 1_048_576)));
    }

    // ---- tables ----
    let pct = |num: u64, den: u64| {
        if den == 0 {
            "—".to_string()
        } else {
            format!("{:.1}%", 100.0 * num as f64 / den as f64)
        }
    };
    let outcomes = format!(
        "<table><thead><tr><th>outcome</th><th>count</th><th>share</th></tr></thead><tbody>\
         <tr><td>accepted (2xx)</td><td>{}</td><td>{}</td></tr>\
         <tr><td>rate-limited</td><td>{}</td><td>{}</td></tr>\
         <tr><td>http errors</td><td>{}</td><td>{}</td></tr>\
         <tr><td>transport errors</td><td>{}</td><td>{}</td></tr>\
         </tbody></table>",
        group(s.accepted), pct(s.accepted, s.requests),
        group(s.rate_limited), pct(s.rate_limited, s.requests),
        group(s.http_errors), pct(s.http_errors, s.requests),
        group(s.transport), pct(s.transport, s.requests),
    );
    let item_row = |label: &str, a: u64, at: u64| {
        format!("<tr><td>{label}</td><td>{}</td><td>{}</td></tr>", group(a), group(at))
    };
    let items = format!(
        "<table><thead><tr><th>signal</th><th>accepted</th><th>attempted</th></tr></thead><tbody>\
         {}{}{}{}{}{}</tbody></table>",
        item_row("errors", s.accepted_items.errors, s.attempted.errors),
        item_row("events", s.accepted_items.events, s.attempted.events),
        item_row("transactions", s.accepted_items.transactions, s.attempted.transactions),
        item_row("identifies", s.accepted_items.identifies, s.attempted.identifies),
        item_row("breadcrumbs", s.accepted_items.breadcrumbs, s.attempted.breadcrumbs),
        item_row("total", item_total(&s.accepted_items), item_total(&s.attempted)),
    );
    let status_codes = if s.status_counts.is_empty() {
        String::new()
    } else {
        let codes = s
            .status_counts
            .iter()
            .map(|(c, n)| format!("<code>{c}</code>×{}", group(*n)))
            .collect::<Vec<_>>()
            .join(" &nbsp; ");
        format!("<p class=\"codes\">status codes: {codes}</p>")
    };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>crebain report — {mode}</title><style>{css}</style></head><body>\
         <header><h1>crebain <span>benchmark report</span></h1>\
         <p class=\"meta\">{mode} · {users} users · {dur}s · events {epm}/min · issues {ipm}/min · gzip {gz} · {gen}</p>\
         </header>\
         <section class=\"cards\">{cards}</section>\
         <section class=\"charts\">{rps}{cum}{interval}{resources}</section>\
         <section class=\"tables\"><div><h2>Outcomes</h2>{outcomes}{codes}</div>\
         <div><h2>Items (accepted / attempted)</h2>{items}</div></section>\
         <footer>Generated by crebain. CPU sampled once/sec from /proc; 1.0 core = one full CPU. Charts are static SVG — hover a point for its value.</footer>\
         </body></html>",
        mode = esc(&meta.mode_label),
        css = CSS,
        users = meta.users,
        dur = meta.duration_secs,
        epm = meta.events_per_min,
        ipm = meta.issues_per_min,
        gz = if meta.gzip { "on" } else { "off" },
        gen = esc(&meta.generated_at),
        cards = cards,
        rps = rps_chart,
        cum = cum_chart,
        interval = interval_chart,
        resources = resource_charts,
        outcomes = outcomes,
        codes = status_codes,
        items = items,
    )
}

/// Render and write the report to `path`.
pub fn write(
    path: &Path,
    summary: &Summary,
    expected: &Expected,
    meta: &ReportMeta,
) -> anyhow::Result<()> {
    let html = render(summary, expected, meta);
    std::fs::write(path, html)
        .map_err(|e| anyhow::anyhow!("write report {}: {e}", path.display()))
}

const CSS: &str = "\
:root{--bg:#f7f8fa;--fg:#1a1c20;--mut:#666;--card:#fff;--bd:#e3e6ea;--grid:#e8ebef}\
@media(prefers-color-scheme:dark){:root{--bg:#14161a;--fg:#e6e8 eb;--mut:#9aa0a8;--card:#1d2026;--bd:#2a2e36;--grid:#262a31}}\
*{box-sizing:border-box}body{margin:0;padding:24px;font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg)}\
header h1{margin:0;font-size:22px}header h1 span{color:var(--mut);font-weight:400}\
.meta{color:var(--mut);margin:4px 0 20px}\
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px;margin-bottom:24px}\
.card{background:var(--card);border:1px solid var(--bd);border-radius:10px;padding:14px}\
.card .v{font-size:22px;font-weight:600}.card .v .sub{font-size:13px;color:var(--mut);font-weight:400}\
.card .l{color:var(--mut);font-size:12px;margin-top:2px;text-transform:uppercase;letter-spacing:.04em}\
.charts{display:grid;grid-template-columns:repeat(auto-fit,minmax(340px,1fr));gap:16px;margin-bottom:24px}\
.chart{background:var(--card);border:1px solid var(--bd);border-radius:10px;padding:14px;margin:0}\
.chart figcaption{font-weight:600;margin-bottom:8px}\
.chart svg{width:100%;height:auto}\
.nodata{color:var(--mut);padding:40px;text-align:center}\
.grid{stroke:var(--grid);stroke-width:1}.ylab{fill:var(--mut);font-size:10px;text-anchor:end}\
.xlab{fill:var(--mut);font-size:10px}.xend{text-anchor:end}\
.legend{margin-top:8px;color:var(--mut);font-size:12px}\
.legend .lg{margin-right:14px}.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:4px;vertical-align:middle}\
.tables{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:16px}\
.tables h2{font-size:15px;margin:0 0 8px}table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--bd);border-radius:10px;overflow:hidden}\
th,td{text-align:left;padding:8px 12px;border-bottom:1px solid var(--bd)}th{color:var(--mut);font-size:12px;text-transform:uppercase;letter-spacing:.04em}\
tr:last-child td{border-bottom:none}td:nth-child(n+2),th:nth-child(n+2){text-align:right;font-variant-numeric:tabular-nums}\
.codes{color:var(--mut);font-size:12px}.codes code{background:var(--bd);padding:1px 5px;border-radius:4px}\
footer{color:var(--mut);font-size:12px;margin-top:24px;border-top:1px solid var(--bd);padding-top:12px}";
```

Note: there is a deliberate typo guard — after pasting, verify the two CSS custom
properties `--fg:#e6e8 eb` and `--mut` inside the dark block have no stray space:
they must read `--fg:#e6e8eb`. (Kept explicit because a stray space silently
breaks that one variable.)

- [ ] **Step 5: Fix the CSS typo flagged above**

Open `bins/crebain/src/report_html.rs`, find `--fg:#e6e8 eb` in the dark-mode block and change it to `--fg:#e6e8eb` (remove the space).

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p crebain report_html`
Expected: PASS (4 tests).

- [ ] **Step 7: Checkpoint**

Run: `cargo build -p crebain`
Expected: clean (a `write`/`render` dead_code warning is fine until Task 5). Do not commit.

---

### Task 5: `--report` flag, `finish()` wiring, README

**Files:**
- Modify: `bins/crebain/src/cli.rs`
- Modify: `bins/crebain/src/main.rs`
- Modify: `bins/crebain/README.md`

**Interfaces:**
- Consumes: `report_html::{ReportMeta, write}` (Task 4), `RunConfig.report_path`.
- Produces: `RunConfig` gains `pub report_path: Option<std::path::PathBuf>`; `Args` gains `--report`.

- [ ] **Step 1: Write the failing CLI test**

Add to the `#[cfg(test)] mod tests` in `bins/crebain/src/cli.rs`:
```rust
    #[test]
    fn report_path_flows_into_runconfig() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y", "--report", "out.html",
        ])
        .unwrap();
        let (cfg, _mode) = args.resolve().unwrap();
        assert_eq!(cfg.report_path, Some(std::path::PathBuf::from("out.html")));
    }

    #[test]
    fn report_path_defaults_to_none() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y",
        ])
        .unwrap();
        let (cfg, _mode) = args.resolve().unwrap();
        assert_eq!(cfg.report_path, None);
    }
```
Add `use clap::Parser;` to the test module if not already imported (the existing tests may not need it). At the top of `mod tests`, ensure `use super::*;` is present (it is).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p crebain cli::tests::report`
Expected: FAIL — `Args` has no `report` field / `RunConfig` has no `report_path`.

- [ ] **Step 3: Add the flag and config field**

In `bins/crebain/src/cli.rs`, add to `struct Args` (after the `--no-gzip` flag, before the isolated-mode options):
```rust
    /// Write a self-contained HTML benchmark report to this path.
    #[arg(long)]
    pub report: Option<std::path::PathBuf>,
```

Add to `struct RunConfig` (after `pub issues_per_min: u32,`):
```rust
    pub report_path: Option<std::path::PathBuf>,
```

In `resolve()`, the `let cfg = RunConfig { ... }` literal gains a field. Add `report_path: self.report,` — but note `self.report` is moved after other `self` fields are read; place it in the struct literal:
```rust
        let cfg = RunConfig {
            users: self.users,
            duration: Duration::from_secs(self.duration),
            event_interval: interval_from_rate(self.events_per_min),
            issue_interval: interval_from_rate(self.issues_per_min),
            gzip: !self.no_gzip,
            events_per_min: self.events_per_min,
            issues_per_min: self.issues_per_min,
            report_path: self.report,
        };
```

The two existing `RunConfig { ... }` literals in `cli.rs` tests (`expected_requests_matches_model`) must also gain `report_path: None,`. Add it after `issues_per_min: 10,` in that test.

- [ ] **Step 4: Run the CLI tests**

Run: `cargo test -p crebain cli`
Expected: PASS.

- [ ] **Step 5: Wire report writing into `finish()` with a mode label**

In `bins/crebain/src/main.rs`, add the new module's imports at the top (with the other `use` lines):
```rust
use report_html::ReportMeta;
```

Change `finish` to take a `mode_label` and write the report. Replace the whole `fn finish(...)`:
```rust
/// Turn the outcome of a run into an exit code. `None` means the run was
/// interrupted (Ctrl-C / SIGTERM) before it produced a summary.
fn finish(ran: Option<Result<Summary>>, cfg: &RunConfig, mode_label: &str) -> Result<ExitCode> {
    match ran {
        Some(Ok(summary)) => {
            report::print_summary(&summary, &cfg.expected());
            if let Some(path) = &cfg.report_path {
                let meta = ReportMeta {
                    mode_label: mode_label.to_string(),
                    users: cfg.users,
                    duration_secs: cfg.duration.as_secs(),
                    events_per_min: cfg.events_per_min,
                    issues_per_min: cfg.issues_per_min,
                    gzip: cfg.gzip,
                    generated_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                    ncpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
                };
                match report_html::write(path, &summary, &cfg.expected(), &meta) {
                    Ok(()) => eprintln!("crebain: wrote report to {}", path.display()),
                    Err(e) => eprintln!("crebain: WARNING failed to write report: {e:#}"),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Err(e)) => Err(e),
        None => {
            eprintln!("crebain: interrupted");
            Ok(ExitCode::from(130))
        }
    }
}
```

Update the two `finish(...)` call sites to pass a label:
- In `run()` (direct mode):
```rust
            finish(run_with_signals(engine::run(&cfg, &target, None)).await, &cfg, "direct")
```
- In `run_isolated()`:
```rust
    finish(ran, cfg, "isolated (ephemeral, self-cleaning)")
```

- [ ] **Step 6: Build and run the full suite**

Run: `cargo test -p crebain && cargo build -p crebain`
Expected: all tests PASS, clean build.

- [ ] **Step 7: Document the flag in the README**

In `bins/crebain/README.md`, add a short subsection (place it after the flags/usage section; match the file's existing heading style):
```markdown
## HTML report

Pass `--report <path>` to write a self-contained HTML report (opens offline, no
network) alongside the text summary:

```
crebain --isolated --database-url "$DATABASE_URL" --duration 60 --report bench.html
```

The report charts requests/sec and success/fail records (cumulative and
per-second) over the run. In `--isolated` mode it also charts the ingest
server's CPU (in cores) and RSS memory over time, sampled once per second from
`/proc`. In `--dsn` mode there is no server process to sample, so the CPU/RAM
charts are omitted.
```

- [ ] **Step 8: Checkpoint**

Run: `cargo test -p crebain`
Expected: green. Do not commit.

---

### Task 6: End-to-end verification

**Files:** none (verification only).

- [ ] **Step 1: Deterministic report render (no infra needed)**

The `report_html` unit tests already render full HTML from synthetic timelines
(with and without resources). Confirm they pass and produce a real file by
adding a temporary throwaway check, OR rely on the existing tests plus this
manual render. Preferred: run the whole suite and clippy:

Run: `cargo test -p crebain && cargo clippy -p crebain -- -D warnings`
Expected: tests PASS; clippy clean (fix any lint inline).

- [ ] **Step 2: Live isolated run if Postgres + Redis are available**

Build the ingest and run a short benchmark writing a report:
```bash
cargo build -p sauron-ingest -p crebain
./target/debug/crebain --isolated \
  --database-url "$DATABASE_URL" \
  --duration 8 --users 200 --events-per-min 60 --issues-per-min 60 \
  --report /tmp/crebain-report.html
```
Expected: run completes, prints `crebain: wrote report to /tmp/crebain-report.html`, teardown drops the bench DB.

- [ ] **Step 3: Verify the report contents**

```bash
grep -c "<svg" /tmp/crebain-report.html      # expect 5 (rps, cum, interval, cpu, rss)
grep -o "CPU (cores)" /tmp/crebain-report.html
grep -o "Memory (RSS)" /tmp/crebain-report.html
```
Open `/tmp/crebain-report.html` in a browser (or the preview tooling) and confirm:
the five charts render, CPU/RSS lines are non-empty, stat cards show peak CPU and
peak RSS, and light/dark themes both look right.

- [ ] **Step 4: Verify direct mode omits resource charts**

If a dev ingest/DSN is available, run with `--dsn <DSN> --duration 5 --report /tmp/crebain-direct.html` and confirm:
```bash
grep -c "<svg" /tmp/crebain-direct.html       # expect 3 (no cpu/rss)
grep -c "CPU (cores)" /tmp/crebain-direct.html # expect 0
```
If no DSN is available, note it — the unit test `omits_resource_charts_without_samples` covers this path deterministically.

- [ ] **Step 5: Final checkpoint**

Report results to the user. Do not commit unless explicitly authorized.

---

## Self-Review

**Spec coverage:**
- Requests/sec over time → Task 4 `rps_chart` (data: Task 2 `interval_rate`). ✓
- Success/fail records, cumulative + per-second → Task 4 `cum_chart` + `interval_chart` (data: Task 2 `cum_accepted/failed`, `interval_accepted/failed`). ✓
- CPU over time (cores, isolated only) → Task 1 `cores_from_deltas`, Task 2 sampling, Task 3 PID plumbing, Task 4 CPU chart. ✓
- RAM over time (isolated only) → Task 1 `parse_status_vmrss_bytes`, Task 4 RSS chart. ✓
- Totals (outcomes, items, latency) → Task 4 stat cards + tables. ✓
- Opt-in `--report` flag → Task 5. ✓
- Self-contained HTML, no deps, `/proc` best-effort → Global Constraints, Tasks 1/4. ✓
- Direct mode omits resource charts → Task 3 (`None`), Task 4 (`has_resources`), Task 6 Step 4. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code. The one non-code judgement ("match the README heading style") is unavoidable and low-risk. ✓

**Type consistency:** `make_timepoint` signature identical in Task 2 definition and use. `aggregate(rx, users, start, target_pid)` matches Task 2 (def) and Task 3 (call). `engine::run(cfg, target, target_pid)` matches Task 3 def and main call sites. `ReportMeta` fields match Task 4 definition and Task 5 construction. `render(summary, expected, meta)` / `write(path, summary, expected, meta)` consistent across Tasks 4–5. `Expected { requests, duration_secs }` matches `cli.rs`. `TimePoint` fields consistent Task 2 ↔ Task 4. ✓
