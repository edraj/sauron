# Search by tag & payload content

**Date:** 2026-07-20
**Status:** Approved design — ready for implementation planning
**Depends on:** the dev tags/contexts/extra feature (`2026-07-20-sdk-tags-contexts-extra-design.md`), which added `tags`/`contexts`/`extra` JSONB columns to `error_events` and `analytics_events`.

## Summary

Let a user find signals by the developer metadata attached to them:

- **By tag** — a precise, structured filter: `tag key=value` (e.g. `region=eu`), matched exactly (`eq`) or by value substring (`contains`).
- **By payload content** — free-text substring anywhere in the `contexts`/`extra` blobs (and analytics `properties`, and raw tag text), via the existing search box.

Applies to three surfaces: the **Issues list**, the **issue-detail occurrences** list, and the **analytics Events** page. Tags/contexts/extra live on `error_events`/`analytics_events` rows; the `issues` group table has none, so issue-level search reaches them through an `EXISTS` subquery into `error_events`.

## Current state (what we build on)

- **Filter infra exists** and is mirrored front↔back:
  - Frontend `dashboard/src/lib/components/filters/filters.ts`: `Filter { field, op, value }`; `Op = eq|neq|contains|gt|lt`; `FieldType = enum|string|number`; `FieldDef { key, label, type, ops, options? }`; `encodeFilters` → `field:op:value` querystring params; `ISSUE_FIELDS`, `EVENT_FIELDS`.
  - Backend `backend/crates/sauron-db/src/filter.rs`: `Op`, `FieldSpec`, `parse_filters(raw, allow)`, `ISSUE_FILTERS`, `EVENT_FILTERS`. Values percent-decoded and validated per field.
  - `FilterBar.svelte` renders chips + a draft form (field → op → value) + a free-text `q` box + date range. Both `Issues.svelte` and `Events.svelte` already use it and sync to the querystring.
- **Free-text `q` today** never touches JSONB: issues `q` hits `title/type_/culprit`; events `q` hits `name/distinct_id`.
- **JSONB search precedent:** the only JSONB search in the codebase is `repo::list_persons` doing `properties::text ILIKE $2` — a deliberately unindexed, bounded (`app_id` + date + row cap) scan, documented in `wiki/Search.md` as house style.
- **No GIN indexes, no `pg_trgm`, no diesel JSONB operators** used anywhere yet. All existing indexes are B-tree on scalar columns, defined on the partitioned parents.
- **Query construction:** filters fold into a boxed diesel query via a hardcoded `match (field, op)` block in `repo::list_issues` (~repo.rs:565-584) and `repo::list_analytics_events` (~repo.rs:1467-1483). `list_error_events_for_issue` (~repo.rs:649-661) currently applies no filters (issue_id + limit only).

## Design

### 1. Structured tag filter

**Frontend:**
- Add `FieldType` value `'tag'` in `filters.ts`. Add a `tag` `FieldDef` to `ISSUE_FIELDS` and `EVENT_FIELDS`: `{ key: 'tag', label: 'Tag', type: 'tag', ops: ['eq', 'contains'] }`.
- In `FilterBar.svelte`, when the draft field's type is `tag`, render **two inputs** — a key input and a value input — instead of the single value input. The chip label reads e.g. `tag = region=eu`.
- Encoding is unchanged: the two inputs combine into the single value slot as `<key>=<value>`, so the querystring param is `filter=tag:eq:region%3Deu`. `encodeFilters`/`parseFilters` need no format change; only the draft-form render and a small `tag`-value composition/split helper.

