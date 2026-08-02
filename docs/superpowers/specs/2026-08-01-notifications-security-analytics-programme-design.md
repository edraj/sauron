# Notifications, security and analytics programme

Date: 2026-08-01
Status: designed, not started

This is the parent document for a six-slice programme. It owns the decisions that
no single slice can make alone: migration numbers, build order, what gets built
once and by whom, and which cross-slice assumption is wrong. Each slice has its
own document and this one deliberately does not repeat their internals.

| Slice | Document |
|---|---|
| S0 email foundation | `docs/superpowers/specs/2026-08-01-email-foundation-design.md` |
| S1 password reset | `docs/superpowers/specs/2026-08-01-password-reset-design.md` |
| S2 session management | `docs/superpowers/specs/2026-08-01-session-management-design.md` |
| S3 notification preferences | `docs/superpowers/specs/2026-08-01-per-user-notification-preferences-design.md` |
| S4 active users | `docs/superpowers/specs/2026-08-01-active-users-design.md` |
| S5 PII Inspector | `docs/superpowers/specs/2026-08-01-pii-inspector-design.md` |

---

## 1. What was asked

> "SMTP config should be configured within .env" … "Emails should have a
> prettified template".
>
> "a user/admin can also reset (user forgot, admin force pwd change for a user).
> if admin init it, the selected user would be able to to use his current
> password, and must check his email for force password reset. a user can forgot
> his pwd, and use his email to change it (like modern services)".
>
> "a member can kill all his sessions (not the current one as he is currently
> using it) (enhance security). admin can kill all selected user sessions (force
> login for a user)".
>
> "each user can define what kind of email notification for which project (all
> apps, selected envs) or for which app (selected envs), or uptime issue, or rate
> of error increasing".
>
> "we want also to get numbers of daily and monthly active users (we can also
> export as csv). it is scoped per app-env. within project, we can select multi
> apps (each app user should select env to go with) and see combined DAU/MAU (if
> user is identified then treat them as one, if a user has many session in diff
> apps and is identified then it should be considered as 1)".
>
> "by default, admin can enable it for project (all apps, all envs), for app (all
> envs), for app-env. once done we check all events, all issues, all context and
> tags and extra details/info for PII. the admin must set a list of keys that
> should be tracked, if found then the admin/user can see it and they have the
> option to mask. mask is a feat that if executed that field value is permanently
> set to ****. since it is heavy operation, the admin can set a periodic scan
> (define days and time). he can also manually start one. results can be exported
> as csv".

### Decomposition

Five requested features, six slices. The sixth exists because two of the five
need to send mail and neither should build a mailer.

| Slice | Subsystem | What it delivers |
|---|---|---|
| **S0** | mail infrastructure | Deployment-level `SMTP_*` config, `sauron-mail` crate, HTML+text house template, `mail_outbox` and its drain, dev log-sink |
| **S1** | credentials | `password_reset_tokens`, self-service forgot/reset, admin force-reset that stops the current password authenticating |
| **S2** | sessions | `auth_sessions` with identity that survives refresh rotation, self-service and admin session kill, sub-poll-interval revocation |
| **S3** | notifications | Personal per-user subscriptions (uptime, error spike/new/regression) scoped project/app × environment, delivered to `users.email` |
| **S4** | product analytics | Daily active users per app-env and combined across apps in a project, split into total / identified / guest, CSV export |
| **S5** | data governance | PII inspector: policy, scheduled and manual scans, findings, irreversible masking of hot Postgres rows only, audit, `pii:*` permissions |

S1..S5 are five genuinely independent subsystems. They do not share a data model,
they do not share a read path, and four of the five could in principle be built
by four people who never speak. They are sequenced anyway, for two reasons that
have nothing to do with the features themselves:

1. **Three of them rewrite the same four files.** `AppState` in
   `backend/bins/sauron-api/src/main.rs`, the `AuthUser` extractor in
   `backend/crates/sauron-auth/src/extractors.rs`, the revocation-reason
   constants in `backend/crates/sauron-db/src/repo.rs`, and
   `dashboard/src/lib/components/members/MembersTable.svelte`. Merged
   simultaneously these produce conflicts that resolve cleanly and are wrong —
   the kind where both sides compile and one side's guard quietly disappears.
2. **Half the work is foundations that only the first slice to need them can
   build.** A durable outbox, a background-task supervisor, an extracted member
   admin guard, a CSV writer, a read-route rate limiter. Built twice they drift;
   built in parallel they get built twice.

The chain S0 → S2 → S1 → S3 is hard. S4 can be built alongside it and S5 after
it, each with named rebases — S4's onto `AppState`, S5's onto S4's ingest edit.

---

## 2. The finding that shaped the programme

**Transactional email is a new capability. SMTP is not.**

The instinct on reading "SMTP config should be configured within .env" is that
Sauron cannot send mail. It can, and has been able to since the alerting engine
shipped. `lettre` 0.11 is already a workspace dependency, already wired for
rustls, already resolving hostnames through the SSRF guard, and already sending
mail in production. What does not exist is any way to send a message *to a
person* rather than *to an org's configured channel*.

| Capability | Today | Where |
|---|---|---|
| SMTP transport (connect, STARTTLS/implicit, auth, send) | **Exists** | `backend/crates/sauron-alerts/src/deliver.rs:144-200`, private `deliver_email` |
| SSRF-pinned relay resolution | **Exists** | `sauron_monitor_core::ssrf::resolve_checked`, called from the same fn |
| Recipient list | **Exists, wrong shape** | Per-org `notification_channels` JSONB, admin-typed static addresses |
| `{{var}}` substitution and HTML escaping | **Exists, private** | `sauron-alerts/src/render.rs:106` (`pub`) and `:133` (private `html_escape`) |
| Transient-vs-permanent classification | **Exists, fragile** | `sauron-alerts/src/engine.rs:209-213`, four `e.contains(..)` substring checks against error strings |
| Deployment-level relay config | **Missing** | No `SMTP_*` anywhere on `sauron_core::Config` |
| A URL the API can put in a link | **Missing** | `DASHBOARD_URL` does not exist; configuration flows dashboard → API today, never the reverse |
| HTML mail | **Missing** | `ContentType::TEXT_PLAIN` is hardcoded |
| Mail addressed to `users.email` | **Missing** | Nothing in the product has ever mailed a user |
| Mail off the request path | **Missing** | `sauron-api` has no background loop at all |
| Any way to observe that mail left the process | **Missing** | No test anywhere can assert delivery |

Two consequences follow, and they are why S0 is the size it is.

The first is that S0 is mostly a **refactor**, not a new integration. The
transport moves into a new leaf crate `backend/crates/sauron-mail`,
`sauron-alerts` drops its `lettre` dependency and depends on `sauron-mail`
instead, and `deliver_email` shrinks to about fifteen lines. No binary gains a
dependency it did not already have transitively — `sauron-api` already links
`sauron-alerts`. This is the same move `sauron-monitor-core` already made so that
`sauron-alerts` could reuse the SSRF guard without depending on a worker binary.

