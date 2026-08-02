# PII Inspector — find it, prove it, mask it in hot Postgres, and say what masking cannot reach

Date: 2026-08-01
Status: designed

**Masking rewrites rows in hot Postgres only.** Cold Parquet, the Redis ingest
stream, the dead-letter queue and everything already delivered to a mailbox or a
chat channel keep the raw bytes, and no part of this feature reaches them. §1
enumerates where they still are; the product copy must never say "permanently
removed".

This document merges the two halves the feature was designed in (S5a — policy,
scheduling, scan engine, findings; S5b — masking, enforcement, audit, UI). They
were written in parallel and disagreed in six places; every disagreement is
resolved below and the resolution is called out where it lands. **They ship as
one slice.** The split was never a release boundary — S5a's routes gate on
permissions S5b minted, so S5a alone does not compile.

## Problem

Developer-supplied payload lands in five jsonb columns per error row
(`tags`, `contexts`, `extra`, plus machine-owned `context` and `event_user`) and
nobody knows what is in them. Live data on the dev box already contains
`event_user.email`, full URLs with query strings inside `stacktrace[].filename`,
and free-text breadcrumb messages. There is no way to answer "does this app
store email addresses" short of `SELECT * FROM error_events` and reading, and no
way to remove one once found.

Three capabilities are missing, and one liability comes with them:

- **Find.** Nothing enumerates jsonb keys. `jsonb_object_keys` and `jsonb_each`
  appear nowhere in the repository, there is no facets endpoint, and the tags
  GIN is `jsonb_path_ops` — containment only, so it cannot serve key existence.
- **Decide.** An admin needs "the key `email` appears in `error_events.extra` in
  41,200 rows of prod, first seen 12 days ago" — not a row dump.
- **Remove.** There is exactly one UPDATE against a partitioned event table in
  the whole workspace (`repo::update_event_symbolication`), no bulk-write
  pattern, no batching precedent, and no ingest-time hook.
- **The liability.** A tool that reports PII is a tool that stores PII. A
  findings table that keeps sample values is a second, longer-lived, more
  concentrated copy in a table nobody tiers — strictly worse than the original.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| How is a tracked key specified? | A literal key **name**, matched case-insensitively and exactly, at any depth. Plus a closed, opt-in library of value **detectors** for "I don't know the key name" | Admin-authored regex (ReDoS authored by an org admin against a shared worker, plus `regex` is only a transitive dep — adding it is a workspace edit); substring matching; dotted-path input (the admin does not know the SDK nested it under `contexts.order`) |
| Keys or values? | Keys by default. Values only (a) for keys that already matched, to derive type + preview, and (b) when detectors are explicitly enabled — separate policy field, separate shorter window | Always scanning values (turns every scan into the worst case, §5); never scanning values (misses PII under innocuous keys like `note`) |
| Where does matching run? | Two phases: a cheap SQL prefilter over an index-bounded row window, then a `serde_json` walk in Rust | A recursive path-walking SQL CTE (untestable in CI, ~100-250 µs on *every* row); `jsonb_path_query` from an admin-derived jsonpath (user bytes in a jsonpath literal, and it loses the location) |
| What does a finding store? | A **locator** plus a shape-only redacted preview and the JSON type. No raw value column. **No hash column** | Full sample values behind `pii:read`; a SHA-256 of the value (a stable pseudonymous identifier of a person, trivially brute-forced for low-entropy values) |
| Mask semantics | The value at the path becomes the JSON string `"****"`. The **key is kept** | Key removal (changes row shape, breaks `contexts` block structure, makes a `has:` predicate report absence where data existed); per-type sentinels; partial masking (`j***@acme.com` — needs a regex engine and leaves recoverable residue) |
| Where does forward enforcement live? | `sauron-pipeline`, off the HTTP handler | The ingest edge before `XADD` — it would keep raw values out of the Redis stream, but it cannot see the enriched `context` and it forces per-app policy onto `EnvRef` (§11) |
| Scheduling | 7-bit weekday mask + `TIME` + IANA timezone + a materialized `next_run_at`, all DST arithmetic in Postgres | A cron expression (no parser in the repo, no cron crate in `Cargo.lock`, plus cron's DOM/DOW OR-semantics footgun); a fixed interval env var (cannot express "Tuesdays at 03:00") |
| Can a running mask be stopped? | **Yes** — a `cancelling` status the batch loop checks between batches | S5b said no, on the grounds that stopping leaves "no record of where it stopped". Its own design refutes that: the cursor is committed in the same statement as every batch. Overruled |
| Which tables may be touched? | An explicit **allowlist** of telemetry tables, asserted by a unit test | A denylist — it silently fails to protect the next account table someone adds. S2 asserts this constraint on this slice and it is honoured here as an allowlist |
| New permissions | `pii:read` and `pii:manage`, Owner and Admin only | Developer inherits `pii:read` (drags bulk PII disclosure across the whole engineering org); one combined permission (then reviewing the audit trail requires the power to mask) |

## Non-goals

- **Scanning or masking cold Parquet.** `sauron-tier` exports with `SELECT *`,
  so every jsonb column is in the file as VARCHAR, and after the drop Parquet is
  the only copy. Reaching it means linking `libduckdb` into a fourth build path
  and a read-patch-write-rename per file whose row-count change would stall
  `sauron-tier`'s idempotency guard permanently. Named as unreachable, loudly.
- Trimming `sauron:ingest:dlq`. It is `XADD` with no `MAXLEN` and no TTL. A
  bounded DLQ is a separate and arguably more urgent fix.
- Un-masking. There is no shadow copy and none is created.
- Read-time redaction as an alternative mode. `strip_source_context` is applied
  at 2 of the 8 response paths that emit raw `ErrorEvent` structs; that drift is
  exactly what a read-time masker would inherit.
- Environment-scoped masking. There is no `authorize_env` (§12).
- A two-person rule, mask quotas, or rate-limiting of mask actions.
- Alerting when a scan finds something new.

---

## 1. What "permanently masked" does not mean

**Read this before writing the UI copy.** The product must not claim a mask is
permanent, because in twelve named places the promise does not hold: eleven
where the bytes survive the mask, and one where the mask silently takes
something else away with it. All of it is one data array in
`dashboard/src/lib/models/inspector.ts` (`UNREACHABLE_COPY`), rendered verbatim
in the MaskDialog, in the Audit tab detail, and in the wiki page — one source, so
support answers and the product cannot diverge.

Its **first entry is the headline**, above the enumerated rows and therefore at
the top of the mask confirmation dialog: *masking rewrites rows in hot Postgres
only.* The twelve rows below it are the detail — the headline states the
boundary, the rows say where the bytes still are.

| What the promise does not cover | Why | Bounded by |
|---|---|---|
| **Cold Parquet** | The partition was exported before the mask ran. Parquet is immutable and, after the drop, the only copy | Nothing. Permanent |
| **Postgres rows older than `TIER_HOT_DAYS`** | The retro-mask deliberately stops at the hot boundary (§9) | The tier drop, which destroys the row entirely |
| **The Redis ingest stream** | `sauron:ingest:stream` holds the full serialized `IngestJob` | `XADD … MAXLEN ~ 1000000` |
| **The Redis DLQ** | `sauron:ingest:dlq` is `XADD` with no `MAXLEN` and no TTL, and no reaper exists. Post-deserialize failures are fixed here (§11); a payload that fails serde still dead-letters raw | Nothing. Permanent |
| **Per-person breadcrumbs in Redis** | `sauron:bc:{app_id}:{distinct_id}`, up to 100 batches | A 1800 s TTL |
| **`alert_events.title` / `.body`** | They embed `issues.title` (= `exception_type: exception_value`) verbatim | `ALERT_EVENT_RETENTION_DAYS` (90) |
| **Already-delivered alerts** | Email, Slack, Discord, Matrix, Telegram and webhook messages containing the same string are gone from our control the moment they send | Nothing |
| **`event_users.properties`** | `upsert_event_user` merges with `\|\|`, which never removes keys. An at-rest mask is undone by the next `identify()` | Forward enforcement only, and only for keys in the mask set |
| **`devices.*`** | Every column is `COALESCE(EXCLUDED.x, devices.x)` — a non-null incoming value always wins, and there is no wire field to enforce on. **`devices` is therefore not maskable at all** and is excluded from the target allowlist | Not offered |
| **Symbolicated source lines** | `stacktrace_symbolicated` frames carry `context_line`/`pre_context`/`post_context` — verbatim customer source. Masking a JSON path never touches them | `strip_source_context`, which redacts the *response* only |
| **Backups, WAL, replicas** | Out of the product's reach entirely | Operator policy |
| **The active-users report stops identifying anyone new through that key** | The enforcer runs before the active-users pipeline stamps `identified_at`, so masking a key an app sends as `context.user.id` means the equality test never passes again. Nobody already stamped is un-identified — those writes are first-write-wins — but everyone first seen afterwards arrives as a guest and never merges across apps, so the identified share decays with no discontinuity to notice | Nothing. The bytes are gone, so it cannot be recomputed later. This is the one row that must be read *before* confirming |

Two more honest caveats that are not "copies" but do defeat the promise:

- **The 30-second seam.** Forward enforcement reads a per-app cache with a 30 s
  TTL (§11). Rows written between confirm and cache refresh land raw. The tail
  sweep (§9) re-runs the seam once, keyed on `received_at`, but an event with a
  client-supplied `occurred_at` from three days ago that arrives *after* the
  sweep is never revisited.
- **Detection is best-effort, not a compliance guarantee.** The phase-1
  prefilter greps the JSON *text* for the quoted key name, so a key serialized
  with a unicode escape (`"email"`) evades it, as does anything inside a
  base64 or URL-encoded blob. This is the right tool for accidental PII, which
  is what it is for, and useless against an adversary. The Findings tab carries
  this sentence non-dismissibly.

The MaskDialog renders the table above before the confirm button is enabled, and
the wording is "masked in hot Postgres and in all future ingest" — never
"permanently removed".

---

## 2. Migrations `000041`, `000042`, `000043`

The programme allocation pins S0=000034 … S4=000038-000040, so S5 starts at
000041 and takes three contiguous numbers. Date prefixes must be monotone with NN — `run_pending_migrations` orders
by the **full** directory string, date first, so a later-authored migration with
an earlier date runs out of order and nobody notices until an FK fails.

### `2026-08-01-000041_pii_perms`

```sql
UPDATE roles SET permissions = permissions || '["pii:read","pii:manage"]'::jsonb
WHERE org_id IS NOT NULL
  AND jsonb_typeof(permissions) = 'array'
  AND permissions @> '["org:manage"]'::jsonb
  AND NOT permissions @> '["pii:read"]'::jsonb
  AND NOT EXISTS (
    SELECT 1 FROM role_grants g WHERE g.role_id = roles.id AND g.scope_type <> 'org'
  );
```

The last clause is the whole point and S5b did not have it. `org:manage` is
**inert** outside org scope — `authorize_org` only accepts an org grant — so a
custom role holding `org:manage` that happens to be granted at app scope is
harmless today. `pii:manage` is enforced by `authorize_app`, so it is fully live
at app scope. Granting the pair on the permission predicate alone would silently
promote those holders to irreversible bulk destruction of one app's data. The
`NOT EXISTS` restricts the grant to roles that only ever sit at org scope.

The condition is evaluated once. A role with zero grants qualifies, and could
later be granted at app scope — but only by someone who already holds
`pii:manage`, because `create_grant`'s escalation check requires it.

Presets need no `UPDATE`: `ensure_preset_roles` re-syncs them from `rbac.rs` at
every API boot. `down.sql` strips both strings from custom roles using migration
26's `jsonb_array_elements` + `jsonb_typeof = 'array'` idiom.

### `2026-08-01-000042_inspector_scan`

Creates `inspector_policies`, `inspector_scans`, `inspector_findings`.

**`inspector_policies`** — where inspection is on, what it looks for, and when.

| Column | Type | Note |
|---|---|---|
| `id` | `UUID PK DEFAULT gen_random_uuid()` | |
| `org_id` | `UUID NOT NULL REFERENCES organizations ON DELETE CASCADE` | Denormalized tenant key, same as `alert_rules` |
| `target_type` | `TEXT NOT NULL CHECK (target_type IN ('project','app','app_env'))` | **Not** named `scope_type`: `dashboard/src/lib/models/scope-type.test.ts` parses the *newest* `CHECK (scope_type IN (...))` out of the migrations directory and asserts it equals `['app','env','org','project']`. A new column with that name fails that test |
| `target_id` | `UUID NOT NULL` | Polymorphic, no FK (matches `role_grants`). For `app_env` it holds an **`app_environments.id`** — the enrollment id — never a catalogue `environments.id` |
| `enabled` | `BOOL NOT NULL DEFAULT true` | |
| `tracked_keys` | `JSONB NOT NULL DEFAULT '[]'` | `[{key, scope:'any'\|'top'}]`, key lowercased at write |
| `detectors` | `JSONB NOT NULL DEFAULT '[]'` | Preset ids from a `&'static` list |
| `scan_columns` | `JSONB` | NULL = the default column set. **Not** named `columns`: `diesel_derives` emits `pub mod columns` inside every generated table module and re-exports it, so a column named `columns` produces `error[E0573]: expected type, found module` on the `table!` block *and* on every `#[diesel(table_name = …)]` derive. Verified against the workspace's diesel 2.3.11 |
| `rollups` | `JSONB NOT NULL DEFAULT '["issues","event_users"]'` | Which non-partitioned tables to include |
| `window_days` | `INT NOT NULL DEFAULT 30 CHECK (window_days BETWEEN 1 AND 400)` | Clamped again at run time to `inspector_window_days` |
| `schedule_enabled` | `BOOL NOT NULL DEFAULT false` | |
| `schedule_days` | `SMALLINT NOT NULL DEFAULT 0 CHECK (schedule_days BETWEEN 0 AND 127)` | Bit N = `EXTRACT(DOW)=N`, so Sunday is bit 0 |
| `schedule_time` | `TIME NOT NULL DEFAULT '03:00'` | Local wall clock |
| `schedule_tz` | `TEXT NOT NULL DEFAULT 'UTC'` | IANA name, validated at write with `SELECT now() AT TIME ZONE $1` → 400 |
| `next_run_at` | `TIMESTAMPTZ` | Materialized due time — the `monitors.next_check_at` pattern |
| `last_run_at`, `last_scan_id`, `last_skip_reason` | | Operator visibility for catch-up skips |
| `created_by` | `UUID REFERENCES users ON DELETE SET NULL` | |
| `created_at`, `updated_at` | `TIMESTAMPTZ NOT NULL DEFAULT now()` | |

Indexes: `UNIQUE (target_type, target_id)` — one policy per node, which is what
makes precedence a database fact rather than an ordering problem; `(org_id)`;
and `inspector_policies_due_idx (next_run_at) WHERE enabled AND schedule_enabled`,
the `monitors_due_idx` mirror.

A policy with `tracked_keys = '[]'` **and** `detectors = '[]'` is rejected at
write with 400. Without that, the single most likely first configuration —
"I don't know my payload shape, turn on the email detector" — combined with the
prefilter being built only from the key list produces a scan that reads zero
rows and finishes `succeeded`, `coverage='full'`, zero findings. A confident
false negative on a privacy scan is the worst thing this feature can emit.

**`inspector_scans`** — one row per run, and the resume cursor.

`id`; `policy_id UUID NOT NULL REFERENCES inspector_policies ON DELETE CASCADE`;
`org_id UUID NOT NULL REFERENCES organizations ON DELETE CASCADE` (so the reaper
and list queries never join upward);
`trigger_type TEXT NOT NULL CHECK IN ('scheduled','manual')` — named for
consistency with `alert_rules.trigger_type`; `requested_by UUID REFERENCES users
ON DELETE SET NULL`; `status TEXT NOT NULL DEFAULT 'queued' CHECK IN
('queued','running','succeeded','failed','cancelled')`;
`coverage TEXT NOT NULL DEFAULT 'full' CHECK IN ('full','partial')` — kept
separate from `status` so a completed-but-incomplete scan is not mistaken for a
failure; `coverage_note TEXT NOT NULL DEFAULT ''`; `window_from`/`window_to
TIMESTAMPTZ NOT NULL` frozen at start; `params JSONB NOT NULL` (a copy of
`tracked_keys`/`detectors`/`scan_columns`/`rollups`); `targets JSONB NOT NULL`
(the resolved ordered `[(app_id, app_env_id|null)]` list, capped at 2000 pairs);
`units_total`/`units_done INT NOT NULL DEFAULT 0`;
`cursor JSONB NOT NULL DEFAULT '{}'`; `rows_scanned BIGINT`;
`findings_count INT`; `worker_id TEXT`; `heartbeat_at TIMESTAMPTZ`;
`attempts INT NOT NULL DEFAULT 0`; `cancel_requested_at TIMESTAMPTZ`;
`error TEXT`; `started_at`/`finished_at`; `created_at`.

Indexes: `(policy_id, created_at DESC)`; `(org_id, created_at DESC)`;
`(status, heartbeat_at)` for the claim; and
`UNIQUE (policy_id) WHERE status IN ('queued','running')`. That partial unique
index is what makes "one active scan per policy" a database invariant instead of
a race between the API and the scheduler.

**`inspector_findings`** — the aggregated result, and the table that creates the
PII-copy problem §6 is about.

`id`; `scan_id UUID NOT NULL REFERENCES inspector_scans ON DELETE CASCADE`;
`org_id UUID NOT NULL`; `app_id UUID NOT NULL REFERENCES apps ON DELETE CASCADE`;
`environment_id UUID NULL`;
`env_scope TEXT NOT NULL CHECK (env_scope IN ('enrollment','unattributed','no_env_column'))`
with `CHECK ((env_scope = 'enrollment') = (environment_id IS NOT NULL))`;
`source_table`/`source_column TEXT NOT NULL` (both from the `&'static`
inventory, never caller bytes); `key_path TEXT NOT NULL`; `matched_key TEXT NOT
NULL`; `detector TEXT NOT NULL DEFAULT ''`; `value_type TEXT NOT NULL`;
`match_count BIGINT NOT NULL DEFAULT 0`;
`match_count_exact BOOL NOT NULL DEFAULT true`;
`sample_preview TEXT NOT NULL DEFAULT ''`; `sample_row_id UUID`;
`sample_occurred_at TIMESTAMPTZ`;
`partition_kind TEXT NOT NULL DEFAULT 'ranged' CHECK IN ('ranged','default','rollup')`;
`first_seen_at`/`last_seen_at`; `created_at`.

`env_scope` is the third state S5a's design lacked. It wrote `environment_id
IS NULL` to mean "the unattributed bucket", but `issues`, `event_users`,
`devices` and `identities` **have no environment column at all**, so every
rollup finding would land in that bucket and conflate "the platform could not
attribute this row" (which `EnvFilter::Unattributed` gates behind app-wide reach)
with "this table has no environment concept". They are different facts and the
UI must say different things.

Uniqueness, and the reason it is an expression index:

```sql
CREATE UNIQUE INDEX inspector_findings_key ON inspector_findings
  (scan_id, app_id, env_scope,
   COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid),
   source_table, source_column, key_path, detector);
