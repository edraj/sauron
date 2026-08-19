# Partial view, locked actions, and environment-scoped members

**Date:** 2026-08-19
**Status:** approved, implementation pending

## Problem

The dashboard already prefers "lock, don't hide" for action buttons — 63
`lockedReason=` sites across 22 components, stated as a rule in
`Button.svelte`'s prop doc. Three places contradict or defeat that rule.

### 1. Source Maps denies a member who could read it

`PAGE_ACCESS['/admin/source-maps']` requires `artifact:write`, so the whole page
is replaced by `PermissionDenied`. But `artifacts.rs:396` lists artifacts on
`issue:read`; only upload (`:181`) and delete (`:429`) need `artifact:write`.
A Viewer holds `issue:read` and is shown a wall on a page they could read.

`SourceMaps.svelte:70` already computes `writeLock` and applies it to all three
write buttons, and its own comment already documents the split. The page is
built to degrade; only the table row is wrong.

### 2. An environment-scoped member sees an empty dashboard

Measured against the real `can()` / `PAGE_ACCESS`, a member holding a full
Developer permission set scoped to one environment reaches:

```
gated pages reachable : 0/28
admin children visible: 0/12
/admin lands on       : NOWHERE (denied)
reachable at all      : /admin, /account, /docs
```

Empty sidebar (`Sidebar.svelte` has no `{:else}` for an empty `visibleGroups`)
and `PermissionDenied` on every page.

This is reachable today, and is a frontend gap rather than a policy:

- `ScopeTree.svelte:299` renders an **enabled** env checkbox; `orgs.rs:264`
  accepts `"env"`. An admin can create this member.
- `projects.rs` deliberately lifts env-only grants so the project appears in the
  switcher — "or there is no path from login to the one app that grant is for."
- `sauron-auth` has a complete env-aware read path (`authorize_env_read`). Its
  doc comment records that the env-blind helper "returned 403 to an env-scoped
  caller **even for their own environment**" — a bug already fixed server-side.
- `PageAccess.level` is typed `'org' | 'project' | 'app'`. `'env'` is not
  expressible, so no row can ever match an env grant.
- `can(perm, { env })` works and carries 10 assertions in `session.test.ts` —
  and has **zero production call sites**.

`AppShell` cannot rescue it: `noAccess` requires `projects.length === 0`, but
the backend lift makes projects non-empty, so it falls through to `pageDenied`.

### 3. Nav hides, and the lock reason is mouse-only

`Sidebar` and `AdminShell` filter unreachable items out, the inverse of the
button rule and justified by the opposite principle. Measured across the four
preset roles, the top-level nav has **no variance at all** (14/14 for every
role); all variance is in the 12 admin children — Viewer 5, Developer 7, Admin
8, Owner 12. Notably the role named **Admin** cannot see 4 of the 12, because
`ADMIN` omits `org:manage`, and nothing says so.

Separately, a locked control uses the native `title` attribute on a genuinely
`disabled` button. `disabled` removes it from the tab order, so keyboard and
screen-reader users can never focus it and never receive the reason — which is
the exact discoverability that locking exists to provide.

## Design

### Fix 1 — Source Maps page gate

`PAGE_ACCESS['/admin/source-maps'].perm`: `artifact:write` → `issue:read`.
No component change; the existing `writeLock` already gates the three writes.

### Fix 2 — environment-scoped page access

Mirror the server rather than invent policy. Route files were classified by
which authorization path they use:

- **Env-aware** (`authorized_read_scope` / `authorize_env_read`): issues,
  sessions, devices, screens, workflows, transactions, journeys, performance,
  active_users, analytics (`persons_list`, `events_list`, `persons_count` all
  confirmed), funnels, search.
- **Env-blind** (`authorize_app` / `_project` / `_org`): artifacts, audit,
  environments, inspector, monitors, notifications, orgs, projects, stores.

Add an optional `envAware?: true` to `PageAccess`, set on exactly the rows whose
endpoints take the env-aware path: `/overview`, `/issues`, `/performance`,
`/events`, `/transactions`, `/sessions`, `/users`, `/persons`, `/devices`,
`/screens`, `/workflows`, `/active-users`, `/funnels`, `/journeys`.
`/monitors` is **not** included — `monitors.rs` is env-blind.

Add `sessionStore.canAtAnyEnv(perm)`: true iff any env-scoped grant carries
`perm`.

