# Per-user notification preferences

Date: 2026-08-01
Status: designed
Slice: S3. Depends on S0 (mail outbox, `sauron-mail`) and S2 (`#/account`, `pub(crate) rate_limit`).
Migration: `2026-08-01-000037_notification_subscriptions`

## Problem

Every notification this product can send is org-owned and admin-typed. An
`alert_rules` row belongs to an organization, and its `notification_channels`
carry a static recipient list somebody pasted into a dialog. There is no
per-person anything:

- A developer who wants to know when *their* app's error rate jumps has to ask
  an admin to add their address to an org channel — and then everyone else on
  that channel gets it too.
- The only way to stop receiving it is to ask the same admin again. No
  unsubscribe exists anywhere in the product.
- There is no per-recipient throttle, no digest, and no quiet hours, so the one
  lever available is "on for everybody, or off for everybody".

A second, unrelated problem is fixed here because this slice cannot be built
correctly around it. `repo::alert_count_errors` (`repo.rs:7270`) narrows by
environment with:

```sql
AND ($5::text IS NULL OR environment_id IN
     (SELECT id FROM environments WHERE name = $5 AND retired_at IS NULL))
```

Since migration `000033_env_per_project`, `environments` is the **project-level
catalogue** and the per-app enrollment table was renamed to `app_environments`.
`error_events.environment_id` holds an **enrollment** id. The subquery therefore
returns catalogue ids, which can never equal an enrollment id, so the predicate
is always false: **every environment-filtered alert rule in the product has been
counting zero and has never fired.** `alert_count_events` has the identical
defect. `alert_latency_metric` takes no environment parameter at all, which is a
different gap and is not fixed here.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| Row shape | One row per `(user, scope, kind)`; `scope_type ∈ {project, app}`; environments in a child table | A single row carrying a JSONB scope tree — unindexable for the evaluator's `WHERE kind = ? AND enabled` load, and it needs bespoke coverage-diff logic on the dashboard instead of `ScopeTree` |
| Which environment id space | Project-level **catalogue** `environments.id` on the subscription; enrollment ids resolved at evaluation time | Storing `app_environments.id` like `role_grants` does — freezes the set at creation and silently excludes apps enrolled later; a magic "all" sentinel UUID — no such token exists anywhere in the codebase |
| Where evaluation runs | A second pass in the existing `sauron-alerts` tick, plus a drain pass; producers enqueue, one drain sends | Synthesizing hidden `alert_rules` per subscription (they would appear in `GET /v1/orgs/{id}/alert-rules` for every admin and need a synthetic org-owned channel an admin could delete); a new worker binary (seven packaging touchpoints for no behavioural gain) |
| What "rate of error increasing" means | Relative with an absolute floor: fire when `C >= min_count AND (B == 0 OR C >= B * factor)` | The shipped `error_spike` predicate (`main.rs:269` guards on `previous > 0`, so zero-to-flood can never fire, and 1→3 pages someone); a purely absolute threshold (that is the org engine's `error_threshold` and the right number differs per app) |
| New permission? | No. `perm::MONITOR_READ` for uptime, `perm::ISSUE_READ` for the error kinds | `notify:subscribe` — a subscription delivers only telemetry the user can already read, so it confers nothing; minting it costs the five coordinated RBAC edits for zero security value. Gating on `alert:read` is wrong: Viewer lacks it entirely |
| One authorization check or two? | Two, plus a sweep: `reach_for` at write time, the same predicate again per queue row at drain time, and a revocation sweep that self-disables | Write-time only (a revoked member keeps receiving telemetry forever); delivery-time only (a 403 arrives as silence instead of an error message) |
| Digest / quiet hours representation | One `deliver_after TIMESTAMPTZ` carries immediate, hourly, daily and quiet-hours deferral | A separate digest table (doubles the code and the audit surface); dropping during quiet hours (silently loses signal and looks identical to "broken") |
| Delivery channel | S0's `mail_outbox`, addressed to `users.email` | `notification_channels` — org-owned with an admin-typed recipient list, so an admin could read, edit or delete another member's personal delivery target |
| The env-resolution bug | Fixed in this slice | Quarantining it and giving subscriptions a private correct resolver — two resolvers, one known-broken, and an invitation to copy the wrong one |
| Does uptime narrow below project? | No. `uptime` accepts `scope_type='project'` only and ignores the environment filter | Accepting app scope and matching the parent project (the user asked for one app and gets the whole project); adding `monitors.app_id` (a real feature, out of scope) |
| Where the unsubscribe link lands | `{DASHBOARD_URL}/#/unsubscribe?token=…`, a no-conditions SPA route that POSTs the token | `GET /v1/notifications/unsubscribe?token=` — mail clients and scanners prefetch GET links, and the API is JSON-only with `default-src 'none'`, so a browser-facing GET would be a new capability |

## Programme reconciliation

These override the S3 slice design where they disagree.

| Item | Reconciled |
|---|---|
| Migration number | **000037**, not 000039. Allocation follows build order: S0=000034, S2=000035, S1=000036, S3=000037, S4=000038-000040, S5=000041-000043, with the date prefix monotone with NN because `run_pending_migrations` orders by the full directory string, date first |
| `mail_outbox.kind` | S0 drops the CHECK in favour of a Rust enum in `sauron-mail`. The variant S3 sends under, `MailKind::PersonalNotification` (`dedup_window = 0`), is defined in S0's `kind.rs`, so S3 neither edits the enum nor needs a migration on that table |
| `#/account` shape | S2 builds it as a **card container** (Profile card, Sessions card), not a tab strip. S3 adds a Notifications card, and adds nothing to `routes.ts` for it |
| `rate_limit` / `client_addr` | Already `pub(crate)` by the time S3 lands (S2 needs them for `routes/account.rs`, S4 for the active-users routes). S3 consumes; it makes no visibility edit |
| Reaper ownership | A table's reaper lives in the process that owns its write path. `notification_queue` is drained by `sauron-alerts`, so its prune runs in the same hourly in-tick slot as `prune_alert_events` |
| `packaging/rpm/SETUP.md` §11 | Created by S0. S3 appends one row to its per-migration table |
| Config documentation | The CI assertion that every `parse("KEY"` literal in `config.rs` appears in `.env.example` exists by now; S3's six keys must satisfy it |

## Non-goals

- Personal subscriptions to `event_threshold` and `perf_degradation`. Analytics
  volume and latency percentiles are team dashboards, not personal inboxes.
- Per-app or per-environment uptime. `monitors` carries only `project_id`.
- Fixing `alert_rules_for_monitor`'s own scoping bug (an app-narrowed rule fires
  for every monitor in its project). Noted, not fixed; it belongs with the
  monitors-app-id work.
- Personal delivery to Slack/Discord/webhook/Telegram. `notification_queue` is
  channel-agnostic by construction, but S3 ships email only.
- `scope_type='org'`. One tick would fan out to every app in the org.
- Admin visibility into other users' subscriptions, org-level defaults, and
  mandatory subscriptions. Subscriptions are strictly personal.
- Migrating the org alerting engine onto `notification_queue`. Only the
  environment resolver is shared.

---

## 1. The three environment id spaces

Read this before anything else. Getting it wrong produces a subscription that
looks right in the database and matches nothing, and that failure is silent at
every layer.

| Where | Id space | Table |
|---|---|---|
| `notification_subscription_envs.environment_id` | **catalogue** | `environments` (project-level, since migration 33) |
| `role_grants.scope_id` for `scope_type='env'`, and `Reach.envs` | **enrollment** | `app_environments` |
| `error_events.environment_id`, `analytics_events.environment_id` | **enrollment** | `app_environments` |
| The dashboard's `currentEnvId` and the `environment_id` query param | **enrollment** | `app_environments` |
| `notification_queue_envs.environment_id` | **enrollment** | `app_environments` |

A subscription stores catalogue ids because the catalogue is exactly the
wildcard RBAC lacks: one catalogue row means "prod, everywhere in this project",
and it stays correct when a new app is created and auto-enrolled in every
project environment. Storing enrollment ids would freeze the set at creation
time.

Everything downstream of the subscription is enrollment ids, because that is
what the event rows and the grants use. Two new resolvers in `repo.rs` are the
only sanctioned bridge:

```rust
pub async fn live_enrollments_for_apps(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>>; // (enrollment_id, app_id, catalogue_env_id)

pub async fn enrollment_ids_for_env_name(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    name: &str,
) -> QueryResult<Vec<Uuid>>; // enrollment ids
```

