//! Deciding whether a failed job is worth trying again.
//!
//! The distinction earns its keep because the two failure populations behave in
//! opposite ways. A malformed payload fails *deterministically*: retrying it
//! three times a minute apart spends three minutes and three attempts to reach
//! a guaranteed-identical result. A pool timeout or a deadlock fails
//! *incidentally*: the same job an instant later usually succeeds, and without
//! a retry the event is lost for good — the edge already answered `202`.
//!
//! **Unknown classifies as [`FailureKind::Permanent`].** That reads backwards
//! until you notice that permanent does not mean discarded: it means the job
//! goes straight to `ingest_failures` where a human sees it, instead of
//! spending three minutes hidden in a retry loop first. Fail-visible, not
//! fail-silent.

use std::fmt::Write as _;

/// What to do with a job that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Plausibly succeeds on a later attempt. Gets up to [`MAX_ATTEMPTS`].
    Transient,
    /// Will fail identically forever. Goes to Postgres on the first failure.
    Permanent,
}

/// Retries before a transient failure is treated as terminal.
pub const MAX_ATTEMPTS: i32 = 3;

/// Low-cardinality slugs for `ingest_failures.error_kind`.
///
/// Also the metrics label, which is why they must stay few and fixed: a slug
/// derived from an error *message* would put unbounded cardinality into
/// Prometheus. The raw message travels in its own column instead.
pub mod kind {
    /// The entry never deserialized into an envelope.
    pub const DECODE: &str = "decode";
    /// Serialization failure or deadlock — the retryable database errors.
    pub const DB_CONTENTION: &str = "db_contention";
    /// The connection or pool was unavailable.
    pub const DB_UNAVAILABLE: &str = "db_unavailable";
    /// Referenced a row that does not exist (typically an unknown `app_id`).
    pub const DB_FK_VIOLATION: &str = "db_fk_violation";
    /// Violated a check, NOT NULL, or unique constraint.
    pub const DB_CONSTRAINT: &str = "db_constraint";
    /// Redis was unreachable mid-job.
    pub const REDIS: &str = "redis";
    /// Anything unrecognised. Permanent by policy, so a human sees it.
    pub const UNKNOWN: &str = "unknown";
}

/// How a failure should be handled, and how it should be grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub failure: FailureKind,
    /// One of [`kind`].
    pub error_kind: &'static str,
}

impl Classified {
    const fn transient(error_kind: &'static str) -> Self {
        Self {
            failure: FailureKind::Transient,
            error_kind,
        }
    }
    const fn permanent(error_kind: &'static str) -> Self {
        Self {
            failure: FailureKind::Permanent,
            error_kind,
        }
    }

    pub fn is_transient(&self) -> bool {
        self.failure == FailureKind::Transient
    }
}

/// The classification for an entry that failed to deserialize.
///
/// Named rather than inferred because this failure happens before there is an
/// error to inspect — `decode_entry` fails in the worker with a serde error and
/// no envelope at all, so there is no `app_id` and nothing to retry.
pub const DECODE_FAILURE: Classified = Classified::permanent(kind::DECODE);

/// Classify an error returned by `process_job`.
///
/// Walks the whole `anyhow` chain rather than inspecting only the outermost
/// error: `process_job` wraps with context, so the diesel error that actually
/// decides this is several links down. Checking only the top would classify
/// every database failure as `unknown`.
pub fn classify(err: &anyhow::Error) -> Classified {
    for cause in err.chain() {
        if let Some(db) = cause.downcast_ref::<diesel::result::Error>() {
            return classify_diesel(db);
        }
        if let Some(redis) = cause.downcast_ref::<redis::RedisError>() {
            // Only connection-level Redis trouble is worth retrying. A type or
            // response error means we asked for something wrong, and asking
            // again produces the same answer.
            return if redis.is_connection_dropped()
                || redis.is_connection_refusal()
                || redis.is_timeout()
            {
                Classified::transient(kind::REDIS)
            } else {
                Classified::permanent(kind::REDIS)
            };
        }
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            return DECODE_FAILURE;
        }
        // Pool exhaustion surfaces as a deadpool error, which carries no useful
        // downcast target across its generic parameters. The string is the only
        // stable handle, and getting this wrong costs a wasted retry rather
        // than a lost event.
        let text = cause.to_string();
        if text.contains("pool timed out") || text.contains("Timeout(") {
            return Classified::transient(kind::DB_UNAVAILABLE);
        }
    }
    Classified::permanent(kind::UNKNOWN)
}

fn classify_diesel(err: &diesel::result::Error) -> Classified {
    use diesel::result::{DatabaseErrorKind as K, Error as E};
    match err {
        E::DatabaseError(K::SerializationFailure, _) => Classified::transient(kind::DB_CONTENTION),
        E::DatabaseError(K::ClosedConnection, _) | E::DatabaseError(K::UnableToSendCommand, _) => {
            Classified::transient(kind::DB_UNAVAILABLE)
        }
        E::DatabaseError(K::ForeignKeyViolation, _) => Classified::permanent(kind::DB_FK_VIOLATION),
        E::DatabaseError(K::UniqueViolation, _)
        | E::DatabaseError(K::CheckViolation, _)
        | E::DatabaseError(K::NotNullViolation, _) => Classified::permanent(kind::DB_CONSTRAINT),
        // `BrokenTransactionManager` means the connection is unusable, not that
        // the work was wrong.
        E::BrokenTransactionManager => Classified::transient(kind::DB_UNAVAILABLE),
        _ => Classified::permanent(kind::UNKNOWN),
    }
}

