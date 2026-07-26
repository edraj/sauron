# Member lifecycle & role editing

Date: 2026-07-26
Status: implemented 2026-07-26

## Problem

Adding a person to an organization today is a two-step, out-of-band flow with an
unavoidable side effect:

1. The person self-registers at `POST /v1/auth/register`. `org_name` is required
   and `register` unconditionally calls `create_org`, so **they get a throwaway
   organization** where they are Owner.
2. Someone with `member:manage` grants them a role by email at
   `POST /v1/orgs/{org_id}/grants`. Until then `create_grant` returns
   `400 "no user with that email (ask them to sign up)"`
   (`backend/bins/sauron-api/src/routes/auth.rs:222`,
   `backend/bins/sauron-api/src/routes/orgs.rs:180`).

Three capabilities are missing:

- **Create** a member directly, without the self-registration detour.
- **Remove** a member's ability to log in. The per-grant `DELETE /v1/grants/{id}`
  revokes one grant; there is no account-level control.
- **Edit** anything. A member's role/scope requires remove-then-re-grant, and a
  custom role's permissions are immutable after `POST /v1/orgs/{org}/roles`.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| How is an account created? | Admin creates it with a **system-generated temp password**, shown once | Admin-chosen password (weak reuse, admin knows a durable credential); full invitation flow (needs email delivery, not wired) |
| What does "delete member" mean? | **Deactivate** — keep the row, block login, keep grants | Hard delete (destroys audit trail and `user_id` references); drop-all-grants (row vanishes while the account squats the unique email index, unrecoverable via UI) |
| What gets an edit button? | **Both** a member's role/scope and a custom role's permissions | — |
| Can an admin set a durable password? | **No.** Temp passwords only, and the holder is forced to change it | — |

## Non-goals

- Email delivery. Temp passwords are handed over out-of-band.
- Invitation tokens or an accept page.
- Editing system preset roles. They are re-synced from `rbac.rs` at every API
  boot (`ensure_preset_roles`, `backend/crates/sauron-auth/src/rbac.rs:347`,
  called from `main.rs:85`), so edits would not survive a restart.
- Registering into an existing org. `register` keeps creating a new org.

---

## 1. Migration `2026-07-26-000023_member_lifecycle`

```sql
ALTER TABLE users ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE users ADD COLUMN must_change_password BOOLEAN NOT NULL DEFAULT false;
```

`down.sql` drops both columns. Existing users default to active with no forced
change, so the migration is a no-op for current accounts.

Update `backend/crates/sauron-db/src/schema.rs:228` (`users` table) and the
`User` struct at `backend/crates/sauron-db/src/models.rs:74`. Both new fields
serialize (unlike `password_hash`, which is `#[serde(skip_serializing)]`) — the
dashboard needs them for the badge and the forced-change redirect.

Deactivation reuses `refresh_tokens.revoked_reason` from migration 22. Add
`REVOKE_DEACTIVATED: &str = "deactivated"` alongside the existing `REVOKE_REUSE`
in `backend/crates/sauron-db/src/repo.rs`.

## 2. Create member

`POST /v1/orgs/{org_id}/members` — requires `member:manage` at org scope.

```json
{ "email": "…", "name": "…", "role_id": "uuid",
  "scope_type": "org|project|app", "scope_id": "uuid" }
```

Returns `201 { "user_id": "uuid", "grant_id": "uuid", "temp_password": "…" }`.

Behavior:

- Generate a 16-character password from `rand::rngs::OsRng` over an
  unambiguous alphabet (no `0/O`, `1/l/I` — it gets read aloud and retyped).
- Hash with the existing Argon2 helper used by `register`.
- Insert the user with `must_change_password = true`, `is_active = true`.
- Create the initial grant **in the same transaction**, so a failure cannot
  leave an orphan account squatting the `users_email_lower_key` index.
- Reuse `create_grant`'s existing guards verbatim: role must be a preset or
  owned by this org (`orgs.rs:187`), scope target must live in this org
  (`orgs.rs:200`), and the caller must hold every permission of the granted
  role at that scope (`orgs.rs:228`). Creating a user must not become an
  escalation path around the grant checks.
