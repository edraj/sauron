//! The seam between the query crate, the planner, and axum.
//!
//! One module rather than three copies inside the handlers: the envelope shape,
//! the legacy bridge and the count policy are the parts most likely to drift
//! apart, and drift here is a client-visible inconsistency between two lists
//! that look identical.
//!
//! Task 3 carried a module-level `#![allow(dead_code)]` here, because
//! `sauron-api` is a bin crate with no `lib.rs` — `pub` alone does not exempt
//! an item from dead-code analysis when nothing outside this compilation unit
//! can call it, and nothing did. **Task 4 removed it**: `routes::issues::list`
//! now calls every item in this module, so the attribute is no longer load
//! bearing and leaving it would permanently mask genuinely dead code added
//! later. Tasks 5 and 6 wire the same items into two more handlers; nothing
//! here needs to become dead again first.

use std::collections::HashSet;

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::AuthUser;
use sauron_db::query_plan::prepare::Clamp;
use sauron_db::query_plan::PlanError;
use sauron_db::repo::TextSearchReach;
use sauron_query::{from_legacy, parse, resolve, Node, ResolvedNode, ResolvedPredicate, Store};

use crate::error::ApiError;
use crate::AppState;

/// Counting stops here when the plan degrades to a scan.
///
/// `total` stays a number and `total_is_capped` carries the nuance, so counting
/// never becomes the expensive part of the request.
pub const COUNT_CAP: i64 = 10_000;

#[derive(Debug, Serialize)]
pub struct ClampInfo {
    pub field: String,
    pub to: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct SearchEnvelope<T> {
    pub data: Vec<T>,
    pub total: i64,
    pub total_is_capped: bool,
    pub next_cursor: Option<String>,
    pub clamped: Option<ClampInfo>,
}

/// The time window a search actually ran over, and the disclosure owed for it.
#[derive(Debug)]
pub struct Window {
    pub since: DateTime<Utc>,
    pub clamped: Option<ClampInfo>,
}

/// Resolve the effective window from the three rules that can narrow it, and
/// describe the one that actually bound.
///
/// Every search route meets the same three: the caller's own `since_days`, the
/// route's own ceiling, and the planner's cost clamp. The window served is the
/// TIGHTEST of them — a clamp must only ever tighten, since one that relaxed an
/// explicit narrowing would return more rows than were asked for.
///
/// `clamped` must then name the window that was **served**, by the rule that
/// actually produced it. The three handlers previously reported
/// `prepared.clamp` unconditionally, which publishes a window the response does
/// not contain: `?since_days=7` on a scanning query serves seven days of rows
/// under `clamped: {"to": "30d"}`, because the caller's own seven days won the
/// `since` comparison while the envelope still described the planner's thirty.
/// A caption built from that field labels seven days of data "last 30 days" —
/// the same shape of confidently-wrong disclosure Task 7 removed from the
/// Events pager, one field over.
///
/// A route's own `max_days` is disclosed on identical terms, and for the same
/// reason. `analytics::events_list` bounds its window at 365 days over the
/// largest table in the system; a caller who asks for 3650 and is served 365
/// under `clamped: null` has been told their window was not narrowed, which is
/// false.
///
/// Only **narrowings** are disclosed. `since_days=0` is raised to 1, which
/// hands the caller more than they asked for rather than less, and `clamped`
/// exists to warn that rows may be missing.
pub fn resolve_window(
    field: &'static str,
    now: DateTime<Utc>,
    requested_days: i64,
    max_days: i64,
    planner: Option<Clamp>,
) -> Window {
    let mut days = requested_days.clamp(1, max_days);
    let mut reason = (requested_days > max_days)
        .then(|| format!("this view bounds its time window at {max_days} days"));

    // Strictly tighter, not `<=`: a planner clamp that merely matches the
    // window already in force changed nothing, and attributing the window to it
    // would credit the wrong rule in `reason`.
    if let Some(c) = planner {
        if c.to_days < days {
            days = c.to_days;
            reason = Some(c.reason.to_string());
        }
    }

    Window {
        since: now - Duration::days(days),
        clamped: reason.map(|reason| ClampInfo {
            // `Clamp.field` is the generic "since" — `prepare` does not know
            // which resource it ran for, so naming the physical column stays
            // the resource-aware caller's job and arrives here as `field`.
            field: field.to_string(),
            to: format!("{days}d"),
            reason,
        }),
    }
}

/// `query=` accepts either the string grammar (`level:error`) or a serialized
/// `Node` AST — the same JSON the dashboard's client-side parser builds, whose
/// wire shape `sauron-query`'s `ast_serde` test pins.
///
/// A value opening with `{` is JSON *by intent*, so a JSON parse failure is a
/// 400 rather than a silent fall back to the string grammar: `{"Pred":{invalid`
/// would otherwise lex as free text, match nothing, and return an empty 200 —
/// a malformed query reported as "no results".
fn parse_query_param(text: &str) -> Result<Node, ApiError> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str::<Node>(trimmed)
            .map_err(|e| ApiError::BadRequest(format!("invalid query AST: {e}")));
    }
    parse(text).map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// Which of the three input shapes the caller used.