The second is that the *shipped* mail path has a latent bug that the programme
inherits unless it is fixed at the bottom. `lettre`'s `.timeout()` is applied per
socket operation, and `resolve_checked` calls `tokio::net::lookup_host` with no
timeout at all, so there is no bound on the total duration of a send. Today that
only delays an alert. Under S1 it would sit inside a handler that an
unauthenticated caller can time. S0 therefore owns a single total-deadline
wrapper over resolve + connect + conversation, which fixes the alerting path as a
side effect.

The corollary is worth stating because it will otherwise be discovered in review:
**"improving" the wording of a `MailError` variant silently stops every alert
email from retrying**, because the retry predicate is a substring match on the
rendered error string and nothing will fail to compile. S0 lifts that predicate
into `sauron_mail::is_transient(&str)` with a unit test per variant, so the
coupling is at least visible from both sides.

---

## 3. Programme decisions

These override the individual slice designs wherever they disagree.

### P1. Migration numbers are pinned now, in build order, and the directory date is the landing date

Every slice was designed against "000034 is next" and four of them claimed it.
`backend/crates/sauron-db/src/lib.rs:24` embeds the migration directory with
`embed_migrations!`, and `run_pending_migrations` drives diesel's
`MigrationHarness`, which orders by the **full** version string
`YYYY-MM-DD-0000NN` — that is, **lexicographically by date first**. A slice that
lands in September carrying an August date prefix and a higher sequence number
runs *before* an August migration with a lower number, and nothing complains
until a foreign key fails on a fresh database. Section 4 pins the allocation.

### P2. The outbox is the programme's async side-effect primitive

`mail_outbox` plus the claim/drain/backoff/reap loop is the first durable,
restart-surviving, observable deferred-work mechanism in the codebase. Nothing in
this programme may `tokio::spawn` a detached network call instead. S1's original
design spawned the lookup-issue-send sequence behind an eight-permit semaphore;
that section is deleted outright, because the outbox is strictly better on every
axis S1 cared about — no timing oracle (the response never touches SMTP),
survival across restart (a bare spawn dies with the process), bounded concurrency
from the single drain loop rather than a hand-rolled semaphore, and it is the
only version an integration test can observe. Write this in the crate doc comment
so the next person wanting "do this after the response" reaches for it rather
than minting a second pattern.

### P3. One background-task supervisor in `sauron-api`, and boot never fails on a task

`backend/bins/sauron-api/src/tasks.rs` is new ground: `main()` currently has
exactly one DB touch at boot and no spawned loops. Three slices independently add
timers to it. One supervisor — named tasks, respawn with capped backoff on panic
or error, per-task `last_success` age surfaced on `/health`.

**No task's initialization may `?` out of `main()`.** S2's design has a
synchronous revocation refresh before the listener binds. The blast radius is
exact: `packaging/rpm/systemd/sauron-migrate.service` has no `[Install]` section,
`sauron.spec` runs `%systemd_postun_with_restart` on the API, and
`sauron-api.service` is `Restart=on-failure` with no `StartLimit` override — so a
`?` against a table that a skipped migration never created burns systemd's
five-starts-in-ten-seconds budget and leaves the unit `failed` with no HTTP
surface left to diagnose from. Start with an empty snapshot, log at ERROR on
every failed poll, and let the `/health` age make it visible.

### P4. A queue's reaper runs in the process that drains it

Three slices proposed three different homes for retention. The rule: the reaper
lives with the drain; where nothing drains a table, with its writer; and never in
an optional worker for a table on a mandatory path. That gives `mail_outbox` and
`password_reset_tokens` to `sauron-api`'s supervisor (S1's own caveat — "if a
deployment runs no `sauron-alerts` the rows simply accumulate" — is the argument
against S1's own choice), `notification_queue` to `sauron-alerts`, and the
inspector tables to `sauron-inspector`.

Note the one asymmetry this creates deliberately: `sauron-alerts` **enqueues**
into `mail_outbox` for S3 but never drains it, so `sauron-alerts` needs no SMTP
configuration at all and personal notification mail cannot be delivered twice by
two processes.

Retention values are compile-time constants, not env vars. Three files of
documentation for a number nobody tunes is how a config surface becomes
unmaintainable.

### P5. Every outbound network call carries one total deadline

`tokio::time::timeout` over the whole of resolve + connect + SMTP conversation in
`sauron-mail`, with a distinct `MailError::DeadlineExceeded` classified as
retryable. This is one fix for three findings: S1's unbounded handler, S0's
tarpit case where a single send outlives the stale-row threshold and a requeued
duplicate races the original, and the shipped alerting path's untimed DNS lookup.

It has a schema consequence. `mark_mail_sent` and `mark_mail_failed` must carry
`WHERE status = 'sending' AND attempts = $n`, or the zombie sender's completion
overwrites the live claim and blanks the body underneath it. And
`requeue_stuck_mail` must enforce `attempts < max_attempts` and reset
`next_attempt_at` — without both, the give-up decision is unreachable on the
crash-recovery path and the backoff ladder is bypassed entirely.

### P6. One revocation-reason registry

There are five `REVOKE_*` constants in `backend/crates/sauron-db/src/repo.rs`
today and `refresh_tokens.revoked_reason` has no CHECK. S1 adds two reasons, S2
adds three plus a CHECK on `auth_sessions.revoked_reason` plus a
"mundane cause, skip the alarm" list consulted by `refresh`'s reuse-detection
branch. Split across two slices this fails two ways: a reason missing from the
CHECK makes the revoke path 500, and a reason missing from the deliberate list
sends the target's still-live refresh token into the theft branch about fifteen
minutes later, firing a family-wide kill. The comment at
`routes/auth.rs:388-397` records that exact bug happening before with routine
deactivations.

So: one `pub const DELIBERATE_REVOKE_REASONS` beside the constants, and S2's
CHECK transcribed from the constant list once — seeded with S1's two reasons
while `auth_sessions` is still empty, so S1 ships no widening migration of its
own — with a doc comment saying that a *later* addition does need one.

Membership is decided per reason by a three-bucket test, not by a blanket rule.
A reason belongs in the list only if it is bucket one: a **deliberate** act by a
human that revokes someone's session. Bucket two is reasons `refresh` already
handles in a branch of their own, and bucket three is theft signals. The
tempting blanket formulation — "every `REVOKE_*` except `REVOKE_REUSE`" — is
actively dangerous, because it sweeps up both other buckets: `REVOKE_ROTATED`
would send every ordinary rotation down the early-return path and break the 10s
multi-tab grace window, and `REVOKE_LOGOUT` would disable replay detection on
exactly the tokens where a replay is most diagnostic. The unit test asserts the
classification rather than the list: every `REVOKE_*` constant falls in exactly
one bucket, and the array is bucket one.

