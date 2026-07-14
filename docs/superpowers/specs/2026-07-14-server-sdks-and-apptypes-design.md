# Design: Server-side SDKs (Python, Node/TS, C#) + new app types

**Date:** 2026-07-14
**Status:** Approved by directive (autonomous build — "add extra sdk: py, nodejs js/ts, c# for server side event dispatch, and also added as app type in the dashboard")
**Area:** `sdks/python` + `sdks/node` + `sdks/csharp` (new) + `backend/` (app_type) + `dashboard/` (app_type UI)

## Goal

Three **server-side** SDKs that dispatch product-analytics events and exceptions to the Sauron ingest gateway, plus first-class `python` and `csharp` app types in the dashboard (Node already exists as `node`).

Server-side = no browser/DOM/History/auto-instrumentation. The surface is: init/config, `track`, `captureException`/`captureMessage`, `identify`, `flush`/`close`, with a buffered background HTTP transport.

## Ingest wire contract (authoritative — from `sauron-core/src/envelope.rs`)

- **DSN:** `https://<public_key>@<host>/<project_id>` (public key = non-secret write key; no password component).
- **Endpoint:** `POST {protocol}://{host}/api/{project_id}/envelope`
- **Headers:** `X-Sauron-Key: <public_key>`, `Content-Type: application/json`, optional `Content-Encoding: gzip`.
- **Envelope JSON:**
  ```json
  {
    "header": { "dsn": "<optional>", "sdk": {"name":"sauron-python","version":"0.1.0"},
                "sent_at": "<iso8601>", "environment": "production", "release": null },
    "context": { "device": {"device_id":"<server-instance-id>"}, "os": {"name":"linux","version":null},
                 "app": {}, "runtime": {"name":"python","version":"3.14"}, "user": null },
    "items": [ /* tagged items */ ]
  }
  ```
  Almost every field has a server-side default; only `header.sdk.{name,version}` is strictly required. `sent_at`/`timestamp` default to now.
- **Item shapes** (`type` is the discriminant, `snake_case`):
  - **event** — `{"type":"event","name":<str, required>,"distinct_id":<str, required>,"properties":{},"timestamp":<iso>,"session_id":null,"screen":null}`
  - **error** — `{"type":"error","event_id":<uuid>,"level":"error|warning|info|debug|fatal","timestamp":<iso>,"exception":{"type":<str, required>,"value":<str?>,"mechanism":{"type":"generic","handled":true},"stacktrace":[{"function":?,"module":?,"filename":?,"abs_path":?,"lineno":?,"colno":?,"in_app":?}]},"message":<str?>,"breadcrumbs":[],"tags":{},"fingerprint":null,"user":{"id":?,"email":?,"username":?},"session_id":null,"screen":null}` — frames ordered call-site → crash (crash **last**).
  - **identify** — `{"type":"identify","distinct_id":<str, required>,"anonymous_id":null,"traits":{},"timestamp":<iso>}`
