# Search UI/UX Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the search UI up to the standard of the query-language backend it already sits on — repairing nine reviewed defects across styling, autocomplete, reachability, error surfacing, disclosure, and docs.

**Architecture:** Four slices. S1 rebuilds the search input on the house design system (Tasks 2–3). S2 makes autocomplete grammar-aware and backs it with app-real tag keys sampled from Postgres behind a Redis cache (Tasks 1, 4, 5). S3 surfaces query errors inline and renders the `clamped` disclosure the backend already computes (Tasks 6–7). S4 rewrites the in-app docs from a live schema fetch so they cannot rot (Task 8). Task 9 is live rendered verification, which is the only gate that can see the S1 defect at all.

**Tech Stack:** Svelte 5 (runes), TypeScript, Vite, Vitest, `@lucide/svelte`; Rust, axum, diesel-async, Postgres 16, Redis.

## Global Constraints

- **Never commit and never create a branch.** Leave all work uncommitted in the
  working tree. This overrides the commit steps in any skill template.
- **No Tailwind, and do not add it.** The dashboard has no utility-class
  framework. All styling goes in the component's own `<style>` block using the
  CSS custom properties defined in `dashboard/src/app.css`.
- **Use house UI components**, never raw `<button>`/`<table>`/`<input>` where one
  exists: `Icon.svelte` (semantic kebab-case names from its registry),
  `SearchInput.svelte` is the styling reference, `DataTable`, `Pagination`.
- **Svelte 5 runes only**: `$state`, `$derived`, `$props`, `$effect`,
  `$bindable`. `$state` deep-proxies stored values, so `===` never matches a
  stored object — use `$state.raw` where identity matters.
- **Never use `<input type="number">` with `bind:value`.** It writes back
  `number | null`, which crashes string validators; use `type="text"
  inputmode="numeric"`.
- **Backend tests need `dangerouslyDisableSandbox`** plus host-network
  containers. The Bash sandbox has its own netns, so DB-backed tests return
  early while printing `ok`. A run that skips is reported as skipped, never as
  green.
- **`cargo test --workspace` is fail-fast by default.** Use `--no-fail-fast` for
  any number you intend to quote.
- **After any task touching `sauron-db`, verify `schema.rs` still has exactly 27
  `diesel::table!` blocks and 1 `handled`** — `diesel migration run` has twice
  silently rewritten it.
- Bound parameters only in SQL. Never interpolate a user value into a query
  string; follow `repo::list_persons`.

---

### Task 1: Grammar-aware suggestion model

Today `getAutocompleteSuggestions` returns `string[]`, and the component
substitutes the raw string plus a space. So picking the field `level` inserts
`level `, which the parser reads as **free text**, not a field — the user has to
know to type the `:` themselves. This task replaces the string with a typed
suggestion that carries its own insertion text, and makes a field completion
chain into its own values.

**Files:**
- Modify: `dashboard/src/lib/api/schema.ts`
- Test: `dashboard/src/lib/api/schema.test.ts` (create)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `interface Suggestion { insert: string; label: string; detail?: string; kind: 'field' | 'value' | 'variable' | 'tagKey' }`
  - `getAutocompleteSuggestions(schema: SchemaDefinition, input: string): Suggestion[]`
  - `placeholderFor(schema: SchemaDefinition | null): string`
  - `didYouMean(schema: SchemaDefinition | null, unknownField: string): string | null`
  - `DimensionDef` gains `aliases?: string[]` (the backend already serialises it;
    the client type omitted it).

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/api/schema.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import {
  getAutocompleteSuggestions,
  placeholderFor,
  didYouMean,
  type SchemaDefinition,
} from './schema';

const schema: SchemaDefinition = {
  resource: 'issues',
  variables: [{ prefix: '@tag', description: 'Developer tags', chainable: true }],
  dimensions: [
    { name: 'level', type: 'enum', ops: ['=', '!=', 'in'], options: ['warning', 'error', 'fatal'] },
    { name: 'status', type: 'enum', ops: ['=', '!='], options: ['unresolved', 'resolved'] },
    { name: 'timesSeen', type: 'integer', ops: ['=', '>', '<'] },
  ],
  available_tags: [{ key: 'region', sample_values: ['eu', 'us'] }],
  available_labels: [{ key: 'team', type: 'string' }],
};

describe('getAutocompleteSuggestions', () => {
  it('completes a field WITH its colon so the parser reads a predicate', () => {
    const s = getAutocompleteSuggestions(schema, 'lev');
    expect(s).toHaveLength(1);
    // The whole point: `level ` would lex as free text.
    expect(s[0].insert).toBe('level:');
    expect(s[0].kind).toBe('field');
    expect(s[0].detail).toBe('enum');
  });

  it('offers enum values once the colon is typed', () => {
    const s = getAutocompleteSuggestions(schema, 'level:');
    expect(s.map((x) => x.insert)).toEqual(['level:warning', 'level:error', 'level:fatal']);
    expect(s.every((x) => x.kind === 'value')).toBe(true);
  });

  it('narrows enum values by the partial value already typed', () => {
    const s = getAutocompleteSuggestions(schema, 'level:f');
    expect(s.map((x) => x.insert)).toEqual(['level:fatal']);
  });

  it('offers nothing for a field with no options, rather than a wrong guess', () => {
    expect(getAutocompleteSuggestions(schema, 'timesSeen:')).toEqual([]);
  });

  it('offers real tag keys after @tag., and keeps bare @tag as its own field', () => {
    expect(getAutocompleteSuggestions(schema, '@tag').map((x) => x.insert)).toContain('@tag');
    const keys = getAutocompleteSuggestions(schema, '@tag.re');
    expect(keys.map((x) => x.insert)).toEqual(['@tag.region:']);
    expect(keys[0].kind).toBe('tagKey');
  });

  it('matches a dimension by alias as well as by name', () => {
    const aliased: SchemaDefinition = {
      ...schema,
      dimensions: [{ name: 'timesSeen', type: 'integer', ops: ['='], aliases: ['count'] }],
    };
    expect(getAutocompleteSuggestions(aliased, 'cou').map((x) => x.insert)).toEqual(['timesSeen:']);
  });

  it('returns nothing for an unmatched token', () => {
    expect(getAutocompleteSuggestions(schema, 'nonexistent')).toEqual([]);
  });
});

describe('placeholderFor', () => {
  it('builds an example from what THIS resource actually declares', () => {
    // Finding C: SessionsList hand-wrote `@tag=v1`, which sessions withhold.
    expect(placeholderFor(schema)).toContain('level:');
    expect(placeholderFor(schema)).toContain('@tag');
  });

  it('never advertises a variable the resource does not declare', () => {
    const sessions: SchemaDefinition = {
      ...schema,
      resource: 'sessions',
      variables: [{ prefix: '@context', description: 'Device context', chainable: true }],
      available_tags: [],
    };
    expect(placeholderFor(sessions)).not.toContain('@tag');
    expect(placeholderFor(sessions)).toContain('@context');
  });

  it('falls back to plain copy before the schema loads', () => {
    expect(placeholderFor(null)).toBe('Search…');
  });
});

describe('didYouMean', () => {
  it('suggests the nearest known field for a typo', () => {
    expect(didYouMean(schema, 'levl')).toBe('level');
    expect(didYouMean(schema, 'staus')).toBe('status');
  });

  it('stays silent when nothing is close, rather than guessing', () => {
    expect(didYouMean(schema, 'zzzzzzzz')).toBeNull();
  });

  it('is silent with no schema in hand', () => {
    expect(didYouMean(null, 'levl')).toBeNull();
  });
});
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cd dashboard && npx vitest run src/lib/api/schema.test.ts`
Expected: FAIL — `placeholderFor is not a function`, and the suggestion
assertions fail because the current return type is `string[]`.

- [ ] **Step 3: Implement the model**

In `dashboard/src/lib/api/schema.ts`, add `aliases` to `DimensionDef`:

```ts
export interface DimensionDef {
  name: string;
  type: 'string' | 'number' | 'boolean' | 'enum' | 'duration' | 'timestamp' | 'integer';
  ops: string[];
  options?: string[];
  /** Alternate spellings the resolver accepts; the backend already sends these. */
  aliases?: string[];
}
```

Replace `getAutocompleteSuggestions` entirely (keep `normalizePropertyChain`,
which other callers use) with:

```ts
/**
 * One row of the autocomplete dropdown.
 *
 * `insert` is what replaces the current token, and it is deliberately NOT the
 * same as `label`: completing a field has to carry its own `:` or the token
 * lands as free text. That was the defect — picking `level` inserted `level `,
 * which lexes as a payload search for the literal word "level".
 */
