//! Tenant + environment scope for telemetry reads.
//!
//! Replaces the bare `app_id: Uuid` that ~36 read functions took. The point is
//! the compile error: adding the environment dimension cannot be done to some
//! reads and forgotten on others, because every call site must construct one.
//!
//! Note what this does NOT buy: a function body can destructure `app_id` and
//! ignore `env`, and it will compile. `tests/env_scoping.rs` is what closes
//! that gap, and for the raw-SQL reads it is the only thing that can.

use uuid::Uuid;

/// Which environments a read covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvFilter {
    /// Every environment, including rows with none. The picker's default, and
    /// what an absent `environment_id` query parameter means.
    All,
    One(Uuid),
    /// Rows whose `environment_id IS NULL` — signals ingested before Slice 1,
    /// or under the old per-app environment cap. Surfaced rather than hidden so
    /// "All" equals the sum of the individual environments instead of exceeding
    /// it, which would be unexplainable to a user reading the numbers.
    Unattributed,
}

impl EnvFilter {
    /// SQL to AND into a raw `sql_query`, or `""` for `All`.
    ///
    /// `bind_index` is the next free positional bind. **Only `One` consumes
    /// it** — `All` emits nothing and `Unattributed` emits a literal `IS NULL`.
    /// A caller that assumes an index is always consumed will shift every
    /// subsequent bind by one, which is the single easiest way to get this
    /// wrong. Pair every call with `bind_uuid()`.
    pub fn sql_fragment(&self, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND environment_id = ${bind_index}"),
            EnvFilter::Unattributed => " AND environment_id IS NULL".to_string(),
        }
    }

    /// `sql_fragment` for a query where `environment_id` needs a table alias.
    pub fn sql_fragment_for(&self, alias: &str, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND {alias}.environment_id = ${bind_index}"),
            EnvFilter::Unattributed => format!(" AND {alias}.environment_id IS NULL"),
        }
    }

    /// The value for the bind `sql_fragment` reserved, or `None` if it reserved
    /// none. Bind it only when this returns `Some`.
    pub fn bind_uuid(&self) -> Option<Uuid> {
        match self {
            EnvFilter::One(id) => Some(*id),
            EnvFilter::All | EnvFilter::Unattributed => None,
        }
    }
}

/// Tenant + environment scope for a telemetry read.
#[derive(Debug, Clone, Copy)]
pub struct ReadScope {
    pub app_id: Uuid,
    pub env: EnvFilter,
}

impl ReadScope {
    pub fn new(app_id: Uuid, env: EnvFilter) -> Self {
        Self { app_id, env }
    }

    /// Scope covering every environment — for callers that genuinely have no
    /// environment context, and for tests.
    pub fn all(app_id: Uuid) -> Self {
        Self {
            app_id,
            env: EnvFilter::All,
        }
    }
}

/// Apply an [`EnvFilter`] to a boxed diesel query over a table with an
/// `environment_id` column.
///
/// A macro rather than a function for the reason `query_plan/issues.rs`
/// documents: a generic bounded only by `Column<Table = …>` cannot prove the
/// downstream diesel operator obligations, because the compiler cannot see a
/// *specific* column's `IsAggregate`. Expanded once per concrete table, where
/// the real diesel-generated types are visible.
///
/// The `.filter()`/`.eq()`/`.is_null()` calls this expands to resolve at the
/// **call site**, not here, so the caller needs `diesel::prelude::*` in
/// scope. Missing it is a loud compile error (`E0599`, rustc naming the
/// missing trait) rather than a silent bug, and every real call site in
/// `repo.rs` already has the import — but it costs nothing to say so here.
#[macro_export]
macro_rules! scope_env {
    ($q:expr, $table:ident, $env:expr) => {
        match $env {
            $crate::scope::EnvFilter::All => $q,
            $crate::scope::EnvFilter::One(id) => $q.filter($table::environment_id.eq(id)),
            $crate::scope::EnvFilter::Unattributed => $q.filter($table::environment_id.is_null()),
        }
    };
}

