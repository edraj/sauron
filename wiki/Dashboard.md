# Dashboard

The dashboard is a Svelte SPA that reads the backend API with JWT auth and per-request
RBAC. Its left sidebar is organized into four groups plus a Docs link. Everything is
scoped to the currently-selected **App** (see the tenancy model in **[Home](Home.md)**).

See also: **[Getting Started](Getting-Started.md)** ·
**[Search & Filtering](Search.md)** (searching & filtering the lists below) ·
**[Architecture](Architecture.md)** (the queries behind these screens) ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

## Signing in

### Forgot your password

The sign-in page carries a **Forgot your password?** link. Enter your address and
Sauron emails a link that expires in **1 hour**. The page shows the same
confirmation whether or not an account exists for that address — deliberately,
so nobody can use it to discover who has an account here.

Opening the link lets you choose a new password. Doing so signs you out of every
device, including the one you are on, and returns you to the sign-in page. Reset
links are single-use, and a link stops working the moment the account's password
changes for any other reason.

If nothing arrives: check your spam folder, then try again in a little while.
Three requests per address per hour are allowed.

## Monitor

- **Overview** — the app's health at a glance: signal volume, top issues, and recent
  activity.
- **Exceptions** — the grouped **issues** list (errors fingerprinted into issues).
  Defaults to the all-time range with an "All" range option. Narrow it with
  structured `field · op · value` filters (level, status, type, culprit, event /
  user counts, tag, workflow) or free-text search over the title, type, and culprit —
  each row shows the exception type and the crash-site frame beneath the title. See
  **[Search & Filtering](Search.md)**. Drill into an issue to see occurrences, the
  stack trace, breadcrumbs, affected users, and the tie-in to the same person's
  events.
- **Performance** — aggregated **transaction** timings (p50/p95/etc.) by route /
  operation, split by `op` (`navigation`, `http`, `resource`, `screen_load`,
  `custom`), with error rates.

### What "crash-free sessions" means

The Overview's **Crash-free sessions** tile is the share of sessions in the range
that recorded **no uncaught exception**. It is worth being precise about, because
"crash" is a word people reasonably read three different ways.

**A crash is an error nothing in your code caught.** The distinction is not a
label anyone applies by hand — it is decided by where the exception ended up:

| your code | what the SDK records | counts as a crash? |
|---|---|---|
| `try { … } catch (e) { Sauron.captureException(e) }` | `mechanism.handled = true` | **No** — you caught it |
| the exception escapes with no `catch` anywhere | `mechanism.handled = false` | **Yes** — nobody caught it |

To call `captureException` you have to be holding the exception object, which
means you caught it. An error only ever reaches the SDK's global hooks
(`FlutterError.onError`, `window.onerror`, `sys.excepthook`, …) *because* no
`catch` intercepted it first. So the signal is automatic and cannot be forgotten:
you never tell Sauron that something crashed.

**A handled error is still an error.** It appears in Exceptions, it counts toward
the error rate, it can page you. It just does not lower crash-free — deliberately,
because an exception you caught and reported is the system working, not the app
breaking.

#### What it does not cover

Crash-free measures **uncaught exceptions**, which is narrower than "the app
died":

- **Native crashes** (SIGSEGV, ANR, an iOS watchdog kill) and **out-of-memory
  kills** are *not* detected. The process is gone before any SDK code can run —
  on iOS an OOM delivers no signal even to a native handler. These sessions count
  as crash-free because nothing was ever reported.
- **Memory leaks** are not an event at all; they only ever surface as an eventual
  OOM, which is the case above.
- Conversely, an uncaught exception that Flutter recovers from (a widget build
  error behind a red screen) *does* count, even though the process survived.

Read the number as "sessions with no uncaught exception", not "sessions where the
app stayed alive".

#### When the tile shows "—" instead of a percentage

Crash-free counts only errors whose `handled` state is **known**. An SDK that
never reports one produces zero crashes by construction, which is
indistinguishable from a perfectly healthy app — so rather than print a confident
`100%`, the tile shows `—` and *"No crash data from this SDK"*.

That happens when:

