# Admin View and Role Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate seven scattered admin pages under a nested `/admin/*` section, complete the role lifecycle with delete + copy, and rework the 30-permission picker with collapse, per-section select-all, and search.

**Architecture:** Frontend-heavy. One new backend endpoint (`DELETE` role) mirroring the existing `update_role_handler` guard order. Dashboard gains an `AdminShell` composing the existing `AppShell` with a sub-nav rail; nine routes move or are created; two new screens (`/admin/roles`, `/admin/environments`) absorb functionality currently embedded in `Members.svelte` and `SettingsApp.svelte`.

**Tech Stack:** Rust (axum, diesel-async), Svelte 5 runes, svelte-spa-router, vitest.

## Global Constraints

- **NEVER commit and never create branches.** This repo's standing rule. Every task below ends at "verify", not "commit". Leave all work in the working tree.
- **Backend tests:** `cargo test --workspace` from `backend/`.
- **Frontend tests:** `npm test` (vitest 2.1.9, `vitest run`) from `dashboard/`. Type check: `npm run check` (svelte-check). Both must be clean.
- **Vitest has no globals** — import `{ describe, expect, it }` from `'vitest'` explicitly. No jsdom, no `@types/node` (node builtins need the `// @ts-expect-error` comment dance used in existing tests).
- **Permissions are unchanged.** No new permission string. `perm::ALL` stays at 30. Do not touch `rbac.rs` or `permissions.ts` — `permissions.test.ts` parses `rbac.rs` from disk and will fail on drift.
- **Route/PAGE_ACCESS parity is test-enforced** in both directions by `page-access.test.ts`. Adding a route without a `PAGE_ACCESS` entry fails the suite, and vice versa.
- **Svelte stores use the `class XStore { field = $state<T>() }` + `export const xStore = new XStore()` idiom** in `.svelte.ts` files, with all `window`/`document` access guarded by `typeof … === 'undefined'` for the node test environment.

---

### Task 1: Widen `routeBases` so nested routes are legal

**Blocks every other task in this plan.** Today `page-access.test.ts:86-90` derives legal `PAGE_ACCESS` keys as `'/' + p.split('/')[1]` — first segment only. A `'/admin/members'` key fails. A single `'/admin'` key would pass but cannot carry nine different permissions.

**Files:**
- Modify: `dashboard/src/lib/models/page-access.test.ts:85-96`
- Test: same file (this task is a test change)

**Interfaces:**
- Produces: a `routeBases` set containing every param-free prefix of every route, so Tasks 5+ can add `'/admin'` and `'/admin/<child>'` keys.

- [ ] **Step 1: Write the failing test**

Add this test inside the existing `describe('PAGE_ACCESS', …)` block in `dashboard/src/lib/models/page-access.test.ts`, immediately after the `'has no entry for a path that is not a real route'` test:

```ts
  // Nested admin routes each need their OWN entry: '/admin/members' and
  // '/admin/storage' have different permissions at different levels, so a
  // single '/admin' key cannot express them. This asserts the prefix
  // derivation below accepts multi-segment keys.
  it('accepts a multi-segment key for a nested route', () => {
    const prefixes = routePrefixes(['/admin/members', '/issues/:id', '/issues']);
    expect(prefixes.has('/admin')).toBe(true);
    expect(prefixes.has('/admin/members')).toBe(true);
    expect(prefixes.has('/issues')).toBe(true);
    // A param segment terminates the walk — '/issues/:id' must NOT contribute
    // '/issues/:id' as a legal key.
    expect(prefixes.has('/issues/:id')).toBe(false);
  });
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/page-access.test.ts`
Expected: FAIL with `routePrefixes is not defined`.

- [ ] **Step 3: Write the helper and use it**

In `dashboard/src/lib/models/page-access.test.ts`, add this function immediately after `parseRoutePaths()` (after line 44):

```ts
/**
 * Every param-free prefix of each route path.
 *
 * Was `'/' + p.split('/')[1]` — first segment only, which was sufficient while
 * every route was one segment deep and made nested keys impossible. A `:param`
 * segment terminates the walk, so `/issues/:id` still contributes only
 * `/issues` and today's keys all stay legal; `/admin/members` now additionally
 * contributes itself, so each admin child can carry its own permission.
 */
function routePrefixes(paths: string[]): Set<string> {
  const out = new Set<string>();
  for (const p of paths) {
    if (!p.startsWith('/') || p.length <= 1) continue;
    const segments = p.split('/').filter(Boolean);
    const acc: string[] = [];
    for (const seg of segments) {
      if (seg.startsWith(':')) break;
      acc.push(seg);
      out.add('/' + acc.join('/'));
    }
  }
  return out;
}
```

Then replace the body of the `'has no entry for a path that is not a real route'` test (currently lines 85-96) with:

```ts
  it('has no entry for a path that is not a real route', () => {
    const routeBases = routePrefixes(ROUTE_PATHS);
    for (const path of Object.keys(PAGE_ACCESS)) {
      expect(routeBases.has(path), `PAGE_ACCESS key "${path}" matches no route in routes.ts`).toBe(
        true,
      );
    }
  });
```

- [ ] **Step 4: Run the full suite to verify nothing regressed**

Run: `cd dashboard && npm test`
Expected: PASS. Every existing `PAGE_ACCESS` key is a first segment of a real route, so all remain legal under the widened derivation.

---

