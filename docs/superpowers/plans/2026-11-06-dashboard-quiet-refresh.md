# Dashboard "Quiet" refresh — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every dashboard page one set of shared parts and rules, and the "Quiet" look (option A).

**Architecture:** Three layers.
1. Tokens in `app.css` change the whole app at once.
2. Shared components are restyled, and the missing ones are added (PageHeader, ErrorState,
   InlineNotice, Select, SegmentedControl, CountChip).
3. Pages move onto them in four batches, deleting their hand-rolled CSS as they go.

Logic that is worth testing lives in `lib/models/*.ts` and `lib/theme/*.ts` with co-located
vitest tests. Components carry no logic, which is the house rule. Visuals are checked in the
browser.

**Tech Stack:** Svelte 5 runes, Vite, vitest, no Tailwind. Fonts come from
`@fontsource-variable/inter` and `@fontsource-variable/noto-sans-arabic`.

**Spec:** `docs/superpowers/specs/2026-11-06-dashboard-quiet-refresh-design.md`. Mockups are
beside it in `2026-11-06-dashboard-quiet-refresh/`.

## Global Constraints

- **No commits, no branches, never `git stash`.** The owner commits, and another session may
  be editing this tree. Leave everything uncommitted.
- Every user-visible string goes through `t()` with `en` **and** `ar` entries, in
  `src/lib/i18n/catalog/*.ts`.
- Use px only: no `rem` font sizes.
- Font sizes come only from `--text-2xs` 11.5 (mono), `--text-xs` 12, `--text-sm` 13,
  `--text-md` 15, `--text-lg` 19 and `--text-xl` 22.
- Font weights are only 400, 500 and 600.
- Radii are `--radius-sm` 4, `--radius` 6, `--radius-lg` 8 and `--radius-pill`. Every
  control is `--control-h` 32px tall.
- `--primary` is used in UI chrome only for:
  - the single primary button,
  - the active nav indicator or count,
  - focus rings,
  - the selected control state,
  - links in prose.
  
  Charts use `--chart-*`.
- Shadows appear only on overlays: menus, dialogs, toasts and tooltips.
- Contrast floors in both themes:
  - `--text` and `--text-muted` reach ≥4.5:1 on `--bg` and on `--surface`.
  - `--text-faint` reaches ≥3:1.
  - Severity colors reach ≥4.5:1 on `--bg`.
- Keep every RTL rule in `app.css` working: LTR islands, mirrored icons, no letter-spacing
  on Arabic.
- No behavior, route, data or backend change beyond the spec's numbered bug fixes.
- Gates (run from the repo root):
  - `npm --prefix dashboard run check` gives 0 errors. The warning baseline is 12; don't add
    to it.
  - `npm --prefix dashboard test` passes.
  - `npm --prefix dashboard run build` succeeds.

## Review Focus

1. **Arabic / RTL:**
   - The back arrow in PageHeader mirrors.
   - The action order reverses naturally.
   - IDs in the meta line stay LTR islands.
   - Arabic glyphs render in Noto Sans Arabic, not a system font.
   
   Pinned in Task 6 step 6 and Task 12.
2. **Light theme with fewer boxes:** sections without a fill on a white page still read as
   separate. Hairlines must be visible, at least `#e6e7ea` on `#f7f7f8`. Pinned in Task 1's
   contrast test and Task 12.
3. **About 375px wide:**
   - PageHeader actions wrap under the title.
   - The StatTiles hairline grid wraps without a filled gap.
   - Tables scroll horizontally inside their own box.
   
   Pinned in Task 4 step 4 and Task 12.
4. **Locked controls** (`lockedReason`): still focusable and still showing the lock glyph,
   and the `.is-locked` opacity remains legible on the new surfaces. Pinned in Task 3 step 5.
5. **Long titles** in PageHeader, such as 200-character issue titles and mono screen names:
   they wrap to at most 2 lines and are clamped, without pushing the actions off-screen.
   Pinned in Task 6 step 6.

---

### Task 1: Tokens, fonts and globals

**Files:**
- Modify: `dashboard/src/app.css`, `dashboard/src/main.ts`, `dashboard/package.json`
- Create: `dashboard/src/lib/theme/tokens.ts`, `dashboard/src/lib/theme/tokens.test.ts`

**Interfaces:**
- Produces:
  - **Tokens** used by every later task:
    - type: `--text-2xs/xs/sm/md/lg/xl`
    - weights: `--weight-regular: 400`, `--weight-medium: 500`, `--weight-strong: 600`
    - charts: `--chart-1..5`, `--chart-neutral`, `--chart-grid`
    - spacing: `--page-gap: 22px` (between sections), `--head-gap: 20px` (header to content)
    - `--radius-*`, `--control-h: 32px`, `--topbar-h: 48px`
  - **Functions** in `lib/theme/tokens.ts`:
    - `parseThemes(css: string): { dark: Record<string,string>; light: Record<string,string> }`
      returns the custom properties declared in `:root` and in `[data-theme='light']`, with
      light inheriting anything it does not redeclare.
    - `contrast(a: string, b: string): number` takes `#rrggbb` hexes and returns the WCAG
      ratio.

- [ ] **Step 1: Write the failing test** `tokens.test.ts`. It imports `../../app.css?raw`. For
  each theme of `parseThemes(css)`:
  - `contrast(text, bg) >= 4.5`, `contrast(text, surface) >= 4.5`
  - `contrast(text-muted, bg) >= 4.5`, `contrast(text-muted, surface) >= 4.5`
  - `contrast(text-faint, bg) >= 3`, `contrast(text-faint, surface) >= 3`
  - for each of `fatal`, `error`, `warning`, `info` and `success`: `contrast(c, bg) >= 4.5`
  - `bg-gradient` is undefined
  - `control-h === '32px'`, `radius === '6px'`