`canAccessPage` gains an env fallback that mirrors `resolve_env_filter`:

| `currentEnvId` | Rule | Mirrors |
|---|---|---|
| a real id | allow if `can(perm, { env })` | `EnvFilter::One` |
| `null` ("all") | allow if `canAtAnyEnv(perm)` | `All → Subset(readable)` — the server narrows, it does not deny |
| `'none'` (unattributed) | app-level only | `Err(UnattributedNeedsAppReach)` |

Env-blind rows are untouched, so an env grant still can never satisfy an
app/project/org-level check. That is the security-relevant direction and it does
not move.

**Accepted imprecision:** `canAtAnyEnv` does not verify the environment belongs
to the current app, so a member granted on another app's environment sees the
page and receives a clean 403. This is the same trade-off `page-access.ts`
already makes deliberately for `/admin/ingest-failures`, and the alternative —
consulting `sessionStore.environments` — makes the gate flip as that list loads,
the flashing-gate failure `AdminIndex` documents at length.

### Fix 3 — nav locks instead of hiding, and a real tooltip

- `Sidebar` and `AdminShell` render every item. Unreachable ones are **inert**
  (no navigation), carry a lock glyph, and expose the missing permission through
  the tooltip. Both already read `PAGE_ACCESS`, so they cannot disagree.
- `/admin`'s bespoke `visibleAdminNav().length > 0` rule in `Sidebar` becomes
  unnecessary and is removed; `AdminIndex`'s redirect/denied logic is unchanged.
- New `lib/actions/lock-tip.ts`, a Svelte ACTION rather than a component:
  `use:lockTip={lock}` works on any element and reduces every call site to one
  line. It sets `aria-disabled`, adds `.is-locked`, shows a `role="tooltip"`
  bubble on hover **and** focus, dismisses on Escape, and suppresses activation
  in the capture phase — `aria-disabled` prevents neither activation nor form
  submission on its own, and capture-phase `preventDefault` covers both without
  each call site having to guard its own handler.
- Applied at **all 19** lock sites: `Button`, both nav rails, and the 13 raw
  controls in `Roles`, `Projects`, `Inspector`, `MonitorDetail`,
  `FunnelBuilder`, `SwitcherMenu` and `MembersTable` that were still on
  `disabled` + native `title`. Zero `title={lockTitle(...)}` remain.
- `Button` keeps a real `disabled` for its `loading` and `disabled` props: both
  are transient states of a control the user is allowed to use, so dropping
  them from the tab order costs nothing. A lock is permanent and must be
  readable.
- Styles live in `app.css`, not a component: the action builds the bubble in
  `document.body`, which Svelte's scoped CSS cannot reach.

## Not a defect (checked, and initially misread)

`error_timeseries`, `event_timeseries` and `transaction_timeseries`
(`analytics.rs:1198/1229/1262`) use env-blind `authorize_app` while every
sibling in that file is env-aware. That is **deliberate**, not a migration
leftover: `api/scope.ts` lists all three in `BACKEND_REJECTS_ENVIRONMENT_ID`
because they are cross-tier reads spanning hot Postgres and cold Parquet, and
cold storage is not partitioned by environment yet — so they reject
`environment_id` outright rather than scoping only the hot half. The Rust
contract test `http_env_scoping.rs` pins that set. The dashboard also has **zero
callers** for them, so no page is affected.

`/funnels` was the real instance of this class and is handled above: its load
path (`list_saved`) is env-blind, so the page is deliberately not `envAware`.

## Testing

- `page-access.test.ts` — the existing both-directions drift test still passes.
  `envAware` is derived per HANDLER (`PAGE_LOAD_HANDLER` maps each page to the
  `file.rs::function` serving its initial load, and the test extracts that
  function's body from the real backend source). Per-FILE was the first version
  and over-approximated: `funnels.rs` holds both the env-aware `compute` and the
  env-blind `list_saved`, so a file-level check marked `/funnels` env-aware when
  its very first request 403s.
- `session.test.ts` — `canAtAnyEnv` across org/project/app/env grant shapes.
- New: the env-only member reaches the 14 env-aware pages and still reaches
  none of the admin children, at each of the three `currentEnvId` states.
- Nav: locked items render, are inert, and name their permission.
- `Button`: locked is focusable, does not fire `onclick`, does not submit.