```

S5a specified `NULLS NOT DISTINCT`, which silently raises the deployment's
Postgres floor to 15. `grep -rn 'NULLS NOT DISTINCT' backend/migrations/` returns
zero; the existing DDL floor is PG12/13-era, `sauron.spec` declares only
`Recommends: postgresql-server` with no version bound, and no doc states a
minimum. On a RHEL 9 host (default module stream = 13) that is a syntax error —
and because `run_pending_migrations` is diesel's ordered runner that stops at the
first failure, 000041 would apply, 000042 would fail, and **every later
migration in the product would be permanently blocked**. The `COALESCE`
expression index is PG11+ and the upsert targets the same expression list.

Also: `(scan_id, match_count DESC)` for the default listing and
`(org_id, created_at)` for the reaper. There is no raw-value column and no hash
column, and the migration comment says why.

### `2026-08-01-000043_inspector_mask_audit`

Creates `inspector_mask_actions`, `inspector_masked_keys`,
`inspector_reveal_audit`. It must run after 000042 because of the FKs to
`inspector_findings` and `inspector_scans`. All three land together, so this is
a within-release ordering, not a cross-slice contract.

**`inspector_mask_actions`** is the repo's first audit table and simultaneously
the job queue, the resume cursor, the progress meter and the record of who did
it.

| Column | Type | Note |
|---|---|---|
| `id` | `UUID PK` | |
| `org_id`, `app_id` | `UUID NOT NULL REFERENCES … ON DELETE CASCADE` | `app_id` is also the key the pipeline enforcer caches on |
| `kind` | `TEXT NOT NULL CHECK (kind IN ('preview','mask'))` | **Load-bearing.** S5b routed previews through the same `status` machine, where `status='preview'` matched neither arm of the claim predicate — no preview would ever run, the dialog would poll forever, and confirm (which requires `previewed`) could never fire. Counting vs. updating branches on `kind`, never on `phase` |
| `finding_id`, `scan_id` | `UUID NULL REFERENCES … ON DELETE SET NULL` | Nullable so the audit row outlives finding pruning. Both validated against `app_id` at preview |
| `targets` | `JSONB NOT NULL DEFAULT '[]'` | The fully resolved `[{table, column, path, wildcard}]` list, frozen at preview so confirm can never widen it. Contains paths, never values |
| `status` | `TEXT NOT NULL DEFAULT 'preview' CHECK IN ('preview','previewed','pending','running','cancelling','done','failed','cancelled')` | `cancelling` is new — see §10 |
| `requested_by` | `UUID NULL REFERENCES users ON DELETE SET NULL` | `SET NULL`, not `CASCADE`: deleting a user must not erase the trail |
| `requested_by_email` | `TEXT NOT NULL DEFAULT ''` | Denormalized snapshot, because `SET NULL` loses the identity |
| `cancelled_by`, `cancelled_by_email`, `cancelled_at` | | S5b had none of these. In an audit table whose whole justification is "who did it", the one adversarial action the design permits — stopping a redaction — was the one it could not attribute |
| `requested_at`, `confirmed_at`, `started_at`, `finished_at` | `TIMESTAMPTZ` | Only `requested_at` is `NOT NULL DEFAULT now()` |
| `previewed_at` | `TIMESTAMPTZ` | The preview TTL runs from here, not from `requested_at` (§10) |
| `confirm_source` | `TEXT NOT NULL DEFAULT ''` | See §12 for why this is usually the proxy's address |
| `estimated_rows`, `rows_scanned`, `rows_masked` | `BIGINT NOT NULL DEFAULT 0` | |
| `cold_rows_skipped` | `BIGINT NOT NULL DEFAULT 0` | |
| `cold_boundary_at` | `TIMESTAMPTZ` | Re-recorded at finish, not only at preview |
| `day_cursor DATE`, `cursor_occurred_at`, `cursor_id` | | Resume state |
| `phase` | `TEXT NOT NULL DEFAULT 'idle' CHECK IN ('idle','counting','hot','default_partition','companions','tail_sweep','finished')` | |
| `worker_id`, `claimed_at` | | Claim + stale detection |
| `vacuum_advised` | `BOOL NOT NULL DEFAULT false` | §9 |
| `error` | `TEXT NOT NULL DEFAULT ''` | Mirrors `alert_events.error` |

Indexes: `(app_id, requested_at DESC)`; `(org_id, requested_at DESC)`; and two
partial indexes for the two independent claim slots,
`(requested_at) WHERE kind='mask' AND status IN ('pending','running','cancelling')`
and `(requested_at) WHERE kind='preview' AND status='preview'`.

**`inspector_masked_keys`** — the forward-enforcement list the pipeline reads.
`id`; `app_id UUID NOT NULL REFERENCES apps ON DELETE CASCADE`;
`target_table TEXT NOT NULL`; `target_column TEXT NOT NULL`;
`json_path TEXT NOT NULL DEFAULT ''` (`''` = the whole column);
`created_at`; `created_by UUID NULL REFERENCES users ON DELETE SET NULL`;
`source_action_id UUID NULL REFERENCES inspector_mask_actions ON DELETE SET NULL`.
`UNIQUE (app_id, target_table, target_column, json_path)` makes re-masking the
same finding idempotent. Index `(app_id)` for the enforcer's cache-miss load.

The `CHECK` on `target_table` transcribes the six **maskable** tables from §3 —
`error_events`, `analytics_events`, `transactions`, `issues`, `event_users`,
`sessions` — and nothing else. The scan-only tables (`devices`, `identities`,
`workflows`) are deliberately absent: a masked-key row for one of them would be
read by the pipeline enforcer and by the retro-mask job, both of which would
report success on a write the next event overwrites.

**`inspector_reveal_audit`** — `id`; `app_id UUID NOT NULL REFERENCES apps ON
DELETE CASCADE`; `org_id UUID NOT NULL`; `finding_id UUID NULL REFERENCES
inspector_findings ON DELETE SET NULL`; `user_id UUID NULL REFERENCES users ON
DELETE SET NULL`; `user_email TEXT NOT NULL DEFAULT ''`;
`source_table`/`source_column`/`key_path TEXT NOT NULL`;
`request_source TEXT NOT NULL DEFAULT ''`; `created_at`. Index
`(app_id, created_at DESC)`.

This table did not exist in either half. `POST /…/reveal` is an endpoint whose
entire purpose is emitting raw customer PII; shipping it with no record of who
revealed what is not defensible, and S5a's own justification for POST-over-GET
was "so the sibling's audit trail has a request body to record". The row is
written **before** the value is returned, so a failure to audit is a failure to
reveal.

### schema.rs and models.rs

Six new `diesel::table!` blocks. **Assert the delta, not an absolute** — both
halves pinned absolute counts (32, 34, 29→31) and they cannot all be right,
because S0-S3 also add tables. The drift detector is
`grep -c '^diesel::table!' schema.rs`, and it must increase by exactly 6.

`joinable!` lines for the non-nullable FKs only, matching `alert_rules.created_by`
which has none: `inspector_policies -> organizations (org_id)`;
`inspector_scans -> inspector_policies (policy_id)`;
`inspector_scans -> organizations (org_id)`;
`inspector_findings -> inspector_scans (scan_id)`;
`inspector_findings -> apps (app_id)`;
`inspector_mask_actions -> organizations (org_id)`;
`inspector_mask_actions -> apps (app_id)`;
`inspector_masked_keys -> apps (app_id)`;
`inspector_reveal_audit -> apps (app_id)`. All six names go into
`allow_tables_to_appear_in_same_query!`.

`models.rs` gains a `Queryable, Selectable, Serialize` row struct and a
`New…<'a>` `Insertable` per table. **Never** add `Queryable` to an `Insertable`
struct: `Insertable` maps by name, `Queryable` decodes positionally, so the
field order that is harmless today silently binds each field to whatever column
occupies its index.

Never run the diesel CLI. `backend/diesel.toml` deliberately omits the `file =`
key; adding one makes `diesel migration run` rewrite `schema.rs` to include
every tier-created partition child and redeclare `error_events`' primary key,
and all of it compiles.

---

## 3. The table allowlist, and why it is an allowlist

`backend/crates/sauron-inspector/src/columns.rs` owns a
`&'static [ScanColumn { table, column, kind: Jsonb | Text, default_on,
maskable, reveal_ok, cost_class }]` inventory, hand-verified against `\d+`, and
**not** derived from the Diesel models. That is precisely why
`error_events.title`/`culprit` are in it despite being absent from
`ErrorEvent::as_select()` — a model-walking scanner misses them, and they are
what the Issues list actually renders.

The table set is closed:

| Table | Partitioned | Scannable columns (default set in bold) | Maskable |
|---|---|---|---|
| `error_events` | yes | **tags, contexts, extra, context, event_user**, breadcrumbs, sdk, debug_meta, stacktrace, stacktrace_symbolicated, **message, exception_value, exception_type, title, culprit** | yes, except the two stacktrace columns |
| `analytics_events` | yes | **properties, tags, contexts, extra, context** | yes |
| `transactions` | **yes** | **url** | yes |
| `issues` | no | **title, culprit** | yes (with the sticky guard, §10) |
| `event_users` | no | **properties** | forward enforcement only |
| `sessions` | no | **context** | yes (context only) |
| `identities` | no | alias_id, distinct_id | no |
| `workflows` | no | cancel_reason | no |
| `devices` | no | — | **no** |

Six of those nine are maskable — `error_events`, `analytics_events`,
`transactions`, `issues`, `event_users`, `sessions` — and that set, exactly, is
the `inspector_masked_keys.target_table` CHECK (§2). `devices`, `identities` and
`workflows` are **scan-only**: reachable by a scan so an admin can see the
exposure, never offered as a mask target, each for its own reason below.
Note that `breadcrumbs` is a jsonb column on `error_events`, not a table of its
own; it is masked as `error_events.breadcrumbs[*].…`.

Nothing else. Not `users`, `auth_sessions`, `refresh_tokens`,
`password_reset_tokens`, `mail_outbox`, `roles`, `role_grants`,
`notification_channels`, `alert_events`. S2 asserts this constraint on this
slice and it is honoured as an allowlist, not a denylist, because a denylist
silently fails to protect the next account table someone adds. A unit test
asserts the allowlist contains no table name matching
`users|session|token|role|grant|secret|mail|channel` outside the telemetry set.