- [ ] **Step 2:** Run `npm --prefix dashboard test -- tokens`. Expect a FAIL, because
  `tokens.ts` is missing.

- [ ] **Step 3: Implement `parseThemes` and `contrast`** using the standard WCAG relative
  luminance. Then **rewrite the token blocks in `app.css`:**

  **Dark (`:root`):**

  | Token | Value |
  |---|---|
  | `--bg` | `#0b0c0e` |
  | `--surface` | `#111215` |
  | `--surface-2` | `#17191d` |
  | `--surface-3` | `#1f2126` |
  | `--overlay` | `rgba(4,5,6,.6)` |
  | `--border` | `#1d1f23` |
  | `--border-strong` | `#26292e` |
  | `--text` | `#e4e5e8` |
  | `--text-muted` | `#8a8f98` |
  | `--text-faint` | `#646972` |
  | `--primary` | `#7c83f7` |
  | `--primary-hover` | `#8d93f8` |
  | `--primary-active` | `#6c73ec` |
  | `--primary-soft` | `rgba(124,131,247,.14)` |
  | `--primary-border` | `rgba(124,131,247,.4)` |
  | `--fatal` / `--error` / `--warning` / `--info` / `--success` | `#e86a9a` / `#ef6b73` / `#e2a85a` / `#5cb0e6` / `#4cb782` |
  | `*-soft` | each color at .12 alpha |
  | `--chart-1..5` | `#7c83f7 #4fb6c9 #e2a85a #c27ad6 #6fbf7e` |
  | `--chart-neutral` | `#2a2d34` |
  | `--chart-grid` | `#1d1f23` |
  | `--shadow-sm` | `none` |
  | `--shadow` | `0 8px 24px rgba(0,0,0,.45)` |
  | `--shadow-lg` | `0 18px 50px rgba(0,0,0,.55)` |

  **Light (`[data-theme='light']`):**

  | Token | Value |
  |---|---|
  | `--bg` | `#f7f7f8` |
  | `--surface` | `#ffffff` |
  | `--surface-2` | `#f1f2f4` |
  | `--surface-3` | `#e8eaed` |
  | `--border` | `#e6e7ea` |
  | `--border-strong` | `#d5d8dd` |
  | `--text` | `#17181b` |
  | `--text-muted` | `#5e636d` |
  | `--text-faint` | `#80858f` |
  | `--primary` | `#5b62e8` |
  | `--primary-hover` | `#4c53d9` |
  | `--primary-active` | `#4249c9` |
  | `--fatal` / `--error` / `--warning` / `--info` / `--success` | `#c4306c` / `#d0323f` / `#a86400` / `#1f74b8` / `#1a7f48` |
  | `--chart-1..5` | `#5b62e8 #1f8fa3 #c4860f #9a4fb3 #3f9a52` |
  | `--chart-neutral` | `#d9dce1` |
  | `--chart-grid` | `#eceef1` |
  | `--shadow` / `--shadow-lg` | the same shapes at 0.08 / 0.14 alpha |

  **Other changes to `app.css`:**
  - Delete `--bg-gradient`, plus `background-image` and `background-attachment` on `body`.
  - Set body to `font-size: var(--text-sm)` and `line-height: 1.5`.
  - Set `h1–h4` to weight 600.
  - Set `.page-title` to `--text-lg`/600.
  - Set `.section-label` to `--text-xs`/500, sentence case, `--text-faint`, with no
    uppercase and no tracking.
  - Keep `--radius-pill`, `--sidebar-w` and `--content-max`.

  **Fonts:**
  - `npm --prefix dashboard install @fontsource-variable/inter @fontsource-variable/noto-sans-arabic`
  - In `main.ts`, import `'@fontsource-variable/inter'` and
    `'@fontsource-variable/noto-sans-arabic'` before `./app.css`.
  - Set `--font-sans: 'Inter Variable', 'Noto Sans Arabic Variable', ui-sans-serif, system-ui, sans-serif`.
  - Set the `[lang='ar']` stack to start `'Inter Variable', 'Noto Sans Arabic Variable'`.

  **Motion:** add a global `@media (prefers-reduced-motion: reduce)` rule that sets
  `transition-duration` and `animation-duration` to `.01ms` on everything except the spinners,
  which stay slowed (as today). New transitions use 120ms, on color and opacity only.

- [ ] **Step 4:** Run `npm --prefix dashboard test -- tokens`. Expect a PASS. If a floor
  fails, tune that one hex until it passes, and note the change in the summary.

- [ ] **Step 5:** Run `npm --prefix dashboard run build`. Expect success, with `.woff2` files
  in `dist/assets`. Leave the work uncommitted.

### Task 2: Every token used is defined, and the hard-coded colors are gone

**Files:**
- Create: `dashboard/src/lib/theme/css-guards.test.ts`
- Modify every file that uses an undefined token or a hard-coded color. The sweep found:
  - `LifecycleChart`, `RetentionGrid`, `HourlyLineChart`, `TransactionDetailPanel`,
    `OperationTransactionsModal`, `DateRangePicker`, `TimeFilter`, `SortableTh`, `Timeline`,
    `StacktraceView`, `CollapsibleFetchCard`, `SectionRow`, `members/ChangeEmailDialog`;
  - pages `Retention`, `Storage`, `IngestFailures`, `Purge`, `SourceMaps`, `WallOfShame`,
    `Account`, `Transactions`, `ConfirmEmailChange`, `CancelEmailChange`;
  - `ui/StatusPill`, `ui/Modal`, `ui/RowActionsMenu`, `ui/Skeleton`, `StatTile`.

