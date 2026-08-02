# Dashboard permission gating — design

**Date:** 2026-08-02
**Status:** approved, ready for planning

## Problem

The dashboard shows every user the whole product surface regardless of what they
can actually do. Concretely:

1. **Navigation is mostly ungated.** Only 7 of 20 sidebar items carry a `show`
   predicate (`Sidebar.svelte:20-70`). The other 13 — Overview, Exceptions,
   Performance, Events, Sessions, Users, Devices, Screens, Workflows, Funnels,
   Journeys, Projects, App settings — render for anyone. A member without
   `event:read` sees eleven nav entries that can only ever produce errors.
2. **Routes have no permission conditions at all.** `routes.ts:60-129` gates on
   authentication and password freshness only. `#/inspector` is reachable by
   typing it; the file's own comment concedes this.
3. **Gating hides rather than explains.** All ~40 existing gates are
   `{#if sessionStore.can(…)}`. Nothing tells a user *why* an action is absent,
   so a restricted user cannot tell a missing feature from a broken one.
4. **A failed permission fetch is indistinguishable from having no permissions.**
   `session.svelte.ts:222-229` does `getAccess(orgId).catch(() => null)`, and
   `can()` returns `false` whenever `access` is `null`.
5. **`can()` can be more permissive than the server.** It ORs org, project and
   app grants for every check, but the backend's `authorize_org` resolves
   strictly at `(org, None, None)`. A project-scoped `member:manage` grant
   therefore lights UI the server will refuse with 403.

## Decisions

Three policy decisions were made and are fixed for this work:

- **D1 — Navigation hides, actions lock.** A page a user cannot use at all
  disappears from the sidebar. Inside a page they *can* see, every action they
  lack permission for stays visible but disabled, with a lock icon and a tooltip
  naming the missing permission.
- **D2 — Full sweep.** Every existing `{#if can(…)}` action gate is converted to
  disabled-with-lock, every missing nav gate is added, and pages with no gating
  today get it. No half-gated pages are left behind.
- **D3 — Blocked deep-links keep their URL.** Navigating to a page the user
  cannot access renders an in-page denied state inside the normal app shell. No
  redirect, no URL rewrite.

## Non-goals

- **Client gates remain cosmetic.** The server is the authority. No gate may
  become the only thing preventing an action, and every page keeps its existing
  403 handling.
- **`source:read` is not touched.** It is a server-side soft gate that changes
  the *body* of `GET /v1/apps/{id}/issues/{iid}` and `…/events`
  (`issues.rs:112,199`). Pages render whatever they receive.
- **No backend changes.** `GET /v1/orgs/{org}/access` already returns the raw
  `grants[]` array, which is everything the client needs.
- **No new permissions.** The 30 in `perm::ALL` are sufficient.

## Architecture

### One table, two consumers

The nav list (`Sidebar.svelte`) and the route list (`routes.ts`) are two
hand-maintained lists that have already drifted. Rather than gate them
separately, both read a single new table.

**New file: `dashboard/src/lib/models/page-access.ts`**

```ts
export interface PageAccess {
  perm: Permission;                 // permission required to see the page at all
  level: 'org' | 'project' | 'app'; // scope the permission must be held at
  title: string;                    // human page name, used in the denied state
}

/** Keyed by BASE route path. `null` means "no permission required". */
export const PAGE_ACCESS: Record<string, PageAccess | null> = { … };

/** Exact match, then strip trailing segments until a key matches. */
export function resolvePageAccess(path: string): PageAccess | null;
```

Keys are base paths (`/issues`, not `/issues/:id`). `resolvePageAccess`
tries an exact match, then strips trailing path segments until a key matches —
so `/issues/abc123` resolves via `/issues`, and `/persons/xyz` via `/persons`.
This mirrors how `Sidebar`'s existing `match: (p) => p.startsWith(…)` already
works. A path with no entry returns `null` (allowed); a drift test guarantees no
such path exists.

### The table

Derived from an exhaustive audit of `authorize_*` call sites in
`backend/bins/sauron-api/src/routes/*.rs`.