`transactions` moved out of the "rollup" class both halves put it in.
`2026-07-14-000013_transactions_partitioned` declares
`PRIMARY KEY (id, occurred_at) … PARTITION BY RANGE (occurred_at)` with a
`transactions_default` child, and `sauron-tier` lists it in `TIERED_TABLES`.
Treating it as a rollup would have meant no `occurred_at` predicate (so no
pruning — the exact behaviour the unit decomposition exists to prevent), an
`id > $2` keyset over a column that is not unique across partitions and has no
global index, and a `_default` sweep that double-scans the same rows.

`devices` is not maskable at all. `upsert_device`'s `DO UPDATE` is
`family = COALESCE(EXCLUDED.family, devices.family)` and the same for
model/os_name/os_version/browser/last_distinct_id — a non-null incoming value
always wins, and the values are derived server-side by `enrich`, so there is no
wire field for the enforcer to touch. A mask there would retro-succeed and be
overwritten by the next event from that device, permanently, with a green
"done" badge. Offering it would be a lie; it is scannable so an admin can see
the exposure, and the Findings row for it carries "not maskable — see the
Devices note".

`workflows` is scan-only for the same shape of reason. `cancel_reason` is
derived server-side in `process.rs` from `properties["reason"]` on a
`$workflow_cancel` analytics event, so there is no wire field for the enforcer
to mask, and `apply_workflow_lifecycle`'s
`CASE WHEN workflows.status = 'active' THEN EXCLUDED.cancel_reason` lets a
later cancellation event write the raw string back over the sentinel. The
finding tells the admin to mask `analytics_events.properties` instead — that is
where the bytes actually arrive, and masking there stops the derivation at
source.

`identities` is scan-only because its two columns *are* the identity graph.
`alias_id` and `distinct_id` are join keys; collapsing them to `'****'` does not
redact a person, it merges every masked person into one, silently and
irreversibly corrupting every downstream identity resolution. The exposure is
worth reporting; the remedy is on the SDK side, not here.

Within `sessions`, `distinct_id` and `ip_address` are excluded for the `devices`
reason — `bump_session` writes both as `COALESCE(EXCLUDED.x, sessions.x)`, so a
non-null incoming value always wins. `sessions.context` is included because the
same statement writes the post-enrichment snapshot whole, so masking the
enriched `context` reaches it on every subsequent event (§10).

---

## 4. The policy model and key matching

### Precedence

Most specific wins, **whole row, no merging**: `app_env` > `app` > `project`.
`UNIQUE (target_type, target_id)` means one policy per node.

A union of tracked keys across levels makes "turn this off for staging"
inexpressible — a narrow row could only add, never subtract — and the schedule
would have to be merged too, which is meaningless.

**Target resolution must actually subtract.** S5a documented "a narrow policy
row with `enabled=false` is the way to exclude one noisy environment" and then
implemented nothing that does it: `claim_due_policies` filters
`WHERE enabled AND schedule_enabled`, which only stops the narrow row from
running its own scan. The parent project policy would still walk the excluded
environment and persist its key paths for 90 days, while the UI showed it as
excluded. So `resolve_targets` is a pure function that:

1. expands the policy's node into `[(app_id, app_env_id|null)]` pairs,
2. loads every `inspector_policies` row whose target falls **under** that node,
3. drops any pair covered by a more-specific row, **enabled or not** — "most
   specific wins, whole row" applies to exclusion as well as configuration,
4. returns the count of subtracted pairs, which goes into `coverage_note`.

It is pure, lives in `sauron-inspector`, and has a unit test per case.

`validate_scope_in_org(conn, org_id, target_type, target_id)` runs on **every**
policy create and PATCH, and again in the worker when the scan is claimed.
`target_id` has no FK, so without it any authenticated user can mint an org
where they hold `org:manage` (`POST /v1/orgs` requires only `AuthUser`), POST a
policy naming a victim's `app_id`, and have the worker scan the victim's
`error_events` into rows carrying the attacker's `org_id` — which is exactly
what list queries filter on. Re-validating at claim time matters because grants
outlive targets.

### Key matching

A tracked key is a literal **name**, lowercased at write, matched
case-insensitively and **exactly** against the leaf key name at any depth
(`scope: 'top'` restricts it to the top level of the column). `Email`, `EMAIL`
and `email` all match `email`; `user_email` and `emails` do **not**.

Case-insensitive because SDK payloads mix the three freely. Exact rather than
substring because substring matching over 15 keys per row across millions of
rows is a cross product that produces findings nobody asked for, and it would
force a per-key `OR` instead of one bound `text[]`.

Dotted paths are wrong as *input* — the admin does not know the SDK nested it
under `contexts.order` — and right as *output*: a finding reports the full
dotted path where the key was actually found, which is exactly what the masker
consumes.

Tag keys are unvalidated free-form UTF-8 on the write path by design
(`tag:<key>=<value>` is the documented escape hatch for non-identifier keys), so
the matcher and the UI must accept keys containing `.`, spaces and `=`.

### Detectors

A fixed, closed preset library of value-shape detectors — `email`, `phone_e164`,
`ipv4`, `ipv6`, `jwt`, `iban`, `ssn_us`, `credit_card` — all hand-rolled byte
scanners plus Luhn for `credit_card`. No `regex` crate: it is only a transitive
dependency today (via `validator`, `woothee`, `arrow-string`), so declaring it
is a workspace edit, and admin-authored patterns would mean accepting ReDoS
authored by an org admin against a shared worker.

Detectors are opt-in per policy, default empty, and get their own shorter window
(`inspector_detector_window_days`, 7) because they change the cost model by an
order of magnitude — see §5.

---

## 5. The scan engine, with numbers

### Two phases

**Phase 1, SQL.** A keyset-paginated window over the index, then the prefilter:

```sql
WITH win AS (
  SELECT id, occurred_at FROM error_events
  WHERE app_id = $1 AND environment_id = $2
    AND occurred_at >= $3 AND occurred_at < $4
    AND (occurred_at, id) > ($5, $6)
  ORDER BY occurred_at, id
  LIMIT $7                              -- inspector_batch_rows
)
SELECT e.id, e.occurred_at, e.tags, e.contexts, e.extra, e.context, e.event_user
FROM error_events e JOIN win ON e.id = win.id AND e.occurred_at = win.occurred_at
WHERE e.occurred_at >= $3 AND e.occurred_at < $4
  AND (e.tags::text ILIKE ANY($8) OR e.contexts::text ILIKE ANY($8) OR …);
```

The inner window and the day range on the outer statement both matter. S5a put
the `LIMIT` on the same statement as the `ILIKE`, which bounds **matches, not
scanned rows** — and the design's own premise is that the prefilter eliminates
95-99% of rows, so that statement must scan the *entire* app-day range to emit
fewer than 5000 rows. Three consequences, all bad: no heartbeat and no
inter-batch pause for the whole scan (so the claimed duty cycle was fiction);
`inspector_statement_timeout_ms` aborts somewhere around 2-3M rows per app-day;
and on abort **the cursor never advances**, so the retry replays the identical
statement and `inspector_max_attempts` permanently fails the scan. With the
window on the index, the cursor advances by exactly $7 scanned rows every time,
the pause is real, and the timeout is a ceiling rather than a livelock.

The `(app_id, environment_id, occurred_at)` predicate matches
`error_events_app_env_time_idx` / `analytics_events_app_env_time_idx` exactly.
Column names are `&'static str`s from the inventory formatted into the SQL with
the house comment stating they are internal identifiers; every value is bound.

Patterns are built in Rust as `like_contains(format!("\"{key}\""))`. `escape_like`
is **private** in `repo.rs` and the new crate is pure with no DB dependency, so
`sauron-inspector::prefilter` re-implements the three-character escape with its
own unit tests rather than citing a function it cannot call.

**When `detectors` is non-empty the ILIKE predicate is omitted entirely** and
every row in the (shorter) detector window is walked. That is what makes a
detector-only policy work at all.

**Phase 2, Rust.** Only surviving rows are parsed. `walk()` yields `(path,
value)` pairs with a depth cap of 6; array elements collapse to a single `[]`
path segment; it tolerates a `contexts` block that is the scalar string
`"[Circular]"` (real live data) and a non-object root. Results accumulate into a
`HashMap` keyed by `(column, path, matched_key, detector)` whose cardinality is
bounded by keys × columns (~50 × 11 = 550 entries) because a unit is a single
`(app, env, table, day)`. Worker RSS is therefore flat regardless of scan size,
and that is the reason units are decomposed this finely.

### Cost

Measured inputs, all from this codebase: `extra::text ILIKE` over 210,146 rows /
678 MB is **184 ms** as a `Parallel Append` with 2 workers, i.e. ~0.9 µs/row over
a 317-byte column (~2.8 ns/byte). Row width is 2742 bytes; the default scan
column set is ~1694 bytes/row (tags 52, contexts 336, extra 317, context 447,
event_user 174, breadcrumbs 368). `stacktrace` alone is another 623 bytes and is
opt-in.

| | Key mode | Detector mode |
|---|---|---|
| Phase-1 SQL per row | ~5 µs (1694 bytes at 2.8 ns/byte, one day partition, no parallel workers on a small child) | 0 — no prefilter |
| Rows reaching phase 2 | 1-5% | 100% |
| Phase-2 walk per parsed row | ~10 µs | ~200 µs (8 detectors over every string leaf) |
| Bytes shipped out of Postgres per row | ~85 bytes amortised | ~1700 bytes |
| **1M rows** | ~5 s scan + 0.5 s walk; ~45 s wall clock at the default duty cycle | ~200 s CPU + 1.7 GB transfer; ~25 min wall clock |
| **30M rows** | ~25 min wall clock | ~100 min CPU + 51 GB transfer; **~8 hours** wall clock |

Read those totals against the hot window, because the run-time clamp on
`window_days` is `TIER_HOT_DAYS` and nothing older is reachable anyway. On a
stock deployment that is 30 days, which coincides with the per-policy default, so
"scan everything still in Postgres" and "scan the last 30 days" are the same
scan. An operator who raises `TIER_HOT_DAYS` to keep more data queryable raises
this ceiling by the same factor and must scale the row counts in the table above
to match — in detector mode that is the difference between a scan that finishes
overnight and one still running at noon.

That ratio is the entire justification for detectors being a separate, opt-in,
separately-windowed field. The policy form shows an estimated row count (from
`repo::hot_rows_by_app_scoped`, served from the Storage page's existing 60 s
Redis cache — never a fresh count on the API pool) before saving, and refuses to
save a project-scoped policy above a target-count threshold.

Per-unit phase-2 work is capped at `inspector_max_phase2_rows_per_unit`
(200,000); hitting it sets `match_count_exact = false` on that unit's findings
and `coverage = 'partial'`.

### Units, and surviving a SIGKILL

A unit is one `(app_id, app_env_id|NULL, table, day)` for partitioned tables,
`(app_id, table)` for rollups, and `(app_id, table)` for `_default` sweeps. The
unit **list** is deterministically recomputable from the frozen `window_from`/
`window_to`, `params` and `targets`, so only `{unit_index, row_cursor}` is
persisted. A separate `inspector_scan_units` table would be ~13,500 bookkeeping
rows for a 50-app project across the 30-day hot window, times 20 retained scans,
and would still need the same freeze.

Freezing is what makes recomputation safe: an admin editing the policy mid-scan
would otherwise silently change what unit #37 means, and a resume would walk a
different list. Units are ordered newest-day-first so a scan killed halfway has
already covered the most recent data.

Each flush is **one data-modifying CTE** — there is no `conn.transaction` in this
repo (MSRV 1.82):

```sql
WITH me AS (SELECT id FROM inspector_scans WHERE id = $1 AND worker_id = $2),
f AS (
  INSERT INTO inspector_findings (…) SELECT … FROM unnest(…)
  WHERE EXISTS (SELECT 1 FROM me)
  ON CONFLICT (scan_id, app_id, env_scope,
               COALESCE(environment_id,'00000000-0000-0000-0000-000000000000'::uuid),
               source_table, source_column, key_path, detector)
  DO UPDATE SET match_count = inspector_findings.match_count + excluded.match_count,
                last_seen_at = GREATEST(inspector_findings.last_seen_at, excluded.last_seen_at),
                match_count_exact = inspector_findings.match_count_exact AND excluded.match_count_exact
  RETURNING (xmax = 0) AS inserted
)
UPDATE inspector_scans SET
  cursor = $3, units_done = $4,
  rows_scanned = rows_scanned + $5,
  findings_count = findings_count + (SELECT count(*) FROM f WHERE inserted),
  heartbeat_at = now()
WHERE id = $1 AND worker_id = $2
RETURNING cancel_requested_at;
```

Three properties are load-bearing.

**Atomicity.** The deltas and the cursor advance in one commit, so a SIGKILL
between them is impossible and re-running the lost range re-adds exact counts
from the last durable cursor. Counts stay correct without needing
`GREATEST`-style deduplication — which would be correct across re-runs but
*wrong* across units, which must sum.

**The `worker_id` fence.** A worker stalled past the lease (GC, IO) can have its
scan reclaimed while still alive, and `match_count + excluded.match_count` would
then double-count. A flush that affects zero rows **must abort the unit**. Any
refactor that drops the fence silently corrupts counts.

**`findings_count` reads the CTE, not the table.** S5a wrote
`findings_count = (SELECT count(*) FROM inspector_findings WHERE scan_id = $1)`.
Postgres executes all sub-statements of a data-modifying `WITH` against one
snapshot and documents that they cannot see one another's effects, so that
subquery counts the table as of *before* `f` ran: the counter is permanently one
flush behind, the final flush's findings are never counted, and a single-unit
scan reports 0 while `GET /findings` returns rows. It is also an aggregate over
the whole finding set on every flush — hundreds of millions of index tuples read
over a scan, on the connection that is supposed to be duty-cycled. Reading
`f`'s own `RETURNING` is snapshot-correct and O(flush).

### The `_default` partitions

`repo::list_child_partitions` excludes `{table}_default` by design
(`c.relname <> ($1 || '_default')`), so those rows are **never tiered and never
dropped** — they are the longest-lived PII in the system. A time-windowed scan
prunes them away precisely because their `occurred_at` is outside every explicit
range. Sizing matters: migration 000011 ran
`INSERT INTO error_events SELECT * FROM error_events_old` while
`error_events_default` was the only partition, so on any deployment that had
real data at upgrade time the default child holds the entire pre-migration
table. The dev box's 6 rows are not representative.

