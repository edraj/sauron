//! The metrics aggregator. A single task owns [`Metrics`] and receives one
//! [`Sample`] per request over an mpsc channel — so counters need no locks. It
//! also drives the once-a-second live line, and produces the final [`Summary`].

use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;

use crate::client::{OutcomeKind, SendOutcome};
use crate::generator::ItemCounts;
use crate::procstat::{self, RawSample, Sampler};

/// Cap on retained latency samples, to bound memory on very large runs. Beyond
/// this the summary flags that percentiles are computed from a prefix.
pub const LATENCY_CAP: usize = 1_000_000;

/// One request's result, handed to the aggregator.
pub struct Sample {
    pub outcome: SendOutcome,
    pub counts: ItemCounts,
}

/// A point-in-time view for the live progress line.
pub struct Snapshot {
    pub elapsed: Duration,
    pub requests: u64,
    pub accepted: u64,
    pub failed: u64,
    pub rate_cumulative: f64,
    pub rate_interval: f64,
    pub users: usize,
}

/// One per-second row of the run timeline, retained for the HTML report.
pub struct TimePoint {
    pub t_secs: f64,
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

/// The final, computed result of a run.
pub struct Summary {
    pub elapsed: Duration,
    pub users: usize,
    pub requests: u64,
    pub accepted: u64,
    pub rate_limited: u64,
    pub http_errors: u64,
    pub transport: u64,
    pub status_counts: Vec<(u16, u64)>,
    pub attempted: ItemCounts,
    pub accepted_items: ItemCounts,
    pub p50_us: u64,
    pub p90_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub latency_samples: u64,
    pub latency_truncated: bool,
    pub timeline: Vec<TimePoint>,
}

struct Metrics {
    start: Instant,
    users: usize,
    requests: u64,
    accepted: u64,
    rate_limited: u64,
    http_errors: u64,
    transport: u64,
    status_counts: std::collections::BTreeMap<u16, u64>,
    attempted: Sums,
    accepted_items: Sums,
    latencies_us: Vec<u64>,
    latency_total: u64,
    /// True max over ALL requests, tracked exactly even past the reservoir cap
    /// (the slowest requests often arrive last and would be dropped otherwise).
    latency_max_us: u64,
    latency_truncated: bool,
}

/// Mutable running sums of per-type item counts.
#[derive(Default)]
struct Sums {
    errors: u64,
    events: u64,
    identifies: u64,
    transactions: u64,
    breadcrumbs: u64,
}

impl Sums {
    fn add(&mut self, c: &ItemCounts) {
        self.errors += c.errors;
        self.events += c.events;
        self.identifies += c.identifies;
        self.transactions += c.transactions;
        self.breadcrumbs += c.breadcrumbs;
    }
    fn into_counts(self) -> ItemCounts {
        ItemCounts {
            errors: self.errors,
            events: self.events,
            identifies: self.identifies,
            transactions: self.transactions,
            breadcrumbs: self.breadcrumbs,
        }
    }
}

impl Metrics {
    fn new(users: usize, start: Instant) -> Self {
        Metrics {
            start,
            users,
            requests: 0,
            accepted: 0,
            rate_limited: 0,
            http_errors: 0,
            transport: 0,
            status_counts: std::collections::BTreeMap::new(),
            attempted: Sums::default(),
            accepted_items: Sums::default(),
            latencies_us: Vec::new(),
            latency_total: 0,
            latency_max_us: 0,
            latency_truncated: false,
        }
    }

    fn record(&mut self, s: Sample) {
        self.requests += 1;
        self.attempted.add(&s.counts);
        match s.outcome.kind {
            OutcomeKind::Accepted => {
                self.accepted += 1;
                self.accepted_items.add(&s.counts);
            }
            OutcomeKind::RateLimited => self.rate_limited += 1,
            OutcomeKind::HttpError => self.http_errors += 1,
            OutcomeKind::Transport => self.transport += 1,
        }
        if let Some(code) = s.outcome.status {
            *self.status_counts.entry(code).or_insert(0) += 1;
        }
        // Latency retained for every completed request (incl. non-2xx / timeouts).
        let latency_us = s.outcome.latency.as_micros() as u64;
        self.latency_total += 1;
        self.latency_max_us = self.latency_max_us.max(latency_us);
        if self.latencies_us.len() < LATENCY_CAP {
            self.latencies_us.push(latency_us);
        } else {
            self.latency_truncated = true;
        }
    }

    fn failed(&self) -> u64 {
        self.rate_limited + self.http_errors + self.transport
    }

    fn snapshot(&self, prev_requests: u64, interval: Duration) -> Snapshot {
        let elapsed = self.start.elapsed();
        let secs = elapsed.as_secs_f64().max(1e-9);
        let rate_interval = (self.requests - prev_requests) as f64 / interval.as_secs_f64();
        Snapshot {
            elapsed,
            requests: self.requests,
            accepted: self.accepted,
            failed: self.failed(),
            rate_cumulative: self.requests as f64 / secs,
            rate_interval,
            users: self.users,
        }
    }

