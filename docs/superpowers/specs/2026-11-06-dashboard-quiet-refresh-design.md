# Dashboard "Quiet" refresh — design

**Date:** 2026-11-06
**Area:** `dashboard/` (Svelte 5 runes, no Tailwind)
**Mockups:** `2026-11-06-dashboard-quiet-refresh/visual-direction.html` (option A is the chosen
look) and `2026-11-06-dashboard-quiet-refresh/quiet-kit.html` (the shared parts). Both are
fragments written for a mockup server; open them in a browser and the drawn parts render
with their own styles even though the page frame around them is unstyled.

## Why

The owner's verdict on the dashboard: it does not look professional. A sweep of all 45 pages
found the cause. No single page is broken. Pages built at different times each made their own
choices, so the product reads as assembled rather than designed. The house components
(`Button`, `Card`, `DataTable`…) are widely used. The drift sits in the parts that were never
shared: page headers, error and empty states, form controls, and an unbounded type scale.

The owner wants two things at once:

1. **One set of rules.** Every page does the same thing the same way.
2. **A new look, "Quiet".** Calm and dense, in the spirit of Linear. Option A in the mockup,
   chosen over a high-contrast black-and-white direction and a tidied version of today's look.

## Goals

- Every page follows the same rules for headers, states, buttons, controls, badges, type,
  numbers, dates and wording. Each rule is enforced by a shared component or token, not by
  convention.
- The whole app adopts the Quiet look, in both dark (the default) and light themes.
- The bugs the sweep found are fixed (see "Bugs").
- Drift cannot silently come back: cheap automated guards fail when a page reintroduces the
  main kinds of drift (see "Guards").

## Non-goals

- No new features, pages, data or backend work. Every page shows what it shows today.
- No behavior changes beyond the listed bug fixes. Time-window presets, which page has two
  time controls, row-click behavior and sorting all stay as they are; only their look changes.
- No route or URL changes. `/issues` and `/persons` keep their paths even where the visible
  wording changes.

## Anti-goals (failure even if it "works")

- A reskin where pages still hand-roll their own header, error banner, select or toggle
  underneath. The rules must live in shared parts.
- Breaking Arabic or right-to-left layout. All existing RTL rules in `app.css` (LTR islands
  for code and IDs, mirrored directional icons, no letter-spacing on Arabic) must keep working.
- Lower legibility. Quiet is restrained, not low-contrast (see "Contrast floors").
- Visual changes that only pass static checks. This repo has a history of styling that passed
  `svelte-check` and vitest while rendering wrong. Every batch is checked in a real browser.

## Constraints

- Svelte 5 runes, **no Tailwind**. Tokens live in `dashboard/src/app.css`; components in
  `src/lib/components/ui/` and `src/lib/components/`.
- All user-visible text goes through `t()` with **both** `en` and `ar` entries
  (`src/lib/i18n/catalog/*.ts`).
- One `AppShell` is mounted in `App.svelte`. Pages never wrap themselves in it
  (`models/shell.ts`, `SHELL_FLAGS`).
- The dashboard ships offline (Docker/nginx, RPM, .deb), so **no runtime font or asset
  CDNs**. Fonts are bundled.
- Gates: `npm --prefix dashboard run check` (0 errors) and `npm --prefix dashboard test` must
  pass.
- Workflow: the owner commits. Leave changes uncommitted, create no branches, never
  `git stash`. Other sessions may be editing this checkout at the same time.

## The Quiet look

The values below are the dark-theme starting point taken from mockup A. The builder may tune
the exact hexes, within the contrast floors.