So each scan gets one extra unit per `(table, app)` that queries the child **by
name** (derived internally from `sauron_tier::TIERED_TABLES`, never from input)
with the same index-bounded keyset window as a ranged unit — `(occurred_at, id)`
cursor, ILIKE outside the `LIMIT`, cursor persisted so a later scan resumes
rather than restarts. Truncation at `inspector_default_sweep_rows` sets
`coverage='partial'` with a note. Findings carry `partition_kind='default'`.

Rollup units (`issues`, `event_users` by default; `sessions`, `identities`,
`workflows` opt-in) use PK keyset pagination and carry `partition_kind='rollup'`,
which the UI renders as "recurring — an at-rest mask will be undone by the next
event".

**Rollup and `_default` units are skipped entirely for `app_env`-scoped
policies**, and the skip is recorded in `coverage_note`. Neither class can be
environment-attributed — `event_users` and `issues` carry `app_id` only — so
running them would mean a policy an admin deliberately scoped to staging
persisting key paths derived from production traffic, readable by anyone with
`pii:read` on staging.

Rows with `environment_id IS NULL` are only reachable from an `app`- or
`project`-scoped policy, because `EnvFilter::Subset` uses `= ANY` which never
matches NULL. If a deployment runs mostly `app_env` policies those rows are
silently unscanned; the effective-policy endpoint surfaces this.

Cold Parquet is out of reach. Every scan whose `window_from` predates
`now() - tier_hot_days` records `coverage='partial'` naming the tiering
boundary, and the Findings tab carries the standing note from §1.

---

## 6. The findings table is a PII store — here is how it is bounded

This is the central constraint of the slice. An admin does not need the value to
decide; they need to know that a key called `email` appears in
`error_events.extra.customer.email` in 41,200 rows of prod, first seen 12 days
ago. The preview and the type confirm it is really an email and not an enum.

- **No raw value column.** Ever.
- **No hash column.** A SHA-256 of an email is a stable pseudonymous identifier
  of a person and is trivially brute-forced for low-entropy values. It is not in
  the schema at all, deliberately.
- **`sample_preview`** is shape-only, capped at 64 chars, char-boundary safe,
  never echoes more than the first and last codepoint, and renders numbers and
  booleans without leaking magnitude. A property test asserts
  `!preview.contains(raw)` over a corpus.
- **`key_path` is untrusted input and must be redacted too.** Both halves missed
  this. `ErrorItem.tags/contexts/extra` are `serde_json::Value` — object *keys*
  are arbitrary dev-controlled UTF-8 of unbounded length. A payload shaped
  `extra.customers["jane@acme.com"].email` or `extra.ssn_123-45-6789.value`
  writes raw PII straight into `key_path`, unredacted, rendered in the UI,
  emitted into the CSV, and reachable by every `pii:read` holder with no reveal
  call. So: each path segment is capped at 64 chars and the whole path at 512;
  any segment that trips a detector or exceeds a shape budget is replaced with
  the literal `<key>`; the same property test (`!key_path.contains(raw)`) applies;
  and the CSV formula-injection guard covers `key_path`, not just free text.
- **The locator** (`source_table`, `sample_row_id`, `sample_occurred_at`) is what
  the reveal endpoint re-reads. `sample_occurred_at` is mandatory for partitioned
  sources so the reveal query prunes to one child.
- **Retention is doubled** because one bound is not enough (§8).

### Reveal

`POST /v1/inspector/findings/{id}/reveal` is the only place a raw value is ever
produced. It loads the finding, authorizes `pii:read` at the finding's app,
**writes the `inspector_reveal_audit` row**, then does one live single-row read:

```sql
SELECT <column> FROM <table> WHERE id = $1 AND occurred_at = $2 AND app_id = $3
```

then extracts exactly the one `key_path` in Rust and returns `{path, value,
type}`. Nothing is persisted.

The `app_id` predicate is not redundant. Without it the tenant decision rests
entirely on `inspector_findings.app_id` being correct — a worker-written value
with no constraint tying it to the row `sample_row_id` points at. Any attribution
bug converts silently into cross-tenant raw-PII disclosure. It costs nothing;
`app_id` leads `error_events_app_env_time_idx`.

`stacktrace`, `stacktrace_symbolicated` and `debug_meta` are **not
reveal-eligible**, and `columns.rs` says why. `stacktrace_symbolicated` frames
carry `context_line`/`pre_context`/`post_context` — verbatim customer source —
which `strip_source_context` removes from responses only when the caller lacks
`perm::SOURCE_READ`. A `pii:read` holder without `source:read` could otherwise
add the tracked key `pre_context`, reveal, and receive de-obfuscated proprietary
source. They remain scannable (opt-in) so the exposure is visible; the redacted
preview is all that is returned.

410 Gone when the row is absent — its partition was dropped by `sauron-tier`, or
a rollup row was replaced. POST rather than GET so the identifier does not land
in access logs and so the audit row has a body to record.

---

## 7. Scheduling without a cron crate

There is no cron parser anywhere in the repo and no cron crate in `Cargo.lock`.
`monitors.next_check_at` plus a `SKIP LOCKED` claim is the only per-row cadence
precedent in the schema, and copying it gives multi-instance safety for free.

`schedule_days` is a 7-bit mask, trivially testable in SQL with
`(days >> dow) & 1`, and maps 1:1 to a row of checkboxes. `NEXT_RUN_SQL` is one
`&'static str` fragment shared by `claim_due_policies` and `reschedule_policy`:

```sql
(SELECT min(ts) FROM (
   SELECT ((date_trunc('day', now() AT TIME ZONE p.schedule_tz)
            + (d || ' day')::interval + p.schedule_time)
           AT TIME ZONE p.schedule_tz) AS ts
   FROM generate_series(0, 8) d) c
 WHERE ((p.schedule_days >> EXTRACT(DOW FROM (c.ts AT TIME ZONE p.schedule_tz))::int) & 1) = 1
   AND c.ts > now())
```

Eight days always covers a once-a-week schedule. **The update target must be
aliased** — `UPDATE inspector_policies AS p SET next_run_at = …` — because the
fragment references `p.*` and the pattern it copies (`claim_due_monitors`)
aliases nothing. The inner sub-select gets its own alias too so the two scopes
cannot collide:

```sql
UPDATE inspector_policies AS p
SET next_run_at = <NEXT_RUN_SQL>, last_run_at = now()
WHERE p.id IN (
  SELECT q.id FROM inspector_policies q
  WHERE q.enabled AND q.schedule_enabled AND q.schedule_days <> 0
    AND q.next_run_at IS NOT NULL AND q.next_run_at <= now()
  ORDER BY q.next_run_at FOR UPDATE SKIP LOCKED LIMIT $1)
RETURNING p.*;
```

The claim **always** advances `next_run_at`, so a row can never get stuck
permanently due; the worker then decides whether to actually start a scan.
`reschedule_policy` is called after every schedule-field write so `next_run_at`
is never stale.

All timezone arithmetic is Postgres's because `chrono-tz` is not a workspace
dependency — Rust cannot resolve `Europe/Paris` at all, and adding it is a
workspace edit plus ~1 MB of tz data in every binary.

**DST, stated rather than discovered.** Candidates are built as local timestamps
and converted back with `AT TIME ZONE`, so Postgres resolves DST. On
spring-forward a 02:30 schedule resolves to a valid instant (effectively 03:30
local); on fall-back it resolves to the first occurrence, so it runs once, not
twice. Never zero runs, never double runs. The UI defaults to 03:00 and warns
for times in 00:00-03:59. These are the only two behaviours available without a
policy layer, and writing them down beats discovering them during a November
incident.

**Catch-up.** Fire once on recovery, never replay missed runs, and skip even
that one if it is more than `inspector_catchup_grace_hours` (6) stale, recording
`last_skip_reason`. A scan is a snapshot over a window, not an event stream;
three replayed runs produce three near-identical finding sets at 3× the load.
And a 03:00 scan firing at 09:00 on a Monday is precisely the production load
spike the schedule existed to avoid.

**The staleness skip must never be caused by the worker's own backlog.** S5a's
tick was "(1) claim due policies → enqueue, (2) claim one scan → run it to
completion", inside the house single-task loop. The design itself admits a
project-scoped policy "would run for days", during which step (1) never
executes — and when the worker finally returns, everything queued behind it is
more than 6 hours stale and gets skipped. Enabling one large policy would
silently disable scheduling for every other policy, with the only signal buried
in a column. §8 splits the loops so this cannot happen.

---

## 8. The worker: `backend/bins/sauron-inspector`

Package name `sauron-inspector-bin` with `[[bin]] name = "sauron-inspector"`,
because the library crate owns the plain name — the exact `sauron-alerts-bin` /
`sauron-tier-bin` precedent. One binary per `bins/` directory; never add a
`[[bin]]` to an existing bin package, because `binaries.txt`, CI and the spec
would never see it.

`main()` is the house shape: `sauron_telemetry::init("sauron-inspector")` first,
then `Config::from_env()?`, then `sauron_db::build_pool(&cfg.database_url, 4)`.
No Redis. **No DuckDB** — deliberately, so it does not inherit the unbundled
`libduckdb` constraint across four build paths. If `!cfg.inspector_enabled` it
logs one info line and sleeps forever.

### Four loops, one pool

This is the reconciliation of the starvation problem in §7 and the preview
starvation problem in §10. The process runs four `tokio::spawn`ed loops, each
with its own interval and each logging-and-swallowing its errors:

| Loop | Interval | Work |
|---|---|---|
| scheduler | `inspector_tick_secs` (30) | `claim_due_policies` → enqueue scans. Never blocked by execution |
| scan executor | 1 s | `claim_one_scan` → run **one unit**, flush, yield. Re-enters the loop between units |
| mask executor | 1 s | `claim_mask_action(kind='mask')` → run one batch, yield |
| preview executor | 1 s | `claim_mask_action(kind='preview')` → one counting batch, yield |
| reaper | hourly | Retention, on its own cadence, never inside a scan tick |

A separate claim slot for previews is what makes the 15-minute preview TTL
achievable. S5b routed previews through the same single-slot FIFO as masks, so a
preview requested while a multi-hour mask ran would expire before it was ever
computed and confirm would become permanently impossible on a busy app.

The scan executor runs one unit per iteration rather than a whole scan, so the
tick is short and the lease heartbeat is frequent. Between units it re-reads the
clock; a scan that has held its lease for a full `inspector_lease_secs` without
finishing is a bug, not a design.

**All four loops share the single 4-connection pool.** S5b specified a second
`build_pool(url, 4)` for the mask module. Today's peak pooled demand is
sauron-api 16 + sauron-ingest 8 + sauron-alerts 8 + sauron-tier 4 +
sauron-monitor (`monitor_max_concurrency` 50 + 8) = 94, against `postgres:16`
with no tuning — the default `max_connections` of 100 with 3 reserved for
superusers. Two more pools push the shipped deployment over the edge, and
connection exhaustion surfaces as API 500s and ingest 202-then-drop, not as an
inspector error. So: one pool, and `docker-compose.yml` gains
`command: postgres -c max_connections=200` on the `postgres` service with a
comment naming the budget, and `packaging/rpm/SETUP.md` states
`max_connections >= 150` as a prerequisite.

`claim_one_scan` copies `claim_due_monitors` verbatim in shape:

```sql
UPDATE inspector_scans SET status='running', worker_id=$1, heartbeat_at=now(),
       attempts=attempts+1, started_at=COALESCE(started_at, now())
WHERE id IN (SELECT id FROM inspector_scans
             WHERE status='queued'
                OR (status='running' AND heartbeat_at < now() - make_interval(secs => $2))
             ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1)
RETURNING *;
```

This is what makes N replicas safe — unlike `sauron-alerts` (no claim) and
`sauron-tier` (a watermark row with no locking). `attempts >
inspector_max_attempts` finalizes the scan as `failed` so one poison unit cannot
loop forever.

### Statement timeout

The inspector sets `SET statement_timeout = <inspector_statement_timeout_ms>`
immediately after checkout and `RESET statement_timeout` before `drop(conn)`.
deadpool's recycle does **not** reset session state, so a leaked `SET` silently
poisons a later checkout in the same process. This must be a single wrapper
helper, never an ad-hoc `SET` at the call site.

### Protecting the ingest write path

The risk is not lock contention — a seq scan takes `ACCESS SHARE`, which does not
conflict with `INSERT`'s `ROW EXCLUSIVE`. It is buffer-cache eviction and CPU.
Mitigations, in the order an operator reaches for them: the whole feature is off
by default (`INSPECTOR_ENABLED=false`); work proceeds in
`inspector_batch_rows` (5000) chunks with an `inspector_batch_pause_ms` (200)
sleep between them; each unit is a single `(app, env, day)` so at most one day
partition's pages are hot at a time — walking one ~30 MB child rather than the
678 MB parent is what keeps the ingest working set resident; and the per-
connection statement timeout kills any pathological batch.

Sampling was rejected: there is no `TABLESAMPLE` and no `random()` anywhere in
`repo.rs`, and a sampled scan cannot honestly report "this key does not appear".

### Retention

Two independent bounds, because one is not enough. A nightly scan producing 33k
findings is 12M rows a year — the exact failure `alert_events`' reaper doc
comment warns about.

- `prune_inspector_scans` keeps the newest `inspector_scan_keep` (20) scans per
  policy. It deletes each scan's findings in **bounded batches**
  (`DELETE … WHERE ctid IN (SELECT ctid … LIMIT n)`) **before** dropping the
  parent row. The house prune idiom has no `LIMIT`, and an unbounded cascading
  `DELETE` of up to 660k findings is a bloat and lock spike.
- `prune_inspector_findings` runs the house idiom
  `DELETE FROM inspector_findings WHERE created_at < now() - ($1 || ' days')::interval`
  with `inspector_finding_retention_days` (90), also batched, and **stamps the
  owning scan `findings_reaped_at`** so a scan row's `findings_count` and its
  empty finding list never silently disagree.
- `prune_mask_previews` always runs (`inspector_preview_gc_days`, 7) —
  abandoned previews are not audit-relevant.
- `prune_mask_actions` runs only when `inspector_audit_retention_days > 0`, and
  only over terminal states. It defaults to **0 = never prune**: this table
  grows per *human action*, not per rule evaluation, and it is the record a
  compliance question is answered from.
- `pseudonymize_mask_actions` nulls `requested_by_email`, `cancelled_by_email`
  and `confirm_source` on rows older than `inspector_audit_pii_days` (730),
  keeping counts and targets. Without it, the privacy feature is the only
  un-erasable store of staff PII in the schema: everywhere else a user row
  cascades (`refresh_tokens`, `role_grants`), so deleting a user is the
  product's de-facto erasure mechanism, and `ON DELETE SET NULL` plus a
  denormalized email breaks it by design. The user-agent portion of
  `confirm_source` is truncated to 120 chars at write.

---

## 9. The retro-mask job