### P7. The programme adds three permissions, in two batches, and the arithmetic moves twice

`member:credential` gates both force-logout (S2) and force-reset (S1), because
control of a mail relay plus the ability to force a reset is a path to account
takeover and an org that hands out `member:manage` has not agreed to that. S2
lands first, so **S2 mints it** and folds its custom-role `UPDATE roles` into
migration 000035 rather than taking a number of its own. S5 then adds `pii:read`
and `pii:manage`. S3 subscriptions authorize on the existing `monitor:read` and
`issue:read` and add nothing.

That means the five coordinated RBAC edits — the `perm` constant and
`perm::ALL`, the four preset count assertions, the migration `UPDATE`ing custom
roles, `dashboard/src/lib/models/permissions.ts`, and the `Permission` union in
`dashboard/src/lib/models/index.ts` — are performed twice, once by S2 and once
by S5. Section 9 carries the arithmetic and the rule about the dashboard mirror.

### P8. No pooled connection is held across network I/O, and the 16 connections are a programme budget

`build_pool(&cfg.database_url, 16)` at `main.rs:68` is the whole process.
`POOL_WAIT_TIMEOUT` is 5s, and `ConcurrencyLimitLayer`/`TimeoutLayer` shed the
HTTP request without cancelling the Postgres query or freeing the slot. This
programme adds two background loops that check out on a timer and the heaviest
read query in the product. Background tasks check out, claim or poll, **drop**,
then do the work. The inspector gets its own 4-connection pool and never touches
the API's.

### P9. Read routes get rate limits, starting here

`rate_limit` and `client_addr` in `backend/bins/sauron-api/src/routes/auth.rs`
are module-private and applied only to login/register/refresh. Four later slices call them from other
modules: S2 from `routes/account.rs`, S1 from `routes/orgs.rs` to populate
`requested_from`, S3 for the unsubscribe limiter, S4 on the active-users
endpoints and S5 for `confirm_source`. Make them `pub(crate)` once, in place, with a doc comment establishing the key convention
`sauron:{area}:{action}:{principal}`. This turns "this repo has no read-route
rate limiting" into "here is how to add the next one", which matters because S4's
endpoint is otherwise a wedge any Viewer can pull.

### P10. Claim-based concurrency only

There are **zero** advisory locks in the repository and this programme introduces
none. Every concurrent claim uses the `FOR UPDATE SKIP LOCKED` shape from
`repo::claim_due_monitors`, optionally fenced on a worker id and a lease. Work
that cannot be expressed that way is a design smell, not a reason to add a lock
primitive — the immediate cost is that a lock held by a process killed with
SIGKILL has no owner to release it, and there is no graceful shutdown anywhere
(P-risk, section 8).

### P11. Config never bails, and every key is documented four times

`Config::from_env` is shared by every binary; a `bail!` there takes down
`sauron-ingest` and `sauron-tier` over a relay setting they never read. That is
the documented reason `jwt_secret` is a recorded `Result`. Every new field
defaults, and fails closed at point of use through a `require_*()` accessor.

This programme adds roughly thirty environment variables across five slices, each
needing a row in `.env.example`, `docker-compose.yml`, the relevant
`packaging/rpm/config/*.env`, and the README table. Nothing enforces that today.
S0 adds a CI assertion that every `var("KEY")` / `parse("KEY"` literal in
`backend/crates/sauron-core/src/config.rs` appears in `.env.example`. It costs an
hour and it is the only mechanism that will still be working in a year.

### P12. `#/account` is a card container, and the member row actions become one menu

S2 creates the first account page. Build it as Profile card + Sessions card from
day one so S3's notification preferences is an added card, not a restructure.
Name the session interface `AccountSession`: `AuthSession` is taken at
`dashboard/src/lib/models/index.ts:28` and shadowing it would compile while
silently changing the auth store's types.

`MembersTable.svelte` has one `.row-actions` div with two buttons. S2 adds
"Sign out all devices" and stays inline at three, which still fits. S1 adds
"Reset password", and four inline buttons in a table row is where that stops
working — so **S1 builds the kebab menu** and folds S2's button into it, in the
order Edit / Reset password / Sign out all devices / Deactivate: Edit promoted,
destructive last. This is real work rather than a wrapper:
`dashboard/src/lib/components/ui/` has fourteen components and none of them is a
Menu, Select, Toggle or Tabs primitive.

### P13. The upgrade runbook is created once and appended by every slice

`packaging/rpm/SETUP.md` has sections 1-10 and **no upgrade section at all**,
while four of the six designs name it as their mitigation. S0 creates §11
"Upgrading" with the gate in the imperative:

```
systemctl stop sauron-api sauron-ingest
systemctl start sauron-migrate
systemctl start sauron-api sauron-ingest
```

Each later slice appends a row to a table in that section: migration number,
what breaks if it is skipped. See section 8 — this is documenting around a root
cause, and the root cause should be filed.

---

## 4. Shared foundations

Built once, by the slice named, consumed by the slices named. Anything in this
table appearing twice in the diff is a review failure.