| Token | Today | Quiet (dark) | Notes |
|---|---|---|---|
| `--bg` | `#0a0c10` + radial glow | `#0b0c0e`, **no gradient** | `--bg-gradient` removed |
| `--surface` | `#12151b` | `#111215` | inputs, menus, raised bits |
| `--surface-2` | `#171b22` | `#17191d` | hover, active nav |
| `--surface-3` | `#1d222b` | `#1f2126` | selected segment, avatars |
| `--border` | `#262c37` | `#1d1f23` | hairlines |
| `--border-strong` | `#343c49` | `#26292e` | control borders |
| `--text` | `#e7eaef` | `#e4e5e8` | |
| `--text-muted` | `#98a2b1` | `#8a8f98` | |
| `--text-faint` | `#626c7a` | `#5b606a` → tune to meet floor | |
| `--primary` | `#6e7bf2` | `#7c83f7` | softer; used sparingly |
| `--fatal/--error/--warning/--info/--success` | as today | `#e86a9a / #ef6b73 / #e2a85a / #5cb0e6 / #4cb782` | |
| Radii | 6 / 10 / 14 | **4 / 6 / 8** + pill | `--radius-sm/--radius/--radius-lg` |
| `--control-h` | 36px | **32px** | one height for every control |
| `--topbar-h` | 60px | **48px** | |
| Shadows | on cards | **only on overlays** | menus, dialogs, toasts, tooltips |

**Light theme.** Built by the same rules: neutral greys with no blue cast and no glow, white
surfaces, hairline borders, and the same accent hue darkened to meet the floors. Exact values
are left to the builder.

**Contrast floors.** `--text` and `--text-muted` must reach at least 4.5:1 on `--bg` and on
`--surface`, in both themes. `--text-faint` must reach at least 3:1 and is only for
non-essential text: placeholders, separators, axis ticks, the "Updated" stamp. Severity colors
used as text must reach 4.5:1.

**Accent discipline.** In the UI, `--primary` appears only on: the one primary button per
view, the active nav item indicator or count, focus rings, the selected state of controls, and
links inside prose. It is never used for stat values, headings or decoration.

**Charts are data, not UI.** They get their own token layer in `app.css`:

- `--chart-1` … `--chart-5`: a categorical palette. `--chart-1` is the accent hue; the others
  are quiet, distinguishable hues tuned for both themes.
- `--chart-neutral`: grey bars.
- `--chart-grid`: axis and grid lines.

Every chart component reads these tokens instead of `--primary` or literals. The rules:

- **Single-series bar time series** (occurrences, sessions per day…) draw neutral bars, with
  the latest bucket in the series color, as in mockup A. The series color is `--chart-1` by
  default and `--error` for exception charts, so they keep their meaning.
- **Line charts** use their series color.
- **Multi-series charts** (Lifecycle, ActiveUsers' active/new split, IssueDetail's segments,
  throughput against latency, Sankey) take the categorical palette in order. Sankey drops its
  computed `hsl()` hues for the palette.
- **The retention heatmap** keeps a single-hue ramp built from `--chart-1`. A neutral ramp
  would hurt legibility.

**Type scale.** Six sizes and three weights replace today's 33 sizes and 14 weights. Use px
only, with no rem.

| Token | Size | Use |
|---|---|---|
| `--text-2xs` | 11.5px mono | IDs, stack frames, culprits |
| `--text-xs` | 12px | labels, captions, table headers, meta |
| `--text-sm` | 13px | **body default**, table cells, buttons, inputs |
| `--text-md` | 15px | section titles, dialog titles |
| `--text-lg` | 19px | page titles |
| `--text-xl` | 22px | stat values |

Weights are 400 (body), 500 (emphasis, buttons, stat values) and 600 (titles). The body
default drops from 14px to 13px.

**Fonts.** `--font-sans` names Inter today, but **no Inter font file is loaded anywhere**, so
users see whatever system font they happen to have. Bundle Inter (variable woff2, latin
subsets) and **Noto Sans Arabic** (woff2 with an Arabic `unicode-range`, so it downloads only
when Arabic is rendered) through `@font-face` in `app.css`, served from the build. The mono
stack stays system fonts.

**Surfaces: hairlines, not boxes.** This is the core of the look.

- A **section** is a title row followed by content, separated from what is above by a top
  hairline or by spacing. It has no fill, no border box and no shadow.
