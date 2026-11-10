//! The `issues.users_seen` write-back: a per-issue throttle, the pending set
//! that makes the throttle lossless, and the drain that empties it.
//!
//! ## What is being written
//!
//! `users_seen` is a HyperLogLog *estimate* whose authoritative copy lives in
//! Redis ([`keys::issue_users`]); Postgres holds a denormalized copy so the
//! Issues page can sort by it. Both write paths (`batch::process_batch` and
//! `process::process_error`) `PFADD` every occurrence's user and ask for a
//! write-back only when the estimate moved (or when `PFADD` failed, since an
//! unknown answer must not be read as "unchanged").
//!
//! ## Why it is throttled
//!
//! Writing that copy on every occurrence put an `UPDATE issues` in the hot
//! path of every error event, and it deadlocked against the issue upsert —
//! two statements touching the same rows, one ordered by `(app_id,
//! fingerprint)` and one by `id`, which no amount of sorting can reconcile.
//! Measured on an 8-worker ingest against 5 fingerprints, nearly every batch
//! lost that race, rolled back, and replayed item-by-item: the write path ran
//! ~14x slower than with the write-back removed entirely. So an issue gets at
//! most one write-back per [`WRITE_BACK_INTERVAL`] PER INGEST PROCESS: the
//! throttle is in memory, so N `sauron-ingest` replicas may each write it
//! once per interval.
//!
//! ## Why there is a pending set
//!
//! The throttle alone LOST updates. An issue whose estimate moved while its
//! gate was closed was skipped, and nothing wrote it back later unless a
//! future occurrence moved the estimate again after the gate reopened — eight
//! users inside one window left `users_seen = 1` for good. Now a closed gate
//! records the issue as pending instead, and a pending issue is written back
//! with the CURRENT `PFCOUNT` once its gate reopens, by whichever comes first:
//!
//! - the end of the next batch (`batch::process_batch`, at most
//!   [`DRAIN_PER_BATCH`] per batch), or
//! - the flusher ([`spawn_flusher`], every [`FLUSH_TICK`], at most
//!   [`DRAIN_PER_TICK`] per tick), so a burst followed by silence converges.
//!
//! The pending set is bounded at [`PENDING_CAP`]; on overflow the entry that
//! has waited longest is dropped (and counted, and the flusher warns). That
//! loses the one issue's catch-up — its count stays at the last written
//! value until a new user moves it again — rather than letting the set grow
//! without limit.
//!
//! ## What the pending set does not cover
//!
//! - **A restart.** The set lives in memory and `sauron-ingest` has no
//!   graceful shutdown, so a restart drops it. The flusher therefore starts by
//!   re-checking the issues an event landed on in the [`RESTART_RECHECK`]
//!   (10 minutes) before startup, newest first, at most [`SEED_CAP`] of them
//!   ([`seed_recent`]): one bounded query, then the ordinary drain. That
//!   covers a restart, or an outage, shorter than about 10 minutes. After a
//!   longer one, re-run `sauron-migrate backfill-users-seen`.
//! - **Counts that went stale before this existed.** An issue left at a stale
//!   value by the old, lossy throttle only converges when a new user arrives.
//!   Repair those once, after upgrading, with the operator-run
//!   `sauron-migrate backfill-users-seen`. Nothing runs it automatically. It
//!   can raise a count a data purge lowered (a HyperLogLog cannot forget the
//!   purged users): an overcount, the direction the purge treats as safe.
//!
//! ## Why the drain cannot deadlock
//!
//! Every drained write-back is its own SINGLE-ROW autocommit statement
//! (`repo::set_issue_users_seen`), issued on a connection that is NOT inside a
//! transaction. A single-row autocommit `UPDATE` holds no other row lock
//! while it waits for its one row, so it cannot be one edge of a lock cycle:
//! whatever holds that row (the issue upsert, a batch's multi-row write-back)
//! finishes without waiting on it. `issues` has no triggers, and the column
//! carries no foreign key, so the statement takes no lock elsewhere either.
//! The drain is never folded into one multi-row statement, and never runs
//! inside the batch transaction (`write_rows` has committed before stage 6).
//!
//! The write is monotonic (`... AND users_seen < $n`): an estimate read
//! earlier and committed later — two replicas, or a drain racing a hot-path
//! write — cannot lower the stored count.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::Utc;
use sauron_db::{repo, AsyncPgConnection, PgPool};
use sauron_redis::{keys, RedisStore};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use uuid::Uuid;

