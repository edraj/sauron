# Table sorting slice 2: generalised cursor + Events and Occurrences — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Events list and an issue's Occurrences list be sorted by any of their scalar columns, by generalising the keyset cursor from a `(timestamp, uuid)` tuple to `(key, value, id)`.

**Architecture:** The cursor gains the sort key it was minted under, so replaying it under a different sort is refused rather than silently answered with wrong rows, and gains a typed value so the keyset predicate can range over text columns as well as timestamps. Nullable columns are coalesced to a sentinel in both the ORDER BY and the keyset predicate, because a `NULL` in a row comparison yields `NULL`, which would drop every null-valued row from every page after the first.

**Tech Stack:** Rust, axum, diesel-async, Postgres 16, Svelte 5, TypeScript.

**Spec:** `docs/superpowers/specs/2026-08-10-table-pagination-and-sorting-design.md`

**Depends on:** slice 1 (`SortableTh`, `setCursorSort`, `sortParam`).

**Slice 1 shipped page-move reducers this plan predates — use them.** Move the
walk with `setCursorPage(list, nextCursor)` and `cursorBack(list)` from
`lib/models/list-state.ts`, never with `{ ...list, page: advance(...) }`. Both
return `list` **by reference** when the underlying `advance`/`goBack` refuses
the move, which is what preserves `cursor-page.ts`'s documented
`advance(p, c) !== p` means "actually moved" contract. A spread always builds a
new outer object, so the three refusal guards become undetectable at the list
level — and those guards exist because each failure they catch is a pager that
lies with total confidence.

## Global Constraints

- **Never commit and never create a branch.** Every task ends with verification, not a commit.
- **Issues is out of scope.** `apply_issue_env_stats` overwrites the columns that would be sorted, so sorting there orders by one number and displays another. The spec's "Issues sorting is deferred to its own slice" section explains it. Do not add `SortableTh` to `Issues.svelte` in this slice.
- Wire format: a bare column name is **descending**, a `-` prefix is **ascending** (`parse_sort`, `backend/bins/sauron-api/src/routes/search.rs:146`).
- **Backend tests silently pass without a database.** The Bash sandbox has its own network namespace, so every DB-backed test returns early while printing `ok`. Run backend tests with `dangerouslyDisableSandbox: true`, against host-network Postgres and Redis containers, with `max_connections=800`. A run reporting ~1354 passed is a run where nothing executed; the real baseline is 1391 plus whatever this slice adds.
- Do not touch the containers `sauron-postgres-1`, `sauron-redis-1` or `sauron-api-task12` — they belong to another session. Create your own ephemeral ones and remove them.
- The keyset predicate and the ORDER BY are **one mechanism split across two clauses**. Any change to one must be made to the other in the same edit; disagreement is how paging silently skips rows.

---

## File Structure

| File | Responsibility |
|---|---|
| `backend/crates/sauron-db/src/query_plan/cursor.rs` (modify) | `Cursor { key, value, id }`, `CursorValue`, encode/decode |
| `backend/crates/sauron-db/src/repo.rs` (modify) | `EventSort`, `OccurrenceSort`; keyset predicate and ORDER BY per column |
| `backend/bins/sauron-api/src/routes/analytics.rs` (modify) | Events sort whitelist; mint cursors with the sort key |
| `backend/bins/sauron-api/src/routes/issues.rs` (modify) | Occurrences sort whitelist; mint cursors with the sort key |
| `backend/bins/sauron-api/tests/http_search.rs` (modify) | Sort acceptance, key-mismatch rejection, paging stability |
| `dashboard/src/lib/api/events.ts`, `issues.ts` (modify) | Document each route's sort set |
| `dashboard/src/pages/Events.svelte`, `IssueDetail.svelte` (modify) | Wire `SortableTh` |

---

### Task 1: Generalise the cursor

**Files:**
- Modify: `backend/crates/sauron-db/src/query_plan/cursor.rs`
- Test: same file's `mod tests`

