<!--
  The one page header, for list and detail pages alike.

  Before this, ~45 pages each hand-rolled a `.head` block: four title sizes,
  the "Updated" stamp and Refresh in five different places, seven copies of a
  back button with five different wordings. The ORDER of the end cluster is the
  rule this component exists to enforce — freshness, refresh, secondary
  actions, then at most one primary action — so it takes them as separate
  snippets rather than one `actions` blob a page could reorder.

  Pages pass `<Freshness>` and `<RefreshButton>` as snippets (not props): the
  refresh-parity test scans page SOURCE for `<RefreshButton` and
  `pageRefresher`, and those stay visible in the page that owns the data.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from '../ui/Icon.svelte';

  interface Props {
    title: string;
    subtitle?: string;
    /** Detail pages: the list this page returns to, by its sidebar label. */
    back?: { label: string; href: string };
    /** One level/status badge beside the title. */
    badge?: Snippet;
    /** A line under the title: id + CopyButton, counts, first seen… */
    meta?: Snippet;
    /** Before the title block (e.g. a person's avatar). */
    leading?: Snippet;
    /** Extra classes on the <h1> (e.g. `mono` for a screen name). */
    titleClass?: string;
    freshness?: Snippet;
    refresh?: Snippet;
    actions?: Snippet;
    primary?: Snippet;
  }

  let {
    title,
    subtitle,
    back,
    badge,
    meta,
    leading,
    titleClass = '',
    freshness,
    refresh,
    actions,
    primary,
  }: Props = $props();

  const hasEnd = $derived(!!(freshness || refresh || actions || primary));
</script>

<header class="ph">
  {#if back}
    <a class="back" href={back.href}><Icon name="arrow-left" size={13} />{back.label}</a>
  {/if}
  <div class="row">
    {#if leading}<div class="leading">{@render leading()}</div>{/if}
    <div class="titles">
      <div class="title-row">
        <h1 class="title {titleClass}" {title}>{title}</h1>
        {#if badge}<span class="badge">{@render badge()}</span>{/if}
      </div>
      {#if subtitle}<p class="sub">{subtitle}</p>{/if}
      {#if meta}<div class="meta">{@render meta()}</div>{/if}
    </div>
    {#if hasEnd}
      <div class="end">
        {#if freshness}{@render freshness()}{/if}
        {#if refresh}{@render refresh()}{/if}
        {#if actions}{@render actions()}{/if}
        {#if primary}{@render primary()}{/if}
      </div>
    {/if}
  </div>
</header>

<style>
  .ph {
    margin-bottom: var(--head-gap);
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 8px;
    font-size: var(--text-xs);
    color: var(--text-muted);
    transition: color 0.12s ease;
  }
  .back:hover {
    color: var(--text);
  }
  /* Wraps on content, not on the viewport: with the sidebar and the admin rail
     beside it, the header can be narrow on a wide screen, and a title squeezed
     beside five controls broke mid-word. The title block claims at least
     ~280px; below that the controls move to their own line. */
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 12px 16px;
  }
  .leading {
    flex-shrink: 0;
    align-self: center;
  }
  .titles {
    flex: 1 1 280px;
    min-width: 0;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  /* Two lines at most: a 200-character exception title would otherwise push
     the actions off-screen or grow the header to half the viewport. The full
     text stays in `title=`. */
  .title {
    font-size: var(--text-lg);
    font-weight: var(--weight-strong);
    letter-spacing: -0.01em;
    line-height: 1.3;
    min-width: 0;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  /* Arabic is cursive: tracking pulls the joins apart (app.css removes it for
     every heading, but this scoped rule outranks that). Its finals also sit
     lower than Latin descenders, so the clamped box needs a taller line. */
  :global([lang='ar']) .title {
    letter-spacing: normal;
    line-height: 1.6;
  }
  .title.mono {
    font-family: var(--font-mono);
    font-size: var(--text-md);
  }
  .badge {
    display: inline-flex;
    flex-shrink: 0;
  }
  .sub {
    margin-top: 4px;
    max-width: 72ch;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px 10px;
    margin-top: 6px;
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .end {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  @media (max-width: 720px) {
    .end {
      flex-wrap: wrap;
      width: 100%;
    }
  }
</style>