### Task 2: Backend `DELETE` role endpoint

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (add `delete_role` after `update_role`, which ends at line 1058)
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs` (append `delete_role_handler` after `update_role_handler`; file ends at 1520)
- Modify: `backend/bins/sauron-api/src/main.rs:461-464`
- Test: `backend/bins/sauron-api/tests/http_orgs.rs`

**Interfaces:**
- Consumes: `repo::get_role`, `guard::drops_org_manage`, `repo::count_org_manage_grants_excluding_role` (all existing).
- Produces: `repo::delete_role(conn, org_id, role_id) -> QueryResult<usize>`; `orgs::delete_role_handler` returning `Json<serde_json::Value>` shaped `{"revoked_grants": <i64>}`.

- [ ] **Step 1: Write the repo function**

In `backend/crates/sauron-db/src/repo.rs`, immediately after `update_role` (which ends line 1058):

```rust
/// Delete a custom role. Scoped by `org_id` as well as `role_id` so a mistaken
/// call cannot reach across orgs, and filtered on `is_system` so a preset can
/// never be deleted even if a caller-side check is missed — the same defence in
/// depth `update_role` uses.
///
/// `role_grants.role_id` is ON DELETE CASCADE, so every grant holding this role
/// disappears with it. Callers must have already counted and confirmed that.
pub async fn delete_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        roles::table
            .filter(roles::id.eq(role_id))
            .filter(roles::org_id.eq(org_id))
            .filter(roles::is_system.eq(false)),
    )
    .execute(conn)
    .await
}
```

- [ ] **Step 2: Write the grant-count helper**

Still in `repo.rs`, after `delete_role`. The UI needs to report what the cascade removed, and the handler needs the number before deleting:

```rust
/// How many grants currently hold `role_id`. Used to report what a delete
/// cascaded, since `role_grants.role_id` is ON DELETE CASCADE and the rows are
/// gone by the time the delete returns.
pub async fn count_grants_for_role(
    conn: &mut AsyncPgConnection,
    role_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM role_grants WHERE role_id = $1",
    )
    .bind::<SqlUuid, _>(role_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}
```

`GrantCountRow` already exists at `repo.rs:1205-1209`; reuse it, do not redeclare.

- [ ] **Step 3: Write the handler**

Append to `backend/bins/sauron-api/src/routes/orgs.rs` (after `update_role_handler`, at EOF). Guard order deliberately mirrors `update_role_handler:1462-1471`:

```rust
/// Delete a role this org owns.
///
/// Presets are refused for the same reason edits are: `ensure_preset_roles`
/// re-creates them from rbac.rs at every API boot, so a delete would silently
/// come back on the next restart.
///
/// `role_grants.role_id` is ON DELETE CASCADE, so this revokes the role from
/// every holder at once. The response reports how many grants went with it —
/// the rows are gone by the time the delete returns, so the count is taken
/// first.
pub async fn delete_role_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ROLE_MANAGE).await?;

    let role = repo::get_role(&mut conn, role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Presets first — their existence is already public via list_roles, so a
    // clear refusal is correct here, not a 404.
    if role.is_system {
        return Err(ApiError::BadRequest(
            "system roles cannot be deleted".into(),
        ));
    }
    // A role owned by another org is not public; NotFound avoids confirming it
    // exists.
    if role.org_id != Some(org_id) {
        return Err(ApiError::NotFound);
    }

    // Deleting a role revokes it from every holder at once. If it is the org's
    // only source of org:manage, that orphans the org exactly as deleting the
    // last owner grant would.
    let old_perms = role_permissions(&role.permissions);
    if old_perms.iter().any(|p| p == perm::ORG_MANAGE) {
        let remaining =
            repo::count_org_manage_grants_excluding_role(&mut conn, org_id, role_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "this is the org's last role granting org:manage — grant it elsewhere first".into(),
            ));
        }
    }

    let revoked = repo::count_grants_for_role(&mut conn, role_id).await?;
    let deleted = repo::delete_role(&mut conn, org_id, role_id).await?;
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "revoked_grants": revoked })))
}
```

If `json!` is not already imported in `orgs.rs`, add `use serde_json::json;` — check the existing imports first; `Value` is already used by `update_role_handler`.

- [ ] **Step 4: Register the route**

In `backend/bins/sauron-api/src/main.rs`, change lines 461-464 from:

```rust
        .route(
            "/v1/orgs/{org_id}/roles/{role_id}",
            patch(routes::orgs::update_role_handler),
        )
```

to:

```rust
        .route(
            "/v1/orgs/{org_id}/roles/{role_id}",
            patch(routes::orgs::update_role_handler).delete(routes::orgs::delete_role_handler),
        )
```

`delete` and `patch` are both already imported (used at `main.rs:455`).

- [ ] **Step 5: Write integration tests**

In `backend/bins/sauron-api/tests/http_orgs.rs`, follow the existing test-harness pattern in that file. Cover exactly these five cases:

1. Owner deletes a custom role with no holders → 200, `revoked_grants == 0`, and a subsequent `GET /v1/orgs/{org}/roles` no longer lists it.
2. Owner deletes a custom role held by 2 grants → 200, `revoked_grants == 2`, and the holders' `GET /v1/orgs/{org}/access` no longer shows those grants.
3. Delete a system preset (e.g. Developer) → 400.
4. Delete a role belonging to another org → 404.
5. Second delete of the same id → 404.

- [ ] **Step 6: Run backend tests**

Run: `cd backend && cargo test --workspace`
Expected: PASS, including the five new cases.

- [ ] **Step 7: Clippy**

Run: `cd backend && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

---

### Task 3: `deleteRole` API client function

**Files:**
- Modify: `dashboard/src/lib/api/orgs.ts` (append after `updateRole`, which ends the file at line 118)

**Interfaces:**
- Produces: `deleteRole(orgId: string, roleId: string): Promise<{ revoked_grants: number }>`, consumed by Task 8.

- [ ] **Step 1: Add the function**

```ts
export async function deleteRole(
  orgId: string,
  roleId: string,
): Promise<{ revoked_grants: number }> {
  const { data } = await api.delete<{ revoked_grants: number }>(
    `/v1/orgs/${orgId}/roles/${roleId}`,
  );
  return data;
}
```