- the SDK does not capture uncaught errors. **[Node](Node-SDK.md)**,
  **[Python](Python-SDK.md)** and **[C#](CSharp-SDK.md)** ship this **off by
  default** — turn on `autoCaptureUnhandled` / `auto_capture_unhandled` /
  `AutoCaptureUnhandled`. **[Flutter](Flutter-SDK.md)** and
  **[Browser](Browser-SDK.md)** capture automatically with no configuration;
- or every error in the range predates the release that began recording it.

An app with sessions and no errors at all is a genuine `100%`, not `—`.

#### How it is computed

Each session carries `unhandled_errors_count`, incremented at ingest whenever an
error arrives with `handled = false`. A session is "crashed" when that count is
above zero. The **Sessions** list uses the same definition, so the two screens
cannot disagree.

The same distinction is searchable: `is:unhandled` selects errors nothing caught,
`is:handled` those your code reported itself, and `has:handled` the rows where the
state is known at all. Which lists accept each one is tabulated in
**[Search & Filtering](Search.md)**, whose field tables are kept honest against the
query catalog by a test.

## Tags, contexts & additional data

Open an issue (**Exceptions → an issue**) and the detail view surfaces the
developer-set metadata from its latest event in three panels:

- **Tags** — a flat `key → value` map you attach for filtering and grouping (e.g.
  `region = eu-central`, `checkout_step = payment`). These are exactly what the
  **`Tag`** filter and free-text search key off (see **[Search & Filtering](Search.md)**).
- **Contexts** — named, structured blocks (e.g. a `request` or `cart` object),
  shown as an expandable JSON tree.
- **Additional data** — loose one-off values (`extra`) that don't warrant a named
  context block.

This is **your app's** metadata — distinct from the SDK's machine-collected `context`
(device / OS / browser). You set it through each SDK's scope + capture API: a value set
on the scope is lifted onto every later error/event, and per-call values merge on top.

### Example

Say a checkout error should carry the region, the step it failed on, the cart, and the
A/B bucket. With the [Browser](Browser-SDK.md) SDK (the others are identical shapes —
see the table below):

```ts
import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: 'https://<public_key>@<host>/<environment_id>',
  tags: { region: 'eu-central' },            // default tag on every signal
});

// On the scope — lifted onto every later error/event:
Sauron.setTag('checkout_step', 'payment');            // → Tags panel
Sauron.setContext('cart', { item_count: 3, total: 42.5 }); // → Contexts panel
Sauron.setExtra('experiment_bucket', 'B');            // → Additional data panel

try {
  await pay();
} catch (err) {
  // …or attach values to just this one capture (merged over the scope):
  Sauron.captureException(err, { tags: { severity: 'high' } });
}
```

Open that error in **Exceptions → the issue** and its **Tags** panel shows
`region=eu-central`, `checkout_step=payment`, `severity=high`; **Contexts** shows the
`cart` block; **Additional data** shows `experiment_bucket=B`. You can then filter the
list with `Tag` `checkout_step` `payment` (see **[Search & Filtering](Search.md)**).

| SDK | Set on the scope | Per capture | Seed at init |
| --- | --- | --- | --- |
| [Browser](Browser-SDK.md) / [Node](Node-SDK.md) | `setTag('region','eu')` · `setTags({…})` · `setContext('cart',{…})` · `setExtra('bucket','B')` | `captureException(err, { tags, contexts, extra })` | `tags` / `contexts` / `extra` options |
| [Python](Python-SDK.md) | `set_tag` · `set_tags` · `set_context` · `set_extra` | `capture_exception(err, tags={…})` | same three options |
| [Flutter](Flutter-SDK.md) | `Sauron.setTag/setTags/setContext/setExtra` | `Sauron.captureException(e, tags: {…}, contexts: {…}, extra: {…})` | `o.tags` / `o.contexts` / `o.extra` |
| [C#](CSharp-SDK.md) | `SauronSdk.SetTag/SetTags/SetContext/SetExtra` | `CaptureException(ex, tags: {…})` | `Tags` / `Contexts` / `Extra` options |

For the tags-vs-contexts-vs-extra decision, see **[Best Practices §4](Best-Practices.md)**;
to find events by them, see **[Search & Filtering](Search.md)**.

## Explore

- **Events** — the raw product-analytics event stream (`track` calls) with names,
  properties, and the person each is attributed to. Filter by event name, user,
  session, environment, or release, or free-text search over the event name and
  `distinct_id`.
- **Sessions** — session analytics: a list of sessions and a per-session detail view
  that stitches a user's events, screens, transactions, and errors onto one timeline.
  A search box filters the loaded page by session, user, or device.
- **Users** — the people explorer. Each person (a `distinct_id`) has a profile showing
  their traits (from `identify`), their events, and their errors — the unified
  observability + analytics view. Search people by `distinct_id` **or any trait
  value**. (Also reachable via `/persons`.)
- **Devices** — a device inventory and per-device detail, keyed by the device context
  the SDKs send. Search across device family, model, OS, and key.
- **Screens** — screen/route analytics driven by the `$screen` views and the `screen`
  stamped on events/errors. A screens list (searchable by screen name) plus a
  per-screen detail view showing the activity on each screen. (Set the screen with
  `setScreen` in the [Browser](Browser-SDK.md) / [Flutter](Flutter-SDK.md) SDKs.)

### Filtering by time

Events, Sessions, Users and Devices each carry a time filter above their table:

    [ Last seen ▾ ]  [ in the last ▾ ]  [ 30 days ▾ ]

Two things are chosen independently — **which timestamp** the window applies to,
and **what shape** the window is.

| Page | Columns offered | Default |
|---|---|---|
| Events | Occurred | in the last 365 days |
| Sessions | Last activity, Started | Last activity, last 30 days |
| Users | Last seen, First seen | Last seen, last 365 days |
| Devices | Last seen, First seen | Last seen, last 30 days |

The shapes are **in the last N**, **after**, **before**, and **between**. Absolute
values are entered in your own timezone — the control shows the offset it is
using — and the range is half-open: a whole-day `to` includes that entire day.

Picking the column is what makes "new" askable. A person first seen a year ago
but active yesterday matches a *Last seen* window and not a *First seen* one, so
`First seen · in the last · 7 days` is the list of people who arrived this week.

Two details worth knowing:

- **The filter governs the table, not the charts.** The stat tiles and graphs
  above each table keep their own range picker, because the summary endpoints
  take a plain day count and cannot express a column choice or an absolute bound.
- **Devices and Users mean subtly different things by the same words.** On
  Devices the window decides *which devices are listed*, using each device's
  app-wide first/last sighting. On Users it filters the value the row actually
  shows, which is environment-scoped when an environment is selected. So under an
  environment, a person's "first seen" is when they first appeared *in that
  environment*.

Every list bounds its window at 365 days. If your request was wider, the response
says so rather than silently narrowing it, and the UI surfaces that.

The window is kept in the URL, so a filtered view can be bookmarked and shared.

## Analyze

- **Funnels** — a funnel builder over your event stream. Define ordered steps and see
  conversion / drop-off between them. Funnels can be **saved as templates**: name and
  store a funnel, then search, load, duplicate, or remove saved funnels later.
- **Journeys** — a journey/path graph that shows how users move between events and
  screens (branches, common paths).

## Manage

- **Projects** — projects and their apps; create apps and pick an `app_type`
  (`web · flutter · ios · android · react_native · node · python · csharp`). (Also
  reachable via `/apps`.)
- **Members** — org/project/app members and role grants (shown only to users with
  `member:read`). RBAC is enforced per request; the UI hides actions the caller can't
  perform.
- **App settings** — per-app configuration, including **Settings → Environments**:
  create/rename/retire environments and copy, rotate, or mute each one's **DSN**.

### Resetting a member's password

The row action menu on the members table carries **Reset password** for anyone
who is not you and is not deactivated. It needs both `member:credential` and
`member:manage`.

**This is a lockout, in the dialog's own words:** the member will not be able to
sign in until they use the emailed link. Their current password stops working
immediately and they are signed out of every device within a few seconds. The
link expires in 24 hours. The row then shows a **Reset pending** badge, visible
to anyone who can read the members list, so whoever fields "I can't log in" has
the answer without asking.

If the mail does not arrive, come back to the same menu — the item has become
**Cancel password reset**. Cancelling lets them sign in with their existing
password again, kills any link already sent, and still asks them to choose a new
password on their next sign-in. Cancel works even when SMTP is unconfigured;
**Reset password** refuses with a 503 and changes nothing, naming the missing
setting.

A member who also holds grants in another organization cannot be reset from
here, and neither can you reset yourself — use Change password for that.

## Docs

A bottom-of-sidebar **Docs** link opens the in-app integration guides — install + init
snippets for each SDK (web, Flutter, Node, Python, C#), mirroring the
**[Getting Started](Getting-Started.md)** flow.

---

Jump to an SDK: **[Browser](Browser-SDK.md)** · **[Flutter](Flutter-SDK.md)** ·
**[Node](Node-SDK.md)** · **[Python](Python-SDK.md)** · **[C#](CSharp-SDK.md)**.
