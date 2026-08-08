# Timestamp Display Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every hand-rolled `relativeTime` + `title={formatDateTime}` pair with one `TimeValue` component whose relative/absolute mode is app-wide and persisted, so clicking any timestamp switches all of them.

**Architecture:** Three pieces — a `formatTimestamp` helper producing `yyyy-MM-DD HH:mm:ss` in local time, a `timeFormatStore` holding the mode in `localStorage`, and a `TimeValue.svelte` that renders a button and toggles the store. Then a migration sweep across ~20 call sites.

**Tech Stack:** Svelte 5 runes, vitest.

## Global Constraints

- **NEVER commit and never create branches.** This repo's standing rule. Every task ends at "verify". Leave work in the working tree.
- **Tests:** `npm test` (vitest 2.1.9) from `dashboard/`. Type check: `npm run check`. Both must be clean.
- **Vitest has no globals** — import `{ describe, expect, it }` from `'vitest'`. No jsdom: tests cover the helper and the store, not component rendering.
- **Store idiom:** `class XStore { field = $state<T>(init) }` + `export const xStore = new XStore()` in a `.svelte.ts` file, with every `window`/`document` access guarded by `typeof … === 'undefined'`. Copy `lib/stores/theme.svelte.ts` — it is the closest existing analogue (localStorage-backed, two-valued, has a `toggle()`).
- **Default is `relative`** — what every site shows today, so nothing changes for an existing user until they ask.
- **Local time, not UTC.** `relativeTime` and `formatDateTime` are both local; toggling must change precision, not the instant's apparent value.
- **The null contract is `'—'`** for `null` / `undefined` / unparseable, matching every neighbour in `format.ts`.

## Scope correction

The spec estimated ~12 call sites. The real count is ~20, and two differ from what the spec assumed:

- **`Inspector.svelte:255`** renders `{f.last_seen_at ?? '—'}` — a raw ISO string. It imports **neither** `relativeTime` nor `formatDateTime`. It is a pre-existing display bug, in scope here.
- **`WorkflowsList.svelte:194`** is a bare `{relativeTime(r.last_seen)}` with **no** `title`; `formatDateTime` is not imported there.

Sites the spec missed entirely: `Account.svelte:203,204,223,225`, `ScreenDetail.svelte:90,106`, `Events.svelte:355`, `SessionsList.svelte:236`, `DeviceDetail.svelte:141,218`, `PersonProfile.svelte:186`, `IssueDetail.svelte:501`, `EnvironmentsCard.svelte:298,385`.

One deliberate exclusion: **`IssueDetail.svelte:410**` uses `title={`${relativeTime(…)} · ${formatDateTimeZone(…)}`}` — a combined tooltip naming the timezone, for lining occurrence rows up against server logs. Different job. Leave it.

---

### Task 1: `formatTimestamp` helper

**Files:**
- Modify: `dashboard/src/lib/utils/format.ts` (add after `formatDateTimeZone`, which ends line 151)
- Create: `dashboard/src/lib/utils/format.test.ts`

**Interfaces:**
- Produces: `formatTimestamp(input: string | number | Date | null | undefined): string` → `yyyy-MM-DD HH:mm:ss` local, or `'—'`. Consumed by Tasks 3 and 4.

Note there is currently **no test file** in `lib/utils/` — this creates the first.

- [ ] **Step 1: Write the failing test**

Create `dashboard/src/lib/utils/format.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { formatTimestamp } from './format';

describe('formatTimestamp', () => {
  // Built from local-time components so the assertion holds in any TZ the
  // suite runs in. Constructing from an ISO string with a Z suffix would make
  // the expected output depend on the runner's timezone.
  it('formats a local instant as yyyy-MM-DD HH:mm:ss', () => {
    const d = new Date(2026, 7, 6, 14, 5, 7); // 2026-08-06 14:05:07 local
    expect(formatTimestamp(d)).toBe('2026-08-06 14:05:07');
  });

  it('zero-pads single-digit month, day, hour, minute and second', () => {
    const d = new Date(2026, 0, 2, 3, 4, 5); // 2026-01-02 03:04:05 local
    expect(formatTimestamp(d)).toBe('2026-01-02 03:04:05');
  });

  it('uses a 24-hour clock', () => {
    const d = new Date(2026, 7, 6, 23, 0, 0);
    expect(formatTimestamp(d)).toBe('2026-08-06 23:00:00');
  });

  it('returns an em dash for null and undefined', () => {
    expect(formatTimestamp(null)).toBe('—');
    expect(formatTimestamp(undefined)).toBe('—');
  });

  it('returns an em dash for an unparseable value', () => {
    expect(formatTimestamp('not a date')).toBe('—');
  });

  it('accepts an ISO string', () => {
    const iso = new Date(2026, 7, 6, 14, 5, 7).toISOString();
    expect(formatTimestamp(iso)).toBe('2026-08-06 14:05:07');
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/utils/format.test.ts`
Expected: FAIL — `formatTimestamp` is not exported.