- [ ] **Step 2: Type check**

Run: `cd dashboard && npm run check`
Expected: 0 errors.

---

### Task 4: `AdminShell` and `AdminIndex`

**Files:**
- Create: `dashboard/src/lib/components/layout/AdminShell.svelte`
- Create: `dashboard/src/pages/AdminIndex.svelte`
- Create: `dashboard/src/lib/models/admin-nav.ts`
- Test: `dashboard/src/lib/models/admin-nav.test.ts`

**Interfaces:**
- Consumes: `canAccessPage`, `resolvePageAccess` from `lib/models/page-access`; `AppShell` from `lib/components/layout/AppShell.svelte`.
- Produces: `ADMIN_NAV: AdminNavItem[]` and `firstAccessibleAdminPath(): string | null` from `admin-nav.ts`; `AdminShell` accepting `{ requireProject?: boolean, requireApp?: boolean, children: Snippet }`.

- [ ] **Step 1: Write the nav table**

Create `dashboard/src/lib/models/admin-nav.ts`:

```ts
import type { IconName } from '../components/ui/Icon.svelte';
import { canAccessPage, resolvePageAccess } from './page-access';

export interface AdminNavItem {
  href: string;
  label: string;
  icon: IconName;
}

/**
 * The admin sub-nav, in display order.
 *
 * Deliberately carries no permission of its own: visibility is resolved
 * through PAGE_ACCESS exactly as Sidebar does, so the rail, the sidebar and
 * AppShell's in-page gate cannot disagree about who may see a page.
 */
export const ADMIN_NAV: AdminNavItem[] = [
  { href: '/admin/members', label: 'Members', icon: 'key-round' },
  { href: '/admin/roles', label: 'Roles', icon: 'shield-check' },
  { href: '/admin/projects', label: 'Projects', icon: 'folders' },
  { href: '/admin/environments', label: 'Environments', icon: 'layers' },
  { href: '/admin/settings', label: 'App settings', icon: 'settings' },
  { href: '/admin/source-maps', label: 'Source Maps', icon: 'braces' },
  { href: '/admin/alerts', label: 'Alerts', icon: 'bell' },
  { href: '/admin/storage', label: 'Storage', icon: 'server' },
  { href: '/admin/privacy', label: 'Privacy', icon: 'shield-alert' },
];

/** Admin children the current user may open, in nav order. */
export function visibleAdminNav(): AdminNavItem[] {
  return ADMIN_NAV.filter((i) => canAccessPage(resolvePageAccess(i.href)));
}

/**
 * Where `/admin` should land. `null` when the user can reach no child at all,
 * which AdminIndex renders as a denial rather than a redirect loop.
 */
export function firstAccessibleAdminPath(): string | null {
  return visibleAdminNav()[0]?.href ?? null;
}
```

Verify `'shield-check'` and `'layers'` exist in `Icon.svelte`'s `IconName` union before using them. If either is absent, either add the icon to the registry or substitute an existing name — `npm run check` will catch it.

- [ ] **Step 2: Write the failing test**

Create `dashboard/src/lib/models/admin-nav.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { ADMIN_NAV } from './admin-nav';
import { PAGE_ACCESS } from './page-access';

describe('ADMIN_NAV', () => {
  // Sidebar and AdminShell both resolve visibility through PAGE_ACCESS. A nav
  // item whose href has no entry falls through resolvePageAccess's
  // fail-open branch and would show to everyone.
  it('every item has its own PAGE_ACCESS entry', () => {
    for (const item of ADMIN_NAV) {
      expect(Object.keys(PAGE_ACCESS), `${item.href} needs a PAGE_ACCESS entry`).toContain(
        item.href,
      );
    }
  });

  it('has no duplicate hrefs', () => {
    const hrefs = ADMIN_NAV.map((i) => i.href);
    expect(new Set(hrefs).size).toBe(hrefs.length);
  });
});
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/admin-nav.test.ts`
Expected: FAIL — `PAGE_ACCESS` has none of the `/admin/*` keys yet. Task 5 adds them.

- [ ] **Step 4: Write AdminShell**

Create `dashboard/src/lib/components/layout/AdminShell.svelte`. Match the house style of `Sidebar.svelte` (nav rail with icon + label, `aria-current` on the active item) and read the current path from `svelte-spa-router`'s `location` store as `Sidebar.svelte:2` does:

```svelte
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { location } from 'svelte-spa-router';
  import AppShell from './AppShell.svelte';
  import Icon from '../ui/Icon.svelte';
  import { visibleAdminNav } from '../../models/admin-nav';

  interface Props {
    requireProject?: boolean;
    requireApp?: boolean;
    children: Snippet;
  }

  // Forwarded to AppShell unchanged — each admin page keeps the scope
  // requirements it had as a top-level route.
  let { requireProject = false, requireApp = false, children }: Props = $props();

  const items = $derived(visibleAdminNav());
</script>

<AppShell {requireProject} {requireApp}>
  <div class="admin">
    <nav class="rail" aria-label="Admin sections">
      {#each items as item (item.href)}
        <a
          href={`#${item.href}`}
          class="item"
          class:active={$location.startsWith(item.href)}
          aria-current={$location.startsWith(item.href) ? 'page' : undefined}
        >
          <Icon name={item.icon} size={15} />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>
    <div class="body">{@render children()}</div>
  </div>
</AppShell>

<style>
  .admin {
    display: grid;
    grid-template-columns: 190px minmax(0, 1fr);
    gap: 22px;
    align-items: start;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    position: sticky;
    top: 0;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 10px;
    border-radius: var(--radius);
    font-size: 13px;
    color: var(--text-muted);
    text-decoration: none;
  }
  .item:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .item.active {
    background: var(--surface-2);
    color: var(--text);
    font-weight: 560;
  }
  .body {
    min-width: 0;
  }
  @media (max-width: 900px) {
    .admin {
      grid-template-columns: 1fr;
    }
    .rail {
      flex-direction: row;
      overflow-x: auto;
      position: static;
    }
  }