export interface Suggestion {
  insert: string;
  label: string;
  detail?: string;
  kind: 'field' | 'value' | 'variable' | 'tagKey';
}

/** Split a token at its FIRST separator. `@tag.k:v` → field `@tag.k`, value `v`. */
function splitToken(token: string): { field: string; value: string | null } {
  const i = token.indexOf(':');
  if (i < 0) return { field: token, value: null };
  return { field: token.slice(0, i), value: token.slice(i + 1) };
}

function dimensionMatches(d: DimensionDef, prefix: string): boolean {
  if (d.name.startsWith(prefix)) return true;
  return (d.aliases ?? []).some((a) => a.startsWith(prefix));
}

export function getAutocompleteSuggestions(
  schema: SchemaDefinition,
  input: string,
): Suggestion[] {
  const token = input.trim();
  if (!token) return [];
  const { field, value } = splitToken(token);

  // --- a colon is already typed: complete the VALUE ------------------------
  if (value !== null) {
    const dim = (schema.dimensions ?? []).find(
      (d) => d.name === field || (d.aliases ?? []).includes(field),
    );
    // No options means we do not know this field's values. Offering a guess
    // would be worse than offering nothing — the user would insert it.
    if (!dim?.options) return [];
    return dim.options
      .filter((o) => o.startsWith(value))
      .map((o) => ({
        insert: `${dim.name}:${o}`,
        label: o,
        detail: dim.name,
        kind: 'value' as const,
      }));
  }

  // --- `@tag.` chaining ----------------------------------------------------
  if (field.startsWith('@tag.')) {
    const prop = field.slice('@tag.'.length);
    return (schema.available_tags ?? [])
      .filter((t) => t.key.startsWith(prop))
      .map((t) => ({
        insert: `@tag.${t.key}:`,
        label: `@tag.${t.key}`,
        detail: t.sample_values?.slice(0, 2).join(', '),
        kind: 'tagKey' as const,
      }));
  }

  const out: Suggestion[] = [];

  // A bare `@tag` is a filterable field in its own right — it means "any tag
  // key" — so it is offered alongside the keys, not only as a chain prefix.
  for (const v of schema.variables ?? []) {
    if (v.prefix.startsWith(field)) {
      out.push({ insert: v.prefix, label: v.prefix, detail: v.description, kind: 'variable' });
    }
  }

  for (const d of schema.dimensions ?? []) {
    if (dimensionMatches(d, field)) {
      out.push({ insert: `${d.name}:`, label: d.name, detail: d.type, kind: 'field' });
    }
  }

  return out;
}

/**
 * The placeholder, built from what THIS resource declares.
 *
 * Hand-written placeholders are what let `SessionsList` advertise `@tag=v1` on
 * a resource whose tag dimension the backend deliberately withholds — a query
 * that always 400s. A page cannot make that mistake if it does not write the
 * copy.
 */
export function placeholderFor(schema: SchemaDefinition | null): string {
  if (!schema) return 'Search…';
  const parts: string[] = [];
  const first = (schema.dimensions ?? [])[0];
  if (first) {
    parts.push(first.options?.length ? `${first.name}:${first.options[0]}` : `${first.name}:…`);
  }
  const variable = (schema.variables ?? [])[0];
  if (variable) parts.push(`${variable.prefix}.key:value`);
  return parts.length ? `Search ${parts.join(', ')}…` : 'Search…';
}

/** Levenshtein distance, iterative two-row. */
function editDistance(a: string, b: string): number {
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const row = [i];
    for (let j = 1; j <= b.length; j++) {
      row[j] = Math.min(
        prev[j] + 1,
        row[j - 1] + 1,
        prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
    prev = row;
  }
  return prev[b.length];
}

/**
 * The nearest known field to a name the server rejected.
 *
 * Client-side because the schema is already in hand — this costs no request.
 * It never invents a field: the candidates are exactly what the schema
 * advertises, so a suggestion is always something the resolver accepts.
 */
export function didYouMean(
  schema: SchemaDefinition | null,
  unknownField: string,
): string | null {
  if (!schema || !unknownField) return null;
  const candidates = (schema.dimensions ?? []).flatMap((d) => [d.name, ...(d.aliases ?? [])]);
  let best: string | null = null;
  let bestScore = Infinity;
  for (const c of candidates) {
    const score = editDistance(unknownField.toLowerCase(), c.toLowerCase());
    if (score < bestScore) {
      bestScore = score;
      best = c;
    }
  }
  // A third of the length, floor 1: close enough to be a typo, not a different
  // word. `zzzzzzzz` must return nothing rather than the least-bad match.
  const tolerance = Math.max(1, Math.floor(unknownField.length / 3));
  return best !== null && bestScore <= tolerance ? best : null;
}
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cd dashboard && npx vitest run src/lib/api/schema.test.ts`
Expected: PASS, 13 tests.

- [ ] **Step 5: Update the stale existing test**

`dashboard/src/lib/components/search/SearchAutocompleteInput.test.ts` asserts
the old `string[]` shape and will now fail. Rewrite its three suggestion
assertions against the new shape:

```ts
  it('provides autocomplete suggestions when user types variable prefixes', () => {
    const tag = schemaApi.getAutocompleteSuggestions(mockSchema, '@tag');
    expect(tag.map((s) => s.insert)).toContain('@tag');

    const label = schemaApi.getAutocompleteSuggestions(mockSchema, '@$label.t');
    expect(label.map((s) => s.insert)).toEqual(['@$label.team']);

    const dim = schemaApi.getAutocompleteSuggestions(mockSchema, 'stat');
    expect(dim.map((s) => s.insert)).toEqual(['status:']);
  });
```

…and in the `handles context switching` test, change the final assertion to
`expect(suggestions.map((s) => s.insert)).toEqual(['duration_ms:'])`, and in
`handles empty or non-matching suggestions gracefully` leave `toEqual([])` as
is.

Note `@$label` chaining: add an arm to `getAutocompleteSuggestions` mirroring
the `@tag.` one, before the generic variable loop:

```ts
  if (field.startsWith('@$label.')) {
    const prop = field.slice('@$label.'.length);
    return (schema.available_labels ?? [])
      .filter((l) => l.key.startsWith(prop))
      .map((l) => ({
        insert: `@$label.${l.key}`,
        label: `@$label.${l.key}`,
        detail: l.type,
        kind: 'tagKey' as const,
      }));
  }
```

- [ ] **Step 6: Run the whole dashboard suite**

Run: `cd dashboard && npm test`
Expected: PASS, no regressions.

---

### Task 2: Rebuild the input on the house design system

The component is written entirely in Tailwind utility classes — `bg-white`,
`bg-blue-600`, `rounded-md`, `shadow-lg`, `px-3 py-2`. **There is no Tailwind in
this project.** `package.json` has no such dependency and `src/app.css` defines
none of those classes, so today the input renders as a raw browser input that
ignores every design token, and the dropdown has **no background at all** — it
draws transparent over the table beneath it. Dark mode is entirely unhandled.

**Files:**
- Rewrite: `dashboard/src/lib/components/search/SearchAutocompleteInput.svelte`
- Reference (do not modify): `dashboard/src/lib/components/SearchInput.svelte`

**Interfaces:**
- Consumes: `Suggestion`, `getAutocompleteSuggestions`, `placeholderFor` (Task 1).
- Produces: a component whose props are
  `{ appId: string; context?: string; value?: string (bindable); placeholder?: string; error?: string | null; onChange?: (q: string) => void }`.
  **`onSearch` is removed** — see Step 1.

- [ ] **Step 1: Delete the dead Search button**

Grep first, so the deletion is evidence-backed rather than assumed:

Run: `cd dashboard && grep -rn "onSearch" src/`
Expected: hits ONLY inside `SearchAutocompleteInput.svelte` itself — no call
site passes it. The button is a no-op, and it is what forces the cramped
two-control layout inside FilterBar's 220px wrapper. Every page drives its
query from a debounced `bind:value` instead.

- [ ] **Step 2: Rewrite the component**

Replace the whole file with:

```svelte
<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import {
    fetchSchema,
    getAutocompleteSuggestions,
    placeholderFor,
    type SchemaDefinition,
    type Suggestion,
  } from '../../api/schema';

  interface Props {
    appId: string;
    context?: string;
    value?: string;
    /** Overrides the schema-derived default. Prefer letting it generate. */
    placeholder?: string;
    /** A query error to mark inline — fed by the page from its 400/403. */
    error?: string | null;
    onChange?: (query: string) => void;
  }

  let {
    appId,
    context = 'issues',
    value = $bindable(''),
    placeholder = undefined,
    error = null,
    onChange,
  }: Props = $props();

  let schema = $state<SchemaDefinition | null>(null);
  let schemaError = $state<string | null>(null);
  let suggestions = $state<Suggestion[]>([]);
  let open = $state(false);
  let selectedIndex = $state(-1);
  let inputEl = $state<HTMLInputElement | null>(null);
  let listEl = $state<HTMLUListElement | null>(null);

  const effectivePlaceholder = $derived(placeholder ?? placeholderFor(schema));

  $effect(() => {
    // Track both, so a context switch refetches.
    const id = appId;
    const ctx = context;
    if (!id || !ctx) {
      schema = null;
      return;
    }
    let cancelled = false;
    fetchSchema(id, ctx)
      .then((s) => {
        if (!cancelled) {
          schema = s;
          schemaError = null;
        }
      })
      .catch((err: unknown) => {
        // A degraded autocomplete must never block typing a query: the input
        // stays fully usable and only the suggestions go away.
        if (!cancelled) {
          schema = null;
          schemaError = err instanceof Error ? err.message : 'Suggestions unavailable';
        }
      });
    return () => {
      cancelled = true;
    };
  });

  /** The token the caret sits in — suggestions complete this, not the line. */
  function currentToken(v: string): string {
    const parts = v.split(/\s+/);
    return parts[parts.length - 1] ?? '';
  }

  function refresh() {
    if (!schema) {
      open = false;
      return;
    }
    suggestions = getAutocompleteSuggestions(schema, currentToken(value));
    open = suggestions.length > 0;
    selectedIndex = -1;
  }

  function handleInput(e: Event) {
    value = (e.target as HTMLInputElement).value;
    onChange?.(value);
    refresh();
  }

  function apply(s: Suggestion) {
    const parts = value.split(/\s+/);
    parts[parts.length - 1] = s.insert;
    // A field completion ends in `:` and must NOT gain a trailing space — the
    // caret stays inside the token so the value suggestions open immediately.
    value = parts.join(' ') + (s.insert.endsWith(':') ? '' : ' ');
    onChange?.(value);
    inputEl?.focus();
    refresh();
  }

  function move(delta: number) {
    if (!suggestions.length) return;
    selectedIndex = (selectedIndex + delta + suggestions.length) % suggestions.length;
    // Arrowing past the visible window must follow the highlight.
    queueMicrotask(() => {
      listEl?.querySelector<HTMLElement>(`#sac-opt-${selectedIndex}`)?.scrollIntoView({
        block: 'nearest',
      });
    });
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (open && suggestions.length) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        move(1);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        move(-1);
        return;
      }
      if ((e.key === 'Enter' || e.key === 'Tab') && selectedIndex >= 0) {
        e.preventDefault();
        apply(suggestions[selectedIndex]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        open = false;
        return;
      }
    }
    if (e.key === 'Enter') {
      // The query is already live via the debounced `onChange`; Enter just
      // dismisses, so the reader is not left with a list covering their rows.
      e.preventDefault();
      open = false;
    }
  }

  function clear() {
    value = '';
    onChange?.('');
    open = false;
    inputEl?.focus();
  }