- [ ] **Step 3: Implement**

Add to `dashboard/src/lib/utils/format.ts` after `formatDateTimeZone` (line 151):

```ts
/**
 * `yyyy-MM-DD HH:mm:ss` in the viewer's local time — the absolute half of the
 * TimeValue toggle.
 *
 * Deliberately NOT `toLocaleString`: the other three absolute formatters here
 * are locale-formatted ("Aug 6, 2026, 02:15:07 PM"), which is right for prose
 * but wrong for a value someone is lining up against a log line. This one is
 * fixed-width and sortable, so a column of them reads as a column.
 *
 * Local rather than UTC because `relativeTime` and `formatDateTime` are both
 * local: toggling changes precision, never the instant's apparent value.
 */
export function formatTimestamp(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  const p = (n: number) => String(n).padStart(2, '0');
  return (
    `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
    `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
  );
}
```

- [ ] **Step 4: Run the tests**

Run: `cd dashboard && npx vitest run src/lib/utils/format.test.ts`
Expected: PASS, all six.

---

### Task 2: The preference store

**Files:**
- Create: `dashboard/src/lib/stores/time-format.svelte.ts`
- Create: `dashboard/src/lib/stores/time-format.test.ts`

**Interfaces:**
- Produces: `timeFormatStore` with `mode: 'relative' | 'absolute'`, `toggle()`, `set(next)`. Consumed by Task 3.

- [ ] **Step 1: Write the store**

Create `dashboard/src/lib/stores/time-format.svelte.ts`, modelled on `theme.svelte.ts`:

```ts
export type TimeFormat = 'relative' | 'absolute';

const STORAGE_KEY = 'sauron.timeFormat';

function initialFormat(): TimeFormat {
  if (typeof window === 'undefined') return 'relative';
  const stored = window.localStorage.getItem(STORAGE_KEY);
  // Anything else — absent, corrupt, or written by an older build — falls back
  // rather than throwing. A bad preference must not break every timestamp in
  // the app.
  return stored === 'absolute' ? 'absolute' : 'relative';
}

class TimeFormatStore {
  /**
   * Relative ("3 minutes ago") or absolute ("2026-08-06 14:05:07").
   *
   * App-wide rather than per-instance: the intent is a mode — "I am
   * correlating timestamps right now" — not a property of one row. Toggling a
   * fifty-row table one cell at a time is not a feature.
   */
  mode = $state<TimeFormat>('relative');

  constructor() {
    this.mode = initialFormat();
  }

  set(next: TimeFormat): void {
    this.mode = next;
    if (typeof window !== 'undefined') {
      // Private-mode Safari throws on setItem with a full quota. The
      // preference is cosmetic; losing persistence must not break the click.
      try {
        window.localStorage.setItem(STORAGE_KEY, next);
      } catch {
        /* keep the in-memory value */
      }
    }
  }

  toggle(): void {
    this.set(this.mode === 'relative' ? 'absolute' : 'relative');
  }
}

export const timeFormatStore = new TimeFormatStore();
```

- [ ] **Step 2: Write the test**

Create `dashboard/src/lib/stores/time-format.test.ts`. Check how `lib/stores/auth.test.ts` handles `localStorage` in the node environment first and follow it — there is no jsdom, so `window` may need stubbing:

