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
use diesel::sql_types::{Array, Bool, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TimeSpec, TypedValue};

use crate::query_plan::{Frag, PlanError, PrepCtx, ResourceLower};
use crate::repo::{like_contains, TextSearchReach};
use crate::schema::issues;
use crate::scope::EnvFilter;

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
        // `checked_sub_months` clamps to the end of a shorter month, so
        // "1 month before 31 March" is 28/29 February rather than overflowing
        // into March. Saturating at the epoch floor on the arithmetic's only
        // failure mode (a count large enough to leave the representable range)
        // keeps an absurd `>=99999month` as "everything" instead of a 500.
        TypedValue::Time(TimeSpec::RelativeMonths(months)) => {
            let n = u32::try_from(*months).unwrap_or(u32::MAX);
            Ok(ctx
                .now
                .checked_sub_months(chrono::Months::new(n))
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC))
        }
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

// ===========================================================================
// The environment predicate inside a correlated subquery.
// ===========================================================================

/// Append the environment predicate to a correlated `EXISTS (SELECT 1 FROM
/// error_events e …` and close its paren.
///
/// **This is a second access boundary, not a repeat of the outer one.** The
/// outer `issue_env_membership` decides which ISSUES are visible; this decides
/// which of an issue's OCCURRENCES a predicate may interrogate. Without it, a
/// member scoped to `staging` — who legitimately sees an issue because it has a
/// staging occurrence — could ask `?q=`, `tag.k:v` or `workflow:~pre` about
/// that issue's PRODUCTION events and read the answer off the row count. That
/// is the same oracle `TextSearchReach` closes for `event:read`, re-pointed at
/// the environment boundary. The pre-planner raw-SQL branch bound the env
/// fragment into every one of these subqueries (`te`/`we`/`qe` aliases in
/// `repo::list_issues_with_reach`); this restores that.
///
/// A macro and not a function because diesel's `.sql()`/`.bind()` chain has no
/// shared trait to be generic over — each arm produces a different concrete
/// builder type, unified only by boxing into `Frag`. `$q` is substituted into
/// every arm but only the matching one ever runs, so it is evaluated once.
///
/// `$q` must already have closed any inner grouping paren: the environment
/// term is ANDed at the subquery's top level, after whatever predicate the
/// caller built.
macro_rules! exists_close_env {
    ($q:expr, $env:expr) => {
        match $env {
            EnvFilter::All => Box::new($q.sql(")")) as Frag<issues::table>,
            EnvFilter::One(id) => Box::new(
                $q.sql(" AND e.environment_id = ")
                    .bind::<SqlUuid, _>(*id)
                    .sql(")"),
            ) as Frag<issues::table>,
            EnvFilter::Subset(ids) => Box::new(
                $q.sql(" AND e.environment_id = ANY(")
                    .bind::<Array<SqlUuid>, _>(ids.to_vec())
                    .sql("))"),
            ) as Frag<issues::table>,
            // A literal predicate, so it consumes no bind — the same asymmetry
            // `EnvFilter::sql_fragment`'s doc warns about for the raw-SQL path.
            EnvFilter::Unattributed => {
                Box::new($q.sql(" AND e.environment_id IS NULL)")) as Frag<issues::table>
            }
        }
    };
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
///
/// The other three fields ARE read, and all three are here for the same
/// reason: each one is a bound that MUST land inside the correlated
/// subqueries, where no caller can apply it afterwards.
///
/// - `text_reach` — [`ResourceLower::text`]. Not a tuning knob: see
///   [`TextSearchReach`] for why the searchable set must equal the readable
///   set. Before S2c Task 4 this lowerer always emitted the payload scan,
///   which would have handed a bare `issue:read` caller the exact oracle
///   `list_issues_with_reach` refuses them.
/// - `env` — the environment scope, ANDed into every `EXISTS` over
///   `error_events` (tag, workflow, payload). The outer membership filter
///   cannot substitute for it: it decides which ISSUES are visible, this
///   decides which OCCURRENCES a predicate may interrogate. See
///   `exists_close_env!`.
/// - `since` — the time bound on the payload scan. Casting jsonb to text is
///   unindexable, so without it a non-matching issue forces a scan of that
///   issue's entire event history, for every issue in the app.
///
/// None has a default, so none can be forgotten at a call site: adding a
/// caller is a compile error until all four are supplied.
pub struct IssuesLower<'a> {
    pub app_id: Uuid,
    pub text_reach: TextSearchReach,
    pub env: &'a EnvFilter,
    pub since: DateTime<Utc>,
}

