<script lang="ts">
  import Badge from './ui/Badge.svelte';
  import type { IssueStatus } from '../models';

  interface Props {
    status: IssueStatus;
    size?: 'sm' | 'md';
  }

  let { status, size = 'md' }: Props = $props();

  const toneMap: Record<IssueStatus, 'warning' | 'success' | 'neutral'> = {
    unresolved: 'warning',
    resolved: 'success',
    ignored: 'neutral',
  };

  const tone = $derived(toneMap[status] ?? 'neutral');
</script>

<Badge {tone} {size}>{status}</Badge>
