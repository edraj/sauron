# Wall of Shame — overnight report, 2026-08-11

**Status:** built, tested and runtime-verified end to end.
**Baseline:** branch `soheyb`, HEAD `d67b8f6` — **unchanged, nothing committed.**
Everything below is in the working tree for you to stage yourself.

> **Re-verified after the ingest-failure-recovery feature landed** (see §9). The
> two features share `main.rs`, `routes/mod.rs`, `routes.ts`, `page-access.ts`
> and `admin-nav.ts`; both work together, and the admin nav now carries eleven
> items. One hole in my own drift guard was found and closed in the process.

---

## 1. What you can look at first

The dev API on **:8090** is running the new build, and there is a dashboard on
**:3011** with a populated org so the page is not empty when you open it:

```bash
open http://localhost:3011/#/admin/wall-of-shame
```

Log in as `zz-wallofshame-verify@example.test` / `WallOfShameVerify!2026`.

If the API is no longer running (background processes do not outlive the
session), restart it with:

```bash
/tmp/claude-1000/-home-splimter-projects-freelance-sauron/7182ca15-cec2-407a-b6d2-ee03616a8c1b/scratchpad/run-api.sh
```

**I deliberately left the verification data in place** so the page has
something to show. It is one org, all records prefixed `ZZ`. To remove it:

```bash
PGPASSWORD=sauron psql -h 172.20.0.2 -U sauron -d sauron -c "DELETE FROM organizations WHERE name = 'ZZ Wall Of Shame Verify';" -c "DELETE FROM users WHERE email LIKE 'zz-%@example.test';"
```

I also **stopped the `sauron-api-task12` container** to free :8090 — it was
running a 10-hour-old binary that predates this schema, so it would 500 on the
new endpoint. `docker start sauron-api-task12` brings the old one back.

---

## 2. What was built

A general administrative audit trail, org-partitioned, plus the admin page that
reads it.

| Layer | Files |
|---|---|
| Schema | `backend/migrations/2026-08-11-000050_audit_log/` |
| Diesel | `sauron-db/src/{schema,models}.rs` (+`AuditLogEntry`, `NewAuditLogEntry`) |
| Repo | `sauron-db/src/repo.rs` — insert, unified feed, 3 facet queries, 3 scope helpers |
| Recording | `sauron-api/src/audit.rs` (new) — action vocabulary, allowlists, `record` |
| Instrumentation | 44 handlers across 10 route modules |
| Drift guard | `sauron-api/tests/audit_coverage.rs` (new) |
| Read API | `sauron-api/src/routes/audit.rs` (new) — `GET /v1/admin/audit` |
| Dashboard | `lib/api/audit.ts`, `lib/models/audit.ts`, `pages/WallOfShame.svelte` (all new) |

**44 of the 68 mutating endpoints are audited; 24 are explicitly exempt**, each
with a written reason (auth events, product-data edits, personal preferences,
and the six inspector routes that already write their own audit tables).

### The trail records

org / project / app / environment create-update-delete, environment key
rotation and enrollment changes, member create / activate / deactivate /
password-reset / session-revoke, role create-update-delete, grant
create-update-delete, alert rules and channels, monitors, source-map artifacts,
store connections, privacy policies, and the deployment-wide tier actions.

For updates it records a **before → after diff of the changed fields only** —
so "someone widened a role" tells you *which* permission was added, which was
the thing you actually asked for.

---

## 3. Verification

Everything below was run, not assumed.

| Gate | Result |
|---|---|
| `sauron-db` audit integration (real Postgres) | **9 passed** |
| `sauron-api` unit tests | **126 passed** |
| Drift guard | **5 passed** |
| Dashboard | **623 passed** (40 files) |
| `svelte-check` | **0 errors, 0 warnings**, 458 files |
| `cargo clippy -D warnings` (`sauron-db`, `sauron-api`, all targets) | **clean** |

