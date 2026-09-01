# Source Maps — Slice 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up artifact storage, upload/list/delete API, `artifact:write` RBAC, and the `sauron-symbols` crate's storage+cache primitives — so maps/symbols can be uploaded, deduped, and served. No symbolication yet (slices 2 & 3).

**Architecture:** A new `sauron-symbols` crate holds storage-agnostic primitives (content-addressing, zstd, byte-bounded LRU with single-flight). `sauron-db` gains `symbol_blobs` (content-addressed, refcounted) + `symbol_artifacts` tables and repo fns. `sauron-api` gets multipart upload/list/delete endpoints gated by a new `artifact:write` permission. An isolated Redis warm-blob cache lives behind a feature-flagged env. A small CLI wraps the upload API.

**Tech Stack:** Rust, axum 0.8 (multipart via `axum-extra`/`Multipart`), diesel + diesel-async (Postgres, `postgres_backend`), `zstd`, `sha2`, an LRU (hand-rolled or `lru`), `moka` NOT required, `tokio`.

## Global Constraints

- Edition 2021, `rust-version = 1.82`, license `AGPL-3.0-only` (copy `license.workspace = true`).
- diesel uses `postgres_backend` (no libpq); all I/O via diesel-async/deadpool.
- Never auto-commit — user runs on local `main` only and commits manually. The `git commit` steps below are written for completeness; **skip them unless the user explicitly asks to commit** (leave changes in the working tree).
- No DB/handler integration-test harness exists — pure logic gets Rust unit tests; DB/API/CLI get `docker compose` e2e verification.
- Migrations are `backend/migrations/YYYY-MM-DD-NNNNNN_name/{up.sql,down.sql}`; next index is `000014`.

---

### Task 1: `artifact:write` permission

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs` (perm mod ~L25-64; PRESETS perm sets ~L380-428)
- Test: same file's `#[cfg(test)] mod tests` (existing `every_preset_permission_is_a_known_permission`, `preset_names_are_unique` already guard consistency)

**Interfaces:**
- Produces: `perm::ARTIFACT_WRITE: &str = "artifact:write"`; present in `perm::ALL` (len 19 → 20) and in Owner/Admin/Developer preset permission arrays.

- [ ] **Step 1: Add the constant + extend `ALL`**

In `pub mod perm` add:
```rust
    pub const ARTIFACT_WRITE: &str = "artifact:write";
```
Change `pub const ALL: [&str; 19]` to `[&str; 20]` and append `ARTIFACT_WRITE,` to the array (canonical order: after `FUNNEL_WRITE` is fine, but keep the array length in sync).

- [ ] **Step 2: Grant it in presets**

Find the OWNER/ADMIN/DEVELOPER `PresetRole { permissions: &[...] }` arrays. Add `perm::ARTIFACT_WRITE` to Owner, Admin, and Developer permission slices (NOT Viewer). Mirror exactly how `FUNNEL_WRITE` is placed.

- [ ] **Step 3: Run auth unit tests**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS — `every_preset_permission_is_a_known_permission` confirms the new preset perm is in `ALL`; `ALL` length assertion (if any) matches 20.

- [ ] **Step 4: Commit** *(skip unless asked)*

```bash
git add backend/crates/sauron-auth/src/rbac.rs
git commit -m "feat(symbols): add artifact:write permission to RBAC presets"
```

---

### Task 2: Migration `000014_symbol_artifacts`

**Files:**
- Create: `backend/migrations/2026-07-15-000014_symbol_artifacts/up.sql`
- Create: `backend/migrations/2026-07-15-000014_symbol_artifacts/down.sql`

**Interfaces:**
- Produces tables `symbol_blobs`, `symbol_artifacts`; columns `error_events.stacktrace_symbolicated`, `.symbolication_status`, `.debug_meta`. Adds `artifact:write` to seeded preset roles' `permissions` jsonb so DB-seeded roles match code presets.

- [ ] **Step 1: Write `up.sql`**