**Interfaces:**
- Produces:
  ```rust
  pub enum CursorValue { Ts(DateTime<Utc>), Text(String) }
  pub struct Cursor { pub key: String, pub value: CursorValue, pub id: Uuid }
  pub enum CursorError { Malformed, BadTimestamp, BadUuid, KeyMismatch { expected: String, got: String } }
  pub fn encode(c: &Cursor) -> String
  pub fn decode(s: &str, expected_key: &str) -> Result<Cursor, CursorError>
  ```

`decode` takes the key the caller is about to page by, so a mismatch cannot
reach a query. Making it a parameter rather than a check the caller remembers to
perform is the whole point of the change.

- [ ] **Step 1: Write the failing tests**

**Keep every existing test in `mod tests`.** Adapt each to the new `Cursor`
shape rather than deleting it — `decode` now parses strictly more structure
(two delimiters and a type tag instead of one delimiter), so its malformed-input
handling matters more than before, not less. In particular `is_url_safe`,
`rejects_garbage_rather_than_panicking` and `rejects_a_truncated_cursor` all
still apply and all need a second argument on `decode`. A cursor is attacker-
reachable in the sense that it arrives in a query string, so "rejects garbage
rather than panicking" is the test that keeps a 400 from becoming a 500.

Then update `sample()` and ADD the tests below:

```rust
    fn sample() -> Cursor {
        Cursor {
            key: "occurred_at".into(),
            value: CursorValue::Ts(Utc.with_ymd_and_hms(2026, 8, 9, 12, 30, 45).unwrap()),
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        }
    }

    #[test]
    fn round_trips_a_timestamp_cursor() {
        let c = sample();
        assert_eq!(decode(&encode(&c), "occurred_at").unwrap(), c);
    }

    #[test]
    fn round_trips_a_text_cursor() {
        let c = Cursor {
            key: "name".into(),
            value: CursorValue::Text("checkout|started".into()),
            id: sample().id,
        };
        // The delimiter appears INSIDE the value here on purpose: a text
        // cursor that split naively would truncate at the first `|` and page
        // from the wrong position.
        assert_eq!(decode(&encode(&c), "name").unwrap(), c);
    }

    #[test]
    fn refuses_a_cursor_minted_under_a_different_sort() {
        // The defect this exists to stop: a cursor is a position within ONE
        // ordering. Compared against another column it yields wrong rows and
        // HTTP 200, which nothing downstream can detect.
        let err = decode(&encode(&sample()), "name").unwrap_err();
        assert_eq!(
            err,
            CursorError::KeyMismatch {
                expected: "name".into(),
                got: "occurred_at".into()
            }
        );
    }

    #[test]
    fn preserves_sub_second_precision() {
        let c = Cursor {
            value: CursorValue::Ts(Utc.timestamp_micros(1_786_000_000_123_456).unwrap()),
            ..sample()
        };
        let CursorValue::Ts(ts) = decode(&encode(&c), "occurred_at").unwrap().value else {
            panic!("timestamp cursor decoded as text");
        };
        assert_eq!(CursorValue::Ts(ts), c.value);
    }

    #[test]
    fn an_empty_text_value_survives_the_round_trip() {
        // Nullable columns are coalesced to `""` before they reach the cursor,
        // so the empty string is a real position, not an absent one.
        let c = Cursor { key: "session_id".into(), value: CursorValue::Text(String::new()), id: sample().id };
        assert_eq!(decode(&encode(&c), "session_id").unwrap(), c);
    }
```

- [ ] **Step 2: Run and verify they fail**

Run: `cd backend && cargo test -p sauron-db --lib cursor`
Expected: compile errors — `Cursor` has no field `key`, `decode` takes 1 argument.

