# Session management (S2)

Date: 2026-08-01
Status: designed, not implemented
Programme: S0 (email foundation) -> **S2** -> S1 (password reset) -> S3 -> S4 -> S5

## Problem

A "session" has no identity in this system. `refresh_tokens` rows are replaced
wholesale on every rotation — new `id`, new `token_hash`, new `created_at` — so
after fifteen minutes there is nothing durable left to name. `user_agent` exists
on the table and is always NULL, because all five `issue_tokens(...)` call sites
in `backend/bins/sauron-api/src/routes/auth.rs` (lines 243, 313, 383, 434, 545)
pass `None`. There is no IP, no `last_used_at`, and nothing linking an access
token to the refresh row minted beside it: `Claims.jti` is generated in
`jwt.rs:56` and read nowhere in the workspace.

Three consequences:

- **A user cannot see where they are logged in.** The API physically cannot
  answer the question; the columns do not exist.
- **A user cannot end one session.** `revoke_all_refresh_tokens_for_user` is
  all-or-nothing and is never reachable from a route.
- **Nothing anyone revokes takes effect for up to fifteen minutes.** There is no
  denylist, no `token_version`, and `jti` is never checked, so an access token
  issued before a logout, a deactivation, a password change or a family kill
  stays valid until its own `exp` — `JWT_ACCESS_TTL_SECS`, default 900s.

That third point is why S2 is sequenced before S1. S1's headline admin action is
"force this person out"; without S2 the honest dialog copy is "within fifteen
minutes", which is not what an admin dealing with a compromised account
believes they are clicking.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| Where does stable session identity live? | New `auth_sessions` table; `refresh_tokens.session_id` as a nullable FK | `session_id` on `refresh_tokens` alone — pushes a `DISTINCT ON` into every read, has nowhere to record who revoked a session that no longer has a token, and leaves the list endpoint one careless `select()` from serialising a token hash |
| How does a request know which session it belongs to? | New `sid: Option<Uuid>` access-token claim, `#[serde(default)]` | Reusing `jti` (per-token, so it breaks the identity-across-rotation property this slice exists to create); returning `sid` to the browser and comparing client-side (needs a JWT decoder the dashboard does not have) |
| How is the residual access-token window closed? | Per-replica in-process revocation snapshot, refreshed by a background Postgres poll; `AuthUser` does a pure in-memory `HashSet` read. Kill latency 900s -> ~5s | A Redis denylist (fail-open silently disables the control, fail-closed 401s the whole API on a blip; and Redis is documented at 9-19s per call when dead); `users.tokens_valid_from` (cannot express per-session granularity, and adds a pool checkout in front of every handler on a 16-connection pool); shortening `JWT_ACCESS_TTL_SECS` (15x the refresh traffic, still leaves a window) |
| Can a user revoke the session they are using? | No — `409`, with the row rendered as "This device" and no button | Treating it as a logout. Defensible, but an identical-looking button in an identical-looking row would do something categorically different, and Log out already exists in the Topbar |
| Does the admin surface list a member's devices? | No. One coarse verb: sign out everything | An admin session list would expose every member's device fingerprints and IPs to anyone who can reach this route — a privacy expansion nobody asked for, in a product simultaneously growing a PII inspector |
| New permission for admin force-logout? | Yes — mint `member:credential`, carved out of `member:manage`. S1's force-reset is gated on the same one | Reusing `member:manage`. That gate means "administer this org's membership"; ending someone's sessions and forcing a password reset act on their *credentials*, and an operator delegating member administration should not be forced to hand over the two verbs that take a person out of their own account |
| Step-up (password re-entry) on "sign out other devices"? | No. Authenticated plus a rate limiter | Re-auth stops nothing an access-token holder can already do; it costs an Argon2 verify and a second failure path on a defensive action a worried user should be able to take instantly |
| Retention for `auth_sessions`? | 30-day reaper in sauron-api's task supervisor | "No reaper needed" (the original design's growth argument is wrong — see §9); NULLing IP/UA at revocation (destroys the evidence in exactly the case the user is investigating) |

## Non-goals

- An absolute session lifetime. `expires_at` slides on every rotation, matching
  today's behaviour, so an actively-used session never expires. Capping it is a
  real security improvement and a real behaviour change; it gets its own
  decision.
- A `refresh_tokens` reaper. Growth is real (~96 rows/day per active session)
  and pre-existing. Any reaper must delete on **expiry**, never merely because a
  row is revoked — `repo::refresh_token_revocation` (repo.rs:235) reads revoked
  rows regardless of state and is the entire theft signal, so a revoked-row
  sweep silently disables replay detection for exactly the tokens most likely to
  be replayed. Too sharp an edge to bolt onto an auth slice.
- Naming a session ("Soheyb's laptop"). No column, no endpoint.
- Geo-IP enrichment. Needs a database, a lookup path and a privacy decision; the
  raw IP plus a device label answers the actual question.
- Making `logout` authenticated. It stays unauthenticated and revokes purely by
  token hash; S2 only extends it to take the session with it.

---

## 1. Migration `2026-08-01-000035_auth_sessions`

**Number.** The programme allocates migration numbers strictly in **build**
order from the last one on disk (`2026-07-30-000033_env_per_project`), and S2
is built before S1: S0 = 000034 (`mail_outbox`), **S2 = 000035**, S1 = 000036
(`password_reset_tokens`). The
date prefix must be monotone non-decreasing with NN, because
`run_pending_migrations` (`backend/crates/sauron-db/src/lib.rs:30`) drives
diesel's `MigrationHarness`, which orders by the **full** version string —
date first. A slice that lands late uses the date it lands, never the date it
was authored. Re-check `ls backend/migrations | tail -1` before writing the SQL.

### The lock cost, stated plainly

`run_pending_migrations` runs the whole of `up.sql` in **one transaction**, and
`CONCURRENTLY` is therefore unavailable. `ALTER TABLE refresh_tokens ADD COLUMN`
takes `AccessExclusiveLock` and holds it to COMMIT, and `refresh_tokens` is
written by every login, refresh, logout and password change. **This migration is
a maintenance window on the login path.** Nothing about it is a background
change.

Three costs, in order of size:

1. **The `CREATE INDEX` on `refresh_tokens`.** A full heap scan, unavoidable —
   the table has exactly one index today, `refresh_tokens_user_idx (user_id)`
   (`2026-07-12-000001_init/up.sql:169`), and nothing has ever reaped it. A
   deployment live for a year with 50 active sessions holds roughly 1.7M rows.
   Making the index **partial** does not avoid the scan; it bounds the resulting
   index to live sessions instead of one entry per historical row.
2. **The backfill `UPDATE`.** Same unavoidable scan, but the write volume is
   tiny: `revoked_at IS NULL AND expires_at > now()` matches only currently-live
   tokens, because every rotated row is already revoked. That is roughly one row
   per active session.
3. **The `ALTER TABLE` itself.** Metadata-only for a nullable column with no
   default; the FK needs a `ShareRowExclusive` on `auth_sessions`, which is
   empty.

The critique proposed bounding the backfill with
`AND created_at > now() - interval '30 days'`. **Do not.** It is redundant under
the default `JWT_REFRESH_TTL_SECS` (2 592 000s = 30 days, and
`expires_at = created_at + ttl`), and it is silently lossy if an operator raised
that TTL — live tokens outside the window would keep `session_id IS NULL`, and
their owners' current sessions would be unmanageable with no error anywhere.
`expires_at > now()` is the correct liveness predicate and it is already
minimal.

### up.sql

Open with the prose comment the house style requires, and put the lock warning
in it — copy the register of
`2026-07-28-000028_issue_env_covering_index/up.sql:64`, which says out loud that
CONCURRENTLY is unavailable and the change needs a maintenance window.

```sql
CREATE TABLE auth_sessions (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at     TIMESTAMPTZ NOT NULL,
  user_agent     TEXT,
  ip             TEXT,
  revoked_at     TIMESTAMPTZ,
  revoked_reason TEXT,
  revoked_by     UUID REFERENCES users(id) ON DELETE SET NULL,
  CONSTRAINT auth_sessions_revoked_reason_check CHECK (
    revoked_reason IS NULL OR revoked_reason IN (
      'logout','user_revoked','user_revoked_others','admin_revoked',
      'password_changed','deactivated','reuse',
      'password_reset','reset_forced')
  )
);

CREATE INDEX auth_sessions_user_live_idx
  ON auth_sessions (user_id) WHERE revoked_at IS NULL;

CREATE INDEX auth_sessions_revoked_idx
  ON auth_sessions (revoked_at) WHERE revoked_at IS NOT NULL;

ALTER TABLE refresh_tokens
  ADD COLUMN session_id UUID REFERENCES auth_sessions(id) ON DELETE SET NULL;

INSERT INTO auth_sessions (id, user_id, created_at, last_used_at, expires_at, user_agent)
  SELECT r.id, r.user_id, r.created_at, r.created_at, r.expires_at, r.user_agent
    FROM refresh_tokens r
   WHERE r.revoked_at IS NULL AND r.expires_at > now();

UPDATE refresh_tokens r SET session_id = r.id
 WHERE r.revoked_at IS NULL AND r.expires_at > now();

CREATE INDEX refresh_tokens_session_idx
  ON refresh_tokens (session_id) WHERE session_id IS NOT NULL;

UPDATE roles SET permissions = permissions || '["member:credential"]'::jsonb
 WHERE permissions @> '["member:manage"]'::jsonb
   AND NOT (permissions @> '["member:credential"]'::jsonb);
```

That last statement seeds the permission §7 mints, and it rides in this migration
rather than taking a number of its own — the programme allocates S1 the next one
and a second S2 migration would push every later slice's number by one for a
three-line `UPDATE`.

Its predicate is `member:manage` **holders**, not the preset names
`2026-07-15-000015_source_read_perm/up.sql` matched. `member:credential` is
carved out of `member:manage` rather than added beside it, so every role that
holds `member:manage` today can already sign a member out via
deactivate-then-reactivate; matching on the preset names would silently strip
that from every custom role an operator has built while leaving Owner and Admin
whole. `ensure_preset_roles` re-syncs Owner and Admin from `rbac.rs` at api
startup regardless, so the presets are covered twice and the custom roles only
here.

