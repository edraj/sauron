//! Per-replica snapshot of revoked session ids.
//!
//! An access token stays valid until its own `exp` — `JWT_ACCESS_TTL_SECS`,
//! default 900s — so without this, nothing anyone revokes takes effect for up to
//! fifteen minutes: not a logout, not a deactivation, not a password change, not
//! a family kill. This closes that to one poll interval, default 5 seconds.
//!
//! **Any binary that wants the `AuthUser` extractor must now supply one of
//! these, and supplying a permanently-empty one compiles and silently disables
//! revocation for that service.** There is no way for the type system to catch
//! that; it is why this sentence is here rather than only in a design document.
//!
//! Rejected alternatives, so nobody re-proposes them:
//! - A Redis denylist. Fail-open silently disables the control; fail-closed 401s
//!   the whole API on a blip. The shared Redis connection is built with
//!   `set_response_timeout(None)` and is measured at 9-19s per call when Redis is
//!   dead, which is a stall on every authenticated request.
//! - A `users.tokens_valid_from` column. Cannot express per-session granularity,
//!   and adds a pool checkout in front of every handler on a 16-connection pool.
//! - Shortening `JWT_ACCESS_TTL_SECS`. Fifteen times the refresh traffic, and it
//!   still leaves a window.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Default)]
struct Snapshot {
    /// Ids the last successful poll returned.
    polled: HashSet<Uuid>,
    /// Ids this replica revoked itself, with the instant it did so. These cover
    /// the gap between a local revoke and the next poll that can see it.
    local: HashMap<Uuid, Instant>,
    refreshed_at: Option<Instant>,
}

/// A cloneable handle onto one process-wide revoked-session snapshot.
#[derive(Clone, Default)]
pub struct SessionRevocations {
    inner: Arc<RwLock<Snapshot>>,
}

