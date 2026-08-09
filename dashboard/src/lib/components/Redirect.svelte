<script lang="ts">
  import { onMount } from 'svelte';
  import { replace } from 'svelte-spa-router';
  import { authStore } from '../stores/auth.svelte';

  interface Props {
    to?: string;
  }

  let { to }: Props = $props();

  onMount(() => {
    const target = to ?? (authStore.isAuthenticated ? '/overview' : '/login');
    // `replace`, not `push`: a redirect is a resolver, not a destination — it
    // should not leave its own entry in history. With `push`, a legacy path
    // (or `/`/`*`) left TWO entries behind it, and Back landed back on the
    // legacy path, which immediately re-redirected forward — Back became
    // impossible to escape without holding it down. AdminIndex.svelte:20-24
    // already documents and uses this same reasoning for its own resolver.
    replace(target);
  });
</script>
