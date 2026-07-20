# Developer tags, contexts & extra across all SDKs — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a developer attach `tags` (flat string→string), `contexts` (structured named blocks), and `extra` (freeform JSON) to errors, messages, and analytics `track()` events — both as init-time defaults and per-capture overrides — end-to-end across all 5 SDKs, the wire contract, Postgres storage, and the dashboard.

**Architecture:** The SDK owns the merge (init-defaults seeded into the global scope, runtime setters, then per-call overrides) and emits the final `tags`/`contexts`/`extra` blobs on each item — omitted when empty. The backend receives already-merged blobs (no server-side merge), stores them in dedicated JSONB columns on `error_events` and `analytics_events`, and the dashboard renders them. Transactions are out of scope.

**Tech Stack:** Rust (diesel/serde, `sauron-core`/`sauron-db`/`sauron-pipeline`, crebain), Svelte/TypeScript dashboard, and 5 SDKs — TypeScript (`@sauron/browser`, `@sauron/node`), Python, C# (.NET), Dart/Flutter.

**Design spec:** `docs/superpowers/specs/2026-07-20-sdk-tags-contexts-extra-design.md`

## Global Constraints

Every task's requirements implicitly include this section.

- **Wire keys are snake_case** (`tags`, `contexts`, `extra`) in `backend/crates/sauron-core/src/envelope.rs`. `ErrorItem` gains `contexts` + `extra`; `AnalyticsItem` gains `tags` + `contexts` + `extra`; all `serde_json::Value` with `#[serde(default)]`. `TransactionItem` is **unchanged**. Never add `#[serde(deny_unknown_fields)]`.
- **Emit convention (all 5 SDKs identical):** OMIT `tags`/`contexts`/`extra` from a serialized item when the map is empty — never emit `{}`.
- **SDK public API parity (same names/semantics in every SDK):** init options gain `tags`/`contexts`/`extra` (optional, default empty); facade setters `setTag(key,value)`, `setTags(map)`, `setContext(name, block)`, `setExtra(key, value)` (setters only — no getters/removers this pass); a per-capture options object `{ tags?, contexts?, extra? }` on `captureException`, `captureMessage`, and `track`.
- **Merge (SDK-side only; backend never merges):** effective scope = init-defaults seeded into the global scope, then runtime setters (last-write-wins); per-call values override scope **per top-level key**. `tags` and `extra` merge by shallow key; `contexts` merge by **block name** (a per-call block replaces the same-named scope block).
- **Naming trap:** the existing `context` (singular) column/field is machine-owned (device/os/app/runtime/user). The new dev `contexts` (plural) is DISTINCT — never reuse or overwrite `context`.
- **Backend storage:** additive `ALTER TABLE … ADD COLUMN … JSONB NOT NULL DEFAULT '{}'` on the **partitioned parent only** (precedent: `2026-07-15-000014_symbol_artifacts`). New migration dirs sort AFTER `2026-07-15-000015_source_read_perm` (use `2026-07-20-000016…`). `process_error`/`process_event` map the new wire fields with the existing `tags` null→`{}` guard. Parquet tiering (`sauron-tier`) needs **no change** (`SELECT *` export + `union_by_name` reads).
- **Golden fixtures:** update each SDK's golden fixture in the SAME task as the model change (C# `EnvelopeGoldenTests` strict key-count, Flutter `envelope_test.dart` exact maps, Node & Python `GOLDEN_*` live-client compares hard-break; JS + backend goldens are reflexive/deserialize-only — extend for coverage).
- **Out of scope:** transactions; tag-based filtering/search/grouping on the dashboard; any `beforeSend` semantics change beyond passing the new fields through.
- **Slice order:** 1 (backend) → 2 (dashboard) → 3 (JS + example) → 4 (Node) → 5 (Python) → 6 (C#) → 7 (Flutter). Slices 4–7 depend only on Slice 1's wire contract and are mutually independent.

---


## Slice 1 — Backend: wire contract + storage + ingest

### Task 1.1: Wire fields on ErrorItem + AnalyticsItem (envelope.rs)

**Files:**
- Modify/Test: `backend/crates/sauron-core/src/envelope.rs` (ErrorItem 99-135, AnalyticsItem 207-221, `roundtrips_item_tag` 448-461, tests mod)

**Interfaces:**
- Consumes: existing `ErrorItem` (already has `pub tags: serde_json::Value`), `AnalyticsItem { name, distinct_id, properties, timestamp, session_id, screen }`, `Envelope`, `EnvelopeItem`.
- Produces: `ErrorItem.contexts`, `ErrorItem.extra`, `AnalyticsItem.tags`, `AnalyticsItem.contexts`, `AnalyticsItem.extra` — all `serde_json::Value`, `#[serde(default)]`. Later tasks (process.rs, crebain) read these.

- [ ] **Step 1: Write the failing deserialize test.** Add to the `tests` module (after `parses_transaction_item`, before `roundtrips_item_tag`):
```rust
    #[test]
    fn parses_error_and_event_scopes() {
        // New dev-owned scopes: tags/contexts/extra parse on BOTH errors and events.
        let json = r#"{
            "header": { "sdk": { "name": "t", "version": "0" } },
            "items": [
                { "type": "error", "timestamp": "2026-07-20T10:00:00Z",
                  "exception": { "type": "X" },
                  "tags": { "region": "eu" },
                  "contexts": { "order": { "id": 7 } },
                  "extra": { "cart": [1, 2] } },
                { "type": "event", "name": "checkout", "distinct_id": "u1",
                  "tags": { "plan": "pro" },
                  "contexts": { "trip": { "n": 1 } },
                  "extra": { "note": "x" } }
            ]
        }"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        match &env.items[0] {
            EnvelopeItem::Error(e) => {
                assert_eq!(e.tags["region"], "eu");
                assert_eq!(e.contexts["order"]["id"], 7);
                assert_eq!(e.extra["cart"][1], 2);
            }
            other => panic!("expected error, got {other:?}"),
        }
        match &env.items[1] {
            EnvelopeItem::Event(ev) => {
                assert_eq!(ev.tags["plan"], "pro");
                assert_eq!(ev.contexts["trip"]["n"], 1);
                assert_eq!(ev.extra["note"], "x");
            }
            other => panic!("expected event, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run — expect a COMPILE failure** (fields don't exist yet).
```
cargo test -p sauron-core
```
Expected: `error[E0609]: no field `contexts` on type `&ErrorItem`` (and `extra` / `tags` on `AnalyticsItem`).

- [ ] **Step 3: Add `contexts` + `extra` to `ErrorItem`.** Replace the existing `tags` field (lines 112-113):
```rust
    #[serde(default)]
    pub tags: serde_json::Value,
    /// Dev-supplied structured context blocks (e.g. {"order":{"id":7}}). DISTINCT
    /// from the envelope-wide machine `context` — never conflate the two.
    #[serde(default)]
    pub contexts: serde_json::Value,
    /// Dev-supplied freeform JSON attached to this error.
    #[serde(default)]
    pub extra: serde_json::Value,
```

- [ ] **Step 4: Add `tags` + `contexts` + `extra` to `AnalyticsItem`.** Replace the trailing `screen` field (lines 218-220):
```rust
    /// Current screen/route the SDK was on when the event was tracked.
    #[serde(default)]
    pub screen: Option<String>,
    /// Dev-supplied flat string tags for this track() event.
    #[serde(default)]
    pub tags: serde_json::Value,
    /// Dev-supplied structured context blocks (DISTINCT from machine `context`).
    #[serde(default)]
    pub contexts: serde_json::Value,
    /// Dev-supplied freeform JSON attached to this event.
    #[serde(default)]
    pub extra: serde_json::Value,
```

- [ ] **Step 5: Fix the `roundtrips_item_tag` literal** (now missing three fields) and extend it to cover them. Replace the `AnalyticsItem { ... }` block (lines 449-456) and add assertions:
```rust
        let item = EnvelopeItem::Event(AnalyticsItem {
            name: "signed_up".into(),
            distinct_id: "u_1".into(),
            properties: serde_json::json!({ "plan": "free" }),
            timestamp: Utc::now(),
            session_id: None,
            screen: None,
            tags: serde_json::json!({ "tier": "gold" }),
            contexts: serde_json::json!({ "order": { "id": 7 } }),
            extra: serde_json::json!({ "trace": "abc" }),
        });
        let s = serde_json::to_string(&item).unwrap();
        assert!(s.contains("\"type\":\"event\""));
        let back: EnvelopeItem = serde_json::from_str(&s).unwrap();
        match back {
            EnvelopeItem::Event(ev) => {
                assert_eq!(ev.tags["tier"], "gold");
                assert_eq!(ev.contexts["order"]["id"], 7);
                assert_eq!(ev.extra["trace"], "abc");
            }
            other => panic!("expected event, got {other:?}"),
        }
```
(Delete the old trailing `let s = ...; assert!(...); let back = ...; matches!(...)` lines this block replaces.)

- [ ] **Step 6: Run — expect PASS.**
```
cargo test -p sauron-core
```
Expected: `test result: ok.` including `parses_error_and_event_scopes` and `roundtrips_item_tag`.

- [ ] **Step 7: Commit.**
```
git add backend/crates/sauron-core/src/envelope.rs
git commit -m "feat(core): add contexts/extra to ErrorItem and tags/contexts/extra to AnalyticsItem"
```

---

### Task 1.2: Additive migrations for error_events + analytics_events

**Files:**
- Create: `backend/migrations/2026-07-20-000016_error_events_scopes/up.sql`
- Create: `backend/migrations/2026-07-20-000016_error_events_scopes/down.sql`
- Create: `backend/migrations/2026-07-20-000017_analytics_events_scopes/up.sql`
- Create: `backend/migrations/2026-07-20-000017_analytics_events_scopes/down.sql`

**Interfaces:**
- Consumes: partitioned parents `error_events` / `analytics_events`; precedent `2026-07-15-000014_symbol_artifacts` (ALTER TABLE ADD COLUMN on the partitioned parent). `MIGRATIONS = embed_migrations!("../../migrations")` in `backend/crates/sauron-db/src/lib.rs:23` picks up new dirs at compile time.
- Produces: columns `error_events.contexts`, `error_events.extra`, `analytics_events.tags`, `analytics_events.contexts`, `analytics_events.extra` (all `JSONB NOT NULL DEFAULT '{}'`). Dirs sort AFTER `2026-07-15-000015_source_read_perm`.

- [ ] **Step 1: Create the error_events migration.** `backend/migrations/2026-07-20-000016_error_events_scopes/up.sql`:
```sql
-- 0016: developer-supplied metadata scopes on error_events. `contexts` (plural)
-- is the dev-owned structured-block map and `extra` is freeform JSON — both are
-- DISTINCT from the machine-owned `context` (singular) column; never conflate.
-- ADD COLUMN on the partitioned parent propagates to every partition.
ALTER TABLE error_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE error_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
```

- [ ] **Step 2: `backend/migrations/2026-07-20-000016_error_events_scopes/down.sql`:**
```sql
ALTER TABLE error_events DROP COLUMN IF EXISTS extra;
ALTER TABLE error_events DROP COLUMN IF EXISTS contexts;
```

- [ ] **Step 3: Create the analytics_events migration.** `backend/migrations/2026-07-20-000017_analytics_events_scopes/up.sql`:
```sql
-- 0017: developer-supplied metadata scopes on analytics_events. `tags` (flat
-- string->string), `contexts` (dev-owned structured blocks — DISTINCT from the
-- machine-owned `context` column), `extra` (freeform JSON). ADD COLUMN on the
-- partitioned parent propagates to every partition.
ALTER TABLE analytics_events ADD COLUMN tags     JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE analytics_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE analytics_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
```

- [ ] **Step 4: `backend/migrations/2026-07-20-000017_analytics_events_scopes/down.sql`:**
```sql
ALTER TABLE analytics_events DROP COLUMN IF EXISTS extra;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS contexts;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS tags;
```

- [ ] **Step 5: Verify the dirs embed (build the crate that runs `embed_migrations!`).** A dir missing `up.sql`/`down.sql` fails this build.
```
cargo build -p sauron-db
```
Expected: `Finished` (no `error: failed to embed migrations`).

- [ ] **Step 6: Apply + roll back against a local DB (if `DATABASE_URL` is set), then re-apply.**
```
DATABASE_URL="$DATABASE_URL" diesel migration run    --migration-dir backend/migrations
DATABASE_URL="$DATABASE_URL" diesel migration revert  --migration-dir backend/migrations
DATABASE_URL="$DATABASE_URL" diesel migration revert  --migration-dir backend/migrations
DATABASE_URL="$DATABASE_URL" diesel migration run    --migration-dir backend/migrations
```
Expected: `Running migration 2026-07-20-000016_error_events_scopes` and `...000017_analytics_events_scopes`; two reverts clean drop; final run re-adds. (Skip if no DB — Step 5 already proves embedding.)

- [ ] **Step 7: Commit.**
```
git add backend/migrations/2026-07-20-000016_error_events_scopes backend/migrations/2026-07-20-000017_analytics_events_scopes
git commit -m "feat(db): additive migrations for error_events/analytics_events metadata scopes"
```

---

### Task 1.3: Add columns to schema.rs

**Files:**
- Modify: `backend/crates/sauron-db/src/schema.rs` (analytics_events 4-20, error_events 46-75)

**Interfaces:**
- Consumes: Diesel `table!` blocks; new columns from Task 1.2 (appended physically last by `ADD COLUMN`).
- Produces: `error_events::contexts`, `error_events::extra`, `analytics_events::tags`, `analytics_events::contexts`, `analytics_events::extra` (all `Jsonb`). Column order matches on-disk (new columns last) so Task 1.4's `Selectable` models line up.

- [ ] **Step 1: Append to `error_events`.** After `debug_meta -> Nullable<Jsonb>,` (line 73), inside the `table!`:
```rust
        debug_meta -> Nullable<Jsonb>,
        contexts -> Jsonb,
        extra -> Jsonb,
```

- [ ] **Step 2: Append to `analytics_events`.** After `screen -> Nullable<Text>,` (line 18):
```rust
        screen -> Nullable<Text>,
        tags -> Jsonb,
        contexts -> Jsonb,
        extra -> Jsonb,
```

- [ ] **Step 3: Verify the crate still compiles** (columns unused by models yet — that's fine).
```
cargo build -p sauron-db
```
Expected: `Finished`.

- [ ] **Step 4: Commit.**
```
git add backend/crates/sauron-db/src/schema.rs
git commit -m "feat(db): schema columns for error_events/analytics_events metadata scopes"
```

---

### Task 1.4: Add fields to row + insert models

**Files:**
- Modify: `backend/crates/sauron-db/src/models.rs` (ErrorEvent 217-250, NewErrorEvent 252-283, AnalyticsEvent 289-307, NewAnalyticsEvent 309-325)

**Interfaces:**
- Consumes: schema columns from Task 1.3; `serde_json::Value` is imported as `Value` (line 10).
- Produces: `ErrorEvent.contexts/extra`, `NewErrorEvent.contexts/extra`, `AnalyticsEvent.tags/contexts/extra`, `NewAnalyticsEvent.tags/contexts/extra`. `process.rs` (Tasks 1.5/1.6) constructs the `New*` structs.

- [ ] **Step 1: `ErrorEvent`** — after `pub debug_meta: Option<Value>,` (line 249):
```rust
    pub debug_meta: Option<Value>,
    /// Dev-supplied structured context blocks (distinct from machine `context`).
    pub contexts: Value,
    /// Dev-supplied freeform JSON.
    pub extra: Value,
```

- [ ] **Step 2: `NewErrorEvent`** — after `pub debug_meta: Option<Value>,` (line 282):
```rust
    pub debug_meta: Option<Value>,
    pub contexts: Value,
    pub extra: Value,
```

- [ ] **Step 3: `AnalyticsEvent`** — after `pub screen: Option<String>,` (line 306):
```rust
    pub screen: Option<String>,
    pub tags: Value,
    /// Dev-supplied structured context blocks (distinct from machine `context`).
    pub contexts: Value,
    pub extra: Value,
```

- [ ] **Step 4: `NewAnalyticsEvent`** — after `pub screen: Option<String>,` (line 324):
```rust
    pub screen: Option<String>,
    pub tags: Value,
    pub contexts: Value,
    pub extra: Value,
```

- [ ] **Step 5: Verify the crate compiles** (`check_for_backend(Pg)` validates each new field against the Task 1.3 columns).
```
cargo build -p sauron-db
```
Expected: `Finished`. NOTE: a full-workspace `cargo build` will now FAIL in `sauron-pipeline` (the `New*` literals in `process.rs` are missing the new fields) — that break is fixed in Tasks 1.5/1.6.

- [ ] **Step 6: Commit.**
```
git add backend/crates/sauron-db/src/models.rs
git commit -m "feat(db): model fields for error_events/analytics_events metadata scopes"
```

---

### Task 1.5: Extract + test the null->{} guard helper (process.rs)

**Files:**
- Modify/Test: `backend/crates/sauron-pipeline/src/process.rs` (guards at 200-204, 268-272, 308-312; helpers section 387-435; new tests mod)

**Interfaces:**
- Consumes: `serde_json::{json, Value}` (already imported, line 3).
- Produces: `fn object_or_empty(v: Value) -> Value` (module-private) — reused by Task 1.6 for `contexts`/`extra`/`tags`.

- [ ] **Step 1: Write the failing unit test.** Append a tests module at the end of `process.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::object_or_empty;
    use serde_json::json;

    #[test]
    fn object_or_empty_maps_null_to_empty_object() {
        assert_eq!(object_or_empty(serde_json::Value::Null), json!({}));
    }

    #[test]
    fn object_or_empty_preserves_non_empty_maps() {
        assert_eq!(object_or_empty(json!({ "region": "eu" })), json!({ "region": "eu" }));
        assert_eq!(
            object_or_empty(json!({ "order": { "id": 7 } })),
            json!({ "order": { "id": 7 } })
        );
    }
}
```

- [ ] **Step 2: Run — expect a COMPILE failure** (helper does not exist).
```
cargo test -p sauron-pipeline
```
Expected: `error[E0432]: unresolved import `super::object_or_empty``.

- [ ] **Step 3: Add the helper.** In the `// --- helpers ---` section (after `fn distinct_id` at line 391):
```rust
/// Normalize a dev-supplied scope map for JSONB storage: `null` (the serde
/// default for an omitted key) becomes an empty object so the column is never
/// NULL; any other value passes through verbatim. The backend does not merge —
/// the SDK ships the already-merged effective scope.
fn object_or_empty(v: Value) -> Value {
    if v.is_null() {
        json!({})
    } else {
        v
    }
}
```

- [ ] **Step 4: Refactor the three existing inline guards to use it.** In `process_error`, replace the `tags:` field block (lines 200-204):
```rust
            tags: object_or_empty(e.tags.clone()),
```
In `process_event`, replace the `properties:` field block (lines 268-272):
```rust
            properties: object_or_empty(ev.properties),
```
In `process_identify`, replace the `traits` binding (lines 308-312):
```rust
    let traits = object_or_empty(id.traits);
```

- [ ] **Step 5: Run — expect PASS.**
```
cargo test -p sauron-pipeline
```
Expected: `test result: ok.` with `object_or_empty_maps_null_to_empty_object` and `object_or_empty_preserves_non_empty_maps`.

- [ ] **Step 6: Commit.**
```
git add backend/crates/sauron-pipeline/src/process.rs
git commit -m "refactor(pipeline): extract object_or_empty null->{} guard with unit tests"
```

---

### Task 1.6: Persist contexts/extra/tags in process_error + process_event

**Files:**
- Modify: `backend/crates/sauron-pipeline/src/process.rs` (NewErrorEvent literal 185-217, NewAnalyticsEvent literal 260-282)

**Interfaces:**
- Consumes: `object_or_empty` (Task 1.5); `NewErrorEvent.contexts/extra` + `NewAnalyticsEvent.tags/contexts/extra` (Task 1.4); `ErrorItem.contexts/extra` + `AnalyticsItem.tags/contexts/extra` (Task 1.1).
- Produces: end-to-end mapping wire item -> DB row for all three dev scopes.

- [ ] **Step 1: Map error scopes.** In the `NewErrorEvent { ... }` literal, immediately after the `tags: object_or_empty(e.tags.clone()),` line, add:
```rust
            tags: object_or_empty(e.tags.clone()),
            contexts: object_or_empty(e.contexts.clone()),
            extra: object_or_empty(e.extra.clone()),
```

- [ ] **Step 2: Map event scopes.** In the `NewAnalyticsEvent { ... }` literal, after the `screen: ev.screen.clone(),` line (the current last field), add:
```rust
            screen: ev.screen.clone(),
            tags: object_or_empty(ev.tags),
            contexts: object_or_empty(ev.contexts),
            extra: object_or_empty(ev.extra),
```

- [ ] **Step 3: Verify the crate compiles and tests stay green** (the `New*` literals are now complete).
```
cargo build -p sauron-pipeline && cargo test -p sauron-pipeline
```
Expected: `Finished`, then `test result: ok.`.

- [ ] **Step 4: Commit.**
```
git add backend/crates/sauron-pipeline/src/process.rs
git commit -m "feat(pipeline): persist contexts/extra on errors and tags/contexts/extra on events"
```

---

### Task 1.7: Fix crebain generator struct literals

**Files:**
- Modify: `backend/bins/crebain/src/generator.rs` (AnalyticsItem literal 133-140, ErrorItem literal 172-213)

