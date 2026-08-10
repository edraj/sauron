//! `OccurrencesLower`: turns one `ResolvedPredicate` (or free-text term) resolved
//! against `Resource::Occurrences` into a diesel boxed fragment over
//! `error_events::table`.
//!
//! This is the largest lowerer in the slice (19 catalog dimensions) and the
//! only one with `Store::JsonRoot` leaves, so most of this module is the JSONB
//! containment rule — measured against a live database, not guessed:
//!
//! | Lowering | Plan |
//! |---|---|
//! | `col @> $1::jsonb` | Index Cond |
//! | `col ? $1` (top-level key existence) | Index Cond |
//! | `col -> 'a' ? 'b'` | Seq Scan |
//! | `col #>> '{a,b}' = 'v'` | Seq Scan |
//!
//! So `Eq` on a JSON root lowers to `@>` containment — never `#>>` equality —
//! with the whole nested object built in Rust from `Store::JsonRoot`'s
//! `prefix` and `ResolvedPredicate.path` (`sauron_query::resolve::resolve_field`
//! already folds `prefix` into `path` when the field had a dotted remainder;
//! see `json_path_segments` in `mod.rs`) and bound as ONE `Jsonb` parameter.
//! The caller-supplied path NEVER appears in SQL text — a strictly stronger
//! injection story than the code this replaces, which built `#>> '{a,b}'`
//! array literals directly from user input.
//!
//! `stack` is the one dimension needing a differently-shaped branch:
//! `stacktrace` holds a JSON ARRAY (`[{filename, function, …}, …]`), so a
//! matched value must be wrapped in a one-element array before it can ever
//! contain anything (`stack_leaf`, kept separate from `json_object_leaf!`).
//!
//! `tag` here is a REAL column (`error_events.tags`), unlike `IssuesLower`,
//! where it became a correlated `EXISTS` because `issues` has no `tags`
//! column at all — so containment/has-key/ILIKE apply directly, no subquery.
//!
//! `environment` is a NAME on the wire but a `Nullable<Uuid>` column
//! (`environment_id`); `ctx.environments` resolves the name, and an unknown
//! name lowers to `Uuid::nil()` so the predicate matches nothing — never to
//! "no filter" (see `resolve_environment`).
//!
//! Base scope for this resource is `error_events.issue_id = $1 AND
//! error_events.app_id = $2` — new versus the code being replaced, which
//! rested tenancy solely on a handler pre-check. Neither `leaf` nor `text`
//! reference `self.app_id`/`self.issue_id` directly (no user predicate
//! targets them), matching `IssuesLower`'s shape: the fields exist so callers
//! construct one `OccurrencesLower` per request and so the base-scope filter
//! — applied once by the caller, alongside `since` and the environment — has
//! a `Uuid` to bind.
//!
//! `text_reach` is NOT decoration (S2c Task 5, this lowerer's first caller).
//! Every leaf here applies to the already-scoped row, so unlike `IssuesLower`
//! nothing needs an environment bind — but the free-text payload scan over
//! `contexts`/`extra`/`tags` is exactly the oracle `TextSearchReach` closes,
//! and the route this lowerer now backs (`/v1/apps/{id}/issues/{id}/events`)
//! authorizes on `issue:read` ALONE. Emitting the payload half unconditionally
//! would have answered "does this occurrence's `extra` contain …" for a caller
//! whose response arrives with `extra` nulled — a regression of D4, reintroduced
//! by the act of moving the route onto the planner. See `repo::TextSearchReach`.

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Nullable, Text};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TypedValue};

use crate::query_plan::{
    json_path_segments, nest_json_object, Frag, PlanError, PrepCtx, ResourceLower,
};
use crate::repo::{like_contains, TextSearchReach};
use crate::schema::error_events;

// ===========================================================================
// Value extraction — see `issues.rs`'s identical section: a bad combination
// here is a planner bug, since `resolve.rs` already enforces the catalog's
// `ops`/`ty` contract per dimension.
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

