//! Personal notification subscriptions: the entire pure decision surface.
//!
//! No diesel, no axum, no network. CI runs `cargo test --workspace` with no
//! Postgres service, so everything that can be decided without a database is
//! decided here and unit-tested unconditionally — the same split `guard.rs`
//! already uses.

use sauron_auth::rbac::Reach;
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// What a personal subscription notifies on. Shaped like
/// [`crate::rule::TriggerType`], deliberately: same `parse`/`as_str`/`ALL`
/// surface so the two enums read the same way at call sites.
///
/// There is no `event_threshold` and no `perf_degradation` here. Analytics
/// volume and latency percentiles are team dashboards, not personal inboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubKind {
    /// A monitor transitioned. Project scope only.
    Uptime,
    /// Error volume in a window jumped relative to the previous window.
    ErrorSpike,
    /// A brand-new issue was first seen.
    ErrorNewIssue,
    /// A resolved/ignored issue started erroring again.
    ErrorRegression,
}

impl SubKind {
    pub fn parse(s: &str) -> Option<SubKind> {
        Some(match s {
            "uptime" => SubKind::Uptime,
            "error_spike" => SubKind::ErrorSpike,
            "error_new_issue" => SubKind::ErrorNewIssue,
            "error_regression" => SubKind::ErrorRegression,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SubKind::Uptime => "uptime",
            SubKind::ErrorSpike => "error_spike",
            SubKind::ErrorNewIssue => "error_new_issue",
            SubKind::ErrorRegression => "error_regression",
        }
    }

    /// `monitors` carries only `project_id` — no `app_id`, no
    /// `environment_id` — so there is nothing below project for an uptime
    /// subscription to narrow on. Accepting an app-scoped uptime subscription
    /// that can never fire is worse than refusing it.
    pub fn allows_app_scope(self) -> bool {
        !matches!(self, SubKind::Uptime)
    }

    /// Same reason as [`Self::allows_app_scope`]: an uptime subscription's
    /// environment set is meaningless, so the dialog says so and the evaluator
    /// ignores it.
    pub fn supports_env_filter(self) -> bool {
        !matches!(self, SubKind::Uptime)
    }

    /// The permission a subscriber must hold over the scope.
    ///
    /// No new permission is minted for subscriptions: a subscription delivers
    /// only telemetry the user can already read, so it confers nothing.
    /// Gating on `alert:read` would be wrong — Viewer lacks it entirely.
    ///
    /// Returned from `sauron_auth::rbac::perm` rather than as a literal: these
    /// strings are matched against stored grants, so a rename in `rbac.rs` that
    /// left a literal behind here would produce an empty `Reach` for every
    /// subscription — no mail, no error, nothing to notice.
    pub fn permission(self) -> &'static str {
        match self {
            SubKind::Uptime => sauron_auth::rbac::perm::MONITOR_READ,
            _ => sauron_auth::rbac::perm::ISSUE_READ,
        }
    }

    pub const ALL: [SubKind; 4] = [
        SubKind::Uptime,
        SubKind::ErrorSpike,
        SubKind::ErrorNewIssue,
        SubKind::ErrorRegression,
    ];
}

/// A subscription's `conditions` bag, parsed and clamped.
///
/// Every field is clamped at parse time rather than trusted, because a
/// subscription is created by any authenticated user — `POST /v1/auth/register`
/// is open and every registrant becomes an org Owner — and an unclamped window
/// or factor is both a cost lever and a coalescing-defeat vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SubConditions {
    pub window_seconds: u32,
    pub factor: f64,
    pub min_count: i64,
    pub level: Option<String>,
}

impl SubConditions {
    pub const DEFAULT_WINDOW_SECONDS: u32 = 900;
    pub const MIN_WINDOW_SECONDS: u32 = 300;
    pub const MAX_WINDOW_SECONDS: u32 = 86_400;
    pub const DEFAULT_FACTOR: f64 = 3.0;
    pub const MIN_FACTOR: f64 = 1.5;
    pub const MAX_FACTOR: f64 = 100.0;
    pub const DEFAULT_MIN_COUNT: i64 = 10;
    pub const MIN_MIN_COUNT: i64 = 1;
    pub const MAX_MIN_COUNT: i64 = 100_000;