/// The most often one issue's `users_seen` is written back, per ingest
/// process.
///
/// The estimate in Redis stays exact and immediate; only the denormalized
/// copy lags, by at most about one interval: a pending issue is flushed once
/// its gate reopens, by the next batch's drain or by the flusher's next tick
/// (so the worst case is this plus [`FLUSH_TICK`]).
pub const WRITE_BACK_INTERVAL: Duration = Duration::from_secs(5);

/// How often the flusher looks for due pending issues.
///
/// Shorter than the interval on purpose: the gate, not the tick, is what
/// bounds the write volume (one write-back per issue per interval), and a
/// tick that finds nothing due takes one mutex and touches neither Postgres
/// nor Redis. A tick equal to the interval would let a pending issue wait
/// almost two intervals.
pub const FLUSH_TICK: Duration = Duration::from_secs(1);

/// Due pending issues each batch writes back after its own write-back. Small,
/// because it adds up to this many single-row round trips to a batch.
pub const DRAIN_PER_BATCH: usize = 16;

/// Due pending issues one flusher tick writes back. The rest stay pending for
/// the next tick, so one tick's duration and burst on Postgres stay bounded.
pub const DRAIN_PER_TICK: usize = 1_000;

/// Rows the flusher writes on one pooled connection before handing it back,
/// so a full tick does not hold a slot of the pool the workers share for its
/// whole duration.
const ROWS_PER_CHECKOUT: usize = 100;

/// Most issues held pending at once; see the module docs for the overflow rule.
pub const PENDING_CAP: usize = 50_000;

/// How far before startup the flusher's first pass looks for issues whose
/// pending entry a restart may have dropped. Comfortably more than one
/// interval, so an orderly restart, a crash loop or a slow boot are all
/// covered; [`SEED_CAP`] bounds it.
pub const RESTART_RECHECK: Duration = Duration::from_secs(10 * 60);

/// Most issues the startup re-check pends: half the pending set, so the hot
/// paths' first deferrals after a restart fit beside the seeded entries
/// instead of evicting them.
pub const SEED_CAP: usize = PENDING_CAP / 2;

/// Startup re-check attempts before giving up. The query scans `issues`, so
/// a persistently failing one must not rerun forever.
const SEED_ATTEMPTS: u32 = 5;

/// Longest wait between two failed startup re-check attempts. The first wait
/// is one [`WRITE_BACK_INTERVAL`], doubling from there.
const SEED_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Most issues whose last write-back is remembered. Cleared wholesale above
/// this (rather than evicting an LRU), which costs at most one extra
/// write-back per issue.
const GATE_CAP: usize = 50_000;

/// The gate and the pending set, under one lock so an issue moves between
/// them atomically: it is never both written by a batch and still pending.
#[derive(Debug)]
pub(crate) struct Throttle {
    interval: Duration,
    gate_cap: usize,
    pending_cap: usize,
    /// Last write-back per issue.
    gate: HashMap<Uuid, Instant>,
    /// Pending issue → when its gate reopens.
    pending: HashMap<Uuid, Instant>,
    /// The same entries ordered by that instant, so both the drain and
    /// overflow take the longest-waiting entry first, in O(log n).
    queue: BTreeSet<(Instant, Uuid)>,
    /// Pending entries dropped by the cap since the last [`Self::take_evicted`].
    evicted: u64,
}

impl Throttle {
    pub(crate) fn new(interval: Duration, gate_cap: usize, pending_cap: usize) -> Self {
        Self {
            interval,
            gate_cap,
            pending_cap,
            gate: HashMap::new(),
            pending: HashMap::new(),
            queue: BTreeSet::new(),
            evicted: 0,
        }
    }