fn as_bool(v: &TypedValue, field: &str) -> Result<bool, PlanError> {
    match v {
        TypedValue::Bool(b) => Ok(*b),
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

fn as_bool_list(v: &TypedValue, field: &str) -> Result<Vec<bool>, PlanError> {
    match v {
        TypedValue::List(items) => items
            .iter()
            .map(|i| match i {
                TypedValue::Bool(b) => Ok(*b),
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
// when it was already nullable, so one macro body serves both.
// ===========================================================================

/// Eq/Ne/In/Has/Like/Contains over a `Text`/`Nullable<Text>` column.
/// `Gt`/`Gte`/`Lt`/`Lte` never reach a text dimension on this resource.
macro_rules! str_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pat = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.not_ilike(pat).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.ilike(pat).nullable()) as Frag<error_events::table>)
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

/// Eq/Ne/In/Has over the `Nullable<Bool>` column `handled`. `Gt`/`Gte`/`Lt`/
/// `Lte`/`Like`/`Contains` never reach a bool dimension — the catalog grants
/// `handled` only `OPS_EQ`.
macro_rules! bool_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_bool(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_bool(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                }
            }
            MatchOp::In => {
                let vs = as_bool_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<error_events::table>)
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

/// Eq/Ne/In/Has over the `Nullable<Uuid>` column `environment_id`, translating
/// the wire NAME to an id via `resolve_environment` first. Same shape as
/// `bool_leaf!`/`int_leaf` in `issues.rs` — only the value extraction differs.
macro_rules! environment_leaf {
    ($col:expr, $ctx:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Ne => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                }
            }
            MatchOp::In => {
                let names = as_str_list(&$p.value, field)?;
                let ids: Vec<Uuid> = names.iter().map(|n| resolve_environment($ctx, n)).collect();
                if $negate {
                    Ok(Box::new($col.ne_all(ids).or($col.is_null()).nullable())
                        as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.eq_any(ids).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<error_events::table>)
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

/// `Store::JsonRoot` over an OBJECT-shaped column (`context`, `contexts`,
/// `extra`, `event_user`, `sdk` — every JSON root except `stack`, which is an
/// array and gets its own function below). Implements the table's central
/// rule: `Eq`/`Ne`/`In` lower to `@>` containment with the path folded into a
/// nested object bind, never `#>>` equality; `Has` on a single segment uses
/// `?`, on multiple segments `@? … ::jsonpath`; `Like`/`Contains` use
/// `#>> … ILIKE` with the path bound as `Array<Text>`, which is unindexable
/// but exactly what the catalog already classes `Cost::Scan`.
///
/// `$col_sql` is the column's own name as a string literal — needed only for
/// the multi-segment `Has` branch, which has to name the column in raw SQL
/// text (there is no diesel-native `@?` operator to hang the typed `$col`
/// expression off of). It is always one of the `&'static str`s already
/// present in `Store::JsonRoot { column, .. }`, never caller input.
macro_rules! json_object_leaf {
    ($col:expr, $col_sql:literal, $prefix:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
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
                    ) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                let obj = nest_json_object(&segments, serde_json::Value::String(v));
                if $negate {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<error_events::table>)
                } else {
                    Ok(Box::new(
                        diesel::dsl::not($col.contains(obj))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<error_events::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                let mut vs = vs.into_iter();
                let first = vs.next().ok_or_else(|| PlanError::BadValue {
                    field: field.to_string(),
                })?;
                let mut positive: Frag<error_events::table> = Box::new(
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
                            as Frag<error_events::table>,
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
                    ) as Frag<error_events::table>)
                } else {
                    Ok(Box::new($col.has_key(key).nullable()) as Frag<error_events::table>)
                }
            }
            MatchOp::Has => {
                // Multi-segment key existence has no `?`-style operator; a
                // jsonpath existence check is the closest correct primitive,
                // and the catalog's `Bounded` classification already prices
                // this as a scan rather than an index hit.
                let jsonpath = format!("$.{}", segments.join("."));
                let positive: Frag<error_events::table> = Box::new(
                    sql::<Nullable<Bool>>(concat!("\"error_events\".\"", $col_sql, "\" @? "))
                        .bind::<Text, _>(jsonpath)
                        .sql("::jsonpath"),
                );
                if $negate {
                    Ok(
                        Box::new(diesel::dsl::not(positive).or($col.is_null()).nullable())
                            as Frag<error_events::table>,
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
                    ) as Frag<error_events::table>)
                } else {
                    Ok(Box::new(
                        $col.retrieve_by_path_as_text(segments.clone())
                            .ilike(pattern)
                            .nullable(),
                    ) as Frag<error_events::table>)
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

/// `stacktrace` holds a JSON ARRAY (`[{filename, function, …}, …]`), not an
/// object — the one dimension needing a differently-shaped branch. A matched
/// value is wrapped in a one-element array so `@>` asks "does some array
/// element contain this", never folded into `json_object_leaf!`'s object
/// nesting (which would build `{"filename": "app.js"}` and never match an
/// array column at all).
fn stack_leaf(p: &ResolvedPredicate, negate: bool) -> Result<Frag<error_events::table>, PlanError> {
    let field = p.dim.name;
    // `stack`'s `Store::JsonRoot` prefix is always empty — the whole path
    // comes from the dotted remainder (`stack.filename` -> path "filename").
    let segments =
        json_path_segments("", p.path.as_deref()).ok_or_else(|| PlanError::BadValue {
            field: field.to_string(),
        })?;
    let col = error_events::stacktrace;
    let array_of = |v: String| {
        serde_json::Value::Array(vec![nest_json_object(
            &segments,
            serde_json::Value::String(v),
        )])
    };
    match p.op {
        MatchOp::Eq => {
            let v = as_str(&p.value, field)?.to_string();
            let arr = array_of(v);
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(arr))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<error_events::table>)
            } else {
                Ok(Box::new(col.contains(arr).nullable()) as Frag<error_events::table>)
            }
        }
        MatchOp::Ne => {
            let v = as_str(&p.value, field)?.to_string();
            let arr = array_of(v);
            if negate {
                Ok(Box::new(col.contains(arr).nullable()) as Frag<error_events::table>)
            } else {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(arr))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<error_events::table>)
            }
        }
        MatchOp::In => {
            let vs = as_str_list(&p.value, field)?;
            let mut vs = vs.into_iter();
            let first = vs.next().ok_or_else(|| PlanError::BadValue {
                field: field.to_string(),
            })?;
            let mut positive: Frag<error_events::table> =
                Box::new(col.contains(array_of(first)).nullable());
            for v in vs {
                positive = Box::new(positive.or(col.contains(array_of(v)).nullable()));
            }
            if negate {
                Ok(
                    Box::new(diesel::dsl::not(positive).or(col.is_null()).nullable())
                        as Frag<error_events::table>,
                )
            } else {
                Ok(positive)
            }
        }
        MatchOp::Has => {
            // No meaningful "top-level key" on an array column, so every
            // `has:stack.*` is "some element has this key" via a jsonpath
            // existence check with an array wildcard — unlike the object
            // branch, this does not distinguish single- from multi-segment.
            let jsonpath = format!("$[*].{}", segments.join("."));
            let positive: Frag<error_events::table> = Box::new(
                sql::<Nullable<Bool>>(r#""error_events"."stacktrace" @? "#)
                    .bind::<Text, _>(jsonpath)
                    .sql("::jsonpath"),
            );
            if negate {
                Ok(
                    Box::new(diesel::dsl::not(positive).or(col.is_null()).nullable())
                        as Frag<error_events::table>,
                )
            } else {
                Ok(positive)
            }
        }
        MatchOp::Like | MatchOp::Contains => {
            // A per-element `#>>` needs an index, which a search query does
            // not have; cast the whole array to text and scan it instead —
            // the same technique `text()` below uses for `contexts`/`extra`/
            // `tags`. This matches the pattern anywhere in the array's
            // rendered text rather than scoped to the named key; scoping it
            // would need a jsonpath `like_regex` filter, which adds its own
            // string-escaping surface for a dimension the catalog already
            // prices as `Cost::Scan` either way.
            let pattern = as_pattern(&p.value, field)?.to_string();
            let as_text = sql::<Text>(r#""error_events"."stacktrace"::text"#);
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(as_text.ilike(pattern))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<error_events::table>)
            } else {
                Ok(Box::new(as_text.ilike(pattern).nullable()) as Frag<error_events::table>)
            }
        }
        MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            })
        }
    }
}

