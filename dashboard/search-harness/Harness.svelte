<!--
  Renders the real search controls side by side in both themes.

  This exists because the defect it verifies is invisible to every static gate:
  `svelte-check` and vitest both passed with the input styled in Tailwind
  classes that do not exist in this project, an input that ignored every design
  token, and a suggestion dropdown with no background that drew transparent
  over the table beneath it.

  The toolbar sections were added with `ListToolbar`: every control that sits
  on a list toolbar, rendered as the pages compose them, so the shared
  `--control-h` height and the no-shift rule (typing, an error, a chip — none
  of them may move a neighbour) can be MEASURED rather than eyeballed. The
  `data-probe` attributes are what the measurement script reads.
-->
<script lang="ts">
  import SearchAutocompleteInput from '../src/lib/components/search/SearchAutocompleteInput.svelte';
  import SearchDisclosure from '../src/lib/components/search/SearchDisclosure.svelte';
  import FilterBar from '../src/lib/components/filters/FilterBar.svelte';
  import ListToolbar from '../src/lib/components/ListToolbar.svelte';
  import SearchInput from '../src/lib/components/SearchInput.svelte';
  import TimeFilter from '../src/lib/components/TimeFilter.svelte';
  import DateRange from '../src/lib/components/DateRange.svelte';
  import RefreshButton from '../src/lib/components/ui/RefreshButton.svelte';
  import Button from '../src/lib/components/ui/Button.svelte';
  import Freshness from '../src/lib/components/ui/Freshness.svelte';
  import Icon from '../src/lib/components/ui/Icon.svelte';
  import { ISSUE_FIELDS, SESSION_FIELDS, type Filter } from '../src/lib/components/filters/filters';
  import { lastDays } from '../src/lib/models/date-range';
  import type { TimeField, TimeFilterState } from '../src/lib/models/time-filter';

  let issuesQuery = $state('');
  let sessionsQuery = $state('');
  let errorQuery = $state('levl:error');

  // Issues-style bar: chips + query box + range pills.
  let issueFilters = $state<Filter[]>([]);
  let issueSearch = $state('');
  let issueRange = $state(lastDays(30));
  // Error state on a full bar, to show the message stays under the box.
  let badFilters = $state<Filter[]>([{ field: 'level', op: 'eq', value: 'error' }]);
  let badSearch = $state('levl:error');
  let badRange = $state(lastDays(7));
  // Sessions-style bar: chips + query box + TimeFilter + status + actions.
  let sessionFilters = $state<Filter[]>([{ field: 'release', op: 'contains', value: '3.1' }]);
  let sessionSearch = $state('');
  let sessionRange = $state(lastDays(30));
  const TIME_FIELDS: TimeField[] = [
    { key: 'started_at', label: 'Started' },
    { key: 'last_event_at', label: 'Last event' },
  ];
  let sessionWindow = $state<TimeFilterState>({ field: 'started_at', mode: 'last', lastDays: 30 });
  // Users/Devices-style bar: plain box + TimeFilter + status + actions.
  let plainSearch = $state('');
  let plainWindow = $state<TimeFilterState>({ field: 'last_seen', mode: 'between', from: '2026-09-01T00:00:00Z', to: '2026-09-20T00:00:00Z' });
  // Screens/Workflows-style bar: plain box + range pills.
  let screenSearch = $state('');
  let screenRange = $state(lastDays(7));

  const fetchedAt = Date.now() - 5 * 60_000;
</script>

