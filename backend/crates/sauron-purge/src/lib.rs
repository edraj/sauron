//! Pure decision logic for the admin data purge.
//!
//! The purge answers four questions before it touches a row, and all four are
//! decided here so they can be tested without a database:
//!
//! 1. Which kinds exist, and which table(s) does each cover? ([`PurgeKind`])
//! 2. Is a kind *deleted* or *recomputed*? ([`Class`])
//! 3. Can a kind be scoped to an environment at all? ([`PurgeKind::env_scoped`])
//! 4. Given a raw deletion, which rollups now need repair?
//!    ([`rollups_to_recompute`])
//!
//! The one thing this crate deliberately does NOT know is how to execute any
//! of it. No SQL, no connection, no I/O.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod recompute;

/// Whether a kind's rows are removed, or repaired from what survives.
///
/// This is the axis that decides what the worker does with a kind, and it is
/// not the same axis as "is it an event table". `workflows` looks like a signal
/// table and is a rollup; see [`PurgeKind::Workflows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Append-only source rows. Deleted outright within the scope.
    Raw,
    /// Carries monotonic counters maintained by the pipeline. Recomputed from
    /// surviving raw rows, and deleted only when nothing survives.
    Rollup,
}

/// One selectable kind of data.
///
/// The slug is the wire form and the storage form (`purge_jobs.kinds`), so it
/// is fixed vocabulary: renaming a variant is a data migration, not a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeKind {
    // --- raw ---
    ErrorEvents,
    AnalyticsEvents,
    Transactions,
    /// `inspector_scans` + `inspector_findings` + `inspector_masked_keys`.
    ///
    /// Raw in the sense that matters here: nothing else derives a counter from
    /// it, so removing it repairs nothing and requires no second pass.
    Inspector,

    // --- rollup ---
    Issues,
    Sessions,
    Devices,
    /// `event_users` + `event_user_environments` + `identities`.
    ///
    /// The two companions are enumerated in `sauron_db::purge`'s
    /// `rollup_companions`, which BOTH delete paths consult. This line is the
    /// statement of intent and that function is the only thing that enforces
    /// it: neither companion has a foreign key to `event_users`, so nothing in
    /// the schema makes a missed one fail. It shows up as a purged person
    /// still listed on the Users Explorer with pre-purge counters.
    Persons,
    /// A rollup, despite the name reading like a signal table.
    ///
    /// `workflows` carries `events_count`, `errors_count`, `started_at` and
    /// `last_event_at`, upserted by the pipeline's `workflow()` fold — exactly
    /// the shape of `sessions`. Classifying it as raw and merely deleting its
    /// rows would leave workflow counters as stale as the ones the purge
    /// exists to repair.
    Workflows,
}

/// Every kind, in a stable order. The UI renders in this order, and the worker
/// executes raw kinds before rollups regardless (see [`execution_order`]).
pub const ALL: &[PurgeKind] = &[
    PurgeKind::ErrorEvents,
    PurgeKind::AnalyticsEvents,
    PurgeKind::Transactions,
    PurgeKind::Inspector,
    PurgeKind::Issues,
    PurgeKind::Sessions,
    PurgeKind::Devices,
    PurgeKind::Persons,
    PurgeKind::Workflows,
];