**Interfaces:**
- Consumes: `AnalyticsItem` (now +tags/contexts/extra) and `ErrorItem` (now +contexts/extra) from Task 1.1. `json` + fields (`user`, `seq`, `pick`, `lineno`) already in scope. `TransactionItem` is UNCHANGED — do not touch its literal.
- Produces: a compiling `crebain` and a green full-workspace build; load traffic now carries dev scopes.

- [ ] **Step 1: Complete the `AnalyticsItem` literal in `event_envelope`.** After `screen: Some(user.screen.to_string()),` (line 139), add:
```rust
        screen: Some(user.screen.to_string()),
        tags: json!({ "screen": user.screen }),
        contexts: json!({ "session": { "seq": seq } }),
        extra: json!({ "value": pick % 100 }),
```

- [ ] **Step 2: Complete the `ErrorItem` literal in `issue_envelope`.** After `tags: json!({ "screen": user.screen }),` (line 206), insert `contexts`/`extra` before `fingerprint: None,`:
```rust
        tags: json!({ "screen": user.screen }),
        contexts: json!({ "issue": { "seq": seq } }),
        extra: json!({ "lineno": lineno }),
        fingerprint: None,
```

- [ ] **Step 3: Verify crebain compiles.**
```
cargo build -p crebain
```
Expected: `Finished` (no `error[E0063]: missing fields ... in initializer of ...Item`).

- [ ] **Step 4: Verify the whole slice — full-workspace build + all touched-crate tests.**
```
cargo build && cargo test -p sauron-core -p sauron-db -p sauron-pipeline
```
Expected: workspace `Finished`; `test result: ok.` for all three crates.

- [ ] **Step 5: Commit.**
```
git add backend/bins/crebain/src/generator.rs
git commit -m "test(crebain): emit tags/contexts/extra on generated events and errors"
```



## Slice 2 — Dashboard: models + IssueDetail + Events display

### Task 2.1: Extend `ErrorEvent` and `AnalyticsEvent` TS models with the dev scopes

**Files:**
- Modify: `dashboard/src/lib/models/index.ts` (`ErrorEvent` 253-278, `AnalyticsEvent` 321-336)

**Interfaces:**
- Consumes: wire keys `contexts`/`extra` on error events and `tags`/`contexts`/`extra` on analytics events (snake_case JSON, omitted when empty per the emit convention → optional on the type). Existing `ErrorEvent.tags: Record<string, unknown> | null` and the machine-owned `context?: Record<string, unknown> | null` stay as-is.
- Produces: `ErrorEvent.contexts?`, `ErrorEvent.extra?`, `AnalyticsEvent.tags?`, `AnalyticsEvent.contexts?`, `AnalyticsEvent.extra?` — all `Record<string, unknown> | null | undefined` — consumed by Tasks 2.2 and 2.3.

- [ ] **Step 1: Establish the green baseline.** Run the typecheck before touching anything so a later failure is attributable.
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
  ```
  Expected: `svelte-check found 0 errors and 0 warnings ...`

- [ ] **Step 2: Add `contexts`/`extra` to `ErrorEvent`.** In `dashboard/src/lib/models/index.ts`, replace the `tags` line inside `ErrorEvent` (line 266). Note the NAMING TRAP: these dev scopes are DISTINCT from the existing machine-owned `context` (singular) at line 265 — do not touch `context`.
  ```ts
  context: Record<string, unknown> | null;
  tags: Record<string, unknown> | null;
  // Developer-attached scopes (distinct from the machine-owned `context` above).
  // Omitted by SDKs when empty, so treat as optional on the wire.
  contexts?: Record<string, unknown> | null;
  extra?: Record<string, unknown> | null;
  ```

- [ ] **Step 3: Add `tags`/`contexts`/`extra` to `AnalyticsEvent`.** In the same file, replace the `properties` line inside `AnalyticsEvent` (line 327). Again, keep the machine `context?` (line 328) untouched and distinct.
  ```ts
  properties: Record<string, unknown> | null;
  // Developer-attached scopes (distinct from the machine-owned `context` below).
  // Omitted by SDKs when empty, so treat as optional on the wire.
  tags?: Record<string, unknown> | null;
  contexts?: Record<string, unknown> | null;
  extra?: Record<string, unknown> | null;
  context?: Record<string, unknown> | null;
  ```

- [ ] **Step 4: Typecheck stays green (mechanical edit — additive optional fields).**
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
  ```
  Expected: `svelte-check found 0 errors and 0 warnings ...` (no consumers yet; this unblocks 2.2/2.3, which reference these fields and would fail `Property 'contexts' does not exist on type 'ErrorEvent'` without this task).

- [ ] **Step 5: Commit.**
  ```bash
  cd /home/splimter/projects/freelance/sauron && git add dashboard/src/lib/models/index.ts && git commit -m "feat(dashboard): add contexts/extra to ErrorEvent and tags/contexts/extra to AnalyticsEvent models"
  ```

---

### Task 2.2: IssueDetail — render dev `contexts` and `extra` as rail cards via `JsonTree`

**Files:**
- Modify: `dashboard/src/pages/IssueDetail.svelte` (imports 1-20; Tags card block 248-252)

**Interfaces:**
- Consumes: `ErrorEvent.contexts?`, `ErrorEvent.extra?` (from Task 2.1); `latestEvent = $derived(issue?.latest_event ?? null)` (line 81); `JsonTree` props `{ value: unknown; name?: string | null; depth?: number; expandTo?: number }`; house `Card` with `title` prop; global `.faint` class (`src/app.css`).
- Produces: two new rail cards ("Contexts", "Additional data") in the `<aside class="rail">`, guarded by non-empty with a faint fallback (mirrors the Events.svelte pattern). No new component API.

- [ ] **Step 1: Add the two cards WITHOUT the import (drives the red).** In `dashboard/src/pages/IssueDetail.svelte`, replace the existing Tags card block (lines 248-252) — extend the same `{#if latestEvent}` guard:
  ```svelte
        {#if latestEvent}
          <Card title="Tags">
            <KeyValueList data={latestEvent.tags} emptyLabel="No tags" />
          </Card>

          <Card title="Contexts">
            {#if latestEvent.contexts && Object.keys(latestEvent.contexts).length > 0}
              <JsonTree value={latestEvent.contexts} name="contexts" expandTo={2} />
            {:else}
              <span class="faint">No contexts</span>
            {/if}
          </Card>

          <Card title="Additional data">
            {#if latestEvent.extra && Object.keys(latestEvent.extra).length > 0}
              <JsonTree value={latestEvent.extra} name="extra" expandTo={2} />
            {:else}
              <span class="faint">No additional data</span>
            {/if}
          </Card>
        {/if}
  ```

- [ ] **Step 2: Run the typecheck — expect it to FAIL (JsonTree not imported).**
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
  ```
  Expected: failure in `src/pages/IssueDetail.svelte`, `Cannot find name 'JsonTree'. (ts 2304)` at the two `<JsonTree ... />` usages.

- [ ] **Step 3: Add the `JsonTree` import.** In the `<script>` block, add the import immediately after the `KeyValueList` import (line 14):
  ```ts
  import KeyValueList from '../lib/components/KeyValueList.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  ```

- [ ] **Step 4: Re-run the typecheck — expect PASS.**
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
  ```
  Expected: `svelte-check found 0 errors and 0 warnings ...`

- [ ] **Step 5: Visual verification via preview (no component-test harness for this page).** Start the dev server and open an issue whose latest event carries `contexts`/`extra` (e.g. seed via `examples/svelte-web` Seeding, or an issue captured with a per-call `{contexts, extra}`).
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run dev
  ```
  Expected in the right rail under Tags: a **Contexts** card and an **Additional data** card. Non-empty scopes render as an expandable `JsonTree` (auto-expanded 2 levels); empty scopes show the faint `No contexts` / `No additional data` fallback. Confirm the machine **Context** section in the main event body (KeyValueList, line 187) is unchanged and distinct.

- [ ] **Step 6: Commit.**
  ```bash
  cd /home/splimter/projects/freelance/sauron && git add dashboard/src/pages/IssueDetail.svelte && git commit -m "feat(dashboard): show error contexts and extra as JsonTree cards on IssueDetail"
  ```

---

### Task 2.3: Events — add `tags`/`contexts`/`extra` blocks to the expandable event detail row

**Files:**
- Modify: `dashboard/src/pages/Events.svelte` (detail row 370-389; `JsonTree` already imported at line 12)

**Interfaces:**
- Consumes: `AnalyticsEvent.tags?`, `AnalyticsEvent.contexts?`, `AnalyticsEvent.extra?` (from Task 2.1); loop var `ev` in `{#each streamEvents as ev}`; already-imported `JsonTree`; existing detail row `<td colspan={5}>` with the `properties` block + `.faint` fallback.
- Produces: three additional `JsonTree` blocks inside the expanded detail row, each guarded by non-empty (no fallback needed — the existing `properties` block already carries the empty-state message).

- [ ] **Step 1: Add the three scope blocks.** In `dashboard/src/pages/Events.svelte`, replace the `properties` block inside the detail row (lines 382-386), appending the new guarded blocks after it (still inside the same `<td colspan={5}>`):
  ```svelte
                    {#if ev.properties && Object.keys(ev.properties).length > 0}
                      <JsonTree value={ev.properties} name="properties" expandTo={2} />
                    {:else}
                      <span class="faint">No properties on this event.</span>
                    {/if}
                    {#if ev.tags && Object.keys(ev.tags).length > 0}
                      <JsonTree value={ev.tags} name="tags" expandTo={2} />
                    {/if}
                    {#if ev.contexts && Object.keys(ev.contexts).length > 0}
                      <JsonTree value={ev.contexts} name="contexts" expandTo={2} />
                    {/if}
                    {#if ev.extra && Object.keys(ev.extra).length > 0}
                      <JsonTree value={ev.extra} name="extra" expandTo={2} />
                    {/if}
  ```

- [ ] **Step 2: Typecheck.** (`JsonTree` and the `AnalyticsEvent` scope fields both already exist, so this is additive.)
  ```bash
  cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
  ```
  Expected: `svelte-check found 0 errors and 0 warnings ...`

- [ ] **Step 3: Visual verification via preview.** With the dev server running (`npm run dev` from `dashboard/`), open the Events page, expand a `track()` event row that was captured with `tags`/`contexts`/`extra`.
  Expected: below the `properties` tree, up to three additional `JsonTree` subtrees labelled `tags`, `contexts`, `extra`, each shown only when non-empty; events without those scopes show only `properties` (or the existing faint `No properties on this event.` fallback) — no empty `{}` blocks appear.

- [ ] **Step 4: Commit.**
  ```bash
  cd /home/splimter/projects/freelance/sauron && git add dashboard/src/pages/Events.svelte && git commit -m "feat(dashboard): show analytics tags/contexts/extra in Events detail row"
  ```



## Slice 3 — JS browser SDK + webapp example (proves the vertical E2E)

### Task 3.1: Scope gains contexts/extra storage + setTags/setContext/setExtra + mergeMeta helper

**Files:**
- Modify: `sdks/js/src/scope.ts` (fields ~17, setters ~50-52, new top-level helper)
- Create/Test: `sdks/js/test/scope.test.ts`

**Interfaces:**
- Consumes: existing `class Scope { readonly tags: Record<string,string> = {}; setTag(key,value): void }` (scope.ts:13-52)
- Produces: `Scope.contexts: Record<string, unknown>`, `Scope.extra: Record<string, unknown>`, `Scope.setTags(tags: Record<string,string>): void`, `Scope.setContext(name: string, block: Record<string, unknown>): void`, `Scope.setExtra(key: string, value: unknown): void`, and `export function mergeMeta(base: Record<string, unknown>, override?: Record<string, unknown>): Record<string, unknown>` (shallow-merge, override wins per top-level key — used for tags/extra by key and contexts by block name)

- [ ] **Step 1: Write the failing unit test** — create `sdks/js/test/scope.test.ts`:
```ts
import { describe, expect, it } from 'vitest';
import { Scope, mergeMeta } from '../src/scope';

describe('Scope metadata scopes (tags/contexts/extra)', () => {
  it('starts with empty tags/contexts/extra', () => {
    const s = new Scope();
    expect(s.tags).toEqual({});
    expect(s.contexts).toEqual({});
    expect(s.extra).toEqual({});
  });

  it('setTag / setTags merge by key (last-write-wins)', () => {
    const s = new Scope();
    s.setTag('a', '1');
    s.setTags({ b: '2', a: '3' });
    expect(s.tags).toEqual({ a: '3', b: '2' });
  });

  it('setContext replaces a whole block by name', () => {
    const s = new Scope();
    s.setContext('order', { id: 1 });
    s.setContext('order', { id: 2, total: 9 });
    s.setContext('page', { path: '/cart' });
    expect(s.contexts).toEqual({ order: { id: 2, total: 9 }, page: { path: '/cart' } });
  });

  it('setExtra sets freeform values by key', () => {
    const s = new Scope();
    s.setExtra('build', 'ci-42');
    s.setExtra('flag', true);
    expect(s.extra).toEqual({ build: 'ci-42', flag: true });
  });
});

describe('mergeMeta', () => {
  it('returns a fresh copy of base when no override', () => {
    const base = { a: 1 };
    const out = mergeMeta(base);
    expect(out).toEqual({ a: 1 });
    expect(out).not.toBe(base);
  });

  it('lets the override win per top-level key', () => {
    expect(mergeMeta({ a: 1, b: 2 }, { b: 3, c: 4 })).toEqual({ a: 1, b: 3, c: 4 });
  });
});
```

- [ ] **Step 2: Run the test — expect FAIL** (import of `mergeMeta`/`contexts` does not resolve yet):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/scope.test.ts
```
  Expected: fails — `SyntaxError: The requested module '../src/scope' does not provide an export named 'mergeMeta'` (or the contexts/extra assertions fail).

- [ ] **Step 3: Add the `mergeMeta` helper** at the top of `sdks/js/src/scope.ts`, right after the import line:
```ts
/**
 * Shallow-merge a per-call override map over a base map. The override wins per
 * top-level key — tags/extra merge by key, contexts merge by block name (a
 * per-call block replaces the same-named base block). Returns a fresh object;
 * callers OMIT the field entirely when the result is empty (emit convention).
 */
export function mergeMeta(
  base: Record<string, unknown>,
  override?: Record<string, unknown>,
): Record<string, unknown> {
  return override ? { ...base, ...override } : { ...base };
}
```

- [ ] **Step 4: Add the storage fields** — in `sdks/js/src/scope.ts` replace the single `tags` field (line 17):
```ts
  readonly tags: Record<string, string> = {};
```
  with the three scope maps:
```ts
  readonly tags: Record<string, string> = {};
  readonly contexts: Record<string, unknown> = {};
  readonly extra: Record<string, unknown> = {};
```

- [ ] **Step 5: Add the setters** — in `sdks/js/src/scope.ts` replace the existing `setTag` method:
```ts
  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }
```
  with the full setter set:
```ts
  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }

  /** Merge a batch of tags into the scope (last-write-wins per key). */
  setTags(tags: Record<string, string>): void {
    Object.assign(this.tags, tags);
  }

  /** Set (replace) a named context block on the scope. */
  setContext(name: string, block: Record<string, unknown>): void {
    this.contexts[name] = block;
  }

  /** Set a single freeform extra value on the scope. */
  setExtra(key: string, value: unknown): void {
    this.extra[key] = value;
  }
```

- [ ] **Step 6: Run the test — expect PASS**:
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/scope.test.ts
```
  Expected: `Test Files  1 passed (1)` / `Tests  6 passed (6)`.

- [ ] **Step 7: Commit**:
```
git add sdks/js/src/scope.ts sdks/js/test/scope.test.ts
git commit -m "feat(sdk-js): add contexts/extra scopes + setTags/setContext/setExtra to Scope"
```

---

### Task 3.2: Wire types — ErrorItem/EventItem meta, InitOptions defaults, Hint fields, CaptureOptions/TrackOptions

**Files:**
- Modify: `sdks/js/src/types.ts` (ErrorItem ~70, EventItem ~88, Hint ~196, InitOptions ~248, new CaptureOptions/TrackOptions)

**Interfaces:**
- Consumes: existing `ErrorItem.tags?: Record<string, unknown>` (types.ts:70), `interface EventItem` (types.ts:81), `type Hint` (types.ts:196), `interface InitOptions` (types.ts:223)
- Produces: `ErrorItem.contexts?`, `ErrorItem.extra?`; `EventItem.tags?/contexts?/extra?`; `Hint.tags?/contexts?/extra?`; `InitOptions.tags?/contexts?/extra?`; `interface CaptureOptions { tags?; contexts?; extra? }`; `interface TrackOptions extends CaptureOptions { screen?: string }` (all optional → additive, no compile break; ResolvedOptions is deferred to Task 3.3)

- [ ] **Step 1: Extend `ErrorItem`** — in `sdks/js/src/types.ts`, immediately after the `tags?: Record<string, unknown>;` field (line 70) insert:
```ts
  /**
   * Dev-owned structured context blocks (e.g. `{ order: { id: 7 } }`). DISTINCT
   * from the machine-owned `context` on the envelope — never overwrites it.
   * Optional — omitted when empty (the backend defaults to `{}`).
   */
  contexts?: Record<string, unknown>;
  /** Freeform JSON bag. Optional — omitted when empty (backend defaults `{}`). */
  extra?: Record<string, unknown>;
```

- [ ] **Step 2: Extend `EventItem`** — after `properties: Record<string, unknown>;` (line 88) insert:
```ts
  /** Scope+call tags lifted onto the event. Optional — omitted when empty. */
  tags?: Record<string, unknown>;
  /** Scope+call named context blocks. Optional — omitted when empty. */
  contexts?: Record<string, unknown>;
  /** Scope+call freeform JSON. Optional — omitted when empty. */
  extra?: Record<string, unknown>;
```

- [ ] **Step 3: Extend `Hint`** with typed per-call fields — replace the `Hint` type (lines 196-199):
```ts
export type Hint = Record<string, unknown> & {
  originalException?: unknown;
  event?: unknown;
  /** Per-call metadata overrides, merged over the current scope before send. */
  tags?: Record<string, string>;
  contexts?: Record<string, Record<string, unknown>>;
  extra?: Record<string, unknown>;
};
```

- [ ] **Step 4: Add `CaptureOptions` / `TrackOptions`** — insert directly after the `Hint` type:
```ts
/** Per-call metadata overrides accepted by captureException/captureMessage/track. */
export interface CaptureOptions {
  tags?: Record<string, string>;
  contexts?: Record<string, Record<string, unknown>>;
  extra?: Record<string, unknown>;
}

/** Options accepted by `track` — {@link CaptureOptions} plus a screen override. */
export interface TrackOptions extends CaptureOptions {
  screen?: string;
}
```

- [ ] **Step 5: Extend `InitOptions`** — in the `InitOptions` interface, immediately before `debug?: boolean;` (line 248) insert:
```ts
  /** Default tags seeded into the global scope (string→string). */
  tags?: Record<string, string>;
  /** Default named context blocks seeded into the global scope. */
  contexts?: Record<string, Record<string, unknown>>;
  /** Default freeform extra seeded into the global scope. */
  extra?: Record<string, unknown>;
```

- [ ] **Step 6: Typecheck — expect PASS** (all additions optional; `ResolvedOptions` intentionally untouched here):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npm run typecheck
```
  Expected: exits 0, no output.

- [ ] **Step 7: Commit**:
```
git add sdks/js/src/types.ts
git commit -m "feat(sdk-js): add tags/contexts/extra to ErrorItem/EventItem/Hint/InitOptions + CaptureOptions"
```

---

### Task 3.3: client.ts — resolveOptions carries defaults, constructor seeds scope, enrichErrorItem merges scope meta

**Files:**
- Modify: `sdks/js/src/client.ts` (import ~14, ResolvedOptions carrier in `resolveOptions` ~262, constructor ~53, `enrichErrorItem` ~163-178)
- Modify: `sdks/js/src/types.ts` (`ResolvedOptions` ~252)
- Test: `sdks/js/test/envelope.test.ts` (extend the `client populates them from scope/hint` describe ~176)

**Interfaces:**
- Consumes: `mergeMeta`, `Scope.setTags/setContext/setExtra`, `Scope.contexts/extra` (Task 3.1); `Scope.tags`; `InitOptions.tags/contexts/extra` (Task 3.2)
- Produces: `ResolvedOptions.tags/contexts/extra` (required, defaulted `{}`); a captured error carries `contexts`/`extra` merged from the init-seeded scope, omitted when empty

- [ ] **Step 1: Add the failing test** — in `sdks/js/test/envelope.test.ts`, inside `describe('client populates them from scope/hint', ...)` (after the existing `omits tags/user` test, before its closing `})` at line 214) add:
```ts
    it('seeds init-default contexts/extra into the scope and lifts them onto errors', () => {
      init({
        dsn: 'https://pk_test@localhost:9/1',
        tags: { app: 'web' },
        contexts: { release_ctx: { channel: 'beta' } },
        extra: { build: 'ci-42' },
        beforeSend: (i) => {
          if (i.type === 'error') items.push(i);
          return null;
        },
      });
      getClient()!.getScope().setContext('order', { id: 7 });

      captureException(new Error('boom'));

      expect(items).toHaveLength(1);
      const err = items[0];
      expect(err.tags).toEqual({ app: 'web' });
      expect(err.contexts).toEqual({ release_ctx: { channel: 'beta' }, order: { id: 7 } });
      expect(err.extra).toEqual({ build: 'ci-42' });
    });