Column-by-column reasoning worth keeping in the migration comment:

- `id` is the stable identity rotation destroys today, and is what goes into the
  JWT `sid` claim.
- `last_used_at` is stamped on **rotation**, not per request, so "last used" is
  accurate only to within `JWT_ACCESS_TTL_SECS`. A session used 30 seconds ago
  can display as "up to 15 minutes ago". Do not let a reviewer "fix" this by
  writing on every request — that turns a read-only auth path into a write on
  every API call.
- `expires_at` mirrors the newest refresh token's expiry (sliding, matching
  today) so liveness needs no join.
- `revoked_by` is `ON DELETE SET NULL`, not CASCADE: deleting the admin must not
  delete the victim's audit row.
- The CHECK deliberately **excludes** `'rotated'`. A rotation revokes a token,
  never a session, so writing `'rotated'` here is a bug the database catches.
  Note that `refresh_tokens.revoked_reason` has no CHECK (migration 22), so the
  two columns share a vocabulary and only one enforces it. **This CHECK is a
  deploy coupling**: adding a reason in code without a widening migration
  produces a 500 on the revoke path. That is the intended trade, and it must be
  in the doc comment of the reason constants (§4) or someone will discover it in
  production.
- `'password_reset'` and `'reset_forced'` are in the list from day one even
  though nothing in S2 writes them. They belong to S1, which lands next and
  revokes sessions on both of its reset paths — one of them unauthenticated
  self-service. If they arrived with S1 instead, every successful reset would
  500 at the revoke step until a second migration caught up, and the failure
  would land on a user who has just proved they cannot get into their account.
  Widening the list here costs nothing — the table is created empty in this same
  transaction — where doing it later is a second migration against a live
  `auth_sessions`. S1 ships no migration of its own against this constraint.

`refresh_tokens.session_id` is **`ON DELETE SET NULL`, not CASCADE**. CASCADE
would pre-authorise the exact failure the non-goals warn about: deleting one
`auth_sessions` row would take that session's whole token history with it, and
`refresh_token_revocation` would then find nothing and treat a replayed token as
"never existed" — a plain 401, no family kill, no WARN. The §9 reaper deletes
`auth_sessions` rows by design, so this is not hypothetical. Put the reason in
the migration prose.

The index on `auth_sessions` is `(user_id) WHERE revoked_at IS NULL` and
deliberately **does not** include `last_used_at`. Indexing it would make every
rotation a non-HOT update — `ON CONFLICT ... DO UPDATE SET last_used_at = now()`
would rewrite the heap tuple *and* both index entries, leaving two dead versions
for autovacuum, on the hottest-updated column in the table. With only `id` and
`user_id` indexed and neither changing on rotation, the update is HOT-eligible
and the page self-vacuums. The ordering the index would have provided buys
nothing: the query is scoped to one `user_id`, capped at 200 rows, and a real
account has single-digit live sessions.

`auth_sessions_revoked_idx` serves both the revocation poller (§5) and the
30-day history query (§6). The §9 reaper is what keeps it small enough that the
history query needs no `user_id` support: the whole partial index is exactly the
last 30 days of revocations.

Add a one-line TODO in the prose naming `refresh_tokens` unbounded growth. S2 is
the slice that makes it materially worse — a second index means more write
amplification and more disk on the workspace's fastest-growing never-pruned
table.

### down.sql

```sql
UPDATE roles SET permissions = permissions - 'member:credential';
DROP INDEX IF EXISTS refresh_tokens_session_idx;
ALTER TABLE refresh_tokens DROP COLUMN IF EXISTS session_id;  -- drops the FK with it
DROP INDEX IF EXISTS auth_sessions_revoked_idx;
DROP INDEX IF EXISTS auth_sessions_user_live_idx;
DROP TABLE IF EXISTS auth_sessions;
```

Order is load-bearing: the referencing column must go before the referenced
table. This is a real inverse; it loses session history, which is acceptable
because the pre-migration system had none. The permission is stripped
unconditionally rather than only from `member:manage` holders, because a role
edited between the up and the down could hold it without the other.

### schema.rs and models.rs

Three hand edits to `backend/crates/sauron-db/src/schema.rs`; the diesel CLI must
never run. S2's `diesel::table!` delta is **+1**, measured against whatever the
count is when S2 lands — never an absolute, because every slice in this
programme adds tables and a number pinned here is wrong for everyone who lands
after the first.

1. A new `diesel::table! { auth_sessions (id) { ... } }` block inserted
   alphabetically before `analytics_events`, fields in migration order.
2. `session_id -> Nullable<Uuid>,` appended as the **last** field of the
   existing `refresh_tokens` block (schema.rs:213), matching the column order
   the migration produces.
3. `auth_sessions,` added to `allow_tables_to_appear_in_same_query!`.

Plus one joinable next to the existing line 486:
`diesel::joinable!(auth_sessions -> users (user_id));`. Deliberately **not**
`joinable!(refresh_tokens -> auth_sessions (session_id))` and not a second users
association for `revoked_by`: diesel allows one association per table pair, no
query in this slice joins the two tables in the DSL (all multi-table work is raw
CTEs), and an unused joinable is a future ambiguous-join trap.

`models.rs` gains `AuthSession` next to `RefreshToken` (models.rs:500) with
`#[derive(Debug, Clone, Queryable, Selectable)]` and **no `Serialize`** — the
same discipline as `RefreshToken`. The API returns a hand-built `SessionView`;
the model never reaches the wire. Add `pub session_id: Option<Uuid>` to
`RefreshToken` (field order must match the `table!` block). Delete
`NewRefreshToken` and `repo::insert_refresh_token` **together** — inserts now go
through the CTE in §3, and leaving a second, session-blind mint path is how the
two tables come to disagree.

## 2. Session identity in the access token

`Claims` (`backend/crates/sauron-auth/src/jwt.rs:14`) gains:

```rust
/// The `auth_sessions.id` this token was minted for. `Option` + serde(default)
/// because tokens issued before this field existed must keep decoding across
/// the deploy — the same reason `must_change_password` is defaulted, and
/// `tokens_minted_before_the_flag_existed_still_decode` is the pin.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub sid: Option<Uuid>,
```

Signature change:

```rust
pub fn issue_access(
    &self,
    user_id: Uuid,
    must_change_password: bool,
    session_id: Option<Uuid>,
) -> anyhow::Result<(String, i64)>
```

`jti` stays as-is and stays unread. It is per-token; `sid` is per-session, and
conflating them breaks the identity-across-rotation property this slice exists
to create.

**Thirteen call sites, not three.** The signature change breaks
`cargo test --workspace` compilation before any test runs unless all of them are
updated — including `env_scoped_member_is_confined_over_http`, the pinned
env-scoping guard:

| File | Lines |
|---|---|
| `backend/bins/sauron-api/src/routes/mod.rs` | 105 (production) |
| `backend/crates/sauron-auth/src/jwt.rs` | 93, 103, 106 (unit tests) |
| `backend/bins/sauron-api/tests/http_workflows.rs` | 378, 381 |
| `backend/bins/sauron-api/tests/http_env_scoping.rs` | 701, 704, 707, 710, 713, 1017, 2125 |

Every test site passes `None`, and that is the semantically required choice, not
just the cheap one: those tests mint bearer tokens in-process and never create
an `auth_sessions` row, so a synthetic `Some(uuid)` would produce a `sid` no row
backs — harmless for `contains`, but it would make any future admin-revoke
assertion silently pass against a session that does not exist.

### Pre-migration tokens

An access token minted before the deploy has no `sid`. Such tokens are
**accepted unchanged**: the extractor has no session to check, every row in
their session list shows `current: false`, and the two self-service revoke
endpoints refuse (§6). This cannot last more than `JWT_ACCESS_TTL_SECS` past the
deploy, because `validate_exp` is on and every login and refresh mints a `sid` —
a <=15-minute condition, not a permanent mode.

Rejecting them instead would sign out every logged-in user at deploy, which is
precisely the failure the existing legacy-decode test was written to prevent.
Running `revoke-others` for them (rather than refusing) is the unsafe direction:
with no `sid` there is nothing to spare, so "revoke others" would silently
become "revoke everything, including the tab you are looking at".

The migration backfill means those users see their sessions listed immediately
rather than an empty page — but no row is badged "This device" until their next
refresh. Documented, not fixed.

## 3. Minting and continuing a session

### `repo::start_or_continue_session`

Replaces `insert_refresh_token` (repo.rs:171). One data-modifying CTE via
`diesel::sql_query` + `.bind()`; no `conn.transaction` (MSRV 1.82).

```rust
pub async fn start_or_continue_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
) -> QueryResult<usize>
```

```sql
WITH s AS (
  INSERT INTO auth_sessions (id, user_id, expires_at, user_agent, ip)
  VALUES ($1,$2,$3,$4,$5)
  ON CONFLICT (id) DO UPDATE
     SET last_used_at = now(),
         expires_at   = EXCLUDED.expires_at,
         user_agent   = COALESCE(auth_sessions.user_agent, EXCLUDED.user_agent),
         ip           = COALESCE(auth_sessions.ip, EXCLUDED.ip)
   WHERE auth_sessions.revoked_at IS NULL AND auth_sessions.user_id = $2
  RETURNING id)
INSERT INTO refresh_tokens (user_id, token_hash, expires_at, session_id)
SELECT $2,$6,$3,s.id FROM s;
```

Three things in that SQL are load-bearing:

**`WHERE auth_sessions.revoked_at IS NULL` is the only thing stopping the
rotation grace window from resurrecting a killed session.** `REFRESH_RACE_GRACE`
is 10 seconds (`auth.rs:37`) and exists because two dashboard tabs share one
localStorage refresh token. Concretely: session B rotates at T (old token ->
`rotated`); the user revokes B at T+2s; B's other tab presents the old token at
T+3s. The reason *is* `rotated`, it *is* inside the grace, and
`user_has_active_refresh_token(user_id)` is *true* because the spared session A
is live — so `raced` is true and `issue_tokens` runs. Postgres's
`DO UPDATE ... WHERE` skips the update, `RETURNING` yields nothing, the outer
INSERT inserts nothing, and `.execute()` returns 0. A Rust-side pre-check would
be a TOCTOU race and must not be substituted. If a future refactor moves the
session upsert out of this CTE, the hole silently reopens.