///
/// `query=` wins outright when present. `filter=`/`q=` keep working and are
/// bridged into the same AST, so an existing bookmark returns the same rows —
/// that equivalence is what Task 4's test asserts.
pub fn resolve_query(
    query: Option<&str>,
    filter: &[String],
    q: Option<&str>,
    resource: sauron_query::Resource,
) -> Result<ResolvedNode, ApiError> {
    let ast = match query {
        Some(text) if !text.trim().is_empty() => parse_query_param(text)?,
        // `from_legacy` takes NO resource — it is a purely syntactic bridge and
        // produces the same untyped `Node` that `parse` does. Field validity is
        // decided one line down, by `resolve`, for both paths alike.
        _ => from_legacy(filter, q).map_err(|e| ApiError::BadRequest(e.to_string()))?,
    };
    resolve(&ast, resource).map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// Returns `(column, descending)`.
///
/// `allowed` is the set of orderings with a supporting `(…, id)` index. Anything
/// else is refused rather than served unstably.
pub fn parse_sort(
    raw: Option<&str>,
    allowed: &[&str],
    default: &str,
) -> Result<(String, bool), ApiError> {
    let spec = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default);
    let (col, descending) = match spec.strip_prefix('-') {
        Some(rest) => (rest, false),
        None => (spec, true),
    };
    if !allowed.contains(&col) {
        return Err(ApiError::BadRequest(format!(
            "cannot sort by `{col}`; this list supports {} (prefix with `-` to reverse). \
             Other columns need a matching index before they can be paged stably.",
            allowed.join(", ")
        )));
    }
    Ok((col.to_string(), descending))
}

/// Whether a free-text term appears anywhere in the resolved tree.
///
/// The honest input to `event_stats`' `payload_searched` flag once that route
/// is bridged (S2c Task 6): free text reaches the planner from BOTH spellings
/// — `?q=boom` and a bare `boom` term inside `?query=` — and `q.q.is_some()`
/// only sees the first. Reporting `null` ("no search ran") for a `query=boom`
/// that WAS narrowed would restate the "absent is not empty is not false"
/// mistake the flag's three states exist to avoid.
///
/// Walks `Not`/`And`/`Or` because free text nests: `!(boom level:error)` still
/// ran a payload scan.
pub fn has_free_text(node: &ResolvedNode) -> bool {
    match node {
        ResolvedNode::Text(_) => true,
        ResolvedNode::Pred(_) => false,
        ResolvedNode::Not(inner) => has_free_text(inner),
        ResolvedNode::And(v) | ResolvedNode::Or(v) => v.iter().any(has_free_text),
    }
}

/// A rejected plan is a bad request, not a server fault. `Database` is the one
/// genuine internal failure and must stay a 500 so it pages someone.
///
/// Every variant is matched explicitly rather than with a catch-all, so a
/// variant added to `PlanError` later forces a decision here instead of
/// silently defaulting to 400 — the wrong direction for anything that turns
/// out to be a server fault.
pub fn map_plan_error(e: PlanError) -> ApiError {
    match e {
        PlanError::Database(inner) => ApiError::Internal(inner),
        e @ PlanError::NotYetSupported { .. }
        | e @ PlanError::UnsupportedOnResource { .. }
        | e @ PlanError::BadValue { .. } => ApiError::BadRequest(e.to_string()),
    }
}

/// The JSON columns a `Store::JsonRoot` dimension may address without
/// `event:read` — the ONLY exceptions to [`reject_withheld_dimensions`]'
/// fail-closed default.
///
/// One entry, serving two dimensions: `properties` on Events (analytics event
/// properties) and `traits` on Persons both sit on a `properties` column.
/// Neither is an `error_events` column, so `strip_event_body` never touches
/// them and there is nothing withheld to probe.
///
/// Keyed on the column and not the dimension name deliberately: this list has
/// to agree with `symbolicate::strip_event_body`, and that function names
/// columns. A new dimension over `properties` needs no edit here; a new
/// dimension over a NEW column is refused until someone adds it and says why.
const NON_WITHHELD_JSON_COLUMNS: &[&str] = &["properties"];

/// Whether the caller may learn environment **names**.
///
/// A second, INDEPENDENT axis from [`TextSearchReach`], and the independence is
/// the point: `event:read` and `env:read` are different permissions, so a
/// caller holding `issue:read + event:read` — who sails past every check
/// [`TextSearchReach`] governs — may still hold no entitlement to environment
/// names at all. Folding this into the existing enum, or into
/// `reach.includes_body()`'s early return, would have left exactly that caller
/// able to enumerate them.
///
/// A two-variant enum rather than a `bool` for the reason [`TextSearchReach`]
/// is one: `true` and `false` are interchangeable at a call site and the
/// mistake is silent in the leaking direction. Distinct TYPES also mean the
/// compiler catches a transposed pair of arguments to
/// [`reject_withheld_dimensions`], which two `bool`s would not.
///
/// Lives here rather than beside `symbolicate::text_search_reach` because that
/// function only sits where it does to cross the `sauron-db` ↔ `sauron-auth`
/// boundary (`sauron-db` owns [`TextSearchReach`] and cannot depend on
/// `sauron-auth`). This enum is owned by `sauron-api`, which depends on both,
/// so the derivation belongs next to the definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvNameReach {
    /// The caller holds `env:read` at the resolved scope.
    Readable,
    /// They do not. An `environment:<name>` predicate is a name-existence
    /// oracle for them and must be refused.
    Withheld,
}

impl EnvNameReach {
    /// The permission set from `scope::authorized_read_scope_with_perms` — i.e.
    /// resolved AT the caller's environment scope, not app-wide. An env-scoped
    /// member's `env:read` counts, and only there.
    pub fn for_perms(perms: &HashSet<String>) -> Self {
        if perms.contains(sauron_auth::perm::ENV_READ) {
            EnvNameReach::Readable
        } else {
            EnvNameReach::Withheld
        }
    }

    fn may_read_names(self) -> bool {
        matches!(self, EnvNameReach::Readable)
    }
}

