# Dashboard Filtering + Full-Width Layout — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Grafana-style ad-hoc filter chips to the Issues and Events screens over a curated field set, and make the dashboard full-width with roomier detail pages.

**Architecture:** A reusable `FilterBar` (Svelte) drives filter state that is mirrored into the URL query and sent to the backend as repeated `filter=field:op:value` params. The backend parses+validates them against a per-resource whitelist (in `sauron-db`) and folds them into diesel boxed queries (typed, parameterized). Layout goes full-width via one CSS-variable change plus detail-page reflow.

**Tech Stack:** Rust (axum 0.8, diesel-async, `axum-extra` query extractor), Svelte 5 (runes), TypeScript, Vitest (new, for the pure codec), Vite.

## Global Constraints

- Backend enum-like columns are `TEXT` (diesel maps to `String`); `issues.type` is `type_` (`#[sql_name="type"]`).
- Filter values are **always** applied through diesel expression methods (`.eq/.ne/.gt/.lt/.ilike`) with bound values — never string-interpolated into SQL. Field keys and operators are a fixed whitelist.
- `issues` has **no** `environment`/`release` columns → those are Events-only (non-goal for Issues).
- Diesel op methods: `neq → .ne`, `contains → .ilike("%v%")`, `gt → .gt`, `lt → .lt`. `.ilike` needs `diesel::TextExpressionMethods` (already imported in `repo.rs`).
- Frontend imports the icon component from `../lib/components/ui/Icon.svelte` (pages) / `./ui/Icon.svelte` (components). Icons inherit `currentColor`.
- Commit after every task. Keep existing `cargo test` (38) and `svelte-check` (0/0) green.
- Branch: `feat/dashboard-filtering-layout` (already created).

---

### Task 1: Backend filter model + parser (`sauron-db::filter`)

**Files:**
- Create: `backend/crates/sauron-db/src/filter.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod filter;`)

**Interfaces:**
- Produces: `Op{Eq,Neq,Contains,Gt,Lt}`, `FieldType{Str,Enum,Num}`, `FieldSpec{key,ty,ops,options}`, `ParsedFilter{field:&'static str, op:Op, value:String}`, `FilterError`, `fn parse_filters(raw:&[String], allow:&[FieldSpec]) -> Result<Vec<ParsedFilter>, FilterError>`, and consts `ISSUE_FILTERS`, `EVENT_FILTERS: &[FieldSpec]`.