**The COALESCE order is inverted from the obvious.** `EXCLUDED` is the *current
rotator's* values, so `COALESCE(EXCLUDED.x, auth_sessions.x)` would overwrite
the row every 15 minutes. The UI renders these next to `created_at` as
"Device | IP | Signed in" — as login-time facts. If an attacker steals a refresh
token and rotates it, last-writer-wins destroys the original login IP and user
agent, and the row becomes visually indistinguishable from before. Since the
sole justification for returning an unmasked IP is "was that login me?", the
login-time values must win. If a "currently seen from" value is ever wanted, it
gets its own columns and its own label — never a mutation of the originals.

**`user_id = $2` in the WHERE is defensive.** It makes a mis-threaded
`session_id` fail rather than cross-link two users' tokens.

The outer INSERT deliberately **omits `user_agent`**. Writing the sanitized UA
to `refresh_tokens` on every rotation (~96 times a day per session) persists
~120 bytes that nothing reads — `list_live_sessions` is designed never to touch
that table, and the same string is already on the session row. On the
workspace's fastest-growing never-pruned table that roughly doubles the on-disk
row. `refresh_tokens.user_agent` stays permanently NULL, which is what it
already is.

### `SessionContext` and `issue_tokens`

In `backend/bins/sauron-api/src/routes/mod.rs` (issue_tokens at :96-116):

```rust
pub(crate) struct SessionContext {
    /// `None` starts a new session; `Some` continues one across a rotation.
    pub session_id: Option<Uuid>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

pub(crate) async fn issue_tokens(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    sess: SessionContext,
    must_change_password: bool,
) -> Result<TokenPair, ApiError>
```

A struct rather than three more positional parameters: seven arguments is at
`clippy::too_many_arguments`' threshold, and
`Option<Uuid>, Option<String>, Option<String>` in a row is a call-site
transposition waiting to happen.

The body order **changes** — today it mints the JWT first:

1. `let session_id = sess.session_id.unwrap_or_else(Uuid::new_v4);` — generated
   in Rust, not by the DB default, because the JWT needs it and a `RETURNING`
   round trip would not be atomic with the token insert.
2. `repo::start_or_continue_session(...)`. Zero rows with
   `sess.session_id.is_some()` -> `ApiError::Auth(AuthError::InvalidToken)` (the
   session was revoked mid-flight). Zero rows with `is_none()` ->
   `ApiError::Internal` (a fresh INSERT cannot conflict, so zero means something
   is genuinely wrong).
3. `state.keys.issue_access(user_id, must_change_password, Some(session_id))`.

Minting last means a token is never handed out for a session that failed to
persist.

Also in `routes/mod.rs`:

```rust
pub(crate) const MAX_USER_AGENT_LEN: usize = 400;
pub(crate) fn sanitize_ua(h: &HeaderMap) -> Option<String>;   // USER_AGENT, to_str, trim, reject empty, truncate by chars
pub(crate) fn sanitize_ip(raw: &str) -> Option<String>;       // raw.parse::<IpAddr>().ok().map(|a| a.to_string())
```

The IP validation is not cosmetic. With `API_TRUST_FORWARDED_HEADERS=1` the
value comes from a client-controlled `X-Forwarded-For`, so parsing it as an
`IpAddr` and storing the canonical form (else NULL) removes an
arbitrary-string-into-the-database vector.

### The five call sites

| Site | `SessionContext` |
|---|---|
| `register` (auth.rs:243) | `session_id: None` — a new session per login |
| `login` (auth.rs:313) | `session_id: None` |
| `refresh`, race path (auth.rs:383) | `session_id: Some(sid)`, **rejecting `None`** — see below |
| `refresh`, rotate path (auth.rs:434) | `session_id: token.session_id` |
| `change_password` (auth.rs:545) | `session_id: None` |

All five pass `user_agent: sanitize_ua(&headers)` and
`ip: sanitize_ip(&client_addr(&headers, &peer, &state))`. `register` and `login`
already take `headers` and `ConnectInfo(peer)`. `change_password`'s signature
gains both (all `FromRequestParts`, so any order before the `Json` body
extractor) — without them the user's post-password-change session shows "Unknown
device" in their own list, which is the exact row they will look at first.

The race path only has the hash, so `repo::refresh_token_revocation` widens to
return the session:

```rust
QueryResult<Option<(Uuid, Option<Uuid>, Option<DateTime<Utc>>, Option<String>)>>
// (user_id, session_id, revoked_at, revoked_reason)
```

**When that `session_id` is `None`, the race path must return
`InvalidToken`, not start a new session.** Letting it degrade to a fresh login
defeats the centrepiece guard, because `WHERE revoked_at IS NULL` is only
reachable when a session id is present. The reachable case is a rolling upgrade
— the RPM ships api and dashboard as separate subpackages, so partial upgrades
happen: an old replica mints a `session_id NULL` token after 000035 has run,
rotates it at T, the user presses "Sign out other devices" at T+2s, and at T+5s
that device's other tab lands in the grace window with `raced` true and gets a
brand-new live session. The kill silently failed for that device, and the new
session appears in the list looking like a legitimate login. Rejecting cannot
harm the legitimate pre-migration case: a row revoked before the migration is by
definition more than 10 seconds old and never satisfies the grace condition.

Extend `user_has_active_refresh_token`'s doc comment. Its stated rationale
("after a family kill there are none") stops holding the moment one session can
be revoked while others live.

## 4. Revocation at the repo layer

All in `backend/crates/sauron-db/src/repo.rs`, beside the existing token
helpers. Each returns the ids it revoked so the calling replica can update its
own snapshot without waiting for a poll.

```rust
pub async fn revoke_session(conn, session_id: Uuid, user_id: Uuid, reason: &str, actor: Option<Uuid>)
    -> QueryResult<Vec<Uuid>>;
pub async fn revoke_sessions_for_user(conn, user_id: Uuid, except: Option<Uuid>, reason: &str, actor: Option<Uuid>)
    -> QueryResult<Vec<Uuid>>;
pub async fn revoke_refresh_token_and_session(conn, token_hash: &str, reason: &str)
    -> QueryResult<Option<Uuid>>;
pub async fn list_sessions(conn, user_id: Uuid, include_revoked: bool) -> QueryResult<Vec<AuthSession>>;
pub async fn revoked_session_ids(conn, window_secs: i64) -> QueryResult<Vec<Uuid>>;
pub async fn prune_auth_sessions(conn, days: i64) -> QueryResult<usize>;
```

**Every one of the revoke fns must make the `auth_sessions` UPDATE the
row-count-bearing statement.** Postgres reports the command tag of the *outer*
statement, so the obvious shape —
`WITH s AS (UPDATE auth_sessions ... RETURNING id) UPDATE refresh_tokens ... FROM s`
read with `.execute()` — returns the number of *token* rows touched. A live
session that currently has no live refresh token would then revoke successfully
in the database while the handler answered 404, and because `mark_revoked` only
runs on the success branch, the killed session's access token would keep working
for the full 900s. That state is reachable today and is *created* by this slice:
before §8 converts it, `set_member_active` kills tokens without touching
sessions, so a deactivate-reactivate cycle leaves exactly that shape.

So the shape is always: session UPDATE in one CTE arm, token UPDATE in a second
arm, `SELECT id FROM s` as the primary query, read with
`.get_results::<RevokedSessionRow>(conn)` where `RevokedSessionRow` is a
`#[derive(QueryableByName)]` with one `#[diesel(sql_type = SqlUuid)] pub id: Uuid`.
Data-modifying CTE arms execute exactly once and to completion whether or not
the primary query reads them, which is why the token arm can be a bare
`RETURNING r.id` nobody selects.

```sql
-- revoke_session
WITH s AS (
  UPDATE auth_sessions SET revoked_at=now(), revoked_reason=$3, revoked_by=$4
   WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL RETURNING id),
t AS (
  UPDATE refresh_tokens r SET revoked_at=now(), revoked_reason=$3
    FROM s WHERE r.session_id=s.id AND r.revoked_at IS NULL RETURNING r.id)
SELECT id FROM s;
```

`user_id = $2` is the ownership check. It is why the handler needs no separate
SELECT, why there is no window between check and write, and why one user can
never revoke another's session by guessing a uuid. An empty result means absent,
already revoked, or someone else's — all mapped to 404, never 403, so the
response cannot be used to probe which session ids exist.

```sql
-- revoke_sessions_for_user
WITH s AS (
  UPDATE auth_sessions SET revoked_at=now(), revoked_reason=$2, revoked_by=$3
   WHERE user_id=$1 AND revoked_at IS NULL AND ($4::uuid IS NULL OR id <> $4)
  RETURNING id),
t AS (
  UPDATE refresh_tokens r SET revoked_at=now(), revoked_reason=$2
   WHERE r.user_id=$1 AND r.revoked_at IS NULL
     AND ($4::uuid IS NULL OR r.session_id IS DISTINCT FROM $4)
  RETURNING r.id)
SELECT id FROM s;
```

The token arm expresses the sparing rule **directly** rather than as
`session_id IN (SELECT id FROM s)`. The `IN` form silently skips live tokens
whose session was already revoked — reachable, because the refresh race path
issues a new token without revoking the presented one (so one session can hold
two live tokens) and `logout` revokes one hash while revoking the session. Those
leftovers are inert for minting, but they make `user_has_active_refresh_token`
return true after an account-wide kill, weakening the invariant the grace window
depends on. `IS DISTINCT FROM` also gives the right NULL semantics: session-less
legacy tokens are still killed by "revoke others", which is correct — a user
asking to kill their other devices means those too.

```sql
-- revoke_refresh_token_and_session (logout)
WITH t AS (
  UPDATE refresh_tokens SET revoked_at=now(), revoked_reason=$2
   WHERE token_hash=$1 AND revoked_at IS NULL RETURNING session_id),
s AS (
  UPDATE auth_sessions a SET revoked_at=now(), revoked_reason=$2
    FROM t WHERE a.id=t.session_id AND a.revoked_at IS NULL RETURNING a.id)
SELECT id FROM s;
```

