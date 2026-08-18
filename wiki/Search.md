# Search & Filtering

Every list in the dashboard — issues, events, users, devices, screens, sessions —
can be narrowed with **search**. There is no global "search everything" box or
command palette: search is always scoped to the **App** you're viewing (see the
tenancy model in **[Home](Home.md)**) and to the resource of the page you're on.

Four pages — **Exceptions**, **Events**, an issue's **Occurrences** list, and
**Sessions** — run their search through a real **query language**. You can send
`is:unresolved level:error timesSeen:>100` as the API's `query=` parameter and
get exactly those rows. The rest of the pages still use the simpler free-text
box described further down.

The search box on those four **autocompletes** what it will accept: press `↓`
to see the fields, and once you have typed a `field:` it offers that field's
values. What it offers comes from the same catalog the resolver enforces, per
resource — Sessions carry no developer tags, so `@tag` is not offered there and
a tag term is rejected. Tag *keys* are sampled from recent events, so a key you
have not sent lately may be missing from the suggestions; you can still type it,
since any key is queryable.

See also: **[Dashboard](Dashboard.md)** (the pages these controls live on) ·
**[Architecture](Architecture.md)** (the queries behind the screens).

## Read this first if you have a saved link

Two things changed that can break a URL you bookmarked or shared.

