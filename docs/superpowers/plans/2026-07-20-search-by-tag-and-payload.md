# Search by tag & payload content — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user filter Issues and analytics Events by a structured tag (`key=value`, `eq`/`contains`) and find rows by free-text content anywhere in the `tags`/`contexts`/`extra` (and event `properties`) JSONB, reusing the existing FilterBar + `q` search box.

**Architecture:** Extend the existing whitelisted `field:op:value` filter model with a new `tag` field type. Backend adds tag match-arms (index-backed `@>` for `eq`, `->> … ILIKE` for `contains`) and broadens the free-text `q` predicate to also `::text ILIKE` the JSONB columns — all via injection-safe bound parameters. A GIN `jsonb_path_ops` index on `tags` backs the `eq` path. Frontend adds a `tag` FieldType rendered as two inputs (key + value) that compose into the existing `key=value` value slot.

**Tech Stack:** Rust (diesel/diesel-async, `sauron-db`), Postgres (JSONB, GIN), Svelte 5 / TypeScript dashboard.

**Design spec:** `docs/superpowers/specs/2026-07-20-search-by-tag-and-payload-design.md`

## Global Constraints

- **Injection safety is mandatory.** The tag *key*, tag *value*, and free-text term are dynamic — they MUST be bound parameters (`.bind::<Text,_>` / `.bind::<Jsonb,_>` / a pre-built `%term%` pattern), never interpolated into SQL text. Follow the `repo::list_persons` precedent (`repo.rs:1300-1333`): SQL string has positional binds only.
- **Filter wire format is unchanged:** `field:op:value`, value `encodeURIComponent`-encoded. A tag filter is `tag:<op>:<key>=<value>` (e.g. `tag:eq:region=eu` → encoded `tag:eq:region%3Deu`). Backend `parse_filters` splits the value on the **first** `=`.
- **Tag ops are `eq` and `contains` only.** `eq` → `tags @> {key:value}` (index-backed). `contains` → `tags ->> key ILIKE '%value%'`.
- **Free-text `q` stays a bounded scan** (app_id + date window + row cap) — no trigram/tsvector, matching the documented house style (`wiki/Search.md`).
- **GIN index** goes on the **partitioned parent** tables only (propagates to partitions), migration dir sorts after `2026-07-20-000017`.
- **Dev `contexts` (plural) ≠ machine `context` (singular).** Only `tags`/`contexts`/`extra`/`properties` are searched; never `context`.
- **Scope:** Issues list + analytics Events page (both already have a FilterBar), **plus** a new filterable occurrences list on the issue-detail page (Tasks 8-9 — `IssueDetail.svelte` today renders only the latest event, so the occurrences list is built new).

---

### Task 1: `tag` field type + whitelist entry + parse validation (filter.rs)

**Files:**
- Modify/Test: `backend/crates/sauron-db/src/filter.rs`

**Interfaces:**
- Consumes: existing `Op`, `FieldSpec`, `FieldType { Str, Enum, Num }`, `parse_filters`, `ISSUE_FILTERS`, `EVENT_FILTERS`.
- Produces: `FieldType::Tag`; a `tag` `FieldSpec` (ops `[Eq, Contains]`) in both `ISSUE_FILTERS` and `EVENT_FILTERS`; `parse_filters` accepts `tag:eq:key=value` and rejects a value with no `=` or an empty key/value with `FilterError::BadValue`. `ParsedFilter.value` keeps the raw `key=value` string (repo splits it).

- [ ] **Step 1: Write failing tests.** Append to the `tests` module in `filter.rs`:
```rust
    #[test]
    fn parses_tag_filter() {
        let got = parse_filters(&["tag:eq:region=eu".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got, vec![ParsedFilter { field: "tag", op: Op::Eq, value: "region=eu".into() }]);
        let got2 = parse_filters(&["tag:contains:feature=check".to_string()], EVENT_FILTERS).unwrap();
        assert_eq!(got2[0].field, "tag");
        assert_eq!(got2[0].op, Op::Contains);
        assert_eq!(got2[0].value, "feature=check");
    }

    #[test]
    fn tag_value_keeps_extra_equals() {
        // Only the FIRST '=' splits key/value; the rest belongs to the value.
        let got = parse_filters(&["tag:eq:expr=a=b".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got[0].value, "expr=a=b");
    }

    #[test]
    fn rejects_tag_without_equals() {
        assert!(matches!(
            parse_filters(&["tag:eq:region".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_tag_empty_key_or_value() {
        assert!(matches!(
            parse_filters(&["tag:eq:=eu".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
        assert!(matches!(
            parse_filters(&["tag:eq:region=".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_tag_disallowed_op() {
        assert!(matches!(
            parse_filters(&["tag:gt:region=eu".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadOp { .. })
        ));
    }
```