| Foundation | Lives in | Built by | Consumed by |
|---|---|---|---|
| `sauron-mail` crate: `smtp.rs`, `template.rs`, `text.rs` | `backend/crates/sauron-mail/` | S0 | S1, S3, `sauron-alerts` |
| `pub fn html_escape` / `pub fn substitute` (moved, not copied, out of `sauron_alerts::render`) | `sauron-mail/src/text.rs` | S0 | S1, S3, `sauron-alerts` |
| Total-deadline wrapper over resolve + connect + send | `sauron-mail/src/smtp.rs` | S0 | `sauron-alerts` today, `sauron-monitor` later |
| `pub fn is_transient(&str) -> bool` | `sauron-mail/src/lib.rs` | S0 | `sauron-alerts/src/engine.rs` |
| `mail_outbox` + `enqueue_mail` / claim / `mark_mail_sent` / `mark_mail_failed` / `requeue_stuck_mail` / `prune_mail_outbox` | `backend/crates/sauron-db/src/repo.rs` | S0 | S1, S3 |
| Background-task supervisor | `backend/bins/sauron-api/src/tasks.rs` | S0 | S1 (token reaper), S2 (revocation poller) |
| `Config::dev_mode` promoted from a local to a `pub` field | `backend/crates/sauron-core/src/config.rs:143` | S0 | S1 |
| `SMTP_SINK` dev log-sink (subsumes S1's separate "log the reset URL" branch) | `sauron-mail/src/smtp.rs` | S0 | S1, S3, all E2E |
| `packaging/rpm/SETUP.md` §11 Upgrading | `packaging/rpm/SETUP.md` | S0 | all |
| CI assertion: every config key documented | `.github/workflows/ci.yml` | S0 | all |
| `DELIBERATE_REVOKE_REASONS` registry + the three-bucket classification test | `backend/crates/sauron-db/src/repo.rs` | S2 | S1 |
| `auth_sessions_revoked_reason_check`, pre-seeded with S1's `password_reset` and `reset_forced` so S1 ships no widening migration | migration `000035` `up.sql` | S2 | S1 |
| `revoke_sessions_for_user(conn, user_id, except: Option<Uuid>, reason: &str, actor: Option<Uuid>) -> QueryResult<Vec<Uuid>>` — the session-aware revoke every call site switches to. `except` is what lets "sign out my other devices" spare the caller, `actor` fills `auth_sessions.revoked_by`, and the returned ids must be handed to `mark_revoked` or the kill is invisible until the next poll | `backend/crates/sauron-db/src/repo.rs` | S2 | S1 |
| `guard_member_admin_action(conn, caller_id, org_id, target_user_id, allow_self) -> Result<Vec<(String, Uuid, Value)>, ApiError>` | `backend/bins/sauron-api/src/routes/orgs.rs` | S2 | S1 |
| `perm::MEMBER_CREDENTIAL` + its `perm::ALL` entry, the Owner/Admin preset grants and the four dashboard/RBAC mirrors | `backend/crates/sauron-auth/src/rbac.rs` | S2 | S1 |
| `pub(crate) rate_limit` / `client_addr`, widened in place | `backend/bins/sauron-api/src/routes/auth.rs` | S2 | S1, S3, S4, S5 |
| `#/account` page as a card container + `lib/api/account.ts` for the `/v1/me/*` namespace | `dashboard/src/pages/Account.svelte` | S2 | S3 |
| Member row-action overflow menu (there is no Menu primitive in `dashboard/src/lib/components/ui/`, so this is a new component, not a wrapper; `MembersTable.svelte` is only rewired to use it) | `dashboard/src/lib/components/ui/RowActionsMenu.svelte` | S1 | S2's inline "Sign out all devices" folds into it |
| Live-enrollment resolvers: `live_enrollments_for_apps`, `enrollment_ids_for_env_name` | `backend/crates/sauron-db/src/repo.rs` | S3 | S5 |
| CSV export: RFC 4180 writer + formula-injection guard | `backend/bins/sauron-api/src/csv.rs` | S4 | S5 |
| Blob download helper that preserves refresh-and-replay and reads the error body back as text | `dashboard/src/lib/api/download.ts` | S4 | S5 |
| CORS `.expose_headers([CONTENT_DISPOSITION])` | `backend/bins/sauron-api/src/main.rs:135-144` | S4 | S5 |

Two of these have a non-obvious requirement worth repeating here because a
reviewer will otherwise flag the workaround as unnecessary:

- `dashboard/src/lib/api/client.ts:119-146` holds the 401 refresh-and-replay, and
  `normalizeError` reads `error.response.data` as an `{error:{code,message}}`
  envelope. With `responseType: 'blob'` that data **is a Blob** and the message is
  lost. `download.ts` must go through the same `api` instance and read the blob
  back as text on a non-2xx before normalizing.
- `html_escape` does not escape the single quote. That is safe only because every
  attribute in the house layout is double-quoted, which must be an inline comment
  at the top of `LAYOUT_HTML`, not tribal knowledge.

---

## 5. Migration allocation

Last on disk is `2026-07-30-000033_env_per_project`. Ten numbers are allocated,
in build order, and **this table wins over every slice document**.

| NN | Directory slug | Slice | What it does |
|---|---|---|---|
| 000034 | `mail_outbox` | S0 | Outbox table + due/stuck/created indexes |
| 000035 | `auth_sessions` | S2 | `auth_sessions`, `refresh_tokens.session_id`, live-session backfill, the `revoked_reason` CHECK pre-seeded with S1's two reasons, and the `UPDATE roles` granting `member:credential` |
| 000036 | `password_reset_tokens` | S1 | Reset tokens table + user/expiry indexes, `users.credentials_invalidated_at` |
| 000037 | `notification_subscriptions` | S3 | Subscriptions, env child table, queue table |
| 000038 | `event_users_identified_at` | S4 | `event_users.identified_at` + backfill + partial index |
| 000039 | `analytics_active_user_index` | S4 | Index substitution on `analytics_events` |
| 000040 | `error_active_user_index` | S4 | Index substitution on `error_events` |
| 000041 | `pii_perms` | S5 | `UPDATE roles` granting `pii:*` to custom roles holding `org:manage` |
| 000042 | `inspector_scan` | S5 | Policies + schedule + scans + findings |
| 000043 | `inspector_mask_audit` | S5 | Mask actions (audit + queue) + masked-key list |

Notes that are load-bearing:

- **S2 takes 000035 and S1 takes 000036**, inverting what both slice documents
  and the first-pass integration analysis say. Numbers follow build order (§6) so
  that number, landing order and date prefix are all monotone together. Neither
  table references the other, so applying 35 then 36 on a fresh database is safe
  either way; the ordering exists to keep the rule simple enough to follow. It
  does carry one live coupling: `member:credential` reaches custom roles only
  through 000035, so S1's admin reset route deployed against a database that
  stopped at 000034 is invisible to every custom role that should hold it, while
  the Owner and Admin presets — which are compiled in — keep working. That
  asymmetry is what makes the failure look like a role bug rather than a missed
  migration.
- **The date prefix is the landing date, not the authoring date, and must never
  decrease as NN increases.** Diesel sorts on the full `YYYY-MM-DD-0000NN`
  string. If 000039 lands in September, its directory is `2026-09-xx-000039_…`;
  if 000040 then lands in October it is `2026-10-xx-000040_…`. Backdating a
  directory to match a design document reorders the run.
- **000041, 000042 and 000043 are one contiguous block belonging to one slice,
  in that order.** 000043's mask actions carry foreign keys to findings and
  scans, so it must follow 000042; 000041 is a standalone `UPDATE roles` but its
  permission constants are compile-time inputs to the route guards the same
  slice ships, so there is no tree in which part of S5 lands and compiles.
- **`schema.rs` claims are deltas, never absolute counts.**
  `backend/crates/sauron-db/src/schema.rs` has 29 `diesel::table!` blocks today
  and every slice was written against that number, so "the file now reports 30"
  is true for whichever slice lands first and false for all the others. Each
  slice states its own delta — S0 +1, S2 +1, S1 +1, S3 +4, S5 +6 — and no
  verification step asserts a total.
- **S4 splits its index work across 000039 and 000040, one partitioned parent
  each.** Both drop and rebuild an index, and a migration runs in one
  transaction with `CONCURRENTLY` unavailable, so a combined migration would
  lock every child of `analytics_events` and `error_events` simultaneously —
  blocking both ingest write paths at once. Splitting halves each lock window
  and lets an operator pause between them.
- Three of these need a maintenance window and must not share a release with
  anything else time-sensitive: 000035 holds `AccessExclusiveLock` on
  `refresh_tokens` across an `ADD COLUMN`, a full-table backfill and a
  non-partial index build, all in one transaction, on the login path; 000039 and
  000040 each drop and rebuild an index on a partitioned parent, which locks
  every child. A migration runs in one transaction and `CONCURRENTLY` is unavailable,
  so neither can be softened.

---

## 6. Build order

```
S0 ──► S2 ──► S1 ──► S3
S4 ──────────────────────  (alongside, merges after S2's AppState change)
                          S5
```

Nothing blocks the start: the questions that changed what gets built are settled
in §10, and the ones still open can be answered while S0 is in flight.

**Step 1 — S0.** Nothing else can start on the mail side. S0 also builds the
supervisor S2 and S1 both need, the `dev_mode` promotion S1 needs, the upgrade
runbook every slice appends to, and the config-documentation CI gate that only
gets cheaper the earlier it lands. Fold in the two blocker fixes S0's own design
omits: reclassify DNS and TLS failures as **transient** (the first failure branch
of `resolve_checked` is literally `DNS resolution failed: {e}`, and S0 blanks the
body on permanent failure, so misclassifying it destroys the reset URL
unrecoverably), and give `requeue_stuck_mail` an attempts cap and a backoff
reset.

**Step 2 — S2 before S1.** This is the one ordering the slice documents get
backwards, and it is a product decision as much as a dependency one. S1's
headline admin feature is "force this person out". Today `must_change_password`
is baked into the JWT at issue time, the extractor reads the claim and not the
row, `Claims.jti` is generated and read nowhere, and there is no denylist — so an
admin reset stops the old password at the login form and still leaves the
target's **already-issued access token good for up to 900 seconds of full
unrestricted access**. S2 closes that to the poll interval, default 5s. Shipping S2 first lets
S1's confirmation dialog say "within a few seconds" and be telling the truth. S2
also builds `pub(crate) rate_limit`, `routes/account.rs` and the `#/account`
page, all of which S1 then reuses rather than invents.

**Step 3 — S1, slimmed.** Consumes S0's outbox (delete the spawn, the semaphore
and the awaited admin send entirely), S2's `revoke_sessions_for_user` and reason
registry, S2's `guard_member_admin_action`, S2's `member:credential`. What is
left is genuinely small: one table, one nullable `users` column, three routes,
their repo functions, two public dashboard pages, one dialog, plus the members
overflow menu that S1's own fourth row action forces (P12). Its
two remaining dashboard subtleties still need
care — `/forgot-password` goes **in** `App.svelte`'s `PUBLIC_ROUTES` array and
`/reset-password` deliberately does **not**, because that array drives an
`$effect` that pushes authenticated users to `/issues` and would bounce a
logged-in user off their own reset link.

