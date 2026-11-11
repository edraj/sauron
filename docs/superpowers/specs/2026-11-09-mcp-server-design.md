# Sauron MCP server — design

Date: 2026-11-09 · Status: draft for review

## Intent

Developers fixing bugs in their apps should be able to ask Claude about what
Sauron sees, from inside the Claude session that already has their code open.
Typical moments:

- "What's blowing up in checkout-web this week?" → Claude lists the trending
  errors.
- "Look at that top one and tell me why it happens." → Claude pulls the error's
  stack trace, release and context, then walks the frames against the code on
  the developer's machine and proposes a fix.

The value is the join between Sauron's production data and the developer's local
source, which only a coding assistant running in the repo can make. So the
primary client is **Claude Code** (CLI, IDE extension, desktop Code tab). Any
other MCP client that can send an `Authorization` header also works, but isn't
a design target.

This ships **inside Sauron itself**, so every team running a Sauron install
gets it.

## Goals

1. A read-only MCP endpoint served by `sauron-api`.
2. Personal API keys that a developer creates in the dashboard and pastes into a
   single `claude mcp add` command.
3. A key sees exactly what its owner can see in the dashboard, re-checked on
   every call.
4. An org-level switch that lets org admins turn MCP access off for their org.
5. Debugging-focused tools: apps, trending errors, error detail, error
   occurrences, search, session timeline, releases and slow operations.
6. A new per-issue time-bucketed rollup, so "trending" ranks by what
   happened **in the window** rather than by all-time totals. Decided by the
   user after the review showed that today's no-environment issue ranking uses
   lifetime `times_seen`.

## Non-goals (v1)

- **Any write.** No resolving, ignoring, assigning or commenting on issues, and
  no changes to anything else. Decided by the user.
- **Browser sign-in (OAuth).** Personal keys only. OAuth would also let the
  claude.ai web and desktop chat connectors work. Those clients are not
  supported in v1 because they require OAuth. Decided by the user; possible
  follow-up.
- **Product analytics tools** (funnels, retention, journeys, user lists, active
  users), **uptime monitors** and **alert history**. They're useful, but not
  for fixing code. Agreed with the user.
- **Keys on the REST API.** A key authenticates only `/v1/mcp`. My call:
  limits what a leaked key exposes.
- **Narrowing a key to specific apps or orgs.** A key carries its owner's full
  read access. My call; possible follow-up.
- **Masking end-user details** (emails, IPs, user IDs) specifically for MCP.
  The org switch is the control for v1. See "Data leaving the org".
- **Crash-free rate per release.** Nothing computes it today:
  `session_stats_release_daily` has no crashed-session count. Follow-up.
- **Release filter on `slow_operations`.** `perf_agg_hourly` has no release
  column. Follow-up.
- **Switching the dashboard's Exceptions list to the new rollup.** The
  dashboard has the same lifetime ranking when no environment is chosen.
  Using the new table there is a natural follow-up, but not part of this
  work.

## Anti-goals: a failure even if it "works"

- Claude can see anything the key's owner couldn't see in the dashboard right
  now, including after a grant is removed, a member is deactivated, or the org
  switch is turned off.
- Any path through MCP that changes data. This means changes a user could see
  or ask for: issue status, settings, grants, and so on. The internal
  write-backs that the dashboard's own read paths already do are fine, and
  tools may share those paths as they are. Examples are caching a
  symbolicated copy of an event (`symbolicate_event`) and repairing blank
  culprits in `issues::list`. My call: read-only variants would make MCP
  output differ from the dashboard for no user-visible gain.
- A key that stays usable after it's revoked, expires, or its owner is signed
  out by an admin.
- Tool output so large or noisy that Claude loses the thread, for example a
  full 500-frame stack dump or a raw jsonb blob.

## How developers use it

1. A developer opens **Account → API keys** in the dashboard, clicks
   **Create key**, names it (for example "laptop Claude Code") and picks an
   expiry.