**Interfaces:**
- Produces: `css-guards.test.ts`, which later tasks extend (Task 13 adds three more cases).
  It also provides the helper `svelteSources(): Record<string,string>`, built with
  `import.meta.glob('../../**/*.svelte', { query: '?raw', import: 'default', eager: true })`,
  and `cssDefinedTokens(): Set<string>`, the union of both themes from Task 1's `parseThemes`.

- [ ] **Step 1: Write the failing test** `every var(--x) used in a component is defined`.
  1. For each source, collect `var(--name` occurrences.
  2. Allow a name if it is in `cssDefinedTokens()`, or if the same source declares `--name:`
     (a local custom property such as `--bar-color`, `--tile-min`, `--c` or `--pg-digits`), or
     sets it with `style="--name` or `style:--name`.
  3. Expect the leftovers to equal `[]`.

  A second test, `no hex or rgb() color literals in components`: scan each `<style>` block and
  the `style=` attributes for `#[0-9a-f]{3,8}\b` and `rgba?\(`. Allowlist `EyeMark.svelte`
  and `Icon.svelte`. Expect `[]`.

- [ ] **Step 2:** Run `npm --prefix dashboard test -- css-guards`. Expect a FAIL that lists
  about 20 files.

- [ ] **Step 3: Replace the undefined tokens**:

  | Undefined | Replace with |
  |---|---|
  | `--danger` | `--error` |
  | `--muted-fg`, `--muted` | `--text-muted` |
  | `--radius-md` | `--radius` |
  | `--accent` | `--primary` |
  | `--fg` | `--text` |
  | `--card` | `--surface` |
  | `--surface-alt` | `--surface-2` |
  | `--on-primary`, `--primary-fg` | `--primary-contrast` |
  | `--border-subtle` | `--border` |
  | `--line` | `--chart-1` |

  Drop every hex fallback inside `var(…, #hex)`. Then fix the hard-coded colors:
  - `Modal` backdrop → `var(--overlay)`
  - `RowActionsMenu` shadow → `var(--shadow)`
  - `StatusPill` hexes → `--success`/`--error`
  - `Skeleton`/`StatTile` shimmer fallbacks → removed
  - `Storage` and `Onboarding` hexes → tokens
  
  A color that genuinely has no token goes into `app.css` as a new token.

- [ ] **Step 4:** Run `npm --prefix dashboard test -- css-guards` (expect PASS) and
  `npm --prefix dashboard run check` (expect 0 errors).

### Task 3: Controls and badges

**Files:**
- Modify in `dashboard/src/lib/components/ui/`: `Button`, `Input`, `Badge`, `Skeleton`,
  `Modal`, `ConfirmDialog`, `Toast`, `CopyButton`, `Freshness`, `RefreshButton`,
  `RowActionsMenu`, `StatusPill`.
- Modify in `dashboard/src/lib/components/`: `LevelBadge`, `StatusBadge`.
- Create in `ui/`: `Select.svelte`, `SegmentedControl.svelte`, `CountChip.svelte`.

**Interfaces:**
- Produces:
  - `Select`, props:
    `{ value: string (bindable); options: { value: string; label: string; disabled?: boolean }[]; label?: string; hint?: string; error?: string; id?: string; disabled?: boolean; size?: 'sm'|'md'; onchange?: (v: string) => void }`.
    Renders a native `<select>` with `Input`'s exact shell.
  - `SegmentedControl`, props:
    `{ value: string (bindable); options: { value: string; label: string; title?: string }[]; ariaLabel: string; size?: 'sm'|'md'; onchange?: (v: string) => void }`.
    Renders `role="radiogroup"` with arrow-key movement.
  - `CountChip`, props: `{ n: number | string; tone?: 'neutral'|'primary' }`.
  - `Button`: variant `subtle` is removed from the `Variant` type. Change the two call sites
    to `ghost`.
  - `LevelBadge` renders a dot plus the word with no pill. `StatusBadge` renders a quiet
    neutral pill, colored only for `resolved`.
  - `StatusPill` renders through the same pill styles: up is green, down is red, the rest are
    neutral.

- [ ] **Step 1: Restyle `Button`.**
  - Base: `--radius`, weight 500, `--text-sm`.
  - Heights: `md` at `min-height: var(--control-h)` with 0 12px padding; `sm` at 28px with
    0 10px padding and `--text-xs`; `lg` at 40px.
  - Variants:
    - `primary`: `--primary` fill.
    - `secondary`: `--surface` fill, `--border-strong` border, `--text`.
    - `ghost`: transparent, `--text-muted`, with a `--surface-2` hover.
    - `danger`: solid `--error` fill, white text, and a darker hover via
      `color-mix(in srgb, var(--error) 85%, black)`.
  - No shadows anywhere.

- [ ] **Step 2:** Restyle `Input`: a `--control-h` shell, `--radius`, `--surface` fill,
  `--border-strong` border, a focus ring in `--primary`, label at `--text-xs`/500 in
  `--text-muted`, and hint and error at `--text-xs`. Then build `Select` on the identical
  shell, with a chevron drawn by `Icon chevron-down` positioned at `inset-inline-end`. Build
  `SegmentedControl` to match the kit: a 3px inset track on `--surface` with a
  `--border-strong` border, inner `--radius-sm`, and the active segment on `--surface-3` in
  `--text`.