**Step 4 — S3.** Needs S0's outbox to enqueue into and S2's account page to hang
a Notifications card on. Also fixes a live bug in the shipped alerting path on
the way through: `repo::alert_count_errors` and `alert_count_events` resolve an
environment name against the project-level `environments` catalogue, whose ids
can never equal `error_events.environment_id` (an `app_environments` enrollment
id), so environment-narrowed alert rules match nothing. Both change to take
resolved enrollment id arrays.

**Step 5 — S4, alongside.** No *feature* dependency on S0-S3, but it is not the
one-line rebase it looks like: S4 adds a `tokio::sync::Semaphore` field to
`AppState`, two routes, the CORS `expose_headers` line and a boot schema probe
for `event_users.identified_at`, which under P3 logs at ERROR rather than `?`ing
out of `main()`. Its `AppState` edit therefore sequences after S2's (C7). It
ships daily numbers only, each split into total / identified / guest. Fold in its
two blockers before the first line of SQL: truncate
`from`/`to` to the UTC day before fingerprinting and before querying (the output
is day-granular, so two identical reports currently produce two different cache
keys and two different queries), and restructure the dedup CTE to reduce `(app_id, distinct_id, day)`
from the raw signal **first**, then join `event_users` to that much smaller set.
The browser SDK anonymous-id fix ships and is adopted **before** the chart is
presented as authoritative — see open question 2.

**Step 6 — S5, as one slice.** Sequenced last deliberately. It needs
`auth_sessions` to exist so that its scan target list can be written as an
**allowlist** of telemetry tables rather than a denylist, which would silently
fail to protect the next account table someone adds. It was drafted in two
halves — policy/scheduling/scan/findings, then masking/enforcement/audit — and
that split is authoring convenience only, never a release boundary: the routes
in the first half gate on permission constants the second half mints, so a tree
carrying only the first half does not compile. Its three migrations land
together, in order. Its export reuses S4's `csv.rs` and `download.ts`.

**Parallelism.** If two people are available, S4 can be *developed* alongside
steps 1-3, but it does not merge in parallel with them: it touches `AppState`, `main()` and the router tail, which
is the same ground S0 and S2 are moving, so its struct edit lands on top of S2's
(C7) rather than beside it. S0 → S2 → S1 → S3 must not be parallelised at all:
every adjacent pair shares `AppState`, the extractor, the revocation registry,
the members table or `routes.ts`.

---

## 7. Cross-slice conflicts and resolutions

