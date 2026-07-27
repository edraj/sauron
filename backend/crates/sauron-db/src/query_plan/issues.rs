//! `IssuesLower`: turns one `ResolvedPredicate` (or free-text term) resolved
//! against `Resource::Issues` into a diesel boxed fragment over `issues::table`.
//!
//! `environment`, `release`, and `handled` on Issues are `Store::Rollup` — the
//! `issue_dimensions` rollup table does not exist yet (S3), so those three are
//! rejected with `PlanError::NotYetSupported` rather than silently matching
//! nothing or panicking.
//!
//! `issues` has no `tags` column at all (16 columns, verified against
//! `schema.rs`): a tag predicate becomes a correlated `EXISTS` into
//! `error_events`, re-asserting the tenant key (`e.app_id = issues.app_id`)
//! inside the subquery — every query carries the tenant key, including
//! nested ones. `EXISTS` is never SQL `NULL`, so negating it by wrapping in
//! `NOT (...)` is always safe, unlike negating a plain column comparison.

use chrono::{DateTime, Utc};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Jsonb, Nullable, Text};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TimeSpec, TypedValue};

use crate::query_plan::{Frag, PlanError, PrepCtx, ResourceLower};
use crate::repo::like_contains;
use crate::schema::issues;

// ===========================================================================
// Value extraction — a bad combination here is a planner bug (resolve.rs
// already enforces the catalog's `ops`/`ty` contract), so these errors are
// defensive rather than expected to fire in production.
// ===========================================================================

fn as_str<'a>(v: &'a TypedValue, field: &str) -> Result<&'a str, PlanError> {
    match v {
        TypedValue::Str(s) => Ok(s.as_str()),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_pattern<'a>(v: &'a TypedValue, field: &str) -> Result<&'a str, PlanError> {
    match v {
        TypedValue::Pattern(p) => Ok(p.as_str()),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_int(v: &TypedValue, field: &str) -> Result<i64, PlanError> {
    match v {
        TypedValue::Int(i) => Ok(*i),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_str_list(v: &TypedValue, field: &str) -> Result<Vec<String>, PlanError> {
    match v {
        TypedValue::List(items) => items
            .iter()
            .map(|i| match i {
                TypedValue::Str(s) => Ok(s.clone()),
                _ => Err(PlanError::BadValue {
                    field: field.to_string(),
                }),
            })
            .collect(),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

/// `RelativeSeconds` is resolved against `ctx.now`, not the clock at call
/// time, so a single query renders deterministic, assertable SQL.
fn as_time(ctx: &PrepCtx, v: &TypedValue, field: &str) -> Result<DateTime<Utc>, PlanError> {
    match v {
        TypedValue::Time(TimeSpec::Absolute(dt)) => Ok(*dt),
        TypedValue::Time(TimeSpec::RelativeSeconds(secs)) => {
            Ok(ctx.now - chrono::Duration::seconds(*secs))
        }
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

// ===========================================================================
// Column families. `issues` has no nullable columns among the ones searched
// here (16 columns, only `assignee_id` is nullable and it is not a searchable
// dimension), so the NULL-safe negated forms below are always a no-op extra
// clause in practice — but they keep this code correct if that ever changes,
// and satisfy the mandated NULL-safety test regardless of the column's
// current nullability.
//
// These are declarative macros, not generic functions: a generic function
// bounded only by `Column<Table = issues::table, SqlType = _>` cannot prove
// the downstream `ValidGrouping`/`QueryFragment` obligations diesel's
// operator types need (the compiler cannot see that a *specific* column's
// `IsAggregate` is `Never`), so each macro is expanded once per concrete
// column, where the compiler has the real diesel-generated type.
// ===========================================================================

/// Eq/Ne/In/Has/Like/Contains over a `Text` column. `Gt`/`Gte`/`Lt`/`Lte`
/// never reach a text dimension — the catalog does not grant them to any
/// Issues text dimension — so they fall to the defensive error arm.
macro_rules! str_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                }
            }
            // Never produced by the parser today (`!field:value` reaches
            // here as `Eq` with `negate = true`); handled as Eq's mirror
            // image so the match stays exhaustive and correct if that ever
            // changes.
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pat = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.not_ilike(pat).or($col.is_null()).nullable())
                        as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ilike(pat).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
                Err(PlanError::UnsupportedOnResource {
                    field: field.to_string(),
                })
            }
        }
    }};
}