Today `logout` revokes one token by hash and leaves nothing else, so the
logged-out session would stay live in the owner's list forever — dead token,
live row. The `AND revoked_at IS NULL` guard is a small, deliberate behaviour
change from the existing `revoke_refresh_token`: without it, logging out with an
already-rotated token rewrites `revoked_reason` from `rotated` to `logout`, and
the 10-second grace window (which fires only on `rotated`) stops firing for the
other tab. This does not widen logout's authorization surface — whoever holds
the raw refresh token could already revoke it.

`list_sessions` is plain diesel DSL and **never touches `refresh_tokens`**: that
is the structural reason `token_hash` cannot leak through the session endpoint,
because the column is not in the query's source table. Live arm:
`user_id.eq(..)`, `revoked_at.is_null()`, `expires_at.gt(Utc::now())`, ordered
`last_used_at.desc()`, `.limit(MAX_SESSIONS_LISTED)` with
`pub const MAX_SESSIONS_LISTED: i64 = 200`. With `include_revoked`, it also
returns rows where `revoked_at >= now() - 30 days`, served by
`auth_sessions_revoked_idx`.

### The reason registry

Three new constants beside the existing five (repo.rs:205-215):

```rust
pub const REVOKE_USER_REVOKED: &str = "user_revoked";
pub const REVOKE_USER_REVOKED_OTHERS: &str = "user_revoked_others";
pub const REVOKE_ADMIN: &str = "admin_revoked";

/// Reasons that mean a human deliberately ended the session. Presenting a token
/// revoked for one of these is NOT evidence of theft and must not trip the
/// family kill in `refresh`.
///
/// This is "deliberate, not theft" — not "every reason". `REVOKE_ROTATED` and
/// `REVOKE_REUSE` must never appear here: rotation would take the early-return
/// path on every ordinary refresh and break the 10-second multi-tab grace
/// window, and reuse is the theft signal itself.
///
/// Adding a reason the `auth_sessions_revoked_reason_check` CHECK does not
/// already list also needs a widening migration, or the revoke path 500s.
pub const DELIBERATE_REVOKE_REASONS: [&str; 3] =
    [REVOKE_USER_REVOKED, REVOKE_USER_REVOKED_OTHERS, REVOKE_ADMIN];
```

The array is `[&str; 3]` as S2 ships it and becomes `[&str; 5]` when S1 lands its
two reset reasons; the length is part of the edit S1 owns.

Two pins, both unit tests with no database:

- **Classification.** Every `REVOKE_*` constant must fall into exactly one of
  three buckets — deliberate, has-its-own-branch-in-`refresh`
  (`REVOKE_ROTATED`, `REVOKE_DEACTIVATED`), or theft-signal (`REVOKE_REUSE`,
  `REVOKE_LOGOUT`). The test enumerates every constant in the module — eight as
  S2 ships, ten after S1 — and fails on an unclassified one, so a new reason
  cannot be added without someone choosing a bucket for it.
- **CHECK parity.** `include_str!` the migration's `up.sql` and assert every
  reason constant except `REVOKE_ROTATED` appears in it. That is the cheapest
  possible defence against the deploy coupling, and it is what will catch S1 if
  it ever renames one of the two reset reasons §1 pre-seeded.

**Hand-off to S1.** S1 adds two reasons: `REVOKE_PASSWORD_RESET`
(`password_reset`, a reset link was consumed) and `REVOKE_RESET_FORCED`
(`reset_forced`, an admin forced a reset). Since S2 lands first, S1 owns
two coupled edits: the constants themselves, and their membership in
`DELIBERATE_REVOKE_REASONS`, which grows to `[&str; 5]`. **No migration** — §1
already pre-seeded both values into the CHECK, precisely so that S1's
unauthenticated reset path cannot 500 while a widening migration is pending.
What S1 can still get wrong is the second edit: miss it and the target's
still-live refresh token lands in the theft branch ~15 minutes later and fires
the family kill — the exact poisoning bug §8 exists to prevent. S1's admin reset
must also call `repo::revoke_sessions_for_user`, never
`revoke_all_refresh_tokens_for_user_with_reason`, or `auth_sessions` desyncs
again.

## 5. Closing the residual access-token window

### `SessionRevocations`

New module `backend/crates/sauron-auth/src/revocations.rs`, exported from
`lib.rs`. No new crate dependencies.

```rust
#[derive(Clone, Default)]
pub struct SessionRevocations { inner: Arc<RwLock<Snapshot>> }

struct Snapshot {
    polled: HashSet<Uuid>,
    local: HashMap<Uuid, Instant>,
    refreshed_at: Option<Instant>,
}
```

| Method | Behaviour |
|---|---|
| `contains(&self, sid: &Uuid) -> bool` | Pure memory read of `polled` union the keys of `local`. No I/O, ever |
| `mark_revoked(&self, ids: &[Uuid])` | Records ids in `local` with `Instant::now()` |
| `replace(&self, ids: HashSet<Uuid>, poll_started_at: Instant)` | Swaps `polled`, evicts `local` entries marked **strictly before** `poll_started_at` |
| `age(&self) -> Option<Duration>` | Time since the last successful poll; `None` before the first |
| `refresh(&self, pool, window_secs) -> anyhow::Result<usize>` | One poll |

**The eviction rule is the subtle part.** Expressing retention as wall-clock age
against the poll interval is wrong: a locally-marked id is only certain to be in
a poll's result if that poll's *query started after* the mark. A poll that
begins at T-1s, a revocation at T, and a slow finish at T+6s would evict a
5-second-old local entry using a snapshot that never contained it — and the
revoked session's access token would be honoured again on that replica until the
next poll. A security control silently ceasing to hold, on exactly the axis this
slice exists to establish. So `refresh` records `Instant::now()` before issuing
the query and hands it to `replace`.

Both guards use `unwrap_or_else(|p| p.into_inner())`, matching
`local_rate_limit_ok` in `routes/auth.rs` and for the same reason it gives:
`contains` runs inside `AuthUser::from_request_parts`, i.e. on every
authenticated request in all 18 route files, and a naive `.unwrap()` would turn
one transient panic under the write guard into a total API outage. `replace`
uses `std::mem::replace` and drops the old set **outside** the guard, so freeing
a large set does not block request tasks on `contains`.

### The poll

```sql
SELECT id FROM auth_sessions
 WHERE revoked_at >= now() - ($1 || ' seconds')::interval
 LIMIT 50000
```

The cutoff is computed **in the database**, binding the window as seconds and
using the interval-binding pattern of `prune_checks` / `prune_alert_events`
(repo.rs:6072, 6090). Computing `Utc::now() - window` in Rust would make the
control depend on API-vs-Postgres clock skew: `revoked_at` is written by
Postgres `now()`, so an API host running ahead by more than the slack would drop
recently-revoked sessions from its snapshot and silently re-enable their access
tokens — invisibly, because the poll succeeded and `age()` still looks fresh.

The window is computed once at startup and **floored independently of the
configured TTL**:

```rust
// Floored at 900 on purpose. The correctness argument — "a token minted before
// a revocation older than the access TTL has already expired on its own exp" —
// only holds if the TTL never DECREASES. An operator hardening 900 -> 120 and
// restarting leaves pre-restart tokens alive for 900s against a 240s window:
// ~11 minutes of accepted-but-revoked access, with no error and no log.
// Clamped above because JWT_ACCESS_TTL_SECS is an unvalidated i64 from the
// environment; `parse()` has no floor, no ceiling and no sign check, and a
// negative value cast to u64 wraps to ~1.8e19.
let window_secs = state.cfg.jwt_access_ttl_secs.clamp(900, 86_400) + 120;
```

The `LIMIT` matters for the same class of reason: without it, a plausible
`JWT_ACCESS_TTL_SECS=604800` plus any bulk event (an offboarding script, a
deactivation sweep) has every replica materialising an enormous set 17 280 times
a day and swapping it into a lock every authenticated request reads. On hitting
the limit, log `tracing::error!` — a silently truncated snapshot is a security
control that has stopped working while reporting healthy.

The poll checks out a pooled connection, runs the one query, and **drops the
connection before** swapping the snapshot. The API pool is 16 for the whole
process; a background task must never hold a slot across work it does not need
one for.

### Wiring: no `?` at boot

The poller is mounted in **S0's `backend/bins/sauron-api/src/tasks.rs`
supervisor** — the one named-task runner with respawn-on-panic, capped backoff,
and a per-task `last_success` age rendered by `/health`. S2 adds no second
background-task pattern.

**There is no synchronous pre-bind `revocations.refresh(&pool, window).await?`.**
That was the original design and it is a boot-time footgun on this deployment's
default upgrade path: `packaging/rpm/systemd/sauron-migrate.service` has no
`[Install]` section (run on demand only), while `sauron.spec`'s `%postun server`
runs `%systemd_postun_with_restart sauron-api.service`, so `dnf upgrade` restarts
the new binary against the old schema every time. A `?` on
`relation "auth_sessions" does not exist` propagates out of `main`,
`sauron-api.service` is `Restart=on-failure` with `RestartSec=2` and no
`StartLimit*` override, systemd's default burst is exhausted in ~10 seconds, and
the unit lands in `failed` and stays there. The operator loses `/health`, every
read route and the whole dashboard backend — the exact surface they would use to
diagnose it.

So the snapshot starts **empty** and the supervisor retries. The cost is one
poll interval of stale revocation data on a cold start, which is strictly
smaller than the 900-second window that exists today. Rejected explicitly:
"start empty but only after logging an error and continuing" is what this is;
"fail closed at boot" is what it is not, because failing closed here means
failing closed on `/health` too.

`AppState` gains `pub revocations: sauron_auth::SessionRevocations` and one
`impl FromRef<AppState> for SessionRevocations` next to the existing `JwtKeys`
impl (main.rs:57). **Sequencing:** S0 lands `mail: Option<MailSender>` on
`AppState` first (additive, no extractor change); S2 rebases onto it.

### The extractor

