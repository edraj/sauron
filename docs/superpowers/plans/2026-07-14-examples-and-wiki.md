# Examples for every SDK + Wiki — Plan (Phase 2, runs after the build workflow lands)

**Goal:** A runnable example for each SDK library, and an in-repo `wiki/` documenting all SDKs + the ingest contract, with examples linked from it.

**Precondition:** the `sauron-build-all` workflow (`w4866u9ju`) has completed and the SDK code compiles/tests green (verify the real public APIs before writing examples — do NOT assume the spec API; read the actual `sdks/*` sources).

## Examples (`examples/`)

Existing: `examples/svelte-web` (browser JS), `examples/flutter-app` (Flutter). Add examples for the three new server SDKs:

- `examples/python-server/` — `main.py` + `README.md` + (optional) `requirements`/`pyproject` referencing the local `sdks/python` (editable install). Init from a `SAURON_DSN` env var; `track("order_placed", distinct_id="user_123", {...})`; wrap a deliberate error in try/except → `capture_exception`; `flush()`/`close()` at exit.
- `examples/node-server/` — `index.ts` (or `.mjs`) + `package.json` (depends on `@sauron/node` via `file:../../sdks/node`) + `README.md`. Same flow: init, track, captureException, flush/close.
- `examples/csharp-server/` — a minimal console app `Program.cs` + `.csproj` referencing `../../sdks/csharp/Sauron/Sauron.csproj` + `README.md`. Same flow.

Each example: reads the DSN from an env var, dispatches one event + one captured exception + one identify, flushes, exits 0. Keep them tiny and copy-pasteable. **Verify each runs** (at least compiles/starts; a live dispatch against a running ingest is optional and can point at the compose stack). If a live run isn't possible, verify build/typecheck and note it.

Also confirm the two existing examples still build after the SDK v0.2.0 changes (svelte-web uses `@sauron/browser`); add a `setScreen(...)` call to `examples/svelte-web` to demo the new screen API, and a `SauronNavigatorObserver` note to `examples/flutter-app`.

## Wiki (`wiki/`)

In-repo Markdown wiki (GitHub-wiki page-naming so it maps cleanly if ever pushed — do NOT push to any remote wiki autonomously). Pages:

- `wiki/Home.md` — what Sauron is, the org→project→app→signals model, links to every page.
- `wiki/_Sidebar.md` — nav list of pages.
- `wiki/Getting-Started.md` — create an app, get a DSN, pick an app type, send a first event.
- `wiki/Ingest-Wire-Contract.md` — DSN format, `POST /api/{project_id}/envelope`, `X-Sauron-Key`, envelope + item JSON shapes (from `sauron-core/src/envelope.rs`).
- One page per SDK: `wiki/Browser-SDK.md`, `wiki/Flutter-SDK.md`, `wiki/Python-SDK.md`, `wiki/Node-SDK.md`, `wiki/CSharp-SDK.md` — install, init, `track`/`captureException`/`identify`/`flush`, screen tracking where applicable, and a link to the matching `examples/` dir. Content must match the **actual** shipped API (read the sources).
- `wiki/Examples.md` — index of the `examples/` apps with run instructions.
- `wiki/Dashboard.md` — brief tour of the sections (Overview, Exceptions, Performance, Events, Sessions, Users, Devices, **Screens**, Funnels + saved templates, Journeys).

Link the wiki from the top-level `README.md` ("📖 See the [wiki](wiki/Home.md)").

## Verification

- Examples: build/typecheck each (`python -m py_compile`/import, `tsc`/`node --check`, `dotnet build`). Note any that need a live ingest to fully exercise.
- Wiki: internal links resolve to real files/dirs; code snippets match the shipped SDK APIs.
- No git commits (leave in working tree); no pushing to any remote/GitHub wiki.
