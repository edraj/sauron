# Privacy Inspector

The **Privacy Inspector** finds developer-supplied PII sitting in Sauron's
telemetry JSON columns, proves what it found **without storing a second copy of
it**, masks it in hot Postgres, and enforces that mask on all future ingest.

Open it from **Manage → Privacy**. It needs `pii:read`; masking needs
`pii:manage`.

The flow is five steps, in this order:

1. **Create a policy** — which node it covers, which key names to track, which
   columns to read, and when to run.
2. **Run a scan** — manually, or on the policy's schedule.
3. **Review findings** — a location and a shape-only preview, never the value.
   `Reveal` returns one raw value and writes an audit row for it.
4. **Preview a mask** — the affected row count, computed against frozen
   targets.
5. **Confirm by typing the app slug** — then the retro-mask runs and the key is
   enforced on every future event for that app.

See also: **[Best Practices](Best-Practices.md)** (keeping PII out in the first
place) · **[Search & Filtering](Search.md)** (what a masked row stops matching)
· **[Active Users](Active-Users.md)** (what an identity-key mask costs you).

---

## What masking does **not** mean

The product never says "permanently removed". It says **masked in hot Postgres
and in all future ingest** — because that is all it can honestly promise. The
list below is the same list the mask dialog shows you before you confirm and
the Audit tab shows afterwards; it is one array in the code, rendered in three
places, so a support answer and the product cannot drift apart.

> **Masking rewrites rows in hot Postgres only.** Everything below still holds
> the original bytes, or is outside this product’s reach.
> *Read the rows below before confirming.*

- **Cold Parquet** — The partition was exported before the mask ran. Parquet is
  immutable and, after the drop, the only copy.
  *Bounded by:* Nothing. Permanent.
- **Postgres rows older than TIER_HOT_DAYS** — The retro-mask deliberately
  stops at the hot boundary.
  *Bounded by:* The tier drop, which destroys the row entirely.
- **The Redis ingest stream** — sauron:ingest:stream holds the full serialized
  job.
  *Bounded by:* XADD … MAXLEN ~ 1000000.
- **The Redis DLQ** — sauron:ingest:dlq is XADD with no MAXLEN and no TTL, and
  no reaper exists. A payload that fails to deserialize still dead-letters raw.
  *Bounded by:* Nothing. Permanent.
- **Per-person breadcrumbs in Redis** — Up to 100 batches are buffered per
  person before an error arrives.
  *Bounded by:* A 1800 s TTL.
- **alert_events.title / .body** — They embed the issue title verbatim.
  *Bounded by:* ALERT_EVENT_RETENTION_DAYS (90).
- **Already-delivered alerts** — Email, Slack, Discord, Matrix, Telegram and
  webhook messages are gone from our control the moment they send.
  *Bounded by:* Nothing.
- **event_users.properties** — The identify() write merges with ||, which never
  removes keys. An at-rest mask is undone by the next identify().
  *Bounded by:* Forward enforcement only, and only for keys in the mask set.
- **devices.\*** — Every column is COALESCE(EXCLUDED.x, devices.x) — a non-null
  incoming value always wins, and there is no wire field to enforce on.
  *Bounded by:* Not offered: devices is not maskable at all.
- **Symbolicated source lines** — Frames carry context_line / pre_context /
  post_context — verbatim customer source. Masking a JSON path never touches
  them.
  *Bounded by:* Redacted from responses only, for callers without source:read.
- **Backups, WAL, replicas** — Out of the product’s reach entirely.
  *Bounded by:* Operator policy.
- **The active-users report stops identifying anyone new through that key** —
  The enforcer runs before the active-users pipeline stamps identified_at, so
  masking a key an app sends as context.user.id means the equality test never
  passes again. Nobody already stamped is un-identified, but everyone first
  seen afterwards arrives as a guest and never merges across apps, so the
  identified share decays with no discontinuity to notice.
  *Bounded by:* Nothing. The bytes are gone, so it cannot be recomputed later.

