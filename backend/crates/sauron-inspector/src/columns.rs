//! The closed inventory of columns a scan may read and a mask may write.
//!
//! Hand-verified against `\d+`, and deliberately NOT derived from the diesel
//! models: `error_events.title` / `culprit` are absent from
//! `ErrorEvent::as_select()` but are exactly what the Issues list renders, so
//! a model-walking scanner would miss the two columns most likely to carry a
//! customer's name.
//!
//! `source_table` / `source_column` on a finding always come from here, never
//! from caller bytes, because SQL identifiers cannot be bound and the batch
//! statements interpolate them.

/// How a column is read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Jsonb,
    /// Masking a TEXT column replaces the WHOLE value with `'****'`. There is
    /// no partial redaction: the workspace has no direct regex dependency and
    /// partial masking leaves recoverable residue.
    Text,
}

/// How a scan decomposes the table into units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableClass {
    /// `PARTITION BY RANGE (occurred_at)`; scanned one `(app, env, day)` at a
    /// time plus one `_default` sweep.
    Partitioned,
    /// One unit per `(app, table)`, PK keyset paginated. An at-rest mask here
    /// may be undone by the next event — see `maskable` per column.
    Rollup,
}

#[derive(Debug, Clone, Copy)]
pub struct ScanColumn {
    pub table: &'static str,
    pub column: &'static str,
    pub kind: ColumnKind,
    /// In the default set when a policy leaves `scan_columns` NULL.
    pub default_on: bool,
    /// May appear in `inspector_masked_keys` and in a mask action's targets.
    pub maskable: bool,
    /// May be returned raw by `POST /findings/{id}/reveal`.
    pub reveal_ok: bool,
    /// Rough bytes-per-row weight, used only to order units cheapest-first
    /// inside a table so a killed scan has covered the most ground.
    pub cost_class: u16,
}

/// Exactly the six tables in the `inspector_masked_keys.target_table` CHECK.
pub const MASKABLE_TABLES: [&str; 6] = [
    "error_events",
    "analytics_events",
    "transactions",
    "issues",
    "event_users",
    "sessions",
];

/// Every table a scan may read, with its unit decomposition.
pub const SCAN_TABLES: [(&str, TableClass); 8] = [
    ("error_events", TableClass::Partitioned),
    ("analytics_events", TableClass::Partitioned),
    ("transactions", TableClass::Partitioned),
    ("issues", TableClass::Rollup),
    ("event_users", TableClass::Rollup),
    ("sessions", TableClass::Rollup),
    ("identities", TableClass::Rollup),
    ("workflows", TableClass::Rollup),
];

const fn c(
    table: &'static str,
    column: &'static str,
    kind: ColumnKind,
    default_on: bool,
    maskable: bool,
    reveal_ok: bool,
    cost_class: u16,
) -> ScanColumn {
    ScanColumn {
        table,
        column,
        kind,
        default_on,
        maskable,
        reveal_ok,
        cost_class,
    }
}