impl ResourceLower for IssuesLower<'_> {
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
            Store::Tag => tag_leaf(p, negate, self.env),
            // Not an `issues` column at all — see `workflow_leaf`.
            Store::Column("workflow") => workflow_leaf(p, negate, self.env),
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
        // ILIKE, plus — for a caller whose reach includes the body — a
        // correlated payload scan over the child events' contexts/extra/tags,
        // bounded by `since` and by the environment scope.
        //
        // An earlier revision of this comment claimed the time-bounding "was
        // dead code (B3)". That is true of the `MAX_PAYLOAD_SEARCH_DAYS`
        // FALLBACK, which never fired because every route already passed a
        // `since` — but the `since` BIND itself was live, and dropping it is
        // not a no-op: `contexts::text ILIKE …` is unindexable, so an issue
        // with no match forces a full scan of its entire event history, for
        // every issue in the app. The outer 30-day clamp does not help — it
        // tightens `issues.last_seen`, which says nothing about how far back
        // that issue's occurrences go.
        let pattern = like_contains(term);
        let title: Frag<issues::table> = Box::new(issues::title.ilike(pattern.clone()).nullable());
        let type_match: Frag<issues::table> =
            Box::new(issues::type_.ilike(pattern.clone()).nullable());
        let culprit: Frag<issues::table> =
            Box::new(issues::culprit.ilike(pattern.clone()).nullable());
        let shell: Frag<issues::table> = Box::new(title.or(type_match).or(culprit));
        // The withheld half. Emitted only under `IncludingBody`, exactly as
        // `list_issues_with_reach` does — a predicate over `contexts`/`extra`/
        // `tags` is a match/no-match oracle over columns `strip_event_body`
        // nulls for a caller holding `issue:read` alone.
        if !self.text_reach.includes_body() {
            return shell;
        }
        let payload: Frag<issues::table> = exists_close_env!(
            sql::<Nullable<Bool>>(
                "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
                 AND e.app_id = issues.app_id AND e.occurred_at >= ",
            )
            .bind::<Timestamptz, _>(self.since)
            .sql(" AND (e.contexts::text ILIKE ")
            .bind::<Text, _>(pattern.clone())
            .sql(" OR e.extra::text ILIKE ")
            .bind::<Text, _>(pattern.clone())
            .sql(" OR e.tags::text ILIKE ")
            .bind::<Text, _>(pattern)
            .sql(")"),
            self.env
        );
        Box::new(shell.or(payload))
    }
}

// ===========================================================================
// Tags — `issues` has no `tags` column, so every op becomes a correlated
// `EXISTS` into `error_events`, re-asserting the tenant key AND the
// environment scope inside the subquery. Only `&'static str` SQL and JSONB
// *binds* are used; the caller supplied key and value never reach SQL text.
// ===========================================================================

fn tag_leaf(
    p: &ResolvedPredicate,
    negate: bool,
    env: &EnvFilter,
) -> Result<Frag<issues::table>, PlanError> {
    // No key named (`tag:value`, `@tag=value`) → match across EVERY tag key.
    let Some(key) = p.path.as_deref() else {
        return tag_any_leaf(p, negate, env);
    };
    let positive: Frag<issues::table> = match p.op {
        MatchOp::Eq => {
            let value = as_str(&p.value, key)?;
            tag_contains(key, value, env)
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, key)?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: key.to_string(),
            })?;
            let mut acc: Frag<issues::table> = tag_contains(key, &first, env);
            for v in values {
                acc = Box::new(acc.or(tag_contains(key, &v, env)));
            }
            acc
        }
        MatchOp::Has => tag_has(key, env),
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, key)?;
            tag_ilike(key, pattern, env)
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

