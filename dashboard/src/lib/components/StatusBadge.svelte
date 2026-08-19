<script lang="ts">
  import Badge from './ui/Badge.svelte';
  import { t, type MessageKey } from '../i18n';
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

  // As in `LevelBadge`: translate the label, keep the wire value.
  const label = $derived.by(() => {
    const key = `status.${String(status).toLowerCase()}` as MessageKey;
    const text = t(key);
    return text === key ? String(status) : text;
  });
</script>

<Badge {tone} {size}>{label}</Badge>
