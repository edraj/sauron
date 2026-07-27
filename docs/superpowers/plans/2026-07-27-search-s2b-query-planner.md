# Search S2b — query planner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/sauron-db/src/query_plan/`, which lowers a `sauron_query::ResolvedNode` into a diesel boxed query for each of the three searchable resources — with **no API change and no new endpoint**.

**Architecture:** One generic tree-walker plus three per-resource leaf mappers behind a `ResourceLower` trait. Everything above the leaf (And/Or/Not combination, De Morgan pushdown, free-text lowering, error propagation) is table-independent and written once; only the leaf knows concrete columns. An async `prepare` pass resolves environment names to uuids in a single query before the synchronous lowering runs.

**Tech Stack:** diesel 2.3 (`BoxableExpression`, `debug_query`), diesel-async, `sauron-query`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-27-pro-search-and-saved-views-design.md`.
- **`diesel::debug_query::<Pg, _>(&q)` renders the full SQL string and the bind list with no database connection.** This is the whole test strategy: CI has no Postgres, so every assertion is an exact-SQL comparison. Write the tests first.
- **The fragment type must be `Nullable<Bool>`, not `Bool`.** `error_events::session_id.eq(x)` on a `Nullable<Text>` column has `SqlType = Nullable<Bool>` and boxing it as `Bool` is a hard compile error. Lift non-nullable leaves with `.nullable()` — it retypes only and emits zero SQL difference.
- **Injection safety is the point of this module.** Only `&'static str` from the catalog may reach SQL text. Every caller-supplied value — including JSON path segments and tag keys — travels as a bind. See Task 4's containment rule.
- Tests are inline `#[cfg(test)] mod tests` in the same file.
- Hard gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- libduckdb must be on the library path for a workspace build (use an **absolute** path — cargo runs test binaries with the crate dir as cwd, so a relative one fails):
  ```bash
  export DUCKDB_LIB_DIR=$(cd "$(ls -d /home/splimter/projects/freelance/sauron/.cache/duckdb/*/*/ | head -1)" && pwd)
  export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH
  ```
- Never `cargo test --all-features`.
- **Never run `diesel migration run`, `diesel migration revert`, or `diesel print-schema`.** This slice needs no migration. `diesel.toml` was recently fixed so migrations no longer clobber the hand-maintained `crates/sauron-db/src/schema.rs`; do not test that fix here, and do not edit `schema.rs`.
- **Never create a git branch. Never commit.** Leave changes staged.
- `sauron-db` must gain a dependency on `sauron-query` (`sauron-query.workspace = true`) — that is expected and is the first time anything depends on it.

---

## Semantics that must be preserved exactly

These come from reading the three functions this planner replaces. Getting one wrong silently changes what users see.

| Resource | Base scope that is NOT a filter |
|---|---|
| Issues | `issues.app_id = $1`, plus `since` on **`issues.last_seen`** |
| Occurrences | `error_events.issue_id = $1` **and `error_events.app_id = $2`** — the `app_id` predicate is NEW, see B4 below |
| Events | `analytics_events.app_id = $1` **and `analytics_events.name <> '$screen'`** — the second is part of the resource definition, not a filter; dropping it leaks synthetic screen-view rows into the Event Explorer |

`since` composes as an outer AND with the query predicates. `firstSeen`/`lastSeen` are now first-class dimensions, but they **add** to the window rather than replacing it.

**Tags on Issues stay a correlated `EXISTS`.** The `issues` table has no `tags` column (16 columns, verified). A tag predicate on Issues lowers to `EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND …)`. The nested `app_id` re-assertion is deliberate and must survive.

**`environment` is a name on the wire and a uuid in the column.** Unknown name on an equality must resolve to `Uuid::nil()` so the predicate matches nothing — never "ignore the filter".

### Bugs to FIX, not reproduce

