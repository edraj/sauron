# Developer tags, contexts & extra across all SDKs

**Date:** 2026-07-20
**Status:** Approved design — ready for implementation planning
**Scope:** All 5 SDKs (js/browser, node, python, flutter, csharp) + wire contract + backend storage/ingest + dashboard display + a webapp example.

## Summary

A developer can attach three kinds of metadata to a captured signal:

- **`tags`** — flat `string → string` map, indexed/filterable.
- **`contexts`** — structured *named blocks*, e.g. `{ "order": { "id": 7 }, "feature_flags": { "beta": true } }`.
- **`extra`** — freeform JSON bag, e.g. `{ "cartValue": 42.5, "retries": 2 }`.

They can be set at **two moments**:

1. **Init defaults** — passed to `init(...)`, applied to every signal the client emits.
2. **Per-capture** — passed at the moment a specific error/message/event is captured (plus runtime scope setters in between).

This applies to **errors, messages, and analytics `track()` events**. Transactions are explicitly out of scope (see [Out of scope](#out-of-scope)).

The feature is delivered **end-to-end**: SDK API → wire contract → Postgres storage → dashboard display, so an attached payload actually persists and is visible.

## Current state (why this is real work, not just exposing a knob)

Mapped across all 5 SDKs and the backend:

| | init default `tags` | init default `contexts`/`extra` | runtime scope `tags` | runtime scope `contexts`/`extra` | per-capture `tags` | per-capture `contexts`/`extra` | reaches + persists server-side |
|---|---|---|---|---|---|---|---|
| **JS (browser)** | ✗ | ✗ | ✓ but **not exported** | ✗ (no concept) | ✗ (hint ignored) | ✗ | `tags` yes / `c+e` n/a |
| **Node** | ✗ | ✗ | ✓ `setTag` | ✓ `setContext`/`setExtra` | ✓ | ✗ | `tags` yes / **`c+e` silently dropped** |
| **Python** | ✗ | ✗ | ✓ `set_tag` | ✓ `set_context`/`set_extra` | ✓ (exc only) | ✗ (no per-call arg) | `tags` yes / **`c+e` silently dropped** |
| **Flutter** | ✗ | ✗ | ✗ **no tags at all** | ✗ | ✗ | ✗ | neither |
| **C#** | ✗ | ✗ | ✓ `SetTag` | ✓ `SetContext`/`SetExtra` | ✓ (exc only) | ✗ | `tags` yes / **`c+e` silently dropped** |

Key facts driving the design:

- **Init-level defaults are missing in all 5 SDKs.**
- **`contexts`/`extra` are broken or absent everywhere.** Node/Python/C# *expose* `setContext`/`setExtra`, but the wire `ErrorItem` has no field for them (only Python's `apply_to_error` even merges them onto the item) and the backend has no column — so they are silently discarded. JS and Flutter have no concept at all. Python emits `contexts`/`extra` keys today that the backend ignores (no `deny_unknown_fields`).
- **`tags` already persist and display end-to-end** — `error_events.tags` exists and `IssueDetail.svelte:250` renders them. So *tags* on errors are the template to mirror.
- The existing `context` (singular) column/field is **machine-owned** (device/os/app/runtime/user, rewritten by `enrich_context`). Developer `contexts` (plural) must be a **separate** column/field — never reuse `context`.

## Data model & merge semantics

Three independent top-level scopes on a signal: `tags`, `contexts`, `extra`.

- **Wire types** stay permissive JSON (`serde_json::Value` in Rust; `Record<string, unknown>` etc.) for backward compatibility. The *SDK-facing API* types `tags` as `string → string`; `contexts` as `map of named blocks`; `extra` as freeform.
- **Merge is SDK-side**, before send. The backend stores the already-merged final blob per item (identical to how `tags` works today — no backend merge logic).
- **Precedence** (per top-level key; per-call wins):
  1. Init defaults seed the global scope.
  2. Runtime scope setters mutate the scope (last-write-wins within the scope).
  3. Per-capture attachments override scope on key collision.
- Merge granularity: `tags` and `extra` merge by shallow key; `contexts` merge by block name (a per-call block replaces the scope block of the same name). This mirrors the existing Python `apply_to_error` behavior.

## Wire contract

File: `backend/crates/sauron-core/src/envelope.rs` (mirrored by every SDK).

- `ErrorItem`: add `contexts` + `extra` (`#[serde(default)]`). `tags` already present at line ~113.
- `AnalyticsItem`: add `tags` + `contexts` + `extra` (`#[serde(default)]`).
- `TransactionItem`: **unchanged** (out of scope).
- No struct uses `deny_unknown_fields`, and all fields are `#[serde(default)]`, so old payloads remain parseable and the backend `deserializes_golden_envelope` test does not break.

### Emit convention (parity)

Pick **one** convention across all 5 SDKs: **omit `tags`/`contexts`/`extra` when empty** (matches JS/Node optional-key style). C#/Flutter/Python currently always-emit keys; their golden fixtures are updated to include the new keys where the builder emits them. Empty maps should be omitted rather than sent as `{}` to keep payloads lean and keep "omits absent optional keys" tests meaningful.

### Golden fixtures — hard breaks, update in lockstep

The following assert exact serialized shape and **will fail** until updated alongside the model change:

- Backend: `envelope.rs` `GOLDEN` const (extend for coverage — deserialize-only, won't break) **and** the `AnalyticsItem { … }` struct literal in `roundtrips_item_tag` (compile break — no `..Default::default()`).
- Backend: `backend/bins/crebain/src/generator.rs` — `AnalyticsItem`, `TransactionItem`, `ErrorItem` struct literals (compile breaks).
- C#: `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs` — `GoldenJson` literal + `BuildGoldenEnvelope()` (strict per-object key-count; `JsonIgnoreCondition.Never`).
- Flutter: `sdks/flutter/test/envelope_test.dart` — `_golden` string, programmatic builders, and the exact `TransactionItem` map literal.
- Node: `sdks/node/test/envelope.test.ts` — `GOLDEN_ERROR`/`GOLDEN_EVENT` literals (live-client compare) + `normalize` if needed.
- Python: `sdks/python/tests/test_golden.py` — `GOLDEN_ERROR`/`GOLDEN_EVENT` literals + `_normalize`.
- JS: `sdks/js/test/envelope.test.ts` — `GOLDEN` (reflexive, won't auto-break; extend for coverage).

## Backend storage & ingest

### Migrations (new, dated after 2026-07-15-000015)

Both are additive `ALTER TABLE … ADD COLUMN` on the **partitioned parent** (precedent: `2026-07-15-000014_symbol_artifacts`). ADD COLUMN with a constant `DEFAULT` propagates to all partitions incl. the DEFAULT partition. Never alter child partitions; never rebuild the table.

- **Migration A — `error_events`:**
  ```sql
  ALTER TABLE error_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
  ALTER TABLE error_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
  ```
- **Migration B — `analytics_events`:**
  ```sql
  ALTER TABLE analytics_events ADD COLUMN tags     JSONB NOT NULL DEFAULT '{}'::jsonb;
  ALTER TABLE analytics_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
  ALTER TABLE analytics_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
  ```
- `down.sql` drops in reverse with `DROP COLUMN IF EXISTS`.

### Rust wiring

- `backend/crates/sauron-db/src/schema.rs` — add `contexts -> Jsonb`, `extra -> Jsonb` to the `error_events` `table!` block; add `tags`/`contexts`/`extra` to `analytics_events`. Keep field order in sync with the migration.
- `backend/crates/sauron-db/src/models.rs` — add fields to `ErrorEvent` + `NewErrorEvent` and `AnalyticsEvent` + `NewAnalyticsEvent`. Both the `Insertable` and `Selectable` structs must gain them or diesel `check_for_backend` fails to compile.
- `backend/crates/sauron-pipeline/src/process.rs` — in `process_error`, set `contexts`/`extra` from `e.contexts`/`e.extra` with the null→`{}` guard used for `tags` (lines ~200-204). In `process_event`, set `tags`/`contexts`/`extra` from the wire item (currently only `properties` is dev-controlled).

### Tiering — no change required

`backend/crates/sauron-tier/src/duck.rs` exports with `COPY (SELECT *, …)` and reads with `read_parquet(?, hive_partitioning=true, union_by_name=true)`. New columns flow into new Parquet files automatically; old Parquet files read the new columns as NULL. Cold reads are aggregate-count-only (`day/count/app_id`), so nothing there references the new fields. **Note:** per-row *detail* for cold-tiered rows is already hot-Postgres-only for every field (message, stacktrace, tags, context) — dev `contexts`/`extra` inherit that same, pre-existing behavior. No regression, no action.

## API read + dashboard display

- `ErrorEvent`/`AnalyticsEvent` are fetched via diesel `as_select()`, so adding the struct+schema fields **auto-fetches and auto-serializes** — no edits to `repo.rs`, `routes/issues.rs`, or `routes/analytics.rs`.
- `dashboard/src/lib/models/index.ts` — add `contexts?`, `extra?` to `ErrorEvent`; add `tags?`, `contexts?`, `extra?` to `AnalyticsEvent`.
- `dashboard/src/pages/IssueDetail.svelte` — the "Tags" card (lines ~248-252) and "Context" section (lines ~185-188) already exist. Add a **Contexts** card and an **Additional Data** (extra) card using `JsonTree` (`dashboard/src/lib/components/JsonTree.svelte`, already used in `Events.svelte`). `KeyValueList` is flat-only and must not be used for nested `contexts`/`extra`.
- `dashboard/src/pages/Events.svelte` — the detail row currently renders `properties` via `JsonTree`; add tags/contexts/extra blocks beside it.

## SDK surface (parity target — identical public API across all 5)

Common public surface every SDK must expose:

- **init options:** `tags`, `contexts`, `extra` (defaults).
- **scope setters:** `setTag(key, value)`, `setTags(map)`, `setContext(name, block)`, `setExtra(key, value)`. Setters only for the first cut (no getters/removers) unless a language idiom makes them trivial — keep the surface minimal.
- **per-capture:** optional `{ tags, contexts, extra }` on `captureException`, `captureMessage`, and `track`.

Per-SDK deltas:

- **JS (browser)** — `sdks/js/src/`:
  - `scope.ts`: add `contexts`/`extra` storage + `setContext`/`setExtra`/`setTags`.
  - `types.ts`: add fields to `InitOptions`/`ResolvedOptions`, to `ErrorItem` + `EventItem`, and a per-call options type for capture.
  - `client.ts`: `resolveOptions` carries init defaults; constructor seeds the scope; `enrichErrorItem` merges scope `contexts`/`extra`.
  - `api/capture.ts`: read per-call `tags`/`contexts`/`extra` in `buildErrorItem` and the `captureMessage` builder.
  - `api/product.ts` (`track`): accept + attach per-call attachments.
  - `index.ts`: export `setTag`, `setTags`, `setContext`, `setExtra` on the `Sauron` facade.
- **Node** — `sdks/node/src/`: extend `scope.ts` `applyToErrorItem` to merge `contexts`/`extra` (currently dropped); add init options in `types.ts`/`client.ts`; add per-call attachments to `captureMessage` + `track`; add `contexts`/`extra` to `ErrorItem` and `tags`/`contexts`/`extra` to `EventItem` wire types.
- **Python** — `sdks/python/sauron/`: `apply_to_error` already merges `contexts`/`extra`; add init kwargs in `__init__.py`/`_client.py` seeding the global scope; add per-call `extra`/`contexts` to `capture_exception` and `tags`/`extra`/`contexts` to `capture_message` and `track`; add the wire keys to the event item builder.
- **C#** — `sdks/csharp/Sauron/`: extend `Scope.ApplyToError` to merge `Contexts`/`Extra`; add `Tags`/`Contexts`/`Extra` to `SauronOptions`; add per-call params to `CaptureException`/`CaptureMessage`/`Track`; add `Contexts`/`Extra` to `ErrorItem` and to `EventItem` in `Envelope.cs`; update `EnvelopeGoldenTests`.
- **Flutter (largest)** — `sdks/flutter/lib/src/`: add `tags`+`contexts`+`extra` to `Scope`, `SauronOptions`, and the `Sauron` facade (setters); add `tags`/`contexts`/`extra`/`data` params to `captureException`/`captureMessage`/`track`; add serialization to `ErrorItem.toJson` + `EventItem.toJson` in `envelope.dart`; also fix the facade currently dropping the existing `level`/`screen` args.

## Webapp example — `examples/svelte-web`

Uses `@sauron/browser` (the JS SDK). Demonstrates both moments:

- `sauron.ts` `connect()`/`init(...)` seeded with **default** `tags`/`contexts`/`extra` (e.g. `tags: { app_tier: 'demo' }`, `contexts: { deployment: { region: 'eu-west' } }`, `extra: { build: '…' }`).
- An action card (new or extending `Showcase`/`Seeding`) that throws and captures an error with **per-call** attachments, e.g. `captureException(err, { tags: { feature: 'checkout' }, contexts: { order: { id: 42 } }, extra: { cartValue: 42.5, items: 3 } })`.
- Extend the existing `seedingSink` (already drives tags via `Scope`) to also drive `contexts`/`extra`.
- **Verified in-browser** with the preview tools: trigger the capture, confirm the outgoing envelope carries `tags`/`contexts`/`extra`, and (with a running backend) confirm IssueDetail renders them.

## Testing

- **Per-SDK unit tests:** init defaults land on emitted items; per-call overrides win on key collision; `contexts`/`extra` reach the built item (regression guard for the Node/C# "dropped" bug).
- **Golden fixtures:** updated in lockstep across all 5 SDKs + backend (see [Golden fixtures](#golden-fixtures--hard-breaks-update-in-lockstep)).
- **Backend:** extend the envelope deserialize test; add a `process_error`/`process_event` test asserting `tags`/`contexts`/`extra` persist to the row.
- **E2E:** the webapp example flows an error with all three → ingest → `IssueDetail` displays them.

## Implementation slices (ordering for the plan)

1. **Wire + backend** — `envelope.rs` fields, migrations A+B, `schema.rs`/`models.rs`, `process.rs`, and the compile-fix golden/crebain literals. The contract everything else depends on.
2. **Dashboard display** — frontend models + `IssueDetail` + `Events`.
3. **JS SDK + webapp example** — proves the whole vertical E2E in the browser early.
4. **Node, Python, C#** — fix the dropped `contexts`/`extra`, add init options + per-capture, update goldens.
5. **Flutter** — add tags from scratch + `contexts`/`extra` + goldens; fix the dropped `level`/`screen` facade args.

## Out of scope

- **Transactions** — the Performance API is aggregate-only (`GROUP BY name, op`); there is no raw-transaction endpoint or per-transaction UI. Adding dev metadata there would persist invisibly or require a whole new endpoint + view. Cleanly additive later using the identical wire/storage pattern. Decided out.
- **Tag-based filtering / search / grouping** on the dashboard — this pass is **display only**. Filtering aggregates by tag (e.g. p95 where `tenant=acme`) is a separate, larger feature (indexed tag filters).
- **`beforeSend` semantics changes** beyond passing the new fields through unchanged.

## Key risks / gotchas

- **`context` vs `contexts` naming clash** — one character apart, same table/struct. Keep names identical everywhere (Postgres column, diesel schema, Rust struct field, TS interface); label them distinctly in the UI ("Device Context" vs "Contexts"). `union_by_name` in DuckDB will NOT error on a typo — it silently NULL-fills, so consistency is load-bearing.
- **Golden fixtures fail hard** on the serialize side for C#/Flutter/Node/Python — model change and golden update must land in the same commit.
- **Diesel model completeness** — add the field to schema.rs, the migration, and BOTH the `Insertable` and `Selectable` structs together, or the crate won't compile.
- **Cold-tier detail gap is pre-existing** — not introduced by this feature; no action.
