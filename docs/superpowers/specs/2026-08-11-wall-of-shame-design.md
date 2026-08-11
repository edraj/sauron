# Wall of Shame — org-scoped admin audit log

**Date:** 2026-08-11
**Status:** approved, implementation authorized to run unattended
**Baseline:** branch `soheyb`, HEAD `d67b8f6`, clean tree

## Problem

Sauron records who revealed PII (`inspector_reveal_audit`) and who masked it
(`inspector_mask_actions`), and nothing else. Every other administrative
mutation — creating an environment, adding a member, resetting a password,
widening a role — leaves no trace at all. An org owner cannot answer "who
deleted that app, and when", and there is no record to consult after an
incident.

This adds a general admin audit trail and an admin page to read it.

## Locked decisions

Twelve decisions were taken before implementation. They are recorded here
because several of them are the kind that look arbitrary later.

1. **Capture scope — admin/config mutations.** Org, project, app, environment
   (including key rotation), member create/deactivate/password-reset/session-
   revoke, role create/update/delete, grants, alert rules and channels,
   monitors, source-map artifacts, tier policy. *Excluded:* product-data edits
   (issue triage, saved funnels, notification prefs) and auth events (login,
   logout). Rationale: issue triage is high-volume and would drown the security
   events the page exists to surface.
2. **Mechanism — explicit `audit::record()` per handler, plus a drift test.**
   Not middleware. Middleware cannot name the entity it just changed or diff
   it, and would record URLs rather than meaning. The forgetting risk that
   middleware would have solved is instead closed by a test (§6).
3. **No backfill.** The table starts empty. Existing rows' `created_at` would
   produce entries with no actor for actions nobody can attribute, and a
   reconstructed history is worse than an honestly empty one. The empty state
   names the date recording began so a blank Wall does not read as data loss.
4. **Read gate — org-scoped `org:manage`.** No new permission. Identical to
   the Storage page: you read the trail for orgs where you hold `org:manage`,
   which in the common single-tenant deployment is simply "the admin".
5. **Change detail — before/after diff of changed fields only**, as JSONB
   `{field: {from, to}}`, with a per-entity field allowlist (§4).
6. **Fail-open.** A failed audit write logs at `error` and the action proceeds.
   An audit-table problem must not take down member management.
7. **Retention — forever.** Admin actions are thousands per year. No prune job,
   no knob, nothing that can silently delete evidence.
8. **Identity snapshot — actor email and target name denormalized.** Without
   it, deleting a user would leave all of their past entries anonymous —
   precisely when the trail matters most. `repo::user_email` already exists for
   exactly this purpose.
8b. **No foreign keys except `org_id`.** Corrected during implementation. The
   first cut used `REFERENCES … ON DELETE SET NULL` on `actor_id`,
   `project_id` and `app_id`; that is wrong twice over. Deleting a project
   would blank `project_id` on every historical entry, so filtering by that
   project — the question you ask *because* it was deleted — silently returns
   nothing. And a delete handler could not record its own event, because the
   referenced row is already gone when the entry is written, so the insert
   would fail the FK and the deletion would go unrecorded through the
   fail-open path. Audit ids are inert snapshots, not live references.
9. **No IP / user-agent capture.** Behind the shipped nginx with
   `API_TRUST_FORWARDED_HEADERS=false` this records the proxy's address for
   every actor, so the column would be uniformly useless.
10. **Existing inspector audit tables — read-only union.** The Wall projects
    `inspector_reveal_audit` and `inspector_mask_actions` into the entry shape
    at read time. No data migration, no double-writing, no drift. The Privacy
    page keeps its own detailed views.
11. **UI — filterable table plus detail drawer.** Default view is everything in
    the current org; filters narrow it.
12. **Org partitioning.** Each org has its own history. The page follows the
    dashboard's existing active-org selection rather than adding a switcher.

## 1. Schema

Migration `2026-08-11-000050_audit_log`.

```sql
CREATE TABLE audit_log (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The partition. CASCADE: if the tenant is gone there is nobody left who
    -- could hold org:manage to read the trail.
    org_id           UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,

    -- No FK: every id here is an inert snapshot. See decision 8b.
    actor_id         UUID,
    actor_email      TEXT NOT NULL DEFAULT '',

    action           TEXT NOT NULL,          -- 'environment.create'
    entity_type      TEXT NOT NULL,          -- 'environment'
    entity_id        UUID,                   -- nullable: the entity may be gone
    entity_name      TEXT NOT NULL DEFAULT '',

    -- Filter axes, denormalized so filtering never joins and stays correct
    -- after the referenced row is deleted. No FKs — see decision 8b.
    project_id       UUID,
    project_name     TEXT NOT NULL DEFAULT '',
    app_id           UUID,
    app_name         TEXT NOT NULL DEFAULT '',
    environment_id   UUID,
    environment_name TEXT NOT NULL DEFAULT '',

    -- {field: {from, to}} for changed fields only. Allowlisted per entity.
    changes          JSONB NOT NULL DEFAULT '{}'::jsonb,

    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Indexes:

```sql
-- The default view and every filtered view page through this.
CREATE INDEX audit_log_org_time_idx ON audit_log (org_id, created_at DESC, id DESC);
CREATE INDEX audit_log_org_project_idx ON audit_log (org_id, project_id, created_at DESC)
    WHERE project_id IS NOT NULL;
