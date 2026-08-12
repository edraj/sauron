# Search UI/UX overhaul — design

Date: 2026-08-12
Status: approved, ready to plan

## Context

The pro search programme (`pro-search-and-saved-views`) landed its backend in
slices S1/S2a/S2b/S2c: a real query language (`sauron-query`), a per-resource
planner (`sauron-db::query_plan`), and a shared route seam
(`bins/sauron-api/src/routes/search.rs`) with an honest response envelope. That
work is sound and is not revisited here.

The **UI layer that sits on top of it was never brought up to the same
standard.** A full review of all four search surfaces (Issues, Events,
IssueDetail occurrences, Sessions) plus the schema endpoint and the in-app docs
found nine defects, listed below. This spec covers all nine.

## Findings under repair

| # | Finding | Where |
|---|---------|-------|
| A | The search input is written entirely in Tailwind utility classes; the dashboard has no Tailwind, so the input is unstyled and the dropdown has **no background** — it renders transparent over the table. Dark mode unhandled. | `lib/components/search/SearchAutocompleteInput.svelte` |
| B | Autocomplete silently dead on 2 of 4 surfaces: `FilterBar` mounted without `appId`/`context`, so `loadSchema()` returns early. | `pages/Events.svelte:545`, `pages/IssueDetail.svelte:648` |
| C | Sessions placeholder advertises `@tag=v1`, which the backend deliberately withholds for that resource — a query that always 400s. | `pages/SessionsList.svelte:163` |
| D | `available_tags` / `available_labels` are hardcoded fixtures (`environment`/`release`/`team`) returned for every app. | `routes/search.rs:608-630` |
| E | Suggestions insert bare words, not predicates: picking `level` inserts `level ` — free text, not a field. Enum `options` already on the wire are unused. | `lib/api/schema.ts:70` |
| F | The "Search" button calls `onSearch`, which no call site passes — a no-op. No clear button, no icon, no click-outside/blur dismiss, no `aria-activedescendant`, no scroll-into-view. Dropdown clips inside a hardcoded `width: 220px` box. | `SearchAutocompleteInput.svelte`, `FilterBar.svelte:128` |
| G | Query errors (precise 400s from the backend) route into the generic page error card. The input is never marked invalid. | all four pages |
| H | `clamped` is computed honestly by `resolve_window` and **read by no page**. A query narrowed 365d→30d looks like a complete result set. | all four pages |
| I | In-app docs still describe the pre-language chip/free-text model. | `pages/Docs.svelte:383-460` |

## Non-goals

- Saved views (S5 of the parent programme). Unchanged.
- The query grammar itself. Frozen as of S1; nothing here changes it.
- The `SearchInput.svelte` simple client-side filter used by five other pages.
  It is correct; it is the reference this work brings the pro input up to.
- Cold-tier (DuckDB) search.

## Architecture

Four slices, each independently shippable and verifiable.

### S1 — Rebuild the input on the house design system

`SearchAutocompleteInput.svelte` is rewritten with the same structure as
`SearchInput.svelte`, which is the house reference: a bordered shell on
`--surface-2` / `--border`, a leading search icon, an inline clear (×) button,
and a `:focus-within` border on `--primary-border`. The dropdown uses
`--surface`, `--border`, `--shadow-lg`, `--radius`.

Every Tailwind class is removed. There is no Tailwind in this project — the
absence is the defect, not a missing dependency, and adding Tailwind to style
one component would fork the styling system for the whole dashboard.

Accessibility gaps closed: `aria-activedescendant` on the input pointing at the
highlighted option, dismissal on click-outside and on blur, and
`scrollIntoView` on keyboard navigation so arrowing past the visible window
follows the highlight.

**The "Search" button is deleted.** No call site passes `onSearch`; every page
drives the query from a debounced `bind:value`. It is dead, and it is what
forces the cramped two-control layout inside FilterBar's 220px wrapper. That
wrapper is removed too — the input becomes `flex: 1; min-width: 260px`, which
is what stops long suggestions clipping.

### S2 — Grammar-aware suggestions over app-real data

**Suggestion shape.** `getAutocompleteSuggestions` stops returning `string[]`
and returns:

```ts
interface Suggestion {
  insert: string;   // exact text substituted for the current token
  label: string;    // what the row shows
  detail?: string;  // type badge / description, right-aligned
  kind: 'field' | 'value' | 'variable' | 'tagKey';
}
```

**Insertion becomes correct by construction.** Today the component substitutes
the raw suggestion and appends a space, so a field lands as a bare word that
the parser reads as free text — the user must know to type `:` themselves.
Instead:

- picking a `field` inserts `name:` (colon included, **no** trailing space) and
  immediately re-opens the dropdown on that field's values;
- picking a `value` inserts `name:value ` and closes;
- a field whose `DimensionDef.options` is populated offers those options as
  `value` suggestions — the enum lists are already on the wire and currently
  discarded;
- a field with no `options` shows its `ops` as a non-insertable hint row;
- `@tag.` chains to real keys (below), and a bare `@tag` stays offered in its
  own right, since `tag:v` means "any key" per the settled semantics.

**Placeholders are derived from the loaded schema**, not hand-written per page.
This is what permanently removes finding C: a page cannot advertise a prefix
the resource does not declare, because it no longer writes the placeholder.
Pages may still pass an override for genuinely resource-specific copy, but the
default is generated.