    /// The estimate for `issue_id` moved at `now`. Returns `true` when the
    /// caller should write it back now (the gate is stamped, and any pending
    /// entry is cleared: the caller's fresh `PFCOUNT` covers it); `false`
    /// when the gate is closed, in which case the issue is recorded as
    /// pending and the caller must NOT write it.
    pub(crate) fn claim(&mut self, issue_id: Uuid, now: Instant) -> bool {
        if let Some(&at) = self.gate.get(&issue_id) {
            if now.saturating_duration_since(at) < self.interval {
                self.defer_until(issue_id, at + self.interval);
                return false;
            }
        }
        self.stamp(issue_id, now);
        self.unqueue(issue_id);
        true
    }

    /// Up to `max` pending issues whose gate has reopened by `now`, longest-
    /// waiting first. Each leaves the pending set and has its gate stamped at
    /// `now`, exactly as if a batch had claimed it.
    pub(crate) fn take_due(&mut self, now: Instant, max: usize) -> Vec<Uuid> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.queue.first() {
                Some(&(due, issue_id)) if due <= now => {
                    self.queue.pop_first();
                    self.pending.remove(&issue_id);
                    self.stamp(issue_id, now);
                    out.push(issue_id);
                }
                _ => break,
            }
        }
        out
    }

    /// A claimed write-back failed at `now`: pend it again, due one interval
    /// later like any other, so a transient error delays it instead of
    /// losing it.
    pub(crate) fn retry_later(&mut self, issue_id: Uuid, now: Instant) {
        self.defer_until(issue_id, now + self.interval);
    }

    /// Pend `issue_ids` due at `now`, for the next drain to re-check (the
    /// startup recovery). Already-pending issues keep their entry. Returns
    /// how many were added.
    pub(crate) fn seed(&mut self, issue_ids: &[Uuid], now: Instant) -> usize {
        let mut added = 0;
        for &issue_id in issue_ids {
            if !self.pending.contains_key(&issue_id) {
                self.defer_until(issue_id, now);
                added += 1;
            }
        }
        added
    }

    pub(crate) fn is_pending(&self, issue_id: Uuid) -> bool {
        self.pending.contains_key(&issue_id)
    }

    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Entries the cap has dropped since the last call.
    pub(crate) fn take_evicted(&mut self) -> u64 {
        std::mem::take(&mut self.evicted)
    }

    fn stamp(&mut self, issue_id: Uuid, now: Instant) {
        if self.gate.len() >= self.gate_cap && !self.gate.contains_key(&issue_id) {
            self.gate.clear();
        }
        self.gate.insert(issue_id, now);
    }

    fn defer_until(&mut self, issue_id: Uuid, due: Instant) {
        // Already pending: keep the entry it has. Its due instant came from
        // the same gate stamp, and one write-back of the then-current count
        // covers every occurrence since.
        if self.pending.contains_key(&issue_id) {
            return;
        }
        if self.pending.len() >= self.pending_cap {
            if let Some((_, oldest)) = self.queue.pop_first() {
                self.pending.remove(&oldest);
                self.evicted += 1;
            }
        }
        self.pending.insert(issue_id, due);
        self.queue.insert((due, issue_id));
    }

    fn unqueue(&mut self, issue_id: Uuid) {
        if let Some(due) = self.pending.remove(&issue_id) {
            self.queue.remove(&(due, issue_id));
        }
    }
}

/// The process-wide throttle. Process-global rather than per-worker so N
/// workers throttle to one write-back per issue per interval combined.
fn global_throttle() -> &'static Mutex<Throttle> {
    static THROTTLE: OnceLock<Mutex<Throttle>> = OnceLock::new();
    THROTTLE.get_or_init(|| Mutex::new(Throttle::new(WRITE_BACK_INTERVAL, GATE_CAP, PENDING_CAP)))
}

fn lock(m: &Mutex<Throttle>) -> MutexGuard<'_, Throttle> {
    // A panic elsewhere poisoned it. The write-back is best-effort, so
    // recover rather than propagate a panic into the write path.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn throttle() -> MutexGuard<'static, Throttle> {
    lock(global_throttle())
}