A `mask` module in the same binary. Each `inspector_mask_actions` row is
simultaneously the queue, the cursor, the progress meter and the audit record.

### Claim

```sql
UPDATE inspector_mask_actions SET status='running', claimed_at=now(), worker_id=$1,
       started_at=coalesce(started_at, now())
WHERE id IN (SELECT id FROM inspector_mask_actions
             WHERE kind='mask'
               AND (status='pending'
                    OR (status IN ('running','cancelling') AND claimed_at < now() - $2))
             ORDER BY requested_at FOR UPDATE SKIP LOCKED LIMIT 1)
RETURNING *;
```

`LIMIT 1` is deliberate — masking is heavy write and one action at a time per
worker is the throttle; N workers take N different actions. Re-claiming a stale
row is the crash-resume mechanism.

**Authorization is re-checked at claim.** The worker loads `requested_by` and
re-evaluates `authorize_app(requested_by, app_id, perm::PII_MANAGE)` plus
`users.is_active`; on failure the action moves to `failed` with an explicit
reason. Confirm re-authorizes, but the action then sits in `pending` — with one
slot per worker and a 200 ms inter-batch pause, a backlog can be hours deep. A
member whose grant was revoked, or whose account was deactivated (which revokes
refresh tokens and touches nothing queued), must not have their queued
destruction execute. `set_member_active`'s deactivation path additionally
cancels that user's pending actions.

### Batching and partition pruning

The job iterates **day by day** over `[now() - tier_hot_days, now())` — 30
iterations at the shipped default — so every statement's `occurred_at` range
maps to exactly one child and the lock scope is one child table. Within a day it
keyset-paginates on `(occurred_at, id)` with
`LIMIT inspector_mask_batch` (2000, halved automatically when any target carries
a wildcard). Each batch is one data-modifying CTE:

```sql
WITH sel AS (
  SELECT id, occurred_at FROM error_events
  WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3
    AND (occurred_at, id) > ($4, $5)
    AND extra #> $6 IS NOT NULL
  ORDER BY occurred_at, id LIMIT $7),
upd AS (
  UPDATE error_events e
  SET extra = jsonb_set(coalesce(e.extra, '{}'::jsonb), $6, '"****"'::jsonb, false)
  FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at
    AND e.occurred_at >= $2 AND e.occurred_at < $3      -- bound params, for pruning
  RETURNING 1)
UPDATE inspector_mask_actions SET cursor_occurred_at=…, cursor_id=…,
  rows_masked = rows_masked + (SELECT count(*) FROM upd),
  rows_scanned = rows_scanned + (SELECT count(*) FROM sel)
WHERE id = $8 AND worker_id = $9
RETURNING status;
```

The day window appears **twice on purpose**. S5b claimed joining `sel` on
`(id, occurred_at)` reproduces `update_event_symbolication`'s pruning shape. It
does not: that function compares `occurred_at` to a **bound scalar parameter**,
which is eligible for runtime pruning; comparing it to a CTE column gives the
planner no pruning key and it plans one `Update` node per child. The design's
own regression test — "`EXPLAIN` the batch `UPDATE` and assert exactly one
`Update on error_events_<child>`, not 22" — would have failed, and the entire
cost model behind the 2000-row/200 ms throttle with it.

The cursor and both counters advance in the same commit as the data change, so a
SIGKILL loses at most one batch and can never double-count. The `RETURNING
status` is how cancellation is observed on a write the worker was making anyway.

### The hot/cold boundary, and the race with `sauron-tier`

`lo_bound = Utc::now() - Duration::days(cfg.tier_hot_days)`, reusing
`symbolicate_with`'s expression and its comment "never write into cold/exported
partitions". An exported partition already holds the raw bytes in immutable
Parquet, so masking the Postgres copy buys nothing while paying the full write
cost, and a partition that is exported-but-not-yet-dropped is on the tier
worker's critical path.

`tier_hot_days` ships as **30**. It is the single number that most changes what a
mask costs and what it can still reach — it sets how many day partitions a full
pass grinds through *and* how long a row stays reachable at all — so an operator
who raises it to keep more data queryable pays on both sides at once. That is
also why it is read from one shared env file rather than declared per binary
(§15): three binaries deriving different boundaries from the same key is the
failure this section is guarding against.

A floor computed from that number is not sufficient on its own, however long the
window is. `sauron-tier` defers the drop to a **later**
cycle than the export — its own comment calls this "a real grace window" — and
the masker grinds oldest-day-first for potentially hours from a floor computed
once at job start. Two silent failures follow: the masker updates rows in a
partition already `COPY`'d to Parquet but not yet dropped, so Postgres shows
masked, Parquet holds raw, and the drop destroys the only masked copy; and a day
dropped mid-run matches zero rows while the action still reports `done` with
`rows_masked > 0`. So:

- The floor is **recomputed per day**, not once at job start.
- Before each day the worker re-reads `repo::get_watermark(table)` and refuses
  any day at or below it plus one `tier_tick_secs`, folding those rows into
  `cold_rows_skipped`.
- `cold_boundary_at` is re-recorded at finish, so the audit shows what execution
  actually skipped rather than what the preview predicted.

### The `_default` phase

After the day loop, `phase='default_partition'` runs the same predicate directly
against the child by name — an internal `&'static str` derived from
`TIERED_TABLES`, with the mandatory comment saying so.

S5b's stated rationale was wrong and the correction matters. It said Postgres
prunes `{table}_default` out of a range fully covered by explicit partitions, so
rows that landed there before their day partition existed are missed. Rows
*cannot* be in the default partition inside a covered range —
`create_range_partition` issues `CREATE TABLE … PARTITION OF …` and Postgres
rejects that outright if the default holds a conflicting row. Default rows have
`occurred_at` **outside** every explicit range (clock-skewed clients, offline
queues), which is why the phase is needed anyway. But it means the phase as
specified — "without an `occurred_at` range" — would happily rewrite rows years
older than `tier_hot_days`, contradicting the hot/cold rule and the
`cold_rows_skipped` number. **The default phase is bounded by the same
`>= now() - tier_hot_days` predicate**, and anything below it counts as cold.

### Companions and the tail sweep

`phase='companions'` runs one keyset loop per non-partitioned table filtered on
`app_id`, ordered by `(started_at, id)` for `sessions` and `(id)` for the rest —
same CTE shape minus the partition-key clause. No day loop; these are orders of
magnitude smaller than the event tables.

`phase='tail_sweep'` closes the enforcement race. Between "mask applied" and
"every pipeline replica's 30 s policy cache refreshes", new rows land unmasked,
and the retro-mask has already passed them. The sweep re-runs the seam once
after the main pass — **keyed on `received_at`, not `occurred_at`**:

```sql
WHERE app_id=$1 AND occurred_at >= $lo AND occurred_at < $hi   -- pruning
  AND received_at >= $run_started_at
```

`occurred_at` is the *client's* timestamp (`process.rs` sets
`occurred_at: ev.timestamp`). A mobile SDK offline queue or a skewed clock
flushes events whose `occurred_at` is days old; those rows land in a partition
the day loop already swept, far outside a 120 s `occurred_at` window, and would
never be revisited. Keeping an `occurred_at` range for pruning while filtering on
the server-set `received_at` gets both. `error_events.received_at` has no index,
which is why the `occurred_at` range must stay.

`inspector_tail_sweep_secs` (120) and `inspector_policy_cache_secs` (30) are
coupled: the sweep window must exceed the cache TTL or it closes nothing. The
worker clamps `tail_sweep = max(tail_sweep, 4 × cache_secs)` at startup with a
`warn!` — never a `bail!` in `Config::from_env`, since every binary shares the
struct.

### Lock, bloat, and the stop lever

Event tables are append-only, so mask `UPDATE`s never contend with ingest for
row locks. The shared cost is WAL, buffer cache and 13 index updates per
`error_events` row: **measured 186 µs/row on `extra`, 136 µs/row on `tags`**. A
2000-row batch is ~0.37 s of write; with the 200 ms pause that is a ~65% duty
cycle. A 210k-row full pass is ~60 s of write plus roughly a doubling of live
tuples until autovacuum catches up, and a pass covers the whole `TIER_HOT_DAYS`
window, so budget from the row count that window actually holds rather than from
a sample day. The job deliberately does **not** run
`VACUUM` — it sets `vacuum_advised` and emits a `warn!`, because an unattended
`VACUUM` is exactly the kind of surprise an operator should authorize. The
release note says to schedule a broad mask off-peak and to `VACUUM` after.

**A running mask can be stopped.** `POST …/cancel` on a `running` action moves it
to `cancelling`; the batch loop checks `status` on the `RETURNING` of every
batch and lands in terminal `cancelled` with the cursor and counters already
durable. S5b refused this on the grounds that stopping halfway "produces an
inconsistent result with no record of where it stopped" — but the cursor *is*
that record, committed with every batch by its own design. The alternative was
worse than inconsistent: cancel 409s, there is no `statement_timeout` anywhere in
the backend, nothing caps `estimated_rows`, and the crash-resume claim re-claims
a stale `running` row, so `systemctl restart sauron-inspector` resumes the grind.
An operator on a 200M-row app at 3am would have had two options: leave the unit
stopped (which also kills the scanner) or hand-write SQL.

Also: `confirm` refuses above `inspector_mask_max_rows` (default 20,000,000)
unless the ceiling is raised explicitly, and each batch carries a `SET LOCAL
statement_timeout`.

---

## 10. Mask semantics, preview, confirm, and re-introduction

### The sentinel

The value at the path becomes the JSON string `"****"` (`const MASK_SENTINEL`).
The **key is retained**: removing it changes row shape, breaks the `contexts`
named-block structure, and makes a `has:<key>` predicate report absence where
data existed — a second, subtler lie. Retaining the key with a visible sentinel
is self-documenting.

Consequences that must be in the spec, in the dialog, and in the wiki:

- **The type changes.** `extra.cart_value_cents: 4200` becomes `"****"`. Any
  consumer doing arithmetic, any `@>` containment filter, and any curated
  B-tree comparison against the old value stops working for masked rows.
- **Masking `event_user.email` breaks the shipped `user` search dimension.**
  `Store::JsonRoot` column `event_user` backs documented, tested queries like
  `user.email:*@acme.com`. Masked rows silently stop matching. The MaskDialog
  warns specifically when a target's column is `event_user`.
- **Masking an identity key stops future identification through it.** The
  enforcer runs ahead of the active-users pipeline, so a key an app sends as
  `context.user.id` arrives as `"****"` and the equality test that stamps
  `identified_at` never passes again. Note what this is *not*: `distinct_id` is
  never a mask target (§3 keeps it out of the allowlist precisely because it is
  a join key), and stamping is first-write-wins, so no existing person is
  un-identified. What happens instead is quieter and harder to catch — everyone
  first seen after the mask accumulates as a guest, and the identified share
  decays with nothing moving on the day the mask lands. This is the twelfth row
  of §1 and it is in `UNREACHABLE_COPY`, so the dialog states it before confirm
  is enabled — after the fact there is nothing left to recompute from.
- If the value at the path is an object or array, the whole subtree collapses to
  `"****"` — the subtree is the PII.
- For TEXT columns (`issues.title`/`culprit`, `error_events.title`/`culprit`/
  `message`/`exception_value`, `transactions.url`) the whole column value becomes
  `'****'`. No partial redaction: the workspace has no direct regex dependency
  and partial masking leaves recoverable residue.

### Path grammar

Dot-separated segments plus **at most one wildcard, legal only on the first
segment**: `breadcrumbs[*].data.email`, `stacktrace[*].abs_path`. A numeric array
index (`breadcrumbs.3.data.email`) is rejected at the API — an index is not
stable across rows, so a finding must never carry one.

Non-wildcard paths lower to
`jsonb_set(coalesce(col,'{}'), $path::text[], '"****"'::jsonb, false)`.
`create_missing=false` so a row lacking the path is untouched; the `coalesce` is
required because **`jsonb_set` returns NULL if any argument is NULL**, and a NULL
written into a `NOT NULL DEFAULT '{}'` column is the single most likely
implementation bug in this slice.

Wildcard paths lower to an array rebuild: `jsonb_agg` over
`jsonb_array_elements(col) WITH ORDINALITY`, per element
`CASE WHEN e #> $sub IS NOT NULL THEN jsonb_set(e, $sub, '"****"', false) ELSE e END`,
`ORDER BY ord` — ordinality is required because `jsonb_agg` order is not
guaranteed — wrapped in `coalesce(…, '[]')` because `jsonb_agg` over an empty
array also returns NULL. The rebuild re-serializes the whole array per row, so
it is measurably more expensive than the 186 µs/row `jsonb_set` case; the batch
size halves when any target carries a wildcard.

The path array is a bound `text[]`. `query_plan::nest_json_object` is
`pub(crate)` — either widen it or keep the path→`text[]` conversion local to the
mask module, but do not fork a second path encoder silently.

### Identifiers are enums, not strings

SQL identifiers cannot be bound, so the batch functions must interpolate
`target_table` and `target_column`. The worker reads `targets` back out of
Postgres in a **different process** from the one that validated it, so
"validated in Rust at write time" is not a control. `MaskTarget` therefore
deserializes into `TargetTable` / `TargetColumn` enums whose `as_sql()` returns
`&'static str`, and the batch functions take those enums, never `String`. The
worker re-validates at claim and `fail_mask_action`s on an unknown pair.
Anything that can write that JSONB column — this API, a future repo fn, a
migration — would otherwise be injection into an unattended `UPDATE` running
with full DB rights.

### Companion expansion

`expand_targets(finding)` is a pure fn applied at **preview** time and frozen
into `targets`. The map:

| Finding target | Also masks | Why |
|---|---|---|
| `error_events.title` / `culprit` | `issues.title` / `culprit`, **and the wire sources** `exception.value`, `exception.type`, `message` (title) and the stacktrace frame fields `build_culprit` reads | See below |
| `error_events.stacktrace[*].X` | `error_events.stacktrace_symbolicated[*].X` | The symbolicated copy holds the same frame data |
| `{error,analytics}_events.context.P` | `sessions.context.P` | `bump_session` snapshots the same enriched jsonb on every event |

Nothing else auto-expands.

The first row is a correction to S5b, which mapped `error_events.title` only to
`issues.title` and added a sticky guard there. That protects the wrong column:
`list_issues`/`get_issue`/`top_issues` render
`COALESCE(latest.title, i.title)` where `latest` is a `LEFT JOIN LATERAL` over
`error_events` ordered by `occurred_at DESC`. `error_events.title` is derived
server-side by `build_title(exc, message)` and has **no wire field**, so
`apply_wire` has nothing to mask for that target — the first event after the
mask writes a raw title into `error_events` and the Issues page shows the PII
again while the audit row reports success. Masking the inputs `build_title` and
`build_culprit` consume is what makes forward enforcement actually reach them.