- `Card` becomes this section by default. A boxed variant (bordered `--surface` panel) is kept
  only for:
  - content that needs a frame: code, stack traces, JSON trees and chart tooltips;
  - floating things: menus, dialogs, popovers and toasts;
  - side rails on detail pages, if they read better boxed (the builder decides per layout,
    then applies it the same way on all detail pages).
- **Tables** (`DataTable`) have no outer border or fill. Header cells are sentence case,
  12px and muted; today they are uppercase. Rows are separated by hairlines, have a subtle
  hover fill and use 13px cells. Numbers stay right-aligned and tabular.
- **Stat tiles** (`StatTiles`/`StatTile`) become one strip of cells divided by vertical
  hairlines, as in mockup A. A value is colored only when it is a severity (fatal, error,
  warning); today some pages color the first tile with the primary color.
- **Sidebar** shares the page background with a right hairline. Group labels are sentence
  case, 11.5–12px and faint. The active item gets a `--surface-2` fill and full-contrast text.
- **Topbar** is 48px with the breadcrumb switcher on the left. The `backdrop-filter` can go;
  it was only needed for the old translucent bar, and it is what forces the portal workaround
  for menus.

**Motion.** Transitions of about 120ms, on color and opacity only. Respect
`prefers-reduced-motion`.

## The shared parts (each built once, used everywhere)

The kit mockup shows all nine. Component names below are suggestions; the builder may name
them differently.

### 1–2. Page header (list and detail): new `PageHeader`

The page header replaces about 45 hand-rolled `.head`/`.detail-head`/`.identity` blocks and
the four ways `Freshness` and Refresh are placed today.

- **List pages:** the title, then a one-line subtitle. On the right, always in this order: the
  "Updated" stamp (`Freshness`), Refresh, secondary actions, and at most one primary action.
- **Detail pages:** add the following.
  - A back link above the title. It always names the list it returns to, using that list's
    sidebar label ("← Exceptions"), and goes to that list.
  - A title row that may hold one level or status badge.
  - A meta line. If the page has an ID, it shows in mono with a Copy button.
- All page titles use `--text-lg`/600. Header-to-content spacing is a single token.
- The seven copies of the `.back` button go away.
- Person profile keeps its avatar, but inside `PageHeader`'s title slot.

### 3–5. States

- **Couldn't load** (new `ErrorState`, or an `EmptyState` tone): an icon, the title
  "Couldn't load {thing}", a reason, and a **Retry** button whenever the page can retry.
  - The reason is a **human sentence** from one new helper, `describeLoadError(err)`, with
    `en` and `ar` copy:
    - network drop: "Couldn't reach the server. Check your connection and retry."
    - timeout or 504: "The server took too long to answer."
    - 5xx: "The server ran into a problem."
    - 429: "Too many requests. Wait a moment and retry."
    - 403: "You don't have access to this."
    - 404: "This no longer exists."
  - 4xx bodies whose backend message is meaningful (400 query errors,
    `release_rollup_pending`, and any coded error the page already special-cases) keep their
    existing handling and text.
  - The raw normalized message stays available as a muted "Details" line under the sentence,
    so support can still read it.
  It comes in two sizes, page-level and inside a section. It replaces the current five
  patterns, including four different `.err-banner` styles.
- **Nothing here** (`EmptyState`) has three wordings. Pick the first that applies:
  1. A filter or search is active: "No {things} match these filters", with a clear-filters
     action where the page has one.
  2. The list is time-windowed: "No {things} in this time range", with "Widen the time
     range…" as the description. The release-scoped variant keeps its own text.
  3. Only for lists that are not time-bound and show everything (monitors, environments,
     projects, members, source maps, roles): "No {things} yet", with a setup action where
     one exists.
  
  A page never says "yet" about time-windowed data: it cannot know that nothing ever
  existed. Reuse existing catalog strings where they already say the right thing.
  
  An action is optional. It comes in the same two sizes as ErrorState, which replaces the
  plain-muted-line versus padded-block split.
