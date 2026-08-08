# Admin view and role management

**Date:** 2026-08-06
**Status:** Approved design, not yet implemented

## Problem

Three related gaps in the dashboard's administrative surface:

1. **No admin section.** Members, Projects, Storage, Privacy, Source Maps,
   Alerts and App settings are seven flat top-level routes scattered across the
   Sidebar's "Manage" and "Uptime" groups. There is no single place an
   administrator goes to administer the org.

2. **Role management is incomplete and cramped.** `role:manage` gates create and
   update, but **role deletion does not exist anywhere in the stack** — no
   route, no repo function, no client function, no UI. Custom roles are
   create-and-edit-forever. There is also no way to base a new role on an
   existing one, so building "Developer but without deletes" means ticking 18
   checkboxes by hand.

3. **The permission picker does not scale.** `PermissionPicker.svelte` renders
   30 checkboxes across 10 always-expanded `<fieldset>` groups with no
   collapse, no select-all, and no search. The catalogue only grows.

Environment management (`env:create` / `env:update` / `env:delete` /
`env:rotate_key`) is fully built and correctly enforced, but it is buried
inside App settings — you must first select an app, then scroll, to rotate a
key. It belongs in the admin section as a project-wide catalogue.

## Scope

**In scope:** the admin section IA, the role lifecycle (create/rename/mutate/
delete/copy), the permission picker rework, and a project-wide Environments
screen.

**Out of scope:** the Active Users chart rework (no bars, per-app/per-env
series). That touches `repo.rs:9911`'s deliberate `GROUP BY day` collapse and
needs a multi-series chart component that does not exist. It gets its own spec.

**No new permission string.** The four verbs in the original request
(create/rename/mutate/revoke) are completed under the existing `role:manage`
rather than split into `role:create` / `role:update` / `role:delete`. Splitting
would multiply the bidirectional escalation guard at `guard.rs:78` — which
checks the *symmetric difference* of a role edit, so both adding and removing a
permission require holding it — into a three-way matrix, and would need a
migration backfilling three permissions onto every holder of the old one. The
capability that is actually missing is delete, not granularity.

---

## Section A — Admin IA

### A1. The blocker: nested routes are currently impossible

`page-access.test.ts:85` asserts every `PAGE_ACCESS` key matches a route base,
where the base set is derived as:

```js
ROUTE_PATHS.filter((p) => p.startsWith('/') && p.length > 1)
           .map((p) => '/' + p.split('/')[1])
```

Only **first-segment** keys are legal. A `'/admin/members'` key fails this test.
A single `'/admin'` key would pass, but cannot express seven different
requirements (`member:read`@org, `org:manage`@org, `pii:read`@app, …).

**Fix:** widen `routeBases` to every param-free prefix of each route path:

| Route | Contributes |
|---|---|
| `/admin/members` | `/admin`, `/admin/members` |
| `/issues/:id` | `/issues` |
| `/issues` | `/issues` |

A segment containing `:` terminates the prefix walk, so param routes keep
contributing only their static parent — preserving today's behaviour exactly.
`findPageAccessKey` already strips right-to-left and needs no change.

This is a prerequisite for everything else in Section A and must land first.

### A2. Route table

All entries `guarded()`. Permissions are unchanged from today's values — this
is a move, not a re-gating.

| New route | From | Permission | Level |
|---|---|---|---|
| `/admin` | *(new)* | `null` | — |
| `/admin/members` | `/members` | `member:read` | org |
| `/admin/roles` | *(new, split from Members)* | `member:read` | org |
| `/admin/projects` | `/projects` | `project:read` | app |
| `/admin/environments` | *(new)* | `env:read` | project |
| `/admin/settings` | `/settings` | `app:read` | app |
| `/admin/source-maps` | `/source-maps` | `artifact:write` | app |
| `/admin/alerts` | `/alerts` | `alert:read` | org |
| `/admin/storage` | `/storage` | `org:manage` | org |
| `/admin/privacy` | `/inspector` | `pii:read` | app |

`/admin/roles` is `member:read`@org because that is what `list_roles` is gated
on today (`orgs.rs:1380`). Note this means any Viewer can enumerate custom
roles and their permission sets; that is pre-existing behaviour and this spec
does not change it.

