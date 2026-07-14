# Multi-Project / Apps + Fine-Grained RBAC — Design

Date: 2026-07-12 · Status: approved

## Problem

Today the ingest key (DSN) lives on `projects`, and every signal is attributed to `project_id`. Customers need to group several distinct SDK integrations under one product ("Project X = 3 Flutter apps + 2 webapps"), switch between products, and control access with fine granularity (admin of Project X, viewer of Project Y).

## Decisions (locked)

- **Ingest boundary = App.** Hierarchy becomes `Organization → Project → App(app_type)`. Each App owns a DSN; signals are attributed to `app_id`.
- **app_type** is a validated enum, **cosmetic** (icon/label): `web, flutter, ios, android, react_native, node`.
- **RBAC = permission-based.** Atomic permissions → roles (permission bundles) → 4 seeded preset roles + custom roles (API-only this pass).
- **Grants are scoped** at `org | project | app`, resolved by **union down the tree** (cascade).
- **Dashboard**: core UI (switchers, project/app CRUD, members with preset-role grants at any scope, permission-aware hiding). Custom-role *builder* deferred.
- **People/analytics identity: per-app** (cross-app profiles are future work).

## Data model (target)

```
organizations
  projects (id, org_id, name, slug)                         -- grouping, NO DSN
    apps (id, project_id, name, slug, app_type,
          public_key UNIQUE, ingest_enabled)                -- the ingest unit / DSN
      environments (id, app_id, name)
      issues / error_events / analytics_events /
      event_users / identities   -> keyed by app_id
roles (id, org_id NULL=preset, name, description, is_system, permissions jsonb)
role_grants (id, org_id, user_id, role_id, scope_type, scope_id)   -- replaces org_members
refresh_tokens (unchanged)
```

DSN: `http://<app_public_key>@<host>/<app_id>`.

Migration `0002_projects_apps_rbac`: rename `projects`→`apps` (+`app_type`,`project_id`); create the new `projects` grouping and backfill one project per existing app; rename `project_id`→`app_id` on all signal tables; add `roles`+`role_grants`; seed the 4 presets; convert `org_members` → org-scoped `role_grants`. (No production data exists; backfill is included for correctness/pattern.)

## RBAC

**Permissions:** `issue:read, issue:write, event:read, app:read, app:create, app:update, app:delete, app:rotate_key, project:read, project:create, project:update, project:delete, member:read, member:manage, role:manage, org:manage`.

**Preset roles:**
| Role | Permissions |
|---|---|
| Owner | all (incl. `org:manage`) |
| Admin | all except `org:manage` |
| Developer | `issue:read/write, event:read, app:read/create/update/rotate_key, project:read, member:read` |
| Viewer | `*:read` (`issue:read, event:read, app:read, project:read, member:read`) |

**Resolution:** authorize `(perm, scope_type, scope_id)` by walking the resource's ancestry (app→project→org) and unioning the permission sets of all the caller's grants matching org / that project / that app. Allow iff `perm ∈ union`. Computed per request from the DB (no permissions in the JWT → immediate revocation). Org creator seeded `Owner @ org`.

**Enforcement:** one helper `require_permission(conn, user_id, perm, scope_type, scope_id)`, replacing `require_org_role`/`require_project_access`.

## API changes

- Projects: `GET/POST /v1/orgs/{org}/projects`, `GET/PATCH/DELETE /v1/projects/{project}`.
- Apps: `GET/POST /v1/projects/{project}/apps`, `GET/PATCH/DELETE /v1/apps/{app}`, `POST /v1/apps/{app}/rotate-key`.
- Signals move under the app: `/v1/apps/{app}/issues…`, `/events/top`, `/persons/{id}`, `/first-event`, `/environments`.
- Access: `GET /v1/orgs/{org}/members`, `POST /v1/orgs/{org}/grants`, `DELETE /v1/grants/{id}`, `GET /v1/roles`, `POST /v1/orgs/{org}/roles` (API-only), `GET /v1/me/access` (grants + effective permissions for UI gating).
- Every handler gated by `require_permission`.

## Dashboard

Project switcher + App switcher; Projects and Apps CRUD (app: type, DSN, rotate/disable); Members page (invite, assign preset role at org/project/app scope, remove); onboarding = project → app → DSN → poll. UI reads `/v1/me/access` and hides actions the caller can't perform.

## Testing

Exhaustive unit tests for permission resolution: each preset role's permission set; each scope; cascade (org grant → all projects/apps; project grant → its apps only, not siblings; app grant → that app only); union of multiple grants; deny cases; cross-project isolation. Plus app-model + existing fingerprint/envelope tests. E2E through compose: project→app→DSN→ingest→app-scoped issue; Viewer 403 on resolve; project-scoped Developer allowed on Project X, 403 on Project Y.

## Out of scope (this pass)

Custom-role visual builder, cross-app person unification, org ownership transfer UI, per-permission audit log.
