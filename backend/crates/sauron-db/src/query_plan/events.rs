//! `EventsLower`: turns one `ResolvedPredicate` (or free-text term) resolved
//! against `Resource::Events` into a diesel boxed fragment over
//! `analytics_events::table`.
//!
//! The catalog declares 8 dimensions for Events (`environment`, `release`,
//! `distinctId`, `session`, `contexts`, `extra`, `properties`, `name`) plus the
//! synthetic `tag`, all `Store::Column`/`Store::JsonRoot`/`Store::Tag` — no
//! `Store::Rollup` here, unlike Issues. Every leaf below either lowers or the
//! coverage test in `mod.rs` (Task 7) fails.
//!
//! `contexts`/`extra`/`properties` share the exact `@>`-containment JSONB rule
//! `OccurrencesLower` established (see its module doc): `Eq`/`Ne`/`In` build a
//! nested object in Rust from `json_path_segments`/`nest_json_object` and bind
//! it as ONE `Jsonb` parameter — the caller-supplied path never reaches SQL
//! text. All three roots have an empty `prefix` (the column itself IS the
//! object), so `properties.plan:pro` lowers to `properties @> {"plan":"pro"}`
//! directly, with no root segment to fold in.
//!
//! `tag` is a REAL column here (`analytics_events.tags`), exactly like
//! `OccurrencesLower` and unlike `IssuesLower`'s correlated `EXISTS` — Events
//! has no separate rollup table to correlate into.
//!
//! `environment` is a NAME on the wire and a `Nullable<Uuid>` column
//! (`environment_id`), resolved via `ctx.environments`: an unknown or absent
//! name lowers to `Uuid::nil()`, which can never equal a real
//! `environments.id`, so it matches nothing — never "ignore the filter" (see
//! `resolve_environment`, copied from `OccurrencesLower` rather than shared,
//! matching the house pattern of one macro/fn expansion per concrete table).
//! This also fixes a real bug in the code being replaced
//! (`repo::list_analytics_events`, which kept a single `Option` slot for an
//! `Eq` filter and a second for `Ne`): two `environment:` terms there silently
//! last-won, because only the final match into each slot survived. The walker
//! in `mod.rs` calls `leaf` once per predicate and ANDs the results, so
//! `environment:prod environment:staging` now emits two separate
//! `environment_id` comparisons — correctly zero rows, since no event can
//! carry two environments at once.
//!
//! Base scope for this resource is `analytics_events.app_id = $1 AND
//! analytics_events.name <> '$screen'` — and the second half is NOT a
//! caller-supplied filter the way `since` is. `$screen` is a synthetic event
//! the mobile SDKs emit for screen views (`repo::list_analytics_events`, the
//! code this replaces, excludes it with exactly this predicate); it belongs
//! to the product's Screens section, not the Event Explorer's event stream,
//! so excluding it is part of what "an analytics event" *means* here, not
//! something a query can opt back into. Because `prepare.rs` (the caller that
//! assembles the final boxed query) does not exist yet (Task 6), that
//! invariant is captured now as `EventsLower::base_scope`, a public method
//! alongside `leaf`/`text`, so the exclusion travels with this resource
//! rather than being left for a later task to rediscover.

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Nullable, Text};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TypedValue};

use crate::query_plan::{
    json_path_segments, nest_json_object, Frag, PlanError, PrepCtx, ResourceLower,
};
use crate::repo::like_contains;
use crate::schema::analytics_events;

// ===========================================================================
// Value extraction — see `issues.rs`/`occurrences.rs`'s identical section: a
// bad combination here is a planner bug, since `resolve.rs` already enforces
// the catalog's `ops`/`ty` contract per dimension. Events has no `Int`/`Bool`
// dimension, so only the string-shaped extractors are needed.
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

/// `environment` is a name on the wire and a uuid in the column. A name
/// missing from `ctx.environments` — because it does not exist for this app,
/// or (defensively) because it was never looked up at all — must still lower
/// to a predicate matching nothing, never to "ignore the filter". `Uuid::nil`
/// can never equal a real `environments.id` (a `gen_random_uuid()` primary
/// key), so it is a safe sentinel that also makes negation correct for free:
/// `!environment:ghost` mirrors `!= Uuid::nil() OR IS NULL`, which is every
/// row — exactly "not restricted to a nonexistent environment" should mean.
fn resolve_environment(ctx: &PrepCtx, name: &str) -> Uuid {
    ctx.environments
        .get(name)
        .copied()
        .flatten()
        .unwrap_or(Uuid::nil())
}