- **Response:** 2xx = accepted. On 401/403 the key is bad → disable + log (don't retry forever). On 429/5xx → retry with backoff (bounded) or drop.

## Common SDK design (all three)

- **Config/init:** `dsn` (required), `environment` (default `production`), `release` (nullable), `sample_rate` (errors, default 1.0), `flush_interval` (default 5s), `max_batch` (default 30), `debug` (default false). A no-op/disabled mode when DSN is missing/invalid (log, don't throw at init in production; but a clearly-invalid DSN may throw a typed error — match each ecosystem's norm).
- **Transport:** in-memory queue; a background timer/thread flushes every `flush_interval` or when `max_batch` reached; `flush()` sends immediately; `close()` flushes then stops. Each flush builds one envelope from the buffered items and POSTs it. Bounded retry on transient failures; drop on hard auth failure. Use the platform's built-in HTTP client (no heavy deps): Python `urllib.request` on a worker thread; Node global `fetch`; C# `HttpClient`. Gzip optional (skip for v0.1 — send plain JSON).
- **Context:** minimal server context — `device.device_id` = a stable per-process id (generate a UUID at init, or use hostname); `runtime.name`/`version` per language; `os` best-effort; `user` = null (server events are attributed via each item's `distinct_id`/`user`).
- **API:**
  - `track(event: str, distinct_id: str, properties?: map)` — **`distinct_id` is required** by the wire contract.
  - `capture_exception(error, *, user?, level='error', tags?)` — extract `{type, value, stacktrace}` from the native exception; frames via the language's stack API, `in_app` heuristic on the app's own modules.
  - `capture_message(message: str, level='info')`.
  - `identify(distinct_id: str, traits?: map)`.
  - `flush(timeout?)`, `close(timeout?)`.
- **SDK header name/version:** `sauron-python` / `sauron-node` / `sauron-dotnet`, all `0.1.0`.
- **Tests:** unit-test the pure parts without network — DSN parsing (valid/invalid), envelope/item JSON shape (golden-ish assertions matching the contract above), stacktrace extraction (crash frame last, `in_app`), and transport batching with an injected fake HTTP sender (assert the POST URL, `X-Sauron-Key` header, and body items). No live network in tests.

## Per-SDK specifics

### Python — `sdks/python/`
- Layout: `pyproject.toml` (name `sauron-sdk`, requires-python `>=3.9`, no runtime deps), `sauron/__init__.py` (public API: `init`, `track`, `capture_exception`, `capture_message`, `identify`, `flush`, `close`), `sauron/_client.py`, `sauron/_dsn.py`, `sauron/_transport.py`, `sauron/_stacktrace.py`, `tests/` (pytest, stdlib `unittest` acceptable).
- Stacktrace via `traceback.extract_tb`; `capture_exception(exc)` reads `exc.__traceback__` (or `sys.exc_info()` when called bare inside `except`). Transport: a daemon `threading.Thread` + `queue.Queue`, `urllib.request.urlopen`.
- Verify: `cd sdks/python && python -m pytest -q` (or `python -m unittest`).

### Node/TS — `sdks/node/`
- Separate package from `sdks/js` (browser). Layout: `package.json` (name `@sauron/node`, `"type":"module"`, build via `tsup` like `sdks/js`, or `tsc`), `tsconfig.json`, `src/index.ts` (public API), `src/client.ts`, `src/dsn.ts`, `src/transport.ts`, `src/stacktrace.ts`, `test/*.test.ts` (vitest).
- Uses Node ≥18 global `fetch`. `captureException(err)` parses `Error.stack` (reuse a V8 stack parser like the browser SDK's `parseStackString` approach). Timer-based flush with `setInterval` + `unref()` so it doesn't hold the event loop open.
- Verify: `cd sdks/node && npm install && npm run build && npm test`.

### C# — `sdks/csharp/`
- Layout: a solution or bare projects — `Sauron/Sauron.csproj` (`net8.0` or `netstandard2.1`; namespace `Sauron`), `Sauron/SauronClient.cs`, `Dsn.cs`, `Transport.cs`, `Envelope.cs` (DTOs + `System.Text.Json`), `StackTraceExtractor.cs`, and `Sauron.Tests/Sauron.Tests.csproj` (xUnit).
- Static facade `Sauron.SauronSdk` with `Init`, `Track`, `CaptureException`, `CaptureMessage`, `Identify`, `Flush`, `Close`. `HttpClient` (singleton) + a `System.Threading.Timer` flush; `CaptureException(Exception ex)` reads `ex.GetType().FullName`, `ex.Message`, and parses `ex.StackTrace` into frames. JSON via `System.Text.Json` with snake_case naming policy.
- Verify: `cd sdks/csharp && dotnet build && dotnet test`.

## App types (backend + dashboard)

Add **`python`** and **`csharp`** (Node already present as `node`).

- **Backend migration `2026-07-14-000008_app_types`:**
  ```sql
  -- up.sql
  ALTER TABLE apps DROP CONSTRAINT IF EXISTS apps_app_type_check;
  ALTER TABLE apps ADD CONSTRAINT apps_app_type_check
    CHECK (app_type IN ('web','flutter','ios','android','react_native','node','python','csharp'));
  -- down.sql
  ALTER TABLE apps DROP CONSTRAINT IF EXISTS apps_app_type_check;
  ALTER TABLE apps ADD CONSTRAINT apps_app_type_check
    CHECK (app_type IN ('web','flutter','ios','android','react_native','node'));
  ```
- **`backend/bins/sauron-api/src/routes/projects.rs`:** extend `const APP_TYPES: [&str; 6]` → `[&str; 8]` adding `"python", "csharp"`.
- **Dashboard:**
  - `models/index.ts`: extend `AppType` union with `'python' | 'csharp'`.
  - `utils/format.ts`: `appTypeIcon` + `appTypeLabel` cases for `python` (icon e.g. `braces` / label "Python") and `csharp` (icon e.g. `hash` / label "C#"), and confirm `node` has entries.
  - App-type pickers (`Projects.svelte` new-app `<select>`, `Onboarding.svelte` type buttons): add Python / C# / Node.js options.
  - `Docs.svelte` integration guides: add install + init snippets for Python, Node, and C# (mirroring the existing web/flutter guides).

## Scope guardrails (YAGNI)

- Server SDKs: no auto-instrumentation, no breadcrumbs ring by default, no offline disk queue, no gzip in v0.1, no framework middleware (Express/ASGI/ASP.NET) adapters — just the core dispatch API.
- No changes to the ingest contract or backend pipeline (the contract already accepts these envelopes).

## Files (new)

- `sdks/python/**`, `sdks/node/**`, `sdks/csharp/**`
- `backend/migrations/2026-07-14-000008_app_types/{up,down}.sql`
- edits: `backend/bins/sauron-api/src/routes/projects.rs`, `dashboard/src/lib/models/index.ts`, `dashboard/src/lib/utils/format.ts`, `dashboard/src/pages/{Projects,Onboarding,Docs}.svelte`