    pub fn from_value(kind: SubKind, v: &Value) -> SubConditions {
        let window_seconds = v
            .get("window_seconds")
            .and_then(Value::as_u64)
            .map(|n| n.min(u32::MAX as u64) as u32)
            .unwrap_or(Self::DEFAULT_WINDOW_SECONDS)
            .clamp(Self::MIN_WINDOW_SECONDS, Self::MAX_WINDOW_SECONDS);

        // A NaN would poison the `BTreeMap<ProbeKey, _>` ordering that probe
        // coalescing depends on, and an infinity would make every ratio
        // comparison false. Neither is representable after this line.
        let factor = v
            .get("factor")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
            .unwrap_or(Self::DEFAULT_FACTOR)
            .clamp(Self::MIN_FACTOR, Self::MAX_FACTOR);

        let min_count = v
            .get("min_count")
            .and_then(Value::as_i64)
            .unwrap_or(Self::DEFAULT_MIN_COUNT)
            .clamp(Self::MIN_MIN_COUNT, Self::MAX_MIN_COUNT);

        let level = match v.get("level") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Null) => None,
            // The issue kinds default to `error`; the spike kind counts every
            // level unless told otherwise.
            None => match kind {
                SubKind::ErrorNewIssue | SubKind::ErrorRegression => Some("error".to_string()),
                _ => None,
            },
            _ => None,
        };

        SubConditions {
            window_seconds,
            factor,
            min_count,
            level,
        }
    }

    /// The clamped bag, back on the wire — what the API stores after
    /// validation, so the dashboard renders the effective values rather than
    /// what was submitted.
    pub fn to_value(&self, kind: SubKind) -> Value {
        match kind {
            SubKind::Uptime => serde_json::json!({}),
            SubKind::ErrorSpike => serde_json::json!({
                "window_seconds": self.window_seconds,
                "factor": self.factor,
                "min_count": self.min_count,
                "level": self.level,
            }),
            _ => serde_json::json!({ "level": self.level }),
        }
    }
}

/// Fire when the current window carries real volume AND either the previous
/// window was empty or the jump is at least `factor`.
///
/// The `baseline == 0` disjunct is the whole point: the shipped org-engine
/// predicate guards on `previous > 0`, so the zero-to-flood case — an app that
/// was silent and is now on fire — is the one case it can never report.
/// `min_count` is equally deliberate: without a floor a 1 -> 3 movement is a 3x
/// spike and pages someone at 04:00.
pub fn spike_fires(current: i64, baseline: i64, min_count: i64, factor: f64) -> bool {
    current >= min_count && (baseline == 0 || current as f64 >= baseline as f64 * factor)
}

/// Whether `local_minute` (minute-of-day, 0..1439, in the subscription's own
/// zone) falls inside `[start, end)`, wrap-around aware.
///
/// The enqueue does not call this — `deliver_after` is computed entirely in SQL
/// because the workspace has no `chrono-tz` and nothing in Rust can produce a
/// subscription's local wall-clock time. This exists because it is the only
/// form a unit test can reach, and a DB test asserts the SQL and this function
/// agree over a shared table of cases.
pub fn in_quiet_hours(local_minute: i32, start: i32, end: i32) -> bool {
    if start == end {
        // A zero-width window must not silence everything forever.
        return false;
    }
    if start < end {
        local_minute >= start && local_minute < end
    } else {
        local_minute >= start || local_minute < end
    }
}

/// A [`SubConditions`] quantized into something that can be a map key.
///
/// `f64` is not `Ord`, so a raw factor cannot key a `BTreeMap` at all — and
/// even if it could, distinct float values would defeat coalescing entirely,
/// which is a cheap denial of service against an evaluator whose whole cost
/// model is "one probe per condition bucket".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CondBucket {
    pub window_seconds: u32,
    pub min_count: i64,
    pub level: Option<String>,
    /// The clamped factor snapped to the nearest 0.25, in thousandths.
    pub factor_milli: u32,
}

impl CondBucket {
    pub fn quantize(c: &SubConditions) -> CondBucket {
        let snapped = (c.factor * 4.0).round() / 4.0;
        CondBucket {
            window_seconds: c.window_seconds,
            min_count: c.min_count,
            level: c.level.clone(),
            factor_milli: (snapped * 1000.0).round() as u32,
        }
    }
}