Both filter `app_environments.retired_at IS NULL`, and the second additionally
filters `environments.retired_at IS NULL`. The `retired_at` rationale comment
currently sitting above `alert_count_errors` moves verbatim onto
`enrollment_ids_for_env_name`: `(app_id, name)` is unique only among *live*
rows, so retiring `staging` and creating a fresh `staging` leaves two rows with
that name, and without the filter the resolver returns both.

## 2. Migration `2026-08-01-000037_notification_subscriptions`

Four new tables, no column added to any existing table. Nothing touches a
partitioned parent and no index is built on `error_events` or
`analytics_events`, so the whole migration is `CREATE TABLE` / `CREATE INDEX`
against new empty relations and takes no meaningful lock. `up.sql` opens with
prose explaining the catalogue-vs-enrollment split and why the queue exists;
`down.sql` drops the four tables in FK order.

```sql
CREATE TABLE notification_subscriptions (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    scope_type        TEXT NOT NULL CHECK (scope_type IN ('project','app')),
    scope_id          UUID NOT NULL,              -- polymorphic, no FK, like role_grants.scope_id
    kind              TEXT NOT NULL CHECK (kind IN
                          ('uptime','error_spike','error_new_issue','error_regression')),
    enabled           BOOLEAN NOT NULL DEFAULT true,
    disabled_reason   TEXT CHECK (disabled_reason IN ('unsubscribed','access_revoked')),
    disabled_at       TIMESTAMPTZ,
    conditions        JSONB NOT NULL DEFAULT '{}'::jsonb,
    delivery          TEXT NOT NULL DEFAULT 'immediate'
                          CHECK (delivery IN ('immediate','hourly','daily')),
    throttle_seconds  INT NOT NULL DEFAULT 900 CHECK (throttle_seconds BETWEEN 0 AND 604800),
    quiet_start_min   SMALLINT CHECK (quiet_start_min BETWEEN 0 AND 1439),
    quiet_end_min     SMALLINT CHECK (quiet_end_min BETWEEN 0 AND 1439),
    quiet_tz          TEXT NOT NULL DEFAULT 'UTC',
    last_evaluated_at TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((quiet_start_min IS NULL) = (quiet_end_min IS NULL))
);
CREATE UNIQUE INDEX notification_subscriptions_user_scope_kind_key
    ON notification_subscriptions (user_id, scope_type, scope_id, kind);
CREATE INDEX notification_subscriptions_kind_idx
    ON notification_subscriptions (kind) WHERE enabled;
CREATE INDEX notification_subscriptions_user_idx ON notification_subscriptions (user_id);
CREATE INDEX notification_subscriptions_org_idx  ON notification_subscriptions (org_id);
```

`scope_id` is polymorphic with no FK, exactly like `role_grants.scope_id`, so a
row can outlive its target and every read path must tolerate an unresolvable id.
`last_evaluated_at` is seeded to `now()` at INSERT so a new subscription cannot
retro-storm, copying what rule creation already does in `routes/notifications.rs`.
`quiet_tz` is an IANA name validated at write time against `pg_timezone_names`.

```sql
CREATE TABLE notification_subscription_envs (
    subscription_id UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    environment_id  UUID NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    PRIMARY KEY (subscription_id, environment_id)
);
CREATE INDEX notification_subscription_envs_env_idx
    ON notification_subscription_envs (environment_id);
```

Composite PK with no surrogate id, mirroring `alert_rule_channels`. **These are
catalogue ids** and the migration says so in a `COMMENT ON COLUMN`. An empty set
means all environments including unattributed events.

```sql
CREATE TABLE notification_queue (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id       UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    user_id               UUID NOT NULL,
    org_id                UUID NOT NULL,
    project_id            UUID NOT NULL,
    app_id                UUID,                    -- NULL for uptime
    includes_unattributed BOOLEAN NOT NULL DEFAULT false,
    kind                  TEXT NOT NULL,
    dedup_key             TEXT NOT NULL,
    severity              TEXT NOT NULL DEFAULT 'warning'
                              CHECK (severity IN ('info','warning','critical')),
    title                 TEXT,                    -- nullable: blanked on dropped_no_access
    body                  TEXT,
    link                  TEXT,
    occurred_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deliver_after         TIMESTAMPTZ NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
                              ('pending','claimed','sent','dropped_no_access',
                               'dropped_inactive','dropped_unsubscribed','failed')),
    attempts              SMALLINT NOT NULL DEFAULT 0,
    message_id            UUID,                    -- one id per delivered email
    claimed_at            TIMESTAMPTZ,
    sent_at               TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ,             -- set on any terminal status
    error                 TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notification_queue_envs (
    queue_id       UUID NOT NULL REFERENCES notification_queue(id) ON DELETE CASCADE,
    environment_id UUID NOT NULL,                  -- app_environments.id; deliberately NO FK
    PRIMARY KEY (queue_id, environment_id)
);

CREATE INDEX notification_queue_due_idx
    ON notification_queue (deliver_after) WHERE status = 'pending';
CREATE UNIQUE INDEX notification_queue_live_dedup_key
    ON notification_queue (subscription_id, dedup_key) WHERE status IN ('pending','claimed');
CREATE INDEX notification_queue_user_created_idx ON notification_queue (user_id, created_at DESC);
CREATE INDEX notification_queue_user_sent_idx
    ON notification_queue (user_id, sent_at DESC) WHERE status = 'sent';
CREATE INDEX notification_queue_finished_idx
    ON notification_queue (finished_at) WHERE finished_at IS NOT NULL;
ALTER TABLE notification_queue
    SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_analyze_scale_factor = 0.01);
```

Three details are load-bearing and were each a review finding:

- **`notification_queue_envs` has no FK on `environment_id`.** A cascade delete
  would silently *shrink* a row's environment list, and an empty list is read as
  "the body spans everything", so a deleted enrollment would **widen** a queue
  row's implied scope instead of narrowing it. An unresolvable enrollment id is
  simply unreachable at drain time, which fails closed.
- **`notification_queue_live_dedup_key` is the explicit `ON CONFLICT` target.**
  Without a unique constraint, `ON CONFLICT DO NOTHING` can only ever fire on the
  `id` PK, i.e. never — the clause would read as idempotency while providing
  none. Scoping the index to live rows means a row that already sent does not
  block the next legitimate notification.
- **`autovacuum_*_scale_factor` are set here.** Unlike `alert_events`, this is a
  work queue: every notification costs one INSERT plus two UPDATEs, `status`
  appears in a partial index predicate so neither update is HOT-eligible, and
  three heap versions per row against default autovacuum thresholds leaves a
  bloated heap the prune then has to scan.

### `schema.rs` and `models.rs`

Four hand-written `diesel::table!` blocks — a **+4** delta on whatever the file
holds when S3 lands, never an absolute count: S0, S1 and S2 each add one ahead of
it, so any number pinned here would be stale before the slice is built. Four
`joinable!` lines (`notification_subscriptions -> users`, `-> organizations`,
`notification_subscription_envs -> notification_subscriptions`,
`notification_queue -> notification_subscriptions`; `notification_queue_envs`
joins on `queue_id` and needs its own). All four names appended to
`allow_tables_to_appear_in_same_query!`. **No `Array<>` column is introduced** —
the environment sets are child tables precisely because `schema.rs` has zero
`Array` columns today, only `Array` binds inside `sql_query`.

`models.rs` gains `NotificationSubscription` (`Queryable, Selectable,
Serialize`) + `NewNotificationSubscription<'a>`, `NotificationQueueItem` (also
`QueryableByName`, because the claim is a `sql_query ... RETURNING *`) +
`NewNotificationQueueItem<'a>`, and the two child-row structs. The joined
environment list and best-effort scope names live on a `SubscriptionView` struct
in the route module, never on the row struct.

## 3. The four kinds

`SubKind` lives in the new `backend/crates/sauron-alerts/src/subscription.rs`
with `parse` / `as_str` / `ALL`, following `TriggerType`'s shape in `rule.rs`.