```

- [ ] **Step 2: Run — expect FAIL** (scope not seeded, error carries no contexts/extra):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: the new test fails — `expected undefined to deeply equal { release_ctx: … }`.

- [ ] **Step 3: Add `ResolvedOptions` fields** — in `sdks/js/src/types.ts`, in the `ResolvedOptions` interface, immediately before `debug: boolean;` (line 264) insert:
```ts
  tags: Record<string, string>;
  contexts: Record<string, Record<string, unknown>>;
  extra: Record<string, unknown>;
```

- [ ] **Step 4: Import `mergeMeta`** — in `sdks/js/src/client.ts` replace the scope import (line 14):
```ts
import { Scope } from './scope.js';
```
  with:
```ts
import { Scope, mergeMeta } from './scope.js';
```

- [ ] **Step 5: Carry the defaults in `resolveOptions`** — in the returned object of `resolveOptions`, immediately after `maxBreadcrumbs: options.maxBreadcrumbs ?? 50,` (line 267) insert:
```ts
    tags: options.tags ?? {},
    contexts: options.contexts ?? {},
    extra: options.extra ?? {},
```

- [ ] **Step 6: Seed the scope in the constructor** — in `sdks/js/src/client.ts`, immediately after `this.scope = new Scope(options.maxBreadcrumbs);` (line 53) insert:
```ts
    // Seed init-default metadata into the global scope so every later signal
    // inherits it; runtime setters still last-write-win over these.
    this.scope.setTags(options.tags);
    for (const [name, block] of Object.entries(options.contexts)) {
      this.scope.setContext(name, block);
    }
    for (const [key, value] of Object.entries(options.extra)) {
      this.scope.setExtra(key, value);
    }
```

- [ ] **Step 7: Merge scope meta in `enrichErrorItem`** — replace the existing tags block (lines 171-174):
```ts
    if (item.tags === undefined) {
      const tags = this.scope.tags;
      if (Object.keys(tags).length > 0) item.tags = { ...tags };
    }
```
  with a merge over any per-call values (per-call, placed by Task 3.4, wins over scope; empty result omitted):
```ts
    const tags = mergeMeta(this.scope.tags, item.tags);
    if (Object.keys(tags).length > 0) item.tags = tags;
    const contexts = mergeMeta(this.scope.contexts, item.contexts);
    if (Object.keys(contexts).length > 0) item.contexts = contexts;
    const extra = mergeMeta(this.scope.extra, item.extra);
    if (Object.keys(extra).length > 0) item.extra = extra;
```

- [ ] **Step 8: Run — expect PASS** (new test green; the existing `omits tags/user` test still passes because empty scope yields no `tags`/`contexts`/`extra` keys → `'tags' in err === false`):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: `Test Files  1 passed (1)` / all tests passed.

- [ ] **Step 9: Commit**:
```
git add sdks/js/src/client.ts sdks/js/src/types.ts sdks/js/test/envelope.test.ts
git commit -m "feat(sdk-js): seed init defaults into scope and merge tags/contexts/extra onto errors"
```

---

### Task 3.4: capture.ts — attach per-call tags/contexts/extra from the hint onto error items

**Files:**
- Modify: `sdks/js/src/api/capture.ts` (new `attachCallMeta` helper; `buildErrorItem` ~52-75; `captureMessage` ~88-108)
- Test: `sdks/js/test/envelope.test.ts` (extend the pure serialize/omit tests ~120-174)

**Interfaces:**
- Consumes: `Hint.tags/contexts/extra`, `ErrorItem.contexts/extra` (Task 3.2); `enrichErrorItem` scope-merge (Task 3.3)
- Produces: error items whose per-call `tags`/`contexts`/`extra` (from the hint) are placed on the item before it reaches `captureItem` → `enrichErrorItem`, where the scope is merged UNDER them (per-call wins per key)

- [ ] **Step 1: Extend the pure golden tests** — in `sdks/js/test/envelope.test.ts`, in the `serializes the optional event_id/message/tags/user keys on an error item` test, add `contexts`/`extra` to the `error` literal (after the `tags:` line, line 136) :
```ts
      contexts: { order: { id: 7 } },
      extra: { build: 'ci-42' },
```
  and add these assertions after the `tags` expectation (line 147):
```ts
    expect(item.contexts).toEqual({ order: { id: 7 } });
    expect(item.extra).toEqual({ build: 'ci-42' });
```
  Then in the `omits the optional keys entirely when absent` test, after `expect(keys).not.toContain('tags');` (line 172) add:
```ts
    expect(keys).not.toContain('contexts');
    expect(keys).not.toContain('extra');
```
  Finally add a new test at the end of the `client populates them from scope/hint` describe (before its closing `})`):
```ts
    it('per-call contexts/extra override the same-named scope block (per-call wins)', () => {
      const scope = getClient()!.getScope();
      scope.setContext('order', { id: 1, source: 'scope' });
      scope.setExtra('build', 'scope');

      captureException(new Error('boom'), {
        contexts: { order: { id: 2 } },
        extra: { build: 'call', attempt: 3 },
      });

      expect(items).toHaveLength(1);
      const err = items[0];
      expect(err.contexts).toEqual({ order: { id: 2 } });
      expect(err.extra).toEqual({ build: 'call', attempt: 3 });
    });
```

- [ ] **Step 2: Run — expect FAIL** (per-call meta not read from the hint yet):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: the `per-call contexts/extra override…` test fails — `expected undefined to deeply equal { order: { id: 2 } }`.

- [ ] **Step 3: Add the `attachCallMeta` helper** — in `sdks/js/src/api/capture.ts`, after `const DEFAULT_MECHANISM` (line 8) insert:
```ts
/** Copy any NON-EMPTY per-call tags/contexts/extra off the hint onto the item. */
function attachCallMeta(item: ErrorItem, hint?: Hint): void {
  if (hint?.tags && Object.keys(hint.tags).length > 0) item.tags = { ...hint.tags };
  if (hint?.contexts && Object.keys(hint.contexts).length > 0) item.contexts = { ...hint.contexts };
  if (hint?.extra && Object.keys(hint.extra).length > 0) item.extra = { ...hint.extra };
}
```

- [ ] **Step 4: Wire it into `buildErrorItem`** — replace the `return { … }` at the end of `buildErrorItem` (lines 65-74) with a named item + attach:
```ts
  const item: ErrorItem = {
    type: 'error',
    timestamp: nowIso(),
    level,
    exception,
    breadcrumbs,
    fingerprint,
    session_id: getSessionId(),
    screen: (hint?.screen as string | undefined) ?? getScreen(),
  };
  attachCallMeta(item, hint);
  return item;
```

- [ ] **Step 5: Wire it into `captureMessage`** — in `captureMessage`, immediately after the `const item: ErrorItem = { … };` block (before `client.captureItem(item, hint);` at line 107) insert:
```ts
  attachCallMeta(item, hint);
```

- [ ] **Step 6: Run — expect PASS**:
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: all tests pass.

- [ ] **Step 7: Commit**:
```
git add sdks/js/src/api/capture.ts sdks/js/test/envelope.test.ts
git commit -m "feat(sdk-js): attach per-call tags/contexts/extra from hint onto captured errors"
```

---

### Task 3.5: product.ts — track() accepts per-call options and attaches scope+call meta to EventItem

**Files:**
- Modify: `sdks/js/src/api/product.ts` (imports ~1-5; `track` ~11-28)
- Test: `sdks/js/test/envelope.test.ts` (new describe)

**Interfaces:**
- Consumes: `mergeMeta`, `Scope.tags/contexts/extra` (Task 3.1); `TrackOptions`, `EventItem.tags/contexts/extra` (Task 3.2)
- Produces: `track(name, properties?, options?: TrackOptions)` — attaches `tags`/`contexts`/`extra` = scope ⊕ per-call (per-call wins per key), omitted when empty; `options.screen` overrides the current screen (repurposes the old, unused positional `screen` arg — `setScreen`'s `track('$screen', { … })` and all other 2-arg callers are unaffected)

- [ ] **Step 1: Add the failing test** — in `sdks/js/test/envelope.test.ts`, add the `track` import to the client import group (line 6) and `EventItem`/`track` to types/product imports, then append a new describe at the end of the file:
```ts
describe('event item metadata (track tags/contexts/extra)', () => {
  let events: EventItem[];
  const capture = () => (i: EnvelopeItem) => {
    if (i.type === 'event') events.push(i as EventItem);
    return null;
  };
  beforeEach(() => {
    events = [];
  });

  it('attaches scope + per-call meta, per-call wins per top-level key', () => {
    init({
      dsn: 'https://pk_test@localhost:9/1',
      tags: { app: 'web' },
      contexts: { app_ctx: { version: '1.0' } },
      beforeSend: capture(),
    });
    getClient()!.getScope().setTag('req', '42');

    track('checkout', { total: 9 }, {
      tags: { req: '99' },
      contexts: { order: { id: 7 } },
      extra: { attempt: 2 },
    });

    expect(events).toHaveLength(1);
    const e = events[0];
    expect(e.tags).toEqual({ app: 'web', req: '99' });
    expect(e.contexts).toEqual({ app_ctx: { version: '1.0' }, order: { id: 7 } });
    expect(e.extra).toEqual({ attempt: 2 });
  });

  it('omits tags/contexts/extra when scope and call carry none', () => {
    init({ dsn: 'https://pk_test@localhost:9/1', beforeSend: capture() });
    track('ping', {});
    const e = events[0];
    expect('tags' in e).toBe(false);
    expect('contexts' in e).toBe(false);
    expect('extra' in e).toBe(false);
  });
});
```
  Add the required imports at the top of the file: extend `import { getClient, init } from '../src/client';` is present; add `import { track } from '../src/api/product';` and extend the types import to `import type { Envelope, EnvelopeItem, ErrorItem, EventItem, TransactionItem } from '../src/types';`.

- [ ] **Step 2: Run — expect FAIL** (`track` doesn’t accept options / doesn’t attach meta):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: the `attaches scope + per-call meta…` test fails — `expected undefined to deeply equal { app: 'web', req: '99' }`.

- [ ] **Step 3: Update imports** — in `sdks/js/src/api/product.ts`, after the existing imports (line 5) add the scope helper, and extend the types import:
```ts
import { mergeMeta } from '../scope.js';
```
  and change:
```ts
import type { EventItem, IdentifyItem, TransactionItem, TransactionOp } from '../types.js';
```
  to:
```ts
import type { EventItem, IdentifyItem, TrackOptions, TransactionItem, TransactionOp } from '../types.js';
```

- [ ] **Step 4: Rewrite `track`** — replace the whole `track` function (lines 11-28):
```ts
export function track(
  name: string,
  properties: Record<string, unknown> = {},
  options: TrackOptions = {},
): void {
  const client = getClient();
  if (!client) return;
  const scope = client.getScope();
  const item: EventItem = {
    type: 'event',
    name,
    distinct_id: client.getDistinctId(),
    session_id: getSessionId(),
    screen: options.screen ?? getScreen(),
    timestamp: nowIso(),
    properties: properties ?? {},
  };
  const tags = mergeMeta(scope.tags, options.tags);
  if (Object.keys(tags).length > 0) item.tags = tags;
  const contexts = mergeMeta(scope.contexts, options.contexts);
  if (Object.keys(contexts).length > 0) item.contexts = contexts;
  const extra = mergeMeta(scope.extra, options.extra);
  if (Object.keys(extra).length > 0) item.extra = extra;
  client.captureItem(item);
}
```

- [ ] **Step 5: Run — expect PASS**:
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run test/envelope.test.ts
```
  Expected: all tests pass.

- [ ] **Step 6: Commit**:
```
git add sdks/js/src/api/product.ts sdks/js/test/envelope.test.ts
git commit -m "feat(sdk-js): track() accepts per-call options and attaches scope+call meta to events"
```

---

### Task 3.6: index.ts facade — export setTag/setTags/setContext/setExtra + track(options) + types; full suite green

**Files:**
- Modify: `sdks/js/src/index.ts` (imports ~25; `track` wrapper ~43-45; new setters ~73-75; `Sauron` object ~93-107; type re-exports ~121-151)

**Interfaces:**
- Consumes: `Scope.setTag/setTags/setContext/setExtra` (Task 3.1); `TrackOptions`, `CaptureOptions` (Task 3.2); `track(name, properties?, options?)` (Task 3.5)
- Produces: public facade `Sauron.setTag/setTags/setContext/setExtra`, `Sauron.track(name, properties?, options?)`, and named exports of the same; re-exported types `CaptureOptions`, `TrackOptions`

- [ ] **Step 1: Update imports** — in `sdks/js/src/index.ts`, change the types import (line 25):
```ts
import type { Hint, InitOptions, Level, UserInput } from './types.js';
```
  to:
```ts
import type { CaptureOptions, Hint, InitOptions, Level, TrackOptions, UserInput } from './types.js';
```

- [ ] **Step 2: Thread options through the `track` wrapper** — replace the `track` wrapper (lines 42-45):
```ts
/** Record a product-analytics event. */
export function track(name: string, properties?: Record<string, unknown>): void {
  trackApi(name, properties);
}
```
  with:
```ts
/** Record a product-analytics event, optionally with per-call tags/contexts/extra. */
export function track(
  name: string,
  properties?: Record<string, unknown>,
  options?: TrackOptions,
): void {
  trackApi(name, properties, options);
}
```

- [ ] **Step 3: Add the scope-setter facade fns** — in `sdks/js/src/index.ts`, immediately after `setUser` (line 75) insert:
```ts
/** Set a single scope tag (lifted onto later errors/events). */
export function setTag(key: string, value: string): void {
  getClient()?.getScope().setTag(key, value);
}

/** Merge a batch of scope tags (last-write-wins per key). */
export function setTags(tags: Record<string, string>): void {
  getClient()?.getScope().setTags(tags);
}

/** Set (replace) a named scope context block. */
export function setContext(name: string, block: Record<string, unknown>): void {
  getClient()?.getScope().setContext(name, block);
}

/** Set a single freeform scope extra value. */
export function setExtra(key: string, value: unknown): void {
  getClient()?.getScope().setExtra(key, value);
}
```

- [ ] **Step 4: Add them to the `Sauron` facade object** — in the `Sauron` const (lines 93-107), after `setUser,` insert:
```ts
  setTag,
  setTags,
  setContext,
  setExtra,
```

- [ ] **Step 5: Re-export the new types** — in the final `export type { … } from './types.js';` block (lines 121-151), add after `InitOptions,`:
```ts
  CaptureOptions,
  TrackOptions,
```

- [ ] **Step 6: Extend the golden envelope for coverage** — in `sdks/js/test/envelope.test.ts` `GOLDEN`, add `contexts`/`extra` to the first (error) item after `fingerprint: null,` (line 59) and `tags`/`contexts`/`extra` to the event item after `properties: { cart_value: 42.5 },` (line 68):
```ts
      // (error item) ...
      fingerprint: null,
      contexts: { order: { id: 7 } },
      extra: { build: 'ci-42' },
      session_id: 'sess_abc123',
```
```ts
      // (event item) ...
      properties: { cart_value: 42.5 },
      tags: { plan: 'pro' },
      contexts: { experiment: { bucket: 'b' } },
      extra: { referrer: 'email' },
```
  (These extend the reflexive `buildEnvelope`/round-trip coverage; `buildEnvelope` passes items through, so the exact-shape assertions still hold.)

- [ ] **Step 7: Run the FULL suite + typecheck — expect PASS**:
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npx vitest run && npm run typecheck
```
  Expected: `Test Files  4 passed (4)`, all tests pass; typecheck exits 0.

- [ ] **Step 8: Commit**:
```
git add sdks/js/src/index.ts sdks/js/test/envelope.test.ts
git commit -m "feat(sdk-js): expose setTag/setTags/setContext/setExtra + track options on the Sauron facade"
```

---

### Task 3.7: Example — extend SeedingSink + seedingSink() + driver to drive contexts/extra (scope + per-call on errors)

**Files:**
- Modify: `examples/svelte-web/src/lib/seeding.ts` (`SeedingSink` ~72-83; `SeedOp` error variant ~269; `pushErrorOps` ~335-361; driver ~421-459)
- Modify: `examples/svelte-web/src/lib/sauron.ts` (`seedingSink()` ~50-86)
- Test: `examples/svelte-web/src/lib/seeding.test.ts` (extend `fakeSink` ~105-148 + assertions)

**Interfaces:**
- Consumes: `Scope.setContext/setExtra` (Task 3.1); SDK `Hint.contexts/extra` (Task 3.2); facade behavior (Tasks 3.3–3.6)
- Produces: `SeedingSink.setContext(name, block)`, `SeedingSink.setExtra(key, value)`, `captureException` hint gains `contexts?`/`extra?`; the driver sets a per-visitor scope context/extra and passes per-call contexts/extra on big-payload errors

- [ ] **Step 1: Add failing test coverage** — in `examples/svelte-web/src/lib/seeding.test.ts`, extend `RecordedErr` (line 96) with the per-call block and the fakeSink to record scope contexts/extra. Change the `RecordedErr` interface to add:
```ts
  callContexts?: Record<string, Record<string, unknown>>;
```
  In `fakeSink()`’s `state`, add:
```ts
    contextSets: [] as { name: string; block: Record<string, unknown> }[],
    extraSets: [] as { key: string; value: unknown }[],
```
  Change `record` to accept + store the hint’s per-call contexts, and update `captureException`:
```ts
  const record = (
    name: string,
    message: string,
    level: string,
    fingerprint: string[] | null,
    callContexts?: Record<string, Record<string, unknown>>,
  ) => {
    state.errors.push({
      name,
      message,
      level,
      fingerprint,
      taggedAtCapture: !!state.currentTags && Object.keys(state.currentTags).length > 0,
      bigTag: !!state.currentTags && 'state_snapshot' in state.currentTags,
      callContexts,
    });
  };
```
  and in the `sink` object add the two new methods + thread the hint contexts:
```ts
    setContext: (name, block) => state.contextSets.push({ name, block }),
    setExtra: (key, value) => state.extraSets.push({ key, value }),
    captureException: (err, hint) => record(err.name, err.message, hint.level, hint.fingerprint, hint.contexts),
```
  Then add a new test:
```ts
test('runSeeding drives scope contexts/extra and per-call contexts on big errors', async () => {
  const { sink, state } = fakeSink();
  await runSeeding(sink, { visitors: 40, runId: 'meta' });
  assert.ok(state.contextSets.length > 0, 'expected per-visitor scope contexts');
  assert.ok(state.extraSets.length > 0, 'expected per-visitor scope extra');
  assert.ok(
    state.errors.some((e) => e.callContexts && Object.keys(e.callContexts).length > 0),
    'expected at least one error captured with per-call contexts',
  );
});
```

- [ ] **Step 2: Run — expect FAIL** (sink has no `setContext`/`setExtra`; driver drives none):
```
cd /home/splimter/projects/freelance/sauron/examples/svelte-web && node --test src/lib/seeding.test.ts
```
  Expected: fails — `expected per-visitor scope contexts` assertion, and/or a `sink.setContext is not a function` TypeError.

- [ ] **Step 3: Extend the `SeedingSink` interface** — in `examples/svelte-web/src/lib/seeding.ts`, replace the `setTags` line + `captureException` line inside `SeedingSink` (lines 76 & 79):
```ts
  setTags(tags: Record<string, string> | null): void;
```
  →
```ts
  setTags(tags: Record<string, string> | null): void;
  /** Set (replace) a named scope context block. */
  setContext(name: string, block: Record<string, unknown>): void;
  /** Set a freeform scope extra value. */
  setExtra(key: string, value: unknown): void;
```
  and:
```ts
  captureException(error: Error, hint: { level: Level; fingerprint: string[] | null }): void;
```
  →
```ts
  captureException(
    error: Error,
    hint: {
      level: Level;
      fingerprint: string[] | null;
      contexts?: Record<string, Record<string, unknown>>;
      extra?: Record<string, unknown>;
    },
  ): void;
```

- [ ] **Step 4: Carry per-call meta on the error `SeedOp`** — replace the `error` variant of `SeedOp` (line 269):
```ts
  | { t: 'error'; kind: 'exception' | 'message'; name: string; message: string; level: Level; fingerprint: string[] | null; big: boolean; tagged: boolean }
```
  with:
```ts
  | { t: 'error'; kind: 'exception' | 'message'; name: string; message: string; level: Level; fingerprint: string[] | null; big: boolean; tagged: boolean; contexts?: Record<string, Record<string, unknown>>; extra?: Record<string, unknown> }
```

- [ ] **Step 5: Populate per-call meta for big errors** — in `pushErrorOps`, replace the final `ops.push({ t: 'error', … })` (line 360):
```ts
  ops.push({ t: 'error', kind: arch.kind, name: arch.name, message, level: arch.level, fingerprint, big, tagged });
```
  with (big errors additionally carry a per-call `failure` context block + an `attempt` extra):
```ts
  const contexts = big ? { failure: { screen, code: pick(rng, ['E_TIMEOUT', 'E_5XX', 'E_OOM']) } } : undefined;
  const extra = big ? { attempt: int(rng, 1, 4), degraded: chance(rng, 0.5) } : undefined;
  ops.push({ t: 'error', kind: arch.kind, name: arch.name, message, level: arch.level, fingerprint, big, tagged, contexts, extra });
