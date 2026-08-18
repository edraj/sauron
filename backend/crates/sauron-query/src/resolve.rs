//! Semantic resolution: raw `Node` → `ResolvedNode`, where every field is a
//! `&'static Dimension` from the catalog and every value is typed.
//!
//! This is the security boundary. After `resolve`, no caller-supplied bytes are
//! ever used as a SQL identifier — `dim.store` supplies every column and path
//! name from a `&'static str`, and values travel as typed binds. It mirrors the
//! guarantee `sauron_db::filter::parse_filters` already provides today.

use chrono::{DateTime, Utc};

use crate::ast::{MatchOp, Node, Predicate};
use crate::catalog::{
    label_dimension, lookup, tag_dimension, Dimension, IndexClass, Resource, Store, ValueType,
    SHORTHANDS,
};
use crate::token::is_field_ident;
use crate::QueryError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSpec {
    /// Seconds *before now*, resolved against the clock at query time.
    RelativeSeconds(i64),
    /// Calendar months *before now*, resolved against the clock at query time.
    ///
    /// Its own variant rather than a second count because a month is not one:
    /// "one month before 31 March" is 28 or 29 February, not 1 or 2 March, and
    /// no fixed number of seconds produces that for every starting date. The
    /// lowerers use `chrono::Months`, which clamps to the end of the shorter
    /// month — the same rule every calendar UI uses.
    RelativeMonths(i64),
    Absolute(DateTime<Utc>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Str(String),
    /// A `LIKE` pattern: the user's `*` has become `%`, and any literal
    /// `%`/`_`/`\` is already escaped.
    Pattern(String),
    Int(i64),
    Bool(bool),
    DurationMs(i64),
    Time(TimeSpec),
    List(Vec<TypedValue>),
    /// `has:` — the predicate is about presence, so there is no value.
    Absent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPredicate {
    pub dim: &'static Dimension,
    /// Path inside a JSONB column, or the tag key for `Store::Tag`. `None` for
    /// plain columns and rollups.
    pub path: Option<String>,
    pub op: MatchOp,
    pub value: TypedValue,
    pub at: usize,
    /// Effective index class for THIS resource, which can be worse than
    /// `dim.index`. A tag filter is GIN-backed on Occurrences and Events, but on
    /// Issues it lowers to a correlated `EXISTS` into `error_events` (the issues
    /// table has no `tags` column), so it is a per-candidate-row subplan.
    pub index: IndexClass,
}

/// `dim.index` is the best case. Downgrade it where the storage for THIS
/// resource cannot deliver that.
fn effective_index(dim: &'static Dimension, r: Resource) -> IndexClass {
    match dim.store {
        // No `tags` column on `issues` — this becomes a correlated EXISTS.
        Store::Tag if r == Resource::Issues => IndexClass::Bounded,
        // The `issue_dimensions` rollup table does not exist yet.
        Store::Rollup => IndexClass::Bounded,
        _other => dim.index,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedNode {
    And(Vec<ResolvedNode>),
    Or(Vec<ResolvedNode>),
    Not(Box<ResolvedNode>),
    Pred(ResolvedPredicate),
    /// Free-text search term, **not** escaped and **not** wrapped in wildcards —
    /// unlike `Contains`/`Like`, which arrive as a ready `TypedValue::Pattern`.
    /// The planner owns escaping this, because it decides which columns the term
    /// is matched against. Forgetting to escape it makes `100%` match every row.
    Text(String),
}

pub fn resolve(node: &Node, r: Resource) -> Result<ResolvedNode, QueryError> {
    Ok(match node {
        Node::And(v) => ResolvedNode::And(
            v.iter()
                .map(|n| resolve(n, r))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Node::Or(v) => ResolvedNode::Or(
            v.iter()
                .map(|n| resolve(n, r))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Node::Not(b) => ResolvedNode::Not(Box::new(resolve(b, r)?)),
        Node::Text(t) => ResolvedNode::Text(t.clone()),
        Node::Pred(p) => resolve_pred_node(p, r)?,
    })
}

/// One `Node::Pred` → one or TWO resolved predicates.
///
/// Everything resolves to a single predicate except a range, `field:[lo..hi]`,
/// which becomes `field:>=lo AND field:<=hi`. Expanding here rather than
/// carrying a `Between` operator all the way down is deliberate: a new
/// `MatchOp` would need a lowering arm in every `query_plan` module and a cost
/// rule of its own, and would still end up emitting exactly this pair of
/// comparisons. Two predicates the planner already knows how to index beat one
/// it has to learn.
fn resolve_pred_node(p: &Predicate, r: Resource) -> Result<ResolvedNode, QueryError> {
    if let Some(node) = resolve_range(p, r)? {
        return Ok(node);
    }
    Ok(ResolvedNode::Pred(resolve_pred(p, r, false)?))
}

/// `field:[lo..hi]` — an INCLUSIVE range, or `None` when `p` is not one.
///
/// Shares the bracket syntax with the `[a,b]` any-of list and is told apart by
/// the `..` separator, which cannot appear in a list (a list splits on commas,
/// and `2026-01-01..2026-02-01` as a single list item was already a hard error).
/// So this adds a spelling rather than reinterpreting one that used to work.
///
/// Gated on the dimension advertising BOTH `>=` and `<=`, which is what
/// confines it to the ordered types — timestamps, integers, durations. On a
/// string or enum field the brackets keep meaning "any of".
///
/// Both ends are required. A half-open `[lo..]` is deliberately rejected: it is
/// spelled `>=lo`, and accepting a second spelling for it would mean deciding
/// whether the missing end is unbounded or a typo, which only the author knows.
fn resolve_range(p: &Predicate, r: Resource) -> Result<Option<ResolvedNode>, QueryError> {
    // A quoted value is literal — `firstSeen:"[a..b]"` is asking for that text.
    if p.quoted {
        return Ok(None);
    }
    let (op, inner) = split_op(&p.value, false);
    if op != MatchOp::In {
        return Ok(None);
    }
    let Some((lo, hi)) = inner.split_once("..") else {
        return Ok(None);
    };
    let (lo, hi) = (lo.trim(), hi.trim());

    // Resolve the field before complaining about the ends, so an unknown field
    // is reported as an unknown field rather than as a bad range.
    let Ok((dim, path)) = resolve_field(&p.field, r, p.at) else {
        return Ok(None);
    };
    if !dim.ops.contains(&MatchOp::Gte) || !dim.ops.contains(&MatchOp::Lte) {
        return Ok(None);
    }
    if lo.is_empty() || hi.is_empty() {
        return Err(QueryError::BadValue {
            field: p.field.clone(),
            value: p.value.clone(),
            at: p.at,
        });
    }

    let index = effective_index(dim, r);
    let bound = |op: MatchOp, raw: &str| -> Result<ResolvedNode, QueryError> {
        Ok(ResolvedNode::Pred(ResolvedPredicate {
            dim,
            path: path.clone(),
            op,
            value: type_value(dim, op, raw, &p.field, p.at)?,
            at: p.at,
            index,
        }))
    };
    Ok(Some(ResolvedNode::And(vec![
        bound(MatchOp::Gte, lo)?,
        bound(MatchOp::Lte, hi)?,
    ])))
}

fn resolve_pred(
    p: &Predicate,
    r: Resource,
    expanded: bool,
) -> Result<ResolvedPredicate, QueryError> {
    // `has:field` — the VALUE names the field, and there is no value of its own.
    if p.field == "has" && !expanded {
        // Every other route into `path` draws from `Predicate.field`, which the
        // lexer already constrained via `is_field_ident`. This is the one place
        // the operand comes from `Predicate.value` instead, so it must be
        // constrained here too — otherwise a JSONB path built as `#>> '{a,b}'`
        // could receive caller-controlled `,`/`}`/`"` metacharacters.
        if !is_field_ident(&p.value) {
            return Err(QueryError::UnknownField {
                field: p.value.clone(),
                at: p.at,
                resource: Some(r),
            });
        }
        let (dim, path) = resolve_field(&p.value, r, p.at)?;
        if !dim.ops.contains(&MatchOp::Has) {
            return Err(QueryError::BadOp {
                field: p.value.clone(),
                at: p.at,
            });
        }
        return Ok(ResolvedPredicate {
            dim,
            path,
            op: MatchOp::Has,
            value: TypedValue::Absent,
            at: p.at,
            index: effective_index(dim, r),
        });
    }

    // `is:<keyword>` expands to a real predicate. `expanded` breaks the recursion
    // for status shorthands, whose target field is also called `is`.
    //
    // Only a bare equality is a shorthand: `is:>unresolved` carries a comparison
    // operator, so it falls through to the catalog `is` dimension and is rejected
    // there as a bad operator — which is the accurate complaint, rather than
    // claiming `>unresolved` is an unknown shorthand.
    if p.field == "is" && !expanded && split_op(&p.value, p.quoted).0 == MatchOp::Eq {
        let sh = SHORTHANDS
            .iter()
            .find(|s| s.keyword == p.value)
            .ok_or_else(|| QueryError::BadShorthand {
                value: p.value.clone(),
                at: p.at,
            })?;
        return resolve_pred(
            &Predicate {
                field: sh.field.to_string(),
                value: sh.value.to_string(),
                // Quoted so the expansion is never re-interpreted as a wildcard
                // or a comparison.
                quoted: true,
                at: p.at,
            },
            r,
            true,
        );
    }

    // `tag:<key>=<value>` or `label:<key>=<value>` — the escape hatch for tag/label keys
    // that are not legal identifiers or carry explicit key=value syntax.
    let clean_field = p.field.strip_prefix('@').unwrap_or(&p.field);
    if (clean_field == "tag" || clean_field == "$label" || clean_field == "label") && !expanded {
        if let Some((key, rest)) = p.value.split_once('=') {
            // An EXPLICIT but empty key (`tag:=value`) is malformed — distinct
            // from omitting the key entirely (`tag:value`), which step 4 of
            // `resolve_field` reads as "any key".
            if key.is_empty() {
                return Err(QueryError::BadValue {
                    field: p.field.clone(),
                    value: p.value.clone(),
                    at: p.at,
                });
            }
            {
                let dim = if clean_field == "tag" {
                    tag_dimension(r)
                } else {
                    label_dimension(r)
                }
                .ok_or_else(|| QueryError::UnknownField {
                    field: p.field.clone(),
                    at: p.at,
                    resource: Some(r),
                })?;
                let (op, value_src) = split_op(rest, p.quoted);
                if !dim.ops.contains(&op) {
                    return Err(QueryError::BadOp {
                        field: p.field.clone(),
                        at: p.at,
                    });
                }
                let value = type_value(dim, op, value_src, &p.field, p.at)?;
                return Ok(ResolvedPredicate {
                    dim,
                    path: Some(key.to_string()),
                    op,
                    value,
                    at: p.at,
                    index: effective_index(dim, r),
                });
            }
        }
    }

    let (op, rest) = split_op(&p.value, p.quoted);
    let (dim, path) = resolve_field(&p.field, r, p.at)?;
    if !dim.ops.contains(&op) {
        return Err(QueryError::BadOp {
            field: p.field.clone(),
            at: p.at,
        });
    }
    let value = type_value(dim, op, rest, &p.field, p.at)?;
    Ok(ResolvedPredicate {
        dim,
        path,
        op,
        value,
        at: p.at,
        index: effective_index(dim, r),
    })
}

/// Field resolution: exact catalog match, then the explicit `tag.<key>` or `$label.<key>` prefix,
/// then a dotted path under a JSON root. Anything else is an error.
fn resolve_field(
    field: &str,
    r: Resource,
    at: usize,
) -> Result<(&'static Dimension, Option<String>), QueryError> {
    let clean = field.strip_prefix('@').unwrap_or(field);

    // 1. Exact match. Covers plain names (`level`), dotted names that are
    //    real columns (`http.status`), and registered JSON roots (`context`, `extra`).
    if let Some(d) = lookup(clean, r) {
        return Ok((d, None));
    }

    // 2. Explicit tag disambiguation (`tag.<key>`).
    if let Some(key) = clean.strip_prefix("tag.") {
        if !key.is_empty() {
            if let Some(d) = tag_dimension(r) {
                return Ok((d, Some(key.to_string())));
            }
        }
    }

    // 3. Explicit label disambiguation (`$label.<key>` or `label.<key>`).
    let label_key = clean
        .strip_prefix("$label.")
        .or_else(|| clean.strip_prefix("label."));
    if let Some(key) = label_key {
        if !key.is_empty() {
            if let Some(d) = label_dimension(r) {
                return Ok((d, Some(key.to_string())));
            }
        }
    }

    // 4. Standalone `@tag` or `tag` (without property dot). No key was named,
    //    so this matches across EVERY tag key — `path: None` is what the
    //    lowering reads as "any key". It must not be `Some("tag")`: that would
    //    silently mean "the tag whose key is literally `tag`", which is a
    //    different (and almost always empty) query.
    if clean == "tag" {
        if let Some(d) = tag_dimension(r) {
            return Ok((d, None));
        }
    }

    // 5. Standalone `@$label` / `$label` / `label` — "any label key", as above.
    if clean == "$label" || clean == "label" {
        if let Some(d) = label_dimension(r) {
            return Ok((d, None));
        }
    }

    // 6. Dotted path under a JSON root.
    if let Some((root, remainder)) = clean.split_once('.') {
        if let Some(d) = lookup(root, r) {
            if let Store::JsonRoot { prefix, .. } = d.store {
                let path = if prefix.is_empty() {
                    remainder.to_string()
                } else {
                    format!("{prefix}.{remainder}")
                };
                return Ok((d, Some(path)));
            }
        }
    }

    Err(QueryError::UnknownField {
        field: field.to_string(),
        at,
        resource: Some(r),
    })
}

/// Peel a comparison prefix, list brackets, or a wildcard off the raw value.
/// Quoting suppresses all of it, which is the only way to search for a literal
/// `>` or `*`.
fn split_op(raw: &str, quoted: bool) -> (MatchOp, &str) {
    if quoted {
        return (MatchOp::Eq, raw);
    }
    // `~` is checked FIRST and returns immediately, so everything after it is
    // literal — including a `*`, a leading `>`, or another `~`. That is the whole
    // point of the operator: it is the one way to say "this exact text appears
    // somewhere in the field" without the value being reinterpreted.
    if let Some(r) = raw.strip_prefix('~') {
        return (MatchOp::Contains, r);
    }
    if let Some(r) = raw.strip_prefix(">=") {
        return (MatchOp::Gte, r);
    }
    if let Some(r) = raw.strip_prefix("<=") {
        return (MatchOp::Lte, r);
    }
    if let Some(r) = raw.strip_prefix('>') {
        return (MatchOp::Gt, r);
    }
    if let Some(r) = raw.strip_prefix('<') {
        return (MatchOp::Lt, r);
    }
    if raw.len() >= 2 && raw.starts_with('[') && raw.ends_with(']') {
        return (MatchOp::In, &raw[1..raw.len() - 1]);
    }
    if raw.contains('*') {
        return (MatchOp::Like, raw);
    }
    (MatchOp::Eq, raw)
}

fn type_value(
    dim: &'static Dimension,
    op: MatchOp,
    raw: &str,
    field: &str,
    at: usize,
) -> Result<TypedValue, QueryError> {
    match op {
        MatchOp::Has => Ok(TypedValue::Absent),
        MatchOp::In => {
            let items = raw
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| type_value(dim, MatchOp::Eq, s, field, at))
                .collect::<Result<Vec<_>, _>>()?;
            if items.is_empty() {
                return Err(QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                });
            }
            Ok(TypedValue::List(items))
        }
        MatchOp::Like => Ok(TypedValue::Pattern(to_like_pattern(raw))),
        // Literal substring: escape the LIKE metacharacters and wrap in `%`
        // ourselves. `*` is NOT translated, so it stays a literal asterisk.
        MatchOp::Contains => Ok(TypedValue::Pattern(format!("%{}%", escape_like(raw)))),
        _ => match dim.ty {
            ValueType::Str => Ok(TypedValue::Str(raw.to_string())),
            ValueType::Enum(opts) => {
                if opts.contains(&raw) {
                    Ok(TypedValue::Str(raw.to_string()))
                } else {
                    Err(QueryError::BadEnum {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
                }
            }
            ValueType::Int => {
                raw.parse::<i64>()
                    .map(TypedValue::Int)
                    .map_err(|_| QueryError::BadValue {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
            }
            ValueType::Bool => match raw {
                "true" => Ok(TypedValue::Bool(true)),
                "false" => Ok(TypedValue::Bool(false)),
                _ => Err(QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }),
            },
            ValueType::Duration => parse_duration_ms(raw)
                .map(TypedValue::DurationMs)
                .ok_or_else(|| QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }),
            ValueType::Timestamp => {
                parse_time(raw)
                    .map(TypedValue::Time)
                    .ok_or_else(|| QueryError::BadValue {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
            }
        },
    }
}

/// Escape SQL `LIKE` metacharacters only. Used by `MatchOp::Contains`, where the
/// whole value is literal. Mirrors `sauron_db::repo::escape_like`.
fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '%' => out.push_str(r"\%"),
            '_' => out.push_str(r"\_"),
            c => out.push(c),
        }
    }
    out
}

/// Escape SQL `LIKE` metacharacters, then promote the user's `*` to `%`.
/// Order matters: escaping first means a literal `%` in the input cannot be
/// confused with the wildcard we are about to introduce.
fn to_like_pattern(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '%' => out.push_str(r"\%"),
            '_' => out.push_str(r"\_"),
            '*' => out.push('%'),
            c => out.push(c),
        }
    }
    out
}

fn parse_duration_ms(raw: &str) -> Option<i64> {
    let (num, mult) = if let Some(n) = raw.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = raw.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = raw.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = raw.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (raw, 1)
    };
    // `checked_mul`, not `*`: `duration:>3000000000000h` parses as a valid i64 but
    // overflows when scaled. Unchecked, that panics in debug builds and silently
    // wraps to a negative duration in release — from an HTTP query parameter.
    num.trim()
        .parse::<i64>()
        .ok()
        .and_then(|v| v.checked_mul(mult))
}

/// Relative-time unit suffixes and their length in seconds.
///
/// **Ordered longest-suffix-first, and the order is load-bearing.** Matching is
/// "first suffix that fits", so a short unit listed early would swallow a long
/// one that ends with the same letter: `1month` ends in `h`, `2days` ends in
/// `s`, `5mins` ends in `s`. With `h`/`s` checked first those parse as hours
/// and seconds of `1mont` and `2day` — which then fail to parse as integers, so
/// the bug surfaces as "bad value" on a spelling the docs advertise, not as a
/// wrong answer. Keep new units in length order.
///
/// `week` is exactly 7 days. **`month` is NOT in this table** — it is calendar
/// arithmetic, handled by [`MONTH_UNITS`] below and carried as
/// [`TimeSpec::RelativeMonths`], because no fixed number of seconds gives the
/// right answer for every starting date.
const TIME_UNITS: &[(&str, i64)] = &[
    ("seconds", 1),
    ("minutes", 60),
    ("minute", 60),
    ("second", 1),
    ("hours", 3_600),
    ("weeks", 604_800),
    ("mins", 60),
    ("secs", 1),
    ("days", 86_400),
    ("hour", 3_600),
    ("week", 604_800),
    ("day", 86_400),
    ("min", 60),
    ("sec", 1),
    ("hrs", 3_600),
    ("hr", 3_600),
    ("d", 86_400),
    ("h", 3_600),
    ("m", 60),
    ("s", 1),
    ("w", 604_800),
];

/// The calendar-month spellings, checked BEFORE [`TIME_UNITS`].
///
/// Order matters between the two tables as much as within them: `1month` ends
/// in `h` and `1mo` in `o`, but `5m` must stay MINUTES. Checking the month
/// table first, and requiring at least `mo`, is what keeps `m` unambiguous —
/// there is no one-letter spelling of "month" and there must not be one.
const MONTH_UNITS: &[&str] = &["months", "month", "mos", "mo"];

fn parse_time(raw: &str) -> Option<TimeSpec> {
    // Relative, as a magnitude *before now*: `7d`, `-7d`, `24h`, `2day`,
    // `1month`. The leading `-` is optional and means nothing on its own — a
    // time filter reads backwards from now either way, and `lastSeen:>=1month`
    // is how people write it. It stays accepted because `-7d` was the only
    // spelling for a while and is in saved views.
    //
    // Tried BEFORE RFC3339 rather than after: an ISO timestamp contains no
    // bare-integer-plus-unit prefix, so the two cannot both match, and running
    // the cheap check first keeps the common case off the date parser.
    let rest = raw.strip_prefix('-').unwrap_or(raw);
    // Months first — see `MONTH_UNITS`.
    if let Some(num) = MONTH_UNITS.iter().find_map(|s| rest.strip_suffix(s)) {
        if num.is_empty() {
            return None;
        }
        return num.parse::<i64>().ok().map(TimeSpec::RelativeMonths);
    }
    if let Some((num, mult)) = TIME_UNITS
        .iter()
        .find_map(|(suffix, mult)| rest.strip_suffix(suffix).map(|n| (n, *mult)))
    {
        // Reject a bare unit (`d`, `month`) rather than reading it as 1: it is
        // far more likely a truncated `7d` than a deliberate "one day".
        if num.is_empty() {
            return None;
        }
        // See `parse_duration_ms` — the same overflow applies to `-<huge>d`.
        return num
            .parse::<i64>()
            .ok()
            .and_then(|v| v.checked_mul(mult))
            .map(TimeSpec::RelativeSeconds);
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| TimeSpec::Absolute(dt.with_timezone(&Utc)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn one(q: &str, r: Resource) -> ResolvedPredicate {
        match resolve(&parse(q).unwrap(), r).unwrap() {
            ResolvedNode::Pred(p) => p,
            other => panic!("expected a single predicate, got {other:?}"),
        }
    }

    fn err(q: &str, r: Resource) -> QueryError {
        resolve(&parse(q).unwrap(), r).unwrap_err()
    }

    #[test]
    fn resolves_a_curated_field() {
        let p = one("level:error", Resource::Issues);
        assert_eq!(p.dim.name, "level");
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("error".into()));
        assert_eq!(p.path, None);
    }

    #[test]
    fn unknown_field_is_rejected_rather_than_read_as_a_tag() {
        // **The ruling this test exists to pin.** An unrecognised name used to
        // resolve to a TAG KEY, so `checkout_stpe:payment` answered 200 with
        // zero rows — indistinguishable from an honest "nothing matched", and
        // the typo stayed invisible. `tag.<key>` (below) remains the explicit
        // spelling, so no capability is lost.
        let e = err("checkout_step:payment", Resource::Issues);
        assert!(matches!(e, QueryError::UnknownField { .. }), "{e:?}");
    }

    #[test]
    fn unknown_field_is_rejected_on_every_taggable_resource() {
        // Issues, Occurrences and Events all carry a `tags` column, and all
        // three therefore used to swallow a typo. One vocabulary, one
        // resolution: the answer must not depend on which list you are on.
        for r in [Resource::Issues, Resource::Occurrences, Resource::Events] {
            let e = err("checkout_step:payment", r);
            assert!(matches!(e, QueryError::UnknownField { .. }), "{r:?}: {e:?}");
        }
    }

    #[test]
    fn unknown_field_message_names_the_field_and_the_tag_spelling() {
        // A 400 that only says "unknown field" trades one bad experience for
        // another: it must name the field AND say what to write instead.
        let msg = err("checkout_step:payment", Resource::Issues).to_string();
        assert!(msg.contains("checkout_step"), "must name the field: {msg}");
        assert!(
            msg.contains("tag.checkout_step"),
            "must show the tag spelling: {msg}"
        );
        assert!(
            msg.contains("culprit") && msg.contains("timesSeen"),
            "must list what IS available on this resource: {msg}"
        );
        // Fields belonging to some OTHER resource are not offered.
        assert!(
            !msg.contains("os.version"),
            "must not advertise another resource's fields: {msg}"
        );
    }

    #[test]
    fn unknown_field_message_offers_the_escape_hatch_for_a_non_identifier_key() {
        // `tag.<key>` cannot spell a key the lexer would not accept as an
        // identifier, so recommending `tag.a b` would be advice that does not
        // work. The `tag:<key>=<value>` escape hatch is what does.
        let msg = err(r#"has:"cart@checkout""#, Resource::Issues).to_string();
        assert!(msg.contains("tag:<key>=<value>"), "{msg}");
        assert!(!msg.contains("tag.cart@checkout"), "{msg}");
    }

    #[test]
    fn explicit_tag_prefix_disambiguates() {
        // `tag.level` must reach the TAG store, not the curated `level` column.
        let p = one("tag.level:custom", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("level"));
    }

    #[test]
    fn explicit_tag_prefix_is_the_surviving_way_to_reach_a_dev_tag() {
        // The capability step 4 used to provide, spelled explicitly. Same
        // dimension, same path, same value — only the spelling is now required.
        let p = one("tag.checkout_step:payment", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("checkout_step"));
        assert_eq!(p.value, TypedValue::Str("payment".into()));
    }

    #[test]
    fn unknown_field_errors_where_tags_do_not_exist() {
        assert!(matches!(
            err("nonsense:1", Resource::Devices),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn unknown_field_message_does_not_offer_tags_where_there_are_none() {
        // Devices/Persons/Sessions have no `tags` column. Telling that caller
        // to write `tag.nonsense` would be a lie.
        //
        // Transactions was in this list until migration 0063 gave it one; it
        // now belongs to the opposite assertion below, which is the whole
        // reason that assertion exists rather than this list simply shrinking.
        for r in [Resource::Devices, Resource::Persons, Resource::Sessions] {
            let msg = err("nonsense:1", r).to_string();
            assert!(msg.contains("nonsense"), "{r:?}: {msg}");
            assert!(!msg.contains("tag."), "{r:?} has no tags column: {msg}");
            assert!(
                !msg.contains("tag:<key>"),
                "{r:?} has no tags column: {msg}"
            );
        }
        assert!(
            err("nonsense:1", Resource::Devices)
                .to_string()
                .contains("os.version"),
            "still lists what IS available"
        );
    }

    /// The mirror of the test above: a resource that DOES carry `tags` must
    /// offer the `tag.` spelling, or the hint is missing exactly where it would
    /// have helped.
    #[test]
    fn unknown_field_message_offers_tags_where_they_exist() {
        for r in [
            Resource::Issues,
            Resource::Occurrences,
            Resource::Events,
            Resource::Transactions,
        ] {
            let msg = err("nonsense:1", r).to_string();
            assert!(msg.contains("tag.nonsense"), "{r:?}: {msg}");
        }
    }

    #[test]
    fn unknown_field_keeps_its_offset_in_the_query_string() {
        // The caret the UI draws. Step 4 meant this offset was never exercised
        // on a taggable resource; now it is the common case.
        let e = err("level:error checkout_step:payment", Resource::Issues);
        assert!(matches!(e, QueryError::UnknownField { .. }), "{e:?}");
        assert_eq!(e.at(), 12);
    }

    /// Every field name the dashboard can put in the `field` slot of
    /// `filter=<field>:<op>:<value>`, per its three closed registries in
    /// `dashboard/src/lib/components/filters/filters.ts`. Removing step 4 turns
    /// any name missing from the catalog into a 400, so a UI chip that no
    /// longer resolves is a broken page — caught here rather than in a browser.
    #[test]
    fn every_field_the_dashboard_can_emit_still_resolves() {
        let cases: &[(Resource, &[&str])] = &[
            // ISSUE_FIELDS
            (
                Resource::Issues,
                &[
                    "level",
                    "status",
                    "type",
                    "culprit",
                    "times_seen",
                    "users_seen",
                ],
            ),
            // EVENT_FIELDS (+ `environment`, still reachable from a bookmark)
            (
                Resource::Events,
                &[
                    "name",
                    "distinct_id",
                    "session_id",
                    "release",
                    "environment",
                ],
            ),
            // OCCURRENCE_FIELDS carries only `tag`, covered below.
            (Resource::Occurrences, &[]),
        ];
        for (r, fields) in cases {
            for f in *fields {
                assert!(
                    lookup(f, *r).is_some(),
                    "the dashboard emits `{f}` on {r:?} and it no longer resolves"
                );
            }
            // `workflow` is offered as a permission-gated chip on all three.
            assert!(lookup("workflow", *r).is_some(), "workflow on {r:?}");
            // The `tag` chip travels as `tag:<op>:<key>=<value>`, which
            // `from_legacy` rewrites to `tag.<key>` — step 2, not step 4.
            let p = one("tag.region:eu", *r);
            assert!(matches!(p.dim.store, Store::Tag), "tag chip on {r:?}");
        }
    }

    #[test]
    fn resolves_json_path_with_empty_prefix() {
        let p = one("extra.cartValue:100", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "extra",
                ..
            }
        ));
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn resolves_json_path_with_a_prefix() {
        // `os.name` lives at context->os->name, so the prefix is retained.
        let p = one("os.name:Windows", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "context",
                prefix: "os"
            }
        ));
        assert_eq!(p.path.as_deref(), Some("os.name"));
    }

    #[test]
    fn resolves_user_email_without_duplicating_the_root() {
        // The column IS the user object, so the path is just `email`.
        let p = one("user.email:a@b.com", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "event_user",
                prefix: ""
            }
        ));
        assert_eq!(p.path.as_deref(), Some("email"));
    }

    #[test]
    fn deep_json_paths_are_preserved() {
        let p = one("extra.cart.items.count:3", Resource::Occurrences);
        assert_eq!(p.path.as_deref(), Some("cart.items.count"));
    }

    #[test]
    fn comparison_prefixes_become_operators() {
        assert_eq!(one("timesSeen:>100", Resource::Issues).op, MatchOp::Gt);
        assert_eq!(one("timesSeen:>=100", Resource::Issues).op, MatchOp::Gte);
        assert_eq!(one("timesSeen:<100", Resource::Issues).op, MatchOp::Lt);
        assert_eq!(one("timesSeen:<=100", Resource::Issues).op, MatchOp::Lte);
        assert_eq!(
            one("timesSeen:>100", Resource::Issues).value,
            TypedValue::Int(100)
        );
    }

    #[test]
    fn list_syntax_becomes_in() {
        let p = one("level:[error,fatal]", Resource::Issues);
        assert_eq!(p.op, MatchOp::In);
        assert_eq!(
            p.value,
            TypedValue::List(vec![
                TypedValue::Str("error".into()),
                TypedValue::Str("fatal".into())
            ])
        );
    }

    #[test]
    fn star_becomes_a_pattern_with_like_metacharacters_escaped() {
        let p = one("user.email:*@acme.com", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Like);
        assert_eq!(p.value, TypedValue::Pattern("%@acme.com".into()));

        // A literal % or _ in the value must not become a wildcard.
        let p2 = one("culprit:*100%_done", Resource::Issues);
        assert_eq!(p2.value, TypedValue::Pattern(r"%100\%\_done".into()));
    }

    #[test]
    fn tilde_is_a_literal_substring_match() {
        let p = one("culprit:~handler", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%handler%".into()));
    }

    #[test]
    fn literal_substring_does_not_interpret_a_star() {
        // The whole reason this operator exists: `*` stays a literal asterisk.
        let p = one("culprit:~foo*bar", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%foo*bar%".into()));
    }

    #[test]
    fn literal_substring_still_escapes_like_metacharacters() {
        let p = one("culprit:~100%_done", Resource::Issues);
        assert_eq!(p.value, TypedValue::Pattern(r"%100\%\_done%".into()));
    }

    #[test]
    fn literal_substring_takes_precedence_over_comparison() {
        // `~` is stripped first and everything after is literal.
        let p = one("culprit:~>=v2", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%>=v2%".into()));
    }

    #[test]
    fn quoting_makes_a_star_literal() {
        let p = one(r#"culprit:"a*b""#, Resource::Issues);
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("a*b".into()));
    }

    #[test]
    fn quoting_disables_comparison_prefixes_too() {
        let p = one(r#"culprit:">100""#, Resource::Issues);
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str(">100".into()));
    }

    #[test]
    fn has_checks_key_existence() {
        let p = one("has:extra.cartValue", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Has);
        assert_eq!(p.value, TypedValue::Absent);
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn has_rejects_an_unknown_field_rather_than_probing_a_tag() {
        // `has:` resolves its operand through the same `resolve_field`, so the
        // ruling has to reach it too — otherwise `has:` becomes a back door to
        // the fallback that `field:value` just lost.
        let e = err("has:checkout_step", Resource::Issues);
        assert!(matches!(e, QueryError::UnknownField { .. }), "{e:?}");
    }

    #[test]
    fn has_works_on_an_explicit_tag_key() {
        let p = one("has:tag.checkout_step", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.op, MatchOp::Has);
        assert_eq!(p.path.as_deref(), Some("checkout_step"));
    }

    #[test]
    fn status_shorthands_expand_to_the_status_column() {
        let p = one("is:unresolved", Resource::Issues);
        assert_eq!(p.dim.name, "is");
        assert!(matches!(p.dim.store, Store::Column("status")));
        assert_eq!(p.value, TypedValue::Str("unresolved".into()));
    }

    #[test]
    fn handled_shorthands_expand_to_the_handled_field() {
        let p = one("is:unhandled", Resource::Occurrences);
        assert_eq!(p.dim.name, "handled");
        assert_eq!(p.value, TypedValue::Bool(false));
        assert_eq!(p.op, MatchOp::Eq);

        let p2 = one("is:handled", Resource::Occurrences);
        assert_eq!(p2.value, TypedValue::Bool(true));
    }

    #[test]
    fn unknown_shorthand_is_rejected_not_treated_as_a_tag() {
        assert!(matches!(
            err("is:banana", Resource::Issues),
            QueryError::BadShorthand { .. }
        ));
    }

    #[test]
    fn rejects_bad_enum_value() {
        assert!(matches!(
            err("level:banana", Resource::Issues),
            QueryError::BadEnum { .. }
        ));
    }

    #[test]
    fn rejects_non_numeric_for_int_fields() {
        assert!(matches!(
            err("timesSeen:>lots", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn rejects_disallowed_operator() {
        // `is` is an enum; ordering comparisons make no sense on it.
        assert!(matches!(
            err("is:>unresolved", Resource::Issues),
            QueryError::BadOp { .. }
        ));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(
            one("duration:>2s", Resource::Transactions).value,
            TypedValue::DurationMs(2000)
        );
        assert_eq!(
            one("duration:>500ms", Resource::Transactions).value,
            TypedValue::DurationMs(500)
        );
        assert_eq!(
            one("duration:>1m", Resource::Transactions).value,
            TypedValue::DurationMs(60_000)
        );
        // A bare number is milliseconds.
        assert_eq!(
            one("duration:>250", Resource::Transactions).value,
            TypedValue::DurationMs(250)
        );
    }

    #[test]
    fn rejects_duration_that_overflows_instead_of_panicking() {
        // Parses as a valid i64, then overflows when scaled to milliseconds.
        assert!(matches!(
            err("duration:>3000000000000h", Resource::Transactions),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn rejects_relative_time_that_overflows_instead_of_panicking() {
        assert!(matches!(
            err("firstSeen:>-9000000000000000d", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn parses_relative_timestamps() {
        assert_eq!(
            one("firstSeen:>-7d", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(7 * 86_400))
        );
        assert_eq!(
            one("firstSeen:>-24h", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(24 * 3600))
        );
    }

    #[test]
    fn the_leading_minus_is_optional() {
        // Both spellings mean the same magnitude before now. The signed one is
        // the original and lives in saved views, so it cannot stop working.
        assert_eq!(
            one("firstSeen:>7d", Resource::Issues).value,
            one("firstSeen:>-7d", Resource::Issues).value
        );
    }

    #[test]
    fn parses_the_long_unit_spellings() {
        let secs = |q: &str| match one(q, Resource::Issues).value {
            TypedValue::Time(TimeSpec::RelativeSeconds(n)) => n,
            other => panic!("expected relative seconds, got {other:?}"),
        };
        assert_eq!(secs("firstSeen:>45sec"), 45);
        assert_eq!(secs("firstSeen:>45seconds"), 45);
        assert_eq!(secs("firstSeen:>30min"), 30 * 60);
        assert_eq!(secs("firstSeen:>30minutes"), 30 * 60);
        assert_eq!(secs("firstSeen:>2hour"), 2 * 3_600);
        assert_eq!(secs("firstSeen:>2day"), 2 * 86_400);
        assert_eq!(secs("firstSeen:>2days"), 2 * 86_400);
        assert_eq!(secs("firstSeen:>3week"), 3 * 604_800);
    }

    #[test]
    fn months_are_calendar_months_not_a_fixed_span() {
        // Its own variant, deliberately: no number of seconds gives the right
        // answer for every starting date. `chrono::Months` at the lowerers
        // clamps to the end of a shorter month.
        for q in ["firstSeen:>1month", "firstSeen:>1mo", "firstSeen:>1months"] {
            assert_eq!(
                one(q, Resource::Issues).value,
                TypedValue::Time(TimeSpec::RelativeMonths(1)),
                "{q}"
            );
        }
        assert_eq!(
            one("firstSeen:>6months", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeMonths(6))
        );
    }

    #[test]
    fn m_is_minutes_and_there_is_no_one_letter_month() {
        // The reason months get their own table checked first. `5m` must not
        // drift into months, and no one-letter month spelling may exist.
        assert_eq!(
            one("firstSeen:>5m", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(5 * 60))
        );
        assert_eq!(
            one("firstSeen:>5min", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(5 * 60))
        );
    }

    #[test]
    fn a_long_unit_is_not_swallowed_by_a_short_one_it_ends_with() {
        // The whole reason `TIME_UNITS` is ordered by length. `1month` ends in
        // `h` and `2days` in `s`; matched shortest-first they would be read as
        // hours-of-`1mont` and seconds-of-`2day` and rejected as bad values.
        let secs = |q: &str| match one(q, Resource::Issues).value {
            TypedValue::Time(TimeSpec::RelativeSeconds(n)) => n,
            other => panic!("expected relative seconds, got {other:?}"),
        };
        assert_eq!(secs("firstSeen:>2days"), 2 * 86_400);
        assert_eq!(secs("firstSeen:>5mins"), 5 * 60);
        // And the short forms keep their old meaning: `m` is MINUTES, not
        // months, which is why `month` needs its own entry rather than a
        // prefix rule.
        assert_eq!(secs("firstSeen:>5m"), 5 * 60);
    }

    #[test]
    fn a_bare_unit_with_no_number_is_rejected() {
        // `firstSeen:>d` is a truncated `7d` far more often than it is a
        // deliberate "one day"; guessing 1 would answer a query nobody asked.
        assert!(matches!(
            err("firstSeen:>d", Resource::Issues),
            QueryError::BadValue { .. }
        ));
        assert!(matches!(
            err("firstSeen:>month", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn a_timestamp_range_becomes_two_inclusive_bounds() {
        let node = resolve(&parse("firstSeen:[7d..1d]").unwrap(), Resource::Issues).unwrap();
        let ResolvedNode::And(parts) = node else {
            panic!("expected an AND of two bounds, got {node:?}");
        };
        assert_eq!(parts.len(), 2);
        let [ResolvedNode::Pred(lo), ResolvedNode::Pred(hi)] = &parts[..] else {
            panic!("expected two predicates, got {parts:?}");
        };
        assert_eq!(lo.dim.name, "firstSeen");
        assert_eq!(lo.op, MatchOp::Gte);
        assert_eq!(
            lo.value,
            TypedValue::Time(TimeSpec::RelativeSeconds(7 * 86_400))
        );
        assert_eq!(hi.dim.name, "firstSeen");
        assert_eq!(hi.op, MatchOp::Lte);
        assert_eq!(
            hi.value,
            TypedValue::Time(TimeSpec::RelativeSeconds(86_400))
        );
    }

    #[test]
    fn a_range_accepts_absolute_instants_and_mixed_ends() {
        for q in [
            "firstSeen:[2026-07-01T00:00:00Z..2026-08-01T00:00:00Z]",
            "firstSeen:[1month..2026-08-01T00:00:00Z]",
        ] {
            let node = resolve(&parse(q).unwrap(), Resource::Issues).unwrap();
            assert!(
                matches!(node, ResolvedNode::And(ref v) if v.len() == 2),
                "{q}: {node:?}"
            );
        }
    }

    #[test]
    fn a_range_works_on_the_other_ordered_types() {
        // Nothing about the expansion is timestamp-specific — it is gated on the
        // dimension advertising both `>=` and `<=`.
        let node = resolve(&parse("timesSeen:[10..100]").unwrap(), Resource::Issues).unwrap();
        let ResolvedNode::And(parts) = node else {
            panic!("expected AND, got {node:?}")
        };
        let [ResolvedNode::Pred(lo), ResolvedNode::Pred(hi)] = &parts[..] else {
            panic!("expected two predicates")
        };
        assert_eq!((lo.op, &lo.value), (MatchOp::Gte, &TypedValue::Int(10)));
        assert_eq!((hi.op, &hi.value), (MatchOp::Lte, &TypedValue::Int(100)));
    }

    #[test]
    fn brackets_without_a_range_separator_still_mean_any_of() {
        // The two spellings share the brackets, so this is the test that keeps
        // the range from swallowing the list it lives beside.
        let p = one("level:[error,fatal]", Resource::Issues);
        assert_eq!(p.op, MatchOp::In);
        assert_eq!(
            p.value,
            TypedValue::List(vec![
                TypedValue::Str("error".into()),
                TypedValue::Str("fatal".into())
            ])
        );
    }

    #[test]
    fn a_range_on_an_unordered_field_is_not_a_range() {
        // `level` is an enum: no `>=`, so brackets keep meaning "any of" and
        // `error..fatal` is one nonsense list item, reported as a bad enum
        // rather than silently becoming a comparison.
        let e = err("level:[error..fatal]", Resource::Issues);
        assert!(matches!(e, QueryError::BadEnum { .. }), "{e:?}");
    }

    #[test]
    fn a_half_open_range_is_rejected_rather_than_guessed() {
        for q in ["firstSeen:[7d..]", "firstSeen:[..7d]"] {
            let e = err(q, Resource::Issues);
            assert!(matches!(e, QueryError::BadValue { .. }), "{q}: {e:?}");
        }
    }

    #[test]
    fn a_quoted_range_is_literal_text_not_a_range() {
        let node = resolve(&parse(r#"culprit:"[a..b]""#).unwrap(), Resource::Issues).unwrap();
        assert!(matches!(node, ResolvedNode::Pred(_)), "{node:?}");
    }

    #[test]
    fn parses_absolute_timestamps() {
        let v = one("firstSeen:>2026-07-01T00:00:00Z", Resource::Issues).value;
        assert!(matches!(v, TypedValue::Time(TimeSpec::Absolute(_))));
    }

    #[test]
    fn rejects_unparseable_timestamp() {
        assert!(matches!(
            err("firstSeen:>yesterday", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn preserves_tree_structure_and_free_text() {
        // Real field names, not placeholders: `a:1`/`b:2` only resolved because
        // an unknown name used to become a tag key.
        let got = resolve(
            &parse("level:error (culprit:a OR type:b) !is:resolved timeout").unwrap(),
            Resource::Issues,
        )
        .unwrap();
        match got {
            ResolvedNode::And(parts) => {
                assert_eq!(parts.len(), 4);
                assert!(matches!(parts[1], ResolvedNode::Or(_)));
                assert!(matches!(parts[2], ResolvedNode::Not(_)));
                assert!(matches!(parts[3], ResolvedNode::Text(_)));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn error_offsets_point_at_the_failing_term() {
        let e = err("level:error timesSeen:>lots", Resource::Issues);
        assert_eq!(e.at(), 12);
    }

    #[test]
    fn has_rejects_an_operand_that_is_not_an_identifier() {
        // A JSONB path is often emitted as a `#>> '{a,b}'` array literal, where
        // `,` `}` and `"` are metacharacters — the planner must never see them.
        assert!(matches!(
            err(r#"has:"extra.a,b}""#, Resource::Occurrences),
            QueryError::UnknownField { .. }
        ));
        assert!(matches!(
            err(r#"has:"a'; DROP TABLE users--""#, Resource::Issues),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn has_still_accepts_a_legal_dotted_path() {
        let p = one("has:extra.cartValue", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Has);
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn tag_escape_hatch_accepts_a_non_identifier_key() {
        // Tag keys are unconstrained on the write path, so the grammar must be
        // able to name any of them.
        let p = one("tag:cart@checkout=eu", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("cart@checkout"));
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("eu".into()));
    }

    #[test]
    fn tag_escape_hatch_composes_with_operators() {
        let p = one("tag:cart@checkout=~eu", Resource::Issues);
        assert_eq!(p.path.as_deref(), Some("cart@checkout"));
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%eu%".into()));

        let w = one("tag:100%off=*sale*", Resource::Issues);
        assert_eq!(w.path.as_deref(), Some("100%off"));
        assert_eq!(w.op, MatchOp::Like);
    }

    #[test]
    fn tag_escape_hatch_keeps_extra_equals_in_the_value() {
        let p = one("tag:expr=a=b", Resource::Issues);
        assert_eq!(p.path.as_deref(), Some("expr"));
        assert_eq!(p.value, TypedValue::Str("a=b".into()));
    }

    #[test]
    fn tag_without_a_key_searches_every_key() {
        // Omitting the key entirely is not an error: `tag:justakey` asks for
        // that value under ANY tag key. `path: None` is how the lowering is
        // told "every key" — see `tag_leaf`.
        let p = one("tag:justakey", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path, None);
        assert_eq!(p.value, TypedValue::Str("justakey".into()));

        // An explicitly EMPTY key is still malformed, though.
        assert!(matches!(
            err("tag:=novalue", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn tag_escape_hatch_is_rejected_where_tags_do_not_exist() {
        assert!(matches!(
            err("tag:a@b=v", Resource::Devices),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn browser_resolves_to_the_runtime_key_that_actually_exists() {
        // Enrichment writes the browser under `context->runtime`, not
        // `context->browser`. Pointing at the wrong key matches nothing forever.
        let p = one("browser.name:Chrome", Resource::Occurrences);
        assert_eq!(p.path.as_deref(), Some("runtime.name"));
        let alias = one("runtime.name:Chrome", Resource::Occurrences);
        assert_eq!(alias.path.as_deref(), Some("runtime.name"));
    }

    #[test]
    fn resolves_at_prefixed_variables_and_labels() {
        let tag_p = one("@tag.env=prod", Resource::Issues);
        assert!(matches!(tag_p.dim.store, Store::Tag));
        assert_eq!(tag_p.path.as_deref(), Some("env"));
        assert_eq!(tag_p.value, TypedValue::Str("prod".into()));

        let ctx_p = one("@context.app_version=3.0.2", Resource::Occurrences);
        assert!(matches!(
            ctx_p.dim.store,
            Store::JsonRoot {
                column: "context",
                ..
            }
        ));
        assert_eq!(ctx_p.path.as_deref(), Some("app_version"));
        assert_eq!(ctx_p.value, TypedValue::Str("3.0.2".into()));

        let extra_p = one("@extra.level=warn", Resource::Occurrences);
        assert!(matches!(
            extra_p.dim.store,
            Store::JsonRoot {
                column: "extra",
                ..
            }
        ));
        assert_eq!(extra_p.path.as_deref(), Some("level"));

        let label_p = one("@$label.team=backend", Resource::Issues);
        assert_eq!(label_p.dim.name, "$label");
        assert_eq!(label_p.path.as_deref(), Some("team"));
        assert_eq!(label_p.value, TypedValue::Str("backend".into()));
    }
}
