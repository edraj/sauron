# Dashboard issue UI: stacktrace collapsing + occurrence identity columns

Date: 2026-07-30
Status: approved

## Problem

Two rough edges on the issue detail screen:

1. **Stacktraces are unbounded.** A 60-frame trace pushes breadcrumbs, context, and
   the occurrence list far below the fold. The interesting frames are at the two
   ends; the middle is almost always framework noise.
2. **Occurrences repeat what the reader already knows.** Every row restates the
   error message the user just read at the top of the page. What is actually
   missing is *who / where / when* each occurrence happened.

Both are frontend-only. The API already returns everything needed.

## Scope

- `dashboard/src/lib/components/StacktraceView.svelte`
- `dashboard/src/pages/IssueDetail.svelte`
- `dashboard/src/lib/models/index.ts` (type completion only)

No backend, migration, or API change.

## Design 1 — Stacktrace frame collapsing

`StacktraceView` renders `ordered`, the frame array reversed so the most recent
call is first (Sentry convention). "Top 5" therefore means the 5 most recent
frames and "last 5" the 5 deepest.

### Visibility rule

Index `i` is **pinned** (always rendered) when any holds:

- `i < 5` — the top of the trace
- `i >= len - 5` — the bottom of the trace
- `frames[i].in_app === true` — the user's own code, wherever it sits

Unpinned indices fall into contiguous **runs**. A run collapses only when it
holds **3 or more frames**. A run of 1–2 frames renders normally: the toggle row
replacing it would occupy as much vertical space as the frames themselves.

That single rule doubles as the global threshold. A 12-frame trace leaves at most
2 unpinned frames and never collapses; 13 frames is the first length that can.
There is deliberately no separate threshold constant to keep in sync.

### Expansion state

Expansion is tracked **per run**, keyed by the run's start index, not by a single
page-wide boolean. In-app frames interleaved with vendor frames produce several
independent runs, and expanding one must not expand the others.

State resets when the active frame array changes — notably when the user toggles
`showRaw` between symbolicated and minified frames, where run boundaries differ.

### Affordance

A collapsed run renders one full-width button row: `⋯ Show 14 more frames`. When
expanded, the same row renders as `⋯ Hide 14 frames` immediately above the
revealed frames. Expansion is symmetric and reversible in place; there is no
separate global collapse control.

The row is a `<button>` carrying explicit styling. The global CSS reset only sets
font and cursor, so an unstyled `<button>` renders as a default gray box.

## Design 2 — Occurrence identity columns

### Type completion

`ErrorEvent` gains `session_id: string | null` and `device_key: string | null`.
The Rust `ErrorEvent` struct already carries and serializes both fields; the
TypeScript interface simply never listed them. This is a type fix, not a
contract change.

### Table

The `<ul class="occ-list">` is replaced by the house `<DataTable>` with four
columns, mirroring the link idiom already used in `Events.svelte`:

| Column  | Value | Link |
|---------|-------|------|
| Time    | `relativeTime(occurred_at)`, `title` = absolute timestamp | — |
| User    | `event_user.email ?? event_user.username ?? distinct_id`, else `anonymous` | `#/persons/{distinct_id}` when set |
| Session | `session_id`, mono, else `—` | `#/sessions/{session_id}` when set |
| Device  | derived label (below), else `—` | `#/devices/{device_key}` when set |

The device label is derived client-side from the event's machine `context`, in
order of preference:

1. `[device.family, device.model]` joined by a space
2. `[os.name, os.version]` joined by a space
3. `runtime.name` or `ua.name`

This matches how the pipeline builds `device_key` in `enrich.rs`, so the label
always describes the device the link points at.

Rows are **not** marked `clickable`. Each cell owns its own link, which avoids
nested-click ambiguity between a row handler and three anchors.

The `LevelBadge`, the repeated message span, and the tags strip are removed, along
with the `.occ*` styles that only they used. `LevelBadge` stays imported — the
Overview rail still uses it. `FilterBar`, the loading spinner, and the empty state
are untouched.

## Verification

A throwaway harness (`harness.html` + `src/harness-main.ts` + a `Harness*.svelte`)
mounts both components with mock data and is deleted afterwards. Cases:

- a long trace with in-app frames interleaved mid-stack, producing multiple runs
- a trace short enough that nothing collapses
- a run of exactly 2 unpinned frames (must not collapse)
- occurrences with null `session_id` / `device_key` / `distinct_id`

Assertions read computed styles and DOM via `preview_eval`; screenshots time out
on this dashboard because `body` uses `background-attachment: fixed`. Viewport
must be set with explicit numeric width/height — `window.innerWidth` reports 0
otherwise and every mobile media query matches.