```

- [ ] **Step 6: Drive scope + per-call meta in `runSeeding`** — in the per-visitor loop, right after `sink.setUser({ id: plan.id, traits: plan.traits });` (line 423) add a per-visitor scope context/extra:
```ts
      sink.setContext('session', { visitor: plan.id, plan: plan.traits.plan, cohort: runId });
      sink.setExtra('cohort', runId);
```
  and in the `case 'error':` block, thread the per-call contexts/extra into the `captureException` hint — replace the `captureException` call (line 448):
```ts
              sink.captureException(err, { level: op.level, fingerprint: op.fingerprint });
```
  with:
```ts
              sink.captureException(err, {
                level: op.level,
                fingerprint: op.fingerprint,
                contexts: op.contexts,
                extra: op.extra,
              });
```

- [ ] **Step 7: Implement the new methods in `seedingSink()`** — in `examples/svelte-web/src/lib/sauron.ts`, inside the returned object of `seedingSink()`, after the `setTags` method (line 66) insert:
```ts
    setContext(name, block) {
      scope()?.setContext(name, block);
    },
    setExtra(key, value) {
      scope()?.setExtra(key, value);
    },
```
  (`captureException` already forwards the full hint via `Sauron.captureException(error, hint)`, so the new `contexts`/`extra` fields flow straight through the SDK’s `Hint`.)

- [ ] **Step 8: Run — expect PASS** (all existing seeding tests plus the new one):
```
cd /home/splimter/projects/freelance/sauron/examples/svelte-web && node --test src/lib/seeding.test.ts
```
  Expected: `# pass` count increases by 1, `# fail 0`.

- [ ] **Step 9: Commit**:
```
git add examples/svelte-web/src/lib/seeding.ts examples/svelte-web/src/lib/sauron.ts examples/svelte-web/src/lib/seeding.test.ts
git commit -m "feat(example): drive scope + per-call contexts/extra through the seeding sink"
```

---

### Task 3.8: Example — init defaults in connect(), captureExampleError() helper, visible UI affordance

**Files:**
- Modify: `examples/svelte-web/src/lib/sauron.ts` (`connect()` `Sauron.init` ~107-114; new `captureExampleError` export)
- Modify: `examples/svelte-web/src/lib/components/Seeding.svelte` (import ~4; button in `.controls` ~83-86; one style rule ~233)

**Interfaces:**
- Consumes: `InitOptions.tags/contexts/extra` (Task 3.2); facade `Sauron.setContext/setExtra/captureException` (Tasks 3.3–3.6); `activity.push(kind, title, detail)` from `store.svelte`
- Produces: E2E vertical — init-seeded default scopes on every signal, plus a one-click error that proves scope ⊕ per-call merge (the per-call `order` block replaces the scope `order` block)

- [ ] **Step 1: Rebuild the SDK so the example resolves the new `dist` types** (the example depends on `@sauron/browser` via `file:../../sdks/js` → `dist/index.d.ts`):
```
cd /home/splimter/projects/freelance/sauron/sdks/js && npm run build
```
  Expected: `tsup` completes; `dist/index.d.ts` now exports `setContext`/`setExtra`/`CaptureOptions`/`TrackOptions`.

- [ ] **Step 2: Seed init-default scopes in `connect()`** — in `examples/svelte-web/src/lib/sauron.ts`, replace the `Sauron.init({ … })` call (lines 107-114):
```ts
    Sauron.init({
      dsn,
      environment: config.environment.trim() || 'demo',
      release: config.release.trim() || undefined,
      // Flush a little more eagerly than the 5s default so freshly-clicked
      // actions show up in the dashboard within a couple of seconds.
      transport: { flushIntervalMs: 3000 },
    });
```
  with (adds default tags/contexts/extra that ride on every subsequent error/message/track):
```ts
    Sauron.init({
      dsn,
      environment: config.environment.trim() || 'demo',
      release: config.release.trim() || undefined,
      // Flush a little more eagerly than the 5s default so freshly-clicked
      // actions show up in the dashboard within a couple of seconds.
      transport: { flushIntervalMs: 3000 },
      // Default metadata scopes — seeded into the global scope, lifted onto
      // every error / message / track() from here on.
      tags: { app: 'svelte-web', surface: 'demo' },
      contexts: { app: { name: 'sauron-web-demo', framework: 'svelte' } },
      extra: { initialized_at: new Date().toISOString() },
    });
```

- [ ] **Step 3: Add the `captureExampleError` helper** — in `examples/svelte-web/src/lib/sauron.ts`, after `isConnected()` (line 15) insert:
```ts
/**
 * Capture one hand-crafted error that exercises the metadata scopes E2E: a
 * scope-level context/extra (via setContext/setExtra) PLUS per-call overrides on
 * the capture itself — proving the SDK merges scope ⊕ call before send. The
 * per-call `order` block replaces the same-named scope block; `feature` tag and
 * `attempt` extra are per-call-only.
 */
export function captureExampleError(): void {
  if (!isConnected()) return;
  Sauron.setContext('order', { id: 4242, total: 99.5, currency: 'USD' });
  Sauron.setExtra('cart_size', 3);
  Sauron.captureException(new Error('Checkout failed at payment step'), {
    level: 'error',
    tags: { feature: 'checkout' },
    contexts: { order: { id: 4242, step: 'payment' } },
    extra: { attempt: 2, gateway: 'stripe' },
  });
  activity.push('error', 'captureException()', 'order+extra scopes attached (scope ⊕ per-call override)');
}
```

- [ ] **Step 4: Wire the UI affordance** — in `examples/svelte-web/src/lib/components/Seeding.svelte`, change the import (line 4):
```svelte
  import { seedingSink } from '../sauron';
```
  to:
```svelte
  import { seedingSink, captureExampleError } from '../sauron';
```
  then add a button inside `.controls`, right after the closing `</button>` of the `.run` button (line 85):
```svelte
    <button class="run ghost" type="button" onclick={captureExampleError} disabled={running || disabled}>
      Capture example error
    </button>
```
  and add a style rule after the `.run:disabled { … }` block (line 249):
```css
  .run.ghost {
    color: var(--text);
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  .run.ghost:hover:not(:disabled) {
    filter: none;
    background: var(--surface-3);
  }
```

- [ ] **Step 5: Typecheck the example — expect PASS** (validates the new SDK types + Svelte usage end-to-end):
```
cd /home/splimter/projects/freelance/sauron/examples/svelte-web && npm run check
```
  Expected: `svelte-check` reports `0 errors`.

- [ ] **Step 6: Commit**:
```
git add examples/svelte-web/src/lib/sauron.ts examples/svelte-web/src/lib/components/Seeding.svelte
git commit -m "feat(example): seed default scopes on init + one-click contexts/extra error affordance"
```



## Slice 4 — Node SDK

### Task 4.1: Merge dev `contexts`/`extra` onto error items (omit-when-empty) + `Scope.mergeMetadata`

**Files:**
- Modify: `sdks/node/src/scope.ts` (`applyToErrorItem` ~110-118; add `mergeMetadata`)
- Test: `sdks/node/test/scope.test.ts` (append inside the `describe('scope', …)` block)

**Interfaces:**
- Consumes: `Scope.data: ScopeData` (already carries `contexts`/`extra`, initialized `{}` at scope.ts ~44-46); wire keys `contexts`/`extra` are `serde(default)` JSON on the backend.
- Produces: `Scope.applyToErrorItem(item)` now merges `contexts`/`extra` (scope UNDER per-call, block-name replacement for contexts) and OMITS empties; `Scope.mergeMetadata(overrides?) => { tags?, contexts?, extra? }` for non-error items (used by `track` in 4.4).

- [ ] **Step 1: Add failing regression tests** — append to `sdks/node/test/scope.test.ts` before the trailing `const tick` (line 62). These use a fresh `new Scope()` so global-scope pollution from earlier tests can't leak in.
```ts
  it('merges scope contexts/extra under per-call on an error item, omitting empties', () => {
    const s = new Scope();
    s.setContext('order', { id: 7 });
    s.setExtra('trace_id', 'abc');
    const item: any = {
      type: 'error',
      tags: {},
      contexts: { order: { id: 99 }, cart: { size: 2 } },
    };
    s.applyToErrorItem(item);
    // per-call 'order' block REPLACES the scope's same-named block; per-call 'cart' is kept
    expect(item.contexts).toEqual({ order: { id: 99 }, cart: { size: 2 } });
    // scope-only extra flows through
    expect(item.extra).toEqual({ trace_id: 'abc' });
  });

  it('omits contexts/extra entirely when both scope and per-call are empty', () => {
    const s = new Scope();
    const item: any = { type: 'error', tags: {} };
    s.applyToErrorItem(item);
    expect('contexts' in item).toBe(false);
    expect('extra' in item).toBe(false);
    // tags stays present ({}) per the existing Node convention
    expect(item.tags).toEqual({});
  });

  it('mergeMetadata layers per-call over scope and omits empty maps', () => {
    const s = new Scope();
    s.setTag('env', 'prod');
    s.setContext('order', { id: 7 });
    const merged = s.mergeMetadata({ tags: { req: '42' }, contexts: { order: { id: 9 } } });
    expect(merged).toEqual({ tags: { env: 'prod', req: '42' }, contexts: { order: { id: 9 } } });
    expect('extra' in merged).toBe(false);
  });

  it('mergeMetadata returns an empty object when nothing is set', () => {
    expect(new Scope().mergeMetadata()).toEqual({});
  });
```

- [ ] **Step 2: Run — expect FAIL** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test -- scope`. Expected: the two `mergeMetadata` tests fail with `TypeError: s.mergeMetadata is not a function`; the contexts/extra error-item tests fail because `applyToErrorItem` currently drops them (`item.contexts`/`item.extra` are `undefined`).

- [ ] **Step 3: Extend `applyToErrorItem`** — replace the method body (scope.ts ~110-118) so contexts/extra merge scope-under-per-call and empties are deleted (never serialized as `{}`):
```ts
  applyToErrorItem(item: {
    tags?: Record<string, string>;
    contexts?: Record<string, unknown>;
    extra?: Record<string, unknown>;
    user?: ErrorUser | null;
    breadcrumbs?: Breadcrumb[];
  }): void {
    item.tags = { ...this.data.tags, ...(item.tags ?? {}) };
    const contexts = { ...this.data.contexts, ...(item.contexts ?? {}) };
    if (Object.keys(contexts).length > 0) item.contexts = contexts;
    else delete item.contexts;
    const extra = { ...this.data.extra, ...(item.extra ?? {}) };
    if (Object.keys(extra).length > 0) item.extra = extra;
    else delete item.extra;
    if (item.user == null) item.user = toErrorUser(this.data.user);
    item.breadcrumbs = this.data.breadcrumbs.slice();
  }
```

- [ ] **Step 4: Add `mergeMetadata`** — insert this method directly after `applyToErrorItem` (before the closing `}` of `class Scope`). It is the non-error-item counterpart (used by `track`): same merge rule, empties omitted.
```ts
  /**
   * Merge this scope's metadata *under* per-call overrides for non-error items
   * (analytics `track`). tags & extra merge by shallow key; contexts merge by
   * block name (a per-call block replaces the same-named scope block). Empty
   * maps are omitted from the result per the emit convention.
   */
  mergeMetadata(
    overrides: {
      tags?: Record<string, string>;
      contexts?: Record<string, unknown>;
      extra?: Record<string, unknown>;
    } = {},
  ): { tags?: Record<string, string>; contexts?: Record<string, unknown>; extra?: Record<string, unknown> } {
    const out: {
      tags?: Record<string, string>;
      contexts?: Record<string, unknown>;
      extra?: Record<string, unknown>;
    } = {};
    const tags = { ...this.data.tags, ...(overrides.tags ?? {}) };
    if (Object.keys(tags).length > 0) out.tags = tags;
    const contexts = { ...this.data.contexts, ...(overrides.contexts ?? {}) };
    if (Object.keys(contexts).length > 0) out.contexts = contexts;
    const extra = { ...this.data.extra, ...(overrides.extra ?? {}) };
    if (Object.keys(extra).length > 0) out.extra = extra;
    return out;
  }
```

- [ ] **Step 5: Run — expect PASS** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test`. Expected: full suite green (`Test Files 14 passed`, all tests passed). Existing `scope.test.ts` line-9 test still passes (tags handling unchanged); `transport.test.ts` strict event `toEqual` still passes (empty scope ⇒ no keys added).

- [ ] **Step 6: Commit**
```bash
git -C /home/splimter/projects/freelance/sauron add sdks/node/src/scope.ts sdks/node/test/scope.test.ts
git -C /home/splimter/projects/freelance/sauron commit -m "feat(sdk-node): merge dev contexts/extra onto error items and add Scope.mergeMetadata

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4.2: Wire types — `contexts`/`extra` on ErrorItem, `tags`/`contexts`/`extra` on EventItem, `MetadataOptions`, init/capture option fields

**Files:**
- Modify: `sdks/node/src/types.ts` (`ErrorItem` ~83-96, `EventItem` ~99-107, `InitOptions` ~258-297, `CaptureExceptionOptions` ~320-328)
- Test: `sdks/node/test/metadata.test.ts` (new file — type round-trip)

**Interfaces:**
- Consumes: existing `ErrorItem`/`EventItem`/`InitOptions`/`CaptureExceptionOptions` shapes.
- Produces: `ErrorItem.contexts?`, `ErrorItem.extra?`; `EventItem.tags?`, `EventItem.contexts?`, `EventItem.extra?`; `InitOptions.tags?/contexts?/extra?`; exported `MetadataOptions { tags?, contexts?, extra? }`; `CaptureExceptionOptions extends MetadataOptions` (gains `contexts`/`extra`, keeps `tags` via inheritance). All optional/additive — no `deny_unknown_fields`.

- [ ] **Step 1: Add the type round-trip test** — create `sdks/node/test/metadata.test.ts`. It references the new optional fields, so it fails to typecheck until the types land.
```ts
import { describe, it, expect } from 'vitest';
import type { ErrorItem, EventItem } from '../src/types.js';

describe('metadata wire types', () => {
  it('ErrorItem carries optional contexts/extra that round-trip through JSON', () => {
    const item: ErrorItem = {
      type: 'error',
      event_id: 'evt_1',
      level: 'error',
      timestamp: 'TS',
      exception: {
        type: 'E',
        value: null,
        mechanism: { type: 'generic', handled: true },
        stacktrace: [],
      },
      message: null,
      breadcrumbs: [],
      tags: { a: '1' },
      contexts: { order: { id: 7 } },
      extra: { trace_id: 'abc' },
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    expect(JSON.parse(JSON.stringify(item))).toEqual(item);
  });

  it('EventItem carries optional tags/contexts/extra that round-trip through JSON', () => {
    const item: EventItem = {
      type: 'event',
      name: 'checkout',
      distinct_id: 'u_1',
      properties: {},
      timestamp: 'TS',
      session_id: null,
      screen: null,
      tags: { plan: 'pro' },
      contexts: { order: { id: 7 } },
      extra: { trace_id: 'abc' },
    };
    expect(JSON.parse(JSON.stringify(item))).toEqual(item);
  });
});
```

- [ ] **Step 2: Run typecheck — expect FAIL** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm run typecheck`. Expected: `error TS2353: Object literal may only specify known properties, and 'contexts' does not exist in type 'ErrorItem'.` (and the analogous `tags`/`contexts` errors for `EventItem`). (`npm test` alone would NOT catch this — vitest transpiles without type-checking — so the typecheck gate is the failing test here.)

- [ ] **Step 3: Add fields to `ErrorItem`** — in types.ts, after `tags: Record<string, string>;` (line 91):
```ts
  tags: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
```

- [ ] **Step 4: Add fields to `EventItem`** — after `properties: Record<string, unknown>;` (line 103):
```ts
  properties: Record<string, unknown>;
  tags?: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
```

- [ ] **Step 5: Add `InitOptions` defaults** — after `release?: string | null;` (line 262):
```ts
  release?: string | null;
  /** Default tags seeded into the global scope at init. */
  tags?: Record<string, string>;
  /** Default named dev context blocks seeded into the global scope at init. Distinct from the machine `context`. */
  contexts?: Record<string, unknown>;
  /** Default freeform extra values seeded into the global scope at init. */
  extra?: Record<string, unknown>;
```

- [ ] **Step 6: Introduce `MetadataOptions` and re-base `CaptureExceptionOptions`** — replace the whole `CaptureExceptionOptions` block (types.ts 320-328):
```ts
/**
 * Per-capture metadata overrides shared by `captureMessage` and `track`, and the
 * metadata subset of {@link CaptureExceptionOptions}. Empty maps are omitted on
 * the wire per the emit convention.
 */
export interface MetadataOptions {
  tags?: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
}

/** Extra attribution for `captureException`. */
export interface CaptureExceptionOptions extends MetadataOptions {
  user?: Partial<ErrorUser> | null;
  level?: Level;
  handled?: boolean;
  /** Client-supplied fingerprint override (honored verbatim by the backend). */
  fingerprint?: string[] | null;
}
```

- [ ] **Step 7: Run — expect PASS** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm run typecheck && npm test`. Expected: typecheck clean; `metadata.test.ts` 2 tests pass; whole suite green. (No runtime emit changed yet, so all existing goldens/`toEqual`s still pass.)

- [ ] **Step 8: Commit**
```bash
git -C /home/splimter/projects/freelance/sauron add sdks/node/src/types.ts sdks/node/test/metadata.test.ts
git -C /home/splimter/projects/freelance/sauron commit -m "feat(sdk-node): add contexts/extra wire fields and MetadataOptions to types

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4.3: Seed the global scope from `init` `tags`/`contexts`/`extra`

**Files:**
- Modify: `sdks/node/src/types.ts` (`ResolvedOptions` ~300-318)
- Modify: `sdks/node/src/client.ts` (`resolveOptions` ~48-79; constructor ~116-140)
- Test: `sdks/node/test/metadata.test.ts` (append client-driven describe + `beforeEach` scope reset + local `makeFakeFetch`)

**Interfaces:**
- Consumes: `getGlobalScope()`, `Scope.setTags`/`setContext`/`setExtra` (scope.ts 65-78); `InitOptions.tags/contexts/extra` (Task 4.2).
- Produces: `ResolvedOptions.tags/contexts/extra` (required, defaulted `{}`); `SauronClient` constructor seeds the global scope so init-defaults flow onto every captured error and tracked event.

- [ ] **Step 1: Add the failing seeding test** — append to `sdks/node/test/metadata.test.ts`. Add the extra imports at the TOP of the file (alongside the existing type import) and a `beforeEach` that resets the module-global scope so tests in this file stay isolated:
```ts
import { describe, it, expect, beforeEach } from 'vitest';
import { SauronClient } from '../src/client.js';
import { getGlobalScope } from '../src/scope.js';
import type { Envelope, ErrorItem, EventItem, FetchLike } from '../src/types.js';
import { bodyToString } from './helpers.js';

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function makeFakeFetch() {
  const envelopes: Envelope[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    envelopes.push(JSON.parse(bodyToString(init)) as Envelope);
    return { status: 200, ok: true };
  };
  return { fetchImpl, envelopes };
}

beforeEach(() => {
  // Global scope is process-wide; reset it so seeding/per-call tests don't leak.
  const g = getGlobalScope().data;
  g.user = null;
  g.tags = {};
  g.contexts = {};
  g.extra = {};
  g.breadcrumbs = [];
});
```
Then append the describe (keep the existing top-of-file `import type { ErrorItem, EventItem } from '../src/types.js';` line — merge it into the import above or leave it; both resolve):
```ts
describe('init seeds the global scope with default metadata', () => {
  it('applies init tags/contexts/extra to captured errors and tracked events', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({
      dsn: DSN,
      flushInterval: 0,
      fetchImpl: fake.fetchImpl,
      tags: { env: 'prod' },
      contexts: { order: { id: 7 } }, // dev contexts — NOT the machine `context` (device/os/app/runtime/user)
      extra: { region: 'eu' },
    });
    client.captureMessage('hello');
    client.track('viewed', 'u_1');
    await client.flush();

    const items = fake.envelopes.flatMap((e) => e.items) as Record<string, any>[];
    const error = items.find((i) => i.type === 'error')!;
    const event = items.find((i) => i.type === 'event')!;
    expect(error.tags).toEqual({ env: 'prod' });
    expect(error.contexts).toEqual({ order: { id: 7 } });
    expect(error.extra).toEqual({ region: 'eu' });
    expect(event.tags).toEqual({ env: 'prod' });
    expect(event.contexts).toEqual({ order: { id: 7 } });
    expect(event.extra).toEqual({ region: 'eu' });
    await client.close();
  });
});
```

- [ ] **Step 2: Run — expect FAIL** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test -- metadata`. Expected: `error.contexts`/`error.extra` are `undefined` (init doesn't seed yet) and the `track` event carries no `tags`/`contexts`/`extra` — assertions fail. (Typecheck also fails: `tags`/`contexts`/`extra` not yet on `ResolvedOptions` once `resolveOptions` references them.)

- [ ] **Step 3: Add `ResolvedOptions` fields** — in types.ts, after `release: string | null;` (line 303):
```ts
  release: string | null;
  tags: Record<string, string>;
  contexts: Record<string, unknown>;
  extra: Record<string, unknown>;
```

- [ ] **Step 4: Populate them in `resolveOptions`** — in client.ts, add to the returned object (after `release: options.release ?? DEFAULTS.release,` at line 51):
```ts
    release: options.release ?? DEFAULTS.release,
    tags: options.tags ?? {},
    contexts: options.contexts ?? {},
    extra: options.extra ?? {},
```

- [ ] **Step 5: Seed the global scope in the constructor** — replace the single `getGlobalScope().setMaxBreadcrumbs(...)` line (client.ts line 119) with:
```ts
    const globalScope = getGlobalScope();
    globalScope.setMaxBreadcrumbs(this.options.maxBreadcrumbs);
    globalScope.setTags(this.options.tags);
    for (const [name, block] of Object.entries(this.options.contexts)) {
      globalScope.setContext(name, block);
    }
    for (const [key, value] of Object.entries(this.options.extra)) {
      globalScope.setExtra(key, value);
    }
```

- [ ] **Step 6: Run — expect PASS** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm run typecheck && npm test`. Expected: typecheck clean; `metadata.test.ts` seeding test passes; whole suite green (other files pass fresh empty `InitOptions` ⇒ seed loops are no-ops).

- [ ] **Step 7: Commit**
```bash
git -C /home/splimter/projects/freelance/sauron add sdks/node/src/types.ts sdks/node/src/client.ts sdks/node/test/metadata.test.ts
git -C /home/splimter/projects/freelance/sauron commit -m "feat(sdk-node): seed the global scope from init tags/contexts/extra defaults

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4.4: Per-call `{tags,contexts,extra}` on captureException/captureMessage/track + facade wrappers (and GOLDEN_EVENT tags)

**Files:**
- Modify: `sdks/node/src/client.ts` (imports ~13-27; `track` ~194-207; `captureException` ~210-236; `captureMessage` ~239-261)
- Modify: `sdks/node/src/index.ts` (imports ~11-18; `track` ~54-60; `captureMessage` ~68-70)
- Modify: `sdks/node/test/envelope.test.ts` (`GOLDEN_EVENT` ~66-74)
- Test: `sdks/node/test/metadata.test.ts` (append per-call override describe)

**Interfaces:**
- Consumes: `MetadataOptions` (Task 4.2); `Scope.mergeMetadata` (Task 4.1); extended `CaptureExceptionOptions` (Task 4.2).
- Produces: `captureException(err, {…, contexts?, extra?})`; `captureMessage(msg, level?, options?: MetadataOptions)`; `track(event, distinctId, properties?, options?: MetadataOptions)`; matching top-level facade wrappers. `track`/analytics events now carry merged scope+per-call metadata (omit-when-empty).

- [ ] **Step 1: Add failing per-call tests** — append to `sdks/node/test/metadata.test.ts`:
```ts
describe('per-call metadata overrides scope', () => {
  it('captureException per-call contexts/extra override same-named scope blocks', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    getGlobalScope().setContext('order', { id: 1 });
    getGlobalScope().setExtra('trace_id', 'scope');
    client.captureException(new Error('boom'), {
      contexts: { order: { id: 2 } },
      extra: { call_key: 'call' },
    });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.contexts).toEqual({ order: { id: 2 } });                 // per-call block replaced scope's
    expect(item.extra).toEqual({ trace_id: 'scope', call_key: 'call' }); // shallow-merged by key
    await client.close();
  });

  it('captureMessage accepts per-call tags/contexts/extra', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    client.captureMessage('note', 'warning', {
      tags: { a: '1' },
      contexts: { page: { route: '/x' } },
    });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.level).toBe('warning');
    expect(item.tags).toEqual({ a: '1' });
    expect(item.contexts).toEqual({ page: { route: '/x' } });
    expect('extra' in item).toBe(false); // omit-when-empty
    await client.close();
  });

  it('track merges per-call tags over scope and omits empty contexts/extra', async () => {
    const fake = makeFakeFetch();
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    getGlobalScope().setTag('env', 'prod');
    client.track('viewed', 'u_1', {}, { tags: { plan: 'pro' } });
    await client.flush();
    const item = fake.envelopes[0].items[0] as Record<string, any>;
    expect(item.tags).toEqual({ env: 'prod', plan: 'pro' });
    expect('contexts' in item).toBe(false);
    expect('extra' in item).toBe(false);
    await client.close();
  });
});
```

- [ ] **Step 2: Run — expect FAIL** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test -- metadata`. Expected: captureException `contexts`/`extra` undefined (not wired from options); `captureMessage` rejects the 3rd argument / drops it; `track` ignores `options` and emits no `tags`. Assertions fail.

