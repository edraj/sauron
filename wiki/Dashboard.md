# Dashboard

The dashboard is a Svelte SPA that reads the backend API with JWT auth and per-request
RBAC. Its left sidebar is organized into four groups plus a Docs link. Everything is
scoped to the currently-selected **App** (see the tenancy model in **[Home](Home.md)**).

See also: **[Getting Started](Getting-Started.md)** ·
**[Search & Filtering](Search.md)** (searching & filtering the lists below) ·
**[Architecture](Architecture.md)** (the queries behind these screens) ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

## Monitor

- **Overview** — the app's health at a glance: signal volume, top issues, and recent
  activity.
- **Exceptions** — the grouped **issues** list (errors fingerprinted into issues).
  Defaults to the all-time range with an "All" range option. Narrow it with
  structured `field · op · value` filters (level, status, type, culprit, event /
  user counts) or free-text search over the title, type, and culprit — see
  **[Search & Filtering](Search.md)**. Drill into an issue to see occurrences, the
  stack trace, breadcrumbs, affected users, and the tie-in to the same person's
  events.
- **Performance** — aggregated **transaction** timings (p50/p95/etc.) by route /
  operation, split by `op` (`navigation`, `http`, `resource`, `screen_load`,
  `custom`), with error rates.

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
import { Sauron } from '@sauron/browser';

Sauron.init({
  dsn: 'https://<public_key>@<host>/<project_id>',
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
- **App settings** — per-app configuration, including the app's **DSN**.

## Docs

A bottom-of-sidebar **Docs** link opens the in-app integration guides — install + init
snippets for each SDK (web, Flutter, Node, Python, C#), mirroring the
**[Getting Started](Getting-Started.md)** flow.

---

Jump to an SDK: **[Browser](Browser-SDK.md)** · **[Flutter](Flutter-SDK.md)** ·
**[Node](Node-SDK.md)** · **[Python](Python-SDK.md)** · **[C#](CSharp-SDK.md)**.