</style>
```

- [ ] **Step 5: Write AdminIndex**

Create `dashboard/src/pages/AdminIndex.svelte`. `/admin` has no permission of its own, so it resolves where to land at mount:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import PermissionDenied from '../lib/components/PermissionDenied.svelte';
  import { firstAccessibleAdminPath } from '../lib/models/admin-nav';

  // `replace`, not `push`: /admin is a resolver, not a destination. Pushing
  // would trap Back on a path that immediately forwards again.
  let denied = $state(false);

  onMount(() => {
    const target = firstAccessibleAdminPath();
    if (target) replace(target);
    else denied = true;
  });
</script>

<AppShell requireProject={false}>
  {#if denied}
    <PermissionDenied />
  {/if}
</AppShell>
```

Check `PermissionDenied.svelte`'s actual import path and required props first — `AppShell.svelte:115` already renders it, so copy that call site's usage exactly.

- [ ] **Step 6: Type check**

Run: `cd dashboard && npm run check`
Expected: 0 errors. The `admin-nav.test.ts` failure from Step 3 is expected to persist until Task 5.

---

### Task 5: Routes, PAGE_ACCESS, legacy redirects

**Files:**
- Modify: `dashboard/src/routes.ts:104-124`
- Modify: `dashboard/src/lib/models/page-access.ts:68-94`
- Modify: `dashboard/src/lib/models/page-access.test.ts:50-59`

**Interfaces:**
- Consumes: `AdminIndex` from Task 4.
- Produces: the nine `/admin/*` routes and their `PAGE_ACCESS` entries, which Tasks 7 and 11 attach pages to.

- [ ] **Step 1: Add the routes**

In `dashboard/src/routes.ts`, add the import beside the others:

```ts
import AdminIndex from './pages/AdminIndex.svelte';
```

Replace the block currently spanning `// Settings` through `'/inspector': guarded(...)` (lines 104-124) with:

```ts
  // Admin. Nested routes, each with its own PAGE_ACCESS entry — the children
  // carry genuinely different permissions at different levels, so a single
  // '/admin' requirement could not express them. '/admin' itself is ungated
  // and resolves to the first child the caller can reach.
  //
  // Route conditions deliberately do NOT carry permissions: a failed condition
  // fires `conditionsFailed`, which navigates, and a deep link the user cannot
  // open should keep its URL and explain itself. AppShell resolves the path
  // through PAGE_ACCESS and renders PermissionDenied in place of the page.
  // Both layers stay cosmetic — every endpoint 403s on its own.
  '/admin': guarded(AdminIndex as Component<never>),
  '/admin/members': guarded(Members as Component<never>),
  '/admin/roles': guarded(Roles as Component<never>),
  '/admin/projects': guarded(Projects as Component<never>),
  '/admin/environments': guarded(Environments as Component<never>),
  '/admin/settings': guarded(SettingsApp as Component<never>),
  '/admin/source-maps': guarded(SourceMaps as Component<never>),
  '/admin/alerts': guarded(Alerts as Component<never>),
  '/admin/storage': guarded(Storage as Component<never>),
  '/admin/privacy': guarded(Inspector as Component<never>),

  '/account': guarded(Account as Component<never>),

  // Legacy paths. Bare Redirect entries, no guarded() — they mount no AppShell
  // and have no page permission, so they are listed in LEGACY_REDIRECTS in
  // page-access.test.ts rather than carrying PAGE_ACCESS rows. Kept so
  // bookmarks and any hardcoded links survive the move.
  '/members': wrap({ component: Redirect as never, props: { to: '/admin/members' } }),
  '/projects': wrap({ component: Redirect as never, props: { to: '/admin/projects' } }),
  '/settings': wrap({ component: Redirect as never, props: { to: '/admin/settings' } }),
  '/source-maps': wrap({ component: Redirect as never, props: { to: '/admin/source-maps' } }),
  '/alerts': wrap({ component: Redirect as never, props: { to: '/admin/alerts' } }),
  '/storage': wrap({ component: Redirect as never, props: { to: '/admin/storage' } }),
  '/inspector': wrap({ component: Redirect as never, props: { to: '/admin/privacy' } }),
```

`Roles` and `Environments` do not exist yet — Tasks 7 and 11 create them. To keep the tree type-checking in between, create both files now as minimal stubs that Tasks 7 and 11 fill in:

```svelte
<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
</script>

<AdminShell requireProject={false}>
  <h1 class="page-title">Roles</h1>
</AdminShell>
```

Create `dashboard/src/pages/Roles.svelte` with the above, and `dashboard/src/pages/Environments.svelte` with the same shape (title "Environments", `requireProject`). Add both imports to `routes.ts`.

Verify `svelte-spa-router`'s `wrap` accepts a `props` option in the installed version (v4 does). If it does not, replace each legacy entry with a tiny per-path component that calls `replace('/admin/…')` on mount, mirroring `AdminIndex`.

- [ ] **Step 2: Update PAGE_ACCESS**

In `dashboard/src/lib/models/page-access.ts`, replace the `// --- Manage ---` block (lines 68-87) with:

```ts
  // --- Admin ---------------------------------------------------------------
  // '/admin' itself is ungated: no single permission expresses "can reach at
  // least one admin child", and inventing one would drift from the nine child
  // gates below. AdminIndex redirects to the first reachable child, or renders
  // PermissionDenied when there is none.
  '/admin': null,
  // orgs.rs:160 uses authorize_org — a project- or app-scoped member:read
  // grant genuinely cannot list members.
  '/admin/members': { perm: 'member:read', level: 'org', title: 'Members' },
  // orgs.rs:1380 gates list_roles on member:read, not role:manage — reading
  // the catalogue is not the same as editing it.
  '/admin/roles': { perm: 'member:read', level: 'org', title: 'Roles' },
  // projects.rs:49 is reach-based: an app-scoped member receives a filtered
  // list rather than a 403, so this is the widest level rather than 'project'.
  '/admin/projects': { perm: 'project:read', level: 'app', title: 'Projects' },
  // environments.rs:196 authorizes the catalogue read at the project.
  '/admin/environments': { perm: 'env:read', level: 'project', title: 'Environments' },
  '/admin/settings': { perm: 'app:read', level: 'app', title: 'App settings' },
  // Listing artifacts only needs issue:read (artifacts.rs:189), but this page
  // exists to upload, so a member who can only list has nothing to do here.
  '/admin/source-maps': { perm: 'artifact:write', level: 'app', title: 'Source Maps' },
  // notifications.rs:66,313 use authorize_org.
  '/admin/alerts': { perm: 'alert:read', level: 'org', title: 'Alerts' },
  // admin.rs:30 uses authorize_org.
  '/admin/storage': { perm: 'org:manage', level: 'org', title: 'Storage' },
  '/admin/privacy': { perm: 'pii:read', level: 'app', title: 'Privacy' },

  // --- Self-service --------------------------------------------------------
  // Self-scoped (/v1/me/*). Always reachable — see the fallback note on
  // `PermissionDenied`, which relies on at least one ungated page existing.
  '/account': null,
```

Delete the now-stale `// NB: no '/apps' key` comment block (lines 75-77) along with the old `/projects` entry, and remove the old `/alerts` entry from the `// --- Alerting ---` section (line 66) since it moved.

- [ ] **Step 3: Exempt the legacy redirects from the drift test**

In `dashboard/src/lib/models/page-access.test.ts`, after the `UNAUTHENTICATED` array (line 59), add:

```ts
// Legacy paths kept as bare Redirect entries after the /admin/* move. They
// mount no AppShell, so like UNAUTHENTICATED they have no page permission —
// but they ARE authenticated app paths, so they get their own list rather
// than making that array's name a lie.
const LEGACY_REDIRECTS = [
  '/members',
  '/projects',
  '/settings',
  '/source-maps',
  '/alerts',
  '/storage',
  '/inspector',
];
```

Then in the `'covers every guarded route'` test (line 76), change:

```ts
      if (UNAUTHENTICATED.includes(routePath)) continue;
```

to:

```ts
      if (UNAUTHENTICATED.includes(routePath) || LEGACY_REDIRECTS.includes(routePath)) continue;
```

- [ ] **Step 4: Fix the moved-page assertions**

Still in `page-access.test.ts`, the `'gates org-only pages at org level'` test (around line 132) references `/members`, `/storage`, `/alerts`. Update to the new paths:

```ts
  it('gates org-only pages at org level', () => {
    // These three call authorize_org server-side (orgs.rs:160, admin.rs:30,
    // notifications.rs:66), which no project- or app-scoped grant can satisfy.
    expect(resolvePageAccess('/admin/members')?.level).toBe('org');
    expect(resolvePageAccess('/admin/storage')?.level).toBe('org');
    expect(resolvePageAccess('/admin/alerts')?.level).toBe('org');
  });
```

- [ ] **Step 5: Run the full suite**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS, including `admin-nav.test.ts` from Task 4, which now finds its entries.

---

### Task 6: Sidebar restructure

**Files:**
- Modify: `dashboard/src/lib/components/layout/Sidebar.svelte:28-66`

- [ ] **Step 1: Replace the Manage group and trim Uptime**

In `dashboard/src/lib/components/layout/Sidebar.svelte`, replace the `Uptime` group (lines 28-34) with:

```ts
    {
      label: 'Uptime',
      items: [
        { href: '#/monitors', label: 'Monitors', icon: 'life-buoy', match: (p) => p.startsWith('/monitors') },
      ],
    },
```

and replace the entire `Manage` group (lines 54-65) with:

```ts
    {
      label: 'Admin',
      items: [
        { href: '#/admin', label: 'Admin', icon: 'shield-check', match: (p) => p.startsWith('/admin') },
      ],
    },
```

- [ ] **Step 2: Move Account to the bottom block**

Account is `null`-gated self-service, not administration. In the `.bottom` block where the Docs link is hardcoded (around line 107), add an Account link above it, matching the Docs link's existing markup exactly:

```svelte
    <a class="nav-item" href="#/account" class:active={$location.startsWith('/account')}>
      <Icon name="user" size={16} />
      <span class="label">Account</span>
    </a>
```

Copy the surrounding element's real class names and structure from the Docs link rather than assuming — the snippet above is illustrative.

- [ ] **Step 3: Verify visibility filtering still works**

The `visibleGroups` derivation at lines 76-83 is unchanged: `#/admin` slices to `/admin`, which resolves to `PAGE_ACCESS['/admin'] === null`, so the Admin group is always visible. That is correct — `AdminIndex` handles the no-reachable-child case with `PermissionDenied`, which is a better experience than a nav item that silently vanishes.

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 4: Runtime check**

Start the dev server and confirm: the sidebar shows an Admin group, clicking it lands on the first admin child, and the sub-nav rail renders only reachable items.

---

### Task 7: `/admin/roles` screen

**Files:**
- Modify: `dashboard/src/pages/Roles.svelte` (replace the Task 5 stub)
- Modify: `dashboard/src/pages/Members.svelte` (remove the Roles card, lines 506-545, and the now-unused state)
- Modify: `dashboard/src/pages/Members.svelte:417` (swap `AppShell` → `AdminShell`)