- [ ] **Step 3: Import `MetadataOptions` in client.ts** — add it to the type import block (client.ts 13-27), e.g. after `Level,`:
```ts
  Level,
  MetadataOptions,
```

- [ ] **Step 4: Wire `captureException` contexts/extra** — in the `ErrorItem` literal (client.ts ~228), insert after `tags: options.tags ?? {},`:
```ts
      tags: options.tags ?? {},
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
```
(`applyToErrorItem` at line 234 then merges scope under these and omits empties.)

- [ ] **Step 5: Give `captureMessage` a per-call options object** — replace the signature + literal (client.ts 239-258):
```ts
  captureMessage(message: string, level: Level = 'info', options: MetadataOptions = {}): void {
    const item: ErrorItem = {
      type: 'error',
      event_id: randomUUID(),
      level,
      timestamp: isoNow(),
      exception: {
        type: 'Message',
        value: message,
        mechanism: { type: 'generic', handled: true },
        stacktrace: [],
      },
      message,
      breadcrumbs: [],
      tags: options.tags ?? {},
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    getCurrentScope().applyToErrorItem(item);
    this.dispatch(item);
  }
```

- [ ] **Step 6: Give `track` a per-call options object + scope merge** — replace `track` (client.ts 194-207):
```ts
  track(
    event: string,
    distinctId: string,
    properties?: Record<string, unknown>,
    options: MetadataOptions = {},
  ): void {
    if (typeof event !== 'string' || event.length === 0) return;
    if (typeof distinctId !== 'string' || distinctId.length === 0) return;
    const item: EventItem = {
      type: 'event',
      name: event,
      distinct_id: distinctId,
      properties: properties ?? {},
      timestamp: isoNow(),
      session_id: null,
      screen: null,
      ...getCurrentScope().mergeMetadata(options),
    };
    this.dispatch(item);
  }
```

- [ ] **Step 7: Update the facade wrappers in index.ts** — add `MetadataOptions` to the type import (index.ts 11-18, e.g. after `Level,`), then replace `track` (54-60) and `captureMessage` (68-70):
```ts
export function track(
  event: string,
  distinctId: string,
  properties?: Record<string, unknown>,
  options?: MetadataOptions,
): void {
  activeClient?.track(event, distinctId, properties, options);
}
```
```ts
export function captureMessage(message: string, level?: Level, options?: MetadataOptions): void {
  activeClient?.captureMessage(message, level, options);
}
```
(`captureException` wrapper is unchanged — `options` already flows through and now carries `contexts`/`extra` via the extended `CaptureExceptionOptions`.)

- [ ] **Step 8: Run — expect the golden EVENT to break** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test`. Expected: `metadata.test.ts` per-call tests pass, but `envelope.test.ts › produces items matching the golden fixture` now FAILS: the golden scenario sets `scope.setTags({ env: 'prod', req: '42' })` before `client.track(...)`, so the event now carries `tags: { env: 'prod', req: '42' }` that `GOLDEN_EVENT` lacks. (This is the same-task golden break required by the contract.)

- [ ] **Step 9: Update `GOLDEN_EVENT`** — in `sdks/node/test/envelope.test.ts`, add the merged scope tags (69-74):
```ts
const GOLDEN_EVENT: EventItem = {
  type: 'event',
  name: 'checkout_completed',
  distinct_id: 'u_123',
  properties: { cart_value: 42.5 },
  timestamp: '2026-07-15T10:29:40.000Z',
  session_id: null,
  screen: null,
  tags: { env: 'prod', req: '42' },
};
```

- [ ] **Step 10: Run — expect PASS** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm run typecheck && npm test`. Expected: whole suite green, including `envelope.test.ts` golden and the strict event `toEqual` in `transport.test.ts` (that test uses an empty scope ⇒ omit-when-empty adds no keys). `index.test.ts` still green (`captureMessage('note')` / `track(...)` with omitted `options` default to `{}`).

- [ ] **Step 11: Commit**
```bash
git -C /home/splimter/projects/freelance/sauron add sdks/node/src/client.ts sdks/node/src/index.ts sdks/node/test/metadata.test.ts sdks/node/test/envelope.test.ts
git -C /home/splimter/projects/freelance/sauron commit -m "feat(sdk-node): accept per-call tags/contexts/extra on capture and track

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4.5: End-to-end golden coverage — scope contexts/extra flow to error + event

**Files:**
- Modify: `sdks/node/test/envelope.test.ts` (`GOLDEN_ERROR` ~59-64; `GOLDEN_EVENT` ~66-75; golden scenario ~181-205; add an omit-when-empty assertion)

**Interfaces:**
- Consumes: full emit path from Tasks 4.1/4.4 (`applyToErrorItem` + `track` scope merge, omit-when-empty).
- Produces: the reconciled Node golden now exercises `contexts`/`extra` on both the error and the analytics event; a dedicated bare-capture test locks the omit-when-empty guarantee.

- [ ] **Step 1: Extend the golden scenario to set scope contexts/extra** — in `envelope.test.ts`, in the `withScope` block, add after `scope.setTags({ env: 'prod', req: '42' });` (line 183):
```ts
      scope.setTags({ env: 'prod', req: '42' });
      scope.setContext('order', { id: 7 });
      scope.setExtra('trace_id', 'abc123');
```

- [ ] **Step 2: Add the bare-capture omit-when-empty test** — append inside `describe('client emits the reconciled golden shape', …)` (after the transaction-omit test, ~247):
```ts
  it('omits contexts/extra on an error captured with no metadata set', async () => {
    const client = newClient(fake.fetchImpl);
    client.captureException(new Error('bare'));
    await client.flush();

    const item = fake.envelopes[0].items[0] as unknown as Record<string, unknown>;
    expect(item.type).toBe('error');
    expect('contexts' in item).toBe(false);
    expect('extra' in item).toBe(false);
    expect(item.tags).toEqual({}); // tags stays present per the existing Node convention
  });
```

- [ ] **Step 3: Run — expect FAIL** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm test -- envelope`. Expected: the golden-compare test fails because the emitted error AND event now include `contexts: { order: { id: 7 } }` / `extra: { trace_id: 'abc123' }` that the `GOLDEN_ERROR`/`GOLDEN_EVENT` literals don't yet declare. (The new bare-capture test passes — global scope is untouched by the `withScope` child.)

- [ ] **Step 4: Update `GOLDEN_ERROR`** — add the two blocks after `tags` (envelope.test.ts 59):
```ts
  tags: { env: 'prod', req: '42' },
  contexts: { order: { id: 7 } },
  extra: { trace_id: 'abc123' },
  fingerprint: ['checkout-failure'],
```

- [ ] **Step 5: Update `GOLDEN_EVENT`** — add `contexts`/`extra` alongside the `tags` added in 4.4:
```ts
  session_id: null,
  screen: null,
  tags: { env: 'prod', req: '42' },
  contexts: { order: { id: 7 } },
  extra: { trace_id: 'abc123' },
};
```

- [ ] **Step 6: Run — expect PASS** — `cd /home/splimter/projects/freelance/sauron/sdks/node && npm run typecheck && npm test`. Expected: whole suite green (`Test Files 15 passed`). `normalize()` leaves `contexts`/`extra` untouched, so the golden-compare now asserts the full metadata shape on both the error and the event.

- [ ] **Step 7: Commit**
```bash
git -C /home/splimter/projects/freelance/sauron add sdks/node/test/envelope.test.ts
git -C /home/splimter/projects/freelance/sauron commit -m "test(sdk-node): cover scope contexts/extra flowing to golden error and event

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```



## Slice 5 — Python SDK

### Task 5.1: init/Client metadata defaults seed the global scope

**Files:**
- Create/Test: `sdks/python/tests/test_metadata.py`
- Modify: `sdks/python/sauron/_client.py` (import block ~15-19; `Client.__init__` signature ~45-65; body after `set_max_breadcrumbs` ~78)
- Modify: `sdks/python/sauron/__init__.py` (`init` signature ~80-99; `Client(...)` call ~133-149)

**Interfaces:**
- Consumes: `Scope.set_tags(map)`, `Scope.set_context(name, block)`, `Scope.set_extra(key, value)`, `get_global_scope() -> Scope` (all already in `sauron/_scope.py`); `Scope.apply_to_error` already merges scope `contexts`/`extra` onto error items (omit-when-empty).
- Produces: `Client.__init__(..., tags=None, contexts=None, extra=None)` and `sauron.init(..., tags=None, contexts=None, extra=None)` that seed the process-wide global scope with init defaults.

- [ ] **Step 1: Write the failing test** — create `sdks/python/tests/test_metadata.py`:
```python
"""Metadata scopes (tags / contexts / extra) across init, per-capture, and track."""

import unittest

import sauron
from sauron._client import Client
from sauron._scope import get_current_scope, get_global_scope, reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestInitDefaults(unittest.TestCase):
    """init()/Client() tags/contexts/extra seed the global scope."""

    def setUp(self):
        reset_scopes()

    def tearDown(self):
        sauron.close()
        reset_scopes()

    def test_init_seeds_global_scope(self):
        sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            tags={"service": "api"},
            contexts={"order": {"id": 7}},
            extra={"build": "abc123"},
            sender=FakeSender(status=200),
        )
        g = get_global_scope()
        self.assertEqual(g.tags, {"service": "api"})
        self.assertEqual(g.contexts, {"order": {"id": 7}})
        self.assertEqual(g.extra, {"build": "abc123"})

    def test_init_defaults_flow_onto_captured_error(self):
        fake = FakeSender(status=200)
        sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            tags={"service": "api"},
            contexts={"order": {"id": 7}},
            extra={"build": "abc123"},
            sender=fake,
        )
        try:
            raise ValueError("boom")
        except ValueError as exc:
            sauron.capture_exception(exc)
        sauron.flush()
        err = fake.items[0]
        self.assertEqual(err["tags"], {"service": "api"})
        self.assertEqual(err["contexts"], {"order": {"id": 7}})
        self.assertEqual(err["extra"], {"build": "abc123"})
```

- [ ] **Step 2: Run (fails)** — `cd sdks/python && python -m pytest tests/test_metadata.py -q`
  Expected: `TypeError: __init__() got an unexpected keyword argument 'tags'` → `2 failed`.

- [ ] **Step 3: Import `get_global_scope` in `_client.py`** — change the `from ._scope import (...)` block (~15-19) to:
```python
from ._scope import (
    build_breadcrumb,
    get_current_scope,
    get_global_scope,
    set_max_breadcrumbs,
)
```

- [ ] **Step 4: Add the three kwargs to `Client.__init__`** — insert after `max_breadcrumbs: int = 100,` (~54):
```python
        max_breadcrumbs: int = 100,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
```

- [ ] **Step 5: Seed the global scope** — insert immediately after `set_max_breadcrumbs(max_breadcrumbs)` (~78):
```python
        # The active breadcrumb ring size lives on the scope; clones inherit it.
        set_max_breadcrumbs(max_breadcrumbs)

        # Seed the process-wide scope with init-time metadata defaults so every
        # error/message/track picks them up (runtime setters last-write-wins).
        gscope = get_global_scope()
        if tags:
            gscope.set_tags(tags)
        if contexts:
            for _name, _block in contexts.items():
                gscope.set_context(_name, _block)
        if extra:
            for _key, _value in extra.items():
                gscope.set_extra(_key, _value)
```

- [ ] **Step 6: Add + forward the kwargs in `init()`** — in `sdks/python/sauron/__init__.py`, add to the `init` signature after `max_breadcrumbs: int = 100,` (~88):
```python
    max_breadcrumbs: int = 100,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
```
  and in the `Client(...)` construction add after `max_breadcrumbs=max_breadcrumbs,` (~140):
```python
        max_breadcrumbs=max_breadcrumbs,
        tags=tags,
        contexts=contexts,
        extra=extra,
```

- [ ] **Step 7: Run (passes)** — `cd sdks/python && python -m pytest tests/test_metadata.py -q`
  Expected: `2 passed`. Then full suite `python -m pytest -q` → `102 passed`.

- [ ] **Step 8: Commit**
```bash
git add sdks/python/sauron/_client.py sdks/python/sauron/__init__.py sdks/python/tests/test_metadata.py
git commit -m "feat(sdk-python): seed global scope from init tags/contexts/extra defaults"
```

---

### Task 5.2: Per-capture metadata on capture_exception and capture_message

**Files:**
- Modify: `sdks/python/sauron/_client.py` (`Client.capture_exception` ~209-267; `Client.capture_message` ~269-292)
- Modify: `sdks/python/sauron/__init__.py` (`capture_exception` wrapper ~171-183; `capture_message` wrapper ~186-189)
- Test: `sdks/python/tests/test_metadata.py` (append)

**Interfaces:**
- Consumes: `Scope.apply_to_error(item)` — already merges scope `tags`/`contexts`/`extra` under any per-call values on the item, per-call wins (tags/extra by key, contexts by block name), empty blocks omitted.
- Produces: `capture_exception(..., contexts=None, extra=None)` and `capture_message(message, level="info", *, tags=None, contexts=None, extra=None)` that stamp per-call metadata onto the outgoing error item.

- [ ] **Step 1: Write the failing tests** — append to `sdks/python/tests/test_metadata.py`:
```python
class TestPerCaptureMetadata(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.fake = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.fake
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def test_capture_exception_per_call_contexts_extra_override_scope(self):
        get_current_scope().set_context("order", {"id": 1})
        get_current_scope().set_extra("a", "scope")
        try:
            raise ValueError("boom")
        except ValueError as exc:
            self.client.capture_exception(
                exc,
                tags={"area": "billing"},
                contexts={"order": {"id": 99}, "cart": {"n": 2}},
                extra={"a": "call", "b": "call"},
            )
        self.client.flush()
        err = self.fake.items[0]
        self.assertEqual(err["tags"], {"area": "billing"})
        # contexts merge by block name (per-call "order" replaces scope "order").
        self.assertEqual(err["contexts"], {"order": {"id": 99}, "cart": {"n": 2}})
        # extra merges by shallow key (per-call "a" wins).
        self.assertEqual(err["extra"], {"a": "call", "b": "call"})

    def test_capture_message_attaches_scope_and_per_call(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 7})
        self.client.capture_message(
            "hi",
            tags={"area": "auth"},
            contexts={"cart": {"n": 2}},
            extra={"k": "v"},
        )
        self.client.flush()
        msg = self.fake.items[0]
        self.assertEqual(msg["tags"], {"env": "prod", "area": "auth"})
        self.assertEqual(msg["contexts"], {"order": {"id": 7}, "cart": {"n": 2}})
        self.assertEqual(msg["extra"], {"k": "v"})

    def test_capture_message_omits_empty_metadata(self):
        self.client.capture_message("hi")
        self.client.flush()
        msg = self.fake.items[0]
        self.assertEqual(msg["tags"], {})
        self.assertNotIn("contexts", msg)
        self.assertNotIn("extra", msg)
```

- [ ] **Step 2: Run (fails)** — `cd sdks/python && python -m pytest tests/test_metadata.py -q`
  Expected: `TypeError: capture_exception() got an unexpected keyword argument 'contexts'` / `capture_message() got an unexpected keyword argument 'tags'` → `3 failed`.

- [ ] **Step 3: Add `contexts`/`extra` to `Client.capture_exception`** — insert into the signature (~215) after `tags`:
```python
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
        fingerprint: Optional[Sequence[str]] = None,
```
  and set them on the item just before the `apply_to_error` call (~263-265), replacing that comment + call with:
```python
        # Per-call metadata: attach only when non-empty so the scope merge in
        # apply_to_error can omit empty blocks (never emit {}).
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        # Merge the active scope (breadcrumbs/tags/user/contexts/extra); per-call
        # user/tags/contexts/extra already on the item take precedence.
        get_current_scope().apply_to_error(item)
        self._dispatch(item)
        return event_id
```

- [ ] **Step 4: Add metadata kwargs to `Client.capture_message`** — replace the signature (~269-271) and item `tags`/scope-apply so it reads:
```python
    def capture_message(
        self,
        message: str,
        level: str = "info",
        *,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
    ) -> Optional[str]:
        if not self.enabled:
            return None
        level = level if level in _VALID_LEVELS else "info"
        event_id = uuid.uuid4().hex
        item = {
            "type": "error",
            "event_id": event_id,
            "level": level,
            "timestamp": _now_iso(),
            "exception": None,
            "message": message,
            "breadcrumbs": [],
            "tags": dict(tags) if tags else {},
            "fingerprint": None,
            "user": None,
            "session_id": None,
            "screen": None,
        }
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        get_current_scope().apply_to_error(item)
        self._dispatch(item)
        return event_id
```

- [ ] **Step 5: Forward from the public wrappers** — in `sdks/python/sauron/__init__.py` replace `capture_exception` (~171-183) and `capture_message` (~186-189):
```python
def capture_exception(
    error: Optional[BaseException] = None,
    *,
    user: Optional[Mapping[str, Any]] = None,
    level: str = "error",
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
    fingerprint: Optional[Sequence[str]] = None,
) -> Optional[str]:
    if _client is not None:
        return _client.capture_exception(
            error,
            user=user,
            level=level,
            tags=tags,
            contexts=contexts,
            extra=extra,
            fingerprint=fingerprint,
        )
    return None


def capture_message(
    message: str,
    level: str = "info",
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> Optional[str]:
    if _client is not None:
        return _client.capture_message(
            message, level, tags=tags, contexts=contexts, extra=extra
        )
    return None
```