`backend/crates/sauron-auth/src/extractors.rs`. The impl bound becomes
`where S: Send + Sync, JwtKeys: FromRef<S>, SessionRevocations: FromRef<S>`.
Verified blast radius: `AuthUser` is used only with sauron-api's `AppState`, so
this is two files, and no handler signature changes.

Inside `from_request_parts`, **after** `Uuid::parse_str(&claims.sub)` and
**before** `password_change_gate`:

```rust
if let Some(sid) = claims.sid {
    if SessionRevocations::from_ref(state).contains(&sid) {
        return Err(AuthError::InvalidToken);
    }
}
```

Order matters: a revoked session must 401 on **every** path including
`/v1/auth/password`, or a revoked temp-password holder could still change the
password. `password_change_allowed_path` and its pinned test
`password_change_allowlist_is_exactly_two_paths` are **unchanged** — no new path
joins the allowlist. A temp-password holder is correctly blocked from every new
endpoint; changing the password nukes all their sessions anyway, which is a
superset of what these endpoints do.

`AuthError::InvalidToken` (401 `invalid_token`) is the right code and needs zero
dashboard changes: the 401 interceptor calls `runRefreshOnce()`, whose refresh
row is also revoked, so `/v1/auth/refresh` 401s and `onRefreshFailure()` sends
the user to `#/login`.

Put the consequence in `SessionRevocations`' doc comment, not just here: any
future binary that wants `AuthUser` must now supply a snapshot, and supplying a
permanently-empty one silently disables revocation for that service.

## 6. Endpoints

New module `backend/bins/sauron-api/src/routes/account.rs`; `pub mod account;`
added to `routes/mod.rs`. Four `.route(...)` lines in `main.rs`:

```rust
.route("/v1/me/sessions",                get(routes::account::list_sessions))
.route("/v1/me/sessions/{session_id}", delete(routes::account::revoke_session))
.route("/v1/me/sessions/revoke-others",  post(routes::account::revoke_other_sessions))
.route("/v1/orgs/{org_id}/members/{user_id}/revoke-sessions",
                                         post(routes::orgs::revoke_member_sessions))
```

Verified: `/v1/sessions` is product telemetry (`GET /v1/apps/{app_id}/sessions`)
and is untouched; `/v1/me` had exactly one route (main.rs:155). None of the new
paths match `/v1/apps/{app_id}/...`, so `routes::scope::reject_environment_id`
is not required and `tests/http_env_scoping.rs`'s router enumeration (which
filters on `/v1/apps/{app_id}` plus `get(`) does not see them.

`rate_limit` and `client_addr` in `routes/auth.rs` both become `pub(crate)`, **in
place** — they stay in `auth.rs`, they do not move to a new shared module. S2's
own limiters key on the user id and need only the first, but every later slice in
the programme needs the second from a different file: S1 populates
`requested_from` from `routes/orgs.rs`, S3 keys its unsubscribe limiter on the
caller's address, S5 records `confirm_source`. Widening both now costs two
keywords; leaving `client_addr` private means three slices each reach for the
same refactor of the same file, and the last two land on a moved function.

Give `rate_limit` a doc comment establishing the key convention
`sauron:{area}:{action}:{principal}` — four slices are about to invent limiter
keys, and this turns "the repo has no read-route rate limiting" into "here is how
to add the next one".

### `GET /v1/me/sessions`

```rust
#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub current: bool,
    pub user_agent: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,
    pub device_kind: Option<String>,
    pub ip: Option<String>,
    /// Present only when `?include_revoked=1`; NULL for live rows.
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
}

pub async fn list_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<ListSessionsQuery>,   // { include_revoked: Option<bool> }
) -> Result<Json<Vec<SessionView>>, ApiError>
```

`current: Some(row.id) == auth.claims.sid`, marked **server-side** so the
dashboard never decodes a JWT (it has no `jwt-decode` dependency and should not
gain one). No permission check beyond `AuthUser` — a user's own sessions are the
definition of "any authenticated user", the same class as `/v1/me`.

`revoked_by` is **never** serialized. Surfacing it would tell a member which
admin signed them out; surfacing `revoked_at` and `revoked_reason` is what makes
the destructive action observable to the person it happened to, which is the
whole point of writing those columns.

The IP is returned **unmasked**, not through `serialize_masked_ip`. This is the
caller's own data, and `192.168.x.x` defeats the entire "was that login me?"
purpose. Masking is a telemetry-PII tool; the admin surface deliberately returns
no session data at all.

**UA parsing** is server-side, using `woothee::parser::Parser::new().parse(ua)`
-> `(r.name, r.os, r.category)`. woothee is already this product's UA
vocabulary — `sauron_pipeline::enrich::enrich_context` parses every ingested UA
with it — and parsing the same string a second way in the same product would
give two different names for the same browser in two places in the same UI. Add
`woothee.workspace = true` to `backend/bins/sauron-api/Cargo.toml`; it is
already a workspace dependency (backend/Cargo.toml:119) declared only by
sauron-pipeline, so this is a declaration, not a new third-party dependency, and
the RPM's vendored-crate story is unchanged.

The gotcha the implementation must handle: **woothee returns the literal string
`"UNKNOWN"` and/or `""`** for fields it cannot determine, so a
`fn norm(s: &str) -> Option<String>` must map both to `None` or every
unrecognised UA renders as "UNKNOWN on UNKNOWN". `parse_ua` is pure and unit
tested in the same file. Instantiating the `Parser` per call is fine at <=200
rows on a rarely-hit endpoint; do not add a `OnceLock` for it.

### `DELETE /v1/me/sessions/{session_id}`

Guards, in order:

1. `rate_limit(&state, &format!("sauron:auth:sessions:{}", auth.user_id), 20, 60)`.
2. `auth.claims.sid == Some(session_id)` -> `409 conflict`, "cannot revoke the
   session you are using — use Log out instead".
3. `repo::revoke_session(&mut conn, session_id, auth.user_id, REVOKE_USER_REVOKED, Some(auth.user_id))`;
   empty result -> `404`.

On success: `state.revocations.mark_revoked(&ids)`,
`tracing::warn!(%user_id, %session_id, "session revoked by user")`, and
`{"ok": true, "revoked": 1}`. The log line is not optional — this is a
destructive account action, and without it the only trace of a session ending is
a row that stops being listed.

### `POST /v1/me/sessions/revoke-others`

No request body (the dashboard sends `{}`; axum needs no `Json` extractor).

1. Same limiter key and budget.
2. `let Some(sid) = auth.claims.sid else { ... }` -> `400`, message
   "your session predates this feature; reload the dashboard and try again". A
   sid-less legacy token cannot name the session to spare, and sparing nothing
   would log the caller out of the tab they are looking at.
3. `repo::revoke_sessions_for_user(&mut conn, auth.user_id, Some(sid), REVOKE_USER_REVOKED_OTHERS, Some(auth.user_id))`.
4. `mark_revoked(&ids)`, `tracing::warn!(%user_id, revoked = ids.len(), "user revoked other sessions")`,
   `{"ok": true, "revoked": ids.len()}`.

## 7. Admin force-logout

`POST /v1/orgs/{org_id}/members/{user_id}/revoke-sessions` in
`routes/orgs.rs`. The handler opens with
`authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?`
before anything else, and the shared guard stack below then re-checks
`member:manage` — so the route needs **both**. That is the carve-out working as
intended: `member:credential` narrows `member:manage`, it does not stand in for
it, and a role that can end a member's sessions without otherwise being able to
see or administer that member is not a shape anyone asked for.

### Minting `member:credential`

The permission does not exist yet. S1's force-reset needs the same gate and lands
next, so S2 mints it; if S1 minted it instead, S2 would ship gated on
`member:manage` and then have to widen its own guard one slice later, which is
the kind of silent authorization change nobody re-reviews.

Five coordinated edits, and they must ship together —
`dashboard/src/lib/models/permissions.test.ts` reads
`backend/crates/sauron-auth/src/rbac.rs` at test time and compares the two
catalogues **in order**, so a half-landed mirror fails the dashboard suite rather
than silently stripping the permission from every role on the next save:

1. `pub const MEMBER_CREDENTIAL: &str = "member:credential";` in `perm`, declared
   beside `MEMBER_MANAGE` and inserted immediately after it in `perm::ALL`, whose
   length annotation goes `[&str; 27]` -> `[&str; 28]`. The `perm::ALL.len()`
   assertion (rbac.rs:875) follows.
2. `perm::MEMBER_CREDENTIAL` appended to `ADMIN.permissions`, and the four preset
   count assertions re-pinned: Owner 27 -> 28 (rbac.rs:812), Admin 26 -> 27
   (rbac.rs:818), Developer stays 18 (rbac.rs:836), Viewer stays 7 (rbac.rs:862).
   Owner is `&perm::ALL` and needs no bag edit; Developer and Viewer hold no
   `member:manage` and must not gain this. **Re-read all four rather than
   assuming the last two.** A passing count is not evidence the permission landed
   in the right bag — a Developer that accidentally gained one and a Developer
   that legitimately stayed at 18 are distinguishable only by looking, which is
   why §Testing pins membership and not just length.
3. The `UPDATE roles` in migration 000035 (§1).
4. `dashboard/src/lib/models/permissions.ts` — `ALL_PERMISSIONS` in the same
   position the backend uses, the `Organization` entry of `PERMISSION_GROUPS`,
   and a `PERMISSION_LABELS` string ("Reset passwords and sign out devices").
   The last two are separately pinned: `permissions.test.ts` asserts every
   permission is grouped exactly once and labelled.
5. `'member:credential'` in the `Permission` union in
   `dashboard/src/lib/models/index.ts`.

### Extract the guard stack first

`set_member_active` (orgs.rs:702) carries six distinct guards and roughly 35
lines of load-bearing why-comment. S1 adds `.../password-reset` and S2 adds
`.../revoke-sessions`, both stating they "copy set_member_active's guard stack".
Three verbatim copies is three places for the next guard to be forgotten. S2
lands first, so S2 owns the extraction:

```rust
async fn guard_member_admin_action(
    conn: &mut AsyncPgConnection,
    caller_id: Uuid,
    org_id: Uuid,
    target_user_id: Uuid,
    allow_self: bool,
) -> Result<Vec<(String, Uuid, Value)>, ApiError>
```