`/admin/environments` is `env:read`@project per `environments.rs:196`.

`/admin` is `null` — the `/account` precedent. No single permission expresses
"can reach at least one admin child", and inventing one would drift from the
nine child gates. It renders `AdminIndex.svelte`, which redirects to the first
child the user can reach and renders a plain `EmptyState` if none — not
`PermissionDenied`, whose copy names exactly one permission, which a 9-way
union failure has no single correct choice for.

### A3. Shell and navigation

**`AdminShell.svelte`** composes the existing `AppShell` and adds a sub-nav
rail. Each admin page swaps its `<AppShell>` wrapper for `<AdminShell>`,
forwarding `requireProject` / `requireApp` unchanged.

Sub-nav visibility reuses `canAccessPage(resolvePageAccess(href))` — the exact
filter `Sidebar.svelte:80` already applies. One predicate, so the rail, the
sidebar and the in-page gate cannot drift.

**Sidebar changes:**

- "Manage" group is replaced by an "Admin" group holding one item, `#/admin`.
- Account moves to the bottom block beside Docs. It is `null`-gated
  self-service (`/v1/me/*`), not administration, and `PermissionDenied` relies
  on at least one always-reachable page existing.
- "Uptime" keeps only Monitors, since Alerts moves under Admin.

### A4. Legacy paths

The seven old paths remain as bare `Redirect` entries carrying `to`, so
bookmarks and any hardcoded links survive. They mount no `AppShell` and
therefore have no page permission.

Add a `LEGACY_REDIRECTS` array to `page-access.test.ts` alongside
`UNAUTHENTICATED`, exempt from the "covers every guarded route" assertion.
A second array rather than extending `UNAUTHENTICATED`, whose name would
otherwise become a lie.

### A5. Environments screen

`/admin/environments` is a project-wide catalogue: every environment in the
project, which apps are enrolled, and per-app ingest keys with rotate.

`EnvironmentsCard.svelte`'s logic **moves** here rather than being duplicated.
App settings keeps only rename / ingest-toggle / delete. Two components
mutating the same resource is how two UIs come to disagree.

The five existing locks carry over verbatim
(`EnvironmentsCard.svelte:77-81`) — note the deliberate split: create / rename
/ retire are project-scoped (catalogue operations) while update / rotate are
app-scoped (`/v1/app-environments/{id}`).

---

## Section B — Role management

### B1. Backend: delete

`DELETE /v1/orgs/{org_id}/roles/{role_id}` → `orgs::delete_role_handler`, plus
`repo::delete_role` filtered on `org_id` **and** `is_system = false`, mirroring
`update_role`'s defence in depth.

Guards, in this order — matching `update_role_handler` at `orgs.rs:1459` so
cross-tenant existence is never confirmed:

1. `ROLE_MANAGE` at org scope.
2. System preset → 400 `"system roles cannot be deleted"`. Checked **before**
   the cross-org check, since preset existence is already public.