```ts
import { describe, expect, it } from 'vitest';
import { timeFormatStore } from './time-format.svelte';

describe('timeFormatStore', () => {
  it('defaults to relative', () => {
    expect(timeFormatStore.mode).toBe('relative');
  });

  it('toggles to absolute and back', () => {
    timeFormatStore.toggle();
    expect(timeFormatStore.mode).toBe('absolute');
    timeFormatStore.toggle();
    expect(timeFormatStore.mode).toBe('relative');
  });

  it('set is idempotent', () => {
    timeFormatStore.set('absolute');
    timeFormatStore.set('absolute');
    expect(timeFormatStore.mode).toBe('absolute');
    timeFormatStore.set('relative');
  });
});
```

If importing a `.svelte.ts` file fails under vitest without the Svelte plugin processing runes, check how `session.test.ts` imports `session.svelte.ts` — that pairing already works, so mirror whatever it does.

- [ ] **Step 3: Run the tests**

Run: `cd dashboard && npx vitest run src/lib/stores/time-format.test.ts`
Expected: PASS.

---

### Task 3: `TimeValue` component

**Files:**
- Create: `dashboard/src/lib/components/TimeValue.svelte`

**Interfaces:**
- Consumes: `formatTimestamp` (Task 1), `relativeTime`, `timeFormatStore` (Task 2).
- Produces: `<TimeValue value={…} />` with optional `muted` and `asText` props. Consumed by Task 4.

- [ ] **Step 1: Build it**

```svelte
<script lang="ts">
  import { relativeTime, formatTimestamp } from '../utils/format';
  import { timeFormatStore } from '../stores/time-format.svelte';

  interface Props {
    value: string | number | Date | null | undefined;
    /** Apply the muted text colour, as most table cells here do. */
    muted?: boolean;
    /**
     * Render as plain text with no toggle. For the handful of places that need
     * a formatted instant inside another control's label or a StatTile `sub`
     * slot, where a nested button would be invalid markup.
     */
    asText?: boolean;
  }

  let { value, muted = false, asText = false }: Props = $props();

  const isRelative = $derived(timeFormatStore.mode === 'relative');
  const shown = $derived(isRelative ? relativeTime(value) : formatTimestamp(value));
  // The other representation stays in the tooltip, so hovering still answers
  // the question without a click.
  const other = $derived(isRelative ? formatTimestamp(value) : relativeTime(value));
  const empty = $derived(shown === '—');
</script>

{#if empty || asText}
  <span class="tv" class:muted>{shown}</span>
{:else}
  <button
    type="button"
    class="tv toggle"
    class:muted
    title={other}
    aria-label={`${shown} — click to show ${isRelative ? 'exact time' : 'relative time'}`}
    onclick={() => timeFormatStore.toggle()}
  >
    {shown}
  </button>
{/if}

<style>
  /* Styled to read as text, not a control: a table of fifty things that all
     look like buttons is worse than the tooltips this replaces. The affordance
     is the dotted underline on hover. */
  .tv {
    font: inherit;
    color: inherit;
  }
  .tv.muted {
    color: var(--text-muted);
  }
  button.tv {
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    cursor: pointer;
    text-align: inherit;
    white-space: nowrap;
  }
  button.tv:hover {
    text-decoration: underline dotted;
  }
  button.tv:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
    border-radius: 2px;
  }
</style>
```

- [ ] **Step 2: Type check**

Run: `cd dashboard && npm run check`
Expected: 0 errors.

---

### Task 4: Migrate the call sites

**Files:** the ~20 sites below.

Do these in the listed order. `IssueDetail` first — it is the site that prompted this — then verify, then sweep the rest.

**Interfaces:**
- Consumes: `TimeValue` (Task 3).

- [ ] **Step 1: IssueDetail (the requested site)**

`dashboard/src/pages/IssueDetail.svelte:475-482` — replace:

```svelte
            <div>
              <dt>First seen</dt>
              <dd title={formatDateTime(issue.first_seen)}>{relativeTime(issue.first_seen)}</dd>
            </div>
            <div>
              <dt>Last seen</dt>
              <dd title={formatDateTime(issue.last_seen)}>{relativeTime(issue.last_seen)}</dd>
            </div>
```

with:

```svelte
            <div>
              <dt>First seen</dt>
              <dd><TimeValue value={issue.first_seen} /></dd>
            </div>
            <div>
              <dt>Last seen</dt>
              <dd><TimeValue value={issue.last_seen} /></dd>
            </div>
```

