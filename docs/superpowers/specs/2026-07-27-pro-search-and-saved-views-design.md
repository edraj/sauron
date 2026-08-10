# Pro search & saved views — design

**Date:** 2026-07-27
**Status:** approved, ready for planning

## 1. Problem

Sauron's search reads as a filter bar but is far narrower than it appears.

Only **3 of 25** query-param endpoints accept structured filters — `GET /v1/apps/{app_id}/issues`, `GET /v1/apps/{app_id}/issues/{issue_id}/events`, and `GET /v1/apps/{app_id}/events/list`. Everything else that shows a search box is either a single `ILIKE` against one or two columns (Persons `repo.rs:1824-1868`, Devices `repo.rs:1730-1770`, Screens `repo.rs:2836-2869`) or client-side filtering over the already-loaded page (`SessionsList.svelte`, `FunnelBuilder.svelte:60-64`). Occurrences accept exactly one field: `tag` (`filter.rs:239-244`).

The grammar is a flat AND-list of `field:op:value` with ops `eq|neq|contains|gt|lt` (`filter.rs:8-27`). No OR, no grouping, no negation beyond `neq`, no sort parameter on any endpoint, no result totals, no cursors.

Meanwhile the data is already in Postgres with no read path to it:

- `error_events.environment_id`, `.release`, `.level` — stored, and the **alerting engine already filters on them** (`crates/sauron-alerts/src/rule.rs:110-116` → `repo.rs:4329`). The capability exists; it just isn't exposed to humans.
- `error_events.distinct_id` — **indexed** `(app_id, distinct_id, occurred_at DESC)` and unreachable from search.
- `error_events.event_user` — you cannot find an error by user email.
- `error_events.stacktrace`, `.breadcrumbs`, `.context` (the enriched os/browser/device/runtime/app blob from `enrich.rs:11-40`), `.symbolication_status`, `.session_id`, `.device_key`, `.screen` — all stored, none searchable.
- `issues.first_seen` — stored and indexed, but the date range filters `last_seen` only (`repo.rs:979`), so "new this week" is inexpressible.
- `transactions.duration_ms`, `.http_status`, `.http_method`, `.url` — Performance has no filter bar at all; `transactions.url` has zero read paths anywhere in the tree.
- `devices.browser`, `.os_version`, `.arch` — displayed, never searched.

Two fields are dropped at ingest: `mechanism.handled` is parsed off the wire (`crates/sauron-core/src/envelope.rs:164`), sent by every SDK, and never persisted; `error_events.sdk` exists as a column but `process.rs:230` hardcodes `sdk: None`.

There is **no saved search, saved view, saved filter set, bookmark, favorite, pinned query, column choice, page size, or user preference anywhere** — 27 tables across 23 migrations, no prefs column on `users` (`schema.rs:227-239`), and five localStorage keys on the client, none of which hold filter state. Only 2 of ~12 filterable pages even sync to the URL. The documented substitute for saved searches is "copy the URL" (`wiki/Search.md:119-122`).

### Live defects this work must fix

1. **Silent truncation.** The Issues UI offers a 3650-day range (`Issues.svelte:26-31`) and the backend honours it, but row-level search is hot-Postgres-only — the DuckDB surface exposes only count-shaped methods (`crates/sauron-tier/src/duck.rs`: `count_parquet_rows`, `counts_by_day`, `count_range`, `counts_by_app`) with no predicate beyond `app_id` + `occurred_at` range. The tier worker drops `error_events` partitions past `TIER_HOT_DAYS` (default **30**, `config.rs:186`). Long-window searches return partial answers and say nothing.
2. **Dead cost guard.** `MAX_PAYLOAD_SEARCH_DAYS = 90` (`repo.rs:966`) never fires — `routes/issues.rs:50` always passes `Some(since)` and `default_since_days()` is 3650, so the `unwrap_or_else` at `repo.rs:1031-1033` is unreachable. The default `q=` search runs an unindexable correlated `jsonb::text` scan across ~10 years per candidate issue.
3. **Unindexed default.** The UI defaults the tag operator to `contains` (`filters.ts:63`) — the `->> ILIKE` path — while the indexed `@>` path is the non-default. The most common tag query is the most expensive one.
4. **Unstable paging.** Every list endpoint returns a bare JSON array (`routes/issues.rs:45`) sorted by a non-unique timestamp with no id tiebreaker (`repo.rs:1062`, `:1162`, `:2094`). Deep paging can duplicate or skip rows; the frontend fakes `hasNext` with `count >= limit`.
5. **Duplicate indexes.** `issues_last_seen_idx` (`init/up.sql:87`, renamed by `0002/up.sql:53`) and `issues_app_last_seen_idx` (`0020/up.sql:45-46`) are the same index `(app_id, last_seen DESC)`. Migration 0020's own header states the rule this violates.

   **Correction (verified by EXPLAIN against the live database, 2026-07-27):** an earlier draft of this spec claimed both are redundant prefixes of `issues_list_idx (app_id, status, last_seen DESC)` and could simply be dropped. That is **false** — `last_seen` sits behind an equality on `status` in that index, and Postgres 16 has no index skip scan, so the default issues list (no status predicate) falls back to a `Sort`. Dropping both without a replacement is a performance regression. Migration 25 must instead create one wider index that both duplicates genuinely *are* prefixes of, and which also serves the keyset cursor:

   ```sql
   CREATE INDEX issues_app_last_seen_id_idx ON issues (app_id, last_seen DESC, id DESC);
   DROP INDEX IF EXISTS issues_app_last_seen_idx;
   DROP INDEX IF EXISTS issues_last_seen_idx;
   ```

   `issues_list_idx` stays (it serves `is:resolved` plus ordering). `issues_app_first_seen_idx` already exists from `0020/up.sql:43-44`, so the `firstSeen` dimension needs no new index — an earlier draft listed one, which would have repeated exactly the mistake this migration exists to fix.

   Two further spec corrections from the same verification pass: the previously-listed `(app_id, environment_id, last_seen DESC)` names columns from **two different tables** (`error_events` has no `last_seen`; `issues` has no `environment_id`) — the intended index is `error_events (app_id, environment_id, occurred_at DESC)`. And every index here builds **synchronously across all live child partitions inside one transaction**: migrations run in a transaction and both target tables are partitioned parents, so `CREATE INDEX CONCURRENTLY` is impossible. This needs a maintenance window.