3. Role belongs to another org → 404.
4. **Anti-sabotage: the caller must hold every permission the role confers.**
   `guard::check_role_edit(&own, &old_perms, &[])` — deleting a role *is*
   removing all of its permissions, so it is an edit to the empty set and takes
   the same guard the edit path takes at `orgs.rs:1493`.

   Added after review found the original four-guard list left a live escalation
   hole. The shipped `Admin` preset holds `role:manage` but not `org:manage`
   (`rbac.rs:129`), and both sibling handlers already refuse the smaller version
   of this operation: `check_role_edit` blocks editing a role to strip
   `org:manage` ("sabotage… disable everyone above them", `guard.rs:75-76`), and
   `delete_grant` blocks removing a single grant conferring a permission the
   caller lacks ("an Admin could delete the Owner's grant and evict them from
   their own org", `orgs.rs:642`). Without this guard, `DELETE` achieved in one
   call the strictly larger version of what those two refuse — stripping
   `org:manage` from every holder of a role at once.

   Effect: an Admin can still delete a Developer-shaped custom role, but not one
   granting `org:manage`.
5. Would drop the org's last `org:manage` holder → 409. Does NOT call
   `guard::drops_org_manage` — that helper takes an `old`/`new` permission pair
   and this path has no `new` (the role is gone, not edited), so
   `delete_role_handler` inlines the equivalent check directly:
   `old_perms.iter().any(|p| p == perm::ORG_MANAGE)`, then
   `repo::count_org_manage_grants_excluding_role`. Behaviourally identical to
   `drops_org_manage(&old_perms, &[])` for this `new = []` case, just written
   inline rather than through the shared helper. Retained even with guard 4 in
   place: guard 4 stops a *lesser* caller, while this one stops an Owner who
   holds `org:manage` from deleting the last role that confers it.

The response returns the count of `role_grants` rows cascaded, so the UI can
report what was actually revoked.

`role_grants.role_id` is `ON DELETE CASCADE`, so the delete itself needs no
grant cleanup — but that cascade is precisely why the UI below is a typed
confirmation rather than a plain one.

### B2. Delete UX

A dedicated `DeleteRoleDialog.svelte`. `ConfirmDialog` cannot be reused: it
takes a plain `message: string` and has no input.

- Names the role and states "N members will lose this access". N comes from the
  `roleMemberCounts` map computed today at `Members.svelte:144`; that
  derivation moves to `/admin/roles` along with the rest of the Roles card in
  B4, so B2 landing first should treat it as temporarily still living in
  Members.
- Requires typing the role name to enable the danger button.
- On success, reports the cascaded grant count returned by the API.

### B3. Copy UX

A Copy action in each role row's `RowActionsMenu`.

The no-escalation guard (`orgs.rs:1409`) requires the creator to hold every
permission at org scope, so copying Owner as an Admin would 403. Rather than
silently trimming the copy — which produces a role quietly different from the
one named — the action is **disabled** when the source role holds any
permission the caller lacks, via `lockedReason` naming the first blocking
permission.

An enabled copy opens the create dialog prefilled with the source's full
permission set and the name `Copy of ‹role›`. Because copy is only enabled when
every permission is held, the subsequent create can never fail escalation.

Copying a **system preset is allowed and is the primary use case** — "Developer
but without deletes" starts as a copy of Developer. Presets are read-only to
edit, not to read.

### B4. Roles screen

Roles move out of the card at `Members.svelte:506` onto `/admin/roles`:
a table of name, description, permission count, member count, and a `system`
badge, with a `RowActionsMenu` per row offering Edit / Copy / Delete.

Row action gating:

| Row type | Edit | Copy | Delete |
|---|---|---|---|
| System preset | "View" (never locked, read-only dialog) | enabled if caller holds all perms | 400 — not offered |
| Custom | locked by `role:manage` | enabled if caller holds all perms | locked by `role:manage` |

`RoleEditorDialog.svelte` keeps its existing read-only path for presets
(`is_system === true`), including the explanation that presets are re-synced
from `rbac.rs` at every API boot.

### B5. Permission picker rework

`PermissionPicker.svelte` gains three things. The catalogue-order emit at
`PermissionPicker.svelte:15` — `PERMISSION_GROUPS.flatMap(...).filter(...)` —
stays untouched, so the stored array remains stable regardless of interaction.

**Collapse.** Each of the 10 `PERMISSION_GROUPS` becomes a disclosure section
following `ScopeTree.svelte`'s existing idiom: a `<button aria-expanded={open}>`
summary row with `{#if open}` content. Not `<details>`/`<summary>`, which
appears nowhere in this codebase.

Sections start **collapsed except those with a selection**, so editing an
existing role shows its shape at a glance while creating a blank one starts
fully collapsed.

**Per-section select-all.** A tri-state checkbox in each summary row:
checked when all of the group's permissions are selected, indeterminate when
some are, unchecked when none. Toggling sets or clears the whole group.
`indeterminate` is a DOM property, not an attribute, so it must be bound rather
than templated.

**Search.** An `Input` filtering across permission strings and
`PERMISSION_LABELS` text. Matching groups auto-expand; non-matching groups
hide. Clearing search restores the collapse state from before the search began
rather than recomputing it, so a search does not silently reorganise the form.

The `disabled` prop continues to propagate to every input and to the select-all
checkboxes, so the read-only preset view stays genuinely read-only.

---

## Data flow

Unchanged in shape. The dashboard's `can()` mirror, `PAGE_ACCESS`, and the
`authorize_*` call sites all keep their current semantics. This spec adds one
endpoint (`DELETE` role), moves seven routes, adds three screens
(`AdminIndex`, `/admin/roles`, `/admin/environments`), and reworks one
component.

The only cross-cutting change is `page-access.test.ts`'s `routeBases`
derivation (A1), which is a widening: every key legal today stays legal.

## Error handling

| Case | Behaviour |
|---|---|
| Delete a system preset | 400, action not offered in UI |
| Delete another org's role | 404 (existence not confirmed) |
| Delete the last `org:manage` source | 409, dialog surfaces the reason |
| Copy a role with permissions you lack | Action disabled, tooltip names the blocking permission |
| `/admin` with no reachable child | Plain `EmptyState` (not `PermissionDenied` — see A2) |
| Legacy path hit | Redirect to the `/admin/*` equivalent |
| `getAccess` failed | Existing `accessError` branch — never renders as "you have no permissions" |

## Testing

**Backend:**
- `delete_role` unit tests for each guard branch: preset → 400, cross-org →
  404, **anti-sabotage → 403** (a caller holding `role:manage` but not
  `org:manage`, deleting a role that confers `org:manage`), **missing
  `role:manage` → 403**, last-owner → 409, happy path returns the cascaded
  grant count.

  The anti-sabotage case must seed a *second* `org:manage` source so the 409
  guard cannot fire first — otherwise the test passes for the wrong reason and
  proves nothing about guard 4.
- Integration test in `http_orgs.rs` confirming the cascade actually revokes
  grants and that a second delete of the same id returns 404.

**Frontend:**
- `page-access.test.ts` continues to pass with the widened `routeBases`, and
  gains coverage that `/admin/members` resolves to its own entry rather than
  falling back to `/admin`.
- Unit tests for the tri-state select-all (all / some / none) and for search
  restoring prior collapse state.
- `permissions.test.ts` is untouched — no permission string changes.

**Runtime verification** (this repo's standing rule: green tests are not
evidence a UI path works):
- Drive `/admin` as a user with exactly one accessible child and confirm the
  redirect target.
- Drive delete on a role held by ≥1 member and confirm the member actually
  loses access.
- Confirm every legacy path redirects.

## Build order

1. **A1** — widen `routeBases` in `page-access.test.ts`. Prerequisite; nothing
   nested can land before it.
2. **A2–A4** — routes, `AdminShell`, `AdminIndex`, sidebar, legacy redirects.
   Pure move; no behaviour change. Verifiable on its own.
3. **A5** — Environments screen; remove `EnvironmentsCard` from App settings.
4. **B1–B2** — backend delete + `DeleteRoleDialog`.
5. **B4** — `/admin/roles` screen, Roles card removed from Members.
6. **B3** — Copy action (depends on B4's row menu).
7. **B5** — PermissionPicker rework.

Steps 2 and 4 are independently shippable; the rest build on them.

## Decisions locked during design

| Decision | Choice | Why |
|---|---|---|
| Spec scope | Admin + RBAC now, Active Users separate | No shared code surface; Active Users needs backend SQL work and its own dialogue |
| Admin structure | Nested routes + sub-nav | Deep-linkable, unlike the existing non-linkable in-page tab idiom |
| Pages moved | All seven, plus new Roles and Environments | Leaves Manage holding only Account |
| Permission shape | Complete `role:manage` with delete | Smallest escalation surface; delete is the actual gap |
| "Revoke" | Meant env management → dedicated Environments screen | Env perms already exist and are enforced |
| Role delete | Typed confirmation, cascade | Explicit intent without forcing a reassignment flow |
| Role copy | Disabled when caller lacks a permission | A silently-trimmed copy is a role different from the one named |
| Picker default | Collapsed except groups with selections | Shows an existing role's shape without expanding everything |
| Picker search | Yes | 30 permissions today, growing |
| Environments | Move, not coexist | Two components mutating one resource is how UIs disagree |