| Kind | Scope types | Env filter | Conditions (default, clamp) | Dedup key |
|---|---|---|---|---|
| `uptime` | `project` only | ignored | none | `sub:{id}:incident:{incident_id}:{trigger}`, falling back to `sub:{id}:monitor:{monitor_id}:{trigger}` |
| `error_spike` | `project`, `app` | yes | `window_seconds` 900 (300..86400), `factor` 3.0 (1.5..100), `min_count` 10 (1..100000), `level` `null` | `sub:{id}:spike:{app_id}` |
| `error_new_issue` | `project`, `app` | yes | `level` `"error"` | `sub:{id}:issue:{issue_id}` |
| `error_regression` | `project`, `app` | yes | `level` `"error"` | `sub:{id}:issue:{issue_id}` |

### `error_spike` — what "rate of error increasing" means

Two windows, the same shape the shipped engine uses so one probe serves both.
With window `W`:

- `C` = error count over `(now - W, now]`
- `B` = error count over `(now - 2W, now - W]`
- Fires when `C >= min_count AND (B == 0 OR C >= B * factor)`.

The `B == 0` disjunct is deliberate: the shipped predicate at
`backend/bins/sauron-alerts/src/main.rs:269` is `previous > 0 && …`, which makes
the zero-to-flood case — an app that was silent and is now on fire — the one
case that can never fire. `min_count` is equally deliberate: without a floor, a
1 → 3 movement is a 3× spike and pages someone at 04:00. The two defaults are
chosen together so that a quiet app emitting 10 errors in 15 minutes notifies
once, while a noisy app must additionally show a genuine 3× jump.

This is relative by construction. An absolute threshold already exists as the
org engine's `error_threshold`; it is admin-owned and the right number differs
per app, which is exactly why it is the wrong instrument for a personal
subscription.

### `error_new_issue` and `error_regression` — the clock the EXISTS uses

Both reuse the shipped queries. `alert_new_issues` filters `issues.created_at`
(Postgres `now()` at INSERT) and its comment records why: `first_seen` is the
SDK-supplied timestamp and loses the race with pipeline latency.
`alert_regressed_issues` filters `last_event_at`, the ingest-side twin of
`last_seen`, and never `updated_at` (status changes bump that).

`issues` has no `environment_id` column, so environment narrowing must go
through `error_events`. The naive form — adding
`AND EXISTS (SELECT 1 FROM error_events e WHERE e.issue_id = i.id AND
e.environment_id = ANY($n) AND e.occurred_at > $from AND e.occurred_at <= $to)` —
**mixes two clocks and silently loses issues**: `$from`/`$to` come from the
server-clock watermark while `occurred_at` is SDK-supplied. A backdated or
offline batch creates an issue whose `created_at` is inside the window while
every one of its events sits outside it, the EXISTS returns false, and the
subscription never fires with nothing logged.

So `alert_new_issues_env` and `alert_regressed_issues_env` bound the EXISTS by
the issue's **own** ingest-side timestamps instead of the tick window:

```sql
AND EXISTS (
    SELECT 1 FROM error_events e
     WHERE e.issue_id = i.id
       AND e.environment_id = ANY($5)
       AND e.occurred_at >  i.first_seen - interval '1 hour'
       AND e.occurred_at <= i.last_event_at
)
```

The `- interval '1 hour'` absorbs client clock skew in the direction that
matters. The predicate is served by `error_events_issue_env_time_idx
(issue_id, environment_id, occurred_at DESC)` from migration 31, and the
`occurred_at` bounds still prune partitions. When a subscription's environment
set is empty the unfiltered fns are used and no EXISTS is added at all.

The outer `LIMIT 20` becomes `LIMIT $n + 1` where `n = min(20 × app_count, 200)`,
because a probe now spans several apps and a fixed 20 lets one noisy app starve
the rest. The extra row is the truncation sentinel: if `n + 1` rows come back the
rendered email says "and more".

### Why uptime does not narrow

`monitors` carries only `project_id` — no `app_id`, no `environment_id` — so
there is nothing below project to narrow on. Accepting an app-scoped uptime
subscription that can never fire is worse than refusing it; `alert_rules_for_monitor`
already has that bug and it is not reproduced here. `POST` rejects
`scope_type='app'` for `kind='uptime'` with 400, and the dialog states that the
environment filter does not apply.

## 4. Where evaluation happens

The tick loop in `backend/bins/sauron-alerts/src/main.rs` gains two in-tick
scheduled sub-jobs, using the established `if (Utc::now() - last_x).num_… >= N`
idiom rather than new timers, beside the existing `last_prune` block:

| Sub-job | Cadence | Work |
|---|---|---|
| `evaluate_subscriptions` | `NOTIFY_SUBS_TICK_SECS` (120) | Load, coalesce, probe, fan out, enqueue |
| `drain_notification_queue` | every tick (`ALERTS_TICK_SECS`, 30) | Claim, re-check reach, group, render, hand to `mail_outbox` |
| `prune_notification_queue` | hourly slot | Delete terminal rows past retention |
| `requeue_stuck_notifications` | hourly slot | Return abandoned `claimed` rows to `pending` |
| `sweep_revoked_subscriptions` | daily slot | Self-disable subscriptions whose owner lost reach |

120s for evaluation is deliberately slower than the 30s org tick: personal email
does not need 30s latency, and cadence is the single largest cost lever in the
whole design. The drain runs every tick so `immediate` really is immediate.

**Producers never send mail.** Both the evaluator and `sauron-monitor`'s inline
`notify_transition` only INSERT into `notification_queue`. That split is what
lets the prober participate without ever learning about SMTP, and it is why the
queue table exists rather than enqueueing straight into `mail_outbox`.

`sauron-alerts` builds its pool with `build_pool(&cfg.database_url, 8)` and
bounds rule evaluation with `Arc<Semaphore::new(4)>`. The subscription pass
reuses both, and follows the same `drop(conn)` discipline the file already
comments: load under a connection, drop it, then fan out.

## 5. Probe coalescing

Naive evaluation is `N users × M scopes` and is the failure mode this design
exists to avoid. But the obvious fix — one probe per app — is *worse* than what
already ships: `alert_count_errors`, `alert_new_issues` and
`alert_regressed_issues` all take `app_ids: &[Uuid]` and filter
`app_id = ANY($1)`, so a rule over a 200-app project costs **one** query today.
Keying a probe on a single app id would turn that into 200.

So the probe key deliberately does **not** contain an app id:

```rust
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct ProbeKey {
    pub org_id: Uuid,
    pub kind: SubKind,
    pub cond: CondBucket,           // fully quantized, see below
    pub catalogue_envs: Vec<Uuid>,  // sorted; empty means "all environments"
}
```

`CondBucket` holds `window_seconds: u32`, `min_count: i64`,
`level: Option<String>` and `factor_milli: u32` — `factor` snapped to the
nearest 0.25 after clamping. Quantizing is not decoration: `f64` is not `Ord`,
so a raw factor cannot be a `BTreeMap` key at all, and distinct float values
would defeat coalescing entirely, which is a cheap denial-of-service given that
`POST /v1/auth/register` is open and every registrant becomes an org Owner.

`org_id` is in the key for two reasons: it makes the per-org ceiling in §5.2
expressible, and it makes a cross-tenant mix-up structurally impossible because
no probe's app array ever spans organizations.

### 5.1 The pass

1. Load every enabled non-uptime subscription in one query
   (`enabled_subscriptions_by_kind`, served by
   `notification_subscriptions_kind_idx`).
2. Resolve every `project` scope to app ids in one batched `apps` query, and
   every subscription's catalogue env set in one batched child-table query.
3. Call `live_enrollments_for_apps` once over the union of all app ids.
4. Group subscriptions into a `BTreeMap<ProbeKey, Vec<SubIdx>>`. Since almost
   every subscription uses defaults, this collapses hard.
5. Per probe: app array = union of the in-scope apps of its subscriptions;
   enrollment array = live enrollments of those apps whose catalogue env is in
   `key.catalogue_envs` (`None` when the set is empty). Run once under the
   existing `Semaphore(4)`.
6. Fan out **by app id**, not by positional index.

Step 6 is the mitigation for the highest-consequence new logic in this slice. A
key-collision bug would attribute one app's counts to another user's
subscription — a telemetry leak inside an email. App ids are globally unique
UUIDs, so a wrong attribution requires an id bug rather than a set-membership
bug, and the drain's independent reach re-check catches the cross-tenant case
even then.

Per-app results need per-app rows, so `repo.rs` gains grouped variants:

```rust
pub async fn alert_count_errors_by_app(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: Option<&[Uuid]>,
) -> QueryResult<Vec<(Uuid, i64)>>;          // GROUP BY app_id
```

`alert_new_issues_env` / `alert_regressed_issues_env` already return `app_id` in
`AlertIssueBrief`, so they need no grouping.

