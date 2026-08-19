<script lang="ts">
  import Badge from './ui/Badge.svelte';
  import { t, type MessageKey } from '../i18n';
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
  const label = $derived.by(() => {
    const key = `level.${String(level).toLowerCase()}` as MessageKey;
    const text = t(key);
    return text === key ? String(level) : text;
  });
</script>

<Badge {tone} {size} dot>{label}</Badge>