| Base path(s) | `perm` | `level` | `title` |
|---|---|---|---|
| `/overview` | `event:read` | app | Overview |
| `/issues` | `issue:read` | app | Exceptions |
| `/performance` | `event:read` | app | Performance |
| `/events` | `event:read` | app | Events |
| `/sessions` | `event:read` | app | Sessions |
| `/users` | `event:read` | app | Users |
| `/persons` | `event:read` | app | Users |
| `/devices` | `event:read` | app | Devices |
| `/screens` | `event:read` | app | Screens |
| `/workflows` | `event:read` | app | Workflows |
| `/funnels` | `event:read` | app | Funnels |
| `/journeys` | `event:read` | app | Journeys |
| `/active-users` | `event:read` | project | Active users |
| `/monitors` | `monitor:read` | project | Monitors |
| `/alerts` | `alert:read` | org | Alerts |
| `/members` | `member:read` | org | Members |
| `/storage` | `org:manage` | org | Storage |
| `/inspector` | `pii:read` | app | Privacy |
| `/source-maps` | `artifact:write` | app | Source Maps |
| `/settings` | `app:read` | app | App settings |
| `/projects` | `project:read` | app | Projects |
| `/onboarding` | `project:create` | org | Onboarding |
| `/account` | — (`null`) | — | Account |
| `/docs` | — (`null`) | — | Docs |
| `/apps` | — (`null`) | — | Projects |

Notes on non-obvious rows:

- `/members` and `/storage` are `level: 'org'` because their endpoints use
  `authorize_org` (`orgs.rs:160`, `admin.rs:30`), which no narrower grant can
  satisfy. `/alerts` is org-level for the same reason
  (`notifications.rs:66,313`).
- `/active-users` is `level: 'project'` — `active_users.rs:525` resolves reach
  across a project's apps.
- `/projects` uses `level: 'app'` (the widest, org∪project∪app) because
  `projects.rs:49` is reach-based: an app-scoped member legitimately receives a
  filtered list rather than a 403.
- `/source-maps` gates on `artifact:write`, matching today's `Sidebar.svelte:65`.
  Listing artifacts only needs `issue:read` (`artifacts.rs:189`), but the page
  exists to upload, so a member who can only list has nothing to do there.
- `/apps` is the Projects nav item's alternate match prefix
  (`Sidebar.svelte:62`). It carries an explicit `null` so the drift test's
  "every path has a deliberate entry" rule is satisfied.
- `/account` and `/docs` are deliberately ungated. They guarantee the denied
  state's "Back to …" button always has a destination, so the UI can never
  dead-end.

### Consumers

**`Sidebar.svelte`** — the per-item `show?: () => boolean` field is deleted.
Visibility comes from `PAGE_ACCESS[item.href]` via `sessionStore.can(perm,
{ level })`. All 13 holes close at once, and a future page cannot be added
ungated because the drift test fails.

**`AppShell.svelte`** — resolves `$location` through `resolvePageAccess` and
renders `PermissionDenied` in place of `{@render children()}`. **No router
changes**, which is what preserves the URL (D3) for free; every gated page
already mounts `AppShell`.

Render precedence in `AppShell`, most specific first:

1. `loadError` → existing "Couldn't load workspace" + Retry
2. `!sessionStore.loaded` → spinner
3. `accessError` (new, see below) → "Couldn't load permissions" + Retry
4. `noAccess` (existing, `AppShell.svelte:47`) → "No apps available"
5. `pageDenied` (new) → `PermissionDenied`
6. otherwise → `children`

`noAccess` precedes `pageDenied` deliberately: a member with zero reachable
projects is better served by "No apps available" than by "requires event:read",
which would be technically true and useless.

### Fixing `can()`'s over-permissiveness

`CanScope` gains an optional `level` that truncates the cascade, mirroring
`has_permission(grants, perm, org, project?, app?, env?)` with the narrower ids
passed as `None`:

| `level` | grants consulted |
|---|---|
| `'org'` | org only |
| `'project'` | org + project |
| `'app'` (default) | org + project + app |
| `'env'` | org + project + app + env (requires an explicit `env`) |

