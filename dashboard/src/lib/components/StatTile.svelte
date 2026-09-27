<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '../i18n';

  type Tone = 'neutral' | 'primary' | 'success' | 'warning' | 'error' | 'info';

  interface Props {
    label: string;
    value: string | number;
    // Optional secondary line under the value — or several, one per array
    // entry, each on its own row (the Audience tiles' "window / identified /
    // guests" triple; one run-on line was unreadable at tile width).
    sub?: string | string[];
    // Optional trend delta, e.g. "+12%" — colored by `deltaTone`.
    delta?: string;
    deltaTone?: 'up' | 'down' | 'flat';
    tone?: Tone;
    // Optional inline visual (sparkline etc.).
    visual?: Snippet;
    /**
     * Span two grid columns.
     *
     * For a label too long to sit on one line in a 150px tile. `.stat-tile` is
     * a flex COLUMN with the label above the value, so a label that wraps to
     * three lines drops that tile's value ~34px below every neighbour's — the
     * numbers stop reading as a row. Measured: "Unhandled-exception-free
     * sessions" needs 246px, a single tile gives 150 and a spanned one 312.
     */
    wide?: boolean;
    // Makes the whole tile a link target.
    href?: string;
    /**
     * The value has not arrived yet: draw a placeholder where it will sit.
     *
     * The LABEL still renders, which is the point of doing this per tile rather
     * than swapping the whole row for one skeleton — the reader sees which
     * numbers are coming, and a row whose tiles are fed by different requests
     * can fill in one tile at a time without the others jumping.
     *
     * `value` is ignored while this is set, so a caller can pass whatever it
     * has (a `0`, a `—`) without it flashing up as though it were the answer.
     */
    loading?: boolean;
  }

  let {
    label,
    value,
    sub,
    delta,
    deltaTone = 'flat',
    tone = 'neutral',
    visual,
    wide = false,
    href,
    loading = false,
  }: Props = $props();
</script>

{#snippet body()}
  <span class="st-label">{label}</span>
  {#if loading}
    <!-- Sized to the value's own line box (26px × 1.15), so the tile is the
         same height loading as loaded and the row below it does not move. -->
    <span class="st-value st-pending" aria-busy="true" aria-label={t('common.loading')}></span>
    <div class="st-foot"></div>
  {:else}
    <span class="st-value {tone}">{value}</span>
    <div class="st-foot">
      {#if delta}<span class="st-delta {deltaTone}">{delta}</span>{/if}
      {#if Array.isArray(sub)}
        <span class="st-sub st-sub-lines">{#each sub as line, i (i)}<span>{line}</span>{/each}</span>
      {:else if sub}<span class="st-sub">{sub}</span>{/if}
    </div>
  {/if}
  {#if visual}<div class="st-visual">{@render visual()}</div>{/if}
{/snippet}

{#if href}
  <a class="stat-tile interactive" class:wide {href}>{@render body()}</a>
{:else}
  <div class="stat-tile" class:wide>{@render body()}</div>
{/if}

<style>
  .stat-tile {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 14px 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    min-width: 0;
    position: relative;
    overflow: hidden;
  }
  .stat-tile.wide {
    grid-column: span 2;
  }
  /* At the narrow breakpoint the grid is already down to one or two columns,
     so spanning two would leave this tile alone on its own row. */
  @media (max-width: 700px) {
    .stat-tile.wide {
      grid-column: auto;
    }
  }
  .stat-tile.interactive {
    transition: border-color 0.13s ease, background 0.13s ease;
  }
  .stat-tile.interactive:hover {
    border-color: var(--border-strong);
    background: var(--surface-2);
  }
  .st-label {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    text-transform: uppercase;
  }
  .st-value {
    font-size: 26px;
    font-weight: 680;
    letter-spacing: -0.02em;
    line-height: 1.15;
    font-variant-numeric: tabular-nums;
  }
  /* The same shimmer as `ui/Skeleton.svelte`, at the value's size. Not that
     component itself: it is a live region, and a row of five tiles would
     announce "Loading" five times. */
  .st-pending {
    display: block;
    height: 30px;
    width: 60%;
    border-radius: 4px;
    background: linear-gradient(
      90deg,
      var(--border, #2a2a2a) 25%,
      var(--surface-2, #333) 50%,
      var(--border, #2a2a2a) 75%
    );
    background-size: 200% 100%;
    animation: st-shimmer 1.4s ease-in-out infinite;
  }
  @keyframes st-shimmer {
    0% {
      background-position: 200% 0;
    }
    100% {
      background-position: -200% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .st-pending {
      animation: none;
    }
  }
  .st-value.primary {
    color: var(--primary);
  }
  .st-value.success {
    color: var(--success);
  }
  .st-value.warning {
    color: var(--warning);
  }
  .st-value.error {
    color: var(--error);
  }
  .st-value.info {
    color: var(--info);
  }
  .st-foot {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 16px;
  }
  .st-delta {
    font-size: 12px;
    font-weight: 620;
  }
  .st-delta.up {
    color: var(--success);
  }
  .st-delta.down {
    color: var(--error);
  }
  .st-delta.flat {
    color: var(--text-faint);
  }
  .st-sub {
    font-size: 12px;
    color: var(--text-faint);
  }
  .st-sub-lines {
    display: flex;
    flex-direction: column;
    line-height: 1.35;
    font-variant-numeric: tabular-nums;
  }
  .st-visual {
    margin-top: 6px;
  }
</style>