- [ ] **Step 1: Write the failing tests** — create `backend/crates/sauron-db/src/filter.rs` with the test module only at first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_filters() {
        let raw = vec!["level:eq:error".to_string(), "times_seen:gt:100".to_string()];
        let got = parse_filters(&raw, ISSUE_FILTERS).unwrap();
        assert_eq!(got, vec![
            ParsedFilter { field: "level", op: Op::Eq, value: "error".into() },
            ParsedFilter { field: "times_seen", op: Op::Gt, value: "100".into() },
        ]);
    }

    #[test]
    fn value_may_contain_colons() {
        let got = parse_filters(&["culprit:contains:foo:bar".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got[0].value, "foo:bar");
    }

    #[test]
    fn rejects_unknown_field() {
        assert_eq!(
            parse_filters(&["nope:eq:x".to_string()], ISSUE_FILTERS),
            Err(FilterError::UnknownField("nope".into()))
        );
    }

    #[test]
    fn rejects_disallowed_op() {
        // `contains` is not allowed on the enum field `level`
        assert!(matches!(
            parse_filters(&["level:contains:err".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadOp { .. })
        ));
    }

    #[test]
    fn rejects_bad_enum_value() {
        assert!(matches!(
            parse_filters(&["status:eq:banana".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(matches!(
            parse_filters(&["times_seen:gt:lots".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(
            parse_filters(&["level=error".to_string()], ISSUE_FILTERS),
            Err(FilterError::Malformed)
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-db filter:: 2>&1 | tail -20`
Expected: FAIL to compile (`parse_filters`, `Op`, etc. not defined).

- [ ] **Step 3: Implement the module** — prepend above the test module:

```rust
//! Ad-hoc list filtering: a small whitelisted `field:op:value` model shared by
//! the API routes (which parse untrusted input) and the repo (which folds the
//! validated result into diesel boxed queries).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Neq, Contains, Gt, Lt }

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "eq" => Op::Eq,
            "neq" => Op::Neq,
            "contains" => Op::Contains,
            "gt" => Op::Gt,
            "lt" => Op::Lt,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType { Str, Enum, Num }

pub struct FieldSpec {
    pub key: &'static str,
    pub ty: FieldType,
    pub ops: &'static [Op],
    pub options: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilter {
    pub field: &'static str,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    Malformed,
    UnknownField(String),
    BadOp { field: String, op: String },
    BadValue { field: String },
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::Malformed => write!(f, "filter must be field:op:value"),
            FilterError::UnknownField(x) => write!(f, "unknown filter field: {x}"),
            FilterError::BadOp { field, op } => write!(f, "operator {op} not allowed for field {field}"),
            FilterError::BadValue { field } => write!(f, "invalid value for filter field {field}"),
        }
    }
}

/// Parse + validate raw `field:op:value` strings against `allow`. Splits on the
/// first two ':' only (values may contain ':'). Rejects unknown fields,
/// disallowed operators, out-of-range enum values, and non-numeric numbers.
pub fn parse_filters(raw: &[String], allow: &[FieldSpec]) -> Result<Vec<ParsedFilter>, FilterError> {
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let mut parts = item.splitn(3, ':');
        let field = parts.next().unwrap_or("");
        let op_s = parts.next().ok_or(FilterError::Malformed)?;
        let value = parts.next().ok_or(FilterError::Malformed)?;

        let spec = allow
            .iter()
            .find(|f| f.key == field)
            .ok_or_else(|| FilterError::UnknownField(field.to_string()))?;
        let op = Op::parse(op_s).ok_or_else(|| FilterError::BadOp {
            field: field.to_string(),
            op: op_s.to_string(),
        })?;
        if !spec.ops.contains(&op) {
            return Err(FilterError::BadOp { field: field.to_string(), op: op_s.to_string() });
        }
        match spec.ty {
            FieldType::Num => {
                value.parse::<i64>().map_err(|_| FilterError::BadValue { field: field.to_string() })?;
            }
            FieldType::Enum => {
                if !spec.options.contains(&value) {
                    return Err(FilterError::BadValue { field: field.to_string() });
                }
            }
            FieldType::Str => {}
        }
        out.push(ParsedFilter { field: spec.key, op, value: value.to_string() });
    }
    Ok(out)
}

const OPS_STR: &[Op] = &[Op::Eq, Op::Neq, Op::Contains];
const OPS_ENUM: &[Op] = &[Op::Eq, Op::Neq];
const OPS_NUM: &[Op] = &[Op::Eq, Op::Gt, Op::Lt];
const NO_OPTS: &[&str] = &[];

pub const ISSUE_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "level", ty: FieldType::Enum, ops: OPS_ENUM, options: &["debug", "info", "warning", "error", "fatal"] },
    FieldSpec { key: "status", ty: FieldType::Enum, ops: OPS_ENUM, options: &["unresolved", "resolved", "ignored"] },
    FieldSpec { key: "type", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "culprit", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "times_seen", ty: FieldType::Num, ops: OPS_NUM, options: NO_OPTS },
    FieldSpec { key: "users_seen", ty: FieldType::Num, ops: OPS_NUM, options: NO_OPTS },
];

// `environment` is validated as a free string here (valid values are per-app and
// dynamic); the repo resolves the name to an environment_id at query time.
pub const EVENT_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "name", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "distinct_id", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "session_id", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "environment", ty: FieldType::Str, ops: OPS_ENUM, options: NO_OPTS },
    FieldSpec { key: "release", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
];
```

Then add `pub mod filter;` to `backend/crates/sauron-db/src/lib.rs` (next to `pub mod repo;`).

- [ ] **Step 4: Run tests to verify pass**

Run: `cd backend && cargo test -p sauron-db filter:: 2>&1 | tail -20`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/filter.rs backend/crates/sauron-db/src/lib.rs
git commit -m "backend: whitelisted field:op:value filter parser in sauron-db"
```

---

### Task 2: Apply filters in the repo list queries

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (`list_issues`, `list_analytics_events`, add `environment_id_by_name` + two small helpers)

**Interfaces:**
- Consumes: `crate::filter::{ParsedFilter, Op}` (Task 1).
- Produces (new signatures used by Task 3):
  - `list_issues(conn, app_id: Uuid, filters: &[ParsedFilter], q: Option<&str>, since: Option<DateTime<Utc>>, limit: i64, offset: i64) -> QueryResult<Vec<Issue>>`
  - `list_analytics_events(conn, app_id: Uuid, filters: &[ParsedFilter], q: Option<&str>, since: Option<DateTime<Utc>>, limit: i64, offset: i64) -> QueryResult<Vec<AnalyticsEvent>>`

> No unit test — these build diesel queries against Postgres and are verified end-to-end in Task 9 (compose smoke). Correctness gate here is `cargo build` + existing tests staying green.

- [ ] **Step 1: Add helpers** near the top of the `// analytics` region of `repo.rs`:

```rust
use crate::filter::{Op, ParsedFilter};

fn like_contains(v: &str) -> String { format!("%{}%", v) }
fn as_i64(v: &str) -> i64 { v.parse().unwrap_or_default() } // parser guarantees numeric

/// Resolve an environment name to its id for this app (None if unknown).
pub async fn environment_id_by_name(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    name: &str,
) -> Option<Uuid> {
    environments::table
        .filter(environments::app_id.eq(app_id))
        .filter(environments::name.eq(name))
        .select(environments::id)
        .first::<Uuid>(conn)
        .await
        .ok()
}
```

- [ ] **Step 2: Replace `list_issues`** with:

```rust
pub async fn list_issues(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<Issue>> {
    let mut query = issues::table.filter(issues::app_id.eq(app_id)).into_boxed();
    if let Some(s) = since {
        query = query.filter(issues::last_seen.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("level", Op::Eq) => query.filter(issues::level.eq(f.value.clone())),
            ("level", Op::Neq) => query.filter(issues::level.ne(f.value.clone())),
            ("status", Op::Eq) => query.filter(issues::status.eq(f.value.clone())),
            ("status", Op::Neq) => query.filter(issues::status.ne(f.value.clone())),
            ("type", Op::Eq) => query.filter(issues::type_.eq(f.value.clone())),
            ("type", Op::Neq) => query.filter(issues::type_.ne(f.value.clone())),
            ("type", Op::Contains) => query.filter(issues::type_.ilike(like_contains(&f.value))),
            ("culprit", Op::Eq) => query.filter(issues::culprit.eq(f.value.clone())),
            ("culprit", Op::Neq) => query.filter(issues::culprit.ne(f.value.clone())),
            ("culprit", Op::Contains) => query.filter(issues::culprit.ilike(like_contains(&f.value))),
            ("times_seen", Op::Eq) => query.filter(issues::times_seen.eq(as_i64(&f.value))),
            ("times_seen", Op::Gt) => query.filter(issues::times_seen.gt(as_i64(&f.value))),
            ("times_seen", Op::Lt) => query.filter(issues::times_seen.lt(as_i64(&f.value))),
            ("users_seen", Op::Eq) => query.filter(issues::users_seen.eq(as_i64(&f.value))),
            ("users_seen", Op::Gt) => query.filter(issues::users_seen.gt(as_i64(&f.value))),
            ("users_seen", Op::Lt) => query.filter(issues::users_seen.lt(as_i64(&f.value))),
            _ => query, // unreachable: Task 1 whitelists field+op
        };
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            issues::title.ilike(p.clone())
                .or(issues::type_.ilike(p.clone()))
                .or(issues::culprit.ilike(p)),
        );
    }
    query
        .select(Issue::as_select())
        .order(issues::last_seen.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}
```

- [ ] **Step 3: Replace `list_analytics_events`** with:

```rust
pub async fn list_analytics_events(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    // Environment filters need a name->id lookup before the query is built.
    let mut env_eq: Option<Option<Uuid>> = None;   // Some(id) filter present
    let mut env_neq: Option<Option<Uuid>> = None;
    for f in filters {
        if f.field == "environment" {
            let id = environment_id_by_name(conn, app_id, &f.value).await;
            match f.op { Op::Eq => env_eq = Some(id), Op::Neq => env_neq = Some(id), _ => {} }
        }
    }

    let mut query = analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .into_boxed();
    if let Some(s) = since {
        query = query.filter(analytics_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("name", Op::Eq) => query.filter(analytics_events::name.eq(f.value.clone())),
            ("name", Op::Neq) => query.filter(analytics_events::name.ne(f.value.clone())),
            ("name", Op::Contains) => query.filter(analytics_events::name.ilike(like_contains(&f.value))),
            ("distinct_id", Op::Eq) => query.filter(analytics_events::distinct_id.eq(f.value.clone())),
            ("distinct_id", Op::Neq) => query.filter(analytics_events::distinct_id.ne(f.value.clone())),
            ("distinct_id", Op::Contains) => query.filter(analytics_events::distinct_id.ilike(like_contains(&f.value))),
            ("session_id", Op::Eq) => query.filter(analytics_events::session_id.eq(f.value.clone())),
            ("session_id", Op::Neq) => query.filter(analytics_events::session_id.ne(f.value.clone())),
            ("session_id", Op::Contains) => query.filter(analytics_events::session_id.ilike(like_contains(&f.value))),
            ("release", Op::Eq) => query.filter(analytics_events::release.eq(f.value.clone())),
            ("release", Op::Neq) => query.filter(analytics_events::release.ne(f.value.clone())),
            ("release", Op::Contains) => query.filter(analytics_events::release.ilike(like_contains(&f.value))),
            _ => query, // environment handled below; others unreachable
        };
    }
    // environment eq: unknown name -> no rows (filter on the impossible nil id).
    if let Some(id) = env_eq {
        query = match id {
            Some(id) => query.filter(analytics_events::environment_id.eq(id)),
            None => query.filter(analytics_events::environment_id.eq(Uuid::nil())),
        };
    }
    // environment neq: unknown name -> nothing to exclude.
    if let Some(Some(id)) = env_neq {
        query = query.filter(analytics_events::environment_id.ne(id));
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            analytics_events::name.ilike(p.clone())
                .or(analytics_events::distinct_id.ilike(p)),
        );
    }
    query
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}
```

> Note: `analytics_events::session_id`/`release`/`environment_id` are `Nullable`; `.eq(String)` / `.eq(Uuid)` compare against the inner type and exclude NULLs, which is the intended semantics.

- [ ] **Step 4: Build + existing tests green**

Run: `cd backend && cargo build -p sauron-db 2>&1 | tail -20 && cargo test 2>&1 | tail -8`
Expected: builds clean; the 38 existing tests still pass. (Callers `list_issues`/`list_analytics_events` in routes will not compile yet — that is fixed in Task 3; run `cargo build -p sauron-db` here, not the whole workspace.)

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "backend: apply whitelisted filters + free-text + since in list_issues/list_analytics_events"
```

---

### Task 3: Wire filters into the API routes

**Files:**
- Modify: `backend/Cargo.toml:23` (enable axum-extra `query` feature)
- Modify: `backend/bins/sauron-api/src/error.rs` (add `From<FilterError>`)
- Modify: `backend/bins/sauron-api/src/routes/issues.rs` (`list`)
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs` (`events_list`)

**Interfaces:**
- Consumes: `sauron_db::filter::{parse_filters, ISSUE_FILTERS, EVENT_FILTERS, FilterError}`, updated repo signatures (Task 2).
- Produces: `GET /apps/:id/issues?filter=…&q=…&since_days=…&limit&offset` and `GET /apps/:id/events?filter=…&q=…&since_days=…&limit&offset`.

- [ ] **Step 1: Enable the query extractor** — `backend/Cargo.toml` line 23:

```toml
axum-extra = { version = "0.12", features = ["typed-header", "query"] }
```

- [ ] **Step 2: Map `FilterError` to 400** — append to `backend/bins/sauron-api/src/error.rs`:

```rust
impl From<sauron_db::filter::FilterError> for ApiError {
    fn from(e: sauron_db::filter::FilterError) -> Self {
        ApiError::BadRequest(e.to_string())
    }
}
```

- [ ] **Step 3: Update the Issues `list` handler** — in `routes/issues.rs`, replace the `use axum::extract::{Path, Query, State};` line with `use axum::extract::{Path, State};` + `use axum_extra::extract::Query;`, then replace `ListQuery` and `list`:

```rust
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_since_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 { 50 }
fn default_since_days() -> i64 { 3650 } // effectively "all" unless narrowed

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Issue>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ISSUE_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 200);
    Ok(Json(
        repo::list_issues(&mut conn, app_id, &filters, search, Some(since), limit, q.offset.max(0)).await?,
    ))
}
```

- [ ] **Step 4: Update the Events `events_list` handler** — in `routes/analytics.rs`, change its `Query` import to `axum_extra::extract::Query` (keep `axum::extract::{Path, State}`), replace `EventsListQuery` + `events_list`:

```rust
#[derive(Deserialize)]
pub struct EventsListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_list_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_events_list_limit() -> i64 { 50 }
fn default_events_since_days() -> i64 { 3650 }

pub async fn events_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<EventsListQuery>,
) -> Result<Json<Vec<AnalyticsEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    Ok(Json(
        repo::list_analytics_events(&mut conn, app_id, &filters, search, Some(since), q.limit.clamp(1, 200), q.offset.max(0)).await?,
    ))
}
```

(Delete the now-unused `opt`/`name`/`distinct_id`/`session_id` locals from the old body. Any other caller of `repo::list_analytics_events` is limited to this handler — confirm with `grep -rn "list_analytics_events" backend/`.)

- [ ] **Step 5: Build the whole backend**

Run: `cd backend && cargo build 2>&1 | tail -20`
Expected: clean build. Fix any leftover unused imports (`Query` from `axum::extract`).

- [ ] **Step 6: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/bins/sauron-api/src/error.rs backend/bins/sauron-api/src/routes/issues.rs backend/bins/sauron-api/src/routes/analytics.rs
git commit -m "backend: accept filter/q/since_days on issues + events list endpoints"
```

---

### Task 4: Frontend filter model + codec (`filters.ts`) with Vitest

**Files:**
- Modify: `dashboard/package.json` (add `vitest` devDep + `"test": "vitest run"`)
- Create: `dashboard/src/lib/components/filters/filters.ts`
- Create: `dashboard/src/lib/components/filters/filters.test.ts`

**Interfaces:**
- Produces: `type Op`, `type FieldType`, `interface FieldDef`, `interface Filter`, `encodeFilters(Filter[]) => string[]`, `parseFilters(string[], FieldDef[]) => Filter[]`, `OP_LABEL: Record<Op,string>`, `ISSUE_FIELDS`, `EVENT_FIELDS: FieldDef[]`.

- [ ] **Step 1: Add Vitest**

Run: `cd dashboard && npm i -D vitest@^2`
Then set `"test": "vitest run"` in `dashboard/package.json` `scripts`.

- [ ] **Step 2: Write the failing test** — `dashboard/src/lib/components/filters/filters.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { encodeFilters, parseFilters, ISSUE_FIELDS, type Filter } from './filters';

describe('filters codec', () => {
  const f: Filter[] = [
    { field: 'level', op: 'eq', value: 'error' },
    { field: 'culprit', op: 'contains', value: 'foo:bar' },
  ];

  it('encodes to field:op:value with encoded value', () => {
    expect(encodeFilters(f)).toEqual(['level:eq:error', 'culprit:contains:foo%3Abar']);
  });

  it('round-trips through parse', () => {
    expect(parseFilters(encodeFilters(f), ISSUE_FIELDS)).toEqual(f);
  });

  it('drops unknown fields and disallowed ops', () => {
    expect(parseFilters(['nope:eq:x', 'level:contains:err'], ISSUE_FIELDS)).toEqual([]);
  });
});
```

- [ ] **Step 3: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/components/filters/filters.test.ts 2>&1 | tail -20`
Expected: FAIL (module not found).

- [ ] **Step 4: Implement `filters.ts`**:

```ts
export type Op = 'eq' | 'neq' | 'contains' | 'gt' | 'lt';
export type FieldType = 'enum' | 'string' | 'number';

export interface FieldDef {
  key: string;
  label: string;
  type: FieldType;
  ops: Op[];
  options?: string[]; // for type 'enum'
}

export interface Filter { field: string; op: Op; value: string; }

export const OP_LABEL: Record<Op, string> = {
  eq: '=', neq: '≠', contains: 'contains', gt: '>', lt: '<',
};

/** field:op:value — value is URL-encoded so ':' and other chars survive. */
export function encodeFilters(filters: Filter[]): string[] {
  return filters.map((f) => `${f.field}:${f.op}:${encodeURIComponent(f.value)}`);
}

/** Inverse of encodeFilters; drops any filter whose field/op is not in `fields`. */
export function parseFilters(raw: string[], fields: FieldDef[]): Filter[] {
  const out: Filter[] = [];
  for (const item of raw) {
    const i1 = item.indexOf(':');
    const i2 = item.indexOf(':', i1 + 1);
    if (i1 < 0 || i2 < 0) continue;
    const field = item.slice(0, i1);
    const op = item.slice(i1 + 1, i2) as Op;
    const value = decodeURIComponent(item.slice(i2 + 1));
    const def = fields.find((d) => d.key === field);
    if (!def || !def.ops.includes(op)) continue;
    out.push({ field, op, value });
  }
  return out;
}

const OPS_STR: Op[] = ['eq', 'neq', 'contains'];
const OPS_ENUM: Op[] = ['eq', 'neq'];
const OPS_NUM: Op[] = ['eq', 'gt', 'lt'];

export const ISSUE_FIELDS: FieldDef[] = [
  { key: 'level', label: 'Level', type: 'enum', ops: OPS_ENUM, options: ['debug', 'info', 'warning', 'error', 'fatal'] },
  { key: 'status', label: 'Status', type: 'enum', ops: OPS_ENUM, options: ['unresolved', 'resolved', 'ignored'] },
  { key: 'type', label: 'Type', type: 'string', ops: OPS_STR },
  { key: 'culprit', label: 'Culprit', type: 'string', ops: OPS_STR },
  { key: 'times_seen', label: 'Events', type: 'number', ops: OPS_NUM },
  { key: 'users_seen', label: 'Users', type: 'number', ops: OPS_NUM },
];

// `environment` options are injected at runtime (loaded from the environments API).
export const EVENT_FIELDS: FieldDef[] = [
  { key: 'name', label: 'Event', type: 'string', ops: OPS_STR },
  { key: 'distinct_id', label: 'User', type: 'string', ops: OPS_STR },
  { key: 'session_id', label: 'Session', type: 'string', ops: OPS_STR },
  { key: 'environment', label: 'Environment', type: 'enum', ops: OPS_ENUM, options: [] },
  { key: 'release', label: 'Release', type: 'string', ops: OPS_STR },
];
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cd dashboard && npx vitest run src/lib/components/filters/filters.test.ts 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add dashboard/package.json dashboard/package-lock.json dashboard/src/lib/components/filters/filters.ts dashboard/src/lib/components/filters/filters.test.ts
git commit -m "dashboard: filter model + URL codec + field registries (vitest)"
```

---

### Task 5: `FilterBar.svelte` component

**Files:**
- Create: `dashboard/src/lib/components/filters/FilterBar.svelte`

**Interfaces:**
- Consumes: `filters.ts` (`FieldDef`, `Filter`, `Op`, `OP_LABEL`), `Icon`, `SearchInput`, `DateRange`, `Button`.
- Produces: `<FilterBar {fields} bind:filters bind:search bind:sinceDays />` — a bindable, self-contained bar.

- [ ] **Step 1: Implement the component**:

```svelte
<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import SearchInput from '../SearchInput.svelte';
  import DateRange from '../DateRange.svelte';
  import { OP_LABEL, type FieldDef, type Filter, type Op } from './filters';

  interface Props {
    fields: FieldDef[];
    filters: Filter[];
    search: string;
    sinceDays: number;
  }
  let {
    fields,
    filters = $bindable([]),
    search = $bindable(''),
    sinceDays = $bindable(30),
  }: Props = $props();

  let adding = $state(false);
  let draftField = $state<string>('');
  let draftOp = $state<Op>('eq');
  let draftValue = $state('');

  const fieldDef = $derived(fields.find((f) => f.key === draftField));

  function openAdd() {
    adding = true;
    draftField = fields[0]?.key ?? '';
    draftOp = fields[0]?.ops[0] ?? 'eq';
    draftValue = fields[0]?.type === 'enum' ? (fields[0]?.options?.[0] ?? '') : '';
  }
  function onFieldChange() {
    const def = fields.find((f) => f.key === draftField);
    draftOp = def?.ops[0] ?? 'eq';
    draftValue = def?.type === 'enum' ? (def?.options?.[0] ?? '') : '';
  }
  function commit() {
    if (!draftField || draftValue === '') return;
    filters = [...filters, { field: draftField, op: draftOp, value: draftValue }];
    adding = false;
  }
  function remove(i: number) {
    filters = filters.filter((_, idx) => idx !== i);
  }
  function labelFor(key: string): string {
    return fields.find((f) => f.key === key)?.label ?? key;
  }
</script>

<div class="filterbar">
  <div class="chips">
    {#each filters as f, i (i)}
      <span class="chip">
        <span class="c-field">{labelFor(f.field)}</span>
        <span class="c-op">{OP_LABEL[f.op]}</span>
        <span class="c-val mono">{f.value}</span>
        <button class="c-x" aria-label="Remove filter" onclick={() => remove(i)}>
          <Icon name="x" size={12} />
        </button>
      </span>
    {/each}

    {#if adding}
      <span class="draft">
        <select bind:value={draftField} onchange={onFieldChange} aria-label="Filter field">
          {#each fields as f (f.key)}<option value={f.key}>{f.label}</option>{/each}
        </select>
        <select bind:value={draftOp} aria-label="Operator">
          {#each fieldDef?.ops ?? [] as op (op)}<option value={op}>{OP_LABEL[op]}</option>{/each}
        </select>
        {#if fieldDef?.type === 'enum'}
          <select bind:value={draftValue} aria-label="Value">
            {#each fieldDef?.options ?? [] as opt (opt)}<option value={opt}>{opt}</option>{/each}
          </select>
        {:else if fieldDef?.type === 'number'}
          <input type="number" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {:else}
          <input type="text" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {/if}
        <button class="d-ok" onclick={commit}>Add</button>
        <button class="d-x" aria-label="Cancel" onclick={() => (adding = false)}>
          <Icon name="x" size={13} />
        </button>
      </span>
    {:else}
      <button class="add" onclick={openAdd}>+ Add filter</button>
    {/if}
  </div>

  <div class="right">
    <SearchInput bind:value={search} placeholder="Search…" width="220px" />
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
  </div>
</div>

<style>
  .filterbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 16px;
  }
  .chips { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .chip {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 6px 4px 10px;
    background: var(--primary-soft); color: var(--primary);
    border: 1px solid var(--primary-border); border-radius: var(--radius);
    font-size: 12.5px;
  }
  .c-op { opacity: 0.75; }
  .c-x, .d-x {
    display: inline-flex; align-items: center;
    background: none; border: none; color: inherit; padding: 2px; opacity: 0.7;
  }
  .c-x:hover { opacity: 1; }
  .draft {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 6px; border: 1px solid var(--border-strong); border-radius: var(--radius);
    background: var(--surface-2);
  }
  .draft select, .draft input {
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border); border-radius: var(--radius-sm);
    padding: 4px 6px; font-size: 12.5px;
  }
  .draft input { width: 130px; }
  .d-ok, .add {
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: var(--radius-sm); color: var(--text-muted);
    padding: 5px 10px; font-size: 12.5px; font-weight: 540;
  }
  .add:hover, .d-ok:hover { color: var(--text); border-color: var(--border-strong); }
  .right { display: flex; align-items: center; gap: 10px; }
</style>
```

- [ ] **Step 2: Type-check**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -6`
Expected: 0 errors (the component is unused so far; this verifies it compiles). Confirm `DateRange`'s prop API matches (`value` + `onchange`) — see `Issues.svelte` usage; adjust if `DateRange` differs.

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/lib/components/filters/FilterBar.svelte
git commit -m "dashboard: FilterBar component (ad-hoc chips + add popover + search + range)"
```

---

### Task 6: Wire FilterBar + URL sync into Issues

**Files:**
- Modify: `dashboard/src/lib/api/issues.ts` (`listIssues` sends `filter[]`, `q`, `since_days`)
- Modify: `dashboard/src/pages/Issues.svelte` (replace status tabs with FilterBar; URL sync)

**Interfaces:**
- Consumes: `FilterBar`, `filters.ts` (`ISSUE_FIELDS`, `encodeFilters`, `parseFilters`, `Filter`), svelte-spa-router `querystring`/`replace`.

- [ ] **Step 1: Update `listIssues`** in `dashboard/src/lib/api/issues.ts` to accept `{ filters?: string[]; q?: string; sinceDays?: number; limit?; offset? }` and build a query string with repeated `filter=` params:

```ts
export async function listIssues(
  appId: string,
  opts: { filters?: string[]; q?: string; sinceDays?: number; limit?: number; offset?: number } = {},
): Promise<Issue[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<Issue[]>(`/apps/${appId}/issues?${p.toString()}`);
  return data;
}
```

(Match the existing axios `api` import + base path style already in `issues.ts`.)

- [ ] **Step 2: Rework `Issues.svelte`** — remove the `FILTERS`/`statusFilter` tabs block and the `.filters` markup/styles; add:

```svelte
<script lang="ts">
  // ...existing imports minus the status-tab bits...
  import { querystring, replace } from 'svelte-spa-router';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import { ISSUE_FIELDS, encodeFilters, parseFilters, type Filter } from '../lib/components/filters/filters';

  // Hydrate from the URL once.
  const initial = new URLSearchParams($querystring ?? '');
  let filters = $state<Filter[]>(parseFilters(initial.getAll('filter'), ISSUE_FIELDS));
  let search = $state(initial.get('q') ?? '');
  let sinceDays = $state(Number(initial.get('since_days')) || 30);
  // Default view: unresolved, only when the URL carried no filters at all.
  if (filters.length === 0 && !initial.has('filter')) {
    filters = [{ field: 'status', op: 'eq', value: 'unresolved' }];
  }

  let issues = $state<Issue[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string) {
    loading = true; error = null;
    try {
      issues = await listIssues(appId, {
        filters: encodeFilters(filters), q: search || undefined, sinceDays, limit: 100,
      });
    } catch (err) { error = errorMessage(err); issues = []; }
    finally { loading = false; }
  }

  // Re-query + rewrite the URL whenever filter state changes.
  $effect(() => {
    const aid = sessionStore.currentAppId;
    const enc = encodeFilters(filters);
    const _s = search, _d = sinceDays; // track
    if (!aid) return;
    const p = new URLSearchParams();
    for (const f of enc) p.append('filter', f);
    if (_s) p.set('q', _s);
    p.set('since_days', String(_d));
    replace(`/issues?${p.toString()}`);
    void load(aid);
  });
  // keep loadStats effect as-is, driven by sinceDays
</script>
```

Then place `<FilterBar fields={ISSUE_FIELDS} bind:filters bind:search bind:sinceDays />` where the `.filters` tabs used to be (above the results `Card`). Keep StatTiles + Occurrences chart. Remove the now-unused `DateRange` in the head (the FilterBar owns it) — or keep the head `DateRange` and drop FilterBar's; **pick one** to avoid two range pickers. Recommended: remove the head `DateRange`, keep FilterBar's.

- [ ] **Step 3: Type-check**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -6`
Expected: 0 errors.

- [ ] **Step 4: Verify in preview** (dev server; requires backend running via compose — see Task 9. If backend is not up, at minimum confirm the page renders + chips add/remove and the URL updates.)

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/lib/api/issues.ts dashboard/src/pages/Issues.svelte
git commit -m "dashboard: Grafana-style filter bar + URL sync on Issues"
```

---

### Task 7: Wire FilterBar + URL sync into Events

**Files:**
- Modify: `dashboard/src/lib/api/events.ts` (`listEvents` → `filter[]`/`q`/`since_days`)
- Modify: `dashboard/src/pages/Events.svelte`

**Interfaces:**
- Consumes: `FilterBar`, `EVENT_FIELDS`, `encodeFilters`/`parseFilters`, `querystring`/`replace`, `listEnvironments` (existing `api` for the environments endpoint) to fill the `environment` enum options.

- [ ] **Step 1: Update `listEvents`** in `dashboard/src/lib/api/events.ts`:

```ts
export async function listEvents(
  appId: string,
  opts: { filters?: string[]; q?: string; sinceDays?: number; limit?: number; offset?: number } = {},
): Promise<AnalyticsEvent[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<AnalyticsEvent[]>(`/apps/${appId}/events?${p.toString()}`);
  return data;
}
```

(Keep the existing `api` import + `AnalyticsEvent` type already used in `events.ts`. Remove the old `name`/`distinct_id`/`session_id`/`search` params.)

- [ ] **Step 2: Rework `Events.svelte`**:
  - Replace `selectedEvent` + the `SearchInput` in the stream header with `<FilterBar fields={eventFields} bind:filters bind:search bind:sinceDays />` above the charts.
  - `let eventFields = $state(EVENT_FIELDS)` and, on app change, load environments and inject options: find the `environment` field def and set `.options` to the returned names (fall back to `[]`).
  - Clicking a top-event in `BarList` adds/replaces a `name:eq:<event>` filter instead of toggling `selectedEvent`; the chart series + stream both read from `filters`.
  - Hydrate `filters`/`search`/`sinceDays` from `$querystring` (like Task 6); the `$effect` rewrites the URL and reloads series + stream.
  - Keep `sinceDays` default 30; the RANGES buttons can stay or be replaced by FilterBar's DateRange (pick one range control — recommend FilterBar's).

- [ ] **Step 3: Type-check**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -6`
Expected: 0 errors.

- [ ] **Step 4: Commit**

```bash
git add dashboard/src/lib/api/events.ts dashboard/src/pages/Events.svelte
git commit -m "dashboard: Grafana-style filter bar + URL sync on Events"
```

---

### Task 8: Full-width layout + roomier detail pages

**Files:**
- Modify: `dashboard/src/app.css` (`--content-max`)
- Modify: `dashboard/src/pages/IssueDetail.svelte` (two-column reflow)
- Modify (light touch): `dashboard/src/pages/SessionDetail.svelte`, `DeviceDetail.svelte`, `PersonProfile.svelte`

- [ ] **Step 1: Widen content** — in `dashboard/src/app.css`, change `--content-max: 1240px;` to `--content-max: 2200px;`. Confirm `.content-inner` (in `AppShell.svelte`) uses `max-width: var(--content-max); margin-inline: auto;` — if it hard-codes 1240 anywhere, update it to the variable.

- [ ] **Step 2: IssueDetail two-column** — wrap the detail body in a responsive grid: main column (title, stacktrace, occurrences chart) + right rail (level, status, first/last seen, times/users seen, tags, assignee). Add:

```css
.issue-body { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: 22px; align-items: start; }
@media (max-width: 900px) { .issue-body { grid-template-columns: 1fr; } }
```

Move the metadata block into an `<aside class="rail">`; keep prose/stacktrace in the main column. Preserve existing data bindings — presentational only.

- [ ] **Step 3: Let detail/timeline pages use the width** — in `SessionDetail`/`DeviceDetail`/`PersonProfile`, remove or raise any fixed narrow `max-width` on the top-level content wrapper so it fills the column; keep `max-width` only on prose blocks for readability.

- [ ] **Step 4: Verify in preview** — full-width layout + IssueDetail two-column at wide and <900px widths (screenshot). See Task 9 for the server.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/app.css dashboard/src/pages/IssueDetail.svelte dashboard/src/pages/SessionDetail.svelte dashboard/src/pages/DeviceDetail.svelte dashboard/src/pages/PersonProfile.svelte
git commit -m "dashboard: full-width layout + roomier detail pages"
```

---

### Task 9: End-to-end verification

**Files:** none (verification only)

- [ ] **Step 1: Backend tests + build**

Run: `cd backend && cargo test 2>&1 | tail -8 && cargo build 2>&1 | tail -4`
Expected: all tests pass (38 + 7 new = 45), clean build.

- [ ] **Step 2: Frontend checks**

Run: `cd dashboard && npm test 2>&1 | tail -8 && npm run build 2>&1 | tail -6`
Expected: vitest green, `svelte-check` 0/0, `vite build` OK.

- [ ] **Step 3: Compose smoke test** — bring the stack up and exercise the filter endpoints against seeded data:

```bash
docker compose up --build -d
# obtain a token per the project's seed/login flow, then:
curl -s "http://localhost:10000/apps/<APP_ID>/issues?filter=level:eq:error&filter=times_seen:gt:1&since_days=30" -H "Authorization: Bearer <TOKEN>" | jq 'length'
curl -s "http://localhost:10000/apps/<APP_ID>/issues?filter=level:contains:x" -H "Authorization: Bearer <TOKEN>" -o /dev/null -w '%{http_code}\n'  # expect 400 (contains not allowed on enum)
curl -s "http://localhost:10000/apps/<APP_ID>/events?filter=name:contains:checkout&since_days=90" -H "Authorization: Bearer <TOKEN>" | jq 'length'
```

Expected: valid filters return a sensibly reduced set; the disallowed-op request returns `400 bad_request`.

- [ ] **Step 4: Preview UX** — with the stack up, `preview_start` the dashboard, log in, and confirm on Issues + Events: adding an enum filter (level=error) narrows the table; a chip removes; the URL carries `?filter=…` and a reload restores state; the layout is full-width; IssueDetail is two-column. Capture a screenshot.

- [ ] **Step 5: Final commit (if any verification fixups were needed)**

```bash
git add -A && git commit -m "dashboard/backend: verification fixups for filtering + layout"
```

---

## Self-review notes (author)

- **Spec coverage:** Section A → Task 8; Section B (FilterBar + registries + URL sync) → Tasks 4–7; Section C (wire contract, whitelist, boxed-builder, env join) → Tasks 1–3; Section D → Task 8. Testing → Tasks 1/4 (unit) + Task 9 (integration). All covered.
- **Non-goals respected:** no jsonb `properties.*`/`tags.*`, no autocomplete endpoint, no env/release on issues, no Sessions/Users/Devices rollout.
- **Type consistency:** `Op`/`FieldType`/`FieldSpec`/`ParsedFilter`/`parse_filters` names identical across Tasks 1→2→3; `FieldDef`/`Filter`/`encodeFilters`/`parseFilters` identical across Tasks 4→5→6→7. Repo signatures in Task 2 match the calls in Task 3.
- **Watch item:** verify `DateRange`'s prop shape (Task 5 Step 2) and that `.content-inner` reads `--content-max` (Task 8 Step 1) before relying on them.
