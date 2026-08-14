//! What a rollup's counters should be, given what survived.
//!
//! Kept pure and separate from the SQL because the arithmetic is where this
//! feature can silently corrupt data: an off-by-one-table recompute produces
//! numbers that look plausible, pass every type check, and are wrong forever
//! with no error anywhere.

use chrono::{DateTime, Utc};

/// Which raw table feeds which counter, taken from the pipeline's three
/// `acc.rollup(…)` call sites rather than from the column names.
///
/// | Signal | `events_count` | `errors_count` |
/// | --- | --- | --- |
/// | `analytics_events` | +1 | 0 |
/// | `error_events` | 0 | +1 |
/// | `transactions` | 0 | 0 |
///
/// **Transactions move neither counter.** The name `events_count` invites
/// counting every signal into it; doing so would inflate every session, device
/// and person on the first purge, and nothing downstream would flag it. This
/// table is the contract, and [`Counts::from_sources`] is the only place it is
/// applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceCounts {
    pub analytics: i64,
    pub errors: i64,
    /// Carried so a caller can assert it was counted and then deliberately
    /// discarded, rather than forgotten.
    pub transactions: i64,
}

/// The recomputed state of one rollup row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub events: i64,
    pub errors: i64,
    /// Surviving rows of EVERY kind, including transactions.
    ///
    /// Separate from `events + errors` because the two answer different
    /// questions, and conflating them deletes live data. A session whose only
    /// signals are transactions legitimately has `events_count = 0` and
    /// `errors_count = 0` — the pipeline creates the session row (the rollup
    /// fold runs for transactions) but bumps neither counter. Treating
    /// zero counters as "no evidence" would delete every transaction-only
    /// session on the first purge that touched the app, destroying data the
    /// operator never selected.
    pub evidence: i64,
    /// `None` when nothing survives — there is no span over an empty set.
    pub first: Option<DateTime<Utc>>,
    pub last: Option<DateTime<Utc>>,
}

impl Counts {
    pub const EMPTY: Self = Self {
        events: 0,
        errors: 0,
        evidence: 0,
        first: None,
        last: None,
    };

    /// Apply the delta table: counters from analytics and errors only,
    /// evidence from all three.
    pub fn from_sources(
        s: SourceCounts,
        first: Option<DateTime<Utc>>,
        last: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            events: s.analytics,
            errors: s.errors,
            evidence: s.analytics + s.errors + s.transactions,
            first,
            last,
        }
    }

    /// Whether the rollup has no surviving evidence at all and should be
    /// deleted rather than kept as an orphan.
    ///
    /// A row describing occurrences that no longer exist is the one outcome
    /// that is actively misleading rather than merely imprecise — the same
    /// judgement `apply_issue_env_stats` documents for its third rejected
    /// option. But see `evidence`: this asks whether any ROW survives, never
    /// whether the counters are zero.
    pub fn is_empty(&self) -> bool {
        self.evidence == 0
    }

    /// Combine hot (Postgres) and cold (Parquet) halves of the same rollup.
    ///
    /// Both halves are required. A Postgres-only recompute would silently
    /// UNDERCOUNT by whatever `sauron-tier` had already exported, turning a
    /// purge intended to correct the numbers into a subtler corruption of
    /// them — and one that looks exactly like success, because the counter
    /// moves in the direction the operator expected.
    pub fn merge(hot: Self, cold: Self) -> Self {
        Self {
            events: hot.events + cold.events,
            errors: hot.errors + cold.errors,
            evidence: hot.evidence + cold.evidence,
            first: min_opt(hot.first, cold.first),
            last: max_opt(hot.last, cold.last),
        }
    }
}

fn min_opt(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (x, None) | (None, x) => x,
    }
}

fn max_opt(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (x, None) | (None, x) => x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, d, 0, 0, 0).unwrap()
    }

    /// The delta table, asserted directly. If someone "fixes" `events` to
    /// include transactions, this is what goes red.
    #[test]
    fn transactions_do_not_count_as_events() {
        let c = Counts::from_sources(
            SourceCounts {
                analytics: 3,
                errors: 2,
                transactions: 99,
            },
            Some(t(1)),
            Some(t(5)),
        );
        assert_eq!(c.events, 3, "transactions must not inflate events_count");
        assert_eq!(c.errors, 2);
    }

    #[test]
    fn errors_come_only_from_error_events() {
        let c = Counts::from_sources(
            SourceCounts {
                analytics: 10,
                errors: 0,
                transactions: 0,
            },
            None,
            None,
        );
        assert_eq!(c.errors, 0);
    }

    fn counts(events: i64, errors: i64, evidence: i64, lo: u32, hi: u32) -> Counts {
        Counts {
            events,
            errors,
            evidence,
            first: Some(t(lo)),
            last: Some(t(hi)),
        }
    }

    #[test]
    fn merge_sums_counts_and_widens_the_span() {
        let m = Counts::merge(counts(2, 1, 3, 5, 9), counts(4, 3, 7, 1, 6));
        assert_eq!((m.events, m.errors, m.evidence), (6, 4, 10));
        assert_eq!(m.first, Some(t(1)), "cold holds the earlier evidence");
        assert_eq!(m.last, Some(t(9)));
    }

    /// The undercount trap: dropping the cold half must be visibly different
    /// from merging it, so a caller that forgets cannot produce the same
    /// answer by accident.
    #[test]
    fn hot_only_undercounts_when_cold_is_nonempty() {
        let hot = counts(2, 0, 2, 5, 9);
        assert_ne!(Counts::merge(hot, counts(40, 0, 40, 1, 2)), hot);
    }

    #[test]
    fn merging_empty_halves_is_identity() {
        let hot = counts(2, 1, 3, 5, 9);
        assert_eq!(Counts::merge(hot, Counts::EMPTY), hot);
        assert_eq!(Counts::merge(Counts::EMPTY, hot), hot);
    }

    #[test]
    fn empty_means_delete_the_rollup() {
        assert!(Counts::EMPTY.is_empty());
        assert!(Counts::merge(Counts::EMPTY, Counts::EMPTY).is_empty());
        assert!(!counts(0, 1, 1, 1, 1).is_empty());
    }

    /// The bug this field exists to prevent.
    ///
    /// A session whose only signals are transactions has zero on BOTH
    /// counters in normal operation — the pipeline creates the row and bumps
    /// neither. If emptiness were read off the counters, the first purge
    /// touching that app would delete every such session, destroying data the
    /// operator never selected and which no raw deletion had removed.
    #[test]
    fn a_transaction_only_rollup_is_not_empty() {
        let c = Counts::from_sources(
            SourceCounts {
                analytics: 0,
                errors: 0,
                transactions: 5,
            },
            Some(t(1)),
            Some(t(2)),
        );
        assert_eq!((c.events, c.errors), (0, 0));
        assert_eq!(c.evidence, 5);
        assert!(
            !c.is_empty(),
            "zero counters must not be read as zero evidence"
        );
    }

    #[test]
    fn evidence_counts_every_source() {
        let c = Counts::from_sources(
            SourceCounts {
                analytics: 3,
                errors: 2,
                transactions: 4,
            },
            None,
            None,
        );
        assert_eq!(c.evidence, 9);
    }
}