| # | Conflict | Resolution |
|---|---|---|
| C1 | Migration collision at 000034 — S0 and S1 both claim it, S2/S3/S4/S5 all mis-numbered downstream | §5, pinned. S1, S2, S3, S4 and S5 all rename their migration directories from what their designs state |
| C2 | Mail delivery specified two incompatible ways: S0's durable outbox vs S1's spawned send behind a semaphore, with S1 declaring a request-path `send_email(...)` "a hard constraint on S0, not a preference" | Outbox wins (P2). S1's spawn, semaphore, awaited admin send and its "no request-level backpressure" risk entry are all struck |
| C3 | S1's blocker (no total deadline on send) is real, is S0's to fix, and is absent from S0's design | P5. Also fixes S0's own tarpit/zombie-claim finding and the shipped alerting path |
| C4 | Dev observability specified twice: S0's `SMTP_SINK`, S1's separate "log the reset URL at INFO under `SAURON_DEV`", both claiming the same `config.rs` edit | S0 owns the `pub dev_mode` promotion; `SMTP_SINK` subsumes S1's logging path entirely (the sink already logs the rendered message including the link). Delete S1's config.rs bullet |
| C5 | S1 requires `render::html_escape` be made `pub` — in a file S0 moves the function out of | Restate against the real target: `sauron_mail::text::{html_escape, substitute}` are `pub`, `sauron_alerts::render` imports them. S1's substantive point (double-quote every attribute) carries into S0's `LAYOUT_HTML` as an inline comment |
| C6 | Three background-task patterns land in `sauron-api`, which has none, and S2's initialization `?`s out of `main()` | P3. One supervisor built by S0; S2 drops the pre-bind synchronous refresh |
| C7 | `AppState` and the `AuthUser` extractor rewritten by two slices at once (S0 adds `mail`, S2 adds `SessionRevocations` plus a new generic bound `SessionRevocations: FromRef<S>`), while S4 adds a `tokio::sync::Semaphore` field of its own and edits the same router tail | Sequence: S0's field is additive and lands first; S2 lands the `FromRef` + bound change and rebases; S4 lands its semaphore, two routes, the CORS `expose_headers` line and its boot schema probe on top of that shape. Put on `SessionRevocations`' **doc comment**, not just the design: any future binary wanting `AuthUser` must now provide a snapshot, and providing a permanently-empty one silently disables revocation. S4 is not "one line of CORS" and must not be scheduled as though it were |
| C8 | Revocation-reason registry extended by two slices with different constraint models — S1 adds two reasons with no CHECK, S2 adds three plus a CHECK plus a skip-alarm list | P6. S2 lands first, so S2's CHECK carries S1's two reasons from day one — free while `auth_sessions` is empty, and the alternative is that every successful self-service reset 500s at the revoke step. S1 ships exactly one migration and it is not a widening of this CHECK |
| C9 | S1's admin reset calls `revoke_all_refresh_tokens_for_user_with_reason`, which desyncs `auth_sessions` from `refresh_tokens` — `GET /v1/me/sessions` would list sessions with no refresh token and the revocation snapshot would never learn about them | S1 calls S2's `revoke_sessions_for_user`. Add a Rust test that counts call sites of the old fn in the API crate, so a sixth site cannot be added silently — this is the same drift class as `strip_source_context` being applied at 2 of 8 response paths |
| C10 | Two admin member-action routes each copy ~50 lines of guard stack: six distinct guards and ~35 lines of load-bearing why-comment at `routes/orgs.rs:702-760` | Extract `guard_member_admin_action(..., allow_self: bool)`, carrying the comments, and refactor `set_member_active` onto it. Exactly one of the six guards is waivable, because exactly one caller genuinely differs: `allow_self`, since self-revoke-sessions *is* "sign out my other devices" while self-deactivation is not. The cross-org refusal is unconditional inside the helper — every one of these actions is destructive to an account that another organization also depends on. The last-owner concern stays in `set_member_active`, outside the helper |
| C11 | `MembersTable.svelte` row actions collide — S1 and S2 each add a button and a dialog to the same one-div, two-button block | P12 |
| C12 | `#/account` is created by S2 but is also where S3 belongs | P12. S3 adds a card, not a page |
| C13 | S2 asserts that S5's masker "must be scoped to telemetry tables and must NOT be pointed at `auth_sessions`", against a slice that had not been designed | Satisfied and now verifiable: `inspector_masked_keys.target_table` is a CHECK-constrained **allowlist** of exactly six tables — `error_events`, `analytics_events`, `transactions`, `issues`, `event_users`, `sessions` — and S5's scan inventory covers only telemetry tables and rollups. No auth table is reachable. `devices` and `workflows` are scanned but deliberately **not** maskable: a mask on `devices` reports success and is undone by the next event through the process, because `upsert_device`'s `DO UPDATE` is `COALESCE(EXCLUDED.family, devices.family)`. `breadcrumbs` is not a table at all — it is a `jsonb` column on `error_events` and is reached through it. Keep it an allowlist; a denylist would silently fail to cover the next account table |
| C14 | S4 and S5 both modify the ingest job path in `sauron-pipeline` — S4 stamps `identified_at` from an envelope's `context.user.id`, S5 masks values before `process_job` and again after `enrich_context` | S4 lands first, S5 rebases. **The semantic interaction is the real issue**: the mask enforcer runs *before* S4's stamping, so masking a key an app sends as `context.user.id` (exactly the kind of field a PII policy flags, and plenty of apps use an email address there) means the equality test that stamps `identified_at` never passes again. What it does **not** do is un-identify anyone: stamping is first-write-wins and `distinct_id` is never a mask target. The effect is quieter and worse for it — everyone first seen after the mask accumulates as a guest and counts once per app, so the identified share decays with nothing moving on the day the mask lands and no step for anyone to attribute. The `MaskDialog`'s "what this does not reach" panel gains a line about active-user counts, and the S4 wiki page says the same thing |
| C15 | S4 substitutes the `(app_id, environment_id, occurred_at DESC)` indexes on `analytics_events` and `error_events` with `INCLUDE (distinct_id)` variants; S5's scan prefilter costs were measured against the originals | No action, but note it in S5: the key is identical, so the prefilter's plan is unchanged and its scans become marginally cheaper. Do not re-measure and do not add a fourth `app_id`-leading btree |
| C16 | `mail_outbox` gains a second writer process (S3 enqueues from `sauron-alerts`) against S0's stated "sauron-api is the only enqueuer and the only drainer" | Enqueue from anywhere, drain in exactly one place. `sauron-api` remains sole drainer and sole reaper (P4), so `sauron-alerts` needs no SMTP configuration. When this happens, `MailSender` moves out of `bins/sauron-api/src/mail.rs` into a crate — S0 already names this as the trigger |

---

## 8. Scope cuts and deferrals