```sql
-- 0014: source-map / symbol artifact storage (content-addressed) + symbolication
-- columns on error_events. Presentational symbolication; grouping unaffected.

CREATE TABLE symbol_blobs (
    sha256            BYTEA PRIMARY KEY,
    content           BYTEA NOT NULL,
    uncompressed_size BIGINT NOT NULL,
    compressed_size   BIGINT NOT NULL,
    refcount          INTEGER NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- content is pre-compressed with zstd; don't let TOAST re-compress it.
ALTER TABLE symbol_blobs ALTER COLUMN content SET STORAGE EXTERNAL;

CREATE TABLE symbol_artifacts (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id                UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    kind                  TEXT NOT NULL,
    platform              TEXT NOT NULL,
    arch                  TEXT,
    release               TEXT,
    dist                  TEXT,
    name                  TEXT,
    debug_id              TEXT,
    blob_sha256           BYTEA NOT NULL REFERENCES symbol_blobs(sha256),
    prebuilt_index_sha256 BYTEA REFERENCES symbol_blobs(sha256),
    uploaded_by           UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX symbol_artifacts_debugid_idx
    ON symbol_artifacts (app_id, debug_id) WHERE debug_id IS NOT NULL;
CREATE INDEX symbol_artifacts_release_name_idx
    ON symbol_artifacts (app_id, release, name);

ALTER TABLE error_events ADD COLUMN stacktrace_symbolicated JSONB;
ALTER TABLE error_events ADD COLUMN symbolication_status TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE error_events ADD COLUMN debug_meta JSONB;

-- Keep DB-seeded preset roles in sync with code presets (ensure_preset_roles also
-- syncs at startup, but seed here so a fresh DB is correct immediately).
UPDATE roles SET permissions = permissions || '["artifact:write"]'::jsonb
WHERE name IN ('Owner','Admin','Developer')
  AND NOT (permissions @> '["artifact:write"]'::jsonb);
```

- [ ] **Step 2: Write `down.sql`**

```sql
ALTER TABLE error_events DROP COLUMN IF EXISTS debug_meta;
ALTER TABLE error_events DROP COLUMN IF EXISTS symbolication_status;
ALTER TABLE error_events DROP COLUMN IF EXISTS stacktrace_symbolicated;
DROP TABLE IF EXISTS symbol_artifacts;
DROP TABLE IF EXISTS symbol_blobs;
UPDATE roles SET permissions = permissions - 'artifact:write'
WHERE name IN ('Owner','Admin','Developer');
```

- [ ] **Step 3: Apply + verify**

