//! `SessionsLower`: turns one `ResolvedPredicate` (or free-text term) resolved
//! against `Resource::Sessions` into a diesel boxed fragment over `sessions::table`.

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Double, Nullable, Text};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TypedValue};

use crate::query_plan::{
    json_path_segments, nest_json_object, Frag, PlanError, PrepCtx, ResourceLower,
};
use crate::repo::like_contains;
use crate::schema::sessions;

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

fn as_int(v: &TypedValue, field: &str) -> Result<i64, PlanError> {
    match v {
        TypedValue::Int(n) => Ok(*n),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_int_list(v: &TypedValue, field: &str) -> Result<Vec<i64>, PlanError> {
    match v {
        TypedValue::List(items) => items.iter().map(|i| as_int(i, field)).collect(),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_duration_ms(v: &TypedValue, field: &str) -> Result<i64, PlanError> {
    match v {
        TypedValue::DurationMs(d) => Ok(*d),
        TypedValue::Int(n) => Ok(*n),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_time(
    v: &TypedValue,
    field: &str,
    ctx: &PrepCtx,
) -> Result<chrono::DateTime<chrono::Utc>, PlanError> {
    match v {
        TypedValue::Time(sauron_query::TimeSpec::Absolute(dt)) => Ok(*dt),
        TypedValue::Time(sauron_query::TimeSpec::RelativeSeconds(secs)) => {
            Ok(ctx.now - chrono::Duration::seconds(*secs))
        }
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn resolve_environment(ctx: &PrepCtx, name: &str) -> Uuid {
    ctx.environments
        .get(name)
        .copied()
        .flatten()
        .unwrap_or(Uuid::nil())
}

macro_rules! str_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pat = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.not_ilike(pat).or($col.is_null()).nullable())
                        as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ilike(pat).nullable()) as Frag<sessions::table>)
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

macro_rules! int_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ne(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Gt => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.le(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Gte => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Lt => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Lte => {
                let v = as_int(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.le(v).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_int_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<sessions::table>)
                }
            }
            // Presence, matching `str_leaf!`/`environment_leaf!` above. The
            // catalog declares `Has` for these counters, so refusing it here
            // made `has:eventsCount` a 400 on a field the schema advertises.
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            }),
        }
    }};
}