- [ ] **Step 2: Run — expect FAIL (compile error: `Tag` variant missing / `tag` not in whitelist).**
```
cargo test -p sauron-db filter::
```
Expected: compile error `no variant named Tag` / the tag tests fail with `UnknownField`.

- [ ] **Step 3: Add the `Tag` field type.** In `filter.rs`, change the enum (line 24):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType { Str, Enum, Num, Tag }
```

- [ ] **Step 4: Validate `Tag` values in `parse_filters`.** In the `match spec.ty { … }` block (lines 90-100), add a `Tag` arm after the `Str` arm:
```rust
            FieldType::Str => {}
            FieldType::Tag => {
                // Value is `key=value`; split on the FIRST '=' and require both sides.
                match value.split_once('=') {
                    Some((k, v)) if !k.is_empty() && !v.is_empty() => {}
                    _ => return Err(FilterError::BadValue { field: field.to_string() }),
                }
            }
```

- [ ] **Step 5: Register the `tag` field in both whitelists.** Add a shared ops const near line 108 and a `tag` `FieldSpec` to each registry:
```rust
const OPS_TAG: &[Op] = &[Op::Eq, Op::Contains];
```
Append to `ISSUE_FILTERS` (before the closing `];` at line 118):
```rust
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
```
Append to `EVENT_FILTERS` (before the closing `];` at line 128):
```rust
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
```

- [ ] **Step 6: Run — expect PASS.**
```
cargo test -p sauron-db filter::
```
Expected: `test result: ok.` including the five new tag tests and all pre-existing filter tests.

- [ ] **Step 7: Commit.**
```
git add backend/crates/sauron-db/src/filter.rs
git commit -m "feat(db): add tag field type + key=value validation to the filter whitelist"
```

---

### Task 2: GIN index migration on `tags`

**Files:**
- Create: `backend/migrations/2026-07-20-000018_tags_gin/up.sql`
- Create: `backend/migrations/2026-07-20-000018_tags_gin/down.sql`

**Interfaces:**
- Consumes: partitioned parents `error_events` / `analytics_events`, both with a `tags JSONB` column. `MIGRATIONS` embed macro in `sauron-db/src/lib.rs` picks up new dirs at compile time.
- Produces: GIN `jsonb_path_ops` indexes `error_events_tags_gin` / `analytics_events_tags_gin` backing the `tags @> …` tag-`eq` path.

- [ ] **Step 1: Create `up.sql`.** `backend/migrations/2026-07-20-000018_tags_gin/up.sql`:
```sql
-- 0018: GIN indexes on the dev-supplied `tags` JSONB, defined on the
-- partitioned PARENT tables so they propagate to the default partition and
-- every range partition (same rule the existing B-tree indexes follow).
-- jsonb_path_ops is the smaller/faster GIN opclass for `@>` containment, which
-- backs the structured tag-`eq` filter (`tags @> '{"key":"value"}'`).
CREATE INDEX error_events_tags_gin     ON error_events     USING gin (tags jsonb_path_ops);
CREATE INDEX analytics_events_tags_gin ON analytics_events USING gin (tags jsonb_path_ops);
```

- [ ] **Step 2: Create `down.sql`.** `backend/migrations/2026-07-20-000018_tags_gin/down.sql`:
```sql
DROP INDEX IF EXISTS analytics_events_tags_gin;
DROP INDEX IF EXISTS error_events_tags_gin;
```

- [ ] **Step 3: Verify the migration embeds and the crate compiles.**
```
cargo build -p sauron-db
```
Expected: `Finished` (the `embed_migrations!` macro validates the new dir at compile time).

- [ ] **Step 4: (If a local Postgres is available) apply + revert + reapply to confirm idempotency.** Skip if no DB — Step 3 already proves embedding.
```
# with DATABASE_URL set to a scratch DB:
cargo run -p sauron-migrate -- up && cargo run -p sauron-migrate -- down && cargo run -p sauron-migrate -- up
```
Expected: no errors; `\d error_events` shows `error_events_tags_gin` propagated.

- [ ] **Step 5: Commit.**
```
git add backend/migrations/2026-07-20-000018_tags_gin
git commit -m "feat(db): GIN jsonb_path_ops index on tags for error_events + analytics_events"
```

---

### Task 3: Tag + payload search in `list_analytics_events` (repo.rs)

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (imports ~1-15; `list_analytics_events` 1440-1509)

**Interfaces:**
- Consumes: `ParsedFilter`, `Op`, `like_contains`, the boxed `analytics_events::table` query.
- Produces: a private `tag_kv(&str) -> (String, String)` helper (reused by Task 4); `("tag", Op::Eq|Op::Contains)` arms and a broadened `q` predicate on `list_analytics_events`.

- [ ] **Step 1: Add the diesel `sql` + sql-type imports.** In `repo.rs`, extend the existing `use diesel::sql_types::{…}` (line 6-8) to include `Jsonb` (already present) and add a `use` for the sql fragment builder after line 9:
```rust
use diesel::dsl::sql;
```
(`diesel::sql_types::{Bool, Jsonb, Text}` are already imported on lines 6-8.)

- [ ] **Step 2: Add the `tag_kv` helper.** Place it just above `list_analytics_events` (before line 1440):
```rust
/// Split a `parse_filters`-validated tag value (`key=value`) on the first `=`.
/// The value slot always contains exactly one leading `key=`, guaranteed by
/// `FieldType::Tag` validation, so the `None` arm is defensive only.
fn tag_kv(value: &str) -> (String, String) {
    match value.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (value.to_string(), String::new()),
    }
}