/// What one database probe is keyed on.
///
/// Deliberately does NOT contain an app id. `alert_count_errors`,
/// `alert_new_issues` and `alert_regressed_issues` all take `app_ids: &[Uuid]`
/// and filter `app_id = ANY($1)`, so a rule over a 200-app project costs ONE
/// query today. Keying a probe on a single app would turn that into 200 —
/// worse than what already ships.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProbeKey {
    pub org_id: Uuid,
    pub kind: SubKind,
    pub cond: CondBucket,
    /// Sorted and deduped catalogue environment ids. Empty means "all
    /// environments, including unattributed rows".
    pub catalogue_envs: Vec<Uuid>,
}

/// One subscription, with its scope already resolved to app ids.
#[derive(Debug, Clone)]
pub struct SubInput {
    /// The caller's index into its own subscription vector. Probes carry these
    /// back so the caller never has to match on anything but position in a
    /// slice it owns.
    pub index: usize,
    pub org_id: Uuid,
    pub kind: SubKind,
    pub cond: SubConditions,
    /// Catalogue environment ids. Empty means all environments.
    pub catalogue_envs: Vec<Uuid>,
    /// The apps this subscription's scope resolves to.
    pub app_ids: Vec<Uuid>,
}

/// One database probe and the subscriptions it answers for.
#[derive(Debug, Clone)]
pub struct Probe {
    pub key: ProbeKey,
    /// `SubInput::index` values, in ascending order.
    pub subs: Vec<usize>,
    /// The union of the in-scope apps of every subscription in `subs`, sorted
    /// and deduped.
    pub app_ids: Vec<Uuid>,
}

/// Group subscriptions into the smallest set of probes that answers all of
/// them.
///
/// Cost is `O(orgs × kinds × distinct condition buckets × distinct env sets)`
/// — independent of both user count and app count, and never worse than the
/// existing org engine. Since almost every subscription uses defaults, this
/// collapses hard in practice.
pub fn coalesce(inputs: &[SubInput]) -> Vec<Probe> {
    let mut grouped: BTreeMap<ProbeKey, (Vec<usize>, Vec<Uuid>)> = BTreeMap::new();
    for s in inputs {
        let mut envs = s.catalogue_envs.clone();
        envs.sort_unstable();
        envs.dedup();
        let key = ProbeKey {
            org_id: s.org_id,
            kind: s.kind,
            cond: CondBucket::quantize(&s.cond),
            catalogue_envs: envs,
        };
        let entry = grouped.entry(key).or_default();
        entry.0.push(s.index);
        entry.1.extend_from_slice(&s.app_ids);
    }
    grouped
        .into_iter()
        .map(|(key, (mut subs, mut app_ids))| {
            subs.sort_unstable();
            app_ids.sort_unstable();
            app_ids.dedup();
            Probe { key, subs, app_ids }
        })
        .collect()
}

/// What a queued notification is *about*, in the terms `covers` decides on.
pub struct QueueTarget<'a> {
    pub project_id: Uuid,
    /// `None` for uptime — `monitors` carries no app dimension.
    pub app_id: Option<Uuid>,
    /// **Enrollment** ids (`app_environments.id`). Empty means the body spans
    /// every environment of the app, including unattributed rows.
    pub env_enrollments: &'a [Uuid],
    pub includes_unattributed: bool,
}