macro_rules! time_leaf {
    ($col:expr, $ctx:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.ne(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq(t).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Ne => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.eq(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ne(t).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Gt => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.le(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.gt(t).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Gte => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.lt(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ge(t).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Lt => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.ge(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.lt(t).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Lte => {
                let t = as_time(&$p.value, field, $ctx)?;
                if $negate {
                    Ok(Box::new($col.gt(t).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.le(t).nullable()) as Frag<sessions::table>)
                }
            }
            // Presence, same as the issues copy of this macro — a timestamp
            // column can be NULL, so `has:` is a real question about it.
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<sessions::table>)
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

macro_rules! environment_leaf {
    ($col:expr, $ctx:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Ne => {
                let name = as_str(&$p.value, field)?;
                let id = resolve_environment($ctx, name);
                if $negate {
                    Ok(Box::new($col.eq(id).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.ne(id).or($col.is_null()).nullable())
                        as Frag<sessions::table>)
                }
            }
            MatchOp::In => {
                let names = as_str_list(&$p.value, field)?;
                let ids: Vec<Uuid> = names.iter().map(|n| resolve_environment($ctx, n)).collect();
                if $negate {
                    Ok(Box::new($col.ne_all(ids).or($col.is_null()).nullable())
                        as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.eq_any(ids).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<sessions::table>)
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

fn duration_leaf(p: &ResolvedPredicate, negate: bool) -> Result<Frag<sessions::table>, PlanError> {
    let field = p.dim.name;
    // `sql()` yields a non-Copy `SqlLiteral`, and the arms that also test for
    // NULL need two independent copies of it — so mint one per use.
    let expr = || {
        sql::<Nullable<Double>>(
            "EXTRACT(EPOCH FROM (sessions.last_event_at - sessions.started_at))",
        )
    };

    // `has:duration` asks whether the session has a duration AT ALL. There is
    // no value to parse, so it has to be answered before `as_duration_ms` —
    // which would otherwise reject the empty value as malformed.
    if matches!(p.op, MatchOp::Has) {
        return Ok(if negate {
            Box::new(expr().is_null().nullable()) as Frag<sessions::table>
        } else {
            Box::new(expr().is_not_null().nullable()) as Frag<sessions::table>
        });
    }

    let ms = as_duration_ms(&p.value, field)?;
    let sec = ms as f64 / 1000.0;
    match (p.op, negate) {
        (MatchOp::Eq, false) | (MatchOp::Ne, true) => {
            Ok(Box::new(expr().eq(sec)) as Frag<sessions::table>)
        }
        (MatchOp::Eq, true) | (MatchOp::Ne, false) => {
            Ok(Box::new(expr().ne(sec).or(expr().is_null())) as Frag<sessions::table>)
        }
        (MatchOp::Gt, false) | (MatchOp::Lte, true) => {
            Ok(Box::new(expr().gt(sec)) as Frag<sessions::table>)
        }
        (MatchOp::Gt, true) | (MatchOp::Lte, false) => {
            Ok(Box::new(expr().le(sec).or(expr().is_null())) as Frag<sessions::table>)
        }
        // These two arms also cover (Lt, false) / (Lt, true) — a later duplicate
        // pair spelled them out again with different NULL handling, which was
        // dead code the compiler could never reach.
        (MatchOp::Gte, false) | (MatchOp::Lt, true) => {
            Ok(Box::new(expr().ge(sec)) as Frag<sessions::table>)
        }
        (MatchOp::Gte, true) | (MatchOp::Lt, false) => {
            Ok(Box::new(expr().lt(sec).or(expr().is_null())) as Frag<sessions::table>)
        }
        _ => Err(PlanError::UnsupportedOnResource {
            field: field.to_string(),
        }),
    }
}

macro_rules! json_object_leaf {
    ($col:expr, $col_sql:literal, $prefix:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        // `has:<root>` with no path asks whether the row carries the object at
        // all — column presence, not a path lookup. `json_path_segments` cannot
        // express that (no path and an empty prefix yields no segments, so it
        // returns None), so it has to be answered before segments exist.
        if $p.path.is_none() && $prefix.is_empty() && matches!($p.op, MatchOp::Has) {
            return Ok(if $negate {
                Box::new($col.is_null().nullable()) as Frag<sessions::table>
            } else {
                Box::new($col.is_not_null().nullable()) as Frag<sessions::table>
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
                    ) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                let obj = nest_json_object(&segments, serde_json::Value::String(v));
                if $negate {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<sessions::table>)
                } else {
                    Ok(Box::new(
                        diesel::dsl::not($col.contains(obj))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<sessions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                let mut vs = vs.into_iter();
                let first = vs.next().ok_or_else(|| PlanError::BadValue {
                    field: field.to_string(),
                })?;
                let mut positive: Frag<sessions::table> = Box::new(
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
                            as Frag<sessions::table>,
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
                    ) as Frag<sessions::table>)
                } else {
                    Ok(Box::new($col.has_key(key).nullable()) as Frag<sessions::table>)
                }
            }
            MatchOp::Has => {
                let jsonpath = format!("$.{}", segments.join("."));
                let positive: Frag<sessions::table> = Box::new(
                    sql::<Nullable<Bool>>(concat!("\"sessions\".\"", $col_sql, "\" @? "))
                        .bind::<Text, _>(jsonpath)
                        .sql("::jsonpath"),
                );
                if $negate {
                    Ok(
                        Box::new(diesel::dsl::not(positive).or($col.is_null()).nullable())
                            as Frag<sessions::table>,
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
                    ) as Frag<sessions::table>)
                } else {
                    Ok(Box::new(
                        $col.retrieve_by_path_as_text(segments.clone())
                            .ilike(pattern)
                            .nullable(),
                    ) as Frag<sessions::table>)
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

pub struct SessionsLower {
    pub app_id: Uuid,
}

impl SessionsLower {
    pub fn base_scope(&self) -> Frag<sessions::table> {
        Box::new(sessions::app_id.eq(self.app_id).nullable())
    }
}

impl ResourceLower for SessionsLower {
    type Table = sessions::table;

    fn leaf(
        &self,
        p: &ResolvedPredicate,
        ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<sessions::table>, PlanError> {
        match p.dim.store {
            Store::Rollup => Err(PlanError::NotYetSupported {
                field: p.dim.name.to_string(),
            }),
            Store::Tag => Err(PlanError::UnsupportedOnResource {
                field: p.dim.name.to_string(),
            }),
            Store::JsonRoot {
                column: "context",
                prefix,
            } => {
                json_object_leaf!(sessions::context, "context", prefix, p, negate)
            }
            Store::JsonRoot { column: other, .. } => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
            Store::Column("started_at") => time_leaf!(sessions::started_at, ctx, p, negate),
            Store::Column("session_id") => str_leaf!(sessions::session_id, p, negate),
            Store::Column("distinct_id") => str_leaf!(sessions::distinct_id, p, negate),
            Store::Column("device_key") => str_leaf!(sessions::device_key, p, negate),
            Store::Column("release") => str_leaf!(sessions::release, p, negate),
            Store::Column("events_count") => int_leaf!(sessions::events_count, p, negate),
            Store::Column("errors_count") => int_leaf!(sessions::errors_count, p, negate),
            Store::Column("duration_ms") => duration_leaf(p, negate),
            Store::Column("environment_id") => {
                environment_leaf!(sessions::environment_id, ctx, p, negate)
            }
            Store::Column(other) => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
        }
    }

    fn text(&self, term: &str) -> Frag<sessions::table> {
        let pattern = like_contains(term);
        let session_id: Frag<sessions::table> =
            Box::new(sessions::session_id.ilike(pattern.clone()).nullable());
        let distinct_id: Frag<sessions::table> =
            Box::new(sessions::distinct_id.ilike(pattern.clone()).nullable());
        let device_key: Frag<sessions::table> =
            Box::new(sessions::device_key.ilike(pattern.clone()).nullable());
        let release: Frag<sessions::table> =
            Box::new(sessions::release.ilike(pattern.clone()).nullable());
        let context: Frag<sessions::table> = Box::new(
            sql::<Nullable<Bool>>(r#""sessions"."context"::text ILIKE "#).bind::<Text, _>(pattern),
        );
        Box::new(
            session_id
                .or(distinct_id)
                .or(device_key)
                .or(release)
                .or(context),
        )
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

    fn lower_sessions_sql(q: &str) -> String {
        let node = resolve(&parse(q).unwrap(), Resource::Sessions).unwrap();
        let l = SessionsLower {
            app_id: Uuid::nil(),
        };
        let frag = lower(&node, &l, &ctx()).unwrap();
        let query = sessions::table
            .into_boxed()
            .filter(frag)
            .select(sessions::id);
        debug_query::<Pg, _>(&query).to_string()
    }

    #[test]
    fn lowers_session_predicates() {
        let sql = lower_sessions_sql("distinctId:user_123");
        assert!(sql.contains(r#""sessions"."distinct_id" ="#), "{sql}");
    }

    #[test]
    fn lowers_session_context_json_predicate() {
        let sql = lower_sessions_sql("context.app_version:3.0.2");
        assert!(sql.contains(r#""sessions"."context" @>"#), "{sql}");
    }

    #[test]
    fn lowers_session_events_count_predicate() {
        let sql = lower_sessions_sql("eventsCount:>5");
        assert!(sql.contains(r#""sessions"."events_count" >"#), "{sql}");
    }
}