**`repo.rs` is in this same crate**, so once `Cursor` changes shape the whole
`sauron-db` lib target stops compiling — its three keyset-predicate blocks read
the removed `.ts` field. That is expected and is Task 2's work; it also means
this task's tests cannot be run through the normal crate target until Task 2
lands. Verify them by copying `cursor.rs` verbatim into a scratch crate with the
same dependency versions — the module has no `crate::`/`super::` references, so
the copy is faithful — and note in the report that the real gate is the run at
the end of Task 2.

Known breaking call sites, for Tasks 2 and 3: `repo.rs` (3 keyset blocks),
`routes/analytics.rs` (1 decode, 1 encode), `routes/issues.rs` (2 decode,
2 encode), and `tests/keyset_plan.rs` (2 struct literals).

- [ ] **Step 3: Implement**

Rewrite the body of `cursor.rs` (keep and extend the module doc):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorValue {
    Ts(DateTime<Utc>),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The sort column this position is a position WITHIN.
    pub key: String,
    pub value: CursorValue,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    Malformed,
    BadTimestamp,
    BadUuid,
    KeyMismatch { expected: String, got: String },
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CursorError::Malformed => f.write_str("cursor is not a valid pagination token"),
            CursorError::BadTimestamp => f.write_str("cursor timestamp is invalid"),
            CursorError::BadUuid => f.write_str("cursor id is invalid"),
            CursorError::KeyMismatch { expected, got } => write!(
                f,
                "this cursor pages a list sorted by `{got}`, but the request sorts by \
                 `{expected}`; start from the first page after changing the sort"
            ),
        }
    }
}

/// `<key>|<uuid>|<type>:<value>`, base64url without padding.
///
/// The value is LAST and unescaped so it may contain the delimiter — event
/// names and session ids routinely do. Key and id are fixed-shape and parse
/// off the front, leaving the remainder to be taken whole.
pub fn encode(c: &Cursor) -> String {
    let (ty, val) = match &c.value {
        CursorValue::Ts(ts) => ("t", ts.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
        CursorValue::Text(s) => ("s", s.clone()),
    };
    URL_SAFE_NO_PAD.encode(format!("{}|{}|{ty}:{val}", c.key, c.id))
}

/// Decode, and refuse a cursor minted under a sort other than `expected_key`.
pub fn decode(s: &str, expected_key: &str) -> Result<Cursor, CursorError> {
    let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| CursorError::Malformed)?;
    let text = String::from_utf8(bytes).map_err(|_| CursorError::Malformed)?;

    let (key, rest) = text.split_once('|').ok_or(CursorError::Malformed)?;
    let (id_s, payload) = rest.split_once('|').ok_or(CursorError::Malformed)?;
    let id = Uuid::parse_str(id_s).map_err(|_| CursorError::BadUuid)?;

    if key != expected_key {
        return Err(CursorError::KeyMismatch {
            expected: expected_key.to_string(),
            got: key.to_string(),
        });
    }

    let (ty, raw) = payload.split_once(':').ok_or(CursorError::Malformed)?;
    let value = match ty {
        "t" => CursorValue::Ts(
            DateTime::parse_from_rfc3339(raw)
                .map_err(|_| CursorError::BadTimestamp)?
                .with_timezone(&Utc),
        ),
        "s" => CursorValue::Text(raw.to_string()),
        _ => return Err(CursorError::Malformed),
    };

    Ok(Cursor { key: key.to_string(), value, id })
}
```

- [ ] **Step 4: Run and verify they pass**

Run: `cd backend && cargo test -p sauron-db --lib cursor`
Expected: PASS, 8 tests (5 new + the 3 kept). The rest of the crate will not compile yet — that
is Task 2.

- [ ] **Step 5: Prove the mismatch guard bites**

Delete the `if key != expected_key` block. Re-run: `refuses_a_cursor_minted_under_a_different_sort` must FAIL. Restore and confirm PASS.

---

### Task 2: Sort columns for Events and Occurrences

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `EventSearch` (near line 3348), `OccurrenceSearch` (near line 3565), and the keyset + ORDER BY blocks in each of their query functions.

**Interfaces:**
- Consumes: `Cursor`, `CursorValue` from Task 1.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum EventSort { OccurredAt, Name, DistinctId, SessionId }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum OccurrenceSort { OccurredAt, DistinctId, SessionId, DeviceKey }
  ```
  each with `fn from_column(col: &str) -> Option<Self>` and `fn column(self) -> &'static str`,
  and a new `pub sort: EventSort` / `pub sort: OccurrenceSort` field on the search struct.