/// Refuse the predicates that are questions about a WITHHELD column.
///
/// The AST-level twin of `routes::issues::reject_body_filters`, which does the
/// same job for the pre-language `ParsedFilter` list. A route bridged onto the
/// query language needs this one instead: `from_legacy` turns
/// `filter=tag:eq:k=v` into exactly the same `Store::Tag` predicate that
/// `query=k:v` produces, so a check that only inspected the raw `filter=`
/// strings would be bypassed by the `query=` spelling of the same request.
///
/// **Two independent permission axes.** `reach` (`event:read`) governs the
/// three storage kinds below; `env_names` (`env:read`) governs the
/// `environment` dimension, which is a name-existence oracle in its own right
/// and is checked by [`reject_withheld_environment`] regardless of `reach`.
/// They are separate parameters because they are separate permissions — a
/// caller can hold either without the other.
///
/// Three storage kinds, and the reasoning is resource-independent — which is
/// why this lives in the shared seam rather than in one handler:
///
/// - **`Store::Tag`.** `symbolicate::strip_event_body` nulls `tags` for a
///   caller holding `issue:read` without `event:read`. A tag predicate asks
///   the database whether a row carries that exact tag, and `tag:~v` gives a
///   per-key ILIKE — a *sharper* oracle than `?q=`, which can only probe the
///   whole `contexts||extra||tags` blob.
/// - **`Store::JsonRoot`, fail-closed.** Every JSON root the catalog declares
///   except one is backed by a column `strip_event_body` nulls — `context`
///   (`os`/`browser`/`device`/`app`), `contexts`, `extra`, `event_user`
///   (`user`), `sdk`, `stacktrace` (`stack`). A dotted predicate such as
///   `os.name:Linux` or `extra.token:~sk_live_` is a containment/ILIKE probe
///   into exactly that withheld blob, and a *sharper* one than a tag filter
///   because it addresses a single nested key.
///
///   This has **no effect on Issues** — `R_ISSUES` declares no JSON root —
///   and is here because Tasks 5 and 6 bridge `issues::events` and
///   `issues::event_stats` onto Occurrences/Events, which authorize on
///   `issue:read` ALONE and where those roots are live. Adding the arm in the
///   linchpin is what stops the hole propagating into both.
///
///   The default is REFUSE and the exceptions are listed by column, not the
///   other way round: an opt-out list fails open for whatever a later slice
///   adds, which is the exact failure `dashboard/src/lib/api/scope.ts` records
///   for its own predecessor. [`NON_WITHHELD_JSON_COLUMNS`] is that list, and
///   it is keyed on the COLUMN because `strip_event_body` is too — a second
///   root over an already-listed column stays correct without an edit.
/// - **`workflow`.** Workflow names are served only by
///   `/v1/apps/{id}/workflows` and its siblings, all of which authorize on
///   `event:read`. A caller holding `issue:read` alone is not entitled to
///   learn them through *any* route, and `workflow:~x` hands them an ILIKE to
///   enumerate them one prefix at a time. The test is "which permission
///   governs this column", not "does this column appear in the body I strip".
///
/// **Refused, not silently dropped.** Dropping the predicate is what
/// [`TextSearchReach`] does to the `q` payload scan, and that is right there:
/// a free-text term is a request to *find* rows, so matching fewer columns
/// still answers it honestly. An explicit narrowing is the opposite —
/// ignoring it returns MORE rows than were asked for, every one of them under
/// a chip claiming they match it. A page that shows non-matching rows beside
/// an active filter is not a smaller answer, it is a wrong one.
pub fn reject_withheld_dimensions(
    node: &ResolvedNode,
    reach: TextSearchReach,
    env_names: EnvNameReach,
) -> Result<(), ApiError> {
    match node {
        // TWO checks per leaf, each on its own permission, and the `reach`
        // early return lives INSIDE the second one rather than at the top of
        // this function. It used to guard the whole walk, which meant a caller
        // holding `event:read` skipped every check here — including the
        // environment one, which `event:read` does not entitle them to.
        ResolvedNode::Pred(p) => {
            reject_withheld_environment(p, env_names)?;
            reject_withheld_body(p, reach)
        }
        // Free text is narrowed, not refused — see the doc comment. It also
        // cannot name an environment: `?q=prod` matches row CONTENT, never the
        // name→id resolution the `environment` dimension performs.
        ResolvedNode::Text(_) => Ok(()),
        ResolvedNode::Not(inner) => reject_withheld_dimensions(inner, reach, env_names),
        // Under a `Not` or inside an `Or` is still a probe: the answer is
        // observable either way, so the walk must reach every leaf.
        ResolvedNode::And(v) | ResolvedNode::Or(v) => v
            .iter()
            .try_for_each(|n| reject_withheld_dimensions(n, reach, env_names)),
    }
}