/// A single-key JSONB object `{key: value}` for a `tags @> …` containment bind.
fn tag_object(key: String, value: String) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(key, serde_json::Value::String(value));
    serde_json::Value::Object(m)
}
```

- [ ] **Step 3: Add the tag arms.** In `list_analytics_events`, inside the `for f in filters { query = match (f.field, f.op) { … } }` block (lines 1468-1482), add before the `_ => query,` arm:
```rust
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(sql::<Bool>("analytics_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("analytics_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
```

- [ ] **Step 4: Broaden the free-text `q` predicate.** Replace the `if let Some(term) = q { … }` block (lines 1495-1501) with:
```rust
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            analytics_events::name
                .ilike(p.clone())
                .or(analytics_events::distinct_id.ilike(p.clone()))
                .or(sql::<Bool>("analytics_events.contexts::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.properties::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
```

- [ ] **Step 5: Compile.** (The gate for query changes — the repo has no DB-backed test harness; `filter.rs` parse tests + compilation are the guard.)
```
cargo build -p sauron-db && cargo test -p sauron-db
```
Expected: `Finished`, then `test result: ok.`. If the boxed-builder + `sql()` fragment fails type inference on `.or(...)`, keep each `sql::<Bool>(...)` fragment as written (it is a valid `BoolExpression`); if it still fights, wrap the whole `q` disjunction in a `diesel::sql_query`-style raw query following `list_persons` (repo.rs:1300-1333) — semantics unchanged, still bound params.

- [ ] **Step 6: Commit.**
```
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): search analytics events by tag (@>/->>) and payload text (contexts/extra/properties/tags)"
```

---

### Task 4: Tag + payload search in `list_issues` via EXISTS subquery (repo.rs)

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (`list_issues` 552-601)

**Interfaces:**
- Consumes: `tag_kv`, `tag_object`, `sql`, `like_contains` from Task 3; the boxed `issues::table` query. Tags live on child `error_events`, so predicates are `EXISTS` subqueries correlated on `error_events.issue_id = issues.id` (uses the `error_events_issue_idx` index; the tag `@>` uses the new GIN).
- Produces: `("tag", Op::Eq|Op::Contains)` arms + broadened `q` on `list_issues`.

- [ ] **Step 1: Add the tag arms.** In `list_issues`, inside the `for f in filters { query = match (f.field, f.op) { … } }` block (lines 566-583), add before the `_ => query,` arm:
```rust
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags @> ",
                    )
                    .bind::<Jsonb, _>(tag_object(k, v))
                    .sql(")"),
                )
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags ->> ",
                    )
                    .bind::<Text, _>(k)
                    .sql(" ILIKE ")
                    .bind::<Text, _>(like_contains(&v))
                    .sql(")"),
                )
            }