/// Which throttle the write paths use, and what clock they read.
///
/// Production is always [`Ctx::global`]: the process-wide throttle and the
/// real clock. The seam exists so a test can drive `process_batch`'s own
/// drain (stage 6b) with a private throttle and a clock one interval ahead,
/// without sleeping and without sharing — or racing — the process-wide
/// pending set with every other test in the binary.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    throttle: &'a Mutex<Throttle>,
    offset: Duration,
}

impl Ctx<'static> {
    pub(crate) fn global() -> Self {
        Ctx {
            throttle: global_throttle(),
            offset: Duration::ZERO,
        }
    }
}

impl<'a> Ctx<'a> {
    #[cfg(test)]
    pub(crate) fn for_test(throttle: &'a Mutex<Throttle>, offset: Duration) -> Self {
        Ctx { throttle, offset }
    }

    fn now(&self) -> Instant {
        Instant::now() + self.offset
    }

    /// See [`Throttle::claim`]; the hot paths' entry point.
    pub(crate) fn claim(&self, issue_id: Uuid) -> bool {
        lock(self.throttle).claim(issue_id, self.now())
    }

    /// See [`Throttle::retry_later`].
    pub(crate) fn retry_later(&self, issue_id: Uuid) {
        lock(self.throttle).retry_later(issue_id, self.now());
    }

    /// Write back up to `max` due pending issues on `conn`. Returns how many
    /// were written.
    ///
    /// `conn` must not be inside an open transaction: the statements are only
    /// cycle-free because each one commits on its own (see the module docs).
    pub(crate) async fn drain(
        &self,
        conn: &mut AsyncPgConnection,
        redis: &RedisStore,
        max: usize,
    ) -> usize {
        let now = self.now();
        // Bound to a local so the guard is released before the first await.
        let due = lock(self.throttle).take_due(now, max);
        write_back(self.throttle, conn, redis, &due, now).await
    }
}

/// [`Ctx::claim`] on the process-wide throttle (the per-item path).
pub(crate) fn claim(issue_id: Uuid) -> bool {
    Ctx::global().claim(issue_id)
}

/// [`Ctx::retry_later`] on the process-wide throttle (the per-item path).
pub(crate) fn retry_later(issue_id: Uuid) {
    Ctx::global().retry_later(issue_id);
}

/// Whether `issue_id` is waiting for a write-back.
pub fn is_pending(issue_id: Uuid) -> bool {
    throttle().is_pending(issue_id)
}

/// How many issues are waiting for a write-back.
pub fn pending_len() -> usize {
    throttle().pending_len()
}

/// The flusher's tick: write back up to `max` pending issues due by `now`.
/// `pub`, with the clock passed in, so tests can run a tick one interval
/// ahead instead of sleeping through it.
///
/// Checks out a connection only when something is due, and hands it back
/// every [`ROWS_PER_CHECKOUT`] rows. If a checkout fails, every issue not yet
/// written goes back into the pending set and the error is returned.
pub async fn flush_due(
    pool: &PgPool,
    redis: &RedisStore,
    now: Instant,
    max: usize,
) -> anyhow::Result<usize> {
    let due = throttle().take_due(now, max);
    let mut written = 0;
    for (i, chunk) in due.chunks(ROWS_PER_CHECKOUT).enumerate() {
        let mut conn = match sauron_db::conn(pool).await {
            Ok(c) => c,
            Err(e) => {
                let mut t = throttle();
                for &issue_id in &due[i * ROWS_PER_CHECKOUT..] {
                    t.retry_later(issue_id, now);
                }
                return Err(e);
            }
        };
        written += write_back(global_throttle(), &mut conn, redis, chunk, now).await;
        // `conn` drops here: back to the pool before the next chunk.
    }
    Ok(written)
}