pub const INVENTORY: &[ScanColumn] = &[
    // --- error_events (partitioned) ---
    c(
        "error_events",
        "tags",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        52,
    ),
    c(
        "error_events",
        "contexts",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        336,
    ),
    c(
        "error_events",
        "extra",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        317,
    ),
    c(
        "error_events",
        "context",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        447,
    ),
    c(
        "error_events",
        "event_user",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        174,
    ),
    c(
        "error_events",
        "breadcrumbs",
        ColumnKind::Jsonb,
        false,
        true,
        true,
        368,
    ),
    c(
        "error_events",
        "sdk",
        ColumnKind::Jsonb,
        false,
        true,
        true,
        64,
    ),
    // Not reveal-eligible: debug images can carry absolute build paths that
    // identify a developer's machine and, with `stacktrace_symbolicated`,
    // de-obfuscate proprietary source.
    c(
        "error_events",
        "debug_meta",
        ColumnKind::Jsonb,
        false,
        false,
        false,
        96,
    ),
    c(
        "error_events",
        "stacktrace",
        ColumnKind::Jsonb,
        false,
        false,
        false,
        623,
    ),
    // `strip_source_context` removes context_line/pre_context/post_context
    // from RESPONSES only when the caller lacks `source:read`. A pii:read
    // holder without it must not get them back through reveal.
    c(
        "error_events",
        "stacktrace_symbolicated",
        ColumnKind::Jsonb,
        false,
        false,
        false,
        700,
    ),
    c(
        "error_events",
        "message",
        ColumnKind::Text,
        true,
        true,
        true,
        80,
    ),
    c(
        "error_events",
        "exception_value",
        ColumnKind::Text,
        true,
        true,
        true,
        80,
    ),
    c(
        "error_events",
        "exception_type",
        ColumnKind::Text,
        true,
        true,
        true,
        32,
    ),
    c(
        "error_events",
        "title",
        ColumnKind::Text,
        true,
        true,
        true,
        96,
    ),
    c(
        "error_events",
        "culprit",
        ColumnKind::Text,
        true,
        true,
        true,
        96,
    ),
    // --- analytics_events (partitioned) ---
    c(
        "analytics_events",
        "properties",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        260,
    ),
    c(
        "analytics_events",
        "tags",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        52,
    ),
    c(
        "analytics_events",
        "contexts",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        200,
    ),
    c(
        "analytics_events",
        "extra",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        200,
    ),
    c(
        "analytics_events",
        "context",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        447,
    ),
    // --- transactions (partitioned) ---
    c(
        "transactions",
        "url",
        ColumnKind::Text,
        true,
        true,
        true,
        120,
    ),
    // --- issues (rollup) ---
    c("issues", "title", ColumnKind::Text, true, true, true, 96),
    c("issues", "culprit", ColumnKind::Text, true, true, true, 96),
    // --- event_users (rollup) ---
    // Maskable, but `upsert_event_user` merges with `||`, which never removes
    // keys — an at-rest mask is undone by the next identify(). Reachable
    // through FORWARD ENFORCEMENT only, and the UI says so.
    c(
        "event_users",
        "properties",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        200,
    ),
    // --- sessions (rollup) ---
    // `bump_session` writes the post-enrichment snapshot whole, so masking the
    // enriched `context` sticks on every subsequent event. `distinct_id` and
    // `ip_address` are excluded: both are `COALESCE(EXCLUDED.x, sessions.x)`,
    // so a non-null incoming value always wins.
    c(
        "sessions",
        "context",
        ColumnKind::Jsonb,
        true,
        true,
        true,
        447,
    ),
    // --- identities (rollup, SCAN ONLY) ---
    // `alias_id` and `distinct_id` ARE the identity graph. Collapsing them to
    // '****' does not redact a person — it merges every masked person into
    // one, silently and irreversibly corrupting downstream identity
    // resolution. The remedy is on the SDK side, not here.
    c(
        "identities",
        "alias_id",
        ColumnKind::Text,
        false,
        false,
        true,
        48,
    ),
    c(
        "identities",
        "distinct_id",
        ColumnKind::Text,
        false,
        false,
        true,
        48,
    ),
    // --- workflows (rollup, SCAN ONLY) ---
    // `cancel_reason` is derived server-side in process.rs from
    // properties["reason"], so there is no wire field to enforce on, and
    // `apply_workflow_lifecycle`'s CASE lets a later cancellation write the
    // raw string back over the sentinel. Mask analytics_events.properties
    // instead — that is where the bytes actually arrive.
    c(
        "workflows",
        "cancel_reason",
        ColumnKind::Text,
        false,
        false,
        true,
        64,
    ),
];

/// Look up one inventory entry. Returns `None` for anything outside the
/// allowlist — which is what makes caller-supplied table/column strings safe
/// to reject before they ever reach an interpolated identifier.
pub fn find(table: &str, column: &str) -> Option<&'static ScanColumn> {
    INVENTORY
        .iter()
        .find(|c| c.table == table && c.column == column)
}

/// The set a policy scans when `scan_columns` is NULL.
pub fn default_columns(table: &str) -> Vec<&'static ScanColumn> {
    INVENTORY
        .iter()
        .filter(|c| c.table == table && c.default_on)
        .collect()
}

pub fn table_class(table: &str) -> Option<TableClass> {
    SCAN_TABLES
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, k)| *k)
}

