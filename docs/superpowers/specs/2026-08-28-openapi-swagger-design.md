# OpenAPI / Swagger documentation for the Sauron HTTP APIs

**Date:** 2026-08-28
**Status:** implemented and verified 2026-08-28

## Goal

Publish accurate, interactive OpenAPI 3.1 documentation for both HTTP surfaces
Sauron exposes:

- `sauron-api` — 142 route paths / 178 operations, JWT-authed, the dashboard/administration API.
- `sauron-ingest` — 4 routes, public-key authed, the gateway every SDK posts to.

"Accurate" is the load-bearing word. A specification that confidently documents a
response shape the API does not return is worse than no specification at all, so
every design decision below is biased toward *mechanically derived* over
*hand-maintained*, and toward *failing the build* over *silently drifting*.

## Decisions taken

| Question | Decision |
| --- | --- |
| Surfaces covered | Both `sauron-api` (all 178 operations, including `/v1/admin/*`) and `sauron-ingest` |
| Delivery | Swagger UI served by the binary. No committed spec file, no wiki page |
| Example depth | Typed schemas derived everywhere; hand-curated examples on key flows |
| Docs access | Env-gated, on by default (`API_DOCS_ENABLED`) |
| Source of truth | `utoipa` derive annotations in place, plus a router-parity test |
| UI hosting | `sauron-api` only; its UI lists both documents |

### Why derive rather than a hand-written spec

A static `openapi.yaml` covering 178 operations reaches the richest prose fastest
and touches no handler. It is also unbacked by any compiler or test, and this
repository has a documented history of second registries that quietly stop
matching the first. Deriving schemas from the real `serde` types means a response
shape cannot drift; the parity test below closes the one remaining gap.

### Why not `utoipa-axum::OpenApiRouter`

`OpenApiRouter` registers path, method and handler in a single call, making drift
structurally impossible — strictly the stronger guarantee. It requires rewriting
the router assembly in `main.rs`, where the layer order (`DefaultBodyLimit` →
`merge(artifact_routes)` → `cors` → `ConcurrencyLimitLayer` → `TimeoutLayer` →
response headers → `TraceLayer`) and the per-route auth extractor placement are
load-bearing and thinly covered by tests that cannot run without a database. The
parity test buys most of the same guarantee at a fraction of the blast radius.

## Architecture

### Dependencies

Added to `[workspace.dependencies]` in `backend/Cargo.toml`:

```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid"] }
utoipa-swagger-ui = { version = "9", default-features = false, features = ["axum", "vendored"] }
```

Two feature choices are not cosmetic:

- **`chrono` and `uuid`.** Without them `DateTime<Utc>` and `Uuid` serialise into
  the document as opaque objects rather than `string/date-time` and
  `string/uuid`. Those two types appear in the majority of Sauron's DTOs, so
  omitting these features would render most of the specification useless while
  still producing a document that looks complete.
- **`default-features = false` plus `vendored`.** `utoipa-swagger-ui`'s build
  script downloads the Swagger UI release archive over the network at *build*
  time by default. `cargo build --release --workspace` runs inside the RPM
  `%build` section, so the default behaviour would put a network fetch on the
  packaging critical path and inside CI. The `vendored` feature substitutes bytes
  compiled into `utoipa-swagger-ui-vendored`. This must be verified by building
  with the network unavailable, not by reading the feature flag.

`utoipa-swagger-ui` 9.0.2 depends on `axum` 0.8, matching the workspace pin
exactly. No version reconciliation is required.

### Two documents, not one

`sauron-api` and `sauron-ingest` are separate binaries on separate ports with
different authentication schemes, and the deployment constraint that the ingest
gateway must be reachable at host root (or events are silently dropped) does not
apply to the API. A merged document could not state a truthful `servers[]` for
both, so each surface gets its own document.

Only `sauron-api` embeds the Swagger UI assets. Its `/docs` page is configured
with two document URLs so the ingest specification appears in the same selector.
`sauron-ingest` serves `/openapi.json` alone — a few kilobytes of JSON, no
embedded assets — which keeps several megabytes of static files out of the
benchmarked hot-path binary.

### Module layout

- `backend/bins/sauron-api/src/openapi.rs` (new) — the `#[derive(OpenApi)]`
  aggregate, tag definitions, the security-scheme `Modify` addon, the shared
  `ErrorResponse` schema, and the drift tests.
- `backend/bins/sauron-api/src/route_table.rs` (new) — parses `main.rs`'s literal
  source into every `(method, path)` pair the router registers. See "Router
  parity" below.
