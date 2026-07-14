<script lang="ts">
  import type { Snippet } from 'svelte';
  import EyeMark from '../EyeMark.svelte';

  interface Props {
    title: string;
    subtitle?: string;
    children: Snippet;
    footer?: Snippet;
  }

  let { title, subtitle, children, footer }: Props = $props();
</script>

<div class="auth-wrap">
  <div class="auth-card">
    <div class="brand">
      <EyeMark size={40} />
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
    box-shadow: var(--shadow-lg);
    padding: 30px 30px 34px;
    animation: fade-in 0.3s ease;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 26px;
  }
  .name {
    font-size: 18px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .heading {
    margin-bottom: 22px;
  }
  h1 {
    font-size: 21px;
    font-weight: 680;
  }
  .sub {
    color: var(--text-muted);
    margin-top: 5px;
    font-size: 13.5px;
  }
  .auth-foot {
    font-size: 13px;
    color: var(--text-muted);
  }
  :global(.auth-foot a) {
    color: var(--primary);
    font-weight: 560;
  }
  :global(.auth-foot a:hover) {
    text-decoration: underline;
  }
</style>