</script>

<svelte:window
  onclick={(e) => {
    if (!(e.target instanceof Node)) return;
    if (!inputEl?.parentElement?.parentElement?.contains(e.target)) open = false;
  }}
/>

<div class="sac">
  <div class="shell" class:invalid={!!error}>
    <span class="ic" aria-hidden="true"><Icon name="search" size={15} /></span>
    <input
      bind:this={inputEl}
      type="text"
      role="combobox"
      aria-expanded={open}
      aria-autocomplete="list"
      aria-controls="sac-listbox"
      aria-activedescendant={selectedIndex >= 0 ? `sac-opt-${selectedIndex}` : undefined}
      aria-invalid={!!error}
      spellcheck="false"
      autocomplete="off"
      placeholder={effectivePlaceholder}
      {value}
      oninput={handleInput}
      onkeydown={handleKeyDown}
      onfocus={refresh}
      onblur={() => setTimeout(() => (open = false), 120)}
    />
    {#if value}
      <button class="clear" type="button" aria-label="Clear search" onclick={clear}>
        <Icon name="x" size={14} />
      </button>
    {/if}
  </div>

  {#if error}
    <p class="msg err" role="alert">{error}</p>
  {:else if schemaError}
    <p class="msg hint">{schemaError} — you can still type a query.</p>
  {/if}

  {#if open && suggestions.length}
    <ul bind:this={listEl} id="sac-listbox" role="listbox" class="menu">
      {#each suggestions as s, idx (s.insert)}
        <li
          id="sac-opt-{idx}"
          role="option"
          aria-selected={idx === selectedIndex}
          class:sel={idx === selectedIndex}
        >
          <button type="button" onmousedown={(e) => e.preventDefault()} onclick={() => apply(s)}>
            <span class="lbl mono">{s.label}</span>
            {#if s.detail}<span class="det">{s.detail}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .sac {
    position: relative;
    flex: 1;
    min-width: 260px;
  }
  .shell {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 0.13s ease;
  }
  .shell:focus-within {
    border-color: var(--primary-border);
  }
  .shell.invalid {
    border-color: var(--error);
  }
  .ic {
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    flex-shrink: 0;
  }
  input {
    flex: 1;
    min-width: 0;
    padding: 8px 0;
    background: none;
    border: none;
    color: var(--text);
    outline: none;
    font-family: var(--font-mono);
    font-size: 12.5px;
  }
  input::placeholder {
    color: var(--text-faint);
    font-family: var(--font-sans);
  }
  .clear {
    display: inline-flex;
    align-items: center;
    background: none;
    border: none;
    color: var(--text-faint);
    padding: 2px;
  }
  .clear:hover {
    color: var(--text);
  }
  .msg {
    margin: 4px 2px 0;
    font-size: 11.5px;
  }
  .msg.err {
    color: var(--error);
  }
  .msg.hint {
    color: var(--text-faint);
  }
  .menu {
    position: absolute;
    z-index: 30;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    margin: 0;
    padding: 4px;
    list-style: none;
    max-height: 260px;
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lg);
  }
  .menu li button {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    padding: 6px 8px;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text);
    text-align: left;
    font-size: 12.5px;
  }
  .menu li.sel button,
  .menu li button:hover {
    background: var(--primary-soft);
    color: var(--primary);
  }
  .lbl {
    font-family: var(--font-mono);
  }
  .det {
    color: var(--text-faint);
    font-size: 11px;
    flex-shrink: 0;
  }
</style>
```

- [ ] **Step 3: Verify no Tailwind class survives anywhere in the component**

Run: `cd dashboard && grep -nE "bg-|text-(white|red|blue)|rounded-|shadow-lg\"|px-[0-9]|py-[0-9]" src/lib/components/search/SearchAutocompleteInput.svelte`
Expected: no output. (`shadow-lg` appears only as `var(--shadow-lg)`, which the
pattern excludes by requiring a closing quote.)

- [ ] **Step 4: Type-check**

Run: `cd dashboard && npm run check`
Expected: 0 errors. `onSearch` is gone from the props, so any call site still
passing it fails here — there should be none.

---

### Task 3: Wire the two dead surfaces and drop the hand-written placeholders

`FilterBar` takes `appId`/`context` and passes them down, but **Events and
IssueDetail mount it without either**, so it hits the `appId=""` branch,
`fetchSchema` bails on its `if (!appId)` guard, and the field list never loads.
Autocomplete is silently absent on half the surfaces that have it.

**Files:**
- Modify: `dashboard/src/lib/components/filters/FilterBar.svelte:127-134`
- Modify: `dashboard/src/pages/Events.svelte:545`
- Modify: `dashboard/src/pages/IssueDetail.svelte:648-653`
- Modify: `dashboard/src/pages/SessionsList.svelte:163`
- Modify: `dashboard/src/pages/Issues.svelte:474`

**Interfaces:**
- Consumes: the rebuilt component from Task 2 (`error` prop, no `onSearch`).
- Produces: `FilterBar` gains an `error?: string | null` prop, forwarded to the input.

- [ ] **Step 1: Simplify FilterBar's right rail**

The hardcoded `width: 220px` wrapper is what clips long suggestions; the
component now sizes itself (`flex: 1; min-width: 260px`). Replace lines 127–134
of `FilterBar.svelte`:

```svelte
  <div class="right">
    <SearchAutocompleteInput bind:value={search} appId={appId ?? ''} {context} {error} />
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} {ranges} />
  </div>
```

Add `error` to the `Props` interface and the destructure:

```ts
    context?: string;
    /** A query error from the page's last request, marked on the input. */
    error?: string | null;
```
```ts
    context = undefined,
    error = null,
```

And widen `.right` so the input can actually grow:

```css
  .right { display: flex; align-items: center; gap: 10px; flex: 1; min-width: 320px; justify-content: flex-end; }
```

- [ ] **Step 2: Wire Events**

`dashboard/src/pages/Events.svelte:545` — pass the app and the resource:

```svelte
  <FilterBar
    fields={EVENT_FIELDS}
    bind:filters
    bind:search
    bind:sinceDays
    appId={sessionStore.currentAppId ?? undefined}
    context="events"
  />
```

Confirm `sessionStore` is already imported in this file (it is — it drives the
page's own loads). If the local name differs, use the one already in scope.

- [ ] **Step 3: Wire IssueDetail's occurrences filter**

`dashboard/src/pages/IssueDetail.svelte:648` — this list is occurrences, not
issues:

```svelte
          <FilterBar
            fields={OCCURRENCE_FIELDS}
            bind:filters={occFilters}
            bind:search={occSearch}
            bind:sinceDays={occSince}
            appId={sessionStore.currentAppId ?? undefined}
            context="occurrences"
          />
```

- [ ] **Step 4: Drop the placeholder that always 400s**

`dashboard/src/pages/SessionsList.svelte:163` currently advertises `@tag=v1`,
which the backend deliberately withholds on sessions — the sessions lowerer
refuses `Store::Tag` outright, and `search.rs`'s
`schema_response_generation` test pins that `@tag` is not advertised there.
Remove the hand-written placeholder so the schema-derived one is used:

```svelte
          <SearchAutocompleteInput bind:value={search} appId={sessionStore.currentAppId} context="sessions" />
```

- [ ] **Step 5: Remove the now-redundant hint line on Issues**

`dashboard/src/pages/Issues.svelte:473` reads "Filter by `Tag` (key = value);
the search box also matches tag & payload content." That describes the
pre-language model, and the input now says what it accepts itself. Delete the
`<p class="filter-hint">` element and its now-unused `.filter-hint` CSS rule.

- [ ] **Step 6: Type-check and test**

Run: `cd dashboard && npm run check && npm test`
Expected: 0 errors; all tests pass.

- [ ] **Step 7: Prove the wiring is real, not just present**

Run: `cd dashboard && grep -n "context=" src/pages/Events.svelte src/pages/IssueDetail.svelte src/pages/Issues.svelte src/pages/SessionsList.svelte`
Expected: four hits — `events`, `occurrences`, `issues`, `sessions`. A missing
one is a surface whose autocomplete is still dead.

---

### Task 4: Sample real tag keys from Postgres

`build_schema_response` returns hardcoded fixtures — literal `environment`,
`release` and `team` — for **every app**. Autocomplete offers keys an app may
never have emitted and hides the ones it does.

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (append near the other search helpers)
- Test: `backend/crates/sauron-db/tests/tag_keys_plan.rs` (create)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  ```rust
  pub struct TagKeySample { pub key: String, pub sample_values: Vec<String> }
  pub async fn sample_tag_keys(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
      table: TagSource,
      since: DateTime<Utc>,
      row_limit: i64,
  ) -> QueryResult<Vec<TagKeySample>>;
  pub enum TagSource { ErrorEvents, AnalyticsEvents }
  ```

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-db/tests/tag_keys_plan.rs`. This pins the SQL
shape without a database, the way `query_plan`'s tests do:

```rust
//! The tag-key sampler's SQL shape.
//!
//! Pinned without a database, following the `query_plan` precedent: the two
//! properties that matter here are structural, and both are invisible to a
//! smoke test that merely returns rows.
//!
//! 1. The scan is BOUNDED. An unbounded `jsonb_object_keys` over a partitioned
//!    parent is a seq scan across every partition — the exact shape recorded
//!    in `env-scoped-analytics-503`, where a time-unbounded correlated
//!    subquery measured 190x slower than its bounded twin.
//! 2. The app id is BOUND, never interpolated.

use sauron_db::repo::{tag_keys_sql, TagSource};

#[test]
fn the_sample_is_bounded_by_both_a_window_and_a_row_limit() {
    let sql = tag_keys_sql(TagSource::ErrorEvents);
    assert!(sql.contains("LIMIT"), "the inner sample must be row-bounded: {sql}");
    assert!(
        sql.contains("occurred_at >"),
        "the inner sample must be time-bounded: {sql}"
    );
    assert!(
        sql.contains("ORDER BY occurred_at DESC"),
        "the sample must be the MOST RECENT rows, not an arbitrary page: {sql}"
    );
}

#[test]
fn the_lateral_runs_over_the_sample_not_the_table() {
    let sql = tag_keys_sql(TagSource::ErrorEvents);
    let lateral = sql.find("LATERAL").expect("uses a LATERAL");
    let limit = sql.find("LIMIT").expect("has a LIMIT");
    assert!(
        limit < lateral,
        "the LIMIT must bound the subquery the LATERAL reads, not follow it: {sql}"
    );
}

#[test]
fn every_user_value_is_a_bind_parameter() {
    for source in [TagSource::ErrorEvents, TagSource::AnalyticsEvents] {
        let sql = tag_keys_sql(source);
        assert!(sql.contains("$1"), "app_id must be bound: {sql}");
        assert!(sql.contains("$2"), "the window must be bound: {sql}");
        assert!(sql.contains("$3"), "the row limit must be bound: {sql}");
        // Nothing that looks like an inlined literal uuid or timestamp.
        assert!(!sql.contains('\''), "no literal may appear in the SQL: {sql}");
    }
}

#[test]
fn the_two_sources_address_their_own_table_and_window_column() {
    assert!(tag_keys_sql(TagSource::ErrorEvents).contains("error_events"));
    assert!(tag_keys_sql(TagSource::AnalyticsEvents).contains("analytics_events"));
}
```

- [ ] **Step 2: Run it and verify it fails**

Run: `cd backend && cargo test -p sauron-db --test tag_keys_plan`
Expected: FAIL to compile — `unresolved import sauron_db::repo::tag_keys_sql`.

- [ ] **Step 3: Implement the sampler**

Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Which table a tag-key sample reads, and therefore which window column
/// bounds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    /// `error_events` — Issues and Occurrences.
    ErrorEvents,
    /// `analytics_events` — Events.
    AnalyticsEvents,
}