| Cut | Why |
|---|---|
| Auto-login after a successful password reset | The reset caller is unauthenticated — whoever holds the mailbox holds the link. Auto-login converts a mailbox compromise into a live session rather than one extra password prompt, and it forces the reset endpoint to mint tokens, widening what a replayed link yields |
| Real invitation tokens and an accept page | Needs email, which now exists, but also an accept flow, an org-join path and expiry semantics. `POST /v1/orgs/{org}/members` with a temp password remains the way in |
| Email address verification | No `users` column exists for it, and no flow in the product needs it yet. Adding it now would gate `register`, which is a behaviour change nobody asked for |
| Re-rendering **alert** mail through the new HTML template | Alert mail keeps `text/plain`, the `[Sauron/{severity}] {title}` subject and the `— Sauron alerting` footer. Obvious follow-up, zero risk to defer, non-zero risk to change while also moving the transport |
| An operator "send a test email" endpoint | Genuinely useful and needs no schema change (`MailKind` already carries the `smtp_test` variant, and `mail_outbox.kind` is plain TEXT), but it needs a route, a permission decision and a dashboard surface. Deferred, not designed away |
| A dashboard view of `mail_outbox` | If it is ever built it must project columns explicitly and never return `body_text`/`body_html` |
| Monthly active users | A trailing 30-day window has at most two complete days behind the tier watermark on every shipped default, so MAU would render as a single point or an empty state. Making it real means either doubling every deployment's hot Postgres storage on upgrade or reading cold Parquet, and neither is worth carrying to ship a daily number that stands on its own. S4 ships active users per day and the whole 30-day apparatus goes with it |
| Cross-app identity resolution (a real person entity above `event_users(app_id, distinct_id)`) | Combined active users matches on the exact `distinct_id` string. A mapping table plus a backfill is substantially more work, and the guest/identified split makes the limitation legible instead of hiding it: guests never merge across apps by construction, and the identified count says outright that it merges on the raw string an SDK sent |
| Reading cold Parquet for windows older than the hot tier, and for inspector scans | The mechanism is proven in the tier worker, but `INSTALL postgres` fetches over the network at first use and both systemd units set `ProtectHome=true` while DuckDB writes extensions under `$HOME/.duckdb`. Not free on an RPM install. S4 clamps and reports `truncated`; S5 records `coverage='partial'` with the reason |
| Unmasking | Masking is irreversible by design — that is the feature. The mitigation is a mandatory preview with exact affected-row counts and a typed app slug, not an undo |
| Retroactive masking of `event_users.properties` | The `||` merge in the write path cannot be guarded cheaply. Declared reachable through forward enforcement only, and stated in the spec and the UI rather than quietly not working |
| Masking at the ingest **edge** | Would drag in the `sauron:dsn:v2:` Redis cache and its version-prefix trap. The cost of staying in the pipeline is that the raw value lives in the Redis stream for the `MAXLEN` window. Accepted and stated |
| Reaping `refresh_tokens` | Roughly 96 rows/day per active session and unreaped today. Revoked rows are load-bearing for replay detection, so a reaper needs a design. S2 makes the growth materially worse and carries a one-line TODO in its migration prose naming it |
| Per-table retention environment variables | Compile-time constants instead. Three files of documentation for a value nobody tunes |
| Graceful shutdown / SIGTERM handling | No server binary handles it today. Adding it in one binary is worse than not adding it, because it creates the impression that the others do. It must be done consistently across all binaries as its own piece of work. Everything durable this programme adds is therefore resumable and idempotent by construction |
| `sauron-api` running `run_pending_migrations` at boot | The correct fix for §9's upgrade gap, and out of scope here. Filed — this is the third programme in a row to pay the tax |
| A chart annotation for the SDK anonymous-id discontinuity | Nothing in the dashboard supports one. Release notes instead. Open question 2 |
| Notification delivery to anything but email | S3 delivers to `users.email` only. `notification_channels` stays org-owned and untouched |
| `List-Unsubscribe` headers, DKIM/SPF/DMARC, bounce handling | Relay-side concerns. S3 will want `List-Unsubscribe` for digests and it is a header, not a design |

---

## 9. Programme risks

**RPM upgrades do not run migrations, and this programme ships ten.**
`packaging/rpm/systemd/sauron-migrate.service` has no `[Install]` section and is
not in `%postun`'s restart list, so a `dnf upgrade` installs new binaries against
an old schema. Each slice's symptom differs and one of them is genuinely
dangerous: a skipped 000036 breaks `login` for everyone, because
`find_user_by_email` selects the whole `User` struct and the reset-required check
adds `credentials_invalidated_at` to it; a skipped 000038 breaks
`touch_event_user` on **every ingested event**,
which silently degrades a subsystem nobody is watching; a skipped 000035 with S2
deployed is an authentication outage; a skipped 000043 leaves the pipeline's mask
enforcer reading a table that does not exist on the hot path of ingest. P13
documents the workaround. The root cause should be filed as its own piece of
work — either `sauron-api` runs `run_pending_migrations` at boot the way the test
harness already does, or `%post server` triggers `sauron-migrate`.

**The API connection pool is 16 for the whole process.** Before this programme,
`sauron-api` held zero connections outside request handling. After it, two
background loops check out on a timer, and S4 adds a query that scans two
partitioned tables across a window the caller chooses. The rate limiter (P9) and
the check-out-then-drop rule (P8) are the whole mitigation; there is no
queue-depth metric and no per-tenant fairness. If a wedge happens, it will look
like `POOL_WAIT_TIMEOUT` errors on unrelated endpoints, which is not an obvious
pointer at an analytics query.

**There are no advisory locks and no plan to add any.** Every claim is
`FOR UPDATE SKIP LOCKED` with a lease. This is a constraint, not an accident: a
lock held by a process that took a SIGKILL has nobody to release it, and there is
no shutdown handler anywhere to release it politely. Anything in review that
wants "just take a lock here" is asking for a design change, not a two-line
patch.

**No graceful shutdown exists.** Every server binary dies mid-work. Everything
durable in this programme is therefore designed at-least-once and resumable: the
mail outbox re-sends a message whose `mark_mail_sent` never landed (a user gets
two identical reset emails — accepted, because losing one is worse), the
inspector persists a unit index and a row cursor rather than in-memory progress,
and the retro-mask loses at most one batch. Any new durable work added later must
make the same choice explicitly.

**Three migrations take a real lock on a live path.** 000035 holds
`AccessExclusiveLock` on `refresh_tokens` across an `ADD COLUMN`, a backfill and
a non-partial index build, in one transaction, on the table that authenticates
every request. 000039 and 000040 each rebuild an index on a partitioned parent,
which locks every child of that parent and blocks its ingest write path.
`CONCURRENTLY` is unavailable inside a migration transaction, so none can be
softened — they need a stated window, and none should share a release with
anything else that must go out that day. Note the specific hazard on 000039 and
000040: while the pipeline is blocked on the index lock, `sauron-ingest` keeps
appending to the Redis stream, which is trimmed with `XADD MAXLEN ~1000000`
regardless of the consumer group's pending list. A long enough window silently
discards undelivered events. Drain or stop ingest first.

**The `perm::ALL` arithmetic happens twice — once in S2, once in S5 — and the
second slice must re-read the numbers rather than trust its own design.** S2 adds
`member:credential`; S5 adds `pii:read` and `pii:manage`. In
`backend/crates/sauron-auth/src/rbac.rs`:

| | today | after S2 | after S5 |
|---|---|---|---|
| `perm::ALL` (line 67, and the assertion at 875) | `[&str; 27]` | 28 | 30 |
| Owner (line 812, via `perm::ALL`) | 27 | 28 | 30 |
| Admin (line 818) | 26 | 27 | 29 |
| Developer (line 836) | 18 | 18 | 18 |
| Viewer (line 862) | 7 | 7 | 7 |

All three permissions go to Owner and Admin only. Two of the four preset
assertions never move in value, and the temptation on a red count is to change
the number until the suite is green rather than to look at which preset gained
the permission — which is exactly how `pii:manage` ends up in Developer with
every test passing. Re-read Developer and Viewer on both passes. Each pass also
carries its custom-role `UPDATE roles`: 000035 for
`member:credential`, 000041 granting the `pii:*` pair to custom roles that
already hold `org:manage`. Every other custom role must be granted them
explicitly, which is the point. Then, in the same change as the Rust edit,
`dashboard/src/lib/models/permissions.ts` and the `Permission` union in
`dashboard/src/lib/models/index.ts` — `permissions.test.ts` parses `rbac.rs` and
fails on drift, so a mirror that lands a slice later fails CI for the slice that
did nothing wrong.