```

- [ ] **Step 2: Broaden the free-text `q` predicate.** Replace the `if let Some(term) = q { … }` block (lines 586-593) with:
```rust
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            issues::title
                .ilike(p.clone())
                .or(issues::type_.ilike(p.clone()))
                .or(issues::culprit.ilike(p.clone()))
                .or(sql::<Bool>(
                    "EXISTS (SELECT 1 FROM error_events e \
                     WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                     AND (e.contexts::text ILIKE ",
                )
                .bind::<Text, _>(p.clone())
                .sql(" OR e.extra::text ILIKE ")
                .bind::<Text, _>(p.clone())
                .sql(" OR e.tags::text ILIKE ")
                .bind::<Text, _>(p)
                .sql("))")),
        );
    }
```

- [ ] **Step 3: Compile + test.**
```
cargo build -p sauron-db && cargo test -p sauron-db
```
Expected: `Finished`, then `test result: ok.`. Same fallback note as Task 3 Step 5 if the `.or()` composition needs adjustment.

- [ ] **Step 4: Full backend build (the API crate consumes these).**
```
cargo build -p sauron-api
```
Expected: `Finished` — the handlers already pass `filters`/`q`; no handler change is needed for Issues/Events because `tag` is now whitelisted (Task 1) and handled (Tasks 3-4).

- [ ] **Step 5: Commit.**
```
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): search issues by tag and payload text via EXISTS into error_events"
```

---

### Task 5: `tag` FieldType + field registration (dashboard filters.ts)

**Files:**
- Modify/Test: `dashboard/src/lib/components/filters/filters.ts`
- Test: `dashboard/src/lib/components/filters/filters.test.ts` (create if absent)

**Interfaces:**
- Consumes: `Op`, `FieldType`, `FieldDef`, `Filter`, `encodeFilters`, `parseFilters`, `ISSUE_FIELDS`, `EVENT_FIELDS`.
- Produces: `FieldType` includes `'tag'`; a `{ key:'tag', label:'Tag', type:'tag', ops:['eq','contains'] }` field in both registries; `composeTag`/`splitTag` helpers.

- [ ] **Step 1: Write failing tests.** Create `dashboard/src/lib/components/filters/filters.test.ts`:
```ts
import { describe, it, expect } from 'vitest';
import {
  encodeFilters, parseFilters, ISSUE_FIELDS, EVENT_FIELDS, composeTag, splitTag,
} from './filters';

describe('tag filter', () => {
  it('round-trips a tag filter through encode/parse', () => {
    const f = [{ field: 'tag', op: 'eq' as const, value: 'region=eu' }];
    const enc = encodeFilters(f);
    expect(enc).toEqual(['tag:eq:region%3Deu']);
    expect(parseFilters(enc, ISSUE_FIELDS)).toEqual(f);
    expect(parseFilters(enc, EVENT_FIELDS)).toEqual(f);
  });

  it('composeTag/splitTag are inverse', () => {
    expect(composeTag('region', 'eu')).toBe('region=eu');
    expect(splitTag('region=eu')).toEqual({ key: 'region', value: 'eu' });
    expect(splitTag('expr=a=b')).toEqual({ key: 'expr', value: 'a=b' });
    expect(splitTag('nope')).toEqual({ key: '', value: '' });
  });

  it('both registries expose a tag field with eq+contains', () => {
    for (const reg of [ISSUE_FIELDS, EVENT_FIELDS]) {
      const tag = reg.find((d) => d.key === 'tag');
      expect(tag?.type).toBe('tag');
      expect(tag?.ops).toEqual(['eq', 'contains']);
    }
  });
});
```

- [ ] **Step 2: Run — expect FAIL.**
```
cd dashboard && npx vitest run src/lib/components/filters/filters.test.ts
```
Expected: fail — `composeTag`/`splitTag` are not exported; no `tag` field.

- [ ] **Step 3: Add `'tag'` to `FieldType` and the helpers.** Edit `filters.ts` line 2:
```ts
export type FieldType = 'enum' | 'string' | 'number' | 'tag';
```
Add after `parseFilters` (after line 43):
```ts
/** Compose a tag key + value into the single `key=value` filter value slot. */
export function composeTag(key: string, value: string): string {
  return `${key}=${value}`;
}