- [ ] **Step 3:** Restyle the badges.
  - `Badge` is a 20px tall rectangle with `--radius-sm`, `--text-xs`/500, a soft tone fill
    at 12% and no border. `sm` is 18px.
  - `LevelBadge` drops `Badge`: it renders `<span class="lvl">`, a 7px dot in the tone color
    and the label in `--text`.
  - `CountChip` is a pill with `--text-xs`, `--surface-2`, `--text-muted`, tabular numerals,
    and 0 6px padding.

- [ ] **Step 4:** Restyle the overlays and helpers.
  - `Modal` and `ConfirmDialog` sit on `--surface`, with a `--border-strong` border,
    `--radius-lg` and `--shadow-lg`. The title is `--text-md`/600. The footer is
    `justify-content: flex-end`, gap 8, with Cancel `secondary` first.
  - `ConfirmDialog danger` uses Button `danger`.
  - `Toast` and `RowActionsMenu` use `--shadow`.
  - `RefreshButton` and the Topbar icon buttons are `--control-h` squares in the `secondary`
    look.
  - `Freshness` text is `--text-xs`/`--text-faint`.
  - `Skeleton` uses a `--surface-2`↔`--surface-3` shimmer.

- [ ] **Step 5: Verify in the browser.** Add a temporary `?kit` route to
  `search-harness/Harness.svelte` that renders every Button variant and size,
  `lockedReason` set and unset, Input, Select, SegmentedControl, Badge tones, LevelBadge,
  StatusBadge and CountChip, in both themes. Start the `search-harness` launch entry with
  numeric `resize_window` 1280×900, then measure with `javascript_tool`:
  - every control's `getBoundingClientRect().height === 32`
  - a locked button `matches(':focus-visible')` after `.focus()` and still shows its lock glyph
  
  Take a screenshot in each theme. Remove the temporary route afterwards.

- [ ] **Step 6:** Run `npm --prefix dashboard run check`; expect 0 errors.

### Task 4: Surfaces — Card, DataTable and StatTiles

**Files:**
- Modify: `ui/Card.svelte`, `DataTable.svelte`, `SortableTh.svelte`, `StatTiles.svelte`,
  `StatTile.svelte`, `KeyValueList.svelte`, `ViewSection.svelte`,
  `CollapsibleFetchCard.svelte`, `SectionRow.svelte`, `ui/CodeBlock.svelte`,
  `StacktraceView.svelte`, `JsonTree.svelte`.

**Interfaces:**
- Produces:
  - `Card` gains `variant?: 'section' | 'boxed'`, defaulting to `'section'`. All other props
    are unchanged.
    - **`section`:** no fill, border or shadow. The head is a title row of `--text-md`/600
      with `actions` on the right. The body sits below it with no inset (`padding` applies
      only to `boxed`). Sections are separated by `border-top: 1px solid var(--border)` and
      `padding-top: 16px`, except when the section is the first child of its container.
    - **`boxed`:** `--surface` fill, `--border` border and `--radius-lg`, with the existing
      padding scale.
  - `KeyValueList` becomes the single key/value layout:
    - dt in `--text-xs`/`--text-muted`
    - dd in `--text-sm`
    - a `minmax(96px, max-content) minmax(0, 1fr)` grid with row gap 8
    - a `mono` prop per item for IDs

- [ ] **Step 1: Restyle `DataTable`.**
  - `.dt-wrap`: no border, background or radius. Keep `overflow-x: auto`.
  - Header cells: `--text-xs`/500, `--text-faint`, sentence case (no `text-transform`, no
    tracking), `background: var(--bg)`, sticky, and a bottom hairline.
  - Body cells: `--text-sm`, padding `11px 12px`, and a `--border` hairline.
  - Hover: `--surface`.
  - `SortableTh`: the active sort arrow in `--text`, the inactive one in `--text-faint`.
  - The first and last columns get `padding-inline: 2px` so the table aligns with the page
    text edge, as in mockup A.

- [ ] **Step 2: Restyle `StatTiles` and `StatTile`.**
  - The grid gets `gap: 1px`.
  - Each tile: transparent background, no border or radius, padding `4px 20px`,
    `box-shadow: -1px 0 0 var(--border)`. The container has `overflow: hidden` so the first
    column's line is clipped.
  - Wrapped rows: add `0 -1px 0 var(--border)` to the shadow so they are divided too. The
    first row's top line is clipped as well.
  - Label: `--text-xs`, `--text-muted`, sentence case.
  - Value: `--text-xl`/500, tabular.
  - The `primary` tone is removed from the `Tone` type. It renders neutral; drop it at call
    sites.
  - `interactive` hover: `--surface`.

- [ ] **Step 3:**
  - `CodeBlock`, `StacktraceView` and `JsonTree` stay boxed: `--surface`, `--border`,
    `--radius-lg`.
  - `ViewSection`, `CollapsibleFetchCard` and `SectionRow` adopt the section look. Change
    `CollapsibleFetchCard` to compose `Card collapsible` instead of its own toggle, which
    unifies the chevron.

- [ ] **Step 4: Verify.** Start `listui-harness` (`?page=sessions`, then `?page=issues`) at
  1280×900 and at 375×812. Measure:
  - `.dt-wrap` has a computed `border-top-width` of `0px`.
  - At 375px there is no StatTiles cell with a background other than transparent, and the
    `.dt-wrap` `scrollWidth > clientWidth` (it scrolls).
  
  Take a screenshot at each width.

### Task 5: Shell — Sidebar, Topbar, AppShell, AdminShell and AuthLayout