- **Loading:** `Skeleton` shaped like the content. This rule already holds; keep it.
- **Inline notice** (new `InlineNotice`, tones error, warning, info and success): one
  component for stale-data banners, success notes and policy messages. It replaces
  `.stale-banner` (3 copies), `.ok-banner`, `.notice` and `.policy-ok`.
- **Feedback rule:**
  - Load failures use ErrorState.
  - Action success uses a toast (`toastStore.success`).
  - Action failure uses a toast, or inline text inside the open form or dialog.

### 6. Controls (one height: `--control-h` = 32px)

- **New `Select`:** a native `<select>` styled exactly like `Input`, with the same label, hint
  and error API. It replaces the six page-local select styles and the raw unstyled selects.
- **New `SegmentedControl`:** replaces the six hand-rolled segmented toggles. `DateRange`'s
  pills render through it.
- **`Input`:** same API, restyled. Raw `<input>`s in Purge, Storage and Wall of shame move to
  it. Checkboxes and radios stay native with `accent-color: var(--primary)`.
- **Buttons:** four variants.
  - `primary`: at most one per view.
  - `secondary`: the default.
  - `ghost`: tertiary or icon-only.
  - `danger`: any irreversible action.
  
  `subtle` folds into `ghost` or `secondary`.
- **Dialog footers:** Cancel (`secondary`) on the left and the action on the right, labelled
  verb + object ("Delete source map", "Create environment").
- **Confirmations:**
  - Every irreversible action is confirmed with `ConfirmDialog danger`.
  - Bulk data destruction (Purge) keeps type-to-confirm, with a `danger` button.
  - The bespoke delete dialogs, inline confirm rows and hand-built modals move to
    `ConfirmDialog` where it fits.

### 7. Feedback colors and badges

- **Level** (`LevelBadge`): a colored dot plus the word, in body text color.
- **Status** (`StatusBadge`): a quiet neutral pill, colored only when the color means something
  (Resolved and Up are green, Down is red). `StatusPill`, `SymbolicationBadge` and the
  page-local pills (`.kindtag`, `.result`, `.type-tag`, `.meta-link`, `.chip`, `.trait`…)
  fold into `Badge`/`StatusBadge` and one `CountChip`.
- **Labels are human words, never raw API values.** `{f.status}`, `{tx.status}` and
  `"stored"` are mapped through `t()`.

### 8. Type

See the scale above. Every local `font-size` and `font-weight` in `.svelte` files moves to the
tokens. The global `.page-title` and `.section-label` classes are either restyled or retired
in favour of `PageHeader` and section titles.

### 9. Numbers and dates

- **Counts** in tables, detail meta and lists are full (`formatNumber` gives "1,234").
  `compactNumber` ("1.2K") is allowed only in stat tiles and chart labels. Screens and
  Workflows tables move to full counts.
- **Percentages** always use `formatPercent`; Monitors' `toFixed` goes.
- **Timestamps** in tables, timelines and meta lines use `TimeValue`. The default display is
  relative, with absolute available, as today. Absolute-only contexts use one helper. The
  three ad-hoc absolute formats (`formatDateTime`, `formatDateTimeSeconds` and inline
  ternaries) collapse to one, with seconds where the page needs them. The builder picks which.
- **Durations** always use `formatDuration` and right-align like numbers.

## Wording rules

- **Sentence case everywhere:** titles, buttons, nav and admin rail. "Source Maps" becomes
  "Source maps"; "Wall of Shame" becomes "Wall of shame".
- **The sidebar's word wins for the name of a page or list**: page titles, back links, and
  load-error and empty-state titles on that list page.
  - "← Exceptions", "Couldn't load exceptions".
  - "Users" as the Users page title, not "People".
  - "Monitors" as the page title and back link. The sidebar group heading stays "Uptime".
  
  Domain nouns for **the things inside** keep their meaning everywhere:
  - An **issue** is a group of exception events. The "Issue" column, "View issue", "Couldn't
    load issue" on the detail page, the notification options and the Docs prose stay as they
    are.
  - **People** means identified users plus guests where that split is meant.
  - Account's **Profile** (the signed-in member) and Members' "People with access" are not
    tracked users and are untouched.
  
  `docs-prose.ts` is out of scope for this rule.
