# Transaction `extra` + `tags` — design

**Date:** 2026-08-16
**Status:** approved, implementing

## Problem

A developer timing an HTTP call with `trackTransaction` can record `name`, `op`,
`duration_ms`, `status`, `http_method`, `http_status`, `url` — and nothing else.
There is no place to put the request body, the response body, an order id, or a
retry count. The motivating question was literally "how do I log request and
response body in the Flutter SDK", and the honest answer today is "you can't,
except by hanging it off an unrelated breadcrumb".

`tags` / `contexts` / `extra` already ship on errors, messages and `track()`
events across all 5 SDKs (migrations `2026-07-20-000016` / `000017`).
Transactions were deliberately excluded at the time, with the recorded reason
"Performance API is aggregate-only, no per-row display path". That reason has
since expired: `SessionDetail.svelte` renders a per-transaction timeline with a
slice modal, so individual spans now have somewhere to be seen.

What is still missing is a per-transaction *list*. `/performance/summary` and
`/performance/series` are both aggregates — `PerfSummaryRow` is one row per
operation (`name`, `op`, `count`, `p50/p75/p95`, `error_rate`). No endpoint
returns individual transaction rows, and no page lists them.

## Decisions

| Decision | Choice |
|---|---|
| Signal | Transactions (not sessions) |
| Fields | `extra` + `tags` (no `contexts`) |
| Inheritance | Per-call only — no scope merge |
| Oversize | SDK-side cap with truncation marker |
| Search home | New standalone Transactions page |
| Session display | Expandable timeline row |
| Gating | `event:read` alone |

## 1. Wire

`TransactionItem` (`backend/crates/sauron-core/src/envelope.rs:260`) gains:

```rust
/// Dev-supplied flat string→string tags attached to this transaction.
#[serde(default)]
pub tags: serde_json::Value,
/// Dev-supplied freeform JSON attached to this transaction.
#[serde(default)]
pub extra: serde_json::Value,
```

Snake_case keys `tags` / `extra`, matching the pinned contract the error and
analytics items already use. SDKs OMIT the keys entirely when empty, so an app
that never sets them is byte-identical to before. The backend never merges —
it stores the blob the SDK supplied, verbatim.

`contexts` is deliberately not added. Named context blocks are a debugging
affordance for errors; a span that wants structure can nest it inside `extra`.
Adding it later is additive and costs nothing we are paying now.

## 2. SDKs (all 5)

### API surface

Every SDK's `trackTransaction` gains optional `tags` and `extra`:

- **js / node** — `TransactionInput` gains `tags?: Record<string, string>` and
  `extra?: Record<string, unknown>`.
- **python** — `track_transaction(..., tags=None, extra=None)`.
- **csharp** — `TrackTransaction(..., IDictionary<string,string>? tags = null,
  IDictionary<string,object?>? extra = null)`.
- **flutter** — `trackTransaction(..., Map<String,String>? tags,
  Map<String,Object?>? extra)`.

Flutter additionally carries them through its stateful API, which no other SDK
has: `startTransaction(...)` accepts both, `ActiveTransaction` exposes them as
mutable fields, and `end({tags, extra})` overrides per top-level key. The
override rule matches `end`'s existing treatment of `status` / `httpStatus` /
`url`.

### Inheritance: per-call only

`setExtra()` / `setTag()` continue to feed errors, messages and `track()`, and
deliberately do **not** reach transactions. Transactions are the highest-volume
signal in the system — every navigation and every HTTP call — and inheriting a
global blob would write it onto every row. Request/response data is per-call by
nature, so the ergonomics point the same way as the storage cost.

This is a documented asymmetry, not an oversight. Each SDK's docstring for
`trackTransaction` must say so explicitly, because "extra works differently
here" is exactly the kind of thing a user discovers by finding an empty column.

### Size cap

`extra` is serialized and measured before the item enters the send queue. Past
`16_384` bytes of serialized JSON the whole map is replaced with:

```json
{"_truncated": true, "_bytes": 84213}
```

Replace-whole rather than truncate-per-key: a half-written JSON value is worse
than an honest marker, and per-key trimming makes the result depend on key
iteration order, which differs across the 5 languages.

The cap exists because ingest rejects any envelope over
`INGEST_MAX_BODY_BYTES` (1 MiB default, `backend/crates/sauron-core/src/config.rs:797`)
and envelopes are **batched** — one oversized response body would drop a whole
batch of unrelated spans with a 413. That is the silent-loss class this project
has already been bitten by.

`tags` reuse each SDK's existing tag normalization (string→string, existing key
count limits). No new tag rules.

Redaction stays where it already is: the existing `beforeSend` hook sees the
item with its `extra` attached and can scrub it.

## 3. Storage

New migration, additive on the partitioned parent (the shape migrations
`000016` / `000017` used):

```sql
ALTER TABLE transactions
    ADD COLUMN tags  JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN extra JSONB NOT NULL DEFAULT '{}'::jsonb;

CREATE INDEX transactions_tags_idx ON transactions
    USING GIN (tags jsonb_path_ops);
```

- GIN on `tags` — it is the cheap, high-selectivity dimension and the tag chip
  UI filters on it. Mirrors the events tag index.