| # | Bug | Fix |
|---|---|---|
| B1 | Repeated `environment` filters silently last-wins (single `Option` slot) | One predicate per term. `environment:a environment:b` correctly returns zero rows |
| B2 | `neq` on a nullable column drops NULL rows | NULL-safe: `NOT (x = v) OR x IS NULL`. **This changes what existing `filter=…:neq:…` URLs return** — it is the spec's mandated behaviour |
| B3 | `MAX_PAYLOAD_SEARCH_DAYS = 90` is dead code — every route passes `Some(since)` so its `unwrap_or_else` is unreachable | Delete the constant; replace with the cost-driven clamp in Task 6 |
| B4 | Occurrences query has no `app_id` predicate; tenancy rests solely on the handler's pre-check | Add `error_events.app_id = $2` to the base scope. Spec §8 requires the tenant key in every query's WHERE clause |

---

## File Structure

| File | Responsibility |
|---|---|
| `backend/crates/sauron-db/src/query_plan/mod.rs` | `Frag`, `PlanError`, `PrepCtx`, `ResourceLower`, `lower()` |
| `backend/crates/sauron-db/src/query_plan/issues.rs` | `IssuesLower` |
| `backend/crates/sauron-db/src/query_plan/occurrences.rs` | `OccurrencesLower` |
| `backend/crates/sauron-db/src/query_plan/events.rs` | `EventsLower` |
| `backend/crates/sauron-db/src/query_plan/prepare.rs` | async `prepare()`, cost/clamp policy |
| `backend/crates/sauron-db/src/lib.rs` | `pub mod query_plan;` |
| `backend/crates/sauron-db/Cargo.toml` | add `sauron-query.workspace = true` |

---

### Task 1: Scaffold, `PlanError`, and the fragment type

**Files:**
- Modify: `backend/crates/sauron-db/Cargo.toml`
- Create: `backend/crates/sauron-db/src/query_plan/mod.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs`

**Interfaces produced:**
- `pub type Frag<T> = Box<dyn BoxableExpression<T, Pg, SqlType = Nullable<Bool>>>;`
- `pub enum PlanError { NotYetSupported { field: String }, UnsupportedOnResource { field: String }, BadValue { field: String } }` with `Display`
- `pub struct PrepCtx { pub environments: HashMap<String, Option<Uuid>>, pub now: DateTime<Utc> }`

`now` lives in `PrepCtx` rather than being read inside the lowering so that relative timestamps (`-7d`) resolve deterministically and the SQL is assertable in a test.

- [ ] **Step 1: Add the dependency**

In `backend/crates/sauron-db/Cargo.toml`, under `[dependencies]`:

```toml
sauron-query.workspace = true
```

- [ ] **Step 2: Write the failing test**

Create `backend/crates/sauron-db/src/query_plan/mod.rs` with only this test module. It proves the boxed-fragment type actually compiles and combines, which is the single riskiest assumption in the slice:

```rust
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
        let e = PlanError::NotYetSupported { field: "environment".into() };
        assert!(e.to_string().contains("environment"));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-db query_plan
```
Expected: compile error — `cannot find type `Frag``.

- [ ] **Step 4: Write the implementation**

Prepend to `mod.rs`:

```rust
//! Lowering a validated `sauron_query::ResolvedNode` into a diesel boxed query.
//!
//! This is the security boundary's second half. `sauron-query::resolve` guarantees
//! every field is a `&'static Dimension` from the catalog; this module guarantees
//! that only those `&'static str`s ever reach SQL text. Every caller-supplied
//! value — including JSON path segments and tag keys — travels as a bind.
//!
//! Testable without a database: `diesel::debug_query::<Pg, _>` renders the SQL and
//! the binds with no connection, so the whole mapping is asserted in CI.

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
```

Add to `backend/crates/sauron-db/src/lib.rs`:

```rust
pub mod query_plan;
```

- [ ] **Step 5: Run the tests and verify gates**

```bash
cd backend && cargo test -p sauron-db query_plan && cargo fmt --all -- --check && cargo clippy -p sauron-db --all-targets -- -D warnings
```
Expected: 3 tests pass, gates clean. Leave staged; do not commit.

---

### Task 2: The generic tree-walker with De Morgan pushdown

**Files:**
- Modify: `backend/crates/sauron-db/src/query_plan/mod.rs`

**Interfaces produced:**
- `pub trait ResourceLower { type Table: 'static; fn leaf(&self, p: &ResolvedPredicate, ctx: &PrepCtx, negate: bool) -> Result<Frag<Self::Table>, PlanError>; fn text(&self, term: &str) -> Frag<Self::Table>; }`
- `pub fn lower<L: ResourceLower>(node: &ResolvedNode, l: &L, ctx: &PrepCtx) -> Result<Frag<L::Table>, PlanError>`

**Why negation is pushed to the leaf.** `diesel::dsl::not(frag)` over a compound yields `NOT (a OR b)`, which is NULL when a leaf is NULL, so the row is silently dropped — exactly the bug B2 forbids. Only the leaf knows whether its column is nullable, so the walker normalises with De Morgan (`Not(And)`→`Or(Not…)`, `Not(Or)`→`And(Not…)`, `Not(Not x)`→`x`) and hands each leaf a `negate: bool`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `mod.rs`. A tiny stub resource keeps the walker's tests independent of any real leaf mapper:

```rust
    use sauron_query::{parse, resolve, Resource};

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
            Box::new(error_events::message.eq(term.to_string()))
        }
    }

    fn stub_sql(q: &str) -> String {
        let node = resolve(&parse(q).unwrap(), Resource::Occurrences).unwrap();
        let ctx = PrepCtx { environments: HashMap::new(), now: Utc::now() };
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
        assert!(!sql.contains(" NOT "), "negation must not wrap the tree: {sql}");
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
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-db query_plan
```
Expected: compile error — `cannot find trait `ResourceLower``.

- [ ] **Step 3: Write the walker**

Add to `mod.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests and verify gates**

```bash
cd backend && cargo test -p sauron-db query_plan && cargo fmt --all -- --check && cargo clippy -p sauron-db --all-targets -- -D warnings
```
Expected: all walker tests pass. Leave staged.

---

### Task 3: `IssuesLower`

**Files:**
- Create: `backend/crates/sauron-db/src/query_plan/issues.rs`
- Modify: `backend/crates/sauron-db/src/query_plan/mod.rs` (add `pub mod issues;`)

**Interfaces produced:** `pub struct IssuesLower { pub app_id: Uuid }` implementing `ResourceLower<Table = issues::table>`.

Dimensions to map, from `sauron_query::dimensions_for(Resource::Issues)`: `is` (→ `status`), `level`, `type`, `culprit`, `title`, `timesSeen`, `usersSeen`, `firstSeen`, `lastSeen` — nine plannable — plus `TAG_DIM` and free text. `environment`, `release` and `handled` on Issues are `Store::Rollup` and must return `PlanError::NotYetSupported`.

Write the leaf as a `match` on `(dim.store, op)`. For each op:

- `Eq` → `.eq(v)`, or under `negate` the NULL-safe pair `col.ne(v).or(col.is_null())`. On a non-nullable column `.is_null()` is still valid SQL and costs nothing, but prefer emitting the plain `.ne(v)` for non-nullable columns so the SQL stays readable — assert whichever you choose in the test.
- `In` → `.eq_any(vs)`; negated → `.ne_all(vs)` plus the NULL arm where nullable.
- `Gt/Gte/Lt/Lte` → the matching diesel method.
- `Like` / `Contains` → `.ilike(pattern)`; the pattern already arrives escaped in `TypedValue::Pattern`.
- `Has` → on a plain column, `col.is_not_null()`.

**The tag arm is the subtle one.** `issues` has no `tags` column, so:

```rust
// Tags live on the child events; `issues` has no tags column. The inner
// `app_id` re-assertion is deliberate — every query carries the tenant key,
// including nested subqueries.
diesel::dsl::sql::<Nullable<Bool>>(
    "EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id \
     AND e.app_id = issues.app_id AND e.tags @> ",
)
.bind::<diesel::sql_types::Jsonb, _>(tag_object(key, value))
.sql(")")
```

Free text reproduces the current behaviour: `title ILIKE p OR type ILIKE p OR culprit ILIKE p`, plus the correlated `EXISTS` over `contexts::text`/`extra::text`/`tags::text`. Use the existing `crate::repo::like_contains` (it is `pub`) rather than writing a second escaper — `ResolvedNode::Text` arrives **unescaped and unwrapped** by design.

- [ ] **Step 1: Write the failing tests**

Assert **exact SQL fragments and bind values** via `debug_query`. One test per `(store, op)` family, plus these three which encode decisions rather than mechanics:

```rust
    #[test]
    fn rollup_dimensions_are_rejected_with_a_message_that_stays_true() {
        let err = lower_issues("environment:production").unwrap_err();
        assert!(matches!(err, PlanError::NotYetSupported { .. }));
        assert!(err.to_string().contains("environment"));
    }

    #[test]
    fn negated_equality_is_null_safe() {
        // B2: `.ne()` alone drops rows where the column IS NULL.
        let sql = lower_issues_sql("!culprit:handler");
        assert!(sql.contains("IS NULL"), "negation must keep NULL rows: {sql}");
    }

    #[test]
    fn a_tag_predicate_becomes_a_correlated_exists_carrying_the_tenant_key() {
        let sql = lower_issues_sql("checkout_step:payment");
        assert!(sql.contains("EXISTS (SELECT 1 FROM error_events e"), "{sql}");
        assert!(sql.contains("e.app_id = issues.app_id"), "tenant key must be re-asserted: {sql}");
        assert!(sql.contains("e.tags @>"), "{sql}");
    }
```

- [ ] **Step 2: Run to verify they fail.** Expected: `cannot find struct `IssuesLower``.
- [ ] **Step 3: Write `IssuesLower`.**
- [ ] **Step 4: Run the tests.** Expected: all pass.
- [ ] **Step 5: Verify gates.** `cargo fmt --all -- --check && cargo clippy -p sauron-db --all-targets -- -D warnings`. Leave staged.

---

### Task 4: `OccurrencesLower` — including the JSONB rule

**Files:**
- Create: `backend/crates/sauron-db/src/query_plan/occurrences.rs`
- Modify: `mod.rs`

**Interfaces produced:** `pub struct OccurrencesLower { pub app_id: Uuid, pub issue_id: Uuid }` implementing `ResourceLower<Table = error_events::table>`.

This resource has the most dimensions (19) and is the only one with JSON roots.

**The JSONB lowering rule, measured against a live database — the obvious lowering is wrong:**

| Lowering | Plan |
|---|---|
| `col @> $1::jsonb` | **Index Cond** |
| `col ? $1` (top-level key existence) | **Index Cond** |
| `col -> 'a' ? 'b'` | Seq Scan |
| `col #>> '{a,b}' = 'v'` | **Seq Scan** |

So:

- **`Store::JsonRoot` + `Eq` → containment, never `#>>` equality.** Build the nested object in Rust from `dim.store`'s `prefix` plus `ResolvedPredicate.path`, and bind the whole thing as **one** `Jsonb` parameter. The user-supplied path never appears in SQL text at all — a strictly stronger injection story than the code this replaces. Example: `os.name:Linux` → `error_events.context @> $1` with bind `{"os":{"name":"Linux"}}`.
- **`Has` on a single-segment path** → `col ? $1` with a `Text` bind. On a multi-segment path it is effectively a scan; emit `col @? $1::jsonpath` and let the cost classifier bound it.
- **`Contains`/`Like` on a JSON path** → `(col #>> $1) ILIKE $2` with the path bound as `Array<Text>`. Unindexable; the catalog already classes those ops `Cost::Scan`.
- **`stack.*` is a JSON ARRAY, not an object.** `stacktrace` holds `[{filename, function, …}, …]`, so object-path containment is wrong there. Use array containment: `stacktrace @> '[{"filename":"a.js"}]'::jsonb`. This is the one dimension needing a differently-shaped branch — do not let it fall through the object path.

Base scope is `issue_id = $1 AND app_id = $2` (B4).

- [ ] **Step 1: Write the failing tests.** One per `(store, op)` family. These four encode the rules above:

```rust
    #[test]
    fn json_equality_lowers_to_containment_with_the_path_as_a_bind() {
        let (sql, binds) = lower_occ("os.name:Linux");
        assert!(sql.contains(r#""error_events"."context" @>"#), "{sql}");
        assert!(!sql.contains("os"), "the path must NOT appear in SQL text: {sql}");
        assert!(binds.contains(r#"{"os":{"name":"Linux"}}"#), "{binds}");
    }

    #[test]
    fn user_email_does_not_duplicate_the_root_segment() {
        // The column IS the user object, so the prefix is empty.
        let (_sql, binds) = lower_occ("user.email:a@b.com");
        assert!(binds.contains(r#"{"email":"a@b.com"}"#), "{binds}");
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
        assert!(binds.contains(r#"[{"filename":"app.js"}]"#), "array shape required: {binds}");
    }
```

- [ ] **Step 2: Run to verify they fail.**
- [ ] **Step 3: Write `OccurrencesLower`.**
- [ ] **Step 4: Run the tests.**
- [ ] **Step 5: Verify gates.** Leave staged.

---

### Task 5: `EventsLower`

**Files:**
- Create: `backend/crates/sauron-db/src/query_plan/events.rs`
- Modify: `mod.rs`

**Interfaces produced:** `pub struct EventsLower { pub app_id: Uuid }` implementing `ResourceLower<Table = analytics_events::table>`.

Eight dimensions plus tag, JSON roots (`properties`, `contexts`, `extra`) and free text. The JSONB rules from Task 4 apply identically — factor the object-building helper into `mod.rs` rather than copying it.

`environment` here is `Store::Column("environment_id")` with a **name** value: look it up in `ctx.environments`, and lower a missing name to `Uuid::nil()` so it matches nothing.

- [ ] **Step 1: Write the failing tests**, including:

```rust
    #[test]
    fn an_unknown_environment_matches_nothing_rather_than_being_ignored() {
        let ctx = PrepCtx { environments: [("ghost".to_string(), None)].into(), now: Utc::now() };
        let (sql, binds) = lower_events_with("environment:ghost", &ctx);
        assert!(sql.contains(r#""analytics_events"."environment_id" ="#), "{sql}");
        assert!(binds.contains("00000000-0000-0000-0000-000000000000"), "{binds}");
    }

    #[test]
    fn repeated_environment_terms_both_apply() {
        // B1: the old code kept a single Option slot and silently last-won.
        let sql = lower_events_sql("environment:prod environment:staging");
        assert_eq!(sql.matches("environment_id").count(), 2, "{sql}");
    }
```

- [ ] **Step 2–5:** as before — fail, implement, pass, gates. Leave staged.

---

### Task 6: async `prepare()` and the cost clamp

**Files:**
- Create: `backend/crates/sauron-db/src/query_plan/prepare.rs`
- Modify: `mod.rs`

**Interfaces produced:**
- `pub struct Clamp { pub field: &'static str, pub to_days: i64, pub reason: &'static str }`
- `pub struct Prepared { pub ctx: PrepCtx, pub cost: sauron_query::Cost, pub clamp: Option<Clamp> }`
- `pub async fn prepare(node: &ResolvedNode, app_id: Uuid, now: DateTime<Utc>, conn: &mut AsyncPgConnection) -> Result<Prepared, PlanError>`

Three jobs, in this order:

1. **Reject `Store::Rollup` dimensions** with `PlanError::NotYetSupported` before any query runs.
2. **Batch-resolve environment names.** Walk the whole tree — including inside `TypedValue::List` and inside OR branches — collect every `environment` value, and issue **one** query: `SELECT name, id FROM environments WHERE app_id = $1 AND name = ANY($2)`. Names with no row map to `None`.
3. **Classify and clamp.** `sauron_query::classify(node)`; when the result is `Cost::Scan`, produce a `Clamp` bounding the window. Put the limit in `crates/sauron-core/src/config.rs` as a new env var, defaulting to `TIER_HOT_DAYS` (30) — that is simultaneously the honest cost bound and the honest *coverage* bound, since the tier worker has dropped anything older from Postgres. Per spec §14 a new env var also goes in the README table **and** `.env.example`.

Nuance worth encoding: an Issues query whose predicates all hit `issues` columns never touches a tiered table and should not be clamped; one with free text or a tag predicate reaches into `error_events` and should be.

Also **delete `MAX_PAYLOAD_SEARCH_DAYS`** from `repo.rs` (B3) — it is unreachable dead code that reads as protection.

- [ ] **Step 1: Write the failing tests.** The environment batching needs a database, so test the pure parts: the tree walk that *collects* names (extract it as a pure `fn collect_environment_names(&ResolvedNode) -> Vec<String>`), the Rollup rejection, and the clamp policy. Include:

```rust
    #[test]
    fn collects_environment_names_from_inside_lists_and_or_branches() {
        let names = collect_names("environment:[a,b] OR (environment:c level:error)");
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn an_all_scan_query_is_clamped() {
        assert!(clamp_for("title:*boom*").is_some());
    }

    #[test]
    fn an_indexed_query_is_not_clamped() {
        assert!(clamp_for("is:unresolved").is_none());
    }
```

- [ ] **Step 2–5:** fail, implement, pass, gates. Leave staged.

---

### Task 7: Coverage — prove nothing is declared-but-unplanned

**Files:**
- Modify: `backend/crates/sauron-db/src/query_plan/mod.rs`

The catalog's own doc comment says this test is S2b's job: *"Adding a dimension here does NOT make it queryable — the planner must also learn to map its `Store` to SQL. `dimensions_for` is what the tests in S2 iterate to prove nothing is declared-but-unplanned."*

- [ ] **Step 1: Write the test**

```rust
    /// Every dimension the catalog advertises for a resource must lower, or return
    /// `NotYetSupported` — never panic, never silently produce wrong SQL. Without
    /// this, adding a catalog entry in a later slice looks like it works and then
    /// 500s at runtime.
    #[test]
    fn every_declared_dimension_lowers_or_is_explicitly_deferred() {
        for (resource, sample) in [
            (Resource::Issues, /* an IssuesLower */),
            (Resource::Occurrences, /* an OccurrencesLower */),
            (Resource::Events, /* an EventsLower */),
        ] {
            for dim in sauron_query::dimensions_for(resource) {
                for op in dim.ops {
                    let q = sample_query_for(dim, *op);
                    let node = match resolve(&parse(&q).unwrap(), resource) {
                        Ok(n) => n,
                        Err(e) => panic!("`{q}` failed to resolve for {resource:?}: {e}"),
                    };
                    match lower(&node, &sample, &ctx()) {
                        Ok(_) => {}
                        Err(PlanError::NotYetSupported { .. }) => {}
                        Err(e) => panic!("`{q}` ({resource:?}) failed to lower: {e}"),
                    }
                }
            }
        }
    }
```

Write `sample_query_for(dim, op)` to synthesize a syntactically valid query for a dimension and operator from `dim.ty` — e.g. an `Enum` uses its first option, `Int` uses `1`, `Duration` uses `1s`, `Timestamp` uses `-1d`, `Bool` uses `true`. Fill in the three lower instances with fixed uuids.

Assert the counts so a silently-shrinking catalog is caught: **Issues 12 declared (3 deferred), Occurrences 19, Events 8.**

- [ ] **Step 2: Run it.** Expect failures for any `(store, op)` pair the leaf mappers missed — fix the mappers, not the test.
- [ ] **Step 3: Full workspace gates.**

```bash
cd backend \
  && export DUCKDB_LIB_DIR=$(cd "$(ls -d ../.cache/duckdb/*/*/ | head -1)" && pwd) \
  && export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets -- -D warnings \
  && cargo test --workspace
```
Expected: green. Leave staged; do not commit.

---

## Definition of done for S2b

- Every `(Store, MatchOp)` pair across all three resources has an exact-SQL test asserted via `debug_query`, **with no database**.
- The coverage test proves no catalog dimension is declared-but-unplanned.
- Negation is NULL-safe at the leaf; a test would fail if it wrapped the tree instead.
- JSON equality lowers to `@>` containment with the path as a bind, and the path never appears in SQL text.
- `stack.*` uses array containment.
- Repeated `environment` terms both apply (B1); occurrences carry `app_id` (B4); `MAX_PAYLOAD_SEARCH_DAYS` is gone (B3).
- `cargo test --workspace` green; fmt and clippy clean.
- **No API change.** No route, response shape, or UI behaviour differs — `query_plan` is not wired to a handler until S2c.
- Nothing committed.
