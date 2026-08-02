//! Mask targets, as ENUMS rather than strings.
//!
//! SQL identifiers cannot be bound, so the batch statements interpolate the
//! table and column names. The worker reads `inspector_mask_actions.targets`
//! back out of Postgres in a DIFFERENT PROCESS from the one that validated it,
//! so "validated in Rust at write time" is not a control. Deserializing into
//! enums whose `as_sql()` returns `&'static str` is: an unknown pair fails
//! deserialization and the worker fails the action rather than interpolating
//! caller bytes into an unattended UPDATE running with full DB rights.

use serde::{Deserialize, Serialize};

use crate::columns::{find, ColumnKind};
use crate::path::{parse_mask_path, PathError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetTable {
    ErrorEvents,
    AnalyticsEvents,
    Transactions,
    Issues,
    EventUsers,
    Sessions,
}

impl TargetTable {
    pub const ALL: [TargetTable; 6] = [
        TargetTable::ErrorEvents,
        TargetTable::AnalyticsEvents,
        TargetTable::Transactions,
        TargetTable::Issues,
        TargetTable::EventUsers,
        TargetTable::Sessions,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            TargetTable::ErrorEvents => "error_events",
            TargetTable::AnalyticsEvents => "analytics_events",
            TargetTable::Transactions => "transactions",
            TargetTable::Issues => "issues",
            TargetTable::EventUsers => "event_users",
            TargetTable::Sessions => "sessions",
        }
    }

    pub fn from_sql(s: &str) -> Option<TargetTable> {
        TargetTable::ALL.into_iter().find(|t| t.as_sql() == s)
    }

    /// Partitioned tables get a day loop and an `occurred_at` range on every
    /// statement; rollups get one keyset pass filtered on `app_id`.
    ///
    /// Every rollup keysets on the bare `id` PK — including `sessions`, whose
    /// `(started_at, id)` ordering would buy locality and nothing else: `id`
    /// is a unique non-null PK on all six maskable tables, so `id > $cursor
    /// ORDER BY id` already visits every row exactly once. A second keyset
    /// shape here would be a second cursor encoding in `BatchOutcome` and a
    /// second resume path to get wrong. (This supersedes design §9's
    /// "`(started_at, id)` for `sessions`".)
    pub fn is_partitioned(self) -> bool {
        matches!(
            self,
            TargetTable::ErrorEvents | TargetTable::AnalyticsEvents | TargetTable::Transactions
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetColumn {
    Tags,
    Contexts,
    Extra,
    Context,
    EventUser,
    Breadcrumbs,
    Sdk,
    DebugMeta,
    Stacktrace,
    StacktraceSymbolicated,
    Message,
    ExceptionValue,
    ExceptionType,
    Title,
    Culprit,
    Properties,
    Url,
}

impl TargetColumn {
    pub const ALL: [TargetColumn; 17] = [
        TargetColumn::Tags,
        TargetColumn::Contexts,
        TargetColumn::Extra,
        TargetColumn::Context,
        TargetColumn::EventUser,
        TargetColumn::Breadcrumbs,
        TargetColumn::Sdk,
        TargetColumn::DebugMeta,
        TargetColumn::Stacktrace,
        TargetColumn::StacktraceSymbolicated,
        TargetColumn::Message,
        TargetColumn::ExceptionValue,
        TargetColumn::ExceptionType,
        TargetColumn::Title,
        TargetColumn::Culprit,
        TargetColumn::Properties,
        TargetColumn::Url,
    ];

    pub fn as_sql(self) -> &'static str {
        match self {
            TargetColumn::Tags => "tags",
            TargetColumn::Contexts => "contexts",
            TargetColumn::Extra => "extra",
            TargetColumn::Context => "context",
            TargetColumn::EventUser => "event_user",
            TargetColumn::Breadcrumbs => "breadcrumbs",
            TargetColumn::Sdk => "sdk",
            TargetColumn::DebugMeta => "debug_meta",
            TargetColumn::Stacktrace => "stacktrace",
            TargetColumn::StacktraceSymbolicated => "stacktrace_symbolicated",
            TargetColumn::Message => "message",
            TargetColumn::ExceptionValue => "exception_value",
            TargetColumn::ExceptionType => "exception_type",
            TargetColumn::Title => "title",
            TargetColumn::Culprit => "culprit",
            TargetColumn::Properties => "properties",
            TargetColumn::Url => "url",
        }
    }

    pub fn from_sql(s: &str) -> Option<TargetColumn> {
        TargetColumn::ALL.into_iter().find(|c| c.as_sql() == s)
    }
}

/// One fully resolved mask target. `path` is `""` for a TEXT column (the whole
/// value is replaced) and a wire-form mask path for a jsonb column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskTarget {
    pub table: TargetTable,
    pub column: TargetColumn,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// The `(table, column)` pair is not in the inventory at all.
    NoSuchColumn,
    /// In the inventory, but `maskable = false`.
    NotMaskable,
    /// A jsonb column with no path would collapse the entire column.
    MissingPath,
    /// A TEXT column takes the whole value; a path there means the caller
    /// believes something is happening that is not.
    PathOnTextColumn,
    Path(PathError),
}

pub fn validate_target(t: &MaskTarget) -> Result<(), TargetError> {
    let Some(entry) = find(t.table.as_sql(), t.column.as_sql()) else {
        return Err(TargetError::NoSuchColumn);
    };
    if !entry.maskable {
        return Err(TargetError::NotMaskable);
    }
    match entry.kind {
        ColumnKind::Text => {
            if t.path.is_empty() {
                Ok(())
            } else {
                Err(TargetError::PathOnTextColumn)
            }
        }
        ColumnKind::Jsonb => {
            if t.path.is_empty() {
                return Err(TargetError::MissingPath);
            }
            parse_mask_path(&t.path)
                .map(|_| ())
                .map_err(TargetError::Path)
        }
    }
}

/// Everything a mask on `t` must ALSO rewrite, `t` first.
///
/// Applied at PREVIEW time and frozen into the action's `targets`, so confirm
/// can never widen what was counted and shown. Nothing outside this map
/// auto-expands.
pub fn expand_targets(t: &MaskTarget) -> Vec<MaskTarget> {
    let mut out = vec![t.clone()];
    let mut push = |table: TargetTable, column: TargetColumn, path: &str| {
        let m = MaskTarget {
            table,
            column,
            path: path.to_string(),
        };
        if !out.contains(&m) {
            out.push(m);
        }
    };
    match (t.table, t.column) {
        // `error_events.title` is derived server-side by `build_title(exc,
        // message)` and has NO wire field, so forward enforcement cannot reach
        // it directly: the next event writes a raw title and the Issues page
        // shows the PII again while the audit row says success. Mask the
        // inputs. `issues.title` additionally gets the sticky guard in
        // `upsert_issue`, because `exception_type` is concatenated into the
        // title too and the 30s cache window restores the raw string on the
        // very next occurrence.
        (TargetTable::ErrorEvents, TargetColumn::Title) => {
            push(TargetTable::Issues, TargetColumn::Title, "");
            push(TargetTable::ErrorEvents, TargetColumn::ExceptionValue, "");
            push(TargetTable::ErrorEvents, TargetColumn::ExceptionType, "");
            push(TargetTable::ErrorEvents, TargetColumn::Message, "");
        }
        (TargetTable::ErrorEvents, TargetColumn::Culprit) => {
            push(TargetTable::Issues, TargetColumn::Culprit, "");
        }
        // The symbolicated copy holds the same frame data.
        (TargetTable::ErrorEvents, TargetColumn::Stacktrace) => {
            push(
                TargetTable::ErrorEvents,
                TargetColumn::StacktraceSymbolicated,
                &t.path,
            );
        }
        // `bump_session` snapshots the same enriched jsonb on every event.
        (TargetTable::ErrorEvents | TargetTable::AnalyticsEvents, TargetColumn::Context) => {
            push(TargetTable::Sessions, TargetColumn::Context, &t.path);
        }
        _ => {}
    }
    out
}

/// Cap on the resolved `(app, enrollment)` list a single scan may carry. A
/// project with more apps than this is a deployment-shaped problem, and a
/// scan whose target list does not fit in one jsonb column is not resumable.
pub const MAX_SCAN_PAIRS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyTargetType {
    Project,
    App,
    AppEnv,
}

impl PolicyTargetType {
    pub fn as_sql(self) -> &'static str {
        match self {
            PolicyTargetType::Project => "project",
            PolicyTargetType::App => "app",
            PolicyTargetType::AppEnv => "app_env",
        }
    }

    pub fn from_sql(s: &str) -> Option<PolicyTargetType> {
        match s {
            "project" => Some(PolicyTargetType::Project),
            "app" => Some(PolicyTargetType::App),
            "app_env" => Some(PolicyTargetType::AppEnv),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyNode {
    pub target_type: PolicyTargetType,
    /// For `AppEnv` this is an `app_environments.id` — the ENROLLMENT id,
    /// never a catalogue `environments.id`. Event rows store the enrollment
    /// id, so the other one matches nothing and the scan silently reads zero
    /// rows.
    pub target_id: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanPair {
    pub app_id: uuid::Uuid,
    /// `None` = the unattributed bucket, reachable only from an app- or
    /// project-scoped policy.
    pub app_env_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTargets {
    pub pairs: Vec<ScanPair>,
    /// How many pairs a more-specific policy row took away. Goes into
    /// `coverage_note` so an operator can see why a scan covered less than the
    /// policy's node.
    pub subtracted: usize,
    pub truncated: bool,
}

/// Whether a policy at this level scans rollup tables and `_default` sweeps.
///
/// False for `app_env`, because neither class can be environment-attributed:
/// `issues` and `event_users` carry `app_id` only, and a `_default` row's
/// `environment_id` is whatever the edge resolved, with no way to bound the
/// sweep to one enrollment without an index that does not exist.
pub fn include_rollups(level: PolicyTargetType) -> bool {
    !matches!(level, PolicyTargetType::AppEnv)
}

/// Subtract every pair covered by a MORE SPECIFIC policy row, enabled or not.
///
/// A union of tracked keys across levels was rejected: it makes "turn this off
/// for staging" inexpressible, because a narrow row could only ever add, and
/// it would force the schedule to be merged too, which is meaningless.
pub fn resolve_targets(
    node: PolicyNode,
    pairs: &[ScanPair],
    narrower: &[PolicyNode],
) -> ResolvedTargets {
    let mut kept: Vec<ScanPair> = Vec::with_capacity(pairs.len());
    let mut subtracted = 0usize;
    for p in pairs {
        let covered = narrower.iter().any(|n| {
            if *n == node {
                return false;
            }
            match n.target_type {
                PolicyTargetType::AppEnv => p.app_env_id == Some(n.target_id),
                PolicyTargetType::App => p.app_id == n.target_id,
                // A project row can only be narrower than another project row
                // if they are the same node, which is excluded above.
                PolicyTargetType::Project => false,
            }
        });
        if covered {
            subtracted += 1;
        } else {
            kept.push(*p);
        }
    }
    let truncated = kept.len() > MAX_SCAN_PAIRS;
    kept.truncate(MAX_SCAN_PAIRS);
    ResolvedTargets {
        pairs: kept,
        subtracted,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(table: TargetTable, column: TargetColumn, path: &str) -> MaskTarget {
        MaskTarget {
            table,
            column,
            path: path.to_string(),
        }
    }

    #[test]
    fn as_sql_is_static_and_round_trips() {
        for tt in TargetTable::ALL {
            assert_eq!(TargetTable::from_sql(tt.as_sql()), Some(tt));
        }
        for tc in TargetColumn::ALL {
            assert_eq!(TargetColumn::from_sql(tc.as_sql()), Some(tc));
        }
        assert_eq!(TargetTable::from_sql("auth_sessions"), None);
        assert_eq!(
            TargetTable::from_sql("error_events; DROP TABLE users"),
            None
        );
    }

    #[test]
    fn a_target_outside_the_inventory_is_rejected() {
        // `identities` is scan-only, so it is not a TargetTable at all, and
        // `stacktrace` is in the inventory but not maskable.
        assert_eq!(
            validate_target(&t(
                TargetTable::ErrorEvents,
                TargetColumn::Stacktrace,
                "abs_path"
            )),
            Err(TargetError::NotMaskable)
        );
        assert_eq!(
            validate_target(&t(TargetTable::Issues, TargetColumn::Extra, "")),
            Err(TargetError::NoSuchColumn)
        );
    }

    #[test]
    fn a_text_column_takes_the_whole_value_and_rejects_a_path() {
        assert_eq!(
            validate_target(&t(TargetTable::Issues, TargetColumn::Title, "")),
            Ok(())
        );
        assert_eq!(
            validate_target(&t(TargetTable::Issues, TargetColumn::Title, "a.b")),
            Err(TargetError::PathOnTextColumn)
        );
    }

    #[test]
    fn a_jsonb_column_requires_a_path() {
        assert_eq!(
            validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "")),
            Err(TargetError::MissingPath)
        );
        assert_eq!(
            validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "a.b")),
            Ok(())
        );
        assert_eq!(
            validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "a.3.b")),
            Err(TargetError::Path(crate::path::PathError::NumericIndex))
        );
    }

    /// `error_events.title` is derived SERVER-SIDE by `build_title(exc,
    /// message)` and has NO wire field, so `apply_wire` has nothing to mask
    /// for that target: the first event after the mask writes a raw title and
    /// the Issues page shows the PII again while the audit row reports
    /// success. Masking the INPUTS `build_title`/`build_culprit` consume is
    /// what makes forward enforcement actually reach them.
    #[test]
    fn title_expands_to_the_wire_sources_and_issues() {
        let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Title, ""));
        let pairs: Vec<(&str, &str)> = out
            .iter()
            .map(|m| (m.table.as_sql(), m.column.as_sql()))
            .collect();
        assert!(pairs.contains(&("error_events", "title")));
        assert!(pairs.contains(&("issues", "title")));
        assert!(pairs.contains(&("error_events", "exception_value")));
        assert!(pairs.contains(&("error_events", "exception_type")));
        assert!(pairs.contains(&("error_events", "message")));
    }

    #[test]
    fn culprit_expands_to_issues_culprit() {
        let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Culprit, ""));
        let pairs: Vec<(&str, &str)> = out
            .iter()
            .map(|m| (m.table.as_sql(), m.column.as_sql()))
            .collect();
        assert!(pairs.contains(&("issues", "culprit")));
    }

    /// The symbolicated copy holds the same frame data.
    #[test]
    fn stacktrace_expands_to_the_symbolicated_copy() {
        let out = expand_targets(&t(
            TargetTable::ErrorEvents,
            TargetColumn::Stacktrace,
            "[*].abs_path",
        ));
        assert!(out
            .iter()
            .any(|m| m.column == TargetColumn::StacktraceSymbolicated && m.path == "[*].abs_path"));
    }

    /// `bump_session` snapshots the same enriched jsonb on every event, so a
    /// mask on `context` that ignores `sessions.context` leaves a live copy.
    #[test]
    fn context_expands_to_sessions_context() {
        for table in [TargetTable::ErrorEvents, TargetTable::AnalyticsEvents] {
            let out = expand_targets(&t(table, TargetColumn::Context, "user.email"));
            assert!(out.iter().any(|m| m.table == TargetTable::Sessions
                && m.column == TargetColumn::Context
                && m.path == "user.email"));
        }
    }

    #[test]
    fn everything_else_expands_to_itself() {
        let one = t(
            TargetTable::ErrorEvents,
            TargetColumn::Extra,
            "customer.email",
        );
        assert_eq!(expand_targets(&one), vec![one.clone()]);
    }

    #[test]
    fn expansion_is_deduplicated_and_contains_the_original_first() {
        let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Title, ""));
        assert_eq!(out[0].column, TargetColumn::Title);
        let mut seen = out.clone();
        seen.dedup();
        assert_eq!(seen.len(), out.len());
    }

    fn u(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    fn pair(app: u128, env: Option<u128>) -> ScanPair {
        ScanPair {
            app_id: u(app),
            app_env_id: env.map(u),
        }
    }

    fn node(kind: PolicyTargetType, id: u128) -> PolicyNode {
        PolicyNode {
            target_type: kind,
            target_id: u(id),
        }
    }

    #[test]
    fn a_project_policy_keeps_every_pair_when_nothing_is_narrower() {
        let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
        let r = resolve_targets(node(PolicyTargetType::Project, 99), &pairs, &[]);
        assert_eq!(r.pairs, pairs);
        assert_eq!(r.subtracted, 0);
    }

    /// The whole point: "most specific wins, whole row" applies to EXCLUSION as
    /// well as configuration, so the narrower row subtracts whether it is enabled
    /// or not. A disabled child policy is how an admin excludes one noisy
    /// environment, and the parent must stop walking it.
    #[test]
    fn a_narrower_app_env_row_subtracts_that_pair() {
        let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
        let r = resolve_targets(
            node(PolicyTargetType::Project, 99),
            &pairs,
            &[node(PolicyTargetType::AppEnv, 11)],
        );
        assert_eq!(r.pairs, vec![pair(1, Some(10)), pair(2, Some(20))]);
        assert_eq!(r.subtracted, 1);
    }

    #[test]
    fn a_narrower_app_row_subtracts_every_pair_of_that_app() {
        let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
        let r = resolve_targets(
            node(PolicyTargetType::Project, 99),
            &pairs,
            &[node(PolicyTargetType::App, 1)],
        );
        assert_eq!(r.pairs, vec![pair(2, Some(20))]);
        assert_eq!(r.subtracted, 2);
    }

    /// A policy never subtracts itself, or an app policy would resolve to nothing.
    #[test]
    fn a_policy_never_subtracts_its_own_node() {
        let pairs = vec![pair(1, Some(10)), pair(1, None)];
        let r = resolve_targets(
            node(PolicyTargetType::App, 1),
            &pairs,
            &[node(PolicyTargetType::App, 1)],
        );
        assert_eq!(r.pairs, pairs);
        assert_eq!(r.subtracted, 0);
    }

    /// `EnvFilter::Subset` uses `= ANY`, which never matches NULL, so
    /// unattributed rows are only reachable from an app- or project-scoped
    /// policy. An app_env narrower row must not silently take them away.
    #[test]
    fn an_app_env_narrower_row_leaves_the_unattributed_pair() {
        let pairs = vec![pair(1, Some(10)), pair(1, None)];
        let r = resolve_targets(
            node(PolicyTargetType::App, 1),
            &pairs,
            &[node(PolicyTargetType::AppEnv, 10)],
        );
        assert_eq!(r.pairs, vec![pair(1, None)]);
        assert_eq!(r.subtracted, 1);
    }

    /// Neither rollups nor `_default` sweeps can be environment-attributed —
    /// `event_users` and `issues` carry `app_id` only — so running them for an
    /// env-scoped policy would mean a policy an admin deliberately scoped to
    /// staging persisting key paths derived from production traffic, readable by
    /// anyone with pii:read on staging.
    #[test]
    fn rollup_and_default_classes_are_absent_for_an_app_env_policy() {
        assert!(!include_rollups(PolicyTargetType::AppEnv));
        assert!(include_rollups(PolicyTargetType::App));
        assert!(include_rollups(PolicyTargetType::Project));
    }

    #[test]
    fn the_pair_cap_truncates_and_is_reported() {
        let pairs: Vec<ScanPair> = (0..2_500)
            .map(|i| pair(i as u128, Some(i as u128 + 10_000)))
            .collect();
        let r = resolve_targets(node(PolicyTargetType::Project, 99), &pairs, &[]);
        assert_eq!(r.pairs.len(), MAX_SCAN_PAIRS);
        assert!(r.truncated);
    }
}