Add the import. Also migrate the bare `relativeTime` at `:501`. **Leave `:410` alone** — its combined `relativeTime · formatDateTimeZone` tooltip is a different job.

- [ ] **Step 2: Verify the first site before sweeping**

Run: `cd dashboard && npm run check`
Then, with the dev server running, open an issue and confirm: First seen shows relative text, clicking it switches both First and Last seen to `yyyy-MM-DD HH:mm:ss`, and the tooltip shows the other form.

- [ ] **Step 3: Migrate the `title={formatDateTime}` + `relativeTime` pairs**

Each becomes `<TimeValue value={…} muted />` (keep `muted` only where the original cell carried a muted class):

- `Issues.svelte:237-239` — `<td class="muted" title=…>` → `<td><TimeValue value={issue.last_seen} muted /></td>`
- `UsersExplorer.svelte:255-260` — two cells, `first_seen` and `last_seen`
- `DevicesInventory.svelte:187-189`
- `Account.svelte:203,204,223,225`
- `ScreenDetail.svelte:90,106`
- `Events.svelte:355`
- `SessionsList.svelte:236`

After each file, remove `formatDateTime` from its import if nothing else in that file uses it — `npm run check` will flag the unused import.

- [ ] **Step 4: Migrate the bare `relativeTime` sites**

These gain the absolute form they never had:

- `WorkflowsList.svelte:194` — bare, no title
- `DeviceDetail.svelte:141,218` — bare
- `PersonProfile.svelte:186` — bare
- `EnvironmentsCard.svelte:298,385` — bare

Note `EnvironmentsCard.svelte` may be deleted or moved by the admin-view plan (Task 11 there). If it is already gone, migrate its replacement in `pages/Environments.svelte` instead.

- [ ] **Step 5: Migrate the StatTile sites**

`StatTile` takes `value` and `sub` as **strings**, not snippets, so `TimeValue` cannot go in them directly. Check `StatTile.svelte`'s props first: it has a `visual` snippet slot (per its interface), but `value`/`sub` are plain strings.

- `PersonProfile.svelte:141-148` — First seen / Last seen tiles, currently `value={relativeTime(…)} sub={formatDateTime(…)}`
- `DeviceDetail.svelte:106-107` — First seen is currently **absolute only** with no relative form; Last seen has both

For these, either:
- (a) render `value={timeFormatStore.mode === 'relative' ? relativeTime(x) : formatTimestamp(x)}` and `sub` as the other — reactive to the store but not clickable; or
- (b) extend `StatTile` to accept an optional snippet for `value`.

Prefer (a) — it keeps `StatTile`'s API unchanged and these tiles are not the primary interaction surface. The mode still follows the app-wide toggle, which is the point.

- [ ] **Step 6: Fix the Inspector raw timestamp**

`Inspector.svelte:255` renders `{f.last_seen_at ?? '—'}` — a raw ISO string, and the file imports neither helper. Replace with `<TimeValue value={f.last_seen_at} />` and add the import.

- [ ] **Step 7: Verify**

Run: `cd dashboard && npm test && npm run check && npm run build`
Expected: all clean, no unused-import warnings.

- [ ] **Step 8: Sweep for stragglers**

```bash
cd dashboard && grep -rn "title={formatDateTime" src/
```
Expected: no results, or only `IssueDetail.svelte:410` (the deliberate exclusion).

```bash
cd dashboard && grep -rn "relativeTime(" src/pages src/lib/components
```
Expected: only `TimeValue.svelte` and the StatTile sites from Step 5.

---

### Task 5: Runtime verification

- [ ] **Step 1: Cross-page persistence**

This is the whole point and is not observable from unit tests. With the dev server running:

1. Open `/issues`, click a Last seen cell → the column switches to `yyyy-MM-DD HH:mm:ss`.
2. Navigate to an issue detail → First/Last seen are **already** absolute.
3. Reload the page → still absolute.
4. Navigate to `/users` → absolute there too.
5. Click any timestamp → everything returns to relative.

- [ ] **Step 2: Keyboard and screen reader**

Tab to a timestamp: it takes focus with a visible ring, and Enter/Space toggles it.

- [ ] **Step 3: Null handling**

Find a row with a missing timestamp (or temporarily stub one) and confirm it renders `—` as plain text with no focusable button.