- [ ] **Step 6: Run (passes)** — `cd sdks/python && python -m pytest tests/test_metadata.py -q`
  Expected: `5 passed`. Then `python -m pytest -q` → `105 passed`.

- [ ] **Step 7: Commit**
```bash
git add sdks/python/sauron/_client.py sdks/python/sauron/__init__.py sdks/python/tests/test_metadata.py
git commit -m "feat(sdk-python): per-capture contexts/extra on capture_exception and tags/contexts/extra on capture_message"
```

---

### Task 5.3: track() attaches tags/contexts/extra to analytics events

**Files:**
- Modify: `sdks/python/sauron/_scope.py` (add `Scope.apply_to_event`, after `apply_to_error` ~161)
- Modify: `sdks/python/sauron/_client.py` (`Client.track` ~187-207)
- Modify: `sdks/python/sauron/__init__.py` (`track` wrapper ~162-168)
- Test: `sdks/python/tests/test_metadata.py` (append)
- Modify (golden break-fix, same task): `sdks/python/tests/test_golden.py` (`GOLDEN_EVENT` ~79-87)

**Interfaces:**
- Consumes: `get_current_scope()`; the event item shape `{type,name,distinct_id,properties,timestamp,session_id,screen}` built in `Client.track`.
- Produces: `Scope.apply_to_event(item)` (tags/contexts/extra-only merge, omit-when-empty) and `track(event, distinct_id, properties=None, *, tags=None, contexts=None, extra=None)`.

- [ ] **Step 1: Write the failing tests** — append to `sdks/python/tests/test_metadata.py`:
```python
class TestTrackMetadata(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.fake = FakeSender(status=200)
        self.client = Client(
            DSN, flush_interval=3600, max_batch=1000, sender=self.fake
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def _event(self):
        self.client.flush()
        return self.fake.items[0]

    def test_track_attaches_scope_metadata(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 7})
        get_current_scope().set_extra("build", "abc")
        self.client.track("checkout", "u_1", {"v": 1})
        ev = self._event()
        self.assertEqual(ev["tags"], {"env": "prod"})
        self.assertEqual(ev["contexts"], {"order": {"id": 7}})
        self.assertEqual(ev["extra"], {"build": "abc"})

    def test_track_per_call_overrides_scope_per_key(self):
        get_current_scope().set_tag("env", "prod")
        get_current_scope().set_context("order", {"id": 1})
        get_current_scope().set_extra("a", "scope")
        self.client.track(
            "checkout",
            "u_1",
            tags={"env": "staging", "area": "billing"},
            contexts={"order": {"id": 99}, "cart": {"n": 2}},
            extra={"a": "call", "b": "call"},
        )
        ev = self._event()
        self.assertEqual(ev["tags"], {"env": "staging", "area": "billing"})
        self.assertEqual(ev["contexts"], {"order": {"id": 99}, "cart": {"n": 2}})
        self.assertEqual(ev["extra"], {"a": "call", "b": "call"})

    def test_track_omits_empty_metadata(self):
        self.client.track("ping", "u_1")
        ev = self._event()
        self.assertNotIn("tags", ev)
        self.assertNotIn("contexts", ev)
        self.assertNotIn("extra", ev)
```

- [ ] **Step 2: Run (fails)** — `cd sdks/python && python -m pytest tests/test_metadata.py -q`
  Expected: `TypeError: track() got an unexpected keyword argument 'tags'` → `3 failed`.

- [ ] **Step 3: Add `Scope.apply_to_event`** — in `sdks/python/sauron/_scope.py`, insert directly after `apply_to_error` (after line ~161):
```python
    def apply_to_event(self, item: Dict[str, Any]) -> None:
        """Stamp scope tags/contexts/extra onto an analytics event item in place.

        Mirrors the tags/contexts/extra half of :meth:`apply_to_error` (no user,
        breadcrumbs, or fingerprint): scope values first, per-call values already
        on ``item`` override (tags/extra by key, contexts by block name). Empty
        results are omitted rather than emitted as ``{}``.
        """
        merged_tags: Dict[str, Any] = dict(self.tags)
        merged_tags.update(item.get("tags") or {})
        if merged_tags:
            item["tags"] = merged_tags
        else:
            item.pop("tags", None)

        merged_ctx: Dict[str, Any] = dict(self.contexts)
        merged_ctx.update(item.get("contexts") or {})
        if merged_ctx:
            item["contexts"] = merged_ctx
        else:
            item.pop("contexts", None)

        merged_extra: Dict[str, Any] = dict(self.extra)
        merged_extra.update(item.get("extra") or {})
        if merged_extra:
            item["extra"] = merged_extra
        else:
            item.pop("extra", None)
```

- [ ] **Step 4: Wire metadata into `Client.track`** — replace `Client.track` (~187-207) with:
```python
    def track(
        self,
        event: str,
        distinct_id: str,
        properties: Optional[Mapping[str, Any]] = None,
        *,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
    ) -> None:
        if not self.enabled:
            return
        if not distinct_id:
            self._log("track() requires a distinct_id; dropping event", event)
            return
        item = {
            "type": "event",
            "name": event,
            "distinct_id": distinct_id,
            "properties": dict(properties) if properties else {},
            "timestamp": _now_iso(),
            "session_id": None,
            "screen": None,
        }
        # Per-call metadata attached only when non-empty; the scope merge then
        # folds in defaults and omits empty blocks (never emit {}).
        if tags:
            item["tags"] = dict(tags)
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        get_current_scope().apply_to_event(item)
        self._dispatch(item)
```

- [ ] **Step 5: Forward from the `track` wrapper** — in `sdks/python/sauron/__init__.py` replace `track` (~162-168):
```python
def track(
    event: str,
    distinct_id: str,
    properties: Optional[Mapping[str, Any]] = None,
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> None:
    if _client is not None:
        _client.track(
            event,
            distinct_id,
            properties,
            tags=tags,
            contexts=contexts,
            extra=extra,
        )
```

- [ ] **Step 6: Run new tests (pass) then observe the golden break** — `cd sdks/python && python -m pytest tests/test_metadata.py -q` → `8 passed`. Then `python -m pytest tests/test_golden.py -q` → **fails**: the golden client's `_emit_golden` sets scope tags `{"area":"billing","tier":"pro"}`, so `track()` now stamps them onto the event, which no longer equals `GOLDEN_EVENT` (`AssertionError` on `_normalize(items[1]) == GOLDEN_EVENT`). Fix in the same task (golden must be updated alongside the model change).

- [ ] **Step 7: Update `GOLDEN_EVENT`** — in `sdks/python/tests/test_golden.py` add the scope-derived tags to `GOLDEN_EVENT` (~79-87):
```python
GOLDEN_EVENT = {
    "type": "event",
    "name": "checkout_completed",
    "distinct_id": "u_123",
    "properties": {"cart_value": 42.5},
    "timestamp": TS,
    "session_id": None,
    "screen": None,
    "tags": {"area": "billing", "tier": "pro"},
}
```
  (`_normalize` needs no change — `tags` is static, not a pinned dynamic field. Dict equality is order-insensitive, so appending the key is safe.)

- [ ] **Step 8: Run (passes)** — `cd sdks/python && python -m pytest -q`
  Expected: `108 passed`.

- [ ] **Step 9: Commit**
```bash
git add sdks/python/sauron/_scope.py sdks/python/sauron/_client.py sdks/python/sauron/__init__.py sdks/python/tests/test_metadata.py sdks/python/tests/test_golden.py
git commit -m "feat(sdk-python): attach tags/contexts/extra to track() analytics events"
```

---

### Task 5.4: Extend golden fixtures for end-to-end error + event metadata scopes

**Files:**
- Modify: `sdks/python/tests/test_golden.py` (`GOLDEN_ERROR` ~49-77; `GOLDEN_EVENT` ~79-88; `_emit_golden` ~198-228; add one assertion test)

**Interfaces:**
- Consumes: `Scope.set_context`/`set_extra` (seeded via `get_current_scope()` in `_emit_golden`), `Scope.apply_to_error` (error) and `Scope.apply_to_event` (event) — both already implemented in Tasks 5.2/5.3.
- Produces: golden fixtures that lock the new `contexts`/`extra` blocks flowing scope → error and scope → event end-to-end.

- [ ] **Step 1: Seed scope contexts/extra in `_emit_golden`** — in `sdks/python/tests/test_golden.py`, add two setters right after the existing `set_tags` line (~203):
```python
        get_current_scope().set_user({"id": "u_123", "email": "a@b.co"})
        get_current_scope().set_tag("area", "billing")
        get_current_scope().set_tags({"tier": "pro"})
        get_current_scope().set_context("order", {"id": 7})
        get_current_scope().set_extra("build", "abc123")
```

- [ ] **Step 2: Run (fails)** — `cd sdks/python && python -m pytest tests/test_golden.py -q`
  Expected: `AssertionError` — the emitted error and event now carry `contexts`/`extra` that `GOLDEN_ERROR`/`GOLDEN_EVENT` lack → `1 failed` (`test_emitted_items_match_golden`).

- [ ] **Step 3: Add `contexts`/`extra` to `GOLDEN_ERROR`** — extend the fixture (append after `"screen": None,` at ~76):
```python
    "user": {"id": "u_123", "email": "a@b.co"},
    "session_id": None,
    "screen": None,
    "contexts": {"order": {"id": 7}},
    "extra": {"build": "abc123"},
}
```

- [ ] **Step 4: Add `contexts`/`extra` to `GOLDEN_EVENT`** — extend the fixture (append after the `tags` line added in Task 5.3):
```python
    "screen": None,
    "tags": {"area": "billing", "tier": "pro"},
    "contexts": {"order": {"id": 7}},
    "extra": {"build": "abc123"},
}
```

- [ ] **Step 5: Add an explicit end-to-end assertion** — append this method to `TestGoldenClientEmitsShape` (after `test_reconciled_fields_present_on_error`, ~264):
```python
    def test_metadata_scopes_present_on_error_and_event(self):
        items = self._emit_golden()
        error, event = items[0], items[1]
        # Dev metadata scopes flow from the scope onto both signal types,
        # distinct from the machine-owned envelope "context" block.
        self.assertEqual(error["contexts"], {"order": {"id": 7}})
        self.assertEqual(error["extra"], {"build": "abc123"})
        self.assertEqual(event["tags"], {"area": "billing", "tier": "pro"})
        self.assertEqual(event["contexts"], {"order": {"id": 7}})
        self.assertEqual(event["extra"], {"build": "abc123"})
```

- [ ] **Step 6: Run (passes)** — `cd sdks/python && python -m pytest tests/test_golden.py -q` → `9 passed`; then full suite `python -m pytest -q` → `109 passed`.

- [ ] **Step 7: Commit**
```bash
git add sdks/python/tests/test_golden.py
git commit -m "test(sdk-python): extend golden fixtures for error/event metadata scopes"
```



## Slice 6 — C# SDK

### Task 6.1: Add Contexts/Extra to ErrorItem + Tags/Contexts/Extra to EventItem (omit-when-empty) and extend the golden

**Files:**
- Modify: `sdks/csharp/Sauron/Envelope.cs` (EventItem 79-88, ErrorItem 90-107; `System.Text.Json.Serialization` already imported at line 3)
- Test/Modify: `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs` (GoldenJson 24-95, BuildGoldenEnvelope 97-176)

**Interfaces:**
- Consumes: `SauronJson.Options` (global `DefaultIgnoreCondition = JsonIgnoreCondition.Never`, snake_case policy) — so per-property omit-when-empty MUST be a nullable field + `[JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]`, which overrides the global `Never`.
- Produces: `ErrorItem.Contexts`, `ErrorItem.Extra`, `EventItem.Tags`, `EventItem.Contexts`, `EventItem.Extra` — all `Dictionary<string, object?>?`, omitted when null. Consumed by Tasks 6.2, 6.4.

- [ ] **Step 1: Write the failing serialization test** — append to `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs` (before the `// ---- helpers` region, ~line 283). It references the not-yet-existing `EventItem` fields, so it fails to compile (red).
```csharp
    [Fact]
    public void EventItem_OmitsEmptyMetadata_ButEmitsWhenSet()
    {
        var empty = JsonSerializer.Serialize(
            new EventItem { Name = "n", DistinctId = "d", Timestamp = "t" }, SauronJson.Options);
        using (var d = JsonDocument.Parse(empty))
        {
            Assert.False(d.RootElement.TryGetProperty("tags", out _));
            Assert.False(d.RootElement.TryGetProperty("contexts", out _));
            Assert.False(d.RootElement.TryGetProperty("extra", out _));
        }

        var full = JsonSerializer.Serialize(new EventItem
        {
            Name = "n", DistinctId = "d", Timestamp = "t",
            Tags = new Dictionary<string, object?> { ["env"] = "prod" },
            Contexts = new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
            Extra = new Dictionary<string, object?> { ["build"] = "1" },
        }, SauronJson.Options);
        using (var d = JsonDocument.Parse(full))
        {
            Assert.Equal("prod", d.RootElement.GetProperty("tags").GetProperty("env").GetString());
            Assert.Equal(7, d.RootElement.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32());
            Assert.Equal("1", d.RootElement.GetProperty("extra").GetProperty("build").GetString());
        }
    }
```

- [ ] **Step 2: Run — expect RED (compile error)**
```
dotnet build /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.Tests/Sauron.Tests.csproj
```
Expected: `error CS0117: 'EventItem' does not contain a definition for 'Tags'` (and Contexts/Extra).

- [ ] **Step 3: Add the fields to `EventItem`** — in `sdks/csharp/Sauron/Envelope.cs`, replace the `EventItem` class body (79-88):
```csharp
internal sealed class EventItem
{
    public string Type { get; set; } = "event";
    public string Name { get; set; } = string.Empty;
    public string DistinctId { get; set; } = string.Empty;
    public Dictionary<string, object?> Properties { get; set; } = new();
    public string Timestamp { get; set; } = string.Empty;
    public string? SessionId { get; set; }
    public string? Screen { get; set; }

    // Dev-owned metadata scopes. Omitted from the wire when null (empty) despite the
    // global JsonIgnoreCondition.Never — the per-property attribute wins.
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public Dictionary<string, object?>? Tags { get; set; }

    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public Dictionary<string, object?>? Contexts { get; set; }

    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public Dictionary<string, object?>? Extra { get; set; }
}
```

- [ ] **Step 4: Add Contexts/Extra to `ErrorItem`** — in the same file, insert after the existing `Tags` property (line 99), keeping `Tags` unchanged (it stays non-nullable/always-emitted, matching the current golden and the backend null->{} guard):
```csharp
    public Dictionary<string, object?> Tags { get; set; } = new();

    /// <summary>Dev-owned structured context blocks (name -> block). DISTINCT from the machine
    /// envelope <c>context</c>. Omitted when null (empty).</summary>
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public Dictionary<string, object?>? Contexts { get; set; }

    /// <summary>Dev-owned freeform extra (key -> any). Omitted when null (empty).</summary>
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public Dictionary<string, object?>? Extra { get; set; }
```

- [ ] **Step 5: Extend the golden error item for coverage** — in `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs`, add `contexts`/`extra` to the error item in `GoldenJson`. Replace the golden error item's `"tags"` line (58):
```json
          "tags": { "env": "prod", "req": "42" },
          "contexts": { "order": { "id": 7 } },
          "extra": { "build": "1.4.2" },
```
Then in `BuildGoldenEnvelope`, replace the error item's `Tags = ...` line (145):
```csharp
                Tags = new Dictionary<string, object?> { ["env"] = "prod", ["req"] = "42" },
                Contexts = new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
                Extra = new Dictionary<string, object?> { ["build"] = "1.4.2" },
```
(The golden EVENT item is intentionally left without tags/contexts/extra — that keeps live omit-when-empty coverage for events; `JsonDeepEquals` compares key-count, so both sides stay consistent.)

- [ ] **Step 6: Run — expect GREEN**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx
```
Expected: `Passed!  - Failed: 0` (all EnvelopeGoldenTests incl. the new `EventItem_OmitsEmptyMetadata_ButEmitsWhenSet` and the golden `SerializedGoldenEnvelope_MatchesTheWireContractShape`).

- [ ] **Step 7: Commit**
```
git add sdks/csharp/Sauron/Envelope.cs sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs
git commit -m "feat(sdk-dotnet): add contexts/extra to ErrorItem and tags/contexts/extra to EventItem (omit-when-empty)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6.2: Merge scope Contexts/Extra onto captured errors in `Scope.ApplyToError`

**Files:**
- Modify: `sdks/csharp/Sauron/Scope.cs` (ApplyToError 63-73)
- Test/Modify: `sdks/csharp/Sauron.Tests/ScopeTests.cs` (append tests)

**Interfaces:**
- Consumes: `ErrorItem.Contexts`, `ErrorItem.Extra` (Task 6.1); `Scope.Contexts`, `Scope.Extra` (existing `Dictionary<string, object?>` at Scope.cs 17-18); `Scope.SetContext`/`SetExtra` (existing 36-46).
- Produces: `ApplyToError` now fills item Contexts (merge by BLOCK NAME) and Extra (merge by shallow KEY), per-call wins via `TryAdd`, empty result normalized to null. Consumed by Tasks 6.3, 6.4.

- [ ] **Step 1: Write failing scope tests** — append to `sdks/csharp/Sauron.Tests/ScopeTests.cs` (before the closing brace, after line 99):
```csharp
    [Fact]
    public void ScopeContextsAndExtra_ApplyToError()
    {
        ScopeManager.Global.SetContext("order", new Dictionary<string, object?> { ["id"] = 7 });
        ScopeManager.Global.SetExtra("trace", "abc");

        var item = new ErrorItem();
        ScopeManager.Current.ApplyToError(item);

        Assert.NotNull(item.Contexts);
        var order = Assert.IsType<Dictionary<string, object?>>(item.Contexts!["order"]);
        Assert.Equal(7, order["id"]);
        Assert.Equal("abc", item.Extra!["trace"]);
    }

    [Fact]
    public void EmptyScope_LeavesContextsExtraNull_ForOmission()
    {
        var item = new ErrorItem();
        ScopeManager.Current.ApplyToError(item);

        Assert.Null(item.Contexts);
        Assert.Null(item.Extra);
    }

    [Fact]
    public void PerCallContextBlock_WinsOverScope_ByBlockName()
    {
        ScopeManager.Global.SetContext("order", new Dictionary<string, object?> { ["id"] = 1 });

        var item = new ErrorItem
        {
            Contexts = new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 99 } },
        };
        ScopeManager.Current.ApplyToError(item);

        var order = Assert.IsType<Dictionary<string, object?>>(item.Contexts!["order"]);
        Assert.Equal(99, order["id"]); // per-call block replaces the same-named scope block
    }
```

- [ ] **Step 2: Run — expect RED**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx --filter "FullyQualifiedName~Sauron.Tests.ScopeTests.ScopeContextsAndExtra_ApplyToError"
```
Expected: FAIL — `Assert.NotNull() Failure: Value is null` (ApplyToError does not yet touch Contexts).

- [ ] **Step 3: Implement the merge** — in `sdks/csharp/Sauron/Scope.cs`, replace `ApplyToError` (63-73):
```csharp
    public void ApplyToError(ErrorItem item)
    {
        foreach (var kv in Tags)
            item.Tags.TryAdd(kv.Key, kv.Value);

        if (Contexts.Count > 0)
        {
            item.Contexts ??= new Dictionary<string, object?>();
            foreach (var kv in Contexts)
                item.Contexts.TryAdd(kv.Key, kv.Value); // per-call block name wins
        }
        if (item.Contexts is { Count: 0 })
            item.Contexts = null; // omit-when-empty

        if (Extra.Count > 0)
        {
            item.Extra ??= new Dictionary<string, object?>();
            foreach (var kv in Extra)
                item.Extra.TryAdd(kv.Key, kv.Value); // per-call key wins
        }
        if (item.Extra is { Count: 0 })
            item.Extra = null; // omit-when-empty

        if (User is not null && item.User is null)
            item.User = new UserInfo { Id = User.Id, Email = User.Email, Username = User.Username };

        foreach (var crumb in Breadcrumbs)
            item.Breadcrumbs.Add(ToWire(crumb));
    }
```

- [ ] **Step 4: Run — expect GREEN**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx --filter "FullyQualifiedName~Sauron.Tests.ScopeTests"
```
Expected: `Passed!  - Failed: 0` (all ScopeTests incl. the 3 new ones).

- [ ] **Step 5: Commit**
```
git add sdks/csharp/Sauron/Scope.cs sdks/csharp/Sauron.Tests/ScopeTests.cs
git commit -m "feat(sdk-dotnet): merge scope contexts/extra onto captured errors" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6.3: Seed init-default tags/contexts/extra into the global scope

**Files:**
- Modify: `sdks/csharp/Sauron/SauronClient.cs` (SauronOptions 9-87; SauronClient ctor 110-148)
- Test/Create: `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs` (new)

**Interfaces:**
- Consumes: `ScopeManager.Global` + `SetTag`/`SetContext`/`SetExtra` (Scope.cs); `ApplyToError` merge (Task 6.2); `TestUtil.NewClient`/`FirstItem` (Sauron.Tests/TestUtil.cs).
- Produces: `SauronOptions.Tags` (`IReadOnlyDictionary<string,string>?`), `SauronOptions.Contexts` / `SauronOptions.Extra` (`IReadOnlyDictionary<string, object?>?`), seeded into `ScopeManager.Global` at construction of an enabled client.

- [ ] **Step 1: Write the failing test** — create `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs`:
```csharp
using System;
using System.Collections.Generic;
using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