/// `environment:<name>` without `env:read`.
///
/// **The same test as `workflow`, applied to the column that fails it next.**
/// Environment names are served by exactly two endpoints —
/// `routes/environments.rs:196` (`authorize_project(… perm::ENV_READ)`) and
/// `:408` (`reach_for(&grants, perm::ENV_READ)`) — so a caller without
/// `env:read` is not entitled to learn them through *any* route. The rule is
/// "which permission governs this column", not "does this column appear in the
/// body I strip".
///
/// **Why it is an oracle and not merely a filter.** `prepare` resolves every
/// `environment` name in the tree app-wide (`query_plan::prepare::
/// resolve_environments` is keyed on `app_id` alone, with no environment
/// reach), and a name with no row lowers to `Uuid::nil()` — which can never
/// equal a real enrollment id, so it matches nothing. A name that DOES exist
/// matches its rows. So `?query=environment:staging` answers "does an
/// environment called `staging` exist in this app", and the answer is readable
/// straight off the envelope's `total` even when `data` is empty. Walk a
/// wordlist and you have the app's environment list.
///
/// S2c Task 5 is what made this reachable: `issues::events` is the first route
/// to expose name→id resolution to a caller authorized on `issue:read` alone,
/// and Task 6 would have duplicated it on Events. Fixed in the shared seam so
/// it is fixed once.
///
/// **Keyed on the caller's permission, not refused outright**, because the
/// distinction is expressible and a blanket refusal would break a real use
/// case: an app-wide caller holding `env:read` narrowing by name in the query
/// language instead of by enrollment id in `?environment_id=`. All four preset
/// roles (Owner/Admin/Developer/Viewer) carry `env:read`, so this gate is
/// invisible to every one of them and bites only a custom role deliberately
/// built without it — which is precisely the caller it exists for. An
/// env-scoped member without `env:read` loses nothing real either: `scope_env!`
/// already pins them to their own environment, so the predicate is a no-op on
/// their own name and empty on any other, and `environments.rs:408` denies them
/// the picker that would tell them a name in the first place.
///
/// **Matched on the STORE, not the dimension name**, exactly as
/// `prepare::collect_environment_names` is: the catalog ALSO declares an
/// `environment` dimension on Issues, as `Store::Rollup`, and that one must
/// keep failing the way it already does (`PlanError::NotYetSupported` → 400
/// from `prepare`) rather than acquiring a 403 that would imply the field is
/// otherwise available there.
///
/// **The whole dimension, not just the name-carrying operators.** `OPS_EQ` is
/// `[Eq, Ne, In, Has]` and only the first three carry a name, so `has:
/// environment` could in principle be allowed — it asks "is this occurrence
/// attributed at all", which is already visible in the rows (`environment_id`
/// survives `strip_event_body`). Refused anyway: this function has no op-level
/// logic anywhere else, and an allowlist of "every op except Has" is the
/// opt-out shape whose failure mode [`NON_WITHHELD_JSON_COLUMNS`]' comment
/// records — it would fail OPEN the day `environment` gains `Contains`, which
/// is a far sharper enumerator than `Eq`.
fn reject_withheld_environment(
    p: &ResolvedPredicate,
    env_names: EnvNameReach,
) -> Result<(), ApiError> {
    if env_names.may_read_names() {
        return Ok(());
    }
    if matches!(p.dim.store, Store::Column("environment_id")) {
        return Err(ApiError::Forbidden(
            "filtering by environment requires env:read: environment names are served only by \
             the environments endpoints, which require that permission, so a filter over them \
             would disclose which environment names exist in this app"
                .into(),
        ));
    }
    Ok(())
}