Run: `cd backend && docker compose up -d db && sleep 3 && cargo run -p sauron-migrate` (or the project's migrate bin). Then verify with psql:
```
\d symbol_blobs
\d symbol_artifacts
\d+ error_events   -- shows the 3 new columns
```
Expected: tables + columns exist; `error_events` still partitioned.

- [ ] **Step 4: Commit** *(skip unless asked)*

```bash
git add backend/migrations/2026-07-15-000014_symbol_artifacts
git commit -m "feat(symbols): migration for symbol_blobs/symbol_artifacts + error_events columns"
```

---

### Task 3: `sauron-symbols` crate — storage-agnostic primitives

**Files:**
- Create: `backend/crates/sauron-symbols/Cargo.toml`
- Create: `backend/crates/sauron-symbols/src/lib.rs`
- Create: `backend/crates/sauron-symbols/src/content.rs` (sha256 + zstd + caps)
- Create: `backend/crates/sauron-symbols/src/cache.rs` (byte-bounded LRU + single-flight)
- Modify: `backend/Cargo.toml` (workspace dep entry)

**Interfaces:**
- Produces:
  - `content::sha256(bytes: &[u8]) -> [u8; 32]`
  - `content::compress(raw: &[u8]) -> Vec<u8>` (zstd level 19)
  - `content::decompress(comp: &[u8], max_uncompressed: usize) -> Result<Vec<u8>, SymbolError>` (bomb-guarded)
  - `cache::ByteLru<K, V>` with `get_or_try_insert_with(key, weight_fn, || Fut) -> Arc<V>` semantics + byte budget eviction. `V` wrapped in `Arc`.
  - `SymbolError` enum (`TooLarge`, `Decompress`, `Corrupt`, ...).

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "sauron-symbols"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
sha2 = "0.10"
zstd = "0.13"
thiserror = "1"
tokio = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
```
And in `backend/Cargo.toml` `[workspace.dependencies]` add:
```toml
sauron-symbols = { path = "crates/sauron-symbols" }
```

- [ ] **Step 2: Write failing test for content-addressing + zstd roundtrip**

`content.rs` bottom:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable_and_hex() {
        let h = sha256(b"hello");
        assert_eq!(hex(&h), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn zstd_roundtrips_under_cap() {
        let raw = b"the quick brown fox".repeat(100);
        let comp = compress(&raw);
        assert!(comp.len() < raw.len());
        let back = decompress(&comp, 1 << 20).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn decompress_rejects_bomb() {
        let raw = vec![0u8; 4 << 20]; // 4 MiB of zeros -> tiny compressed
        let comp = compress(&raw);
        let err = decompress(&comp, 1 << 20).unwrap_err(); // cap 1 MiB
        assert!(matches!(err, SymbolError::TooLarge { .. }));
    }
}
```

- [ ] **Step 3: Run — expect fail (unresolved names)**

Run: `cd backend && cargo test -p sauron-symbols content::`
Expected: FAIL (no `sha256`/`compress`/`decompress`).

- [ ] **Step 4: Implement `content.rs`**

```rust
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum SymbolError {
    #[error("artifact too large: {size} bytes exceeds cap {cap}")]
    TooLarge { size: usize, cap: usize },
    #[error("decompression failed: {0}")]
    Decompress(String),
    #[error("corrupt artifact: {0}")]
    Corrupt(String),
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn compress(raw: &[u8]) -> Vec<u8> {
    zstd::encode_all(raw, 19).expect("zstd encode is infallible for in-memory")
}

/// Streaming decompress that aborts once `max_uncompressed` bytes are produced.
pub fn decompress(comp: &[u8], max_uncompressed: usize) -> Result<Vec<u8>, SymbolError> {
    use std::io::Read;
    let mut dec = zstd::stream::read::Decoder::new(comp)
        .map_err(|e| SymbolError::Decompress(e.to_string()))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(|e| SymbolError::Decompress(e.to_string()))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max_uncompressed {
            return Err(SymbolError::TooLarge { size: out.len() + n, cap: max_uncompressed });
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}
```
Add `pub mod content;` and `pub mod cache;` + `pub use content::SymbolError;` in `lib.rs`.

- [ ] **Step 5: Run content tests — expect pass**

Run: `cd backend && cargo test -p sauron-symbols content::`
Expected: PASS (3 tests).

- [ ] **Step 6: Write failing test for `ByteLru` eviction + single-flight**

`cache.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn evicts_by_byte_budget() {
        let lru: ByteLru<u32, Vec<u8>> = ByteLru::new(10);
        for k in 0..5u32 {
            lru.get_or_insert(k, |v| v.len(), || async move { vec![0u8; 4] }).await;
        }
        // budget 10, each value weighs 4 -> at most 2 resident
        assert!(lru.len() <= 2);
    }

    #[tokio::test]
    async fn single_flights_concurrent_misses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let lru: ByteLru<u32, u32> = ByteLru::new(1000);
        let mut hs = vec![];
        for _ in 0..8 {
            let lru = lru.clone();
            let calls = calls.clone();
            hs.push(tokio::spawn(async move {
                lru.get_or_insert(1u32, |_| 1, || {
                    let calls = calls.clone();
                    async move { calls.fetch_add(1, Ordering::SeqCst); 42u32 }
                }).await
            }));
        }
        for h in hs { h.await.unwrap(); }
        assert_eq!(calls.load(Ordering::SeqCst), 1); // built once
    }
}
```

- [ ] **Step 7: Run — expect fail**

Run: `cd backend && cargo test -p sauron-symbols cache::`
Expected: FAIL.

- [ ] **Step 8: Implement `cache.rs`**

Implement `ByteLru<K,V>` as `Arc`-cloneable: an inner `Mutex<LruInner>` holding a `HashMap<K, Arc<V>>` + access-order `VecDeque<K>` + `current_bytes`/`budget`, plus a `HashMap<K, Arc<tokio::sync::Mutex<()>>>` (or a `HashMap<K, tokio::sync::broadcast>`), used as a per-key single-flight lock so only one builder runs per key. Signature:
```rust
pub struct ByteLru<K, V> { /* Arc<Mutex<...>> */ }
impl<K: Eq + Hash + Clone + Send + 'static, V: Send + Sync + 'static> ByteLru<K, V> {
    pub fn new(budget_bytes: usize) -> Self { /* ... */ }
    pub fn clone(&self) -> Self { /* Arc clone */ }
    pub fn len(&self) -> usize { /* ... */ }
    pub async fn get_or_insert<W, F, Fut>(&self, key: K, weight: W, build: F) -> Arc<V>
    where W: FnOnce(&V) -> usize, F: FnOnce() -> Fut, Fut: std::future::Future<Output = V> { /* ... */ }
}
```
Single-flight: on miss, take/create the per-key async lock, re-check cache under it, build if still absent, insert (evicting LRU keys until `current_bytes + weight <= budget`), release. Return the `Arc<V>`.

- [ ] **Step 9: Run cache tests — expect pass**

Run: `cd backend && cargo test -p sauron-symbols cache::`
Expected: PASS (2 tests).

- [ ] **Step 10: Commit** *(skip unless asked)*

```bash
git add backend/crates/sauron-symbols backend/Cargo.toml
git commit -m "feat(symbols): sauron-symbols crate — content-addressing, zstd, byte-bounded LRU"
```

---

### Task 4: `sauron-db` — symbol tables, models, repo fns

**Files:**
- Modify: `backend/crates/sauron-db/src/schema.rs` (add `symbol_blobs`, `symbol_artifacts` tables; add 3 cols to `error_events`)
- Modify: `backend/crates/sauron-db/src/models.rs` (`SymbolBlob`, `NewSymbolArtifact`, `SymbolArtifact`)
- Modify: `backend/crates/sauron-db/src/repo.rs` (blob dedup+refcount, artifact CRUD, GC)

**Interfaces:**
- Produces:
  - `repo::put_blob(conn, sha256: &[u8], compressed: &[u8], uncompressed_size, compressed_size) -> QueryResult<()>` — insert-or-bump-refcount (idempotent on existing hash).
  - `repo::insert_symbol_artifact(conn, NewSymbolArtifact) -> QueryResult<SymbolArtifact>`
  - `repo::list_symbol_artifacts(conn, app_id) -> QueryResult<Vec<SymbolArtifact>>`
  - `repo::get_symbol_artifact(conn, app_id, id) -> QueryResult<Option<SymbolArtifact>>`
  - `repo::delete_symbol_artifact(conn, app_id, id) -> QueryResult<bool>` — deletes row, decrements referenced blob refcount(s), GC blobs at 0.
  - `repo::get_blob(conn, sha256: &[u8]) -> QueryResult<Option<Vec<u8>>>` — compressed bytes.
  - `repo::find_artifacts_for_release(conn, app_id, release) -> QueryResult<Vec<SymbolArtifact>>` (used by slice 2).

- [ ] **Step 1: Add schema entries**

In `schema.rs`, hand-add (this project keeps `schema.rs` hand-maintained alongside migrations):
```rust
diesel::table! {
    symbol_blobs (sha256) {
        sha256 -> Bytea,
        content -> Bytea,
        uncompressed_size -> Int8,
        compressed_size -> Int8,
        refcount -> Int4,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    symbol_artifacts (id) {
        id -> Uuid,
        app_id -> Uuid,
        kind -> Text,
        platform -> Text,
        arch -> Nullable<Text>,
        release -> Nullable<Text>,
        dist -> Nullable<Text>,
        name -> Nullable<Text>,
        debug_id -> Nullable<Text>,
        blob_sha256 -> Bytea,
        prebuilt_index_sha256 -> Nullable<Bytea>,
        uploaded_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
    }
}
```
Add the 3 columns to the existing `error_events!` table macro: `stacktrace_symbolicated -> Nullable<Jsonb>`, `symbolication_status -> Text`, `debug_meta -> Nullable<Jsonb>`. Add `symbol_artifacts` + `symbol_blobs` to `allow_tables_to_appear_in_same_query!` if that macro lists tables, and `joinable!(symbol_artifacts -> apps (app_id))`.

- [ ] **Step 2: Add models**

In `models.rs`:
```rust
#[derive(Queryable, Selectable, Identifiable)]
#[diesel(table_name = crate::schema::symbol_artifacts)]
pub struct SymbolArtifact {
    pub id: Uuid,
    pub app_id: Uuid,
    pub kind: String,
    pub platform: String,
    pub arch: Option<String>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub name: Option<String>,
    pub debug_id: Option<String>,
    pub blob_sha256: Vec<u8>,
    pub prebuilt_index_sha256: Option<Vec<u8>>,
    pub uploaded_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::symbol_artifacts)]
pub struct NewSymbolArtifact {
    pub app_id: Uuid,
    pub kind: String,
    pub platform: String,
    pub arch: Option<String>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub name: Option<String>,
    pub debug_id: Option<String>,
    pub blob_sha256: Vec<u8>,
    pub prebuilt_index_sha256: Option<Vec<u8>>,
    pub uploaded_by: Option<Uuid>,
}
```

- [ ] **Step 3: Implement repo fns**

In `repo.rs` add the functions from Interfaces. `put_blob` uses `ON CONFLICT (sha256) DO UPDATE SET refcount = symbol_blobs.refcount + 1` (bump on re-put) — but note: for a *new* artifact reusing an existing blob you bump; for a re-uploaded identical artifact you may double-count. Keep it simple: bump on every `put_blob`, decrement on artifact delete. `delete_symbol_artifact` collects `blob_sha256` + `prebuilt_index_sha256`, deletes the artifact row, then for each hash `UPDATE ... SET refcount = refcount - 1` and `DELETE FROM symbol_blobs WHERE sha256 = $1 AND refcount <= 0`. Wrap in a transaction (`conn.transaction`).

- [ ] **Step 4: Build**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles.

- [ ] **Step 5: Commit** *(skip unless asked)*

```bash
git add backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}
git commit -m "feat(symbols): symbol_blobs/symbol_artifacts models + repo (dedup, refcount GC)"
```

---

### Task 5: Isolated Redis warm-blob cache

**Files:**
- Modify: `backend/crates/sauron-redis/src/lib.rs` (add `SymbolBlobCache`)
- Modify: `backend/crates/sauron-core/src/config.rs` (env: `symbols_redis_url`, `symbols_redis_max_blob_mb`, `symbols_cache_mb`, `symbols_max_artifact_mb`, `symbols_max_uncompressed_mb`, `symbols_ingest_timeout_ms` with the spec defaults)

**Interfaces:**
- Produces:
  - `SymbolBlobCache::connect(url: Option<&str>, max_blob_bytes: usize) -> Self` — no-op cache when `url` is None.
  - `async fn get(&self, sha_hex: &str) -> Option<Vec<u8>>`
  - `async fn put(&self, sha_hex: &str, compressed: &[u8])` — skips when `compressed.len() > max_blob_bytes`.
- Config fields on the existing config struct with defaults: cache 256MB, max_blob 8MB, max_artifact 128MB, max_uncompressed 512MB, ingest_timeout 150ms.

- [ ] **Step 1: Add config fields + env parsing**

Follow the existing pattern in `config.rs` (each field parsed from env with a default). Add the six fields above.

- [ ] **Step 2: Implement `SymbolBlobCache`**

A thin wrapper over a dedicated connection (separate from the ingest-stream client). Key format: `sym:{sha_hex}`. `get` → `GET`; `put` → `SET` with the per-blob size guard; on any Redis error, log at debug and behave as a miss (never fail the caller). When `url` is None, both ops are no-ops.

- [ ] **Step 3: Build**

Run: `cd backend && cargo build -p sauron-redis -p sauron-core`
Expected: compiles.

- [ ] **Step 4: Commit** *(skip unless asked)*

```bash
git add backend/crates/sauron-redis/src/lib.rs backend/crates/sauron-core/src/config.rs
git commit -m "feat(symbols): isolated Redis warm-blob cache + symbols config env"
```

---

### Task 6: Upload / list / delete API

**Files:**
- Create: `backend/bins/sauron-api/src/routes/artifacts.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (`pub mod artifacts;`)
- Modify: `backend/bins/sauron-api/src/main.rs` (3 routes + any `AppState` field for symbols config/cache)
- Modify: `backend/bins/sauron-api/Cargo.toml` (add `sauron-symbols`, ensure `axum` multipart feature / `axum-extra`)

**Interfaces:**
- Consumes: Task 1 `perm::ARTIFACT_WRITE`; Task 3 `content::{sha256,hex,compress}`; Task 4 repo fns; Task 5 config + cache.
- Produces routes:
  - `POST /v1/apps/{app_id}/artifacts` (multipart) → 201 `{ id, blob_sha256, deduped: bool }`
  - `GET /v1/apps/{app_id}/artifacts` → `[{ id, kind, platform, arch, release, name, debug_id, compressed_size, uncompressed_size, created_at }]`
  - `DELETE /v1/apps/{app_id}/artifacts/{artifact_id}` → 204

- [ ] **Step 1: Handler skeleton + authz**

Each handler resolves the app, then `authorize_app` + `require_permission(perm::ARTIFACT_WRITE)` for upload/delete (list uses `perm::ISSUE_READ` or existing app-read). Reuse the exact authz helper pattern from `routes/monitors.rs` (a sibling app-scoped resource).

- [ ] **Step 2: Multipart upload**

Read text fields (`kind`, `platform`, `arch`, `release`, `dist`, `name`, `debug_id`) + one file field. Enforce `symbols_max_artifact_mb` on the raw file (reject 413). Then:
```
raw = file bytes
if raw.len() > max_artifact { return 413 }
sha = sha256(&raw); sha_hex = hex(&sha)
compressed = compress(&raw)
put_blob(conn, &sha, &compressed, raw.len(), compressed.len())      // dedup + refcount
cache.put(&sha_hex, &compressed).await
// slice-2 will add: if kind == js_sourcemap { build prebuilt index, put_blob, set prebuilt_index_sha256 }
insert_symbol_artifact(conn, NewSymbolArtifact { ... blob_sha256: sha.to_vec(), ... uploaded_by })
```
Return the created artifact id. Idempotency: if an artifact with the same `(app_id, debug_id)` or `(app_id, release, name, blob_sha256)` exists, return it with `deduped:true` instead of inserting a dup.

- [ ] **Step 3: List + delete**

List → `list_symbol_artifacts`. Delete → `delete_symbol_artifact` (returns 404 if not found for this app).

- [ ] **Step 4: Wire routes in `main.rs`**

```rust
.route("/v1/apps/{app_id}/artifacts", post(routes::artifacts::upload).get(routes::artifacts::list))
.route("/v1/apps/{app_id}/artifacts/{artifact_id}", delete(routes::artifacts::delete))
```
Add a `symbols` bundle (config + `SymbolBlobCache`) to `AppState` if handlers need it (they need config caps + cache). Construct it in `main` from env.

- [ ] **Step 5: Build**

Run: `cd backend && cargo build -p sauron-api`
Expected: compiles.

- [ ] **Step 6: Commit** *(skip unless asked)*

```bash
git add backend/bins/sauron-api
git commit -m "feat(symbols): artifact upload/list/delete API gated by artifact:write"
```

---

### Task 7: Uploader CLI

**Files:**
- Create: `backend/bins/sauron-symcli/Cargo.toml`
- Create: `backend/bins/sauron-symcli/src/main.rs`

**Interfaces:**
- Consumes: the upload API.
- Produces a binary `sauron-symcli` with:
  - `sauron-symcli upload-sourcemap --api <url> --token <jwt> --app <uuid> --release <r> --name <path> <file.map>`
  - `sauron-symcli upload-dart --api <url> --token <jwt> --app <uuid> --platform android|ios --dir <split-debug-info-dir>` (walks per-arch symbol files, derives `debug_id`/`arch` from filename, uploads each).

- [ ] **Step 1: Scaffold with `clap` + `reqwest` (multipart)**

Use `reqwest::multipart` to POST to `/v1/apps/{app}/artifacts` with `Authorization: Bearer <token>`. Print the returned artifact id (and `deduped`).

- [ ] **Step 2: Build**

Run: `cd backend && cargo build -p sauron-symcli`
Expected: compiles; `--help` lists both subcommands.

- [ ] **Step 3: Commit** *(skip unless asked)*

```bash
git add backend/bins/sauron-symcli
git commit -m "feat(symbols): sauron-symcli uploader (sourcemap + dart symbols)"
```

---

### Task 8: End-to-end verification (Foundation)

**Files:** none (verification only).

- [ ] **Step 1: Bring up the stack**

Run: `docker compose up --build -d` (API 10000, ingest 10001, dashboard 10002 per project ports). Confirm migration 000014 applied in API logs.

- [ ] **Step 2: Upload a source map via the API**

Register/login to get a JWT, create org/project/app (or reuse a seeded app), then:
```bash
curl -sS -X POST http://localhost:10000/v1/apps/$APP/artifacts \
  -H "Authorization: Bearer $JWT" \
  -F kind=js_sourcemap -F platform=web -F release=web@1.4.2 -F name='~/static/app.abc.js' \
  -F file=@app.abc.js.map
```
Expected: 201 with an artifact id.

- [ ] **Step 2b: Upload the SAME file again → dedup**

Re-run the identical curl. Expected: `deduped:true`, and `SELECT refcount FROM symbol_blobs` did not create a second blob row.

- [ ] **Step 3: List + delete + GC**

`GET .../artifacts` shows the row. `DELETE .../artifacts/{id}` → 204. Verify `SELECT count(*) FROM symbol_blobs` dropped to 0 (refcount GC).

- [ ] **Step 4: Authz negative test**

Repeat the upload with a Viewer-role token → expect 403.

- [ ] **Step 5: Full backend test + build**

Run: `cd backend && cargo test --workspace && cargo build --workspace`
Expected: green.

---

## Self-Review

- **Spec coverage:** §4.2 data model → Task 2; §4.3 storage/cache → Tasks 3 & 5; §5 upload API + RBAC → Tasks 1 & 6 + CLI Task 7; §12 config → Task 5. Symbolication (§6/§7), SDK (§8), dashboard (§9) are deliberately slices 2 & 3, not this plan.
- **Placeholders:** none — pure-logic tasks (1,3) carry real test + impl code; DB/API/CLI tasks (4,6,7) carry exact signatures + e2e verification (no unit-test harness exists for them, per the spec).
- **Type consistency:** `content::{sha256→[u8;32], hex, compress, decompress}`, `ByteLru::get_or_insert`, `NewSymbolArtifact`/`SymbolArtifact`, `repo::{put_blob,insert_symbol_artifact,list_symbol_artifacts,get_symbol_artifact,delete_symbol_artifact,get_blob,find_artifacts_for_release}`, `SymbolBlobCache::{connect,get,put}` used consistently across Tasks 3–7.