2. The dashboard shows the key **once**, along with a ready-to-copy command:

   ```
   claude mcp add --transport http sauron <API_BASE>/v1/mcp \
     --header "Authorization: Bearer <key>"
   ```

   `<API_BASE>` is the API URL the dashboard already uses (from `config.js`),
   so the command works behind whatever proxy or prefix the install uses.
3. In Claude Code, they ask about their errors. Claude calls the tools.
4. Back in the dashboard, they can see their keys with created, last-used and
   expiry dates, and revoke any key. Revoking takes effect on the next call.

## Architecture

### Endpoint and transport

- Path: **`/v1/mcp`** on `sauron-api`. Living under `/v1/` means any proxy that
  already exposes the API also exposes MCP. No new nginx `location` is needed.
  The dashboard and API run on separate origins/prefixes in real deployments
  (see memory: deployment ingest path constraint). The API base URL is the
  only thing the command needs.
- Transport: MCP **Streamable HTTP**, run **stateless with plain JSON
  responses** (no SSE streams, no server-side MCP session). My call. Reasons:
  - Sauron can run several API instances behind a load balancer, and stateless
    needs no sticky sessions.
  - Proxy buffering doesn't break it.
  - None of the tools need server-initiated messages.

  `GET` on the endpoint returns 405, as the spec allows for servers that don't
  offer a stream.
- Library: the official Rust MCP SDK (`rmcp`), which has an axum integration
  for Streamable HTTP. The builder pins a current version and checks that it
  supports stateless JSON-response mode on axum 0.8. If it doesn't fit cleanly,
  a hand-rolled JSON-RPC handler for `initialize`, `tools/list` and
  `tools/call` is acceptable, because the surface is small.
- Server instructions: the `initialize` response carries a short `instructions`
  string. It tells Claude what Sauron is, to call `list_apps` first, how
  environments and releases work, and that stack-frame paths are as reported
  by the SDK and may need mapping to the local repo layout.

### Authentication: personal API keys

New table (migration 83), sketch:

```
api_keys
  id            uuid pk
  user_id       uuid not null references users(id) on delete cascade
  name          text not null
  token_prefix  text not null      -- first chars, shown in the UI to tell keys apart
  token_hash    text not null unique   -- hex SHA-256 of the full token (sauron_auth::hash_token)
  created_at    timestamptz not null default now()
  expires_at    timestamptz null   -- null = never
  last_used_at  timestamptz null
  revoked_at    timestamptz null
```

- **Token format:** `sauron_pat_` followed by 64 hex characters from
  `sauron_core::ids::opaque_token()` (32 random bytes). The prefix makes leaked keys easy to recognize with secret
  scanners. Store SHA-256 only: the token is high-entropy, so a slow hash adds
  nothing.
- **Validation per request:** hash the token and look it up. Reject if revoked,
  expired, or the owner's account would be refused by the normal login rules.
  That includes an account with `must_change_password` set; mirror
  `password_change_gate` by refusing MCP entirely.
- **`last_used_at`:** throttle the write, for example to at most once per
  minute per key. Don't add a write to every call.
- **Admin sign-out:** "End all sessions" for a member
  (`/v1/orgs/{org_id}/members/{user_id}/revoke-sessions`) **also revokes that
  member's API keys**. My call: admins use this button for a compromised
  account, and leaving long-lived keys alive would defeat it. For the same
  reason, an **admin-forced password reset**
  (`/v1/orgs/{org_id}/members/{user_id}/password-reset`) also revokes the
  member's keys. Otherwise a key an attacker made with the stolen password
  would start working again once the reset is finished. Changing your own
  password does **not** revoke keys, matching how personal access tokens
  usually behave. The builder notes this in the UI copy.
  *Amended 2026-11-10 (full review):* the self-service **forgot-password**
  reset (the emailed link) revokes keys too. It is the account-recovery path
  and already kills every session for that reason; leaving keys alive there
  let a key minted by whoever held the account outlive the recovery.
  *Amended 2026-11-11 (user request):* keys record why and by whom they were
  revoked (migration 85, the `auth_sessions` reason vocabulary); the owner's
  Account page lists revoked keys with the reason; the Members page shows each
  member's working-key count and flags a forgotten-password revocation for 30
  days; and admins revoke every key of one or more members at once with
  `POST /v1/orgs/{org_id}/members/revoke-api-keys` (same permissions and guard
  stack as "End all sessions", all-or-nothing checks, audited as
  `access_key.revoke_all`, which the emailed reset also records).