/// <summary>Metadata-scope feature: init defaults, per-call overrides, and analytics parity.</summary>
[Collection("SauronScope")]
public class MetadataScopeTests
{
    public MetadataScopeTests() => ScopeManager.ResetForTests();

    [Fact]
    public void InitDefaults_SeedGlobalScope_AndApplyToCapturedError()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            Tags = new Dictionary<string, string> { ["env"] = "prod" },
            Contexts = new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
            Extra = new Dictionary<string, object?> { ["build"] = "123" },
        });

        client.CaptureMessage("hi");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("prod", item.GetProperty("tags").GetProperty("env").GetString());
        Assert.Equal(7, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32());
        Assert.Equal("123", item.GetProperty("extra").GetProperty("build").GetString());
    }
}
```

- [ ] **Step 2: Run — expect RED (compile error)**
```
dotnet build /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.Tests/Sauron.Tests.csproj
```
Expected: `error CS0117: 'SauronOptions' does not contain a definition for 'Tags'` (and Contexts/Extra).

- [ ] **Step 3: Add the option properties** — in `sdks/csharp/Sauron/SauronClient.cs`, insert into `SauronOptions` after the `Release` property (line 18):
```csharp
    /// <summary>Default tags seeded into the global scope at init (string -> string). Optional.</summary>
    public IReadOnlyDictionary<string, string>? Tags { get; set; }

    /// <summary>Default context blocks seeded into the global scope at init (name -> block). Optional.
    /// DISTINCT from the machine envelope <c>context</c>.</summary>
    public IReadOnlyDictionary<string, object?>? Contexts { get; set; }

    /// <summary>Default extra values seeded into the global scope at init (key -> any). Optional.</summary>
    public IReadOnlyDictionary<string, object?>? Extra { get; set; }
```

- [ ] **Step 4: Seed the global scope in the enabled-client path** — in the `SauronClient` constructor, replace the block at lines 142-143:
```csharp
        _transport = new Transport(dsn, options, http, ownsHttp);
        _enabled = true;
```
with:
```csharp
        _transport = new Transport(dsn, options, http, ownsHttp);
        _enabled = true;

        // Seed init-default metadata scopes into the process-wide global scope.
        if (options.Tags is not null)
            foreach (var kv in options.Tags)
                ScopeManager.Global.SetTag(kv.Key, kv.Value);
        if (options.Contexts is not null)
            foreach (var kv in options.Contexts)
                ScopeManager.Global.SetContext(kv.Key, kv.Value);
        if (options.Extra is not null)
            foreach (var kv in options.Extra)
                ScopeManager.Global.SetExtra(kv.Key, kv.Value);
```

- [ ] **Step 5: Run — expect GREEN**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx --filter "FullyQualifiedName~Sauron.Tests.MetadataScopeTests"
```
Expected: `Passed!  - Failed: 0` (`InitDefaults_SeedGlobalScope_AndApplyToCapturedError`).

- [ ] **Step 6: Commit**
```
git add sdks/csharp/Sauron/SauronClient.cs sdks/csharp/Sauron.Tests/MetadataScopeTests.cs
git commit -m "feat(sdk-dotnet): seed init tags/contexts/extra into the global scope" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6.4: Per-call tags/contexts/extra on CaptureException/CaptureMessage/Track (+ scope merge for analytics)

**Files:**
- Modify: `sdks/csharp/Sauron/Scope.cs` (add `ApplyToEvent`)
- Modify: `sdks/csharp/Sauron/SauronClient.cs` (Track 160-177, CaptureException 275-290, CaptureUnhandled 296-304, CaptureExceptionCore 306-348, CaptureMessage 354-372)
- Test/Modify: `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs` (append)

**Interfaces:**
- Consumes: `EventItem.Tags/Contexts/Extra` (Task 6.1), `ErrorItem.Contexts/Extra` (Task 6.1), `ApplyToError` (Task 6.2).
- Produces: `Scope.ApplyToEvent(EventItem)`; per-call params `contexts`/`extra` on `CaptureException` and `tags`/`contexts`/`extra` on `CaptureMessage` + `Track`. Consumed by Task 6.5 (facade).

- [ ] **Step 1: Write failing capture/track tests** — append to `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs` (before the closing brace):
```csharp
    [Fact]
    public void CapturedError_MergesPerCallOverScope_ContextsExtra()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetContext("order", new Dictionary<string, object?> { ["id"] = 1 });
        ScopeManager.Current.SetExtra("build", "scope");

        try { throw new InvalidOperationException("x"); }
        catch (Exception ex)
        {
            client.CaptureException(ex,
                contexts: new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 99 } },
                extra: new Dictionary<string, object?> { ["req"] = "call" });
        }
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal(99, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32()); // block name wins
        Assert.Equal("call", item.GetProperty("extra").GetProperty("req").GetString());                   // per-call key
        Assert.Equal("scope", item.GetProperty("extra").GetProperty("build").GetString());                // scope key kept
    }

    [Fact]
    public void CaptureMessage_CarriesPerCallMetadata()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.CaptureMessage("hi",
            tags: new Dictionary<string, object?> { ["k"] = "v" },
            contexts: new Dictionary<string, object?> { ["c"] = new Dictionary<string, object?> { ["n"] = 1 } },
            extra: new Dictionary<string, object?> { ["e"] = "x" });
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("v", item.GetProperty("tags").GetProperty("k").GetString());
        Assert.Equal(1, item.GetProperty("contexts").GetProperty("c").GetProperty("n").GetInt32());
        Assert.Equal("x", item.GetProperty("extra").GetProperty("e").GetString());
    }

    [Fact]
    public void TrackedEvent_CarriesScopeAndPerCallMetadata()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetTag("env", "prod");
        client.Track("checkout", "u_1",
            tags: new Dictionary<string, object?> { ["plan"] = "pro" },
            contexts: new Dictionary<string, object?> { ["cart"] = new Dictionary<string, object?> { ["n"] = 3 } },
            extra: new Dictionary<string, object?> { ["ab"] = "v2" });
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("event", item.GetProperty("type").GetString());
        Assert.Equal("prod", item.GetProperty("tags").GetProperty("env").GetString());  // scope
        Assert.Equal("pro", item.GetProperty("tags").GetProperty("plan").GetString());  // per-call
        Assert.Equal(3, item.GetProperty("contexts").GetProperty("cart").GetProperty("n").GetInt32());
        Assert.Equal("v2", item.GetProperty("extra").GetProperty("ab").GetString());
    }

    [Fact]
    public void TrackedEvent_OmitsMetadata_WhenNoneSet()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.Track("plain", "u_1");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.False(item.TryGetProperty("tags", out _));
        Assert.False(item.TryGetProperty("contexts", out _));
        Assert.False(item.TryGetProperty("extra", out _));
    }
```

- [ ] **Step 2: Run — expect RED (compile error)**
```
dotnet build /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.Tests/Sauron.Tests.csproj
```
Expected: `error CS1739/CS1503: ... 'contexts'` — no such named argument on `CaptureException`/`CaptureMessage`/`Track`.

- [ ] **Step 3: Add `ApplyToEvent` to `Scope`** — in `sdks/csharp/Sauron/Scope.cs`, insert immediately after `ApplyToError` (after line 73):
```csharp
    /// <summary>Merge this scope's tags/contexts/extra onto an outgoing analytics event.
    /// Per-call values already on the item win (tags/extra by key, contexts by block name).
    /// Empty results are normalized to null so they are omitted from the wire.</summary>
    public void ApplyToEvent(EventItem item)
    {
        if (Tags.Count > 0)
        {
            item.Tags ??= new Dictionary<string, object?>();
            foreach (var kv in Tags)
                item.Tags.TryAdd(kv.Key, kv.Value);
        }
        if (Contexts.Count > 0)
        {
            item.Contexts ??= new Dictionary<string, object?>();
            foreach (var kv in Contexts)
                item.Contexts.TryAdd(kv.Key, kv.Value);
        }
        if (Extra.Count > 0)
        {
            item.Extra ??= new Dictionary<string, object?>();
            foreach (var kv in Extra)
                item.Extra.TryAdd(kv.Key, kv.Value);
        }
        if (item.Tags is { Count: 0 }) item.Tags = null;
        if (item.Contexts is { Count: 0 }) item.Contexts = null;
        if (item.Extra is { Count: 0 }) item.Extra = null;
    }
```

- [ ] **Step 4: Thread per-call metadata through `Track`** — in `sdks/csharp/Sauron/SauronClient.cs`, replace `Track` (160-177):
```csharp
    /// <summary>Track a product-analytics event. <paramref name="distinctId"/> is required by the wire contract.</summary>
    public void Track(
        string @event,
        string distinctId,
        IReadOnlyDictionary<string, object?>? properties = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (string.IsNullOrEmpty(@event))
            throw new ArgumentException("event name is required.", nameof(@event));
        if (string.IsNullOrEmpty(distinctId))
            throw new ArgumentException("distinctId is required.", nameof(distinctId));

        var item = new EventItem
        {
            Name = @event,
            DistinctId = distinctId,
            Properties = properties is null ? new() : new Dictionary<string, object?>(properties),
            Timestamp = Transport.Iso8601Now(),
            Tags = tags is null || tags.Count == 0 ? null : new Dictionary<string, object?>(tags),
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
        };
        ScopeManager.Current.ApplyToEvent(item);
        Dispatch(item);
    }
```

- [ ] **Step 5: Add per-call params to `CaptureException`** — replace `CaptureException` (275-290):
```csharp
    public void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (exception is null)
            throw new ArgumentNullException(nameof(exception));

        CaptureExceptionCore(
            exception, user, level, tags, fingerprint, contexts, extra,
            mechanismType: "generic", handled: true, applySampling: true);
    }
```

- [ ] **Step 6: Update `CaptureUnhandled` + `CaptureExceptionCore`** — replace the `CaptureExceptionCore` call inside `CaptureUnhandled` (301-303):
```csharp
        CaptureExceptionCore(
            exception, user: null, level: "error", tags: null, fingerprint: null,
            contexts: null, extra: null,
            mechanismType: mechanismType, handled: false, applySampling: false);
```
Then replace the `CaptureExceptionCore` signature + `ErrorItem` construction (306-344). New signature adds `contexts`/`extra`, and the item sets them (null when empty) before `ApplyToError` fills absent keys:
```csharp
    private void CaptureExceptionCore(
        Exception exception,
        SauronUser? user,
        string level,
        IReadOnlyDictionary<string, object?>? tags,
        IReadOnlyList<string>? fingerprint,
        IReadOnlyDictionary<string, object?>? contexts,
        IReadOnlyDictionary<string, object?>? extra,
        string mechanismType,
        bool handled,
        bool applySampling)
    {
        // Error sampling (handled captures only; an uncaught crash is always kept).
        if (applySampling && _options.SampleRate < 1.0)
        {
            double roll;
            lock (Rng) { roll = Rng.NextDouble(); }
            if (roll >= _options.SampleRate)
                return;
        }

        var item = new ErrorItem
        {
            EventId = Guid.NewGuid().ToString("N"),
            Level = string.IsNullOrEmpty(level) ? "error" : level,
            Timestamp = Transport.Iso8601Now(),
            Exception = new ExceptionInfo
            {
                Type = exception.GetType().FullName ?? exception.GetType().Name,
                Value = exception.Message,
                Mechanism = new MechanismInfo
                {
                    Type = string.IsNullOrEmpty(mechanismType) ? "generic" : mechanismType,
                    Handled = handled,
                },
                Stacktrace = StackTraceExtractor.Extract(exception, _options.InAppInclude),
            },
            Tags = tags is null ? new() : new Dictionary<string, object?>(tags),
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
            Fingerprint = fingerprint is null ? null : new List<string>(fingerprint),
            User = user is null ? null : new UserInfo { Id = user.Id, Email = user.Email, Username = user.Username },
        };
        // Merge the active scope (tags/contexts/extra/user under any per-call overrides, plus breadcrumbs).
        ScopeManager.Current.ApplyToError(item);
        Dispatch(item);
    }
```

- [ ] **Step 7: Add per-call params to `CaptureMessage`** — replace `CaptureMessage` (354-372):
```csharp
    public void CaptureMessage(
        string message,
        string level = "info",
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (message is null)
            throw new ArgumentNullException(nameof(message));

        var item = new ErrorItem
        {
            EventId = Guid.NewGuid().ToString("N"),
            Level = string.IsNullOrEmpty(level) ? "info" : level,
            Timestamp = Transport.Iso8601Now(),
            Exception = null,
            Message = message,
            Tags = tags is null ? new() : new Dictionary<string, object?>(tags),
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
            Fingerprint = fingerprint is null ? null : new List<string>(fingerprint),
        };
        ScopeManager.Current.ApplyToError(item);
        Dispatch(item);
    }
```

- [ ] **Step 8: Run — expect GREEN**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx
```
Expected: `Passed!  - Failed: 0` (new MetadataScopeTests pass; existing `CapturedError_LiftsScopedBreadcrumbsTagsUser_AndHonorsFingerprintOverride` and golden tests unchanged — contexts/extra stay null/omitted when unset).

- [ ] **Step 9: Commit**
```
git add sdks/csharp/Sauron/Scope.cs sdks/csharp/Sauron/SauronClient.cs sdks/csharp/Sauron.Tests/MetadataScopeTests.cs
git commit -m "feat(sdk-dotnet): per-call tags/contexts/extra on captureException/captureMessage/track" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6.5: Forward metadata-scope options through the `SauronSdk` static facade

**Files:**
- Modify: `sdks/csharp/Sauron/SauronSdk.cs` (Track 40-41, CaptureException 44-50, CaptureMessage 53-54)
- Test/Modify: `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs` (append)

**Interfaces:**
- Consumes: client `Track`/`CaptureException`/`CaptureMessage` new signatures (Task 6.4); `SauronOptions.HttpMessageHandler`/`FlushInterval` seams; `SauronSdk.Init`/`Flush`/`Close`.
- Produces: static facade parity — `SauronSdk.CaptureException(... contexts, extra)`, `SauronSdk.CaptureMessage(... tags, contexts, extra)`, `SauronSdk.Track(... tags, contexts, extra)`.

- [ ] **Step 1: Write failing facade test** — append to `sdks/csharp/Sauron.Tests/MetadataScopeTests.cs` (before the closing brace):
```csharp
    [Fact]
    public void Facade_ForwardsPerCallMetadata_ThroughInitializedClient()
    {
        var handler = new CapturingHandler();
        SauronSdk.Init(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 1000,
        });
        try
        {
            SauronSdk.CaptureMessage("hi",
                contexts: new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
                extra: new Dictionary<string, object?> { ["e"] = "x" });
            SauronSdk.Flush();

            var item = TestUtil.FirstItem(handler.LastBody!);
            Assert.Equal(7, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32());
            Assert.Equal("x", item.GetProperty("extra").GetProperty("e").GetString());
        }
        finally
        {
            SauronSdk.Close();
        }
    }
```

- [ ] **Step 2: Run — expect RED (compile error)**
```
dotnet build /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.Tests/Sauron.Tests.csproj
```
Expected: `error CS1739: ... 'contexts'` — the facade `CaptureMessage` overload has no such parameter.

- [ ] **Step 3: Update the facade forwards** — in `sdks/csharp/Sauron/SauronSdk.cs`, replace `Track` (40-41):
```csharp
    /// <summary>Track a product-analytics event. <paramref name="distinctId"/> is required.</summary>
    public static void Track(
        string @event,
        string distinctId,
        IReadOnlyDictionary<string, object?>? properties = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
        => Current?.Track(@event, distinctId, properties, tags, contexts, extra);
```
replace `CaptureException` (44-50):
```csharp
    /// <summary>Capture a native exception. <paramref name="fingerprint"/> is an optional grouping override.</summary>
    public static void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
        => Current?.CaptureException(exception, user, level, tags, fingerprint, contexts, extra);
```
and replace `CaptureMessage` (53-54):
```csharp
    /// <summary>Capture a plain message (default level <c>info</c>). <paramref name="fingerprint"/> is an optional grouping override.</summary>
    public static void CaptureMessage(
        string message,
        string level = "info",
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
        => Current?.CaptureMessage(message, level, fingerprint, tags, contexts, extra);
```

- [ ] **Step 4: Run full suite — expect GREEN**
```
dotnet test /home/splimter/projects/freelance/sauron/sdks/csharp/Sauron.slnx
```
Expected: `Passed!  - Failed: 0` across the whole `.slnx` (facade test + all prior tasks + unchanged golden/scope/transport suites).

- [ ] **Step 5: Commit**
```
git add sdks/csharp/Sauron/SauronSdk.cs sdks/csharp/Sauron.Tests/MetadataScopeTests.cs
git commit -m "feat(sdk-dotnet): forward metadata-scope options through the SauronSdk facade" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```



## Slice 7 — Flutter SDK

> Slice scope note: `sauron_flutter` has **no `captureMessage`** API (verified: `grep captureMessage lib/` is empty). MESSAGES coverage doesn't apply here — this slice wires tags/contexts/extra into `captureException` (errors) and `track` (analytics) only. `track`'s per-call `properties`/`screen` params already exist and stay; we add `tags`/`contexts`/`extra` alongside. `Scope` is internal (not in the export barrel), so its unit test imports `package:sauron_flutter/src/scope.dart` directly.

### Task 7.1: ErrorItem + EventItem carry tags/contexts/extra (omit-when-empty) + golden update

**Files:**
- Modify: `sdks/flutter/lib/src/envelope.dart` (ErrorItem ctor/fields/toJson ~104-161, EventItem ctor/fields/toJson ~164-198)
- Test: `sdks/flutter/test/envelope_test.dart` (`_golden` ~15-45, error builder ~54-83, event builder ~85-91, add omit test)

**Interfaces:**
- Consumes: wire contract — ErrorItem gains `tags`/`contexts`/`extra`; EventItem gains `tags`/`contexts`/`extra`; empty maps OMITTED (no `{}`), non-empty under keys `tags`/`contexts`/`extra`.
- Produces: `ErrorItem({... Map<String,String> tags, Map<String,Map<String,Object?>> contexts, Map<String,Object?> extra})` and identical trio on `EventItem`, each defaulting to `const {}` — consumed by Task 7.3's client merge.

- [ ] **Step 1: Update the golden test to exercise the new fields (failing).** In `test/envelope_test.dart`, edit the `_golden` error item (line ~37) to append the three keys, and the event item (line ~38):

```dart
      "fingerprint": null, "session_id": "$_sessionId", "screen": null,
      "tags": { "feature": "checkout" }, "contexts": { "order": { "id": 7 } }, "extra": { "attempt": 2 } },
    { "type": "event", "name": "checkout_completed", "distinct_id": "u_123", "timestamp": "2026-07-12T10:29:40.000Z", "properties": { "cart_value": 42.5 }, "session_id": "$_sessionId", "screen": null,
      "tags": { "plan": "pro" }, "contexts": { "cart": { "items": 3 } }, "extra": { "coupon": "SAVE10" } },
```

Then pass them in the builders. On the `ErrorItem(` (after `sessionId: _sessionId,` line ~82) add:

```dart
        tags: const <String, String>{'feature': 'checkout'},
        contexts: const <String, Map<String, Object?>>{
          'order': <String, Object?>{'id': 7},
        },
        extra: const <String, Object?>{'attempt': 2},
```

On the `EventItem(` (after `sessionId: _sessionId,` line ~90) add:

```dart
        tags: const <String, String>{'plan': 'pro'},
        contexts: const <String, Map<String, Object?>>{
          'cart': <String, Object?>{'items': 3},
        },
        extra: const <String, Object?>{'coupon': 'SAVE10'},
```

Then add an omit-when-empty test inside `group('Envelope golden shape', ...)` (after the transaction test, ~line 205):

```dart
    test('error/event omit tags/contexts/extra when the maps are empty', () {
      final ErrorItem error = ErrorItem(
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 58, 900),
        exception: const SauronException(
          type: 'StateError',
          value: 'boom',
          mechanism: Mechanism(type: 'manual', handled: true),
          stacktrace: <StackFrame>[],
        ),
      );
      final EventItem event = EventItem(
        name: 'tapped',
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 40),
      );
      final Map<String, dynamic> errJson =
          jsonDecode(jsonEncode(error.toJson())) as Map<String, dynamic>;
      final Map<String, dynamic> evtJson =
          jsonDecode(jsonEncode(event.toJson())) as Map<String, dynamic>;
      for (final String key in <String>['tags', 'contexts', 'extra']) {
        expect(errJson.containsKey(key), isFalse, reason: 'error.$key');
        expect(evtJson.containsKey(key), isFalse, reason: 'event.$key');
      }
    });
```

- [ ] **Step 2: Run the test — expect FAILURE (compile error).**

```
cd sdks/flutter && flutter test test/envelope_test.dart
```

Expected: compile failure — `Error: No named parameter with the name 'tags'.` (ErrorItem/EventItem don't accept the trio yet).

- [ ] **Step 3: Add the fields to `ErrorItem` in `lib/src/envelope.dart`.** Extend the constructor (add after `this.debugMeta,`):

```dart
    this.debugMeta,
    this.tags = const <String, String>{},
    this.contexts = const <String, Map<String, Object?>>{},
    this.extra = const <String, Object?>{},
  });
