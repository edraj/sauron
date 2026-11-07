<script lang="ts">
  import Badge from './ui/Badge.svelte';
  import { labelFor } from '../i18n';
  import { STATUS_KEYS } from '../models/issue-labels';
  import type { IssueStatus } from '../models';

  interface Props {
    status: IssueStatus;
    size?: 'sm' | 'md';
  }

  let { status, size = 'md' }: Props = $props();

  // Colour only where it carries meaning. "Unresolved" is the normal state of
  // almost every row in the list, so tinting it amber painted the whole column
  // as a warning; it is neutral, and "resolved" alone earns green.
  const toneMap: Record<IssueStatus, 'success' | 'neutral'> = {
    unresolved: 'neutral',
    resolved: 'success',
    ignored: 'neutral',
  };

  const tone = $derived(toneMap[status] ?? 'neutral');

  // As in `LevelBadge`: translate the label, keep the wire value.
  const label = $derived(labelFor(STATUS_KEYS, String(status).toLowerCase()));
</script>

<Badge {tone} {size}>{label}</Badge>