/** Split a `key=value` tag filter value on the first `=` (inverse of composeTag). */
export function splitTag(v: string): { key: string; value: string } {
  const i = v.indexOf('=');
  if (i <= 0 || i === v.length - 1) return { key: '', value: '' };
  return { key: v.slice(0, i), value: v.slice(i + 1) };
}
```

- [ ] **Step 4: Register the `tag` field in both registries.** Add `OPS_TAG` near line 47 and a tag entry to each list:
```ts
const OPS_TAG: Op[] = ['eq', 'contains'];
```
Append to `ISSUE_FIELDS` (before its closing `];`, line 56):
```ts
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
```
Append to `EVENT_FIELDS` (before its closing `];`, line 65):
```ts
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
```

- [ ] **Step 5: Run — expect PASS.**
```
cd dashboard && npx vitest run src/lib/components/filters/filters.test.ts
```
Expected: all tag tests pass.

- [ ] **Step 6: Commit.**
```
git add dashboard/src/lib/components/filters/filters.ts dashboard/src/lib/components/filters/filters.test.ts
git commit -m "feat(dashboard): add tag field type + composeTag/splitTag and register on issues/events"
```

---

### Task 6: Two-input tag render in FilterBar.svelte

**Files:**
- Modify: `dashboard/src/lib/components/filters/FilterBar.svelte`

**Interfaces:**
- Consumes: `composeTag`, `splitTag` from Task 5; existing `draftField`, `draftOp`, `draftValue`, `fieldDef`, `commit()`.
- Produces: when the draft field's type is `tag`, two inputs (key + value) whose combined `key=value` is committed as the filter value.

- [ ] **Step 1: Import the helpers + add draft key/value state.** In the `<script>`, extend the filters import:
```ts
  import { OP_LABEL, composeTag, splitTag, type FieldDef, type Filter, type Op } from './filters';
```
Add two state vars after `draftValue` (line 26):
```ts
  let draftTagKey = $state('');
  let draftTagVal = $state('');
```

- [ ] **Step 2: Reset the tag inputs when the field changes.** In `openAdd()` and `onFieldChange()`, after the existing `draftValue = …` line, add:
```ts
    draftTagKey = '';
    draftTagVal = '';
```

- [ ] **Step 3: Compose the tag value on commit.** Replace `commit()` (lines around 38-42) with:
```ts
  function commit() {
    if (fieldDef?.type === 'tag') {
      if (!draftTagKey.trim() || !draftTagVal.trim()) return;
      filters = [...filters, { field: draftField, op: draftOp, value: composeTag(draftTagKey.trim(), draftTagVal.trim()) }];
      adding = false;
      return;
    }
    if (!draftField || draftValue === '') return;
    filters = [...filters, { field: draftField, op: draftOp, value: draftValue }];
    adding = false;
  }