The last one is not about bytes surviving — it is the mask silently taking
something else away with it. See
**[Active Users](Active-Users.md#two-things-that-silently-change-these-numbers)**
before you mask anything an app uses as an identity.

## Detection is best-effort, not a compliance guarantee

A scan runs in two phases. Phase 1 is a cheap `column::text ILIKE ANY(...)`
over an index-bounded window, which eliminates 95-99% of rows; phase 2 parses
only the survivors and walks them in Rust.

Phase 1 greps the JSON **text** for the quoted key name. So:

- a key serialized with a unicode escape (`"\u0065mail"`) is **not** found;
- anything inside a base64 or URL-encoded blob is **not** found;
- a value split across two fields is **not** found.

That is the right tool for **accidental** PII — a developer who put an email in
`extra` without thinking — and it is useless against an adversary. The Findings
tab says so on every scan, non-dismissibly. Do not present a clean scan as
evidence that an app holds no PII.

## Policies

A policy is one row per node, and **precedence is most specific wins, whole
row, no merging**:

```
app_env  >  app  >  project
```

Exactly one policy may exist per node (a unique index enforces it), so the
ranking is a database fact rather than an ordering problem. There is no merging
of key lists across levels: the winning row is used in full.

**A narrower row subtracts its pairs from the parent's scan, enabled or not.**
That is the mechanism for excluding one noisy environment: create an
`app_env` policy on it and disable it, and the project-level scan stops walking
that environment. A scan that lost pairs this way reports `partial` coverage
and names how many pairs were excluded, so a smaller number is never silent.

Two consequences worth knowing before you place a policy:

- An **`app_env` policy scans neither the rollup tables nor the unattributed
  rows.** `issues` and `event_users` carry `app_id` only, so they cannot be
  attributed to one enrollment at all, and the unattributed sweep cannot be
  bounded to one environment without an index that does not exist. Put the
  policy on the **app** if you want those covered.
- An `app_env` policy is authorized at its **parent app**. A member holding
  `pii:manage` on one environment only cannot edit that environment's policy —
  the same documented gap grant management carries.

Tables scanned: `error_events`, `analytics_events` and `transactions` always,
plus whichever rollups the policy opts into (`issues` and `event_users` by
default; `sessions`, `identities` and `workflows` are available).

## Keys vs. detectors

**A tracked key is a literal name** — lowercased when saved, matched
case-insensitively, **exactly**, against a leaf's own key at any depth (or only
at the top level, per key). It is not a pattern: there is no regex, deliberately
— admin-authored regex on a shared worker is admin-authored ReDoS. Dotted paths
are wrong as *input* (you do not know that the SDK nested the field under
`contexts.order`) and right as *output*, which is exactly what a finding's
`key_path` is.

**Detectors are opt-in and change the cost model by an order of magnitude.**
Eight value-shape detectors ship — `email`, `phone_e164`, `ipv4`, `ipv6`,
`jwt`, `iban`, `ssn_us`, `credit_card` (Luhn-checked). Turning any of them on
**removes the phase-1 prefilter entirely**: every row in the window leaves
Postgres and every string leaf is scanned, roughly 20× the CPU and 20× the
bytes of key mode. That is why detector scans get their own, much shorter
window (`INSPECTOR_DETECTOR_WINDOW_DAYS`, 7 days by default) regardless of the
policy's `window_days`.

A finding carries **one** detector — the first one it trips — not all of them,
so the findings table does not multiply by eight for no extra information.

Findings never contain the value. They carry the table, the column, a redacted
`key_path`, the matched key, the value's **type**, a shape-only preview capped
at 64 characters, and a count. Counts on large units are reported as "at least
N" rather than pretending to an exactness the batch cap cannot deliver.

## Masking semantics, and the three visible regressions

The value at the masked path becomes the JSON string `"****"` and **the key is
kept**. Removing the key was rejected: it changes row shape, breaks the
`contexts` named-block structure, and makes a "does this key exist" question
answer *absent* where data existed — a second, subtler lie. On a TEXT column
there is no partial redaction at all: the **whole** value becomes `****`.

Three consequences are visible to users, and all three are intentional:

1. **The type changes.** `extra.cart_value_cents: 4200` becomes `"****"`, a
   string. Arithmetic, `@>` containment and B-tree comparison stop working for
   that row. A masked row does not merely fail to match the old value — it
   drops out of every predicate over that column.
2. **Masking `event_user` breaks the `user.` search dimension.** `user.email`
   reads `error_events.event_user`; once it is `"****"`, searching for a person
   by email finds nothing for masked rows. See
   **[Search & Filtering](Search.md)**.
3. **`issues.title` sticks forever once masked.** The issue upsert carries a
   guard: once a fingerprint's title is `****` it stays `****`, even if every
   subsequent occurrence is benign. `error_events.title` is derived server-side
   from the exception and message and has no wire field, so without the guard
   the raw string would come back on the very next occurrence. A fingerprint is
   a stable error identity, so this is the correct trade — but it is a visible
   regression on the most-read string in the product, and **this is the one
   support will be asked about.** `issues.culprit` behaves the same way.

Masking one column also masks its companions, because forward enforcement can
only reach a derived column through its inputs: masking `error_events.title`
also masks `issues.title`, `exception_value`, `exception_type` and `message`;
masking `error_events.culprit` also masks `issues.culprit`; masking either
events table's `context` also masks `sessions.context`, which is the same
enriched blob snapshotted on every event. Nothing outside that map
auto-expands, and the dialog lists the full expanded set before you confirm.

## What is scanned but never maskable, and why

| Column | Why it is not maskable |
|---|---|
| `identities.alias_id`, `identities.distinct_id` | These **are** the identity graph. Masking them merges people rather than redacting them: two masked rows become the same person. |
| `workflows.cancel_reason` | Derived server-side, no wire field to enforce on. Mask `analytics_events.properties` instead, which is where the value came from. |
| `error_events.debug_meta`, `stacktrace`, `stacktrace_symbolicated` | Machine-owned build and frame data. Not maskable and not revealable — frames carry verbatim customer source lines. |

