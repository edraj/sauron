//! Unit decomposition: how a frozen scan is cut into pieces small enough that
//! one of them is a tick's worth of work.
//!
//! Pure on purpose, and NOT in the worker binary: the API freezes a manual
//! scan and the scheduler freezes a scheduled one, and both must agree on the
//! table list, the pair list and `units_total` down to the integer. A second
//! copy of this in a handler is how a manual scan comes to walk environments
//! a narrower disabled policy excluded.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use uuid::Uuid;

use crate::columns::{self, TableClass};
use crate::targets::{self, PolicyTargetType, ScanPair};

/// One indivisible piece of scan work.
///
/// A unit is a single `(app, env, table, day)` for partitioned tables, so at
/// most one day partition's pages are hot at a time — walking one ~30 MB child
/// rather than the 678 MB parent is what keeps the ingest working set
/// resident. It is also what bounds the phase-2 accumulator: keyed on
/// `(column, path, matched_key, detector)`, its cardinality is keys x columns
/// (~50 x 11 = 550 entries), so worker RSS is flat regardless of scan size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Ranged {
        app_id: Uuid,
        env_id: Option<Uuid>,
        table: String,
        day: NaiveDate,
    },
    /// The `_default` child, by name. Never tiered, never dropped.
    DefaultSweep { app_id: Uuid, table: String },
    /// A non-partitioned companion, PK keyset paginated.
    Rollup { app_id: Uuid, table: String },
}

/// Deterministically recompute a scan's unit list.
///
/// Freezing `window_from`/`window_to`/`params`/`targets` is what makes this
/// safe: an admin editing the policy mid-scan would otherwise silently change
/// what unit #37 means, and a resume would walk a different list.
pub fn units_for(
    pairs: &[ScanPair],
    tables: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: PolicyTargetType,
) -> Vec<Unit> {
    let mut units = Vec::new();
    let include_rollups = targets::include_rollups(level);

    // Newest day first.
    //
    // Step back one INSTANT, not one DAY, to find the last day the half-open
    // window `[from, to)` touches. `to - 1 day` is only correct when `to` is
    // exactly midnight; every real scan freezes `to = now()`, and there the
    // subtraction skipped the current day entirely — a scan started at 13:46
    // enumerated days up to YESTERDAY, so PII ingested today was invisible
    // while `coverage` still reported `full`. Measured: two seeded rows dated
    // today produced 0 findings from a 185-unit scan; the identical rows dated
    // yesterday produced 2. The one unit test covering this passed throughout,
    // because it pins `to` to 00:00:00 where both spellings agree.
    let mut days: Vec<NaiveDate> = Vec::new();
    let mut d = (to - Duration::nanoseconds(1)).date_naive();
    while d >= from.date_naive() {
        days.push(d);
        d -= Duration::days(1);
    }

    for table in tables {
        match columns::table_class(table) {
            Some(TableClass::Partitioned) => {
                for day in &days {
                    for p in pairs {
                        units.push(Unit::Ranged {
                            app_id: p.app_id,
                            env_id: p.app_env_id,
                            table: table.clone(),
                            day: *day,
                        });
                    }
                }
                if include_rollups {
                    let mut apps: Vec<Uuid> = pairs.iter().map(|p| p.app_id).collect();
                    apps.sort_unstable();
                    apps.dedup();
                    for app_id in apps {
                        units.push(Unit::DefaultSweep {
                            app_id,
                            table: table.clone(),
                        });
                    }
                }
            }
            Some(TableClass::Rollup) => {
                if !include_rollups {
                    continue;
                }
                let mut apps: Vec<Uuid> = pairs.iter().map(|p| p.app_id).collect();
                apps.sort_unstable();
                apps.dedup();
                for app_id in apps {
                    units.push(Unit::Rollup {
                        app_id,
                        table: table.clone(),
                    });
                }
            }
            // Not in the allowlist at all: silently absent, never scanned.
            None => {}
        }
    }
    units
}