**The nullable-column trap.** `session_id`, `device_key` and `error_events.distinct_id` are `Nullable`. A row comparison against `NULL` evaluates to `NULL`, not `true`, so a keyset predicate over a raw nullable column drops every null-valued row from every page after the first — silently, and only for the rows that have no session. Both the ORDER BY and the keyset predicate therefore use `COALESCE(col, '')`, which makes the ordering total. A genuinely empty-string value and a `NULL` then sort together; both mean "no session" in this data, and the alternative is losing rows.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/keyset_plan.rs`:

```rust
/// Paging a nullable sort column must reach rows whose value is NULL.
///
/// The defect: `WHERE (session_id, id) < ($1, $2)` is NULL — not true — for a
/// row with no session, so every such row vanishes from page two onward. It
/// looks like a short result set, not like a bug.
#[tokio::test]
async fn paging_by_session_reaches_rows_with_no_session() {
    let Some(mut h) = harness().await else { return };
    seed_events_with_some_null_sessions(&mut h, 40).await;

    let mut seen: Vec<Uuid> = Vec::new();
    let mut after: Option<sauron_db::query_plan::cursor::Cursor> = None;
    for _ in 0..20 {
        let page = page_events_by(&mut h, sauron_db::repo::EventSort::SessionId, after.clone(), 7).await;
        if page.is_empty() { break; }
        after = Some(cursor_from_last(&page, sauron_db::repo::EventSort::SessionId));
        seen.extend(page.iter().map(|r| r.id));
    }

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "a row was returned on more than one page");
    assert_eq!(seen.len(), 40, "paging by a nullable column lost rows with a NULL value");
}
```

Write `harness`, `seed_events_with_some_null_sessions`, `page_events_by` and
`cursor_from_last` alongside, following the existing helpers in that file —
`harness()` returns `None` when no database is reachable, matching the file's
existing early-return convention.

- [ ] **Step 2: Run and verify it fails**

Run (with `dangerouslyDisableSandbox: true`, containers up):
`cd backend && cargo test -p sauron-db --test keyset_plan paging_by_session -- --nocapture`
Expected: compile error — `EventSort` does not exist.

- [ ] **Step 3: Add the enums and thread them through**

In `repo.rs`, beside the existing `IssueSort` (near line 3081):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSort {
    OccurredAt,
    Name,
    DistinctId,
    SessionId,
}

impl EventSort {
    /// The column name as `routes/search.rs`' sort whitelist spells it.
    pub fn from_column(col: &str) -> Option<Self> {
        match col {
            "occurred_at" => Some(EventSort::OccurredAt),
            "name" => Some(EventSort::Name),
            "distinct_id" => Some(EventSort::DistinctId),
            "session_id" => Some(EventSort::SessionId),
            _ => None,
        }
    }

    pub fn column(self) -> &'static str {
        match self {
            EventSort::OccurredAt => "occurred_at",
            EventSort::Name => "name",
            EventSort::DistinctId => "distinct_id",
            EventSort::SessionId => "session_id",
        }
    }

    /// Whether the cursor for this column carries a timestamp or text.
    pub fn is_temporal(self) -> bool {
        matches!(self, EventSort::OccurredAt)
    }
}
```

and the same shape for `OccurrenceSort { OccurredAt, DistinctId, SessionId, DeviceKey }`,
whose `is_temporal` is true only for `OccurredAt`.