```

Add the fields after the `debugMeta` field (~line 135):

```dart
  final DebugMeta? debugMeta;

  /// Developer-attached flat tags (string->string). Omitted from the wire when
  /// empty. Distinct from breadcrumbs and the machine-owned `context`.
  final Map<String, String> tags;

  /// Developer-attached structured contexts (name -> block). Omitted when empty.
  final Map<String, Map<String, Object?>> contexts;

  /// Developer-attached freeform extra (JSON). Omitted when empty.
  final Map<String, Object?> extra;
```

In `ErrorItem.toJson`, insert the omit-when-empty writes just before `return json;` (~line 158):

```dart
    if (tags.isNotEmpty) {
      json['tags'] = tags;
    }
    if (contexts.isNotEmpty) {
      json['contexts'] = contexts;
    }
    if (extra.isNotEmpty) {
      json['extra'] = extra;
    }
    return json;
```

- [ ] **Step 4: Add the fields to `EventItem` and rewrite its `toJson`.** Extend the constructor (~line 165) — add the trio and initialize maps to empty when null:

```dart
  EventItem({
    required this.name,
    required this.timestamp,
    this.distinctId,
    this.sessionId,
    this.screen,
    Map<String, Object?>? properties,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  })  : properties = properties ?? const <String, Object?>{},
        tags = tags ?? const <String, String>{},
        contexts = contexts ?? const <String, Map<String, Object?>>{},
        extra = extra ?? const <String, Object?>{};
```

Add the fields after `final Map<String, Object?> properties;` (~line 183):

```dart
  final Map<String, Object?> properties;

  /// Developer-attached flat tags (string->string). Omitted when empty.
  final Map<String, String> tags;

  /// Developer-attached structured contexts (name -> block). Omitted when empty.
  final Map<String, Map<String, Object?>> contexts;

  /// Developer-attached freeform extra (JSON). Omitted when empty.
  final Map<String, Object?> extra;
```

Replace the map-literal `toJson` (~line 188-197) with a mutable-map form:

```dart
  @override
  Map<String, Object?> toJson() {
    final Map<String, Object?> json = <String, Object?>{
      'type': type,
      'name': name,
      'distinct_id': distinctId,
      'timestamp': sauronIso(timestamp),
      'properties': properties,
      'session_id': sessionId,
      'screen': screen,
    };
    if (tags.isNotEmpty) {
      json['tags'] = tags;
    }
    if (contexts.isNotEmpty) {
      json['contexts'] = contexts;
    }
    if (extra.isNotEmpty) {
      json['extra'] = extra;
    }
    return json;
  }
```

- [ ] **Step 5: Run the test — expect PASS.**

```
cd sdks/flutter && flutter test test/envelope_test.dart
```

Expected: `All tests passed!` (golden equality + omit-when-empty both green).

- [ ] **Step 6: Commit.**

```
git add sdks/flutter/lib/src/envelope.dart sdks/flutter/test/envelope_test.dart
git commit -m "feat(flutter): carry tags/contexts/extra on ErrorItem/EventItem (omit when empty)"
```

---

### Task 7.2: Scope gains tags/contexts/extra state + setters

**Files:**
- Modify: `sdks/flutter/lib/src/scope.dart` (whole file — add fields + setters after `clearBreadcrumbs`, ~line 37)
- Test: `sdks/flutter/test/scope_test.dart` (NEW)

**Interfaces:**
- Consumes: nothing new (pure in-memory state).
- Produces: `Scope.tags` (`Map<String,String>`), `Scope.contexts` (`Map<String,Map<String,Object?>>`), `Scope.extra` (`Map<String,Object?>`), and `setTag(String,String)`, `setTags(Map<String,String>)`, `setContext(String,Map<String,Object?>)`, `setExtra(String,Object?)` — consumed by Task 7.3's client seeding/merge.

- [ ] **Step 1: Write the failing Scope unit test.** Create `test/scope_test.dart` (imports the internal `src/scope.dart` since `Scope` is not exported):

```dart
import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/src/scope.dart';

void main() {
  group('Scope metadata', () {
    test('setters accumulate tags/contexts/extra', () {
      final Scope scope = Scope();
      scope.setTag('a', '1');
      scope.setTags(<String, String>{'b': '2', 'c': '3'});
      scope.setContext('order', <String, Object?>{'id': 7});
      scope.setExtra('flag', true);

      expect(scope.tags, <String, String>{'a': '1', 'b': '2', 'c': '3'});
      expect(scope.contexts,
          <String, Map<String, Object?>>{'order': <String, Object?>{'id': 7}});
      expect(scope.extra, <String, Object?>{'flag': true});
    });

    test('setTag is last-write-wins per key', () {
      final Scope scope = Scope();
      scope.setTag('env', 'seed');
      scope.setTag('env', 'runtime');
      expect(scope.tags['env'], 'runtime');
    });

    test('setContext replaces the whole block by name', () {
      final Scope scope = Scope();
      scope.setContext('order', <String, Object?>{'id': 1, 'total': 10});
      scope.setContext('order', <String, Object?>{'id': 2});
      expect(scope.contexts['order'], <String, Object?>{'id': 2});
    });

    test('fresh scope has empty metadata maps', () {
      final Scope scope = Scope();
      expect(scope.tags, isEmpty);
      expect(scope.contexts, isEmpty);
      expect(scope.extra, isEmpty);
    });
  });
}
```

- [ ] **Step 2: Run the test — expect FAILURE (compile error).**

```
cd sdks/flutter && flutter test test/scope_test.dart
```

Expected: compile failure — `The method 'setTag' isn't defined for the type 'Scope'.`

- [ ] **Step 3: Add the fields and setters to `lib/src/scope.dart`.** Insert after `clearBreadcrumbs()` (line 37), before the closing brace:

```dart
  /// Clears all breadcrumbs.
  void clearBreadcrumbs() => _breadcrumbs.clear();

  /// Developer-attached flat tags (string->string), seeded from init options
  /// and mutated by [setTag]/[setTags]. Merged under per-call tags on capture.
  final Map<String, String> tags = <String, String>{};

  /// Developer-attached structured contexts (name -> block). Distinct from the
  /// machine-owned device/os/app/runtime context.
  final Map<String, Map<String, Object?>> contexts =
      <String, Map<String, Object?>>{};

  /// Developer-attached freeform extra (JSON).
  final Map<String, Object?> extra = <String, Object?>{};

  /// Sets a single tag (last-write-wins by key).
  void setTag(String key, String value) => tags[key] = value;

  /// Merges the given tags into the scope (last-write-wins by key).
  void setTags(Map<String, String> values) => tags.addAll(values);

  /// Sets (replaces) a named context block.
  void setContext(String name, Map<String, Object?> block) =>
      contexts[name] = block;

  /// Sets a single extra value (last-write-wins by key).
  void setExtra(String key, Object? value) => extra[key] = value;
}
```

- [ ] **Step 4: Run the test — expect PASS.**

```
cd sdks/flutter && flutter test test/scope_test.dart
```

Expected: `All tests passed!`

- [ ] **Step 5: Commit.**

```
git add sdks/flutter/lib/src/scope.dart sdks/flutter/test/scope_test.dart
git commit -m "feat(flutter): add tags/contexts/extra state + setters to Scope"
```

---

### Task 7.3: SauronOptions defaults + client seeding, merge, capture params, and client setters

**Files:**
- Modify: `sdks/flutter/lib/src/sauron_options.dart` (add fields after `maxBreadcrumbs`, ~line 37)
- Modify: `sdks/flutter/lib/src/client.dart` (ctor seeding ~30, `captureException` ~126-166, `track` ~169-183, add setters + merge helpers)
- Test: `sdks/flutter/test/scope_metadata_test.dart` (NEW — mock-http harness mirroring `screen_test.dart`)

**Interfaces:**
- Consumes: `ErrorItem`/`EventItem` trio params (Task 7.1); `Scope.tags`/`contexts`/`extra` + setters (Task 7.2).
- Produces: `SauronOptions.tags`/`contexts`/`extra`; `SauronClient.captureException(..., {Map<String,String>? tags, Map<String,Map<String,Object?>>? contexts, Map<String,Object?>? extra})`; `SauronClient.track(name, {..., tags, contexts, extra})`; `SauronClient.setTag/setTags/setContext/setExtra` — consumed by Task 7.4's facade.

- [ ] **Step 1: Write the failing client integration test.** Create `test/scope_metadata_test.dart`:

```dart
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// Drives the client directly (as the other client tests do), capturing posted
/// envelope bodies via a mock HTTP client, and asserts the SDK-side merge of
/// init-default scope + runtime setters + per-call overrides on error/event.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_scope_meta_test');
    httpClient = _MockClient();
    items.clear();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((Invocation invocation) async {
      final Object? body = invocation.namedArguments[const Symbol('body')];
      final List<int> bytes =
          body is String ? utf8.encode(body) : body as List<int>;
      final Map<String, dynamic> env =
          jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>;
      for (final dynamic item in env['items'] as List<dynamic>) {
        items.add((item as Map<String, dynamic>).cast<String, Object?>());
      }
      return http.Response('', 202);
    });
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  Future<SauronClient> buildClient({bool seed = true}) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      // Never gzip in tests so the posted body is plain JSON.
      ..gzipThresholdBytes = 1 << 30;
    if (seed) {
      options
        ..tags = <String, String>{'env_tag': 'seed'}
        ..contexts = <String, Map<String, Object?>>{
          'order': <String, Object?>{'id': 1},
        }
        ..extra = <String, Object?>{'boot': true};
    }
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> events() =>
      items.where((Map<String, Object?> i) => i['type'] == 'event').toList();

  Future<Map<String, Object?>> errorAfter(
      SauronClient client, Future<void> Function() act) async {
    await act();
    // captureException fires its own unawaited flush; let it settle, then flush.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();
    return items.firstWhere((Map<String, Object?> i) => i['type'] == 'error');
  }

  test('init defaults seed the scope and are emitted on track', () async {
    final SauronClient client = await buildClient();
    client.track('viewed');
    await client.flush();
    await client.close();

    final Map<String, Object?> viewed =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'viewed');
    expect(viewed['tags'], <String, Object?>{'env_tag': 'seed'});
    expect(viewed['contexts'],
        <String, Object?>{'order': <String, Object?>{'id': 1}});
    expect(viewed['extra'], <String, Object?>{'boot': true});
  });

  test('runtime setters + per-call override merge per top-level key', () async {
    final SauronClient client = await buildClient();
    client.setTag('env_tag', 'runtime'); // overrides the seed
    client.setTag('extra_tag', 'x');
    client.setContext('cart', <String, Object?>{'items': 2});
    client.setExtra('flag', 'on');
    // Per-call: tag env_tag wins by key; contexts.order block replaced.
    client.track(
      'checkout',
      tags: <String, String>{'env_tag': 'call'},
      contexts: <String, Map<String, Object?>>{
        'order': <String, Object?>{'id': 99},
      },
    );
    await client.flush();
    await client.close();

    final Map<String, Object?> checkout = events()
        .firstWhere((Map<String, Object?> e) => e['name'] == 'checkout');
    expect(checkout['tags'],
        <String, Object?>{'env_tag': 'call', 'extra_tag': 'x'});
    expect(checkout['contexts'], <String, Object?>{
      'order': <String, Object?>{'id': 99},
      'cart': <String, Object?>{'items': 2},
    });
    expect(checkout['extra'], <String, Object?>{'boot': true, 'flag': 'on'});
  });

  test('captureException merges scope + per-call tags/extra', () async {
    final SauronClient client = await buildClient();
    client.setTag('feature', 'checkout');
    final Map<String, Object?> error = await errorAfter(client, () async {
      client.captureException(
        StateError('boom'),
        tags: <String, String>{'severity': 'high'},
        extra: <String, Object?>{'retries': 3},
      );
    });
    expect(error['tags'], <String, Object?>{
      'env_tag': 'seed',
      'feature': 'checkout',
      'severity': 'high',
    });
    expect(error['contexts'],
        <String, Object?>{'order': <String, Object?>{'id': 1}});
    expect(error['extra'], <String, Object?>{'boot': true, 'retries': 3});
  });

  test('no scope + no per-call metadata omits the keys', () async {
    final SauronClient client = await buildClient(seed: false);
    client.track('bare');
    await client.flush();
    await client.close();

    final Map<String, Object?> bare =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'bare');
    expect(bare.containsKey('tags'), isFalse);
    expect(bare.containsKey('contexts'), isFalse);
    expect(bare.containsKey('extra'), isFalse);
  });
}
```

- [ ] **Step 2: Run the test — expect FAILURE (compile error).**

```
cd sdks/flutter && flutter test test/scope_metadata_test.dart
```

Expected: compile failure — `The setter 'tags' isn't defined for the type 'SauronOptions'.` / `No named parameter with the name 'tags'.` / `The method 'setTag' isn't defined for the type 'SauronClient'.`

- [ ] **Step 3: Add the default fields to `lib/src/sauron_options.dart`.** Insert after `int maxBreadcrumbs = 100;` (line 37):

```dart
  /// Maximum breadcrumbs retained per scope.
  int maxBreadcrumbs = 100;

  /// Default tags (string->string) seeded into the client's global scope at
  /// init. Per-call tags override these by key on each capture.
  Map<String, String> tags = <String, String>{};

  /// Default contexts (name -> structured block) seeded into the global scope.
  /// Distinct from the machine-owned device/os/app/runtime `context`.
  Map<String, Map<String, Object?>> contexts = <String, Map<String, Object?>>{};

  /// Default extra (freeform JSON) seeded into the global scope.
  Map<String, Object?> extra = <String, Object?>{};
```

- [ ] **Step 4: Seed the scope in the client constructor.** In `lib/src/client.dart`, extend the ctor body after `_currentScreen = options.screen;` (line 30):

```dart
    _currentScreen = options.screen;
    _scope.setTags(options.tags);
    _scope.contexts.addAll(options.contexts);
    _scope.extra.addAll(options.extra);
```

- [ ] **Step 5: Add the merge helpers and public setters to the client.** In the `// ---- internals` section of `lib/src/client.dart`, before `_dispatch` (line ~270), add the merge helpers:

```dart
  /// Effective tags = scope (init defaults + runtime setters) then per-call,
  /// last-write-wins by key. Empty result is omitted on the wire.
  Map<String, String> _mergeTags(Map<String, String>? call) =>
      <String, String>{..._scope.tags, if (call != null) ...call};

  /// Effective contexts merge by BLOCK NAME — a per-call block replaces the
  /// same-named scope block.
  Map<String, Map<String, Object?>> _mergeContexts(
          Map<String, Map<String, Object?>>? call) =>
      <String, Map<String, Object?>>{
        ..._scope.contexts,
        if (call != null) ...call,
      };

  /// Effective extra = scope then per-call, shallow last-write-wins by key.
  Map<String, Object?> _mergeExtra(Map<String, Object?>? call) =>
      <String, Object?>{..._scope.extra, if (call != null) ...call};
```

Add the public setters alongside `setUser` (after line 249):

```dart
  /// Sets (or clears) the current user.
  void setUser(SauronUser? user) => _scope.user = user;

  /// Sets a single scope tag (last-write-wins by key).
  void setTag(String key, String value) => _scope.setTag(key, value);

  /// Merges scope tags (last-write-wins by key).
  void setTags(Map<String, String> values) => _scope.setTags(values);

  /// Sets (replaces) a named scope context block.
  void setContext(String name, Map<String, Object?> block) =>
      _scope.setContext(name, block);

  /// Sets a single scope extra value (last-write-wins by key).
  void setExtra(String key, Object? value) => _scope.setExtra(key, value);
```

- [ ] **Step 6: Add capture params and attach merged metadata.** In `captureException`, extend the signature (line 126-132):

```dart
  void captureException(
    Object error, {
    StackTrace? stackTrace,
    Mechanism? mechanism,
    SauronLevel level = SauronLevel.error,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) {
```

and extend the `ErrorItem(` construction (line 152-161) with the merged trio:

```dart
    final ErrorItem item = ErrorItem(
      exception: exception,
      timestamp: DateTime.now().toUtc(),
      level: level,
      breadcrumbs: _scope.breadcrumbs,
      sessionId: sessionId,
      screen: screen ?? _currentScreen,
      rawStacktrace: obfuscated ? rawTrace : null,
      debugMeta: obfuscated ? DebugMeta.fromTrace(rawTrace) : null,
      tags: _mergeTags(tags),
      contexts: _mergeContexts(contexts),
      extra: _mergeExtra(extra),
    );
```

In `track`, extend the signature (line 169) and the `EventItem(` construction (line 174-181):

```dart
  void track(
    String name, {
    Map<String, Object?>? properties,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) {
    if (!isEnabled) {
      return;
    }
    _dispatch(
      EventItem(
        name: name,
        timestamp: DateTime.now().toUtc(),
        distinctId: _scope.distinctId,
        sessionId: sessionId,
        screen: screen ?? _currentScreen,
        properties: properties,
        tags: _mergeTags(tags),
        contexts: _mergeContexts(contexts),
        extra: _mergeExtra(extra),
      ),
    );
  }
```

> Note: `setScreen` calls `track(r'$screen', ...)` with no metadata — it now inherits scope defaults via the merge, matching the parity intent (analytics events carry scope metadata). No change needed there.

- [ ] **Step 7: Run the new test + the existing client-driven suites — expect PASS.**

```
cd sdks/flutter && flutter test test/scope_metadata_test.dart test/screen_test.dart test/before_send_test.dart
```

Expected: `All tests passed!` (merge/seed/omit assertions green; screen + beforeSend suites unaffected — `EventItem` replacement in `before_send_test.dart` still compiles since the trio is optional).

- [ ] **Step 8: Commit.**

```
git add sdks/flutter/lib/src/sauron_options.dart sdks/flutter/lib/src/client.dart sdks/flutter/test/scope_metadata_test.dart
git commit -m "feat(flutter): seed scope from options + merge tags/contexts/extra on capture/track"
```

---

### Task 7.4: Facade — setters + capture params, and fix dropped level/screen

**Files:**
- Modify: `sdks/flutter/lib/src/sauron.dart` (`captureException` ~55-65, `track` ~68-69, add setters after `setUser` ~118)

**Interfaces:**
- Consumes: `SauronClient.captureException(..., {level, screen, tags, contexts, extra})`, `SauronClient.track(name, {properties, tags, contexts, extra})`, `SauronClient.setTag/setTags/setContext/setExtra` (Task 7.3).
- Produces: static facade parity — `Sauron.captureException`/`Sauron.track` accept the trio (+ `level`/`screen` now forwarded), and `Sauron.setTag/setTags/setContext/setExtra`.

- [ ] **Step 1: Replace the facade `captureException` to forward `level`, `screen`, and the metadata trio.** In `lib/src/sauron.dart` (lines 55-65):

```dart
  /// Captures an exception manually.
  static void captureException(
    Object error, {
    StackTrace? stackTrace,
    Mechanism? mechanism,
    SauronLevel level = SauronLevel.error,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) =>
      _client?.captureException(
        error,
        stackTrace: stackTrace,
        mechanism: mechanism,
        level: level,
        screen: screen,
        tags: tags,
        contexts: contexts,
        extra: extra,
      );
```

> This fixes the pre-existing bug where the facade dropped `level`/`screen` (the client accepted them but the static entry point never forwarded them). `SauronLevel` is already imported via `types.dart` (line 9).

- [ ] **Step 2: Replace the facade `track` to forward the metadata trio.** In `lib/src/sauron.dart` (lines 68-69):

```dart
  /// Records a product-analytics event.
  static void track(
    String name, {
    Map<String, Object?>? properties,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) =>
      _client?.track(
        name,
        properties: properties,
        tags: tags,
        contexts: contexts,
        extra: extra,
      );
```

- [ ] **Step 3: Add the facade scope setters.** After the `setUser` forwarder (line 118):

```dart
  /// Sets (or clears) the current user.
  static void setUser(SauronUser? user) => _client?.setUser(user);

  /// Sets a single scope tag (last-write-wins by key).
  static void setTag(String key, String value) =>
      _client?.setTag(key, value);

  /// Merges scope tags (last-write-wins by key).
  static void setTags(Map<String, String> values) =>
      _client?.setTags(values);

  /// Sets (replaces) a named scope context block.
  static void setContext(String name, Map<String, Object?> block) =>
      _client?.setContext(name, block);

  /// Sets a single scope extra value (last-write-wins by key).
  static void setExtra(String key, Object? value) =>
      _client?.setExtra(key, value);
```

- [ ] **Step 4: Analyze + run the full suite — expect clean + PASS.** The facade is thin static delegation (no facade unit tests exist in this SDK — it wraps global `_client` state that requires `Sauron.init`); `flutter analyze` proves the forwarders match the Task 7.3 client signatures, and the full suite confirms nothing regressed.

```
cd sdks/flutter && flutter analyze && flutter test
```

Expected: `No issues found!` from analyze, then `All tests passed!` for the whole `test/` directory (envelope, scope, scope_metadata, screen, before_send, and the untouched dart_symbolication/queue/stacktrace/transport suites).

- [ ] **Step 5: Commit.**

```
git add sdks/flutter/lib/src/sauron.dart
git commit -m "feat(flutter): expose setTag/setTags/setContext/setExtra + tags/contexts/extra on facade; forward level/screen"
```

