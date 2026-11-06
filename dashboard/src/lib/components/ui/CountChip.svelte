<!--
  A small count beside a label: nav items, tab and filter headings, section
  titles. Pages had four slightly different count pills.
-->
<script lang="ts">
  interface Props {
    n: number | string;
    tone?: 'neutral' | 'primary';
  }

  let { n, tone = 'neutral' }: Props = $props();
</script>

<span class="count {tone}">{n}</span>

<style>
  .count {
    display: inline-flex;
    align-items: center;
    height: 18px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .count.primary {
    background: var(--primary-soft);
    color: var(--primary);
  }
</style>