// ===========================================================================
// Workflow — `issues` has no workflow column either, so this is the same
// correlated-EXISTS shape as `tag`, over `error_events.workflow_name`.
// ===========================================================================

/// Byte-for-byte the predicate `repo::list_issues_with_reach`'s `("workflow",
/// …)` filter arms build, so a bridged `filter=workflow:…` bookmark selects
/// the same rows through the planner as it did before.
///
/// Two details are load-bearing and both are copied deliberately:
///
/// - `e.workflow_id IS NOT NULL` is redundant semantically (the pipeline
///   stamps id and name together) but is what lets Postgres consider the
///   PARTIAL `error_events_app_workflow_idx` (`WHERE workflow_id IS NOT
///   NULL`) — a partial index is only usable when the query's WHERE implies
///   the index predicate, and `workflow_name = $1` does not.
/// - negation is `NOT EXISTS`, not `EXISTS(… <> …)`. An issue whose
///   occurrences carry no workflow at all DOES match `!workflow:checkout` —
///   "not part of workflow X" is true of a row that is part of no workflow.
///   `EXISTS` is never SQL NULL, so wrapping in `NOT` is NULL-safe here in a
///   way a plain column comparison would not be.
fn workflow_leaf(
    p: &ResolvedPredicate,
    negate: bool,
    env: &EnvFilter,
) -> Result<Frag<issues::table>, PlanError> {
    let field = p.dim.name;
    let (pattern, comparison) = match p.op {
        MatchOp::Eq => (
            as_str(&p.value, field)?.to_string(),
            " AND e.workflow_name = ",
        ),
        MatchOp::Like | MatchOp::Contains => (
            as_pattern(&p.value, field)?.to_string(),
            " AND e.workflow_name ILIKE ",
        ),
        // `Ne` never reaches here (`!workflow:x` arrives as `Eq` + negate),
        // and the catalog grants this dimension no ordering/list operator —
        // both arms exist for exhaustiveness.
        MatchOp::Ne => {
            let v = as_str(&p.value, field)?.to_string();
            return workflow_exists(v, " AND e.workflow_name = ", !negate, env);
        }
        MatchOp::In | MatchOp::Has | MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            return Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            })
        }
    };
    workflow_exists(pattern, comparison, negate, env)
}

fn workflow_exists(
    value: String,
    comparison: &'static str,
    negate: bool,
    env: &EnvFilter,
) -> Result<Frag<issues::table>, PlanError> {
    let positive: Frag<issues::table> = exists_close_env!(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.workflow_id IS NOT NULL",
        )
        .sql(comparison)
        .bind::<Text, _>(value),
        env
    );
    Ok(if negate {
        Box::new(diesel::dsl::not(positive))
    } else {
        positive
    })
}

/// `tag:<value>` with no key — the same predicate applied across every key of
/// the `tags` object, via `jsonb_each_text`. Splitting this out rather than
/// threading an `Option<&str>` through `tag_contains`/`tag_ilike`/`tag_has`
/// keeps the keyed path (which can use the `@>` containment index) untouched.
fn tag_any_leaf(
    p: &ResolvedPredicate,
    negate: bool,
    env: &EnvFilter,
) -> Result<Frag<issues::table>, PlanError> {
    let positive: Frag<issues::table> = match p.op {
        MatchOp::Eq => {
            let value = as_str(&p.value, "tag")?;
            tag_any_cmp(" = ", value, env)
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, "tag")?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: "tag".to_string(),
            })?;
            let mut acc: Frag<issues::table> = tag_any_cmp(" = ", &first, env);
            for v in values {
                acc = Box::new(acc.or(tag_any_cmp(" = ", &v, env)));
            }
            acc
        }
        // `has:tag` — the row carries at least one tag at all.
        MatchOp::Has => exists_close_env!(
            sql::<Nullable<Bool>>(
                "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
                 AND e.app_id = issues.app_id AND e.tags <> '{}'::jsonb",
            ),
            env
        ),
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, "tag")?;
            tag_any_cmp(" ILIKE ", pattern, env)
        }
        MatchOp::Ne | MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            return Err(PlanError::UnsupportedOnResource {
                field: "tag".to_string(),
            })
        }
    };
    Ok(if negate {
        Box::new(diesel::dsl::not(positive))
    } else {
        positive
    })
}