- `409 Conflict` if the email already exists, with a message pointing at Grant
  access. Matching is case-insensitive via the existing lower(email) index.
- Rate-limit per caller, mirroring `REGISTER_ATTEMPTS_PER_HOUR`
  (`auth.rs:29`).

`temp_password` is returned exactly once, in this response. It is never stored
in plaintext and no endpoint can retrieve it again. If it is lost the admin
deactivates and recreates, or the member uses a future reset flow.

## 3. Deactivate / reactivate

`PATCH /v1/orgs/{org_id}/members/{user_id}` — requires `member:manage` at org.

```json
{ "is_active": false }
```

**This is not the existing per-grant Remove, and does not replace it.** Remove
revokes one grant. Deactivate is an account-level login kill switch that
**leaves all grants intact**, so the member stays visible in the table with a
"Deactivated" badge and a Reactivate button. Dropping grants *and* deactivating
would make the row disappear from every UI surface while an unusable account
kept holding the email — with no way back.

Guards, each returning `409 Conflict` with a distinct message:

- **Cross-org**: refuse if the target holds any `role_grant` with
  `org_id != {org_id}`. One org's admin must not be able to lock someone out of
  an org they do not control. In a single-org deployment this never fires.
- **Self**: refuse deactivating your own account.
- **Last owner**: refuse if the target is the last holder of `org:manage`,
  reusing `repo::count_org_manage_grants_excluding` (`repo.rs:380`).

On deactivate, revoke all the user's refresh tokens with
`REVOKE_DEACTIVATED`. Reactivate (`{"is_active": true}`) only flips the flag;
the member logs in fresh.

### Login and refresh

`login` (`auth.rs:251`) checks `is_active` **only after the password verifies**,
then returns `403 account_deactivated`. Checking before the verify would make
the endpoint an account-existence oracle — the handler already burns a dummy
Argon2 verify on unknown emails specifically to avoid that (`auth.rs:283-298`),
and an early `is_active` branch would reintroduce the timing gap it closes.
Someone who does not know the password learns nothing.

`refresh` performs the same check, so an existing access token cannot be
extended past deactivation.

## 4. Forced password change

`POST /v1/auth/password` — authenticated, self-only.

```json
{ "current_password": "…", "new_password": "…" }
```

Verifies `current_password`, applies the same 8..=256 length bound as
`register`, sets the new hash, and clears `must_change_password`.

It then revokes **all** the user's refresh tokens and returns a fresh token
pair, same shape as `login`. Keeping the caller's existing session would not
work: their access token still carries the `must_change_password` claim, so the
§4 gate below would keep rejecting them until it expired, immediately after they
did the one thing it was demanding. Reissuing also logs out every other device,
which is the right outcome when the old credential may be compromised.

**Enforcement is server-side.** A dashboard redirect alone is bypassable with a
curl against the API, which would leave the admin holding a working durable
credential for someone else's account — exactly what decision 4 forbids. So:

- `must_change_password` becomes a claim in the access token.
- The auth extractor rejects every request carrying that claim with
  `403 password_change_required`, allowlisting only `POST /v1/auth/password`
  and `POST /v1/auth/logout`.

A temp password therefore grants no capability except replacing itself, which
is why it needs no separate expiry.

The dashboard reads the flag from the login response and routes to a
non-dismissable change-password screen before any authenticated page renders.

## 5. Edit a member's grant

`PATCH /v1/grants/{grant_id}` — requires `member:manage` in the grant's org.

```json
{ "role_id": "uuid", "scope_type": "project", "scope_id": "uuid" }
```

All fields optional; omitted fields keep their current value. Applied in one
transaction rather than client-side delete-then-recreate: a failed recreate
would silently strand the member with no access, and the last-owner guard has
to evaluate the **final** state, not the intermediate one where the grant is
already gone.

Guards: the same role-ownership, scope-containment, and no-escalation checks as
`create_grant`, evaluated against the new values, **plus** the escalation check
against the *old* role — otherwise a caller could edit away a grant whose role
outranks them. Last-`org:manage` guard applies if the edit removes that
permission. Unique constraint `(user_id, role_id, scope_type, scope_id)` may
reject the edit as a duplicate of an existing grant → `409`.