**Files:**
- Modify in `layout/`: `Sidebar.svelte`, `Topbar.svelte`, `AppShell.svelte`,
  `AdminShell.svelte`, `AuthLayout.svelte`, `SwitcherMenu.svelte`.
- Modify: `BreadcrumbTrail.svelte` and `lib/models/admin-nav.ts`. The admin rail labels move
  to `t()` keys, sentence case.

**Interfaces:**
- Produces: `AuthLayout` gains `width?: 'sm' | 'md'`, at 400px and 560px. `md` is used by
  Onboarding.

- [ ] **Step 1: Sidebar.**
  - Background `--bg`, with a `--border` hairline on the end side.
  - Brand row: 14px/600.
  - Group labels: `--text-xs`, `--text-faint`, sentence case, no tracking.
  - Items: `--text-sm`/400, `--text-muted`, 30px tall, `--radius`.
  - Active item: `--surface-2` fill, `--text`, weight 500.
  - Counts use `CountChip`.

- [ ] **Step 2: Topbar.**
  - `--topbar-h` (48px), `--bg` background, a bottom hairline.
  - Remove `backdrop-filter`. Keep `position: sticky`.
  - The icon buttons match `RefreshButton`.
  - Breadcrumb: segments in `--text-muted`, current segment in `--text`/500, `/` separators
    in `--text-faint`.

- [ ] **Step 3: Layout shells.**
  - `AppShell .content-inner` padding: `24px 28px 64px`.
  - `AdminShell` rail: same item style as the Sidebar.
  - `AuthLayout` card: `--surface`, `--border`, `--radius-lg`, 32px padding.
    - h1 at `--text-lg`/600.
    - Footer links in `--primary` with an underline on hover.
    - `EyeMark` at 36px.

- [ ] **Step 4:** In `admin-nav.ts`, the labels become `t('admin.nav.…')` keys: "Source
  maps", "Wall of shame", "Ingest failures", "Purge data", "Storage" and so on. Update
  `admin-nav.test.ts` for the keys, then run `npm --prefix dashboard test -- admin-nav`.
  Expect a PASS.

- [ ] **Step 5:** Run `npm --prefix dashboard run check`; expect 0 errors.

### Task 6: PageHeader

**Files:**
- Create: `dashboard/src/lib/components/layout/PageHeader.svelte`

**Interfaces:**
- Produces `PageHeader`. Props:
  `{ title: string; subtitle?: string; back?: { label: string; href: string }; badge?: Snippet; meta?: Snippet; titleClass?: string; leading?: Snippet; freshness?: Snippet; refresh?: Snippet; actions?: Snippet; primary?: Snippet }`.
  - It renders, in DOM order: the back link, then a row of `[leading][title block][end
    cluster]`.
  - The end cluster is `freshness`, `refresh`, `actions`, `primary`, always in that order.
  - Pages pass `<Freshness …/>` and `<RefreshButton …/>` as snippets, so
    `refresh-parity.test.ts` (which scans for `<RefreshButton` and `pageRefresher`) keeps
    passing unchanged.
  - The title is an `<h1>` at `--text-lg`/600, clamped to 2 lines (`-webkit-line-clamp: 2`),
    with the full text in `title=`.
  - The subtitle is `--text-sm` in `--text-muted`, with a 72ch max.
  - The back link is `<a href>` at `--text-xs` in `--text-muted`, preceded by
    `Icon arrow-left`, which mirrors in RTL via the existing `[data-icon]` rule.
  - `meta` sits on its own line under the title. Pages put the ID in a `.mono` span next to
    a `CopyButton`.
  - The header has `margin-bottom: var(--head-gap)` and `align-items: flex-end`. Under
    720px the end cluster wraps below the title block.

- [ ] **Step 1:** Write `PageHeader.svelte` to the interface above.
- [ ] **Step 2:** In `listui-harness`, swap `SessionsList` (list) and `SessionDetail`
  (detail) onto it as the pilot. The full page migration happens in Task 9; this is just the
  header.
- [ ] **Step 3:** Run `npm --prefix dashboard test -- refresh-parity`; expect a PASS.
- [ ] **Step 4:** Run `npm --prefix dashboard run check`; expect 0 errors.
- [ ] **Step 5:** Verify the harness at 1280×900 with a 200-character title injected:
  - `h1.offsetHeight <= 2 * lineHeight + 1`
  - the end cluster's `getBoundingClientRect().right <= innerWidth`
- [ ] **Step 6:** Verify in Arabic. Set `localStorage` locale `ar` and reload.
  - `getComputedStyle(backIcon).transform` is not `none`.
  - The ID span's computed `direction` is `ltr`.
  - `document.fonts.check('13px "Noto Sans Arabic Variable"', 'ع')` returns true.
  
  Take screenshots in English and in Arabic.

### Task 7: States — ErrorState, EmptyState wordings, InlineNotice and describeLoadError

**Files:**
- Create: `dashboard/src/lib/models/load-error.ts`, `dashboard/src/lib/models/load-error.test.ts`
- Create: `dashboard/src/lib/components/ui/ErrorState.svelte`,
  `dashboard/src/lib/components/ui/InlineNotice.svelte`
- Modify: `ui/EmptyState.svelte`, `lib/i18n/catalog/common.ts`

**Interfaces:**
- Consumes: `normalizeError` and `NormalizedError` from `lib/api/client.ts`
  (`{ status, code, message, isNetwork }`).