{#snippet panel(theme: 'dark' | 'light')}
  <div class="pane" data-theme={theme}>
    <h2>{theme}</h2>

    <section data-probe="issues-bar">
      <h3>Issues — <code>FilterBar</code>: add filter · query box · range pills. Type, then add a chip: the first line must not move.</h3>
      <FilterBar
        fields={ISSUE_FIELDS}
        bind:filters={issueFilters}
        bind:search={issueSearch}
        bind:range={issueRange}
        appId="harness"
        context="issues"
        onSearch={() => {}}
      />
    </section>

    <section data-probe="error-bar">
      <h3>Issues with a query error — the message sits under the box, the neighbours stay put</h3>
      <FilterBar
        fields={ISSUE_FIELDS}
        bind:filters={badFilters}
        bind:search={badSearch}
        bind:range={badRange}
        appId="harness"
        context="issues"
        error="unknown field `levl` — did you mean `level`?"
        onSearch={() => {}}
      />
    </section>

    <section data-probe="sessions-bar">
      <h3>Sessions — <code>FilterBar</code> with a <code>TimeFilter</code>, status, refresh and export in <code>actions</code></h3>
      <FilterBar
        fields={SESSION_FIELDS}
        bind:filters={sessionFilters}
        bind:search={sessionSearch}
        bind:range={sessionRange}
        showRange={false}
        appId="harness"
        context="sessions"
        onSearch={() => {}}
      >
        {#snippet actions()}
          <TimeFilter fields={TIME_FIELDS} value={sessionWindow} onchange={(v) => (sessionWindow = v)} />
          <Freshness {fetchedAt} />
          <RefreshButton />
          <Button variant="secondary"><Icon name="download" size={15} />Export CSV</Button>
        {/snippet}
      </FilterBar>
    </section>

    <section data-probe="plain-bar">
      <h3>Users / Devices — <code>ListToolbar</code>: plain box · <code>TimeFilter</code> (between) · status · refresh · export</h3>
      <ListToolbar>
        {#snippet searchBox()}
          <SearchInput bind:value={plainSearch} onsearch={() => {}} placeholder="Search users by id, email…" />
        {/snippet}
        {#snippet timeWindow()}
          <TimeFilter fields={[{ key: 'last_seen', label: 'Last seen' }]} value={plainWindow} onchange={(v) => (plainWindow = v)} />
        {/snippet}
        {#snippet actions()}
          <Freshness {fetchedAt} />
          <RefreshButton />
          <Button variant="secondary"><Icon name="download" size={15} />Export CSV</Button>
        {/snippet}
      </ListToolbar>
    </section>

    <section data-probe="screens-bar">
      <h3>Screens / Workflows — <code>ListToolbar</code>: plain box · range pills · status · refresh</h3>
      <ListToolbar>
        {#snippet searchBox()}
          <SearchInput bind:value={screenSearch} onsearch={() => {}} placeholder="Search screens…" />
        {/snippet}
        {#snippet timeWindow()}
          <DateRange value={screenRange} onchange={(v) => (screenRange = v)} />
        {/snippet}
        {#snippet actions()}
          <Freshness {fetchedAt} />
          <RefreshButton />
        {/snippet}
      </ListToolbar>
    </section>

    <section>
      <h3>Issues — type <code>lev</code> then pick, to chain into values</h3>
      <SearchAutocompleteInput bind:value={issuesQuery} appId="harness" context="issues" />
      <p class="out">value: <code>{issuesQuery || '(empty)'}</code></p>
    </section>

    <section>
      <h3>Sessions — placeholder is derived, and never offers <code>@tag</code></h3>
      <SearchAutocompleteInput bind:value={sessionsQuery} appId="harness" context="sessions" />
    </section>

    <section>
      <h3>Error state</h3>
      <SearchAutocompleteInput
        bind:value={errorQuery}
        appId="harness"
        context="issues"
        error="unknown field `levl` — did you mean `level`?"
      />
    </section>

    <section>
      <h3>Disclosure</h3>
      <SearchDisclosure
        clamped={{
          field: 'last_seen',
          to: '30d',
          reason: 'unindexed predicate requires a bounded time window',
        }}
        payloadSearched={false}
      />
    </section>
  </div>
{/snippet}

<div class="grid">
  {@render panel('dark')}
  {@render panel('light')}
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    min-height: 100vh;
  }
  .pane {
    padding: 24px;
    background: var(--bg);
    color: var(--text);
    font-family: var(--font-sans);
  }
  h2 {
    margin: 0 0 16px;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-faint);
  }
  h3 {
    margin: 0 0 8px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-muted);
  }
  section {
    margin-bottom: 28px;
  }
  .out {
    margin: 6px 2px 0;
    font-size: 11.5px;
    color: var(--text-faint);
  }
  code {
    font-family: var(--font-mono);
  }
</style>
