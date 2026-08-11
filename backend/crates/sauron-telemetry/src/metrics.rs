//! Ingest accounting counters and their Prometheus text rendering.
//!
//! # The question these answer
//!
//! The edge answers an SDK `202 Accepted` the moment an envelope is appended to
//! the Redis ingest stream — before anything durable is written. Everything
//! between that append and the Postgres commit (a trim, an exhausted pool, a
//! wedged worker) can therefore lose telemetry that the SDK was told had
//! arrived. `packaging/rpm/SETUP.md` warns operators about that in two separate
//! places — the 000039 migration window and Postgres connection exhaustion —
//! and both now point at these counters, because they are the only number that
//! reports it. Measured on an isolated instance with a deliberately small
//! `INGEST_STREAM_MAXLEN`: 176,026 of 239,872 accepted items never persisted,
//! and the ingest logged not one WARN or ERROR line about it.
//!
//! # Units — the one thing that must not be got wrong
//!
//! `accepted` and `persisted` are both in **ITEMS**, so subtracting them is
//! meaningful. Redis's own `MAXLEN`, `entries-added` and `lag` are in
//! **ENTRIES**, and one entry is a whole envelope carrying up to 1000 items;
//! those must never be subtracted from an item count. Most of them are named for
//! it — `sauron_ingest_stream_length`, `..._stream_entries_added`,
//! `..._stream_entries_read`, `..._stream_group_lag`,
//! `..._stream_unread_trimmed` — but two probe-derived gauges are not:
//! `sauron_ingest_dlq_length` and `sauron_ingest_retry_length`, neither with
//! `stream_` in its name, so "the
//! `..._stream_...` gauges" is not a way to select the entries-unit ones. What
//! does hold, and is pinned by
//! `every_help_states_a_unit_and_only_dlq_length_lacks_the_stream_marker`: every
//! gauge rendered from a probe is in entries, and every metric here states its
//! unit as `Unit: <x>` in its help text.
//! `sauron_ingest_envelopes_accepted_total` is in ENVELOPES. It is a LOWER BOUND
//! on entries appended, not an equality: `RedisStore::xadd_job` issues one `XADD`
//! per call, but a zero-item envelope appends an entry without incrementing this
//! counter (the edge only counts when `accepted > 0`). Measured on one isolated
//! process: `stream_entries_added` 607 against `envelopes_accepted_total` 603.
//!
//! # Why `persisted` is not "rows written"
//!
//! Not every item produces a Postgres row: an `Identify` item is folded into a
//! user upsert whose errors the batch writer deliberately swallows, and a
//! `BreadcrumbBatch` item is written to Redis only. Counting rows would show
//! permanent phantom loss for any app that calls `identify()` or records
//! breadcrumbs. `persisted` therefore counts items whose entry reached a
//! terminal durable outcome and was acked.
//!
//! # Two ways the delta lies, both structural
//!
//! * **It is a fleet sum, never a per-process ratio.** The consumer group is
//!   shared (`sauron_redis::keys::CONSUMER_GROUP`), so one replica's edge is
//!   drained by another replica's workers. A per-target `accepted - persisted`
//!   is garbage; only the sum across every scrape target is valid.
//! * **It can go slightly negative.** An entry left unacked is reclaimed from
//!   the pending-entries list and reprocessed, so an item accepted once can be
//!   persisted twice. Small negative excursions are redeliveries. Only a
//!   persistently growing positive delta is loss.
//!
//! # Cost
//!
//! **Two** `fetch_add(_, Relaxed)`s per envelope on the edge, not one:
//! `items_accepted(accepted)` and then `envelopes_accepted(1)`. An envelope
//! whose `XADD` failed pays one, because `items_accepted(0)` still adds; the
//! rejections that return before the enqueue pay none. The worker's batch path
//! pays one per batch (`items_persisted(items)`), plus one per entry that would
//! not decode; only the rare per-item fallback path pays one per item. No Redis
//! call, no allocation and no lock on any of those paths. Measured on this machine
//! (`rustc -O`, 20 threads available, every thread contending on ONE shared
//! counter): 3.55 ns at 1 thread, 8.47 ns at 2, 10.14 ns at 8, 11.40 ns at 16.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