- Produces:
  - `describeLoadError(err: unknown): { reason: MessageKey | null; details: string }`.
    `reason` is `null` when the page's own handling should keep the backend text.
  - `ErrorState`, props:
    `{ title: string; error: unknown; onretry?: () => void; size?: 'page'|'section' }`.
    It renders the reason via `t()` (or `details` when `reason` is null), plus a muted
    "Details" line holding `details`. A Retry button (`secondary` `sm`) appears when
    `onretry` is set.
  - `EmptyState` gains `size?: 'page'|'section'`. The `page` size has 40px padding, a 30px
    hairline-circle icon and a `--text-md`/600 title. The `section` size has 20px padding and
    no icon.
  - `InlineNotice`, props:
    `{ tone: 'error'|'warning'|'info'|'success'; children: Snippet; action?: Snippet }`.
    It is a `--radius` box with a soft tone fill, a 1px tone border at 25% and `--text-sm`.
    The `error` tone sets `role="alert"`; the others set `role="status"`.
  - Catalog keys (en + ar):
    - `common.loadError.network`, `common.loadError.timeout`, `common.loadError.server`,
      `common.loadError.rateLimited`, `common.loadError.forbidden`,
      `common.loadError.notFound`, `common.loadError.details`
    - `common.couldntLoad` = "Couldn't load {thing}"
    - `common.empty.match` = "No {things} match these filters"
    - `common.empty.range` = "No {things} in this time range"
    - `common.empty.rangeHint` = "Widen the time range to see more."
    - `common.empty.yet` = "No {things} yet"
    - `common.retry` stays "Retry"

- [ ] **Step 1: Write the failing tests** in `load-error.test.ts`:
  - `{status:0,isNetwork:true}` gives `reason 'common.loadError.network'`
  - 504 and `code:'timeout'` give `timeout`
  - 500, 502 and 503 (when `code` is not `release_rollup_pending`) give `server`
  - 429 gives `rateLimited`
  - 403 gives `forbidden`
  - 404 gives `notFound`
  - 400 gives `reason null` with `details` equal to the backend message
  - 503 with `code:'release_rollup_pending'` gives `reason null`
  - a plain `Error('x')` gives `server` with `details 'x'`
- [ ] **Step 2:** Run `npm --prefix dashboard test -- load-error`; expect a FAIL.
- [ ] **Step 3:** Implement `describeLoadError` in `load-error.ts`, using `normalizeError`.
  Add the catalog keys. Use this English copy, verbatim from the spec:
  - network: "Couldn't reach the server. Check your connection and retry."
  - timeout: "The server took too long to answer."
  - server: "The server ran into a problem."
  - rateLimited: "Too many requests. Wait a moment and retry."
  - forbidden: "You don't have access to this."
  - notFound: "This no longer exists."
  - details: "Details"
- [ ] **Step 4:** Run `npm --prefix dashboard test -- load-error`, then the i18n tests
  (`npm --prefix dashboard test -- i18n catalog leak`). Expect a PASS.
- [ ] **Step 5:** Build `ErrorState`, restyle `EmptyState` and build `InlineNotice` to the
  kit. Then run `npm --prefix dashboard run check`; expect 0 errors.

### Task 8: Charts on chart tokens

**Files:**
- Modify: `TimeSeriesChart`, `Sparkline`, `RetentionGrid`, `LifecycleChart`,
  `HourlyLineChart`, `UserActivityChart`, `DurationHistogram`, `StoreInstallsChart`,
  `FunnelChart`, `BarList`, `SankeyChart`, `DateRangePicker` (its calendar highlight).

**Interfaces:**
- Produces: `TimeSeriesChart` gains `highlightLatest?: boolean`, default `true` for bar mode.
  - When it is on, bars use `--chart-neutral` and the last bucket uses `color`.
  - `color` defaults to `var(--chart-1)`.
  - Exception charts pass `color="var(--error)"`.

- [ ] **Step 1:** Replace every `--primary`, `--line`, literal or `hsl()` series color with
  `--chart-1..5`, in series order. Grid and axis lines use `--chart-grid`; tick text is
  `--text-xs` in `--text-faint`.
  - `RetentionGrid` builds its ramp from `--chart-1` with `color-mix` at 8–100%.
  - Sankey takes the palette by node index modulo 5.
  - Line charts keep a single series color.
- [ ] **Step 2:** Run `npm --prefix dashboard test -- css-guards` (expect PASS) and
  `npm --prefix dashboard run check` (expect 0 errors).
- [ ] **Step 3:** Verify `listui-harness ?page=issues` and `?page=sessions` in both themes.
  Read a mid bar's computed `fill`/`background` (expect `--chart-neutral`) and the last bar's
  (expect the series color). Take screenshots.

### Page migration recipe (Tasks 9–12 apply it to every page in their batch)

For each page:

1. Replace the hand-rolled `.head`/`.detail-head`/`.identity`/`h1` block with `PageHeader`.
   - The title uses the page's **own** key. Add `<area>.title` keys where a page borrows one;
     known borrowers are Events, Sessions and Monitors.
   - Detail pages pass `back={{ label: t('nav.<list>'), href: '#/<list>' }}`.
   - Freshness, Refresh and actions go into their snippets.
   - Delete the page's `.head`, `.sub`, `.controls`, `.back`, `.refresh-slot` and
     `.head-actions` CSS.
   - The vertical gap between top-level sections is `var(--page-gap)`, replacing ad-hoc
     margins of 12–40px.
2. Replace load-error rendering with `<ErrorState title={t('common.couldntLoad', {thing})} error={…} onretry={…}>`.
   - Use `size="section"` inside a section.
   - Remove `.err-banner`, `.err`, `.err-msg` and `.stale-banner`. The stale and success
     banners become `InlineNotice`.
