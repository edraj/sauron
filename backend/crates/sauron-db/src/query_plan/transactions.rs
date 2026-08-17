//! `TransactionsLower`: turns one `ResolvedPredicate` (or free-text term)
//! resolved against `Resource::Transactions` into a diesel boxed fragment over
//! `transactions::table`.
//!
//! The catalog declares nine dimensions here — `name`, `op`, `duration`, `url`,
//! `http.status`, `http.method`, `extra`, `tag` and `$label` — which is a much
//! smaller surface than the event resources carry. There is deliberately no
//! `contexts`: named context blocks are an error-debugging affordance, and a
//! span that wants structure nests it inside `extra` (migration 0063).
//!
//! `text_reach` is not decoration. `strip_transaction_body` NULLS `tags` and
//! `extra` for a caller without `event:read`, and answering "does this column
//! contain this substring?" over a column the same response withholds is the
//! byte-at-a-time oracle `TextSearchReach` exists to close. The shell columns
//! this lowerer scans unconditionally (`name`, `url`) are exactly the ones
//! `strip_transaction_body` keeps.

use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{Bool, Nullable, Text};
use uuid::Uuid;

use sauron_query::{MatchOp, ResolvedPredicate, Store, TypedValue};

use crate::query_plan::{
    json_path_segments, nest_json_object, Frag, PlanError, PrepCtx, ResourceLower,
};
use crate::repo::{like_contains, TextSearchReach};
use crate::schema::transactions;

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