## 6. Edit a custom role

`PATCH /v1/orgs/{org_id}/roles/{role_id}` — requires `role:manage` at org.

```json
{ "name": "…", "description": "…", "permissions": ["issue:read", "…"] }
```

Guards:

- `404` if `role.org_id != {org_id}` (no cross-org edits).
- `403` if `role.is_system` — presets are read-only. `GET` still returns them so
  the UI can display their permissions greyed out.
- Every permission in the new set must be one the caller holds at org scope, and
  every permission being **removed** must likewise be one they hold. Both
  directions matter: widening is escalation, and narrowing a role you do not
  outrank lets a Developer defang an Admin role.
- Reject an edit that would strip `org:manage` from the org's last holder.
- Validate every string against `perm::ALL` (`rbac.rs:56`) → `400` on unknown.

Editing a role immediately changes access for everyone holding it. The UI
states how many members are affected before saving.

## 7. Frontend

### Permission catalog drift — fix first

`dashboard/src/pages/Members.svelte:23` lists **16 of the 23** permissions in
`perm::ALL`. Missing: `funnel:write`, `artifact:write`, `source:read`,
`monitor:read`, `monitor:write`, `alert:read`, `alert:write`.

Harmless for *create* today, but a role editor that submits the full checkbox
state would **silently strip those 7 from any role that has them** on first
save. This must land before §6 ships.

Replace the inline array with `lib/models/permissions.ts` exporting all 23 in
`rbac.rs` order, grouped by resource for rendering. A test asserts the list
matches the backend catalog so the two cannot drift again.

### Component split

`Members.svelte` is 546 lines and this work roughly doubles it. Extract into
`dashboard/src/lib/components/members/`:

| Component | Responsibility |
|---|---|
| `CreateMemberDialog.svelte` | Email/name/role/scope form; renders the returned temp password once with a copy button and a "this will not be shown again" warning |
| `EditMemberDialog.svelte` | Change a member's role/scope; add an additional grant |
| `RoleEditorDialog.svelte` | Create **and** edit a role; read-only for presets |
| `PermissionPicker.svelte` | Checkbox grid grouped by resource, used by `RoleEditorDialog` |

`Members.svelte` keeps data loading, permission gating, and composition.

### Table regrouped by member

Today the table renders **one row per grant**, so a person with three grants
appears three times — and would show three identical Deactivate buttons. It
regroups to **one row per member**, with their grants as chips in the Scope
column, each chip carrying its own edit/remove affordance.

Row actions: Edit (`member:manage`), Deactivate/Reactivate (`member:manage`),
per-chip Remove (existing `DELETE /v1/grants/{id}`). Deactivated members render
with a badge and dimmed styling. The Roles list gains a per-row Edit button
(`role:manage`), disabled with a tooltip on system presets.

All controls gate through the existing `sessionStore.can(...)`
(`Members.svelte:62-64`) — no new client-side permission logic.

Use house UI components (`Button`, `DataTable`, `Modal`, `Badge`), not raw
elements, per the dashboard conventions.

### API client

`dashboard/src/lib/api/orgs.ts` gains `createMember`, `setMemberActive`,
`updateGrant`, `updateRole`. `lib/api/auth.ts` gains `changePassword`.
`MemberGrant` in `lib/models/index.ts:152` grows `is_active` and the model
gains a grouped `Member` type.

## Error handling

| Case | Status | Note |
|---|---|---|
| Create with existing email | 409 | Points at Grant access |
| Grant/role/edit escalation | 403 | Same shape as existing `create_grant` denial |
| Deactivate self / cross-org / last owner | 409 | Distinct message per case |
| Login while deactivated | 403 `account_deactivated` | Only after password verifies |
| Any request with `must_change_password` | 403 `password_change_required` | Except `/v1/auth/password`, `/v1/auth/logout` |
| Edit a system role | 403 | |
| Unknown permission string | 400 | |
| Duplicate grant after edit | 409 | |

## Testing