- **Verbs:**
  - "New {thing}" on the button that opens a form; "Create {thing}" on the button that submits it.
  - "Retry", never "Try again".
- **Organization:** "this organization", never "org" or "organisation".
- **Empty-state copy:** titles have no trailing period; descriptions are full sentences.
- **Page title keys:** each page's title uses its own key, not one borrowed from another context
  (today Events uses `overview.stat.events` and Sessions uses `explore.column.sessions`).
- **Hard-coded English** found in any file you touch moves into the catalog with an Arabic
  entry. Known spots include Monitor detail, Screen detail, Session detail, Purge, Storage,
  Members, Roles, Environments, `admin-nav.ts`, the ResetPassword and ChangePassword hints, and
  the Refresh titles.

## Sign-in and standalone screens

Every page in this group uses `AuthLayout`: Login, Register, Forgot password, Reset password,
Change password, Onboarding, Confirm and Cancel email change, and Unsubscribe.

- One brand mark (`EyeMark`), one card width (Onboarding may use a wider variant of the same
  layout), one error banner (`InlineNotice` error with `role="alert"`), and one link style.
- ChangePassword and Onboarding drop their hand-rolled gradient mark.
- Unsubscribe gains the brand.
- The duplicated "Back to sign in" on the email-change pages goes.

## Bugs fixed along the way

1. `pages/SourceMaps.svelte`: Delete removes an artifact with no confirmation. Add
   `ConfirmDialog danger`.
2. `pages/Environments.svelte`: the Create and Rename dialog confirm buttons have no variant,
   so they render `secondary`, identical to Cancel. Make them `primary`.
3. `pages/Purge.svelte`: the irreversible "Purge N rows" button is `primary`. Make it `danger`.
4. `pages/PersonProfile.svelte`: the back link says "Back to events" and goes to `/events`.
   Make it "← Users" and send it to `/users`.
5. **Undefined tokens.** About a dozen `var(--x)` names are used but never defined, so they
   fall back silently, sometimes to nothing:
   - `--danger` → `--error`
   - `--muted-fg`, `--muted` → `--text-muted`
   - `--radius-md` → `--radius`
   - `--accent` → `--primary`
   - `--fg` → `--text`
   - `--card` → `--surface`
   - `--surface-alt` → `--surface-2`
   - `--on-primary`, `--primary-fg` → `--primary-contrast`
   - `--border-subtle` → `--border`
   - `--line` → `--primary`, or a chart token
   
   Retention, RetentionGrid and LifecycleChart are the worst affected.
6. **Hard-coded colors** that bypass the theme: `StatusPill` hex values, `Modal`'s backdrop
   (use `--overlay`), `RowActionsMenu`'s shadow (use `--shadow`), and the hex fallbacks
   (`#c0392b`, `#b91c1c`, `#e5484d`…).
7. **Card-wrapped tables.** Performance, Events, Transactions, Journeys and Inspector put a
   `DataTable` inside `<Card padding="none">`, which likely draws two borders. Once tables are
   borderless this goes away; check it renders cleanly.

## Guards (so drift can't creep back)

Each is a vitest test in the style of the existing parity tests (`shell.test.ts`). They read
`.svelte` sources through Vite `?raw` imports.

- Every `var(--name)` used in `src/**/*.svelte` is defined in `app.css` or set locally on the
  same element or component (a local custom property such as `--bar-color`).
- No `font-size` literal outside the token set, and no `rem` font sizes.
- No hex or `rgb()` color literals in `.svelte` files outside an explicit allowlist (for
  example the `EyeMark` artwork).
- No `<h1` in `src/pages/**` outside `PageHeader` usage.

The builder may add more and may pick the exact mechanism.

## Rollout order

Each step leaves the app working.

