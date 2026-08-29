<script lang="ts">
  import { t, formatNumber } from '../i18n';
  import { cellKind, cellLabel, columnCount, rampStep, retentionRate } from '../models/retention';
  import type { Cohort, Granularity, GridMode } from '../models/retention';

  interface Props {
    cohorts: Cohort[];
    granularity: Granularity;
    /** Percentages or absolute people — owned by the PAGE, because the error
     *  split renders two of these grids and they must flip together. */
    mode: GridMode;
    /** Clicking any data cell is a shortcut for the header toggle. */
    onmodetoggle?: () => void;
  }

  let { cohorts, granularity, mode, onmodetoggle }: Props = $props();

  const cols = $derived(columnCount(cohorts));
  const columns = $derived(Array.from({ length: cols }, (_, i) => i));

  /** "N days/weeks after each user's first day" — the Day-N header tooltip. */
  function headerTitle(n: number): string {
    if (n === 0) return t('retention.day0Title');
    return granularity === 'week'
      ? t('retention.weekNTitle', { n })
      : t('retention.dayNTitle', { n });
  }
</script>

<!--
  The cohort x period matrix.

  Two rules this markup enforces, both of which are the difference between an
  honest chart and a confidently wrong one:

  1. A cell whose period has not elapsed is EMPTY, with its own visual
     treatment — not the palest step of the colour ramp, which would read as
     "almost nobody came back". `cellKind` decides; nothing here defaults a
     null to zero.
  2. Period 0 shows the cohort SIZE. It is 100% by construction, and a column
     of "100%" only pushes the informative columns off screen.

  The scroll container is on the wrapper, not the page: a wide grid scrolls
  itself rather than making the whole document scroll sideways.
-->
<div class="grid-scroll">
  <table class="retention-grid">
    <thead>
      <tr>
        <th scope="col" class="cohort-col">{t('retention.cohort')}</th>
        <th scope="col" class="size-col">{t('retention.users')}</th>
        {#each columns as n (n)}
          <th scope="col" class="period-col" title={headerTitle(n)}>
            {granularity === 'week' ? t('retention.weekN', { n }) : t('retention.dayN', { n })}
          </th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each cohorts as c (c.start)}
        <tr>
          <th scope="row" class="cohort-col">{c.start}</th>
          <td class="size-col">{formatNumber(c.size)}</td>
          {#each columns as n (n)}
            {@const users = c.periods[n] ?? null}
            {@const kind = cellKind(n, users, c.size)}
            {@const rate = retentionRate(users, c.size)}
            <!-- Clickable as a convenience; the accessible control is the
                 %/# toggle in the card header, so no per-cell tab stops. -->
            <td
              class="cell"
              class:clickable={kind !== 'empty'}
              data-period={n}
              data-empty={kind === 'empty' ? 'true' : 'false'}
              data-step={kind === 'rate' && rate !== null ? rampStep(rate) : undefined}
              title={kind === 'rate' && users !== null
                ? t('retention.cellTitle', { users: formatNumber(users), size: formatNumber(c.size) })
                : kind === 'size'
                  ? t('retention.day0Title')
                  : undefined}
              onclick={kind !== 'empty' ? onmodetoggle : undefined}
            >
              {cellLabel(kind, mode, users, c.size, formatNumber)}
            </td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<p class="legend">{t('retention.legend.periods')} {t('retention.legend.empty')}</p>

<style>
  /* The grid scrolls itself; the page body never scrolls sideways. */
  .grid-scroll {
    overflow-x: auto;
    width: 100%;
  }

  .retention-grid {
    border-collapse: collapse;
    font-size: 0.8125rem;
    white-space: nowrap;
  }

  /*
   * Logical properties throughout, because Arabic is a first-class locale and
   * this table renders under dir="rtl": `text-align: right` and `left: 0`
   * would pin the sticky label column to the WRONG edge there, leaving it
   * overlapping the numbers it labels. `end`/`inset-inline-start` flip with
   * the direction; numeric cells stay end-aligned in both scripts (Arabic
   * pins Latin digits — ar-u-nu-latn — so end alignment reads correctly).
   */
  .retention-grid th,
  .retention-grid td {
    padding: 6px 10px;
    text-align: end;
    border-bottom: 1px solid var(--border);
  }

  .retention-grid thead th {
    color: var(--muted-fg);
    font-weight: 500;
    text-align: end;
  }

  .cohort-col {
    text-align: start !important;
    position: sticky;
    inset-inline-start: 0;
    background: var(--card);
    font-weight: 500;
  }

  .size-col {
    color: var(--muted-fg);
  }

  .cell {
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  .cell.clickable {
    cursor: pointer;
  }

  /*
    An unelapsed cell is visually distinct from every ramp step — hatched and
    blank rather than tinted. Painting it as step 0 would be indistinguishable
    from a period in which nobody returned.
  */
  .cell[data-empty='true'] {
    background-image: repeating-linear-gradient(
      45deg,
      transparent,
      transparent 4px,
      var(--border) 4px,
      var(--border) 5px
    );
    opacity: 0.35;
  }

  .cell[data-step='0'] {
    background: var(--muted);
  }
  .cell[data-step='1'] {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
  }
  .cell[data-step='2'] {
    background: color-mix(in srgb, var(--primary) 28%, transparent);
  }
  .cell[data-step='3'] {
    background: color-mix(in srgb, var(--primary) 48%, transparent);
  }
  .cell[data-step='4'] {
    background: color-mix(in srgb, var(--primary) 70%, transparent);
  }

  .legend {
    margin: 10px 0 0;
    font-size: 0.75rem;
    color: var(--muted-fg);
  }
</style>
