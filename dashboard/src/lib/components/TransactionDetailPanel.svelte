<script lang="ts">
  import { t } from '../i18n';
  /**
   * Everything stored about one span: its fields, its tags, its extra blob.
   *
   * Rendered by the Transactions list's expanded row and by the Performance
   * drill-down modal. It is one component rather than two copies because the
   * decision about WHICH fields a span shows (`models/transaction-detail.ts`)
   * is a disclosure decision, and a second copy is how one surface starts
   * showing a column nobody cleared.
   *
   * **When this sits inside a `DataTable`, the containing `<td>` must carry
   * `class="wrap"`.** `DataTable` sets `white-space: nowrap` on every cell, and
   * nowrap suppresses line breaking outright — `overflow-wrap: anywhere` below
   * never gets a say, so a uuid paints over the next column's label and a long
   * URL runs off the panel. `DataTable` ships the seam (`td.wrap`); use it.
   */
  import Icon from './ui/Icon.svelte';
  import Badge from './ui/Badge.svelte';
  import JsonTree from './JsonTree.svelte';
  import { detailRows, isTruncated, truncatedBytesLabel } from '../models/transaction-detail';
  import type { Transaction } from '../models';

  interface Props {
    transaction: Transaction;
  }

  let { transaction }: Props = $props();
</script>

{#if isTruncated(transaction)}
  <p class="truncated" role="status">
    <Icon name="info" size={14} />
    <span>
      The SDK capped this payload at 16 KB and sent a marker instead
      ({truncatedBytesLabel(transaction)}). The span and its timing are accurate; only the
      attached data was dropped.
    </span>
  </p>
{/if}

<div class="meta-block">
  <h4>{t('ui.section.span')}</h4>
  <dl class="detail">
    {#each detailRows(transaction) as row (row.label)}
      <div class="detail-row" class:wide={row.wide}>
        <dt>{row.label}</dt>
        <dd>
          {#if row.value === null}
            <span class="muted">—</span>
          {:else if row.href}
            <a class="mono" href={row.href}>{row.value}</a>
          {:else}
            <span class:mono={row.mono}>{row.value}</span>
          {/if}
        </dd>
      </div>
    {/each}
  </dl>
</div>

{#if transaction.tags === null}
  <!--
    `null` is WITHHELD, not empty — `strip_transaction_body` nulls both for a
    caller without `event:read`. Saying so beats rendering nothing, which reads
    as "this span had no data" and sends people looking for a bug.
  -->
  <p class="withheld">
    <Icon name="lock" size={13} />
    <span>{t('prose.tx.withheld.a')} <code>event:read</code>.</span>
  </p>
{:else}
  {#if Object.keys(transaction.tags).length > 0}
    <div class="meta-block">
      <h4>{t('ui.section.tags')}</h4>
      <div class="tag-list">
        {#each Object.entries(transaction.tags) as [k, v] (k)}
          <Badge tone="neutral" size="sm">{k}: {v}</Badge>
        {/each}
      </div>
    </div>
  {/if}
  {#if transaction.extra && Object.keys(transaction.extra).length > 0}
    <div class="meta-block">
      <h4>{t('ui.section.extra')}</h4>
      <JsonTree value={transaction.extra} expandTo={1} />
    </div>
  {/if}
  {#if Object.keys(transaction.tags).length === 0 && (!transaction.extra || Object.keys(transaction.extra).length === 0)}
    <p class="muted no-meta">
      {t('prose.tx.noMeta.a')}
      <code>tags</code> {t('prose.tx.noMeta.b')} <code>extra</code>
      {t('prose.tx.noMeta.c')} <code>trackTransaction()</code>.
    </p>
  {/if}
{/if}

<style>
  .meta-block + .meta-block {
    margin-top: 14px;
  }
  .meta-block h4 {
    margin: 0 0 8px;
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
  }
  .tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  /* 373px = the 92px label + its 10px gap + 271px, the measured width of a
     uuid at this font. Below that a session id wraps mid-token, which is both
     ugly and hard to copy; above it the columns just get roomier. `auto-fill`
     keeps a maximised window from stretching 16 rows into two very tall
     columns of whitespace. */
  .detail {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(373px, 1fr));
    gap: 2px 24px;
    margin: 0;
  }
  .detail-row {
    display: grid;
    /* `minmax(0, 1fr)`, NOT `1fr`. A bare `1fr` is `minmax(auto, 1fr)`, and
       `auto`'s minimum is MIN-CONTENT — so a 200-char URL inflates the track
       to its own width instead of wrapping into the space available. The
       explicit `0` minimum is what lets the track shrink and the text wrap. */
    grid-template-columns: 92px minmax(0, 1fr);
    gap: 10px;
    align-items: baseline;
    padding: 3px 0;
    min-width: 0;
  }
  /* The full-width row, for a value with no useful upper bound. `1 / -1` spans
     however many columns `auto-fill` produced, so it stays correct at every
     viewport without a media query. */
  .detail-row.wide {
    grid-column: 1 / -1;
  }
  .detail dt {
    font-size: 12px;
    color: var(--muted);
    /* Labels are short, known, and read as a column — wrapping "Transaction id"
       onto two lines would ripple the baseline of every row beside it. */
    white-space: nowrap;
  }
  .detail dd {
    margin: 0;
    font-size: 12.5px;
    min-width: 0;
    /* Long urls, uuids and release strings have few break opportunities, so
       normal wrapping leaves them overflowing. `anywhere` breaks mid-token.
       See the header note: this is inert unless the containing `<td>` carries
       `class="wrap"`. */
    overflow-wrap: anywhere;
  }
  .withheld,
  .no-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 14px 0 0;
    font-size: 12.5px;
  }
  .withheld {
    color: var(--muted);
  }
  .truncated {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 14px;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: var(--info-soft);
    color: var(--info);
    font-size: 12.5px;
  }
</style>