1. **Tokens, fonts and globals** (`app.css`): new values, no glow, bundled fonts, type tokens,
   radii, control height. Fix the undefined tokens in the same step so nothing falls back.
2. **Restyle existing shared components:** Button, Card, DataTable, SortableTh, Badge,
   LevelBadge, StatusBadge, StatTiles/StatTile, EmptyState, Skeleton, Modal, ConfirmDialog,
   Input, Toast, ListToolbar, DateRange, TimeFilter, SearchInput, Sidebar, Topbar, AdminShell,
   AuthLayout.
3. **Build the new shared parts:** PageHeader, ErrorState (or EmptyState tone), InlineNotice,
   Select, SegmentedControl, CountChip.
4. **Move pages over in batches:**
   - list and analytics pages;
   - detail pages;
   - settings and admin;
   - sign-in and standalone screens.
   
   Delete each page's now-unused local CSS as you go; svelte-check's unused-selector warnings
   serve as the checklist.
5. **Wording pass and bug fixes** (some can land with their batch).
6. **Guards.**

## How to verify

- `npm --prefix dashboard run check` gives 0 errors, `npm --prefix dashboard test` passes, and
  `npm --prefix dashboard run build` succeeds.
- **In a real browser, per batch:** every page in **dark, light, and Arabic (RTL)**, at desktop
  width and at about 375px. Existing render harnesses (`listui-harness`, `detail-harness`,
  `search-harness`, `i18n-harness`; launch entries in `.claude/launch.json`) render real pages
  against mocked data. Mocking at the axios adapter is the cheapest way to render any page
  with no backend.
- **Known Browser-pane traps:**
  - Set a numeric viewport size before measuring.
  - A hidden pane does not advance transitions, and synthetic clicks do nothing while it is
    hidden.
  - Verify computed styles numerically when screenshots are unavailable.
- Measure the contrast floors with computed colors in both themes.
- Compare against the mockups. The look should match option A and the kit; exact pixels need
  not.

## Left to the builder

- Exact light-theme values, and dark hex tuning within the floors.
- Component names and APIs, and whether `Card`'s section/boxed split is a prop or two
  components.
- Whether detail-page side rails are boxed, applied the same way on all detail pages.
- Icon sizes and stroke, and spacing tokens.
- Which single absolute timestamp format survives.
- Batch boundaries within the rollout steps, and the exact guard mechanics.

## Calls made in brainstorming (not explicitly confirmed by the owner)

The owner approved the direction (Quiet), the shared parts in the kit, the scope (every page
including sign-in), dark default plus light, "the sidebar's word wins", and no new features or
backend work. These narrower calls were made on their behalf:

- Body text drops to 13px and controls to 32px (denser than today). The topbar becomes 48px.
- Inter and Noto Sans Arabic are bundled; the mono font stays system.
- Sentence case extends to "Wall of shame".
- Page title "Monitors", while the sidebar group stays "Uptime".
- Full counts in tables; compact only in tiles and charts.
- Time-window presets and the two time controls on Sessions and Users stay as they are, with
  only their look changing.
- Checkboxes stay native, with `accent-color`.

The builder check raised four questions. The owner asked for the work to go ahead directly, so
they were settled as calls, and each is written into the sections above:

- **Chart colors:** a chart-token layer, with neutral bars and the latest bucket in the series
  color. Exception charts stay red, multi-series charts use the palette, and the heatmap keeps
  an accent ramp.
- **Empty states:** three wordings (filtered, time range, "yet"). "Yet" is only used for lists
  that are not time-bound.
- **"The sidebar's word wins":** it covers page and list names only. "Issue", "people" and
  "profile" keep their domain meanings.
- **Load-error reason:** a human sentence from `describeLoadError`, with the raw message kept
  as a muted "Details" line.

One repo note for the builder: `lib/models/refresh-parity.test.ts` checks each page's source
for `<RefreshButton`. Moving Refresh into `PageHeader` means updating that test to accept
`PageHeader`'s refresh prop instead.