/// `transactions.http_status` is `INTEGER`, but the query language parses every
/// integer as `i64`. An out-of-range literal is a `BadValue`, not a silent
/// truncation: `http.status:4294967496` must not quietly become `200`.
fn as_i32(v: &TypedValue, field: &str) -> Result<i32, PlanError> {
    match v {
        TypedValue::Int(n) => i32::try_from(*n).map_err(|_| PlanError::BadValue {
            field: field.to_string(),
        }),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

fn as_i32_list(v: &TypedValue, field: &str) -> Result<Vec<i32>, PlanError> {
    match v {
        TypedValue::List(items) => items.iter().map(|i| as_i32(i, field)).collect(),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

/// The duration a predicate asks about, in MILLISECONDS.
///
/// `SessionsLower`'s namesake divides by 1000 because its expression is
/// `EXTRACT(EPOCH FROM …)`, which yields seconds. **This column is already
/// milliseconds** (`transactions.duration_ms`), so the value goes straight
/// through. Copying the `/ 1000.0` across would make `duration:>2s` match
/// everything over 2ms.
fn as_duration_ms(v: &TypedValue, field: &str) -> Result<i64, PlanError> {
    match v {
        TypedValue::DurationMs(d) => Ok(*d),
        TypedValue::Int(n) => Ok(*n),
        _ => Err(PlanError::BadValue {
            field: field.to_string(),
        }),
    }
}

macro_rules! str_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => {
                let pat = as_pattern(&$p.value, field)?.to_string();
                if $negate {
                    Ok(Box::new($col.not_ilike(pat).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.ilike(pat).nullable()) as Frag<transactions::table>)
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

/// `http.status` — a NULLABLE `INTEGER`.
///
/// Every negated arm carries `OR col IS NULL` for the reason the whole codebase
/// repeats: SQL three-valued logic makes a bare `col <> 500` drop every row
/// where the column is NULL, so `!http.status:500` would silently exclude every
/// non-HTTP span rather than including it. A navigation transaction is very
/// much "not a 500".
macro_rules! int_leaf {
    ($col:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        match $p.op {
            MatchOp::Eq => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.eq(v).nullable()) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.ne(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                }
            }
            MatchOp::Gt => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.le(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.gt(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Gte => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.lt(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.ge(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Lt => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ge(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.lt(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Lte => {
                let v = as_i32(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.gt(v).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.le(v).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_i32_list(&$p.value, field)?;
                if $negate {
                    Ok(Box::new($col.ne_all(vs).or($col.is_null()).nullable())
                        as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.eq_any(vs).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Has => {
                if $negate {
                    Ok(Box::new($col.is_null().nullable()) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.is_not_null().nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Like | MatchOp::Contains => Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            }),
        }
    }};
}

/// `duration` — the real `duration_ms` DOUBLE PRECISION column, NOT NULL.
///
/// Not a macro because there is exactly one such column and the ms/seconds unit
/// question (see [`as_duration_ms`]) deserves to be stated once, in a place a
/// reader lands on rather than scrolls past.
///
/// NOT NULL is why no arm carries `OR col IS NULL`: unlike `http.status`, every
/// transaction has a duration. `has:duration` is therefore constant-true, and
/// says so rather than emitting a predicate that reads as though it filtered.
fn duration_leaf(
    p: &ResolvedPredicate,
    negate: bool,
) -> Result<Frag<transactions::table>, PlanError> {
    let field = p.dim.name;
    let col = transactions::duration_ms;

    if matches!(p.op, MatchOp::Has) {
        return Ok(if negate {
            Box::new(sql::<Nullable<Bool>>("FALSE")) as Frag<transactions::table>
        } else {
            Box::new(sql::<Nullable<Bool>>("TRUE")) as Frag<transactions::table>
        });
    }

    let ms = as_duration_ms(&p.value, field)? as f64;
    match (p.op, negate) {
        (MatchOp::Eq, false) | (MatchOp::Ne, true) => {
            Ok(Box::new(col.eq(ms).nullable()) as Frag<transactions::table>)
        }
        (MatchOp::Eq, true) | (MatchOp::Ne, false) => {
            Ok(Box::new(col.ne(ms).nullable()) as Frag<transactions::table>)
        }
        (MatchOp::Gt, false) | (MatchOp::Lte, true) => {
            Ok(Box::new(col.gt(ms).nullable()) as Frag<transactions::table>)
        }
        (MatchOp::Lte, false) | (MatchOp::Gt, true) => {
            Ok(Box::new(col.le(ms).nullable()) as Frag<transactions::table>)
        }
        (MatchOp::Gte, false) | (MatchOp::Lt, true) => {
            Ok(Box::new(col.ge(ms).nullable()) as Frag<transactions::table>)
        }
        (MatchOp::Lt, false) | (MatchOp::Gte, true) => {
            Ok(Box::new(col.lt(ms).nullable()) as Frag<transactions::table>)
        }
        // `OPS_ORD` grants no text ops, and `Has` returned above; kept explicit
        // rather than as a catch-all so a widened op list forces a decision
        // here instead of silently 500ing.
        (MatchOp::In, _) | (MatchOp::Like, _) | (MatchOp::Contains, _) | (MatchOp::Has, _) => {
            Err(PlanError::UnsupportedOnResource {
                field: field.to_string(),
            })
        }
    }
}

macro_rules! json_object_leaf {
    ($col:expr, $col_sql:literal, $prefix:expr, $p:expr, $negate:expr) => {{
        let field = $p.dim.name;
        // `has:extra` with no path asks whether the row carries the object at
        // all — column presence, not a path lookup.
        if $p.path.is_none() && $prefix.is_empty() && matches!($p.op, MatchOp::Has) {
            return Ok(if $negate {
                Box::new($col.is_null().nullable()) as Frag<transactions::table>
            } else {
                Box::new($col.is_not_null().nullable()) as Frag<transactions::table>
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
                    ) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Ne => {
                let v = as_str(&$p.value, field)?.to_string();
                let obj = nest_json_object(&segments, serde_json::Value::String(v));
                if $negate {
                    Ok(Box::new($col.contains(obj).nullable()) as Frag<transactions::table>)
                } else {
                    Ok(Box::new(
                        diesel::dsl::not($col.contains(obj))
                            .or($col.is_null())
                            .nullable(),
                    ) as Frag<transactions::table>)
                }
            }
            MatchOp::In => {
                let vs = as_str_list(&$p.value, field)?;
                let mut vs = vs.into_iter();
                let first = vs.next().ok_or_else(|| PlanError::BadValue {
                    field: field.to_string(),
                })?;
                let mut positive: Frag<transactions::table> = Box::new(
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
                            as Frag<transactions::table>,
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
                    ) as Frag<transactions::table>)
                } else {
                    Ok(Box::new($col.has_key(key).nullable()) as Frag<transactions::table>)
                }
            }
            MatchOp::Has => {
                let jsonpath = format!("$.{}", segments.join("."));
                let positive: Frag<transactions::table> = Box::new(
                    sql::<Nullable<Bool>>(concat!("\"transactions\".\"", $col_sql, "\" @? "))
                        .bind::<Text, _>(jsonpath)
                        .sql("::jsonpath"),
                );
                if $negate {
                    Ok(
                        Box::new(diesel::dsl::not(positive).or($col.is_null()).nullable())
                            as Frag<transactions::table>,
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
                    ) as Frag<transactions::table>)
                } else {
                    Ok(Box::new(
                        $col.retrieve_by_path_as_text(segments.clone())
                            .ilike(pattern)
                            .nullable(),
                    ) as Frag<transactions::table>)
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

/// One `jsonb_each_text` scan comparing every tag VALUE with `op` (` = ` or
/// ` ILIKE `). `op` is a fixed literal chosen by the caller, never user input.
fn tag_any_cmp(op: &'static str, value: &str) -> Frag<transactions::table> {
    Box::new(
        sql::<Nullable<Bool>>(
            "EXISTS (SELECT 1 FROM jsonb_each_text(\"transactions\".\"tags\") kv \
             WHERE kv.value",
        )
        .sql(op)
        .bind::<Text, _>(value.to_string())
        .sql(")"),
    )
}

/// `tag:<value>` with no key — the same predicate across every key of `tags`.
/// Kept separate from the keyed path so that one can go on using the `@>`
/// containment index (`transactions_tags_gin`).
fn tag_any_leaf(
    p: &ResolvedPredicate,
    negate: bool,
) -> Result<Frag<transactions::table>, PlanError> {
    let positive: Frag<transactions::table> = match p.op {
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
            let mut acc: Frag<transactions::table> = tag_any_cmp(" = ", &first);
            for v in values {
                acc = Box::new(acc.or(tag_any_cmp(" = ", &v)));
            }
            acc
        }
        MatchOp::Has => Box::new(sql::<Nullable<Bool>>(
            "\"transactions\".\"tags\" <> '{}'::jsonb",
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

fn tag_leaf(p: &ResolvedPredicate, negate: bool) -> Result<Frag<transactions::table>, PlanError> {
    // No key named (`tag:value`, `@tag=value`) → match across EVERY tag key.
    let Some(key) = p.path.as_deref() else {
        return tag_any_leaf(p, negate);
    };
    let col = transactions::tags;
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
                ) as Frag<transactions::table>)
            } else {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<transactions::table>)
            }
        }
        MatchOp::Ne => {
            let value = as_str(&p.value, key)?;
            let obj = nest_json_object(
                &[key.to_string()],
                serde_json::Value::String(value.to_string()),
            );
            if negate {
                Ok(Box::new(col.contains(obj).nullable()) as Frag<transactions::table>)
            } else {
                Ok(Box::new(
                    diesel::dsl::not(col.contains(obj))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<transactions::table>)
            }
        }
        MatchOp::In => {
            let values = as_str_list(&p.value, key)?;
            let mut values = values.into_iter();
            let first = values.next().ok_or_else(|| PlanError::BadValue {
                field: key.to_string(),
            })?;
            let mut positive: Frag<transactions::table> = Box::new(
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
                        as Frag<transactions::table>,
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
                ) as Frag<transactions::table>)
            } else {
                Ok(Box::new(col.has_key(key.to_string()).nullable()) as Frag<transactions::table>)
            }
        }
        MatchOp::Like | MatchOp::Contains => {
            let pattern = as_pattern(&p.value, key)?.to_string();
            if negate {
                Ok(Box::new(
                    diesel::dsl::not(col.retrieve_as_text(key.to_string()).ilike(pattern))
                        .or(col.is_null())
                        .nullable(),
                ) as Frag<transactions::table>)
            } else {
                Ok(Box::new(
                    col.retrieve_as_text(key.to_string())
                        .ilike(pattern)
                        .nullable(),
                ) as Frag<transactions::table>)
            }
        }
        // No ordering comparison is declared for `TAG_DIM`'s ops; kept only for
        // match exhaustiveness, matching the other lowerers.
        MatchOp::Gt | MatchOp::Gte | MatchOp::Lt | MatchOp::Lte => {
            Err(PlanError::UnsupportedOnResource {
                field: key.to_string(),
            })
        }
    }
}

/// Lowers predicates resolved against `Resource::Transactions`.
pub struct TransactionsLower {
    pub app_id: Uuid,
    /// Whether a free-text term may reach `tags`/`extra` — the two columns
    /// `strip_transaction_body` NULLS. See this module's header.
    pub text_reach: TextSearchReach,
}

impl TransactionsLower {
    /// Base scope: the tenant boundary, and nothing else.
    ///
    /// Unlike `EventsLower`, there is no synthetic-row exclusion to carry —
    /// every row in `transactions` is a transaction a developer asked for.
    pub fn base_scope(&self) -> Frag<transactions::table> {
        Box::new(transactions::app_id.eq(self.app_id).nullable())
    }
}

impl ResourceLower for TransactionsLower {
    type Table = transactions::table;

    fn leaf(
        &self,
        p: &ResolvedPredicate,
        _ctx: &PrepCtx,
        negate: bool,
    ) -> Result<Frag<transactions::table>, PlanError> {
        match p.dim.store {
            // No `Store::Rollup` dimension is declared for Transactions today;
            // kept for exhaustiveness in case the catalog ever grows one.
            Store::Rollup => Err(PlanError::NotYetSupported {
                field: p.dim.name.to_string(),
            }),
            Store::Tag => tag_leaf(p, negate),
            Store::JsonRoot {
                column: "extra",
                prefix,
            } => {
                json_object_leaf!(transactions::extra, "extra", prefix, p, negate)
            }
            // `contexts` is deliberately not declared for this resource — see
            // the module header. Reaching here means the catalog grew a root
            // this lowerer has no column for, which is a 400, not a 500.
            Store::JsonRoot { column: other, .. } => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
            Store::Column("name") => str_leaf!(transactions::name, p, negate),
            Store::Column("op") => str_leaf!(transactions::op, p, negate),
            // Both are indexed on this table
            // (`transactions_app_session_idx`, `transactions_app_distinct_idx`),
            // which is what lets the Transactions list's Session column be
            // filtered and not merely read.
            Store::Column("session_id") => str_leaf!(transactions::session_id, p, negate),
            Store::Column("distinct_id") => str_leaf!(transactions::distinct_id, p, negate),
            Store::Column("url") => str_leaf!(transactions::url, p, negate),
            Store::Column("http_method") => str_leaf!(transactions::http_method, p, negate),
            Store::Column("http_status") => int_leaf!(transactions::http_status, p, negate),
            Store::Column("duration_ms") => duration_leaf(p, negate),
            Store::Column(other) => Err(PlanError::UnsupportedOnResource {
                field: other.to_string(),
            }),
        }
    }

    fn text(&self, term: &str) -> Frag<transactions::table> {
        let pattern = like_contains(term);
        // The shell: `name` and `url` are exactly the text columns
        // `strip_transaction_body` KEEPS, so every row matched here can be read
        // back in full by the caller who matched it.
        let name: Frag<transactions::table> =
            Box::new(transactions::name.ilike(pattern.clone()).nullable());
        let url: Frag<transactions::table> =
            Box::new(transactions::url.ilike(pattern.clone()).nullable());
        let shell: Frag<transactions::table> = Box::new(name.or(url));
        // The withheld half. `tags`/`extra` are the two columns
        // `strip_transaction_body` NULLS, and matching them for a caller who
        // will receive them as `null` answers a question the response is
        // forbidden to — one substring at a time. Request and response bodies
        // are precisely the class of data that rule exists for.
        if !self.text_reach.includes_body() {
            return shell;
        }
        let payload: Frag<transactions::table> = Box::new(
            sql::<Nullable<Bool>>(r#""transactions"."tags"::text ILIKE "#)
                .bind::<Text, _>(pattern.clone())
                .sql(r#" OR "transactions"."extra"::text ILIKE "#)
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

    fn lower_tx_result(
        q: &str,
        reach: TextSearchReach,
    ) -> Result<Frag<transactions::table>, PlanError> {
        let node = resolve(&parse(q).unwrap(), Resource::Transactions).unwrap();
        let l = TransactionsLower {
            app_id: Uuid::nil(),
            text_reach: reach,
        };
        lower(&node, &l, &ctx())
    }

    fn lower_tx_sql(q: &str) -> String {
        lower_tx_sql_reach(q, TextSearchReach::IncludingBody)
    }

    /// **The WHERE clause only.** `debug_query` renders the full statement, and
    /// `Transaction::as_select()`'s column list names `tags` and `extra` on
    /// every query — so a naive `sql.contains("extra")` passes for a predicate
    /// that never mentions the column, which is precisely backwards for the
    /// reach assertions below.
    fn lower_tx_sql_reach(q: &str, reach: TextSearchReach) -> String {
        let frag = lower_tx_result(q, reach).unwrap();
        let full = debug_query::<Pg, _>(&transactions::table.filter(frag)).to_string();
        full.split_once(" WHERE ")
            .expect("a filtered query always has a WHERE")
            .1
            .to_string()
    }

    #[test]
    fn extra_path_lowers_to_containment() {
        let sql = lower_tx_sql("extra.order_id:abc123");
        assert!(sql.contains("extra"), "{sql}");
        assert!(sql.contains("@>"), "{sql}");
    }

    #[test]
    fn tag_key_lowers_to_containment_on_transactions_tags() {
        let sql = lower_tx_sql("@tag.tier:premium");
        assert!(sql.contains("tags"), "{sql}");
        assert!(sql.contains("@>"), "{sql}");
    }

    /// The unit bug this module's `as_duration_ms` doc warns about: the column
    /// is already milliseconds, so `duration:>2s` must bind 2000, not 2.
    #[test]
    fn duration_binds_milliseconds_not_seconds() {
        let sql = lower_tx_sql("duration:>2s");
        assert!(sql.contains("2000"), "expected ms, got: {sql}");
        assert!(!sql.contains("$1 = 2.0"), "seconds leaked in: {sql}");
    }

    /// `http.status` is nullable, so a negated comparison must keep the rows
    /// that have no status at all — a navigation span IS "not a 500".
    #[test]
    fn negated_http_status_keeps_null_rows() {
        let sql = lower_tx_sql("!http.status:500");
        assert!(sql.contains("IS NULL"), "{sql}");
    }

    /// `duration_ms` is NOT NULL, so the same rule must NOT be copied there —
    /// an `OR IS NULL` on a NOT NULL column is dead weight that reads as though
    /// it filtered.
    #[test]
    fn negated_duration_has_no_null_arm() {
        let sql = lower_tx_sql("!duration:>2s");
        assert!(!sql.contains("IS NULL"), "{sql}");
    }

    /// The invariant: a caller who cannot read `tags`/`extra` cannot probe them
    /// either. Without this, `?q=sk_live_a`, `?q=sk_live_ab`, … spells a secret
    /// out of the row counts one byte at a time.
    #[test]
    fn shell_only_reach_omits_the_payload_scan() {
        let sql = lower_tx_sql_reach("boom", TextSearchReach::ShellOnly);
        assert!(sql.contains("name"), "{sql}");
        assert!(!sql.contains("extra"), "extra leaked into ShellOnly: {sql}");
        assert!(!sql.contains("tags"), "tags leaked into ShellOnly: {sql}");
    }

    #[test]
    fn including_body_reach_scans_tags_and_extra() {
        let sql = lower_tx_sql("boom");
        assert!(sql.contains("extra"), "{sql}");
        assert!(sql.contains("tags"), "{sql}");
    }

    /// The Session column on the Transactions list is filterable, not just
    /// readable — a column you can see and cannot narrow on is the first thing
    /// somebody tries and the first thing that disappoints them.
    #[test]
    fn session_and_user_resolve_and_lower_on_transactions() {
        for q in [
            "session:sess_1",
            "session_id:sess_1",
            "distinctId:u_1",
            "distinct_id:u_1",
        ] {
            let sql = lower_tx_sql(q);
            assert!(
                sql.contains("session_id") || sql.contains("distinct_id"),
                "{q} did not reach a column: {sql}"
            );
        }
    }

    /// `contexts` is not declared for this resource, so it must fail at
    /// RESOLUTION — an unknown field, not a silent zero-row answer.
    #[test]
    fn contexts_is_not_a_transaction_field() {
        assert!(resolve(
            &parse("contexts.order.id:7").unwrap(),
            Resource::Transactions
        )
        .is_err());
    }
}