It performs, in order, carrying the comments verbatim:

1. `authorize_org(conn, caller_id, org_id, perm::MEMBER_MANAGE)` — org-scoped by
   construction, so a project-scoped Admin cannot reach it.
2. `repo::get_user(target_user_id)` -> 404.
3. `repo::user_grants_in_org(target_user_id, org_id)`; empty -> 404, so an admin
   cannot act on an arbitrary account in the deployment by guessing a uuid. The
   rows double as the escalation input — one query, not two.
4. Self-target -> 409 unless `allow_self`.
5. `check_no_escalation(&effective_at_org(caller_id, org_id), &union_permissions(&grants_from_rows(target_grants)))`.
6. `repo::count_user_grants_outside_org(target_user_id, org_id) > 0` -> 409,
   unconditionally. An org-A admin acting on a member who is also an org-B Owner
   is reaching outside their blast radius, and no caller of this helper — S2's or
   S1's — has a reason to. It is not behind a flag, because a flag is an
   invitation: the next slice to want the easy answer sets it to `true` and the
   refusal quietly stops applying to the account it most protects.

It returns the target's grant rows so callers do not re-query.
`set_member_active` is refactored onto it, passes `allow_self: false`, and keeps
its **last-`org:manage` guard outside** the helper — that concern is specific to
deactivation.

`revoke_member_sessions` calls it with `allow_self: false`. The programme
integration note asserted S2 wants self-target *allowed* ("that IS sign out my
other devices from the admin page"); that reading is wrong and the parameter
should not be set to `true` here. This endpoint passes `except: None`, so a
self-target would log the admin out of the page they are standing on — "sign out
my other devices" is a different verb, lives on `/account`, and spares the
current session.

Every caller passes `false` today, S1's reset included, so `allow_self` could be
a hard-coded refusal like the cross-org check beside it. It stays a parameter
because the two rules are different in kind: self-target is an ergonomic rule
about which surface owns a verb, and a future admin action may legitimately want
the other answer, whereas cross-org is a blast-radius boundary and there is no
such action.

`revoke_member_sessions` deliberately **omits the last-`org:manage` guard**.
Deactivation is irreversible without an admin; a forced logout is reversible by
the victim simply logging in again, so it cannot orphan an org.

Then:

```rust
let ids = repo::revoke_sessions_for_user(
    &mut conn, user_id, None, repo::REVOKE_ADMIN, Some(auth.user_id)).await?;
state.revocations.mark_revoked(&ids);
tracing::warn!(actor = %auth.user_id, %user_id, %org_id, revoked = ids.len(),
    "admin revoked all sessions for a member");
```

It does **not** set `must_change_password` — "force login" is not "force
password reset", and `repo::set_user_password` unconditionally clears that flag
anyway — and it does not touch `is_active`.

## 8. Converting the existing revocation sites

The invariant is that `auth_sessions` and `refresh_tokens` can never disagree,
which means **every** site that revokes tokens moves to a session-aware fn. Four
sites exist today:

| Site | Today | After |
|---|---|---|
| `logout` (auth.rs:450) | `revoke_refresh_token(hash, REVOKE_LOGOUT)` | `revoke_refresh_token_and_session(hash, REVOKE_LOGOUT)`, then `mark_revoked` on `Some(sid)` |
| `change_password` (auth.rs:538) | `revoke_all_refresh_tokens_for_user_with_reason(.., REVOKE_PASSWORD_CHANGED)` | `revoke_sessions_for_user(user_id, None, REVOKE_PASSWORD_CHANGED, Some(user_id))` + `mark_revoked` |
| `refresh` family kill (auth.rs:413) | `revoke_all_refresh_tokens_for_user(user_id)` | `revoke_sessions_for_user(user_id, None, REVOKE_REUSE, None)` + `mark_revoked` |
| `set_member_active` deactivate (orgs.rs:778) | `revoke_all_refresh_tokens_for_user_with_reason(.., REVOKE_DEACTIVATED)` | `revoke_sessions_for_user(user_id, None, REVOKE_DEACTIVATED, Some(auth.user_id))` + `mark_revoked` |

**The deactivation conversion is the one most likely to be dropped, and it is
the most important.** Skipping it makes the most severe admin action the weakest
one: `AuthUser` reads claims, not `users.is_active`, so a deactivated member's
access token keeps full API access for up to 900 seconds — while the reversible,
strictly-less-severe "Sign out" added by this same slice closes that window in
~5 seconds. It also leaves the victim's `auth_sessions` rows with
`revoked_at IS NULL` and `expires_at` up to 30 days out, so `list_sessions`
reports phantom live sessions and the 404-on-successful-revoke shape from §4
becomes reachable. The `'deactivated'` value in the new CHECK is written by
nothing else; if it stays dead, the conversion was dropped.

`change_password`'s existing comment block explaining the
revoke-then-set-then-issue ordering stays valid and must be preserved.

After the conversion, no call site of `revoke_all_refresh_tokens_for_user` or
`..._with_reason` remains in `backend/bins/sauron-api`. Pin that with a test
that reads the crate's sources and asserts the count is zero, with a comment
naming why — a sixth site added later would desync the two tables silently, the
same drift class as `strip_source_context` being applied at 2 of 8 response
paths. Keep the repo fns themselves; deleting them is a separate change.

### The theft-alarm poisoning trap

This is the single highest-risk interaction in the slice and the three lines
that fix it must not be dropped in review as redundant.

`refresh`'s reuse-detection branch fires the family kill for any non-active
token whose reason is not `rotated` and not `deactivated`. A device killed by
"sign out other devices" will present its dead token on its existing 15-minute
timer, land in that branch, and kill the user's **whole** family — including the
session they explicitly chose to keep. The symptom is "sign out other devices
logs me out too, about fifteen minutes later", which reads as a flaky bug rather
than a design fault. The codebase's own comment at auth.rs:388-397 records this
exact class of bug happening before, with routine deactivations.

In `refresh`, immediately after the `REVOKE_DEACTIVATED` branch and before the
family kill:

```rust
// A session the user or an admin deliberately ended is not evidence of theft.
// Without this, the killed device's next refresh lands here, trips the family
// kill, and logs the user out of the session they explicitly chose to KEEP —
// turning "sign out my other devices" into "sign out all my devices, on a delay".
if reason.as_deref().is_some_and(|r| repo::DELIBERATE_REVOKE_REASONS.contains(&r)) {
    return Err(ApiError::Auth(AuthError::InvalidToken));
}
```

Scope is surgical: only the three new reasons. `REVOKE_LOGOUT` keeps its current
family-kill behaviour — changing it is a separate decision this slice does not
make. `REVOKE_DEACTIVATED` keeps its existing re-check branch, which is still
correct.

## 9. Retention

The original design shipped no reaper, on the argument that the partial index
"stays proportional to live sessions rather than to every session ever created".
**That argument is false.** Nothing writes `revoked_at` when a session merely
expires — liveness is `revoked_at IS NULL AND expires_at > now()`, and only the
second half lapses for the overwhelmingly common case of a user closing the
browser and never returning. So `WHERE revoked_at IS NULL` excludes only
explicitly-revoked sessions, and every abandoned session stays in the index
forever. The index is proportional to lifetime logins, precisely what it claimed
to avoid.

Read latency survives this (the `LIMIT 200` and the small live set), so the
damage is unbounded table and index growth — on a table that is also a new PII
class. `refresh_tokens` never had an `ip` column at all; `auth_sessions` is a
permanent per-user record of where and on what device someone signed in, with no
deletion path and no user control.

So S2 ships a reaper, following the programme rule that **a table's reaper lives
in the process that owns the table's write path** — sauron-api, in S0's task
supervisor, daily:

```rust
pub const AUTH_SESSION_RETENTION_DAYS: i64 = 30;

pub async fn prune_auth_sessions(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize>
// DELETE FROM auth_sessions
//  WHERE (revoked_at IS NOT NULL AND revoked_at < now() - ($1 || ' days')::interval)
//     OR expires_at < now() - ($1 || ' days')::interval
```

Same shape as `repo::prune_checks` / `repo::prune_alert_events` (repo.rs:6072,
6090). A compile-time const, not an environment variable: three files of
documentation for a value nobody tunes.

Deleting an `auth_sessions` row sets its tokens' `session_id` to NULL rather
than deleting them — that is exactly why the FK is `ON DELETE SET NULL` (§1),
and it is what keeps replay detection intact through a reap.

Rejected: NULLing `ip` and `user_agent` inside the revoke CTEs. It is cheaper
and worker-free, but it destroys the evidence in exactly the case the user is
investigating — they sign out an unrecognised device, then want to look at what
it was. The 30-day history view in §6 depends on those columns surviving
revocation.

`refresh_tokens` stays unreaped, per the non-goals.

## 10. Frontend

### `#/account`, built as a card container

`dashboard/src/pages/Account.svelte` is the first account/profile surface in the
dashboard. Build it as a **stack of cards from day one** — Profile, Active
sessions — so S3's notification preferences is an added card rather than a
restructure.

Three edits, per the house pattern:

1. The page itself, root `<AppShell requireProject={false}>` (sessions are
   user-scoped, matching Members and Storage). Standard page head
   (`h1.page-title` + `p.muted.sub` + `<RefreshButton>`) and the loading / error
   / empty triad.
2. `dashboard/src/routes.ts`: `'/account': guarded(Account as Component<never>)`.
3. A `NavItem` in the Manage group of
   `dashboard/src/lib/components/layout/Sidebar.svelte`:
   `{ href: '#/account', label: 'Account', icon: 'user', match: (p) => p.startsWith('/account') }`.
   **No `show:` gate** — every authenticated user has an account, unlike
   Members / Storage / Source Maps. `user` is already in `Icon.svelte`'s
   registry.

**Profile card.** `authStore.user`'s name, email and `last_login_at` via
`formatDateTime`, read-only, plus
`<Button variant="secondary" href="#/change-password">Change password</Button>`.
That link is a side benefit worth naming: `/change-password` has no discoverable
entry point today outside the forced temp-password redirect. Its route stays
`wrap({ component, conditions: [authed] })`, **not** `guarded()` — do not "fix"
that, because gating it on `passwordCurrent` bounces a temp-password holder off
it forever.

**Active sessions card.** `<Card title="Active sessions">` with an
`{#snippet actions()}` holding `<RefreshButton>` and
`<Button variant="danger" size="sm" disabled={otherCount === 0 || !hasCurrent}>Sign out other devices</Button>`.
Body is `<DataTable>` (`dashboard/src/lib/components/DataTable.svelte`) with
head `Device | IP | Signed in | Last used | <th aria-label="actions">`.

- Device cell: `describeSession(s)`, plus `<Badge tone="primary" size="sm">This device</Badge>`
  and no action button when `s.current`.
- IP cell: `class="cell-mono cell-muted"`.
- Dates: `relativeTime` with `title={formatDateTime(...)}`.
- Rows are **not** `class="clickable"` — there is nothing to drill into.
- When `!hasCurrent` (a legacy access token with no `sid`), render an inline
  `.err-banner` with `<Icon name="info" size={15} />` reading "Reload the
  dashboard to manage your devices" and disable both revoke affordances. That is
  the visible face of the 400 from `revoke-others`, and it disappears on the next
  refresh.
- When every live row shares one IP, render one line of muted help text under
  the table: "All sessions show the same address — the API is behind a proxy and
  `API_TRUST_FORWARDED_HEADERS` is not set." This is computed client-side from
  the data by a pure `allSameIp(list)`, needs no new API surface, and turns a
  column that looks broken into a legible configuration message. It matters
  because `api_trust_forwarded_headers` defaults to false in `config.rs`, in
  `packaging/rpm/config/api.env` and in docker-compose, and the shipped nginx
  sits in front — so on both shipped topologies every row *will* read the same
  address.

A "Show recent sign-outs" ghost button re-fetches with `?include_revoked=1` and
renders the revoked rows dimmed, with "Signed out {relative}" and a
human-readable reason. This is where a user learns that something ended their
session, which is the counterpart to the `tracing::warn!` lines in §6.

**Confirm flow.** One `<ConfirmDialog danger open={pending !== null} ...>` driven
by a single
`let pending = $state<{kind:'one'; id:string; label:string} | {kind:'all'} | null>(null)`,
matching `Members.svelte`'s `requestToggle` / `confirmDeactivate` shape. Copy:

- `one` — "Sign out this device" / "That device will be signed out within a few
  seconds and will have to log in again."
- `all` — "Sign out other devices" / "Every device except this one will be
  signed out. You will stay logged in here."

`confirmLabel="Sign out"`, `loading={busy}`. **"Within a few seconds" is
deliberate** — it is the honest description of the poll-interval residual window
and must never be written as "immediately".

### Client modules

`dashboard/src/lib/api/account.ts` (new), following the `api/alerts.ts` template
— `import { api } from './client'`, bearer required so **not** `bareClient`, one
exported async fn per endpoint: `listMySessions(includeRevoked?)`,
`revokeMySession(id)`, `revokeMyOtherSessions()`,
`revokeMemberSessions(orgId, userId)`. No change to `api/scope.ts`:
`computeScopeParams` only matches `/^\/v1\/apps\/[^/]+/`, so none of these get an
`environment_id` param and no `BACKEND_REJECTS_ENVIRONMENT_ID` entry is needed —
which is what keeps the Rust-side `http_env_scoping.rs` cross-check green.

`dashboard/src/lib/models/index.ts` gains `AccountSession`, mirroring
`SessionView` field for field. **The name is mandatory, not stylistic:**
`AuthSession` is already taken at models/index.ts:28 (`extends AuthTokens`, the
login response), and shadowing it would compile in some files while silently
changing the meaning of the auth store's types in others.

`dashboard/src/lib/models/account-sessions.ts` holds the decision logic, pure and
DOM-free per the house rule (vitest is node-only; there is no DOM test
environment): `describeSession`, `sortSessions` (current first, then
`last_used_at` desc), `otherSessionCount`, `hasCurrentSession`, `allSameIp`.
`describeSession` is the client half of a deliberate split — the server answers
the *data* question (what does this UA string mean) with woothee, the client
answers the *copy* question (how is it phrased): "Chrome on Mac OSX" when both
parts are present, browser-or-os alone when one is, else the raw `user_agent`
truncated to 60 characters, else "Unknown device".

### `MembersTable.svelte`

Add to `Props`: `onrevokesessions: (member: Member) => void;`,
`revokingUserId: string | null;` — mirroring the existing `ontoggle` /
`togglingUserId` pair — and `canRevokeSessions: boolean;`. In the `.row-actions`
div (MembersTable.svelte:142), beside the existing Deactivate/Reactivate button:

```svelte
{#if canRevokeSessions && member.user_id !== authStore.user?.id}
  <Button size="sm" variant="ghost"
          loading={revokingUserId === member.user_id}
          onclick={() => onrevokesessions(member)}>Sign out</Button>
{/if}
```

Hidden for self because the backend 409s that case — the UI must not offer an
action the server refuses.

The gate is a **new prop**, not the existing `canManage`. `Members.svelte:143`
derives `canManage` from `sessionStore.can('member:manage')`, and reusing it
would show a Sign out button to the holder of a custom role that has
`member:manage` without `member:credential` — the exact role the carve-out exists
to make possible — where every click 403s. So `Members.svelte` gains
`const canRevokeSessions = $derived(sessionStore.can('member:credential'))`
beside it and passes it down. The column header stays behind `canManage`; a role
that can do neither still gets no actions column.

`Members.svelte` owns the confirm: `revokingUserId`, `pendingRevoke`,
`requestRevokeSessions(member)`, `confirmRevokeSessions()`, and a second
`<ConfirmDialog danger title="Sign out all sessions">` beside the existing
deactivate dialog. Message:
``${name} will be signed out on every device and will have to log in again. Their account stays active.``
**"Their account stays active" is load-bearing copy** — an admin reaching for
this button is one click away from Deactivate and the two are easy to confuse.

**Why the button stays inline.** `.row-actions` does eventually become a
kebab/overflow menu, but not here, and not "using the house UI components" —
there is nothing to use. `dashboard/src/lib/components/ui/` has fourteen
components and none of them is a Select, Toggle, Tabs or Menu. Building one
properly (outside-click, focus trap, keyboard navigation, escape handling) is a
real component, and doing it badly inside an auth slice is worse than three
inline buttons. S2 takes the row from two buttons to three, which still fits.
**S1 builds the menu** and folds this Sign out button into it, because S1's Reset
password makes it four, which is where a row stops working. The ordering is
already decided: Edit / Reset password / Sign out all devices / Deactivate,
destructive last.

## 11. Config and packaging

One new config field, in `backend/crates/sauron-core/src/config.rs`, using the
existing `parse` helper (config.rs:108):

```rust
pub auth_revocation_poll_secs: u64,   // parse("AUTH_REVOCATION_POLL_SECS", 5)
```

No fail-closed accessor is needed — there is no secret, and a bad value degrades
to the default. **Clamp at the use site**: `poll.clamp(1, 60)` seconds, so a
fat-fingered 0 cannot spin the poller and a 3600 cannot silently restore a
one-hour revocation window. (The *window* is clamped separately; see §5.)

The four-file wiring rule, appended **after** S0's larger block lands (S0 defines
the new section headers, S2 adds one line next to `JWT_ACCESS_TTL_SECS`):
`.env.example`, the `api:` service `environment:` block in `docker-compose.yml`,
`packaging/rpm/config/api.env`, and the README env table. One-line comment: "how
long a revoked session can still be used, in seconds — this is the real kill
latency". `packaging/rpm/systemd/sauron-api.service` needs no change; it already
loads `/etc/sauron/api.env`.