- **Deactivated members:** their keys are refused while they're deactivated,
  because access is re-checked on every call, but the keys aren't revoked.
  They work again after reactivation, the same way grants are kept. My call.
- **Expiry choices in the UI:** 30 days, 90 days (default), 1 year, never. My
  call.
- **Rate limiting:** a per-key limit on tool calls, using the existing
  Redis-backed limiter infrastructure. The builder picks the number; something
  like 120 calls per minute. Note the shared-Redis test trap in memory: API
  tests share a Redis limiter.
- **Auth failures** return the MCP/HTTP 401 that Claude Code surfaces as an
  auth error. The response must not reveal whether a key exists.

Key management endpoints use the normal dashboard JWT (`AuthUser`), never an
API key:

- `GET /v1/me/api-keys`: list your own keys. Never returns token or hash.
- `POST /v1/me/api-keys`: create; returns the token exactly once.
- `DELETE /v1/me/api-keys/{id}`: revoke your own key. Soft delete via
  `revoked_at`.

Audit: create and revoke go to the audit log ("Wall of Shame") via
`audit::record_for_user_orgs`, because keys are user-level and not org-scoped.
Admin sign-out revocations are covered by that action's existing entry; add
the revoked key count to its `changes`. Tool calls are **not** audited
individually in v1. My call: they're reads, and the dashboard doesn't audit
reads either.

### Authorization: same rules as the dashboard

The MCP layer must not invent its own permission logic. Every tool resolves the
key to its `user_id` and then goes through the same authorization the
dashboard path uses:

- App access: `sauron_auth::authorize_env_read` / `authorize_env_read_with_perms`,
  via the helpers in `bins/sauron-api/src/routes/scope.rs`. Environment-scoped
  grants must work exactly as they do in the dashboard. An env-only grant
  still sees its own environment.
- Error event bodies (stack trace, breadcrumbs, context, tags, event user, IP)
  pass through **`symbolicate::gate_event_body`**: both `issue:read` and
  `event:read` are required. Source context lines additionally require
  `source:read`. A key whose owner lacks a permission gets the same reduced
  view the dashboard would show, and the tool output says what was withheld
  and why, so Claude doesn't guess.
- Inspector masking (irreversible PII masking on stored rows) already happens
  upstream of reads, so MCP inherits it with no extra work.

**Implementation shape:** most route handlers take axum extractors such as
`AuthUser`, `RawQuery` and `Query<T>`. Tools must not make HTTP calls back into
the API. Factor the query and shaping logic each tool needs into plain
functions that take `(conn, user_id, typed params)` and return typed results.
Call them from both the existing handler and the tool. The environment
`RawQuery` extractor trap documented in `scope.rs` is a handler concern. Tools
pass environment and release as typed arguments straight to `parse_env` /
`parse_release`.

### The org switch

- Migration 83 also adds `organizations.mcp_enabled boolean not null default true`.
  **Default on**: decided with the user, so it works out of the box and
  admins can turn it off.
- New endpoints: `GET /v1/orgs/{org_id}/settings` (any member) and
  `PATCH /v1/orgs/{org_id}/settings` with `{ "mcp_enabled": bool }`, gated on
  `org:manage`. The PATCH is audited. A settings resource rather than a
  one-off flag endpoint is my call, so later org settings have a home.
- Enforcement is on every tool call. An app whose org has MCP disabled behaves
  as not found / not accessible, and `list_apps` omits it. The error text
  says "MCP access is turned off for this organization" when the caller does
  have dashboard access, so the developer knows why.
- Keys are per user, not per org. One key covers every org the user belongs
  to where MCP is enabled. My call.