// ===========================================================================
// Column families — `macro_rules!`, not generic functions, expanded once per
// concrete column: diesel's `ValidGrouping`/`QueryFragment` obligations are
// not provable over a column bounded only by `Column<Table = _, SqlType = _>`
// (see `issues.rs`'s identical note). `.nullable()` is applied unconditionally
// in every branch regardless of the concrete column's actual nullability —
// diesel's `Nullable<T>: IntoNullable<Nullable = Self>` makes the lift a no-op
// when it was already nullable, so one macro body serves both `name`/
// `distinct_id` (non-nullable `Text`) and `release`/`session_id` (nullable).
// ===========================================================================

/// Eq/Ne/In/Has/Like/Contains over a `Text`/`Nullable<Text>` column.
/// `Gt`/`Gte`/`Lt`/`Lte` never reach a text dimension on this resource — the
/// catalog grants every Events text dimension `OPS_TEXT`, never `OPS_ORD`.
macro_rules! str_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pat = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.not_ilike(pat).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.ilike(pat).nullable()) as Frag<analytics_events::table>)
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

/// Eq/Ne/In/Has over the `Nullable<Uuid>` column `environment_id`, translating
/// the wire NAME to an id via `resolve_environment` first. Same shape as
/// `OccurrencesLower`'s `environment_leaf!` — only the table differs.
macro_rules! environment_leaf {
    ($col:expr, $ctx:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Ne => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                }
            }
            MatchOp::In => {
                let names = as_str_list(&$p.value, field)?;
                let ids: Vec<Uuid> = names.iter().map(|n| resolve_environment($ctx, n)).collect();
                if $negate {
                    Ok(Box::new($col.ne_all(ids).or($col.is_null()).nullable())
                        as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.eq_any(ids).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Gt
            | MatchOp::Gte
            | MatchOp::Lt
            | MatchOp::Lte
            | MatchOp::Like
            | MatchOp::Contains => Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            }),
        }
    }};
}