**Interfaces:**
- Consumes: `listRoles`, `RoleEditorDialog`, `roleMemberCounts` derivation (moved from `Members.svelte:144-159`).
- Produces: the row-actions surface Tasks 8 and 9 hang Delete and Copy off.

- [ ] **Step 1: Build the Roles page**

Replace `dashboard/src/pages/Roles.svelte` with a full page. Move these pieces out of `Members.svelte` verbatim:
- the `roleMemberCounts` `$derived.by` block (`Members.svelte:144-159`) — it needs `grouped`, so the page must load members too, via `listMembers` + `groupMembers`;
- `roleManageLock` (`Members.svelte:171`);
- the `RoleEditorDialog` mount (`Members.svelte:550-557`) and `onRoleSaved` (`Members.svelte:324-326`).

Render a `DataTable` (not a `<ul>`) with columns: Name, Description, Permissions (count), Members (count), and a row-actions cell using `RowActionsMenu`. Keep the existing gating rule from `Members.svelte:531-540` exactly — a system role's action reads "View" and is never locked; a custom role's reads "Edit" and is locked by `roleManageLock`.

Wrap in `<AdminShell requireProject={false}>`.

- [ ] **Step 2: Strip Roles out of Members**

In `dashboard/src/pages/Members.svelte`:
- delete the Roles `<Card>` block (lines 506-545);
- delete the `RoleEditorDialog` mount (lines 550-557);
- delete `roleMemberCounts`, `roleManageLock`, `roleDialogOpen`, `editingRole`, `openNewRole`, `openEditRole`, `onRoleSaved`, and the `roles` state plus its `listRoles` call — but **only** if nothing else in the file uses them. `roles` is likely still needed by the grant-creation form's role picker; check every reference before deleting.
- change `<AppShell requireProject={false}>` at line 417 to `<AdminShell requireProject={false}>` and update the import.

- [ ] **Step 3: Swap the remaining moved pages to AdminShell**

In each of `Projects.svelte:158`, `SettingsApp.svelte:85`, `SourceMaps.svelte` (the `<AppShell>` at the top of the template), `Alerts.svelte:354`, `Storage.svelte:46`, `Inspector.svelte:193`: change `AppShell` → `AdminShell` in both the import and the element, preserving each page's existing `requireProject` / `requireApp` props unchanged.

- [ ] **Step 4: Verify**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 5: Runtime check**

Confirm `/admin/roles` lists roles with correct member counts, `/admin/members` still works with the Roles card gone, and every moved page renders inside the admin rail.

---

### Task 8: Delete role, with typed confirmation

**Files:**
- Create: `dashboard/src/lib/components/members/DeleteRoleDialog.svelte`
- Modify: `dashboard/src/pages/Roles.svelte`

**Interfaces:**
- Consumes: `deleteRole` (Task 3), `Modal`, `Button`, `Input`.
- Produces: nothing downstream.

`ConfirmDialog` cannot be reused — it takes a plain `message: string` and has no input (`ConfirmDialog.svelte:5-15`).

- [ ] **Step 1: Build the dialog**

Create `dashboard/src/lib/components/members/DeleteRoleDialog.svelte`:

```svelte
<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import { deleteRole } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Role } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    role: Role | null;
    /** Distinct members holding this role — the blast radius of the cascade. */
    memberCount: number;
    onclose: () => void;
    ondeleted: (role: Role, revokedGrants: number) => void;
  }

  let { open, orgId, role, memberCount, onclose, ondeleted }: Props = $props();

  let confirmName = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!open) return;
    confirmName = '';
    error = null;
  });

  // Typed confirmation, because role_grants.role_id is ON DELETE CASCADE:
  // deleting strips access from every holder at once and nothing undoes it.
  const canDelete = $derived(!busy && role !== null && confirmName.trim() === role.name);

  async function submit() {
    if (!canDelete || !role) return;
    busy = true;
    error = null;
    try {
      const { revoked_grants } = await deleteRole(orgId, role.id);
      ondeleted(role, revoked_grants);
      onclose();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal {open} title={`Delete ${role?.name ?? 'role'}`} onclose={onclose}>
  {#if memberCount > 0}
    <p class="warning">
      {memberCount}
      {memberCount === 1 ? 'member' : 'members'} will lose this access immediately. This cannot be undone.
    </p>
  {:else}
    <p class="lede">No members hold this role. This cannot be undone.</p>
  {/if}

  <Input
    label={`Type "${role?.name ?? ''}" to confirm`}
    bind:value={confirmName}
    placeholder={role?.name ?? ''}
  />

  {#if error}<p class="err-msg">{error}</p>{/if}

  {#snippet footer()}
    <Button variant="secondary" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="danger" disabled={!canDelete} loading={busy} onclick={submit}>
      Delete role
    </Button>
  {/snippet}
</Modal>

<style>
  .lede {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 14px;
  }
  .warning {
    font-size: 12.5px;
    color: var(--warning);
    background: var(--warning-soft);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);
    border-radius: var(--radius);
    padding: 8px 12px;
    margin-bottom: 14px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13px;
    margin-top: 12px;
  }
</style>
```

Confirm `Button` supports `variant="danger"` — `ConfirmDialog.svelte:33` uses it, so it does.

- [ ] **Step 2: Wire it into the Roles page**

Add a Delete item to each **custom** role's `RowActionsMenu`, locked by `roleManageLock`. System presets get no Delete item at all (the server returns 400). On `ondeleted`, remove the role from local state and toast the revoked count:

```ts
  function onRoleDeleted(role: Role, revokedGrants: number) {
    roles = roles.filter((r) => r.id !== role.id);
    toastStore.success(
      revokedGrants > 0
        ? `Deleted "${role.name}" and revoked ${revokedGrants} grant${revokedGrants === 1 ? '' : 's'}.`
        : `Deleted "${role.name}".`,
    );
  }
```

