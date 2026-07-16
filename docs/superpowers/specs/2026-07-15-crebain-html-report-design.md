# crebain HTML benchmark report — design

**Date:** 2026-07-15
**Component:** `backend/bins/crebain`
**Status:** approved, pending implementation

## Goal

Add an opt-in, self-contained HTML report to crebain. When the user passes
`crebain --report bench.html`, crebain writes a single HTML file (opens offline,
no CDN/network) containing:

- **Requests per second** over time.
- **Records success/fail** over time — **both** cumulative and per-second.
- **CPU usage over time** (in cores) of the target `sauron-ingest` server —
  `--isolated` mode only.
- **RAM usage over time** (RSS) of the target server — `--isolated` mode only.
- The headline totals crebain already computes (outcomes, item counts by signal
  type, latency percentiles), as stat cards + tables.

Without `--report`, behavior is unchanged: the existing text summary only.

## Scope & decisions (from brainstorming)

- **Resource target:** the `sauron-ingest` server that crebain spawns as its own
  child in `--isolated` mode. We own the child PID, so sampling is reliable and
  needs no config. In `--dsn` (direct) mode there is no PID to sample, so the
  HTML report still renders the load charts but **omits** CPU/RAM.
- **Charting:** hand-drawn inline **SVG** generated in Rust. **Zero new crate
  dependencies.** Native SVG `<title>` elements give hover tooltips with no JS.
- **Trigger:** opt-in `--report <PATH>` flag. Default off.
- **CPU unit:** **cores** (e.g. "3.2 cores of 8"), not percent (ambiguous on
  multi-core).
- **Success/fail chart:** render **both** a cumulative accepted-vs-failed chart
  and a per-second (interval) accepted/failed chart.

## Non-goals (YAGNI)

- No configurable sample interval — reuse the existing 1-second aggregator tick.
- No sampling of remote/`--dsn` targets, no `--target-pid` flag.
- No third-party JS charting library, no CDN assets.
- No sampling of crebain's own process.
- Windows/macOS resource sampling — `/proc` is Linux-only; on other platforms
  the resource charts are simply omitted (load charts still render).

## Architecture

### 1. Time-series collection (extend `metrics.rs`)

The metrics aggregator (`metrics::aggregate`) already owns `Metrics`, ticks once
per second via a `tokio::time::interval`, prints the live line, and discards the
snapshot. Change: on each tick, **retain** one `TimePoint`, and when a target PID
is known, sample that process too.

```rust
pub struct TimePoint {
    pub t_secs: f64,             // wall seconds since run start
    pub cum_requests: u64,
    pub cum_accepted: u64,
    pub cum_failed: u64,
    pub interval_rate: f64,      // req/s over the last interval
    pub interval_accepted: u64,  // accepted during the last interval
    pub interval_failed: u64,    // failed during the last interval
    pub cpu_cores: Option<f64>,  // None: no PID / non-Linux / first tick
    pub rss_bytes: Option<u64>,  // None: no PID / non-Linux
}
```

- The timeline is a `Vec<TimePoint>` accumulated in the aggregator loop and
  attached to the final `Summary` as a new field `pub timeline: Vec<TimePoint>`.
- The aggregator keeps prior-tick state for interval deltas: `prev_requests`
  (already present), plus `prev_accepted`, `prev_failed`, and — for CPU —
  `prev_proc_jiffies`, `prev_total_jiffies`.
- `metrics::aggregate` gains a parameter `target_pid: Option<u32>`.
- First resource tick has no prior CPU sample, so `cpu_cores` is `None` on the
  first tick; RSS is available from the first tick.

### 2. Resource sampling — new `procstat.rs` (Linux `/proc`, zero new deps)

Pure parsing functions over `&str` (unit-testable without real `/proc`) plus thin
reader functions:

- `parse_proc_pid_stat(&str) -> Option<u64>`: utime+stime (fields 14+15, in
  jiffies) from `/proc/<pid>/stat`. Must handle a `comm` field containing spaces
  or parentheses by scanning to the last `')'` before splitting fields.
- `parse_proc_stat_total(&str) -> Option<u64>`: sum of all numbers on the
  aggregate `cpu ` line of `/proc/stat` (total system jiffies).
- `parse_vmrss_bytes(&str) -> Option<u64>`: `VmRSS:` line of `/proc/<pid>/status`
  (kB → bytes).
- `struct Sampler { pid, ncpus }` with `fn sample(&self) -> Option<RawSample>`
  reading the three files. `ncpus` from `std::thread::available_parallelism()`.

**CPU as cores (USER_HZ-independent):** over an interval,
`cores = Δproc_jiffies / Δtotal_jiffies × ncpus`. Because both numerator and
denominator are in the same jiffie units, USER_HZ cancels — no `libc` dependency
needed. `cores = 1.0` means one full core; may exceed 1.0 on multi-core.

All reads are best-effort: any I/O or parse failure yields `None` (charts
omitted), never an error that fails the run. On non-Linux, the files are absent →
`None`.

### 3. PID plumbing (`engine.rs`, `main.rs`)

- `engine::run(cfg, target)` → `engine::run(cfg, target, target_pid: Option<u32>)`,
  forwarding `target_pid` to `metrics::aggregate`.