/// `Store::JsonRoot` over `contexts`/`extra`/`properties` — all three have an
/// empty `prefix` (the column IS the object), unlike Occurrences' `os`/
/// `browser`/`device`/`app`, which nest inside a shared `context` column.
/// Implements the same central rule as `OccurrencesLower::json_object_leaf!`:
/// `Eq`/`Ne`/`In` lower to `@>` containment with the path folded into a nested
/// object bind, never `#>>` equality; `Has` on a single segment uses `?`, on
/// multiple segments `@? … ::jsonpath`; `Like`/`Contains` use `#>> … ILIKE`
/// with the path bound as `Array<Text>` — unindexable, matching the catalog's
/// `Cost::Bounded` classification for these three dimensions.
///
/// `$col_sql` is the column's own name as a string literal — needed only for
/// the multi-segment `Has` branch, which has to name the column in raw SQL
/// text. It is always one of the `&'static str`s already present in
/// `Store::JsonRoot { column, .. }`, never caller input.
macro_rules! json_object_leaf {
    ($col:expr, $col_sql:literal, $prefix:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        // `has:<root>` with no path asks whether the row carries the object at
        // all — column presence, not a path lookup. See the sessions copy of
        // this macro for the full reasoning.
        if $p.path.is_none() && $prefix.is_empty() && matches!($p.op, MatchOp::Has) {
            return Ok(if $negate {
                Box::new($col.is_null().nullable()) as Frag<analytics_events::table>
            } else {
                Box::new($col.is_not_null().nullable()) as Frag<analytics_events::table>
            });
        }
        let segments =
            json_path_segments($prefix, $p.path.as_deref()).ok_or_else(|| PlanError::BadValue {
                field: field.to_string(),
            })?;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                let obj = nest_json_object(&segments, serde_json::Value::String(v));
                if $negate {
                    Ok(Box::new(
                        diesel::dsl::not($col.contains(obj))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                let obj = nest_json_object(&segments, serde_json::Value::String(v));
                if $negate {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new(
                        diesel::dsl::not($col.contains(obj))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<analytics_events::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                let mut vs = vs.into_iter();
                let first = vs.next().ok_or_else(|| PlanError::BadValue {
                    field: field.to_string(),
                })?;
                let mut positive: Frag<analytics_events::table> = Box::new(
                    $col.contains(nest_json_object(
                        &segments,
                        serde_json::Value::String(first),
                    ))
                    .nullable(),
                );
                for v in vs {
                    let obj = nest_json_object(&segments, serde_json::Value::String(v));
                    positive = Box::new(positive.or($col.contains(obj).nullable()));
                }
                if $negate {
                    Ok(
                        Box::new(diesel::dsl::not(positive).or($col.is_null()).nullable())
                            as Frag<analytics_events::table>,
                    )
                } else {
                    Ok(positive)
                }
            }
            MatchOp::Has if segments.len() == 1 => {
                let key = segments[0].clone();
                if $negate {
                    Ok(Box::new(
                        diesel::dsl::not($col.has_key(key))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new($col.has_key(key).nullable()) as Frag<analytics_events::table>)
                }
            }
            MatchOp::Has => {
                // Multi-segment key existence has no `?`-style operator; a
                // jsonpath existence check is the closest correct primitive,
                // and the catalog's `Bounded` classification already prices
                // this as a scan rather than an index hit.
                let jsonpath = format!("$.{}", segments.join("."));
                let positive: Frag<analytics_events::table> = Box::new(
                    sql::<Nullable<Bool>>(concat!("\"analytics_events\".\"", $col_sql, "\" @? "))
                        .bind::<Text, _>(jsonpath)
                        .sql("::jsonpath"),
                );
                if $negate {
                    Ok(
                        Box::new(diesel::dsl::not(positive).or($col.is_null()).nullable())
                            as Frag<analytics_events::table>,
                    )
                } else {
                    Ok(positive)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pattern = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new(
                        diesel::dsl::not(
                            $col.retrieve_by_path_as_text(segments.clone())
                                .ilike(pattern),
                        )
                        .or($col.is_null())
                        .nullable(),
                    ) as Frag<analytics_events::table>)
                } else {
                    Ok(Box::new(
                        $col.retrieve_by_path_as_text(segments.clone())
                            .ilike(pattern)
                            .nullable(),
                    ) as Frag<analytics_events::table>)
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

/// `analytics_events.tags` — a REAL column on this table (unlike
/// `IssuesLower`, where a tag predicate becomes a correlated `EXISTS` into
/// `error_events` because `issues` has no `tags` column at all), so
/// containment/has-key/ILIKE apply directly.
///
/// Tag keys are entirely unconstrained on the write path — `tag:<key>=<value>`
/// is a deliberate escape hatch for keys that are not legal identifiers (see
/// `sauron_query::resolve`) — so the key is always exactly ONE flat segment
/// and must never be split on `.` the way a `Store::JsonRoot` path is; it
/// only ever reaches SQL as a bind, alongside the value.
/// `tag:<value>` with no key — the same predicate across every key of `tags`,
/// via `jsonb_each_text`. Kept separate from the keyed path so that one can go
/// on using the `@>` containment index.
fn tag_any_leaf(
    p: &ResolvedPredicate,
    negate: bool,
) -> Result<Frag<analytics_events::table>, PlanError> {
    let positive: Frag<analytics_events::table> = match p.op {
        MatchOp::Eq | MatchOp::Ne => {
            let value = as_str(&p.value, "tag")?;
            tag_any_cmp(" = ", value)
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, "tag")?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: "tag".to_string(),
            })?;
            let mut acc: Frag<analytics_events::table> = tag_any_cmp(" = ", &first);
            for v in values {
                acc = Box::new(acc.or(tag_any_cmp(" = ", &v)));
            }
            acc
        }
        MatchOp::Has => Box::new(sql::<Nullable<Bool>>(
            "\"analytics_events\".\"tags\" <> '{}'::jsonb",
        )),
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, "tag")?;
            tag_any_cmp(" ILIKE ", pattern)
        }
        MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            return Err(PlanError::UnsupportedOnResource {
                field: "tag".to_string(),
            })
        }
    };
    // `Ne` means "no key holds this value", which is the negation of the same
    // EXISTS — so it flips alongside an explicit NOT.
    let flip = negate ^ matches!(p.op, MatchOp::Ne);
    Ok(if flip {
        Box::new(diesel::dsl::not(positive).nullable())
    } else {
        positive
    })
}

/// One `jsonb_each_text` scan comparing every tag VALUE with `op` (` = ` or
/// ` ILIKE `). `op` is a fixed literal chosen by the caller, never user input.
fn tag_any_cmp(op: &'static str, value: &str) -> Frag<analytics_events::table> {
    Box::new(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM jsonb_each_text(\"analytics_events\".\"tags\") kv \
             WHERE kv.value",
        )
        .sql(op)
        .bind::<Text, _>(value.to_string())
        .sql(")"),
    )
}

fn tag_leaf(
    p: &ResolvedPredicate,
    negate: bool,
) -> Result<Frag<analytics_events::table>, PlanError> {
    // No key named (`tag:value`, `@tag=value`) → match across EVERY tag key.
    let Some(key) = p.path.as_deref() else {
        return tag_any_leaf(p, negate);
    };
    let col = analytics_events::tags;
    match p.op {
        MatchOp::Eq => {
            let value = as_str(&p.value, key)?;
            let obj = nest_json_object(
                &[key.to_string()],
                serde_json::Value::String(value.to_string()),
            );
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(obj))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<analytics_events::table>)
            } else {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<analytics_events::table>)
            }
        }
        MatchOp::Ne => {
            let value = as_str(&p.value, key)?;
            let obj = nest_json_object(
                &[key.to_string()],
                serde_json::Value::String(value.to_string()),
            );
            if negate {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<analytics_events::table>)
            } else {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(obj))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<analytics_events::table>)
            }
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, key)?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: key.to_string(),
            })?;
            let mut positive: Frag<analytics_events::table> = Box::new(
                col.contains(nest_json_object(
                    &[key.to_string()],
                    serde_json::Value::String(first),
                ))
                .nullable(),
            );
            for v in values {
                let obj = nest_json_object(&[key.to_string()], serde_json::Value::String(v));
                positive = Box::new(positive.or(col.contains(obj).nullable()));
            }
            if negate {
                Ok(
                    Box::new(diesel::dsl::not(positive).or(col.is_null()).nullable())
                        as Frag<analytics_events::table>,
                )
            } else {
                Ok(positive)
            }
        }
        MatchOp::Has => {
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.has_key(key.to_string()))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<analytics_events::table>)
            } else {
                Ok(Box::new(col.has_key(key.to_string()).nullable())
                    as Frag<analytics_events::table>)
            }
        }
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, key)?.to_string();
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.retrieve_as_text(key.to_string()).ilike(pattern))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<analytics_events::table>)
            } else {
                Ok(Box::new(
                    col.retrieve_as_text(key.to_string())
                        .ilike(pattern)
                        .nullable(),
                ) as Frag<analytics_events::table>)
            }
        }
        // No ordering comparison is declared for `TAG_DIM`'s ops; kept only
        // for match exhaustiveness, matching `OccurrencesLower::tag_leaf`.
        MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            Err(PlanError::UnsupportedOnResource {
                field: key.to_string(),
            })
        }
    }
}