impl PurgeKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::ErrorEvents => "error_events",
            Self::AnalyticsEvents => "analytics_events",
            Self::Transactions => "transactions",
            Self::Inspector => "inspector",
            Self::Issues => "issues",
            Self::Sessions => "sessions",
            Self::Devices => "devices",
            Self::Persons => "persons",
            Self::Workflows => "workflows",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL.iter().copied().find(|k| k.slug() == s)
    }

    pub fn class(self) -> Class {
        match self {
            Self::ErrorEvents | Self::AnalyticsEvents | Self::Transactions | Self::Inspector => {
                Class::Raw
            }
            Self::Issues | Self::Sessions | Self::Devices | Self::Persons | Self::Workflows => {
                Class::Rollup
            }
        }
    }

    /// Whether rows of this kind can be attributed to a single environment.
    ///
    /// `false` means the underlying table has no `environment_id` column at
    /// all: a device and a person exist ACROSS environments, and an issue's
    /// per-environment figures are computed on read (`apply_issue_env_stats`)
    /// rather than stored. There is therefore no predicate that decides
    /// whether such a row "belongs to" a selected environment, and inferring
    /// one from the surviving raw rows would be wrong — a device whose only
    /// remaining events are in another environment is not thereby a different
    /// device, and deleting it would destroy data outside the requested scope.
    ///
    /// Consequence, enforced by [`validate_scope`]: when an environment filter
    /// is active these kinds are recompute-only and cannot be deleted.
    pub fn env_scoped(self) -> bool {
        match self {
            Self::ErrorEvents
            | Self::AnalyticsEvents
            | Self::Transactions
            | Self::Sessions
            | Self::Workflows => true,
            // `inspector_findings` has an environment_id but `inspector_scans`
            // does not, and a scan is the parent of its findings. Deleting
            // findings while their scan survives leaves a scan reporting a
            // finding count it no longer has, so the kind moves as one unit
            // and is app-scoped.
            Self::Inspector | Self::Issues | Self::Devices | Self::Persons => false,
        }
    }

    /// Raw kinds first, then rollups.
    ///
    /// Not cosmetic: recompute reads what SURVIVES, so every deletion that
    /// could change a rollup's inputs has to have happened before the rollup
    /// is repaired. Interleaving them would recompute against rows that a
    /// later batch then removes, producing counters that are wrong in a way no
    /// subsequent pass corrects.
    pub fn is_raw(self) -> bool {
        self.class() == Class::Raw
    }
}

/// Sort a selection into execution order: all raw kinds, then all rollups,
/// each group in [`ALL`] order.
pub fn execution_order(kinds: &[PurgeKind]) -> Vec<PurgeKind> {
    let mut out: Vec<PurgeKind> = ALL.iter().copied().filter(|k| kinds.contains(k)).collect();
    out.sort_by_key(|k| (!k.is_raw(), ALL.iter().position(|a| a == k).unwrap_or(0)));
    out
}

/// Which rollups need recomputing after the given raw kinds are deleted.
///
/// **Transactions trigger a repair despite moving no counter.** The pipeline
/// still runs its rollup fold for a transaction — it creates the session,
/// device and person rows and bumps neither counter (deltas `0, 0`). So a
/// session whose only signals were transactions has real rows behind it, and
/// deleting those transactions leaves it an orphan describing occurrences that
/// no longer exist. Scheduling repair off "does this kind move a counter"
/// would skip exactly that case. What the counters are computed FROM is a
/// separate question, answered by [`recompute::Counts::from_sources`].
///
/// `inspector` is the one raw kind that genuinely repairs nothing: no rollup
/// derives anything from a scan.
///
/// The returned set is independent of what the operator ticked. A rollup
/// touched by a raw deletion is repaired whether or not its own kind was
/// selected — ticking a rollup kind adds outright deletion of fully-contained
/// rows, it is never what causes repair.
pub fn rollups_to_recompute(raw: &[PurgeKind]) -> Vec<PurgeKind> {
    let touches_rollups = raw.iter().any(|k| {
        matches!(
            k,
            PurgeKind::AnalyticsEvents | PurgeKind::ErrorEvents | PurgeKind::Transactions
        )
    });
    if !touches_rollups {
        return Vec::new();
    }
    let mut out = vec![
        PurgeKind::Sessions,
        PurgeKind::Devices,
        PurgeKind::Persons,
        PurgeKind::Workflows,
    ];
    // `issues` is derived from error_events alone; neither analytics events
    // nor transactions carry an `issue_id`, so neither can change an issue.
    if raw.contains(&PurgeKind::ErrorEvents) {
        out.push(PurgeKind::Issues);
    }
    out.sort();
    out
}

/// The time window a purge applies to.
///
/// `All` is a distinct variant rather than `Range { start: None, end: None }`
/// for the same reason `purge_jobs.all_time` is its own column: wiping an
/// app's whole history is legitimate, but it must be an affirmative choice
/// rather than the accidental result of a date field left blank. Making the
/// two unrepresentable in the same shape is what guarantees the UI cannot
/// produce one while meaning the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    All,
    Range {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
}

