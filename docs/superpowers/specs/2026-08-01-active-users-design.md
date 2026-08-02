# Active users: combined across apps, with the repo's first CSV export

Date: 2026-08-01
Status: designed
Slice: S4 of the 2026-08 programme (S0 email, S1 password reset, S2 sessions,
S3 notification prefs, S4 active users, S5 PII inspector)

## Problem

`GET /v1/apps/{app_id}/users/summary` already returns `dau`/`wau`/`mau` for one
app in one environment, and `UsersExplorer.svelte` renders a stickiness ratio
built from them. Three things are wrong with that as the product's answer to
"how many people use this":

1. **It is one app.** A deployment with a web app, a mobile app and a backend
   has no way to ask "how many distinct people touched any of these". Summing
   the three numbers double-counts anyone who used two.
2. **It has no series.** The tiles are three scalars anchored to the database
   clock at the moment of the request. There is no chart, so there is no trend,
   and nothing can be exported.
3. **The numbers themselves are anchored to `now()` inside the SQL** — three
   separate `now()` calls in one statement, evaluated by Postgres, which makes
   them untestable without freezing the server clock and makes them the last
   database-clock dependency in the analytics read path.

Underneath all three is a modelling gap: Sauron has no person entity above
`event_users(app_id, distinct_id)`. "Count this user once across apps" is not
expressible today, because nothing on the server distinguishes a real user id
from an SDK-minted anonymous one.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| How does the server know an id is a person, not an anonymous token? | New `event_users.identified_at` + `identified_source`, set first-write-wins by `identify()` and by an ingested envelope whose `context.user.id` equals the recorded `distinct_id`; backfilled from `identities` and non-empty `properties` | Testing the `anon_` prefix (only the browser SDK mints it; any app may legitimately use it); a query-time re-attribution pass over the two largest tables |
| What is the cross-app identity key? | `'u:'‖distinct_id` when identified, `'a:'‖app_id‖':'‖distinct_id` otherwise | Joining on `distinct_id` alone — the count for {A,B} would then change depending on whether C is also selected. A metric that is not stable under widening the selection is unexplainable |
| What does a day's figure say? | Three numbers — `active_total`, `active_identified`, `active_guest` — everywhere one number appears | A single combined figure whose trustworthiness silently depends on whether the selected apps happen to name people the same way. Splitting it hands the reader the evidence instead of a caveat |
| One endpoint or two? | One project-scoped `GET /v1/projects/{project_id}/active-users`; the single-app view is N=1 | A second `/v1/apps/{app_id}/active-users` — the axios interceptor would attach a global `environment_id` to it, which is exactly the dimension this feature expresses per selection |
| Window contract | Explicit `from`/`to` RFC3339, **floored to UTC day boundaries in the handler** before both the query and the cache key | `since_days` — the CSV must be reproducible from the URL that produced it |
| Windows past the tiering horizon | Clamp to the live watermark, report `truncated` + `effective` + a human `truncation_reason` | 400 on any window past the horizon; routing through DuckDB from `sauron-api` (see §5.2) |
| CSV shape | Separate `GET .../active-users.csv` route sharing one `build_report()` with the JSON route | `?format=csv` — the handler's success type collapses to `Response` for both shapes and content negotiation via a query param is easy to mis-validate |
| New permission? | No. Both routes gate on `perm::EVENT_READ` | `export:write` — that rationale was "bulk PII extraction is granted explicitly"; this export is three aggregate numbers per day, already on screen under `event:read` |

## Non-goals

- Cross-tier (cold Parquet) active users. Clamped at the watermark instead.
- A materialized `(app_id, environment_id, day, identity_key)` rollup. Named as
  the follow-up that unlocks 12-month windows, with `sauron-tier` (before
  `detach_and_drop_partition`) as its home — that is the only worker that
  already knows a partition is about to disappear and already owns an
  at-most-once watermark.
- A real identity-resolution table. Cross-app merging is by exact `distinct_id`
  string equality and this slice does not change that.
- Per-user or per-org display timezone. Every day boundary is 00:00 UTC.
- A `GROUP BY environment_id` breakdown. This feature *filters* by environment
  per selection; distinct users are not additive across environments anyway.
- Fixing the anonymous-id gap in the Flutter and server SDKs. Only the browser
  SDK's in-memory churn is fixed here.

---

## 1. Identity: what "the same person" means

### 1.1 The mechanism

`event_users` gains two columns. `identified_at TIMESTAMPTZ` is the flag — every
read tests `IS NOT NULL` only, the value is informational. `identified_source
TEXT` records which of three paths set it.

Writes are first-write-wins and an unidentified event can never clear the flag.
Three producers:

| Source | Path | Test |
|---|---|---|
| `identify` | `process_identify` → `repo::upsert_event_user` | identified by construction |
| `context_user` | `process_event` / `process_error` → `repo::mark_event_user_identified` | `job.context.user.id` (or the error's own user) is non-empty **and equals the recorded `distinct_id`** |
| `backfill` | migration up.sql | non-empty `properties`, or a matching `identities` alias row |

The `context.user.id == distinct_id` equality is deliberate and it applies to
**both** the event and the error path. Server SDKs take an explicit `distinctId`
argument that may differ from any scope user; marking *that* identified would be
wrong. An earlier draft passed `identified = true` unconditionally on the error
path on the grounds that `process_error` derives its distinct from
`e.user.or(job.context.user)` anyway — but that makes the two paths disagree for
no benefit, and a single error envelope carrying `user.id = "<anything>"` would
then be enough to flag an id. The two paths run the same test.

### 1.2 The trust model, stated plainly

Both inputs to the `context_user` rule are client-supplied. Ingest authenticates
with `app_environments.public_key` (`repo::find_env_by_public_key`), a value
embedded in browser bundles and mobile binaries — public by construction. So
anyone who can read an app's public key can set `identified_at` on any
`distinct_id` string in that app.

This adds no new *class* of harm: the same actor can already forge arbitrary
events and inflate the counts directly, and restricting the flag to
`EnvelopeItem::Identify` would not help, because forging an identify envelope is
exactly as easy. What is new is **durability** — the flag is sticky, and flipping
it retroactively moves every historical figure for that id from the guest column
to the identified one, including days already exported.

Durability is what `identified_source` addresses. A poisoned `context_user`
cohort is repairable with a targeted

```sql
UPDATE event_users SET identified_at = NULL, identified_source = NULL
WHERE app_id = $1 AND identified_source = 'context_user' AND identified_at > $2;
```

without touching real `identify()` rows. Write that statement into the migration
prose as the named repair path; without the source column the only remedy is
dropping the column wholesale, which is what `down.sql` does.

### 1.3 What it does not do

Cross-app merging is **exact string equality on `distinct_id`**. If app A calls
the user `u-42` and app B calls them `auth0|abc`, `active_identified` counts two
people where there is one. That sentence belongs *next to the identified number
on the page*, not only in the page subtitle ("Users are matched across apps by
the distinct ID your SDK sends — apps must use the same identifier") and in
`wiki/Active-Users.md` — a caveat one scroll away from the figure it qualifies
gets read after the figure has already been believed. There is no server-side fix
short of an identity-resolution table.

**A PII mask on an identity-bearing key silently dismantles all of this.** The
mask enforcer runs *before* the §1.1 stamping, so once `context.user.id` — or
whatever key an app uses as its `distinct_id`, and an email address is both a
common choice and exactly the kind of value a PII policy flags — is masked, the
equality test can never pass again.

The damage is gradual rather than a step, which is what makes it dangerous.
§1.1's writes are first-write-wins, so nobody already carrying an
`identified_at` loses it — a mask cannot un-identify a single existing person.
What it stops is every *future* stamping through that key. People first seen
after the mask arrive as new guest keys and never merge across apps, so
`active_identified` plateaus and then decays at whatever rate the existing
identified population churns, while `active_guest` climbs to meet it. Nothing
moves on the day the mask lands. The split is what makes this survivable at all:
a drifting guest share is at least a number someone can watch, where a single
combined total would absorb the entire effect in silence. Nothing labels the
cause, though, and nothing downstream can
reconstruct it after the fact. It has to surface *before* the mask is applied, so
the same sentence goes in the mask confirmation dialog's "what this does not
reach" panel and in `wiki/Active-Users.md`.

`identified_at` also under-merges by design. The backfill can only see
`identify()` calls that left traits in `properties` or an alias in `identities`
(browser-only — only the browser SDK ever populates `anonymous_id`). An
`identify()` with empty traits and no anonymous id — the Node/Python/C#/Flutter
shape — leaves no trace, so those users stay app-local until their next
`identify()`. That is the fail-closed direction: under-merge rather than
over-merge, at the cost of numbers that drift upward as the backfill catches up
organically after deploy.

`event_users` has no `environment_id`, so identified-ness is env-blind. A
`distinct_id` identified in staging is identified in production. That is
desirable — a person does not become anonymous by switching environment — but it
means the flag is a single global fact per `(app, distinct_id)` and cannot be
scoped.

---

## 2. Migrations

**Numbering.** Numbers follow build order, so the programme's allocation is
S0 = 000034, S2 = 000035, S1 = 000036, S3 = 000037. S4 takes **000038, 000039
and 000040** — three, not two, because the index work is split per table (§2.3).
S5 takes the contiguous block 000041-000043. The date prefix must be monotone
with NN: `run_pending_migrations`
orders by the full directory version string, i.e. lexicographically **by date
first**, so a slice authored earlier but landing later must use its landing
date or it runs out of order and nobody notices until a FK fails.

### 2.1 `2026-08-01-000038_event_users_identified`

```sql
ALTER TABLE event_users ADD COLUMN identified_at TIMESTAMPTZ;
ALTER TABLE event_users ADD COLUMN identified_source TEXT
  CHECK (identified_source IN ('identify', 'context_user', 'backfill'));

CREATE INDEX identities_app_distinct_idx ON identities (app_id, distinct_id);

UPDATE event_users eu
   SET identified_at = eu.first_seen, identified_source = 'backfill'
 WHERE eu.identified_at IS NULL
   AND (eu.properties <> '{}'::jsonb
        OR EXISTS (SELECT 1 FROM identities i
                    WHERE i.app_id = eu.app_id AND i.distinct_id = eu.distinct_id));

CREATE INDEX event_users_app_identified_idx
  ON event_users (app_id, distinct_id) WHERE identified_at IS NOT NULL;
```

This is the first read of `identities` in the product's history — it has been
write-only dead storage since migration 1. Two legs because `identify()` merges
traits into `properties` and writes an `identities` row only when
`anonymous_id` was non-empty.

**`identities_app_distinct_idx` is not optional.** `identities` carries only
`UNIQUE (app_id, alias_id)`; `distinct_id` is unindexed, so the `EXISTS` leg has
no support and the backfill degrades to a per-row scan.

**This migration carries a maintenance-window warning in its prose header, and
the "`event_users` is small" justification is wrong.** The browser SDK re-mints
`anon_${uuidv4()}` in memory on every page load and `process_event` calls
`touch_event_user` for every non-empty `distinct_id`, so `event_users` holds one
row per *page load* per browser app — which is precisely the 5–10× inflation
§9.2 exists to fix. The header must size the window on page loads, not people.
The partial index takes a SHARE lock that blocks every `touch_event_user` for
its duration.

`down.sql` drops the two indexes and both columns. The backfill is not
recoverable from the down, but it is re-derivable from `identities` +
`properties`, which the down does not touch.

### 2.2 schema.rs and models.rs

Hand-edit the `diesel::table! { event_users (id) { … } }` block
(`backend/crates/sauron-db/src/schema.rs:133`) to **append**
`identified_at -> Nullable<Timestamptz>,` and `identified_source -> Nullable<Text>,`
as the last two fields, after `updated_at`. Append-at-end is load-bearing:
`models::EventUser` derives `Queryable`, which decodes positionally, and
`ALTER TABLE … ADD COLUMN` appends physically. Mirror the same two fields in the
same order at the end of `EventUser` (`models.rs:485`). No `joinable!` edit (no
new FK), no `allow_tables_to_appear_in_same_query!` edit (no new table). The
diesel CLI must never run.

`EventUser` derives `Serialize`, but no route returns it (the person endpoints
return `repo::PersonRow`), so no wire shape changes.

### 2.3 `2026-08-01-000039_analytics_active_user_index` and `-000040_error_active_user_index`

One migration per table, each doing a substitution:

```sql
-- 000039
DROP INDEX IF EXISTS analytics_events_app_env_time_idx;
CREATE INDEX analytics_events_app_env_time_users_idx
  ON analytics_events (app_id, environment_id, occurred_at DESC) INCLUDE (distinct_id);

-- 000040
DROP INDEX IF EXISTS error_events_app_env_time_idx;
CREATE INDEX error_events_app_env_time_users_idx
  ON error_events (app_id, environment_id, occurred_at DESC) INCLUDE (distinct_id);
```

The dominant scan is `WHERE app_id AND environment_id AND occurred_at BETWEEN`,
projecting only `distinct_id` and `occurred_at`. The existing indexes give a
perfect index cond but carry no payload, so every matching row costs a heap
fetch of a ~1–2 KB tuple. This **adds zero indexes** — it widens two existing
btree leaves by one short text column, the same class of change migration 28
measured at 1–6% on `error_events`, and `INCLUDE` on a partitioned parent is
already proven here (migrations 28 and 31). New names, per the rule that an
index name an earlier migration took is never reused; replace-don't-accumulate,
per the rule migrations 28 and 31 each invoked.

Three constraints on how these land:

- **Split per table, and never in the same release as anything else
  time-sensitive.** Each is a `DROP` + `CREATE` on a partitioned parent inside
  one migration transaction (`CONCURRENTLY` is unavailable), so with
  `TIER_GRANULARITY=day` and `TIER_PARTITION_AHEAD=7` that is ~37 synchronous
  child builds per table under locks that block every INSERT. Migrations 28 and
  31 each did this to `error_events` alone; doing both parents in one
  transaction blocks *both* ingest write paths at once.
- **The prose header states an operational precondition, not just "expect read
  latency": stop `sauron-ingest` or drain the stream before running.** While the
  pipeline is blocked on the index lock the Redis stream keeps growing, and
  `xadd_job(&payload, 1_000_000)` issues `XADD … MAXLEN ~ 1000000`, which trims
  by ID regardless of the consumer group's pending list. The oldest,
  still-undelivered entries are trimmed away. That is permanent silent event
  loss, not backpressure.
- **Measure before, not after.** The entire justification is an index-only scan,
  and migration 28's own prose flags the dependency: heap fetches depend on the
  visibility map, and on append-only partitioned tables the newest partition —
  the one an active-users query touches most — is the least likely to be
  all-visible. Run
  `EXPLAIN (ANALYZE, BUFFERS)` on the exact `active_users_combined` statement
  against the *existing* indexes on a real dataset first, and ship 000039/000040
  only if heap fetches actually dominate. Note that
  `analytics_distinct_idx (app_id, distinct_id, occurred_at DESC)` already
  covers the `EnvFilter::All` shape index-only today, which weakens the case for
  the analytics half specifically. The repo's standard (migrations 25, 28, 31)
  is measurement-then-change; this is the first index migration proposed on
  analogy alone, and it should not stay that way.

`down.sql` for each drops the `*_users_idx` and recreates the original
definition from migrations 25/27 verbatim, and carries the same warning — a
rollback is also a synchronous rebuild.

### 2.4 The upgrade gap, and why the write path is a separate statement

RPM upgrades do not re-run `sauron-migrate`. That is a standing platform gap and
this slice must not be the one that turns it into data loss.

The naive design — add `identified_at` to `touch_event_user`'s and
`upsert_event_user`'s column lists — fails badly against an un-migrated schema.
`process_event`'s call site is `let _ = repo::touch_event_user(...)`, so every
statement fails with `undefined_column` and the failure is *discarded*:
`event_users.first_seen`/`last_seen` silently stop advancing deployment-wide,
with no dead letter, no metric and no log. Worse, `process_identify`'s upsert is
`.await?`, and `worker.rs:163` moves a failed job to the dead-letter list with
no retry — so every `identify()` in the window is discarded, and `identify()` is
exactly what populates the `properties` and `identities` rows the backfill later
depends on.

So:

- **`touch_event_user` and `upsert_event_user` keep their existing statements
  verbatim.** No column-list change, no signature change, nothing that a missing
  column can break.
- Identification is written by a new
  `repo::mark_event_user_identified(conn, app_id, distinct_id, source) -> QueryResult<usize>`
  running
  `UPDATE event_users SET identified_at = now(), identified_source = $3 WHERE app_id = $1 AND distinct_id = $2 AND identified_at IS NULL`.
  First-write-wins falls out of the `IS NULL` predicate rather than a `COALESCE`,
  and after the first hit it is a primary-key no-op.
- It is called only when the §1.1 test passes, and only when a process-local
  `OnceLock<bool>` probe (`SELECT identified_at FROM event_users LIMIT 0`, run
  once at worker boot) says the column exists. A false probe logs one ERROR
  naming `sauron-migrate` and skips the write for the process lifetime.
- Both `touch_event_user` call sites change from `let _ = …` to
  `if let Err(e) = … { tracing::warn!(…) }`. Swallowing the error was how this
  became invisible in the first place.

`sauron-api` gets the same probe at boot, but does not `?` out of `main()` — it
sets a flag that makes `/v1/projects/{id}/active-users` return
`503 schema_migration_required` naming `sauron-migrate`, instead of a raw 500
from a missing column. Refusing to start would be the wrong posture for the
ingest worker (it drops all telemetry) and an unnecessary outage for the API.

Both migration prose headers, and `packaging/rpm/SETUP.md` §11 (created by S0,
appended to by each slice), name 000038/000039/000040 as MUST-RUN-BEFORE-RESTART
with the specific symptom: without them, active users returns 503 and the
`identified_at` signal is not collected — which the backfill **cannot** recover
later, because it only sees `properties` and `identities`. Every person active in
the un-migrated window is filed under `active_guest` forever, and the split is
permanently wrong for those days.

---

## 3. The query

### 3.1 Types

```rust
/// One resolved (app, environment) pair. Deliberately NOT `ReadScope`:
/// `ReadScope` is singular by contract and ~36 read fns take it, so a plural
/// variant of it would let a caller hand a multi-app scope to a single-app query.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppEnvScope { pub app_id: Uuid, pub env: EnvFilter }

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ActiveUserDay {
    #[diesel(sql_type = diesel::sql_types::Date)] pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)] pub active_total: i64,
    #[diesel(sql_type = BigInt)] pub active_identified: i64,
    #[diesel(sql_type = BigInt)] pub active_guest: i64,
}

pub async fn active_users_combined(
    conn: &mut AsyncPgConnection,
    scopes: &[AppEnvScope],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<ActiveUserDay>>
```

`Date`/`NaiveDate` mirrors `repo::DayCountRow` exactly, including the
fully-qualified `diesel::sql_types::Date` (`Date` is not in `repo.rs`'s import
list).

### 3.2 Bind layout

`$1` `from`, `$2` `to`, then per scope in order: `app_id`, and — **only if
`scope.env.consumes_bind()`** — the env bind.

```rust
let mut next = 3;
for s in scopes {
    let app_bind = next; next += 1;
    let env_bind = next;
    let env_a = s.env.sql_fragment_for("analytics_events", env_bind);
    let env_e = s.env.sql_fragment_for("error_events", env_bind);
    if s.env.consumes_bind() { next += 1; }
    …
}
```

Then `.bind::<Timestamptz,_>(from).bind::<Timestamptz,_>(to)` followed by, per
scope,
`stmt = stmt.bind::<SqlUuid,_>(s.app_id); stmt = crate::bind_env!(stmt, &s.env);`
— `bind_env!`'s `All`/`Unattributed` arm is a no-op, so it pairs exactly with
`consumes_bind()`. Never assume a fixed offset. Deriving the index from anything
other than `consumes_bind()` is the documented easiest way to get `EnvFilter`
wrong, and here it silently mis-pairs an environment with the wrong app.

### 3.3 The SQL

```sql
WITH signal AS (
  -- 2N legs: one per (selection × {analytics_events, error_events})
  SELECT app_id, occurred_at, distinct_id FROM analytics_events
   WHERE app_id = $k AND occurred_at >= $1 AND occurred_at < $2 {env_a}
     AND distinct_id IS NOT NULL AND distinct_id <> ''
  UNION ALL
  SELECT app_id, occurred_at, distinct_id FROM error_events
   WHERE app_id = $k AND occurred_at >= $1 AND occurred_at < $2 {env_e}
     AND distinct_id IS NOT NULL AND distinct_id <> ''
),
days AS (
  SELECT DISTINCT app_id, distinct_id, (occurred_at AT TIME ZONE 'UTC')::date AS day
    FROM signal
),
keyed AS (
  SELECT DISTINCT
         CASE WHEN eu.distinct_id IS NOT NULL
              THEN 'u:' || d.distinct_id
              ELSE 'a:' || d.app_id::text || ':' || d.distinct_id END AS identity_key,
         (eu.distinct_id IS NOT NULL) AS identified,
         d.day
    FROM days d
    LEFT JOIN event_users eu
      ON eu.app_id = d.app_id AND eu.distinct_id = d.distinct_id
     AND eu.identified_at IS NOT NULL
),
per_day AS (
  SELECT day,
         count(*)::bigint                                  AS active_total,
         count(*) FILTER (WHERE identified)::bigint        AS active_identified,
         count(*) FILTER (WHERE NOT identified)::bigint    AS active_guest
    FROM keyed GROUP BY day
),
grid AS (SELECT generate_series(($1 AT TIME ZONE 'UTC')::date,
                                (($2 - interval '1 microsecond') AT TIME ZONE 'UTC')::date,
                                interval '1 day')::date AS day)
SELECT g.day AS day,
       COALESCE(p.active_total, 0)::bigint       AS active_total,
       COALESCE(p.active_identified, 0)::bigint  AS active_identified,
       COALESCE(p.active_guest, 0)::bigint       AS active_guest
  FROM grid g
  LEFT JOIN per_day p ON p.day = g.day
 ORDER BY g.day;
```

### 3.4 Why `days` exists, and why the split cannot fail to add up

**`days` is the whole cost story.** An earlier draft joined `event_users`
directly against `signal`. Because the projected key depends on `eu`, Postgres
cannot push the `DISTINCT` below the join — the outer side is every matching raw
event row across up to 20 selections and up to 92 days, with no LIMIT, and the
text key `'u:'||distinct_id` is materialized once per event row before the dedup
sort. Interposing `days` collapses the join input by the average
events-per-user-per-day factor (typically 10–1000×) with a HashAggregate over
three narrow columns, and makes the `event_users` join cost proportional to the
answer rather than to the input. The inner side is the table dominated by
anonymous-id churn and it has no reaper, so this matters. The tier clamp does
not save the naive shape on deployments that never enabled `sauron-tier` — no
watermark means no clamp means the full retained history is in scope, which is
exactly the deployment with the most rows.

**`identified` is a property of the key, not of the row, and that is what makes
`active_total = active_identified + active_guest` an identity rather than an
approximation.** A `'u:'` key exists only because some selected app has
`identified_at IS NOT NULL` for that `distinct_id`; an `'a:'` key exists only
where no selected app does. The prefix therefore determines the flag, so
carrying `identified` inside the `DISTINCT` cannot split one key across both
buckets and cannot change the cardinality `active_total` counts. Two `count(*)
FILTER` clauses over one already-deduplicated set is the only shape with that
property — computing the two halves as separate subqueries and adding them would
reintroduce the possibility of a total that does not match its parts, which is
precisely the failure a split report exists to avoid.

That the split is exact does *not* make the identified half true: a person named
`u-42` in one app and `auth0|abc` in another is two `'u:'` keys and counts twice.
Exact arithmetic over a lossy join is still lossy, and §1.3's caveat is what the
page must say alongside the number.

Put both cost and disjointness paragraphs in the function's doc comment.

### 3.5 Timezone

Days are UTC calendar days via `(occurred_at AT TIME ZONE 'UTC')::date`.
`date_trunc('day', occurred_at)` on a `timestamptz` — what `active_user_series`
uses today — truncates in the session `TimeZone` GUC, which nothing in
diesel-async sets, so those bucket boundaries depend on the Postgres server's
configuration. The `AT TIME ZONE 'UTC'` idiom is already house style in
`repo::{error,event,transaction}_counts_by_day_hot`, is GUC-independent, and
matches `DuckEngine::open`'s pinned `SET TimeZone='UTC'` so a future cross-tier
version cannot disagree with the hot half.

---

## 4. The endpoints

New module `backend/bins/sauron-api/src/routes/active_users.rs`, registered with
`pub mod active_users;` in `routes/mod.rs`. A new file rather than growing
`analytics.rs`: this is the first project-scoped telemetry read in the product
and its authorization shape has nothing in common with `analytics.rs`'s
`authorized_read_scope` handlers.

```rust
pub async fn active_users(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ActiveUsersQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ActiveUsersReport>, ApiError>

pub async fn active_users_csv(/* identical extractors */)
    -> Result<axum::response::Response, ApiError>
```

Both call one `async fn build_report(…) -> Result<ActiveUsersReport, ApiError>`,
so they can never disagree about the numbers — the only thing `?format=csv`
really bought.

`ActiveUsersQuery { from: DateTime<Utc>, to: DateTime<Utc>, #[serde(default)] selection: Vec<String> }`
is deserialized with `axum_extra::extract::Query` (serde_html_form), the codec
already used for repeated-key `Vec<String>` fields (`issues.rs:23`,
`analytics.rs:195`). `environment_id` is **not** a field, and the handler calls
`super::scope::reject_environment_id(super::scope::raw_environment_id(raw_query.as_deref()).as_deref())?`
first. The environment dimension is expressed per selection, so accepting a
global one and ignoring it is exactly the bug `routes::scope`'s module docs
exist to prevent.

Route registration is two lines in `main.rs`'s table beside the other
`/v1/projects/{project_id}/…` routes, under the JSON router's existing
`DefaultBodyLimit`, CORS, `ConcurrencyLimitLayer(512)` and 30 s `TimeoutLayer`.

### 4.1 Selection encoding

Repeated `?selection=<app_uuid>[:<env_token>]` keys, where `<env_token>` is an
`app_environments.id` UUID, the literal `all`, or the literal `none`. A bare
`<app_uuid>` means `all`. UUIDs contain hyphens but never colons, so `:` is
unambiguous, and the whole thing round-trips through `URLSearchParams.getAll()`
with no custom codec.

Parsed by a pure, separately tested
`fn parse_selection(raw: &[String]) -> Result<Vec<(Uuid, EnvFilter)>, ApiError>`:
`all` → `EnvFilter::All`, `none` → `EnvFilter::Unattributed`, a uuid →
`EnvFilter::One(id)`. `Subset` is never requestable, the same rule `parse_env`
already enforces. It rejects, each with a 400 naming the offending token: an
empty list, more than `MAX_SELECTED_APPS`, a duplicate app id, a malformed uuid,
an unknown env token.

Parallel `app_ids=` / `env_ids=` arrays were rejected because a length mismatch
or a reordering silently pairs the wrong environment with the wrong app with no
error; a JSON blob in a query param because it is unreadable in logs and defeats
the URL round-trip; POST with a body because the CSV sibling must be a GET
(browsers download GETs) and the view must be bookmarkable.

### 4.2 Authorization — the three-step reach pattern, verbatim

```rust
let org_id = repo::project_org(&mut conn, project_id).await?.ok_or(ApiError::NotFound)?;
let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
if rows.is_empty() { return Err(ApiError::Auth(AuthError::Forbidden)); }
let grants = grants_from_rows(rows);
let reach = reach_for(&grants, perm::EVENT_READ);
```

Then app-in-project validation: `repo::app_ancestries(&mut conn, &requested_app_ids)`
filtered to `ancestor_project == project_id`. An app id that does not resolve
into *this* project is a 400 `"app {id} is not in project {project_id}"`,
mirroring how `validate_scopes_in_org` treats a scope id that does not belong —
the caller's app ids carry no FK to the path's project.

`repo::orgs_with_permission` is **unusable here**: it hardcodes
`g.scope_type = 'org'` and would 403 every project-, app- and env-scoped member.

### 4.3 Per-selection environment resolution

For each selection, replicate `authorize_env_read_inner`'s decision without its
single-app I/O:

- Fast path when the request is `All` **and**
  `has_permission(&grants, perm::EVENT_READ, org_id, Some(project_id), Some(app_id), None)`.
- Otherwise call the pure, already-shipped
  `sauron_auth::resolve_env_filter(&grants, perm::EVENT_READ, org_id, project_id, app_id, &app_env_ids, requested)`.

`app_env_ids` comes from one batched
`repo::env_ids_for_apps(conn, &app_ids) -> QueryResult<Vec<(Uuid, Uuid)>>`
returning `(app_id, app_environments.id)` — the batched `env_ids_for_app`, same
semantics including RETIRED enrollments, because retired history stays readable
and `resolve_env_filter` needs the full set for its `EnvNotInApp` check.

**Collect that result into a `HashMap<Uuid, Vec<Uuid>>` keyed by `app_id` and
pass `map.get(&app_id).map(Vec::as_slice).unwrap_or(&[])` — never the flat
vector.** `resolve_env_filter` uses `app_env_ids` for two decisions: the
`EnvNotInApp` membership test, and `readable = app_env_ids ∩ reach.envs`. Hand
it the union across every selected app and both break in the same direction —
*towards granting*. Concretely: a caller holding an env grant only on app B's
`staging` enrollment requests `?selection=<appA>`. With the union, `readable`
for app A is non-empty (it contains app B's staging id), so instead of
`EnvDenied::NoReach` → 403, app A resolves to `Subset([<appB-staging-id>])` and
contributes `WHERE app_id = A AND environment_id = ANY('{appB-staging}')` —
zero rows, silently, inside a combined number the caller should have been
refused outright. The same union turns `selection=<appA>:<appB-env-uuid>` from a
403 into an accepted zero-row leg. `role_grants.scope_id` for
`scope_type='env'` holds an `app_environments.id`, which is per-app; a flat set
of them is meaningless.

Reusing the shipped pure decision function rather than re-deriving the cascade
also preserves `UnattributedNeedsAppReach` (so `selection=<app>:none` still
requires app-wide reach) and the ordering of `EnvNotInApp` before
`EnvNotGranted` (so probing for env ids learns nothing).

### 4.4 Partial reach is a 403, never partial data

If any requested app resolves to a denial, the whole request fails:

```rust
Err(ApiError::Forbidden(format!("no read access to app(s): {}", denied.join(", "))))
```

There is no honest way to render "combined active users across A,B,C,D,E" from
A,B,C. A number computed over a silent subset is a wrong number presented as a
right one,
and the CSV carries it out of the UI where no notice travels with it. The denied
ids are echoed because the caller supplied them, so nothing new is disclosed,
and the page needs them to drop a stale selection and retry. The picker is
populated from `GET /v1/projects/{id}/apps` (already reach-filtered), so this is
normally only reachable from a hand-crafted request or a stale bookmark.

### 4.5 The response, and telling the truth about `Subset`

```rust
pub struct ActiveUsersReport {
    pub requested: Window,
    pub effective: Window,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
    pub selections: Vec<SelectionView>,
    pub series: Vec<ActiveUserPoint>,
    pub latest: Option<ActiveUserPoint>,
}
pub struct Window { pub from: DateTime<Utc>, pub to: DateTime<Utc> }
pub struct ActiveUserPoint {
    pub day: NaiveDate,
    pub active_total: i64,
    pub active_identified: i64,
    pub active_guest: i64,
}

pub struct SelectionView {
    pub app_id: Uuid,
    pub app_name: String,
    /// The filter that was actually applied: "all" | "one" | "subset" | "unattributed".
    pub resolved: &'static str,
    /// Populated for "one" and "subset". Empty otherwise.
    pub environment_ids: Vec<Uuid>,
    pub environment_labels: Vec<String>,
}
```

`SelectionView` carries the **resolved** filter, not the requested one, and it
is a tagged shape rather than `environment_id: Option<Uuid>`. That is not
cosmetic. `rbac.rs:574` is explicit:
`match requested { EnvFilter::All | EnvFilter::Subset(_) => Ok(EnvFilter::Subset(readable)), … }`.
So a member holding env grants on 2 of an app's 5 environments who sends the
default bare `?selection=<app_uuid>` gets a number computed over 2 environments.
With `Option<Uuid>` that renders as `None` — indistinguishable from a true
`All` — and the picker they used still reads "All environments". One headline
number under a label that says it covers everything. The page renders "2 of 5
environments" whenever the server came back `subset`.

This matters more here than elsewhere because `EnvFilter::All` includes
`environment_id IS NULL` rows while `Subset` uses `= ANY(...)`, which never
matches NULL (pinned by `subset_fragment_uses_any_not_in`). An app-wide caller
and a partial-reach caller can legitimately get different totals for what looks
like the same selection, and the numbers are not comparable across callers. The
label is the only thing that makes that legible.

`ActiveUsersReport` derives `Deserialize` as well as `Serialize`, and every
field added after v1 carries `#[serde(default)]`, following
`AppStorage.project_name`'s comment — a report cached by an older build must
still deserialize rather than missing the cache for a whole TTL.
`truncation_reason` is a full human sentence naming the effective floor date,
because the UI renders it verbatim.

### 4.6 Post-query pure functions

Colocated `#[cfg(test)] mod`, per the house rule that merging/bucketing/ratio
logic lives in a plain Rust fn next to the query with its own tests:

| Function | Purpose |
|---|---|
| `latest_full_day(series, today_utc: NaiveDate) -> Option<&ActiveUserPoint>` | Last point with `day < today_utc`. Today is still accumulating, and a headline tile that falls as the day starts and climbs until midnight reads as a product problem |

`effective.from = max(requested.from, from_after_clamp)`, and
`series[0].day == effective.from` **by construction**. State that explicitly,
because the clamp can land *inside* the display window (`from = today-60d`,
watermark at `today-45d`): the grid then starts at the watermark and the days in
`[from, watermark)` are absent from `series` entirely — not zero, not flagged,
just missing. `truncation_reason` names that date, and the CSV filename is built
from the **effective** dates, not the requested ones, so a downloaded file's name
matches its contents.

If `latest_full_day` returns `None` — a window containing only today — the tiles
render an em-dash, not `0`. Zero active users is a real and reportable answer;
rendering "we have no complete day yet" as that answer is exactly the
plausible-but-wrong number this slice exists to stop producing.

---

## 5. Window semantics, and the tier boundary

### 5.1 Constants and validation

```rust
const MAX_ACTIVE_USER_DAYS: i64 = 92;
const MAX_SELECTED_APPS: usize = 20;
const MAX_SCAN_BUDGET: i64 = 1200; // selections × displayed days
```

The handler **floors `from` and `to` to UTC day boundaries** before anything
else. The output is day-bucketed, so this loses nothing — and it fixes a real
correctness bug the raw contract has, where a mid-day `from` renders a partial
first day as a full day's count. It is also what makes the cache key mean
something (§6).

Then: `to > from` (400); `to - from <= MAX_ACTIVE_USER_DAYS` (400, same wording
shape as `TimeseriesQuery::range`'s message); and
`selections.len() as i64 * days <= MAX_SCAN_BUDGET` (400, before any DB work).
The last one bounds the **product**, which is the thing actually being handed
out — 20 apps × 92 days is 1840 partition-day scans, and bounding the two
dimensions independently does not bound that.

### 5.2 The clamp

```rust
let floor = ["analytics_events", "error_events"]
    .iter()
    .filter_map(|t| repo::get_watermark(&mut conn, t).ok().flatten())
    .max();
```

`None` for a table means nothing has ever been tiered for it, so it imposes no
floor; the union is only complete from the **maximum** of the present
watermarks. If `floor` is `Some(f)` and `from < f`, set `from = f` and
`truncated = true`.

This is deliberately conservative. Between `sauron-tier`'s export (which
advances the watermark) and the DETACH+DROP `TIER_DROP_LAG_HOURS` later, rows
past the watermark are still physically in Postgres, so callers will sometimes
see `truncated: true` for a day that would still have returned rows. Reporting
numbers that vanish 24 h later is worse.

Using the live watermark rather than `now - tier_hot_days` means a deployment
that never runs `sauron-tier` gets its full history.

**On shipped defaults the clamp fires constantly, and the page has to say so
rather than shrink quietly.** `TIER_HOT_DAYS` defaults to 30, `TIER_GRANULARITY`
to `"day"`, `sauron-tier` exports everything older than `now - tier_hot_days` and
advances the watermark to each exported range's end — and the worker is on by
default in both shipped topologies (`docker-compose.yml` runs the `tier` service
with no `profiles:` key, and the RPM ships `sauron-tier` plus its unit). So the
watermark sits at roughly `now - 30d` and a 92-day request returns about 30 days
for essentially every operator. That is the steady state, not an edge case, and
it is why `truncated` / `effective` / `truncation_reason` are load-bearing rather
than defensive: a chart that silently starts a month ago when the picker says
three months is the kind of number nobody questions. An operator who wants more
raises `TIER_HOT_DAYS` themselves and pays for the extra hot Postgres; this slice
does not spend their disk on their behalf.

**Verify the clamp against a deployment that has actually run `sauron-tier`
before calling the slice done.** Every Postgres test seeds a fresh DB where no
watermark exists and the clamp never fires, so none of them would catch a break
here.

DuckDB was rejected for the cold half, not deferred casually: `DuckEngine` would
need `INSTALL postgres`, which fetches over the network at first use into
`$HOME/.duckdb`, while `sauron-api.service` sets `ProtectHome=true` and
`sauron.spec` pre-stages only `libduckdb.so`. It would work in docker and fail
on the packaged product — the worst possible failure distribution. `DuckEngine::open()`
also pins `memory_limit='512MB'` with no per-request accounting for a year-wide
holistic distinct, the blocking thread outlives a 30 s `TimeoutLayer`
cancellation, and cold Parquet is hive-partitioned on `(app_id, year, month)`
with `environment_id` as an ordinary VARCHAR — which is the stated reason all
three existing cross-tier endpoints reject `environment_id` outright, and
per-app-environment selection is this feature's entire point.

---

## 6. Cost controls

The design's original mitigation for the query's cost was "a 60 s Redis cache".
That mitigation did not exist as specified, and it is worth being precise about
why, because three separate things had to be fixed.

### 6.1 The cache key

`const ACTIVE_USERS_CACHE_TTL_SECS: u64 = 60;`, key
`format!("sauron:activeusers:{}", sauron_auth::hash_token(&fingerprint))`.

The fingerprint must be **injective by construction**. `admin_storage`'s
`hash_token(sorted_org_uuids.join(","))` is injective only because every element
is a fixed-length UUID with no nesting; this fingerprint is a list of
`(app_id, EnvFilter)` pairs where `Subset(Vec<Uuid>)` is variable-length — a
nested structure with two levels of repetition. A naive join lets two distinct
resolved selections flatten to the same bytes, and the cached entry holds the
whole series plus `selections[].app_name`, so a collision is a cross-caller and
across projects cross-tenant **data leak**, not a staleness bug.

So: derive `Serialize` on `AppEnvScope` and `EnvFilter`, build a canonical
struct `{ project_id, from, to, scopes }` with `scopes` sorted by `app_id` and
each `Subset`'s uuids sorted, and hash
`serde_json::to_string(&canon)?`. JSON is self-delimiting, so no flattening
ambiguity exists.

The key uses the **resolved** filter, never the requested token. That is what
keeps a caller with app-wide reach (`All`) and a caller with only env-X reach
(`Subset([X])`) from ever sharing an entry — the same rule that makes the
storage cache tenant-safe. Treat any deviation from it in review as a Critical.

`from`/`to` are the **day-floored** values from §5.1. Full-precision RFC3339 in
the key against day-granular output means `from + 1µs` mints a brand-new key for
a byte-identical series: unlimited free cache misses, and Redis filling with
60 s-TTL report blobs. Flooring is what makes the JSON call and the CSV call
moments later produce the same key by construction — which is the whole point of
sharing `build_report`.

The page must also have defined refresh semantics: `to` is the **server's**
`Utc::now()` floored to the day, not a client clock, and `RefreshButton` re-runs
the request rather than advancing a pinned window. A `to` pinned in the URL at
page load would make the TTL and the Refresh button both no-ops.

### 6.2 Redis timeouts

Do **not** copy `collect_storage_cached` verbatim. Its
`if let Ok(Some(hit)) = state.redis.get(key).await` / `let _ = set_ex(…)` has no
timeout, and `sauron-redis` builds the connection with
`set_response_timeout(None)`. The repo has already measured what that costs: the
comment at `routes/auth.rs:125` records 9–19 s per command against a dead Redis,
"long enough that the in-flight cap fills and the whole API stalls", which is
why `rate_limit` wraps its call in a 250 ms `tokio::time::timeout`.

"A Redis error is logged and the report computed, never surfaced" is only true
for an *error*. An outage is a hang, twice per request. Wrap both the `get` and
the `set_ex` in `tokio::time::timeout` (500 ms is defensible for a larger
payload than a limiter token), treating elapsed as a miss and as a failed write.
`admin_storage` gets away without this because it is a rarely-loaded admin page;
this is a nav-item page with a Refresh button.

### 6.3 Rate limit and concurrency gate

This is the heaviest query in the product and the lowest-privileged role
(`Viewer` holds `event:read`) can run it. The repo's only rate limiting today is
`rate_limit` in `routes/auth.rs`, applied to login/register/refresh; no read
route has any per-user limit.

- `rate_limit` and `client_addr` are already `pub(crate)` in `routes/auth.rs` by
  the time this slice lands — S2 widens both in place and puts the key convention
  `sauron:{area}:{action}:{principal}` in their doc comments. Call them where
  they are; do not move them. Three slices editing one file to relocate the same
  two functions is a rebase conflict with nothing to show for it. Both
  active-users routes gate on `sauron:analytics:active_users:{user_id}` at
  30/min. This is the repo's first read-route rate limit, and that is the point:
  it is the template.
- Add a `tokio::sync::Semaphore` to `AppState` sized at 2–4 permits, acquired
  with `try_acquire` around the `build_report` call in both handlers, returning
  `503` when unavailable rather than queueing. The pool is `max_size = 16` for
  the whole process (`main.rs:68`) and `POOL_WAIT_TIMEOUT` is 5 s, and
  `pool::conn` maps a checkout failure to a 500. Sixteen people hitting Refresh
  — or one person with the shareable URL open in a few tabs — starves every
  other endpoint in the deployment, including `/v1/auth/login` and `/health`,
  with `db pool checkout failed` 500s. `ConcurrencyLimitLayer(512)` and the 30 s
  `TimeoutLayer` shed the HTTP request but cancel neither the Postgres query nor
  the pool slot.

A per-statement timeout would be the natural control and is deliberately not
attempted: `SET LOCAL` needs a transaction (banned by MSRV 1.82) and a bare
`SET` on a pooled connection leaks to the next borrower. There is no
`statement_timeout` anywhere in the workspace; that stays a flagged platform
gap, and the semaphore is the shape available today.

Do not describe the cache as a DoS control anywhere in the code comments. It is
a latency optimization; the limiter, the budget and the semaphore are the
control.

---

## 7. The CSV export pattern

This is the first CSV in the repo, and S5's PII report is the next one. What
gets built here is what gets copied, so the pattern matters more than this
particular file.

### 7.1 `backend/bins/sauron-api/src/csv.rs`

~40 lines plus tests. `pub fn escape_field(s: &str) -> String` and
`pub fn write_row(out: &mut String, fields: &[&str])`.

1. **RFC 4180 quoting** — quote iff the field contains `,`, `"`, `\r`, `\n`, or
   a leading/trailing space; double every embedded `"`.
2. **Formula-injection guard** — if the first byte is one of `= + - @ \t \r`,
   prefix a single `'` *before* quoting, so a spreadsheet treats it as text.
3. Line ending `\r\n`.
4. **No UTF-8 BOM.** v1 emits pure ASCII so the question is moot, and a BOM
   breaks naive line-oriented tooling in a way that is harder to diagnose than
   an Excel encoding prompt. Name the trigger to revisit in the module doc: the
   first export that carries non-ASCII text.

The module exists even though v1's four columns need none of rule 2, because a
hand-rolled join-with-commas here is what would get copied into the export that
*does* carry app, environment and person names.

The `csv` crate is rejected, recorded in the module doc: `backend/Cargo.toml`
has no `csv` dependency, adding one puts a crate in every RPM build, and — the
decisive point — the `csv` crate does not do formula-injection escaping, so the
one non-trivial rule is hand-rolled either way. The repo's precedent is to
hand-roll small fully-testable primitives (`render::substitute` instead of a
template engine, hand-rolled `hmac_sha256_hex`, hand-rolled config parsing).

### 7.2 The response

`content-type: text/csv; charset=utf-8`, and
`content-disposition: attachment; filename="sauron-active-users-{project_id}-{YYYYMMDD}_{YYYYMMDD}.csv"`
built from the **effective** window (§4.6). Buffered `String` → `Body::from`:
the body is at most 93 lines of ASCII. Streaming is not an option anyway —
`backend/Cargo.toml` has no `futures`, no `tokio-util`, and tokio's feature list
has no `fs`.

Header `day,active_total,active_identified,active_guest`, then one row per
displayed point. Every field is machine-generated (an ISO date and three
integers), so v1 carries zero user-controlled bytes. Both halves ride along
rather than only the total, because a spreadsheet is exactly where someone
re-derives a figure months later with no page around it to carry §1.3's caveat —
a guest column they can see is the only warning that survives the download. The
selection context deliberately does not go in the body — it is a per-file
constant, not a per-row value. Row count is structurally capped by
`MAX_ACTIVE_USER_DAYS`.

### 7.3 CORS

`main.rs:135` builds `CorsLayer::new().allow_origin(origins).allow_methods([…])
.allow_headers([AUTHORIZATION, CONTENT_TYPE])` and exposes nothing. In both
shipped topologies the dashboard origin is not the API origin (nginx serves the
SPA on :80 with `API_BASE_URL` elsewhere; dev is :3000 vs :8090), so
`res.headers['content-disposition']` is `undefined` in the browser today. Add
`.expose_headers([axum::http::header::CONTENT_DISPOSITION])`.

Without it the download silently falls back to a generic filename — a bug that
reproduces in dev *and* in production. This one line collides with S0's and S2's
edits to the same `main.rs` router tail; whichever of the three lands last
applies it, with a note in the PR body so it is not lost in a conflict
resolution.

### 7.4 `dashboard/src/lib/api/download.ts`, and the Blob trap

```ts
export async function downloadCsv(
  url: string, params: Record<string, unknown>, fallbackFilename: string,
): Promise<void>
```

It calls `api.get(url, { params, responseType: 'blob' })` on the **shared** `api`
instance, so it keeps the bearer header and the 401 refresh-and-replay (the
replay path does `api(original)` with the original config, so `responseType`
survives). It parses `Content-Disposition` for the filename, falling back to
`fallbackFilename` — which the caller builds from the same ids and effective
dates the server uses, so the file is correctly named even if CORS ever stops
exposing the header — creates an object URL, clicks a synthetic anchor, and
`URL.revokeObjectURL`s in a `finally`. This is the repo's first Blob/download
code of any kind.

**The Blob-error unwrap belongs in `client.ts`, not here.** With
`responseType: 'blob'`, an error response body is a `Blob`, so
`normalizeError`'s `response.data as ApiErrorEnvelope` read yields `undefined`
and the message degrades to axios's generic `"Request failed with status code
403"`. An unwrap inside `download.ts` cannot fix that: every branch of the
interceptor at `client.ts:119` already ends in
`return Promise.reject(normalizeError(error))`, so by the time the caller's
`catch` runs there is no `error.response` left to re-read. The concrete failure
is exactly the case §4.4 was designed for — a user opens a shared
`/active-users?…` URL after a grant was revoked, clicks Export CSV, and the
toast says "Request failed with status code 403" instead of naming the app.

So, inside `client.ts`'s rejection handler, before the status branching (the
handler is already `async`):

```ts
if (error.response?.data instanceof Blob) {
  try { error.response.data = JSON.parse(await error.response.data.text()); }
  catch { /* not JSON — leave as-is */ }
}
```

Then `download.ts` needs no error handling beyond `errorMessage(err)`, and every
future blob-returning endpoint gets the fix for free. Add a vitest for the
interceptor covering a Blob-bodied 403.

---

## 8. The dashboard

### 8.1 A new page, not a tab

`dashboard/src/pages/ActiveUsers.svelte`, root
`<AppShell requireProject requireApp={false}>`.

Not a tab on `UsersExplorer`: that page is `<AppShell requireApp>` and both of
its `$effect`s key off `sessionStore.scopeKey` (`${currentAppId}:${currentEnvId}`).
An N-app, per-app-environment selector there would put two contradictory scope
selectors on one screen and have the Topbar environment switcher fight the local
selection on every change.

Page head follows the house convention: `.head` > `page-title` + `.muted.sub` +
`.controls`. The subtitle carries the cross-app matching caveat from §1.3.
Loading/error/empty triad: `Spinner` / `Card` + `.err-msg` / `EmptyState`.

Registration is three edits plus one icon edit: create the page; add the import
and `'/active-users': guarded(ActiveUsers as Component<never>)` to
`src/routes.ts` under the Analyze section; add a `NavItem` to `Sidebar.svelte`'s
Analyze group with `match: (p) => p.startsWith('/active-users')` and
`show: () => sessionStore.can('event:read')` — the existing Users entry matches
`p.startsWith('/users')`, which does not match `/active-users`, so there is no
collision. `ui/Icon.svelte` gains a `'download'` registry entry importing
`@lucide/svelte/icons/download`; nothing else may import from `@lucide/svelte`
directly.

### 8.2 `dashboard/src/lib/models/active-users.ts`

Pure, DOM-free, with a colocated `*.test.ts`, per the house rule that anything
deciding what a control *means* lives in `src/lib/models/*.ts` — there is no DOM
test environment, so this is the only layer that can be tested.

```ts
export type EnvChoice = string;              // AppEnvironment.id | 'all' | 'none'
export interface AppEnvSelection { [appId: string]: EnvChoice }
export const MAX_SELECTED_APPS = 20;
export function encodeSelection(sel: AppEnvSelection): string[];   // sorted by appId
export function decodeSelection(params: string[]): AppEnvSelection; // bare appId → 'all'
export function selectionCount(sel: AppEnvSelection): number;
export function validateSelection(sel): { ok: true } | { ok: false; reason: string };
export function describeSelection(sel, appName, envLabel): string;
export function utcDayLabel(day: string): string;   // see 8.5
```

`encodeSelection` sorts by `appId` so the URL is stable and the server's cache
key is stable.

### 8.3 `AppEnvPicker.svelte`

Props deliberately share `ScopeTree.svelte`'s vocabulary so the caches are
interchangeable:
`{ apps, envsByApp, loadingEnvApps, value, onchange, onopenapp }`.

`ScopeTree` itself cannot be reused. `ScopeSelection` is
`{ org, projects[], apps[], envs[] }` and `selectionToScopes` **collapses** a
ticked env under a ticked app — that collapse is the whole point of the grant
model and is exactly the pairing this feature must preserve. Reusing
`grant-plan.ts`'s coverage-diff machinery here would actively destroy the
per-app environment choice.

One row per app: a raw `<input type="checkbox">` inside a `<label class="node">`
(the ScopeTree/PermissionPicker idiom — there is no Checkbox primitive) plus a
raw `<select class="sel">` for the environment, disabled until the app is
ticked. Options: "All environments" (`all`), each live enrollment by name, and
"Unattributed" (`none`) — the last shown only when
`sessionStore.can('event:read', { app: appId })`, mirroring the backend's
`UnattributedNeedsAppReach`.

When the server returns `resolved: "subset"` for a selection, the row's label
reads "2 of 5 environments" rather than "All environments" (§4.5).

`$state` Records and Sets are **replaced, never mutated**:
`envsByApp = { ...envsByApp, [appId]: envs }`,
`loadingEnvApps = new Set(loadingEnvApps).add(appId)`.

`ActiveUsers.svelte` owns `envsByApp` / `loadingEnvApps` / `ensureEnvsLoaded(appId)`,
copied from `Members.svelte:231` (guard on
`appId in envsByApp || loadingEnvApps.has(appId)`, replace both references).
Apps come from `sessionStore.apps`, which already holds the current project's
reach-filtered list, so no new listing call. Environments stay N+1 (one
`GET /v1/apps/{id}/environments` per opened app) — there is no batched
environments endpoint and building one is out of scope; lazy loading keeps the
cost proportional to what the user actually picks.

### 8.4 URL round-trip

House pattern from `Issues.svelte:36,137`: read
`new URLSearchParams($querystring ?? '')` **once** at init for `from`, `to` and
`initial.getAll('selection')` → `decodeSelection`, and write back with
`void replace('/active-users?' + p.toString())` inside the same `$effect` that
reloads. This makes the view shareable and makes an export reproducible from a
link.

### 8.5 Charts, and the local-timezone trap

One `Card` with the existing `TimeSeriesChart` over the total —
`series.map(p => ({ bucket: p.day, count: p.active_total }))`. The split is
carried by the tiles (§8.6) rather than by a second and third chart.
`TimeSeriesChart` is single-series, a stacked variant would be a new component
whose only consumer is this page, and three bar charts side by side invite the
reader to compare shapes when the number that matters is the ratio on a single
day. `StatTile` already accepts a `visual` snippet for exactly this, and
`Sparkline` already draws a bare trend from `number[]`.

**`TimeSeriesChart` cannot be used as-is for date-only buckets.** Its
`label(bucket)` does `new Date(bucket).toLocaleDateString(undefined, …)` and its
tooltip calls `formatDateTime(point.bucket)`, which is
`toLocaleString(undefined, …)`. Parsing `'2026-07-31'` is UTC; *rendering* is
not. In `America/New_York` that Date is 2026-07-30T20:00 local, so the bar is
labelled "Jul 30" and its tooltip reads "Jul 30, 2026, 08:00 PM" — a time of day
on a pure calendar-day bucket. The entire slice is built on UTC calendar days
(the SQL idiom, the CSV `day` column, the filename range), so every viewer at a
negative UTC offset would see the chart and the CSV disagree about which day a
number belongs to.

Fix: add a `label?: (bucket: string) => string` prop to
`TimeSeriesChart.svelte`, defaulting to today's behaviour, and pass
`utcDayLabel` from `models/active-users.ts` (which formats with
`timeZone: 'UTC'`). The tooltip uses the same function instead of
`formatDateTime` when the prop is supplied. Pin it with a colocated test that
`'2026-07-31'` labels as Jul 31 under a non-UTC `TZ`.

The chart's last bar is today, which is still filling. It is drawn — dropping it
would make the range shorter than the picker says — but the tiles read from
`latest_full_day`, so the headline number never dips at midnight.

### 8.6 Tiles, banner, export button

`<StatTiles min={150}>`: Active users (`latest.active_total`, sub = the day),
Identified (`latest.active_identified`, `visual` = a `Sparkline` over the
series' identified counts), Guests (`latest.active_guest`, same treatment), Peak
(`Math.max` over `active_total`, sub = the effective range), and Apps
(`selectionCount(selection)`, sub = `describeSelection(…)`). All numbers through
`compactNumber` from `utils/format.ts` — no inline formatting. Em-dash, not `0`,
when `latest` is `None`.

**The Identified tile carries §1.3's caveat inline**, as `sub` when one app is
selected and as a `.muted` line under the tile row when more than one is:
matching across apps is a raw `distinct_id` string comparison, so two apps that
name the same person differently count that person twice. A caveat that lives
only in the wiki and the page subtitle arrives after the figure has already been
read and believed; beside the figure it qualifies, it arrives with it. Guests are
the companion cue: they are never merged across apps by construction, so a large
guest share tells the reader how much of the total was never a candidate for
merging in the first place.

The three tiles must be read as one sentence, so their arithmetic has to hold on
screen: `active_total = active_identified + active_guest` is guaranteed by the
query (§3.4), and the page must not recompute either half client-side. A
derived percentage is fine; a derived count is how the tiles start disagreeing.

When `report.truncated`, an `.err-banner`-styled `div role="status"` with
`<Icon name="info" size={15} />` and `report.truncation_reason` renders above
the tiles. Not a toast: it is a persistent property of the displayed data, not a
transient event.

`<Button variant="secondary" onclick={exportCsv} loading={exporting}>` with the
download icon, in the page `.controls`, disabled while
`!report || !validateSelection(selection).ok`. On success
`toastStore.success('Export downloaded.')`, on failure
`toastStore.error(errorMessage(err))` — mutations toast, reads set local error
state.

`dashboard/src/lib/api/activeUsers.ts` holds
`ActiveUsersParams { from, to, selection: string[] }` and `getActiveUsers`;
axios serializes an array param as repeated keys by default, matching
`serde_html_form`'s `Vec<String>`. The response/domain types
(`ActiveUsersReport`, `ActiveUserPoint`, `SelectionView`, `Window`) go in
`src/lib/models/index.ts`; the request-param interface stays in the api module,
per the `api/alerts.ts` convention.

---

## 9. Two existing defects this slice has to fix

### 9.1 `repo::user_stats` is anchored to the database clock

```rust
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    now: DateTime<Utc>,   // new
) -> QueryResult<UserStats>
```

The three `now() - interval '1 day' | '7 days' | '30 days'` literals become
binds computed once in Rust from a single `Utc::now()`. Bind order: `$1` app_id,
`$2` since, `[$3 env if consumes_bind()]`, then `$n`/`$n+1`/`$n+2` = now−1d /
now−7d / now−30d where `n = if scope.env.consumes_bind() { 4 } else { 3 }` —
derived from `consumes_bind()`, never assumed. `bind_env!` is called between the
`since` bind and the three cutoffs so positional order matches.

**Re-anchored, not re-parameterized.** The literals are not the bug: `dau`/`wau`/
`mau` mean 1/7/30 days by definition and the `UsersExplorer` tiles are literally
labelled "7-day"/"30-day". Repointing them at `since_days` would silently make a
user on the 90d range read "MAU" as a 90-day count. The real defects are that
these were the last reads in the analytics path anchored to the *database*
clock, that the three `now()` calls are three different instants inside one
statement, and that they were untestable without freezing the server clock.

**There are six call sites, not one**: `routes/analytics.rs:351` plus
`backend/crates/sauron-db/tests/env_scoping.rs:1252, 1295, 1322, 1344, 5377` —
the last inside the Subset smoke test that walks every `ReadScope`-taking read
in sequence. Adding the parameter is a compile break in the test crate. The four
assertion-bearing tests at 1252–1344 currently rely on the database clock to
place their seeded rows inside the 1/7/30-day windows, so they pass `Utc::now()`
to preserve exactly today's behaviour; only the new
`user_stats_dau_wau_are_anchored_to_the_supplied_now` passes a fixed instant.
`users_summary` passes the same `Utc::now()` binding it already uses to compute
`since` — one binding, so the two cannot disagree.

Doc-comment the remaining known limitation rather than fixing it: `user_stats`
is **hot-tier only**, and its 30-day `mau` window is exactly the default
`TIER_HOT_DAYS`, so once `sauron-tier` has run, that number silently loses its
oldest days. The new endpoint's `truncated` flag is the principled answer;
`users_summary` keeps the cheap behaviour and says so.

While in `UsersExplorer.svelte`, add the missing DAU tile
(`<StatTile label="DAU" value={compactNumber(analytics.stats.dau)} sub="24h" />`
before the WAU tile at :152). `stats.dau` has always been in the payload and in
`models/index.ts:742`; the tile was simply never rendered, which is why the page
shows a stickiness ratio whose numerator is invisible. Add a one-line link under
the Audience heading pointing at `#/active-users`.

### 9.2 The browser SDK re-mints its anonymous id on every page load

`sdks/js/src/client.ts:144` mints `anon_${uuidv4()}` into an in-memory field, so
`track()` sends a new `distinct_id` on every page load. Active users for any web
app counts page loads, not people — a systematic 5–10× inflation, all of it
landing in `active_guest`. Shipping the chart on top of that is worse than
shipping nothing.

Persist it under `sauron.anon_id` through the existing `sdks/js/src/identity.ts`,
which already implements exactly this pattern one field over for
`sauron.device_id`.

**Ship rotation in the same version.** Persistence interacts with a write path
worth stating: `process_identify` inserts
`identities(app_id, alias_id = <anonymous_id>, distinct_id = <user id>)`
whenever `anonymous_id` is non-empty. Today that alias is scoped to one page
load. Once the anon id is durable, a single `identify()` permanently binds that
browser profile to a named user — and every subsequent anonymous visitor on the
same browser reuses the same `sauron.anon_id`, so on a kiosk or a shared machine
person B's anonymous activity is aliased to person A's account, server-side,
forever. There is no escape hatch today: `index.ts` exports `setUser` but no
`reset()`, and `identity.ts`'s `resetIdentity()` is documented as test-only. And
this slice is what promotes those alias rows from dead storage into a live
signal (the 000038 backfill).

So the same SDK version adds:

- A public `reset()` on `SauronClient`, exported from `index.ts`: clears
  `sauron.anon_id`, regenerates it, clears the scope user. Documented in the SDK
  wiki as MUST-CALL-ON-LOGOUT, and called from `setUser(null)`.
- `anonymous_id` is sent on the identify item **only when the anon id was
  actually used as the `distinct_id` for prior events in this browser session**.
  A persisted id that has never been observed anonymously should not create a
  permanent alias row.
- A release-note line: the anon id becomes a durable first-party identifier
  stored on the user's terminal, which is a retention and consent consequence,
  not just an implementation detail.

**The SDK change must ship and be adopted before the chart is presented as
authoritative.** On adoption day a web app's reported total drops sharply and
permanently, essentially all of it out of the guest half. That discontinuity is a
data artifact, but anyone looking at a brand-new chart will read a 5–10× drop as
a bug in the new feature, so it goes in the release notes.

---

## Error handling

| Case | Status | Note |
|---|---|---|
| Caller not a member of the project's org | 403 | After `project_org` resolves; a missing project is 404 |
| Any selected app unreadable | 403 | Message names the denied app ids |
| App id not in the path's project | 400 | `"app {id} is not in project {project_id}"` |
| Malformed / unknown / duplicate selection token | 400 | Message names the offending token |
| Empty selection, or > `MAX_SELECTED_APPS` | 400 | |
| `to <= from`, or span > `MAX_ACTIVE_USER_DAYS` | 400 | Same wording shape as `TimeseriesQuery::range` |
| `selections × days > MAX_SCAN_BUDGET` | 400 | Rejected before any DB work |
| `?environment_id=…` present | 400 | `reject_environment_id`; the dimension is per selection |
| Over 30 requests/min for this user | 429 | `sauron:analytics:active_users:{user_id}` |
| No semaphore permit | 503 | Ahead of the pool, not behind it |
| `event_users.identified_at` missing | 503 `schema_migration_required` | Names `sauron-migrate` |

---

## Testing

**Constraint:** CI runs `cargo test --workspace` with no Postgres service, and
the dashboard has no DOM test environment. DB-backed tests live in
`backend/crates/sauron-db/tests/` and skip when `TEST_DATABASE_URL` is unset;
everything else is a pure unit test.

### Real Postgres — `backend/crates/sauron-db/tests/env_scoping.rs`

`TestDb::setup()` + `seed_two_envs()`.

| Test | Asserts |
|---|---|
| `active_users_combined_merges_identified_users_across_apps` | Same `distinct_id` in two apps, identified in both, active the same day → `active_total` 1, not 2, and it lands in `active_identified` |
| `active_users_combined_keeps_anonymous_ids_app_local` | Identical `distinct_id` string in two apps, `identified_at` NULL in both → `active_total` 2, all of it `active_guest`. The anti-test for the `'a:'‖app_id‖':'` prefix; without `app_id` in the key it silently returns 1 |
| `active_users_combined_does_not_merge_an_identified_id_with_an_unidentified_copy` | Identified in app A only → `active_total` 2, split 1 identified / 1 guest, pinning the under-merge as intentional |
| `active_users_combined_split_always_sums_to_the_total` | Over a mixed seed of identified, guest and both-in-one-day identities, every point satisfies `active_total == active_identified + active_guest`. The one invariant the page renders as three tiles side by side |
| `active_users_combined_respects_per_app_environment_filters` | App A restricted to env X, app B to env Y; a user present only in app A's env Z must not appear. Mixed `One`/`All` selection, so the bind-index walk is the thing under test |
| `active_users_combined_refuses_cross_app_env_ids` | The §4.3 negative: a member with an env grant on app B only, requesting app A, gets 403 — not a zero-valued leg |
| `active_user_days_are_utc_calendar_days` | Events at 23:30Z and 00:30Z the next day for one identity → two days, `active_total` 1 each — run with the session `TimeZone` GUC set to a non-UTC value, proving GUC-independence (the exact hazard `date_trunc` has) |
| `active_users_combined_returns_zero_rows_for_days_with_no_signal` | A gap day inside the window is present with all three counts 0, not absent. The grid is what the CSV row count is checked against |
| `active_users_combined_excludes_empty_and_null_distinct_ids` | An analytics row with `distinct_id = ''` and an error row with `distinct_id IS NULL` contribute nothing. The empty string is a real value — server SDKs deliberately let the three `$workflow_*` events through with one |
| `user_stats_dau_wau_are_anchored_to_the_supplied_now` | One event 2 days before the passed `now` → `dau == 0`, `wau == 1`, `mau == 1`. Impossible to write before the re-anchoring, which is the point |

**`backend/crates/sauron-db/tests/common/mod.rs` must change first.**
`note_identity()` at :1699 calls `repo::upsert_event_user(…)` and its own doc
comment says it runs from `seed_analytics_event` and `seed_error_event` for
every seeded row. Left alone, every `event_users` row `TestDb::setup()` produces
would be identified, every seeded `distinct_id` would key as `'u:'‖distinct_id`
and merge across apps, `active_guest` would be zero in every test, and the two
anonymity tests above could not be expressed against the harness at all — while
every other assertion silently got merge semantics it did not ask for. Left
unfixed the split would look correct and be untested, which is the worst of both.
`note_identity` gains an `identified: bool` and
routes the ordinary event-seed path through a plain touch, reserving the
identify shape for an explicit identify seed.

### Real Postgres — pipeline and migration

- `process_identify_marks_the_user_identified` (source `'identify'`).
- `process_event_marks_identified_only_when_the_envelope_user_id_matches_the_distinct_id`
  — three cases: matching user → set, mismatched → not set, no user → not set.
- The same three cases for `process_error`, since it now runs the same test.
- `identified_at_is_never_cleared_by_a_later_anonymous_event`.
- `event_users_maintenance_survives_a_missing_identified_at_column` — drop the
  column, run an event through, assert `last_seen` still advances and no job is
  dead-lettered. This is the §2.4 contract and it is the only thing that pins it.
- Migration test: apply 000038 against a DB seeded with (a) an `event_users` row
  with non-empty `properties`, (b) one with a matching `identities` alias row,
  (c) a bare one — assert exactly (a) and (b) get `identified_at` and
  `identified_source = 'backfill'`. This is the only exercise the `identities`
  table has ever had.

### Pure unit tests

- `routes/active_users.rs`: `parse_selection` — bare uuid → All, `:all` → All,
  `:none` → Unattributed, `:<uuid>` → One, malformed uuid → 400 naming the
  token, unknown token → 400, duplicate app id → 400, empty → 400,
  `MAX_SELECTED_APPS + 1` → 400.
- Same module: `latest_full_day` skips today and returns `None` when the window
  contains only today. Plus `effective.from == series[0].day` for a clamp landing
  inside the display window.
- Same module: the cache fingerprint is injective — `[(A, Subset[X,Y])]` and
  `[(A, Subset[X]), (B, Subset[Y])]` never collide, and `EnvFilter::All` versus
  `Subset(<all of the app's envs>)` are distinct keys.
- `csv.rs`: plain field unquoted; `,` quoted; `"` quoted and doubled; embedded
  `\r\n` quoted; leading/trailing space quoted; each of `=`, `+`, `-`, `@`,
  `\t`, `\r` as a first byte gets the `'` prefix; empty field emits nothing
  between commas; terminator is `\r\n`.
- `dashboard/src/lib/models/active-users.test.ts`: `encodeSelection` is sorted
  by appId and round-trips through `decodeSelection`; a bare appId decodes to
  `all`; `validateSelection` rejects empty and > `MAX_SELECTED_APPS`;
  `describeSelection` for 1, 2 and N apps; `utcDayLabel('2026-07-31')` is
  Jul 31 under a non-UTC `TZ`.
- `dashboard/src/lib/api/client.test.ts`: a Blob-bodied 403 normalizes to the
  envelope's message, not axios's generic string.

### HTTP — `backend/bins/sauron-api/tests/http_active_users.rs`

Spawns the compiled binary via `TestServer`, copied from `http_workflows.rs`,
skipping when `TEST_DATABASE_URL`/`TEST_REDIS_URL` are unset: 403 for a
non-member; 403 naming the app when one of two selections is unreadable by an
env-scoped member; 200 for that same member when only their own app+env is
selected; **a bare `selection=<app>` from an env-scoped member returns a
`SelectionView` whose `resolved` is `"subset"`, never `"all"`**; 400 on
`?environment_id=<valid uuid>`; 400 on `to < from`; 400 on a span > 92 days;
400 on an app in a sibling project.

CSV: `200`, `content-type: text/csv; charset=utf-8`, `content-disposition`
matching `attachment; filename="sauron-active-users-<uuid>-\d{8}_\d{8}\.csv"`, a
first line of exactly `day,active_total,active_identified,active_guest\r\n`, and
a body row count equal to the JSON route's `series.len()` for the same query —
the shared-`build_report` guarantee, checked rather than assumed.

### `http_env_scoping.rs` gains a project-scoped class

These two routes are the first telemetry reads outside `/v1/apps/{id}/…`, so
they sit outside the only mechanised check that a telemetry GET resolves
environment scoping rather than accepting-and-ignoring it — `APP_SCOPED_URL` in
`dashboard/src/lib/api/scope.ts:79` never matches them and
`app_scoped_get_route_templates()` never enumerates them. Compensating with one
bespoke case in a new file means the next author will not know to replicate it.

Add `project_scoped_get_route_templates()` alongside the app-scoped one,
covering `/v1/projects/{project_id}/…` GETs, with the same two-directional
correspondence against a new `PROJECT_SCOPED_*` array in `scope.ts`. That makes
`reject_environment_id` mandatory-by-test for every future project-scoped
telemetry route.

### Measurement and manual verification

- **Before 000039/000040**, `EXPLAIN (ANALYZE, BUFFERS)` the exact
  `active_users_combined` statement for a single `(app, One(env))` selection over
  30 days on a real dataset; record `Heap Fetches` and shared-buffer counts, and
  ship the index substitution only if heap fetches dominate. Re-run after and
  record both in the migration report — the evidence standard migrations 25, 28
  and 31 each set.
- **Against a deployment that has actually run `sauron-tier`**, confirm the
  truncation banner names the real watermark date and that `effective.from`
  matches the first bar on the chart (§5.2). No seeded-DB test can catch this.
- Seed two apps in one project with an overlapping identified `distinct_id`,
  open `#/active-users`, tick both with different environments, confirm
  `active_total` is less than the sum of the two single-app totals and that the
  three tiles add up on screen, click Export CSV and confirm the file's numbers
  equal the on-screen tiles. Then revoke the caller's grant on one app and reload
  the shareable URL: the 403 names it and the page recovers.

---

## Config and packaging

- **No config change of any kind.** No new environment variable, no changed
  default — `TIER_HOT_DAYS` in particular keeps its shipped value, and this slice
  touches neither `config.rs` nor `.env.example` nor `docker-compose.yml` nor any
  file under `packaging/rpm/config/`. The horizon it inherits is the horizon it
  reports through `truncated`; buying a longer one by doubling every existing
  deployment's hot Postgres on upgrade is not this feature's decision to make.
- No new binary, no new workspace dependency (`csv`, `futures`, `tokio-util`,
  tokio `fs` all stay out), nothing added to `packaging/rpm/binaries.txt` or
  `sauron.spec`'s `%files`.
- **No new permission.** Both routes gate on the existing `perm::EVENT_READ`, so
  `perm::ALL`, `rbac.rs`'s four preset-role count assertions,
  `dashboard/src/lib/models/permissions.ts` and the `Permission` union are all
  untouched — whatever counts the slices before this one left behind stand.
- Four `main.rs` edits, all rebased onto the pinned `AppState` sequence: S0 adds
  its `mail` field (additive), S2 adds `SessionRevocations` plus the new
  extractor bound, and S4 lands on top of that shape with the concurrency
  semaphore in `AppState`, its two routes, the CORS `expose_headers` line
  (§7.3) and the boot schema probe. The order is fixed, not
  whichever-lands-last.
- `packaging/rpm/SETUP.md` §11 (created by S0) gains three rows —
  000038/000039/000040 — each with its symptom.

## Risks

- **Cross-app merging is exact string equality.** If two apps name the same
  person differently, `active_identified` is a sum, not a union. Stated beside
  the tile, in the page subtitle and in `wiki/Active-Users.md`; there is no
  server-side fix short of an identity-resolution table. The split is the
  mitigation — a deployment whose apps do not share an identifier sees it in the
  guest share rather than being told after the fact — but a reader who ignores
  all three still walks away with an inflated number.
- **The SDK fix causes a permanent one-time drop** on adoption day for every web
  app. Release notes, not a footnote.
- **The backfill under-merges and then catches up organically**, so the
  identified/guest split moves for a period after deploy for reasons unrelated to
  user behaviour.
- **No `statement_timeout` exists in the workspace.** The semaphore, the
  limiter, the scan budget and the clamp are indirect; a query whose HTTP
  request has timed out keeps running in Postgres. Standing platform gap.
- **On shipped defaults the visible window is about 30 days, not 92**, because
  `TIER_HOT_DAYS` is 30 and `sauron-tier` runs by default in both topologies.
  Widening it is an operator's call and their disk; the banner is what stops it
  being a silent one.
- **The clamp is conservative and will sometimes hide data still physically
  present** between export and DETACH.
- **The index substitution is proposed on analogy, not measurement**, and it is
  a synchronous rebuild across every child partition on the two largest tables.
  §2.3's measure-first rule is what keeps that honest.
- **`repo::user_stats` gains a parameter with six call sites**, one of which is a
  smoke test that walks every `ReadScope` read.

## Files

**New**

- `backend/migrations/2026-08-01-000038_event_users_identified/{up,down}.sql`
- `backend/migrations/2026-08-01-000039_analytics_active_user_index/{up,down}.sql`
- `backend/migrations/2026-08-01-000040_error_active_user_index/{up,down}.sql`
- `backend/bins/sauron-api/src/routes/active_users.rs` — both handlers,
  `parse_selection`, `latest_full_day`, the fingerprint
- `backend/bins/sauron-api/src/csv.rs` — the export primitive
- `backend/bins/sauron-api/tests/http_active_users.rs`
- `dashboard/src/pages/ActiveUsers.svelte`
- `dashboard/src/lib/components/AppEnvPicker.svelte`
- `dashboard/src/lib/models/active-users.ts` + `.test.ts`
- `dashboard/src/lib/api/activeUsers.ts`, `dashboard/src/lib/api/download.ts`
- `wiki/Active-Users.md` + entries in `wiki/_Sidebar.md` and `wiki/Home.md` —
  carries the exact-string-equality caveat and the masking warning from §1.3

**Modified**

- `backend/crates/sauron-db/src/{schema.rs,models.rs}` — two appended fields
- `backend/crates/sauron-db/src/repo.rs` — `AppEnvScope`, `ActiveUserDay`,
  `active_users_combined`, `env_ids_for_apps`, `mark_event_user_identified`,
  `user_stats` signature
- `backend/crates/sauron-db/tests/common/mod.rs` — `note_identity(identified)`
- `backend/crates/sauron-db/tests/env_scoping.rs` — five `user_stats` call sites
  plus the new tests
- `backend/crates/sauron-pipeline/src/process.rs` — the identification test at
  both call sites, `let _ =` → logged
- `backend/bins/sauron-api/src/main.rs` — two routes, CORS `expose_headers`,
  the semaphore in `AppState`, the boot schema probe
- `backend/bins/sauron-api/src/routes/{mod.rs,analytics.rs}` — module
  registration, `user_stats` call site
- `backend/bins/sauron-api/tests/http_env_scoping.rs` +
  `dashboard/src/lib/api/scope.ts` — the project-scoped route class
- `dashboard/src/lib/api/client.ts` — Blob unwrap before `normalizeError`
- `dashboard/src/lib/components/TimeSeriesChart.svelte` — optional `label` prop
- `dashboard/src/lib/components/layout/Sidebar.svelte`, `dashboard/src/routes.ts`,
  `dashboard/src/lib/ui/Icon.svelte`, `dashboard/src/lib/models/index.ts`
- `dashboard/src/pages/UsersExplorer.svelte` — the missing DAU tile
- `sdks/js/src/{client.ts,identity.ts,index.ts}` — persisted anon id + `reset()`
- `packaging/rpm/SETUP.md` — the three migration rows

## Follow-ups (out of scope)

- The materialized `(app_id, environment_id, day, identity_key)` rollup inside
  `sauron-tier`, run before `detach_and_drop_partition`. The only path to
  windows longer than the hot horizon that does not go through DuckDB.
- Cross-tier active users via `DuckEngine` ATTACHing Postgres — mechanically
  proven in `export_from_postgres`, blocked on `INSTALL postgres` versus
  `ProtectHome=true`.
- Rolling multi-day windows (weekly and monthly active users) on top of the same
  identity key. They need a lookback that exceeds the hot horizon on every
  shipped default, so they are blocked on the rollup above, not on the query.
- A `(app_id, occurred_at, distinct_id)` index, if and only if the §2.3
  measurement shows heap fetches still dominating under `EnvFilter::All`.
- A per-org display timezone for day bucketing. Not a display fix — it
  re-buckets and changes every historical number, and needs its own cache
  dimension.
- Anonymous-id persistence in the Flutter and server SDKs.
- A batched "environments for every app in this project" endpoint.
- A `statement_timeout` mechanism, once a shape exists that neither needs a
  transaction nor leaks to the next borrower of a pooled connection.
- Truncation reporting on `GET /v1/apps/{app_id}/users/summary`.