pub fn is_maskable_table(table: &str) -> bool {
    MASKABLE_TABLES.contains(&table)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ALLOWLIST, asserted. A denylist silently fails to protect the next
    /// account table someone adds, and S2's session work asserts this
    /// constraint on this slice.
    #[test]
    fn inventory_contains_no_account_table() {
        const FORBIDDEN: [&str; 8] = [
            "users", "session", "token", "role", "grant", "secret", "mail", "channel",
        ];
        for c in INVENTORY {
            // `sessions` and `event_users` are telemetry rollups and are
            // allowed by EXACT name; anything else matching a forbidden
            // substring is not. The exemption stays exact-name rather than
            // dropping "session"/"users" from FORBIDDEN, because that is what
            // still catches the account tables `users` and `auth_sessions`.
            if c.table == "sessions" || c.table == "event_users" {
                continue;
            }
            for bad in FORBIDDEN {
                assert!(
                    !c.table.contains(bad),
                    "{} looks like an account table and must not be scannable",
                    c.table
                );
            }
        }
    }

    /// The maskable subset of the inventory must equal the six tables in the
    /// `inspector_masked_keys.target_table` CHECK, exactly. A masked-key row
    /// for a scan-only table would be read by the pipeline enforcer and the
    /// retro-mask, both of which would report success on a write the next
    /// event overwrites.
    #[test]
    fn maskable_subset_matches_the_check_constraint() {
        let mut from_inventory: Vec<&str> = INVENTORY
            .iter()
            .filter(|c| c.maskable)
            .map(|c| c.table)
            .collect();
        from_inventory.sort_unstable();
        from_inventory.dedup();
        let mut expected = MASKABLE_TABLES.to_vec();
        expected.sort_unstable();
        assert_eq!(from_inventory, expected);
    }

    /// devices has no maskable column at all: `upsert_device`'s DO UPDATE is
    /// `family = COALESCE(EXCLUDED.family, devices.family)`, the values are
    /// derived server-side by `enrich`, and there is no wire field for the
    /// enforcer to touch. Offering it would retro-succeed and be overwritten
    /// by the next event from that device, permanently, with a green badge.
    #[test]
    fn scan_only_tables_are_never_maskable() {
        for table in ["devices", "identities", "workflows"] {
            assert!(
                INVENTORY.iter().any(|c| c.table == table) || table == "devices",
                "{table} must still be reachable by a scan"
            );
            assert!(
                !INVENTORY.iter().any(|c| c.table == table && c.maskable),
                "{table} must never be a mask target"
            );
        }
    }

    /// error_events.title / culprit are absent from `ErrorEvent::as_select()`
    /// but ARE what the Issues list renders. A model-walking scanner misses
    /// them; this inventory is hand-verified against `\d+` for that reason.
    #[test]
    fn title_and_culprit_are_scannable_on_error_events() {
        for col in ["title", "culprit"] {
            let c = find("error_events", col).expect("missing from inventory");
            assert_eq!(c.kind, ColumnKind::Text);
            assert!(c.default_on);
            assert!(c.maskable);
        }
    }

    /// Source lines are verbatim customer source. A `pii:read` holder without
    /// `source:read` could otherwise track the key `pre_context`, reveal, and
    /// receive de-obfuscated proprietary source.
    #[test]
    fn source_bearing_columns_are_not_reveal_eligible() {
        for col in ["stacktrace", "stacktrace_symbolicated", "debug_meta"] {
            let c = find("error_events", col).expect("missing from inventory");
            assert!(!c.reveal_ok, "{col} must not be reveal-eligible");
            assert!(!c.default_on, "{col} must be opt-in");
        }
    }

    /// `transactions` is PARTITIONED (migration 000013 declares
    /// `PARTITION BY RANGE (occurred_at)`) and `sauron-tier` lists it in
    /// TIERED_TABLES. Treating it as a rollup would mean no `occurred_at`
    /// predicate, an `id`-keyset over a column that is not unique across
    /// partitions, and a `_default` sweep that double-scans the same rows.
    #[test]
    fn transactions_is_partitioned_not_a_rollup() {
        assert_eq!(table_class("transactions"), Some(TableClass::Partitioned));
        assert_eq!(table_class("issues"), Some(TableClass::Rollup));
        assert_eq!(table_class("auth_sessions"), None);
    }

    #[test]
    fn default_columns_are_the_bold_set() {
        let mut d: Vec<&str> = default_columns("error_events")
            .iter()
            .map(|c| c.column)
            .collect();
        d.sort_unstable();
        assert_eq!(
            d,
            [
                "context",
                "contexts",
                "culprit",
                "event_user",
                "exception_type",
                "exception_value",
                "extra",
                "message",
                "tags",
                "title"
            ]
        );
    }
}