/// Whether `reach` releases `t`'s content to its holder.
///
/// Callers MUST pass a `Reach` built from grants already filtered to a single
/// organization (as `repo::user_grants_in_org` does) — `reach_for`'s org arm is
/// `Scope::Org(_) => reach.org = true` and never compares the org id, so an
/// unfiltered grant list would leak another org's visibility.
pub fn covers(reach: &Reach, t: &QueueTarget<'_>) -> bool {
    if reach.org {
        return true;
    }
    if reach.projects.contains(&t.project_id) {
        return true;
    }
    let Some(app_id) = t.app_id else {
        // Uptime stops here. Every monitor read in the product resolves with
        // `app: None, env: None`, so an app- or env-scoped member gets 403 from
        // every monitor endpoint; authorizing an uptime notification with the
        // per-app coverage test below would mail them monitor names, targets,
        // causes and incident ids the API itself refuses them.
        return false;
    };
    if reach.apps.contains(&app_id) {
        return true;
    }
    // An env grant is released only when EVERY enrollment behind the body is
    // one the holder reaches. An empty list is never "unconstrained": it means
    // the probe counted across all environments and unattributed rows, which
    // needs app-level reach.
    !t.includes_unattributed
        && !t.env_enrollments.is_empty()
        && t.env_enrollments.iter().all(|e| reach.envs.contains(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spike_fires_on_zero_to_flood() {
        // The shipped org-engine predicate is `previous > 0 && …`, which makes
        // an app that was silent and is now on fire the ONE case that can never
        // fire. That is the case this whole kind exists for.
        assert!(spike_fires(10, 0, 10, 3.0));
        assert!(
            !spike_fires(9, 0, 10, 3.0),
            "the floor still applies at B = 0"
        );
    }

    #[test]
    fn spike_needs_an_absolute_floor_as_well_as_a_ratio() {
        // 1 -> 3 is a 3x spike and would page someone at 04:00 without a floor.
        assert!(!spike_fires(3, 1, 10, 3.0));
        assert!(spike_fires(30, 10, 10, 3.0));
        assert!(!spike_fires(29, 10, 10, 3.0));
    }

    #[test]
    fn conditions_clamp_to_their_documented_bounds() {
        let v = serde_json::json!({
            "window_seconds": 5, "factor": 900.0, "min_count": 0, "level": "warning"
        });
        let c = SubConditions::from_value(SubKind::ErrorSpike, &v);
        assert_eq!(c.window_seconds, 300);
        assert_eq!(c.factor, 100.0);
        assert_eq!(c.min_count, 1);
        assert_eq!(c.level.as_deref(), Some("warning"));

        let v =
            serde_json::json!({ "window_seconds": 999_999, "factor": 0.1, "min_count": 9_999_999 });
        let c = SubConditions::from_value(SubKind::ErrorSpike, &v);
        assert_eq!(c.window_seconds, 86_400);
        assert_eq!(c.factor, 1.5);
        assert_eq!(c.min_count, 100_000);
    }

    #[test]
    fn a_non_finite_factor_never_survives_parsing() {
        // A NaN factor would poison a BTreeMap key ordering; an infinite one
        // would make every comparison false. Both are rejected before either
        // can happen.
        let v = serde_json::json!({ "factor": f64::NAN });
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorSpike, &v).factor,
            3.0
        );
        let v = serde_json::json!({ "factor": "not a number" });
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorSpike, &v).factor,
            3.0
        );
    }

    #[test]
    fn issue_kinds_default_to_error_level() {
        let empty = serde_json::json!({});
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorNewIssue, &empty)
                .level
                .as_deref(),
            Some("error")
        );
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorRegression, &empty)
                .level
                .as_deref(),
            Some("error")
        );
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorSpike, &empty)
                .level
                .as_deref(),
            None
        );
    }

    #[test]
    fn quiet_hours_wrap_around_midnight() {
        // 22:00 -> 06:00
        let (start, end) = (22 * 60, 6 * 60);
        assert!(in_quiet_hours(23 * 60, start, end));
        assert!(in_quiet_hours(3 * 60, start, end));
        assert!(!in_quiet_hours(7 * 60, start, end));
        assert!(!in_quiet_hours(21 * 60 + 59, start, end));
        assert!(
            in_quiet_hours(start, start, end),
            "the start minute is inside"
        );
        assert!(
            !in_quiet_hours(end, start, end),
            "the end minute is outside"
        );
    }

    #[test]
    fn quiet_hours_same_day_window() {
        let (start, end) = (60, 5 * 60); // 01:00 -> 05:00
        assert!(in_quiet_hours(2 * 60, start, end));
        assert!(!in_quiet_hours(6 * 60, start, end));
        assert!(!in_quiet_hours(0, start, end));
    }

    #[test]
    fn quiet_hours_with_equal_bounds_is_never_quiet() {
        // A zero-width window must not silence everything forever.
        assert!(!in_quiet_hours(0, 300, 300));
        assert!(!in_quiet_hours(300, 300, 300));
        assert!(!in_quiet_hours(1439, 300, 300));
    }

    #[test]
    fn uptime_refuses_app_scope_and_the_environment_filter() {
        assert!(!SubKind::Uptime.allows_app_scope());
        assert!(!SubKind::Uptime.supports_env_filter());
        for k in [
            SubKind::ErrorSpike,
            SubKind::ErrorNewIssue,
            SubKind::ErrorRegression,
        ] {
            assert!(k.allows_app_scope());
            assert!(k.supports_env_filter());
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_string() {
        for k in SubKind::ALL {
            assert_eq!(SubKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(SubKind::parse("event_threshold"), None);
    }

    fn sub(
        index: usize,
        org: u128,
        kind: SubKind,
        factor: f64,
        envs: &[u128],
        apps: &[u128],
    ) -> SubInput {
        SubInput {
            index,
            org_id: uuid::Uuid::from_u128(org),
            kind,
            cond: SubConditions {
                window_seconds: 900,
                factor,
                min_count: 10,
                level: None,
            },
            catalogue_envs: envs.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            app_ids: apps.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
        }
    }

    #[test]
    fn quantization_collapses_float_noise_but_not_real_differences() {
        // `f64` is not `Ord`, so a raw factor cannot be a BTreeMap key at all;
        // and distinct float values would defeat coalescing entirely, which is
        // a cheap denial of service given that registration is open.
        let a = SubConditions {
            window_seconds: 900,
            factor: 3.0,
            min_count: 10,
            level: None,
        };
        let b = SubConditions {
            window_seconds: 900,
            factor: 3.0000001,
            min_count: 10,
            level: None,
        };
        let c = SubConditions {
            window_seconds: 900,
            factor: 3.5,
            min_count: 10,
            level: None,
        };
        assert_eq!(CondBucket::quantize(&a), CondBucket::quantize(&b));
        assert_ne!(CondBucket::quantize(&a), CondBucket::quantize(&c));
        // Snapped to the nearest 0.25.
        let d = SubConditions {
            window_seconds: 900,
            factor: 3.13,
            min_count: 10,
            level: None,
        };
        assert_eq!(CondBucket::quantize(&d).factor_milli, 3_250);
    }

    #[test]
    fn every_subscription_lands_in_exactly_one_probe() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10, 11]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[], &[11, 12]),
            sub(2, 1, SubKind::ErrorSpike, 3.5, &[], &[10]),
        ];
        let probes = coalesce(&inputs);
        let mut seen: Vec<usize> = probes.iter().flat_map(|p| p.subs.clone()).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);
        assert_eq!(probes.len(), 2, "two distinct factor buckets");
    }

    #[test]
    fn a_probes_app_array_is_exactly_the_union_of_its_subscriptions_scopes() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10, 11]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[], &[11, 12]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 1);
        let mut apps = probes[0].app_ids.clone();
        apps.sort_unstable();
        assert_eq!(
            apps,
            vec![
                uuid::Uuid::from_u128(10),
                uuid::Uuid::from_u128(11),
                uuid::Uuid::from_u128(12)
            ]
        );
    }

    #[test]
    fn a_probe_never_spans_two_organizations() {
        // `org_id` is in the key so a cross-tenant mix-up is structurally
        // impossible: no probe's app array can span organizations.
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10]),
            sub(1, 2, SubKind::ErrorSpike, 3.0, &[], &[20]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 2);
        for p in &probes {
            assert_eq!(p.subs.len(), 1);
        }
    }

    #[test]
    fn environment_sets_are_order_insensitive_but_membership_sensitive() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[7, 8], &[10]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[8, 7], &[11]),
            sub(2, 1, SubKind::ErrorSpike, 3.0, &[7], &[12]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(
            probes.len(),
            2,
            "{{7,8}} coalesces with {{8,7}}, not with {{7}}"
        );
    }

    #[test]
    fn probe_count_is_bounded_by_orgs_times_kinds_times_buckets_times_env_sets() {
        // 200 subscriptions, one org, one kind, all defaults: one probe. This
        // is the property the whole design exists for — cost is independent of
        // both user count and app count.
        let inputs: Vec<SubInput> = (0..200)
            .map(|i| sub(i, 1, SubKind::ErrorSpike, 3.0, &[], &[(i as u128) + 100]))
            .collect();
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].subs.len(), 200);
        assert_eq!(probes[0].app_ids.len(), 200);
    }

    #[test]
    fn permissions_come_from_the_rbac_constants() {
        assert_eq!(
            SubKind::Uptime.permission(),
            sauron_auth::rbac::perm::MONITOR_READ
        );
        assert_eq!(
            SubKind::ErrorSpike.permission(),
            sauron_auth::rbac::perm::ISSUE_READ
        );
        assert_eq!(
            SubKind::ErrorNewIssue.permission(),
            sauron_auth::rbac::perm::ISSUE_READ
        );
        assert_eq!(
            SubKind::ErrorRegression.permission(),
            sauron_auth::rbac::perm::ISSUE_READ
        );
    }

    use sauron_auth::rbac::Reach;

    fn reach(org: bool, projects: &[u128], apps: &[u128], envs: &[u128]) -> Reach {
        Reach {
            org,
            projects: projects.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            apps: apps.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            envs: envs.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
        }
    }

    fn target(
        project: u128,
        app: Option<u128>,
        envs: &'static [uuid::Uuid],
        unattributed: bool,
    ) -> QueueTarget<'static> {
        QueueTarget {
            project_id: uuid::Uuid::from_u128(project),
            app_id: app.map(uuid::Uuid::from_u128),
            env_enrollments: envs,
            includes_unattributed: unattributed,
        }
    }

    #[test]
    fn org_reach_covers_everything() {
        assert!(covers(
            &reach(true, &[], &[], &[]),
            &target(1, Some(2), &[], true)
        ));
        assert!(covers(
            &reach(true, &[], &[], &[]),
            &target(1, None, &[], false)
        ));
    }

    #[test]
    fn a_project_grant_covers_its_apps_and_its_monitors() {
        let r = reach(false, &[1], &[], &[]);
        assert!(covers(&r, &target(1, Some(2), &[], true)));
        assert!(
            covers(&r, &target(1, None, &[], false)),
            "uptime needs project reach"
        );
        assert!(!covers(&r, &target(9, Some(2), &[], true)));
    }

    #[test]
    fn uptime_is_refused_to_app_and_env_scoped_members() {
        // Every monitor read in the product is
        // `authorize_project(user, project, monitor:read)`, which resolves with
        // `app: None, env: None`, and `grant_applies` never lets a `Scope::App`
        // or `Scope::Env` grant satisfy that. An app-scoped member gets 403 from
        // every monitor endpoint — so mailing them monitor names, targets,
        // causes and incident ids would hand over exactly what the API refuses.
        assert!(!covers(
            &reach(false, &[], &[2], &[]),
            &target(1, None, &[], false)
        ));
        assert!(!covers(
            &reach(false, &[], &[], &[3]),
            &target(1, None, &[], false)
        ));
    }

    #[test]
    fn an_app_grant_covers_its_own_app_only() {
        let r = reach(false, &[], &[2], &[]);
        assert!(covers(&r, &target(1, Some(2), &[], true)));
        assert!(!covers(&r, &target(1, Some(99), &[], true)));
    }

    #[test]
    fn an_env_grant_needs_every_listed_enrollment() {
        const E3: uuid::Uuid = uuid::Uuid::from_u128(3);
        const E4: uuid::Uuid = uuid::Uuid::from_u128(4);
        static BOTH: [uuid::Uuid; 2] = [E3, E4];
        static ONE: [uuid::Uuid; 1] = [E3];
        static SIBLING: [uuid::Uuid; 1] = [E4];

        let holds_both = reach(false, &[], &[], &[3, 4]);
        let holds_one = reach(false, &[], &[], &[3]);

        assert!(covers(&holds_both, &target(1, Some(2), &BOTH, false)));
        assert!(covers(&holds_one, &target(1, Some(2), &ONE, false)));
        assert!(
            !covers(&holds_one, &target(1, Some(2), &BOTH, false)),
            "partial coverage of the listed enrollments must be refused"
        );
        assert!(!covers(&holds_one, &target(1, Some(2), &SIBLING, false)));
    }

    #[test]
    fn an_empty_environment_list_is_never_read_as_unconstrained() {
        // A probe with no environment predicate counts across every enrollment
        // AND unattributed rows, so it needs app-level reach. Reading NULL as
        // "unconstrained" leaks; reading it as "nothing" starves an env-scoped
        // subscriber silently.
        let env_only = reach(false, &[], &[], &[3]);
        assert!(!covers(&env_only, &target(1, Some(2), &[], false)));
        assert!(!covers(&env_only, &target(1, Some(2), &[], true)));
    }

    #[test]
    fn includes_unattributed_is_refused_to_an_env_grant() {
        const E3: uuid::Uuid = uuid::Uuid::from_u128(3);
        static ONE: [uuid::Uuid; 1] = [E3];
        let env_only = reach(false, &[], &[], &[3]);
        assert!(covers(&env_only, &target(1, Some(2), &ONE, false)));
        assert!(
            !covers(&env_only, &target(1, Some(2), &ONE, true)),
            "unattributed rows belong to no single environment"
        );
    }
}