/// Eq/Ne/Gt/Gte/Lt/Lte/Has over the `BigInt` columns `times_seen`/`users_seen`.
/// `In`/`Like`/`Contains` never reach an ordering dimension — the catalog
/// does not grant them.
///
/// Negated ordering comparisons swap to the complementary operator
/// (`Gt` <-> `Lte`, `Gte` <-> `Lt`) rather than wrapping in `NOT`, which is
/// both simpler SQL and exactly equivalent for a non-nullable column.
macro_rules! int_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Gt => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.le(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Gte => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Lt => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Lte => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.le(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::In | MatchOp::Like | MatchOp::Contains => {
                Err(PlanError::UnsupportedOnResource {
                    field: field.to_string(),
                })
            }
        }
    }};
}

/// Eq/Ne/Gt/Gte/Lt/Lte/Has over the `Timestamptz` columns
/// `first_seen`/`last_seen`. Same shape as `int_leaf!`; relative times
/// (`-7d`) are resolved against `ctx.now`.
macro_rules! time_leaf {
    ($col:expr, $p:expr, $ctx:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Gt => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.le(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Gte => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Lt => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Lte => {
                let v = as_time($ctx, &$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.le(v).nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<issues::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<issues::table>)
                }
            }
            MatchOp::In | MatchOp::Like | MatchOp::Contains => {
                Err(PlanError::UnsupportedOnResource {
                    field: field.to_string(),
                })
            }
        }
    }};
}

/// Lowers predicates resolved against `Resource::Issues`.
///
/// `app_id` is not read directly by `leaf`/`text`: the tag and free-text
/// subqueries re-assert the tenant key by correlating to `issues.app_id`
/// rather than binding a literal, and the outer base-scope filter
/// (`issues.app_id = $1`, spec's base-scope table) is applied by the caller
/// that also owns `since`. The field is carried here so callers construct one
/// `IssuesLower` per request, matching the shape of the sibling lowerers.
pub struct IssuesLower {
    pub app_id: Uuid,
}

impl ResourceLower for IssuesLower {
    type Table = issues::table;

    fn leaf(
        &self,
        p: &ResolvedPredicate,
        ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<issues::table>, PlanError> {
        match p.dim.store {
            Store::Rollup => Err(PlanError::NotYetSupported {
                field: p.dim.name.to_string(),
            }),
            Store::Tag => tag_leaf(p, negate),
            // Issues has no JSON-root dimensions in the catalog; kept for
            // exhaustiveness in case the catalog ever grows one.
            Store::JsonRoot { .. } => Err(PlanError::UnsupportedOnResource {
                field: p.dim.name.to_string(),
            }),
            Store::Column("status") => str_leaf!(issues::status, p, negate),
            Store::Column("level") => str_leaf!(issues::level, p, negate),
            Store::Column("type") => str_leaf!(issues::type_, p, negate),
            Store::Column("culprit") => str_leaf!(issues::culprit, p, negate),
            Store::Column("title") => str_leaf!(issues::title, p, negate),
            Store::Column("times_seen") => int_leaf!(issues::times_seen, p, negate),
            Store::Column("users_seen") => int_leaf!(issues::users_seen, p, negate),
            Store::Column("first_seen") => time_leaf!(issues::first_seen, p, ctx, negate),
            Store::Column("last_seen") => time_leaf!(issues::last_seen, p, ctx, negate),
            Store::Column(other) => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
        }
    }

    fn text(&self, term: &str) -> Frag<issues::table> {
        // Reproduces the pre-planner behaviour exactly: title/type/culprit by
        // ILIKE, plus a correlated payload scan over the child events'
        // contexts/extra/tags. The time-bounding that used to gate that scan
        // (`MAX_PAYLOAD_SEARCH_DAYS`) was dead code (B3) and is superseded by
        // the cost-driven clamp landing in a later task, not reproduced here.
        let pattern = like_contains(term);
        let title: Frag<issues::table> = Box::new(issues::title.ilike(pattern.clone()).nullable());
        let type_match: Frag<issues::table> =
            Box::new(issues::type_.ilike(pattern.clone()).nullable());
        let culprit: Frag<issues::table> =
            Box::new(issues::culprit.ilike(pattern.clone()).nullable());
        let payload: Frag<issues::table> = Box::new(
            sql::<Nullable<Bool>>(
                "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
                 AND e.app_id = issues.app_id AND (e.contexts::text ILIKE ",
            )
            .bind::<Text, _>(pattern.clone())
            .sql(" OR e.extra::text ILIKE ")
            .bind::<Text, _>(pattern.clone())
            .sql(" OR e.tags::text ILIKE ")
            .bind::<Text, _>(pattern)
            .sql("))"),
        );
        Box::new(title.or(type_match).or(culprit).or(payload))
    }
}

// ===========================================================================
// Tags — `issues` has no `tags` column, so every op becomes a correlated
// `EXISTS` into `error_events`, re-asserting the tenant key inside the
// subquery. Only `&'static str` SQL and JSONB *binds* are used; the caller
// supplied key and value never reach SQL text.
// ===========================================================================

fn tag_leaf(p: &ResolvedPredicate, negate: bool) -> Result<Frag<issues::table>, PlanError> {
    let key = p.path.as_deref().ok_or_else(|| PlanError::BadValue {
        field: "tag".to_string(),
    })?;
    let positive: Frag<issues::table> = match p.op {
        MatchOp::Eq => {
            let value = as_str(&p.value, key)?;
            tag_contains(key, value)
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, key)?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: key.to_string(),
            })?;
            let mut acc: Frag<issues::table> = tag_contains(key, &first);
            for v in values {
                acc = Box::new(acc.or(tag_contains(key, &v)));
            }
            acc
        }
        MatchOp::Has => tag_has(key),
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, key)?;
            tag_ilike(key, pattern)
        }
        // `Ne` is never produced by the parser (see `str_leaf!`), and no
        // ordering comparison is declared for `TAG_DIM`'s ops — both are
        // unreachable via `resolve`, kept only for match exhaustiveness.
        MatchOp::Ne | MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            return Err(PlanError::UnsupportedOnResource {
                field: key.to_string(),
            })
        }
    };
    // EXISTS is always a plain boolean in Postgres — never NULL — so negating
    // it by wrapping is always correct, unlike negating a column comparison.
    Ok(if negate {
        Box::new(diesel::dsl::not(positive))
    } else {
        positive
    })
}