- `backend/bins/sauron-ingest/src/openapi.rs` (new) — the ingest document.
- Per-operation `#[utoipa::path(...)]` annotations live beside their handlers in
  `routes/*.rs`, so a handler and its documentation move together.
- `main.rs` gains only the Swagger UI merge and the specification route. Its 142
  existing `.route()` calls and its entire layer stack are untouched.

## Annotation strategy

**Schemas.** Roughly 168 DTOs gain `#[derive(ToSchema)]` alongside their existing
`Serialize`/`Deserialize`. utoipa 5 collects schemas transitively from annotated
responses; confirm this on the first module converted and fall back to an
explicit `components(schemas(...))` list only if transitive collection proves
incomplete.

**Operations.** 178 `#[utoipa::path(...)]` annotations, each carrying tag,
summary, description, parameters, request body where applicable, and **only the
status codes that operation actually emits**. A copy-pasted blanket error list
across every operation is the cheapest way to make the document start lying, and
is explicitly rejected.

**Errors.** One shared `ErrorResponse` schema models the uniform envelope
produced by `error.rs`:

```json
{ "error": { "code": "bad_request", "message": "..." } }
```

Documented `code` values are drawn from `ApiError`'s variants: `bad_request`,
`forbidden`, `not_found`, `conflict`, `unprocessable`, `gone`, `rate_limited`,
`internal`, plus the `&'static str` codes carried by `ApiError::Unavailable`.

**Parameters.** The query structs already exist as `#[derive(Deserialize)]`
extractor types. They gain `#[derive(IntoParams)]` so the documented parameters
*are* the parsed ones rather than a hand-written description of them.

> **Hazard, and how it was resolved.** The window-parameter structs use
> `#[serde(flatten)]`. Deriving `IntoParams` on a parent containing one makes
> utoipa demand `ToSchema` for the flattened field, because it wants to document
> it as a **single object-typed query parameter** — while the wire format is four
> separate scalars (`time_field`, `from`, `to`, `since_days`). Satisfying the
> trait would have published a parameter named `window` that the extractor does
> not accept.
>
> The fix is `#[param(ignore = true)]` on the flattened field, with
> `TimeFilterQuery` (which derives `IntoParams` itself) listed separately in each
> route's `params(...)`. Applied in `sessions`, `devices`, `transactions` and
> `analytics`. The existing pinning test
> `routes::search::tests::flattened_window_params_survive_the_query_extractor`
> still passes; its `Probe` struct deliberately does **not** derive `ToSchema`.

**Security.** A `Modify` addon declares `bearerAuth` (`http`, scheme `bearer`,
bearer format `JWT`) for `sauron-api` and `sauronKey` (`apiKey` in header
`X-Sauron-Key`) for `sauron-ingest`. Authentication is applied per handler via
the `AuthUser` extractor rather than by a router layer, so each operation
declares its own requirement.

The eight operations that take no `AuthUser` declare `security(())` explicitly.
Seven are `/v1` routes; `/health` is the eighth and is equally unauthenticated:

| Route | Handler |
| --- | --- |
| `GET /health` | inline |
| `POST /v1/auth/register` | `routes::auth::register` |
| `POST /v1/auth/login` | `routes::auth::login` |
| `POST /v1/auth/refresh` | `routes::auth::refresh` |
| `POST /v1/auth/logout` | `routes::auth::logout` |
| `POST /v1/auth/forgot-password` | `routes::auth::forgot_password` |
| `POST /v1/auth/reset-password` | `routes::auth::reset_password` |
| `POST /v1/notifications/unsubscribe` | `routes::notification_prefs::unsubscribe` |

**Tags.** Approximately 26, one per route-module domain, each with a description:
Health, Auth, Account, Organizations, Projects, Apps, Environments, Issues,
Sessions, Events, Analytics, Active Users, Screens, Devices, Persons,
Performance, Transactions, Workflows, Journeys, Funnels, Search, Monitors,
Alerts, Notifications, Inspector, Artifacts, Stores, Admin, Audit.

## Examples

Schema-derived samples cover every operation. Hand-curated request and response
examples are written for the flows where a generic sample actively misleads:

- **Auth** — register, login, refresh, and the `TokenPair` shape.
- **Ingest envelope** — the single most important example in the document.
- **Search / query language** — the DSL has non-obvious semantics (a bare `@tag`
  matches any key; a bare `sort=` is descending; time units resolve
  longest-suffix-first, so `m` means minutes) that a generated sample cannot convey.