/// The three storage kinds withheld from a caller without `event:read` — see
/// [`reject_withheld_dimensions`]' doc for the reasoning behind each.
fn reject_withheld_body(p: &ResolvedPredicate, reach: TextSearchReach) -> Result<(), ApiError> {
    if reach.includes_body() {
        return Ok(());
    }
    if matches!(p.dim.store, Store::Tag) {
        return Err(ApiError::Forbidden(
            "filtering by tag requires event:read: an event's tags are withheld from a \
             caller holding only issue:read, and a filter over them would disclose their \
             contents"
                .into(),
        ));
    }
    if let Store::JsonRoot { column, .. } = p.dim.store {
        if !NON_WITHHELD_JSON_COLUMNS.contains(&column) {
            return Err(ApiError::Forbidden(format!(
                "filtering by `{}` requires event:read: it is a predicate over the \
                 event body, which is withheld from a caller holding only issue:read, \
                 and would disclose its contents",
                p.dim.name
            )));
        }
    }
    if p.dim.name == "workflow" {
        return Err(ApiError::Forbidden(
            "filtering by workflow requires event:read: workflow names are served only by \
             the workflows endpoints, which require that permission, so a filter over \
             them would disclose names this caller cannot otherwise read"
                .into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaVariable {
    pub prefix: String,
    pub description: String,
    pub chainable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaDimension {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub ops: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagInfo {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelInfo {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaResponse {
    pub resource: String,
    pub variables: Vec<SchemaVariable>,
    pub dimensions: Vec<SchemaDimension>,
    pub available_tags: Vec<TagInfo>,
    pub available_labels: Vec<LabelInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SchemaQuery {
    pub context: Option<String>,
}

pub fn build_schema_response(
    context_str: &str,
    resource: sauron_query::Resource,
) -> SchemaResponse {
    // Advertise only the variables this resource can actually resolve. The
    // catalog declares each dimension's `resources`, and `resolve_field`
    // enforces it — `issues` has no `context`/`extra`/`tags` column at all, so
    // listing them here handed the autocomplete a prefix that every query using
    // it would then reject as an unknown field.
    let mut variables = Vec::new();
    if sauron_query::catalog::tag_dimension(resource).is_some() {
        variables.push(SchemaVariable {
            prefix: "@tag".to_string(),
            description: "Developer tags".to_string(),
            chainable: true,
        });
    }
    if sauron_query::catalog::lookup("context", resource).is_some() {
        variables.push(SchemaVariable {
            prefix: "@context".to_string(),
            description: "Device/runtime context".to_string(),
            chainable: true,
        });
    }
    if sauron_query::catalog::lookup("extra", resource).is_some() {
        variables.push(SchemaVariable {
            prefix: "@extra".to_string(),
            description: "Extra metadata".to_string(),
            chainable: true,
        });
    }
    if sauron_query::catalog::label_dimension(resource).is_some() {
        variables.push(SchemaVariable {
            prefix: "@$label".to_string(),
            description: "Label properties".to_string(),
            chainable: true,
        });
    }

    let dimensions: Vec<SchemaDimension> = sauron_query::catalog::dimensions_for(resource)
        .map(|d| {
            let (ty, options) = match d.ty {
                sauron_query::ValueType::Str => ("string".to_string(), None),
                sauron_query::ValueType::Enum(opts) => (
                    "enum".to_string(),
                    Some(opts.iter().map(|s| s.to_string()).collect()),
                ),
                sauron_query::ValueType::Int => ("integer".to_string(), None),
                sauron_query::ValueType::Bool => ("boolean".to_string(), None),
                sauron_query::ValueType::Duration => ("duration".to_string(), None),
                sauron_query::ValueType::Timestamp => ("timestamp".to_string(), None),
            };

            let ops = d
                .ops
                .iter()
                .map(|op| match op {
                    sauron_query::MatchOp::Eq => "=".to_string(),
                    sauron_query::MatchOp::Ne => "!=".to_string(),
                    sauron_query::MatchOp::Gt => ">".to_string(),
                    sauron_query::MatchOp::Gte => ">=".to_string(),
                    sauron_query::MatchOp::Lt => "<".to_string(),
                    sauron_query::MatchOp::Lte => "<=".to_string(),
                    sauron_query::MatchOp::In => "in".to_string(),
                    sauron_query::MatchOp::Has => "has".to_string(),
                    sauron_query::MatchOp::Like => "like".to_string(),
                    sauron_query::MatchOp::Contains => "contains".to_string(),
                })
                .collect();

            let aliases = d.aliases.iter().map(|s| s.to_string()).collect();

            SchemaDimension {
                name: d.name.to_string(),
                ty,
                ops,
                options,
                aliases,
            }
        })
        .collect();

    let available_tags = if sauron_query::catalog::tag_dimension(resource).is_some() {
        vec![
            TagInfo {
                key: "environment".to_string(),
                sample_values: Some(vec!["production".to_string(), "staging".to_string()]),
            },
            TagInfo {
                key: "release".to_string(),
                sample_values: None,
            },
        ]
    } else {
        vec![]
    };

    let available_labels = if sauron_query::catalog::label_dimension(resource).is_some() {
        vec![LabelInfo {
            key: "team".to_string(),
            ty: "string".to_string(),
        }]
    } else {
        vec![]
    };

    SchemaResponse {
        resource: context_str.to_string(),
        variables,
        dimensions,
        available_tags,
        available_labels,
    }
}

pub async fn schema(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<SchemaQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SchemaResponse>, ApiError> {
    let mut conn = super::db(&state).await?;
    let _scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        sauron_auth::perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let context_str = q.context.as_deref().unwrap_or("issues");
    let resource = match context_str {
        "issues" => sauron_query::Resource::Issues,
        "sessions" => sauron_query::Resource::Sessions,
        "occurrences" => sauron_query::Resource::Occurrences,
        "events" => sauron_query::Resource::Events,
        "devices" => sauron_query::Resource::Devices,
        "persons" => sauron_query::Resource::Persons,
        "transactions" => sauron_query::Resource::Transactions,
        other => return Err(ApiError::BadRequest(format!("invalid context: {other}"))),
    };

    Ok(Json(build_schema_response(context_str, resource)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauron_db::repo::TextSearchReach::{IncludingBody, ShellOnly};

    // -----------------------------------------------------------------------
    // resolve_window
    // -----------------------------------------------------------------------

    fn planner(to_days: i64) -> Option<Clamp> {
        Some(Clamp {
            field: "since",
            to_days,
            reason: "unindexed predicate requires a bounded time window",
        })
    }

    /// Days between `now` and the resolved `since`, which is the only property
    /// of the timestamp these tests care about.
    fn days_back(w: &Window, now: DateTime<Utc>) -> i64 {
        (now - w.since).num_days()
    }

    #[test]
    fn an_unnarrowed_window_discloses_nothing() {
        let now = Utc::now();
        let w = resolve_window("last_seen", now, 30, 3650, None);
        assert_eq!(days_back(&w, now), 30);
        assert!(w.clamped.is_none());
    }

    /// The defect this function was written for. `analytics::events_list` has a
    /// 365-day ceiling; asking for 3650 served 365 under `clamped: null`, so
    /// the envelope asserted the window had NOT been narrowed while serving a
    /// tenth of it.
    #[test]
    fn a_request_past_the_route_ceiling_is_narrowed_and_says_so() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, 3650, 365, None);
        assert_eq!(days_back(&w, now), 365);
        let c = w.clamped.expect("a tenfold narrowing must be disclosed");
        assert_eq!(c.field, "occurred_at");
        assert_eq!(c.to, "365d", "the window served, not the one requested");
        assert!(
            c.reason.contains("365"),
            "reason should name the bound: {c:?}"
        );
    }

    /// The adjacent defect, and the more dangerous one: all three handlers
    /// reported `prepared.clamp` whenever it existed, including when the
    /// caller's own window was tighter and the clamp therefore never bound.
    /// Seven days of rows shipped under `clamped: {"to": "30d"}`.
    #[test]
    fn a_planner_clamp_that_does_not_bind_is_not_reported() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, 7, 365, planner(30));
        assert_eq!(days_back(&w, now), 7, "the tighter window must win");
        assert!(
            w.clamped.is_none(),
            "a clamp wider than the window served narrowed nothing: {:?}",
            w.clamped
        );
    }

    #[test]
    fn a_planner_clamp_that_does_bind_is_reported_at_the_window_served() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, 365, 365, planner(30));
        assert_eq!(days_back(&w, now), 30);
        let c = w.clamped.expect("a binding clamp must be disclosed");
        assert_eq!(c.to, "30d");
        assert!(
            c.reason.contains("unindexed"),
            "the planner's reason: {c:?}"
        );
    }

    /// Both rules narrow; the tighter one owns both the window and the reason.
    /// Crediting the ceiling here would send the caller to raise `since_days`,
    /// which would change nothing.
    #[test]
    fn when_both_narrow_the_tighter_rule_owns_the_reason() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, 3650, 365, planner(30));
        assert_eq!(days_back(&w, now), 30);
        let c = w.clamped.expect("still narrowed");
        assert_eq!(c.to, "30d");
        assert!(
            c.reason.contains("unindexed"),
            "the planner bound at 30d, so the ceiling must not take credit: {c:?}"
        );
    }

    /// A clamp equal to the window in force changed nothing, so it is not the
    /// reason for anything. `<` rather than `<=` is what makes this hold.
    #[test]
    fn a_planner_clamp_equal_to_the_window_is_not_a_narrowing() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, 30, 365, planner(30));
        assert_eq!(days_back(&w, now), 30);
        assert!(w.clamped.is_none(), "{:?}", w.clamped);
    }

    /// Raising a nonsense window to 1 day hands the caller MORE than they asked
    /// for. `clamped` warns that rows may be missing, so widening must stay out
    /// of it or the field stops meaning one thing.
    #[test]
    fn a_sub_day_request_is_raised_to_one_day_without_disclosure() {
        let now = Utc::now();
        for requested in [0, -1, i64::MIN] {
            let w = resolve_window("last_seen", now, requested, 3650, None);
            assert_eq!(days_back(&w, now), 1, "since_days={requested}");
            assert!(w.clamped.is_none(), "since_days={requested}");
        }
    }

    /// The ceiling is applied before the planner comparison, so an absurd
    /// `since_days` cannot slip past a clamp that is wider than the ceiling.
    #[test]
    fn the_ceiling_binds_even_when_the_planner_clamp_is_wider() {
        let now = Utc::now();
        let w = resolve_window("occurred_at", now, i64::MAX, 365, planner(3650));
        assert_eq!(days_back(&w, now), 365);
        assert_eq!(w.clamped.expect("narrowed").to, "365d");
    }

    #[test]
    fn sort_defaults_when_absent() {
        let (col, desc) = parse_sort(None, &["last_seen", "first_seen"], "last_seen").unwrap();
        assert_eq!((col.as_str(), desc), ("last_seen", true));
    }

    #[test]
    fn sort_accepts_a_leading_minus_for_ascending() {
        // `-` reads as "reverse the default", and the default everywhere here is
        // newest-first, so `-last_seen` is oldest-first.
        let (col, desc) = parse_sort(Some("-last_seen"), &["last_seen"], "last_seen").unwrap();
        assert_eq!((col.as_str(), desc), ("last_seen", false));
    }

    #[test]
    fn sort_rejects_a_column_with_no_keyset_index() {
        // Not cosmetic: an unindexed ordering cannot page stably, and silently
        // returning duplicate rows is the bug this slice removes.
        let err = parse_sort(Some("times_seen"), &["last_seen"], "last_seen").unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("times_seen"),
            "error should name the bad field: {msg}"
        );
        assert!(
            msg.contains("last_seen"),
            "error should list what is allowed: {msg}"
        );
    }

    fn issues_node(q: &str) -> ResolvedNode {
        resolve_query(Some(q), &[], None, sauron_query::Resource::Issues).unwrap()
    }

    /// Every pre-environment test predates the second axis and is about the
    /// `event:read` one, so they pass `Readable` — the value that leaves the
    /// environment check a no-op and cannot make them pass for a new reason.
    fn reject(node: &ResolvedNode, reach: TextSearchReach) -> Result<(), ApiError> {
        reject_withheld_dimensions(node, reach, EnvNameReach::Readable)
    }

    #[test]
    fn only_a_database_plan_error_is_a_five_hundred() {
        assert!(matches!(
            map_plan_error(PlanError::Database("connection reset".into())),
            ApiError::Internal(_)
        ));
        for e in [
            PlanError::NotYetSupported {
                field: "environment".into(),
            },
            PlanError::UnsupportedOnResource {
                field: "duration".into(),
            },
            PlanError::BadValue {
                field: "timesSeen".into(),
            },
        ] {
            let named = e.to_string();
            match map_plan_error(e) {
                // The caller must be told which field they got wrong; a bare
                // "bad request" leaves them guessing at a query they typed.
                ApiError::BadRequest(m) => assert_eq!(m, named),
                other => panic!("expected 400, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_tag_predicate_is_refused_without_event_read() {
        let err = reject(&issues_node("tag.checkout_step:payment"), ShellOnly).unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn a_tag_predicate_is_allowed_with_event_read() {
        assert!(reject(&issues_node("tag.checkout_step:payment"), IncludingBody).is_ok());
    }

    /// The bypass the `filter=`-only version of this check could not see: the
    /// probe is the same question, spelled in the new grammar.
    #[test]
    fn a_withheld_predicate_nested_under_not_and_or_is_still_refused() {
        for q in [
            "!tag.checkout_step:payment",
            "is:unresolved OR tag.checkout_step:payment",
            "!(level:error tag.checkout_step:payment)",
            "workflow:~check",
        ] {
            assert!(
                reject(&issues_node(q), ShellOnly).is_err(),
                "`{q}` must be refused"
            );
        }
    }

    /// No-op on Issues (`R_ISSUES` declares no JSON root), so the arm is
    /// exercised on the resources Tasks 5 and 6 bridge — which is exactly why
    /// it had to land in the linchpin rather than in either of them.
    #[test]
    fn a_json_root_predicate_is_refused_without_event_read() {
        use sauron_query::Resource;
        // Every JSON root on Occurrences sits on a column
        // `strip_event_body` nulls: `context`, `contexts`, `extra`,
        // `event_user`, `sdk`, `stacktrace`.
        for q in [
            "os.name:Linux",
            "browser.version:~12",
            "device.model:Pixel",
            "app.build:100",
            "contexts.k:v",
            "extra.token:~sk_live_",
            "user.email:a@b.com",
            "sdk.name:sauron",
            "stack.frame:main",
        ] {
            let node = resolve_query(Some(q), &[], None, Resource::Occurrences).unwrap();
            match reject(&node, ShellOnly) {
                Err(ApiError::Forbidden(m)) => assert!(
                    m.contains("event:read"),
                    "`{q}`: the refusal must name the permission that lifts it: {m}"
                ),
                other => panic!("`{q}` must be refused with a 403, got {other:?}"),
            }
            // …and allowed once the caller may read the body it probes.
            assert!(reject(&node, IncludingBody).is_ok());
        }
    }

    #[test]
    fn a_json_root_over_a_non_withheld_column_is_allowed() {
        use sauron_query::Resource;
        // `properties` (Events) and `traits` (Persons) both sit on a
        // `properties` column, which is not an `error_events` column at all —
        // `strip_event_body` never touches it, so there is nothing to probe.
        for (q, r) in [
            ("properties.plan:pro", Resource::Events),
            ("traits.plan:pro", Resource::Persons),
        ] {
            let node = resolve_query(Some(q), &[], None, r).unwrap();
            assert!(
                reject(&node, ShellOnly).is_ok(),
                "`{q}` must NOT be refused — it is not a withheld column"
            );
        }
    }

    /// The list is fail-closed, and this is what pins that: it must name
    /// COLUMNS `symbolicate::strip_event_body` leaves alone. If a later slice
    /// adds a root over a withheld column and quietly adds it here too, this
    /// test is the place the reviewer looks.
    #[test]
    fn the_json_exception_list_names_only_non_event_columns() {
        assert_eq!(NON_WITHHELD_JSON_COLUMNS, &["properties"]);
    }

    // -- The `env:read` axis (S2c Task 5 fix round) -------------------------

    fn occ_node(q: &str) -> ResolvedNode {
        resolve_query(Some(q), &[], None, sauron_query::Resource::Occurrences).unwrap()
    }

    /// `environment:<name>` resolves a NAME against the whole app, and a name
    /// that exists matches rows while one that does not lowers to `Uuid::nil()`
    /// and matches none. Without `env:read` that is an enumeration oracle.
    #[test]
    fn an_environment_predicate_is_refused_without_env_read() {
        for q in [
            "environment:staging",
            "!environment:staging",
            "environment:[staging,prod]",
            "level:error OR environment:staging",
            "!(level:error environment:staging)",
            // `Has` carries no name and is refused anyway — the whole
            // dimension is gated, deliberately. See
            // `reject_withheld_environment`'s doc.
            "has:environment",
        ] {
            match reject_withheld_dimensions(
                &occ_node(q),
                TextSearchReach::IncludingBody,
                EnvNameReach::Withheld,
            ) {
                Err(ApiError::Forbidden(m)) => assert!(
                    m.contains("env:read"),
                    "`{q}`: the refusal must name the permission that lifts it: {m}"
                ),
                other => panic!("`{q}` must be refused with a 403, got {other:?}"),
            }
        }
    }

    /// **The regression this fix round exists for.** `reach.includes_body()`
    /// used to short-circuit the entire walk, so a caller holding
    /// `issue:read + event:read` — but NOT `env:read` — skipped every check in
    /// here, environment included. `event:read` does not entitle anyone to
    /// environment names, so the two axes must be evaluated independently.
    ///
    /// Both legs are asserted: the widest `reach` must not lift the env gate,
    /// and the narrowest must not lift it either.
    #[test]
    fn event_read_does_not_lift_the_environment_gate() {
        for reach in [IncludingBody, ShellOnly] {
            assert!(
                matches!(
                    reject_withheld_dimensions(
                        &occ_node("environment:staging"),
                        reach,
                        EnvNameReach::Withheld,
                    ),
                    Err(ApiError::Forbidden(_))
                ),
                "{reach:?}: event:read must not stand in for env:read"
            );
        }
    }

    /// …and the gate is permission-keyed, not a blanket refusal: an app-wide
    /// caller holding `env:read` narrows by name as usual. Without this leg a
    /// handler that refused every environment predicate would pass above.
    #[test]
    fn an_environment_predicate_is_allowed_with_env_read() {
        for q in [
            "environment:staging",
            "!environment:staging",
            "environment:[staging,prod]",
            "has:environment",
        ] {
            assert!(
                reject_withheld_dimensions(
                    &occ_node(q),
                    TextSearchReach::ShellOnly,
                    EnvNameReach::Readable,
                )
                .is_ok(),
                "`{q}` must be served to a caller holding env:read"
            );
        }
    }

    /// The gate keys on the STORE, so Issues' same-named `Store::Rollup`
    /// dimension is untouched: it must keep failing as
    /// `PlanError::NotYetSupported` (a 400 from `prepare`) rather than
    /// acquiring a 403 that would imply the field is otherwise available there.
    #[test]
    fn the_issues_environment_rollup_is_not_turned_into_a_forbidden() {
        assert!(reject_withheld_dimensions(
            &issues_node("environment:production"),
            ShellOnly,
            EnvNameReach::Withheld,
        )
        .is_ok());
    }

    /// Free text can never name an environment — `?q=prod` matches row
    /// content, not the name→id resolution the dimension performs — so the
    /// narrowing rule is unchanged by the new axis.
    #[test]
    fn free_text_is_not_refused_by_the_environment_gate() {
        assert!(reject_withheld_dimensions(
            &occ_node("production"),
            ShellOnly,
            EnvNameReach::Withheld,
        )
        .is_ok());
    }

    #[test]
    fn env_name_reach_is_derived_from_env_read_and_nothing_else() {
        use sauron_auth::perm;
        let set = |ps: &[&str]| ps.iter().map(|p| p.to_string()).collect::<HashSet<_>>();
        assert_eq!(
            EnvNameReach::for_perms(&set(&[perm::ENV_READ])),
            EnvNameReach::Readable
        );
        // The whole point: neither of the permissions this route already
        // resolves is a substitute.
        assert_eq!(
            EnvNameReach::for_perms(&set(&[perm::ISSUE_READ, perm::EVENT_READ])),
            EnvNameReach::Withheld
        );
        assert_eq!(EnvNameReach::for_perms(&set(&[])), EnvNameReach::Withheld);
    }

    /// Every preset role carries `env:read`, so this gate is invisible to all
    /// four and bites only a custom role deliberately built without it. Pinned
    /// because it is the whole basis for choosing a permission-keyed gate over
    /// an unconditional refusal — if a preset ever drops `env:read`, that
    /// choice needs revisiting rather than silently narrowing that role.
    #[test]
    fn every_preset_role_can_still_filter_by_environment() {
        for preset in sauron_auth::rbac::PRESETS {
            let perms: HashSet<String> = preset.permissions.iter().map(|p| p.to_string()).collect();
            assert_eq!(
                EnvNameReach::for_perms(&perms),
                EnvNameReach::Readable,
                "preset role {:?} lost env:read — the environment gate now narrows it",
                preset.name
            );
        }
    }

    #[test]
    fn free_text_is_narrowed_rather_than_refused() {
        // `?q=` is answered with a smaller predicate (see `IssuesLower::text`),
        // which is still an honest answer to "find rows matching this".
        assert!(reject(&issues_node("boom"), ShellOnly).is_ok());
        assert!(reject(&issues_node("is:unresolved"), ShellOnly).is_ok());
    }

    // -- `has_free_text` (S2c Task 6, `event_stats`' `payload_searched`) -----

    /// The whole reason this is not `q.q.is_some()`: since `event_stats` was
    /// bridged, a bare term inside `query=` narrows exactly as `?q=` does, and
    /// both must report as a search that ran.
    #[test]
    fn free_text_is_detected_in_either_spelling_and_at_any_depth() {
        for q in [
            "boom",
            "is:unresolved boom",
            "!boom",
            "is:unresolved OR boom",
            "!(level:error boom)",
        ] {
            assert!(has_free_text(&issues_node(q)), "`{q}` contains free text");
        }
    }

    /// …and a predicate-only query must NOT report one, or every filtered
    /// request would claim a narrowing that never happened.
    #[test]
    fn a_predicate_only_query_reports_no_free_text() {
        for q in [
            "",
            "is:unresolved",
            "level:error timesSeen:>5",
            "!(is:resolved OR level:error)",
        ] {
            assert!(!has_free_text(&issues_node(q)), "`{q}` has no free text");
        }
    }

    /// An empty `?q=` is normalized to `None` by the handler before it reaches
    /// `resolve_query`, so it contributes no `Text` node and still reports as
    /// "no search ran" — the `None` of the flag's three states.
    #[test]
    fn an_empty_free_text_term_is_not_a_search_that_ran() {
        let node = resolve_query(None, &[], None, sauron_query::Resource::Issues).unwrap();
        assert!(!has_free_text(&node));
    }

    #[test]
    fn envelope_serialises_the_documented_shape() {
        let env = SearchEnvelope {
            data: vec![1_i32, 2],
            total: 1204,
            total_is_capped: false,
            next_cursor: None,
            clamped: None,
        };
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["total"], 1204);
        // A number, never a display string like "1204+": every client would
        // otherwise have to parse a number back out of it.
        assert!(v["total"].is_number());
        assert_eq!(v["total_is_capped"], false);
        assert!(v["next_cursor"].is_null());
        assert!(v["clamped"].is_null());
        assert_eq!(v["data"], serde_json::json!([1, 2]));
    }

    #[test]
    fn schema_response_generation() {
        let resp = build_schema_response("sessions", sauron_query::Resource::Sessions);
        assert_eq!(resp.resource, "sessions");
        assert!(resp.dimensions.iter().any(|d| d.name == "startedAt"));

        // Sessions carry no tags — `TAG_DIM` does not list the resource, and the
        // sessions lowering refuses `Store::Tag` outright — so `@tag` must NOT
        // be advertised here. Offering it handed autocomplete a prefix that
        // every query built from it would then be rejected for as an unknown
        // field. `context` IS declared for Sessions, so that one is offered.
        assert!(!resp.variables.iter().any(|v| v.prefix == "@tag"));
        assert!(resp.variables.iter().any(|v| v.prefix == "@context"));

        // ...and the converse, so this cannot be "fixed" by dropping every
        // variable: Issues have tags but no `context`/`extra` column.
        let issues = build_schema_response("issues", sauron_query::Resource::Issues);
        assert!(issues.variables.iter().any(|v| v.prefix == "@tag"));
        assert!(!issues.variables.iter().any(|v| v.prefix == "@context"));
    }
}