/// Pend every issue an event landed on since `since`, due now, so the next
/// drain re-checks its count. The flusher's first pass; `pub` for tests.
///
/// One query, newest first and capped at [`SEED_CAP`]. It reads
/// `last_event_at` (the ingest clock, moved only when an event lands) rather
/// than `last_seen` (the SDK's clock): an offline client's backdated batch
/// still moved the estimate, but would not move `last_seen` into the window.
pub async fn seed_recent(pool: &PgPool, since: chrono::DateTime<Utc>) -> anyhow::Result<usize> {
    let mut conn = sauron_db::conn(pool).await?;
    let ids = repo::issue_ids_with_events_since(&mut conn, since, SEED_CAP as i64).await?;
    drop(conn);
    Ok(throttle().seed(&ids, Instant::now()))
}

/// One `PFCOUNT` and one single-row autocommit `UPDATE` per issue, in turn.
/// Deliberately NOT one multi-row statement; see the module docs.
async fn write_back(
    throttle: &Mutex<Throttle>,
    conn: &mut AsyncPgConnection,
    redis: &RedisStore,
    issue_ids: &[Uuid],
    now: Instant,
) -> usize {
    let mut written = 0;
    let mut failed: Vec<Uuid> = Vec::new();
    let mut first_error: Option<String> = None;
    for &issue_id in issue_ids {
        let outcome = match redis
            .pf_count(&keys::issue_users(&issue_id.to_string()))
            .await
        {
            Ok(n) => repo::set_issue_users_seen(conn, issue_id, n)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match outcome {
            Ok(()) => written += 1,
            Err(e) => {
                first_error.get_or_insert(e);
                failed.push(issue_id);
            }
        }
    }
    if !failed.is_empty() {
        // One line per drain, not per issue: an outage would otherwise log
        // every pending issue every tick.
        warn!(
            failed = failed.len(),
            error = first_error.as_deref().unwrap_or(""),
            "users_seen write-back failed; those issues stay pending for the next interval"
        );
        let mut t = lock(throttle);
        for issue_id in failed {
            t.retry_later(issue_id, now);
        }
    }
    written
}

/// Spawn the flusher: first re-check the issues active just before startup
/// (see [`seed_recent`]), then every [`FLUSH_TICK`] write back the pending
/// issues whose gate has reopened. One per process, beside the worker pool it
/// serves (the pending set is process-global).
///
/// The startup re-check runs INSIDE the task, never before `main` goes on:
/// `sauron-ingest` must start and buffer into Redis through a Postgres
/// outage. If the query fails it is retried with a doubling backoff
/// ([`SeedRetry`]) against the same window (anchored at startup, not at the
/// retry), and abandoned with an `error!` after [`SEED_ATTEMPTS`] failures.
pub fn spawn_flusher(pool: PgPool, redis: RedisStore) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(
            interval_secs = WRITE_BACK_INTERVAL.as_secs(),
            "users_seen flusher running"
        );
        let since = Utc::now()
            - chrono::Duration::from_std(RESTART_RECHECK).unwrap_or(chrono::Duration::minutes(10));
        let mut seed = SeedRetry::new(Instant::now());
        let mut tick = tokio::time::interval(FLUSH_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if seed.due(Instant::now()) {
                match seed_recent(&pool, since).await {
                    Ok(n) => {
                        seed.succeeded();
                        info!(
                            issues = n,
                            cap = SEED_CAP,
                            "users_seen: re-checking issues active in the 10 minutes before startup"
                        );
                    }
                    Err(e) => match seed.failed(Instant::now()) {
                        Some(wait) => warn!(
                            error = %e,
                            retry_in_secs = wait.as_secs(),
                            "users_seen startup re-check failed; retrying"
                        ),
                        None => error!(
                            error = %e,
                            attempts = SEED_ATTEMPTS,
                            "users_seen startup re-check gave up: issues whose write-back was \
                             pending when ingest last stopped converge only when a new user \
                             arrives. Run `sauron-migrate backfill-users-seen` to repair them"
                        ),
                    },
                }
            }
            if let Err(e) = flush_due(&pool, &redis, Instant::now(), DRAIN_PER_TICK).await {
                warn!(error = %e, "users_seen flush: no database connection; pending issues kept");
            }
            let evicted = throttle().take_evicted();
            if evicted > 0 {
                warn!(
                    evicted,
                    cap = PENDING_CAP,
                    "users_seen pending set overflowed: the longest-waiting issues were dropped \
                     and keep their last written count until a new user moves it"
                );
            }
        }
    })
}