Also: `woothee.workspace = true` in `backend/bins/sauron-api/Cargo.toml`.
`packaging/rpm/binaries.txt` is unchanged — S2 adds no binary; the poller is a
supervised task inside sauron-api.

**RBAC.** S2 mints `member:credential` — the five coordinated edits are
enumerated in §7 and the role seeding rides in migration 000035 (§1). Nothing
about it is optional or deferrable: `permissions.test.ts` parses `rbac.rs`
directly, so the dashboard mirrors either land in the same change or the
dashboard suite goes red.

### Upgrade gate

RPM upgrades do not re-run `sauron-migrate`. Without 000035 there is no
`refresh_tokens.session_id` and no `auth_sessions`, so `start_or_continue_session`
fails on **every** login, register, refresh and password change — a total
authentication outage, not a degraded feature, on the exact path an operator
would use to diagnose it.

S0 creates `packaging/rpm/SETUP.md` §11 "Upgrading" (verified: SETUP.md has
sections 1-10 and no upgrade guidance at all). S2 appends its row to that
section's table, and states the gate in the imperative, including the
maintenance-window warning from §1:

```
systemctl stop sauron-api sauron-ingest
systemctl start sauron-migrate     # 000035 locks refresh_tokens; schedule it
systemctl start sauron-api sauron-ingest
```

Follow the in-repo convention of a `%changelog` line in `packaging/rpm/sauron.spec`
saying the same thing (see spec:265).

## Error handling

| Case | Status | Code | Note |
|---|---|---|---|
| Access token whose `sid` is in the revocation snapshot | 401 | `invalid_token` | Every path, including `/v1/auth/password` |
| Revoking the session you are using | 409 | `conflict` | "use Log out instead" |
| Revoking an unknown / already-revoked / foreign session | 404 | `not_found` | Never 403 — a 403 would confirm the id exists |
| `revoke-others` with a sid-less legacy token | 400 | `bad_request` | "your session predates this feature; reload the dashboard and try again" |
| Refresh with a token whose session was deliberately revoked | 401 | `invalid_token` | Skips the theft alarm; no family kill |
| Refresh race path with `session_id IS NULL` | 401 | `invalid_token` | Cannot mint a new session from a stale pre-migration token |
| `issue_tokens` when the session was revoked mid-flight | 401 | `invalid_token` | Zero rows from the CTE with `session_id: Some(..)` |
| Admin revoke: no `member:credential`, or no `member:manage` | 403 | `forbidden` | Both are required; the route checks the first, the shared guard stack the second |
| Admin revoke: target has no grants in the org, or does not exist | 404 | `not_found` | |
| Admin revoke: self-target | 409 | `conflict` | "use your account page to manage your own sessions" |
| Admin revoke: target outranks caller | 403 | `forbidden` | Same shape as `create_grant`'s denial |
| Admin revoke: target holds grants outside the org | 409 | `conflict` | |
| Over the session-action limiter | 429 | `rate_limited` | 20 per 60s per user |