impl Window {
    /// Whether a rollup row's ENTIRE activity span lies inside this window.
    ///
    /// A rollup row is not a point in time — a session spans
    /// `started_at`..`last_event_at`, a device `first_seen`..`last_seen`, an
    /// issue `first_seen`..`last_seen`. There is no coherent way to "partially
    /// delete" such a row for a sub-range, so the only two honest outcomes are
    /// delete it whole or repair it, and this predicate is which.
    ///
    /// Deliberately containment and NOT overlap. Overlap would delete a
    /// session that merely brushed the window, destroying evidence outside the
    /// requested scope — the one failure a purge must never have.
    ///
    /// Bounds are inclusive on both ends: a span that starts exactly at
    /// `start` and ends exactly at `end` is contained. The window came from a
    /// human picking dates, and a row excluded for touching the boundary
    /// instant would be indistinguishable from a bug.
    pub fn contains_span(&self, span_start: DateTime<Utc>, span_end: DateTime<Utc>) -> bool {
        match self {
            Self::All => true,
            Self::Range { start, end } => span_start >= *start && span_end <= *end,
        }
    }
}

/// Why a requested purge scope was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    NoKinds,
    /// A kind with no `environment_id` column was selected for deletion while
    /// an environment filter was active.
    KindNotEnvScoped(PurgeKind),
    /// `environment_ids: []` — a scope that matches nothing. Distinct from
    /// `null`, which means every environment.
    EmptyEnvironmentList,
    RangeNotOrdered,
}

impl ScopeError {
    pub fn message(&self) -> String {
        match self {
            Self::NoKinds => "select at least one kind of data to purge".into(),
            Self::KindNotEnvScoped(k) => format!(
                "'{}' cannot be purged with an environment filter: the table has no \
                 environment_id, so no row can be attributed to one environment. \
                 Clear the environment filter to purge it, or leave it unticked \
                 and it will still be recomputed.",
                k.slug()
            ),
            Self::EmptyEnvironmentList => {
                "environment list is empty, which matches nothing; omit it to mean \
                 every environment"
                    .into()
            }
            Self::RangeNotOrdered => "range start must be before range end".into(),
        }
    }
}