/// The tables a policy scans: the default column set's tables plus whatever
/// rollups it opted into.
///
/// Takes the raw `rollups` jsonb rather than a policy row, so this stays in
/// the pure crate that both `sauron-db` and the worker can call.
pub fn tables_for(rollups: &serde_json::Value) -> Vec<String> {
    let mut tables = vec![
        "error_events".to_string(),
        "analytics_events".to_string(),
        "transactions".to_string(),
    ];
    if let Some(arr) = rollups.as_array() {
        for t in arr.iter().filter_map(|v| v.as_str()) {
            // Only names the inventory knows; a stale rollup id from a
            // downgraded binary must not become an interpolated identifier.
            if columns::table_class(t).is_some() && !tables.iter().any(|x| x == t) {
                tables.push(t.to_string());
            }
        }
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::{PolicyTargetType, ScanPair};
    use chrono::TimeZone;
    use uuid::Uuid;

    fn pair(n: u128, env: Option<u128>) -> ScanPair {
        ScanPair {
            app_id: Uuid::from_u128(n),
            app_env_id: env.map(Uuid::from_u128),
        }
    }

    /// Units are ordered NEWEST DAY FIRST, so a scan killed halfway has
    /// already covered the most recent data — which is what an admin asking
    /// "does this app store email addresses" actually cares about.
    #[test]
    fn ranged_units_are_newest_day_first() {
        let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let from = to - chrono::Duration::days(3);
        let units = units_for(
            &[pair(1, Some(10))],
            &["error_events".to_string()],
            from,
            to,
            PolicyTargetType::App,
        );
        let days: Vec<String> = units
            .iter()
            .filter_map(|u| match u {
                Unit::Ranged { day, .. } => Some(day.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(days, ["2026-07-31", "2026-07-30", "2026-07-29"]);
    }

    /// A scan freezes `to = now()`, so the window's last day is a PARTIAL one
    /// and it must still be enumerated. Skipping it made every scan blind to
    /// PII ingested the same day while still reporting `coverage = full`,
    /// which is the worst possible combination for a privacy tool: the answer
    /// looks authoritative and omits the newest data.
    ///
    /// Both boundary shapes are pinned here on purpose. The midnight case is
    /// the one the original test used, and it agrees under either spelling —
    /// which is exactly why it never caught this.
    #[test]
    fn the_current_partial_day_is_scanned_but_an_exclusive_midnight_is_not() {
        let days_for = |to: DateTime<Utc>, span: i64| {
            units_for(
                &[pair(1, Some(10))],
                &["error_events".to_string()],
                to - chrono::Duration::days(span),
                to,
                PolicyTargetType::App,
            )
            .iter()
            .filter_map(|u| match u {
                Unit::Ranged { day, .. } => Some(day.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
        };

        // Mid-day `to`: today is partially covered by [from, to) and must appear.
        let midday = chrono::Utc.with_ymd_and_hms(2026, 8, 2, 13, 46, 0).unwrap();
        assert_eq!(
            days_for(midday, 2),
            ["2026-08-02", "2026-08-01", "2026-07-31"],
            "the current partial day was dropped"
        );

        // Exactly-midnight `to` is EXCLUSIVE, so that date owns no rows in the
        // window and must NOT appear. The same 2-day span therefore covers
        // three days from a mid-day bound and only two from a midnight one —
        // `[Jul 31 00:00, Aug 2 00:00)` genuinely touches two dates.
        let midnight = chrono::Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).unwrap();
        assert_eq!(
            days_for(midnight, 2),
            ["2026-08-01", "2026-07-31"],
            "an exclusive midnight bound pulled in a day with no rows"
        );
    }

    /// The `_default` child is never tiered and never dropped, so those rows
    /// are the longest-lived PII in the system — and a time-windowed scan
    /// prunes them away precisely because their occurred_at is outside every
    /// explicit range. One extra unit per (table, app) covers them.
    #[test]
    fn a_default_sweep_unit_exists_per_table_and_app() {
        let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let units = units_for(
            &[pair(1, Some(10)), pair(1, Some(11))],
            &["error_events".to_string()],
            to - chrono::Duration::days(1),
            to,
            PolicyTargetType::App,
        );
        let defaults = units
            .iter()
            .filter(|u| matches!(u, Unit::DefaultSweep { .. }))
            .count();
        assert_eq!(defaults, 1, "one per (table, app), not per enrollment");
    }

    /// Neither rollups nor `_default` sweeps can be environment-attributed, so
    /// an env-scoped policy that ran them would persist key paths derived from
    /// PRODUCTION traffic under a policy an admin deliberately scoped to
    /// staging, readable by anyone with pii:read on staging.
    #[test]
    fn an_app_env_policy_gets_neither_rollups_nor_default_sweeps() {
        let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let units = units_for(
            &[pair(1, Some(10))],
            &["error_events".to_string(), "issues".to_string()],
            to - chrono::Duration::days(1),
            to,
            PolicyTargetType::AppEnv,
        );
        assert!(units.iter().all(|u| matches!(u, Unit::Ranged { .. })));
    }

    #[test]
    fn rollup_units_are_one_per_app_and_table() {
        let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let units = units_for(
            &[pair(1, Some(10)), pair(1, None), pair(2, Some(20))],
            &["issues".to_string(), "event_users".to_string()],
            to - chrono::Duration::days(1),
            to,
            PolicyTargetType::Project,
        );
        let rollups = units
            .iter()
            .filter(|u| matches!(u, Unit::Rollup { .. }))
            .count();
        assert_eq!(rollups, 4, "2 apps x 2 rollup tables");
    }

    /// The unit LIST is deterministically recomputable from the frozen window,
    /// params and targets, so only `{unit_index, row_cursor}` is persisted. A
    /// separate table would be ~13,500 bookkeeping rows for a 50-app project
    /// across a 30-day window, times 20 retained scans.
    #[test]
    fn the_unit_list_is_deterministic() {
        let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let from = to - chrono::Duration::days(5);
        let pairs = [pair(1, Some(10)), pair(2, None)];
        let tables = ["error_events".to_string(), "issues".to_string()];
        let a = units_for(&pairs, &tables, from, to, PolicyTargetType::Project);
        let b = units_for(&pairs, &tables, from, to, PolicyTargetType::Project);
        assert_eq!(a, b);
    }
}