impl TagSource {
    fn table(self) -> &'static str {
        match self {
            TagSource::ErrorEvents => "error_events",
            TagSource::AnalyticsEvents => "analytics_events",
        }
    }
}

/// One tag key an app has actually emitted, with a few of its values.
#[derive(Debug, Clone, QueryableByName)]
pub struct TagKeySample {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub key: String,
    #[diesel(sql_type = diesel::sql_types::Array<diesel::sql_types::Text>)]
    pub sample_values: Vec<String>,
}

/// The sampler's SQL, split out so its shape can be pinned without a database.
///
/// **Bounded twice, deliberately.** `jsonb_object_keys` over a partitioned
/// parent with no time bound is a seq scan across every partition whose cost
/// scales with retained data rather than with the question asked. The window
/// and the row limit both sit on the INNER subquery so the LATERAL expands at
/// most `row_limit` rows.
///
/// This is a HINT, not an authoritative key list: a key that appears only on
/// rows older than the sample will not be offered. That is the accepted cost of
/// not paying for it on the write path — the grammar still accepts any key the
/// user types, including via the `tag:<key>=<value>` escape hatch for keys
/// outside the identifier charset, so nothing becomes unqueryable.
pub fn tag_keys_sql(source: TagSource) -> String {
    format!(
        "SELECT kv.key AS key, \
                (array_agg(DISTINCT kv.value))[1:5] AS sample_values \
         FROM (SELECT tags FROM {table} \
               WHERE app_id = $1 AND occurred_at > $2 AND tags IS NOT NULL \
               ORDER BY occurred_at DESC LIMIT $3) s, \
              LATERAL jsonb_each_text(s.tags) kv \
         GROUP BY kv.key ORDER BY kv.key",
        table = source.table()
    )
}