Add `pub sort: EventSort` to `EventSearch` and `pub sort: OccurrenceSort` to
`OccurrenceSearch`, documented as: *"The ordering this page walks. The cursor in
`after` must have been minted under the same column — `cursor::decode` enforces
it at the route."*

- [ ] **Step 4: Extend the keyset predicate and ORDER BY together**

Follow the existing `IssueSort` match at `repo.rs:3240-3272` exactly: one arm
per `(column, direction)` in the predicate, and the mirror set in the ORDER BY,
with the existing comment about the two clauses being one mechanism kept.

For the nullable text columns use diesel's `sql` fragment for the coalesce so
predicate and ordering share one spelling.

**The raw fragment MUST be wrapped in its own parentheses.** Diesel composes a
filter as `And(existing, predicate)` and does *not* group the predicate, while
`SqlLiteral` emits its text verbatim. A fragment containing a top-level `OR`
therefore escapes the WHERE clause, because `AND` binds tighter:

```sql
-- what an UNPARENTHESISED fragment actually produces
WHERE (app_id = $1 AND occurred_at >= $2 AND <env> AND COALESCE(session_id,'') < $3
       OR (COALESCE(session_id,'') = $4 AND id < $5))
```

The second disjunct carries no tenant key, no environment filter and no time
window, so every page after the first can return rows belonging to other apps.
This is a cross-tenant leak, and it is invisible to any test whose fixture holds
a single app. Open with `(` and close with `))`:

```rust
(EventSort::SessionId, true) => q.filter(
    sql::<Bool>("(COALESCE(session_id,'') < ")
        .bind::<Text, _>(text_of(&c))
        .sql(" OR (COALESCE(session_id,'') = ")
        .bind::<Text, _>(text_of(&c))
        .sql(" AND id < ")
        .bind::<SqlUuid, _>(c.id)
        .sql("))"),
),
```

Pin it with `debug_query::<Pg, _>` — `query_plan/events.rs:1046` already uses
that idiom — and with a paging test whose fixture holds **two** apps.

`sql::<Bool>` for the predicate and `sql::<Text>` for the ORDER BY fragment;
`sql::<()>` does not satisfy `TypedExpressionType` on diesel 2.3.11.

with the matching order `sql::<()>("COALESCE(session_id,'') DESC, id DESC")`.
`text_of(&Cursor) -> String` extracts `CursorValue::Text`, returning an empty
string for a `Ts` — unreachable, because the route pairs the sort column with
its cursor type, and it keeps this function total.

- [ ] **Step 5: Run and verify the test passes**

Run: `cd backend && cargo test -p sauron-db --test keyset_plan -- --nocapture`
Expected: PASS, including the two pre-existing index tests.

- [ ] **Step 6: Prove the coalesce is what makes it pass**

Change one `COALESCE(session_id,'')` to a bare `session_id` in the predicate
only. Re-run: `paging_by_session_reaches_rows_with_no_session` must FAIL with
fewer than 40 rows seen. Restore and confirm PASS.

---

### Task 3: Route wiring — whitelists, decode, mint

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs:344` (Events) and `:396` (mint)
- Modify: `backend/bins/sauron-api/src/routes/issues.rs:517` (Occurrences) and `:574` (mint)

**Interfaces:**
- Consumes: `parse_sort`, `EventSort`, `OccurrenceSort`, `cursor::decode(s, key)`.

- [ ] **Step 1: Widen the whitelists**

`analytics.rs:344`:

```rust
    let (sort_col, descending) = super::search::parse_sort(
        q.sort.as_deref(),
        &["occurred_at", "name", "distinct_id", "session_id"],
        "occurred_at",
    )?;
    // `parse_sort` already refused anything outside the list, so this cannot be
    // None; the expect states that rather than inventing a fallback ordering
    // that would page unstably if the two lists ever drifted apart.
    let sort = sauron_db::repo::EventSort::from_column(&sort_col)
        .expect("parse_sort whitelist and EventSort::from_column must agree");