- **Funnels** — the parameter encoding here has produced a 422 in production
  before; the example must show the correct body form.
- **Analytics time windows** — including the clamping behaviour on future and
  far-past timestamps.
- **Admin purge/restore** — the confirm/cancel handshake is stateful and
  destructive, and the example should make the ordering explicit.

The ingest envelope example **reuses the existing golden fixture** from
`sauron-core/src/envelope.rs` rather than a hand-written copy, so the documented
example is the same bytes the SDK parity tests already guard.

## Serving and configuration

`Config` gains `api_docs_enabled`, parsed with the established pattern and
defaulting to on:

```rust
api_docs_enabled: var("API_DOCS_ENABLED")
    .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
    .unwrap_or(true),
```

with the corresponding `.env.example` entry.

`/docs` and `/openapi.json` register inside the existing router chain so they
inherit CORS, the request timeout and tracing. Both are unauthenticated —
documentation, not data — and both are absent entirely (404, not 403) when the
gate is off, so a hardened deployment does not advertise that docs exist.

`sauron-ingest` gains `/openapi.json` only. Its existing `CorsLayer` must permit
the docs origin, otherwise the second entry in the UI's document selector fails
to load with a console-only error that no Rust test can observe.

Two checks belong in implementation rather than assumption:

1. The docs paths must not be picked up by the env-scoping router enumeration in
   `tests/http_env_scoping.rs`, which walks `main.rs`'s literal route table.
   Neither path sits under `/v1/apps/{app_id}`, so this should hold — confirm it.
2. `cargo build --release --workspace` must remain network-free.

No new binary is produced, so the RPM `binaries.txt` manifest is unchanged.

## Drift protection

All three tests are **unit tests in `src/openapi.rs`**, deliberately not in the
`tests/http_*.rs` harness. That harness returns `None` and reports a green pass
in 0.00 s when no database is reachable; a drift test that can silently skip is
not a drift test.

### 1. Router parity

`src/route_table.rs` parses `include_str!`'d `main.rs` source into every
`(method, path)` pair the router registers, covering both the main chain and the
separately-merged `artifact_routes` block. The test asserts set-equality against
`ApiDoc::openapi()`, reporting both directions of the difference — present in the
router but undocumented, and documented but absent from the router.

The extraction asserts a **floor of at least 139 pairs**. Without it, a parser
that quietly stops matching after a syntax change in `main.rs` would compare an
empty set against an empty set and pass, which is the exact failure mode this
test exists to prevent.

> **Consolidated (2026-08-29).** `tests/http_env_scoping.rs` previously carried
> *two* near-identical copies of this scanner (app-scoped and project-scoped
> `GET`). It now includes `route_table.rs` via
> `#[path = "../src/route_table.rs"]` — an integration test cannot `use` a binary
> crate — and both functions are one-line filters over the shared parser. 108
> lines of duplicated parsing removed.
>
> Verified by **diffing both scanners' output against the pre-refactor
> originals** (byte-identical: 60 app-scoped, 6 project-scoped), not by observing
> that the tests still pass — those tests skip silently without Postgres and
> Redis, so green alone would have proved nothing. The suite was then run against
> real services: 38 passed in 1.7s.
>
> Side effect worth knowing: the `#[path]` include also brings `route_table`'s
> five parser unit tests into that binary. They need no services, so they are the
> one part of that file that cannot silently skip.

### 2. Public-route allowlist

Asserts that the set of operations declaring no security requirement equals the
eight-row table above, hardcoded. Adding an unauthenticated endpoint then
requires a deliberate edit to this list, and cannot happen by omission.

### 3. Document validity

Asserts the document serialises and that every `$ref` resolves against
`components`. utoipa will happily emit a dangling reference when a type is
mentioned in a response but never registered as a schema.

## Testing beyond drift

- HTTP tests against the compiled binary: `/openapi.json` returns a parseable
  document, `/docs` returns the UI shell, and both return 404 with
  `API_DOCS_ENABLED=false`.
- Existing suites must continue to pass unchanged; the annotations are additive
  and the router is not restructured.

## Out of scope

- Generating dashboard TypeScript clients from the specification. The 44
  hand-written client modules stay as they are; replacing them is a separate
  piece of work with its own risk profile.
- A committed specification file or a CI drift check against one.
- A rendered wiki reference page.
- Documenting `sauron-monitor`, `sauron-tier`, `sauron-storesync` or any other
  binary that exposes no HTTP surface to users.

## Outcome (2026-08-28)

