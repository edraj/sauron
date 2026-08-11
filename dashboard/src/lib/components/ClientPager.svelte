<script lang="ts">
  import Pagination from './Pagination.svelte';

  /**
   * A pager for a list held complete in the browser.
   *
   * Wraps `Pagination` only to compute `hasNext` from a known total, so no
   * caller has to remember that `rows.length >= limit` is the wrong test.
   * The caller slices with `pageSlice` and passes the pre-slice total here.
   */
  interface Props {
    offset: number;
    limit: number;
    /** Rows in the WHOLE list, before slicing. */
    total: number;
    onchange: (offset: number) => void;
  }

  let { offset, limit, total, onchange }: Props = $props();

  const count = $derived(Math.max(0, Math.min(limit, total - offset)));
  const hasNext = $derived(offset + limit < total);
</script>

<Pagination {offset} {limit} {count} {hasNext} {onchange} />
