<script lang="ts">
  import type { Snippet } from 'svelte';
  import EyeMark from '../EyeMark.svelte';

  interface Props {
    title: string;
    subtitle?: string;
    /** `sm` (400px) for sign-in forms; `md` (560px) for onboarding. */
    width?: 'sm' | 'md';
    children: Snippet;
    footer?: Snippet;
  }

  let { title, subtitle, width = 'sm', children, footer }: Props = $props();
</script>

<div class="auth-wrap">
  <div class="auth-card {width}">
    <div class="brand">
      <EyeMark size={32} />
      <span class="name">Sauron</span>
    </div>
    <div class="heading">
      <h1>{title}</h1>
      {#if subtitle}<p class="sub">{subtitle}</p>{/if}
    </div>
    {@render children()}
  </div>
  {#if footer}
    <div class="auth-foot">{@render footer()}</div>
  {/if}
</div>

<style>
  .auth-wrap {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 18px;
    padding: 32px 20px;
  }
  .auth-card {
    width: 100%;
    max-width: 400px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 32px;
    animation: fade-in 0.16s ease;
  }
  .auth-card.md {
    max-width: 560px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 24px;
  }
  .name {
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
  }
  .heading {
    margin-bottom: 22px;
  }
  h1 {
    font-size: var(--text-lg);
    font-weight: var(--weight-strong);
  }
  .sub {
    color: var(--text-muted);
    margin-top: 5px;
    font-size: var(--text-sm);
  }
  .auth-foot {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  /* One link style for every auth screen: in the footer and inside the card
     (a "back to sign in" in a panel used to render as plain inherited text). */
  :global(.auth-foot a),
  .auth-card :global(a:not(.btn)) {
    color: var(--primary);
    font-weight: var(--weight-medium);
  }
  :global(.auth-foot a:hover),
  .auth-card :global(a:not(.btn):hover) {
    text-decoration: underline;
  }
</style>