/// `Store::Column("workflow")` on this resource — the real
/// `analytics_events.workflow_name` column, not `OccurrencesLower`'s plain
/// `str_leaf!` and not `IssuesLower`'s correlated `EXISTS`.
///
/// **Why this one gets its own function instead of `str_leaf!`.** The code this
/// replaces (`repo::list_analytics_events`' `("workflow", …)` arms) pairs every
/// POSITIVE match with `workflow_id IS NOT NULL`, and that term is not
/// decoration: migration `2026-07-29-000032`'s `analytics_events_app_workflow_idx`
/// is `WHERE workflow_id IS NOT NULL`, and Postgres uses a partial index only
/// when the query's WHERE *implies* that predicate — `workflow_name = $N` does
/// not. It is semantically a no-op (the pipeline stamps id and name together),
/// and it was measured on the largest table in the system at 14 buffers / cost
/// 2,025 with the term versus 52,744 / 56,190 without. Dropping it while
/// "unifying" this with `str_leaf!` would be a silent 3,700x regression that no
/// row-level assertion could see.
///
/// **And why the NEGATED arms deliberately omit it.** Their whole purpose is to
/// RETURN the unstamped rows — `!workflow:x` means "not part of workflow x",
/// which is true of a row that is part of no workflow — and those are exactly
/// the rows the partial index excludes. `OR workflow_name IS NULL` is likewise
/// mandatory: SQL's three-valued logic makes a bare `workflow_name <> 'x'`
/// drop every NULL, so the same chip would mean two opposite things on the
/// Events list and on the issues list beside it. See
/// `list_error_events_for_issue`'s `workflow` arms for the full reasoning.
fn workflow_leaf(
    p: &ResolvedPredicate,
    negate: bool,
) -> Result<Frag<analytics_events::table>, PlanError> {
    let field = p.dim.name;
    let col = analytics_events::workflow_name;
    // The positive shape, shared by `Eq`/`!Ne` and (with an ILIKE) `Contains`:
    // the partial-index term ANDed with the name comparison.
    let stamped = || analytics_events::workflow_id.is_not_null().nullable();
    match (p.op, negate) {
        (MatchOp::Eq, false) | (MatchOp::Ne, true) => {
            let v = as_str(&p.value, field)?.to_string();
            Ok(Box::new(stamped().and(col.eq(v).nullable())) as Frag<analytics_events::table>)
        }
        (MatchOp::Eq, true) | (MatchOp::Ne, false) => {
            let v = as_str(&p.value, field)?.to_string();
            Ok(Box::new(col.ne(v).or(col.is_null()).nullable()) as Frag<analytics_events::table>)
        }
        (MatchOp::Contains, false) | (MatchOp::Like, false) => {
            let pat = as_pattern(&p.value, field)?.to_string();
            Ok(Box::new(stamped().and(col.ilike(pat).nullable())) as Frag<analytics_events::table>)
        }
        (MatchOp::Contains, true) | (MatchOp::Like, true) => {
            let pat = as_pattern(&p.value, field)?.to_string();
            Ok(Box::new(col.not_ilike(pat).or(col.is_null()).nullable())
                as Frag<analytics_events::table>)
        }
        // `OPS_WORKFLOW` is `[Eq, Ne, Contains]`, so `resolve` never produces
        // the rest; kept explicit rather than as a catch-all so a widened op
        // list forces a decision here instead of silently 500ing.
        (MatchOp::In, _)
        | (MatchOp::Has, _)
        | (MatchOp::Gt, _)
        | (MatchOp::Gte, _)
        | (MatchOp::Lt, _)
        | (MatchOp::Lte, _) => Err(PlanError::UnsupportedOnResource {
            field: field.to_string(),
        }),
    }
}