## Testing

**Constraint:** CI runs `cargo test --workspace` with no Postgres service, and
the dashboard has no DOM test environment. That pushes the work in a useful
direction — the decision logic lives in pure functions on both sides.

**Unit, `sauron-auth/src/revocations.rs`** (no DB): `contains` after
`mark_revoked`; `replace` retains a local entry marked **after** the poll's start
instant and evicts one marked before it (the eviction race in §5, which is the
one bug in this module that would silently un-revoke a session); `age()` is
`None` before the first refresh and `Some` after; the snapshot survives a failed
refresh with its last good contents.

**Unit, `sauron-auth/src/jwt.rs`**: `sid` round-trips through issue/decode; a
legacy token encoded without `sid` decodes to `sid: None` (extend
`tokens_minted_before_the_flag_existed_still_decode`); two calls with the same
`session_id` produce tokens carrying the same `sid` — the
identity-across-rotation property, asserted at the JWT layer.

**Unit, `sauron-api/src/routes/account.rs`**: `parse_ua` on a real Chrome-on-macOS
UA, a real Safari-on-iOS UA, a garbage string, an empty string and `None`,
asserting that woothee's literal `"UNKNOWN"` and `""` both normalise to `None`.
`sanitize_ip` rejecting a non-IP string and canonicalising a v6 address;
`sanitize_ua` truncating at `MAX_USER_AGENT_LEN`.

**Unit, `sauron-db/src/repo.rs`**: the reason-classification test and the
`include_str!` CHECK-parity test from §4.

**Unit, `sauron-auth/src/rbac.rs`**: the four preset count assertions, updated
per §7, plus explicit **membership** assertions — `OWNER` and `ADMIN` contain
`perm::MEMBER_CREDENTIAL`, `DEVELOPER` and `VIEWER` do not. The counts alone
cannot tell those apart, and the failure they miss is a Developer who can sign
any member out.

**Unit, `sauron-api`**: the call-site test asserting zero remaining uses of
`revoke_all_refresh_tokens_for_user*` in the API crate.

**Integration, new `backend/crates/sauron-db/tests/sessions.rs`** (skips when
`TEST_DATABASE_URL` is unset, mirroring the existing convention):
`start_or_continue_session` with a fresh id creates one session and one token;
the same id again creates only a token and bumps `last_used_at` / `expires_at`
while **leaving `user_agent` and `ip` at their login-time values**; a revoked
session id affects zero rows (the resurrection guard, asserted at the SQL layer
where it lives); `revoke_session` with a foreign `user_id` affects zero rows and
returns an empty vec even when the session has no live token (the 404-on-success
bug); `revoke_sessions_for_user` with `except` spares exactly that session, kills
session-less legacy tokens, and leaves no live token behind when `except` is
`None`; `list_sessions` excludes revoked and expired rows, and includes revoked
ones within 30 days under the flag; `prune_auth_sessions` deletes an old revoked
row and leaves its `refresh_tokens` row present with `session_id IS NULL`.

**Integration, new `backend/bins/sauron-api/tests/http_sessions.rs`**, spawning
the compiled binary via `CARGO_BIN_EXE_sauron-api` (copy `TestServer` from
`http_workflows.rs`, keeping the timestamp-first ephemeral DB naming the stale-DB
reaper depends on), with `AUTH_REVOCATION_POLL_SECS=1` in the child env so timing
assertions are seconds:

1. Two logins produce two sessions from `GET /v1/me/sessions`, exactly one marked
   `current`.
2. `DELETE` on the current session returns 409.
3. `revoke-others` leaves one session and the spared refresh token still
   refreshes successfully.
4. **The regression test.** After `revoke-others`, `POST /v1/auth/refresh` with
   the killed session's token returns 401 **and** the spared session still
   refreshes afterwards. Without the `DELIBERATE_REVOKE_REASONS` branch the
   spared session dies here.
5. The killed session's **access** token stops working within ~3s — the
   residual-window claim, measured rather than asserted in prose.
6. Deactivating a member kills their access token within ~3s (the §8 conversion).
7. `token_hash` appears nowhere in the `GET /v1/me/sessions` response body — a
   raw substring check on the JSON.
8. The rotation-grace interaction, which no unit test can see: rotate session B,
   revoke B, then present B's pre-rotation token inside `REFRESH_RACE_GRACE`
   while session A is live, and assert 401 rather than a re-issued pair.
9. The admin guard matrix for `revoke-sessions`: 403 for a custom role holding
   `member:manage` but not `member:credential` — the carve-out, asserted over
   HTTP where it is enforced; 404 for a user with no grants in the org; 409 on
   self-target; 403 when the target outranks the caller (Admin targeting an
   Owner); 409 when the target holds grants in another org; 200 plus dead tokens
   in the happy path. Assert it does **not** set `must_change_password` and does
   **not** flip `is_active`.

**Dashboard, `dashboard/src/lib/models/account-sessions.test.ts`** (vitest,
node-only): `describeSession` across all four fallback rungs; `sortSessions`
current-first then `last_used_at` desc; `otherSessionCount` and
`hasCurrentSession` on empty, all-current and no-current lists (the last being
the legacy-token state that disables the UI); `allSameIp` on mixed, identical and
null-bearing lists.

**Migration verification:** run 000035 against a database with live sessions and
a hand-made custom role, and confirm every live refresh token gains a
`session_id` and a matching `auth_sessions` row, that revoked and expired rows
keep `session_id IS NULL`, that the custom role holding `member:manage` gained
`member:credential` while one without it did not, and that `down.sql` restores
the prior schema cleanly. Time the run on a realistically-sized `refresh_tokens`
and put the number in the upgrade note.

**Manual/e2e**, the real gate for the parts nothing above can see: log in from
two real browsers, confirm both appear with correct device labels, revoke one
from the other, and confirm the revoked browser is bounced to `#/login` on its
next API call rather than showing a broken page — that exercises the
401 -> `runRefreshOnce()` -> refresh-401 -> `onRefreshFailure()` chain end to
end. Then run the admin Sign out from Members as a second user and confirm the
victim is ejected.

## Files

**New**
- `backend/migrations/2026-08-01-000035_auth_sessions/{up,down}.sql`
- `backend/crates/sauron-auth/src/revocations.rs`
- `backend/bins/sauron-api/src/routes/account.rs`
- `backend/crates/sauron-db/tests/sessions.rs`
- `backend/bins/sauron-api/tests/http_sessions.rs`
- `dashboard/src/pages/Account.svelte`
- `dashboard/src/lib/api/account.ts`
- `dashboard/src/lib/models/account-sessions.ts` + `.test.ts`

**Modified**
- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}`
- `backend/crates/sauron-auth/src/{jwt.rs,extractors.rs,lib.rs}`
- `backend/crates/sauron-auth/src/rbac.rs` — `perm::MEMBER_CREDENTIAL`,
  `perm::ALL`, the `ADMIN` bag, the preset assertions
- `backend/crates/sauron-core/src/config.rs`
- `backend/bins/sauron-api/src/main.rs` — `AppState.revocations`, the `FromRef`
  impl, four routes, the poller mounted in S0's `tasks.rs` supervisor
- `backend/bins/sauron-api/src/routes/mod.rs` — `SessionContext`, `issue_tokens`,
  `sanitize_ua`, `sanitize_ip`, `pub mod account`
- `backend/bins/sauron-api/src/routes/auth.rs` — five call sites, the deliberate-
  revocation branch, `logout`, `pub(crate)` on `rate_limit` and `client_addr`
- `backend/bins/sauron-api/src/routes/orgs.rs` — `guard_member_admin_action`,
  `set_member_active` refactor + conversion, `revoke_member_sessions`
- `backend/bins/sauron-api/Cargo.toml` — `woothee`
- `backend/bins/sauron-api/tests/{http_workflows.rs,http_env_scoping.rs}` — nine
  `issue_access` call sites
- `dashboard/src/routes.ts`, `dashboard/src/lib/components/layout/Sidebar.svelte`
- `dashboard/src/lib/models/index.ts` — `AccountSession`, and
  `'member:credential'` in the `Permission` union
- `dashboard/src/lib/models/permissions.ts` — `ALL_PERMISSIONS`,
  `PERMISSION_GROUPS`, `PERMISSION_LABELS`
- `dashboard/src/lib/components/members/MembersTable.svelte`,
  `dashboard/src/pages/Members.svelte`
- `.env.example`, `docker-compose.yml`, `packaging/rpm/config/api.env`,
  `README.md`, `packaging/rpm/SETUP.md` §11, `packaging/rpm/sauron.spec`
  `%changelog`
- `dashboard/src/pages/Docs.svelte` — document the account page and the kill
  latency

## Follow-ups (out of scope)

- An email on a new sign-in from an unrecognised device. This is the natural
  sequel and it now has the data it needs (`created_at` + `user_agent` + `ip`),
  but it depends on S0's SMTP foundation and the programme's `DASHBOARD_URL`.
- An admin view of a member's individual sessions, if an operator asks "which
  device was that login from".
- An absolute session lifetime (90 days regardless of activity).
- Step-up / sudo re-authentication, as a general mechanism rather than a bespoke
  password field on one endpoint.
- A `refresh_tokens` reaper, deleting on expiry only.
- **S5 constraint, asserted here and to be re-asserted in S5's own design:** the
  PII inspector and masker must be scoped by an explicit **allowlist** of
  telemetry tables — S5's own inventory table is the authority on which ones and
  on the scan-only/maskable split — never a denylist. `auth_sessions` stores raw
  user agents and IP addresses as **account** data; masking it would destroy the
  exact fields that make "was that login me?" answerable, and a denylist
  silently fails to protect the next account table someone adds.