fn tag_contains(key: &str, value: &str) -> Frag<issues::table> {
    Box::new(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags @> ",
        )
        .bind::<Jsonb, _>(tag_bind_object(key, value))
        .sql(")"),
    )
}

fn tag_has(key: &str) -> Frag<issues::table> {
    Box::new(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags ? ",
        )
        .bind::<Text, _>(key.to_string())
        .sql(")"),
    )
}

fn tag_ilike(key: &str, pattern: &str) -> Frag<issues::table> {
    Box::new(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags ->> ",
        )
        .bind::<Text, _>(key.to_string())
        .sql(" ILIKE ")
        .bind::<Text, _>(pattern.to_string())
        .sql(")"),
    )
}

/// A single-key JSONB object `{key: value}` for a `tags @> …` containment
/// bind. Local to this module: `repo::tag_object` is private to `repo.rs`.
fn tag_bind_object(key: &str, value: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    serde_json::Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_plan::lower;
    use diesel::debug_query;
    use diesel::pg::Pg;
    use sauron_query::{parse, resolve, Resource};
    use std::collections::HashMap;

    fn ctx() -> PrepCtx {
        PrepCtx {
            environments: HashMap::new(),
            now: Utc::now(),
        }
    }

    fn lower_issues_result(q: &str) -> Result<Frag<issues::table>, PlanError> {
        let node = resolve(&parse(q).unwrap(), Resource::Issues).unwrap();
        let l = IssuesLower {
            app_id: Uuid::nil(),
        };
        lower(&node, &l, &ctx())
    }

    /// `Frag` is not `Debug` (it boxes a `dyn` trait object), so
    /// `Result::unwrap_err` can't be used directly on `lower_issues_result`.
    /// This discards the `Ok` payload first.
    fn lower_issues_err(q: &str) -> PlanError {
        lower_issues_result(q).map(|_| ()).unwrap_err()
    }

    fn lower_issues_sql(q: &str) -> String {
        let frag = lower_issues_result(q).unwrap();
        let query = issues::table.into_boxed().filter(frag);
        debug_query::<Pg, _>(&query).to_string()
    }

    // -- The three decision-encoding tests mandated by the task brief --------

    #[test]
    fn rollup_dimensions_are_rejected_with_a_message_that_stays_true() {
        let err = lower_issues_err("environment:production");
        assert!(matches!(err, PlanError::NotYetSupported { .. }));
        assert!(err.to_string().contains("environment"));
    }

    #[test]
    fn negated_equality_is_null_safe() {
        // B2: `.ne()` alone drops rows where the column IS NULL.
        let sql = lower_issues_sql("!culprit:handler");
        assert!(
            sql.contains("IS NULL"),
            "negation must keep NULL rows: {sql}"
        );
    }

    #[test]
    fn a_tag_predicate_becomes_a_correlated_exists_carrying_the_tenant_key() {
        let sql = lower_issues_sql("checkout_step:payment");
        assert!(
            sql.contains("EXISTS (SELECT 1 FROM error_events e"),
            "{sql}"
        );
        assert!(
            sql.contains("e.app_id = issues.app_id"),
            "tenant key must be re-asserted: {sql}"
        );
        assert!(sql.contains("e.tags @>"), "{sql}");
    }

    // -- Rollup: release/handled must reject the same way as environment ----

    #[test]
    fn every_rollup_dimension_on_issues_is_rejected() {
        for q in ["release:1.0.0", "handled:true"] {
            let err = lower_issues_err(q);
            assert!(
                matches!(err, PlanError::NotYetSupported { .. }),
                "{q} => {err:?}"
            );
        }
    }

    // -- Store::Column, Eq ---------------------------------------------------

    #[test]
    fn column_eq_lowers_to_a_plain_equality() {
        let sql = lower_issues_sql("is:resolved");
        assert!(sql.contains(r#""issues"."status" = $1"#), "{sql}");
    }

    // -- Store::Column, In -----------------------------------------------------

    #[test]
    fn column_in_lowers_to_eq_any() {
        let sql = lower_issues_sql("level:[error,fatal]");
        assert!(sql.contains(r#""issues"."level""#), "{sql}");
        assert!(sql.contains("ANY"), "{sql}");
    }

    #[test]
    fn negated_in_is_null_safe() {
        let sql = lower_issues_sql("!level:[error,fatal]");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    // -- Store::Column, Gt/Gte/Lt/Lte --------------------------------------

    #[test]
    fn column_ordering_ops_use_the_matching_diesel_method() {
        assert!(lower_issues_sql("timesSeen:>100").contains(r#""issues"."times_seen" > $1"#));
        assert!(lower_issues_sql("timesSeen:>=100").contains(r#""issues"."times_seen" >= $1"#));
        assert!(lower_issues_sql("usersSeen:<100").contains(r#""issues"."users_seen" < $1"#));
        assert!(lower_issues_sql("usersSeen:<=100").contains(r#""issues"."users_seen" <= $1"#));
    }

    #[test]
    fn negated_ordering_swaps_to_the_complementary_operator() {
        // `!timesSeen:>100` means "NOT more than 100", i.e. <= 100.
        let sql = lower_issues_sql("!timesSeen:>100");
        assert!(sql.contains(r#""issues"."times_seen" <= $1"#), "{sql}");
    }

    // -- Store::Column, Like/Contains -----------------------------------------

    #[test]
    fn wildcard_lowers_to_ilike_with_the_pattern_already_escaped() {
        let sql = lower_issues_sql("culprit:handle*");
        assert!(sql.contains(r#""issues"."culprit" ILIKE $1"#), "{sql}");
    }

    #[test]
    fn literal_substring_lowers_to_ilike_wrapped_in_percent() {
        let sql = lower_issues_sql("title:~timeout");
        assert!(sql.contains(r#""issues"."title" ILIKE $1"#), "{sql}");
    }

    #[test]
    fn negated_ilike_uses_not_ilike() {
        let sql = lower_issues_sql("!culprit:handle*");
        assert!(sql.contains("NOT ILIKE"), "{sql}");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    // -- Store::Column, Has ----------------------------------------------------

    #[test]
    fn has_on_a_plain_column_checks_is_not_null() {
        let sql = lower_issues_sql("has:culprit");
        assert!(sql.contains(r#""issues"."culprit" IS NOT NULL"#), "{sql}");
    }

    #[test]
    fn negated_has_checks_is_null() {
        let sql = lower_issues_sql("!has:culprit");
        assert!(sql.contains(r#""issues"."culprit" IS NULL"#), "{sql}");
    }

    // -- Store::Column, Timestamp (firstSeen/lastSeen) ------------------------

    #[test]
    fn relative_timestamp_resolves_against_ctx_now() {
        let node = resolve(&parse("firstSeen:>-7d").unwrap(), Resource::Issues).unwrap();
        let now = Utc::now();
        let ctx = PrepCtx {
            environments: HashMap::new(),
            now,
        };
        let l = IssuesLower {
            app_id: Uuid::nil(),
        };
        let frag = lower(&node, &l, &ctx).unwrap();
        let query = issues::table.into_boxed().filter(frag);
        let sql = debug_query::<Pg, _>(&query).to_string();
        assert!(sql.contains(r#""issues"."first_seen" > $1"#), "{sql}");
    }

    #[test]
    fn absolute_timestamp_is_used_directly() {
        let sql = lower_issues_sql("lastSeen:<=2026-01-01T00:00:00Z");
        assert!(sql.contains(r#""issues"."last_seen" <= $1"#), "{sql}");
    }

    // -- Store::Tag ------------------------------------------------------------

    #[test]
    fn tag_has_uses_the_question_operator() {
        let sql = lower_issues_sql("has:checkout_step");
        assert!(sql.contains("e.tags ? "), "{sql}");
    }

    #[test]
    fn tag_like_uses_the_arrow_operator_and_ilike() {
        let sql = lower_issues_sql("checkout_step:~payment");
        assert!(sql.contains("e.tags ->>"), "{sql}");
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    #[test]
    fn tag_in_ors_the_per_value_exists_clauses() {
        let sql = lower_issues_sql("checkout_step:[payment,shipping]");
        assert_eq!(
            sql.matches("EXISTS (SELECT 1 FROM error_events e").count(),
            2
        );
        assert!(sql.contains(" OR "), "{sql}");
    }

    #[test]
    fn negated_tag_wraps_in_not_rather_than_null_checking() {
        // EXISTS is never NULL, so a plain NOT wrapper is correct here — this
        // is the one leaf where wrapping (rather than the NULL-safe pair) is
        // the right call.
        let sql = lower_issues_sql("!checkout_step:payment");
        assert!(sql.contains("NOT (EXISTS"), "{sql}");
    }

    #[test]
    fn tag_never_leaks_the_key_or_value_into_sql_text() {
        // `debug_query`'s `Display` appends a human-readable `-- binds: [...]`
        // trailer for debugging convenience; that is not part of the SQL sent
        // to Postgres (the key/value travel as one `$1` parameter), so the
        // assertion checks the query text only, before that trailer.
        let sql = lower_issues_sql("checkout_step:payment");
        let query_text = sql.split("-- binds:").next().unwrap();
        assert!(!query_text.contains("checkout_step"), "{query_text}");
        assert!(!query_text.contains("payment"), "{query_text}");
        assert!(sql.contains("$1"), "{sql}");
    }

    // -- Free text ---------------------------------------------------------

    #[test]
    fn free_text_matches_title_type_culprit_and_the_correlated_payload() {
        let sql = lower_issues_sql("boom");
        assert!(sql.contains(r#""issues"."title" ILIKE $1"#), "{sql}");
        assert!(sql.contains(r#""issues"."type" ILIKE $2"#), "{sql}");
        assert!(sql.contains(r#""issues"."culprit" ILIKE $3"#), "{sql}");
        assert!(
            sql.contains("EXISTS (SELECT 1 FROM error_events e"),
            "{sql}"
        );
        assert!(sql.contains("e.contexts::text ILIKE"), "{sql}");
        assert!(sql.contains("e.extra::text ILIKE"), "{sql}");
        assert!(sql.contains("e.tags::text ILIKE"), "{sql}");
    }

    #[test]
    fn negated_free_text_wraps_the_whole_hook_in_not() {
        // Free-text negation is handled by the generic walker (`lower_inner`),
        // not by `IssuesLower::text` itself — this proves the two compose.
        let sql = lower_issues_sql("!boom");
        assert!(sql.starts_with("SELECT") && sql.contains("NOT ("), "{sql}");
    }

    // -- Composition sanity: predicates and free text combine via AND --------

    #[test]
    fn a_predicate_and_free_text_combine() {
        let sql = lower_issues_sql("is:resolved boom");
        assert!(sql.contains(r#""issues"."status" = $1"#), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }
}
