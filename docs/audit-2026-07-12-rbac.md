# Security & Performance Audit — Multi-App + RBAC

Date: 2026-07-12 · Scope: the new Project→App hierarchy and fine-grained RBAC. All findings below were fixed and re-verified against a live backend.

## Security findings (fixed)

### 1. IDOR — reading another app's error events via a foreign issue id (High)
`GET /v1/apps/{app}/issues/{issue}/events` authorized the **app** but then fetched error events by `issue_id` without confirming the issue belonged to that app. A user with access to app A could read app B's error events (potentially another org's) by passing B's issue id.
**Fix:** the handler now calls `get_issue(app_id, issue_id)` first and returns `404` if the issue is not in the authorized app. Verified: cross-app request → `404`, no leak. (`bins/sauron-api/src/routes/issues.rs`)

### 2. Privilege escalation via custom roles (High)
`POST /v1/orgs/{org}/roles` only required `role:manage`. An Admin (who lacks `org:manage`) could create a custom role **containing** `org:manage` and assign it to themselves.
**Fix:** a new role's permissions must be a **subset of the creator's own org-scope permissions**. Verified: Admin creating a role with `org:manage` → `403`; a role of permissions the Admin holds → `200`. (`routes/orgs.rs::create_role`)

### 3. Privilege escalation via grants (High)
`POST /v1/orgs/{org}/grants` let any `member:manage` holder assign **any** role, including Owner — so an Admin could grant Owner (which has `org:manage`) to escalate.
**Fix:** the granter must hold **every permission the granted role confers, at the grant's scope** (`role.permissions ⊆ granter.effective_at(scope)`). Verified: Admin granting Owner → `403`; granting Developer (⊆ Admin) → `200`. (`routes/orgs.rs::create_grant`)

### 4. Brute-force on login (Medium)
No throttle on `POST /v1/auth/login`.
**Fix:** a per-account fixed-window limit (10/min) backed by Redis; over the limit → `429`. Verified. (`routes/auth.rs::login`)

## Verified-correct (no change needed)

- **Tenant isolation / cascade:** `authorize_app`/`authorize_project` resolve the resource's real ancestry from the DB and check grants only in that org; a project grant covers its apps but not sibling projects; an app grant is narrowest. Confirmed live (Viewer→403 write, Developer@app→200 that app / 403 siblings, cross-org grant→400).
- **Cross-org grant prevention:** grant scope targets are validated to belong to the org.
- **Custom-role permission validation:** unknown permission strings → `400`.
- **SQL injection:** all queries use the diesel builder or parameterized `sql_query().bind()`; no user input is interpolated into SQL (including the `event_series` dynamic query, which binds `$1..$3`).
- **Secret handling:** `password_hash` is `#[serde(skip)]`; `refresh_tokens` are not serialized and only their SHA-256 is stored; refresh tokens rotate on use; JWT is identity-only with authz resolved per request (immediate revocation). `JWT_SECRET` is required in compose.
- **Ingest auth:** write-only public key, per-app rate limit, permissive CORS is acceptable (key-authed, no cookies).

## Performance

- **Permission resolution** loads a user's grants for one org (`role_grants` filtered by `user_id, org_id`) and unions in memory — O(grants), typically a handful of rows. Added a composite index `role_grants (user_id, org_id)` (migration `0003`) for the hot path.
- **App-scoped requests** issue ~3 small indexed queries (get_app + ancestry + grants). Acceptable; a combined `app + ancestry` query is a possible future micro-optimization.
- **Members page** uses a single 3-table join (no N+1).
- **Ingest hot path** caches DSN→app resolution in Redis (5-min TTL), invalidated on key rotation / app update / delete.
- No unbounded queries: list endpoints clamp `limit`.

## Deferred (noted, not blocking)

- Prevent removing the **last Owner** grant of an org (lockout guard).
- Per-(user,org) permission cache in Redis with grant-change invalidation, if resolution ever shows up hot under load.
- Register/org-creation abuse throttling (login is now throttled; signup is not).