The default is `'app'`, so all ~40 existing call sites keep today's behaviour and
this is a purely additive change. `env` continues never to default from
`currentEnvId`, for the reasons documented at `session.svelte.ts:123-146`.

Call sites that must adopt `level: 'org'` because their endpoint uses
`authorize_org`: `member:read`, `member:manage`, `member:credential`,
`role:manage`, `org:manage`, `alert:read`, `alert:write`.

### The lock affordance

**`Button.svelte`** gains `lockedReason?: Permission | null`. When non-null the
button is disabled, renders `<Icon name="lock" size={13} />` before its label,
and carries `title="Requires: {PERMISSION_LABELS[perm]} ({perm})"`. The locked
title wins over any caller-supplied `title`, because the reason matters more
than the hint it would replace.

Folding `lockedReason` into the existing `isDisabled` derived
(`Button.svelte:34`) also handles the `href` case for free: an `<a>` cannot be
disabled, and `Button.svelte:37` already routes to `<button>` when disabled.

**Helper** (in `page-access.ts`, alongside the table):

```ts
/** null when allowed, else the missing permission — exactly Button's prop shape. */
export function lockedBy(perm: Permission, scope?: CanScope): Permission | null;

/** The tooltip string, for controls that cannot take a Button prop. */
export function lockTitle(perm: Permission): string;
```

Call sites go from `{#if canWrite}<Button>Resolve</Button>{/if}` to
`<Button lockedReason={lockedBy('issue:write', { app })}>Resolve</Button>`.

**Non-`Button` controls.** Not everything gated is a Button. These use
`lockTitle(perm)` plus an inline `<Icon name="lock" size={12} />`:

- `EnvironmentsCard.svelte` — `<select>`s and toggles (`:267,308,322,342,352`)
- `MembersTable.svelte` — `RowActionsMenu` items, whose markup the caller owns
  (`:115,151,168,181,191,202`)
- `AppEnvPicker.svelte` — the "Unattributed" `<option>` (`:106`) becomes
  `disabled` with a lock prefix in its label rather than being omitted

**`PermissionDenied.svelte`** (new, `lib/components/`) — a thin wrapper over the
existing `EmptyState`, which already accepts `icon="lock"`:

- title: `You don't have access to {title}`
- description: `Requires: {PERMISSION_LABELS[perm]} ({perm}). Ask an organization owner for access.`
- action: `Back to {fallback.title}`, where `fallback` is the first sidebar entry
  in nav order the user can see, falling back to `/account`.

### Fixing the silent lockout

`session.svelte.ts:222-229` currently swallows a failed access fetch into
`access = null`, which makes `can()` return `false` for everything. Today that
is merely ugly. After this change it blanks the entire sidebar and locks every
button — visually identical to "you are a new member with no grants".

`loadOrgScope` therefore records an `accessError: string | null` field on the
store instead of swallowing, and `AppShell` renders it at precedence 3 above.
**This is a prerequisite of the sweep, not a follow-up**: shipping the gating
without it converts a transient network blip into a convincing fake lockout.

## Files

**New**

- `dashboard/src/lib/models/page-access.ts` — table, `resolvePageAccess`,
  `lockedBy`, `lockTitle`
- `dashboard/src/lib/models/page-access.test.ts` — drift guards
- `dashboard/src/lib/components/PermissionDenied.svelte`

**Modified**