/// Lowers predicates resolved against `Resource::Events`.
pub struct EventsLower {
    pub app_id: Uuid,
}

impl EventsLower {
    /// Base scope for this resource: the tenant boundary AND the definition of
    /// "an analytics event" in the Event Explorer's sense. `name <> '$screen'`
    /// excludes the synthetic screen-view rows the mobile SDKs emit (see
    /// `repo::list_analytics_events`, the hand-written code this replaces,
    /// which applies exactly this predicate with the comment "Synthetic
    /// screen-view events belong to the Screens section, not the stream").
    ///
    /// This is deliberately NOT expressed as a `leaf`/`text` predicate a query
    /// could ever produce, and it is not optional: dropping it would leak
    /// `$screen` rows into the product's event stream. Neither `leaf` nor
    /// `text` reference `self.app_id` directly — same shape as
    /// `OccurrencesLower`/`IssuesLower` — because this method, not a
    /// per-predicate hook, is where the caller (Task 6's `prepare.rs`) is
    /// expected to pull both the tenant key and the `$screen` exclusion from,
    /// alongside `since`.
    pub fn base_scope(&self) -> Frag<analytics_events::table> {
        Box::new(
            analytics_events::app_id
                .eq(self.app_id)
                .and(analytics_events::name.ne("$screen"))
                .nullable(),
        )
    }
}

impl ResourceLower for EventsLower {
    type Table = analytics_events::table;