```

- [ ] **Step 4: Render two inputs for the tag type.** In the draft-value block (lines 75-83), add a `tag` branch as the first `{#if}` case:
```svelte
        {#if fieldDef?.type === 'tag'}
          <input type="text" bind:value={draftTagKey} placeholder="key" aria-label="Tag key" class="tag-key" />
          <span class="tag-eq">=</span>
          <input type="text" bind:value={draftTagVal} placeholder="value" aria-label="Tag value" class="tag-val" />
        {:else if fieldDef?.type === 'enum'}
          <select bind:value={draftValue} aria-label="Value">
            {#each fieldDef?.options ?? [] as opt (opt)}<option value={opt}>{opt}</option>{/each}
          </select>
        {:else if fieldDef?.type === 'number'}
          <input type="number" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {:else}
          <input type="text" bind:value={draftValue} placeholder="value" aria-label="Value" />
        {/if}
```

- [ ] **Step 5: Add styles for the tag inputs.** In the `<style>` block, after the `.draft input { width: 130px; }` rule, add:
```css
  .draft input.tag-key { width: 90px; }
  .draft input.tag-val { width: 110px; }
  .tag-eq { opacity: 0.6; }
```

- [ ] **Step 6: Typecheck the dashboard.**
```
cd dashboard && npm run check
```
Expected: `0 errors`.

- [ ] **Step 7: Commit.**
```
git add dashboard/src/lib/components/filters/FilterBar.svelte
git commit -m "feat(dashboard): two-input tag (key + value) filter in FilterBar"
```

---

### Task 7: Docs + search-box hint

**Files:**
- Modify: `wiki/Search.md`
- Modify: `dashboard/src/pages/Issues.svelte` (FilterBar usage — the `<SearchInput>` placeholder is inside FilterBar; add a help line near the page's filter usage)
- Modify: `dashboard/src/pages/Events.svelte`

**Interfaces:**
- Consumes: nothing new. Documentation + a one-line UI hint that `q` now searches payload and that a `Tag` filter exists.

- [ ] **Step 1: Document the new capability in `wiki/Search.md`.** Add a section describing: the `Tag` filter (`key=value`, `=`/`contains`, `@>`/`->>` semantics, GIN-backed for `=`); that the free-text search now also scans `tags`/`contexts`/`extra`/`properties` as a bounded `::text ILIKE` (same unindexed-scan caveat as the existing person search). Keep the tone/format of the existing file (read it first; mirror its headings).

- [ ] **Step 2: Add a short help hint on the Issues and Events pages.** Near each page's `<FilterBar …>` usage, add a muted one-liner (match the page's existing help/subtitle style), e.g.:
```svelte
<p class="filter-hint">Filter by <code>Tag</code> (key = value); the search box also matches tag &amp; payload content.</p>
```
Add a `.filter-hint { font-size: 12px; color: var(--text-muted); margin: -4px 0 8px; }` rule if the page has no equivalent muted-hint class.

- [ ] **Step 3: Typecheck.**
```
cd dashboard && npm run check
```
Expected: `0 errors`.

- [ ] **Step 4: Commit.**
```
git add wiki/Search.md dashboard/src/pages/Issues.svelte dashboard/src/pages/Events.svelte
git commit -m "docs: document tag + payload search; add filter hint to Issues/Events"
```

---

### Task 8: Backend occurrence filtering (`list_error_events_for_issue` + issue-events handler)

**Files:**
- Modify: `backend/crates/sauron-db/src/filter.rs`
- Modify: `backend/crates/sauron-db/src/repo.rs` (`list_error_events_for_issue` 649-661)
- Modify: `backend/bins/sauron-api/src/routes/issues.rs` (`EventsQuery` 123-131, `events` handler 133-159)

**Interfaces:**
- Consumes: `tag_kv`, `tag_object`, `sql`, `like_contains` (Task 3); `Op`, `FieldSpec`, `FieldType::Tag`, `OPS_TAG`, `NO_OPTS` (Task 1); `parse_filters`.
- Produces: `ERROR_EVENT_FILTERS: &[FieldSpec]` (just `tag`); `list_error_events_for_issue(conn, issue_id, filters, q, since, limit)`; the `events` handler parses `filter`/`q`/`since_days` and passes them through.

- [ ] **Step 1: Add the occurrence filter whitelist.** In `filter.rs`, after `EVENT_FILTERS` (line 128), add:
```rust
// Per-error-event occurrences (issue detail). Only the developer `tag` is
// filterable per-occurrence; issue-group fields (level/status/...) live on the
// issue, not the individual event.
pub const ERROR_EVENT_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
];
```

- [ ] **Step 2: Rewrite `list_error_events_for_issue` to a boxed, filterable query.** Replace lines 649-661 in `repo.rs`:
```rust
pub async fn list_error_events_for_issue(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let mut query = error_events::table
        .filter(error_events::issue_id.eq(issue_id))
        .into_boxed();
    if let Some(s) = since {
        query = query.filter(error_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(sql::<Bool>("error_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("error_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
            _ => query,
        };
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            error_events::message
                .ilike(p.clone())
                .or(error_events::exception_value.ilike(p.clone()))
                .or(error_events::exception_type.ilike(p.clone()))
                .or(sql::<Bool>("error_events.contexts::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
    query
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}
```

- [ ] **Step 3: Extend `EventsQuery` + the `events` handler.** In `issues.rs`, replace the `EventsQuery` struct (123-131) with:
```rust
#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_limit")]
    pub limit: i64,
}

fn default_events_limit() -> i64 {
    30
}
fn default_events_since_days() -> i64 {
    3650
}
```
Then in the `events` handler body, replace the `let limit = …; let mut events = repo::list_error_events_for_issue(&mut conn, issue_id, limit).await?;` lines (149-150) with:
```rust
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ERROR_EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 100);
    let mut events =
        repo::list_error_events_for_issue(&mut conn, issue_id, &filters, search, Some(since), limit).await?;
```
(`Duration` and `Utc` are already imported at issues.rs:7.)

- [ ] **Step 4: Compile + test.**
```
cargo test -p sauron-db filter:: && cargo build -p sauron-db && cargo build -p sauron-api
```
Expected: filter tests pass; both crates `Finished`.

- [ ] **Step 5: Commit.**
```
git add backend/crates/sauron-db/src/filter.rs backend/crates/sauron-db/src/repo.rs backend/bins/sauron-api/src/routes/issues.rs
git commit -m "feat: filter issue occurrences by tag + payload text (list_error_events_for_issue)"
```

---

### Task 9: Occurrences list UI on the issue-detail page

**Files:**
- Modify: `dashboard/src/lib/components/filters/filters.ts` (add `OCCURRENCE_FIELDS`)
- Modify: `dashboard/src/lib/api/issues.ts` (`listIssueEvents` 56-66)
- Modify: `dashboard/src/pages/IssueDetail.svelte`

**Interfaces:**
- Consumes: `FilterBar`, `OCCURRENCE_FIELDS`, `encodeFilters`, `type Filter` (Task 5/6); `listIssueEvents`; the `ERROR_EVENT_FILTERS`-backed endpoint (Task 8). `LevelBadge`, `relativeTime`, `Card`, `Spinner` are already imported in IssueDetail.
- Produces: a filterable "Occurrences" card on the issue page.

- [ ] **Step 1: Add `OCCURRENCE_FIELDS`.** In `filters.ts`, after `EVENT_FIELDS` (line 65):
```ts
// Issue-detail occurrences: only the per-event `tag` is filterable.
export const OCCURRENCE_FIELDS: FieldDef[] = [
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];
```

- [ ] **Step 2: Extend the `listIssueEvents` client to accept filters/q.** Replace `listIssueEvents` (issues.ts 56-66) with:
```ts
export async function listIssueEvents(
  appId: string,
  issueId: string,
  opts: { filters?: string[]; q?: string; sinceDays?: number; limit?: number } = {},
): Promise<ErrorEvent[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  p.set('limit', String(opts.limit ?? 50));
  const { data } = await api.get<ErrorEvent[]>(
    `/v1/apps/${appId}/issues/${issueId}/events?${p.toString()}`,
  );
  return data;
}
```
(First `grep -rn "listIssueEvents" dashboard/src` — if any caller uses the old `(appId, issueId, limit)` positional form, update it to `{ limit }`. Currently `IssueDetail.svelte` does not call it.)

- [ ] **Step 3: Add occurrences state + a debounced fetch effect in `IssueDetail.svelte`.** Extend the imports and `<script>` — add to the import block:
```ts
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import { OCCURRENCE_FIELDS, encodeFilters, type Filter } from '../lib/components/filters/filters';
  import { getIssue, updateIssueStatus, listIssueEvents } from '../lib/api/issues';
  import type { IssueDetail, IssueStatus, ErrorEvent } from '../lib/models';
```
(Merge with the existing `getIssue, updateIssueStatus` and `IssueDetail, IssueStatus` imports — do not duplicate. `LevelBadge` may already be imported; keep one.)
Add state + fetch after the existing `load(...)`/effect (near line 53):
```ts
  let occurrences = $state<ErrorEvent[]>([]);
  let occLoading = $state(false);
  let occFilters = $state<Filter[]>([]);
  let occSearch = $state('');
  let occSince = $state(3650);
  let occTimer: ReturnType<typeof setTimeout> | undefined;

  async function loadOccurrences(appId: string, id: string, enc: string[], term: string, since: number) {
    occLoading = true;
    try {
      occurrences = await listIssueEvents(appId, id, {
        filters: enc,
        q: term || undefined,
        sinceDays: since,
        limit: 50,
      });
    } catch {
      occurrences = [];
    } finally {
      occLoading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const id = issueId;
    const enc = encodeFilters(occFilters);
    const term = occSearch;
    const since = occSince;
    if (!aid || !id) return;
    clearTimeout(occTimer);
    occTimer = setTimeout(() => void loadOccurrences(aid, id, enc, term, since), 250);
    return () => clearTimeout(occTimer);
  });
```

- [ ] **Step 4: Render the Occurrences card.** In the main content column (after the latest-event card, following the existing `<Card …>` house pattern — read the render section to place it), add:
```svelte
  {#if issue}
    <Card title="Occurrences">
      <FilterBar
        fields={OCCURRENCE_FIELDS}
        bind:filters={occFilters}
        bind:search={occSearch}
        bind:sinceDays={occSince}
      />
      {#if occLoading}
        <div class="center"><Spinner size={20} /></div>
      {:else if occurrences.length === 0}
        <p class="faint">No occurrences match this filter.</p>
      {:else}
        <ul class="occ-list">
          {#each occurrences as ev (ev.id)}
            <li class="occ">
              <LevelBadge level={ev.level} />
              <span class="occ-time">{relativeTime(ev.occurred_at)}</span>
              <span class="occ-msg mono">{ev.message ?? ev.exception_value ?? ''}</span>
              {#if ev.tags && Object.keys(ev.tags).length > 0}
                <span class="occ-tags mono">
                  {Object.entries(ev.tags).map(([k, v]) => `${k}=${v}`).join(' · ')}
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </Card>
  {/if}
```
Add styles in the `<style>` block:
```css
  .occ-list { list-style: none; margin: 8px 0 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .occ { display: flex; align-items: center; gap: 10px; font-size: 12.5px; padding: 6px 8px; border-radius: var(--radius-sm); background: var(--surface-2); }
  .occ-time { color: var(--text-muted); white-space: nowrap; }
  .occ-msg { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .occ-tags { color: var(--primary); white-space: nowrap; }
  .faint { color: var(--text-muted); font-size: 12.5px; }
```

- [ ] **Step 5: Typecheck.**
```
cd dashboard && npm run check
```
Expected: `0 errors`.

- [ ] **Step 6: Commit.**
```
git add dashboard/src/lib/components/filters/filters.ts dashboard/src/lib/api/issues.ts dashboard/src/pages/IssueDetail.svelte
git commit -m "feat(dashboard): filterable occurrences list on the issue detail page"
```

---

## Verification (E2E, run after all tasks)

The webapp seed already emits errors and events carrying tags (`region`, `feature`, `customer_tier`, `surface`) and `contexts`/`extra` payloads. With the stack + dashboard running:
1. Seed data from the example app.
2. On **Issues**: add a filter `Tag region = <a seeded value>` → the list narrows to matching issues; type a payload term (e.g. an `extra` value) in the search box → list narrows.
3. On **Events**: same with the analytics tags/properties.
4. On an **Issue detail** page: the new Occurrences card lists events; add a `Tag key = value` filter and a payload search term → the occurrences list narrows.
5. Confirm no console/network errors; confirm an unmatched tag returns an empty list (not an error).

Verify via the preview tools against a running dashboard; capture a screenshot of a narrowed Issues/Events list as proof.

## Out of scope (this plan)

- **Tag-key autocomplete/discovery** endpoint.
- **`pg_trgm`/indexed payload substring search**; typed JSON-path filters; cold-tier (Parquet) search.
