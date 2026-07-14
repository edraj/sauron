<script lang="ts">
  import Badge from './ui/Badge.svelte';
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
</script>

<Badge {tone} {size} dot>{level}</Badge>
