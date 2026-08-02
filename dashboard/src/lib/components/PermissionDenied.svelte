<!--
  Shown in place of a page the current user has no permission for.

  Deliberately keeps the URL rather than redirecting. A bookmark that stops
  working should say so instead of silently landing somewhere else, and if the
  client-side gate is ever wrong this shows a message rather than bouncing the
  user out of a page they could actually have used. The server gates every
  endpoint regardless — this is an explanation, not a security boundary.
-->
<script lang="ts">
  import EmptyState from './ui/EmptyState.svelte';
  import Button from './ui/Button.svelte';
  import { PERMISSION_LABELS } from '../models/permissions';
  import { PAGE_ACCESS, canAccessPage, type PageAccess } from '../models/page-access';

  interface Props {
    access: PageAccess;
  }

  let { access }: Props = $props();

  const requirement = $derived(
    PERMISSION_LABELS[access.perm]
      ? `${PERMISSION_LABELS[access.perm]} (${access.perm})`
      : access.perm,
  );

  // The first page the user can actually reach, in PAGE_ACCESS declaration
  // order. '/account' and '/docs' carry a null requirement precisely so this
  // can never come up empty and strand someone on a dead end.
  const fallback = $derived(
    Object.entries(PAGE_ACCESS).find(([, entry]) => canAccessPage(entry)) ??
      (['/account', null] as [string, PageAccess | null]),
  );
  const fallbackTitle = $derived(fallback[1]?.title ?? 'Account');
</script>

<EmptyState
  icon="lock"
  title="You don't have access to {access.title}"
  description="Requires: {requirement}. Ask an organization owner for access."
>
  {#snippet action()}
    <Button variant="primary" href="#{fallback[0]}">Back to {fallbackTitle}</Button>
  {/snippet}
</EmptyState>
