# Persistent shell, admin-only overview recompute, 60s request budget

Date: 2026-08-26. Requested as three items: (1) section clicks must swap only
the inner content, keeping the sidebar; (2) an admin-only way to fetch fresh
overview data on demand; (3) a 60s request timeout, with partial per-section
data shown where possible.

## 1. Persistent shell (content-pane-only navigation)

**Problem.** Every page wraps ITSELF in `<AppShell>` (25 pages directly, 12 via
`AdminShell`), and `svelte-spa-router` swaps the whole routed component. So a
section click unmounts sidebar + topbar and remounts them, and while a lazy
route chunk downloads, `LazyRoute`'s loading state renders with **no shell at
all** — the "whole page loads" the user sees.

**Design.** Hoist the shell above the router:

- `lib/models/shell.ts` — a route→shell-flags map (`requireProject`,
  `requireApp`, or "no shell"), one entry per top-level route, preserving each
  page's current flags byte-for-byte. Unit-tested. This is the single source;
  pages no longer carry the flags.
- `App.svelte` — for a booted, authenticated session on a shell route, render
  `<AppShell {…flags}><Router/></AppShell>`; otherwise the bare `<Router/>`
  (login, register, reset/forgot/change-password, unsubscribe, onboarding).
  The `#if` flips only at auth boundaries, so between app pages the AppShell
  instance — and the sidebar DOM — persists; only the routed component swaps.
  Chunk loading and page data loading now happen inside the persistent shell's
  content pane.
- The 25 pages drop their `<AppShell>` wrapper; `AdminShell` drops its inner
  `<AppShell>` and keeps only the admin rail + body grid.
- `AppShell` behavior is unchanged: flags arrive as (now reactive) props; its
  gate logic (`resolvePageAccess($routePath)`, onboarding/empty-org steering)
  already derives from stores, so a single long-lived instance keeps working.
  `sessionStore.load()` is already idempotent (`loaded` early-return), so
  mounting once per login instead of once per page is a no-op semantically.

**Rejected:** wrapping only `LazyRoute`'s loading state in a shell — the
sidebar would still remount per navigation (scroll/focus/animation reset), and
two shell instances would exist during transitions.

## 2. Admin-only "fetch new data now" for the overview

**Current state.** `POST /v1/apps/{id}/overview/refresh` already exists and
force-recomputes all five sections (`read_section(..., force=true)`), 202 +
SSE. The dashboard's Refresh button calls it **for every viewer**. Plain
section GETs already self-enqueue a recompute whenever the cached value is
older than `FRESH_FOR` (2 minutes) — reads are never blocked on it.

**Design.** Make the force path the admin's option, exactly as asked:

- Backend: `overview_refresh` additionally requires `org:manage` reach over
  the app (`authorize_app(.., perm::ORG_MANAGE)`) after the existing read-scope
  resolution; others get 403. Route set unchanged, audit exemption unchanged.
- Dashboard: the Refresh button stays for everyone (it doubles as the
  revalidate spinner). For `sessionStore.can('org:manage')` holders it calls
  `refreshOverview` (server recompute now) + re-read; for others it only
  re-reads the cached sections — the server still recomputes anything stale
  by itself. Tooltip distinguishes "Recompute now" (admin) from "Refresh".

**Cost rationale:** each force is five aggregate recomputes; on large
deployments that is exactly the load class that was 503ing. Every viewer
holding that trigger is a self-DoS button; admins keep it.

## 3. 60s request budget + partial data

- `REQUEST_TIMEOUT_SECS` 30 → 60 in `bins/sauron-api/src/main.rs` (the one
  constant behind the `TimeoutLayer`); prose that documents "30s" as the
  request budget (code comments near the layer, SETUP/docs, wiki if present)
  updated in the same pass. No client-side axios timeout exists (axios default
  is no timeout), so nothing to mirror there; packaging proxy samples checked
  for a shorter `proxy_read_timeout` so the proxy does not undercut the API.
- Caveat carried from the layer's own docs: the timeout sheds the HTTP
  response, not the Postgres query — doubling the budget doubles how long a
  pathological query can hold a pool slot. Accepted: the active-users
  semaphore (3) and `MAX_INFLIGHT_REQUESTS` still bound concurrency.
- "Show available data in chunks": the Overview page already renders each of
  its five sections independently (per-section endpoints, per-card skeletons,
  envelope with nullable `data`, SSE pushes) — no page-wide loading gate
  remains there. That pattern is the intended model for other slow pages;
  extending it to `/users/summary`-class pages is out of scope here (their
  fix is the rollup backfill + the membership-rollup task already flagged).

## Testing

- Dashboard: unit test for `shell.ts` (flags parity list); full vitest +
  svelte-check; manual drive of navigation (sidebar persistence, chunk-load
  state inside the pane) via the dev server.
- Backend: api test that `overview_refresh` 403s without `org:manage` and
  202s with it; full `sauron-api`/`sauron-db` suites against real PG
  (dangerously-unsandboxed, per the silently-skipping-suite trap); clippy+fmt.
