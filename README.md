# Sauron 👁️

**Unified error reporting + product analytics** — Sentry-style crash/error grouping and PostHog-style product events in one platform, on one timeline. When an error fires you can see the same person's events; when you look at a person you see their errors. One SDK emits both signals.

📖 Documentation: see the [wiki](wiki/Home.md). Jump to:

- [Getting Started](wiki/Getting-Started.md) · [Ingest Wire Contract](wiki/Ingest-Wire-Contract.md) · [Capabilities](wiki/Capabilities.md) — the SDK feature-parity matrix (v0.3.0)
- SDKs: [Browser](wiki/Browser-SDK.md) · [Flutter](wiki/Flutter-SDK.md) · [Node](wiki/Node-SDK.md) · [Python](wiki/Python-SDK.md) · [C#](wiki/CSharp-SDK.md)
- Guides: [Framework Integrations](wiki/Framework-Integrations.md) · [Best Practices](wiki/Best-Practices.md) · [Troubleshooting](wiki/Troubleshooting.md)

This repository is a working MVP: a client SDK emits an error or event → the backend ingests, groups, and enriches it → the dashboard shows the grouped issue and the analytics. Session replay/video, ClickHouse/Kafka/object storage, SSO, and billing are intentionally out of scope for this cut (see [`plan.md`](plan.md) for the full product vision).

## Architecture

```
 @sauron/browser  ┐                        ┌─────────────────────────────┐
 sauron_flutter   ├── gzip envelope ──────▶│ sauron-ingest (axum edge)   │
                  ┘  POST /api/{pid}/       │  DSN auth → rate-limit →     │
                     envelope               │  validate → Redis stream     │
                                            │  → [co-located workers]:     │
                                            │    enrich → fingerprint →    │
                                            │    group into issues         │
                                            └──────┬────────────┬──────────┘
                                          Postgres │            │ Redis
                                            ┌───────▼──┐   ┌─────▼────┐
                                            │ Postgres │   │  Redis   │
                                            └───────▲──┘   └─────▲────┘
                        axios + JWT                 │            │
     dashboard (Svelte SPA) ────────────────▶ sauron-api (axum, JWT)
```

- **Write path** (SDK → ingest): authenticated by the non-secret DSN public key, rate-limited per app, fire-and-forget (`202`). Workers drain a Redis stream and write durable rows.
- **Read path** (dashboard → api): JWT auth, with fine-grained RBAC enforced per request.

**Stack:** Rust + axum + diesel-async + JWT · PostgreSQL + Redis · Svelte + axios · JS/TS + Flutter SDKs · Docker Compose.

## Tenancy & access control

```
Organization
  └─ Project        (grouping / product)
       └─ App       (app_type + its own DSN — the ingest unit)
            └─ Environments, Issues, Events, People   (keyed by app_id)
```

One product ("Project X") can hold many heterogeneous **apps** (e.g. 3 Flutter apps + 2 webapps), each with its own DSN. `app_type ∈ web · flutter · ios · android · react_native · node`.

**Fine-grained RBAC.** Atomic permissions (`issue:read`, `issue:write`, `event:read`, `app:*`, `project:*`, `member:*`, `role:manage`, `org:manage`) are bundled into **roles**. Four presets ship — **Owner ⊇ Admin ⊇ Developer ⊇ Viewer** — plus custom roles. A user is granted a role at **org, project, or app** scope; permissions resolve as a **union down the tree** (an org grant covers everything; a project grant covers its apps but not siblings; an app grant is narrowest). So "Admin of Project X, Viewer of Project Y" is expressible. Grants and custom roles are guarded against privilege escalation (you can't grant permissions you don't hold). The dashboard reads `GET /v1/orgs/{org}/access` and hides actions the caller can't perform. See [`docs/audit-2026-07-12-rbac.md`](docs/audit-2026-07-12-rbac.md).

## Repository layout

```
backend/          Rust Cargo workspace
  crates/
    sauron-core       envelope wire contract, fingerprint algorithm, config
    sauron-db         diesel schema/models, async pool, repositories, migrations
    sauron-redis      DSN cache, rate limiter, ingest stream, HLL counters
    sauron-auth       argon2, JWT, axum extractor + authorization helpers
    sauron-pipeline   enrich → fingerprint → issue upsert; the worker loop
    sauron-telemetry  tracing setup
  bins/
    sauron-ingest     SDK edge + co-located worker pool
    sauron-api        JWT dashboard API
    sauron-migrate    one-shot migration runner
dashboard/        Vite + Svelte 5 (runes) + TypeScript + axios SPA
sdks/
  js/             @sauron/browser (TypeScript, tsup)
  flutter/        sauron_flutter (Dart)
examples/
  svelte-web/     runnable demo webapp wired to @sauron/browser
  flutter-app/    runnable demo app wired to sauron_flutter
docs/             design specs + the RBAC security/performance audit
```

## Quick start (Docker Compose)

```bash
cp .env.example .env        # then set JWT_SECRET
docker compose up --build
```

- Dashboard → http://localhost:3000
- API → http://localhost:8080
- Ingest → http://localhost:8081

Register in the dashboard, create a project → an app, copy that app's DSN into an SDK, and watch the first event land.

> First build compiles the Rust workspace three times (one per service image); subsequent builds are cached.

## Local development (without full compose)

```bash
make dev-infra                                   # just Postgres + Redis
export DATABASE_URL=postgres://sauron:sauron@localhost:5432/sauron
export REDIS_URL=redis://localhost:6379
export JWT_SECRET=dev-secret-change-me
make migrate                                     # apply migrations
make api        # terminal 1   (:8080)
make ingest     # terminal 2   (:8081)
cd dashboard && npm install && npm run dev       # terminal 3 (:3000, matches API CORS)
```

## Sending your first event

**Web (`@sauron/browser`):**
```js
import { Sauron } from '@sauron/browser';
Sauron.init({ dsn: 'http://<app_public_key>@localhost:8081/<app_id>' });
throw new Error('hello from the browser');   // auto-captured & grouped
Sauron.track('checkout_completed', { cart_value: 42.5 });
Sauron.identify('u_42', { plan: 'pro' });
```

**Flutter (`sauron_flutter`):**
```dart
await Sauron.init(
  (o) => o.dsn = 'http://<app_public_key>@localhost:8081/<app_id>',
  appRunner: () => runApp(const MyApp()),
);
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

## The wire contract

One JSON envelope, shared by both SDKs and the backend (defined in `backend/crates/sauron-core/src/envelope.rs`; a golden fixture guards parity across all three test suites):

```
POST /api/{app_id}/envelope
X-Sauron-Key: <public_key>          # or ?k=<public_key> for sendBeacon
Content-Encoding: gzip              # optional
```

Error grouping is line-number–independent: two occurrences of the same bug on different lines/releases collapse into one issue.

## Testing

```bash
cd backend && cargo test --workspace     # fingerprint grouping, JWT, envelope parity
cd sdks/js && npm test                   # envelope shape, stacktrace parsing, offline queue
cd sdks/flutter && flutter test          # golden envelope, error capture, queue
```

An end-to-end check — register → create project → POST an envelope to `:8081` → the grouped issue appears via `:8080` — is described in [`plan.md`](plan.md) under *Verification*.

## License

[AGPL-3.0-only](LICENSE) — GNU Affero General Public License v3.0.
