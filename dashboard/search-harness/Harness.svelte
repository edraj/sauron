<!--
  Renders the real search controls side by side in both themes.

  This exists because the defect it verifies is invisible to every static gate:
  `svelte-check` and vitest both passed with the input styled in Tailwind
  classes that do not exist in this project, an input that ignored every design
  token, and a suggestion dropdown with no background that drew transparent
  over the table beneath it.
-->
<script lang="ts">
  import SearchAutocompleteInput from '../src/lib/components/search/SearchAutocompleteInput.svelte';
  import SearchDisclosure from '../src/lib/components/search/SearchDisclosure.svelte';

  let issuesQuery = $state('');
  let sessionsQuery = $state('');
  let errorQuery = $state('levl:error');
</script>

{#snippet panel(theme: 'dark' | 'light')}
  <div class="pane" data-theme={theme}>
    <h2>{theme}</h2>

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
