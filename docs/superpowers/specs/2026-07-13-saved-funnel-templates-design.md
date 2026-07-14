# Design: Saved funnel templates

**Date:** 2026-07-13
**Status:** Approved (brainstorming) — pending spec review
**Area:** `backend/` (migration, sauron-db, sauron-auth, sauron-api) + `dashboard/`

## Goal

Funnels today are ephemeral: a definition is just an ordered list of event names (`steps: string[]`), computed on demand via `POST /v1/apps/{app_id}/funnel` and never stored. This feature lets users **save a funnel as a named template**, **reuse** it (load + re-run), **clone** it (duplicate, then modify), and **edit/delete** it — as a **shared, app-scoped team library**.

## Decisions (settled in brainstorming)

- **App-scoped, not cross-app/project/org.** A template's steps are event names that only exist within one app, so a template can't meaningfully run on an app that never emits those events.
- **Shared team library.** Any member who can read the app (`event:read`) sees all saved funnels; writes are gated (see Permissions) so Viewers can't delete the team's templates.
- **Template stores `name` + `description` + `steps` only** — *not* the date range. The range stays a live view control, so a template runs against whatever window the user is viewing.
- **Clone = create** (no dedicated endpoint). Two flows, both hit `POST .../funnels`: a one-click Duplicate (`Copy of {name}`), and load → edit → "Save as new".

## Data model

New migration **`backend/migrations/2026-07-13-000006_saved_funnels/{up,down}.sql`** and a `diesel::table!` entry in `backend/crates/sauron-db/src/schema.rs`.

```sql
-- up.sql
CREATE TABLE saved_funnels (
  id          uuid PRIMARY KEY,
  app_id      uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  name        text NOT NULL,
  description text,
  steps       jsonb NOT NULL,          -- ordered array of event-name strings
  created_by  uuid REFERENCES users(id) ON DELETE SET NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_saved_funnels_app ON saved_funnels(app_id, updated_at DESC);
-- down.sql
DROP TABLE saved_funnels;
```

- `steps` is a JSON array of strings, validated **2–10 entries** on write (same bounds as `compute`).
- No unique-name constraint — clones produce `Copy of {name}`; `created_by` + `updated_at` disambiguate in the UI.
- `ON DELETE SET NULL` on `created_by` so deleting a user doesn't cascade-delete their team templates.
- `diesel::table!` maps `steps` to `Jsonb`, `description`/`created_by` as `Nullable<...>`, following the existing `event_users` / `sessions` table definitions.

## Permissions (`sauron-auth`)

Read uses the existing **`event:read`**. Add a new atomic permission **`funnel:write`** for create/update/delete:

- `crates/sauron-auth/src/rbac.rs`: add `pub const FUNNEL_WRITE: &str = "funnel:write";`, include it in `perm::ALL`, and add it to the **Owner** (via `ALL`), **Admin**, and **Developer** preset permission lists — **not Viewer**.
- `ensure_preset_roles` (runs at API startup) syncs preset roles from these lists, so preset roles gain `funnel:write` on boot — **no preset-seed migration edit required**. (Custom roles are unaffected, by design.)
- Update the exhaustive `rbac` unit tests for the new permission (e.g. any test asserting the full `ALL` set or Viewer's exact permission set).

## Backend API (`sauron-api`)

All handlers in `bins/sauron-api/src/routes/funnels.rs`, reusing `AuthUser`, `authorize_app`, `db(&state)`. CRUD lives on the **plural collection** `funnels`; the existing **singular** compute action stays at `funnel`.

- `GET    /v1/apps/{app_id}/funnels` — `authorize_app(..., EVENT_READ)` → `Json<Vec<SavedFunnel>>`, ordered `updated_at DESC`. Includes `created_by_name` via `LEFT JOIN users`.
- `POST   /v1/apps/{app_id}/funnels` — `authorize_app(..., FUNNEL_WRITE)`; body `{ name, description?, steps }`; validate name non-empty and `2 <= steps.len() <= 10` (reuse/extract the existing compute validation); `created_by = auth.user_id`; returns the created `SavedFunnel`.
- `PATCH  /v1/apps/{app_id}/funnels/{funnel_id}` — `FUNNEL_WRITE`; updates `name`/`description`/`steps` (same validation); bumps `updated_at`; 404 if the funnel isn't in this app.
- `DELETE /v1/apps/{app_id}/funnels/{funnel_id}` — `FUNNEL_WRITE`; 404 if not in this app; 204/empty on success.
- `POST   /v1/apps/{app_id}/funnel` (compute) — **unchanged.**

Register the four routes in `bins/sauron-api/src/main.rs` next to the existing funnel route.

DTOs (`Serialize`/`Deserialize`, snake_case): `SavedFunnel { id, name, description: Option<String>, steps: Vec<String>, created_by_name: Option<String>, created_at, updated_at }`, `SaveFunnelReq { name, description: Option<String>, steps: Vec<String> }`, `UpdateFunnelReq { name, description: Option<String>, steps: Vec<String> }`.

## Repo (`sauron-db`)

Diesel query-builder functions (mirroring existing app-scoped CRUD style):

- `list_saved_funnels(conn, app_id) -> QueryResult<Vec<SavedFunnelRow>>` — join `users` for `created_by_name`.
- `create_saved_funnel(conn, app_id, created_by, name, description, steps) -> QueryResult<SavedFunnelRow>` — id via `ids::uuid_v7()`.
- `update_saved_funnel(conn, app_id, id, name, description, steps) -> QueryResult<usize>` — scoped by `app_id AND id`, sets `updated_at = now()`.
- `delete_saved_funnel(conn, app_id, id) -> QueryResult<usize>` — scoped by `app_id AND id`.

`steps` serialized to/from `serde_json::Value` (`Jsonb`).

## Frontend (`dashboard/`)

### Models (`src/lib/models/index.ts`)
```ts
export interface SavedFunnel {
  id: string; name: string; description?: string | null;
  steps: string[]; created_by_name?: string | null;
  created_at: string; updated_at: string;
}
```

### API client (`src/lib/api/funnels.ts`, extend)
- `listSavedFunnels(appId) => GET  /v1/apps/{appId}/funnels`
- `saveFunnel(appId, { name, description?, steps }) => POST /v1/apps/{appId}/funnels`
- `updateFunnel(appId, id, { name, description?, steps }) => PATCH /v1/apps/{appId}/funnels/{id}`
- `deleteFunnel(appId, id) => DELETE /v1/apps/{appId}/funnels/{id}`

### `FunnelBuilder.svelte`
- A **Saved funnels** panel (loaded via `$effect` on `currentAppId`) listing templates with `name`, step count, `created_by_name`, `updated_at`, and actions: **Load** (sets `steps` + recomputes), **Duplicate** (POST a `Copy of {name}` copy), **Rename/Edit**, **Delete** (confirm).
- A **Save** action in the builder → a small name/description dialog → `saveFunnel`.
- Track the currently-loaded template id in state. When a loaded template's steps/name are edited, surface **Update** (PATCH) and **Save as new** (POST). With no template loaded, only **Save** shows.
- Gate write actions with the existing `sessionStore.can('funnel:write', { app: currentAppId })` helper so Viewers see a read-only library; the backend enforces regardless. Add `'funnel:write'` to the `Permission` union in `src/lib/models/index.ts`.
- The existing ad-hoc build + compute flow is otherwise unchanged.

## Testing

- **Backend** (`cargo test`): repo CRUD round-trip (create → list → update → delete, app-scoping enforced); an authz test — a Viewer (`event:read` only) can `GET` but receives 403 on `POST`/`PATCH`/`DELETE`; steps-count validation (reject <2 / >10); cross-app isolation (can't PATCH/DELETE a funnel belonging to another app). Updated `rbac` unit tests for `funnel:write`.
- **Frontend**: `svelte-check` 0 errors + `vite build` clean.
- **End-to-end** (compose, API :10000 / dashboard :10002): save a funnel, reload the page, load it, duplicate it, edit + Save-as-new, delete; confirm a Viewer login sees the library read-only.

## Scope guardrails (YAGNI — out)

- No cross-app / project / org sharing.
- No versioning / change history.
- No per-user private templates (library is shared).
- No scheduled or emailed funnel reports.
- No dedicated clone endpoint (clone = create).
- Date range is **not** persisted with the template.

## Files touched (summary)

- `backend/migrations/2026-07-13-000006_saved_funnels/{up,down}.sql` (new).
- `backend/crates/sauron-db/src/schema.rs` — `saved_funnels` table.
- `backend/crates/sauron-db/src/repo.rs` — 4 CRUD fns + row struct.
- `backend/crates/sauron-auth/src/rbac.rs` — `funnel:write` perm + preset lists + tests.
- `backend/bins/sauron-api/src/routes/funnels.rs` — 4 handlers + DTOs.
- `backend/bins/sauron-api/src/main.rs` — 4 route registrations.
- `dashboard/src/lib/models/index.ts` — `SavedFunnel`.
- `dashboard/src/lib/api/funnels.ts` — 4 client fns.
- `dashboard/src/pages/FunnelBuilder.svelte` — saved-funnels panel + save/update/clone/delete.
- Backend + frontend tests as above.
