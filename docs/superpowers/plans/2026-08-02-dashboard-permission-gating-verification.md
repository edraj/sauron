# Runtime verification record — dashboard permission gating

Verified 2026-08-02 against a live stack: API on `:8090` (docker
`sauron-api-task12`), Postgres + Redis in docker, dashboard dev server on
`:3001`. Three users seeded in the **Acme** org (2 projects, 3 apps), each with
exactly one grant.

| User | Grant | Scope |
|---|---|---|
| `gate-viewer@gate.test` | Viewer | org |
| `gate-dev@gate.test` | Developer | org |
| `gate-appscoped@gate.test` | Developer | app `mobile` only |

Password for all three: `GateTest123!pw`. They are left in the dev database so
this can be re-run.

## Results

**Viewer — nav hiding.** 17 of 20 items shown. Hidden: Alerts (`alert:read`),
Source Maps (`artifact:write`), Storage (`org:manage`), Privacy (`pii:read`).
The Uptime group collapsed to Monitors alone. Matches `PAGE_ACCESS` exactly.

**Viewer — action locking.** On `/projects`: "New project", and per-project
"Rename" / "Delete", all rendered visible + `disabled` + lock glyph, with
`title` naming both the human label and the raw permission, e.g.
`Requires: Edit project settings (project:update)`.

**Viewer — blocked deep link.** `#/inspector` kept its URL and rendered
"You don't have access to Privacy / Requires: View PII scan findings … (pii:read)"
with a working "Back to Overview" fallback.

**App-scoped member — the `level` fix.** Active users, Monitors, Members,
Alerts, Storage and Privacy were all correctly hidden. Each is `project`- or
`org`-level, and an app grant cannot satisfy those. Before `CanScope.level`
existed these were all shown and every one of them 403'd. The "New app" item in
the app switcher was likewise locked (`Requires: Create apps (app:create)`) —
`app:create` authorizes at the project (`projects.rs:239`).

Incidental finding: the app switcher only ever offers apps the server's reach
filter returns, so the "nav churn on app switch" risk flagged in the design is
narrower than predicted — a user cannot select an app they have no grant on.

**Developer — Members visible, actions locked.** Page rendered with 4 member
rows. Locked: "Create member", "Grant", every grant-removal `×`, "New role",
and all four row-menu items. The `member:credential` carve-out survived: "Reset
password" and "Sign out all devices" report `member:credential` while "Edit" and
"Deactivate" report `member:manage`. System roles still open read-only ("View"
unlocked), as intended.

**Failed access fetch.** With `GET /v1/orgs/{org}/access` forced to a network
error (XHR redirected to a closed port; every other request untouched), the
shell rendered "Couldn't load permissions" + Retry rather than an empty sidebar
with everything locked. `accessError === true`, `access === null`. Reloading
restored all 20 nav items. This is the state the fix exists for: without it the
outage is pixel-identical to a legitimate no-grants account.

**Console:** no errors across the whole session. `npm run check` 0 errors over
395 files; `npm test` 285 passing across 20 files.

## Known, pre-existing, not caused by this change

At viewport widths below ~860px the page overflows horizontally by 106px
(`.content-inner` in `AppShell`). Measured identically on `/account`, a page
this change never touched, so it is a pre-existing narrow-viewport layout issue
in the mobile grid, not a regression. Left alone as out of scope.