/// The tag keys an app has emitted recently, for search autocomplete.
///
/// Never fails a caller: see the route, which treats an error as an empty list.
pub async fn sample_tag_keys(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    source: TagSource,
    since: DateTime<Utc>,
    row_limit: i64,
) -> QueryResult<Vec<TagKeySample>> {
    diesel::sql_query(tag_keys_sql(source))
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Timestamptz, _>(since)
        .bind::<diesel::sql_types::BigInt, _>(row_limit)
        .load::<TagKeySample>(conn)
        .await
}
```

Add `QueryableByName` to the `diesel::prelude` import at the top of `repo.rs` if
it is not already in scope.

- [ ] **Step 4: Run the test and verify it passes**

Run: `cd backend && cargo test -p sauron-db --test tag_keys_plan`
Expected: PASS, 4 tests.

- [ ] **Step 5: Verify the schema file was not rewritten**

Run: `cd backend && grep -c "diesel::table!" crates/sauron-db/src/schema.rs && grep -c "handled" crates/sauron-db/src/schema.rs`
Expected: `27` and `1`. Anything else means `schema.rs` was regenerated and must
be reverted before continuing.

- [ ] **Step 6: Lint**

Run: `cd backend && cargo fmt --all && cargo clippy -p sauron-db --all-targets -- -D warnings`
Expected: clean. **Do not skip `fmt`** — a fmt failure skips clippy and test in
CI and has previously hidden a crate that did not compile at all.

---

### Task 5: Serve real keys from the schema route, cached in Redis

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/search.rs:526-671`
- Test: in-file `#[cfg(test)] mod tests` (append)

**Interfaces:**
- Consumes: `repo::sample_tag_keys`, `repo::TagSource`, `repo::TagKeySample` (Task 4).
- Produces: `build_schema_response` gains a fourth parameter,
  `tags: Vec<TagInfo>`; `tag_source_for(resource) -> Option<TagSource>`;
  cache key helper `tag_cache_key(app_id, resource) -> String`.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `search.rs`:

```rust
    // -- Real tag keys (search UX overhaul, Task 5) -------------------------

    /// The fixtures were returned for every app, so autocomplete offered keys
    /// an app may never have emitted and hid the ones it did.
    #[test]
    fn the_schema_serves_the_tags_it_is_given_not_a_fixture() {
        let real = vec![TagInfo {
            key: "checkout_step".to_string(),
            sample_values: Some(vec!["payment".to_string()]),
        }];
        let resp = build_schema_response("issues", sauron_query::Resource::Issues, real);
        let keys: Vec<&str> = resp.available_tags.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["checkout_step"]);
        assert!(
            !keys.contains(&"release"),
            "the hardcoded fixture must be gone: {keys:?}"
        );
    }

    /// A resource with no tag dimension must not be handed tags even if the
    /// sampler produced some — the autocomplete would offer a prefix every
    /// query built from it is then rejected for.
    #[test]
    fn a_resource_without_a_tag_dimension_is_served_no_tags() {
        let real = vec![TagInfo { key: "region".to_string(), sample_values: None }];
        let resp = build_schema_response("sessions", sauron_query::Resource::Sessions, real);
        assert!(resp.available_tags.is_empty(), "{:?}", resp.available_tags);
        assert!(!resp.variables.iter().any(|v| v.prefix == "@tag"));
    }

    /// Which physical table a resource's tags live on. `None` means the
    /// resource has no tags to sample, and no query is issued at all.
    #[test]
    fn each_resource_samples_its_own_table() {
        use sauron_db::repo::TagSource;
        use sauron_query::Resource;
        assert_eq!(tag_source_for(Resource::Issues), Some(TagSource::ErrorEvents));
        assert_eq!(tag_source_for(Resource::Occurrences), Some(TagSource::ErrorEvents));
        assert_eq!(tag_source_for(Resource::Events), Some(TagSource::AnalyticsEvents));
        assert_eq!(tag_source_for(Resource::Sessions), None);
    }

    /// The cache is keyed per app AND per resource: Issues and Events read
    /// different tables, so one key would serve analytics tags on Issues.
    #[test]
    fn the_cache_key_separates_apps_and_resources() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert_ne!(tag_cache_key(a, "issues"), tag_cache_key(b, "issues"));
        assert_ne!(tag_cache_key(a, "issues"), tag_cache_key(a, "events"));
        assert!(tag_cache_key(a, "issues").starts_with("search:tagkeys:"));
    }
```

- [ ] **Step 2: Run and verify failure**

Run: `cd backend && cargo test -p sauron-api --bin sauron-api search::tests 2>&1 | tail -20`
Expected: FAIL to compile — `build_schema_response` takes 2 arguments, and
`tag_source_for` / `tag_cache_key` do not exist.

- [ ] **Step 3: Implement**

In `search.rs`, replace the two hardcoded blocks (lines ~608–630) and the
signature:

```rust
/// How long a sampled key list is served from cache.
///
/// Five minutes: long enough that the bounded scan runs at most a handful of
/// times an hour per app, short enough that a newly deployed tag shows up in
/// autocomplete within one coffee.
const TAG_CACHE_TTL_SECS: u64 = 300;

/// How many recent rows the sampler expands. See `repo::tag_keys_sql` for why
/// this is bounded at all.
const TAG_SAMPLE_ROWS: i64 = 2_000;

/// The window the sample reads. Independent of the caller's `since_days`: the
/// key list describes the app, not the query.
const TAG_SAMPLE_DAYS: i64 = 7;

/// Which table a resource's tags live on — `None` when it has none, in which
/// case no query is issued.
pub fn tag_source_for(resource: sauron_query::Resource) -> Option<sauron_db::repo::TagSource> {
    use sauron_db::repo::TagSource;
    use sauron_query::Resource;
    match resource {
        Resource::Issues | Resource::Occurrences => Some(TagSource::ErrorEvents),
        Resource::Events => Some(TagSource::AnalyticsEvents),
        Resource::Sessions
        | Resource::Devices
        | Resource::Persons
        | Resource::Transactions => None,
    }
}

pub fn tag_cache_key(app_id: Uuid, resource: &str) -> String {
    format!("search:tagkeys:{app_id}:{resource}")
}
```

Change `build_schema_response`'s signature and its `available_tags` block:

```rust
pub fn build_schema_response(
    context_str: &str,
    resource: sauron_query::Resource,
    tags: Vec<TagInfo>,
) -> SchemaResponse {
```
```rust
    // Gated on the catalog, not on whether the sampler found anything: a
    // resource with no tag dimension must not be offered a `@tag` prefix that
    // every query built from it would then be rejected for.
    let available_tags = if sauron_query::catalog::tag_dimension(resource).is_some() {
        tags
    } else {
        vec![]
    };
```

Leave `available_labels` as it is — labels are a Sessions concept with no
sampler in this slice, and inventing one is out of scope. Change its fixture to
an empty vec so nothing false is advertised:

```rust
    let available_labels: Vec<LabelInfo> = vec![];
```

Now the handler. Replace the tail of `schema`:

```rust
    let tags = load_tag_keys(&state, &mut conn, app_id, context_str, resource).await;
    Ok(Json(build_schema_response(context_str, resource, tags)))
}

/// Sampled tag keys, cached — and **never a failure**.
///
/// Autocomplete is a hint. A sampler that timed out, a Redis that is down, or a
/// resource with no tags at all must all degrade to "no suggestions", never to
/// a failed schema request: the input stays usable and the user types the query
/// themselves.
async fn load_tag_keys(
    state: &AppState,
    conn: &mut sauron_db::pool::PgConn,
    app_id: Uuid,
    context_str: &str,
    resource: sauron_query::Resource,
) -> Vec<TagInfo> {
    let Some(source) = tag_source_for(resource) else {
        return vec![];
    };
    let cache_key = tag_cache_key(app_id, context_str);

    if let Ok(Some(hit)) = state.redis.get(&cache_key).await {
        if let Ok(tags) = serde_json::from_str::<Vec<TagInfo>>(&hit) {
            return tags;
        }
    }

    let since = Utc::now() - Duration::days(TAG_SAMPLE_DAYS);
    let sampled = match sauron_db::repo::sample_tag_keys(
        conn,
        app_id,
        source,
        since,
        TAG_SAMPLE_ROWS,
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, %app_id, "tag key sample failed; serving no suggestions");
            return vec![];
        }
    };

    let tags: Vec<TagInfo> = sampled
        .into_iter()
        .map(|r| TagInfo {
            key: r.key,
            sample_values: (!r.sample_values.is_empty()).then_some(r.sample_values),
        })
        .collect();

    if let Ok(json) = serde_json::to_string(&tags) {
        // A cache write failure is not the caller's problem — they still get
        // the freshly sampled list.
        let _ = state.redis.set_ex(&cache_key, &json, TAG_CACHE_TTL_SECS).await;
    }
    tags
}
```

- [ ] **Step 4: Fix the pre-existing test that now has the wrong arity**

`schema_response_generation` calls `build_schema_response` with two arguments.
Update all three call sites in that test to pass `vec![]`. Its assertions about
which `variables` are advertised are unchanged and must still pass — they are
the pin for finding C.

- [ ] **Step 5: Run and verify pass**

Run: `cd backend && cargo test -p sauron-api --bin sauron-api search:: 2>&1 | tail -20`
Expected: PASS. If the run reports `0 filtered out; 0 passed`, the tests did not
run — see the sandbox constraint.

- [ ] **Step 6: Lint and full backend check**

Run: `cd backend && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

---

### Task 6: Inline query errors with "did you mean"

The backend writes precise 400s (`unknown field `levl``) and 403s that name the
permission which lifts them. Every page routes them into the generic page-level
error card, so the input the reader is looking at is never marked invalid.

**Files:**
- Create: `dashboard/src/lib/utils/query-error.ts`
- Test: `dashboard/src/lib/utils/query-error.test.ts` (create)
- Modify: `dashboard/src/pages/Issues.svelte`, `Events.svelte`, `IssueDetail.svelte`, `SessionsList.svelte`

**Interfaces:**
- Consumes: `didYouMean` (Task 1), `error` prop on `FilterBar`/`SearchAutocompleteInput` (Tasks 2–3).
- Produces: `queryErrorFor(status: number | null, message: string | null, schema: SchemaDefinition | null): string | null`
  and `preflight(query: string): string | null`.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/utils/query-error.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { queryErrorFor, preflight } from './query-error';
import type { SchemaDefinition } from '../api/schema';

const schema: SchemaDefinition = {
  resource: 'issues',
  variables: [],
  dimensions: [{ name: 'level', type: 'enum', ops: ['='], options: ['error'] }],
  available_tags: [],
  available_labels: [],
};

describe('queryErrorFor', () => {
  it('is silent when nothing failed', () => {
    expect(queryErrorFor(null, null, schema)).toBeNull();
  });

  it('surfaces a 400 verbatim and appends a suggestion', () => {
    const msg = queryErrorFor(400, 'unknown field `levl`', schema);
    expect(msg).toContain('unknown field `levl`');
    expect(msg).toContain('did you mean `level`');
  });

  it('surfaces a 400 with no suggestion when nothing is close', () => {
    const msg = queryErrorFor(400, 'unknown field `zzzzzzzz`', schema);
    expect(msg).toBe('unknown field `zzzzzzzz`');
  });

  it('passes a 403 through unchanged — the backend names the permission', () => {
    const back = 'filtering by tag requires event:read';
    expect(queryErrorFor(403, back, schema)).toBe(back);
  });

  it('ignores failures that are not about the query', () => {
    // A 500 or a network drop is the page error card's job, not the input's.
    expect(queryErrorFor(500, 'internal error', schema)).toBeNull();
    expect(queryErrorFor(0, 'Network Error', schema)).toBeNull();
  });
});

describe('preflight', () => {
  it('passes a well-formed query', () => {
    expect(preflight('level:error (a OR b)')).toBeNull();
    expect(preflight('')).toBeNull();
  });

  it('catches an unbalanced paren before a request is issued', () => {
    expect(preflight('(level:error')).toContain('parenthes');
  });

  it('catches a dangling boolean operator', () => {
    expect(preflight('level:error OR')).toContain('OR');
  });
});
```

- [ ] **Step 2: Run and verify failure**

Run: `cd dashboard && npx vitest run src/lib/utils/query-error.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

Create `dashboard/src/lib/utils/query-error.ts`:

```ts
/**
 * Turning a failed search request into a message that belongs ON the input.
 *
 * Only failures that are ABOUT the query land here. A 500 or a dropped
 * connection is the page's error card's job — marking the input invalid for a
 * server fault would tell the reader to fix a query that is fine.
 */
import { didYouMean, type SchemaDefinition } from '../api/schema';