3. Empty states use one of the three wordings from Task 7, by the spec's rule: filtered, then
   time-windowed, then "yet" only for lists that are not time-bound. Use `size="section"`
   inside a section.
4. Native or locally styled `<select>` becomes `Select`. Hand-rolled segmented toggles become
   `SegmentedControl`. Raw `<input>` becomes `Input`. Raw `<button>` elements styled as
   buttons become `Button`; links styled as buttons stay `<a>`.
5. Every `font-size` and `font-weight` in the page's `<style>` goes to a token. Unused
   selectors left behind are deleted, using the svelte-check unused-selector warnings as the
   list.
6. Tables:
   - Unwrap any `<Card padding="none">` around a `DataTable`. Use `Card variant="section"`
     with a title, or nothing.
   - Counts use `formatNumber`; `compactNumber` is allowed only in StatTile values and chart
     labels.
   - Percentages use `formatPercent`.
   - Durations use `formatDuration` with `class="num"`.
   - Timestamps use `TimeValue`.
7. Raw API values in badges are mapped through `t()`.
8. Copy follows the spec's wording rules: sentence case, the sidebar's word for page names,
   "New X"/"Create X", "Retry", and "this organization". Hard-coded English in the file moves
   to the catalog with `ar`.

**Per-batch verification:**
- `npm --prefix dashboard run check` gives 0 errors with no new warnings.
- `npm --prefix dashboard test` passes.
- Each migrated page renders in the browser at 1280×900, dark, with zero console errors.

### Task 9: Batch A — list and analytics pages

**Files:**
- Pages: `Overview`, `Issues`, `Events`, `SessionsList`, `UsersExplorer`, `ScreensList`,
  `WorkflowsList`, `DevicesInventory`, `Transactions`, `Performance`, `ActiveUsers`,
  `Retention`, `FunnelBuilder`, `JourneyExplorer`, `Monitors`, `Inspector`.
- Components: `ListToolbar`, `FilterBar`, `SearchInput`, `DateRange`, `TimeFilter`,
  `TimelineFilters`, `devices/*`, `IssueQuickView`.

Apply the recipe. Known specifics:
- `Transactions` and `Retention` use a bare `h1`.
- `Retention` uses rem, its own segmented control, and is the main user of the undefined
  tokens (fixed in Task 2).
- Six segmented controls (Performance, Journey, Users, ActiveUsers, Retention, Inspector tabs)
  become `SegmentedControl`. Inspector's underline tabs may stay as tabs if they really are
  tabs, but restyle them to tokens.
- Remove the local StatTile copy in `FunnelBuilder` (`.sum-*`) in favour of `StatTile`.
- `Monitors` uses `toFixed` and shows the title "Uptime"; it becomes "Monitors".
- In `ScreensList` and `WorkflowsList`, `compactNumber` in tables becomes `formatNumber`.

- [ ] Step 1: Migrate the pages in the order listed, using the recipe.
- [ ] Step 2: Run the per-batch verification. For the browser check, start the
  `listui-harness` entries (sessions, users, issues). Pages the harness cannot mount get
  checked in Task 13's live drive.

### Task 10: Batch B — detail pages

**Files:**
- Pages: `IssueDetail`, `SessionDetail`, `DeviceDetail`, `ScreenDetail`, `PersonProfile`,
  `MonitorDetail`.
- Components: `TransactionDetailPanel`, `OperationTransactionsModal`, `PerfDayModal`,
  `Timeline`, `KeyValueList`.

Apply the recipe. Known specifics:
- **Back labels:** use the `nav.*` label of the list ("Exceptions", "Sessions", "Devices",
  "Screens", "Users", "Monitors").
- **Bug 4:** PersonProfile's back link goes to `#/users`.
- **Key/value blocks:** the four hand-rolled `<dl>`s (`IssueDetail .side-dl`,
  `DeviceDetail .kv`, `PersonProfile .sm-row`, `ScreenDetail .facts`) become `KeyValueList`.
- **IDs and copy:** every page with an ID shows it in `meta` with a `CopyButton`. That means
  adding one to the Issue fingerprint (show it in full), the Screen name and the Person
  distinct_id.
- **Not-found vs load error:** every page separates the two (as SessionDetail already does)
  and offers Retry on a load error.
- **Rail width:** 320px on every page that has a rail, with a 900px breakpoint.
- **Local pills:** `IssueDetail .type-tag`, `SessionDetail .meta-link`/`.meta-static`,
  `MonitorDetail .kindtag`/`.result`/`.ongoing`, and `PersonProfile .tl-session`/`.prop`
  become `Badge`/`StatusBadge`/`CountChip`.
- **MonitorDetail interval tiles:** `.interval-tile` becomes `StatTile`.
- **Timestamps:** all go through `TimeValue`. The Session "Started" tile and Monitor's tables
  stop using `formatDateTime`.

- [ ] Step 1: Migrate the pages.
- [ ] Step 2: Run the per-batch verification, using `detail-harness` and `listui-harness`
  `?page=session-detail`/`issue-detail`.

### Task 11: Batch C — settings and admin

**Files:**
- Pages: `Account`, `AdminIndex`, `Alerts`, `Environments`, `Members`, `Roles`, `Projects`,
  `SettingsApp`, `SourceMaps`, `Storage`, `Purge`, `IngestFailures`, `WallOfShame`, `Docs`.
- Components: `members/*`, `account/*`, `settings/StoreConnectionsCard`,
  `inspector/MaskDialog`.