- [ ] **Step 3: Verify**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 4: Runtime check**

Create a throwaway custom role, grant it to a member, delete it, and confirm: the typed confirmation gates the button, the toast reports the revoked count, and the member's access actually disappears from `/admin/members`.

---

### Task 9: Copy role

**Files:**
- Modify: `dashboard/src/pages/Roles.svelte`
- Modify: `dashboard/src/lib/components/members/RoleEditorDialog.svelte`

- [ ] **Step 1: Add a copy-source prop to the editor**

`RoleEditorDialog` currently repopulates from `role` (lines 39-45), where `role === null` means create. Copy needs "create, but prefilled". Add a `copyFrom: Role | null` prop and extend the `$effect`:

```ts
  // Copy opens the CREATE path (role === null) prefilled from another role, so
  // submit() still calls createRole and the server's no-escalation check still
  // applies. The Copy action is disabled when the caller lacks any of these
  // permissions, so that check cannot fail from here.
  $effect(() => {
    if (!open) return;
    const source = role ?? copyFrom;
    name = role ? source?.name ?? '' : copyFrom ? `Copy of ${copyFrom.name}` : '';
    description = source?.description ?? '';
    permissions = [...(source?.permissions ?? [])];
    error = null;
  });
```

Update the `title` derivation so a copy reads `New role from ${copyFrom.name}` rather than the bare `New role`.

- [ ] **Step 2: Gate the Copy action**

In `Roles.svelte`, compute per-role whether the caller holds every permission the source grants. The server requires this at org scope (`orgs.rs:1409-1416`):

```ts
  import { sessionStore } from '../lib/stores/session.svelte';
  import type { Permission, Role } from '../lib/models';

  /**
   * The first permission in `role` the caller does not hold at org scope, or
   * null when they hold all of them.
   *
   * create_role (orgs.rs:1409) rejects any permission the creator lacks, so a
   * copy of a role holding more than the caller does would 403 on save. Naming
   * the blocking permission up front beats a dead-end dialog.
   */
  function copyBlockedBy(role: Role): Permission | null {
    for (const p of role.permissions) {
      if (!sessionStore.can(p, { level: 'org' })) return p;
    }
    return null;
  }
```

Render the Copy item with `lockedReason={copyBlockedBy(role)}`. Offer it for **system presets too** — copying Developer or Owner is the primary use case; presets are read-only to edit, not to read.

- [ ] **Step 3: Verify**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 4: Runtime check**

As an Owner, copy Developer → dialog opens named "Copy of Developer" with 18 permissions ticked; save creates a new custom role. As an Admin (who lacks `org:manage`), confirm Copy on Owner is locked and the tooltip names `org:manage`.

---

### Task 10: PermissionPicker rework

**Files:**
- Modify: `dashboard/src/lib/components/members/PermissionPicker.svelte` (full rewrite of the template + script)
- Test: `dashboard/src/lib/models/permission-picker.test.ts`

**Interfaces:**
- Consumes: `PERMISSION_GROUPS`, `PERMISSION_LABELS`.
- Produces: unchanged `onchange(next: Permission[])` contract — **the catalog-order emit at line 15-22 must survive verbatim.**

- [ ] **Step 1: Extract the pure logic and test it first**

Create `dashboard/src/lib/models/permission-picker.ts`:

```ts
import { PERMISSION_GROUPS, PERMISSION_LABELS } from './permissions';
import type { Permission } from './index';

export type GroupState = 'all' | 'some' | 'none';

/** Whether a group is fully, partly, or not at all selected. */
export function groupState(groupPermissions: Permission[], selected: Set<Permission>): GroupState {
  const hits = groupPermissions.filter((p) => selected.has(p)).length;
  if (hits === 0) return 'none';
  if (hits === groupPermissions.length) return 'all';
  return 'some';
}

/** Emit in catalog order so a role's stored array is stable regardless of click order. */
export function inCatalogOrder(selected: Set<Permission>): Permission[] {
  return PERMISSION_GROUPS.flatMap((g) => g.permissions).filter((p) => selected.has(p));
}

/** Case-insensitive match against the permission string and its label. */
export function matchesQuery(permission: Permission, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (permission.toLowerCase().includes(q)) return true;
  return (PERMISSION_LABELS[permission] ?? '').toLowerCase().includes(q);
}
```