**1. An unknown field name is now an error, not an empty list.**
A field the system doesn't recognise used to be silently reinterpreted as one of
your own event **tags**. So `checkout_step:payment` quietly became "the tag
called `checkout_step`" and answered `200 OK` with zero rows — indistinguishable
from an honest "nothing matched", and a typo like `enviroment:prod` looked the
same way. That fallback is gone. An unrecognised field is now a **400** that
names it and tells you what to write instead. To filter on a tag, say so:
`tag.checkout_step:payment`. See
[Filtering on your own tags](#filtering-on-your-own-tags) and
[When a field isn't recognised](#when-a-field-isnt-recognised).

**2. `offset=` is accepted but ignored on the three searched lists.**
They page with an opaque **cursor** now. A link carrying `?offset=100` no longer
400s — it returns the **first** page, which is not the page it used to return.
Follow `next_cursor` from the response instead. See
[Paging with a cursor](#paging-with-a-cursor). Every other list in the API
(sessions, devices, screens, workflows, top events) still uses `offset` exactly
as before.

Nothing else about existing links changed: the old `filter=field:op:value` and
`q=` parameters still work, and they parse into the same query as the new
syntax.

## Where you can search

| Page | Query language | Free-text box matches | Runs |
|------|----------------|-----------------------|------|
| **Exceptions** (Issues) | ✅ | issue `title`, `type`, `culprit`, plus event `tags`/`contexts`/`extra` payload | server |
| **Issue → Occurrences** | ✅ | `message`, `exception_type`, `exception_value`, plus `tags`/`contexts`/`extra` payload | server |
| **Events** | ✅ | event `name`, `distinct_id`, plus `tags`/`contexts`/`extra`/`properties` payload | server |
| **Users** (People) | — | `distinct_id` **and any trait** | server |
| **Devices** | — | `family`, `model`, `os_name`, `device_key` | server |
| **Screens** | — | `screen` name | server |
| **Sessions** | — | session id / user / device | client (loaded page) |
| **Funnels** | — | saved funnel **template name** | client |

Performance (transactions), Journeys, Overview, and the Manage pages have no
free-text search. Sessions, Devices, Users and Transactions are scheduled to
join the query language later; until then their boxes are plain substring
matches.

## The query language

Terms are separated by whitespace. Sitting them next to each other means
**AND**. `OR` is written out, `!` negates, and parentheses group.

```
Exceptions:   is:unresolved level:error timesSeen:>100
Exceptions:   !is:resolved firstSeen:>-7d "connection refused"
Occurrences:  (os.name:Windows OR os.name:Linux) has:extra.cartValue
Occurrences:  user.email:*@acme.com !release:2.1.4 level:[error,fatal]
Events:       name:checkout_started tag.region:eu environment:production
```

On the wire this is a single parameter:

```
GET /v1/apps/{app_id}/issues?query=is%3Aunresolved%20level%3Aerror
```

**Which fields you can use depends on which view you're searching.** `timesSeen`
is an Exceptions field and means nothing on Events; `os.name` is an Occurrences
field and means nothing on Exceptions. Naming one from the wrong view is a
400 that lists the right ones — see [Fields by page](#fields-by-page), which is
the authority. The examples below are labelled where it matters.

### Bare terms — free text

A word with no `field:` in front of it is a **free-text** term. It's matched as
a case-insensitive substring against that resource's text columns (and, if you
hold `event:read`, the event payload — see
[What each free-text box matches](#what-each-free-text-box-matches)).

```
timeout                     the word "timeout" anywhere in the searched columns
"connection refused"        quote it when it contains a space
level:error timeout         both: an error-level issue mentioning "timeout"
```

### `field:value` — equality

```
level:error                 exactly "error"
release:2.1.4               exactly "2.1.4"
name:"checkout started"     quote a value containing spaces or a colon
```

Quoting also switches off every operator below, which is the only way to search
for a value that *starts with* one: `culprit:">100"` looks for the literal text
`>100` rather than comparing anything.

### `!` — negation

```
!level:error                not error-level
!release:2.1.4              anything but that release
!(level:error is:resolved)  negate a whole group
```

Negation is **NULL-safe**: a row whose value was never recorded is *not* the
same as a row whose value differs, and `!field:value` keeps the unrecorded ones
rather than silently dropping them.

### `AND`, `OR`, and adjacency

Adjacency **is** AND — `level:error is:unresolved` and
`level:error AND is:unresolved` are the same query, and the explicit `AND` is
there only for readability. `OR` binds **looser** than AND, so:

```
level:error is:unresolved OR level:fatal
```

reads as `(level:error AND is:unresolved) OR level:fatal` — the same precedence
every search bar you have met before uses.

### `( … )` — grouping

Parentheses override that precedence:

```
(level:error OR level:fatal) is:unresolved
```

Now both levels are filtered down to unresolved issues, instead of the `OR`
swallowing the second term.

### `field:[a,b,c]` — a list

```
level:[error,fatal]         either level
release:[2.1.4, 2.1.5]      spaces around the commas are fine
```

This runs as a single `= ANY(…)` rather than fanning out into an `OR` chain, so
a long list stays as cheap as one comparison.

### `field:[lo..hi]` — inclusive range

Same brackets as the any-of list, told apart by the `..`. Available on the
ordered types — timestamps, integers, durations — i.e. wherever `>=` and `<=`
are.

```
firstSeen:[7d..1d]                             first seen between 7 and 1 days ago
lastSeen:[1month..2026-08-01T00:00:00Z]        mixed ends are fine
timesSeen:[10..100]                            between 10 and 100 occurrences
duration:[500ms..2s]                           (Transactions)
```

Both ends are inclusive, and both are **required**: `[7d..]` is rejected rather
than guessed at, because it is already spelled `>=7d`.

It expands to `field:>=lo field:<=hi` before the planner sees it, so a range
costs and indexes exactly what the two comparisons would — there is no separate
range operator underneath.

On a string or enum field the brackets keep meaning "any of": `level:[error..fatal]`
is one nonsense list item and is reported as a bad enum value, not read as a
comparison.

### `>` `>=` `<` `<=` — comparisons

Available on numbers, durations and timestamps.

```
timesSeen:>100                    seen more than 100 times          (Exceptions)
usersSeen:>=10                    at least 10 distinct users        (Exceptions)
firstSeen:>7d                     first seen in the last 7 days     (Exceptions)
lastSeen:>=1month                 seen within the last month        (Exceptions)
lastSeen:<2day                    nothing seen for over two days    (Exceptions)
lastSeen:<2026-07-01T00:00:00Z    last seen before an instant       (Exceptions)
duration:>2s                      slower than two seconds           (Transactions)
duration:>500ms                   …or five hundred milliseconds     (Transactions)
http.status:>=500                 server errors only                (Transactions)
```

The Transactions lines are the shape of the grammar, not something you can send
yet — Transactions, Devices, Users and Sessions have field definitions but no
route on the query language. They're listed so the duration syntax has somewhere
to be shown.

- **Durations** accept `500ms`, `2s`, `5m`, `1h`, or a bare number, which means
  milliseconds.
- **Timestamps** accept a relative offset *before now*, or a full ISO-8601
  instant. A relative offset is a number plus a unit:

  | Unit | Spellings | Length |
  |---|---|---|
  | second | `s` `sec` `secs` `second` `seconds` | 1s |
  | minute | `m` `min` `mins` `minute` `minutes` | 60s |
  | hour | `h` `hr` `hrs` `hour` `hours` | 3 600s |
  | day | `d` `day` `days` | 86 400s |
  | week | `w` `week` `weeks` | 7 days |
  | month | `mo` `mos` `month` `months` | **a calendar month** |

  A leading `-` is optional and changes nothing — `firstSeen:>7d` and
  `firstSeen:>-7d` are the same query. Both spellings work because `-7d` was the
  only one for a while and is baked into saved views.

  Note `m` is **minutes**, not months; months are `mo` or longer, and there is
  deliberately no one-letter spelling of "month".

  A month is a **real calendar month**, not a fixed span: `>=1month` on 31 March
  means 28 (or 29) February, clamping to the end of a shorter month rather than
  spilling into the next one. Every other unit is a fixed number of seconds, so
  `4week` and `1month` are not the same query.

  Read the comparison the way you'd read it on a number line: the value is an
  instant, so `lastSeen:>=1month` is "at or after (now − 1 month)", i.e. *within*
  the last month, and `lastSeen:<2day` is "before (now − 2 days)", i.e. nothing
  for over two days.
- A value that isn't a valid number, duration or timestamp for that field is a
  400 naming the field, not a silently-dropped term.

### `*` wildcard vs `field:~text` literal substring

These are two different things and the difference matters.

```
user.email:*@acme.com       WILDCARD — * stands for "any run of characters"
culprit:~checkout           LITERAL SUBSTRING — find "checkout" anywhere
```

`*` is a **pattern**: any `*` you type becomes a wildcard, and a `%` or `_` in
your value is escaped so it stays literal. `~` is the opposite — **everything
after the `~` is taken verbatim**, including `*`:

```
culprit:~foo*bar            matches the literal text "foo*bar"
culprit:*foo*bar*           matches anything containing "foo" then "bar"
release:~1.0                any release whose name contains "1.0"
```

Reach for `~` when the value you're pasting might itself contain a `*`, a `%`
or a leading `>` — it's the one operator that never re-interprets what follows
it. It is also what the old `contains` filter operator meant, so shared links
using `filter=culprit:contains:checkout` translate straight onto it.

### `has:field` — is this key present at all?

```
has:extra.cartValue         rows that carry that key            (Occurrences, Events)
has:handled                 rows where "handled" is known       (Occurrences)
has:tag.checkout_step       rows carrying that tag key          (Exceptions, Occurrences, Events)
```

`has:` answers presence, not value. It's the honest way to ask about a field
that has three states: `handled:true` and `handled:false` both exclude rows
where the value was never recorded, and `has:handled` is how you select the ones
where it *was*.

### `is:` — curated shorthands

`is:` is a small namespace of named shorthands, not a field you can put
arbitrary values in:

| Term | Means | Where |
|---|---|---|
| `is:unresolved` `is:resolved` `is:ignored` | the issue's status | Exceptions |
| `is:handled` | the error was caught and handled | Occurrences |
| `is:unhandled` | the error was not handled | Occurrences |

`is:handled` and `is:unhandled` both exclude rows where handledness is unknown
(events ingested before that was recorded), and `has:handled` is how you select
the rows where it *is* known. An unknown keyword — `is:banana` — is a 400, not
an empty result.

### Filtering on your own tags

Tags are the key/value pairs your own code attaches to an event. They are
**not** in the field list, because the field list is fixed and your tags are
whatever you decided to send. Two spellings reach them:

```
tag.region:eu                     the tag named "region"
tag.region:~eu                    …containing "eu" (matches "eu-central")
tag.region:[eu,us]                either value
has:tag.checkout_step             the key exists
tag:cart@checkout=eu              ESCAPE HATCH for awkward keys
tag:100%off=*sale*                …and it composes with every operator
```

Use `tag.<key>` when the key looks like an identifier: it must start with a
letter or `_`, and continue with letters, digits, `_`, `-` or `.`. Tag keys are
**not validated anywhere on the way in** — a developer can store `cart@checkout`
or `100%off` — so for anything else use `tag:<key>=<value>`, where the key is
everything before the **first** `=` and the rest is an ordinary value. A key
containing a **space** is the one shape that can't be expressed, and it fails
loudly rather than matching nothing.

`tag.` only looks at the developer-set `tags` map — not `contexts`, `extra` or
`properties` (those have their own dotted fields), and never the machine-owned
`context` (singular) blob.

Tags are only available on Exceptions, Occurrences and Events. Devices, Users,
Sessions and Transactions carry no tags, and say so rather than offering a
spelling that would match nothing.

### Nested paths into JSON

Where a field holds JSON, address inside it with dots:

```
user.email:*@acme.com               (Occurrences)
os.name:Windows                     (Occurrences)
browser.version:~12                 (Occurrences)
extra.cartValue:1200                (Occurrences, Events)
has:extra.cartValue                 (Occurrences, Events)
contexts.checkout.step:payment      (Occurrences, Events)
stack.function:handleRequest        (Occurrences)
properties.plan:pro                 (Events)
traits.company:"Acme Inc"           (Users — not on the query language yet)
```

**Values inside JSON are compared as text, not as numbers.** `extra.cartValue`
holds whatever you sent, so `=`, `≠`, a list, `~`, `*` and `has:` all work on it
and `>` / `<` do not — asking for `extra.cartValue:>100` is a 400 saying the
operator isn't allowed on that field, rather than a comparison that quietly
sorts `"9"` after `"100"`. Numeric comparison is available on the typed columns
(`timesSeen`, `usersSeen`, `http.status`, `duration`).

On **Exceptions** and **Occurrences**, filtering on a path into the event body —
`user.*`, `sdk.*`, `os.*`, `browser.*`, `device.*`, `app.*`, `contexts.*`,
`extra.*`, `stack.*`, and `tag.<key>` — needs `event:read`, because those are
the columns your event bodies arrive with nulled without it. Without the
permission you get a **403** naming it, rather than a quietly widened result:
the point of a filter is that every row you get back satisfies it, so dropping
it would be worse than refusing it. (`properties.*` on Events and `traits.*` on
Users are not part of any withheld body and need nothing extra.)

`environment:<name>` is gated separately, on `env:read`, because resolving a
name tells you whether an environment by that name exists. All four preset
roles carry it, so this only bites a custom role built without it.

### Limits

- **Depth: 8.** Nesting parentheses or `!` more than eight levels deep is a 400.
- **Terms: 64.** More than 64 terms in one query is a 400. Repeated `filter=`
  parameters count against the same limit.

Both are structural guards, not quotas — no real query comes near them.

## Fields by page

The canonical name is on the left; anything in brackets is an accepted alias
(usually the underlying database column, so old links keep working).

### Exceptions (Issues)

| Field | Type | Notes |
|---|---|---|
| `is` (`status`) | enum | `unresolved` `resolved` `ignored` |
| `level` | enum | `debug` `info` `warning` `error` `fatal` |
| `type` | text | the exception class |
| `culprit` | text | the frame blamed for the error, `function (file:line)` — de-obfuscated when symbols are uploaded |
| `title` | text | unindexed — a scan |
| `timesSeen` (`times_seen`) | number | |
| `usersSeen` (`users_seen`) | number | |
| `firstSeen` (`first_seen`) | timestamp | |
| `lastSeen` (`last_seen`) | timestamp | |
| `screen` | text | matches an issue with at least one occurrence on that screen |
| `distinctId` (`distinct_id`) | text | matches an issue that affected that user |
| `deviceKey` (`device_key`) | text | matches an issue seen on that device — exact match only, no `~` substring |
| `workflow` | text | needs `event:read` |
| `tag.<key>` | text | needs `event:read` |
| `environment` `release` `handled` | — | **declared but not searchable yet on this page** — they need the issue-dimension rollup, and asking for one returns a 400 saying exactly that. `environment` and `release` *are* searchable on Occurrences and Events, and `handled` on Occurrences. |

`screen`, `distinctId` and `deviceKey` are not columns of an issue — they belong
to its individual occurrences — so on this page they ask **"does this issue have
an occurrence that matches?"**, evaluated inside the time range and environment
the page is already scoped to. Two consequences worth knowing:

- Narrowing the range can drop an issue that still appears without the filter:
  the issue is in range because it was *seen* in range, but its `/checkout`
  occurrence may be older than that.
- Negation reads as "no matching occurrence". `!screen:/checkout` keeps an issue
  that has never been seen on `/checkout`, including one whose occurrences
  recorded no screen at all — it does not mean "seen on some other screen".

### Issue → Occurrences

| Field | Type | Notes |
|---|---|---|
| `level` | enum | `debug` `info` `warning` `error` `fatal` |
| `handled` | boolean | also spelled `is:handled` / `is:unhandled` |
| `environment` | text | |
| `release` | text | |
| `distinctId` (`distinct_id`) | text | |
| `session` (`session_id`) | text | |
| `deviceKey` (`device_key`) | text | |
| `screen` | text | |
| `symbolication` (`symbolication_status`) | enum | `pending` `processing` `symbolicated` `failed` `skipped` `unsupported` |
| `message` | text | unindexed — a scan |
| `workflow` | text | |
| `user` | JSON root | `user.email`, `user.id`, … |
| `sdk` | JSON root | `sdk.name`, `sdk.version`, … |
| `os` | JSON root | `os.name`, `os.version`, … |
| `browser` (`runtime`) | JSON root | `browser.name`, … |
| `device` | JSON root | `device.model`, … |
| `app` | JSON root | `app.version`, … |
| `contexts` | JSON root | needs `event:read` |
| `context` | JSON root | `context.app_version`, `context.os.name`, … — also `@context.<path>` |
| `extra` | JSON root | needs `event:read` |
| `stack` | JSON root | unindexed — a scan |
| `tag.<key>` | text | needs `event:read` |

### Events

| Field | Type | Notes |
|---|---|---|
| `name` | text | the event name |
| `distinctId` (`distinct_id`) | text | |
| `session` (`session_id`) | text | |
| `environment` | text | |
| `release` | text | |
| `workflow` | text | |
| `properties` | JSON root | `properties.plan`, … |
| `contexts` | JSON root | needs `event:read` |
| `context` | JSON root | `context.app_version`, `context.os.name`, … — also `@context.<path>` |
| `extra` | JSON root | needs `event:read` |
| `tag.<key>` | text | needs `event:read` |

### Not yet on the query language

These have catalog entries and will light up when their pages are bridged:
Transactions (`op`, `name`, `duration`, `http.status`, `http.method`, `url`),
Devices (`device.family`, `device.model`, `device.arch`, `os.name`,
`os.version`, `browser`, `deviceKey`), Users (`distinctId`, `traits.*`),
Sessions (`session`, `startedAt`).

### When a field isn't recognised

The API answers **400** and the message does three things — names the field,
gives you the tag spelling, and lists what *is* available on that view. Asking
Exceptions for `checkout_step:payment`:

```
`checkout_step` is not a valid field for this view; to filter on a
developer-supplied tag, write `tag.checkout_step`. Available fields: culprit,
environment, firstSeen, handled, is, lastSeen, level, release, timesSeen,
title, type, usersSeen, workflow
```

(One line on the wire; wrapped here to fit.) If the name can't be spelled as
`tag.<key>` — it has an `@`, a `%`, or some other character the field syntax
won't take — the advice switches to the escape hatch. You'll meet this through
`has:`, whose operand is a value rather than a field, so it can carry anything:
`has:"cart@checkout"` gives

```
`cart@checkout` is not a valid field for this view; to filter on a
developer-supplied tag, write `tag:<key>=<value>`. Available fields: …
```

On **Devices, Users, Sessions and Transactions** there is no `tags` column at
all, so no tag advice is offered — you get the name and the available fields
only.

Errors come back in the usual envelope:

```json
{ "error": { "code": "bad_request", "message": "`checkout_step` is not a valid field for this view; …" } }
```

Two details worth knowing:

- **The same rule applies to the old `filter=` spelling.** One grammar, one
  vocabulary, so `filter=checkout_step:eq:payment` and
  `query=checkout_step:payment` fail identically. That is the point — the two
  spellings can't drift into accepting different things. The one difference is
  the *message*: `filter=` is translated before the view is known, so a field
  name that isn't even a valid identifier (`filter=cart@checkout:eq:eu`) gets
  the bare sentence with no tag advice and no field list.
- **In `query=`, a word whose field side isn't a valid identifier is free
  text, not an error.** `cart@checkout:eu` can't be a `field:value` pair, so it
  is searched for as the literal string `cart@checkout:eu`. If you meant the
  tag, write `tag:cart@checkout=eu`.

## The response envelope

The three searched lists answer an object, not a bare array:

```jsonc
{
  "data": [ … ],             // the rows
  "total": 1204,             // always a number, never "1204+"
  "total_is_capped": false,  // true => counting stopped early
  "next_cursor": "…",        // null on the last page
  "clamped": null            // or { "field": …, "to": …, "reason": … }
}
```

**`total` with `total_is_capped: true` means "at least this many".** Counting
stops at **10,000** so that counting never becomes the expensive part of the
request. When the flag is true the number is a floor, not a total — render it as
`10,000+`, never as an exact figure, and never divide by a page size to get a
page count.

**`clamped` is the planner telling you it narrowed your time window.** It fires
when your query has no index to stand on — a wildcard, a `~` substring, or a
free-text term — because an unbounded scan of the largest tables in the system
is not something to do quietly. The object names the column that was narrowed on
*that* resource (`last_seen` on Exceptions, `occurred_at` on Occurrences and
Events), how far back the window now reaches, and why:

```json
{ "field": "occurred_at", "to": "30d",
  "reason": "unindexed predicate (a wildcard, substring, or free-text match) requires a bounded time window" }
```

It only ever **tightens** the window you asked for, never widens it, so you can
always trust that every row returned is inside the range you selected. The
default is 30 days and an operator can change it with `SEARCH_SCAN_CLAMP_DAYS`.

Note the honest gap: `clamped` reports the *planner's* clamp. The Events
endpoint separately caps `since_days` at 365 whatever you send, and that cap is
**not** reported here — so a request asking for ten years of events silently
gets one, with `clamped: null`.

## Paging with a cursor

`next_cursor` is an opaque token. Send it back as `cursor=` to get the next
page; when it's `null` you're on the last one.

```
GET /v1/apps/{id}/issues?query=is:unresolved&limit=50
   -> { data: [50 rows], next_cursor: "MjAyNi0wOC0wOVQxMjozMDo0NS4wMDAwMDBafDExMT…" }
GET /v1/apps/{id}/issues?query=is:unresolved&limit=50&cursor=MjAyNi0wOC0w…
   -> { data: [50 rows], next_cursor: … }
```

It's opaque, not secret — it encodes only the sort timestamp and row id you just
received — but treat it as a token to echo back rather than a structure to build
yourself.

**Why not `offset`?** Offset paging over a live table repeats and skips rows: an
event ingested while you're reading page 2 shifts everything down, so page 3
starts on a row page 2 already showed you. A cursor names a *position in the
ordering* rather than a count of rows skipped, so a walk visits every row
exactly once. `offset=` is still accepted on these three endpoints so old links
don't 400 — but it is **ignored**, and you get page one.

**`sort=`** takes a column, optionally prefixed with `-` to reverse it (the
default everywhere is newest-first, so `-last_seen` is oldest-first). It is
restricted to orderings that have an index able to page stably:

| List | Sortable |
|---|---|
| Exceptions | `last_seen` (default), `first_seen` |
| Occurrences | `occurred_at` (default) |
| Events | `occurred_at` (default) |

Anything else is a 400 naming what is allowed. More orderings arrive with their
indexes; serving an unstable sort and letting it duplicate rows is precisely the
defect cursors were introduced to fix.

**Page size.** `limit` defaults to 50 on Exceptions and Events (max 200) and 30
on an issue's Occurrences (max 100).

### In the dashboard

Exceptions, Events and the issue-detail Occurrences list have **Previous /
Next** buttons that walk the cursor for you. They show `412 issues · Page 2`
rather than a row range like `51–100`, deliberately: a row range would be
guessing that every page before this one was exactly full.

Changing the query, the filters, the date range or the environment resets you to
page one — a cursor from one result set means nothing against another.

## Panels that don't follow your filters

Some panels beside those tables run their own request that carries **less** of
the page's query than the list does — the Exceptions stat tiles and Occurrences
chart, and the Events volume chart and Top-events list. That is on purpose: with
the default `status:unresolved` chip applied, a filtered "Unresolved" tile would
just equal "Total" and every other tile would read 0. They're the broad view.

What changed is that they now **say so**. When a control they don't carry is
active, a caption appears under the panel naming it — for example:

- *The filters and search don't apply to these totals.*
- *The filter doesn't apply to this chart.*
- *Only the Event filter applies to this chart — the date range doesn't.*

No caption means the panel and the list below it are looking at the same set.
The caption names what the panel **dropped**, not the scope it covers, because
"app-wide" would be wrong: these panels are app- *and* environment-scoped just
like the list, and the environment switcher moves them together. The mismatch
is never the environment.

## Per-environment issue statistics

With an environment selected in the Topbar, an issue's `times_seen`,
`users_seen`, `first_seen`, `last_seen`, `level`, `culprit` and `title` are
**re-derived from that environment's occurrences** rather than showing app-wide
totals. Pick `staging` and the counts are staging's counts.

Two things follow from that, both worth knowing:

- **The numbers are windowed.** They count occurrences inside the date range you
  selected, so they won't match the app-wide lifetime totals even for the same
  environment.
- **Ordering stays by the stored, app-wide `last_seen`.** So under an
  environment selection rows can appear *slightly out of order* relative to the
  per-environment `last_seen` shown in the row. This is a real, accepted
  limitation: stable paging requires ordering by an indexed stored column, and a
  per-environment `last_seen` isn't one. The alternative — offset paging on a
  derived value — is the duplicate-rows bug this all exists to remove.

With no environment selected (the whole app) none of this applies: the stored
columns already are the app-wide truth, and they cover data old enough to have
been tiered out of the events table.

## The older spellings still work

Everything above is the `query=` parameter. The pre-language wire format is
unchanged and is translated into exactly the same query, so shared links and
bookmarks keep returning the same rows:

| Old | New |
|---|---|
| `filter=level:eq:error` | `level:error` |
| `filter=level:neq:error` | `!level:error` |
| `filter=culprit:contains:checkout` | `culprit:~checkout` |
| `filter=times_seen:gt:100` | `timesSeen:>100` |
| `filter=tag:eq:region=eu` | `tag.region:eu` |
| `q=timeout` | a bare `timeout` term |

Repeated `filter=` params AND together, and `q=` ANDs on top, same as before.
When `query=` is present and non-empty it wins outright; otherwise `filter=`/`q=`
are used.

### The filter chips in the UI

On **Exceptions** and **Events**, **+ Add filter** still builds `field ·
operator · value` chips, and they still encode into the page URL alongside `q=`,
so a filtered view remains shareable and survives a reload. The query box runs
on an explicit submit — the **Search** button or **Enter** — so typing never
queries; the button turns blue while you have text the list has not run yet,
and clearing the box with **×** applies at once. Adding a chip or changing the
date range still reloads immediately.

**Operators**, by field type:

| Field type | Operators | Meaning |
|------------|-----------|---------|
| text | `=` &nbsp; `≠` &nbsp; `contains` | exact / not-exact / case-insensitive substring |
| enum | `=` &nbsp; `≠` | exact / not-exact against a fixed option list |
| number | `=` &nbsp; `>` &nbsp; `<` | numeric compare |
| tag | `contains` &nbsp; `=` | key/value match against the event's `tags` JSONB (`contains` is the default) |

**Exceptions** chips: `Level`, `Status`, `Type`, `Culprit`, `Events` =
times-seen, `Users` = users-seen, `Tag`. **Events** chips: `Event` name,
`User` = distinct_id, `Session` id, `Release`, `Tag`. An issue's
**Occurrences** offers `Tag` only.

There is no `Environment` chip: environment is picked once in the Topbar and
scopes every panel on the page together. (The API still accepts
`filter=environment:eq:<name>` for back-compatibility, and `environment:<name>`
in the query language, both gated on `env:read`.)

The `Tag` chip is two inputs — a key and a value — composed into one
`key=value` filter value, split on the **first** `=` so a value containing `=`
still round-trips. Its `contains` is the default because "search by tag" usually
means a forgiving match; its `=` is the index-backed one.

`Tag` and `workflow` are refused outright without `event:read` rather than
quietly narrowed, so a page that hits that 403 offers to drop the chip instead
of a Retry that could never work.

## What each free-text box matches

A free-text term is a **case-insensitive substring** matched against a fixed set
of columns per resource. Anything containing your term, anywhere in one of those
columns, matches.

- **Exceptions** — `title` **or** `type` **or** `culprit`, **or** the underlying
  event's `tags`/`contexts`/`extra` payload. So `timeout` finds it whether it's
  in the error message, the exception class, the culprit frame, or a
  tag/context/extra value.
- **Occurrences** — `message` **or** `exception_type` **or** `exception_value`,
  **or** the `tags`/`contexts`/`extra` payload.
- **Events** — event `name` **or** `distinct_id`, **or** the
  `tags`/`contexts`/`extra`/`properties` payload. (The stream always hides the
  synthetic `$screen` view events.)
- **Users** — `distinct_id` **or the entire traits blob**. Because it searches
  the JSON properties as text, you can find people by *any* trait value — an
  email, a plan name, a company — not just their id.
- **Devices** — a single blob of `family`, `model`, `os_name` and `device_key`
  glued together, so `iphone 15` or `macos` both work. (`os_version`, `arch`,
  `browser` and last-seen user are shown but **not** searched.)
- **Screens** — the `screen` name only.

**The payload half needs `event:read`.** Without it your term is matched against
the plain text columns only, and the `contexts`/`extra`/`tags` scan is skipped —
because those are the very columns your event bodies arrive with nulled. The
search is narrowed rather than refused: it's still an honest answer to "find
rows containing this". On an issue's occurrences the API also reports
`payload_searched` on the `events/stats` response — `null` for "no search ran",
`false` for "ran, payload excluded", `true` for "ran over everything".

Leaving the box empty applies no filter. The search runs within the page's
**date range** and returns the most recent matches first.

## What's fast and what's a scan

Every dimension is classified, and the classification is what drives the window
clamp described above.

- **Indexed** — served directly by an index. `is`/`status`, `firstSeen`,
  `lastSeen`, `environment`, `distinctId`, event `name`, and `tag.<key>` with
  `=` (which compiles to a JSONB containment check `tags @> {"key":"value"}`
  backed by a GIN `jsonb_path_ops` index).
- **Bounded** — no index of its own, but only ever evaluated after an indexed
  predicate has already narrowed the candidate rows. Most columns land here:
  `level`, `type`, `culprit`, `release`, `session`, `screen`, the `os.*` /
  `browser.*` / `device.*` / `extra.*` / `contexts.*` JSON paths, `timesSeen`,
  `usersSeen`.
- **Scan** — the value has to be read off every candidate row. Issue `title`,
  occurrence `message`, `stack.*`, `traits.*`, every `~` substring and `*`
  wildcard, and free text. A query that is *all* scan is what triggers the
  window clamp.

Some further guarantees that are easy to want and worth stating:

- **Injection-safe.** Every value you type is a **bound query parameter**, never
  spliced into SQL text — including JSON paths and tag keys, which are folded
  into a JSON object built in code and bound whole. Search input cannot alter
  the query.
- **Scoped and bounded.** Every search is filtered to your `app_id`, to your
  environment, and to the selected date window; ordered by recency; and capped
  at the page limit. Users search is the one exception with **no** date window —
  it looks across all time.
- **Not full-text search.** No stemming, relevance ranking, fuzzy matching or
  tokenization, and no external search engine (no Elasticsearch, no RediSearch)
  or `tsvector`/trigram index. Substring matching is a scan bounded by app +
  environment + date window + row cap — simple and predictable.
- **`%` and `_` are literal.** They're SQL `LIKE` metacharacters, and every
  search box and every `~` value escapes them, so typing `50%` searches for the
  text `50%`. The one place they behave as wildcards is where you asked for it:
  the `*` in a pattern like `user.email:*@acme.com`.

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

- **Mind the date range.** Server-side search only looks inside the selected
  window (except Users). The Exceptions picker goes up to **All**; the Events
  picker stops at **90d**, and the Events API caps anything you send at
  **365 days** regardless.
- **A typo is now visible.** If a query returns nothing, check for a 400 before
  concluding there's no data — an unknown field says so out loud instead of
  looking like an empty result.
- **Combine, don't cram.** Reach for a field term (`is:unresolved`,
  `timesSeen:>100`) when you want a precise cut, and keep bare words for "does
  this string appear anywhere".
- **Prefer `=` over `~` on tags.** `tag.region:eu` is index-backed;
  `tag.region:~eu` is a scan and will get your window clamped.
- **Share a filtered view.** The filters live in the URL — copy the address bar
  to hand someone the exact same view. Just don't expect a `cursor=` in a shared
  link to mean anything later; positions move.

## What a privacy mask does to search

The **[Privacy Inspector](Privacy-Inspector.md)** rewrites a masked value to the
JSON string `"****"` and keeps the key. Search has no special case for that, on
purpose: a masked row is not hidden and not excluded — it simply no longer
*contains* what you are looking for, so it stops matching and nothing says why.

For a column that has been masked:

- **Free-text stops matching.** The scan over
  `tags`/`contexts`/`extra`/`properties` sees `"****"`, not the old value.
- **`tag.<key>:value` stops matching.** JSONB containment compares against the
  stored value, which is now `"****"`.
- **Matching a stored number stops working.** The masked value is the
  **string** `"****"`, whatever it used to be, so `extra.cart_value_cents:4200`
  no longer matches the rows it used to. (Values inside JSON are compared as
  text either way — `>` / `<` were never available on them; the typed numeric
  columns `timesSeen` / `usersSeen` are aggregates and are not maskable.)
- **Finding a person by email stops working on `event_user`.**
  `error_events.event_user` is the per-event snapshot of the person (`user.id`,
  `user.email`) and is the column behind the `user.*` fields. Once it is masked,
  `user.email:*@acme.com` returns nothing for masked rows.
- **`has:` still works.** Masking rewrites the value but keeps the key, so
  key-existence questions are unaffected. `has:extra.cart_value_cents` therefore
  separates "this field was never sent" from "this field was sent" — it cannot
  tell you *which* of the latter were masked, but it is the one question a mask
  does not change the answer to.
- **The Users box loses masked traits.** That box searches the whole traits
  blob — `event_users.properties` — so a masked trait is no longer findable
  there either.

Masking is not retroactive across the whole dataset: it covers hot Postgres and
all future ingest, and it stops at the tiering boundary. A term that matches
older rows and nothing recent (or the reverse) may be a mask, not a data gap —
**Manage → Privacy → Audit** is where you find out. The full list of what a mask
does and does not reach is on **[Privacy Inspector](Privacy-Inspector.md)**.

---

Related: **[Dashboard](Dashboard.md)** · **[Architecture](Architecture.md)** ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.