impl SessionRevocations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is this session revoked? Pure memory read — no I/O, ever.
    ///
    /// Runs inside `AuthUser::from_request_parts`, i.e. on every authenticated
    /// request in every route file. The poisoned-lock recovery matches
    /// `local_rate_limit_ok` in `routes/auth.rs` and exists for the same reason:
    /// a naive `.unwrap()` would turn one transient panic under the write guard
    /// into a total API outage.
    pub fn contains(&self, sid: &Uuid) -> bool {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.polled.contains(sid) || guard.local.contains_key(sid)
    }

    /// Record ids this replica just revoked, so the kill takes effect here
    /// immediately rather than at the next poll.
    pub fn mark_revoked(&self, ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        for id in ids {
            guard.local.insert(*id, now);
        }
    }

    /// Swap in a fresh polled set and evict the local entries it has superseded.
    ///
    /// **The eviction rule is the subtle part.** Expressing retention as
    /// wall-clock age against the poll interval is wrong: a locally-marked id is
    /// only certain to be in a poll's result if that poll's *query started
    /// after* the mark. A poll that begins at T-1s, a revocation at T, and a slow
    /// finish at T+6s would evict a 5-second-old local entry using a snapshot
    /// that never contained it — and the revoked session's access token would be
    /// honoured again on this replica until the next poll. A security control
    /// silently ceasing to hold, on exactly the axis this module exists to
    /// establish. So the caller records `Instant::now()` *before* issuing the
    /// query and hands it here.
    ///
    /// The old set is dropped outside the guard, so freeing a large allocation
    /// does not block request tasks parked on `contains`.
    pub fn replace(&self, ids: HashSet<Uuid>, poll_started_at: Instant) {
        let old = {
            let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let old = std::mem::replace(&mut guard.polled, ids);
            guard.local.retain(|_, marked| *marked >= poll_started_at);
            guard.refreshed_at = Some(Instant::now());
            old
        };
        drop(old);
    }

    /// Time since the last **successful** poll; `None` before the first one.
    ///
    /// A failed poll deliberately leaves this stale — the age is the signal that
    /// the control has stopped refreshing.
    ///
    /// **Nothing in this slice reads it.** The signal a stalled poller actually
    /// surfaces on today is `sauron-api`'s task supervisor, which tracks its own
    /// `last_success` per named task and renders it as `last_success_secs` in the
    /// `/health` body — that covers "the poll loop stopped running". This method
    /// covers the narrower case the supervisor cannot see: a loop still ticking
    /// while every poll fails, which leaves the snapshot frozen. Do not delete
    /// this as dead code — the pin below is what keeps it compiling and correct.
    pub fn age(&self) -> Option<Duration> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.refreshed_at.map(|at| at.elapsed())
    }

    /// One poll. Returns the number of ids in the new snapshot.
    ///
    /// Checks out a pooled connection, runs the one query, and **drops the
    /// connection before** swapping the snapshot: the API pool is 16 for the
    /// whole process and a background task must never hold a slot across work it
    /// does not need one for.
    pub async fn refresh(
        &self,
        pool: &sauron_db::PgPool,
        window_secs: i64,
    ) -> anyhow::Result<usize> {
        /// Must match the `LIMIT` in `repo::revoked_session_ids`.
        const POLL_LIMIT: usize = 50_000;

        // Recorded before the query is issued — see `replace`.
        let started_at = Instant::now();
        let ids = {
            let mut conn = sauron_db::conn(pool).await?;
            sauron_db::repo::revoked_session_ids(&mut conn, window_secs).await?
        };
        let count = ids.len();
        if count >= POLL_LIMIT {
            // A silently truncated snapshot is a security control that has
            // stopped working while reporting healthy.
            tracing::error!(
                count,
                "revocation snapshot hit its row limit; sessions revoked beyond it keep working \
                 until their access tokens expire"
            );
        }
        self.replace(ids.into_iter().collect(), started_at);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locally_marked_session_is_revoked_immediately() {
        let revs = SessionRevocations::new();
        let sid = Uuid::new_v4();
        assert!(!revs.contains(&sid));
        revs.mark_revoked(&[sid]);
        assert!(revs.contains(&sid));
    }

    #[test]
    fn replace_evicts_only_marks_older_than_the_polls_start() {
        let revs = SessionRevocations::new();
        let before = Uuid::new_v4();
        revs.mark_revoked(&[before]);

        // The poll's query starts here — after `before` was marked, so a
        // snapshot taken now legitimately supersedes it.
        std::thread::sleep(Duration::from_millis(2));
        let poll_started_at = Instant::now();
        std::thread::sleep(Duration::from_millis(2));

        let after = Uuid::new_v4();
        revs.mark_revoked(&[after]);

        revs.replace(HashSet::new(), poll_started_at);

        assert!(
            !revs.contains(&before),
            "a mark the poll could see is superseded by the poll's result"
        );
        assert!(
            revs.contains(&after),
            "a mark made after the poll started was never in its result and must survive; \
             evicting it un-revokes a killed session until the next poll"
        );
    }

    #[test]
    fn a_polled_id_is_revoked_and_a_later_poll_can_clear_it() {
        let revs = SessionRevocations::new();
        let sid = Uuid::new_v4();
        revs.replace(HashSet::from([sid]), Instant::now());
        assert!(revs.contains(&sid));
        // Once the id ages out of the poll window its access tokens have expired
        // on their own `exp`, so dropping it is correct.
        revs.replace(HashSet::new(), Instant::now());
        assert!(!revs.contains(&sid));
    }

    #[test]
    fn age_is_none_before_the_first_poll_and_some_after() {
        let revs = SessionRevocations::new();
        assert!(revs.age().is_none());
        revs.replace(HashSet::new(), Instant::now());
        assert!(revs.age().is_some());
    }

    #[test]
    fn a_failed_poll_leaves_the_last_good_snapshot_intact() {
        // `refresh` only calls `replace` on the success path, so a failure is
        // expressed here as "no replace happened". The snapshot must not be
        // cleared, or a Postgres blip would re-enable every revoked session.
        let revs = SessionRevocations::new();
        let polled = Uuid::new_v4();
        let local = Uuid::new_v4();
        revs.replace(HashSet::from([polled]), Instant::now());
        revs.mark_revoked(&[local]);

        assert!(revs.contains(&polled));
        assert!(revs.contains(&local));
    }
}