- Dashboard: a toggle labeled something like "Allow AI assistants (MCP)" with
  one line of explanation. It goes wherever org-level settings live today; if
  there's no such page, put it on the org page next to members. Only
  `org:manage` holders see it as editable.

### Data leaving the org

Tool results go into the developer's Claude conversation, and from there to
the model provider. Error data can include end-user identifiers, request
bodies and developer-supplied `extra`. v1 gives orgs three controls: the switch
above, the existing permission gates (a role without `event:read` never sends
bodies), and the existing inspector masking. The org-settings toggle says this
in one sentence. Per-field MCP masking is a possible follow-up.

## Tools (v1)

All tools are read-only. Each declares MCP annotations
`readOnlyHint: true, openWorldHint: false`. Arguments use names and slugs
where it's natural. Apps can be referenced by id or by a `project/app` slug
pair, and environments by name. Claude shouldn't have to juggle UUIDs it can't
see. Every result carries a dashboard deep link (`<DASHBOARD_BASE>/#/...`) so
the developer can click through. The dashboard base comes from a server config
value; if it's unset, links are omitted.

| Tool | Purpose | Permission (same as dashboard) |
|---|---|---|
| `list_apps` | Orgs → projects → apps the key can see, with app type, environments, and the latest releases. Claude calls this first. | `app:read` (MCP-disabled orgs omitted) |
| `trending_issues` | Issues for an app in a time window (default 24h; also 1h/7d/14d/30d), ranked by **in-window** events (default, from the new rollup) or affected users (counted on read; see "New rollup"). Filters: environment, release, status (default unresolved), `query`. Each row: id, title, culprit, level, in-window events and users, the **previous equal window's** events (so Claude can say "up 4×"), the issue's first and last seen, a **new** flag (the issue's app-wide `first_seen` is inside the window), link. Default 20 rows, max 50. | `issue:read` |
| `get_issue` | One issue: summary fields and the **latest event in full**: exception type and message, stack trace (symbolicated when available, with frames as function / file / line / col / in-app), breadcrumbs (last N), tags, release, environment, device/OS/runtime, SDK. Optional `event_id` to fetch a specific occurrence instead of the latest. | `issue:read`; body needs `event:read`; source lines need `source:read` |
| `list_issue_events` | Occurrences of one issue: per event id, time, release, environment, device/OS, user (if allowed). Also a breakdown of the issue's events in the window by release, environment and OS. This is a **new** GROUP BY over that one issue's `error_events` rows, because `events/stats` returns only totals; OS is read from `contexts`. It's bounded by the issue and the window, so it's cheap. My call to keep it. Answers "only one release, or everywhere?" | `issue:read`; per-event bodies gated as above |
| `describe_search` | The search language's fields and tags for an app, from the existing `/search/schema`, so Claude can write correct `query` strings. | `issue:read` or `event:read` |
| `get_session_timeline` | The ordered events, screens and errors of one session, so Claude can see what the user did before a crash. Takes a session id; `get_issue` returns one. | `event:read`; embedded error bodies gated as above |
| `list_releases` | Releases of an app with first and last seen (existing `/releases`), plus error events, distinct issues and **new issues** (issues whose first bucket falls in that release) over a window, from the new rollup. Answers "did my last release break this?" No crash-free rate in v1 (see non-goals). | Whatever `/releases` uses today, plus `issue:read` for the error counts |
| `slow_operations` | Slowest transactions by p95 for an app over a window, by `(name, op)` pair, with p50/p95/count, from the existing performance summary. Filters: environment, op. No release filter in v1 (see non-goals). | `event:read` |

Search is a `query` argument on `trending_issues` and `list_issue_events`,
backed by `describe_search`, not a separate free-form search tool. My call:
the query language already plugs into those lists, and one fewer tool makes
the set easier for Claude to pick from.

### New rollup: per-issue hourly counts

**Why:** today `repo::top_issues` (no environment) filters on
`issues.last_seen` and ranks by lifetime `times_seen` / `users_seen`. The
environment path counts in-window, but only over the 10,000 most recently seen
issues (`ENV_RANK_CANDIDATES`), and its `first_seen` is the in-window minimum.
Neither answers "what's spiking now". Counting raw `error_events` per request
is too slow for 7d/30d windows on large installs because of the 30 s budget.

**Table** (migration 84; keys and the switch stay in 83), sketch:

```
issue_stats_hourly
  app_id          uuid not null  -> apps on delete cascade
  issue_id        uuid not null  -> issues on delete cascade
  environment_id  uuid null      -> app_environments on delete cascade (NULL = unattributed, like every rollup)
  release         text not null default ''   -- '' = unknown, same sentinel as migration 82
  hour            timestamptz not null       -- hour start, UTC (named like perf_agg_hourly)
  events          bigint not null
  unique (app_id, hour, issue_id, release, COALESCE(environment_id, nil uuid))
```

- **Hourly buckets** (my call) make the 1h and 24h windows exact to the hour.
  `perf_agg_hourly` is the precedent for an hourly rollup.
- **Events only, no users sketch** (my call, made while planning). Sauron's
  HLL (`sketch.rs`) is a fixed 4 KB dense sketch. One per issue, per hour,
  per environment, per release would cost gigabytes per busy app per year,
  and rollups are never pruned. Instead, **users are counted on read** for
  the rows a tool returns. One `count(DISTINCT distinct_id)` over those
  issues' window rows uses `error_events_issue_env_time_idx`, under a 5 s
  statement timeout. If it times out, users are shown as unknown, with a
  note. **Sorting by users** ranks the 200 issues with the most events in the
  window by their counted users, and the output says so.
- **No double counting** (my call, made while planning). The live fold has
  its own watermark (`issue_stats`), seeded at the table's own epoch. The
  backfill **replaces** an app's rows one day at a time, counting raw rows
  with `received_at <=` that watermark. It reads the watermark inside the
  rollup advisory lock, so whatever the fold adds later is strictly newer.
  The consistency rebuild uses the same bound. The base rollups' additive
  backfill path never writes this table.
- **Window semantics:** the window is the last N whole hours plus the current
  partial hour, read from buckets. The previous window is the N hours before
  that.
- **Retention:** the builder follows whatever `perf_agg_hourly` does. At least
  60 days must stay readable, so 30d windows have a previous window to compare
  against.
- **Written by the existing fold loop** (`crates/sauron-db/src/rollups/fold.rs`),
  as one more fold from `error_events`, with its own watermark like the
  existing folds. It inherits these fold traps from memory:
  - The pull must bound `occurred_at` (≤ 3 partitions) as well as
    `received_at`, or every cycle scans every partition.
  - Late rows heal through the trailing-35-day `consistency_check_trailing` →
    `rebuild_day` net. Add this table to both.
  - Admin data purge and the guest→identified merge must keep it consistent,
    the same way they handle `user_activity_daily`. Check `purge.rs` and
    `identity_merge::fold_rollups`.
- **Readiness gate:** reads use the rollup only when the app is ready, following
  `rollups::is_ready` (apps created after the rollup epoch are ready at once).
  Older apps become ready through an **operator-run backfill**, a new opt-in
  `sauron-migrate backfill-issue-stats` subcommand that writes a marker per
  app. Following the established pattern (memory: rollup backfills are
  operator-run), it must **never** run from systemd units, the RPM/deb
  scripts or compose. Slow work in the migrator is a boot outage.
- **Before the backfill** (my call): `trending_issues` falls back to today's
  ranking and says so in its output: "ranked by all-time count among issues
  seen in the window; in-window ranking needs the issue-stats backfill (ask
  your Sauron admin)". `list_releases` omits the error columns with the same
  note. This works, but degrades visibly, and is never silently wrong.
- **Tests:** test databases pin the rollup epoch 10 years out (memory: rollup
  gate closed in tests), so ordinary tests never touch this table. Add an
  equivalence test in the style of `crates/sauron-db/tests/rollup_equivalence.rs`:
  seed events, fold, and check the in-window counts against a raw count
  of `error_events`, including the env, release and unknown-release cases.
- **Docs:** the backfill command goes in `INSTALL.md` / the RPM `SETUP.md`
  upgrade notes, next to the existing backfill commands.

### Output shape

- Return **compact, readable text**, with structured content where the SDK
  supports it. No raw jsonb dumps. Stack traces render as one line per frame
  (`at fn (path:line:col)`, with an in-app marker), innermost first. Cap at
  about 50 frames, keeping in-app frames when truncating, and say what was
  cut.
- Every list says how many rows were returned, whether more exist, and how to
  get them. Use a cursor where the underlying endpoint has one.
- Withheld fields say so explicitly ("stack trace withheld: your role lacks
  event:read").
- Errors are MCP tool errors (`isError: true`) with an actionable message: not
  found, no access, MCP disabled for org, bad query string (include the parser
  message), or timed out.
- The 30 s `TimeoutLayer` applies to `/v1/mcp` like any route. A tool that
  can't finish returns a clear "query timed out, narrow the window or add an
  environment" error. Memory has many traps where env-scoped analytics queries
  503 on big installs, so prefer the rollup-backed paths the dashboard uses.

## Fit with existing guards and tests

- **Audit coverage test** (`bins/sauron-api/tests/audit_coverage.rs`): every
  `post/put/patch/delete` route in `main.rs` must be classified. `/v1/mcp` is
  a `post`, so classify it as not audited (reads). The key create/revoke
  routes and settings PATCH are audited.
- **OpenAPI parity test** (`openapi.rs`): the key and settings endpoints get
  `#[utoipa::path]` docs. Document `/v1/mcp` as a single JSON-RPC operation,
  or exempt it with a comment explaining why. The builder chooses whichever the
  parity test makes cleaner.
- **Release guard** is GET-only and doesn't touch `/v1/mcp`. Release scoping
  for tools comes from their `release` argument through `parse_release`.
- **Memory traps the builder should read first:** backend tests silently skip
  in the sandbox; rollup gate closed in tests (tests take legacy paths); API
  tests share a Redis limiter; clippy toolchain skew (use `cargo +1.98.0`);
  never commit, never `git stash` (shared tree).

## Testing

- **Unit:** token generation, prefix, hashing and lookup; expiry and revocation
  logic; output formatters (frame rendering, truncation, withheld notices).
- **HTTP integration** (`bins/sauron-api/tests/http_mcp.rs`, next to the other
  `http_*` tests):
  - `initialize` and `tools/list`, with and without a key, and with an expired
    or revoked key.
  - Each tool against seeded data.
  - The permission matrix: a Viewer without `event:read` gets no bodies; an
    env-scoped grant sees only its env; removing a grant takes effect on the
    next call.
  - The org switch: off → app hidden and calls refused; on → visible.
  - Admin "end all sessions" revokes keys.
  - A key used against a normal REST route gets 401.
- **Live:** run the local stack, create a key in the dashboard, run the shown
  `claude mcp add` command in a real Claude Code session, and ask "what are the
  top errors in <app>, and why does the top one happen?" against a repo with
  matching source. This is the acceptance test.

## Docs

- New wiki page "AI assistants (MCP)": what it does, creating a key, the
  command, the tool list, the org switch, what data leaves the org, and how to
  revoke. Link it from the key page in the dashboard and from `INSTALL.md`.

## Left to the builder

- Exact `rmcp` version and integration, or the hand-rolled fallback.
- Module layout. The suggestion is `bins/sauron-api/src/mcp/` with one file per
  tool group, since `gate_event_body` and the scope helpers live in that crate.
- Rate limit numbers, `last_used_at` throttle interval, breadcrumb count, frame
  cap.
- Where the org toggle sits in the dashboard.
- Rollup retention (follow `perf_agg_hourly`, with at least 60 days) and the
  exact env key type.
- Text wording for errors, withheld notices and the server `instructions`.