/** Pulls the field name out of the backend's `unknown field \`x\`` wording. */
function unknownFieldIn(message: string): string | null {
  const m = message.match(/unknown field [`'"]?([A-Za-z0-9_.$@-]+)[`'"]?/i);
  return m ? m[1] : null;
}

export function queryErrorFor(
  status: number | null,
  message: string | null,
  schema: SchemaDefinition | null,
): string | null {
  if (!message) return null;
  // 400 = the query is malformed or names something unknown.
  // 403 = a withheld dimension; the backend's text already names the
  //       permission that lifts it, so it is passed through unparaphrased.
  if (status !== 400 && status !== 403) return null;
  if (status === 403) return message;

  const bad = unknownFieldIn(message);
  const near = bad ? didYouMean(schema, bad) : null;
  return near ? `${message} — did you mean \`${near}\`?` : message;
}

/**
 * Structural problems worth catching before a request goes out.
 *
 * Deliberately shallow: unknown FIELDS stay a server-side 400, because only the
 * backend holds the catalog and a client-side copy is exactly the rot the
 * anti-rot test exists to prevent.
 */
export function preflight(query: string): string | null {
  const q = query.trim();
  if (!q) return null;

  let depth = 0;
  let inQuote = false;
  for (const ch of q) {
    if (ch === '"') inQuote = !inQuote;
    if (inQuote) continue;
    if (ch === '(') depth++;
    if (ch === ')') depth--;
    if (depth < 0) return 'Unbalanced parentheses — a `)` has no opening `(`.';
  }
  if (depth > 0) return 'Unbalanced parentheses — close the `(` before searching.';
  if (inQuote) return 'Unclosed quote — close the `"` before searching.';

  const last = q.split(/\s+/).pop() ?? '';
  if (last === 'OR' || last === 'AND') {
    return `Dangling \`${last}\` — add the term it joins.`;
  }
  return null;
}
```

- [ ] **Step 4: Run and verify pass**

Run: `cd dashboard && npx vitest run src/lib/utils/query-error.test.ts`
Expected: PASS, 8 tests.

- [ ] **Step 5: Wire it on Issues**

In `dashboard/src/pages/Issues.svelte`, add near the other deriveds (the page
already exposes `issuesView.errorStatus`, used for its 403 filter gating):

```ts
  import { queryErrorFor, preflight } from '../lib/utils/query-error';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';
```
```ts
  // The input's own schema, for `did you mean`. Fetched once per app+resource;
  // a failure just means no suggestion text.
  let searchSchema = $state<SchemaDefinition | null>(null);
  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    fetchSchema(id, 'issues')
      .then((s) => !cancelled && (searchSchema = s))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  /**
   * What to mark ON the input, as opposed to on the page's error card.
   *
   * A local parse problem wins: it is more specific than whatever the last
   * request said, and it means no request was worth issuing at all.
   */
  const searchError = $derived(
    preflight(search) ?? queryErrorFor(issuesView.errorStatus, error, searchSchema),
  );
```

Pass it down at line 474:

```svelte
  <FilterBar fields={ISSUE_FIELDS} bind:filters bind:search bind:sinceDays ranges={ISSUE_RANGES} appId={sessionStore.currentAppId ?? undefined} context="issues" error={searchError} />
```

- [ ] **Step 6: Repeat for the other three pages**

Apply the identical pattern to `Events.svelte` (context `events`),
`IssueDetail.svelte` (context `occurrences`, using `occSearch` and that view's
error/status), and `SessionsList.svelte` (context `sessions`, passing `error`
straight to `SearchAutocompleteInput` since it does not use FilterBar). Repeat
the code rather than extracting a helper — each page names its view state
differently, and a shared hook would need all four names threaded through it.

If a page does not expose an `errorStatus`, pass `null` for the status: the 400
path is then skipped and only `preflight` marks the input. Note that in the
task's Step 5 code and here.

- [ ] **Step 7: Type-check and test**

Run: `cd dashboard && npm run check && npm test`
Expected: 0 errors; all tests pass.

---

### Task 7: Render the disclosure the backend already computes

`resolve_window` goes to real lengths to return `clamped` naming the window
actually **served**, by the rule that actually bound. **No page reads it.** A
query silently narrowed from 365 days to 30 renders as a complete result set.

**Files:**
- Create: `dashboard/src/lib/components/search/SearchDisclosure.svelte`
- Test: `dashboard/src/lib/components/search/SearchDisclosure.test.ts` (create)
- Modify: `dashboard/src/pages/Issues.svelte`, `Events.svelte`, `IssueDetail.svelte`

**Interfaces:**
- Consumes: `ClampInfo` from `lib/api/search.ts`.
- Produces: a component with props
  `{ clamped?: ClampInfo | null; payloadSearched?: boolean | null }`.

- [ ] **Step 1: Write the failing test**

Create `dashboard/src/lib/components/search/SearchDisclosure.test.ts`. Test the
message builder, which is exported from the component's `module` block so it is
reachable without mounting:

```ts
import { describe, it, expect } from 'vitest';
import { disclosuresFor } from './SearchDisclosure.svelte';

describe('disclosuresFor', () => {
  it('says nothing when nothing was narrowed', () => {
    expect(disclosuresFor(null, null)).toEqual([]);
    expect(disclosuresFor(null, true)).toEqual([]);
  });

  it('names the window ACTUALLY SERVED and the reason it bound', () => {
    const msgs = disclosuresFor(
      { field: 'last_seen', to: '30d', reason: 'unindexed predicate requires a bounded time window' },
      null,
    );
    expect(msgs).toHaveLength(1);
    expect(msgs[0].text).toContain('30d');
    expect(msgs[0].text).toContain('unindexed predicate');
    expect(msgs[0].tone).toBe('warning');
  });

  it('reports a narrowed payload search only when it is false', () => {
    // `null` means no search ran; `true` means it ran in full. Only `false` —
    // "it ran and quietly matched less than you think" — is worth a line.
    expect(disclosuresFor(null, false)).toHaveLength(1);
    expect(disclosuresFor(null, false)[0].text).toContain('event:read');
    expect(disclosuresFor(null, true)).toHaveLength(0);
    expect(disclosuresFor(null, null)).toHaveLength(0);
  });

  it('renders both when both are true', () => {
    const msgs = disclosuresFor(
      { field: 'occurred_at', to: '7d', reason: 'bounded window' },
      false,
    );
    expect(msgs).toHaveLength(2);
  });
});
```

- [ ] **Step 2: Run and verify failure**

Run: `cd dashboard && npx vitest run src/lib/components/search/SearchDisclosure.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

Create `dashboard/src/lib/components/search/SearchDisclosure.svelte`:

```svelte
<!--
  What the result set below this line LEAVES OUT.

  Both facts here are ones the rows themselves cannot show: a window the
  planner narrowed, and a payload search that matched fewer columns than the
  reader's permissions let them see. A page that renders neither shows a
  partial answer that looks complete.

  `total_is_capped` deliberately does NOT appear here — `CursorPagination`
  already renders it as a `+` on the count, and a second surface for the same
  fact is noise.
-->
<script module lang="ts">
  import type { ClampInfo } from '../../api/search';

  export interface Disclosure {
    text: string;
    tone: 'warning' | 'info';
  }

  /**
   * `clamped` names the window SERVED, by the rule that actually bound — the
   * handler is careful to report the tightest of the caller's own window, the
   * route ceiling and the planner clamp, so this copy can quote it directly.
   */
  export function disclosuresFor(
    clamped: ClampInfo | null | undefined,
    payloadSearched: boolean | null | undefined,
  ): Disclosure[] {
    const out: Disclosure[] = [];
    if (clamped) {
      out.push({
        text: `Showing the last ${clamped.to} only — ${clamped.reason}. Rows outside that window are not included.`,
        tone: 'warning',
      });
    }
    // Three states, and only one is worth a line. `null` = no search ran.
    // `true` = it ran in full. `false` = it ran and silently matched less.
    if (payloadSearched === false) {
      out.push({
        text: 'Your search matched titles and metadata only — event payloads need event:read, so some matching rows may be missing.',
        tone: 'info',
      });
    }
    return out;
  }
</script>

<script lang="ts">
  import Icon from '../ui/Icon.svelte';

  interface Props {
    clamped?: ClampInfo | null;
    payloadSearched?: boolean | null;
  }
  let { clamped = null, payloadSearched = null }: Props = $props();

  const items = $derived(disclosuresFor(clamped, payloadSearched));
</script>

{#each items as d (d.text)}
  <p class="disclosure {d.tone}">
    <Icon name={d.tone === 'warning' ? 'triangle-alert' : 'info'} size={14} />
    <span>{d.text}</span>
  </p>
{/each}

<style>
  .disclosure {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 12px;
    padding: 8px 12px;
    border-radius: var(--radius);
    font-size: 12.5px;
  }
  .disclosure.warning {
    background: var(--warning-soft);
    color: var(--warning);
  }
  .disclosure.info {
    background: var(--info-soft);
    color: var(--info);
  }
</style>
```

- [ ] **Step 4: Run and verify pass**

Run: `cd dashboard && npx vitest run src/lib/components/search/SearchDisclosure.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 5: Wire on Issues**

In `Issues.svelte`, add the import and a derived beside `totalIsCapped`:

```ts
  import SearchDisclosure from '../lib/components/search/SearchDisclosure.svelte';
```
```ts
  const clamped = $derived(issuesView.data?.clamped ?? null);
```

…and render it immediately after `<FilterBar …/>` (line ~474), before the
table:

```svelte
  <SearchDisclosure {clamped} />
```

- [ ] **Step 6: Wire on Events and IssueDetail**

`Events.svelte`: read `clamped` off the same envelope `streamTotalCapped` comes
from (`streamPage?.clamped ?? null`) and render `<SearchDisclosure {clamped} />`
directly after its `<FilterBar>`.

`IssueDetail.svelte`: it already renders a bespoke `payload_searched === false`
line near line 653. Replace that block with
`<SearchDisclosure clamped={occClamped} payloadSearched={occStats?.payload_searched ?? null} />`,
adding `const occClamped = $derived(occPage?.clamped ?? null);` beside the
page's other occurrence deriveds. Use the local names already in that file —
grep for `payload_searched` to find them.

- [ ] **Step 7: Verify every searched list discloses**

Run: `cd dashboard && grep -c "SearchDisclosure" src/pages/Issues.svelte src/pages/Events.svelte src/pages/IssueDetail.svelte`
Expected: `2` for each (import + usage).

- [ ] **Step 8: Type-check and test**

Run: `cd dashboard && npm run check && npm test`
Expected: 0 errors; all tests pass.

---

### Task 8: Rewrite the in-app search docs from a live schema

`Docs.svelte:383-460` describes the pre-language model — chips, a free-text box,
and an ops table of `contains`/`=`. The surfaces it describes now run the query
language.

**Files:**
- Modify: `dashboard/src/pages/Docs.svelte:383-465`

**Interfaces:**
- Consumes: `fetchSchema`, `SchemaDefinition` (Task 1); `sessionStore` for the app id.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Replace the hardcoded field tables with a live fetch**

The current `searchCoverageRows`, the three per-resource signature arrays, and
`tagFilterExample` are hand-maintained lists that have already rotted once.
Delete them and fetch instead, so the table cannot disagree with the resolver:

```ts
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';

  const SEARCHABLE = ['issues', 'events', 'occurrences', 'sessions'] as const;

  let schemas = $state<Record<string, SchemaDefinition>>({});
  let schemaError = $state<string | null>(null);

  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    Promise.all(
      SEARCHABLE.map((ctx) =>
        fetchSchema(id, ctx)
          .then((s) => [ctx, s] as const)
          .catch(() => null),
      ),
    ).then((pairs) => {
      if (cancelled) return;
      const next: Record<string, SchemaDefinition> = {};
      for (const p of pairs) if (p) next[p[0]] = p[1];
      schemas = next;
      schemaError = Object.keys(next).length ? null : 'Could not load the field list.';
    });
    return () => {
      cancelled = true;
    };
  });