/// One `jsonb_each_text` scan comparing every tag VALUE with `op` (` = ` or
/// ` ILIKE `). The operator is a fixed literal chosen by the caller, never
/// user input, so it cannot carry injection.
fn tag_any_cmp(op: &'static str, value: &str, env: &EnvFilter) -> Frag<issues::table> {
    exists_close_env!(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND EXISTS (SELECT 1 FROM \
             jsonb_each_text(e.tags) kv WHERE kv.value",
        )
        .sql(op)
        .bind::<Text, _>(value.to_string())
        .sql(")"),
        env
    )
}

fn tag_contains(key: &str, value: &str, env: &EnvFilter) -> Frag<issues::table> {
    exists_close_env!(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags @> ",
        )
        .bind::<Jsonb, _>(tag_bind_object(key, value)),
        env
    )
}

fn tag_has(key: &str, env: &EnvFilter) -> Frag<issues::table> {
    exists_close_env!(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags ? ",
        )
        .bind::<Text, _>(key.to_string()),
        env
    )
}

fn tag_ilike(key: &str, pattern: &str, env: &EnvFilter) -> Frag<issues::table> {
    exists_close_env!(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
             AND e.app_id = issues.app_id AND e.tags ->> ",
        )
        .bind::<Text, _>(key.to_string())
        .sql(" ILIKE ")
        .bind::<Text, _>(pattern.to_string()),
        env
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
            text_reach: TextSearchReach::IncludingBody,
            env: &EnvFilter::All,
            since: Utc::now() - chrono::Duration::days(30),
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
        let sql = lower_issues_sql("tag.checkout_step:payment");
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
            text_reach: TextSearchReach::IncludingBody,
            env: &EnvFilter::All,
            since: Utc::now() - chrono::Duration::days(30),
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
        let sql = lower_issues_sql("has:tag.checkout_step");
        assert!(sql.contains("e.tags ? "), "{sql}");
    }

    #[test]
    fn tag_like_uses_the_arrow_operator_and_ilike() {
        let sql = lower_issues_sql("tag.checkout_step:~payment");
        assert!(sql.contains("e.tags ->>"), "{sql}");
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    #[test]
    fn tag_in_ors_the_per_value_exists_clauses() {
        let sql = lower_issues_sql("tag.checkout_step:[payment,shipping]");
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
        let sql = lower_issues_sql("!tag.checkout_step:payment");
        assert!(sql.contains("NOT (EXISTS"), "{sql}");
    }

    #[test]
    fn tag_never_leaks_the_key_or_value_into_sql_text() {
        // `debug_query`'s `Display` appends a human-readable `-- binds: [...]`
        // trailer for debugging convenience; that is not part of the SQL sent
        // to Postgres (the key/value travel as one `$1` parameter), so the
        // assertion checks the query text only, before that trailer.
        let sql = lower_issues_sql("tag.checkout_step:payment");
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

    // -- Environment scope inside the correlated subqueries -----------------
    //
    // The second access boundary. `issue_env_membership` (repo.rs) decides
    // which ISSUES are visible; these decide which OCCURRENCES a predicate may
    // interrogate. A member scoped to one environment legitimately sees an
    // issue that has an occurrence there — and without the fragment below,
    // `?q=`/`tag`/`workflow` would then answer against that issue's events in
    // environments the member holds no grant on.

    fn lower_issues_sql_env(q: &str, env: &EnvFilter) -> String {
        let node = resolve(&parse(q).unwrap(), Resource::Issues).unwrap();
        let l = IssuesLower {
            app_id: Uuid::nil(),
            text_reach: TextSearchReach::IncludingBody,
            env,
            since: Utc::now() - chrono::Duration::days(30),
        };
        let frag = lower(&node, &l, &ctx()).unwrap();
        let query = issues::table.into_boxed().filter(frag);
        debug_query::<Pg, _>(&query).to_string()
    }

    /// Every correlated subquery, under every non-`All` filter shape. Written
    /// as a full cross-product rather than one representative case because the
    /// failure mode is per-leaf: the earlier revision of this file bound the
    /// tenant key in all five and the environment in none, and any one of them
    /// left un-fragmented is a live oracle on its own.
    #[test]
    fn every_correlated_subquery_carries_the_environment_scope() {
        let one = EnvFilter::One(Uuid::from_u128(7));
        let subset = EnvFilter::Subset(vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
        let unattributed = EnvFilter::Unattributed;
        // free text (payload scan), tag @>, tag ?, tag ->> ILIKE, workflow
        let queries = [
            "boom",
            "tag.checkout_step:payment",
            "has:tag.checkout_step",
            "tag.checkout_step:~payment",
            "workflow:checkout",
        ];
        for q in queries {
            let sql = lower_issues_sql_env(q, &one);
            assert!(
                sql.contains("e.environment_id = $"),
                "`{q}` under One must bind an environment equality: {sql}"
            );
            let sql = lower_issues_sql_env(q, &subset);
            assert!(
                sql.contains("e.environment_id = ANY("),
                "`{q}` under Subset must bind an environment array: {sql}"
            );
            let sql = lower_issues_sql_env(q, &unattributed);
            assert!(
                sql.contains("e.environment_id IS NULL"),
                "`{q}` under Unattributed must emit IS NULL: {sql}"
            );
            // `All` is the one shape that must add nothing at all — an
            // accidental predicate there would silently narrow every
            // unscoped request.
            let sql = lower_issues_sql_env(q, &EnvFilter::All);
            assert!(
                !sql.contains("environment_id"),
                "`{q}` under All must not mention environment_id: {sql}"
            );
        }
    }

    #[test]
    fn a_negated_tag_keeps_the_environment_scope_inside_the_not() {
        // `NOT EXISTS (… env …)`, never `NOT EXISTS (…) AND env`: the scope
        // belongs to the subquery it bounds, and hoisting it out inverts with
        // the negation.
        let env = EnvFilter::One(Uuid::from_u128(7));
        let sql = lower_issues_sql_env("!tag.checkout_step:payment", &env);
        assert!(sql.contains("NOT (EXISTS"), "{sql}");
        let subquery = sql.split("NOT (EXISTS").nth(1).unwrap();
        assert!(
            subquery.contains("e.environment_id = $"),
            "the env predicate must be INSIDE the negated EXISTS: {sql}"
        );
    }

    #[test]
    fn the_payload_scan_is_bounded_by_since() {
        // Not cosmetic: `contexts::text ILIKE` is unindexable, so an issue with
        // no match scans its entire event history — for every issue in the app.
        // The outer 30-day clamp bounds `issues.last_seen`, which says nothing
        // about how far back an issue's occurrences reach.
        let sql = lower_issues_sql_env("boom", &EnvFilter::All);
        assert!(
            sql.contains("e.occurred_at >= "),
            "the payload EXISTS must carry a time bound: {sql}"
        );
    }

    // -- Composition sanity: predicates and free text combine via AND --------

    #[test]
    fn a_predicate_and_free_text_combine() {
        let sql = lower_issues_sql("is:resolved boom");
        assert!(sql.contains(r#""issues"."status" = $1"#), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }
}
