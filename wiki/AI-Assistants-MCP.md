# AI assistants (MCP)

**Sauron has a read-only MCP server.** Connect Claude Code (or any MCP client that
supports Streamable HTTP and custom headers) with a personal API key, and ask it
what is breaking in your apps, why, and what the user did before the crash.

The flow is three steps:

1. **Create a key** on **Account → API keys**.
2. **Run one command** to connect Claude Code.
3. **Ask a question** — "What are the top errors in my-org/my-project/web this
   week, and why does the top one happen?"

See also: **[Privacy Inspector](Privacy-Inspector.md)** (masking what the
assistant can see) · **[Search & Filtering](Search.md)** (the query language the
tools accept) · **[Troubleshooting](Troubleshooting.md)**.

---

## What it is

One endpoint: `POST <API_BASE>/v1/mcp`, where `<API_BASE>` is the same API base
URL the dashboard uses. It speaks MCP over Streamable HTTP, stateless, with JSON
responses. `GET` returns `405` with a key and `401` without one.

Every tool is **read-only**. There are no write actions in v1. Each tool answers
as *you*: it sees only what your role and scope would let you open in the
dashboard.

## Create a key

Open **Account → API keys** and create one.

- The token starts with `sauron_pat_` and is **shown once**. Copy it then.
- Expiry: 30 days, 90 days (the default), 1 year, or never.
- You can hold at most 25 unrevoked keys; expired keys count until you revoke them.
- Revoke a key on the same page.

## Connect Claude Code

```bash
claude mcp add --transport http sauron <API_BASE>/v1/mcp \
  --header "Authorization: Bearer <key>"
```

The dashboard builds this command for you, with your API base filled in, when
you create the key. Open Claude Code in your repository: the tools return stack
frames, and Claude can then open the local files they name.

## Tools

All eight are read-only.

| Tool | What it does |
| --- | --- |
| `list_apps` | The organizations, projects and apps you can read, with each app's reference (`org/project/app`), type, environments and latest releases. Call it first. |
| `trending_issues` | An app's issues ranked by what happened inside a window (default 24h), by events or by users, with the change against the previous window and a NEW flag. |
| `get_issue` | One issue in full: summary, plus its latest (or a chosen) event with the stack trace, breadcrumbs and tags. |
| `list_issue_events` | The occurrences of one issue in a window, with a breakdown by release, environment and OS. |
| `describe_search` | The search language for an app: fields, tag keys with sample values, and example queries. |
| `get_session_timeline` | What one user did before a crash: the session header, then its events, errors and transactions in time order. |
| `list_releases` | An app's releases, newest seen first, with error counts and NEW issues per release when statistics are ready. |
| `slow_operations` | The slowest transactions of an app by p95 latency, per `(name, op)` pair. |

Parts of a result your role may not read are withheld, and the output says which.

## Organization switch

On the **Members** page, the **AI assistants (MCP)** card controls whether
members can connect assistants to the organization. Only holders of
`org:manage` can change it. It is **on by default**.

When it is off, the organization's apps are hidden from `list_apps` and refused
by every other tool.

## What data leaves your org

Tool results go into the developer's Claude conversation, so they can include
end-user details that your telemetry carries (emails, user ids, request data in
breadcrumbs and tags). Three controls decide how much:

- **The organization switch** above — off means nothing is returned.
- **Role permissions** — without `event:read` a member gets no event bodies;
  without `source:read` no source lines appear under frames.
- **The Privacy Inspector's masking** — rows masked in hot Postgres are masked in what the
  assistant reads too (cold Parquet is not rewritten). See **[Privacy Inspector](Privacy-Inspector.md)**.

Event text is untrusted input: titles, messages, breadcrumbs, tags and properties
come from end users' devices (browser ingest keys are public, so anyone can send
events), and the server tells the assistant to treat that text strictly as data
and never follow instructions found in it.

## Revoking keys

- Revoke any key yourself on **Account → API keys**.
- An admin's **End all sessions** for a member revokes that member's keys.
- An admin-forced password reset revokes them too.
- Changing your **own** password does **not** revoke your keys.
- Keys of a **deactivated** member are refused until the member is reactivated.

## Trending and the backfill

`trending_issues` ranks by events **inside the window**, read from an hourly
per-issue table that the ingest service keeps up to date continuously. Users
are counted on demand with a 5-second cap; a count that takes longer shows `?`.

User counts can differ between tools: `get_issue` (with no environment) shows
the issue's stored all-time users figure, an estimate that can lag behind, while
`trending_issues` counts users exactly for its window. The lag is short: each
ingest process writes that stored figure back at most once every 5 seconds per
issue, and catches up on any issue it held back once that interval passes, so
it trails new users by about one interval at most. A short restart of the
ingest service is covered too: on startup it re-checks the issues that
received events in the previous 10 minutes (up to 25,000 of them, newest
first). After an ingest outage longer than about 10 minutes, run the command
below once.

Figures that went stale **before the upgrade** that introduced this catch-up
are repaired by a one-time operator command (not automatic; safe to re-run
and to run while ingest is up, and it only ever raises a count):

```bash
sauron-migrate backfill-users-seen
```

It can raise a count that a data purge lowered, because the per-issue user
sketch cannot forget the purged users; that errs toward overcounting, the
safe direction.

Apps created **before the upgrade** that introduced MCP have no history in that
table. Their trending answer starts with `Fallback ranking:`, meaning in-window
counts are not available yet and the ranking is approximate. Fix it once, as an
operator:

```bash
sauron-migrate backfill-issue-stats
```

It is **not automatic**, it is safe to re-run, and it covers only history still
in Postgres (not archived Parquet). Apps created after the upgrade need nothing.

## Troubleshooting

| Status | Meaning |
| --- | --- |
| `401` `invalid_token` | The key is missing, unknown, revoked or expired — or you sent a dashboard login token instead of a `sauron_pat_` key. |
| `403` `account_deactivated` | The member is deactivated. |
| `403` `password_change_required` / `password_reset_required` | The member must change or reset their password first. |
| `429` `rate_limited` | More than `MCP_CALLS_PER_MIN` requests (default **120** per key per minute). Every MCP request counts, not just tool calls. |

- **Behind a reverse proxy**, it must forward the `Authorization` header and
  allow `POST` to `/v1/mcp` under the same API base the dashboard uses. A proxy
  that strips the header shows up as a `401` for a key that is correct.
- **claude.ai web and desktop chat connectors are not supported.** They need
  OAuth, which v1 does not offer; use Claude Code or another client that can
  send a custom `Authorization` header.
- **`Fallback ranking:` in trending** — run the backfill above.
