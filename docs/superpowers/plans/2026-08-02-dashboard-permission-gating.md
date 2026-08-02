# Dashboard Permission Gating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hide dashboard pages a user has no permission for, and disable (rather than remove) every in-page action they lack permission for, with a lock icon naming the missing permission.

**Architecture:** One new table (`page-access.ts`) maps route paths to a required permission and the scope level it must be held at; `Sidebar` reads it for nav visibility and `AppShell` reads it to render an in-page denied state without touching the router. `sessionStore.can()` gains a `level` that truncates the grant cascade so client checks stop being more permissive than `authorize_org`/`authorize_project`. `Button` gains a `lockedReason` prop.

**Tech Stack:** Svelte 5 (runes), TypeScript, vitest, svelte-spa-router, `@lucide/svelte` via the `Icon` registry.

## Global Constraints

- **Never commit and never create branches.** All work stays uncommitted in the working tree on the current branch. (Project rule — overrides the skill's default commit steps, which are omitted from every task below.)
- **Client gates are cosmetic.** The server is the authority. No gate may become the only thing preventing an action; every page keeps its existing 403 handling.
- **`can()` must never be more permissive than the backend.** Its cascade mirrors `has_permission(grants, perm, org, project?, app?, env?)` in `backend/crates/sauron-auth/src/rbac.rs:272`.
- **No backend changes, no new permissions.** `GET /v1/orgs/{org}/access` already returns raw `grants[]`; the 30 permissions in `perm::ALL` are sufficient.
- **`env` never defaults from `currentEnvId`** — preserve the invariant documented at `src/lib/stores/session.svelte.ts:123-146`.
- Test command: `npm test` in `dashboard/`. Type check: `npm run check`.
- All paths below are relative to `dashboard/`.

---

### Task 1: `can()` gains a scope `level`

**Files:**
- Modify: `src/lib/stores/session.svelte.ts:230-238` (`CanScope`), `:148-162` (`can`)
- Test: `src/lib/stores/session.test.ts` (append a new `describe`)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: `type CanLevel = 'org' | 'project' | 'app' | 'env'` and `CanScope.level?: CanLevel`, both exported from `src/lib/stores/session.svelte.ts`. `sessionStore.can(perm: Permission, scope?: CanScope): boolean` keeps its signature; default `level` is `'app'`, which is byte-for-byte today's behaviour.

- [ ] **Step 1: Write the failing tests**

Append to `src/lib/stores/session.test.ts`. Follow the existing file's pattern for seeding `access` (see the `can() — environment-scoped checks` describe at `:634`).

```ts
describe('can() — scope level truncation', () => {
  // Mirrors rbac.rs's `org_scope_check_ignores_lower_scoped_grants`: a grant
  // narrower than the check can never satisfy it. Without `level`, every one
  // of these returned true and lit UI the server answers with 403.
  beforeEach(() => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
  });

  it('level org ignores a project grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'project', scope_id: 'proj-1', permissions: ['member:manage'] }],
    };
    expect(sessionStore.can('member:manage')).toBe(true); // default level 'app'
    expect(sessionStore.can('member:manage', { level: 'org' })).toBe(false);
  });

  it('level org ignores an app grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['org:manage'] }],
    };
    expect(sessionStore.can('org:manage', { level: 'org' })).toBe(false);
  });

  it('level project ignores an app grant but honours a project grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['monitor:read'] }],
    };
    expect(sessionStore.can('monitor:read', { level: 'project' })).toBe(false);
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'project', scope_id: 'proj-1', permissions: ['monitor:read'] }],
    };
    expect(sessionStore.can('monitor:read', { level: 'project' })).toBe(true);
  });

  it('an org grant satisfies every level', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'org', scope_id: 'org-1', permissions: ['issue:read'] }],
    };
    for (const level of ['org', 'project', 'app'] as const) {
      expect(sessionStore.can('issue:read', { level })).toBe(true);
    }
    expect(sessionStore.can('issue:read', { level: 'env', env: 'env-1' })).toBe(true);
  });

  it('an env grant satisfies no level above env', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };
    for (const level of ['org', 'project', 'app'] as const) {
      expect(sessionStore.can('issue:read', { level, env: 'env-1' })).toBe(false);
    }
    expect(sessionStore.can('issue:read', { level: 'env', env: 'env-1' })).toBe(true);
  });

  it('null access denies at every level', () => {
    sessionStore.access = null;
    for (const level of ['org', 'project', 'app', 'env'] as const) {
      expect(sessionStore.can('issue:read', { level, env: 'env-1' })).toBe(false);
    }
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm test -- session`
Expected: FAIL — `level` is not a property of `CanScope`, so `can('member:manage', { level: 'org' })` still returns `true`.

- [ ] **Step 3: Implement the level truncation**

In `src/lib/stores/session.svelte.ts`, extend the `CanScope` interface (currently at `:230`):

```ts
/**
 * How far down the scope cascade a check is allowed to look, mirroring which
 * ids the backend's matching `authorize_*` helper passes to `has_permission`.
 *
 * - `'org'`     — org grants only. `authorize_org` resolves at `(org, None, None, None)`,
 *                 so no narrower grant can ever satisfy it.
 * - `'project'` — org + project grants (`authorize_project`).
 * - `'app'`     — org + project + app grants (`authorize_app`). The DEFAULT, so
 *                 every pre-existing call site keeps its behaviour exactly.
 * - `'env'`     — all four. Requires an explicit `env`; see `can()`'s doc comment
 *                 for why `env` is never defaulted from `currentEnvId`.
 */
export type CanLevel = 'org' | 'project' | 'app' | 'env';

export interface CanScope {
  org?: string | null;
  project?: string | null;
  app?: string | null;
  // Deliberately not defaulted from `currentEnvId` the way org/project/app are
  // — see `can()`'s doc comment. Omit it entirely unless the check really is
  // an environment-scoped one.
  env?: string | null;
  level?: CanLevel;
}
```

Then rewrite the body of `can()` (currently `:148-162`), leaving its existing doc comment in place and appending a paragraph about `level`:

```ts
  can(perm: Permission, scope: CanScope = {}): boolean {
    if (!this.access) return false;
    const level = scope.level ?? 'app';
    const org = scope.org ?? this.currentOrgId ?? undefined;
    // A level narrower than the grant type zeroes that id out, exactly as the
    // backend passes `None` for every scope below the one it authorizes at.
    const project =
      level === 'org' ? undefined : (scope.project ?? this.currentProjectId ?? undefined);
    const app =
      level === 'org' || level === 'project'
        ? undefined
        : (scope.app ?? this.currentAppId ?? undefined);
    const env =
      level === 'env' && scope.env && scope.env !== 'none' ? scope.env : undefined;
    return this.access.grants.some((g) => {
      const scopeMatch =
        (g.scope_type === 'org' && g.scope_id === org) ||
        (g.scope_type === 'project' && g.scope_id === project) ||
        (g.scope_type === 'app' && g.scope_id === app) ||
        (g.scope_type === 'env' && env !== undefined && g.scope_id === env);
      return scopeMatch && g.permissions.includes(perm);
    });
  }
```

**Critical compatibility note:** the old code accepted an `env` at the default level. Every existing env-scoped call site (`can('issue:read', { env: … })` in `AppEnvPicker.svelte:61`, `EnvironmentsCard.svelte`, and the `session.test.ts` cases at `:634-724`) passes `env` without a `level`. To keep those green, `env` must be honoured when `scope.env` is explicitly supplied — so the `env` line above is:

```ts
    const env =
      (level === 'env' || scope.env !== undefined) && scope.env && scope.env !== 'none'
        ? scope.env
        : undefined;
```

Use that version. It reads as: an explicit `env` argument opts into the env level for that call, which is what the pre-existing call sites already meant.

- [ ] **Step 4: Run the full suite**

Run: `npm test`
Expected: PASS — the six new cases plus every pre-existing `can()` test at `session.test.ts:634-724`.

- [ ] **Step 5: Type check**

Run: `npm run check`
Expected: no new errors.

---

### Task 2: Surface a failed access fetch instead of swallowing it

**Files:**
- Modify: `src/lib/stores/session.svelte.ts:221-229` (`loadOrgScope`), plus the `reset()` method
- Modify: `src/lib/components/layout/AppShell.svelte:63-87`
- Test: `src/lib/stores/session.test.ts`

**Interfaces:**
- Consumes: Task 1's `CanScope`.
- Produces: `sessionStore.accessError: boolean` — `true` iff the most recent `getAccess` for the current org failed. Follows the exact shape and naming of the existing `environmentsError` flag (`session.svelte.ts:264`), including its "cleared at the start of every attempt" rule.

- [ ] **Step 1: Write the failing test**

```ts
describe('accessError', () => {
  it('is set when getAccess fails and cleared when it succeeds', async () => {
    mockListOrgs.mockResolvedValue([{ id: 'org-1', name: 'Acme', slug: 'acme' }] as never);
    mockListProjects.mockResolvedValue([]);
    mockGetAccess.mockRejectedValueOnce(new Error('network'));

    await sessionStore.load(true);
    expect(sessionStore.accessError).toBe(true);
    expect(sessionStore.access).toBe(null);

    mockGetAccess.mockResolvedValueOnce({ permissions: [], grants: [] });
    await sessionStore.load(true);
    expect(sessionStore.accessError).toBe(false);
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `npm test -- session`
Expected: FAIL — `sessionStore.accessError` is `undefined`.

- [ ] **Step 3: Implement**

Add the field beside `access` (`session.svelte.ts:~268`):

```ts
  // Access grants for the current org — drives every permission check.
  access = $state<AccessResponse | null>(null);
  // True iff the most recent `getAccess` for the current org failed. Distinct
  // from "loaded and genuinely holds no grants": both leave `access` unusable
  // and `can()` returning false for everything, but only one of them is an
  // error the user can retry. Since nav visibility and every button's enabled
  // state now derive from `can()`, collapsing the two would render a network
  // blip as a fully convincing "you have no permissions" UI. Mirrors
  // `environmentsError` — cleared at the start of every attempt so a stale
  // `true` from a previous org never leaks into the next.
  accessError = $state(false);
```

Rewrite `loadOrgScope`'s access fetch (`:222-226`):

```ts
  private async loadOrgScope(orgId: string): Promise<void> {
    this.accessError = false;
    const [access, projects] = await Promise.all([
      getAccess(orgId).then(
        (a) => a,
        () => {
          this.accessError = true;
          return null;
        },
      ),
      listProjects(orgId).catch(() => [] as Project[]),
    ]);
    this.access = access;
    // …rest unchanged
```

Add `this.accessError = false;` to `reset()` beside the existing `this.access = null;` (`:505`), and to the `orgs.length === 0` early-return branch in `performLoad` (`:199-209`) beside its `this.access = null;`.

- [ ] **Step 4: Run the tests**

Run: `npm test -- session`
Expected: PASS.

- [ ] **Step 5: Render it in `AppShell`**

In `src/lib/components/layout/AppShell.svelte`, insert a branch between the `!sessionStore.loaded` spinner and the `noAccess` empty state (`:74-81`):

```svelte
      {:else if sessionStore.accessError}
        <EmptyState
          title="Couldn't load permissions"
          description="We couldn't check what you have access to, so the dashboard is showing nothing rather than guessing. This is usually temporary."
          icon="triangle-alert"
        >
          {#snippet action()}
            <Button variant="primary" onclick={() => location.reload()}>Retry</Button>
          {/snippet}
        </EmptyState>
```

- [ ] **Step 6: Type check**

Run: `npm run check`
Expected: no new errors.

---

### Task 3: The `page-access` table

**Files:**
- Create: `src/lib/models/page-access.ts`
- Create: `src/lib/models/page-access.test.ts`

**Interfaces:**
- Consumes: Task 1's `CanLevel`, `CanScope`, `sessionStore.can`.
- Produces:
  - `interface PageAccess { perm: Permission; level: 'org' | 'project' | 'app'; title: string }`
  - `const PAGE_ACCESS: Record<string, PageAccess | null>`
  - `function resolvePageAccess(path: string): PageAccess | null`
  - `function canAccessPage(access: PageAccess | null): boolean`
  - `function lockedBy(perm: Permission, scope?: CanScope): Permission | null`
  - `function lockTitle(perm: Permission): string`

- [ ] **Step 1: Write the failing tests**

Create `src/lib/models/page-access.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { routes } from '../../routes';
import { ALL_PERMISSIONS } from './permissions';
import { PAGE_ACCESS, resolvePageAccess, findPageAccessKey, lockTitle } from './page-access';

// Routes that never mount AppShell, so they have no page-level permission.
const UNAUTHENTICATED = [
  '/login',
  '/register',
  '/forgot-password',
  '/reset-password',
  '/change-password',
  '/unsubscribe',
  '/',
  '*',
];

describe('PAGE_ACCESS', () => {
  it('only references permissions the backend recognises', () => {
    for (const [path, entry] of Object.entries(PAGE_ACCESS)) {
      if (!entry) continue;
      expect(ALL_PERMISSIONS, `${path} uses unknown permission ${entry.perm}`).toContain(
        entry.perm,
      );
    }
  });

  // NOTE: this asserts on findPageAccessKey, not resolvePageAccess. The latter
  // returns `PageAccess | null` and can never be `undefined`, so a coverage
  // check written against it would pass for every input and test nothing.
  it('covers every guarded route', () => {
    for (const path of Object.keys(routes)) {
      if (UNAUTHENTICATED.includes(path)) continue;
      const concrete = path.replace(/\/:[^/]+/g, '/x');
      expect(
        findPageAccessKey(concrete),
        `route "${path}" has no PAGE_ACCESS entry — add one (use null if it needs no permission)`,
      ).not.toBe(null);
    }
  });

  it('has no entry for a path that is not a real route', () => {
    const routeBases = new Set(
      Object.keys(routes)
        .filter((p) => p.startsWith('/') && p.length > 1)
        .map((p) => '/' + p.split('/')[1]),
    );
    for (const path of Object.keys(PAGE_ACCESS)) {
      expect(
        routeBases.has(path),
        `PAGE_ACCESS key "${path}" matches no route in routes.ts`,
      ).toBe(true);
    }
  });
});

describe('resolvePageAccess', () => {
  it('matches a base path exactly', () => {
    expect(resolvePageAccess('/issues')?.perm).toBe('issue:read');
  });

  it('strips trailing segments until a key matches', () => {
    expect(resolvePageAccess('/issues/abc-123')?.perm).toBe('issue:read');
    expect(resolvePageAccess('/persons/user-9')?.perm).toBe('event:read');
    expect(resolvePageAccess('/monitors/m-1')?.perm).toBe('monitor:read');
  });

  it('returns null for an ungated page', () => {
    expect(resolvePageAccess('/account')).toBe(null);
    expect(resolvePageAccess('/docs')).toBe(null);
  });

  it('returns null for an unknown path rather than failing closed', () => {
    expect(resolvePageAccess('/nope')).toBe(null);
  });

  it('gates org-only pages at org level', () => {
    expect(resolvePageAccess('/members')?.level).toBe('org');
    expect(resolvePageAccess('/storage')?.level).toBe('org');
    expect(resolvePageAccess('/alerts')?.level).toBe('org');
  });
});

describe('lockTitle', () => {
  it('names both the human label and the raw permission', () => {
    const title = lockTitle('issue:write');
    expect(title).toContain('Resolve, assign, and comment on issues');
    expect(title).toContain('issue:write');
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npm test -- page-access`
Expected: FAIL — cannot resolve `./page-access`.

- [ ] **Step 3: Implement `src/lib/models/page-access.ts`**

```ts
import type { Permission } from './index';
import { PERMISSION_LABELS } from './permissions';
import { sessionStore, type CanScope } from '../stores/session.svelte';

/**
 * What a user needs in order to see a page at all.
 *
 * `level` is the scope the permission must be held at, and it must match the
 * `authorize_*` helper the page's endpoints actually call in
 * `backend/bins/sauron-api/src/routes/`. Getting it wrong in the permissive
 * direction shows a page that only 403s; getting it wrong in the strict
 * direction hides a page the user could have used.
 */
export interface PageAccess {
  perm: Permission;
  level: 'org' | 'project' | 'app';
  title: string;
}

/**
 * Route base path → requirement, or `null` for "no permission needed".
 *
 * Keyed by BASE path, not by the router's parameterised path: `/issues`, not
 * `/issues/:id`. `resolvePageAccess` strips trailing segments to find a key,
 * which mirrors how `Sidebar`'s `match: (p) => p.startsWith(…)` predicates
 * already work.
 *
 * Every route in `routes.ts` must appear here — `page-access.test.ts` enforces
 * it — so that adding a page forces a deliberate decision about who sees it.
 * `null` is that decision, spelled out.
 */
export const PAGE_ACCESS: Record<string, PageAccess | null> = {
  // Monitor
  '/overview': { perm: 'event:read', level: 'app', title: 'Overview' },
  '/issues': { perm: 'issue:read', level: 'app', title: 'Exceptions' },
  '/performance': { perm: 'event:read', level: 'app', title: 'Performance' },

  // Explore
  '/events': { perm: 'event:read', level: 'app', title: 'Events' },
  '/sessions': { perm: 'event:read', level: 'app', title: 'Sessions' },
  '/users': { perm: 'event:read', level: 'app', title: 'Users' },
  '/persons': { perm: 'event:read', level: 'app', title: 'Users' },
  '/devices': { perm: 'event:read', level: 'app', title: 'Devices' },
  '/screens': { perm: 'event:read', level: 'app', title: 'Screens' },
  '/workflows': { perm: 'event:read', level: 'app', title: 'Workflows' },

  // Analyze. `/active-users` is project-level: active_users.rs:525 resolves
  // reach across every app in the project, not one app.
  '/active-users': { perm: 'event:read', level: 'project', title: 'Active users' },
  '/funnels': { perm: 'event:read', level: 'app', title: 'Funnels' },
  '/journeys': { perm: 'event:read', level: 'app', title: 'Journeys' },

  // Uptime. monitors.rs:67 authorizes at the project.
  '/monitors': { perm: 'monitor:read', level: 'project', title: 'Monitors' },

  // Alerting. notifications.rs:66,313 use authorize_org.
  '/alerts': { perm: 'alert:read', level: 'org', title: 'Alerts' },

  // Manage
  '/account': null, // self-scoped (/v1/me/*) — always reachable
  // projects.rs:49 is reach-based: an app-scoped member gets a filtered list,
  // not a 403, so this is the widest level rather than 'project'.
  '/projects': { perm: 'project:read', level: 'app', title: 'Projects' },
  // NB: no '/apps' key. Sidebar's Projects item lists '/apps' as an alternate
  // `match` prefix, but no such route exists in routes.ts and lookups here go
  // through `item.href` ('#/projects'), never through `match`. Adding it would
  // fail the "matches no route" drift test for no benefit.
  // orgs.rs:160 uses authorize_org — a project- or app-scoped member:read
  // grant genuinely cannot list members.
  '/members': { perm: 'member:read', level: 'org', title: 'Members' },
  '/settings': { perm: 'app:read', level: 'app', title: 'App settings' },
  // Listing artifacts only needs issue:read (artifacts.rs:189), but the page
  // exists to upload, so a member who can only list has nothing to do here.
  '/source-maps': { perm: 'artifact:write', level: 'app', title: 'Source Maps' },
  // admin.rs:30 uses authorize_org.
  '/storage': { perm: 'org:manage', level: 'org', title: 'Storage' },
  '/inspector': { perm: 'pii:read', level: 'app', title: 'Privacy' },

  '/onboarding': { perm: 'project:create', level: 'org', title: 'Onboarding' },
  '/docs': null, // integration guides — no data, always reachable
};

/**
 * The requirement for a concrete path, or `null` if the page needs none.
 *
 * Fails OPEN for an unknown path on purpose: the drift test guarantees every
 * real route has an entry, so an unknown path here is a typo in a link, and
 * showing the page (which the server will gate anyway) beats showing a
 * confusing denial for a page that has no requirement to name.
 */
export function resolvePageAccess(path: string): PageAccess | null {
  const key = findPageAccessKey(path);
  return key === null ? null : PAGE_ACCESS[key];
}

/**
 * The PAGE_ACCESS key a path resolves to, or `null` if it has no entry.
 *
 * Separate from `resolvePageAccess` because that function collapses "no entry"
 * and "an entry of null" into the same `null` return — fine for rendering,
 * useless for the drift test, which has to tell an undecided page from a
 * deliberately ungated one.
 */
export function findPageAccessKey(path: string): string | null {
  const clean = path.split('?')[0].replace(/\/+$/, '') || '/';
  const segments = clean.split('/').filter(Boolean);
  for (let i = segments.length; i > 0; i--) {
    const key = '/' + segments.slice(0, i).join('/');
    if (key in PAGE_ACCESS) return key;
  }
  return null;
}

/** Whether the current user satisfies a requirement. `null` is always allowed. */
export function canAccessPage(access: PageAccess | null): boolean {
  if (!access) return true;
  return sessionStore.can(access.perm, { level: access.level });
}

/**
 * `null` when the user may act, else the permission they are missing —
 * exactly the shape `Button`'s `lockedReason` prop wants, so a call site reads
 * `lockedReason={lockedBy('issue:write', { app })}` with no ternary.
 */
export function lockedBy(perm: Permission, scope?: CanScope): Permission | null {
  return sessionStore.can(perm, scope) ? null : perm;
}

/** Tooltip text for a locked control, for anything that can't take a Button prop. */
export function lockTitle(perm: Permission): string {
  const label = PERMISSION_LABELS[perm];
  return label ? `Requires: ${label} (${perm})` : `Requires: ${perm}`;
}
```

- [ ] **Step 4: Run the tests**

Run: `npm test -- page-access`
Expected: PASS. If `covers every guarded route` fails, the named route is missing from `PAGE_ACCESS` — add it rather than loosening the test.

- [ ] **Step 5: Type check**

Run: `npm run check`

---

### Task 4: `Button.lockedReason` and `PermissionDenied`

**Files:**
- Modify: `src/lib/components/ui/Button.svelte`
- Create: `src/lib/components/PermissionDenied.svelte`

**Interfaces:**
- Consumes: Task 3's `lockTitle`, `PageAccess`, `PAGE_ACCESS`, `canAccessPage`.
- Produces: `Button` prop `lockedReason?: Permission | null`. `PermissionDenied` props `{ access: PageAccess }`.

- [ ] **Step 1: Add `lockedReason` to `Button.svelte`**

Add the import and prop:

```svelte
  import Icon from './Icon.svelte';
  import type { Permission } from '../../models';
  import { lockTitle } from '../../models/page-access';
```

In `Props`:

```ts
    /**
     * The permission the user is missing, or `null` if they may act. Non-null
     * disables the button, prefixes a lock glyph, and sets a title naming the
     * permission — so a disabled control always explains itself, which a bare
     * `disabled` never does.
     */
    lockedReason?: Permission | null;
```

Destructure `lockedReason = null,` alongside the other props, then:

```ts
  const isDisabled = $derived(disabled || loading || lockedReason !== null);
  // The lock reason outranks a caller-supplied title: why you cannot act
  // matters more than the hint it replaces.
  const resolvedTitle = $derived(lockedReason ? lockTitle(lockedReason) : title);
```

Replace `{title}` with `title={resolvedTitle}` in **both** the `<a>` and `<button>` branches, and render the glyph inside the label span of the `<button>` branch:

```svelte
    <span class="label" class:hidden={loading}>
      {#if lockedReason}<span class="lock" aria-hidden="true"><Icon name="lock" size={13} /></span>{/if}
      {@render children()}
    </span>
```

Add to the style block:

```css
  .lock {
    display: inline-flex;
    vertical-align: -2px;
    margin-right: 5px;
  }
```

Note: folding `lockedReason` into `isDisabled` also routes a locked `href` button through the `<button>` branch (`Button.svelte:37` already tests `href && !isDisabled`), because an `<a>` cannot be disabled.

- [ ] **Step 2: Create `src/lib/components/PermissionDenied.svelte`**

```svelte
<!--
  Shown in place of a page the current user has no permission for.

  Deliberately keeps the URL: a bookmark that stops working should say so
  rather than silently redirect somewhere else, and if the client-side gate is
  ever wrong this shows a message instead of bouncing the user out of a page
  they could actually use.
-->
<script lang="ts">
  import EmptyState from './ui/EmptyState.svelte';
  import Button from './ui/Button.svelte';
  import { PERMISSION_LABELS } from '../models/permissions';
  import { PAGE_ACCESS, canAccessPage, type PageAccess } from '../models/page-access';

  interface Props {
    access: PageAccess;
  }

  let { access }: Props = $props();

  const requirement = $derived(
    PERMISSION_LABELS[access.perm]
      ? `${PERMISSION_LABELS[access.perm]} (${access.perm})`
      : access.perm,
  );

  // The first page the user can actually reach. `/account` and `/docs` carry a
  // null requirement precisely so this can never come up empty.
  const fallback = $derived(
    Object.entries(PAGE_ACCESS).find(([, entry]) => canAccessPage(entry)) ?? ['/account', null],
  );
  const fallbackTitle = $derived(
    (fallback[1] as PageAccess | null)?.title ?? 'Account',
  );
</script>

<EmptyState
  icon="lock"
  title="You don't have access to {access.title}"
  description="Requires: {requirement}. Ask an organization owner for access."
>
  {#snippet action()}
    <Button variant="primary" href="#{fallback[0]}">Back to {fallbackTitle}</Button>
  {/snippet}
</EmptyState>
```

- [ ] **Step 3: Type check**

Run: `npm run check`
Expected: no new errors.

- [ ] **Step 4: Run the suite**

Run: `npm test`
Expected: PASS (no behaviour change yet — nothing passes `lockedReason`).

**Coverage note:** the spec asked for a `Button` unit test. This project has no
component-testing harness — all 18 existing test files are pure TypeScript, and
there is no `@testing-library/svelte` or jsdom setup. Adding one is scope creep
for this change. `Button`'s locked behaviour is therefore covered by the
`lockTitle` unit test in Task 3 plus the runtime drive in Task 11, which is
where a rendering bug would actually show. Do not skip Task 11 on the
assumption that `npm test` covers this.

---

### Task 5: Sidebar and AppShell read the table

**Files:**
- Modify: `src/lib/components/layout/Sidebar.svelte:7-76`
- Modify: `src/lib/components/layout/AppShell.svelte`
- Modify: `src/routes.ts:116-122` (comment only)

**Interfaces:**
- Consumes: Task 3's `PAGE_ACCESS`, `resolvePageAccess`, `canAccessPage`; Task 4's `PermissionDenied`.
- Produces: nothing new.

- [ ] **Step 1: Rewrite Sidebar visibility**

Delete `show?: () => boolean` from the `NavItem` interface and delete every `show:` line from the `groups` array (7 of them: `:33`, `:35`, `:53`, `:63`, `:65`, `:66`, `:67`). Add the import and replace `visibleGroups`:

```ts
  import { resolvePageAccess, canAccessPage } from '../../models/page-access';

  // Visibility comes from PAGE_ACCESS, not a per-item predicate. Two
  // hand-maintained lists (this one and routes.ts) had already drifted; one
  // table means a page cannot be added to the nav without a deliberate
  // decision about who sees it, and page-access.test.ts fails if one is.
  const visibleGroups = $derived(
    groups
      .map((g) => ({ ...g, items: g.items.filter((i) => canAccessPage(resolvePageAccess(i.href.slice(1)))) }))
      .filter((g) => g.items.length > 0),
  );
```

(`i.href` is `'#/issues'`; `.slice(1)` yields `'/issues'`.)

- [ ] **Step 2: Render the denied state in AppShell**

Add imports:

```ts
  import PermissionDenied from '../PermissionDenied.svelte';
  import { resolvePageAccess, canAccessPage } from '../../models/page-access';
  import { location } from 'svelte-spa-router';
```

Add the derived:

```ts
  const pageAccess = $derived(resolvePageAccess($location));
  const pageDenied = $derived(sessionStore.loaded && !canAccessPage(pageAccess));
```

Insert the branch **after** `noAccess` and before `{:else}` (`AppShell.svelte:76-84`), so a member with zero reachable projects still gets the more useful "No apps available" message:

```svelte
      {:else if pageDenied && pageAccess}
        <PermissionDenied access={pageAccess} />
```

- [ ] **Step 3: Update the stale comment in `routes.ts`**

Replace the comment block at `:116-122` with:

```ts
  // Admin. Route-level conditions are deliberately NOT used for permissions:
  // AppShell resolves the current path through PAGE_ACCESS and renders
  // PermissionDenied in place of the page, which keeps the URL intact instead
  // of bouncing a bookmark somewhere else. The endpoints 403 regardless.
```

- [ ] **Step 4: Run the suite and type check**

Run: `npm test && npm run check`
Expected: PASS.

---

### Task 6: Convert layout + settings components from hide to lock

**Files:**
- Modify: `src/lib/components/layout/Topbar.svelte:64-65,90-91,105-106`
- Modify: `src/lib/components/settings/EnvironmentsCard.svelte:76-80,267,308,322,342,352`
- Modify: `src/lib/components/AppEnvPicker.svelte:61-62,106`

**Interfaces:**
- Consumes: Task 3's `lockedBy`, `lockTitle`; Task 4's `Button.lockedReason`.
- Produces: nothing new.

- [ ] **Step 1: Topbar**

This is the canonical hide→lock conversion; Tasks 7–10 repeat this exact shape.

Before:

```svelte
{#if sessionStore.can('project:create')}
  <Button size="sm" onclick={newProject}>New project</Button>
{/if}
```

After:

```svelte
<Button
  size="sm"
  lockedReason={lockedBy('project:create', { level: 'org' })}
  onclick={newProject}
>New project</Button>
```

Apply to both the "New project" and "New app" affordances, with
`lockedBy('app:create', { project: projectId, level: 'project' })` for the
latter. `projects.rs:102` authorizes project creation at the org and
`projects.rs:239` authorizes app creation at the project, hence the levels.

Add the import: `import { lockedBy } from '../../models/page-access';`

- [ ] **Step 2: EnvironmentsCard**

The five gated controls at `:267,308,322,342,352` become locked rather than hidden. Keep the existing scope arguments; add the level that matches the endpoint:
- `env:create` → `{ project: projectId, level: 'project' }` (`environments.rs:213`)
- `env:update` / `env:delete` at project scope → `{ project: projectId, level: 'project' }` (`environments.rs:285,323`)
- `env:update` / `env:rotate_key` at app scope → `{ app: appId, level: 'app' }` (`environments.rs:444,525`)

For the `<select>` and toggle controls that are not `Button`s, set `disabled` and `title={lockTitle(perm)}` and prefix an `<Icon name="lock" size={12} />` in the adjacent label. Delete the doc comment at `:65-75` that apologises for `can()` over-lighting — Task 1's `level` is the fix it was describing.

- [ ] **Step 3: AppEnvPicker**

At `:106` the "Unattributed" option is conditionally offered. Render it always, `disabled` when `lockedBy('event:read', { app: appId, level: 'app' })` is non-null, with `🔒` replaced by prefixing the option label with "Unattributed (locked)" — a `<select>` option cannot host an icon component.

- [ ] **Step 4: Run the suite and type check**

Run: `npm test && npm run check`

---

### Task 7: Convert Members and MembersTable

**Files:**
- Modify: `src/pages/Members.svelte:161,165,166,167,489`
- Modify: `src/lib/components/members/MembersTable.svelte:21,31,41,115,151,168,181,191,202`

**Interfaces:**
- Consumes: Task 3's `lockedBy`, `lockTitle`; Task 4's `Button.lockedReason`.
- Produces: `MembersTable` props change from `canManage: boolean` / `canRevokeSessions: boolean` / `canCredential: boolean` to `manageLock: Permission | null` / `revokeLock: Permission | null` / `credentialLock: Permission | null`.

- [ ] **Step 1: Add `level: 'org'` to every Members permission check**

`Members.svelte:161-167` currently calls `can('member:manage')`, `can('member:credential')`, `can('member:read')`, `can('role:manage')` with no level. All four endpoints use `authorize_org` (`orgs.rs:160,552,809,1020,1399`), so all four gain `{ level: 'org' }`. **This is a real behaviour change:** a project-scoped grant carrying these permissions stops lighting the UI, correctly.

- [ ] **Step 2: Keep the page-level gate, drop its bespoke empty state**

`Members.svelte:416` renders its own "No access" EmptyState when `member:read` is absent. `AppShell` now owns that (Task 5), so delete the bespoke state and the `:243` load-skip guard stays as-is (it avoids a pointless 403).

- [ ] **Step 3: Convert the action buttons**

`:430` (create-member card), `:499` (role editor), `:518` (non-system role rows), `:489` (password-reset menu item) go from `{#if …}` to `lockedReason={…}`. The password-reset item requires **both** `member:credential` and `member:manage` (`orgs.rs:1020` + `755`), so its reason is `lockedBy('member:credential', { level: 'org' }) ?? lockedBy('member:manage', { level: 'org' })`.

- [ ] **Step 4: Convert MembersTable's props and menu items**

Change the three boolean props to `Permission | null` locks and thread them into the six gated sites. `RowActionsMenu` items are caller-authored markup, so each locked item renders `disabled` with `title={lockTitle(lock)}` and an inline `<Icon name="lock" size={12} />`.

- [ ] **Step 5: Run the suite and type check**

Run: `npm test && npm run check`

---

### Task 8: Convert Projects, SettingsApp, SourceMaps, Storage

**Files:**
- Modify: `src/pages/Projects.svelte:44,161,185,194,195,196,231,236,296`
- Modify: `src/pages/SettingsApp.svelte:24-25,103,125`
- Modify: `src/pages/SourceMaps.svelte`, `src/pages/Storage.svelte`

**Interfaces:**
- Consumes: Task 3's `lockedBy`; Task 4's `Button.lockedReason`.
- Produces: nothing new.

- [ ] **Step 1: Projects**

`project:create` → `{ level: 'org' }` (`projects.rs:102`). `project:update` / `project:delete` → `{ project: p.id, level: 'project' }` (`projects.rs:146,162`). `app:create` → `{ project: p.id, level: 'project' }` (`projects.rs:239`). Convert `:161,185,231,236,296` from hide to lock.

- [ ] **Step 2: SettingsApp**

`app:update` / `app:delete` → `{ app: app.id, level: 'app' }` (`apps.rs:71,106` — both use the strict `authorize_app`). Convert `:103` (settings form submit) and `:125` (danger zone) from hide to lock.

- [ ] **Step 3: SourceMaps and Storage**

Audit both for ungated mutations. SourceMaps' upload and delete both need `artifact:write` at the app (`artifacts.rs:89,222`) — add `lockedReason` if absent. Storage is read-only; the page gate from Task 3 is sufficient, so expect no diff beyond confirming that.

- [ ] **Step 4: Run the suite and type check**

Run: `npm test && npm run check`

---

### Task 9: Convert IssueDetail, FunnelBuilder, Inspector

**Files:**
- Modify: `src/pages/IssueDetail.svelte:47,244`
- Modify: `src/pages/FunnelBuilder.svelte:44,332,372`
- Modify: `src/pages/Inspector.svelte:62,205,287,303,319,356,374,421,467`

**Interfaces:**
- Consumes: Task 3's `lockedBy`, `lockTitle`; Task 4's `Button.lockedReason`.
- Produces: nothing new.

- [ ] **Step 1: IssueDetail**

`issue:write` → `{ app: currentAppId, level: 'app' }` (`issues.rs:153` uses strict `authorize_app`). Convert the resolve/assign controls at `:244` from hide to lock.

- [ ] **Step 2: FunnelBuilder**

`funnel:write` → `{ app: currentAppId, level: 'app' }` (`funnels.rs:204,232,257`). Convert `:332,372`.

- [ ] **Step 3: Inspector**

`pii:manage` → `{ app: currentAppId, level: 'app' }`. Convert the six hidden sites (`:205,287,303,319,421,467`) to locked. `:356,374` already disable — give them a `lockedReason` so they gain the glyph and tooltip.

- [ ] **Step 4: Run the suite and type check**

Run: `npm test && npm run check`

---

### Task 10: Convert Alerts, Monitors, MonitorDetail

**Files:**
- Modify: `src/pages/Alerts.svelte:51,400,547,583,617,790,824`
- Modify: `src/pages/Monitors.svelte:34,92,181`
- Modify: `src/pages/MonitorDetail.svelte:33-34,129,150`

**Interfaces:**
- Consumes: Task 3's `lockedBy`; Task 4's `Button.lockedReason`.
- Produces: nothing new.

- [ ] **Step 1: Alerts**

`alert:write` → `{ level: 'org' }` (`notifications.rs:113,187,260,272,443,522,580` all use `authorize_org`). Convert all six sites from hide to lock.

- [ ] **Step 2: Monitors and MonitorDetail**

`monitor:write` → `{ project: projectId, level: 'project' }` (`monitors.rs:99,191,223`). MonitorDetail already scopes to `detail?.monitor.project_id`; keep that and add the level. Convert `:92,181,129,150`.

- [ ] **Step 3: Run the suite and type check**

Run: `npm test && npm run check`

---

### Task 11: Runtime verification

**Files:** none modified.

**Interfaces:**
- Consumes: everything above.
- Produces: a verification record.

The repo's own history is explicit that gating bugs of this class pass every static gate and only surface in a live session. Static checks are not sufficient evidence for this change.

- [ ] **Step 1: Bring up the stack**

Run the backend and dashboard per the project Makefile / docker-compose. Seed an org with three members: a Viewer (7 permissions), a Developer (18), and a member holding a single app-scoped grant.

- [ ] **Step 2: Drive as Viewer**

Log in. Expected: Alerts, Source Maps, Storage, Privacy absent from the nav; every write button visible but locked with a tooltip naming a real permission; no locked button is clickable.

- [ ] **Step 3: Drive as Developer**

Expected: Members visible but its management actions locked; Storage and Privacy absent; Alerts visible read-only.

- [ ] **Step 4: Drive as the app-scoped member**

Expected: nav items appear and disappear when switching apps in the Topbar; `#/members` typed directly shows `PermissionDenied` with the URL unchanged, not a blank page.

- [ ] **Step 5: Click every unlocked button in each role**

Expected: no 403. A 403 from an unlocked button means a `level` in `PAGE_ACCESS` or a call site is too permissive.

- [ ] **Step 6: Break the access endpoint**

Force `GET /v1/orgs/{org}/access` to fail (offline, or a proxy 500). Expected: "Couldn't load permissions" with a Retry button — **not** an empty sidebar with everything locked.

- [ ] **Step 7: Record the results**

Write what was observed for each role, including anything that did not match. Do not report the change as verified on the strength of `npm test` alone.
