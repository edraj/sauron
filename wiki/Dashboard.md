# Dashboard

The dashboard is a Svelte SPA that reads the backend API with JWT auth and per-request
RBAC. Its left sidebar is organized into four groups plus a Docs link. Everything is
scoped to the currently-selected **App** (see the tenancy model in **[Home](Home.md)**).

See also: **[Getting Started](Getting-Started.md)** ·
**[Architecture](Architecture.md)** (the queries behind these screens) ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

## Monitor

- **Overview** — the app's health at a glance: signal volume, top issues, and recent
  activity.
- **Exceptions** — the grouped **issues** list (errors fingerprinted into issues).
  Defaults to the all-time range with an "All" range option. Drill into an issue to
  see occurrences, the stack trace, breadcrumbs, affected users, and the tie-in to the
  same person's events.
- **Performance** — aggregated **transaction** timings (p50/p95/etc.) by route /
  operation, split by `op` (`navigation`, `http`, `resource`, `screen_load`,
  `custom`), with error rates.

## Explore

- **Events** — the raw product-analytics event stream (`track` calls) with names,
  properties, and the person each is attributed to.
- **Sessions** — session analytics: a list of sessions and a per-session detail view
  that stitches a user's events, screens, transactions, and errors onto one timeline.
- **Users** — the people explorer. Each person (a `distinct_id`) has a profile showing
  their traits (from `identify`), their events, and their errors — the unified
  observability + analytics view. (Also reachable via `/persons`.)
- **Devices** — a device inventory and per-device detail, keyed by the device context
  the SDKs send.
- **Screens** — screen/route analytics driven by the `$screen` views and the `screen`
  stamped on events/errors. A screens list plus a per-screen detail view showing the
  activity on each screen. (Set the screen with `setScreen` in the
  [Browser](Browser-SDK.md) / [Flutter](Flutter-SDK.md) SDKs.)

## Analyze

- **Funnels** — a funnel builder over your event stream. Define ordered steps and see
  conversion / drop-off between them. Funnels can be **saved as templates**: name and
  store a funnel, then load, duplicate, or remove saved funnels later.
- **Journeys** — a journey/path graph that shows how users move between events and
  screens (branches, common paths).

## Manage

- **Projects** — projects and their apps; create apps and pick an `app_type`
  (`web · flutter · ios · android · react_native · node · python · csharp`). (Also
  reachable via `/apps`.)
- **Members** — org/project/app members and role grants (shown only to users with
  `member:read`). RBAC is enforced per request; the UI hides actions the caller can't
  perform.
- **App settings** — per-app configuration, including the app's **DSN**.

## Docs

A bottom-of-sidebar **Docs** link opens the in-app integration guides — install + init
snippets for each SDK (web, Flutter, Node, Python, C#), mirroring the
**[Getting Started](Getting-Started.md)** flow.

---

Jump to an SDK: **[Browser](Browser-SDK.md)** · **[Flutter](Flutter-SDK.md)** ·
**[Node](Node-SDK.md)** · **[Python](Python-SDK.md)** · **[C#](CSharp-SDK.md)**.
