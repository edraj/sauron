//! Records every (app, env, release) the worker sees into `app_releases`,
//! throttled per key so a busy release costs one UPDATE per minute per
//! worker rather than one per job.
//!
//! Two entry points, one throttle: [`note_release`] is the per-item form
//! called from [`crate::process::process_job`] (the fallback path), and
//! [`note_releases`] is the batch form called from
//! [`crate::batch::process_batch`] (the primary write path). Both go through
//! the same `SEEN` cache, so whichever path actually runs for a given job,
//! the throttle sees it.
//!
//! A release row is a convenience, the event is the data: neither caller may
//! fail a job or a batch because of one. [`note_release`] returns the error so
//! its caller can log it; [`note_releases`] logs each failure itself, with the
//! app and release it belongs to, and returns nothing — it attempts every
//! distinct key in the batch even after one fails, because one bad key must
//! not starve every other release in the batch of its upsert, and by the time
//! it returns there is nothing left for a caller to do.
//!
//! Both call sites run AFTER their writes have succeeded. See
//! `batch::process_batch` and `process::process_job` for why.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sauron_core::envelope::IngestJob;
use sauron_db::AsyncPgConnection;
use uuid::Uuid;

pub const WRITE_INTERVAL: Duration = Duration::from_secs(60);

/// Upper bound on live throttle keys before [`should_write`] sweeps.
///
/// The steady-state population is apps × environments × releases *still
/// reporting*, which is small — but every one of those factors is attacker- or
/// accident-controlled (a misconfigured sender can put a build hash, or a
/// timestamp, in `release`), and this cache previously had no bound at all, in
/// a process that runs for weeks. 4096 keys is far above any real deployment's
/// working set and still only a few hundred KB.
const MAX_KEYS: usize = 4096;

/// `(app_id, environment_id, release)` — the throttle and upsert identity.
pub(crate) type ReleaseKey = (Uuid, Uuid, String);

/// Last write time per key, plus the bookkeeping that keeps the map bounded
/// without paying for it on every insert. Shared by both call sites (batch
/// and per-item) through [`SEEN`].
pub(crate) struct ReleaseCache {
    seen: HashMap<ReleaseKey, Instant>,
    /// When the sweep last ran. The sweep is O(n) **under the process-global
    /// mutex**, so it is rate-limited to once per [`WRITE_INTERVAL`]: without
    /// this, more than [`MAX_KEYS`] *fresh* keys made every single insert
    /// `retain` over the whole map (the sweep can only drop entries older than
    /// the interval, so a fresh population frees nothing and the map stays
    /// over the bound), serialising every worker task behind a full scan per
    /// job.
    last_swept: Instant,
    /// How many sweeps have run. Nothing in production reads it; it exists so
    /// the "at most one sweep per interval" bound is something a test can
    /// assert directly rather than infer from `len()`.
    sweeps: u64,
}

impl ReleaseCache {
    fn new(now: Instant) -> Self {
        Self {
            seen: HashMap::new(),
            // Back-dated by one interval so the FIRST overflow may sweep
            // immediately. Stamping `now` here would instead grant a
            // freshly-started worker one full interval of unbounded growth.
            last_swept: now.checked_sub(WRITE_INTERVAL).unwrap_or(now),
            sweeps: 0,
        }
    }
}

static SEEN: Mutex<Option<ReleaseCache>> = Mutex::new(None);

/// The rule every release value passes through before it becomes a key:
/// trim like `str::trim`, and treat an empty result as no release at all.
///
/// One function, both entry points. The ingest edge already applies it, but
/// this process also serves the per-item replay path and (on an upgrade) an
/// edge that predates the rule, so the worker re-applies it rather than
/// trusting it. Borrows rather than allocating — the caller decides whether it
/// needs an owned `String`.
pub(crate) fn clean(release: Option<&str>) -> Option<&str> {
    release.map(str::trim).filter(|s| !s.is_empty())
}

