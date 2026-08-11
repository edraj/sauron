//! The terminal tier: writing a failure somewhere a human can act on it.
//!
//! A failure arrives here when retrying is finished with it — either because it
//! was never retryable (malformed JSON cannot become valid JSON) or because it
//! burned all of [`MAX_ATTEMPTS`]. It is folded into a fingerprint group in
//! `ingest_failures` and its payload retained, up to a per-group cap.
//!
//! **The Redis dead-letter stream survives this feature**, narrowed to one job:
//! the backstop for when the Postgres write itself fails. That is not a
//! theoretical case — the transient failure most worth retrying is Postgres
//! being unavailable, which is exactly when `record` cannot write. Without the
//! fallback, a database outage would turn into silent event loss, which is the
//! failure mode this whole design exists to remove.
//!
//! [`MAX_ATTEMPTS`]: crate::classify::MAX_ATTEMPTS

use sha2::{Digest, Sha256};
use tracing::{error, warn};
use uuid::Uuid;

use sauron_db::models::NewIngestFailure;
use sauron_db::{repo, PgPool};
use sauron_redis::RedisStore;

use crate::classify::{normalize_message, Classified};

/// Ceiling on retained payloads per fingerprint group.
///
/// Bounds the storage a single runaway failure can claim while keeping enough
/// to make "fix the root cause, then retry" mean something. Occurrences past
/// the cap still count — they are reported as `dropped` on the page rather than
/// quietly forgotten.
pub fn payload_cap() -> i64 {
    static N: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_FAILURE_PAYLOAD_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1000)
    })
}

/// How long a failure group outlives its last occurrence.
///
/// A bound in TIME, for the same reason the dead-letter reaper has one: these
/// rows are masked copies of real user events, living outside every retention
/// window the product otherwise enforces. Without this the feature would just
/// relocate the dead-letter stream's unbounded growth into Postgres.
pub fn retention_days() -> i64 {
    static N: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_FAILURE_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(30)
    })
}

/// Everything known about a failure at the moment it goes terminal.
pub struct Terminal<'a> {
    pub class: Classified,
    pub message: &'a str,
    pub payload: &'a str,
    pub attempts: i32,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
}

/// Group key for a failure.
///
/// Hashes the *normalized* message, not the raw one. With the raw message a
/// row number or a UUID makes every occurrence hash differently, and the
/// grouping this design rests on degenerates into one row per event — 242,700
/// rows for one bad deploy, which is the situation being fixed.
pub fn fingerprint(error_kind: &str, message: &str, app_id: Option<Uuid>) -> String {
    let mut h = Sha256::new();
    h.update(error_kind.as_bytes());
    h.update(b"\x00");
    h.update(normalize_message(message).as_bytes());
    h.update(b"\x00");
    h.update(app_id.unwrap_or(Uuid::nil()).as_bytes());
    hex::encode(&h.finalize()[..16])
}