All of these are **opt-in** columns: a default policy does not read them at all.

`devices` is a special case: it is **neither scanned nor maskable**. Every
column is written as `COALESCE(EXCLUDED.x, devices.x)`, so a non-null incoming
value always wins and there is no wire field to enforce on — a mask there would
be undone by the next event and would read as a bug. It is listed in the
"what this does not reach" panel for exactly that reason.

## `event_users.properties` is forward enforcement only

The `identify()` write merges with `||`, which **never removes keys**. An
at-rest mask on `event_users.properties` therefore holds only for as long as
forward enforcement keeps rewriting the incoming value — which holds for as
long as the key stays in the mask set, and not one write longer. For anything
else, the next `identify()` puts it back. Treat masking here as "stop accepting
this key", not as "erase this key".

## The audit trail, and its trade

Every mask action is a row: who requested it, from what address, what was
counted, what was confirmed, what actually got written, and what it skipped for
being cold. Every reveal is a row too, written **before** the value is returned
— a failure to audit is a failure to reveal.

Both the findings list and the audit list export to CSV (RFC 4180, with a
leading-`=`/`+`/`-`/`@` guard so a value never becomes a spreadsheet formula).

**The org-wide audit CSV carries `requested_by_email` for every action.** That
makes a downloadable roster of staff email addresses available to anyone
holding `pii:read` at org scope. This is deliberate — an audit trail that
cannot name the actor is not an audit trail — and it is bounded:
`INSPECTOR_AUDIT_PII_DAYS` (730 by default) pseudonymizes the denormalized
email on older rows, so the privacy feature does not end up being the one
un-erasable store of staff PII in the schema.

Audit rows themselves are kept forever by default
(`INSPECTOR_AUDIT_RETENTION_DAYS=0`): they grow per human action, not per rule
evaluation, and they are what a compliance question gets answered from.
Findings are pruned on their own schedule (`INSPECTOR_SCAN_KEEP`,
`INSPECTOR_FINDING_RETENTION_DAYS`).

## Operating it

- **`INSPECTOR_ENABLED` is `false` by default.** The `sauron-inspector` worker
  starts, logs that it is idle, and sleeps. Scans and masks queue but nothing
  executes until an operator flips it and restarts the unit. The API and the
  page work either way — which means a queued scan that never moves is usually
  this, not a bug.
- **Schedule broad masks off-peak, and `VACUUM` after.** A full pass over a
  hot window roughly doubles live tuples until autovacuum catches up. The job
  **never runs `VACUUM` itself** — it sets `vacuum_advised` and logs a warning
  instead, because an unattended `VACUUM` is exactly the kind of surprise an
  operator should authorize.
- **A running mask can be stopped, but not undone.** Cancel moves it to a
  terminal `cancelled` with a durable cursor, and the rows written before that
  point stay written. There is no reverse operation, anywhere, ever.
- **A preview expires.** `INSPECTOR_PREVIEW_TTL_SECS` (900) runs from the
  preview *completing*, not from the request. Confirming after that returns a
  409 and you run the preview again — the count you were shown must be the
  count you confirmed.
- **The previewed row count is an estimate, not a receipt.** It counts whole
  past days only, so rows written *today* are not in it; it does not subtract
  rows an earlier pass already masked, so re-previewing after a cancelled pass
  repeats the original number; and the pass also sweeps rows whose timestamps
  fall outside every counted day, which the count never reaches. Read it as a
  blast radius. The real number is `rows_masked` on the finished action, which
  is what the audit row and the audit CSV carry.
- **Rows written on the day you mask may want a second pass.** The retro-mask
  walks whole past days, and the sweep at the end of the pass catches rows that
  arrived *while it ran*. A row written earlier the same day, before you
  started the mask, can fall between the two. Re-run the mask the next day if
  that matters: already-masked rows are skipped, so a second pass only touches
  what the first one missed.
- **Masks and previews claim independently.** A multi-hour mask cannot starve
  previews past their TTL.
- **After an upgrade, run the migrations.** RPM upgrades do not re-run
  `sauron-migrate`. Until `000041`-`000043` are applied, forward masking is off
  deployment-wide with only a log line to say so — see
  **[Install via RPM](RPM-Install.md)**.

## Permissions

Two permissions, and neither is inherited by the role every engineer gets:

| Permission | Grants |
|---|---|
| `pii:read` | See policies, scans, findings, the audit trail; **reveal** a single value |
| `pii:manage` | Create and edit policies, start and cancel scans, preview and confirm a mask |

**Owner and Admin hold both. Developer and Viewer hold neither** — Developer
stays at 18 permissions and Viewer at 7. `pii:read` is bulk PII disclosure and
`pii:manage` is irreversible bulk destruction; a role handed to every engineer
by default should carry neither. Grant them at org scope, or at the app whose
data they cover.