### The three re-introduction paths

**`issues.title`/`culprit` — sticky guard.** `upsert_issue` gains
`title = CASE WHEN issues.title = '****' THEN issues.title ELSE excluded.title END`
and the same for `culprit`. One string compare on a write bounded by distinct
fingerprints, not by event volume. Forward enforcement alone leaves two gaps —
PII inside `exception_type`, which `build_title` also concatenates, and the 30 s
cache window — and both restore the raw string on the very next occurrence.

This guard is permanent: once a fingerprint's title is `'****'` it stays
`'****'` forever, even if every subsequent occurrence is benign. That is the
correct trade (a fingerprint is a stable error identity) but it is a visible
regression on the most-looked-at page in the product, and support will be asked
about it. It is in the wiki.

**`sessions.context` — sticks.** `bump_session` writes the post-enrichment
snapshot, so masking the enriched context reaches it on every subsequent event.
No guard needed.

**`event_users.properties` — does not stick, and we say so.**
`upsert_event_user` merges with `properties = event_users.properties ||
EXCLUDED.properties`, a whole-document merge with no cheap per-key guard. Any
`CASE`-style protection would live inside the hottest `identify()` path and
would silently drift from `inspector_masked_keys`. It is reachable through
forward enforcement only, and the Findings row, the MaskDialog and the wiki all
say that. Honesty is cheaper than a fragile guard.

### Preview

Counting `col #> path IS NOT NULL` over an app's hot window is a Parallel Append
seq scan — 184 ms per 210k rows measured — with no index that can serve it, since
the tags GIN is `jsonb_path_ops` and answers `@>` only. Running that on the
API's 16-connection pool is how the whole dashboard goes down.

So `POST /v1/apps/{app_id}/inspector/mask-preview` inserts a row with
`kind='preview'`, `status='preview'`, and returns `202` plus the id. The preview
executor claims it, runs the identical day loop with `count(*)` instead of
`UPDATE` (`phase='counting'`), fills `estimated_rows` / `cold_rows_skipped` /
`cold_boundary_at`, sets `previewed_at = now()` and `status='previewed'`. The
dashboard polls. The preview is auditable for free.

There is **no synchronous upper bound**. S5b proposed calling
`repo::hot_rows_by_app_scoped` from the handler for an instant "up to N rows"
figure — but that function is `SELECT app_id, count(*) FROM {table} WHERE app_id
= ANY($1) GROUP BY app_id` with **no time predicate**, counting every hot row the
app ever wrote across all ~20 children. Its only existing caller runs it on a
dedicated connection behind a 60 s Redis cache. Called uncached from every
MaskDialog open it holds a pooled connection for tens of seconds — the exact
pattern the design rejects two paragraphs earlier for the exact count. The
dialog shows "Counting…" until the worker answers.

### Confirm

