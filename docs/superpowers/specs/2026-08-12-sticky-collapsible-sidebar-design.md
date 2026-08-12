# Sticky sidebar with collapsible nav groups

Date: 2026-08-12
Status: approved

## Problem

Two defects in `dashboard/src/lib/components/layout/Sidebar.svelte`:

1. **The sidebar scrolls away.** `.shell` is `min-height: 100vh` and the *window*
   is the scroll container. `Topbar` survives this because it is
   `position: sticky; top: 0`. The sidebar is a grid item spanning both rows, so
   on a long page its box grows with the row while its content stays at the top —
   scroll down and the nav is gone. Its existing `overflow-y: auto` never engages,
   because the element is never shorter than its content.

2. **Nav groups can't be collapsed.** `Monitor`, `Uptime`, `Explore`, `Analyze`,
   and `Admin` are inert `<span>` labels. Twenty items are always on screen.

## Scope

`Sidebar.svelte` plus one new store. No change to `.shell`, `.content`, `Topbar`,
routing, or `page-access` visibility rules.

## 1. Sticky sidebar

CSS only, in `Sidebar.svelte`:

```css
.sidebar {
  position: sticky;
  top: 0;
  align-self: start;
  height: 100vh;
  overflow-y: auto;
}
```

`align-self: start` is load-bearing. Without it the grid item stretches to the
full height of the `sidebar` row, is never shorter than its containing block, and
`position: sticky` is a silent no-op — the rule would be present and green and do
nothing.

**Rejected alternative: full app shell.** Making `.shell` `height: 100vh` and
`.content` the sole scroll container is the nicer end state and would incidentally
fix `DataTable`'s `thead th { top: 0 }` currently sticking *behind* the topbar.
Rejected because three components compute sticky offsets against the window and
would each need re-tuning: `Docs.svelte`'s `.docs-nav`
(`top: calc(var(--topbar-h) + 16px)`, `max-height: calc(100vh - …)`),
`AdminShell.svelte`'s rail (`top: 0`), and `DataTable`. The sticky-sidebar-only
change touches none of them.

**Mobile.** At `≤860px` the sidebar is a horizontal rail in the *first* grid row.
The `@media` block must reset `position: static; height: auto`, or a `100vh`
sticky bar eats the viewport and collides with the already-sticky topbar.

## 2. Collapsible groups

### Store: `src/lib/stores/nav-collapse.svelte.ts`

Follows `theme.svelte.ts` / `time-format.svelte.ts`: `typeof window` guard,
`try/catch` around `setItem` (private-mode Safari throws on a full quota; a
cosmetic preference must not break the click), corrupt value falls back rather
than throwing.

Key `sauron.nav.collapsed`. It persists the **collapsed** labels, not the expanded
ones. Default is therefore expanded, and a group added to the nav later shows up
instead of silently starting hidden.

API: `isCollapsed(label)`, `toggle(label)`, `expand(label)`.

### Markup

`.group-label` becomes `<button aria-expanded aria-controls>` with a
`chevron-down` `Icon` that rotates `-90deg` when collapsed.

### Items always render; CSS hides them

Items stay in the DOM unconditionally. A `collapsed` class on `.group` hides them
with `display: none`, and the `≤860px` block un-hides them and drops the chevron.

This keeps the breakpoint entirely in CSS. Conditionally rendering with `{#if}`
would put the decision in JS, which does not know the breakpoint — a group
collapsed on desktop would then have its items hidden on the mobile rail, where
the group label (and therefore the toggle) is `display: none` and there is no way
to bring them back.

### Auto-expand on route change, not force-expand

An `$effect` reads `$location` and calls `expand()` on the group owning the new
route, reading the collapsed set under `untrack` so it is not a dependency and
cannot loop.

Deliberately not "the active group is always expanded": that reading makes the
toggle look broken — collapse `Explore` while on `/events` and nothing happens.
Auto-expand fires only when the route actually changes, so a manual collapse
afterwards sticks, and you can still never land on a page whose group is hidden.

## Testing

- Unit: `nav-collapse.test.ts` — default expanded, toggle round-trips through
  `localStorage`, corrupt JSON falls back, `setItem` throwing does not break the
  in-memory toggle.
- Browser: sticky holds at full scroll on a long page; toggle persists across
  reload; navigating into a collapsed group expands it; `≤860px` rail shows all
  items with no chevrons. Both themes.

No component-render harness exists in this repo (stores only), so the markup
claims are verified in the browser, not by a test.