## 2. Goals

- A professional-grade query experience: expressive syntax, discoverable via autocomplete, usable without reading docs.
- Every dimension already present in the data becomes filterable.
- Search works on **every** list surface, not three of them.
- Filters persist server-side, per user, across browser sessions and machines, with opt-in team sharing.
- Search stops lying about coverage and cost.

## 3. Non-goals

- **Row-level cold-tier search.** DuckDB has no row-projection path; adding one means two query planners kept in sync, paginating a merge of two engines with different sort stability, a fresh in-memory engine per request (no pooling, `duck.rs:19-26`), and a cross-tier cursor that is not expressible. Separate project.
- **Relevance ranking / scoring.** Search returns filtered results in a chosen sort order, not "best match".
- **An external search engine.** No Elasticsearch, no OpenSearch. Postgres only.
- **Backfilling `handled` for existing rows.** Not possible — the data was never recorded.
- **Cross-app or cross-org search.** Everything stays app-scoped, matching the existing tenancy model.
- **Migrating all 25 list endpoints to the new response envelope.** Only the endpoints this feature touches.

## 4. Decisions

| # | Decision | Chosen |
|---|---|---|
| 1 | How filters are expressed | **Hybrid**: a real query language is the wire format and source of truth; the chip bar is a view over the parsed AST |
| 2 | Saved view ownership | **Personal + opt-in share** — `owner_id NOT NULL` + `visibility ('private'\|'app')`, private by default |
| 3 | Indexing strategy | **Curated typed columns + `jsonb_ops` GIN**; substring/free-text stays an honestly-bounded scan |
| 4 | Time horizon | **Cap honestly + roll dimensions onto `issues`**, which is not tiered |
| 5 | Pagination | **Envelope + keyset cursor on searched endpoints only** |
| 6 | Dropped ingest fields | **Persist `handled` and `sdk`; NULL means unknown**, never folded into either bucket |

## 5. Query grammar

Whitespace-separated terms. Implicit `AND`, explicit `OR`, parens for grouping, `!` for negation.

```
is:unresolved level:error !environment:staging
user.email:*@acme.com release:2.1.4 firstSeen:-7d
(os.name:Windows OR os.name:Linux) has:extra.cartValue
duration:>2s http.status:>=500 "connection refused"
```

### Term forms

| Form | Meaning |
|---|---|
| `field:value` | Equality. `*` anywhere in the value makes it a wildcard match |
| `field:~text` | Literal substring — `text` is matched verbatim, so a `*` in it stays a `*`. This is what the pre-language `contains` operator meant, and the legacy bridge maps onto it so existing shared URLs keep returning the same rows |
| `field:>v` `:>=v` `:<v` `:<=v` | Numeric / duration / date comparison |
| `field:[a,b,c]` | IN — planned as a single `= ANY`, not an OR fan-out |
| `!field:value` | Negation. NULL-safe: compiles to `NOT (x = v) OR x IS NULL` so NULL rows are not silently dropped |
| `has:field` | Key existence — served by `jsonb_ops` GIN, which `jsonb_path_ops` cannot do |
| `is:<shorthand>` | Curated shorthand namespace (see below) |
| `"quoted text"` / bare word | Free text → the bounded payload scan |

### The `is:` namespace

`is:` is a curated shorthand namespace, not a field. Each value maps to a specific predicate, so there is no ambiguity with a real field named `is`:

| Term | Compiles to |
|---|---|
| `is:unresolved` `is:resolved` `is:ignored` | `issues.status = <value>` |
| `is:unhandled` | `error_events.handled = false` |
| `is:handled` | `error_events.handled = true` |

`is:unhandled` and `is:handled` both exclude NULL, which is the point of decision 6 — rows ingested before the `handled` column existed are *unknown*, not handled. `has:handled` selects rows where the value is known.

### Field resolution order

First match wins.

1. **Curated fields** — typed, backed by a real column or rollup.
2. **Explicit tag keys** — the `tag.<key>` prefix, and the `tag:<key>=<value>` escape hatch below for a key the field syntax cannot spell.
3. **Structured JSONB paths** under a known root: `user.*`, `os.*`, `browser.*`, `device.*`, `runtime.*`, `app.*` (→ enriched `context`), `contexts.*`, `extra.*`, `properties.*`, `stack.*`, `traits.*`.
4. ~~**Anything unrecognised → a tag lookup.**~~ **REMOVED IN S2c. There is no fourth step.** An unrecognised name is now a 400 that names the field, offers the `tag.<key>` spelling (or `tag:<key>=<value>` where the key is not an identifier, and neither where the resource has no `tags` column), and lists that resource's available fields. The rule was written to make dev-defined tags first-class without ceremony; the price was that every typo — `enviroment:prod`, `checkout_stpe:payment` — resolved to a tag nobody had ever written and answered **200 with zero rows**, indistinguishable from an honest "no matches". `resolve_field` is shared by `query=` and `filter=`, so neither spelling can drift into a laxer vocabulary than the other. No capability is lost: step 2 names any tag key explicitly. See `crates/sauron-query/src/resolve.rs` and `error.rs`.

### Tag keys are unconstrained, so the grammar needs an escape hatch

Tag keys have **no validation anywhere on the write path** — `envelope.rs` types `tags` as a raw `serde_json::Value`, the ingest edge inspects nothing, `process.rs`'s `object_or_empty` only maps `null`→`{}`, the JSONB column carries no `CHECK`, and no SDK sanitizes keys (`sdks/csharp/Sauron/Envelope.cs:18` says so deliberately: *"Dictionary keys … are left untouched on purpose"*). A developer can therefore store `cart@checkout`, `100%off`, `a+b` or `café`.

The `tag.<key>` prefix resolves a key through the identifier rule `[A-Za-z_][A-Za-z0-9_.-]*`, so those keys would be unsearchable. The escape hatch is the legacy spelling, which needs no new syntax because the identifier rule constrains only the *field* side of a term:

| Form | Use |
|---|---|
| `checkout_step:payment` | ergonomic, identifier-like keys |
| `tag.checkout_step:payment` | explicit disambiguation |
| `tag:cart@checkout=eu` | **any** key — everything before the first `=` is the key |
| `tag:cart@checkout=~eu` | any key, literal substring |
| `tag:cart@checkout=*eu*` | any key, wildcard |

The remainder after the first `=` is an ordinary value, so every operator composes. The one key shape still rejected is one containing **whitespace**, because the lexer breaks a word on whitespace and quoting the value would suppress the operator prefix that `~`/`>`/`*` need — such a key is not renderable, so it fails loudly with `BadValue`.

A bare `tag:foo` with no `=` is an error rather than "a tag whose key is literally `tag`".

~~Rule 3 means the grammar never rejects a query as "unknown field", which removes the single largest source of user frustration and makes dev-defined tags first-class without ceremony.~~ **False since S2c** — see the removed step 4 above. The grammar rejects an unknown field, and the frustration it was meant to remove is smaller than the frustration of a typo that returns 200 with nothing in it and no way to tell why. What survives is the ergonomics: `tag.checkout_step:payment` is one prefix longer than `checkout_step:payment`, and the error message hands you that exact string.

### Value literals

- Durations: `2s`, `500ms`, `1m`
- Relative dates: `-24h`, `-7d`, `-30d`
- Absolute: ISO-8601 timestamps
- Quoted strings for values containing spaces or colons
- Bare numerics for numeric fields

### Back-compatibility

The AST is transported as a single `query=` parameter. The existing repeated `filter=field:op:value` and `q=` parameters keep working and are parsed into the same AST, so today's shared URLs and the syntax documented in `wiki/Search.md` continue to function.

### Shared with alerting

`crates/sauron-alerts/src/rule.rs:110-116` maintains its own parallel `Filters` struct that already performs environment/level filtering the UI cannot express. It moves onto the same AST, so alert rule conditions become expressible in the syntax typed into the search bar.

## 6. Dimension catalog

The catalog is the single source of truth. Each entry declares: canonical name, aliases, value type, storage location (`column` | `jsonb_path` | `tag` | `rollup`), applicable resources, allowed operators, and index class (`indexed` | `bounded` | `scan`).

Five artefacts derive from it rather than being maintained in parallel: the parser's field resolution, the planner's SQL mapping and cost classification, the `/search/fields` autocomplete endpoint, the in-app Docs field reference, and the `wiki/Search.md` field table.