`POST /v1/inspector/mask-actions/{id}/confirm { confirm_text }` promotes
`previewed → pending` only if all of: `status='previewed'`;
`now() - previewed_at < inspector_preview_ttl_secs` (900) — measured from the
preview *completing*, not from the request, or a queued preview expires before
it is readable; `confirm_text` equals the app's slug; `estimated_rows <=
inspector_mask_max_rows`; and a **fresh** `authorize_app(…, perm::PII_MANAGE)`.
`targets` is used as-is and cannot be supplied on confirm, so a confirm can
never widen what was counted and shown.

Typing the slug is the only confirmation that forces attention onto the thing
that actually goes wrong. The realistic failure is not a mis-click — it is
masking the wrong app, because the operator saw a finding and forgot which app
was selected. A typed literal like `MASK` proves intent and proves nothing about
scope; `ConfirmDialog` has no text input at all.

`confirm_source` records `client_addr(headers, peer, state)` plus a 120-char
user agent — see §12 for what that value is really worth.

Preview and execution are separated in time, so an actively-ingesting app will
have more matching rows at execution than the preview counted.
`rows_masked > estimated_rows` is normal and the UI must not render it as an
error.

---

## 11. Forward enforcement, and the `EnvRef` trap

### The choke point

`crates/sauron-pipeline`, not the ingest edge. Three reasons in order:

1. It is the only point that sees the server-derived enriched context — the
   `woothee` `ua` block and `device_key` — which `error_events.context` and
   `sessions.context` are both written from. The ingest edge physically cannot
   mask those.
2. It is off the HTTP handler. Note the pipeline workers run as tokio tasks
   **inside the `sauron-ingest` binary** via `spawn_workers`, sharing its
   8-connection pool, so "off the request path" means "off the handler", not "a
   different process".
3. It needs no policy read at DSN resolution, which is what lets `EnvRef` stay
   untouched.

### The `EnvRef` trap, avoided rather than worked around

`sauron-ingest` resolves a DSN key to
`EnvRef { env_id, app_id, project_id, org_id, env_ingest_enabled,
app_ingest_enabled }`, cached in Redis under
`keys::dsn_cache(key)` = `sauron:dsn:v2:{first-16-bytes-of-sha256(public_key)}`
with a 300 s TTL. Adding policy fields to `EnvRef` **requires bumping the
`v2` prefix to `v3`** — the `dsn_cache` doc comment states the version segment is
load-bearing precisely because entries written by the previous binary would
otherwise deserialize into the wrong struct, or fail and silently fall through
to Postgres, for the full TTL after every deploy. And a policy edit would take
up to 300 s to reach every ingest replica unless the API fanned out cache
deletions over `repo::live_app_environment_keys`.

Enforcing in the pipeline means none of that happens. `EnvRef` is not extended,
the prefix is not bumped, and there is no invalidation plumbing to get wrong.
The cost is that the raw value lives in the Redis stream for the `MAXLEN ~1e6`
window — which §1 names.

### The two application sites

An `Arc<PolicyCache>` is threaded through `spawn_workers → worker_loop →
process_entries → process_job` exactly as `SymbolizeCtx` already is.

In `process_entries`, immediately after `serde_json::from_str::<IngestJob>`
succeeds: resolve the app's `MaskSet` once, call
`mask::apply_wire(&set, &mut job)` — every payload field is `pub` and owned
(`ErrorItem.tags/contexts/extra/message/user/breadcrumbs/exception`,
`AnalyticsItem.properties/tags/contexts/extra`, `IdentifyItem.traits`) — then
`process_job`.

Inside `process_job`, right after `let context = enrich_context(&job);`,
`mask::apply_context(&set, &mut context)` applies only the targets whose column
is `context`. That is the enriched-only surface. Two functions, one module
(`crates/sauron-pipeline/src/mask.rs`), one policy lookup per job.

### The DLQ

`dead_letter` currently re-`XADD`s the original raw payload to
`sauron:ingest:dlq`, which has no `MAXLEN` and no TTL. The fix is to
dead-letter the **masked** job. It cannot be written as
`dead_letter(&id, &serde_json::to_string(&job)?)`: at the call site `job` has
already been moved into `process_job(pool, redis, sym, job).await` before the
`Err` arm runs, and `process_entries` returns `()` so `?` is not usable. Capture
it immediately after `apply_wire`:

```rust
let masked_payload = serde_json::to_string(&job).unwrap_or_else(|_| payload.clone());
```

and dead-letter that local. A payload that fails to **deserialize** still
dead-letters raw — a small, permanent, named hole.

### Policy propagation, and failing stale

`PolicyCache` is an in-process `RwLock<HashMap<Uuid, (Arc<MaskSet>, Instant)>>`
with TTL `inspector_policy_cache_secs` (30), negative-cached. A miss issues one
`repo::masked_keys_for_app`. A mask takes effect on every pipeline replica
within about 30 seconds; the API returns the number so the UI can state it
literally.

On error the enforcer **fails stale, not open**: it serves the last successfully
loaded `MaskSet` past its TTL, and falls back to an empty set only when no
successful load has ever happened for that app. S5b specified fail-open on the
grounds that failing closed would drop telemetry — correct as far as it goes,
but the trigger set is much wider than the RPM-upgrade case it named: a pool
checkout timeout, a statement timeout, a failover, or a rolled-back migration
all silently disable masking deployment-wide with only a `warn!`. Because the
retro-mask is a one-shot job that ends at `done`, every row written during that
window stays raw **forever**. A five-minute Postgres blip must not permanently
defeat an irreversible redaction the operator was told had converged.

The error outcome is cached for the same TTL and the `warn!` is rate-limited to
once per app per TTL. Without that, an upgrade where migrations have not been
re-run means one failing query and one log line **per ingested event**, doubling
DB round-trips on the same 8 connections that accept traffic and flooding
journald at ingest rate.

`INSPECTOR_POLICY_CACHE_SECS` and `INSPECTOR_TAIL_SWEEP_SECS` therefore live in
`packaging/rpm/config/sauron.env`, not `inspector.env`. Every unit loads
`sauron.env` plus its own file; `sauron-ingest` and `sauron-api` never read
`inspector.env`, so the "30 seconds" the API reports to the UI would silently
diverge from what the enforcer uses. `TIER_HOT_DAYS` moves there for the same
reason — this slice takes it out of `tier.env` and declares it once in
`sauron.env` (§15), value unchanged at 30. `sauron-tier`, `sauron-inspector` and
`sauron-api` all derive the same hot/cold boundary from that one key, and a
divergence means the masker grinding into a partition the tier worker has
already exported.

### In-flight items

Items sitting in `sauron:ingest:stream` when the mask lands were `XADD`ed raw
but still pass **through** the masker on their way out, so they persist masked
(subject to the 30 s cache). Items already processed are what the retro-mask
cleans, and the tail sweep covers the seam. Items already in the DLQ are
unreachable.

---

## 12. Permissions, routes, and authorization

### The five coordinated edits

`pii:read` and `pii:manage` take `perm::ALL` from `[&str; 28]` to `[&str; 30]`.
The starting number is 28, not the 27 in `rbac.rs` today: S2 lands first and
mints `member:credential`. Miss any one of these five and something breaks
silently:

1. **`backend/crates/sauron-auth/src/rbac.rs`** — two `pub const`s, two entries
   appended to `perm::ALL`, the array length literal `[&str; 28] → [&str; 30]`,
   and **five** test assertions: `owner_has_every_permission` 28→**30**,
   `admin_is_all_except_org_manage` 27→**29**,
   `all_permissions_are_unique` 28→**30**. `developer_can_write_issues_not_manage_members`
   stays **18** and `viewer_is_read_only` stays **7** — deliberately untouched,
   and both re-read against the preset bags rather than assumed, because a count
   that still passes is no evidence the pair landed in the right presets.
2. **The `PRESETS` bags** — Owner gets them via `&perm::ALL`; `Admin` gets both
   explicitly. Developer and Viewer get neither, so
   `roles_form_a_strict_ladder` (Viewer ⊂ Developer ⊂ Admin ⊂ Owner) still holds.
3. **Migration `000041`** for existing custom roles (§2). Presets are re-synced
   by `ensure_preset_roles` at boot and need no `UPDATE`.
4. **`dashboard/src/lib/models/permissions.ts`** — `ALL_PERMISSIONS` in
   `perm::ALL` order, a new `Privacy` entry in `PERMISSION_GROUPS`, and
   `PERMISSION_LABELS`. `RoleEditorDialog` submits the full checkbox state, so a
   missing entry **silently strips the permission from any role that has it** on
   first save. `permissions.test.ts` parses `rbac.rs` and fails on drift.
5. **The `Permission` union** in `dashboard/src/lib/models/index.ts`.

Why Owner and Admin only: `pii:read` is bulk PII disclosure and `pii:manage` is
irreversible bulk destruction. Neither should be inherited by the role every
engineer gets by default.

### Routes

All in `backend/bins/sauron-api/src/routes/inspector.rs`.

| Route | Gate |
|---|---|
| `GET\|POST /v1/orgs/{org_id}/inspector/policies` | GET: discovery pattern, see below. POST: `authorize_org(PII_MANAGE)` + `validate_scope_in_org` |
| `GET\|PATCH\|DELETE /v1/inspector/policies/{id}` | Load row → ancestry → `authorize_project`/`authorize_app`, `PII_READ` / `PII_MANAGE` |
| `GET /v1/apps/{app_id}/inspector/policy` | `authorize_app(PII_READ)` — effective-policy resolution for the app picker |
| `GET\|POST /v1/inspector/policies/{id}/scans` | `PII_READ` / `PII_MANAGE` |
| `GET /v1/inspector/scans/{id}` | `PII_READ` |
| `POST /v1/inspector/scans/{id}/cancel` | `PII_MANAGE` |
| `GET /v1/inspector/scans/{id}/findings` | `PII_READ`. Clamped `limit`, keyset on `(match_count DESC, id)`, `format=csv` |
| `POST /v1/inspector/findings/{id}/reveal` | `PII_READ` + audit row |
| `POST /v1/apps/{app_id}/inspector/mask-preview` | `authorize_app(PII_MANAGE)` |
| `GET /v1/apps/{app_id}/inspector/mask-actions` | `authorize_app(PII_READ)`, `format=csv` |
| `GET /v1/apps/{app_id}/inspector/masked-keys` | `authorize_app(PII_READ)` |
| `GET /v1/inspector/mask-actions/{id}` | ancestry → `authorize_app(PII_READ)` |
| `POST /v1/inspector/mask-actions/{id}/confirm` | ancestry → `authorize_app(PII_MANAGE)` |
| `POST /v1/inspector/mask-actions/{id}/cancel` | ancestry → `authorize_app(**PII_MANAGE**)` |
| `GET /v1/orgs/{org_id}/inspector/mask-actions` | `authorize_org(PII_READ)`, `format=csv` |

Cancel is `PII_MANAGE`, not the group's `PII_READ`. S5b left the permission
unnamed; inheriting `PII_READ` would let every audit reader block a queued
redaction.

Three rules the router-enumeration test enforces:

- **Use `authorize_app`, never `authorize_app_reachable`.** The latter is
  read-only by explicit contract, and an env-scoped grant must not see app-wide
  findings.
- **Every new `/v1/apps/{app_id}/…` GET calls
  `routes::scope::reject_environment_id_with_message("the inspector is
  app-scoped; masking cannot be limited to one environment")`** and is added to
  `BACKEND_REJECTS_ENVIRONMENT_ID` in `dashboard/src/lib/api/scope.ts`.
  `bins/sauron-api/tests/http_env_scoping.rs` reads that array **out of the
  TypeScript source** and fails on drift. This includes
  `/inspector/policy` — S5a wanted it to narrow via
  `authorized_read_scope_with_perms`, but findings carry their own environment
  dimension in the payload and masking is app-scoped, so one consistent rule is
  better than one exception.
- **The org-level policy LIST must not use `authorize_org`.** A fixed-scope check
  can never be satisfied by a narrower grant — the historical 403-for-scoped-
  members bug. Use the house discovery pattern: `repo::user_grants_in_org` → 403
  on empty → `grants_from_rows` → `reach_for(PII_READ)` → filter, lifting env
  grants to their app via `repo::env_ancestries`.

### The `authorize_env` gap

There is no `authorize_env`, and this slice does not invent one.
`require_permission`/`effective_at` have no env parameter and always resolve with
`env: None`, so an env-scoped grant can never satisfy them. An `app_env`-scoped
policy is therefore authorized at its **parent app**: a member holding
`pii:manage` only on one environment cannot edit that environment's policy. This
is the same documented gap `orgs::delete_grant` carries. Accept and document.

Masking is per **app**, not per environment: the pipeline has `app_id` in hand,
environment adds a second cache dimension for no benefit, and a policy that masks
in prod but not staging is a footgun that produces exactly the leak the feature
exists to prevent.

### `confirm_source` is usually the proxy

`client_addr` returns the peer socket address unless
`api_trust_forwarded_headers` is true, and that defaults to **false** in
`Config::from_env`, in `packaging/rpm/config/api.env`, and in docker-compose. The
RPM ships nginx in front of the API, so the audit field whose stated purpose is
"from where" records the same constant for every actor in the only packaged
topology. So: `inspector.rs` calls the `pub(crate)` `client_addr` in
`routes/auth.rs` — S2 widened it there, in place, for exactly this class of
caller — and the value records its own trust decision,
`"ip=… (untrusted-peer)"` versus `"ip=… (xff)"`, so a reader can tell a real
client address from a proxy hop. The release note
says `API_TRUST_FORWARDED_HEADERS=true` is required for the field to mean
anything behind the shipped nginx.

There is no rate limit, no quota and no second approver on `pii:manage`. The
audit trail is the only control. If that is unacceptable the feature needs a
two-person rule, which is not designed here.

---

## 13. Dashboard

Three mandatory edits, in order: `src/pages/Inspector.svelte` rooted in
`<AppShell requireApp>`; a `'/inspector': guarded(Inspector as Component<never>)`
entry in `src/routes.ts` under the admin section comment; a `NavItem` in
`Sidebar.svelte`'s Manage group with `show: () => sessionStore.can('pii:read')`.
Per the Storage precedent the sidebar `show` is cosmetic — the endpoint's 403 is
the real gate, and `#/inspector` is reachable by typing it.

`ui/Icon.svelte` gains `import ShieldAlert from '@lucide/svelte/icons/shield-alert'`
and `import EyeOff from '@lucide/svelte/icons/eye-off'` plus the two registry
entries. Nothing else in the dashboard imports from `@lucide/svelte` directly.

Four hand-rolled tabs copying `Alerts.svelte` verbatim
(`<nav class="tabs"><button class="tab" class:active={…}>`): **Findings /
Policy / Scans / Audit**. There is no Tabs primitive and introducing one is out
of scope.

**Findings.** `DataTable` with head Severity | Table | Column | Path | Matches |
Last seen | actions. Row expand renders the matched sample through the existing
`JsonTree` with `expandTo={2}` — nested data, so **not** `KeyValueList`, which is
flat-only. Per the documented `DataTable` trap, the expanded panel is a CSS grid
with ARIA roles, not a nested `<table>`, with background/white-space/cursor set
inline on the `<td>` (`Storage.svelte:152` is the worked example) because
`DataTable`'s `:global(tbody td)` rules beat a nested component's scoped styles.
Per-row Mask `Button variant="danger" size="sm"` with the `eye-off` icon, gated
on `$derived(sessionStore.can('pii:manage', { app: sessionStore.currentAppId }))`.
Rows whose `partition_kind` is `rollup` carry "recurring"; `default` carries
"never ages out"; non-maskable targets carry "not maskable".

**Policy.** Card stack. Enabled state is a `Button` that flips a boolean plus a
status `Badge` (there is no Toggle primitive). The tracked-key list is a raw
`<input>` with Enter-to-add rendering chips as `Badge` + an x `Button`. The
schedule uses a raw `<select class="sel">` fed by
`src/lib/constants/inspectorSchedules.ts`, whose doc comment names the backend
constant it mirrors (`monitorIntervals.ts` is the precedent). Below it a
read-only "Forward enforcement" Card lists every `inspector_masked_keys` row with
its source action and the literal sentence "New events are masked within about
30 seconds of a change." — with the number coming from the API, not hardcoded.

**Scans.** `DataTable` over `inspector_scans` (started / finished / status /
rows scanned / findings / coverage) plus a "Run scan now" `Button` gated on
`pii:manage`. A running scan shows a `Spinner` and polls.

**Audit.** `DataTable` over `inspector_mask_actions`: When | Who | Targets |
Status | Rows masked | Cold skipped | Cancelled by. Expand shows the frozen
target list, the error text and the §1 panel. Gated on `pii:read` only —
deliberately readable by someone other than the actor, which is affordable
precisely because the row stores paths and counts and never a value. Polls every
3 s while any action is `pending|running|cancelling|preview`, via a `$effect`
that clears its interval in the teardown, and not at all otherwise.

**`MaskDialog`** (`src/lib/components/inspector/MaskDialog.svelte`). `Modal
size="md"` — `ConfirmDialog` is insufficient, it has no text input. Flow: open →
`POST mask-preview` → `Spinner` "Counting affected rows…" → per-target preview
table (rows to mask, cold rows skipped) → the permanently-visible,
non-collapsible §1 panel → the `event_user` search warning when it applies → a
note that a running mask can be stopped but not undone → an `<Input>` labelled
"Type the app slug ({slug}) to confirm" → footer Cancel + `Button
variant="danger"` disabled until the typed value matches **and** the preview is
ready and unexpired. Prop-seeding reads are wrapped in `untrack()` so a parent
reload cannot wipe a half-typed confirmation. On success: `toastStore.success`,
close, switch to the Audit tab with the new action highlighted.

**Pure modules** (no Svelte, no DOM — vitest is node-only and there is no DOM
test environment, so this is where all the coverage lives):

- `src/lib/models/inspector-schedule.ts` — weekday bitmask ↔ checkbox array, a
  human "Every Tue, Thu at 03:00 (Europe/Paris)" description, and a next-3-runs
  preview computed with `Intl.DateTimeFormat`. The model states that the
  server's `next_run_at` is authoritative and this is display only.
- `src/lib/models/inspector-findings.ts` — grouping and sorting by app → column
  → path, the "at least N" rendering when `match_count_exact` is false, and the
  `partition_kind` / `env_scope` badge logic.
- `src/lib/models/inspector.ts` — `describeTarget`, `expandCompanionTargets`
  (mirroring the backend map so the dialog can describe the blast radius before
  the server answers), `maskConfirmReady(typed, slug, preview)`,
  `UNREACHABLE_COPY` (the §1 headline plus its twelve rows as a data array, so it
  is testable and cannot drift between the dialog, the audit detail and the
  wiki), and
  `csvFilename(kind, scope, from, to)`.

`src/lib/api/inspector.ts` imports only `{ api }` from `./client` so the bearer
header and the single-flight 401 refresh-and-replay apply. One exported async fn
per endpoint; request-body interfaces live here, response types in
`models/index.ts`. The `Permission` union and the inspector response types land
in `models/index.ts` together, in **one** edit — they were specified in different
sections of this document, which is not a reason to touch the file twice.

---

## 14. CSV export

Reuses the export foundation S4 builds: `backend/bins/sauron-api/src/csv.rs`
(hand-rolled RFC 4180, quote-doubling plus a leading apostrophe before `=`, `+`,
`-`, `@`, tab and CR) and `dashboard/src/lib/api/download.ts` — it imports the
shared `api` instance, so it belongs beside the other API modules. No `csv` crate.
No second escaper.

`format=csv`, buffered, `text/csv; charset=utf-8`,
`Content-Disposition: attachment; filename="sauron-inspector-{findings|mask-actions}_{slug}_{from}_{to}.csv"`.

- Findings columns: `finding_id, scan_id, detected_at, app_slug, environment,
  env_scope, table, column, json_path, matched_key, detector, match_count,
  match_count_exact, first_seen_at, last_seen_at, partition_kind, masked,
  mask_action_id`.
- Audit columns: `action_id, requested_at, confirmed_at, finished_at,
  requested_by_email, cancelled_by_email, app_slug, status, targets
  (semicolon-joined `table.column.path`), estimated_rows, rows_masked,
  cold_rows_skipped, cold_boundary_at, error`.

**No value or sample column in either export.** An export of a PII report that
contains the PII is a PII dump with a friendly filename — it lands in email,
Slack and laptops, i.e. precisely the places this feature exists to keep data out
of. Locations are what the export is for: handing a remediation list to the team
that owns the SDK integration. An `include_values=1` opt-in was rejected because
an opt-in that everybody ticks is a default.

The injection guard applies to `json_path` and `matched_key`, not only to free
text — they are dev-controlled bytes (§6).

Over `inspector_export_max_rows` (50,000) the route returns `400 too_many_rows`
telling the caller to narrow the range, because a buffered export cannot be
truncated honestly.

Note the org-wide audit CSV exports `requested_by_email` for every action, which
makes a downloadable staff-email roster available to any org-scoped `pii:read`
holder. That is a deliberate trade for an audit trail, it is bounded by the
pseudonymization reaper (§8), and it is stated in the wiki.

`download.ts` must go **through** the `api` instance so refresh-and-replay still
works, and must read the blob back as text on a non-2xx before normalizing —
`normalizeError` reads `error.response.data` as an `{error:{code,message}}`
envelope, and with `responseType: 'blob'` that is a Blob and the message is lost.
The CORS layer needs `.expose_headers([CONTENT_DISPOSITION])` for the
split-origin topology the product ships.

---

## 15. Config, packaging, and the upgrade hazard

New `// --- pii inspector ---` section on `sauron_core::Config` using the
existing `var()`/`parse()` helpers. **Never `bail!` in `from_env`** — every
binary shares the struct, and a bail would take down unrelated services (the
`jwt_secret` precedent).

| Key | Default | Read by |
|---|---|---|
| `INSPECTOR_ENABLED` | **false** | inspector |
| `INSPECTOR_TICK_SECS` | 30 (clamped 5..3600) | inspector |
| `INSPECTOR_BATCH_ROWS` | 5000 | inspector |
| `INSPECTOR_BATCH_PAUSE_MS` | 200 | inspector |
| `INSPECTOR_LEASE_SECS` | 120 | inspector |
| `INSPECTOR_MAX_ATTEMPTS` | 3 | inspector |
| `INSPECTOR_STATEMENT_TIMEOUT_MS` | 30000 | inspector |
| `INSPECTOR_WINDOW_DAYS` | `search_scan_clamp_days` (→ `tier_hot_days`, **30**) | inspector |
| `INSPECTOR_DETECTOR_WINDOW_DAYS` | 7 | inspector |
| `INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT` | 200000 | inspector |
| `INSPECTOR_DEFAULT_SWEEP_ROWS` | 50000 | inspector |
| `INSPECTOR_CATCHUP_GRACE_HOURS` | 6 | inspector |
| `INSPECTOR_SCAN_KEEP` | 20 | inspector |
| `INSPECTOR_FINDING_RETENTION_DAYS` | 90 | inspector |
| `INSPECTOR_MASK_BATCH` | 2000 | inspector |
| `INSPECTOR_MASK_PAUSE_MS` | 200 | inspector |
| `INSPECTOR_MASK_MAX_ROWS` | 20000000 | api |
| `INSPECTOR_CLAIM_STALE_SECS` | 300 | inspector |
| `INSPECTOR_PREVIEW_TTL_SECS` | 900 | api |
| `INSPECTOR_PREVIEW_GC_DAYS` | 7 | inspector |
| `INSPECTOR_AUDIT_RETENTION_DAYS` | 0 = never | inspector |
| `INSPECTOR_AUDIT_PII_DAYS` | 730 | inspector |
| `INSPECTOR_EXPORT_MAX_ROWS` | 50000 | api |
| `INSPECTOR_POLICY_CACHE_SECS` | 30 | **ingest + api** → `sauron.env` |
| `INSPECTOR_TAIL_SWEEP_SECS` | 120 | **inspector + api** → `sauron.env` |
| `TIER_HOT_DAYS` (existing key, relocated from `tier.env` by this slice) | **30**, unchanged | **tier + inspector + api** → `sauron.env` |

Every key is documented three times with consistent wording:
`packaging/rpm/config/inspector.env` (or `sauron.env` for the shared ones),
`.env.example` under a `# --- inspector (sauron-inspector) ---` header, and the
service's `environment:` block in `docker-compose.yml`. Each comment states the
operational consequence of getting it wrong, per the house convention. A CI grep
asserting every `var("…")`/`parse("…"` literal in `config.rs` appears in
`.env.example` is the only thing that will keep 25 new keys documented in a year.

Packaging touchpoints, all seven in lockstep:

- `packaging/rpm/config/{sauron.env,tier.env}`: **delete `TIER_HOT_DAYS` from
  `tier.env` and declare it once, still 30, in `sauron.env`,** with the two new
  shared keys. The value does not change; only the place it is declared does.
  Until this slice, `TIER_HOT_DAYS` was one worker's private tuning knob; it is
  now the hot/cold boundary three binaries derive independently, and a divergence
  means the masker rewriting rows in a partition `sauron-tier` has already
  exported to Parquet — Postgres masked, Parquet raw, and the drop destroys the
  only masked copy. The relocation carries an upgrade hazard that must be in the
  release note: `sauron-tier.service` loads `sauron.env` first and `tier.env`
  second, and `tier.env` is `%config(noreplace)`, so on any host whose operator
  ever edited it rpm keeps their file verbatim and ships the new one beside it as
  `.rpmnew`. Their stale `TIER_HOT_DAYS=` line then wins for `sauron-tier` alone,
  while `sauron-inspector` and `sauron-api` — which never read `tier.env` — use
  the shared declaration. That is exactly the divergence the move exists to
  remove, so SETUP.md §11 instructs deleting the line by hand after upgrading,
  and the inspector must not be enabled before that is done.
- `packaging/rpm/binaries.txt`: add `sauron-inspector` to the `sauron-server`
  group. This single file drives CI's prebuilt assemble, `build-rpm.sh
  --prebuilt` preflight, and the spec's `%install` loop — but **not** `%files`,
  which is manual, and rpmbuild fails on installed-but-unpackaged files.
- `packaging/rpm/sauron.spec`: `Source16 = systemd/sauron-inspector.service`,
  `Source37 = config/inspector.env`, matching `%install` lines, explicit
  `%files server` entries for `/usr/bin/sauron-inspector`, the unit, and
  `%attr(0640,root,sauron) %config(noreplace) /etc/sauron/inspector.env`, plus
  `sauron-inspector.service` added to `%post`, `%preun` and
  `%postun_with_restart`.
- `packaging/rpm/systemd/sauron-inspector.service`: `sauron-alerts.service`
  verbatim minus `EnvironmentFile=/etc/sauron/secret.env` (it decrypts nothing)
  and minus `ReadWritePaths` (it writes no files). `Type=exec`,
  `After=network-online.target sauron-migrate.service`, `Restart=on-failure`,
  `RestartSec=2`, full hardening block, `StateDirectory=sauron`.
- `packaging/rpm/build-rpm.sh`: the two hardcoded `install -m0644` lines for the
  unit and the env file. This list is **not** driven by `binaries.txt`; missing
  it yields a cryptic rpmbuild SOURCE-not-found failure.
- `docker-compose.yml`: an `inspector` service copied from the `alerts` block
  (`build` context `./backend`, `args BIN: sauron-inspector`, `${VAR:-default}`
  interpolation, `depends_on` migrate `service_completed_successfully` and
  postgres `service_healthy`). No JWT_SECRET, no NOTIFY_SECRET_KEY, no Redis.
  Compose has no shared env file, so `TIER_HOT_DAYS: ${TIER_HOT_DAYS:-30}` — today
  set only on the `tier` service — is repeated on `inspector` and `api`, all three
  reading the same variable so they cannot drift. Plus `max_connections=200` on
  the postgres service (§8).
- `packaging/rpm/SETUP.md` §11 "Upgrading" (created by S0) gains a row for
  migrations 000041-000043 with its specific symptom, and the row telling an
  operator to remove `TIER_HOT_DAYS` from a pre-existing `/etc/sauron/tier.env`.

No new workspace dependency. `regex`, `chrono-tz`, `cron` and `csv` are all
deliberately avoided; the walker, detectors, redaction, scheduling arithmetic and
CSV escaping are hand-rolled, matching the `render::substitute` precedent.

CI gates as-is: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings` (the mask executor exceeds 7 args and needs
`#[allow(clippy::too_many_arguments)]` like every existing worker),
`cargo test --workspace`, `rpmspec -P` and `build-rpm.sh --srpm`.

**Upgrade hazard.** `sauron-migrate.service` has no `[Install]` section and is
not in `%postun`'s restart list, so `dnf upgrade` leaves new binaries running
against the old schema. Until `systemctl start sauron-migrate` is run by hand the
inspector routes 500 and — worse — the pipeline's `masked_keys_for_app` query
fails on every cache miss. The fail-stale enforcer plus the rate-limited `warn!`
keeps ingest flowing, but forward masking is off deployment-wide with only a log
line. This goes in the migration comment, the release note and SETUP.md §11.

## Error handling

| Case | Status | Note |
|---|---|---|
| Policy with no tracked keys and no detectors | 400 | Would otherwise produce a confident false negative |
| Invalid `schedule_tz` | 400 | Validated with `SELECT now() AT TIME ZONE $1` |
| Policy target not in org | 404 | `validate_scope_in_org`; 404 not 403, so it is not an existence oracle |
| Second scan for a policy already queued/running | 409 + active scan id | Enforced by the partial unique index, not a handler check |
| Reveal on a dropped partition or replaced rollup row | 410 Gone | Also 410 on an `app_id` mismatch — an attribution bug becomes a benign miss |
| Reveal on `stacktrace`/`stacktrace_symbolicated`/`debug_meta` | 400 | Not reveal-eligible; preview only |
| Numeric array index in a mask path | 400 | An index is not stable across rows |
| Wildcard anywhere but the first segment, or more than one | 400 | |
| Confirm with a stale preview | 409 | TTL measured from `previewed_at` |
| Confirm with a wrong slug | 400 | |
| Confirm above `INSPECTOR_MASK_MAX_ROWS` | 409 | Raise the ceiling explicitly |
| Cancel a `done`/`failed`/`cancelled` action | 409 | `running` → `cancelling` is allowed |
| Requester lost `pii:manage` or was deactivated before the claim | action → `failed` | With an explicit reason in `error` |
| CSV over `INSPECTOR_EXPORT_MAX_ROWS` | 400 `too_many_rows` | A buffered export cannot be truncated honestly |
| `?environment_id=` on any `/v1/apps/{id}/inspector/*` | 400 | With the "masking cannot be limited to one environment" message |

## Testing

CI runs `cargo test --workspace` against `postgres:16` and `redis:7` service
containers, and `crates/sauron-db/tests/common/mod.rs`'s `TestDb::setup()`
returns `None` (skip, not fail) without `TEST_DATABASE_URL`. That pushes the
design in a useful direction: **every decision lives in a pure crate.**

Pure, no Postgres (`sauron-inspector` + `sauron-auth`):

- Walker: one-level `tags`; two-level `contexts`; arbitrary-depth `extra`; an
  array of breadcrumbs collapsing to `[]` segments; the depth-6 cap; a `contexts`
  block that is the scalar string `"[Circular]"` (real live data); a non-object
  root; an empty object.
- Key matching: `Email` matches `email`; `user_email` and `emails` do **not**;
  keys containing `.`, spaces and `=`.
- Redaction: preview never echoes more than the first and last codepoint, never
  exceeds 64 chars, truncates on a char boundary for multibyte input, renders
  numbers/bools/null without leaking magnitude. Property test
  `!preview.contains(raw)` over a corpus — **and the same test for `key_path`**.
- Detectors: Luhn positives and negatives (a 16-digit non-Luhn number must not be
  flagged); e164 with and without `+`; an email with a plus-tag; a three-segment
  JWT; and a negative corpus of UUIDs, ISO timestamps and order ids.
- Prefilter builder: `escape_like` over a key containing `%`, `_`, backslash and
  a double quote; and the assertion that an empty key set with **non-empty**
  detectors omits the ILIKE rather than making the unit a no-op.
- `resolve_targets`: a disabled child policy subtracts its pairs from the
  parent's target list; rollup and `_default` classes are absent for an
  `app_env`-scoped policy.
- Path grammar and `expand_targets`: reject a numeric index, reject a
  non-leading or duplicate wildcard; `error_events.title` expands to the wire
  sources **and** `issues.title`; `stacktrace` includes
  `stacktrace_symbolicated`; `context` includes `sessions.context`; everything
  else expands to itself.
- Mask applier over `serde_json::Value`: nested three levels into `extra`; a
  missing path leaves the document byte-identical; an object value collapses to
  `"****"`; a wildcard preserves element order and length; an empty array stays
  `[]`; a null column normalizes through `object_or_empty` rather than becoming
  SQL NULL.
- The table allowlist contains no auth table, and the `maskable` subset of the
  inventory is exactly the six tables in the `inspector_masked_keys` CHECK —
  `devices`, `identities` and `workflows` are scannable and never mask targets.
- `rbac.rs`: `perm::ALL.len() == 30`, no duplicates, Owner 30 / Admin 29 /
  Developer 18 / Viewer 7, ladder and `admin_is_all_except_org_manage` still pass.

Dashboard vitest (node-only): `permissions.test.ts` passes after the mirror
edits; `inspector.test.ts` covers `describeTarget`, `expandCompanionTargets`
agreeing with the backend map, `maskConfirmReady` (wrong slug / right slug /
preview not ready / preview stale), and `UNREACHABLE_COPY` leading with the
hot-Postgres headline and carrying all twelve §1 rows beneath it — a dropped row
is a promise the dialog stops making.

Postgres-backed (`TestDb::setup()`):

- Two concurrent `claim_due_policies` return disjoint sets; a claimed row's
  `next_run_at` is strictly `> now()`; `schedule_days = 0` is never claimed.
- DST for `America/New_York` and `Europe/Paris`: a 02:30 Sunday schedule on the
  spring-forward day yields a valid instant `> now()` landing on a set bit; the
  fall-back day yields exactly one instant.
- The flush CTE: two sequential flushes accumulate `match_count` correctly and
  advance the cursor; a stale `worker_id` affects zero rows and does not move the
  cursor; `RETURNING` surfaces `cancel_requested_at`; and **`findings_count`
  equals `SELECT count(*)` after a one-unit scan** — the assertion that catches
  the snapshot bug.
- The partial unique index rejects a second queued scan with 409, not 500.
- **`EXPLAIN` the batch `UPDATE` and assert exactly one `Update on
  error_events_<child>` node, not 22.** This is the regression that silently
  destroys the cost model.
- A row in `error_events_default` with an `occurred_at` outside every explicit
  range is missed by the day loop and caught by the default phase — and a row
  below `tier_hot_days` in that same child is **not** touched.
- A row older than `tier_hot_days` is unmodified and `cold_rows_skipped` /
  `cold_boundary_at` are populated; a day at or below the tier watermark is
  refused.
- Crash resume: run one batch, reset `claimed_at` into the past, re-claim, assert
  the total masked equals the row count with no double-count and no gap.
- Re-introduction: after masking `issues.title`, a second `upsert_issue` for the
  same fingerprint with a raw title leaves `'****'`; with the pipeline masker
  enabled, one `ErrorItem` carrying a masked key lands masked in
  `error_events.extra`, `error_events.context` **and** `sessions.context`;
  `dead_letter` receives the re-serialized masked job.
- Authz matrix: a project-scoped `pii:manage` holder cannot edit an `app_env`
  policy in another project; an env-scoped grant writes nothing; a
  `pii:read`-only caller gets 403 on `/scans`, `/cancel` and `/confirm` but 200
  on `/findings`; reveal 410s on a dropped locator and on a mismatched `app_id`.
- End-to-end: seed one app-env with rows containing `extra.customer.email` plus
  one row in `error_events_default`; run one scan; assert two findings with the
  expected `key_path`, correct `match_count`, `partition_kind` `ranged` and
  `default`; assert **no finding's `sample_preview` or `key_path` contains the
  seeded email**. Kill the worker mid-scan, reclaim after the lease, assert final
  counts equal the uninterrupted run.

`http_env_scoping.rs` still passes with all six new app-scoped routes.

**Manual e2e is the real gate for the UI and for CSV bodies** (there is no HTTP
harness assertion for either), via the project's harness verification pattern:
create a policy → run a scan → see findings → reveal one and see the audit row →
preview a mask → confirm is disabled until the slug matches → let an expired
preview be rejected → confirm → cancel mid-run and see terminal `cancelled` with
a durable cursor → re-run and see it complete → send a new event and confirm it
lands masked → open both CSVs in a spreadsheet and confirm a leading-`=` value
renders as text, not a formula.

## Files

**New**

- `backend/migrations/2026-08-01-000041_pii_perms/{up,down}.sql`
- `backend/migrations/2026-08-01-000042_inspector_scan/{up,down}.sql`
- `backend/migrations/2026-08-01-000043_inspector_mask_audit/{up,down}.sql`
- `backend/crates/sauron-inspector/` — `columns.rs`, `walk.rs`, `match.rs`,
  `detect.rs`, `redact.rs`, `prefilter.rs`, `targets.rs`, `path.rs`, `mask.rs`
  (the pure applier). No DB, no axum.
- `backend/bins/sauron-inspector/` — package `sauron-inspector-bin`; `main.rs`,
  `scan.rs`, `mask.rs`, `preview.rs`, `reap.rs`
- `backend/bins/sauron-api/src/routes/inspector.rs`
- `backend/crates/sauron-pipeline/src/mask.rs` — `PolicyCache`, `apply_wire`,
  `apply_context`
- `packaging/rpm/systemd/sauron-inspector.service`,
  `packaging/rpm/config/inspector.env`
- `dashboard/src/pages/Inspector.svelte`
- `dashboard/src/lib/components/inspector/MaskDialog.svelte`
- `dashboard/src/lib/api/inspector.ts`
- `dashboard/src/lib/models/{inspector,inspector-schedule,inspector-findings}.ts`
  + colocated `.test.ts`
- `dashboard/src/lib/constants/inspectorSchedules.ts`
- `wiki/Privacy-Inspector.md` + entries in `wiki/_Sidebar.md` and `wiki/Home.md`,
  containing the §1 list verbatim; extend `wiki/Best-Practices.md` §2 and add a
  sentence to `wiki/Search.md` about masked rows and the `user` dimension

**Modified**

- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}` — six tables, the
  claim/flush/batch/prune functions, and the `upsert_issue` sticky guard
- `backend/crates/sauron-auth/src/rbac.rs` — two permissions, `perm::ALL` 28→30,
  the Admin preset bag, five assertions
- `backend/crates/sauron-core/src/config.rs` — the inspector section
- `backend/crates/sauron-pipeline/src/{worker.rs,process.rs}` — the two
  application sites and the masked dead-letter payload
- `backend/bins/sauron-api/src/main.rs` — 17 routes, CORS `expose_headers`
- `backend/bins/sauron-api/src/routes/orgs.rs` — cancel a deactivated member's
  pending mask actions
- `packaging/rpm/{binaries.txt,sauron.spec,build-rpm.sh,SETUP.md}`,
  `packaging/rpm/config/{sauron.env,tier.env}`, `docker-compose.yml`,
  `.env.example`, `README.md`
- `dashboard/src/{routes.ts,lib/components/layout/Sidebar.svelte,lib/components/ui/Icon.svelte}`
- `dashboard/src/lib/models/{index.ts,permissions.ts}`,
  `dashboard/src/lib/api/scope.ts`
- `dashboard/src/pages/Docs.svelte` — document the flow

## Follow-ups (out of scope)

- **A bounded DLQ.** `sauron:ingest:dlq` is the single worst item in §1 and
  fixing it is independently valuable.
- Cold-tier scanning (a DuckDB `json_extract` aggregate over the cold glob as its
  own unit class) and cold-tier masking (read → `json_merge_patch` → write-temp →
  rename, which must not disturb `sauron-tier`'s count-based idempotency guard —
  a row-count change stalls that table's watermark permanently, and DuckDB will
  happily `COPY TO` the path it is reading).
- Ingest-time detection. There is no hook point in `sauron-pipeline` today and
  adding one is an ingest-latency decision, not a scanner decision.
- Regex, prefix and suffix key matching; per-key column overrides.
- Per-row findings and a "show me every matching event" drill-down.
- Alerting when a scan finds something new — build an `AlertContext`, call
  `AlertEngine::fire` with a new `trigger_type`, and reuse all six channel kinds,
  the throttle and the `alert_events` history.
- Partial / format-preserving masking, and any read-time redaction mode (which
  means auditing all 8 response paths that emit raw `ErrorEvent` structs — the
  same drift that already leaves `strip_source_context` applied at only 2).
- A two-person approval rule or per-actor mask quotas.