```

- [ ] **Step 2: Write the grammar reference**

Replace the prose in the search section with the operators the grammar actually
accepts. Copy these exactly — they are the spellings `sauron-query` resolves:

```ts
  const queryOperators: { sig: string; desc: string }[] = [
    { sig: 'field:value', desc: 'Equals. `level:error`' },
    { sig: 'field:!value', desc: 'Not equal. `level:!info`' },
    { sig: 'field:>n  field:>=n', desc: 'Greater than / or equal. `timesSeen:>5`' },
    { sig: 'field:<n  field:<=n', desc: 'Less than / or equal.' },
    { sig: 'field:[a,b]', desc: 'Any of. `level:[error,fatal]`' },
    { sig: 'field:~text', desc: 'Contains this literal substring — `*` is not a wildcard here.' },
    { sig: 'has:field', desc: 'The field is present at all. Carries no value.' },
    { sig: 'bare words', desc: 'Free text against the payload. `boom`' },
    { sig: 'A OR B', desc: 'Either. Terms separated by a space are AND by default.' },
    { sig: '!term  !(a b)', desc: 'Negation, over a term or a whole group.' },
    { sig: 'sort=col  sort=-col', desc: 'A bare column is DESCENDING; `-` reverses it.' },
  ];

  const queryVariables: { sig: string; desc: string }[] = [
    { sig: '@tag:value', desc: 'Matches across EVERY tag key.' },
    { sig: '@tag.key:value', desc: 'One named key. `@tag.region:eu`' },
    { sig: 'tag:key=value', desc: 'Escape hatch for keys with characters outside `A-Za-z0-9_.-`.' },
    { sig: '@context.os.name:Linux', desc: 'Device/runtime context. Needs event:read.' },
    { sig: '@extra.key:value', desc: 'Developer-attached extra metadata. Needs event:read.' },
  ];
```

Render both as the same definition table the page already uses for SDK
signatures — reuse the existing markup and CSS class rather than adding a new
table style.

- [ ] **Step 3: Render the live field table**

For each fetched schema, a section listing its dimensions with type and ops:

```svelte
  {#each SEARCHABLE as ctx (ctx)}
    {#if schemas[ctx]}
      <h4>{ctx}</h4>
      <table class="sig-table">
        <thead><tr><th>Field</th><th>Type</th><th>Operators</th></tr></thead>
        <tbody>
          {#each schemas[ctx].dimensions as d (d.name)}
            <tr>
              <td class="mono">{d.name}</td>
              <td>{d.type}{#if d.options}<span class="muted"> ({d.options.join(' | ')})</span>{/if}</td>
              <td class="mono">{d.ops.join(' ')}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/each}
  {#if schemaError}<p class="muted">{schemaError}</p>{/if}
```

Match the existing table classes in this file; grep for `sig-table` and use
whatever it actually uses.

- [ ] **Step 4: State the sampler's honest limit**

Add one line under the field table, because a reader who sees an incomplete
autocomplete list needs to know it is a hint:

> Tag-key suggestions come from a sample of recent events, so a key you have
> not sent lately may not be offered. You can still type it — any key is
> queryable.

- [ ] **Step 5: Type-check and test**

Run: `cd dashboard && npm run check && npm test`
Expected: 0 errors; all tests pass.

- [ ] **Step 6: Verify no stale copy survives**

Run: `cd dashboard && grep -n "no query language\|no operators\|free-text box" src/pages/Docs.svelte`
Expected: no output.

---

### Task 9: Live rendered verification, both themes

This is the only gate that can see the defect Task 2 repairs. `svelte-check` and
vitest **pass today**, with the input visually broken and the dropdown
transparent — no static gate covers rendered appearance.

**Files:** none modified; this task produces evidence.

- [ ] **Step 1: Bring up the API**

Confirm Postgres and Redis are reachable at the addresses in
`.claude/launch.json` first (`docker ps`), then build and start:

Run: `cd backend && cargo build --bin sauron-api`
Then start the `sauron-api-slice3` configuration via the preview tooling (never
via a bare Bash `&`).

If the stack will not come up, **stop and report it as unverified.** Do not
mark Task 2 complete on a green type-check.

- [ ] **Step 2: Start the dashboard**

Start the `dashboard-slice3` configuration. Confirm it is talking to port 8100,
not a stale committed `static/config.js` pinning `:8090`.

- [ ] **Step 3: Drive the search box on Issues**

Navigate to the Issues page. Type `lev` and confirm:
- the dropdown has a **solid** background and does not show the table through it;
- picking `level` inserts `level:` and the value list opens immediately;
- picking `error` yields `level:error ` and the list closes;
- the clear (×) button appears and empties the box;
- Escape closes the list; ArrowDown/ArrowUp move the highlight.

- [ ] **Step 4: Confirm the error path**

Type `levl:error`. Expected: the input border goes red and the message reads the
backend's text plus "did you mean `level`?". Then type `(level:error` and
confirm the parenthesis message appears with **no request on the wire** — check
the network panel.

- [ ] **Step 5: Confirm autocomplete now works on Events and the issue detail**

Navigate to Events, type `lev`, confirm suggestions appear (they did not
before). Open any issue, scroll to Occurrences, type in its box, confirm the
same.

- [ ] **Step 6: Screenshot both themes**

Capture the Issues search box with its dropdown open in **dark and light**.
Confirm text is legible and the dropdown background is opaque in both. The
component uses only CSS variables, so a failure here means a variable is wrong,
not that a theme was forgotten.

- [ ] **Step 7: Confirm real tag keys are served**

With an app that has emitted tags, type `@tag.` and confirm the offered keys are
the app's own, not `environment`/`release`. Check the network panel: a second
schema request within five minutes should be served from the Redis cache — the
API log shows no repeat sampler query.

- [ ] **Step 8: Report**

Write what was verified and what was not. Any step that could not run is
reported as not run, never inferred from the steps around it.

---

## Self-review

**Spec coverage.** A→Task 2; B→Task 3 (Steps 2–3); C→Task 1 (`placeholderFor`) +
Task 3 Step 4; D→Tasks 4–5; E→Task 1; F→Task 2 (Steps 1–2) + Task 3 Step 1;
G→Task 6; H→Task 7; I→Task 8. Testing section→Tasks 1–8 test steps plus Task 9.
Error-handling table→Task 2 (schema fetch failure), Task 6 (parse/400/403),
Task 5 (`load_tag_keys` never fails).

**Type consistency.** `Suggestion` is defined in Task 1 and consumed by name in
Task 2. `build_schema_response`'s third parameter is `Vec<TagInfo>` in Task 5,
matching the `TagInfo` already declared in `search.rs`. `TagSource` /
`TagKeySample` / `sample_tag_keys` are produced in Task 4 and consumed by the
same names in Task 5. `ClampInfo` in Task 7 is the existing type from
`lib/api/search.ts`, not a new one. `disclosuresFor` is the same name in the
component and its test.

**Known soft spots**, flagged rather than papered over:
- Task 6 Step 6 and Task 7 Step 6 say "use the local names already in that
  file" for Events/IssueDetail state. Those files are large and their view
  state is hand-rolled; the exact identifiers must be grepped at
  implementation time rather than guessed here.
- `available_labels` becomes an empty vec in Task 5. That is a deliberate
  narrowing — the fixture was false — but it means `@$label` autocomplete
  offers nothing until a labels sampler exists. Out of scope, recorded here.