/// Validate a requested scope before anything is written.
///
/// `env_filter_active` is whether the request named specific environments at
/// all, NOT whether that list is non-empty — an explicitly empty list is its
/// own error rather than "no filter".
pub fn validate_scope(
    kinds: &[PurgeKind],
    env_filter_active: bool,
    env_count: usize,
    window: Window,
) -> Result<(), ScopeError> {
    if kinds.is_empty() {
        return Err(ScopeError::NoKinds);
    }
    if env_filter_active && env_count == 0 {
        return Err(ScopeError::EmptyEnvironmentList);
    }
    if let Window::Range { start, end } = window {
        if start >= end {
            return Err(ScopeError::RangeNotOrdered);
        }
    }
    if env_filter_active {
        if let Some(k) = kinds.iter().copied().find(|k| !k.env_scoped()) {
            return Err(ScopeError::KindNotEnvScoped(k));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn window() -> Window {
        Window::Range {
            start: t(2026, 8, 1),
            end: t(2026, 8, 10),
        }
    }

    #[test]
    fn every_slug_round_trips() {
        for k in ALL {
            assert_eq!(PurgeKind::parse(k.slug()), Some(*k), "slug {}", k.slug());
        }
        assert_eq!(PurgeKind::parse("nope"), None);
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in ALL {
            assert!(seen.insert(k.slug()), "duplicate slug {}", k.slug());
        }
        assert_eq!(seen.len(), ALL.len());
    }

    /// The misclassification this crate exists to prevent. `workflows` carries
    /// pipeline-maintained counters, so deleting its rows without recomputing
    /// would leave exactly the staleness the purge is meant to fix.
    #[test]
    fn workflows_is_a_rollup_not_raw() {
        assert_eq!(PurgeKind::Workflows.class(), Class::Rollup);
    }

    #[test]
    fn raw_kinds_run_before_rollups() {
        let order = execution_order(&[
            PurgeKind::Sessions,
            PurgeKind::ErrorEvents,
            PurgeKind::Devices,
            PurgeKind::AnalyticsEvents,
        ]);
        let first_rollup = order.iter().position(|k| !k.is_raw()).unwrap();
        let last_raw = order.iter().rposition(|k| k.is_raw()).unwrap();
        assert!(
            last_raw < first_rollup,
            "recompute would read rows a later batch still deletes: {order:?}"
        );
    }

    /// Transactions move no counter but DO create rollup rows, so purging
    /// them must still schedule a repair — otherwise a session whose only
    /// signals were transactions survives as an orphan.
    #[test]
    fn transactions_trigger_repair_despite_moving_no_counter() {
        let out = rollups_to_recompute(&[PurgeKind::Transactions]);
        assert!(out.contains(&PurgeKind::Sessions));
        assert!(out.contains(&PurgeKind::Devices));
        assert!(out.contains(&PurgeKind::Persons));
        // ...but they carry no issue_id, so issues are untouched.
        assert!(!out.contains(&PurgeKind::Issues));
    }

    /// The one raw kind nothing derives from.
    #[test]
    fn inspector_recomputes_nothing() {
        assert!(rollups_to_recompute(&[PurgeKind::Inspector]).is_empty());
    }

    #[test]
    fn analytics_does_not_touch_issues_but_errors_do() {
        let from_analytics = rollups_to_recompute(&[PurgeKind::AnalyticsEvents]);
        assert!(!from_analytics.contains(&PurgeKind::Issues));
        assert!(from_analytics.contains(&PurgeKind::Sessions));

        let from_errors = rollups_to_recompute(&[PurgeKind::ErrorEvents]);
        assert!(from_errors.contains(&PurgeKind::Issues));
    }

    /// Repair does not depend on what the operator ticked.
    #[test]
    fn recompute_set_ignores_selection() {
        assert_eq!(
            rollups_to_recompute(&[PurgeKind::ErrorEvents]),
            rollups_to_recompute(&[PurgeKind::ErrorEvents, PurgeKind::Sessions]),
        );
    }

    #[test]
    fn span_must_be_contained_not_merely_overlapping() {
        let w = window();
        // Fully inside.
        assert!(w.contains_span(t(2026, 8, 2), t(2026, 8, 5)));
        // Straddles the start — must be kept and recomputed, never deleted.
        assert!(!w.contains_span(t(2026, 7, 30), t(2026, 8, 5)));
        // Straddles the end.
        assert!(!w.contains_span(t(2026, 8, 5), t(2026, 8, 20)));
        // Encloses the window entirely.
        assert!(!w.contains_span(t(2026, 7, 1), t(2026, 9, 1)));
        // Entirely outside.
        assert!(!w.contains_span(t(2026, 6, 1), t(2026, 6, 2)));
    }

    #[test]
    fn span_boundaries_are_inclusive() {
        let w = window();
        assert!(w.contains_span(t(2026, 8, 1), t(2026, 8, 10)));
    }

    #[test]
    fn all_time_contains_every_span() {
        assert!(Window::All.contains_span(t(1970, 1, 1), t(2999, 1, 1)));
    }

    #[test]
    fn env_filter_refuses_app_scoped_kinds() {
        let err = validate_scope(&[PurgeKind::Devices], true, 1, window()).unwrap_err();
        assert_eq!(err, ScopeError::KindNotEnvScoped(PurgeKind::Devices));
        for k in [PurgeKind::Issues, PurgeKind::Persons, PurgeKind::Inspector] {
            assert!(validate_scope(&[k], true, 1, window()).is_err(), "{k:?}");
        }
    }

    #[test]
    fn app_scoped_kinds_are_fine_without_an_env_filter() {
        assert!(validate_scope(&[PurgeKind::Devices], false, 0, window()).is_ok());
    }

    #[test]
    fn env_scoped_kinds_pass_with_a_filter() {
        assert!(validate_scope(&[PurgeKind::Sessions], true, 2, window()).is_ok());
    }

    /// `[]` and `null` must not mean the same thing: one matches nothing, the
    /// other matches everything.
    #[test]
    fn empty_env_list_is_not_no_filter() {
        assert_eq!(
            validate_scope(&[PurgeKind::Sessions], true, 0, window()).unwrap_err(),
            ScopeError::EmptyEnvironmentList
        );
    }

    #[test]
    fn no_kinds_is_refused() {
        assert_eq!(
            validate_scope(&[], false, 0, window()).unwrap_err(),
            ScopeError::NoKinds
        );
    }

    #[test]
    fn inverted_range_is_refused() {
        let w = Window::Range {
            start: t(2026, 8, 10),
            end: t(2026, 8, 1),
        };
        assert_eq!(
            validate_scope(&[PurgeKind::Sessions], false, 0, w).unwrap_err(),
            ScopeError::RangeNotOrdered
        );
    }

    #[test]
    fn zero_length_range_is_refused() {
        let w = Window::Range {
            start: t(2026, 8, 1),
            end: t(2026, 8, 1),
        };
        assert!(validate_scope(&[PurgeKind::Sessions], false, 0, w).is_err());
    }
}
