# Design: Styled funnel dialogs + searchable saved funnels

**Date:** 2026-07-14
**Status:** Approved (brainstorming) — pending spec review
**Area:** `dashboard/` (new `ui/Modal`, `ui/ConfirmDialog`; `pages/FunnelBuilder.svelte`)

## Goal

The funnel builder currently leans on browser primitives that break the dashboard's visual language: saving a template uses `window.prompt()` and deleting uses `window.confirm()`. The saved-funnels list is a flat wrap of chips with no way to search when the library grows. Replace these with **styled, design-system dialogs** and make the saved list **searchable**, so picking (load) or cloning (duplicate) a funnel scales past a handful of templates.

This is the dashboard's **first modal** — Projects/Settings/Members all use inline forms and native `prompt`/`confirm`. So the core deliverable is a reusable `Modal` primitive that those pages can adopt later, not a one-off.

## Decisions (settled in brainstorming)

- **Inline search on the card** (not a separate picker modal). Keep the "Saved funnels" card in-page; add a search box above the chips that filters by name. Load = pick, Duplicate = clone stay as per-chip actions.
- **Save dialog captures name + optional description.** The API's `SaveFunnelBody` already carries `description`; the current `prompt()` throws it away.
- **One dialog, two modes.** The same funnel-details dialog powers **Save-as-new** (create mode: empty name / "Copy of…" seed, empty description) and **Update** (edit mode: pre-filled with the loaded funnel's name + description, editable). Update thus overwrites `{name, description, steps}`, not just steps.
- **Styled confirm for delete** replaces `window.confirm()`, reusing the same `Modal` primitive.
- **Feedback via toasts.** Route save/update/duplicate/delete success + errors through the existing `toastStore` (matching Projects/Settings), replacing the tiny inline `saveError` text.

## Approach

Implement `Modal` on the **native `<dialog>` element** via `showModal()`. This gives focus-trapping, Escape-to-close, background inertness, and top-layer stacking for free — all fully stylable with the existing CSS-variable system. The alternative (a hand-rolled `position:fixed` overlay like `Toast`) would mean re-implementing focus trap + scroll lock by hand; rejected.

## New components

### `lib/components/ui/Modal.svelte`
Reusable dialog primitive.

- **Props:** `open: boolean` (bindable), `title?: string`, `size?: 'sm' | 'md'` (default `md`), `onclose?: () => void`, `children: Snippet`, `footer?: Snippet`.
- Binds a `<dialog>` element; a `$effect` syncs `open` → `dialog.showModal()` / `dialog.close()` (guarded against redundant calls).
- Header row: `title` + a close (`X`) icon button. Body wraps `children` in a scrollable region (`max-height` capped so long content scrolls, not the page). Optional `footer` snippet, right-aligned button row.
- Escape (`cancel` event) and backdrop click (a click whose target is the `<dialog>` itself, outside the panel) both call `onclose`.
- Styled with `--surface`, `--border-strong`, `--radius-lg`, `--shadow-lg`; `::backdrop` is dim + slight blur. Scale/fade-in animation reusing `Toast`'s easing.
- `role`/labelling handled by native `<dialog>` + `aria-labelledby` pointing at the title.

### `lib/components/ui/ConfirmDialog.svelte`
Thin wrapper composing `Modal`.

- **Props:** `open` (bindable), `title`, `message`, `confirmLabel?` (default `Confirm`), `cancelLabel?` (default `Cancel`), `danger?: boolean`, `loading?: boolean`, `onconfirm`, `oncancel`.
- Renders the message in the body; footer = Cancel (`secondary`) + Confirm (`danger` when `danger`, else `primary`) with the `loading` spinner.

## `FunnelBuilder.svelte` changes

New local state: `showDetailsDialog`, `dialogMode: 'create' | 'edit'`, `dialogName`, `dialogDesc`, `savingDialog`, `pendingDelete: SavedFunnel | null`, `deleting`, `funnelSearch`.

- **Save-as-new** → opens the details dialog in `create` mode (name empty; description empty). Submit → `saveFunnel(aid, { name, description })`, set `loadedId`, reload list, `toastStore.success`, close.
- **Update** → opens the details dialog in `edit` mode, pre-filled with the loaded funnel's `name`/`description`. Submit → `updateFunnel(aid, loadedId, { name, description, steps })`, reload, toast, close.
- **Details dialog body:** required **Name** `Input` + optional **Description** `<textarea>` styled to match `Input`. Primary button disabled until `name.trim()` is non-empty. Escape / backdrop / Cancel discard and reset fields.
- **Delete** → sets `pendingDelete`; `ConfirmDialog` (danger) message names the funnel. Confirm → `deleteFunnel`, keep existing "clear `loadedId` if it was the loaded one" behavior, toast, close.
- **Duplicate** → unchanged call, but add success/error toasts.
- **Inline search:** render a `SearchInput` above the chips whenever the "Saved funnels" card is shown (i.e. `saved.length > 0`). `filteredFunnels = $derived` — case-insensitive, trimmed match on `f.name`. Render `filteredFunnels`; if the filter yields none, show a small "No funnels match …" line. Clearing search restores all.
- Remove the inline `saveError` text and its state; all outcomes go through `toastStore`.

## Data flow & non-goals

- **No API or backend changes.** `SaveFunnelBody` already supports `description`; `updateFunnel` already accepts the full body.
- Dialogs are pure local UI state; no new stores.
- **Non-goals:** retrofitting Projects/Settings/Members onto `Modal` (future), search over step contents (name-only), keyboard-driven list navigation, unsaved-changes guards.

## Edge cases

- Empty/whitespace name disables Save.
- Escape, backdrop click, and Cancel all discard edits and reset dialog fields.
- Deleting the currently-loaded funnel clears the builder's `loadedId` link (existing behavior preserved).
- Search is case-insensitive and trims; empty query shows all; no-match shows an empty line, not a broken card.
- Rapid open/close and double-submit guarded by the `saving`/`deleting` flags (buttons show `loading`, stay disabled).

## Testing / verification

Frontend-only, no component-test harness in `dashboard/`. Verify by:

1. `svelte-check` (types) clean.
2. Run the dashboard dev server and drive the flow in the preview: open Save dialog → save with name + description → confirm chip appears; filter the list; Update a loaded funnel (name/desc editable); trigger delete → confirm dialog → row removed; Escape/backdrop close paths. Capture a screenshot of the styled dialog as proof.
