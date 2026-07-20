# Search & Filtering

Every list in the dashboard — issues, events, users, devices, screens, sessions —
can be narrowed with **search**. There is no global "search everything" box or
command palette: search is always scoped to the **App** you're viewing (see the
tenancy model in **[Home](Home.md)**) and to the resource of the page you're on.

Two mechanisms do the work, and on some pages they sit side by side:

- **Free-text search** — a single search box. You type a term and rows whose
  relevant text columns *contain* it (case-insensitive) stay. It's a substring
  match, nothing more — no query language, no operators.
- **Structured filters** — typed `field · operator · value` chips (on the
  **Exceptions** and **Events** pages). These let you say precise things like
  `status = unresolved`, `times_seen > 100`, or `culprit contains checkout`.

Most searches run **server-side** in Postgres over the whole dataset. Two boxes
(**Sessions** and **Funnels**) filter **client-side** over what's already on
screen — see [Server-side vs client-side](#server-side-vs-client-side) below.

See also: **[Dashboard](Dashboard.md)** (the pages these controls live on) ·
**[Architecture](Architecture.md)** (the queries behind the screens).

## Where you can search

| Page | Free-text box matches | Structured filters | Runs |
|------|-----------------------|--------------------|------|
| **Exceptions** (Issues) | issue `title`, `type`, `culprit`, plus event `tags`/`contexts`/`extra` payload | ✅ 7 fields (incl. `Tag`) | server |
| **Events** | event `name`, `distinct_id`, plus `tags`/`contexts`/`extra`/`properties` payload | ✅ 6 fields (incl. `Tag`) | server |
| **Users** (People) | `distinct_id` **and any trait** | — | server |
| **Devices** | `family`, `model`, `os_name`, `device_key` | — | server |
| **Screens** | `screen` name | — | server |
| **Sessions** | session id / user / device | — | client (loaded page) |
| **Funnels** | saved funnel **template name** | — | client |

Performance (transactions), Journeys, Overview, and the Manage pages have no
free-text search.

An individual **Issue detail** page also has its own scoped **Occurrences**
list (the events that rolled into that one issue), with the same `Tag` filter
and free-text payload search as Exceptions/Events — narrowed further to just
that issue.

## Free-text search: what each box matches

The search term is matched as a **case-insensitive substring** against a fixed
set of columns per resource. Anything containing your term, anywhere in one of
those columns, matches.

- **Exceptions** — `title` **or** `type` **or** `culprit` **or** the event's
  `tags`/`contexts`/`extra` payload (matched as text, so a value inside any of
  those JSON blobs counts). So searching `timeout` finds it whether it's in the
  error message, the exception class, the culprit frame, or a tag/context/extra
  value on the underlying event.
- **Events** — event `name` **or** `distinct_id` **or** the event's
  `tags`/`contexts`/`extra`/`properties` payload. (The stream always hides the
  synthetic `$screen` view events.)
- **Users** — `distinct_id` **or the entire traits blob**. Because it searches
  the JSON properties as text, you can find people by *any* trait value — e.g.
  an email, a plan name, or a company — not just their id.
- **Devices** — a single blob of `family`, `model`, `os_name`, and `device_key`
  glued together, so `iphone 15` or `macos` both work. (`os_version`, `arch`,
  `browser`, and last-seen user are shown but **not** searched.)
- **Screens** — the `screen` name only.

Leaving the box empty applies no filter (you get everything in the current
window). The search runs within the page's **date range** and returns the most
recent matches first, paginated — so it narrows the *current view*, not a
separate index.

## Structured filters (Exceptions & Events)

On the **Exceptions** and **Events** pages, click **+ Add filter** to build a
chip. Each chip is a `field`, an `operator`, and a `value`. Add several and they
combine with **AND** (every chip must match). The free-text box and the date
range still apply on top.

**Operators**, by field type:

| Field type | Operators | Meaning |
|------------|-----------|---------|
| text | `=` &nbsp; `≠` &nbsp; `contains` | exact / not-exact / case-insensitive substring |
| enum | `=` &nbsp; `≠` | exact / not-exact against a fixed option list |
| number | `=` &nbsp; `>` &nbsp; `<` | numeric compare |
| tag | `contains` &nbsp; `=` | key/value match against the event's `tags` JSONB (`contains` is the default) |

**Exceptions** fields: `Level` (enum: debug/info/warning/error/fatal),
`Status` (enum: unresolved/resolved/ignored), `Type` (text), `Culprit` (text),
`Events` = times-seen (number), `Users` = users-seen (number), `Tag`
(key=value, see below).

**Events** fields: `Event` name (text), `User` = distinct_id (text),
`Session` id (text), `Environment` (enum, populated from the app's real
environments), `Release` (text), `Tag` (key=value, see below).

### The `Tag` filter

`Tag` is a two-input chip — you type a **key** and a **value** — that composes
into a single `key=value` filter value under the hood (so it still fits the
existing `field:op:value` wire format; the backend splits the value on the
**first** `=`, so a value that itself contains `=` still round-trips). Two
operators are offered — **`contains` is the default**, since "search by tag"
usually means a forgiving match:

- **`contains`** *(default)* — case-insensitive substring match on that one key's
  value (`tags ->> key ILIKE '%value%'`). Reach for this when you know the key but
  only part of the value (e.g. `region` `eu` matches `eu-central`). Like the rest of
  `contains`, it's an unindexed scan bounded by app + date window + row cap.
- **`=`** — exact whole-value match, using Postgres's JSONB containment operator
  (`tags @> {"key": "value"}`). This is **index-backed** (a GIN `jsonb_path_ops`
  index on `tags`), so it stays fast even over a large table — use it when you want
  a precise match.

`Tag` only looks at the developer-set `tags` map — not `contexts`, `extra`, or
`properties` (use the free-text box for those), and never the machine-owned
`context` (singular) blob.

Filters and the search term are **encoded into the page URL** (as repeated
`filter=field:op:value` params plus `q=…`), so a filtered view is shareable and
survives a reload. Typing in the free-text box is **debounced** (it waits until
you pause before querying), while adding a filter or changing the date range
reloads immediately.

## How it works under the hood

- **Postgres `ILIKE '%term%'`.** Free-text search — and the `contains` filter
  operator — compile to a case-insensitive substring match. `=`/`≠` become exact
  equality, and `>`/`<` become numeric comparisons. On Exceptions/Events, `q`
  additionally casts the `tags`/`contexts`/`extra` (and, for Events,
  `properties`) JSONB columns `::text` and `ILIKE`s them — a bounded scan, not
  an indexed search, same caveat as the rest of `contains`. Only the developer
  `contexts` (plural) blob is searched this way; the machine-owned `context`
  (singular) blob on error events is never searched.
- **`Tag` is the one indexed path.** `Tag =` compiles to a JSONB containment
  check (`tags @> …`) backed by a GIN `jsonb_path_ops` index on the
  (partitioned-parent) `tags` column, so it doesn't pay the linear-scan cost
  the rest of search does. `Tag contains` still falls back to `->> ILIKE` and
  is a scan.
- **Injection-safe.** Every value you type is sent as a **bound query
  parameter**, never spliced into SQL text. Search input cannot alter the query.
- **Scoped and bounded.** Every search is filtered to your `app_id` and the
  selected date window, ordered by recency, and capped (the API returns at most
  ~200 rows per page via `limit`/`offset`). Users search is the one exception
  with **no date window** — it looks across all time.
- **Not full-text search.** There's no stemming, relevance ranking, fuzzy
  matching, or tokenization, and no external search engine (no Elasticsearch, no
  RediSearch) or `tsvector`/trigram index. It's a plain substring scan — simple
  and predictable. Because a leading-`%` `ILIKE` can't use a B-tree index, the
  match is a scan bounded by the app + date window + row cap, which is fast at
  MVP scale.
- **Filters validated twice.** The `field:op:value` filters are checked against a
  per-resource whitelist on **both** the frontend and the backend. The frontend
  silently drops any filter it can't recognise (e.g. when reading filters back
  from a shared URL); the backend is stricter — a `filter=` value with an unknown
  field, a disallowed operator, an out-of-range enum, or a non-numeric number is
  **rejected outright** (HTTP 400). Because the dashboard only ever builds valid
  filters, you won't hit that in normal use.