/// Pure throttle decision, separated so it can be unit-tested without a DB.
///
/// Returns `Some(stamp)` when the caller may write — `stamp` being the mark
/// just stored, which [`forget`] needs to undo it safely — and `None` when the
/// key is still inside its window.
///
/// Takes `key` by REFERENCE and clones only on the insert branch: the
/// throttled branch is the common one once a release is busy (that is the
/// entire point of the throttle), and it now costs no allocation at all.
///
/// # Keeping the map bounded
///
/// Over [`MAX_KEYS`] entries the map is swept, but at most once per
/// [`WRITE_INTERVAL`] — see [`ReleaseCache::last_swept`]. The sweep drops
/// everything older than `WRITE_INTERVAL`, which changes no decision: those
/// keys are past their throttle and would answer `Some` on the next sighting
/// regardless. If the map is STILL over the bound afterwards — i.e. the
/// population is over `MAX_KEYS` genuinely-live keys, which is what a
/// misconfigured sender putting a timestamp in `release` looks like — it is
/// cleared outright, because a bound that a caller can hold the process above
/// indefinitely is not a bound. The cost of clearing is bounded and small:
/// throttle state is not data, and losing it only buys some redundant upserts
/// (`upsert_seen` is idempotent) until the map refills.
pub(crate) fn should_write(
    cache: &mut ReleaseCache,
    key: &ReleaseKey,
    now: Instant,
) -> Option<Instant> {
    if let Some(last) = cache.seen.get(key) {
        if now.duration_since(*last) < WRITE_INTERVAL {
            return None;
        }
    }
    cache.seen.insert(key.clone(), now);
    if cache.seen.len() > MAX_KEYS && now.duration_since(cache.last_swept) >= WRITE_INTERVAL {
        cache.last_swept = now;
        cache.sweeps += 1;
        cache
            .seen
            .retain(|_, last| now.duration_since(*last) < WRITE_INTERVAL);
        if cache.seen.len() > MAX_KEYS {
            cache.seen.clear();
        }
    }
    Some(now)
}

/// Undo a `should_write` mark, e.g. after the write it gated turned out to
/// fail. A failed upsert must not leave the key throttled — otherwise a
/// release that hit one transient DB error would go unrecorded for a full
/// `WRITE_INTERVAL`, even though nothing was ever actually written for it.
///
/// `stamp` is what [`should_write`] returned, and the entry is removed ONLY if
/// it still holds that exact mark. The lock is released across the upsert's
/// `.await` (it must be — it is not an async mutex), so by the time a failure
/// gets here another worker task may have re-marked the same key and had its
/// own write SUCCEED. An unconditional `remove` would delete that newer,
/// legitimate mark, and the throttle would then let the same key write again
/// immediately — the failure of one task silently un-throttling another's key.
fn forget(cache: &mut ReleaseCache, key: &ReleaseKey, stamp: Instant) {
    if cache.seen.get(key) == Some(&stamp) {
        cache.seen.remove(key);
    }
}

/// The shared body of both entry points: throttle, upsert, un-mark on failure.
/// Takes the key already built, so the batch path — whose `distinct_keys` map
/// owns its `String`s — does not allocate a second copy of every release.
async fn upsert_throttled(
    conn: &mut AsyncPgConnection,
    key: &ReleaseKey,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let (app_id, environment_id, release) = key;
    let stamp = {
        let now = Instant::now();
        let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
        let cache = guard.get_or_insert_with(|| ReleaseCache::new(now));
        should_write(cache, key, now)
    };
    let Some(stamp) = stamp else {
        return Ok(());
    };
    if let Err(e) =
        sauron_db::releases::upsert_seen(conn, *app_id, Some(*environment_id), release, at).await
    {
        // The throttle was marked optimistically above, before the write was
        // known to succeed. It didn't, so undo that mark rather than silently
        // dropping this release for the rest of the window — but only if the
        // mark is still ours (see `forget`). The lock is taken and released
        // here only — never held across the `.await` above.
        let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cache) = guard.as_mut() {
            forget(cache, key, stamp);
        }
        return Err(e.into());
    }
    Ok(())
}

/// Upsert `(app_id, environment_id, release)` if this worker has not written
/// it in the last minute. A missing or blank (after trimming) release writes
/// nothing.
pub async fn note_release(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Uuid,
    release: Option<&str>,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(release) = clean(release) else {
        return Ok(());
    };
    upsert_throttled(conn, &(app_id, environment_id, release.to_string()), at).await
}

