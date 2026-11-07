<script lang="ts">
  import { labelFor } from '../i18n';
  import { LEVEL_KEYS } from '../models/issue-labels';
  import type { IssueLevel } from '../models';

  interface Props {
    level: IssueLevel;
    size?: 'sm' | 'md';
  }

  let { level, size = 'md' }: Props = $props();

  const toneMap: Record<string, 'error' | 'warning' | 'info' | 'neutral' | 'fatal'> = {
    fatal: 'fatal',
    error: 'error',
    warning: 'warning',
    info: 'info',
    debug: 'neutral',
  };

  const tone = $derived(toneMap[String(level).toLowerCase()] ?? 'neutral');

  // The wire value stays the value — it is the sort key and the filter chip's
  // payload. Only the label is translated, and an unrecognised level falls
  // back to printing what the server sent rather than a blank badge.
  const label = $derived(labelFor(LEVEL_KEYS, String(level).toLowerCase()));
</script>

<!-- A coloured dot and the word, in body text colour. A filled pill per row
     turned the level column into a stripe of colour that shouted over the
     issue titles; the dot carries the severity without competing. -->
<span class="lvl {tone} {size}"><span class="dot" aria-hidden="true"></span>{label}</span>

<style>
  .lvl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    line-height: 1;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--neutral);
    flex-shrink: 0;
  }
  /* `md` sits among body text (a FactList value, 13px); `sm` is for dense
     rows and timelines, where the badge is the smaller type beside a title. */
  .md {
    font-size: var(--text-sm);
  }
  .md .dot {
    width: 8px;
    height: 8px;
  }
  .fatal .dot {
    background: var(--fatal);
  }
  .error .dot {
    background: var(--error);
  }
  .warning .dot {
    background: var(--warning);
  }
  .info .dot {
    background: var(--info);
  }
</style>