- `lib/stores/session.svelte.ts` — `level` in `CanScope`; `accessError` state
- `lib/components/ui/Button.svelte` — `lockedReason`
- `lib/components/layout/Sidebar.svelte` — drop `show`, read the table
- `lib/components/layout/AppShell.svelte` — denied state + `accessError`
- `lib/components/layout/Topbar.svelte` — hide → lock (`:64-65,90-91,105-106`)
- `lib/components/settings/EnvironmentsCard.svelte` — hide → lock (`:267,308,322,342,352`)
- `lib/components/members/MembersTable.svelte` — hide → lock (`:115,151,168,181,191,202`)
- `lib/components/AppEnvPicker.svelte` — hide → lock (`:106`)
- `pages/Members.svelte` (`:161,165,166,167,489`)
- `pages/Projects.svelte` (`:161,185,231,236,296`)
- `pages/SettingsApp.svelte` (`:103,125`)
- `pages/Inspector.svelte` (`:205,287,303,319,421,467`; `:356,374` already disable)
- `pages/IssueDetail.svelte` (`:244`)
- `pages/Alerts.svelte` (`:400,547,583,617,790,824`)
- `pages/Monitors.svelte` (`:92,181`)
- `pages/MonitorDetail.svelte` (`:129,150`)
- `pages/FunnelBuilder.svelte` (`:332,372`)
- `pages/SourceMaps.svelte`, `pages/Storage.svelte` — audit and add gates
- `routes.ts` — **comment only**. No conditions are added: D3 puts the denied
  state in `AppShell`, not the router. The stale comment at `:116-122` claiming
  `/inspector`'s gate is "cosmetic" is rewritten to point at the new mechanism.

**Deliberately unmodified**

- `pages/Account.svelte` — self-scoped (`/v1/me/*`); audited, expected to need
  no gate. Listed here rather than under Modified because the expected diff is
  empty.
- `pages/Login.svelte:36-37` — post-login destination is routing, not an action.
- `AppShell.svelte:43,47,50` — the onboarding / `noAccess` logic is unchanged and
  continues to take precedence over the new denied state.

## Testing

**Unit — `page-access.test.ts` (drift guards).** These permanently kill the
existing nav↔routes drift:

- every key in `PAGE_ACCESS` resolves to a route in `routes`
- every route in `routes` resolves through `resolvePageAccess` to an entry —
  including explicit `null` entries, so a new page cannot be added without a
  deliberate decision. Excluded: the unauthenticated routes (`/login`,
  `/register`, `/forgot-password`, `/reset-password`, `/change-password`,
  `/unsubscribe`) and the two `Redirect` entries (`/`, `*`), none of which
  mount `AppShell`.
- every Sidebar `href` has an entry
- every `perm` is a member of `ALL_PERMISSIONS`

**Unit — `can()` level truncation.** Mirrors the backend's own cases:

- `level: 'org'` with only a project grant → `false` (the analogue of
  `rbac.rs`'s `org_scope_check_ignores_lower_scoped_grants`)
- `level: 'project'` with only an app grant → `false`
- `level: 'app'` (default) with an org grant → `true`
- an env grant never satisfies `'org' | 'project' | 'app'`
- `access === null` → `false` for every level

**Unit — `lockedBy` / `Button`.** Allowed ⇒ `null` ⇒ enabled, no lock glyph.
Denied ⇒ disabled, lock glyph present, `title` contains both the human label and
the raw permission string.

**Runtime verification.** The repo's history is explicit that gating bugs of this
class pass every static gate and only surface in a live session. Drive the real
dashboard as three users against a seeded org:

1. **Viewer** (7 read permissions) — every write button locked, no nav item
   hidden except Alerts / Source Maps / Storage / Privacy.
2. **Developer** (18 permissions) — Members visible read-only, Storage and
   Privacy hidden, member-management buttons locked.
3. **App-scoped member** (a single `app` grant) — confirm nav items appear and
   disappear when switching apps in the Topbar, and that `/members` shows the
   denied state rather than a blank page.

For each: walk every nav group, confirm hidden-vs-locked matches the table,
click each *unlocked* button and confirm no 403, and confirm every locked
button's tooltip names a real permission. Then break the access endpoint
(offline, or a forced 500) and confirm the "Couldn't load permissions" state
appears rather than a silent empty shell.

## Risks

- **Over-hiding.** A `level` that is stricter than the endpoint hides a page a
  user could legitimately use. Mitigated by deriving every row from the actual
  `authorize_*` call site, and by the runtime pass as three distinct roles.
- **Nav churn on app switch.** App-level pages appearing and disappearing as the
  Topbar app changes is correct but visible. Accepted; called out here so it is
  not later reported as a bug.
- **Sweep size.** 3 new files and ~20 modified. Mitigated by landing the prerequisites
  (`accessError`, `level`, `Button.lockedReason`, the table) first, so each page
  conversion afterwards is mechanical and independently verifiable.
