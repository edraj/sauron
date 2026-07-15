//! The metrics aggregator. A single task owns [`Metrics`] and receives one
//! [`Sample`] per request over an mpsc channel — so counters need no locks. It
//! also drives the once-a-second live line, and produces the final [`Summary`].

use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;

use crate::client::{OutcomeKind, SendOutcome};
use crate::generator::ItemCounts;

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
        self.latency_total += 1;
        if self.latencies_us.len() < LATENCY_CAP {
            self.latencies_us.push(s.outcome.latency.as_micros() as u64);
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

    fn finalize(mut self) -> Summary {
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
            max_us: self.latencies_us.last().copied().unwrap_or(0),
            latency_samples: self.latency_total,
            latency_truncated: self.latency_truncated,
            attempted: self.attempted.into_counts(),
            accepted_items: self.accepted_items.into_counts(),
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

/// Run the aggregator to completion: record every sample, print the live line
/// each second, and return the final summary once all senders have dropped.
pub async fn aggregate(mut rx: UnboundedReceiver<Sample>, users: usize, start: Instant) -> Summary {
    let interval_dur = Duration::from_secs(1);
    let mut metrics = Metrics::new(users, start);
    let mut ticker = tokio::time::interval(interval_dur);
    ticker.tick().await; // consume the immediate first tick
    let mut prev_requests = 0u64;

    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(sample) => metrics.record(sample),
                None => break, // all user tasks finished
            },
            _ = ticker.tick() => {
                crate::report::live_line(&metrics.snapshot(prev_requests, interval_dur));
                prev_requests = metrics.requests;
            }
        }
    }
    crate::report::clear_live_line();
    metrics.finalize()
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
        let s = m.finalize();
        assert_eq!(s.requests, 3);
        assert_eq!(s.accepted, 1);
        assert_eq!(s.rate_limited, 1);
        assert_eq!(s.transport, 1);
        assert_eq!(s.attempted.events, 3);
        assert_eq!(s.accepted_items.events, 1); // only the accepted one counts
        assert_eq!(s.status_counts, vec![(202, 1), (429, 1)]);
    }
}