```

`issues.rs:517` the same, with
`&["occurred_at", "distinct_id", "session_id", "device_key"]` and `OccurrenceSort`.

- [ ] **Step 2: Pass the sort key to `decode`**

At each of the two decode sites, pass `&sort_col`:

```rust
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            sauron_db::query_plan::cursor::decode(c, &sort_col)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };
```

Keep whatever error mapping is already there; only the extra argument is new.

- [ ] **Step 3: Mint the cursor from the sorted column**

At each mint site, build the value from the column actually sorted, not from
`occurred_at` unconditionally — by calling the `cursor_value` method Task 2 put
on each enum:

```rust
    let next_cursor = has_more.then(|| {
        let last = rows.last().expect("has_more implies a row");
        sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
            key: sort_col.clone(),
            value: sort.cursor_value(last),
            id: last.id,
        })
    });
```

**Do not inline a `match` over the sort column here.** `cursor_value` already
coalesces each nullable column to `""` to match the ORDER BY and the keyset
predicate, and that rule needs exactly one spelling. Writing it a second time at
the route is how the two copies drift — and the failure when they drift is a
cursor aimed at a position the query cannot express, which pages wrongly with no
error.

The Occurrences mint is the identical three lines over `OccurrenceSort`.

- [ ] **Step 4: Add the HTTP tests**

Add to `backend/bins/sauron-api/tests/http_search.rs`:

```rust
#[tokio::test]
async fn events_sort_by_name_orders_and_pages() {
    let Some(app) = app().await else { return };
    seed_named_events(&app, &["delta", "alpha", "charlie", "bravo"]).await;

    let first = get_json(&app, "/v1/apps/{app_id}/events?sort=-name&limit=2").await;
    assert_eq!(names(&first), ["alpha", "bravo"]);

    let cursor = first["next_cursor"].as_str().expect("a second page exists");
    let second = get_json(&app, &format!("/v1/apps/{{app_id}}/events?sort=-name&limit=2&cursor={cursor}")).await;
    assert_eq!(names(&second), ["charlie", "delta"]);
}

#[tokio::test]
async fn a_cursor_from_another_sort_is_refused() {
    let Some(app) = app().await else { return };
    seed_named_events(&app, &["a", "b", "c"]).await;

    let first = get_json(&app, "/v1/apps/{app_id}/events?sort=-name&limit=1").await;
    let cursor = first["next_cursor"].as_str().unwrap().to_string();

    // Same cursor, different sort. Before the key was embedded this returned
    // 200 and a page from the wrong position in the wrong ordering.
    let res = get(&app, &format!("/v1/apps/{{app_id}}/events?sort=occurred_at&cursor={cursor}")).await;
    assert_eq!(res.status(), 400);
    assert!(body_text(res).await.contains("start from the first page"));
}

#[tokio::test]
async fn an_unlisted_sort_column_is_refused() {
    let Some(app) = app().await else { return };
    let res = get(&app, "/v1/apps/{app_id}/events?sort=properties").await;
    assert_eq!(res.status(), 400);
}
```

Use the file's existing fixture helpers rather than the placeholder names above
where equivalents already exist; `{app_id}` stands for whatever that file's
harness substitutes.

- [ ] **Step 5: Run the API suite**

Run (with `dangerouslyDisableSandbox: true`): `cd backend && cargo test -p sauron-api --test http_search`
Expected: PASS, 32 tests (29 existing + 3 new).

- [ ] **Step 6: Run the whole backend suite**

Run (with `dangerouslyDisableSandbox: true`): `cd backend && cargo test --workspace`
Expected: PASS. Confirm the total is at or above 1391 — a run near 1354 means
the database was unreachable and nothing actually executed.

---

### Task 4: Wire the two pages

**Files:**
- Modify: `dashboard/src/lib/api/events.ts:6`, `dashboard/src/lib/api/issues.ts:67` — document each route's sort set
- Modify: `dashboard/src/pages/Events.svelte`, `dashboard/src/pages/IssueDetail.svelte`

**Interfaces:**
- Consumes: `SortableTh`, `setCursorSort`, `sortParam`, `CursorListState` from slice 1.

- [ ] **Step 1: Replace the separate sort and page state**

In `Events.svelte`, replace the existing page state with the combined one:

```ts
  import SortableTh from '../lib/components/SortableTh.svelte';
  import { setCursorSort, type CursorListState } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';
  import { emptyPage } from '../lib/models/cursor-page';

  let list = $state<CursorListState>({
    sort: { key: 'occurred_at', dir: 'desc' },
    page: emptyPage(),
  });

  function onsort(key: string, columnDefault: SortDir) {
    // `setCursorSort` returns the reset page with the new sort, so the walk
    // cannot survive a sort change — see list-state.ts.
    list = setCursorSort(list, key, columnDefault);
  }
