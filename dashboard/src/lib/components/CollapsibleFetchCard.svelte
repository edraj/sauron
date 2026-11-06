<!--
  A collapsible card that holds one paged list and loads NOTHING until asked.

  The screen detail page carries four of these. Fetching all four eagerly would
  put four unbounded reads on a page whose stat tiles already answer the
  "is there anything here" question, so each section stays empty until the user
  expands it and presses Fetch, and each pages independently afterwards.

  The component owns the whole section — collapse state, rows, offset, the
  request — parameterised by a `fetcher` and a `row` snippet. That is what lets
  the page reset all four by remounting them (see `ScreenDetail.svelte`): the
  state that must not leak between screens lives here, not on the page.
-->
<script lang="ts" generics="T">
  import { t } from '../i18n';
  import { localeStore } from '../i18n/locale.svelte';
  import type { Snippet } from 'svelte';
  import Card from './ui/Card.svelte';
  import CountChip from './ui/CountChip.svelte';
  import Button from './ui/Button.svelte';
  import Icon, { type IconName } from './ui/Icon.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import Pagination from './Pagination.svelte';
  import { SectionPage } from '../stores/section-page.svelte';
  import type { ListPage } from '../models/list-state';

  interface Props {
    title: string;
    icon: IconName;
    /** Rows per page. The fetcher is responsible for the `limit + 1` probe. */
    limit?: number;
    /** Shown when a fetch succeeds and returns nothing. */
    emptyNote: string;
    fetcher: (offset: number, limit: number) => Promise<ListPage<T>>;
    /**
     * Stable identity for a row, used as the `{#each}` key.
     *
     * Required, and deliberately not defaulted to the array index. `rows` is
     * replaced wholesale on every page turn while each row's expanded state
     * lives inside its own `SectionRow` instance — so under an index key Svelte
     * REUSES instance 3 across the swap and page 2's third record inherits page
     * 1's third record's open panel, showing details the user never asked for
     * under a different identity. Keying by identity destroys the instance
     * instead, which is what collapses it.
     */
    rowKey: (item: T) => string;
    row: Snippet<[T]>;
  }

  let { title, icon, limit = 25, emptyNote, fetcher, rowKey, row }: Props =
    $props();

  // All the fetch/paging state lives in `SectionPage` — including the
  // out-of-order guard and the two-offset retry rule — so it can be tested
  // without a component harness. See `section-page.svelte.ts`. This component
  // owns only what is genuinely presentational.
  const section = new SectionPage<T>();

  const call = (offset: number) => fetcher(offset, limit);

  let open = $state(false);

  function toggle() {
    open = !open;
  }

  // `aria-controls` must name a valid, unique DOM id, and `title` is human text
  // — "Recent events" contains a space, which is not valid in an id. Slugged
  // here. Uniqueness rests on the four titles on a page being distinct
  // (Events / Exceptions / Devices / Users); two cards given the same title
  // would collide, which is why this is derived from the title rather than
  // hardcoded per call site.
  const bodyId = $derived(
    `section-${title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}-body`,
  );
</script>

<Card>
  {#snippet header()}
    <button
      class="head-toggle"
      onclick={toggle}
      aria-expanded={open}
      aria-controls={bodyId}
    >
      <span class="chev" class:collapsed={!open}><Icon name="chevron-down" size={15} /></span>
      <Icon name={icon} size={15} />
      <span class="title">{title}</span>
      {#if section.loaded}
        <CountChip n={`${section.seen}${section.hasNext ? '+' : ''}`} />
      {/if}
    </button>
  {/snippet}

  {#snippet actions()}
    {#if open && section.loaded}
      <Button
        variant="ghost"
        size="sm"
        onclick={() => section.refresh(call)}
        disabled={section.loading}
      >
        <Icon name="refresh" size={13} />
      </Button>
    {/if}
  {/snippet}

  <div id={bodyId} class="body" hidden={!open}>
    {#if !section.loaded && !section.loading && !section.error}
      <div class="prompt">
        <p class="muted">{t('ui.empty.notLoaded')}</p>
        <Button variant="secondary" size="sm" onclick={() => section.load(0, call)}>
          {t('ui.fetchCard.load', { title: title.toLocaleLowerCase(localeStore.tag) })}
        </Button>
      </div>
    {:else if section.loading && !section.loaded}
      <Skeleton rows={4} />
    {:else if section.error}
      <div class="prompt">
        <p class="error-note">{section.error}</p>
        <!-- `retry`, not `refresh`: after a failed Next the page on screen is
             still the previous one, and re-fetching THAT would succeed and look
             recovered without ever reaching the page the user asked for. -->
        <Button variant="secondary" size="sm" onclick={() => section.retry(call)}>
          {t('common.retry')}
        </Button>
      </div>
    {:else if section.rows.length === 0}
      <p class="muted empty-note">{emptyNote}</p>
    {:else}
      <ul class="rows" class:stale={section.loading}>
        {#each section.rows as item (rowKey(item))}
          <li>{@render row(item)}</li>
        {/each}
      </ul>
    {/if}
  </div>

  {#snippet footer()}
    {#if open && section.loaded && (section.offset > 0 || section.hasNext)}
      <div class="pager">
        <Pagination
          offset={section.offset}
          {limit}
          count={section.rows.length}
          hasNext={section.hasNext}
          total={null}
          onchange={(next) => section.load(next, call)}
        />
      </div>
    {/if}
  {/snippet}
</Card>

<style>
  .head-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    background: none;
    border: none;
    padding: 0;
    color: var(--text);
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
    cursor: pointer;
    width: 100%;
    text-align: start;
  }
  /* Same disclosure chevron as `Card collapsible`: one glyph, rotated. */
  .chev {
    display: inline-flex;
    color: var(--text-faint);
    transition: transform 0.12s ease;
  }
  .chev.collapsed {
    transform: rotate(-90deg);
  }
  :global([dir='rtl']) .chev.collapsed {
    transform: rotate(90deg);
  }
  .title {
    white-space: nowrap;
  }
  .body {
    padding: 0;
  }
  /* `hidden` is overridden by the `display` this rule's siblings set, so it is
     restated here — without it the collapsed body still occupies the card. */
  .body[hidden] {
    display: none;
  }
  .prompt {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .muted {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .empty-note {
    font-size: var(--text-sm);
  }
  .error-note {
    color: var(--error);
    font-size: var(--text-sm);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  /* A page swap keeps the old rows visible and dims them, rather than blanking
     to a spinner — the card would otherwise jump to spinner height and back on
     every Next, moving the pager out from under the cursor. */
  .rows.stale {
    opacity: 0.5;
  }
  .rows li {
    border-bottom: 1px solid var(--border);
  }
  .rows li:last-child {
    border-bottom: none;
  }
  /* Flush with the rows above it, as the section body now is. */
  .pager {
    padding: 10px 0 0;
    border-top: 1px solid var(--border);
  }
</style>