Apply the recipe. Known specifics:
- **Bug 1:** `SourceMaps` Delete is gated by `ConfirmDialog danger`. The title is
  "Delete source map?" and the confirm label is "Delete source map".
- **Bug 2:** the `Environments` Create and Rename confirm buttons become
  `variant="primary"`.
- **Bug 3:** the `Purge` confirm button becomes `variant="danger"`.
- **Destructive confirms:** `Projects` and `SettingsApp` inline confirm rows, and the
  `IngestFailures` hand-built modal, become `ConfirmDialog`. `DeleteRoleDialog` keeps its
  content but uses `Modal` with the standard footer.
- **Cancel and footers:** Cancel is always `secondary` and always on the left. Projects'
  inline forms flip their order.
- **Feedback:** `Alerts .ok-banner`, `IngestFailures .notice` and `Storage .policy-ok` become
  `InlineNotice`. `SourceMaps`'s `toastStore.push(msg, kind)` becomes `.success`/`.error`.
- **Controls:** the selects in Alerts, SourceMaps, Members, Projects, Storage, WallOfShame
  and IngestFailures, and Purge's raw inputs, move to `Select`/`Input`. Checkboxes get
  `accent-color` globally in `app.css`.
- **Badges:** badge `md` is used for status in tables everywhere it is `sm` elsewhere. Make
  it `sm`.
- `Docs` gets PageHeader and type tokens only. Its layout stays.

- [ ] Step 1: Migrate the pages.
- [ ] Step 2: Run the per-batch verification. Build an axios-adapter mock in
  `perm-harness` or `detail-harness` for Alerts, SourceMaps and Environments to screenshot
  the bug fixes. Test the dialogs with real clicks: Cancel closes, and Confirm calls the
  handler.

### Task 12: Batch D — sign-in and standalone screens

**Files:**
- Pages: `Login`, `Register`, `ForgotPassword`, `ResetPassword`, `ChangePassword`,
  `Onboarding`, `ConfirmEmailChange`, `CancelEmailChange`, `Unsubscribe`.

Apply the recipe where it is relevant (there is no PageHeader here). Specifically:
- Every page uses `AuthLayout` (Onboarding with `width="md"`). The hand-rolled gradient marks
  and headers in `ChangePassword` and `Onboarding` are deleted.
- Error banners become `InlineNotice tone="error"`. On the email-change pages they replace
  the plain red text.
- The duplicate "Back to sign in" links go.
- The inline links in panels pick up the footer link style.
- The hard-coded hints and errors in `ResetPassword` and `ChangePassword` move to the catalog.

- [ ] Step 1: Migrate the pages.
- [ ] Step 2: Run the per-batch verification. Visit the auth pages live (`#/login`,
  `#/register`, `#/forgot-password`) on the dev server; they need no backend to render.

### Task 13: Guards against drift, and the wording pass

**Files:**
- Modify: `dashboard/src/lib/theme/css-guards.test.ts`

- [ ] **Step 1:** Add the three remaining guard tests:
  - `font-size values come from the scale`: in `<style>` blocks, every `font-size:` value is
    `var(--text-…)`, `inherit` or `1em`, and no value ends in `rem`.
  - `font-weight values are 400/500/600`: `var(--weight-…)` is also accepted.
  - `pages render titles through PageHeader`: no `<h1` in `src/pages/*.svelte`, except in
    the auth pages that render inside `AuthLayout`.
- [ ] **Step 2:** Run `npm --prefix dashboard test -- css-guards`. Fix whatever the guards
  flag until they pass.
- [ ] **Step 3: Wording grep pass.**
  - `rg -n "Try again|try again" dashboard/src/lib/i18n` → `common.retry`
  - `rg -n "\borg\b|organisation" dashboard/src/lib/i18n/catalog` → "organization"
  - `rg -n "'Source Maps'|'Wall of Shame'" dashboard/src`
  - Title-case button labels in `catalog/*.ts` become sentence case.
  
  Then run `npm --prefix dashboard test -- i18n leak catalog`; expect a PASS.

### Task 14: Full verification and live screenshots

- [ ] **Step 1: Gates.** Run `npm --prefix dashboard run check` (0 errors, no more than 12
  warnings), `npm --prefix dashboard test` (all pass) and `npm --prefix dashboard run build`
  (success).
- [ ] **Step 2: Live stack.** Boot the API with migrations and a seeded app, following the
  memory recipes `local-e2e-api-drive` and `dashboard-browser-drive`:
  - Serve the API on :8090, which `static/config.js` pins.
  - Seed org → project → app → channel → some events over HTTP and ingest.
  - Log in with the refresh token injected into localStorage, then reload.
  - If the stack cannot boot, fall back to the harnesses with axios-adapter mocks, and say so.
- [ ] **Step 3: Screenshot each feature live**, at 1440×900:
  - **Shell:** sidebar and topbar.
  - **List header and table:** Exceptions.
  - **StatTiles strip and chart:** Sessions.
  - **Detail header:** Issue detail (back link, badge, meta and Copy).
  - **States:** ErrorState (force one with a bad app id) and the EmptyState wordings.
  - **Controls:** Select and SegmentedControl on a filter row.
  - **Confirmations:** ConfirmDialog danger on Source maps delete.
  - **Fixed buttons:** the Environments create dialog's primary button, and the Purge danger
    button.
  - **Badges:** LevelBadge and StatusBadge in the Exceptions table.
  - **Sign-in:** the Login page.
  - **Light theme:** Exceptions.
  - **Arabic RTL:** Exceptions, and Issue detail.
  
  Reset the viewport afterwards. Revert any temporary edits to `static/config.js` or
  `vite.config.ts`.