Cost is therefore `O(orgs × kinds × distinct condition buckets × distinct env
sets)` — independent of both user count and app count, and never worse than the
existing engine.

### 5.2 Ceilings, and who gets clipped

A single global probe ceiling is a cross-tenant starvation vector: a handful of
self-registered accounts saturating it silently stops evaluating a paying
tenant's subscriptions. So:

- `MAX_SUBSCRIPTIONS_PER_USER = 50`, a compile-time const enforced at write time
  with a 409.
- `NOTIFY_SUBS_MAX_PROBES_PER_ORG` (default 50) — a **per-org** ceiling.
- Orgs are processed in rotating order: sort org ids, start at
  `tick_counter % org_count`, wrap. A clip therefore moves around instead of
  always landing on the same alphabetically-unlucky tenant.
- When an org is clipped, log
  `warn!(org = %id, probes, skipped, "subscription probe ceiling reached")` and
  increment a per-org counter surfaced in the same log line, so "we are not
  evaluating your subscriptions" is observable rather than inferred.

Both numbers are guesses, not measurements. They are named as such in the
follow-ups.

## 6. Enqueue: throttle, dedup, and `deliver_after`

Throttling reuses the shipped primitive. At enqueue time,
`redis.set_nx_ex("sauron:notify:{subscription_id}:{dedup_key}", "1",
throttle_seconds)`:

- `Ok(true)` → enqueue.
- `Ok(false)` → skip silently.
- `Err(_)` → durable fallback `repo::notification_recently_queued(subscription_id,
  dedup_key, throttle_seconds)`, the direct analogue of `alert_recently_sent`.

Extending the Redis key with the subscription id is what gives per-recipient
throttling with no new infrastructure — the org engine's key is per rule.

`deliver_after` is computed **entirely in the enqueue SQL**. It has to be: the
workspace has no `chrono-tz` (adding one is a workspace-dependency edit
affecting every crate), so nothing in Rust can produce a subscription's local
wall-clock time. The insert is one statement joining the subscription row:

```sql
INSERT INTO notification_queue
    (subscription_id, user_id, org_id, project_id, app_id, includes_unattributed,
     kind, dedup_key, severity, title, body, link, deliver_after)
SELECT s.id, s.user_id, s.org_id, v.project_id, v.app_id, v.includes_unattributed,
       v.kind, v.dedup_key, v.severity, v.title, v.body, v.link,
       <deliver_after CASE over s.delivery, s.quiet_*, tz>
  FROM unnest($1::uuid[], $2::uuid[], …) AS v(subscription_id, project_id, …)
  JOIN notification_subscriptions s ON s.id = v.subscription_id
    ON CONFLICT (subscription_id, dedup_key) WHERE status IN ('pending','claimed')
    DO NOTHING
```

The `CASE` resolves in three steps against
`tz := COALESCE((SELECT n.name FROM pg_timezone_names n WHERE n.name = s.quiet_tz), 'UTC')`:

1. base = `now()` for `immediate`; `date_trunc('hour', now()) + interval '1 hour'`
   for `hourly`; `(date_trunc('day', now() AT TIME ZONE tz) + interval '1 day')
   AT TIME ZONE tz` for `daily`.
2. if `quiet_start_min IS NULL` → base.
3. otherwise, if base's local minute-of-day falls inside `[quiet_start_min,
   quiet_end_min)` (wrap-around aware), push it to that day's `quiet_end_min` in
   local time, converted back with `AT TIME ZONE tz`, plus a day when the window
   wrapped past midnight.

The `pg_timezone_names` lookup is not paranoia: a zone that validated at write
time can vanish with an OS tzdata update, `now() AT TIME ZONE 'Missing/Zone'`
raises, and one bad row must not kill the batch. Falling back to UTC is visible
in the tab (which renders the effective zone) rather than silent.

The pure wrap-around predicate still lives in Rust as
`subscription::in_quiet_hours(local_minute, start, end)` — not because the
enqueue calls it, but because it is the only form a unit test can reach, and a
DB test asserts the SQL and the Rust agree over a shared table of cases.

## 7. Uptime: how the monitor participates

`sauron-monitor`'s `notify_transition` (`bins/sauron-monitor/src/main.rs:279`)
already acquires a pooled connection to load alert rules and channels. S3
enqueues there — but **not** where the slice design put it. The function returns
early three times before the channel-loading window:

```rust
TransitionKind::None => return,              // correct: nothing transitioned
Err(e) => { warn!(…); return; }              // no db connection
if rules.is_empty() { return; }              // ← the problem
```

A project whose admin configured no `monitor_down`/`monitor_up` alert rule is
*exactly* the deployment where a personal uptime subscription is the entire
point, and under the third early return it would enqueue nothing, forever, with
no log line. So:

- The subscription enqueue happens immediately after the connection is acquired
  and **before** `alert_rules_for_monitor`'s result is inspected.
- `if rules.is_empty() { return; }` becomes a skip of the rule loop, not a
  function return.

Ordering around Redis is the other half, and it is the difference between safe
and a prober outage. `RedisStore` is built with `set_response_timeout(None)`
(`sauron-redis/src/lib.rs:74`), and `routes/auth.rs:132` records a measured 9–19s
stall per command against a dead Redis — which is why `rate_limit` wraps it in a
250 ms `LIMITER_TIMEOUT`. `notify_transition` is `tokio::spawn`ed per transition,
the monitor's pool is `monitor_max_concurrency + 8` and `monitor_batch` is 100,
so a network fault that both degrades Redis and flips many monitors could pin
up to 100 connections for 19s each and starve `record_check_and_state`. Uptime
probing would die precisely when it matters. Therefore, in the monitor:

1. Load `repo::uptime_subscriptions_for_project(m.project_id)`.
2. `drop(conn)`.
3. Run the Redis claim wrapped in `tokio::time::timeout(Duration::from_millis(250), …)`,
   falling through to the durable check on timeout or error.
4. Re-acquire a connection for the INSERT.

`uptime_subscriptions_for_project` returns only subscriptions whose owner passes
the §8 project-level reach test, so the enqueue does not manufacture rows the
drain will only discard.

## 8. Authorization, twice

### 8.1 The predicate

`subscription::covers` is pure and lives beside the rest of the decision logic:

```rust
pub struct QueueTarget<'a> {
    pub project_id: Uuid,
    pub app_id: Option<Uuid>,          // None for uptime
    pub env_enrollments: &'a [Uuid],   // app_environments ids
    pub includes_unattributed: bool,
}