/// `scope_env!`'s three arms — `All => $q`, `One(id) => .eq(id)`,
/// `Unattributed => .is_null()` — all typecheck identically against any
/// table with an `environment_id` column. Swap `All` and `Unattributed` and
/// the crate still compiles and `cargo test --workspace` still passes,
/// because nothing here forces the compiler to check *which* predicate (or
/// lack of one) came out the other side. Only asserting on the emitted SQL
/// via `debug_query` can distinguish the three, so that's what these tests
/// do, over `analytics_events` (a real table with a nullable
/// `environment_id`). Five later tasks (S2 tasks 5-9) build on this mapping
/// being right; a regression here would surface only as silently wrong
/// environment scoping in whichever of those happens to get its own
/// behavioural test first.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::analytics_events;
    use diesel::debug_query;
    use diesel::pg::Pg;
    use diesel::prelude::*;
    use uuid::Uuid;

    #[test]
    fn scope_env_all_emits_no_environment_predicate() {
        let query = analytics_events::table
            .select(analytics_events::id)
            .into_boxed();
        let scoped = scope_env!(query, analytics_events, EnvFilter::All);
        let sql = debug_query::<Pg, _>(&scoped).to_string();
        assert!(
            !sql.contains("environment_id"),
            "All must not touch environment_id at all: {sql}"
        );
    }

    #[test]
    fn scope_env_one_emits_an_equality_bind() {
        let id = Uuid::from_u128(42);
        let query = analytics_events::table
            .select(analytics_events::id)
            .into_boxed();
        let scoped = scope_env!(query, analytics_events, EnvFilter::One(id));
        let sql = debug_query::<Pg, _>(&scoped).to_string();
        assert!(
            sql.contains(r#""analytics_events"."environment_id" = $1"#),
            "{sql}"
        );
    }

    #[test]
    fn scope_env_unattributed_emits_is_null_and_no_bind() {
        let query = analytics_events::table
            .select(analytics_events::id)
            .into_boxed();
        let scoped = scope_env!(query, analytics_events, EnvFilter::Unattributed);
        let sql = debug_query::<Pg, _>(&scoped).to_string();
        assert!(
            sql.contains(r#""analytics_events"."environment_id" IS NULL"#),
            "{sql}"
        );
        assert!(
            !sql.contains("environment_id\" = $"),
            "Unattributed must not bind an equality: {sql}"
        );
    }

    #[test]
    fn all_reserves_no_bind_and_emits_nothing() {
        let f = EnvFilter::All;
        assert_eq!(f.sql_fragment(3), "");
        assert_eq!(f.bind_uuid(), None);
    }

    #[test]
    fn one_reserves_the_given_bind_index() {
        let id = Uuid::from_u128(7);
        let f = EnvFilter::One(id);
        assert_eq!(f.sql_fragment(3), " AND environment_id = $3");
        assert_eq!(f.bind_uuid(), Some(id));
    }

    /// Unattributed needs no bind: `IS NULL` is a literal predicate. A caller
    /// that reserved an index for it would leave a gap in the positional
    /// sequence and every later bind would be off by one.
    #[test]
    fn unattributed_emits_is_null_and_reserves_no_bind() {
        let f = EnvFilter::Unattributed;
        assert_eq!(f.sql_fragment(3), " AND environment_id IS NULL");
        assert_eq!(f.bind_uuid(), None);
    }

    /// The fragment is table-qualifiable for queries that join, where a bare
    /// `environment_id` would be ambiguous.
    #[test]
    fn qualified_fragment_prefixes_the_table() {
        let id = Uuid::from_u128(9);
        assert_eq!(
            EnvFilter::One(id).sql_fragment_for("e", 4),
            " AND e.environment_id = $4"
        );
        assert_eq!(
            EnvFilter::Unattributed.sql_fragment_for("e", 4),
            " AND e.environment_id IS NULL"
        );
        assert_eq!(EnvFilter::All.sql_fragment_for("e", 4), "");
    }
}