    fn finalize(mut self, timeline: Vec<TimePoint>) -> Summary {
        self.latencies_us.sort_unstable();
        let p = |q: f64| percentile(&self.latencies_us, q);
        Summary {
            elapsed: self.start.elapsed(),
            users: self.users,
            requests: self.requests,
            accepted: self.accepted,
            rate_limited: self.rate_limited,
            http_errors: self.http_errors,
            transport: self.transport,
            status_counts: self.status_counts.into_iter().collect(),
            p50_us: p(50.0),
            p90_us: p(90.0),
            p99_us: p(99.0),
            max_us: self.latency_max_us,
            latency_samples: self.latency_total,
            latency_truncated: self.latency_truncated,
            attempted: self.attempted.into_counts(),
            accepted_items: self.accepted_items.into_counts(),
            timeline,
        }
    }
}

/// Nearest-rank percentile over a pre-sorted slice. Returns 0 for an empty slice.
fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (q / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Build one timeline row from current cumulative counters and the prior tick's
/// counters. Pure and total (no divide-by-zero: `interval` is always ≥ the 1s
/// tick). Resource values are passed in already-resolved so this stays testable.
#[allow(clippy::too_many_arguments)]
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
        cum_accepted,
        cum_failed,
        interval_rate: (cum_requests - prev_requests) as f64 / secs,
        interval_accepted: cum_accepted - prev_accepted,
        interval_failed: cum_failed - prev_failed,
        cpu_cores,
        rss_bytes,
    }
}

/// Turns each successful CPU sample into a cores-used value by differencing it
/// against the previous successful sample. The first sample has no prior, so it
/// yields `None`; every later sample yields `Δproc / Δtotal × ncpus` cores. Kept
/// as its own unit so the first-tick / no-prior state machine is testable without
/// the async loop or a live `/proc`.
struct CpuTracker {
    ncpus: f64,
    prev_proc: Option<u64>,
    prev_total: Option<u64>,
}

impl CpuTracker {
    fn new(ncpus: f64) -> Self {
        CpuTracker {
            ncpus,
            prev_proc: None,
            prev_total: None,
        }
    }

    /// Record a sample and return cores used since the previous one (`None` on the
    /// first sample). A transient gap in sampling is not reset here, so the next
    /// value simply averages over the widened window.
    fn update(&mut self, raw: &RawSample) -> Option<f64> {
        let cores = match (self.prev_proc, self.prev_total) {
            (Some(pp), Some(pt)) => Some(procstat::cores_from_deltas(
                raw.proc_jiffies.saturating_sub(pp),
                raw.total_jiffies.saturating_sub(pt),
                self.ncpus,
            )),
            _ => None, // first sample: no prior to delta against
        };
        self.prev_proc = Some(raw.proc_jiffies);
        self.prev_total = Some(raw.total_jiffies);
        cores
    }
}

/// Run the aggregator to completion: record every sample, print the live line
/// each second, and return the final summary once all senders have dropped.
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
    let mut cpu = sampler.as_ref().map(|s| CpuTracker::new(s.ncpus()));
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

                // Resolve this tick's resource sample (best-effort). CPU differencing
                // lives in CpuTracker; RSS is a direct read of the sample.
                let (cpu_cores, rss_bytes) = match sampler.as_ref().and_then(|s| s.sample()) {
                    Some(raw) => (
                        cpu.as_mut().and_then(|t| t.update(&raw)),
                        Some(raw.rss_bytes),
                    ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect(); // sorted 1..100
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 90.0), 90);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&v, 100.0), 100);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn records_outcomes_and_items() {
        let mut m = Metrics::new(4, Instant::now());
        let sample = |kind, status| Sample {
            outcome: SendOutcome {
                kind,
                status,
                latency: Duration::from_millis(5),
            },
            counts: ItemCounts {
                events: 1,
                transactions: 1,
                ..Default::default()
            },
        };
        m.record(sample(OutcomeKind::Accepted, Some(202)));
        m.record(sample(OutcomeKind::RateLimited, Some(429)));
        m.record(sample(OutcomeKind::Transport, None));
        let s = m.finalize(vec![]);
        assert_eq!(s.requests, 3);
        assert_eq!(s.accepted, 1);
        assert_eq!(s.rate_limited, 1);
        assert_eq!(s.transport, 1);
        assert_eq!(s.attempted.events, 3);
        assert_eq!(s.accepted_items.events, 1); // only the accepted one counts
        assert_eq!(s.status_counts, vec![(202, 1), (429, 1)]);
    }

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

    #[test]
    fn cpu_tracker_first_sample_none_then_cores() {
        let raw = |proc, total| RawSample {
            proc_jiffies: proc,
            total_jiffies: total,
            rss_bytes: 0,
        };
        let mut t = CpuTracker::new(8.0);
        // First sample: no prior to difference against.
        assert_eq!(t.update(&raw(1000, 100_000)), None);
        // Δproc 400 / Δtotal 4000 × 8 cores = 0.8 cores.
        let cores = t.update(&raw(1400, 104_000)).unwrap();
        assert!((cores - 0.8).abs() < 1e-9, "got {cores}");
        // Δproc 0 → 0 cores (idle interval), still updates state.
        assert_eq!(t.update(&raw(1400, 108_000)), Some(0.0));
    }

    // Real-time integration smoke test of the async loop against a live `/proc`:
    // samples THIS test process, so it only runs where `/proc` exists.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aggregate_samples_real_process_and_builds_timeline() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Sample>();
        let handle = tokio::spawn(aggregate(rx, 2, Instant::now(), Some(std::process::id())));
        let mk = || Sample {
            outcome: SendOutcome {
                kind: OutcomeKind::Accepted,
                status: Some(202),
                latency: Duration::from_millis(2),
            },
            counts: ItemCounts { events: 1, ..Default::default() },
        };
        for _ in 0..3 {
            tx.send(mk()).unwrap();
        }
        // Let the aggregator drain the sends, then fire exactly one 1s tick.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        drop(tx);
        let s = handle.await.unwrap();

        assert_eq!(s.requests, 3);
        assert!(!s.timeline.is_empty(), "expected at least one timeline tick");
        // RSS is available from the very first tick; CPU has no prior sample yet.
        assert!(s.timeline[0].rss_bytes.is_some(), "RSS should sample from self");
        assert!(s.timeline[0].cpu_cores.is_none(), "first tick has no prior CPU sample");
    }
}
