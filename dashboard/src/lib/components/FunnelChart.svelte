<script lang="ts">
  import { formatNumber, t } from '../i18n';
  import type { FunnelResult } from '../models';
  import { formatPercent } from '../utils/format';

  interface Props {
    result: FunnelResult;
  }

  let { result }: Props = $props();
</script>

<div class="funnel">
  {#each result.steps as step, i (i)}
    {@const dropoff = i === 0 ? 0 : 1 - step.conv_from_prev}
    <div class="fstep">
      <div class="fhead">
        <span class="fname"><span class="fnum">{i + 1}</span>{step.name}</span>
        <span class="fstat">
          <span class="fcount">{formatNumber(step.count)}</span>
          <span class="fconv muted">{formatPercent(step.conv_from_start)}</span>
        </span>
      </div>
      <div class="ftrack">
        <div class="fbar" style="width:{Math.max(1, step.conv_from_start * 100)}%">
          <span class="fbar-label">{formatPercent(step.conv_from_start, 0)}</span>
        </div>
      </div>
      {#if i > 0 && dropoff > 0.0001}
        <div class="fdrop">
          <span class="drop-ic">↓</span>
          {t('funnels.dropOff', { pct: formatPercent(dropoff) })}
          <span class="faint"
            >· {t('funnels.continued', {
              n: formatNumber(step.count),
              total: formatNumber(result.steps[i - 1].count),
            })}</span
          >
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .funnel {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .fstep {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .fhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .fname {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    min-width: 0;
  }
  .fnum {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-strong);
    flex-shrink: 0;
  }
  .fstat {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }
  .fcount {
    font-size: var(--text-sm);
    font-weight: var(--weight-strong);
  }
  .fconv {
    font-size: var(--text-xs);
  }
  .ftrack {
    width: 100%;
    height: 30px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .fbar {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    padding-inline-end: 8px;
    /* The solid fill, not `--chart-1`: the bar carries its white percentage,
       and dark-theme `--chart-1` gives that label only 3.26:1. */
    background: var(--primary-solid);
    border-radius: var(--radius-sm);
    transition: width 0.4s ease;
    min-width: 2px;
  }
  .fbar-label {
    font-size: var(--text-xs);
    font-weight: var(--weight-strong);
    color: var(--primary-contrast);
  }
  .fdrop {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 0 10px 29px;
    font-size: var(--text-xs);
    color: var(--error);
  }
  .drop-ic {
    font-weight: var(--weight-strong);
  }
</style>