### A wildcard nuance

Because search is `LIKE`-based, the characters `%` (any run of characters) and
`_` (any single character) are technically wildcards. On **Exceptions**,
**Events**, and **Screens** these are **escaped** — typing `50%` searches for the
literal text `50%`. On **Users** and **Devices** they are **not** escaped, so a
`%` or `_` there behaves as a live wildcard. Either way it's only a matching
quirk — it's never a security issue, since the value is always a bound parameter.

## Server-side vs client-side

Two boxes are different from the rest — they filter only what's **already
loaded**, in the browser:

- **Sessions** — the box filters the current page of sessions by session id,
  user, or device. It does not query the server, so it only searches sessions
  already fetched. (The Sessions API itself supports exact `distinct_id` /
  `device_key` filters, which the app uses when you drill in from a user or
  device — not from this box.)
- **Funnels** — the box filters your list of **saved funnel templates** by name.
  It has nothing to do with searching your event data.

Everything else in the table above sends your term to the backend and searches
the full dataset.

## Tips

- **Find a person by trait, not just id.** The Users search covers the whole
  traits object, so an email or company name typed there will find the matching
  person.
- **Mind the date range.** Server-side search only looks inside the selected
  window (except Users). Widen the range if you expect older matches — the
  Exceptions and Events ranges go all the way to "all time".
- **Combine, don't cram.** On Exceptions/Events, reach for a structured filter
  (`status = unresolved`, `Events > 100`) when you want a precise cut, and keep
  the free-text box for "does this string appear anywhere".
- **Share a filtered view.** On Exceptions and Events the filters live in the
  URL — copy the address bar to hand someone the exact same view.

---

Related: **[Dashboard](Dashboard.md)** · **[Architecture](Architecture.md)** ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.