/// `error_events.tags` — a REAL column on this table (unlike `IssuesLower`,
/// where a tag predicate becomes a correlated `EXISTS` because `issues` has
/// no `tags` column), so containment/has-key/ILIKE apply directly.
///
/// Tag keys are entirely unconstrained on the write path — `tag:<key>=<value>`
/// is a deliberate escape hatch for keys that are not legal identifiers (see
/// `sauron_query::resolve`) — so the key is always exactly ONE flat segment
/// and must never be split on `.` the way a `Store::JsonRoot` path is; it
/// only ever reaches SQL as a bind, alongside the value.
fn tag_leaf(p: &ResolvedPredicate, negate: bool) -> Result<Frag<error_events::table>, PlanError> {
    let key = p.path.as_deref().ok_or_else(|| PlanError::BadValue {
        field: "tag".to_string(),
    })?;
    let col = error_events::tags;
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
                ) as Frag<error_events::table>)
            } else {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<error_events::table>)
            }
        }
        MatchOp::Ne => {
            let value = as_str(&p.value, key)?;
            let obj = nest_json_object(
                &[key.to_string()],
                serde_json::Value::String(value.to_string()),
            );
            if negate {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<error_events::table>)
            } else {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(obj))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<error_events::table>)
            }
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, key)?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: key.to_string(),
            })?;
            let mut positive: Frag<error_events::table> = Box::new(
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
                        as Frag<error_events::table>,
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
                ) as Frag<error_events::table>)
            } else {
                Ok(Box::new(col.has_key(key.to_string()).nullable()) as Frag<error_events::table>)
            }
        }
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, key)?.to_string();
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.retrieve_as_text(key.to_string()).ilike(pattern))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<error_events::table>)
            } else {
                Ok(Box::new(
                    col.retrieve_as_text(key.to_string())
                        .ilike(pattern)
                        .nullable(),
                ) as Frag<error_events::table>)
            }
        }
        // `Ne` handled above for exhaustiveness parity with `str_leaf!`; no
        // ordering comparison is declared for `TAG_DIM`'s ops.
        MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            Err(PlanError::UnsupportedOnResource {
                field: key.to_string(),
            })
        }
    }
}