- **No index on `extra`.** It is a containment/ILIKE probe
  (`IndexClass::Bounded`), the same treatment `extra` gets on
  occurrences/events. Indexing freeform JSON of unbounded shape buys little and
  costs write throughput on the busiest table.
- `NOT NULL DEFAULT '{}'` so existing rows and any client that omits the field
  land on `{}`, never `null`. The pipeline maps through the existing
  `object_or_empty()` null→`{}` guard for the same reason.

Diesel `schema.rs` gains both columns on `transactions`; `Transaction` (model),
`NewTransaction`, `insert_transaction` and the batch insert path all carry them.

**Tiering needs no change** — `sauron-tier` reads `SELECT *` and writes Parquet
with `union_by_name`, so new columns flow through and old Parquet files stay
readable.

## 4. Read API

### New route

`GET /v1/apps/{app_id}/transactions`, built on the existing search seam
(`backend/bins/sauron-api/src/routes/search.rs`) so it behaves identically to
the other searched lists:

- returns `SearchEnvelope<Transaction>` (`data`, `total`, `total_is_capped`,
  `next_cursor`, `clamped`)
- keyset cursor pagination, sort via the existing `SortSpec` machinery
- `?q=` free-text plus the `sauron-query` predicate language
- time window with planner clamp
- authorized on `event:read`

### Catalog

`backend/crates/sauron-query/src/catalog.rs`:

- add `Resource::Transactions` to the existing `extra` dimension's `resources`
  (currently `R_OCC_EVENTS`) — this is what makes `extra.order_id:123` resolve
- register a `tag` dimension for transactions, and make `tag_dimension()` in
  `search.rs:766` return the transactions table instead of `None`

`op`, `duration`, `url`, `http.status`, `http.method`, `name` are already
declared for `Resource::Transactions`, so `duration:>2s op:http` works the
moment the route exists.

### Gating — the invariant

This codebase holds a strict rule (`symbolicate.rs:214`): **what you may search
is exactly what you may read back**. Answering "does this column contain this
substring?" for a column you may not read spells the value out one byte at a
time. Request and response bodies are precisely the class of data that rule
exists for.

So:

- `strip_transaction_body(&mut Transaction)` nulls `extra` and `tags`, leaving
  the shell (`name`, `op`, `duration_ms`, `status`, `http_*`, timestamps) so a
  coarse-gated caller still sees that the span happened.
- `may_read_transaction_body(perms)` = `perms.contains(EVENT_READ)`. A span is
  not an issue, so `issue:read` is not required — but `sessions::detail` already
  authorizes on `event:read`, so this composes cleanly there.
- The list's free-text reach is derived from **the same** predicate, never a
  second copy of it. One function answers both questions; two copies drift, and
  the drift that matters (searchable wider than readable) is silent.
- Applies to **both** the new list route and the transactions array inside
  `sessions::detail`.

## 5. Dashboard

### Transactions page (new, top-level)

`dashboard/src/pages/Transactions.svelte`, sitting alongside Sessions and
Events:

- `DataTable` — name, op, duration (`LatencyBadge`), status, http method +
  status, occurred_at
- the shared search bar with query-language support and tag filter chips
- sortable columns through the existing `SortableTh` + keyset cursor pattern
- an extras indicator on rows that carry one, expanding to a `JsonTree`
- `DateRange` + `CachedView`, matching the other lists

Nav entry and a `PAGE_ACCESS` entry gated on `event:read`, so the page is not
merely hidden but unreachable without the permission — the gating map is the
single source of truth.

### Session detail

`Timeline.svelte` transaction rows that carry `extra` or `tags` get a chevron.
Expanding renders `JsonTree` inline (the component Issue detail's
Additional-data card already uses) plus a tags row. Rows without extras render
byte-identically to today — no chevron, no layout shift.

## 6. Testing

- **Wire fixtures** for all 5 SDKs through the existing `sdks/wire-fixtures`
  parity harness — one fixture with `tags` + `extra` set, one with neither, to
  pin the omit-when-empty rule.
- **Truncation test per SDK**: a >16 KB blob produces the marker, and the
  resulting envelope stays under the batch limit.
- **Catalog resolution tests**: `extra.x:` and `tag:` resolve on
  `Resource::Transactions` and render the expected SQL.
- **Gating test asserting the pair in one test** — a caller without
  `event:read` gets nulled `extra`/`tags` AND a narrowed `?q=` reach. Split
  across two tests, the half that matters can pass while the other rots.
- **Plan-shape guard** on the list query. A counts test cannot see a scan; this
  project's history is emphatic on that point.

## Risks

- **The 16 KB cap is a guess.** It should be measured against real payloads
  before it hardens into a contract. Too low and bodies are useless; too high
  and the busiest table grows fast.
- **Two JSONB columns on the highest-volume table.** Empty `{}` on existing
  rows, so growth tracks adoption rather than arriving on day one — but it does
  arrive.
- **The inheritance asymmetry is a documentation risk.** `setExtra` reaching
  errors but not spans is defensible and will still surprise people.

## Out of scope

- `contexts` on transactions (additive later if wanted)
- Backfilling anything — new columns start empty
- Indexing `extra`
- Any change to the aggregate Performance page