**Backend:**
- `filter.rs`: add `FieldType::Tag`; add a `tag` `FieldSpec` (ops `Eq`, `Contains`) to `ISSUE_FILTERS` and `EVENT_FILTERS`. In `parse_filters`, a `Tag` field validates that the (already percent-decoded) value splits on the **first** `=` into a non-empty `key` and a `value` (value may be empty for `contains`? — require non-empty for both; reject otherwise with `FilterError`). Store the split as part of `ParsedFilter` (e.g. keep the raw `key=value` in `value` and split at apply time, or add optional `key`/`value2` fields — implementer's choice, but the split must be validated at parse time).

**Query (`repo.rs`):**
- Bind the tag key and value as **parameters** (never string-interpolate) to stay injection-safe, following the `list_persons` bound-param precedent.
- Analytics events (`list_analytics_events`), direct on the row:
  - `eq` → `analytics_events.tags @> jsonb_build_object($key, $value)` (index-backed by the new GIN).
  - `contains` → `analytics_events.tags ->> $key ILIKE '%' || $value || '%'`.
- Issues (`list_issues`), via subquery (tags live on child rows):
  - `EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.occurred_at >= <window_start> AND e.tags @> jsonb_build_object($key,$value))` for `eq`; the `->>` form for `contains`. The date window keeps the subquery bounded (reuse the request's `since_days`/range).
- Issue occurrences (`list_error_events_for_issue`): add optional filter params and apply the same direct `tags @>` / `->>` predicates on `error_events`.

Implementation note: diesel's `JsonbExpressionMethods` (`.contains()`, `.retrieve_as_text()` / `->>`) are available under the enabled `postgres_backend` feature but unused so far; equivalently a bound-parameter `diesel::dsl::sql::<Bool>` fragment (as `list_persons` uses) is acceptable. Either way, **all dynamic key/value/term inputs are bound parameters.**

### 2. Free-text payload search

Broaden the existing `q` predicate (no new UI, no new param):
- `list_analytics_events` `q` block: add `OR contexts::text ILIKE p OR extra::text ILIKE p OR properties::text ILIKE p OR tags::text ILIKE p` alongside the current `name/distinct_id` clauses (`p` = the existing `%term%` bound pattern).
- `list_issues` `q` block: add `OR EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.occurred_at >= <window_start> AND (e.contexts::text ILIKE p OR e.extra::text ILIKE p OR e.tags::text ILIKE p))` alongside the current `title/type_/culprit` clauses.
- `list_error_events_for_issue`: if a `q` param is added there, apply the direct `contexts/extra/tags ::text ILIKE p` clauses.
- These are bounded scans (house style); update the search-box placeholder/help text and `wiki/Search.md` to note that `q` now also searches payload.

### 3. Indexing

- New migration `2026-07-20-000018_tags_gin`:
  - `CREATE INDEX error_events_tags_gin ON error_events USING gin (tags jsonb_path_ops);`
  - `CREATE INDEX analytics_events_tags_gin ON analytics_events USING gin (tags jsonb_path_ops);`
  - Both on the **partitioned parent** (propagates to the default partition and all range partitions, exactly like the existing B-tree indexes defined on the parent). `down.sql` drops both.
- `jsonb_path_ops` GIN accelerates the `@>` containment used by the tag `eq` path. Payload free-text (`::text ILIKE`) stays unindexed by design.
- No `schema.rs` change (indexes aren't modeled by diesel `table!`).

### 4. Data flow

`FilterBar` tag chip → `encodeFilters` → `filter=tag:op:key=value` + `q=term` on the querystring → `listIssues`/`listEvents` API client → GET `/v1/apps/{app_id}/issues` | `/events/list` (+ the issue events endpoint) → handler runs `parse_filters` against the whitelist (now including `tag`) → `repo::list_*` builds the boxed diesel query with the JSONB predicates → rows returned and rendered by the existing pages.

### 5. Testing

- `filter.rs` (pure): `tag:eq:region=eu` parses to a tag filter with key `region`, value `eu`; malformed (`no-equals`, empty key) is rejected; op outside `{eq,contains}` rejected.
- `filters.ts` (frontend): `encodeFilters`/`parseFilters` round-trip a `tag` filter; FilterBar renders two inputs for `type: 'tag'` and composes `key=value`.
- Backend query tests where a DB harness exists; otherwise compile + the parse tests are the gate (the repo has no DB-backed test harness — consistent with prior slices).
- E2E: the webapp seed already emits tagged errors/events (`region`, `feature`, `customer_tier`, and `contexts`/`extra` payloads). Run the seed, then in the dashboard filter Issues/Events by `tag region=eu` and by a `q` payload term, and confirm results narrow (verify via preview tools).

## Out of scope (YAGNI)

- **Tag-key autocomplete/discovery** (a `SELECT DISTINCT jsonb_object_keys(tags)` endpoint to populate a key dropdown) — for now the user types the key. Future nicety.
- **`pg_trgm` / indexed payload substring search** and **typed JSON-path filters** (`extra.cartValue > 100`) — chose bounded scan + `eq`/`contains` only.
- **Cold-tier (Parquet) search** — `list_*` are hot-Postgres-only today; all existing search is too. Unchanged.
- **Rolling tags up onto the `issues` group table** (denormalization) — we use the `EXISTS` subquery instead; no schema change to `issues`.

## Key risks / gotchas

- **Injection safety:** the tag *key* is dynamic. It MUST be a bound parameter (`tags ->> $key`, `jsonb_build_object($key,$value)`), never string-interpolated into SQL.
- **Issues subquery cost:** the `EXISTS` into `error_events` must carry the `app_id` + an `occurred_at` window so it doesn't scan the whole partitioned table; reuse the request's date range.
- **GIN on partitioned parent:** supported and propagates (same as existing B-tree parent indexes), but this repo has no prior GIN migration — follow the "define on parent" comment precedent from `2026-07-14-000011_error_events_partitioned`.
- **`contains` op has no index** — it's a bounded `::text ILIKE` / `->>` scan by design; acceptable per house style.
- **Migration ordering:** `2026-07-20-000018_tags_gin` sorts after `-000017` (the analytics scopes migration from the prior feature).