    fn leaf(
        &self,
        p: &ResolvedPredicate,
        ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<analytics_events::table>, PlanError> {
        match p.dim.store {
            // No `Store::Rollup` dimension is declared for Events today;
            // kept for exhaustiveness in case the catalog ever grows one.
            Store::Rollup => Err(PlanError::NotYetSupported {
                field: p.dim.name.to_string(),
            }),
            Store::Tag => tag_leaf(p, negate),
            Store::JsonRoot {
                column: "contexts",
                prefix,
            } => {
                json_object_leaf!(analytics_events::contexts, "contexts", prefix, p, negate)
            }
            // Distinct from `contexts` above — a separate column and a separate
            // catalog dimension, reachable as `context.<path>` or `@context.<path>`.
            Store::JsonRoot {
                column: "context",
                prefix,
            } => {
                json_object_leaf!(analytics_events::context, "context", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "extra",
                prefix,
            } => {
                json_object_leaf!(analytics_events::extra, "extra", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "properties",
                prefix,
            } => {
                json_object_leaf!(
                    analytics_events::properties,
                    "properties",
                    prefix,
                    p,
                    negate
                )
            }
            Store::JsonRoot { column: other, .. } => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
            Store::Column("name") => str_leaf!(analytics_events::name, p, negate),
            Store::Column("release") => str_leaf!(analytics_events::release, p, negate),
            Store::Column("distinct_id") => str_leaf!(analytics_events::distinct_id, p, negate),
            Store::Column("session_id") => str_leaf!(analytics_events::session_id, p, negate),
            Store::Column("environment_id") => {
                environment_leaf!(analytics_events::environment_id, ctx, p, negate)
            }
            // S2c Task 6 widened the catalog's `workflow` dimension to this
            // resource; see `workflow_leaf` for why it is not a `str_leaf!`.
            Store::Column("workflow") => workflow_leaf(p, negate),
            Store::Column(other) => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
        }
    }

    fn text(&self, term: &str) -> Frag<analytics_events::table> {
        // Reproduces the pre-planner behaviour exactly
        // (`repo::list_analytics_events`'s `q` handling): name and distinct_id
        // by ILIKE, plus a payload scan over contexts/extra/properties/tags.
        let pattern = like_contains(term);
        let name: Frag<analytics_events::table> =
            Box::new(analytics_events::name.ilike(pattern.clone()).nullable());
        let distinct_id: Frag<analytics_events::table> = Box::new(
            analytics_events::distinct_id
                .ilike(pattern.clone())
                .nullable(),
        );
        let payload: Frag<analytics_events::table> = Box::new(
            sql::<Nullable<Bool>>(r#""analytics_events"."contexts"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "analytics_events"."extra"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "analytics_events"."properties"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "analytics_events"."tags"::text ILIKE "#)
                .bind::<Text, _>(pattern),
        );
        Box::new(name.or(distinct_id).or(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_plan::lower;
    use chrono::Utc;
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

    fn lower_events_result_with(
        q: &str,
        ctx: &PrepCtx,
    ) -> Result<Frag<analytics_events::table>, PlanError> {
        let node = resolve(&parse(q).unwrap(), Resource::Events).unwrap();
        let l = EventsLower {
            app_id: Uuid::nil(),
        };
        lower(&node, &l, ctx)
    }

    fn lower_events_result(q: &str) -> Result<Frag<analytics_events::table>, PlanError> {
        lower_events_result_with(q, &ctx())
    }

    /// `Frag` is not `Debug`, so `Result::unwrap_err` can't be used directly.
    fn lower_events_err(q: &str) -> PlanError {
        lower_events_result(q).map(|_| ()).unwrap_err()
    }

    fn lower_events_sql_with(q: &str, ctx: &PrepCtx) -> String {
        let frag = lower_events_result_with(q, ctx).unwrap();
        // Selecting just `id` (rather than the full row) keeps assertions that
        // count column-name occurrences (e.g. `repeated_environment_terms_
        // both_apply`) honest — a `SELECT *`-style projection would also name
        // every filtered column once in the select list, double-counting it.
        let query = analytics_events::table
            .into_boxed()
            .filter(frag)
            .select(analytics_events::id);
        debug_query::<Pg, _>(&query).to_string()
    }

    fn lower_events_sql(q: &str) -> String {
        lower_events_sql_with(q, &ctx())
    }

    /// Splits `debug_query`'s `"<sql> -- binds: [...]"` rendering into the SQL
    /// text and the binds trailer, so a test can assert the path is absent
    /// from the former while present in the latter.
    fn lower_events_with(q: &str, ctx: &PrepCtx) -> (String, String) {
        let full = lower_events_sql_with(q, ctx);
        let mut parts = full.splitn(2, "-- binds:");
        let sql = parts.next().unwrap_or_default().to_string();
        let binds = parts.next().unwrap_or_default().to_string();
        (sql, binds)
    }

    fn lower_events(q: &str) -> (String, String) {
        lower_events_with(q, &ctx())
    }

    // -- The brief-mandated environment tests, verbatim ---------------------

    #[test]
    fn an_unknown_environment_matches_nothing_rather_than_being_ignored() {
        let ctx = PrepCtx {
            environments: [("ghost".to_string(), None)].into(),
            now: Utc::now(),
        };
        let (sql, binds) = lower_events_with("environment:ghost", &ctx);
        assert!(
            sql.contains(r#""analytics_events"."environment_id" ="#),
            "{sql}"
        );
        assert!(
            binds.contains("00000000-0000-0000-0000-000000000000"),
            "{binds}"
        );
    }

    #[test]
    fn repeated_environment_terms_both_apply() {
        // B1: the old code (`list_analytics_events`) kept a single `Option`
        // slot per op and silently last-won.
        let sql = lower_events_sql("environment:prod environment:staging");
        assert_eq!(sql.matches("environment_id").count(), 2, "{sql}");
    }

    #[test]
    fn known_environment_resolves_to_its_id() {
        let id = Uuid::new_v4();
        let ctx = PrepCtx {
            environments: [("prod".to_string(), Some(id))].into(),
            now: Utc::now(),
        };
        let sql = lower_events_sql_with("environment:prod", &ctx);
        assert!(sql.contains(&id.to_string()), "{sql}");
    }

    #[test]
    fn missing_from_the_map_entirely_also_matches_nothing() {
        // Defensive: `prepare()` (a later task) is expected to populate every
        // name that appears, but a name absent from an empty map must still
        // be "matches nothing", never "no filter".
        let sql = lower_events_sql("environment:never-looked-up");
        assert!(
            sql.contains("00000000-0000-0000-0000-000000000000"),
            "{sql}"
        );
    }

    // -- JSONB roots: properties/contexts/extra ------------------------------

    #[test]
    fn json_equality_lowers_to_containment_with_the_path_as_a_bind() {
        let (sql, binds) = lower_events("properties.plan:pro");
        assert!(
            sql.contains(r#""analytics_events"."properties" @>"#),
            "{sql}"
        );
        assert!(
            !sql.contains("plan"),
            "the path must NOT appear in SQL text: {sql}"
        );
        assert!(
            binds.contains(r#"Object {"plan": String("pro")}"#),
            "{binds}"
        );
    }

    #[test]
    fn key_existence_uses_the_question_operator() {
        let sql = lower_events_sql("has:extra.cartValue");
        assert!(sql.contains(r#""analytics_events"."extra" ?"#), "{sql}");
    }

    #[test]
    fn multi_segment_has_uses_the_jsonpath_operator() {
        let (sql, binds) = lower_events("has:properties.plan.tier");
        assert!(
            sql.contains(r#""analytics_events"."properties" @? "#),
            "{sql}"
        );
        assert!(sql.contains("::jsonpath"), "{sql}");
        assert!(
            !sql.contains(".tier"),
            "the path must NOT appear in SQL text: {sql}"
        );
        assert!(binds.contains("$.plan.tier"), "{binds}");
    }

    #[test]
    fn json_path_like_uses_the_path_operator_with_ilike() {
        let sql = lower_events_sql("contexts.page:~home");
        assert!(
            sql.contains(r#""analytics_events"."contexts" #>> "#),
            "{sql}"
        );
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    #[test]
    fn json_in_ors_the_per_value_containment_clauses() {
        let sql = lower_events_sql("properties.plan:[pro,enterprise]");
        assert_eq!(
            sql.matches(r#""analytics_events"."properties" @>"#).count(),
            2
        );
        assert!(sql.contains(" OR "), "{sql}");
    }

    #[test]
    fn negated_json_equality_is_null_safe() {
        let sql = lower_events_sql("!properties.plan:pro");
        assert!(sql.contains("NOT"), "{sql}");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    #[test]
    fn a_bare_json_root_with_no_dotted_remainder_and_no_prefix_is_rejected() {
        // `properties`'s prefix is empty (the column IS the object), so a
        // bare `properties:foo` has no segment to nest under.
        let err = lower_events_err("properties:foo");
        assert!(matches!(err, PlanError::BadValue { .. }));
    }

    // -- Tag: a real column here, unlike `IssuesLower`'s correlated EXISTS --

    #[test]
    fn tag_predicate_is_a_direct_column_containment_not_an_exists() {
        let sql = lower_events_sql("tag.plan:pro");
        assert!(sql.contains(r#""analytics_events"."tags" @>"#), "{sql}");
        assert!(!sql.contains("EXISTS"), "{sql}");
    }

    #[test]
    fn tag_never_leaks_the_key_or_value_into_sql_text() {
        let sql = lower_events_sql("tag.plan:pro");
        let query_text = sql.split("-- binds:").next().unwrap();
        assert!(!query_text.contains("plan"), "{query_text}");
        assert!(!query_text.contains("\"pro\""), "{query_text}");
    }

    #[test]
    fn tag_has_uses_the_question_operator() {
        let sql = lower_events_sql("has:tag.plan");
        assert!(sql.contains(r#""analytics_events"."tags" ?"#), "{sql}");
    }

    #[test]
    fn tag_like_uses_the_arrow_operator_and_ilike() {
        let sql = lower_events_sql("tag.plan:~pro");
        assert!(sql.contains(r#""analytics_events"."tags" ->>"#), "{sql}");
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    // -- Store::Column, plain text -------------------------------------------

    #[test]
    fn column_eq_lowers_to_a_plain_equality() {
        let sql = lower_events_sql("name:signup");
        assert!(sql.contains(r#""analytics_events"."name" = $1"#), "{sql}");
    }

    #[test]
    fn negated_equality_is_null_safe() {
        // B2: `.ne()` alone drops rows where the column IS NULL — `release`
        // is nullable.
        let sql = lower_events_sql("!release:1.0.0");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    #[test]
    fn has_on_a_plain_column_checks_is_not_null() {
        let sql = lower_events_sql("has:session");
        assert!(
            sql.contains(r#""analytics_events"."session_id" IS NOT NULL"#),
            "{sql}"
        );
    }

    #[test]
    fn column_in_lowers_to_eq_any() {
        let sql = lower_events_sql("distinctId:[u1,u2]");
        assert!(sql.contains(r#""analytics_events"."distinct_id""#), "{sql}");
        assert!(sql.contains("ANY"), "{sql}");
    }

    // -- Free text -------------------------------------------------------------

    #[test]
    fn free_text_matches_name_distinct_id_and_the_payload() {
        let sql = lower_events_sql("signup");
        assert!(
            sql.contains(r#""analytics_events"."name" ILIKE $1"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""analytics_events"."distinct_id" ILIKE $2"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""analytics_events"."contexts"::text ILIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""analytics_events"."extra"::text ILIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""analytics_events"."properties"::text ILIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""analytics_events"."tags"::text ILIKE"#),
            "{sql}"
        );
    }

    // -- Composition sanity ---------------------------------------------------

    #[test]
    fn a_predicate_and_free_text_combine() {
        let sql = lower_events_sql("name:signup boom");
        assert!(sql.contains(r#""analytics_events"."name" = $1"#), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }

    // -- workflow: a real column, with the partial-index term ----------------

    /// The measured reason `workflow_leaf` exists rather than a `str_leaf!`:
    /// `analytics_events_app_workflow_idx` is a PARTIAL index
    /// (`WHERE workflow_id IS NOT NULL`), and Postgres only uses one when the
    /// query's WHERE implies its predicate.
    #[test]
    fn positive_workflow_carries_the_partial_index_term() {
        for q in ["workflow:checkout", "workflow:~check"] {
            let sql = lower_events_sql(q);
            assert!(
                sql.contains(r#""analytics_events"."workflow_id" IS NOT NULL"#),
                "`{q}` must imply the partial index's predicate: {sql}"
            );
            assert!(
                sql.contains(r#""analytics_events"."workflow_name""#),
                "{sql}"
            );
        }
    }

    /// …and the negated arms must NOT, because the rows they exist to return
    /// are exactly the ones that index excludes.
    #[test]
    fn negated_workflow_keeps_unstamped_events_and_drops_the_index_term() {
        let sql = lower_events_sql("!workflow:checkout");
        assert!(
            sql.contains(r#""analytics_events"."workflow_name" IS NULL"#),
            "a bare `<>` would drop every unstamped row: {sql}"
        );
        assert!(
            !sql.contains(r#""analytics_events"."workflow_id" IS NOT NULL"#),
            "the partial-index term would exclude the very rows this arm returns: {sql}"
        );
    }

    #[test]
    fn negated_workflow_contains_is_also_null_safe() {
        let sql = lower_events_sql("!workflow:~check");
        assert!(sql.contains("NOT ILIKE"), "{sql}");
        assert!(
            sql.contains(r#""analytics_events"."workflow_name" IS NULL"#),
            "{sql}"
        );
    }

    /// The whole reason the catalog entry was widened to Events: without it,
    /// `resolve_field`'s step-4 fallback reads the bare field `workflow` as a
    /// TAG KEY and every `filter=workflow:…` bookmark on the Event Explorer
    /// silently probes `analytics_events.tags` instead. No error — a different
    /// answer, which is worse.
    #[test]
    fn workflow_is_not_reinterpreted_as_a_tag_key() {
        let sql = lower_events_sql("workflow:checkout");
        // The tag OPERATORS, not the bare column name: `tags` appears in the
        // SELECT list of most queries over this table, so `contains("tags")`
        // alone would be an assertion that can never fail.
        for tag_op in [
            r#""analytics_events"."tags" @>"#,
            r#""analytics_events"."tags" ?"#,
            r#""analytics_events"."tags" ->>"#,
        ] {
            assert!(
                !sql.contains(tag_op),
                "workflow must not fall through to the tag store ({tag_op}): {sql}"
            );
        }
    }

    // -- Base scope: tenant key + the `$screen` exclusion --------------------

    #[test]
    fn base_scope_carries_the_tenant_key_and_excludes_synthetic_screen_views() {
        let app_id = Uuid::new_v4();
        let l = EventsLower { app_id };
        let query = analytics_events::table.into_boxed().filter(l.base_scope());
        let sql = debug_query::<Pg, _>(&query).to_string();
        assert!(sql.contains(r#""analytics_events"."app_id" = $1"#), "{sql}");
        assert!(sql.contains(r#""analytics_events"."name""#), "{sql}");
        let binds = sql.split("-- binds:").nth(1).unwrap_or_default();
        assert!(binds.contains("$screen"), "{binds}");
        assert!(binds.contains(&app_id.to_string()), "{binds}");
    }
}