pub fn covers(reach: &Reach, t: &QueueTarget<'_>) -> bool
```

In order:

1. `reach.org` → true.
2. `reach.projects.contains(&t.project_id)` → true.
3. `t.app_id.is_none()` → **false**. Uptime stops here.
4. `reach.apps.contains(&app_id)` → true.
5. `!t.includes_unattributed && !t.env_enrollments.is_empty() &&
   t.env_enrollments.iter().all(|e| reach.envs.contains(e))` → true.
6. otherwise false.

Arm 3 exists because every monitor read in the product is
`authorize_project(&mut conn, user_id, project_id, perm::MONITOR_READ)`
(`routes/monitors.rs:67`), which resolves with `app: None, env: None`, and
`grant_applies` never lets a `Scope::App` or `Scope::Env` grant satisfy that.
An app- or env-scoped member gets 403 from every monitor endpoint. Authorizing
an uptime subscription with the per-app coverage test would have mailed them
monitor names, targets, causes and incident ids the API itself refuses them.

Arm 5 is why the environment set is never collapsed to a single column and why
an empty list is never read as "unconstrained". A probe with no environment
predicate counts across every enrollment *and* unattributed rows, so it needs
app-level reach; a probe with an explicit enrollment list can be released to an
env-scoped member only if they hold **every** enrollment in it. Getting this
backwards in either direction is the whole failure: NULL-as-unconstrained leaks,
NULL-as-nothing starves an env-scoped subscriber silently at debug level.

### 8.2 Write time

`POST` / `PATCH`:

1. Resolve the scope's org from the scope itself — `repo::project_org` or
   `repo::app_ancestry`; 404 on unknown. **`org_id` is never accepted from the
   request body.**
2. `repo::user_grants_in_org(user_id, org_id)`; 403 on empty (non-membership).
3. `grants_from_rows` + `reach_for(perm::MONITOR_READ)` for uptime,
   `reach_for(perm::ISSUE_READ)` for the error kinds.
4. Uptime: accept only if `reach.org || reach.projects.contains(&scope_id)`.
5. Error kinds: expand the scope to its app set; **reject an empty app set with
   422** rather than letting a `∀` succeed vacuously (a project with no apps
   would otherwise let any org member subscribe to anything), then require
   `covers()` for every app with the subscription's resolved enrollments.
6. Validate every submitted environment id is a live `environments` row of the
   scope's project.
7. Enforce `MAX_SUBSCRIPTIONS_PER_USER`.

### 8.3 Delivery time

The write-time check is a point-in-time snapshot, and reach can be revoked
afterwards. The drain repeats the entire computation against freshly loaded
grants immediately before rendering — the last moment the data is still inside
the trust boundary — and it does **not** trust the denormalized `org_id`:

`reach_for`'s org arm is `Scope::Org(_) => reach.org = true`
(`rbac.rs:291`); it never compares the org id, and its own doc comment warns
that an unfiltered grant list would leak another org's visibility (pinned by
`reach_for_org_arm_does_not_compare_the_org_id`). If a queue row's `org_id` ever
diverged from the true owner of its `project_id`, `reach.org` would go true and
`covers()` would accept a foreign tenant's project. So the drain re-derives the
org with a batched `repo::project_org_batch(project_ids)`, uses **that** for
`user_grants_in_org`, and treats a mismatch with the stored `org_id` as a hard
drop plus a `warn!`. The denormalized column stays for indexing and the sweep;
it is no longer the tenant boundary.

Rows failing the check are marked `dropped_no_access` at debug level (losing
access is normal, not a warning) **and have `title`, `body` and `link` set to
NULL in the same UPDATE**. The content has no further purpose and must not sit
at rest for the retention window outside the reader's authorization.

### 8.4 The revocation sweep

`sweep_revoked_subscriptions` does not ask "does this user still have any
grants in the org". The overwhelmingly common revocation is *partial* — moved
off a project, an env grant narrowed, a role downgraded so it no longer carries
`issue:read` — and in every one of those the user still has grants in the org.
The sweep therefore evaluates the §8.2 predicate against each subscription's
actual scope and required permission, and on failure sets `enabled = false`,
`disabled_reason = 'access_revoked'`, `disabled_at = now()`.

A daily sweep still leaves a 24h window, so it is additionally invoked
synchronously — same request, after the grant change commits — from
`routes/orgs.rs`'s `delete_grant`, the `PATCH /v1/grants/{id}` handler, and
`set_member_active`. The daily pass remains as the backstop for paths nobody
remembered (role permission edits, project deletion).

Re-granting access does **not** silently resurrect a disabled subscription. The
user re-enables it themselves, and the card explains why it is off.

### 8.5 The history endpoint

`GET /v1/me/notifications` must apply `covers()` too, with the same freshly
loaded grants, and drop non-covered rows. Ownership alone is not a sufficient
gate: the row was written with a title and body at enqueue time, and a member
whose grant was revoked would otherwise authenticate and read exactly the issue
titles and counts the drain just refused to mail them. Blanking on
`dropped_no_access` (§8.3) covers the rows the drain caught; the filter covers
the rows whose access changed after they were already sent.

## 9. The drain

```sql
UPDATE notification_queue
   SET status = 'claimed', claimed_at = now(), attempts = attempts + 1
 WHERE id IN (
     SELECT id FROM notification_queue
      WHERE status = 'pending' AND deliver_after <= now()
      ORDER BY deliver_after
      FOR UPDATE SKIP LOCKED
      LIMIT $1
 ) RETURNING *
```

The `status = 'claimed'` write is the entire point and is the one thing that
cannot be copied from `claim_due_monitors` without thinking. That query's
exclusivity comes from its SET clause — `next_check_at = now() + make_interval(…)`
moves the row out of the inner SELECT's predicate at commit. `FOR UPDATE SKIP
LOCKED` alone only skips rows locked by an *uncommitted* transaction; once
replica A commits, replica B's next pass re-selects the same rows and mails them
again. A `claimed` state that leaves the partial index is what makes the claim
real, and `attempts` is what makes a crash between claim and terminal status
recoverable instead of an infinite redelivery loop.

`requeue_stuck_notifications` returns rows `claimed` for longer than 15 minutes
to `pending`, or to `failed` once `attempts >= 3`, both compile-time consts.

One drain pass **loops** the claim until it returns fewer than
`NOTIFY_SUBS_BATCH` rows or a `NOTIFY_DRAIN_BUDGET_MS` wall-clock budget is
spent. A single 200-row batch per 30s tick is ~400 rows/minute, and two shapes
exceed that routinely: every `daily` subscriber's rows come due at the same
bucket boundary, and a broad incident enqueues across many subscribers at once.
Each pass logs pending depth and the oldest pending `deliver_after`, because
nothing else in the system would reveal a backlog — `status='sent'` means only
"handed to the outbox".

Per claimed batch, grouped by `user_id`:

1. Reload the user row. `is_active = false` → mark every row
   `dropped_inactive`, never mail.
2. Re-derive orgs, reload grants once per `(user_id, org_id)`, run `covers()`
   per row (§8.3).
3. Count `COUNT(DISTINCT message_id)` for that user over the trailing hour from
   `status='sent'` rows. Over `NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR`, the
   surviving rows are merged into **one** digest message rather than dropped.
   Counting distinct `message_id` rather than rows is not a nicety: one
   legitimate grouped email carrying 25 issue rows would otherwise report 25
   against a cap of 20 and degrade the user to digests on their first normal
   delivery.
4. Render the survivors into one HTML + plain-text multipart body using S0's
   house template, mint one `message_id`, insert **one** `mail_outbox` row under
   `MailKind::PersonalNotification` addressed to `users.email`, stamp
   `message_id`, `status='sent'`, `sent_at`, `finished_at` on every row in the
   message.

`MailKind::PersonalNotification` is defined in S0's `kind.rs` and its
`dedup_window` is zero, which is load-bearing rather than an omission. S3 already
de-duplicates twice — the Redis `SET NX EX` per `(subscription, dedup_key)` at
enqueue and the partial unique index standing behind it — so a per-recipient
suppression window at the outbox can only discard mail that survived both. With
the hourly cap at 20 messages per user, a 15-minute window would swallow roughly
sixteen of them while their queue rows still read `sent`: silent loss that no
signal in this design would reveal.

Rendering deliberately does not go through `sauron_alerts::deliver` or construct
an `AlertContext`: `render::email_subject` would stamp `[Sauron/info]` on it and
`render::email_body` would sign it "— Sauron alerting". Personal mail must not
carry alert-engine branding, and `sauron-alerts-bin` takes a dependency on
`sauron-mail` solely for the template renderer — no `lettre`, no SMTP config.
Note that `sauron_mail::text::html_escape` does **not** escape the single quote,
so every attribute in the rendered body must be double-quoted.

## 10. Noise control

| Lever | Where it applies | Behaviour |
|---|---|---|
| `throttle_seconds` (default 900) | enqueue | Redis `SET NX EX` per `(subscription, dedup_key)`; durable fallback when Redis is down |
| `delivery` | enqueue | `deliver_after` = now / next hour boundary / next local midnight |
| Quiet hours | enqueue | Defer to the window end. **Never drop** — a night-time outage is still reported at 08:00 |
| Per-user hourly cap | drain | Degrade to one digest, never discard |
| `MAX_SUBSCRIPTIONS_PER_USER` | write | 409 at 50 |

Quiet hours defer rather than drop because a user who enables them expects to
sleep, not to lose the incident. Dropping would also make "quiet" and "broken"
indistinguishable from the user's side, which for an observability product is
the worst available outcome.

The cap's degradation can starve: a user permanently over it only ever receives
digests and may never notice their configured `immediate` is not what they get.
The list endpoint therefore returns `effective_delivery` alongside `delivery`,
and the card renders the effective one.

## 11. Unsubscribe

No unsubscribe mechanism exists anywhere in the product and `hmac_sha256_hex`
(`sauron-alerts/src/crypto.rs:85`) is the only signing primitive. Both binaries
already depend on the `sauron-alerts` crate, so the token functions go beside it
in the same file:

```rust
pub fn unsubscribe_token(key: &[u8], sub_id: Uuid, user_id: Uuid, issued_day: i64) -> String;
pub fn verify_unsubscribe_token(key: &[u8], token: &str) -> Option<Uuid>;
```

Token = `{subscription_id}.{issued_day}.{first 32 hex chars of
hmac_sha256_hex(unsub_key, "sauron-unsub-v1:{sub_id}:{user_id}:{issued_day}")}`,
where `issued_day` is days since the epoch at send time. Verification is a
constant-time compare, and no database read happens before the rate limiter.

Two things the slice design got wrong and this fixes:

- **The key is derived, not raw.** `unsub_key = hmac_sha256_hex(notify_key,
  b"sauron-unsub-key-v1")`. `NOTIFY_SECRET_KEY` is documented in `README.md` as
  the AES-GCM key that encrypts stored channel secrets, so "rotate it to
  invalidate outstanding links" is not an available mitigation — rotating it
  makes every stored Slack webhook URL and SMTP password undecryptable. Domain
  separation at least keeps the two uses independent.
- **Tokens expire.** `issued_day` is inside the signed message, and verification
  rejects anything older than `UNSUB_TOKEN_TTL_DAYS = 90` (a compile-time const,
  not an env var) or dated in the future. Every send mints a fresh token, so
  links in live mail always work; a token forwarded into an archive stops being
  a permanent silencer of someone else's uptime alerts.

`POST /v1/notifications/unsubscribe` is unauthenticated, rate limited at
`sauron:notify:unsub:{client_addr}` 30/60s using the now-`pub(crate)`
`rate_limit`/`client_addr`, sets `enabled = false`,
`disabled_reason = 'unsubscribed'`, `disabled_at = now()` on exactly that one
subscription, and returns a generic 200 whether or not the token matched.

Because this repo has no audit table, the only repudiation control is
observability at both ends: a structured `info!` line with the subscription and
user ids, and **one `mail_outbox` row to the owner** confirming which
subscription was disabled with a link back to `#/account`. Without it, neither
the owner nor an operator could tell a silencing happened. It goes out under the
same `MailKind::PersonalNotification`, and is the sharpest case for that kind's
zero dedup window: a confirmation suppressed because a notification reached the
same address minutes earlier would erase the only evidence of the silencing.

The link in every notification points at
`{DASHBOARD_URL}/#/unsubscribe?token=…`. `DASHBOARD_URL` (S0) is load-bearing
here and fails closed at point of use: unset means the notification still sends
with `link = NULL` and the unsubscribe footer replaced by a line telling the
user where to manage subscriptions. Never bail in `Config::from_env`.

## 12. The environment-resolution bug fix

Query-only; no migration.

- `alert_count_errors` and `alert_count_events` change their `environment:
  Option<&str>` parameter to `env_ids: Option<&[Uuid]>`, and `$5` from the
  catalogue subquery to `environment_id = ANY($5)`, bound as
  `Nullable<Array<Uuid>>`.
- Name resolution moves into the callers. The legacy `alert_rules` path in
  `bins/sauron-alerts/src/main.rs` resolves `conditions.filters.environment` (a
  name — the right admin-facing input) through `enrollment_ids_for_env_name`.
  The subscription path skips the name step entirely because it stores catalogue
  ids.
- A resolution yielding an empty set short-circuits to a count of zero
  **explicitly**, with a comment, rather than by accident through an empty
  `ANY()`.
- The `retired_at IS NULL` rationale comment moves verbatim onto the resolver.

**Deploy landmine, and it belongs in the release notes.** Alert rules that name
a real environment have been counting zero since migration 33 and will start
firing for the first time on the first tick after deploy. This is the same
failure shape migration 21's `GREATEST` backfill existed to prevent.
`throttle_seconds` and the window clamp bound it to one message per rule per
throttle period, but an operator with many environment-filtered rules should
expect a burst and may want to disable them for one tick. Rules naming a
*misspelled* environment resolve to empty and keep counting zero, as they always
did — now deliberately.

## 13. API surface

New module `backend/bins/sauron-api/src/routes/notification_prefs.rs`. S2's
`routes/account.rs` already owns `/v1/me/*` for sessions and profile; the
subscription surface is large enough to justify its own module while sharing the
namespace. Every route registers explicitly in `main.rs`'s flat table.

| Method | Path | Auth | Notes |
|---|---|---|---|
| GET | `/v1/me/notification-subscriptions` | AuthUser | Mine, with catalogue env ids, best-effort scope/app names, `effective_delivery` |
| POST | `/v1/me/notification-subscriptions` | AuthUser | Upsert on `(user, scope, kind)` |
| PATCH | `/v1/me/notification-subscriptions/{id}` | AuthUser | Owner only, 404 otherwise |
| DELETE | `/v1/me/notification-subscriptions/{id}` | AuthUser | Owner only |
| GET | `/v1/me/notifications?limit=` | AuthUser | History, `covers()`-filtered (§8.5) |
| POST | `/v1/notifications/unsubscribe` | none | Rate limited, generic 200 |
| GET | `/v1/alert-meta` | AuthUser | **Extended**, not replaced |

```json
POST /v1/me/notification-subscriptions
{
  "scope_type": "project",            // or "app"; no "org"
  "scope_id": "uuid",
  "kind": "error_spike",
  "environment_ids": ["catalogue-uuid"],   // [] means all environments
  "conditions": { "window_seconds": 900, "factor": 3.0, "min_count": 10 },
  "delivery": "immediate",
  "throttle_seconds": 900,
  "quiet_start_min": 1320, "quiet_end_min": 360, "quiet_tz": "Europe/Paris"
}
```

There is no `org_id` field and there never will be one (§8.3).

`upsert_subscription` writes the parent and replaces the env child rows in a
**single data-modifying CTE** — one statement, therefore atomic, without
`conn.transaction` (MSRV 1.82).

Every handler calls `super::scope::reject_environment_id`. These are not
`/v1/apps/{id}/…` routes so the dashboard interceptor never adds the parameter,
but silently ignoring an unsupported query parameter is treated as a bug in this
codebase even on static endpoints.

None of these is added to the password-change allowlist in `extractors.rs`: a
temp-password holder must not reach them, so that file is untouched.

`/v1/alert-meta` gains a `subscription_kinds` key: per kind, `{key, scope_types,
env_filter, defaults, clamps}`. The endpoint is already reachable by any
authenticated user and already rejects `environment_id`, and the house
convention is to publish enum/option metadata there rather than hardcode lists
in Svelte. This is what drives the dialog's conditional fields and its per-kind
"the environment filter does not apply" notice.

## 14. Dashboard

### The Notifications card

S2 builds `dashboard/src/pages/Account.svelte` as a card container. S3 adds
`dashboard/src/lib/components/account/NotificationSubscriptions.svelte` to it
and touches nothing else on that page — no route, no `Sidebar.svelte` entry. The
`bell` icon is already registered in `Icon.svelte:64`, so the icon registry is
untouched too.

The card is a `DataTable` (scope, kind, environments, delivery, throttle, state)
with per-row Enable/Disable `Button`s, a `ConfirmDialog` for delete, and a "New
subscription" `Button`. House UI components only. It follows the
loading/error/empty triad and the mutations-toast / reads-set-local-error
convention. A subscription with `disabled_reason='access_revoked'` renders an
explanatory `Badge` instead of looking broken.

### `SubscriptionDialog.svelte`, and the env id trap

`Modal(size='lg')`, modelled on `CreateMemberDialog`: a kind selector (a raw
`<select class="sel">` — there is no Select primitive), a `<ScopeTree>` for the
scope, a **separate** catalogue-environment chip row, per-kind condition
`Input`s revealed by a `triggerNeeds()`-style `$derived` copied from
`Alerts.svelte`, delivery mode, throttle, and quiet hours. Reseed-from-props is
wrapped in `untrack()` so a parent reload cannot wipe a half-finished edit.

`ScopeTree.svelte` renders an environment level under every app, and those rows
are `AppEnvironment.id` — **enrollment** ids. Rendering them above a catalogue
chip row puts two id spaces in one form with identical labels. So `ScopeTree`
gains **two** additive optional props, both defaulting to `true` so
Members/EditMember behaviour is unchanged:

```ts
allowOrg?: boolean = true;
allowEnv?: boolean = true;
```

The dialog passes both `false`, and `selectionToSubscriptionScope` **rejects** a
non-empty `value.envs` rather than ignoring it, so a future regression that
re-enables the level fails loudly.

The chip row is filled from `GET /v1/projects/{id}/environments` for a project
scope. That endpoint requires `authorize_project(env:read)`, so an app-scoped
member gets 403; the fallback is `GET /v1/apps/{app_id}/environments` (which is
`reach_for`-based) mapped through each `AppEnvironment.environment_id` field.
The dialog's parent owns `appsByProject` and the on-demand loading discipline
copied from `Members.svelte`, with the same replace-never-mutate handling of the
`Record`, because there is no batched org-wide environments endpoint.

Subscriptions are one row per scope, not a collapsed grant set, so
`grant-plan.ts`'s coverage-diff machinery is deliberately **not** reused; a
multi-node selection is rejected with "pick one project or one app".

### `/unsubscribe`

`dashboard/src/pages/Unsubscribe.svelte`, registered in `routes.ts` as
`wrap({ component: Unsubscribe, conditions: [] })` — not `guarded()`, and
deliberately **not** added to `App.svelte`'s `PUBLIC_ROUTES`. That array drives
an `$effect` that pushes authenticated users *off* those paths, which is exactly
wrong here: a logged-in user clicking an unsubscribe link must still see the
confirmation. This is the same subtlety S1 hit with `/reset-password`. The page
reads the token once at init from `$querystring` (never inside an effect), POSTs
it through `bareClient`, and shows a result plus a link to `#/account`.

### Pure model and API modules

`dashboard/src/lib/models/notification-prefs.ts` (+ colocated `.test.ts`) is
DOM-free and vitest-node-testable, because there is no DOM test environment:
`selectionToSubscriptionScope`, `kindSupportsEnvFilter`, `kindScopeTypes`,
`clampConditions` (mirroring the backend clamps), `describeSubscription`,
`quietHoursLabel`, `validateSubscription` returning the exact reasons the save
button is disabled. The `.svelte` files render only.

`dashboard/src/lib/api/notification-prefs.ts` is one thin wrapper per endpoint
returning `data`, on the `api/alerts.ts` template — the `api` instance for
`/v1/me/*` (bearer + refresh interceptor), `bareClient` for the unauthenticated
unsubscribe POST.

`models/index.ts` gains `NotificationSubscription`, the `SubscriptionKind` and
`SubscriptionDelivery` unions, `SubscriptionConditions`,
`NotificationQueueItem`, and a `subscription_kinds` field on `AlertMeta`. The
`Permission` union is unchanged — S3 mints no permission.

## 15. Config and packaging

Six fields on `sauron_core::Config` under a `// --- personal notifications ---`
comment, all through the existing private `parse::<T>()` helper. **Every one is
clamped at point of use**, following `alerts_tick_secs`'s `.clamp(5, 3600)`:

| Variable | Default | Clamp | Consequence of getting it wrong |
|---|---|---|---|
| `NOTIFY_SUBS_TICK_SECS` | 120 | 30..3600 | Evaluation latency and the dominant cost lever |
| `NOTIFY_SUBS_BATCH` | 200 | 1..5000 | Unclamped, the claim's `RETURNING *` is unbounded |
| `NOTIFY_SUBS_MAX_PROBES_PER_ORG` | 50 | 1..1000 | Over the ceiling, some of that org's subscriptions are skipped for the tick |
| `NOTIFY_DRAIN_BUDGET_MS` | 10000 | 500..60000 | Caps one drain pass so a backlog cannot stall the tick |
| `NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR` | 20 | 1..1000 | Above it, delivery degrades to one digest |
| `NOTIFY_QUEUE_RETENTION_DAYS` | 14 | 1..365 | `0` would otherwise evaluate to `now() - '0 days'` and wipe the table |

No new secret and no new required variable; `Config::from_env` gains no bail.
Each is documented in all three required places — README's
`### Alerting & notifications` table, `.env.example`'s
`# --- alerting (sauron-alerts) ---` block, and
`packaging/rpm/config/alerts.env` (whose convention is to state the operational
consequence) — and added to the `alerts:` service environment map in
`docker-compose.yml` with `${VAR:-default}` interpolation.

Retention is 14 days, not 30, and the prune is **not** the `alert_events` prune:

```sql
DELETE FROM notification_queue
 WHERE finished_at IS NOT NULL
   AND finished_at < now() - ($1 || ' days')::interval
```

`alert_events` is append-only audit; this is a work queue. Pruning on
`created_at` with no status guard would destroy still-`pending` rows — precisely
the evidence of the outage that made them pile up — and none of the other
indexes leads with `created_at`, so the hourly DELETE would seq-scan a churned
heap. `notification_queue_finished_idx` serves this predicate directly.

No new binary, so `packaging/rpm/binaries.txt`, `sauron.spec`'s `%install` loop
and `%files`, `build-rpm.sh`, and the `%post`/`%preun`/`%postun` unit lists are
untouched. `sauron-alerts.service` is unchanged — it already loads
`/etc/sauron/secret.env` (needed for the unsubscribe HMAC material) and already
permits outbound AF_INET. `backend/bins/sauron-alerts/Cargo.toml` gains
`sauron-mail.workspace = true`.

`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`
are hard gates; the evaluator's fan-out fn will exceed seven arguments and needs
the same `#[allow(clippy::too_many_arguments)]` the existing workers carry.

New wiki page `Notifications.md` covering personal subscriptions, the four kinds
and their defaults, quiet hours, digests and unsubscribe, registered in **both**
`wiki/_Sidebar.md` (Guides) and `wiki/Home.md`'s Pages index. There is no
alerting or notifications wiki page today, so this is net-new.

**RPM upgrades never re-run `sauron-migrate`.** A `sauron-alerts` binary
carrying the subscription pass against a database without migration 000037 fails
every tick, and because tick failures are logged-and-swallowed by design it does
so quietly forever. S3 appends its row to `packaging/rpm/SETUP.md` §11's
migration table with exactly that symptom.

## Error handling

| Case | Status | Note |
|---|---|---|
| Unknown project/app scope | 404 | Before any grant lookup |
| Caller has no grants in the scope's org | 403 | Non-membership |
| Caller lacks `issue:read` / `monitor:read` reach over the scope | 403 | Message names the scope, not the permission set |
| `kind='uptime'` with `scope_type='app'` | 400 | `monitors` has no app dimension |
| Project scope resolving to zero apps (error kinds) | 422 | The vacuous-∀ hole |
| Environment id not a live catalogue row of the scope's project | 400 | Catches an enrollment id pasted into the field |
| `quiet_tz` not in `pg_timezone_names` | 400 | |
| Only one of `quiet_start_min` / `quiet_end_min` supplied | 400 | Mirrors the CHECK |
| Duplicate `(user, scope, kind)` | — | Upsert, not an error |
| More than 50 subscriptions | 409 | `MAX_SUBSCRIPTIONS_PER_USER` |
| PATCH/DELETE of someone else's subscription | 404 | Never 403 — do not confirm existence |
| Unsubscribe with a bad, foreign, truncated or expired token | 200 | Generic; nothing is disclosed |
| `environment_id` query parameter on any of these routes | 400 | `reject_environment_id` |

## Testing

**Constraint:** CI runs `cargo test --workspace` with no Postgres service, and
`backend/crates/sauron-db/tests/` skips with a printed notice when
`TEST_DATABASE_URL` is unset. So the decision logic is deliberately pushed into
pure functions in `sauron-alerts/src/subscription.rs`, following `guard.rs`'s
precedent, and the DB-dependent guarantees get integration tests that run only
where a database exists.

Pure Rust, always in CI:

- `spike_fires`: `B = 0` with `C >= min_count` fires (the zero-to-flood case the
  shipped predicate cannot); 1→3 does not fire at `min_count = 10`; 10→30 fires
  at `factor = 3.0`; clamps pin `factor` and `min_count` at their bounds.
- `in_quiet_hours` wrap-around table: `22:00 → 06:00` against local 23:00,
  03:00, 07:00, 21:59; plus `start == end` and the both-NULL case.
- `CondBucket` quantization: `3.0` and `3.0000001` land in the same bucket;
  `3.0` and `3.5` do not; a `NaN` or infinite factor is rejected before it can
  reach a `BTreeMap` key.
- `coalesce()`: every subscription maps to exactly one probe; no probe's app
  array contains an app outside its subscriptions' scopes; probes never span
  organizations; probe count is bounded by `orgs × kinds × buckets × env-sets`.
- `covers()` mirroring `rbac.rs`'s cascade tests: org reach accepts everything;
  a project grant accepts its apps; an app grant accepts its environments; an
  env grant accepts **only** when every listed enrollment is in `reach.envs`; a
  sibling env grant is rejected; `app_id = None` (uptime) is rejected for both
  app and env grants; `includes_unattributed = true` is rejected for an env
  grant; an empty app set is rejected rather than vacuously accepted.
- Unsubscribe token: a token signed with key A fails under key B; a token for
  subscription X does not verify against Y; a token dated 91 days ago is
  rejected; a truncated or non-hex token is rejected without panicking; the
  comparison is constant-time.

DB-backed, in `backend/crates/sauron-db/tests/notifications.rs`:

- **The regression test for the live bug.** Seed a project, an app, a catalogue
  environment, its `app_environments` enrollment, and `error_events` rows
  carrying that enrollment id; assert the old name-based subquery returns 0 and
  the new `environment_id = ANY($5)` predicate returns the true count. This is
  the test that would have caught it and the one that stops it regressing.
- `enrollment_ids_for_env_name`: a retired environment sharing a name with a
  live one contributes nothing; neither does a retired enrollment of a live
  environment. Both `retired_at IS NULL` filters are load-bearing and only a DB
  test can prove it.
- The claim: insert N pending rows, run two concurrent
  `claim_due_notifications`, assert disjoint result sets and every row claimed
  exactly once — then run a **third** claim after both commit and assert it
  returns nothing, which is the guarantee `SKIP LOCKED` alone does not give.
- `requeue_stuck_notifications`: a row claimed 20 minutes ago returns to
  `pending`; at `attempts = 3` it becomes `failed` with `finished_at` set.
- `upsert_subscription`'s single CTE: changing the env set from
  `{prod, staging}` to `{prod}` leaves exactly one child row and does not touch
  `created_at`; a failed insert leaves no orphaned child rows, proving atomicity
  without `conn.transaction`.
- The delivery-time re-check: create a subscription, enqueue a row, delete the
  user's grant, run the drain — assert `status = 'dropped_no_access'`, that
  `title`/`body`/`link` are NULL, and that no `mail_outbox` row was created.
- `deliver_after`: the SQL `CASE` and Rust's `in_quiet_hours` agree over a shared
  table of `(tz, now, start, end)` cases, including a DST transition day in
  `Europe/Paris`; a `quiet_tz` absent from `pg_timezone_names` falls back to UTC
  instead of raising.
- The prune leaves `pending` and `claimed` rows untouched at any retention value.
- The dedup index: two identical enqueues in the same statement produce one row;
  a third after the first is marked `sent` produces a new row.

Monitor-side: a transition on a project with **zero** `alert_rules` still
produces a `notification_queue` row. That is the early-return bug in §7 and it
is invisible to every other test.

Dashboard vitest over `models/notification-prefs.ts`:
`selectionToSubscriptionScope` rejects multi-node selections, org selections and
a non-empty `envs`; `clampConditions` matches the backend clamps exactly (the
test hardcodes the same numbers, and a mismatch is the drift it catches);
`kindSupportsEnvFilter` is false for `uptime`; `validateSubscription` enumerates
every disable reason.

Manual e2e — there is no mail sink in the harness, and that is the accepted gap:
create a subscription on `#/account`, trigger a real error burst against a dev
app, watch a `notification_queue` row appear with the right `app_id` and
`notification_queue_envs` rows, watch the drain create a `mail_outbox` row,
click the unsubscribe link and confirm the subscription flips to
`enabled = false` with `disabled_reason = 'unsubscribed'` and the owner receives
the confirmation.

## Files

**New**

- `backend/migrations/2026-08-01-000037_notification_subscriptions/{up,down}.sql`
- `backend/crates/sauron-alerts/src/subscription.rs` — `SubKind`,
  `SubConditions`, `CondBucket`, `spike_fires`, `in_quiet_hours`, `ProbeKey`,
  `coalesce`, `QueueTarget`, `covers`. No diesel, no axum, no network.
- `backend/crates/sauron-db/tests/notifications.rs`
- `backend/bins/sauron-api/src/routes/notification_prefs.rs`
- `dashboard/src/lib/components/account/{NotificationSubscriptions,SubscriptionDialog}.svelte`
- `dashboard/src/lib/models/notification-prefs.ts` + `.test.ts`
- `dashboard/src/lib/api/notification-prefs.ts`
- `dashboard/src/pages/Unsubscribe.svelte`
- `wiki/Notifications.md`

**Modified**

- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}` — four tables, the
  new repo fns, the two env resolvers, the `alert_count_*` signature change
- `backend/crates/sauron-alerts/src/crypto.rs` — unsubscribe token mint/verify
- `backend/crates/sauron-core/src/config.rs` — six fields
- `backend/bins/sauron-alerts/src/main.rs` — evaluate/drain/prune/requeue/sweep
  sub-jobs, and the legacy env-name resolution for `alert_rules`
- `backend/bins/sauron-alerts/Cargo.toml` — `sauron-mail`
- `backend/bins/sauron-monitor/src/main.rs` — `notify_transition` enqueue and the
  `rules.is_empty()` early-return fix
- `backend/bins/sauron-api/src/main.rs` — six routes
- `backend/bins/sauron-api/src/routes/notifications.rs` — `subscription_kinds` on
  `/v1/alert-meta`
- `backend/bins/sauron-api/src/routes/orgs.rs` — synchronous sweep from the three
  grant-mutation sites
- `dashboard/src/pages/Account.svelte` — one card
- `dashboard/src/lib/components/members/ScopeTree.svelte` — `allowOrg`,
  `allowEnv`
- `dashboard/src/lib/models/index.ts`, `dashboard/src/routes.ts`
- `README.md`, `.env.example`, `docker-compose.yml`,
  `packaging/rpm/config/alerts.env`, `packaging/rpm/SETUP.md`,
  `wiki/_Sidebar.md`, `wiki/Home.md`

## Accepted risks

- **Evaluation still double-runs on two `sauron-alerts` replicas.** The engine
  has had this property since it shipped and S3 does not fix it. The Redis
  `SET NX EX` claim plus the partial unique dedup index makes duplicate enqueue
  very unlikely, and delivery is genuinely exclusive via the `claimed` status —
  so the worst case is duplicate mail, not duplicate work. A leader lock is
  deliberately not attempted here.
- **S3 has a hard runtime dependency on whichever process owns S0's
  `mail_outbox` drain.** A deployment running `sauron-alerts` without it
  accumulates queue rows that become outbox rows that never send. `status='sent'`
  means "handed to the outbox", which is misleading in exactly that case; the
  drain's depth logging is the compensating signal. Note that a deployment
  without `sauron-alerts` at all has both symptoms (nothing drains, nothing
  prunes) from one cause.
- **The residual revocation window between the drain's check and the mail
  leaving S0's outbox** is bounded by the outbox drain interval, not by S3, and
  S3 cannot close it — a `mail_outbox` row is committed and unretractable.
- **`MAX_SUBSCRIPTIONS_PER_USER = 50` and `NOTIFY_SUBS_MAX_PROBES_PER_ORG = 50`
  are guesses.** Rotation makes a clip fair rather than arbitrary, but it is
  still a correctness degradation, and the ceiling should be re-derived from
  measured probe latency against `NOTIFY_SUBS_TICK_SECS` once there is data.
- **No `List-Unsubscribe` / RFC 8058 header** is emitted, because S0's send
  signature does not accept custom headers. At any volume, Gmail and Outlook
  penalise bulk mail without it. Follow-up, but a deliverability risk from day
  one.
- **Environment narrowing on the issue kinds is an `EXISTS` over
  `error_events`** because `issues` has no `environment_id`. It is index-served
  and bounded by the outer LIMIT, but it is a per-issue probe whose cost scales
  with how many issues appear in a window — worth measuring before large orgs
  enable environment filters on those kinds.

## Follow-ups (out of scope)

- A rolling multi-day baseline for `error_spike` (same hour last week). Needs a
  state table and a warm-up period.
- `List-Unsubscribe` one-click headers, once S0's send fn accepts headers.
- Personal delivery to Slack/Discord/webhook. The queue is channel-agnostic by
  construction.
- An in-app notification centre. `GET /v1/me/notifications` already has the data.
- `monitors.app_id` / `monitors.environment_id`, which would let uptime narrow
  below project and would also fix `alert_rules_for_monitor`'s scoping bug.
- An environment parameter for `alert_latency_metric`, which has none.
- A real audit table, which would replace §11's log-line-and-confirmation-email
  substitute.