**Real tag keys (finding D).** A new bounded sampler in `sauron-db`:

```
SELECT DISTINCT k
FROM (SELECT tags FROM <table>
      WHERE app_id = $1 AND <window_col> > $2
      ORDER BY <window_col> DESC LIMIT 2000) s,
     LATERAL jsonb_object_keys(s.tags) k
```

…with `sample_values` drawn from the same sample (top 5 distinct values per
key). Wrapped in a Redis cache via the existing `set_ex`, keyed
`search:tagkeys:{app_id}:{resource}` with a 300s TTL, so the scan runs at most
once per five minutes per app+resource.

**The ingest-maintained catalog table is rejected.** The programme ledger
records the `issue_dimensions` rollup being turned down after measuring it at
15–25% of the per-error write path, and the user chose compute-on-read. This
sampler needs no migration and adds nothing to the write path at all.

**Stated cost of that choice:** a tag key that appears only on rows older than
the sample window will not be offered. That is acceptable — autocomplete is a
hint, not a filter. The grammar still accepts any key the user types, including
via the `tag:<key>=<value>` escape hatch for keys outside the identifier
charset, so nothing becomes unqueryable. The sampler must never be read as an
authoritative key list, and the docs (S4) say so.

### S3 — Errors and disclosure

**Inline errors.** The component gains an `error?: string | null` prop that
each page feeds from its 400. On error the shell border goes `--error` and the
message renders beneath the input in `--error` text. When the message names an
unknown field, a "did you mean `<x>`?" is appended, computed client-side by
Levenshtein distance against the loaded `dimensions[].name` (plus their
aliases) — the schema is already in hand, so this costs no request.

**Client-side pre-validation.** `parseQuery` already exists and already runs,
but only on submit — which, since the Search button is dead, means never.
Moving it onto the debounce catches unbalanced parentheses and trailing
operators before the request goes out. Unknown *fields* stay a server-side 400:
only the backend holds the catalog, and duplicating it client-side is exactly
the rot the anti-rot test exists to prevent.

**`SearchDisclosure.svelte`**, a new component rendered between the FilterBar
and the table on all four searched lists. It renders only what is true:

- `clamped` present → "Showing the last {to} — {reason}", naming the window
  actually served. This is the field `resolve_window` works hard to compute
  correctly and that no page currently reads.
- `payload_searched === false` → the line IssueDetail already carries,
  generalized so Issues and Events get it too.

`total_is_capped` is deliberately **not** included: `CursorPagination` already
renders it as a `+` on the count, and a second surface for the same fact would
be noise.

### S4 — Docs

`Docs.svelte:383-460` is rewritten against the live grammar: an operator table,
the variable prefixes, and worked examples per resource. The "what each list
searches" table is corrected — it currently describes chips and a free-text box
on surfaces that now run the query language.

**The field table is built from a fetched schema**, not a hardcoded list, so it
cannot rot the way the current one did. This is the same anti-rot principle as
`sauron-query/tests/wiki_catalog.rs`, applied to the in-app surface.

## Data flow

Unchanged on the wire. The component still owns a `string` and pages still send
it as `query=`; every change here is in how that string is composed, how the
response envelope is displayed, and how errors are surfaced. No new endpoint —
the schema route gains real data in place of its fixtures.

## Error handling

| Condition | Behaviour |
|-----------|-----------|
| Schema fetch fails | Input stays fully usable; suggestions are simply absent. A degraded autocomplete must never block typing a query. |
| Client-side parse error (parens, trailing op) | Marked inline on debounce; **no request issued**. |
| Server 400 (unknown field, bad value) | Inline message + "did you mean"; previous rows retained per the existing stale-error policy. |
| Server 403 (withheld dimension) | Inline message naming the permission that lifts it — the backend already writes this text; the UI must not paraphrase it. |
| Tag-key sampler fails or times out | Logged, treated as an empty key list. It is a hint; it must never fail a schema request. |

## Testing

- **Vitest**: grammar-aware insertion (field → `name:`), value chaining from
  `options`, keyboard navigation and dismissal, error and "did you mean"
  rendering, schema-derived placeholders.
- **Rust**: a `debug_query` test pinning the sampler's SQL shape (no database
  needed, per the `query_plan` precedent) and an `http_*` test asserting the
  schema endpoint returns app-real keys rather than the fixtures, and that a
  second call is served from cache.
- **Live**: the `sauron-api-slice3` / `dashboard-slice3` launch pair, with
  browser screenshots in **both** light and dark themes. S1 is not done on a
  green `svelte-check` — the defect it repairs is invisible to every static
  gate and only observable rendered.

Per the recorded harness trap, backend tests run with
`dangerouslyDisableSandbox` against host-network containers; a run that skips is
reported as skipped, never as green.

## Risks

- The dashboard's own `svelte-check` and vitest pass **today**, with the input
  visually broken. No existing gate covers rendered appearance, which is why
  live screenshots are part of the definition of done rather than a nicety.
- Live verification needs the API binary built and Postgres reachable at the
  address in `.claude/launch.json`. If the stack will not come up, that is
  reported as unverified rather than assumed.