static ITEMS_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static ENVELOPES_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static ITEMS_PERSISTED: AtomicU64 = AtomicU64::new(0);
static ITEMS_DEADLETTERED: AtomicU64 = AtomicU64::new(0);
static ENTRIES_DEADLETTERED: AtomicU64 = AtomicU64::new(0);
static EMPTY_ENVELOPES_DROPPED: AtomicU64 = AtomicU64::new(0);
static DLQ_WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Add `n` ITEMS that the edge has enqueued and answered `202` for.
///
/// Call with the number actually enqueued, never with the envelope's item
/// count: the edge has two `202` paths that enqueue nothing (a serialization
/// failure and a failed `XADD`), and both report `accepted: 0`. Passing the
/// item count would inflate this counter on exactly the requests that lost the
/// data.
#[inline]
pub fn items_accepted(n: u64) {
    ITEMS_ACCEPTED.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` ENVELOPES the edge enqueued with at least one item. A zero-item
/// envelope is enqueued but NOT counted here, so this is a lower bound on stream
/// entries appended rather than an equality.
#[inline]
pub fn envelopes_accepted(n: u64) {
    ENVELOPES_ACCEPTED.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` envelopes ACCEPTED (202) but deliberately not enqueued because they
/// carried no items.
///
/// These used to append a stream entry holding nothing, consuming an
/// `INGEST_STREAM_MAXLEN` slot and — once the stream is at its cap — displacing
/// a real event through a silent trim. Counted rather than merely skipped so the
/// volume is visible: a client emitting them in a loop is a bug worth finding,
/// and after the short-circuit there is no other trace of it.
#[inline]
pub fn empty_envelopes_dropped(n: u64) {
    EMPTY_ENVELOPES_DROPPED.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` failed dead-letter WRITES.
///
/// The most serious counter in this module. A non-zero value means an event
/// failed to persist AND failed to reach the dead-letter queue, so the only
/// remaining copy is the un-acked stream entry. The worker withholds the ack
/// when this fires, so the entry is redelivered rather than lost — which means
/// a sustained non-zero rate also implies duplicate writes for the successful
/// items sharing those envelopes. Alert on it.
#[inline]
pub fn dlq_write_failures(n: u64) {
    DLQ_WRITE_FAILURES.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` ITEMS whose entry reached a terminal durable outcome and was acked.
#[inline]
pub fn items_persisted(n: u64) {
    ITEMS_PERSISTED.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` ITEMS pushed to the dead-letter queue individually.
#[inline]
pub fn items_deadlettered(n: u64) {
    ITEMS_DEADLETTERED.fetch_add(n, Ordering::Relaxed);
}

/// Add `n` whole ENTRIES dead-lettered before they could be decoded.
///
/// In entries, not items, because a payload that failed to deserialize has no
/// knowable item count.
#[inline]
pub fn entries_deadlettered(n: u64) {
    ENTRIES_DEADLETTERED.fetch_add(n, Ordering::Relaxed);
}

/// A consistent-enough read of every counter.
///
/// The loads are independent, so a scrape taken mid-batch can see `accepted`
/// already incremented and `persisted` not yet. That is a sub-second skew on a
/// counter meant to be read as a rate over minutes; making it atomic would
/// require a lock on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// ITEMS.
    pub items_accepted: u64,
    /// ENVELOPES with >= 1 item; a LOWER BOUND on entries appended, not equal to it.
    pub envelopes_accepted: u64,
    /// ITEMS.
    pub items_persisted: u64,
    /// ITEMS.
    pub items_deadlettered: u64,
    /// ENTRIES.
    pub entries_deadlettered: u64,
    /// ENVELOPES accepted with no items and deliberately not enqueued.
    pub empty_envelopes_dropped: u64,
    /// Dead-letter writes that FAILED. Non-zero means an event reached neither
    /// Postgres nor the DLQ.
    pub dlq_write_failures: u64,
}

/// Read every counter.
pub fn snapshot() -> Counters {
    Counters {
        items_accepted: ITEMS_ACCEPTED.load(Ordering::Relaxed),
        envelopes_accepted: ENVELOPES_ACCEPTED.load(Ordering::Relaxed),
        items_persisted: ITEMS_PERSISTED.load(Ordering::Relaxed),
        items_deadlettered: ITEMS_DEADLETTERED.load(Ordering::Relaxed),
        entries_deadlettered: ENTRIES_DEADLETTERED.load(Ordering::Relaxed),
        empty_envelopes_dropped: EMPTY_ENVELOPES_DROPPED.load(Ordering::Relaxed),
        dlq_write_failures: DLQ_WRITE_FAILURES.load(Ordering::Relaxed),
    }
}

/// A reading of the Redis ingest stream, in ENTRIES.
///
/// Plain data on purpose: `sauron-telemetry` has no in-workspace dependencies
/// and the Redis probe that fills this in lives in `sauron-redis`.
///
/// `entries_read` and `lag` are `Option` because Redis really does return nil
/// for them. Measured on redis 7.4.10: `XDEL` of an entry the group had not
/// been delivered nils `lag` (leaving `entries-read` intact), and
/// `XGROUP SETID` without an `ENTRIESREAD` argument nils BOTH. Rendering a nil
/// as `0` would report "no loss" at precisely the moment the number is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamSnapshot {
    /// Entries currently in the stream.
    pub length: u64,
    /// Entries ever appended, since this Redis instance last lost its state.
    pub entries_added: u64,
    /// Entries the consumer group has been delivered, or `None` if Redis
    /// reported nil.
    pub entries_read: Option<u64>,
    /// Entries appended but not yet delivered to the group, or `None` if Redis
    /// reported nil.
    pub lag: Option<u64>,
    /// Entries in the dead-letter stream.
    pub dlq_length: u64,
    /// Jobs currently waiting out a retry backoff.
    ///
    /// The recovery feature's own backlog. Without it the tier that exists to
    /// stop transient failures becoming data loss is itself unobservable: a
    /// database wobble parks thousands of jobs and the only visible symptom is
    /// that `items_persisted` quietly lags `items_accepted`.
    pub retry_length: u64,
}

impl StreamSnapshot {
    /// Entries that were trimmed away before the consumer group ever saw them.
    ///
    /// `entries_added - (entries_read + lag)`. Measured on redis 7.4.10 (and
    /// pinned by `sauron_redis`'s
    /// `detects_entries_trimmed_before_the_group_read_them`): with 11 added,
    /// 3 read and lag 2 the formula gives 6, which was exactly the number of
    /// undelivered entries `MAXLEN` had dropped.
    ///
    /// **A live gauge, not a ledger.** Also measured, in the same test: once
    /// the group was read to the tail, Redis raised `entries-read` from 3 to 11
    /// — a jump of 8 for 2 actually-delivered entries — absorbing the trimmed
    /// gap so this returns 0 again. Redis keeps no cumulative count of
    /// trimmed-unread entries, which is why the durable accounting is the item
    /// counters above and this is only the corroborating live reading.
    ///
    /// `None` when either input was nil, or if the arithmetic underflows (which
    /// would mean the relationship no longer holds and the answer is unknown,
    /// not zero).
    pub fn unread_trimmed(&self) -> Option<u64> {
        let read = self.entries_read?;
        let lag = self.lag?;
        self.entries_added.checked_sub(read.checked_add(lag)?)
    }
}

/// The Prometheus text-format body for `GET /metrics`.
pub fn render(stream: Option<&StreamSnapshot>) -> String {
    render_counters(&snapshot(), stream)
}

/// [`render`] against an explicit `Counters`, so it can be tested without
/// touching process-global state.
pub fn render_counters(c: &Counters, stream: Option<&StreamSnapshot>) -> String {
    let mut out = String::with_capacity(4096);

    metric(
        &mut out,
        "sauron_ingest_items_accepted_total",
        "counter",
        "Envelope ITEMS this process enqueued on the ingest stream and answered HTTP 202 for. \
         Unit: items. Subtract sauron_ingest_items_persisted_total from this ONLY as a sum over \
         every replica - the Redis consumer group is shared, so one replica's edge is drained by \
         another replica's workers and a per-target difference is meaningless.",
        c.items_accepted,
    );
    metric(
        &mut out,
        "sauron_ingest_envelopes_accepted_total",
        "counter",
        "Envelopes this process enqueued carrying at least one item. Unit: envelopes. Since the \
         empty-envelope short-circuit this equals the stream entries this process appended, \
         because a zero-item envelope no longer appends one - see \
         sauron_ingest_empty_envelopes_dropped_total. It was previously a LOWER BOUND. Present \
         so item counts and the entries-unit gauges below can be related; do not use it to \
         convert one into the other for traffic this process did not accept.",
        c.envelopes_accepted,
    );
    metric(
        &mut out,
        "sauron_ingest_empty_envelopes_dropped_total",
        "counter",
        "Envelopes answered HTTP 202 that carried no items and were deliberately NOT enqueued. \
         Unit: envelopes. Before this existed such an envelope appended a stream entry holding \
         nothing, consuming one of INGEST_STREAM_MAXLEN's slots and displacing a real event once \
         the stream is at its cap - a loss the trim performs silently. A steadily rising value \
         means a client is posting empty bodies; it is not itself an error.",
        c.empty_envelopes_dropped,
    );
    metric(
        &mut out,
        "sauron_ingest_dlq_write_failures_total",
        "counter",
        "Dead-letter writes that FAILED. Unit: writes. Non-zero means an event failed to persist \
         AND failed to reach the dead-letter queue: the only surviving copy is the un-acked \
         stream entry, which will be redelivered. Because redelivery replays the whole envelope, \
         a sustained non-zero rate also implies duplicate writes for that envelope's successful \
         items. This is the counter to alert on.",
        c.dlq_write_failures,
    );
    metric(
        &mut out,
        "sauron_ingest_items_persisted_total",
        "counter",
        "ITEMS whose stream entry reached a terminal durable outcome and was acked. Unit: items. \
         Not rows written: identify and breadcrumb items legitimately write no event row. May \
         exceed accepted slightly, because a reclaimed unacked entry is reprocessed; only a \
         persistently growing accepted-minus-persisted gap is loss.",
        c.items_persisted,
    );
    metric(
        &mut out,
        "sauron_ingest_items_deadlettered_total",
        "counter",
        "ITEMS pushed to the dead-letter queue one at a time after their own write failed. \
         Unit: items. Subtract from the accepted-minus-persisted gap, or dead-lettered items are \
         misread as trim loss.",
        c.items_deadlettered,
    );
    metric(
        &mut out,
        "sauron_ingest_entries_deadlettered_total",
        "counter",
        "Whole stream ENTRIES dead-lettered because the payload would not decode. Unit: entries, \
         NOT items - a payload that failed to deserialize has no knowable item count, so this \
         cannot be subtracted from an item counter.",
        c.entries_deadlettered,
    );

    // Absent, never zero: no successful probe means the numbers are unknown.
    if let Some(s) = stream {
        metric(
            &mut out,
            "sauron_ingest_stream_length",
            "gauge",
            "ENTRIES currently in the ingest stream, as of the last background probe. \
             Unit: entries.",
            s.length,
        );
        metric(
            &mut out,
            "sauron_ingest_stream_entries_added",
            "gauge",
            "ENTRIES ever appended to the ingest stream, as Redis reports it. Unit: entries. \
             Resets if Redis loses its state, so it is not comparable with our own counters \
             across a Redis restart.",
            s.entries_added,
        );
        if let Some(read) = s.entries_read {
            metric(
                &mut out,
                "sauron_ingest_stream_entries_read",
                "gauge",
                "ENTRIES the shared consumer group has been delivered, as Redis reports it. \
                 Unit: entries. Absent when Redis reports nil.",
                read,
            );
        }
        if let Some(lag) = s.lag {
            metric(
                &mut out,
                "sauron_ingest_stream_group_lag",
                "gauge",
                "ENTRIES appended but not yet delivered to the consumer group. Unit: entries. \
                 Absent when Redis reports nil. Not a trim detector on its own: a MAXLEN trim \
                 does not nil this out, it silently recomputes it downward.",
                lag,
            );
        }
        if let Some(trimmed) = s.unread_trimmed() {
            metric(
                &mut out,
                "sauron_ingest_stream_unread_trimmed",
                "gauge",
                "ENTRIES trimmed before the consumer group ever saw them, derived as \
                 entries_added - entries_read - lag. Unit: entries. A LIVE gauge, not a total: \
                 it falls back to 0 once the group catches up to the tail, because Redis absorbs \
                 the gap into entries-read. Absent when either input is nil.",
                trimmed,
            );
        }
        metric(
            &mut out,
            "sauron_ingest_dlq_length",
            "gauge",
            "ENTRIES in the dead-letter stream. Unit: entries. Note the name: this and \
             sauron_ingest_retry_length carry no stream_ marker, so a query written as \
             sauron_ingest_stream_* silently omits both. Since ingest failure recovery landed \
             this is only the BACKSTOP — it counts failures that could not even be written to \
             Postgres — so a non-zero value here means the database was unreachable, not merely \
             that an event was malformed. Bounded by INGEST_DLQ_MAXLEN and reaped after \
             INGEST_DLQ_RETENTION_HOURS, but both bounds require a running worker.",
            s.dlq_length,
        );
        metric(
            &mut out,
            "sauron_ingest_retry_length",
            "gauge",
            "JOBS currently waiting out a retry backoff. Unit: entries. A live gauge of the \
             recovery tier's own backlog: transient failures park here for ~60s per attempt and \
             leave once re-enqueued, so a sustained non-zero value means failures are arriving \
             faster than they are clearing. Only worker-0 drains it, so this rising while \
             nothing else changes means no worker is running.",
            s.retry_length,
        );
    }

    out
}

/// One `# HELP` / `# TYPE` / sample triple.
///
/// `write!` into a `String` cannot fail, so the results are discarded rather
/// than propagated.
fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
    let _ = writeln!(out, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sample line must be preceded by its own `# HELP` and `# TYPE`, and
    /// the help must name the unit — the whole failure mode of this metric is
    /// someone subtracting an entry count from an item count.
    #[test]
    fn renders_help_type_and_unit_for_every_sample() {
        let c = Counters {
            items_accepted: 7,
            envelopes_accepted: 2,
            items_persisted: 5,
            items_deadlettered: 1,
            entries_deadlettered: 3,
            empty_envelopes_dropped: 4,
            dlq_write_failures: 6,
        };
        let text = render_counters(&c, None);

        for (name, value) in [
            ("sauron_ingest_items_accepted_total", 7),
            ("sauron_ingest_envelopes_accepted_total", 2),
            ("sauron_ingest_empty_envelopes_dropped_total", 4),
            ("sauron_ingest_dlq_write_failures_total", 6),
            ("sauron_ingest_items_persisted_total", 5),
            ("sauron_ingest_items_deadlettered_total", 1),
            ("sauron_ingest_entries_deadlettered_total", 3),
        ] {
            assert!(
                text.contains(&format!("\n{name} {value}\n"))
                    || text.starts_with(&format!("{name} {value}\n")),
                "missing sample line `{name} {value}` in:\n{text}"
            );
            let help = text
                .lines()
                .position(|l| l.starts_with(&format!("# HELP {name} ")))
                .unwrap_or_else(|| panic!("no HELP for {name} in:\n{text}"));
            let kind = text
                .lines()
                .position(|l| l == format!("# TYPE {name} counter"))
                .unwrap_or_else(|| panic!("no counter TYPE for {name} in:\n{text}"));
            let sample = text
                .lines()
                .position(|l| l == format!("{name} {value}"))
                .unwrap();
            assert!(
                help < kind && kind < sample,
                "{name}: HELP/TYPE must precede the sample"
            );

            let help_line = text
                .lines()
                .find(|l| l.starts_with(&format!("# HELP {name} ")))
                .unwrap();
            assert!(
                help_line.contains("Unit: items")
                    || help_line.contains("Unit: entries")
                    || help_line.contains("Unit: envelopes")
                    // "writes" is a genuinely distinct unit, not a synonym for
                    // one of the above: a FAILED dead-letter write produces no
                    // entry, so labelling it "entries" to satisfy this list
                    // would be labelling it wrongly. The list exists to stop
                    // someone subtracting an entry count from an item count —
                    // it should enumerate the real units, not force a metric
                    // into the nearest wrong one.
                    || help_line.contains("Unit: writes"),
                "{name} help must state its unit, got: {help_line}"
            );
        }
    }

    /// The two claims the module doc makes about NAMES, checked rather than
    /// asserted in prose: every metric states its unit, and `dlq_length` and
    /// `retry_length` are the probe-derived gauges whose names lack the
    /// `stream_` marker — which is why `sauron_ingest_stream_*` is not a way to
    /// select the entries-unit gauges.
    #[test]
    fn every_help_states_a_unit_and_only_dlq_length_lacks_the_stream_marker() {
        // A snapshot with every optional field present, so nothing is skipped.
        let s = StreamSnapshot {
            length: 3,
            entries_added: 11,
            entries_read: Some(3),
            lag: Some(2),
            dlq_length: 1,
            retry_length: 4,
        };
        let text = render_counters(
            &Counters {
                items_accepted: 7,
                envelopes_accepted: 2,
                items_persisted: 5,
                items_deadlettered: 1,
                entries_deadlettered: 3,
                empty_envelopes_dropped: 4,
                dlq_write_failures: 6,
            },
            Some(&s),
        );

        let helps: Vec<&str> = text.lines().filter(|l| l.starts_with("# HELP ")).collect();
        assert_eq!(
            helps.len(),
            14,
            "expected 7 counters + 7 gauges, got:\n{text}"
        );
        for h in &helps {
            assert!(h.contains("Unit: "), "help line states no unit: {h}");
        }

        let gauges: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("# TYPE "))
            .filter(|l| l.ends_with(" gauge"))
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(gauges.len(), 7, "expected seven probe-derived gauges");
        for g in &gauges {
            let help = helps
                .iter()
                .find(|h| h.starts_with(&format!("# HELP {g} ")))
                .unwrap_or_else(|| panic!("no HELP for {g}"));
            assert!(
                help.contains("Unit: entries"),
                "every probe-derived gauge is in entries; {g} says: {help}"
            );
        }
        let unmarked: Vec<&&str> = gauges
            .iter()
            .filter(|n| !n.starts_with("sauron_ingest_stream_"))
            .collect();
        assert_eq!(
            unmarked,
            vec![&"sauron_ingest_dlq_length", &"sauron_ingest_retry_length"],
            "the set of gauges without a stream_ marker changed; the module doc and the \
             dlq_length help must both name every gauge that lacks the marker, or a \
             dashboard written as sauron_ingest_stream_* silently omits one"
        );
    }

    /// No probe means no stream gauges at all, rather than a row of zeroes that
    /// would read as a healthy stream.
    #[test]
    fn omits_stream_gauges_entirely_without_a_probe() {
        let text = render_counters(&Counters::default(), None);
        assert!(
            !text.contains("sauron_ingest_stream_"),
            "unprobed render leaked stream gauges:\n{text}"
        );
        assert!(!text.contains("sauron_ingest_dlq_length"));
    }

    /// The nil-as-zero bug, pinned. A nil `lag` must make the derived gauge
    /// ABSENT; emitting `0` would announce "nothing was trimmed" using a number
    /// Redis declined to provide.
    #[test]
    fn nil_lag_renders_the_derived_gauge_as_absent_not_zero() {
        let s = StreamSnapshot {
            length: 2,
            entries_added: 40,
            entries_read: Some(11),
            lag: None,
            dlq_length: 0,
            retry_length: 0,
        };
        assert_eq!(s.unread_trimmed(), None);

        let text = render_counters(&Counters::default(), Some(&s));
        assert!(
            !text.contains("sauron_ingest_stream_unread_trimmed"),
            "nil lag must not render the derived gauge:\n{text}"
        );
        assert!(
            !text.contains("sauron_ingest_stream_group_lag"),
            "nil lag must not render as a lag sample:\n{text}"
        );
        // The inputs Redis DID give must still be reported.
        assert!(text.contains("\nsauron_ingest_stream_entries_added 40\n"));
        assert!(text.contains("\nsauron_ingest_stream_entries_read 11\n"));
    }

    /// Same guard for a nil `entries-read`.
    #[test]
    fn nil_entries_read_renders_the_derived_gauge_as_absent() {
        let s = StreamSnapshot {
            entries_added: 40,
            entries_read: None,
            lag: Some(2),
            ..Default::default()
        };
        assert_eq!(s.unread_trimmed(), None);
        let text = render_counters(&Counters::default(), Some(&s));
        assert!(!text.contains("sauron_ingest_stream_unread_trimmed"));
        assert!(!text.contains("sauron_ingest_stream_entries_read"));
        assert!(text.contains("\nsauron_ingest_stream_group_lag 2\n"));
    }

    /// The arithmetic, against the numbers measured on redis 7.4-alpine.
    #[test]
    fn unread_trimmed_matches_the_measured_arithmetic() {
        let round1 = StreamSnapshot {
            entries_added: 11,
            entries_read: Some(3),
            lag: Some(2),
            ..Default::default()
        };
        assert_eq!(round1.unread_trimmed(), Some(6));

        let round2 = StreamSnapshot {
            entries_added: 32,
            entries_read: Some(11),
            lag: Some(5),
            ..Default::default()
        };
        assert_eq!(round2.unread_trimmed(), Some(16));

        // Caught up to the tail: nothing unread, nothing trimmed-unread.
        let drained = StreamSnapshot {
            entries_added: 32,
            entries_read: Some(32),
            lag: Some(0),
            ..Default::default()
        };
        assert_eq!(drained.unread_trimmed(), Some(0));

        // Inconsistent inputs are unknown, not zero.
        let impossible = StreamSnapshot {
            entries_added: 5,
            entries_read: Some(9),
            lag: Some(1),
            ..Default::default()
        };
        assert_eq!(impossible.unread_trimmed(), None);
    }

    /// The global counters really are the thing `snapshot` reads. Asserted as a
    /// DELTA so it does not depend on test ordering within this binary.
    #[test]
    fn global_counters_accumulate() {
        let before = snapshot();
        items_accepted(4);
        envelopes_accepted(1);
        items_persisted(3);
        items_deadlettered(1);
        entries_deadlettered(2);
        let after = snapshot();

        assert_eq!(after.items_accepted - before.items_accepted, 4);
        assert_eq!(after.envelopes_accepted - before.envelopes_accepted, 1);
        assert_eq!(after.items_persisted - before.items_persisted, 3);
        assert_eq!(after.items_deadlettered - before.items_deadlettered, 1);
        assert_eq!(after.entries_deadlettered - before.entries_deadlettered, 2);
    }
}
