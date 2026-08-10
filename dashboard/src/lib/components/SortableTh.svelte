<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './ui/Icon.svelte';
  import type { SortDir, SortState } from '../models/sort';

  /**
   * A sortable column header for `DataTable`.
   *
   * Used inside a page's own `head` snippet, beside plain `<th>` elements for
   * columns that do not sort:
   *
   * ```svelte
   * <tr>
   *   <SortableTh key="title" columnDefault="asc" {sort} {onsort}>Issue</SortableTh>
   *   <th>Actions</th>
   * </tr>
   * ```
   *
   * The label goes in a real `<button>` rather than a click handler on the
   * `<th>`, so it is focusable, operable with Enter and Space, and announced as
   * a control — none of which a clickable `<th>` gets.
   *
   * `onsort` is handed the key and the default direction rather than a
   * finished `SortState`, because the page must funnel it through
   * `setCursorSort`/`setOffsetSort` — the reducers that reset paging. Handing
   * over a finished state here would let a page apply the sort and forget the
   * reset, which is the bug those reducers exist to make inexpressible.
   */
  interface Props {
    key: string;
    sort: SortState;
    onsort: (key: string, columnDefault: SortDir) => void;
    /** `desc` suits times and counts; pass `asc` for names. */
    columnDefault?: SortDir;
    class?: string;
    children: Snippet;
  }

  let {
    key,
    sort,
    onsort,
    columnDefault = 'desc',
    class: klass = '',
    children,
  }: Props = $props();

  const active = $derived(sort.key === key);
  // `aria-sort` takes the literal tokens "ascending"/"descending"/"none"; the
  // internal 'asc'/'desc' spelling is not valid there.
  const ariaSort = $derived(
    active ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none',
  );
</script>

<th class="sortable {klass}" aria-sort={ariaSort}>
  <button type="button" class="sort-btn" class:active onclick={() => onsort(key, columnDefault)}>
    {@render children()}
    <span class="caret" aria-hidden="true">
      {#if active}
        <Icon name={sort.dir === 'asc' ? 'chevron-up' : 'chevron-down'} size={12} />
      {/if}
    </span>
  </button>
</th>

<style>
  /* DataTable styles `thead th` through :global, so padding is inherited from
     there and must NOT be repeated — the button carries the click target
     instead, stretched to fill the cell. */
  .sortable {
    padding: 0 !important;
  }
  .sort-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    padding: 9px 14px;
    background: none;
    border: none;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .sort-btn:hover,
  .sort-btn.active {
    color: var(--text);
  }
  .sort-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  /* Reserve the caret's width always, so the header does not shift sideways
     when a column becomes active. */
  .caret {
    display: inline-flex;
    width: 12px;
    flex: none;
  }
  /* A right-aligned numeric column reads wrong with the label pushed left. */
  :global(th.num) .sort-btn {
    justify-content: flex-end;
  }
</style>
