# Sauron Wiki

**Sauron** is a unified **observability + product analytics** platform: Sentry-style
crash/error grouping and PostHog-style product events on **one timeline**, behind
**one SDK**. When an error fires you can see the same person's events; when you look
at a person you see their errors. A client SDK emits an error or event → the backend
ingests, groups, and enriches it → the dashboard shows the grouped issue alongside
the analytics.

## The model: Org → Project → App → signals

```
Organization
  └─ Project        (grouping / product)
       └─ App       (app_type + its own DSN — the ingest unit)
            └─ Environments, Issues, Events, Sessions, People, Screens, Transactions
```

- An **Organization** owns projects and members.
- A **Project** groups related apps (one product can hold many heterogeneous apps).
- An **App** is the ingest unit. Each app has an `app_type` and its own **DSN**. Signals
  are keyed by `app_id`.
- **Signals** are the things SDKs emit: **errors** (grouped into issues), **events**
  (product analytics), **identify** calls (people/traits), **transactions**
  (performance), and **breadcrumb batches**.

`app_type ∈ web · flutter · ios · android · react_native · node · python · csharp`.

Access is governed by fine-grained RBAC: atomic permissions bundled into roles
(Owner ⊇ Admin ⊇ Developer ⊇ Viewer, plus custom), granted at org / project / app
scope and resolved as a union down the tree.

## Pages

- **[Getting Started](Getting-Started.md)** — create an app, get its DSN, pick an app
  type, send your first event.
- **[Ingest Wire Contract](Ingest-Wire-Contract.md)** — the DSN format, the
  `POST /api/{project_id}/envelope` endpoint, the `X-Sauron-Key` header, and every
  envelope / item JSON shape. This is what all SDKs emit.
- **[Architecture](Architecture.md)** — how it works under the hood: the
  ingest pipeline, error grouping & symbolication, the SQL behind the analytics, data
  tiering, uptime probing, and RBAC.
- **[Capabilities](Capabilities.md)** — the SDK feature-parity matrix (scope,
  breadcrumbs, transactions, `beforeSend`, gzip, retry, queue, auto-capture) across all
  five SDKs, as of **v0.3.0**.

### SDKs

- **[Browser SDK](Browser-SDK.md)** — `@sauron/browser` (client, errors + analytics +
  performance + screens + breadcrumbs).
- **[Flutter SDK](Flutter-SDK.md)** — `sauron_flutter` (client, four uncaught-error
  layers + analytics + screens).
- **[Python SDK](Python-SDK.md)** — `sauron-sdk` (server-side dispatch).
- **[Node SDK](Node-SDK.md)** — `@sauron/node` (server-side dispatch).
- **[C# SDK](CSharp-SDK.md)** — `Sauron` / `sauron-dotnet` (server-side dispatch).

### Guides

- **[Framework Integrations](Framework-Integrations.md)** — copy-paste recipes for
  Express/Fastify/Koa, Flask/FastAPI/Django, ASP.NET Core, and React/Vue/Svelte.
- **[Best Practices](Best-Practices.md)** — event naming, PII scrubbing via `beforeSend`,
  sampling, tags vs context, `distinct_id`/identify, and flush/shutdown for short-lived
  processes.
- **[Troubleshooting](Troubleshooting.md)** — nothing showing up, disabled no-op mode,
  gzip/retry/queue behavior, scope-leak pitfalls, and the version check.

### Reference

- **[Examples](Examples.md)** — the runnable apps in [`examples/`](../examples), with
  copy-paste run commands.
- **[Dashboard](Dashboard.md)** — a tour of the dashboard sections: Overview,
  Exceptions, Performance, Events, Sessions, Users, Devices, Screens, Funnels,
  Journeys, and the Manage section.

---

*This wiki documents the shipped MVP. Session replay/video, ClickHouse/Kafka/object
storage, SSO, and billing are intentionally out of scope for this cut.*