### Issues (`issues`)

| Field | Storage | Ops | Index |
|---|---|---|---|
| `is` (status) | `issues.status` | `= != in` | indexed (`issues_list_idx`) |
| `level` | `issues.level` | `= != in` | bounded |
| `type` | `issues.type` | `= != contains` | bounded |
| `culprit` | `issues.culprit` | `= != contains` | bounded |
| `title` | `issues.title` | `= != contains` | scan |
| `timesSeen` | `issues.times_seen` | `= > < >= <=` | bounded |
| `usersSeen` | `issues.users_seen` | `= > < >= <=` | bounded |
| `firstSeen` | `issues.first_seen` | `> < >= <=` | indexed (`0020`) |
| `lastSeen` | `issues.last_seen` | `> < >= <=` | indexed |
| `environment` `release` `handled` `<tag>` | `issue_dimensions` rollup | `= != in has` | indexed |

`level` on the Issues resource resolves to the `issues.level` column (the issue's own representative level), **not** to the rollup. It is deliberately not a rollup dimension, so there is exactly one meaning for `level:error` on Issues rather than an ambiguous "the issue's level" versus "has any occurrence at this level".

### Error events / occurrences (`error_events`)

| Field | Storage | Notes |
|---|---|---|
| `environment` | `environment_id` | name→uuid resolved before the query |
| `release` | `release` | new btree |
| `level` | `level` | new btree |
| `handled` | `handled` | **new column**; NULL = unknown |
| `distinctId` | `distinct_id` | already indexed |
| `user.email` `user.id` `user.username` | `event_user` jsonb | `jsonb_ops` GIN |
| `session` | `session_id` | |
| `deviceKey` | `device_key` | |
| `screen` | `screen` | |
| `symbolication` | `symbolication_status` | 6 values |
| `sdk.name` `sdk.version` | `sdk` jsonb | **newly populated** |
| `os.*` `browser.*` `device.*` `runtime.*` `app.*` | `context` (singular, enriched) | `jsonb_ops` GIN |
| `contexts.*` | `contexts` (dev-supplied) | `jsonb_ops` GIN |
| `extra.*` | `extra` | `jsonb_ops` GIN |
| `stack.filename` `stack.function` `stack.module` | `stacktrace` jsonb | scan |
| `<tag>` | `tags` | existing `jsonb_path_ops` GIN for `=` |
| breadcrumb text | `breadcrumbs` | free-text only |

### Analytics events (`analytics_events`)

`name`, `distinctId`, `session`, `environment`, `release`, `<tag>`, `properties.*`, `contexts.*`, `extra.*`.

### Transactions

`op`, `name`, `duration` (`duration_ms`), `http.status`, `http.method`, `url`.

### Devices

`device.family`, `device.model`, `os.name`, `os.version`, `browser`, `device.arch`, `deviceKey`.

### Persons

`distinctId`, `traits.*` (`event_users.properties`).

## 7. Architecture

### `crates/sauron-query/` (new)

Pure crate, zero DB dependencies: tokenizer → AST → validator against the catalog → cost classifier.

The isolation is load-bearing for the grammar: a pure crate means every operator, every precedence rule and every error message is unit-tested with no infrastructure at all, and handlers stay thin fetch → authorize → plan → run shells.

**CORRECTION (S2c): "CI has no database" is no longer true, and the constraint it justified applied only to S1.** `.github/workflows/ci.yml:32-51` now runs `postgres:16` and `redis:7` service containers with `TEST_DATABASE_URL`/`TEST_REDIS_URL`, so route behaviour IS testable in CI through the real router via the `bins/sauron-api/tests/http_*.rs` harness — which is how S2c verified its own routes. The hazard the original wording pointed at is real but different: with no reachable database `TestDb::setup()` returns `None` and every DB-backed test *silently reports passed in 0.00s*, so a green count is not by itself proof they ran.

### `crates/sauron-db/src/query_plan.rs` (new)

Takes `(AST, resource)` → boxed diesel query.

**Injection safety is preserved exactly as today.** Only catalog-derived `&'static str` reaches SQL text; every user value is a diesel bind (`.bind::<Jsonb|Text|Timestamptz,_>` or diesel's own `.eq/.ilike/...`); `sql::<Bool>(…)` fragments stay static literals with binds interleaved. `escape_like` continues to escape `\ % _`.

**Cost guard.** Each predicate is classified `indexed` / `bounded` / `scan`. A plan that is all-`scan` over a long window has its window clamped, and the clamp plus its reason is returned in the response so the UI can say why. This replaces the dead `MAX_PAYLOAD_SEARCH_DAYS` constant with a guard that actually fires, and it is the honest answer to why `OR` was previously avoided — an OR of two unindexed predicates defeats every index, and the planner now sees that and bounds it.

### Migrations

Next prefix is `2026-07-27-000024` (previous: `2026-07-26-000023_member_lifecycle`). The 6-digit counter, not the date, is the ordering authority. `up.sql` opens with a *why* comment; `down.sql` is bare DDL reversing statement order.

**Numbering is assigned when a slice STARTS, not here.** The counter is shared with every other line of work in the repo, so a number written into this spec is a guess that goes stale the moment anything else lands a migration — which is exactly what happened to the two rows below (they said 26 and 27; by the time S2c finished, the tree was already past 46). Treat the numbers in this table as *identifiers for migrations that have shipped* and, for anything still ahead, read the next free number off `backend/migrations/` at the moment you write it.

`crates/sauron-db/src/schema.rs` is **hand-maintained** despite its `@generated` header — columns must be appended in physical order or `Queryable` binds the wrong fields, and new tables must be added to `allow_tables_to_appear_in_same_query!` (`schema.rs:452-480`) plus any `joinable!`.

| Migration | Contents |
|---|---|
| `…24_event_handled_sdk` | **Shipped (S2a).** `error_events.handled BOOLEAN NULL`; populate `sdk` (pipeline change at `process.rs:230`) |
| `…25_search_indexes` | **Shipped (S2a).** Curated btrees; **replace** the duplicate issues indexes with one wider index (see below). **JSONB GINs deferred — measured and rejected, see §13** |
| `2026-08-09-000047_analytics_keyset_index` | **Shipped (S2c).** The `(app_id, occurred_at DESC, id DESC)` tiebreaker on `analytics_events`. Migration 25 gave `issues` and `error_events` their keyset indexes but the closest analytics index was `analytics_project_idx (app_id, occurred_at DESC)` — no `id` — so deep paging on Events was exactly the duplicate-rows bug S2c exists to fix. Runs **synchronously** across live child partitions in one transaction; `CONCURRENTLY` is not available, so it needs a maintenance window |
| `…49_issue_dimensions` (S3) | The rollup table. **Was written here as 26.** 47 and 48 are taken (48 is `alert_rule_monitor`, unrelated work), so 49 is the next free number as of 2026-08-10 — re-check before creating it |
| `…50_saved_views` (S5) | `saved_views` + `view:write` permission backfill. **Was written here as 27.** Same caveat: read the real next free number when S5 starts |

### `issue_dimensions`

`(app_id, issue_id, key, value, count, first_seen, last_seen)`, unique on `(app_id, issue_id, key, value)`, upserted at ingest.

This is what makes `environment:production` on Issues both index-backed **and** tier-proof — replacing today's correlated `EXISTS` subquery into `error_events` (`repo.rs:1001-1024`), which runs a correlated subplan per candidate issue and stops working when partitions drop. Because `issues` is not tiered at all (`crates/sauron-tier/src/lib.rs:25-38` tiers only `error_events`, `analytics_events`, `transactions`), issue-level dimension filtering survives partition drop permanently.

Two secondary wins: it powers value autocomplete with values that actually exist in the caller's data, and it gives the issue-detail page a tag-distribution breakdown (Sentry's `GroupTagValue` view) the product doesn't have today.

### `saved_views`

```
id          UUID PK
app_id      UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE
owner_id    UUID NOT NULL REFERENCES users(id)
resource    TEXT NOT NULL   -- issues|events|occurrences|sessions|devices|users|transactions
name        TEXT NOT NULL
description TEXT
query       TEXT NOT NULL   -- raw source, not a parsed blob
sort        TEXT
visibility  TEXT NOT NULL   -- 'private' | 'app'
is_default  BOOLEAN NOT NULL DEFAULT false
created_at, updated_at
```

Indexes on `(app_id, resource, visibility)` and `(owner_id, resource)`.

**Query stored as text, not a parsed structure.** A catalog change can then never leave a stored view unparseable — it degrades to a readable error the user can edit, instead of a row that fails to deserialize.

**Deactivation.** Private views follow their owner; app-shared views survive with `owner_id` intact. Note `users.is_active` is *not* re-checked per request (`extractors.rs:113-153`), so deactivation is not a live authorization boundary today and this design does not pretend otherwise.

**Precedent.** The only prior art is `saved_funnels` (`2026-07-13-000006_saved_funnels/up.sql:1-14`): app-scoped, shared-by-default, `created_by` display-only and not load-bearing in any WHERE clause. There is no private-vs-shared visibility primitive anywhere in the codebase, so `visibility` is a new concept and needs its own tests.

## 8. API surface

- `GET|POST /v1/apps/{app_id}/saved-views`, `GET|PATCH|DELETE /v1/apps/{app_id}/saved-views/{id}` — load-then-authorize-by-owning-org, copying `load_channel_authorized` (`routes/notifications.rs:133-156`). Mutations follow the established `WHERE tenant_key = $1 AND id = $2` → `usize` → 0 maps to 404 pattern.
- `GET /v1/apps/{app_id}/search/fields` — catalog for autocomplete.
- `GET /v1/apps/{app_id}/search/values?field=` — value autocomplete from `issue_dimensions`.
- Searched list endpoints gain `query=`, `sort=`, `cursor=` and move to an envelope:

```jsonc
{
  "data": [ … ],
  "total": 1204,           // always a number
  "total_is_capped": false, // true => "1204+", counting stopped at the cap
  "next_cursor": "…",       // null on the last page
  "clamped": null           // or { "field": "last_seen", "to": "30d", "reason": "…" }
}
```

**Shipped as drawn, with one correction (S2c):** `clamped.field` carries the **resource's own window column** — `last_seen` on Issues, `occurred_at` on Occurrences and Events — not the generic `"since"` this example originally showed. `prepare()` takes no `Resource`, so it reports the window generically and each resource-aware handler maps it onto the real column before serializing.

**`offset=` is accepted and IGNORED** on the three migrated endpoints, rather than dropped. Dropping the field would turn every bookmarked `?offset=50` into a 400 from an unknown parameter; accepting it returns page one, which is different from what that link used to return but is at least correct rows. Callers follow `next_cursor`. Every endpoint still on `clamp_offset` (screens, sessions, devices, workflows, top events) is unaffected.

`total` stays a number and `total_is_capped` carries the nuance — counting is exact when the plan is index-backed and stops at a cap when it degrades to a scan, so counting never becomes the expensive part of the request. Returning a string like `"1000+"` was rejected: it forces every client to parse a number out of a display string.

**Endpoints accepting repeated params must use `axum_extra::extract::Query`** — plain `axum::Query` silently drops repeats, and today only `routes/issues.rs:6` and `routes/analytics.rs:6` import the extra one.

### Permissions

One new permission, `view:write`. Adding it is a six-file lockstep change: the `rbac.rs` const, the length-asserted `perm::ALL` array, all four system role presets (with tests enforcing Viewer ⊂ Developer ⊂ Admin ⊂ Owner, `rbac.rs:485-494`), a `UPDATE roles SET permissions = permissions || …` backfill migration (pattern: `2026-07-15-000015_source_read_perm/up.sql:7-9`), `dashboard/src/lib/models/permissions.ts`, and the vitest that parses `rbac.rs` off disk. Reads reuse each resource's existing read permission.

Authorization remains two-layered and both layers are mandatory: `authorize_app` in the handler after the single pool checkout, **and** the tenant key in every query's WHERE clause including nested subqueries.

### Remaining surfaces

Sessions, Devices, Users and Transactions gain real server-side filtering through the same planner. Until they have it, saving a filter on those pages is not possible.

## 9. Frontend

### New primitives

There is no `Select`, `Popover`, `Dropdown`, `Combobox`, `Menu`, `Checkbox`, `Tabs`, `Tooltip` or `DatePicker` component in this codebase. `ui/Popover.svelte` and `ui/Combobox.svelte` are built by lifting the pattern proven in `layout/SwitcherMenu.svelte`: portal-to-body, outside-`pointerdown` on capture, document-level `Escape` (focus often sits on the trigger, not inside the menu, so a menu-scoped handler misses it), reposition on scroll/resize.

The portal is not polish. `Topbar`'s `.left` has `overflow: hidden` and `.topbar` has `backdrop-filter`, which makes it a containing block for `position: fixed` descendants — a naively positioned dropdown is clipped or anchored to the wrong box.

### `SearchBar.svelte`

Replaces `FilterBar.svelte`. The text input is the source of truth; chips render from the parsed AST and are individually editable and removable; the combobox suggests fields, then values (fed by `issue_dimensions`). Keyboard-first throughout.

Saved views sit in an adjacent dropdown: **My views** / **Shared**, star-to-default, save / save-as. The URL carries `?query=` plus `?view=<id>`, so links remain shareable exactly as today.

### Cleanups in scope

- `FilterBar` currently bypasses `ui/Input.svelte` and `ui/Badge.svelte` with raw `<select>` and hand-rolled `<span class="chip">` elements.
- Icons `plus`, `save`, `bookmark`, `trash`, `filter` must be registered in `ui/Icon.svelte`'s `iconRegistry` or they silently do not render.
- Debounce is re-implemented per page at 220/250/300 ms — unify into one helper.
- `DataTable` gains sortable column headers, backed by the new `sort` parameter.

Svelte 5 runes only. House UI components only — a raw `<button>` renders as a browser-default grey box because the global reset only sets font and cursor.

## 10. Documentation

**Docs ship with the slice that makes them true, not in a batch at the end.** An earlier
draft of this spec deferred all three surfaces to the final slice. That is wrong: the
moment S2c lands, `wiki/Search.md:12` — *"no query language, no operators"* — becomes an
active lie, and it stays one through S3, S4 and S5. Each surface is therefore tied to the
slice that changes the behaviour it describes:

| Surface | Lands in | Why then |
|---|---|---|
| `wiki/Search.md` — grammar, operators, field table, "what's fast vs what's a scan" | **S2c** | First slice where `query=` is reachable. A developer hitting the API needs the grammar the day it exists |
| `wiki/Dashboard.md` — Search & saved views walkthrough | **S4** | First slice with a UI to walk through |
| `dashboard/src/pages/Docs.svelte` — in-app cheatsheet | **S4** | Reachable from the bar it documents; hangs off the existing `queries` section (`Docs.svelte:403`, `:1035`) |
| Saved-views semantics (private vs shared, defaults) | **S5** | The feature they describe |
| Anti-rot test — generate the field table from the catalog and fail on drift | ~~S2c~~ → **S6** | **DID NOT SHIP IN S2c.** The page was rewritten and its field table verified against `CATALOG` by a manual `comm` check (plan Task 9 Step 2), which is a point-in-time check and not a guard. §12 already scheduled the test in S6 and that is where it now lives — noted here rather than quietly left, because "ships with the table it guards, so it can never be added later" was the whole argument for S2c and the argument lost |

The grammar itself is **frozen** as of S1 — `sauron-query` is complete and its 159 tests
pin every operator — so the syntax reference can be written without waiting on anything.

- **`wiki/Search.md`** — full rewrite. **DONE in S2c.** Grammar reference, per-resource field tables, the response envelope and cursor paging, and an honest "what is fast vs what is a scan" section. The page's central promise was *"no query language, no operators"*, so this was a replacement rather than an edit. Saved-views semantics are not in it — they move to S5 with the feature. The three pre-existing stale statements were fixed in the same pass: the Events range does **not** go to all-time (the picker stops at 90d, the API caps at 365d, while the Exceptions picker really does offer "All"); `%`/`_` **are** escaped on Users and Devices, which now route through `like_contains` like everything else; and the "~200 rows per page via `limit`/`offset`" line is replaced by the cursor contract.
- **`wiki/Dashboard.md`** — a Search & saved views section in the UI walkthrough, cross-linked to `Search.md`.
- **`dashboard/src/pages/Docs.svelte`** — in-app syntax cheatsheet, hung off the existing `queries` entry under "Under the hood".

**Anti-rot test — still owed, now scheduled for S6.** The field-reference tables in `wiki/Search.md` are hand-written against `CATALOG` and verified once, by hand; nothing yet regenerates them and fails on drift. Precedent for the test when it is written: the existing vitest that parses `rbac.rs` off disk and fails if `permissions.ts` drifts. Until it exists, **anyone adding, renaming or re-scoping a dimension in `crates/sauron-query/src/catalog.rs` must edit `wiki/Search.md`'s "Fields by page" section in the same change**, and re-run the check in the S2c plan's Task 9 Step 2.

## 11. Testing & verification

- **Unit** — the whole of `sauron-query` (tokenizer, precedence, negation, wildcards, ranges, field resolution including the tag fallback, error messages) and the planner's AST→fragment mapping and cost classification, as pure functions. Tests are inline `#[cfg(test)] mod` in the same file, per repo convention.
- **Frontend** — vitest over AST round-tripping (text → chips → text must be stable) and the permissions parity test.
- **Route behaviour** — through the real router with the `bins/sauron-api/tests/http_*.rs` harness. **CORRECTED in S2c: CI does have Postgres and Redis** (`.github/workflows/ci.yml:32-51`), so these run in CI, not only locally. But they `return` early when `TEST_DATABASE_URL` is unset, which means a green count proves nothing on its own — quote per-target counts and check the DB was actually reachable.
- **E2E** — manual, against the docker stack. Each slice ends with a live walkthrough. Verifying the dev server against the live API hits CORS: `CORS_ALLOWED_ORIGINS` allows only `http://localhost:10002` while `dashboard-dev` serves on `:5199`.

**Gates:** `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` are hard; plus `cargo test --workspace`, `npm run build`, `npm test`. Never `--all-features` (rebuilds DuckDB from source). Builds need `DUCKDB_LIB_DIR` / `LD_LIBRARY_PATH` from `packaging/rpm/fetch-libduckdb.sh`.

## 12. Sequencing

This spec covers a multi-slice programme, not a single implementation plan. **Each slice gets its own plan** written when it starts, so the plan for S4 can be informed by what S1–S3 actually produced rather than guessed at now. S1 is the only slice with no dependency on a decision made during another.

**S2c shipped `sort=` RESTRICTED to keyset-backed columns** — `last_seen`/`first_seen` on Issues, `occurred_at` on Occurrences and Events, each optionally prefixed with `-` to reverse. Anything else is a 400 listing what is allowed. An ordering with no supporting `(…, id)` index cannot page stably, and silently returning duplicate rows is the defect this slice exists to remove; §9's "`DataTable` gains sortable column headers" therefore cannot offer an arbitrary column until that column has an index. More orderings arrive **with** their indexes, never ahead of them.

**Two other behaviours differ from what this spec describes, and the spec text above is now wrong on both.** §5's field-resolution rule 3 (*"anything unrecognised → a tag lookup"*) was **removed** in S2c: an unknown field is now a 400 naming the field, offering the `tag.<key>` spelling, and listing the resource's available fields — see `crates/sauron-query/src/resolve.rs`' `resolve_field`, which documents why (a typo resolved to a tag nobody had ever written and answered 200 with zero rows). Rule 3's closing claim, *"the grammar never rejects a query as unknown field"*, is consequently false. And §8's envelope shipped exactly as drawn, but `clamped.field` carries the **resource's own window column** (`last_seen` / `occurred_at`), not the generic `"since"` in the example.

| Slice | Contents | Done when |
|---|---|---|
| S1 | `sauron-query` crate: grammar, AST, catalog, cost classifier | Pure unit tests green; zero behaviour change |
| S2a | Migrations 24–25, `schema.rs`/`models.rs`, `handled`+`sdk` ingest | An ingested event lands with non-NULL `handled` and populated `sdk`; EXPLAIN confirms each new index is chosen. No user-visible change; fully revertible via `down.sql` |
| S2b | `query_plan.rs` — `ResourceLower` trait, 3 leaf mappers, generic tree-walker, async `prepare` pass, cost/clamp policy | Exact SQL + binds asserted via `diesel::debug_query` for every `(Store, MatchOp)` pair across all 3 resources — **with no database**, so it runs in CI |
| S2c | **DONE (2026-08-10).** `query=`/`sort=`/`cursor=` params, response envelope, keyset paging, api clients, `CursorPagination.svelte`, **+ the `wiki/Search.md` syntax reference** | ✅ `query=` and `filter=` return identical rows for equivalent queries; deep paging no longer duplicates rows; a developer can read the grammar and use `query=` without reading the source |
| S3 | `issue_dimensions` rollup, backfill, autocomplete endpoints (migration 26) | Value autocomplete returns real data |
| S4 | `Popover`/`Combobox` primitives, `SearchBar` | Replaces `FilterBar` on the three core pages |
| S5 | Saved views end-to-end (migration 27, `view:write`, UI) | Create / share / set-default all work live |
| S6 | Sessions, Devices, Users, Transactions + all three doc surfaces + anti-rot test | Search works everywhere; docs cannot drift |

## 13. Risks

| Risk | Mitigation |
|---|---|
| ~~`jsonb_ops` GIN write amplification~~ — **MEASURED, and the GINs were dropped** | Seeded 59,665 error + 61,962 analytics events with production-shaped JSONB (9–13 keys, realistic cardinality). Result: **583 bytes/row of index against a 2050 bytes/row heap (+28%)**, and **9.0× write amplification** on 40k inserts (142 ms → 1273 ms; `jsonb_path_ops` was 4.4×). Decisive factor: every dimension on those columns is declared `IndexClass::Bounded`, not `Indexed`, so the shipped cost model never plans around them — they would buy only `has:` key existence for a query nothing issues until the planner lands. **Deferred to S2c**, where `CREATE INDEX` is additive and the real query mix is known. The reasoning is recorded in migration 25's `up.sql` so it is not silently re-added |
| `issue_dimensions` adds a write to the ingest hot path | Upsert batched with the existing issue upsert in the same transaction; cardinality bounded per issue |
| Two response shapes coexist in the API | Confined to the endpoints this feature touches; documented; remaining endpoints migrate when someone needs them |
| `OR` makes cost unpredictable | The planner's classifier sees an all-scan plan and clamps the window, returning the reason |
| `is:unhandled` is partial for ~30 days after upgrade | NULL is a distinct third state, never folded into either bucket; the UI notes the cutover. Self-heals once pre-upgrade partitions age out |
| The grammar rewrite invalidates documented behaviour | `filter=` and `q=` keep working and parse into the same AST; `wiki/Search.md` is rewritten in the same slice as the surfaces it describes |

## 14. Deployment notes

- This ships **five migrations** across the programme, not four: `…24_event_handled_sdk` and `…25_search_indexes` (S2a), `2026-08-09-000047_analytics_keyset_index` (S2c), plus the `issue_dimensions` rollup (S3) and `saved_views` (S5) still to come. RPM upgrades never re-run `sauron-migrate`, so an upgrade without a manual migrator run leaves new binaries against an old schema and scatters 500s. The release notes must say so.
- **Migration 47 takes a lock on `analytics_events` and every live child partition, in one transaction.** `CREATE INDEX CONCURRENTLY` is not available inside a migration transaction, so this needs a maintenance window sized to the largest table in the system. Its real-world lock duration is **unmeasured** — the scratch database it was verified against held one child partition and ~6k rows.
- Any new env var goes in `crates/sauron-core/src/config.rs`, the README table, **and** `.env.example`.
- Any new binary goes in `packaging/rpm/binaries.txt`.
- Per the standing rule: never create a branch, never commit. Changes are staged; the user commits.
