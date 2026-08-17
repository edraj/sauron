# Navigation direction (push / pop) on the JS SDK and the breadcrumb trail

**Date:** 2026-08-17
**Status:** design, awaiting review

## Problem

A navigation breadcrumb tells you the user moved between two screens, but not
which way. "Went to `/checkout`" and "backed out of `/checkout`" are the same
row in the trail, so an issue's breadcrumb history cannot answer whether the
user was progressing through a flow or retreating from it.

This is only half missing. Flutter already records the direction; the web SDK
throws it away, and the dashboard renders what Flutter sends as unreadable
key-value text.

## Inventory

| Surface | Records direction? | Where |
| --- | --- | --- |
| Flutter `SauronNavigatorObserver` | **yes** — `data: {'operation': 'push' \| 'pop' \| 'replace' \| 'remove'}` | [`widgets_binding_observer.dart:109`](../../../sdks/flutter/lib/src/integrations/widgets_binding_observer.dart) |
| JS `installHistory` | **no** — `pushState`, `replaceState` and `popstate` all funnel through one `emit()` | [`history.ts:38`](../../../sdks/js/src/integrations/history.ts) |
| JS `addNavigationBreadcrumb` | **no** — `data: {from, to}` only | [`breadcrumbs.ts:38`](../../../sdks/js/src/api/breadcrumbs.ts) |
| Backend `Breadcrumb.data` | free-form `serde_json::Value` | [`envelope.rs:219`](../../../backend/crates/sauron-core/src/envelope.rs) |
| Dashboard `BreadcrumbTrail` | renders `data` as generic `k: v` pairs | [`BreadcrumbTrail.svelte:11`](../../../dashboard/src/lib/components/BreadcrumbTrail.svelte) |

Two consequences fall out of that table and shape the whole design:

1. **No backend work.** `data` is an untyped `serde_json::Value`, so a new key
   needs no migration, no schema change, and no API version bump. The JS wire
   fixture (`sdks/wire-fixtures/js.json`) contains no breadcrumbs at all, so
   nothing regenerates and the `sdk_wire_conformance` test is untouched.
2. **The dashboard is already receiving direction it does not show.** A Flutter
   pop renders today as the literal string `operation: pop` in the summary line.
   The display work is not "surface new data", it is "stop printing a debug dump".

## Decisions

### A — JS adopts Flutter's vocabulary, not a new one

`operation` takes the same four values Flutter emits: `push`, `pop`, `replace`,
`remove`. Web maps `history.pushState` → `push`, `history.replaceState` →
`replace`, `popstate` → `pop`. `remove` has no web equivalent and simply never
appears — the reader handles four values, the web writer emits three.

The key lives inside `data`, beside `from` and `to`, because that is where
Flutter puts it. A JS navigation breadcrumb becomes:

```json
{ "type": "navigation", "category": "history", "level": "info", "message": null,
  "data": { "from": "/", "to": "/settings", "operation": "push" } }
```

`addNavigationBreadcrumb(from, to)` grows a third parameter. It is exported from
the SDK's public surface, so the parameter is optional and defaults to `push`;
existing callers keep compiling and keep their current meaning.

### B — a forward navigation is labelled `pop`

`popstate` fires for both `history.back()` and `history.forward()`, and the
event alone cannot separate them. Both are recorded as `pop`.

The alternative — stamping a monotonic index into `history.state` on each push
and comparing it on `popstate` — was rejected. `history.state` is also owned by
the host app's router (SvelteKit, React Router, Next), and writing to it to
improve a breadcrumb risks breaking the navigation being instrumented. The
observer's first duty is not to break the app.

This is a real fidelity limit, and the SDK docs must state it rather than let a
reader infer that `pop` means "the user went back".

### C — direction reads off the timeline node, not a chip

`BreadcrumbTrail` gives every crumb a 9px dot coloured by `level`. For
`type === 'navigation'` crumbs carrying an `operation`, the dot is replaced by a
directional glyph from the existing `Icon` registry — all four already exist, so
no new imports:

| operation | icon | colour | reading |
| --- | --- | --- | --- |
| `push` | `arrow-right` | `var(--info)` | forward, deeper into the flow |
| `pop` | `arrow-left` | `var(--neutral)` | backward |
| `replace` | `refresh` | `var(--neutral)` | lateral — same depth, new route |
| `remove` | `x` | `var(--warning)` | route torn out of the stack |

Colour carries direction (forward / not-forward / destructive) and the glyph
names the exact operation, so the two are legible together and the colour alone
is never the only signal.

Every other breadcrumb type keeps today's level-coloured dot, and the summary
line keeps today's generic rendering — with one subtraction: `operation` is
dropped from the `k: v` flattening, since the node now shows it. A JS crumb's
summary therefore reads `from: /, to: /settings`, and a Flutter crumb's keeps
its route name from `message`.

**Rail geometry.** The node column is a fixed 12px wide and the connecting line
is absolutely positioned inside it. A glyph is larger than a dot, so the column
widens to 14px **for every crumb, not just navigation ones** — a per-row width
would bend the vertical rail into a zigzag on any trail mixing navigation with
other crumbs. The glyph needs `background: var(--surface)` for the same reason
the dot has a 3px surface-coloured box-shadow: to punch a hole in the line
running behind it.

`.line`'s `top` stays at 12px. An earlier draft moved it to 14px to clear the
taller glyph; that turned out to be unnecessary — the line already starts
*behind* the mark and is hidden by the same opaque background, so moving it
would only have opened a gap on dot rows. Verified in the harness: every
junction overlaps by a uniform −3px in both themes.

**Accessibility.** `Icon` renders `aria-hidden="true"`, so an icon-only
direction would be silent to a screen reader. The node wrapper carries
`role="img"` and `aria-label={operation}`.

## Testing

**JS SDK** — `installHistory` has no test today; `sdks/js/test/history.test.ts`
is new and pins:

- `pushState` → `operation: 'push'`, `replaceState` → `'replace'`, `popstate` → `'pop'`
- `history.forward()` also yields `'pop'` — the decision in B, asserted rather
  than left as a comment, so a later "fix" has to argue with a test
- the existing same-path dedupe (`if (from === to) return`) still suppresses a
  `replaceState` to the current URL, and does so *before* direction is recorded
- `addNavigationBreadcrumb(from, to)` with no third argument still yields `'push'`

**Dashboard** — `BreadcrumbTrail` currently has no test file. The visual
treatment is not worth a DOM test, but two things are:

- a navigation crumb with `operation` renders the matching icon and does **not**
  print `operation:` in the summary text
- a navigation crumb *without* `operation` (an older SDK, or a hand-built
  breadcrumb) falls back to the level-coloured dot instead of rendering nothing

**Docs** — `sdks/js/README.md` and `wiki/JS-SDK.md` gain the `operation` key and
state the back/forward limit from B explicitly.

## Out of scope

- The `$screen` product event keeps carrying `{screen}` only. Putting direction
  into the events table is a separate change with a backend and query surface.
- Timeline navigation rows in `timeline-row.ts` are driven by `$screen` events,
  not breadcrumbs, so they are untouched by this work.
- Node and Python SDKs have no history API to instrument.
- No SDK version bump is decided here; it rides whatever release picks this up.
