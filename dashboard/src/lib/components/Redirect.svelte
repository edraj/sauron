<script lang="ts">
  import { onMount } from 'svelte';
  import { push } from 'svelte-spa-router';
  import { authStore } from '../stores/auth.svelte';

  interface Props {
    to?: string;
  }

  let { to }: Props = $props();

  onMount(() => {
    const target = to ?? (authStore.isAuthenticated ? '/overview' : '/login');
    push(target);
  });
</script>