Create `dashboard/src/lib/models/permission-picker.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { groupState, inCatalogOrder, matchesQuery } from './permission-picker';
import { PERMISSION_GROUPS } from './permissions';
import type { Permission } from './index';

describe('groupState', () => {
  const group = ['issue:read', 'issue:write'] as Permission[];

  it('is none when nothing is selected', () => {
    expect(groupState(group, new Set())).toBe('none');
  });

  it('is some when part of the group is selected', () => {
    expect(groupState(group, new Set(['issue:read'] as Permission[]))).toBe('some');
  });

  it('is all when the whole group is selected', () => {
    expect(groupState(group, new Set(group))).toBe('all');
  });
});

describe('inCatalogOrder', () => {
  it('emits in catalog order regardless of insertion order', () => {
    const catalog = PERMISSION_GROUPS.flatMap((g) => g.permissions);
    const shuffled = new Set([catalog[5], catalog[0], catalog[2]]);
    expect(inCatalogOrder(shuffled)).toEqual([catalog[0], catalog[2], catalog[5]]);
  });
});

describe('matchesQuery', () => {
  it('matches the permission string', () => {
    expect(matchesQuery('issue:read' as Permission, 'issue')).toBe(true);
  });

  it('matches the human label', () => {
    // 'org:manage' is labelled with prose that does not contain "org:manage".
    expect(matchesQuery('org:manage' as Permission, 'org')).toBe(true);
  });

  it('is true for an empty query', () => {
    expect(matchesQuery('issue:read' as Permission, '   ')).toBe(true);
  });

  it('is false for a non-match', () => {
    expect(matchesQuery('issue:read' as Permission, 'zzzznope')).toBe(false);
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/permission-picker.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Run it again after creating the module**

Run: `cd dashboard && npx vitest run src/lib/models/permission-picker.test.ts`
Expected: PASS.

- [ ] **Step 4: Rewrite the component**

Rewrite `dashboard/src/lib/components/members/PermissionPicker.svelte` using the helpers. Requirements, all load-bearing:

- **Disclosure idiom:** `<button aria-expanded={open}>` + `{#if open}`, following `ScopeTree.svelte:223,265`. **Not** `<details>`/`<summary>` — that appears nowhere in this codebase.
- **Initial collapse:** open only groups with at least one selected permission. Compute once when the picker mounts or the selection is replaced wholesale, **not** in a `$derived` — otherwise unticking a group's last box would collapse it mid-interaction.
- **Tri-state select-all** per group in the summary row. `indeterminate` is a DOM property, not an attribute: bind it (`bind:indeterminate`) or set it in an effect. Templating `indeterminate={...}` silently does nothing.
- **Search** via `Input`, filtering with `matchesQuery`. Matching groups auto-expand; groups with zero matches hide entirely. **Clearing search restores the pre-search collapse state** — snapshot it when the query goes from empty to non-empty, restore when it returns to empty.
- **`disabled` propagates** to every checkbox including the select-alls, so the read-only preset view stays genuinely read-only.
- Show a live `N of 30 selected` count.

- [ ] **Step 5: Verify**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 6: Runtime check**

Open a custom role with a partial selection: only its groups are expanded, their checkboxes are indeterminate. Search "env" → only the env group shows, expanded. Clear search → prior collapse state returns. Tick a group's select-all → all its boxes tick and the count updates. Open a system preset → everything is disabled.

---

### Task 11: `/admin/environments` screen

**Files:**
- Modify: `dashboard/src/pages/Environments.svelte` (replace the Task 5 stub)
- Modify: `dashboard/src/pages/SettingsApp.svelte` (remove the `EnvironmentsCard` mount + import)
- Possibly delete: `dashboard/src/lib/components/settings/EnvironmentsCard.svelte`

- [ ] **Step 1: Build the page**

`EnvironmentsCard.svelte` is per-app. The new screen is project-wide: list every environment in `sessionStore.currentProjectId`'s catalogue, and for each, which apps are enrolled plus per-app ingest keys.

Move the card's logic across. The five locks carry over **verbatim** from `EnvironmentsCard.svelte:77-81` — note the deliberate split, which the card's own comment at `:35-36` explains:

```ts
  // Catalogue operations (create / rename / retire) hit the PROJECT and change
  // what every app in it sees. Mute / promote / rotate hit
  // /v1/app-environments/{id} and are per-app. Hence two different scopes.
  const createLock = $derived(lockedBy('env:create', { project: projectId, level: 'project' }));
  const renameLock = $derived(lockedBy('env:update', { project: projectId, level: 'project' }));
  const retireLock = $derived(lockedBy('env:delete', { project: projectId, level: 'project' }));
  const updateLock = $derived(lockedBy('env:update', { app: appId, level: 'app' }));
  const rotateLock = $derived(lockedBy('env:rotate_key', { app: appId, level: 'app' }));
```

The per-app locks (`updateLock`, `rotateLock`) now need computing **per row**, since the page spans apps rather than sitting inside one. Do not hoist them to a single `appId`.

Wrap in `<AdminShell requireProject>`.

- [ ] **Step 2: Remove it from App settings**

In `dashboard/src/pages/SettingsApp.svelte`, delete the `EnvironmentsCard` mount and its import. App settings keeps only rename / ingest-toggle / delete.

Then check whether `EnvironmentsCard.svelte` has any remaining importer:

Run: `cd dashboard && grep -rn "EnvironmentsCard" src/`
If nothing remains, delete the file. Two components mutating the same resource is how two UIs come to disagree.

- [ ] **Step 3: Verify**

Run: `cd dashboard && npm test && npm run check`
Expected: PASS.

- [ ] **Step 4: Runtime check**

Confirm the screen lists every environment in the project with enrolled apps, that create/rename/retire affect all apps in the project, that rotate is per-app, and that App settings no longer shows environments.

---

### Task 12: Full-stack verification

**Files:** none — this task only runs things.

- [ ] **Step 1: Full test sweep**

```bash
cd backend && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cd ../dashboard && npm test && npm run check && npm run build
```
Expected: all clean.

- [ ] **Step 2: Legacy redirect sweep**

With the dev server running, visit each old path and confirm it lands on its new home: `/members`→`/admin/members`, `/projects`→`/admin/projects`, `/settings`→`/admin/settings`, `/source-maps`→`/admin/source-maps`, `/alerts`→`/admin/alerts`, `/storage`→`/admin/storage`, `/inspector`→`/admin/privacy`.

- [ ] **Step 3: Permission-shape check**

This is the step that catches gating regressions, and it cannot be done from tests. Sign in as a **Viewer** and confirm:
- The Admin nav group is visible (`/admin` is ungated by design).
- `/admin` lands on `/admin/roles` (Viewer holds `member:read`) — **not** a blank page.
- `/admin/storage` renders `PermissionDenied`, not a crash.
- Role rows show "View" for presets, and Edit/Copy/Delete are locked.

Then as a user with **no** `member:read`, confirm `/admin` lands on the first child they *can* reach, or renders `PermissionDenied` if there is none.

- [ ] **Step 4: Delete cascade check**

Confirmed already in Task 8, but re-verify after everything is wired: create a custom role, grant it, delete it, and check that `GET /v1/orgs/{org}/access` for that member no longer returns the grant.