```

Every existing read of the page state becomes `list.page`, and the request
gains `sort: sortParam(list.sort)`.

- [ ] **Step 2: Add `sort` to the view cache key**

Find the `viewKey('events.stream', …)` call and add the sort:

```ts
  const key = viewKey('events.stream', appId, filterList, q.trim(), days, sortParam(list.sort), cursorOf(list.page));
```

Omitting it would serve the previous ordering's rows straight from the cache on
a sort click, with no request on the wire to notice.

- [ ] **Step 3: Make the headers sortable**

```svelte
  {#snippet head()}
    <tr>
      <SortableTh key="name" columnDefault="asc" sort={list.sort} {onsort}>Event</SortableTh>
      <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>User</SortableTh>
      <SortableTh key="session_id" columnDefault="asc" sort={list.sort} {onsort}>Session</SortableTh>
      <th>Properties</th>
      <SortableTh key="occurred_at" sort={list.sort} {onsort}>Time</SortableTh>
    </tr>
  {/snippet}
```

Properties stays a plain `<th>`: it renders a JSON blob, which has no order.

- [ ] **Step 4: Do the same for `IssueDetail.svelte`'s occurrences table**

```svelte
  <SortableTh key="occurred_at" sort={list.sort} {onsort}>Time</SortableTh>
  <SortableTh key="distinct_id" columnDefault="asc" sort={list.sort} {onsort}>User</SortableTh>
  <SortableTh key="session_id" columnDefault="asc" sort={list.sort} {onsort}>Session</SortableTh>
  <SortableTh key="device_key" columnDefault="asc" sort={list.sort} {onsort}>Device</SortableTh>
```

with the same state replacement, cache-key addition and `onsort` handler.

- [ ] **Step 5: Document the sort sets in the API clients**

In `events.ts` and `issues.ts`, update the doc comment above each list function
to name that route's accepted columns, so the anti-drift habit the repo already
follows for the catalog holds here too.

- [ ] **Step 6: Verify types and tests**

Run: `npm --prefix dashboard run check` — expected 0 errors.
Run: `npm --prefix dashboard test` — expected PASS.

- [ ] **Step 7: Verify in the running app**

`preview_start` the dashboard, open Events, and confirm with `read_page` and
`read_network_requests`:
- clicking "Event" issues a request carrying `sort=-name` and no `cursor`;
- the rows come back A-Z and the header reads `aria-sort="ascending"`;
- clicking Next carries both `sort=-name` and a cursor, and the next page continues A-Z;
- clicking "Time" while on page 2 drops the cursor from the request and returns to page 1;
- no console errors.

Repeat on an issue's Occurrences table.

---

## Done when

- `cargo test --workspace` passes at ≥1391 tests, run with the sandbox disabled against a reachable database.
- `npm --prefix dashboard test` and `run check` pass.
- Both sabotage checks (Task 1 step 5, Task 2 step 6) were run and observed to fail.
- Events and Occurrences sort and page correctly in the running app.
- `Issues.svelte` is untouched.
- Nothing is committed and no branch was created.