/// Dedups identical `(app_id, environment_id, release)` keys within `jobs`,
/// dropping jobs with no (or blank, after trimming) release, and keeping the
/// LATEST `received_at` per key (rather than `Utc::now()`) so `last_seen_at`
/// reflects the events actually being recorded, not the moment the batch
/// happened to be flushed. Pure, so it is unit-tested without a DB.
fn distinct_keys<'a>(
    jobs: impl IntoIterator<Item = &'a IngestJob>,
) -> HashMap<ReleaseKey, DateTime<Utc>> {
    let mut distinct: HashMap<ReleaseKey, DateTime<Utc>> = HashMap::new();
    for job in jobs {
        let Some(release) = clean(job.release.as_deref()) else {
            continue;
        };
        let key = (job.app_id, job.environment_id, release.to_string());
        distinct
            .entry(key)
            .and_modify(|at| *at = (*at).max(job.received_at))
            .or_insert(job.received_at);
    }
    distinct
}

/// Batch form: dedups identical `(app_id, environment_id, release)` keys
/// within the batch first, so a batch of 50 events on one release costs one
/// throttle check and (at most) one upsert instead of 50.
///
/// Infallible by construction — see the module docs. Every distinct key is
/// attempted even if an earlier one fails, and each failure is logged here
/// with the app and release it belongs to.
pub async fn note_releases<'a>(
    conn: &mut AsyncPgConnection,
    jobs: impl IntoIterator<Item = &'a IngestJob>,
) {
    for (key, at) in distinct_keys(jobs) {
        if let Err(e) = upsert_throttled(conn, &key, at).await {
            tracing::warn!(
                error = %e,
                app_id = %key.0,
                release = %key.2,
                "app_releases upsert failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauron_core::envelope::{AnalyticsItem, EnvelopeContext, EnvelopeItem};
    use std::time::{Duration, Instant};

    /// A throttle key on the nil app/env, which is all these tests need: the
    /// release string is the only part they vary.
    fn key(release: &str) -> ReleaseKey {
        (Uuid::nil(), Uuid::nil(), release.to_string())
    }

    #[test]
    fn first_sight_writes_then_throttles_then_writes_again() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        let k = key("1.0.0");
        assert_eq!(should_write(&mut cache, &k, t0), Some(t0));
        assert_eq!(
            should_write(&mut cache, &k, t0 + Duration::from_secs(10)),
            None
        );
        let past = t0 + WRITE_INTERVAL + Duration::from_secs(1);
        assert_eq!(should_write(&mut cache, &k, past), Some(past));
    }

    #[test]
    fn different_keys_do_not_share_a_throttle() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        assert!(should_write(&mut cache, &key("a"), t0).is_some());
        assert!(should_write(&mut cache, &key("b"), t0).is_some());
    }

    #[test]
    fn forget_clears_the_throttle_immediately() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        let k = key("1.0.0");

        let stamp = should_write(&mut cache, &k, t0).expect("first sight writes");
        // Still inside the throttle window: a second call is suppressed.
        assert_eq!(
            should_write(&mut cache, &k, t0 + Duration::from_millis(1)),
            None
        );

        forget(&mut cache, &k, stamp);

        // forget() undoes the mark, so should_write answers again right away
        // — no need to wait out WRITE_INTERVAL. This is the behavior
        // note_release relies on when an upsert fails after the throttle was
        // optimistically marked.
        assert!(should_write(&mut cache, &k, t0 + Duration::from_millis(2)).is_some());
    }

    /// The stale-forget race. `SEEN`'s mutex is a `std::sync::Mutex`, so it is
    /// released across the upsert's `.await`: between one task marking a key
    /// and that task's write failing, another task can re-mark the same key
    /// and have its own write SUCCEED. The failing task must not then delete
    /// the winner's mark.
    ///
    /// Simulated rather than threaded: the interleaving is entirely described
    /// by which stamp is in the map when `forget` runs, and a real two-task
    /// race would only make the same assertion nondeterministically.
    #[test]
    fn forget_leaves_a_newer_mark_in_place() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        let k = key("1.0.0");

        // Task A marks the key, then stalls inside its upsert.
        let stale = should_write(&mut cache, &k, t0).expect("A marks");

        // Task B's window opens, B marks the key and its write succeeds.
        let fresh = t0 + WRITE_INTERVAL + Duration::from_secs(1);
        let newer = should_write(&mut cache, &k, fresh).expect("B marks");
        assert_ne!(stale, newer);

        // Only now does A's write come back as a failure.
        forget(&mut cache, &k, stale);

        // B's mark must survive: it stands for a write that really happened.
        assert_eq!(cache.seen.get(&k), Some(&newer));
        assert_eq!(
            should_write(&mut cache, &k, fresh + Duration::from_secs(1)),
            None,
            "A's stale forget un-throttled a key B had legitimately written"
        );
    }

    /// The cache had no bound at all, in a process that runs for weeks.
    /// Anything older than `WRITE_INTERVAL` is past its throttle and would
    /// answer `Some` on its next sighting regardless, so sweeping it changes
    /// no decision — only memory.
    #[test]
    fn the_cache_sweeps_expired_keys_once_it_outgrows_max_keys() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        // Fill to EXACTLY the bound, not past it: `should_write` sweeps only
        // when `len() > MAX_KEYS`, so `0..=MAX_KEYS` would trip a sweep during
        // the fill — with every key fresh, that sweep frees nothing and falls
        // through to the `clear()`, and the assertions below would then hold
        // even if the age-based `retain` were an unconditional `clear()`. One
        // key short of the bound, the fill sweeps nothing and the sweep under
        // test is the one the last insert triggers.
        for i in 0..MAX_KEYS {
            assert!(should_write(&mut cache, &key(&i.to_string()), t0).is_some());
        }
        assert_eq!(cache.sweeps, 0, "the fill itself must not sweep");
        assert_eq!(cache.seen.len(), MAX_KEYS);

        // One more key, a full interval later: now the map is over the bound
        // AND the rate limit has elapsed, so the sweep runs — and every key
        // from the fill is past its throttle window, so `retain` drops all of
        // them and the fresh key alone survives. `len() == 1`, not `0`, is
        // what separates the age-based `retain` from a blanket `clear()`.
        let later = t0 + WRITE_INTERVAL + Duration::from_secs(1);
        assert!(should_write(&mut cache, &key("fresh"), later).is_some());
        assert_eq!(
            cache.seen.len(),
            1,
            "only the fresh key may survive the sweep"
        );
        assert!(cache.seen.contains_key(&key("fresh")));
        assert_eq!(cache.sweeps, 1);
    }

    /// Item 2: the sweep is O(n) and runs under the process-global mutex, so
    /// it must not run on every insert. Before the fix the only thing the
    /// sweep removed was entries older than `WRITE_INTERVAL`; with more than
    /// `MAX_KEYS` *fresh* keys it therefore removed nothing and ran again on
    /// the very next insert — an O(n) scan per job, serialised across every
    /// worker task, for as long as the key population stayed over the bound.
    #[test]
    fn a_flood_of_fresh_keys_sweeps_at_most_once_per_interval() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);

        // Well past the bound, all of them fresh (same instant).
        for i in 0..(MAX_KEYS * 2) {
            assert!(should_write(&mut cache, &key(&i.to_string()), t0).is_some());
        }
        assert_eq!(
            cache.sweeps, 1,
            "one sweep for the whole flood, not one per insert past the bound"
        );

        // Still inside the same interval: no second sweep, however many more
        // distinct keys arrive.
        let same_window = t0 + WRITE_INTERVAL - Duration::from_secs(1);
        for i in 0..(MAX_KEYS * 2) {
            should_write(&mut cache, &key(&format!("b{i}")), same_window);
        }
        assert_eq!(cache.sweeps, 1, "a second sweep inside one interval");

        // A full interval after the first sweep, one more insert may sweep.
        let next_window = t0 + WRITE_INTERVAL + Duration::from_secs(1);
        should_write(&mut cache, &key("next"), next_window);
        assert_eq!(cache.sweeps, 2);
    }

    /// Item 2, the other half: a sweep that frees nothing leaves the map over
    /// the bound, and the bound has to mean something. Losing throttle state
    /// costs only redundant (idempotent) upserts, so the cache is cleared.
    #[test]
    fn a_sweep_that_frees_nothing_clears_the_cache() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);

        for i in 0..=MAX_KEYS {
            assert!(should_write(&mut cache, &key(&i.to_string()), t0).is_some());
        }

        assert_eq!(cache.sweeps, 1, "the overflow must have swept");
        assert!(
            cache.seen.len() <= MAX_KEYS,
            "the bound is not a bound: {} keys after the sweep",
            cache.seen.len()
        );
        assert_eq!(
            cache.seen.len(),
            0,
            "every key was fresh, so the sweep freed nothing and must clear"
        );
    }

    /// Item 3: the throttled path — the common one, once a release is busy —
    /// must not allocate a `String` per job just to look the key up. The
    /// lookup takes the key by reference; only the insert branch clones.
    #[test]
    fn the_throttled_path_looks_up_by_reference() {
        let t0 = Instant::now();
        let mut cache = ReleaseCache::new(t0);
        let k = key("1.0.0");

        assert_eq!(should_write(&mut cache, &k, t0), Some(t0));
        // `k` is still owned by the caller — `should_write` borrowed it, and
        // the cache holds its own clone made on the insert branch only.
        assert_eq!(
            should_write(&mut cache, &k, t0 + Duration::from_secs(1)),
            None
        );
        assert_eq!(cache.seen.len(), 1);
        assert_eq!(k.2, "1.0.0");
    }

    #[test]
    fn clean_matches_the_edges_trim_and_blank_rule() {
        assert_eq!(clean(Some(" 1.4.0 ")), Some("1.4.0"));
        assert_eq!(clean(Some("1.4.0")), Some("1.4.0"));
        assert_eq!(clean(Some("   ")), None);
        assert_eq!(clean(Some("")), None);
        assert_eq!(clean(None), None);
    }

    /// Builds an `IngestJob` directly (no DB, no envelope decoding) — copies
    /// the constructor pattern from `batch.rs`'s `equivalence_tests::job`,
    /// trimmed to the fields `distinct_keys` actually looks at.
    fn job(
        app_id: Uuid,
        environment_id: Uuid,
        release: Option<&str>,
        received_at: DateTime<Utc>,
    ) -> IngestJob {
        IngestJob {
            app_id,
            project_id: Uuid::nil(),
            org_id: Uuid::nil(),
            environment_id,
            release: release.map(str::to_string),
            received_at,
            ip: None,
            user_agent: None,
            context: EnvelopeContext::default(),
            sdk: None,
            item: EnvelopeItem::Event(AnalyticsItem {
                name: "test_event".to_string(),
                distinct_id: "person-1".to_string(),
                properties: serde_json::json!({}),
                timestamp: received_at,
                session_id: None,
                workflow_id: None,
                workflow_name: None,
                screen: None,
                tags: serde_json::json!({}),
                contexts: serde_json::json!({}),
                extra: serde_json::json!({}),
            }),
        }
    }

    #[test]
    fn distinct_keys_drops_blank_releases_and_keeps_latest_received_at() {
        let app_id = Uuid::new_v4();
        let env_id = Uuid::new_v4();
        let t0 = Utc::now();

        let jobs = [
            job(app_id, env_id, Some("1.0.0"), t0),
            // Same key, later received_at — the max should win.
            job(
                app_id,
                env_id,
                Some("1.0.0"),
                t0 + chrono::Duration::seconds(30),
            ),
            // Same key, earlier received_at — must not overwrite the max.
            job(
                app_id,
                env_id,
                Some("1.0.0"),
                t0 - chrono::Duration::seconds(30),
            ),
            // Dropped: no release at all.
            job(app_id, env_id, None, t0),
            // Dropped: whitespace-only release.
            job(app_id, env_id, Some("   "), t0),
            // A distinct key (different release) survives on its own.
            job(app_id, env_id, Some("2.0.0"), t0),
        ];

        let distinct = distinct_keys(jobs.iter());

        assert_eq!(distinct.len(), 2);
        assert_eq!(
            distinct[&(app_id, env_id, "1.0.0".to_string())],
            t0 + chrono::Duration::seconds(30)
        );
        assert_eq!(distinct[&(app_id, env_id, "2.0.0".to_string())], t0);
    }
}