/// Record a terminal failure, falling back to the Redis DLQ if Postgres will
/// not take it.
///
/// Returns `true` if the failure was durably recorded somewhere. A `false`
/// means BOTH sinks refused, and the caller must not ack the stream entry —
/// leaving it pending is the last thing standing between a failure and silent
/// loss.
pub async fn record(pool: &PgPool, redis: &RedisStore, dlq_maxlen: usize, t: Terminal<'_>) -> bool {
    let fp = fingerprint(t.class.error_kind, t.message, t.app_id);
    // Truncated for the same reason the fingerprint normalizes: a multi-megabyte
    // serde error is not more informative than its first lines, and it would be
    // stored on every occurrence.
    let message: String = t.message.chars().take(2000).collect();

    let payload_json = match serde_json::from_str::<serde_json::Value>(t.payload) {
        Ok(v) => Some(v),
        // A payload that does not parse as JSON is precisely the `decode`
        // failure case. Store it as a JSON string so the column stays typed and
        // the admin can still read the bytes that broke.
        Err(_) => Some(serde_json::Value::String(
            t.payload.chars().take(64 * 1024).collect(),
        )),
    };

    let recorded = match sauron_db::conn(pool).await {
        Ok(mut conn) => {
            let new = NewIngestFailure {
                fingerprint: &fp,
                error_kind: t.class.error_kind,
                error_message: &message,
                org_id: t.org_id,
                project_id: t.project_id,
                app_id: t.app_id,
            };
            match repo::record_ingest_failure(
                &mut conn,
                &new,
                payload_json.as_ref(),
                t.attempts,
                payload_cap(),
            )
            .await
            {
                Ok(r) => {
                    if !r.retained {
                        // Never silent. A group at its cap is still counting
                        // occurrences, and the page shows the gap — but the log
                        // is where an operator watching a live incident sees it.
                        warn!(
                            fingerprint = %fp,
                            kind = t.class.error_kind,
                            "failure recorded but payload dropped: group at cap"
                        );
                    }
                    true
                }
                Err(e) => {
                    error!(error = %e, fingerprint = %fp, "failed to record ingest failure");
                    false
                }
            }
        }
        Err(e) => {
            error!(error = %e, "no connection to record ingest failure");
            false
        }
    };

    if recorded {
        return true;
    }

    // Backstop. This is the path a Postgres outage takes, so it must not depend
    // on Postgres in any way.
    match redis.dlq_push(t.payload, dlq_maxlen).await {
        Ok(()) => {
            warn!(
                fingerprint = %fp,
                "ingest failure fell back to the Redis dead-letter stream"
            );
            sauron_telemetry::metrics::entries_deadlettered(1);
            true
        }
        Err(e) => {
            error!(error = %e, "BOTH failure sinks refused; entry stays pending");
            sauron_telemetry::metrics::dlq_write_failures(1);
            false
        }
    }
}

/// Delete failure groups whose last occurrence predates the retention window.
pub async fn reap_once(pool: &PgPool) {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days());
    let mut conn = match sauron_db::conn(pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "ingest failure reap skipped: no connection");
            return;
        }
    };
    match repo::reap_ingest_failures(&mut conn, cutoff).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(removed = n, "reaped aged ingest failure groups"),
        Err(e) => warn!(error = %e, "ingest failure reap failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{kind, Classified, FailureKind};

    fn class(k: &'static str) -> Classified {
        // Constructed directly: the point of these tests is the fingerprint,
        // not the classifier that produced the kind.
        Classified {
            failure: FailureKind::Permanent,
            error_kind: k,
        }
    }

    /// The property the page depends on: one problem is one row.
    #[test]
    fn same_problem_different_ids_is_one_fingerprint() {
        let app = Uuid::new_v4();
        let a = fingerprint(
            class(kind::DB_FK_VIOLATION).error_kind,
            "insert violates fk for app 3fa85f64-5717-4562-b3fc-2c963f66afa6 at row 4821",
            Some(app),
        );
        let b = fingerprint(
            class(kind::DB_FK_VIOLATION).error_kind,
            "insert violates fk for app 00000000-0000-0000-0000-000000000001 at row 7",
            Some(app),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn different_kinds_are_different_groups() {
        let app = Some(Uuid::new_v4());
        assert_ne!(
            fingerprint(kind::DECODE, "bad", app),
            fingerprint(kind::DB_CONSTRAINT, "bad", app),
        );
    }

    /// Two apps hitting the same bug are two groups: they have different owners
    /// and one may be fixed while the other is not.
    #[test]
    fn different_apps_are_different_groups() {
        assert_ne!(
            fingerprint(kind::DECODE, "bad", Some(Uuid::new_v4())),
            fingerprint(kind::DECODE, "bad", Some(Uuid::new_v4())),
        );
    }

    /// Undecodable payloads have no app, and must still group rather than
    /// producing one row per event.
    #[test]
    fn missing_app_id_still_groups() {
        assert_eq!(
            fingerprint(kind::DECODE, "expected value at line 1 column 1", None),
            fingerprint(kind::DECODE, "expected value at line 9 column 3", None),
        );
    }
}