**Constraint:** the repo has no DB-backed test harness. Every Rust test is an
inline `#[cfg(test)] mod tests` unit test, and CI runs `cargo test --workspace`
(`.github/workflows/ci.yml:59`) with **no Postgres service**. Tests needing a
live database cannot run here, so this work introduces none.

That pushes the design in a useful direction: **guards become pure functions
over already-fetched data rather than logic tangled into handlers.** Handlers
stay thin — fetch, call guard, persist. Extract into
`backend/crates/sauron-auth/src/guard.rs`:

| Function | Purpose |
|---|---|
| `role_permissions(&Value) -> Vec<String>` | Parse the `permissions` JSONB array. Currently duplicated verbatim at `orgs.rs:231` and `orgs.rs:276`; this DRYs it and gives it a test |
| `check_no_escalation(caller, required) -> Result<(), AuthError>` | The `orgs.rs:241` / `orgs.rs:300` loop, named once |
| `check_role_edit(caller, old_perms, new_perms) -> Result<(), AuthError>` | Both directions: added *and* removed permissions must each be held |
| `generate_temp_password(rng) -> String` | Takes an RNG so tests are deterministic |
| `scope_parts(scope_type, scope_id, ancestry) -> (Option<Uuid>, Option<Uuid>)` | The `orgs.rs:200` / `orgs.rs:288` match |

Unit tests, no DB required:

- `generate_temp_password`: 16 chars; alphabet excludes `0O1lI`; two calls
  differ; deterministic under a seeded RNG.
- `check_no_escalation`: superset passes; missing one permission fails; empty
  required passes.
- `check_role_edit`: adding a permission you lack fails; **removing** one you
  lack fails (a Developer must not be able to defang the Admin role); no-op
  passes.
- `role_permissions`: valid array; empty array; non-array JSONB → empty, never
  a panic.
- `scope_parts`: org/project/app each map correctly.
- `Claims` serde: a token minted **before** this change (no
  `must_change_password` field) still decodes, defaulting to `false`. Without
  `#[serde(default)]` every live session breaks on deploy.
- Catalog parity: a Rust test asserting `perm::ALL.len() == 23`, and a vitest
  asserting the TS constant equals the same 23 strings in the same order.

**End-to-end verification is manual**, via the project's harness pattern, and is
the real gate for DB-dependent behavior (transaction rollback, the cross-org
guard, token revocation). Run: create a member → copy the temp password →
confirm every endpoint but change-password is blocked → change it → confirm the
fresh token works → edit their role → edit a custom role → deactivate → confirm
login refused → reactivate → confirm login works.

Frontend: verify the flow end-to-end in the running dashboard per the project's
harness verification pattern — create a member, read the temp password, log in
as them, get forced through the change screen, edit their role, deactivate,
confirm login is refused, reactivate.

## Files

**New**
- `backend/migrations/2026-07-26-000023_member_lifecycle/{up,down}.sql`
- `backend/crates/sauron-auth/src/guard.rs` — pure, unit-tested guard functions
- `dashboard/src/lib/components/members/{CreateMemberDialog,EditMemberDialog,RoleEditorDialog,PermissionPicker}.svelte`
- `dashboard/src/lib/models/permissions.ts`
- `dashboard/src/pages/ChangePassword.svelte`

**Modified**
- `backend/bins/sauron-api/src/routes/orgs.rs` — create member, patch member, patch grant, patch role
- `backend/bins/sauron-api/src/routes/auth.rs` — `is_active` in login/refresh, change-password
- `backend/bins/sauron-api/src/main.rs` — 4 routes
- `backend/crates/sauron-auth/src/extractors.rs` — `must_change_password` claim + gate
- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}`
- `dashboard/src/pages/Members.svelte` — split, regroup, new actions
- `dashboard/src/lib/api/{orgs.ts,auth.ts}`, `dashboard/src/lib/models/index.ts`
- `dashboard/src/routes.ts` — change-password route
- `dashboard/src/pages/Docs.svelte:1127` — document the new flow

## Follow-ups (out of scope)

- Self-service password reset by email.
- Real invitation tokens, once email delivery exists.
- Audit log of member lifecycle events.
- Temp password expiry, if the `password_change_required` gate proves
  insufficient in practice.
