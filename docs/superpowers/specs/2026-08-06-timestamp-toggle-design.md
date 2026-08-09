# Timestamp display toggle

**Date:** 2026-08-06
**Status:** Approved design, not yet implemented

## Problem

Relative timestamps ("3 minutes ago") are readable at a glance and useless when
you need to correlate an exception with a deploy, a log line, or another
incident. The dashboard currently offers the absolute value only as a hover
`title` — invisible on touch devices, unselectable, and impossible to copy.

The pattern is repeated verbatim across the app:

```svelte
<dd title={formatDateTime(issue.first_seen)}>{relativeTime(issue.first_seen)}</dd>
```

`IssueDetail.svelte:477` and `:481` (First seen / Last seen) are the sites that
prompted this, but the same construction appears at `Issues.svelte:237`,
`UsersExplorer.svelte:255,258`, `DevicesInventory.svelte:187`,
`WorkflowsList.svelte:194`, `PersonProfile.svelte:120,141,146`, and
`DeviceDetail.svelte:106,107` — roughly a dozen call sites, all hand-rolled,
already slightly inconsistent (`DeviceDetail` uses `StatTile`'s `sub` slot
instead of a tooltip; `PersonProfile` renders bare relative text with no
absolute value at all).

## Solution

One `TimeValue` component replacing every site, with the relative/absolute
choice held in a single app-wide store and persisted. Clicking any timestamp
toggles all of them — the behaviour Sentry has, and the reason it works is that
the question "when exactly?" is never about one row.

### Why not per-instance state

Independent toggles mean toggling a 50-row table one cell at a time. The user's
intent is a mode ("I am correlating timestamps right now"), not a property of a
particular value.

### Why a component rather than a helper function

A helper would still leave every call site to wire up its own click handler,
`title`, keyboard affordance and ARIA. The variance already visible across the
dozen existing sites is the argument: a shared component is the only version of
this that stays consistent.

---

## Design

### The store

A small rune-based store persisted to `localStorage`, following the existing
store idiom in `lib/stores/`.

- Two states: `relative` (default) and `absolute`.
- Default is `relative` — it is what every site shows today, so an existing
  user sees no change until they ask for one.
- A malformed or absent stored value falls back to `relative` rather than
  throwing. A corrupt preference must not break every timestamp in the app.

Note for implementation: `$state` deep-proxies stored values, so a plain string
preference is the right shape here — no object wrapper, no identity comparison.

### The formatter

No existing helper produces `yyyy-MM-DD HH:mm:ss`:

- `formatDateTime` → "Aug 6, 2026, 02:15 PM" (locale, minute precision)
- `formatDateTimeSeconds` → locale-formatted with seconds
- `formatDateTimeZone` → adds a timezone name

Add `formatTimestamp` to `lib/utils/format.ts` producing exactly
`yyyy-MM-DD HH:mm:ss`, zero-padded, 24-hour, in the **viewer's local time** —
consistent with `relativeTime` and `formatDateTime`, both of which are local.
Toggling therefore changes precision, not the instant's apparent value.

It follows the file's existing null contract: `null` / `undefined` / unparseable
all return `—`, exactly as its neighbours do.

### The component

`TimeValue.svelte` takes a timestamp and renders the current mode's text as a
`<button>`, because it is interactive and must be keyboard-reachable and
screen-reader-announced. It is styled to read as text, not a control — a table
of fifty buttons that all look like buttons is worse than what exists now.

- The opposite representation stays in `title`, so hovering still answers the
  question without a click.
- `aria-label` names both the value and the action.
- Clicking toggles the global store.
- Null/invalid input renders `—` as plain text with no button — nothing to
  toggle.

An optional prop covers `DeviceDetail`'s `StatTile` usage, where the absolute
value belongs in the `sub` slot rather than a tooltip.

### Migration

Replace all dozen sites. Two are not like the others and are the reason to do
this as one pass rather than only touching `IssueDetail`:

- `PersonProfile.svelte:120-121` currently shows relative text with **no**
  absolute value anywhere — it gains the capability.
- `DeviceDetail.svelte:106` shows First seen as **absolute only**, with no
  relative form — it becomes consistent with every other First seen in the app.

`Inspector.svelte:255` renders `f.last_seen_at ?? '—'` — a raw unformatted
timestamp, which is a pre-existing display bug. It is in scope for this pass
since it is the same problem.

---

## Error handling

| Case | Behaviour |
|---|---|
| `null` / `undefined` / unparseable | `—`, plain text, not interactive |
| Corrupt `localStorage` value | Falls back to `relative` |
| `localStorage` unavailable (private mode) | Works in-memory for the session, no throw |

## Testing

- `formatTimestamp` unit tests: known instant → exact expected string;
  zero-padding for single-digit month/day/hour; `—` for each null-ish and
  unparseable input.
- Store: default is `relative`; toggle flips; value survives a reload; corrupt
  stored value falls back.
- Component: renders relative by default, absolute after toggle; null input
  renders no button.

**Runtime verification:** toggle a timestamp on Issues and confirm the mode
persists across a reload *and* across navigation to Issue detail — the whole
point is that it is one app-wide mode, and that is not observable from unit
tests.

## Build order

1. `formatTimestamp` in `lib/utils/format.ts`.
2. The preference store.
3. `TimeValue.svelte`.
4. Migrate `IssueDetail` first (the requested site), verify, then the remaining
   sites in one pass.

## Decisions locked during design

| Decision | Choice | Why |
|---|---|---|
| Scope | Shared component, all sites | Sites are already identical and already drifting |
| State | App-wide, persisted | The intent is a mode, not a per-row property |
| Timezone | Viewer's local | Matches every other timestamp; toggle changes precision only |
| Format | `yyyy-MM-DD HH:mm:ss` | As requested; no existing helper produces it |
| Default | `relative` | Matches today's behaviour, so nothing changes unasked |