The trap adjacent to this is worth naming because it is silent: a permission
picker that submits its full checkbox state **strips every permission missing
from the picker's catalogue** on first save. The catalogue must be regenerated
from `rbac.rs` order in the same change, with the parity test that already exists
for it.

**The extractor's generic bound changes for every future binary.** After S2,
`AuthUser` requires `SessionRevocations: FromRef<S>`. Providing a permanently
empty snapshot compiles and silently disables revocation. That belongs on the
type's doc comment.

---

## 10. Questions answered, and questions still open

### Answered

Each of these is built into the slice documents. They are recorded because a
reader who disagrees with one is disagreeing with a design, not with a default.

- **An admin-initiated reset stops the old password working at the login form.**
  `login` refuses on `users.credentials_invalidated_at` **after** the Argon2
  verification succeeds, never before it, so the timing is identical on both
  branches; it returns a distinct `password_reset_required` code and completing
  the reset clears the column. The admin can also cancel a reset they started:
  destroying a credential on the strength of a mail relay that may be
  misconfigured needs an undo, or one bounced message leaves an account nobody
  can reach. There is exactly one admin mode — the non-destructive "send this
  person a link" variant was never asked for and is not built.
- **No auto-login after a reset.** `{"ok":true}` and a redirect to `#/login`; §8
  carries the reasoning.
- **S4 ships daily numbers only.** Monthly active users is deferred (§8), and
  `TIER_HOT_DAYS` keeps its shipped default of 30 rather than doubling every
  existing deployment's hot Postgres storage on upgrade to feed a window nobody
  is now asking for.
- **Active users are reported as total / identified / guest**, not as one
  combined figure whose accuracy depends on facts the design cannot know.
  Identified keys merge across apps on the raw `distinct_id` string, guest keys
  never merge, and the buckets are disjoint so the three numbers add up. The
  reader can see how much of the total is un-mergeable by construction instead of
  finding it in a wiki footnote. Cross-app identity resolution stays cut (§8).
- **`member:credential` is a new permission, minted by S2** (P7), gating
  force-reset and force-logout. An org can hand out `member:manage` without
  also handing out a path to account takeover through whoever controls the relay.
- **Masking is scoped per app, not per app-environment.** A key that is PII in
  production is PII in staging, and an enforcer keyed on
  `(app_id, environment_id)` doubles the cache and adds an environment lookup to
  the hot ingest path. The cost is that an admin masking a field while testing
  against staging has silently masked it in production, irreversibly, so the mask
  dialog says so.
- **`pii:read` and `pii:manage` go to Owner and Admin only.** Developer stays at
  18 permissions and Viewer at 7. Splitting `reveal` — which returns the raw
  value — onto `pii:manage` would make `pii:read` safe to widen later; it is not
  designed and nothing depends on it.

### Still open

Each has a default, and **every default is already baked into the corresponding
slice document**. Changing one is not a preference edit — it means revising that
slice before it is built. None of them blocks a start.

1. **What does the force-logout UI promise?**
   *Default:* Ship S2 before S1 (§6) so the dialog can honestly say "within a few
   seconds". Kill latency becomes `AUTH_REVOCATION_POLL_SECS` (default 5, clamped
   1-60) per API replica.
   *Alternative:* Ship S1 first and say "within 15 minutes". Faster to a visible
   feature, but an admin containing a compromised account is precisely the person
   who will not read the fine print, and who will skip the other containment steps
   because they believe the button was instant.

2. **The browser SDK anonymous-id fix causes a sharp, permanent, one-time drop in
   every web app's reported active users on the day it is adopted.**
   *Default:* Ship the fix (an inflated count is worse than a discontinuity), land
   it **before** the chart is presented as authoritative, and note the
   discontinuity in the release notes.
   *Alternative:* Ship the chart against today's inflated numbers and fix the SDK
   later. Whoever looks at the chart will read a 5-10× drop as a bug in the
   brand-new feature; if anyone is reporting these numbers onward, a silent
   restatement of historical activity is a trust problem, not a data-quality
   footnote.

3. **Every session row will display the same IP address on both shipped
   topologies.**
   *Default:* Render a dash when `API_TRUST_FORWARDED_HEADERS` is false, and
   document that a meaningful IP requires setting it true behind a proxy that
   **overwrites** `X-Forwarded-For`.
   *Alternative:* Always show the stored address with a tooltip. A sessions list
   where every row reads `10.0.0.5` looks broken and, worse, invites the user to
   conclude "that login wasn't me" on the basis of noise. The inverse hazard is
   real too: turning the flag on without an overwriting proxy lets any client
   claim any IP.

4. **Three forgot-password requests against a known address deny that person
   self-service reset for an hour, and no administrator can shorten it.**
   *Default:* Accept it. The per-email limiter is consumed before the user lookup,
   exactly as `login` does today; the per-IP budget on both `forgot-password` and
   `reset-password` is 60 per 60 seconds, a burst limiter rather than a lockout,
   because behind the shipped proxy every anonymous caller shares one bucket — an
   hour-long deployment-wide budget is a one-second denial of service against the
   whole reset feature, after which every legitimate link-holder gets 429 for the
   remaining 59 minutes. What is genuinely open is only the per-email 3/hour
   lockout. Note that the admin route is no remedy for everyone: it refuses on a
   member who holds grants outside the org, so a multi-org member waits out the
   hour with nothing anyone can do, and for everyone else the "remedy" is a
   destructive reset of their credential.
   *Alternative:* Raise the per-email budget, scope the lockout per (email, IP), or
   drop the per-email limiter and rely on the outbox's natural pacing. This is an
   availability lever an anonymous attacker can pull against a named victim, and
   for a small self-hosted deployment at 2am the victim *is* often the admin,
   locked out.

5. **`SMTP_TLS=none` is accepted for any private relay whenever
   `SMTP_ALLOW_PRIVATE=true`.**
   *Default:* Allow it, with the consequence spelled out in `.env.example` and in
   the config error message.
   *Alternative:* Accept `none` only when the relay resolves to `127.0.0.0/8` or
   `::1`. The default means an operator who set `SMTP_ALLOW_PRIVATE=true` for a
   LAN relay and then set `SMTP_TLS=none` is shipping password-reset links in
   cleartext across their network and the config accepts it silently.
   Loopback-only is meaningfully safer and covers the actual common case (a local
   Postfix); the cost is that a legitimate LAN relay without TLS becomes
   unusable.
