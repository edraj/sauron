<script lang="ts">
  import { onMount } from 'svelte';
  import Header from './lib/components/Header.svelte';
  import ActionCard from './lib/components/ActionCard.svelte';
  import ActivityLog from './lib/components/ActivityLog.svelte';
  import Showcase from './lib/components/Showcase.svelte';
  import Seeding from './lib/components/Seeding.svelte';
  import { actions } from './lib/actions';
  import { initStatus } from './lib/store.svelte';
  import { connect } from './lib/sauron';

  // Auto-initialize from the persisted/default config so the demo works on load.
  onMount(() => {
    void connect();
  });

  const ready = $derived(initStatus.state === 'ready');
</script>

<main class="page">
  <Header />

  <section class="showcase-section" class:locked={!ready}>
    <div class="section-head">
      <h2>Seed the dashboard with demo data</h2>
      <p>One click fires a bulk, mixed stream of errors and events — every level, grouped &amp; one-off issues, some tagged, some with big payloads, across many users and screens.</p>
    </div>
    <Seeding disabled={!ready} />
  </section>

  <section class="showcase-section" class:locked={!ready}>
    <div class="section-head">
      <h2>Showcase funnels, journeys &amp; performance</h2>
      <p>One click seeds a realistic multi-user cohort so the analytics screens have something to show.</p>
    </div>
    <Showcase disabled={!ready} />
  </section>

  <section class="actions-section">
    <div class="section-head">
      <h2>Trigger the SDK</h2>
      <p>Each button makes a real call into <code>@sauron/browser</code>. Initialize first, then fire away.</p>
    </div>
    <div class="grid" class:locked={!ready}>
      {#each actions as action (action.id)}
        <ActionCard {action} disabled={!ready} />
      {/each}
    </div>
    {#if !ready}
      <p class="lock-hint">Actions unlock once the SDK reports <strong>Connected</strong>.</p>
    {/if}
  </section>

  <ActivityLog />

  <footer class="footer">
    <p>
      Now open the <strong>Sauron dashboard</strong> → the <strong>Web Demo</strong> app →
      <strong>Issues / Events</strong> to see these grouped and streamed in.
    </p>
    <p class="fine">
      Errors and events are batched, gzipped and delivered in the background (flushing every ~3s,
      plus on page unload via <code>sendBeacon</code>). Give it a few seconds after clicking.
    </p>
  </footer>
</main>

<style>
  .page {
    max-width: 1080px;
    margin: 0 auto;
    padding: 32px 20px 56px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }

  .section-head {
    margin-bottom: 14px;
  }
  .section-head h2 {
    font-size: 15px;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .section-head p {
    color: var(--text-muted);
    font-size: 13px;
    margin-top: 3px;
  }
  .section-head code,
  .footer code {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text);
    background: var(--surface-3);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(258px, 1fr));
    gap: 14px;
    transition: opacity 0.2s ease;
  }
  .grid.locked {
    opacity: 0.62;
  }

  .showcase-section.locked {
    opacity: 0.62;
    transition: opacity 0.2s ease;
  }

  .lock-hint {
    margin-top: 12px;
    font-size: 12.5px;
    color: var(--text-faint);
  }

  .footer {
    margin-top: 4px;
    padding: 18px 20px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-left: 3px solid var(--primary);
    border-radius: var(--radius);
    box-shadow: var(--shadow-sm);
  }
  .footer p {
    font-size: 13px;
    color: var(--text-muted);
  }
  .footer strong {
    color: var(--text);
    font-weight: 600;
  }
  .footer .fine {
    margin-top: 6px;
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