/// Lowers predicates resolved against `Resource::Occurrences`.
pub struct OccurrencesLower {
    pub app_id: Uuid,
    pub issue_id: Uuid,
    /// Whether a free-text term may reach `contexts`/`extra`/`tags` — the
    /// three columns `symbolicate::strip_event_body` nulls for a caller
    /// holding `issue:read` without `event:read`. See the module docs.
    pub text_reach: TextSearchReach,
}

impl ResourceLower for OccurrencesLower {
    type Table = error_events::table;

    fn leaf(
        &self,
        p: &ResolvedPredicate,
        ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<error_events::table>, PlanError> {
        match p.dim.store {
            // No `Store::Rollup` dimension is declared for Occurrences today;
            // kept for exhaustiveness in case the catalog ever grows one.
            Store::Rollup => Err(PlanError::NotYetSupported {
                field: p.dim.name.to_string(),
            }),
            Store::Tag => tag_leaf(p, negate),
            Store::JsonRoot {
                column: "stacktrace",
                ..
            } => stack_leaf(p, negate),
            Store::JsonRoot {
                column: "context",
                prefix,
            } => {
                json_object_leaf!(error_events::context, "context", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "event_user",
                prefix,
            } => {
                json_object_leaf!(error_events::event_user, "event_user", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "sdk",
                prefix,
            } => {
                json_object_leaf!(error_events::sdk, "sdk", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "contexts",
                prefix,
            } => {
                json_object_leaf!(error_events::contexts, "contexts", prefix, p, negate)
            }
            Store::JsonRoot {
                column: "extra",
                prefix,
            } => {
                json_object_leaf!(error_events::extra, "extra", prefix, p, negate)
            }
            Store::JsonRoot { column: other, .. } => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
            Store::Column("level") => str_leaf!(error_events::level, p, negate),
            Store::Column("handled") => bool_leaf!(error_events::handled, p, negate),
            Store::Column("environment_id") => {
                environment_leaf!(error_events::environment_id, ctx, p, negate)
            }
            Store::Column("release") => str_leaf!(error_events::release, p, negate),
            Store::Column("distinct_id") => str_leaf!(error_events::distinct_id, p, negate),
            Store::Column("session_id") => str_leaf!(error_events::session_id, p, negate),
            Store::Column("device_key") => str_leaf!(error_events::device_key, p, negate),
            Store::Column("screen") => str_leaf!(error_events::screen, p, negate),
            Store::Column("symbolication_status") => {
                str_leaf!(error_events::symbolication_status, p, negate)
            }
            Store::Column("message") => str_leaf!(error_events::message, p, negate),
            // `Store::Column("workflow")` names where the value lives *for the
            // resource being lowered*: on Issues it is a correlated EXISTS
            // (there is no such column on `issues`), here it is the real
            // `workflow_name` column, so an ordinary `str_leaf!` serves it.
            //
            // `str_leaf!`'s negated `Eq` emits `<> $1 OR IS NULL`, which is
            // exactly the semantics `error_events_for_issue_query`'s
            // `("workflow", Op::Neq)` arm went out of its way to hand-write —
            // a bare `<>` would drop every unstamped row and make one chip mean
            // two opposite things at two levels of the same drill-down. Read
            // that arm's comment before "simplifying" either side.
            Store::Column("workflow") => str_leaf!(error_events::workflow_name, p, negate),
            Store::Column(other) => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
        }
    }

    fn text(&self, term: &str) -> Frag<error_events::table> {
        // Reproduces the pre-planner behaviour exactly
        // (`repo::error_events_for_issue_query`'s `q` handling): message and
        // the two exception fields by ILIKE, plus — for a caller whose reach
        // includes the body — a payload scan over contexts/extra/tags. Neither
        // `message` nor `exception_type`/`exception_value` require
        // `app_id`/`issue_id` re-assertion here — unlike `IssuesLower`'s
        // correlated subquery, this predicate applies directly to the row
        // already scoped by the caller's base filter.
        let pattern = like_contains(term);
        let message: Frag<error_events::table> =
            Box::new(error_events::message.ilike(pattern.clone()).nullable());
        let exception_type: Frag<error_events::table> = Box::new(
            error_events::exception_type
                .ilike(pattern.clone())
                .nullable(),
        );
        let exception_value: Frag<error_events::table> = Box::new(
            error_events::exception_value
                .ilike(pattern.clone())
                .nullable(),
        );
        let shell: Frag<error_events::table> =
            Box::new(message.or(exception_type).or(exception_value));
        // The withheld half. `message`/`exception_type`/`exception_value` above
        // are exactly the text columns `strip_event_body` KEEPS, so every row
        // the shell matched can be read back; `contexts`/`extra`/`tags` are
        // three it NULLS, and matching them for a caller who will receive them
        // as `null` answers a question the response is forbidden to. Same
        // branch, same reasoning, as `IssuesLower::text`.
        if !self.text_reach.includes_body() {
            return shell;
        }
        let payload: Frag<error_events::table> = Box::new(
            sql::<Nullable<Bool>>(r#""error_events"."contexts"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "error_events"."extra"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "error_events"."tags"::text ILIKE "#)
                .bind::<Text, _>(pattern),
        );
        Box::new(shell.or(payload))
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

    fn lower_occ_result_with(
        q: &str,
        ctx: &PrepCtx,
    ) -> Result<Frag<error_events::table>, PlanError> {
        lower_occ_result_reach(q, ctx, TextSearchReach::IncludingBody)
    }

    /// `IncludingBody` is the default everywhere above because it is the
    /// wider predicate — a test that asserted a column IS matched would pass
    /// vacuously under `ShellOnly`. The narrowing itself gets its own tests.
    fn lower_occ_result_reach(
        q: &str,
        ctx: &PrepCtx,
        text_reach: TextSearchReach,
    ) -> Result<Frag<error_events::table>, PlanError> {
        let node = resolve(&parse(q).unwrap(), Resource::Occurrences).unwrap();
        let l = OccurrencesLower {
            app_id: Uuid::nil(),
            issue_id: Uuid::nil(),
            text_reach,
        };
        lower(&node, &l, ctx)
    }

    fn lower_occ_sql_reach(q: &str, text_reach: TextSearchReach) -> String {
        let frag = lower_occ_result_reach(q, &ctx(), text_reach).unwrap();
        let query = error_events::table.into_boxed().filter(frag);
        debug_query::<Pg, _>(&query).to_string()
    }

    fn lower_occ_result(q: &str) -> Result<Frag<error_events::table>, PlanError> {
        lower_occ_result_with(q, &ctx())
    }

    /// `Frag` is not `Debug`, so `Result::unwrap_err` can't be used directly.
    fn lower_occ_err(q: &str) -> PlanError {
        lower_occ_result(q).map(|_| ()).unwrap_err()
    }

    fn lower_occ_sql_with(q: &str, ctx: &PrepCtx) -> String {
        let frag = lower_occ_result_with(q, ctx).unwrap();
        let query = error_events::table.into_boxed().filter(frag);
        debug_query::<Pg, _>(&query).to_string()
    }

    fn lower_occ_sql(q: &str) -> String {
        lower_occ_sql_with(q, &ctx())
    }

    /// Splits `debug_query`'s `"<sql> -- binds: [...]"` rendering into the SQL
    /// text and the binds trailer, so a test can assert the path is absent
    /// from the former while present in the latter.
    fn lower_occ(q: &str) -> (String, String) {
        let full = lower_occ_sql(q);
        let mut parts = full.splitn(2, "-- binds:");
        let sql = parts.next().unwrap_or_default().to_string();
        let binds = parts.next().unwrap_or_default().to_string();
        (sql, binds)
    }

    // -- The four JSONB-rule tests mandated by the task brief ---------------

    // NOTE on the bind assertions below: `debug_query`'s binds trailer renders
    // each bound Rust value with `{:?}`, and `serde_json::Value`'s `Debug` impl
    // is NOT compact JSON — it is `Object {"k": String("v")}`-shaped (see
    // `serde_json::value::Value`'s hand-written `Debug`). The assertions match
    // that real rendering rather than the compact-JSON text one might expect,
    // while still proving the nested shape (and array-vs-object shape, for
    // `stack`) that the containment bind actually carries.

    #[test]
    fn json_equality_lowers_to_containment_with_the_path_as_a_bind() {
        let (sql, binds) = lower_occ("os.name:Linux");
        assert!(sql.contains(r#""error_events"."context" @>"#), "{sql}");
        assert!(
            !sql.contains("os"),
            "the path must NOT appear in SQL text: {sql}"
        );
        assert!(
            binds.contains(r#"Object {"os": Object {"name": String("Linux")}}"#),
            "{binds}"
        );
    }

    #[test]
    fn user_email_does_not_duplicate_the_root_segment() {
        // The column IS the user object, so the prefix is empty.
        let (_sql, binds) = lower_occ("user.email:a@b.com");
        assert!(
            binds.contains(r#"Object {"email": String("a@b.com")}"#),
            "{binds}"
        );
    }

    #[test]
    fn key_existence_uses_the_question_operator() {
        let sql = lower_occ_sql("has:extra.cartValue");
        assert!(sql.contains(r#""error_events"."extra" ?"#), "{sql}");
    }

    #[test]
    fn stack_uses_array_containment_not_object_paths() {
        // `stacktrace` is a JSON array; object containment would never match.
        let (sql, binds) = lower_occ("stack.filename:app.js");
        assert!(sql.contains(r#""error_events"."stacktrace" @>"#), "{sql}");
        assert!(
            binds.contains(r#"Array [Object {"filename": String("app.js")}]"#),
            "array shape required: {binds}"
        );
    }

    // -- More JSONB coverage: multi-segment Has, Like/Contains, browser alias --

    #[test]
    fn multi_segment_has_uses_the_jsonpath_operator() {
        let (sql, binds) = lower_occ("has:os.name");
        assert!(sql.contains(r#""error_events"."context" @? "#), "{sql}");
        assert!(sql.contains("::jsonpath"), "{sql}");
        assert!(
            !sql.contains(".name"),
            "the path must NOT appear in SQL text: {sql}"
        );
        assert!(binds.contains("$.os.name"), "{binds}");
    }

    #[test]
    fn single_segment_has_ignores_the_prefix_double_count() {
        // `has:os` has no dotted remainder — `prefix` ("os") is the sole
        // segment, so this must take the single-segment `?` branch, not the
        // multi-segment jsonpath one.
        let sql = lower_occ_sql("has:os");
        assert!(sql.contains(r#""error_events"."context" ?"#), "{sql}");
        assert!(!sql.contains("@?"), "{sql}");
    }

    #[test]
    fn json_path_like_uses_the_path_operator_with_ilike() {
        let sql = lower_occ_sql("os.name:~inux");
        assert!(sql.contains(r#""error_events"."context" #>> "#), "{sql}");
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    #[test]
    fn browser_alias_and_canonical_name_target_the_same_runtime_prefix() {
        let (_sql, canonical_binds) = lower_occ("browser.name:Chrome");
        let (_sql2, alias_binds) = lower_occ("runtime.name:Chrome");
        assert!(
            canonical_binds.contains(r#"Object {"runtime": Object {"name": String("Chrome")}}"#),
            "{canonical_binds}"
        );
        assert_eq!(canonical_binds, alias_binds);
    }

    #[test]
    fn json_in_ors_the_per_value_containment_clauses() {
        let sql = lower_occ_sql("os.name:[Linux,macOS]");
        assert_eq!(sql.matches(r#""error_events"."context" @>"#).count(), 2);
        assert!(sql.contains(" OR "), "{sql}");
    }

    #[test]
    fn negated_json_equality_is_null_safe() {
        let sql = lower_occ_sql("!os.name:Linux");
        assert!(sql.contains("NOT"), "{sql}");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    #[test]
    fn a_bare_json_root_with_no_dotted_remainder_and_no_prefix_is_rejected() {
        // `extra`'s prefix is empty (the column IS the object), so a bare
        // `extra:foo` has no segment to nest under — must error, not build a
        // vacuous filter.
        let err = lower_occ_err("extra:foo");
        assert!(matches!(err, PlanError::BadValue { .. }));
    }

    // -- Tag: a real column here, unlike `IssuesLower`'s correlated EXISTS --

    #[test]
    fn tag_predicate_is_a_direct_column_containment_not_an_exists() {
        let sql = lower_occ_sql("tag.checkout_step:payment");
        assert!(sql.contains(r#""error_events"."tags" @>"#), "{sql}");
        assert!(!sql.contains("EXISTS"), "{sql}");
    }

    #[test]
    fn tag_never_leaks_the_key_or_value_into_sql_text() {
        let sql = lower_occ_sql("tag.checkout_step:payment");
        let query_text = sql.split("-- binds:").next().unwrap();
        assert!(!query_text.contains("checkout_step"), "{query_text}");
        assert!(!query_text.contains("payment"), "{query_text}");
    }

    #[test]
    fn tag_has_uses_the_question_operator() {
        let sql = lower_occ_sql("has:tag.checkout_step");
        assert!(sql.contains(r#""error_events"."tags" ?"#), "{sql}");
    }

    #[test]
    fn tag_like_uses_the_arrow_operator_and_ilike() {
        let sql = lower_occ_sql("tag.checkout_step:~payment");
        assert!(sql.contains(r#""error_events"."tags" ->>"#), "{sql}");
        assert!(sql.contains("ILIKE"), "{sql}");
    }

    // -- Store::Column, plain text/bool -------------------------------------

    #[test]
    fn column_eq_lowers_to_a_plain_equality() {
        let sql = lower_occ_sql("level:error");
        assert!(sql.contains(r#""error_events"."level" = $1"#), "{sql}");
    }

    #[test]
    fn negated_equality_is_null_safe() {
        // B2: `.ne()` alone drops rows where the column IS NULL — `screen` is
        // nullable, so this is not a no-op the way it is on `issues`.
        let sql = lower_occ_sql("!screen:checkout");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    #[test]
    fn bool_column_accepts_true_and_false() {
        assert!(lower_occ_sql("handled:true").contains(r#""error_events"."handled" = $1"#));
        assert!(lower_occ_sql("handled:false").contains(r#""error_events"."handled" = $1"#));
    }

    #[test]
    fn has_on_a_plain_column_checks_is_not_null() {
        let sql = lower_occ_sql("has:screen");
        assert!(
            sql.contains(r#""error_events"."screen" IS NOT NULL"#),
            "{sql}"
        );
    }

    // -- environment: name on the wire, uuid in the column -------------------

    #[test]
    fn unknown_environment_matches_nothing_rather_than_being_ignored() {
        let ctx = PrepCtx {
            environments: [("ghost".to_string(), None)].into(),
            now: Utc::now(),
        };
        let sql = lower_occ_sql_with("environment:ghost", &ctx);
        assert!(
            sql.contains(r#""error_events"."environment_id" ="#),
            "{sql}"
        );
        assert!(
            sql.contains("00000000-0000-0000-0000-000000000000"),
            "{sql}"
        );
    }

    #[test]
    fn known_environment_resolves_to_its_id() {
        let id = Uuid::new_v4();
        let ctx = PrepCtx {
            environments: [("prod".to_string(), Some(id))].into(),
            now: Utc::now(),
        };
        let sql = lower_occ_sql_with("environment:prod", &ctx);
        assert!(sql.contains(&id.to_string()), "{sql}");
    }

    #[test]
    fn missing_from_the_map_entirely_also_matches_nothing() {
        // Defensive: `prepare()` (a later task) is expected to populate every
        // name that appears, but a name absent from an empty map must still
        // be "matches nothing", never "no filter".
        let sql = lower_occ_sql("environment:never-looked-up");
        assert!(
            sql.contains("00000000-0000-0000-0000-000000000000"),
            "{sql}"
        );
    }

    // -- Free text -----------------------------------------------------------

    #[test]
    fn free_text_matches_message_exception_fields_and_the_payload() {
        let sql = lower_occ_sql("boom");
        assert!(
            sql.contains(r#""error_events"."message" ILIKE $1"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."exception_type" ILIKE $2"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."exception_value" ILIKE $3"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."contexts"::text ILIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."extra"::text ILIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."tags"::text ILIKE"#),
            "{sql}"
        );
    }

    /// The D4 invariant, restated for this lowerer: what you may SEARCH is
    /// exactly what you may READ BACK. Under `ShellOnly` the three columns
    /// `strip_event_body` nulls must be absent from the predicate entirely —
    /// not merely unlikely to match.
    #[test]
    fn free_text_omits_the_payload_scan_without_event_read() {
        let sql = lower_occ_sql_reach("boom", TextSearchReach::ShellOnly);
        // The readable half survives — the search is narrowed, never refused.
        assert!(
            sql.contains(r#""error_events"."message" ILIKE $1"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."exception_type" ILIKE $2"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""error_events"."exception_value" ILIKE $3"#),
            "{sql}"
        );
        for withheld in ["contexts", "extra", "tags"] {
            assert!(
                !sql.contains(&format!(r#""error_events"."{withheld}"::text"#)),
                "`{withheld}` is nulled by strip_event_body for this caller and must not be \
                 searchable: {sql}"
            );
        }
    }

    // -- workflow: a real column here, an EXISTS on Issues -------------------

    #[test]
    fn workflow_is_the_real_column_not_an_exists() {
        let sql = lower_occ_sql("workflow:checkout");
        assert!(
            sql.contains(r#""error_events"."workflow_name" = $1"#),
            "{sql}"
        );
        assert!(!sql.contains("EXISTS"), "{sql}");
    }

    /// The semantics `error_events_for_issue_query`'s hand-written
    /// `("workflow", Op::Neq)` arm exists for: a bare `<>` drops every
    /// unstamped row, so the same chip would mean two opposite things on the
    /// issues list and on that issue's occurrences.
    #[test]
    fn negated_workflow_keeps_unstamped_occurrences() {
        let sql = lower_occ_sql("!workflow:checkout");
        assert!(
            sql.contains(r#""error_events"."workflow_name" IS NULL"#),
            "{sql}"
        );
    }

    #[test]
    fn workflow_contains_uses_ilike() {
        let sql = lower_occ_sql("workflow:~check");
        assert!(
            sql.contains(r#""error_events"."workflow_name" ILIKE $1"#),
            "{sql}"
        );
    }

    /// The whole reason the catalog entry was widened to Occurrences: without
    /// it, `resolve_field`'s step-4 fallback reads the bare field `workflow`
    /// as a TAG KEY, and every `filter=workflow:…` bookmark on this route
    /// silently probes `error_events.tags` instead. No error — a different
    /// answer, which is worse.
    #[test]
    fn workflow_is_not_reinterpreted_as_a_tag_key() {
        let sql = lower_occ_sql("workflow:checkout");
        // The three TAG OPERATORS, not the bare column name: `tags` is in the
        // SELECT list of every query over this table, so `contains("tags")`
        // alone is true no matter what the predicate does — an assertion that
        // could never fail. (It didn't: this test failed on its first run and
        // that is how the flaw was found.)
        for tag_op in [
            r#""error_events"."tags" @>"#,
            r#""error_events"."tags" ?"#,
            r#""error_events"."tags" ->>"#,
        ] {
            assert!(
                !sql.contains(tag_op),
                "workflow must not fall through to the tag store ({tag_op}): {sql}"
            );
        }
    }

    // -- Composition sanity ---------------------------------------------------

    #[test]
    fn a_predicate_and_free_text_combine() {
        let sql = lower_occ_sql("level:error boom");
        assert!(sql.contains(r#""error_events"."level" = $1"#), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }
}