Backend tests were run with the Bash sandbox disabled and `TEST_DATABASE_URL`
set. Sandboxed runs have their own netns and every DB-backed test returns early
while printing `ok`; the 9 above genuinely executed (3.85s of real query time).

**The drift guard was itself tested**: removing one handler from `AUDITED` makes
`every_mutating_route_is_audited_or_explicitly_exempt` fail, and restoring it
makes it pass. A guard that cannot fail is worthless.

### Runtime drive

Eight real admin actions were driven through the live API and read back:

```
environment.rotate_key   dev                Checkout Platform / checkout-web   {}
project.update           Checkout Platform  Checkout Platform                  {"name":{"from":"Checkout Service","to":"Checkout Platform"}}
member.create            zz-teammate@…                                         {"email":{"from":null,"to":"zz-teammate@…"}}
role.update              Support                                               {"permissions":{"from":["issue:read","event:read"],"to":[…,"issue:write"]}}
role.create              Support
environment.create       staging            Checkout Service / staging
app.create               checkout-web       Checkout Service / checkout-web
project.create           Checkout Service   Checkout Service
```

Note the two project names in that list: entries created *before* the rename
still say "Checkout Service". That is the snapshot behaviour working — the trail
records what things were called when the action happened.

Also verified live, in the browser:

- the page renders, all six filter dropdowns populate, filtering narrows
  correctly and the URL stays linkable;
- the drawer shows the role permission diff (`issue:write` added);
- the key-rotation entry shows **no credential** and explains why;
- pagination pages exactly (8 rows across 3 pages, 8 unique — no skips or repeats).

Security, live:

| Check | Result |
|---|---|
| Cross-tenant read (another org's `org_id`) | **403** |
| Unauthenticated | **401** |
| Member *without* `org:manage`, in their own org | **403** |
| …same member, `/v1/orgs` | **200** (gate is specific, not blanket) |
| …same member, nav item | **hidden** |
| …same member, direct URL | denial naming `org:manage` |
| Bogus `entity_type` | **400** with the valid list |
| Malformed cursor | **400** |

---

## 4. Three bugs the runtime drive caught that every test passed through

These are the reason the live drive was worth doing.

**1. A deep link to any filter value could never work — infinite request loop.**
`<select bind:value>` resets its binding to `null` when the bound value is not
among its `<option>`s. The options come from facets, which arrive one request
*after* the filters are hydrated from the URL. So on `?action=role.update` the
select had no matching option, the binding wrote `null` back into the filter
state, that write retriggered the load, the reply replaced the option list, and
it never settled — spinner stuck, requests every 4ms. Fixed by pinning the
selected value as an option (`withSelected`), which is independently right: the
trail can name a project or actor that no longer exists and the dropdown must
still show it. Regression-tested.

**2. The URL→state and state→URL effects compared encoded strings.**
Key order differs between a pasted URL and the canonical encoding, so
`?action=x&range=30d` and `?range=30d&action=x` are the same view and different
strings — the two effects disagreed permanently. Fixed with a semantic
`sameFilters`, the same shape `DevicesInventory`'s `sameGroupKey` comment
already warns about one level down. Regression-tested.

**3. The page read `window.location.hash` directly** instead of the router's
`$querystring`, latching whatever the address bar held before the router
settled. It was the only page in the dashboard doing this. A deep link restored
`action` but silently fell back to the default date range.

---

## 5. Two design corrections made during the build

**No foreign keys except `org_id`.** The first cut used
`REFERENCES … ON DELETE SET NULL` on `actor_id`, `project_id` and `app_id`.
That is wrong twice: deleting a project would blank `project_id` on every
historical entry, so filtering by that project — the question you ask *because*
it was deleted — silently returns nothing; and a delete handler could not record
its own event, because the referenced row is already gone when the entry is
written, so the insert would fail the FK and the deletion would be the one
action guaranteed to go unrecorded. Audit ids are inert snapshots. Pinned by
`an_entry_can_name_rows_that_no_longer_exist`.

**Facets use the most recent name, not `MAX(name)`.** A renamed project appears
in the trail under both names and `MAX` picks whichever sorts higher — renaming
"Checkout Service" to "Checkout Platform" left the dropdown offering the old
name, because 'S' > 'P'. Caught in the live drive, fixed with `DISTINCT ON …
ORDER BY created_at DESC`, and pinned by a test.

---

## 6. Deliberate decisions worth knowing

- **Fail-open.** A failed audit write logs at `error` and the action proceeds.
  An audit-table problem must not take down member management. The trade is
  that a gap in the trail is possible and shows up only in the logs.
- **No backfill** (your call). The table starts empty; the empty state says so
  explicitly rather than looking like data loss.
- **Retention is forever.** No prune job, no knob, nothing that can silently
  delete evidence.
- **Secrets can never enter `changes`.** Two independent guards: a per-entity
  allowlist, and a runtime substring check (`FORBIDDEN_FIELDS`) that catches
  `slack_webhook_url` / `smtp_password` even if someone adds one to an
  allowlist. Key rotation records *that* it happened and neither key.
- **Deployment-wide tier actions are written to every org's trail**, because
  they change every tenant's data retention and the page is org-scoped.
- **The two inspector audit tables are unioned in at read time**, not copied —
  no double-writing, no drift, and the Privacy page keeps its own views.

---

## 7. Open items / hazards

- ~~**RPM upgrades never re-run `sauron-migrate`.**~~ **Retracted — this was
  wrong, see §10.** The migration runs itself on upgrade; no manual step is
  needed.
- **`changes` is a new place personal data rests.** Member emails and role
  permission sets are now persisted outside the `users`/`roles` tables, kept
  forever, readable by org admins. Deliberate — flagged when the design was
  approved — but worth remembering if a data-retention question ever comes up.
- **Filtered queries fall back to the time index.** Filters are expressed as
  `($n IS NULL OR col = $n)` so the SQL stays static and fully bound (no
  injection surface, no parameter-numbering drift). The cost is that Postgres
  cannot use the per-axis partial indexes on a filtered query. Fine at
  administrative volume — thousands of rows a year, not event data — but it is
  the thing to revisit if a tenant ever has a very long trail.
- **Not done:** CSV export, alerting on audit events, and capturing auth
  events (login/logout) — all out of scope by decision, none precluded.
- **Untouched, pre-existing:** `offset_sort.rs`, `device_groups.rs`,
  `env_scoping.rs`, `routes/devices.rs` and `vite.config.slice3.mjs` are dirty
  in the tree from the table-sorting work and are not mine.

---

## 8. Files changed

**New (12):** the migration, `sauron-api/src/audit.rs`,
`sauron-api/src/routes/audit.rs`, `sauron-api/tests/audit_coverage.rs`,
`sauron-db/tests/audit_log.rs`, `dashboard/src/lib/api/audit.ts`,
`dashboard/src/lib/models/audit.ts` + `.test.ts`,
`dashboard/src/pages/WallOfShame.svelte`, the design spec.

**Modified (19):** `sauron-db/src/{schema,models,repo}.rs`; ten route modules in
`sauron-api/src/routes/`; `sauron-api/src/main.rs`; and in the dashboard
`routes.ts`, `admin-nav.ts`, `page-access.ts`, `Icon.svelte` (one icon added),
`scope.test.ts` (Wall classified as non-telemetry, with the reason).

`.claude/launch.json` gained a `dashboard-wallofshame` entry on :3011.

The design spec is at
`docs/superpowers/specs/2026-08-11-wall-of-shame-design.md`.

---

## 9. Re-verification after ingest-failure-recovery landed

A second feature (migration 51, `sauron-pipeline`, `routes/failures.rs`,
`IngestFailures.svelte`) arrived in the tree from another session. Both features
touch the same five shared files, so everything was re-run.

**The drift guard did its job, then revealed a hole in itself.**

It correctly forced the two new mutating routes (`routes::failures::retry`,
`routes::failures::drop_group`) to be classified, and they were added to
`AUDITED` — and `failures.rs` genuinely does call `audit::record_all_orgs` for
both, so the classification is honest.

But `audited_handlers_actually_call_record` verifies AUDITED entries against a
**hand-maintained `SOURCES` list**, because `include_str!` needs a literal path.
`routes::failures` was never added to it, so those two entries were checked by
nobody. Had they been listed in AUDITED *without* being instrumented, the guard
would have reported green — which is the exact failure mode the guard exists to
prevent, one level up.

Closed with a new test, `every_audited_module_has_a_source`, which asserts every
module named in AUDITED has a `SOURCES` entry and prints the exact line to add.
Verified to fail: deleting the `routes::failures` line makes it red with an
actionable message, while `audited_handlers_actually_call_record` stays green —
proving it *was* blind. The guard is now 6 tests.

**Migration 51 was applied to the dev database.** It was on disk but unapplied,
and `sauron-api` fail-closes on a schema gap — it refused to boot with
`DATABASE SCHEMA IS BEHIND THIS BINARY`. That is the RPM hazard in §7 showing up
locally, and it is working as designed. The migration is purely additive (new
tables only, no `ALTER`/`DROP`, does not touch `audit_log`); `audit_log` was
confirmed intact afterwards.

### Results

| Gate | Result |
|---|---|
| `sauron-db` audit integration (real Postgres) | **9 passed** |
| `sauron-api` unit tests | **159 passed** |
| Drift guard | **6 passed** (was 5) |
| Dashboard | **640 passed** (41 files) |
| `svelte-check` | **0 errors, 0 warnings**, 462 files |
| `cargo clippy -D warnings` | **clean** |

Live, against the rebuilt API on the 51-migration schema:

- schema reports `applied=51 embedded=51`;
- the feed returns 10 entries with populated facets;
- cross-tenant **403**, unauthenticated **401**, member without `org:manage`
  **403** — and in the browser that member still gets the nav item hidden plus
  the denial naming `org:manage`;
- a fresh `environment.create` driven through the live API appeared immediately
  with the right diff (`{"name":{"from":null,"to":"resume-check"}}`);
- the page renders with all six filters and no stuck spinner, and the admin nav
  now shows eleven items — Wall of Shame and Ingest Failures side by side.

---

## 10. Retraction: the RPM upgrade-migration hazard does not exist

§7 originally carried "RPM upgrades never re-run `sauron-migrate`, so run it by
hand after deploying". **That is wrong, and I should not have written it.** It
came from a note that predates the packaging work, and I repeated it without
checking `packaging/rpm/` — including into migration 50's own header comment,
where it would have misled the next person to read it.

What the packaging actually does:

- all six daemon units carry `Requires=sauron-migrate.service` **and**
  `After=sauron-migrate.service` (`packaging/rpm/systemd/sauron-api.service:19-20`);
- `sauron-migrate.service` deliberately has **no** `RemainAfterExit`, so it is
  `inactive` between runs and therefore re-runs on *every* daemon start rather
  than roughly once per boot;
- `%postun server` runs `%systemd_postun_with_restart` over the six daemons,
  which restarts them, which pulls the migrator in ahead of each one.

The two facts the old note rested on are true — no `[Install]` section, absent
from `%postun`'s list — but the conclusion drawn from them is not. Both are
deliberate, and the spec explains why at length: adding migrate to `%postun`
was tested and is provably a no-op, because the restart marker matches nothing
on a unit that is inactive between runs.

So there was no gap to close. What I have done instead is correct the three
places I propagated the false claim: this report, migration 50's header, and
the note that caused me to repeat it.

**The real residual risk is the opposite one, and it is already handled:** if a
migration fails, `Requires=` (not `Wants=`) means the daemons refuse to start
rather than running against a stale schema. That is the fail-closed behaviour
observed live in §9, where the API refused to boot with `DATABASE SCHEMA IS
BEHIND THIS BINARY` until migration 51 was applied.