/// The startup re-check's retry schedule: due at once, then after each
/// failure a wait of one [`WRITE_BACK_INTERVAL`] doubling up to
/// [`SEED_BACKOFF_MAX`], abandoned after [`SEED_ATTEMPTS`] failures.
#[derive(Debug)]
struct SeedRetry {
    failures: u32,
    next_at: Instant,
    done: bool,
}

impl SeedRetry {
    fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            next_at: now,
            done: false,
        }
    }

    fn due(&self, now: Instant) -> bool {
        !self.done && now >= self.next_at
    }

    fn succeeded(&mut self) {
        self.done = true;
    }

    /// Record a failure at `now`. `Some(wait)` until the attempts run out,
    /// then `None`, after which it is never due again.
    fn failed(&mut self, now: Instant) -> Option<Duration> {
        self.failures += 1;
        if self.failures >= SEED_ATTEMPTS {
            self.done = true;
            return None;
        }
        let wait = WRITE_BACK_INTERVAL
            .saturating_mul(1 << (self.failures - 1))
            .min(SEED_BACKOFF_MAX);
        self.next_at = now + wait;
        Some(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I: Duration = Duration::from_secs(5);

    #[test]
    fn the_startup_recheck_backs_off_doubling_and_gives_up() {
        let t0 = Instant::now();
        let mut r = SeedRetry::new(t0);
        assert!(r.due(t0), "the first attempt is immediate");

        let mut at = t0;
        let mut waits = Vec::new();
        while let Some(w) = r.failed(at) {
            assert!(
                !r.due(at + w - Duration::from_millis(1)),
                "not before the wait"
            );
            at += w;
            assert!(r.due(at));
            waits.push(w.as_secs());
        }
        assert_eq!(waits, vec![5, 10, 20, 40], "doubling from one interval");
        assert!(
            !r.due(at + Duration::from_secs(3600)),
            "after five failures it never runs again"
        );

        let mut ok = SeedRetry::new(t0);
        ok.succeeded();
        assert!(!ok.due(t0), "a success is final");
    }

    fn t(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn an_open_gate_writes_now_and_a_closed_one_records_pending() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let id = Uuid::new_v4();

        assert!(th.claim(id, t0), "first sighting: the gate is open");
        assert!(
            !th.is_pending(id),
            "a write-back now leaves nothing pending"
        );

        assert!(!th.claim(id, t(t0, 1)), "inside the interval: gate closed");
        assert!(
            th.is_pending(id),
            "a closed gate must record the issue, not drop it"
        );
        assert!(!th.claim(id, t(t0, 2)));
        assert_eq!(th.pending_len(), 1, "repeat deferrals keep one entry");
    }

    #[test]
    fn once_due_the_drain_returns_it_and_stamps_the_gate() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let id = Uuid::new_v4();
        assert!(th.claim(id, t0));
        assert!(!th.claim(id, t(t0, 1)));

        assert!(
            th.take_due(t(t0, 4), usize::MAX).is_empty(),
            "not before the gate reopens"
        );
        assert_eq!(th.take_due(t(t0, 5), usize::MAX), vec![id]);
        assert!(!th.is_pending(id));
        assert!(
            th.take_due(t(t0, 60), usize::MAX).is_empty(),
            "a drained issue is taken once"
        );

        // The drain stamped the gate at t0+5, so the interval counts from there.
        assert!(!th.claim(id, t(t0, 9)), "gate stamped by the drain");
        assert!(th.is_pending(id));
        assert!(th.claim(id, t(t0, 10)), "and open again one interval later");
        assert!(
            !th.is_pending(id),
            "a direct write-back clears the pending entry it covers"
        );
    }

    #[test]
    fn the_drain_is_bounded_per_call_and_takes_the_longest_waiting_first() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        // Gates stamped at t0+2, t0, t0+1 → due at t0+7, t0+5, t0+6.
        assert!(th.claim(b, t0));
        assert!(th.claim(c, t(t0, 1)));
        assert!(th.claim(a, t(t0, 2)));
        for id in [a, b, c] {
            assert!(!th.claim(id, t(t0, 3)));
        }

        assert_eq!(th.take_due(t(t0, 10), 2), vec![b, c]);
        assert_eq!(th.take_due(t(t0, 10), 2), vec![a]);
        assert_eq!(th.pending_len(), 0);
    }

    #[test]
    fn the_pending_set_is_bounded_and_drops_the_longest_waiting() {
        let mut th = Throttle::new(I, 100, 2);
        let t0 = Instant::now();
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for (i, &id) in ids.iter().enumerate() {
            assert!(th.claim(id, t(t0, i as u64)));
        }
        for &id in &ids {
            assert!(!th.claim(id, t(t0, 3)));
        }

        assert_eq!(th.pending_len(), 2, "never above the cap");
        assert!(
            !th.is_pending(ids[0]),
            "the longest-waiting entry is the one dropped"
        );
        assert!(th.is_pending(ids[1]) && th.is_pending(ids[2]));
        assert_eq!(th.take_evicted(), 1);
        assert_eq!(th.take_evicted(), 0, "the counter resets once read");
    }

    #[test]
    fn a_failed_write_back_is_retried_one_interval_later() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let id = Uuid::new_v4();
        assert!(th.claim(id, t0));
        assert!(!th.claim(id, t(t0, 1)));
        assert_eq!(th.take_due(t(t0, 5), usize::MAX), vec![id]);

        th.retry_later(id, t(t0, 5));
        assert!(th.is_pending(id));
        assert!(th.take_due(t(t0, 9), usize::MAX).is_empty());
        assert_eq!(th.take_due(t(t0, 10), usize::MAX), vec![id]);
    }

    #[test]
    fn the_gate_is_bounded_by_clearing_it() {
        let mut th = Throttle::new(I, 2, 100);
        let t0 = Instant::now();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(th.claim(a, t0));
        assert!(th.claim(b, t0));
        assert!(th.claim(c, t0), "a third issue clears the full gate");
        assert!(
            th.claim(a, t(t0, 1)),
            "a cleared issue may write back once more — the documented cost"
        );
        assert!(
            !th.claim(c, t(t0, 1)),
            "the newest stamp survives the clear"
        );
    }

    #[test]
    fn a_seeded_issue_is_due_at_once_and_a_pending_one_keeps_its_entry() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let (gated, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(th.claim(gated, t0));
        assert!(!th.claim(gated, t(t0, 1)), "pending, due at t0+5");

        assert_eq!(
            th.seed(&[gated, fresh], t(t0, 2)),
            1,
            "only the issue not already pending is added"
        );
        assert_eq!(
            th.take_due(t(t0, 2), usize::MAX),
            vec![fresh],
            "a seeded issue is re-checked on the next drain"
        );
        assert_eq!(th.take_due(t(t0, 5), usize::MAX), vec![gated]);
    }

    /// The regression guard for the deadlock constraint, structurally: the
    /// hot paths write back only what `claim` returned true for, so a gated
    /// issue must come back false AND be pending — never in the write set.
    #[test]
    fn a_gated_issue_is_never_handed_to_the_hot_path_write_back() {
        let mut th = Throttle::new(I, 100, 100);
        let t0 = Instant::now();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        for &id in &ids[..2] {
            assert!(th.claim(id, t0));
        }
        // A later batch touches all four: two gated, two new.
        let write_now: Vec<Uuid> = ids
            .iter()
            .copied()
            .filter(|&id| th.claim(id, t(t0, 1)))
            .collect();
        assert_eq!(write_now, ids[2..].to_vec());
        assert!(ids[..2].iter().all(|&id| th.is_pending(id)));
        assert!(ids[2..].iter().all(|&id| !th.is_pending(id)));
    }
}
