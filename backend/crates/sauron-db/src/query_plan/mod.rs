//! Lowering a validated `sauron_query::ResolvedNode` into a diesel boxed query.
//!
//! This is the security boundary's second half. `sauron-query::resolve` guarantees
//! every field is a `&'static Dimension` from the catalog; this module guarantees
//! that only those `&'static str`s ever reach SQL text. Every caller-supplied
//! value — including JSON path segments and tag keys — travels as a bind.
//!
//! Testable without a database: `diesel::debug_query::<Pg, _>` renders the SQL and
//! the binds with no connection, so the whole mapping is asserted in CI.

pub mod cursor;
pub mod events;
pub mod issues;
pub mod occurrences;
pub mod prepare;
pub mod sessions;

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use diesel::expression::BoxableExpression;
use diesel::pg::Pg;
use diesel::sql_types::{Bool, Nullable};
use uuid::Uuid;

/// A boxed boolean fragment over one table.
///
/// `Nullable<Bool>` and not `Bool`: a comparison against a nullable column has
/// `SqlType = Nullable<Bool>`, and boxing it as `Bool` fails to compile. Leaves on
/// non-nullable columns are lifted with `.nullable()`, which retypes only and emits
/// no SQL difference.
pub type Frag<T> = Box<dyn BoxableExpression<T, Pg, SqlType = Nullable<Bool>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The dimension is declared in the catalog but its storage does not exist yet.
    NotYetSupported { field: String },
    /// The dimension exists but not for this resource.
    UnsupportedOnResource { field: String },
    /// The value could not be lowered (e.g. a list where a scalar was required).
    BadValue { field: String },
    /// The async `prepare` pass's database round-trip (currently: the
    /// environment-name batch lookup) failed. Never a caller mistake — a
    /// transport hiccup or a down database — kept distinct from `BadValue`
    /// so a future caller can tell "your query is invalid" apart from "we
    /// couldn't ask the database".
    Database(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NotYetSupported { field } => write!(
                f,
                "`{field}` is not searchable yet — it needs the issue dimension rollup"
            ),
            PlanError::UnsupportedOnResource { field } => {
                write!(f, "`{field}` cannot be used on this view")
            }
            PlanError::BadValue { field } => write!(f, "invalid value for `{field}`"),
            PlanError::Database(message) => write!(f, "search query failed: {message}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// Everything the synchronous lowering needs that required a database round-trip
/// or a clock read.
#[derive(Debug, Clone)]
pub struct PrepCtx {
    /// Environment NAME -> id. `None` means the name does not exist in this app,
    /// which must lower to a predicate matching nothing (never to "no filter").
    pub environments: HashMap<String, Option<Uuid>>,
    /// Resolved once so relative timestamps produce deterministic, assertable SQL.
    pub now: DateTime<Utc>,
}

use diesel::BoolExpressionMethods;
use sauron_query::{ResolvedNode, ResolvedPredicate};

/// Per-resource knowledge: how one predicate and one free-text term become SQL
/// against a concrete table. Everything above this is shared.
pub trait ResourceLower {
    type Table: 'static;

    /// `negate` is passed IN rather than applied outside, so the leaf can emit the
    /// NULL-safe form for its own column. See `lower`'s De Morgan normalisation.
    fn leaf(
        &self,
        p: &ResolvedPredicate,
        ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<Self::Table>, PlanError>;

    fn text(&self, term: &str) -> Frag<Self::Table>;
}

/// A fragment that is always true, for an empty query.
fn always_true<T: 'static>() -> Frag<T> {
    Box::new(diesel::dsl::sql::<Nullable<Bool>>("TRUE"))
}

// ===========================================================================
// JSON path helpers, shared by every resource with `Store::JsonRoot` leaves
// (Occurrences today, Events next). Kept here rather than duplicated per
// module so both lowerers apply the exact same containment shape.
// ===========================================================================

/// The ordered key path to build a nested-object bind from, for one
/// `Store::JsonRoot` predicate.
///
/// `ResolvedPredicate.path` already folds `Store::JsonRoot`'s `prefix` in as
/// its first segment whenever the field was written with a dotted remainder
/// (`sauron_query::resolve::resolve_field`'s dotted-path branch formats
/// `"{prefix}.{remainder}"`), so `prefix` must NOT be prepended again in that
/// case — doing so would duplicate it (`os.name` must become `["os",
/// "name"]`, not `["os", "os", "name"]`).
///
/// `path` is only `None` when the field was written bare, with no dotted
/// remainder at all (e.g. `has:os` rather than `has:os.name`) — the exact
/// match branch of `resolve_field` returns `None` unconditionally. In that
/// case the dimension's own `prefix` (if any) is the sole segment, so
/// `has:os` still means "does the top-level `os` key exist in `context`".
/// When `prefix` is ALSO empty (the column itself IS the object — `extra`,
/// `contexts`, `user`, `sdk`), there is no key to select at all, and `None` is
/// returned so the caller can reject it rather than silently building a
/// vacuous filter.
pub(crate) fn json_path_segments(prefix: &str, path: Option<&str>) -> Option<Vec<String>> {
    match path {
        Some(p) => Some(p.split('.').map(str::to_string).collect()),
        None if !prefix.is_empty() => Some(vec![prefix.to_string()]),
        None => None,
    }
}

/// Nest `leaf` inside successive single-key objects named by `segments`,
/// innermost first, e.g. `(["os", "name"], "Linux")` ->
/// `{"os": {"name": "Linux"}}`. `segments` must be non-empty (callers get it
/// from `json_path_segments`, which never returns `Some(vec![])`).
///
/// This is the ONLY place a caller-supplied JSON path touches the query: the
/// result is bound as one `Jsonb` parameter, so the path itself never reaches
/// SQL text.
pub(crate) fn nest_json_object(segments: &[String], leaf: serde_json::Value) -> serde_json::Value {
    let mut value = leaf;
    for seg in segments.iter().rev() {
        let mut object = serde_json::Map::with_capacity(1);
        object.insert(seg.clone(), value);
        value = serde_json::Value::Object(object);
    }
    value
}

pub fn lower<L: ResourceLower>(
    node: &ResolvedNode,
    l: &L,
    ctx: &PrepCtx,
) -> Result<Frag<L::Table>, PlanError> {
    lower_inner(node, l, ctx, false)
}

/// `negate` is threaded down and flipped by `Not`, rather than being applied at the
/// point a `Not` is seen. That is De Morgan performed lazily: by the time a leaf is
/// reached, `negate` says whether an odd number of `Not`s enclose it, and the
/// combinators swap And<->Or whenever it is set.
fn lower_inner<L: ResourceLower>(
    node: &ResolvedNode,
    l: &L,
    ctx: &PrepCtx,
    negate: bool,
) -> Result<Frag<L::Table>, PlanError> {
    match node {
        ResolvedNode::Pred(p) => l.leaf(p, ctx, negate),
        ResolvedNode::Text(t) => {
            let frag = l.text(t);
            Ok(if negate {
                Box::new(diesel::dsl::not(frag))
            } else {
                frag
            })
        }
        ResolvedNode::Not(inner) => lower_inner(inner, l, ctx, !negate),
        // Under negation And becomes Or and vice versa.
        ResolvedNode::And(v) => combine(v, l, ctx, negate, !negate),
        ResolvedNode::Or(v) => combine(v, l, ctx, negate, negate),
    }
}

/// `conjunction = true` joins with AND, `false` with OR.
fn combine<L: ResourceLower>(
    parts: &[ResolvedNode],
    l: &L,
    ctx: &PrepCtx,
    negate: bool,
    conjunction: bool,
) -> Result<Frag<L::Table>, PlanError> {
    let mut it = parts.iter();
    let first = match it.next() {
        Some(n) => lower_inner(n, l, ctx, negate)?,
        None => return Ok(always_true()),
    };
    let mut acc = first;
    for n in it {
        let next = lower_inner(n, l, ctx, negate)?;
        acc = if conjunction {
            Box::new(acc.and(next))
        } else {
            Box::new(acc.or(next))
        };
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::error_events;
    use diesel::prelude::*;

    #[test]
    fn fragments_box_and_combine_as_nullable_bool() {
        // A NON-nullable column must be lifted with `.nullable()`; a nullable one
        // is already the right type. Both must box into the same `Frag`.
        let a: Frag<error_events::table> = Box::new(error_events::level.eq("error").nullable());
        let b: Frag<error_events::table> = Box::new(error_events::session_id.eq("s1"));
        let combined: Frag<error_events::table> = Box::new(a.and(b));
        let q = error_events::table.into_boxed().filter(combined);
        let sql = diesel::debug_query::<diesel::pg::Pg, _>(&q).to_string();
        assert!(sql.contains(r#""error_events"."level" = $1"#), "{sql}");
        assert!(sql.contains(r#""error_events"."session_id" = $2"#), "{sql}");
        // `.and()` emits parentheses via Grouped, so precedence is structural.
        assert!(sql.contains("AND"), "{sql}");
    }

    #[test]
    fn nullable_lift_changes_no_sql() {
        let plain = error_events::table
            .into_boxed()
            .filter(error_events::level.eq("error"));
        let lifted = error_events::table
            .into_boxed()
            .filter(error_events::level.eq("error").nullable());
        assert_eq!(
            diesel::debug_query::<diesel::pg::Pg, _>(&plain).to_string(),
            diesel::debug_query::<diesel::pg::Pg, _>(&lifted).to_string()
        );
    }

    #[test]
    fn plan_errors_name_the_field() {
        let e = PlanError::NotYetSupported {
            field: "environment".into(),
        };
        assert!(e.to_string().contains("environment"));
    }

    use sauron_query::{
        dimensions_for, parse, resolve, Dimension, MatchOp, Resource, Store, ValueType,
    };

    struct StubLower;
    impl ResourceLower for StubLower {
        type Table = error_events::table;
        fn leaf(
            &self,
            p: &sauron_query::ResolvedPredicate,
            _ctx: &PrepCtx,
            negate: bool,
        ) -> Result<Frag<Self::Table>, PlanError> {
            // Encode the negate flag in the emitted SQL so the tests can see it.
            let marker = if negate { "NEG" } else { "POS" };
            Ok(Box::new(
                error_events::level
                    .eq(format!("{marker}:{}", p.dim.name))
                    .nullable(),
            ))
        }
        fn text(&self, term: &str) -> Frag<Self::Table> {
            Box::new(error_events::message.eq(term.to_string()).nullable())
        }
    }

    fn stub_sql(q: &str) -> String {
        let node = resolve(&parse(q).unwrap(), Resource::Occurrences).unwrap();
        let ctx = PrepCtx {
            environments: HashMap::new(),
            now: Utc::now(),
        };
        let frag = lower(&node, &StubLower, &ctx).unwrap();
        let query = error_events::table.into_boxed().filter(frag);
        diesel::debug_query::<diesel::pg::Pg, _>(&query).to_string()
    }

    #[test]
    fn and_becomes_a_conjunction() {
        let sql = stub_sql("level:error release:1.0");
        assert!(sql.contains("AND"), "{sql}");
        assert!(sql.contains("POS:level"), "{sql}");
        assert!(sql.contains("POS:release"), "{sql}");
    }

    #[test]
    fn or_becomes_a_disjunction() {
        let sql = stub_sql("level:error OR release:1.0");
        assert!(sql.contains("OR"), "{sql}");
    }

    #[test]
    fn negation_reaches_the_leaf_rather_than_wrapping_the_tree() {
        // The leaf must be told to negate itself — a NOT around a compound is
        // NULL-unsafe and would silently drop rows where the column IS NULL.
        let sql = stub_sql("!level:error");
        assert!(sql.contains("NEG:level"), "{sql}");
        assert!(
            !sql.contains(" NOT "),
            "negation must not wrap the tree: {sql}"
        );
    }

    #[test]
    fn de_morgan_distributes_over_and() {
        // !(a AND b)  ==  (!a OR !b)
        let sql = stub_sql("!(level:error release:1.0)");
        assert!(sql.contains("NEG:level"), "{sql}");
        assert!(sql.contains("NEG:release"), "{sql}");
        assert!(sql.contains("OR"), "{sql}");
    }

    #[test]
    fn de_morgan_distributes_over_or() {
        // !(a OR b)  ==  (!a AND !b)
        let sql = stub_sql("!(level:error OR release:1.0)");
        assert!(sql.contains("NEG:level"), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }

    #[test]
    fn double_negation_cancels() {
        let sql = stub_sql("!!level:error");
        assert!(sql.contains("POS:level"), "{sql}");
        assert!(!sql.contains("NEG"), "{sql}");
    }

    #[test]
    fn free_text_reaches_the_text_hook() {
        let sql = stub_sql("boom");
        assert!(sql.contains(r#""error_events"."message""#), "{sql}");
    }

    #[test]
    fn an_empty_query_lowers_to_a_true_fragment() {
        // `parse("")` is And([]) — it must not error, and must not filter anything out.
        let sql = stub_sql("");
        assert!(sql.contains("TRUE") || sql.contains("$1"), "{sql}");
    }

    // -- `json_path_segments` / `nest_json_object` --------------------------

    #[test]
    fn dotted_path_already_carries_the_prefix() {
        // `resolve_field` folds `prefix` into `path` itself when the field had a
        // dotted remainder, so it must not be prepended a second time here.
        assert_eq!(
            json_path_segments("os", Some("os.name")),
            Some(vec!["os".to_string(), "name".to_string()])
        );
    }

    #[test]
    fn empty_prefix_path_is_not_prefixed() {
        // `user.email` -> prefix "" (the column IS the user object), path "email".
        assert_eq!(
            json_path_segments("", Some("email")),
            Some(vec!["email".to_string()])
        );
    }

    #[test]
    fn bare_root_falls_back_to_the_prefix_alone() {
        // `has:os` (no dotted remainder) resolves with `path = None`; the
        // dimension's own prefix becomes the sole segment.
        assert_eq!(json_path_segments("os", None), Some(vec!["os".to_string()]));
    }

    #[test]
    fn bare_root_with_no_prefix_has_no_segments() {
        // `extra`/`contexts`/`user`/`sdk` bare (no dot, no prefix) has nothing to
        // select — the caller must reject this rather than build a vacuous filter.
        assert_eq!(json_path_segments("", None), None);
    }

    #[test]
    fn nest_json_object_builds_from_the_innermost_out() {
        let obj = nest_json_object(
            &["os".to_string(), "name".to_string()],
            serde_json::Value::String("Linux".to_string()),
        );
        assert_eq!(obj, serde_json::json!({"os": {"name": "Linux"}}));
    }

    #[test]
    fn nest_json_object_with_one_segment_does_not_double_nest() {
        let obj = nest_json_object(
            &["email".to_string()],
            serde_json::Value::String("a@b.com".to_string()),
        );
        assert_eq!(obj, serde_json::json!({"email": "a@b.com"}));
    }

    // ===========================================================================
    // Task 7 — coverage: every dimension the catalog advertises for a resource
    // must lower, or return `NotYetSupported` — never panic, never any other
    // error. Without this, adding a catalog entry in a later slice looks like
    // it works and then 500s at runtime (see `catalog.rs`'s module doc).
    // ===========================================================================

    use super::events::EventsLower;
    use super::issues::IssuesLower;
    use super::occurrences::OccurrencesLower;
    use super::sessions::SessionsLower;

    fn ctx() -> PrepCtx {
        PrepCtx {
            environments: HashMap::new(),
            now: Utc::now(),
        }
    }

    /// The field text a query needs to reach `dim` at all. A `Store::JsonRoot`
    /// dimension needs a dotted remainder to produce a `path` at all — a bare
    /// `extra:x` has no segment to nest under and is a deliberate `BadValue`
    /// elsewhere (see `occurrences.rs`'s
    /// `a_bare_json_root_with_no_dotted_remainder_and_no_prefix_is_rejected`
    /// test) — so every JSON root here is addressed as `{name}.k`. That is
    /// valid whether the root's own `prefix` is empty (`extra`, `user`, `sdk`,
    /// `contexts`, `properties`, `stack`) or not (`os`, `browser`, `device`,
    /// `app`): `resolve_field`'s dotted-path branch folds a non-empty prefix
    /// into the path and leaves an empty prefix's path as the bare remainder,
    /// either way producing at least one JSONB path segment, never the `None`
    /// that a bare root would.
    fn field_text(dim: &Dimension) -> String {
        match dim.store {
            Store::JsonRoot { .. } => format!("{}.k", dim.name),
            _ => dim.name.to_string(),
        }
    }

    /// A bare value literal — no operator syntax, no brackets — chosen from
    /// `dim.ty` per the task brief: `Enum` uses its first option, `Int` uses
    /// `1`, `Duration` uses `1s`, `Timestamp` uses `-1d` (relative, so it never
    /// depends on wall-clock skew), `Bool` uses `true`, `Str` uses a plain word
    /// with no operator metacharacters in it.
    fn sample_value(dim: &Dimension) -> String {
        match dim.ty {
            ValueType::Str => "sample".to_string(),
            ValueType::Enum(opts) => opts[0].to_string(),
            ValueType::Int => "1".to_string(),
            ValueType::Bool => "true".to_string(),
            ValueType::Duration => "1s".to_string(),
            ValueType::Timestamp => "-1d".to_string(),
        }
    }

    /// Synthesize a syntactically valid query for one `(dimension, operator)`
    /// pair. `Ne` is never produced by the parser — `!field:value` resolves to
    /// `Not(Eq)`, not a literal `MatchOp::Ne` (see `ast::MatchOp::Ne`'s own doc
    /// comment) — so it is driven through `!` instead: the resulting tree still
    /// reaches the same leaf with `negate = true`, exactly what a hand-typed
    /// `!field:value` query would produce.
    fn sample_query_for(dim: &Dimension, op: MatchOp) -> String {
        let field = field_text(dim);
        let value = sample_value(dim);
        match op {
            MatchOp::Eq => format!("{field}:{value}"),
            MatchOp::Ne => format!("!{field}:{value}"),
            MatchOp::Gt => format!("{field}:>{value}"),
            MatchOp::Gte => format!("{field}:>={value}"),
            MatchOp::Lt => format!("{field}:<{value}"),
            MatchOp::Lte => format!("{field}:<={value}"),
            MatchOp::In => format!("{field}:[{value}]"),
            MatchOp::Has => format!("has:{field}"),
            MatchOp::Like => format!("{field}:{value}*"),
            MatchOp::Contains => format!("{field}:~{value}"),
        }
    }

    /// Run every `(dimension, operator)` pair the catalog declares for
    /// `resource` through `resolve` then `lower`. `resolve` must never fail —
    /// `sample_query_for` only ever builds queries the grammar and the catalog
    /// accept — and `lower` must return either a fragment or an explicit
    /// `PlanError::NotYetSupported`; any other outcome (including a panic) is a
    /// `(Store, MatchOp)` pair the leaf mappers missed.
    fn assert_full_coverage<L: ResourceLower>(resource: Resource, sample: &L) {
        for dim in dimensions_for(resource) {
            for op in dim.ops {
                let q = sample_query_for(dim, *op);
                let node = match resolve(&parse(&q).unwrap(), resource) {
                    Ok(n) => n,
                    Err(e) => panic!("`{q}` failed to resolve for {resource:?}: {e}"),
                };
                match lower(&node, sample, &ctx()) {
                    Ok(_) => {}
                    Err(PlanError::NotYetSupported { .. }) => {}
                    Err(e) => panic!("`{q}` ({resource:?}) failed to lower: {e}"),
                }
            }
        }
    }

    /// Every dimension the catalog advertises for a resource must lower, or
    /// return `NotYetSupported` — never panic, never silently produce wrong
    /// SQL. Without this, adding a catalog entry in a later slice looks like it
    /// works and then 500s at runtime.
    #[test]
    fn every_declared_dimension_lowers_or_is_explicitly_deferred() {
        assert_eq!(dimensions_for(Resource::Issues).count(), 13);
        assert_eq!(dimensions_for(Resource::Occurrences).count(), 20);
        assert_eq!(dimensions_for(Resource::Events).count(), 9);
        assert_eq!(dimensions_for(Resource::Sessions).count(), 10);

        let fixed = Uuid::nil();
        assert_full_coverage(
            Resource::Issues,
            &IssuesLower {
                app_id: fixed,
                text_reach: crate::repo::TextSearchReach::IncludingBody,
                env: &crate::scope::EnvFilter::All,
                since: Utc::now() - chrono::Duration::days(30),
            },
        );
        assert_full_coverage(
            Resource::Occurrences,
            &OccurrencesLower {
                app_id: fixed,
                issue_id: fixed,
                text_reach: crate::repo::TextSearchReach::IncludingBody,
            },
        );
        assert_full_coverage(Resource::Events, &EventsLower { app_id: fixed });
        assert_full_coverage(Resource::Sessions, &SessionsLower { app_id: fixed });
    }
}