CREATE INDEX audit_log_org_app_idx ON audit_log (org_id, app_id, created_at DESC)
    WHERE app_id IS NOT NULL;
CREATE INDEX audit_log_org_actor_idx ON audit_log (org_id, actor_id, created_at DESC);
CREATE INDEX audit_log_org_action_idx ON audit_log (org_id, action, created_at DESC);
```

`(org_id, created_at DESC, id DESC)` is the keyset tuple. `id` is the
tiebreaker: two entries written in the same transaction share a `created_at`
to microsecond precision, and an untiebroken keyset cursor would skip or
repeat one of them at the page boundary.

## 2. Action taxonomy

`action` is `entity.verb`, lowercase, dot-separated. The full set is a const
array in `sauron-api/src/audit.rs` so the drift test and the API's facet list
share one definition.

| entity_type | actions |
|---|---|
| `org` | `org.create` |
| `project` | `project.create`, `project.update`, `project.delete` |
| `app` | `app.create`, `app.update`, `app.delete` |
| `environment` | `environment.create`, `environment.update`, `environment.retire`, `environment.enrollment_update`, `environment.rotate_key` |
| `member` | `member.create`, `member.activate`, `member.deactivate`, `member.reset_password`, `member.revoke_sessions` |
| `role` | `role.create`, `role.update`, `role.delete` |
| `grant` | `grant.create`, `grant.update`, `grant.delete` |
| `alert_rule` | `alert_rule.create`, `alert_rule.update`, `alert_rule.delete` |
| `alert_channel` | `alert_channel.create`, `alert_channel.update`, `alert_channel.delete`, `alert_channel.test` |
| `monitor` | `monitor.create`, `monitor.update`, `monitor.delete` |
| `artifact` | `artifact.upload`, `artifact.delete` |
| `store` | `store.upsert`, `store.delete`, `store.sync` |
| `inspector_policy` | `inspector_policy.create`, `inspector_policy.update`, `inspector_policy.delete` |
| `tier_policy` | `tier_policy.update`, `tier_restore.create`, `tier_pin.release`, `tier_pin.extend` |

Read-time projections from the existing tables (decision 10) add
`pii.reveal`, `pii.mask_preview` and `pii.mask` under entity_type `pii`. These
are never written to `audit_log`.

## 3. Recording API

```rust
// sauron-api/src/audit.rs
pub struct Entry<'a> {
    pub org_id: Uuid,
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<Uuid>,
    pub entity_name: &'a str,
    pub project: Option<(Uuid, &'a str)>,
    pub app: Option<(Uuid, &'a str)>,
    pub environment: Option<(Uuid, &'a str)>,
    pub changes: serde_json::Value,
}

/// Record an administrative action. Fail-open by contract: a write failure is
/// logged at error level and swallowed, because an audit-table problem must
/// never block member management or project creation. Callers therefore do
/// NOT propagate its result — there is nothing useful for a handler to do.
pub async fn record(conn: &mut AsyncPgConnection, actor: Uuid, entry: Entry<'_>);
```

Actor email is resolved inside `record` via the existing `repo::user_email`,
so no handler has to remember the snapshot. One extra SELECT per audited
action is acceptable at this volume.

Recording happens **after** the action's transaction commits, not inside it.
Inside, a fail-open audit error would still abort the caller's transaction —
which is fail-closed by accident, and the exact opposite of decision 6.

## 4. Diff allowlist

`changes` is built by a per-entity allowlist, never by serializing the whole
entity. The allowlist is the security boundary: it is what guarantees an
ingest key, a channel secret, or a password hash can never reach a table that
org admins can read and that is kept forever.

| entity | allowlisted fields |
|---|---|
| project | `name`, `slug` |
| app | `name`, `platform` |
| environment | `name`, `ingest_enabled`, `is_default`, `retired_at` |
| member | `is_active`, `email` |
| role | `name`, `permissions` |
| grant | `role_id`, `scope_type`, `scope_id` |
| alert_rule | `name`, `enabled`, `threshold`, `window_minutes`, `monitor_id` |
| alert_channel | `name`, `kind`, `enabled` |
| monitor | `name`, `url`, `interval_seconds`, `enabled` |
| tier_policy | `hot_days` |

Explicitly excluded and asserted by test: `public_key`, `config`,
`webhook_url`, `password_hash`, any `*_secret`, any `*_token`.

`environment.rotate_key` records that a rotation happened and by whom. It
never records either key.

## 5. Read API

`GET /v1/admin/audit`

Gate: org-scoped `org:manage` for the requested org. `org_id` is required and
validated against the caller's grants; there is no deployment-wide view, so
one tenant can never observe another's activity.

Query parameters, all optional except `org_id`:

| param | meaning |
|---|---|
| `org_id` | required; the org whose history to read |
| `project_id`, `app_id`, `environment_id` | narrow to one project / app / environment |
| `actor_id` | narrow to one person |
| `action` | exact action string |
| `entity_type` | entity family |
| `from`, `to` | RFC3339 bounds on `created_at` |
| `cursor` | opaque keyset cursor `(created_at, id)` |
| `limit` | default 50, max 200 |

Response:

```jsonc
{
  "entries": [ /* AuditEntry */ ],
  "next_cursor": "…",       // null on the last page
  "facets": {
    "actors":  [{ "id": "…", "email": "…" }],
    "actions": ["environment.create", …],
    "projects":[{ "id": "…", "name": "…" }],
    "apps":    [{ "id": "…", "name": "…" }]
  }
}
```

Facets are computed from the org's own entries so the dropdowns only ever
offer values that would return results. All filters are bound parameters.

The two inspector tables are projected into the same shape with a `UNION ALL`
over a subquery, ordered and paginated as one stream. Their rows are marked
`source: "inspector"` so the drawer can explain why they carry no diff.

## 6. Drift test

The failure mode this design must defend against is somebody adding a
mutating endpoint next month and never wiring it to the audit log — the
feature silently stops being complete, and every test still passes.

`tests/audit_coverage.rs` reads `main.rs` via `include_str!`, extracts every
handler named inside `post(…)`, `put(…)`, `patch(…)` and `delete(…)`, and
asserts each appears in either `AUDITED_HANDLERS` or `AUDIT_EXEMPT`. Adding a
route without classifying it fails the build.

`AUDIT_EXEMPT` carries a one-line reason per entry (auth endpoints, ingest,
product-data edits per decision 1), so the exemption list is reviewable rather
than a dumping ground.

A second test asserts every allowlist in §4 excludes the forbidden field names.

## 7. Dashboard

New page `dashboard/src/pages/WallOfShame.svelte`, route
`/admin/wall-of-shame`, tenth entry in `ADMIN_NAV` (icon `scroll-text`), with
a matching `PAGE_ACCESS` entry requiring org-level `org:manage` so the sidebar,
the admin rail and the in-page gate cannot disagree.

Layout:

```
Wall of Shame
Every administrative action taken in <org>, newest first.

[ Project ▾ ] [ App ▾ ] [ Environment ▾ ] [ Who ▾ ] [ Action ▾ ] [ Last 7 days ▾ ]  [Clear]  ⟳

When            Who                Action              Target            Where
2m ago          soheyb@…           Created environment staging           Acme / checkout
1h ago          admin@…            Updated role        Developer         Acme
…
                                                                    [ Load more ]
```

- Default range is **last 7 days** with an explicit "All time" option, so the
  first paint is bounded on an org with a long history.
- Row click opens a drawer: full timestamp, actor, action, target, scope, and
  the before→after diff rendered as a two-column table. Inspector-sourced rows
  show their detail fields instead and a note pointing at the Privacy page.
- Empty state distinguishes "no activity yet" (naming the date recording
  began) from "no results for these filters" (offering Clear).
- Filter state lives in the URL query string so a filtered view is linkable.

Uses the existing `DataTable`, `Button`, `Card`, `EmptyState`, `Spinner` and
`Icon` components — no raw `<button>`/`<table>`, per house convention.

## 8. Testing

Backend:
- repo round trip: insert an entry, read it back with each filter axis.
- keyset pagination across a same-`created_at` boundary (the tiebreaker test).
- authorization: a caller without `org:manage` in the target org gets 403; a
  caller with it in org A cannot read org B by passing its id.
- fail-open: `record` against a broken connection leaves the caller's action
  successful.
- the two drift tests of §6.
- one instrumentation test per entity family asserting the action produces an
  entry with the right action, target and diff.

Frontend: `svelte-check` clean, plus unit tests for the filter→query-string
model and the diff renderer.

Runtime: migrations applied to real Postgres, a real admin action driven
through the live API, and the resulting row confirmed visible and correctly
filtered in the browser. Backend tests must run with the Bash sandbox disabled
— sandboxed runs have their own netns and every DB-backed test returns early
while printing `ok`.

## 9. Out of scope

CSV export, alerting on audit events, cross-org views, retention configuration,
and capturing auth events. None are precluded by this schema.