- Direct mode (`main::run`): pass `None`.
- Isolated mode (`main::isolated_body`): after `provision` returns, read the PID
  from the harness child (`guard.child.id()` / a `Prepared`/guard accessor) and
  pass it in. The child is a `tokio::process::Child`; `.id()` returns
  `Option<u32>`.

### 4. HTML rendering — new `report_html.rs`

Signature roughly:
```rust
pub fn render(summary: &Summary, expected: &Expected, meta: &ReportMeta) -> String;
pub fn write(path: &Path, html: &str) -> anyhow::Result<()>;
```
`ReportMeta` carries run context for the header (mode label, users, duration,
events/issues per min, gzip, a timestamp string). The timestamp is produced by
the caller (crebain already depends on `chrono`).

Report body:

- **Header:** title, mode, users, duration, events/issues per min, gzip,
  timestamp.
- **Stat cards:** total requests; req/s achieved vs target; accepted; failed;
  accept-rate %; latency p50/p90/p99/max; peak CPU (cores, of N); peak RSS.
  Peaks derived from the timeline max.
- **Charts (inline SVG):**
  1. Requests/sec over time (`interval_rate`).
  2. Cumulative accepted vs failed (two lines).
  3. Per-second accepted vs failed (two lines, `interval_accepted`/`interval_failed`).
  4. CPU cores over time — omitted if no resource data.
  5. RSS (MB) over time — omitted if no resource data.
- **Tables** mirroring the text summary: outcomes (accepted / rate-limited /
  http errors / transport + status-code breakdown) and items accepted/attempted
  by signal type.
- Theme-aware light/dark CSS, everything inlined; no external requests.

**SVG chart helper:** a function that takes a title, y-axis unit/label, and one
or more named+colored series of `(t_secs, value)` points, and returns an `<svg>`
string with: a fixed `viewBox`, background/plot rect, horizontal gridlines with
y labels, x-axis time labels, one `<polyline>` per series, per-point `<circle>`
carrying a native `<title>` tooltip (`"t=12s: 3.2 cores"`), and a small legend.
Must handle degenerate input: 0 points → a "no data" placeholder; 1 point → a
single dot; all-equal values → a sensible flat line without divide-by-zero.

### 5. CLI (`cli.rs`)

- Add `--report <PATH>` (`Option<PathBuf>`) to `Args`.
- Carry it into `RunConfig` as `pub report_path: Option<PathBuf>`.
- `main::finish`: when `Some(Ok(summary))` and `cfg.report_path` is set, render
  and write the HTML after printing the text summary; print a
  `crebain: wrote report to <path>` line. A write failure is surfaced but does
  not change the run's success exit code (the run already completed).

## Data flow

```
run_user ──Sample──▶ mpsc ──▶ aggregate(rx, users, start, target_pid)
                                   │ each 1s tick:
                                   │   • push TimePoint (load counters + deltas)
                                   │   • Sampler::sample(pid) → cpu_cores, rss
                                   ▼
                               Summary { …totals…, timeline }
                                   │
                          finish() ─┴─▶ print_summary (stdout, unchanged)
                                        └─▶ if report_path: report_html::render → write file
```

## Testing

- **`procstat` parsing:** feed sample `/proc/<pid>/stat` (incl. a `comm` with
  spaces/parens), `/proc/stat`, and `/proc/<pid>/status` strings; assert
  utime+stime, total jiffies, and VmRSS bytes. Assert the cores formula on two
  synthetic samples.
- **SVG scaling:** points map into the `viewBox` (min→bottom, max→top); 0-point,
  1-point, and all-equal-value inputs don't panic and produce valid output.
- **HTML assembly:** rendering a synthetic `Summary` (with a small timeline)
  contains the headline numbers and each expected section; resource charts are
  absent when the timeline carries no `cpu_cores`/`rss_bytes` and present when it
  does.
- **Existing metrics tests:** update `Summary` construction for the new
  `timeline` field (empty vec).
- **E2E:** (a) deterministic — render HTML from a hand-built timeline, write to a
  temp path, assert the file exists and contains the charts. (b) if Postgres +
  Redis are available, a short `crebain --isolated --duration 5 --report <tmp>`
  run and confirm the file has all four/five charts populated. If infra is
  unavailable, note it and rely on (a) + unit tests.

## Files

| File | Change |
|------|--------|
| `src/procstat.rs` | **new** — Linux `/proc` sampling + pure parsers |
| `src/report_html.rs` | **new** — HTML + inline SVG rendering |
| `src/metrics.rs` | `TimePoint`, timeline retention, resource sampling, `target_pid` param, `Summary.timeline` |
| `src/engine.rs` | thread `target_pid` into `aggregate` |
| `src/main.rs` | pass child PID (isolated), write report in `finish`, `ReportMeta` |
| `src/cli.rs` | `--report` flag → `RunConfig.report_path` |
| `src/main.rs` mod list | declare `procstat`, `report_html` |
| `README.md` | document `--report` |

No new dependencies in `Cargo.toml`.

## Git

Per the standing project rule (work stays on the local branch; never commit
unless explicitly told), this spec and all implementation are left **uncommitted**
until the user asks for a commit. Work happens in the `crebain-benchmark`
worktree where the crate lives.