Implemented as designed, with the deviations and discoveries below.

**Counts.** 142 route paths / **178 operations**, not the 143 first estimated —
the original figure counted paths, and several register more than one method.
The document carries 171 schemas and 19 tags; every operation has a summary and
144 have prose descriptions.

**Verified, not assumed:**

- The `vendored` feature does eliminate the build-time download. The build script
  prints `using vendored Swagger UI`, and a clean **release** rebuild of
  `utoipa-swagger-ui` succeeds with `CARGO_NET_OFFLINE=true` and all proxies
  pointed at a closed port. The RPM `%build` path is safe.
- `router_parity` passes: 0 undocumented, 0 phantom.
- `/docs` and `/openapi.json` answer 200 when enabled and **404** — not 403 —
  under `API_DOCS_ENABLED=0`, while `/health` keeps serving.
- The full backend suite passes against **real** Postgres and Redis (reached on
  the compose network; neither publishes a host port, and host 5432 is an
  unrelated server). Suite durations are non-zero, which is the signal that the
  DB-backed tests actually ran rather than silently skipping.
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -D
  warnings` both clean — the exact CI gates.

**Unplanned work that proved necessary.** The API's response bodies are types
owned by other crates, so `ToSchema` had to be derived where they live rather
than mirrored: `sauron-db` (79 structs), `sauron-core` (the envelope), and
`sauron-inspector`, which gained a `utoipa` dependency. That crate documents
itself as deliberately dependency-light; `utoipa` is a proc-macro with no
runtime or I/O, so it does not affect the property that crate cares about
(unit-testable without services), and the manifest says so.

**A security property now under test.** `User` carries `#[serde(skip_serializing)]`
on `password_hash`. Whether the *schema* derive honours that is a separate
question from whether the *serializer* does, so
`sauron_db::models::openapi_schema_tests` asserts the published schema omits it,
with a vacuity guard so the assertion cannot pass by describing nothing.

**Single-source example.** The ingest envelope example is
`sauron_core::envelope::GOLDEN_ENVELOPE`, hoisted from the test module to a
public constant so the published example and the SDK parity fixture are the same
bytes. A test asserts it still parses as an `Envelope`.

**Follow-up, now done (2026-08-29).** `tests/http_env_scoping.rs` no longer
carries its own scanners; it shares `route_table.rs` by `#[path]` include. See
the note under "Router parity" for how the equivalence was proven.

## Post-implementation defect: the blank docs page (2026-08-29)

`/docs` shipped rendering **blank in a browser** while every request returned
200. `main.rs` applies an API-wide
`Content-Security-Policy: default-src 'none'` under the rationale that "the API
only ever returns JSON" — a premise this feature invalidated by adding an HTML +
CSS + JS surface. The assets were delivered correctly and the browser then
refused to execute them.

**Why the original verification missed it.** `/docs` was checked with `curl`,
which returned 200 and `text/html`. That proves transport, not rendering. The
failure is visible only in a browser console, and no test in the suite opened
one.

**The fix.** `/docs` carries its own policy; every other route keeps the strict
one. Same-origin scripts (`script-src 'self'`, no `unsafe-inline` — the shipped
`index.html` has no inline `<script>`), `unsafe-inline` for styles only,
`img-src 'self' data:`, and `connect-src` widened to the configured ingest
origin so the selector's second entry loads. `frame-ancestors 'none'`,
`nosniff`, `X-Frame-Options` and `Referrer-Policy` are unchanged on `/docs`.

The API-wide layer changed from `overriding` to `if_not_present`. That single
word **was** the bug: the policy string was already correct, and the outermost
layer replaced it.

**The regression test, and why the obvious one would not have worked.** A unit
test asserting the docs policy contains `script-src 'self'` would have passed
against the broken build, because the string was never wrong. The test has to
read the header that actually goes out, through the real layer composition. So
`docs_router` and `apply_security_headers` were extracted from `main`, generic
over router state, and `docs_serving_tests` composes them exactly as `main` does
and drives requests with `ServiceExt::oneshot`.

No database or Redis, so these tests cannot silently skip — which matters, since
that is how the rest of this file's HTTP coverage behaves without services.

Proven to catch the defect by reverting `if_not_present` to `overriding` and
confirming the suite goes red; the failure message names the fix. Five tests:
the asset CSP, the strict policy surviving on JSON routes, the ingest origin
being allowed as an *origin* (a CSP source may not carry a path), the disabled
gate answering 404 rather than 403, and `origin_of` refusing `javascript:`.