/// Collapse an error message into something stable enough to group on.
///
/// Strips the parts that differ between two occurrences of the *same* problem —
/// UUIDs, bare integers, quoted literals, and byte offsets. Without this the
/// fingerprint would include `row 4821`, every occurrence would hash
/// differently, and the grouping the whole feature rests on would produce one
/// row per event: 242,700 rows for one bad deploy, which is the situation being
/// fixed.
///
/// Truncated to 512 bytes at a char boundary: a multi-megabyte serde error
/// carries no more grouping signal than its first line, and the untruncated
/// text would be hashed on every single failure.
pub fn normalize_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len().min(512));
    let mut chars = msg.chars().peekable();
    let mut in_quote = false;
    while let Some(c) = chars.next() {
        if out.len() >= 512 {
            break;
        }
        match c {
            '"' | '\'' => {
                if !in_quote {
                    out.push_str("<str>");
                }
                in_quote = !in_quote;
            }
            _ if in_quote => {}
            c if c.is_ascii_hexdigit() || c == '-' => {
                // Consume the whole run, then decide what it was. Deciding
                // per-character would turn a UUID into a dozen placeholders.
                let mut run = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_hexdigit() || n == '-' {
                        run.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if looks_like_uuid(&run) {
                    out.push_str("<uuid>");
                } else if run.chars().any(|c| c.is_ascii_digit())
                    && run.chars().all(|c| c.is_ascii_digit() || c == '-')
                {
                    // `any(digit)` rather than a length floor. A length floor
                    // is what a first cut writes, and it silently exempts
                    // single digits — `row 9` survives normalization while
                    // `row 4821` does not, so the two occurrences of one
                    // problem hash differently and the grouping quietly
                    // degrades into one row per event.
                    out.push_str("<num>");
                } else if run.len() >= 16 && run.chars().all(|c| c.is_ascii_hexdigit()) {
                    out.push_str("<hex>");
                } else {
                    let _ = write!(out, "{run}");
                }
            }
            c if c.is_ascii_digit() => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                    chars.next();
                }
                out.push_str("<num>");
            }
            c => out.push(c),
        }
    }
    out
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy that makes an unrecognised failure visible instead of
    /// spending three minutes hiding it in a retry loop.
    #[test]
    fn unknown_errors_are_permanent() {
        let e = anyhow::anyhow!("something nobody has seen before");
        let c = classify(&e);
        assert_eq!(c.failure, FailureKind::Permanent);
        assert_eq!(c.error_kind, kind::UNKNOWN);
    }

    #[test]
    fn deadlock_is_transient_and_fk_violation_is_not() {
        use diesel::result::{DatabaseErrorKind, Error};
        struct Info(String);
        impl diesel::result::DatabaseErrorInformation for Info {
            fn message(&self) -> &str {
                &self.0
            }
            fn details(&self) -> Option<&str> {
                None
            }
            fn hint(&self) -> Option<&str> {
                None
            }
            fn table_name(&self) -> Option<&str> {
                None
            }
            fn column_name(&self) -> Option<&str> {
                None
            }
            fn constraint_name(&self) -> Option<&str> {
                None
            }
            fn statement_position(&self) -> Option<i32> {
                None
            }
        }

        let deadlock = anyhow::Error::from(Error::DatabaseError(
            DatabaseErrorKind::SerializationFailure,
            Box::new(Info("deadlock detected".into())),
        ));
        assert!(classify(&deadlock).is_transient());

        let fk = anyhow::Error::from(Error::DatabaseError(
            DatabaseErrorKind::ForeignKeyViolation,
            Box::new(Info("apps_fkey".into())),
        ));
        assert!(!classify(&fk).is_transient());
        assert_eq!(classify(&fk).error_kind, kind::DB_FK_VIOLATION);
    }

    /// `process_job` wraps its errors in context, so the deciding error is
    /// several links down the chain. Inspecting only the outermost error would
    /// classify every database failure as `unknown` — the bug this guards.
    #[test]
    fn classification_walks_the_whole_context_chain() {
        use diesel::result::Error;
        let wrapped = anyhow::Error::from(Error::NotFound)
            .context("writing issue")
            .context("processing job");
        assert_eq!(classify(&wrapped).error_kind, kind::UNKNOWN);

        let deadlock = anyhow::Error::from(Error::BrokenTransactionManager)
            .context("writing issue")
            .context("processing job");
        assert!(
            classify(&deadlock).is_transient(),
            "a wrapped connection error must still classify as transient"
        );
    }

    /// The property the whole grouping design rests on: two occurrences of one
    /// problem must hash the same, or the page shows one row per event.
    #[test]
    fn normalization_collapses_volatile_parts() {
        let a = normalize_message(
            "insert failed for app 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0 at row 4821",
        );
        let b = normalize_message(
            "insert failed for app ffffffff-0000-1111-2222-333344445555 at row 9",
        );
        assert_eq!(a, b, "uuid and row number must both normalize away");
        assert!(a.contains("<uuid>"), "got {a}");
    }

    #[test]
    fn normalization_keeps_the_distinguishing_text() {
        let a = normalize_message("column \"foo\" does not exist at row 1");
        let b = normalize_message("relation \"foo\" does not exist at row 1");
        assert_ne!(a, b, "different problems must not collapse together");
    }

    /// A multi-megabyte serde error must not be hashed in full on every
    /// occurrence, and truncation must not split a char.
    #[test]
    fn normalization_truncates_without_panicking() {
        let huge = format!("error {} end", "é".repeat(5000));
        let out = normalize_message(&huge);
        assert!(out.len() <= 512 + 4, "len was {}", out.len());
    }
}
