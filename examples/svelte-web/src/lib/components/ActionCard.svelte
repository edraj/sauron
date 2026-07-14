<script lang="ts">
  import type { DemoAction } from '../actions';

  let { action, disabled = false }: { action: DemoAction; disabled?: boolean } = $props();
</script>

<article class="card {action.category}">
  <header class="card-head">
    <span class="dot" aria-hidden="true"></span>
    <h3>{action.title}</h3>
    <span class="tag">{action.category}</span>
  </header>
  <p class="desc">{action.description}</p>
  <button class="run" {disabled} onclick={() => action.run()}>
    {action.cta}
  </button>
</article>

<style>
  .card {
    --accent: var(--neutral);
    --accent-soft: var(--neutral-soft);
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
    transition: border-color 0.15s ease, transform 0.15s ease, box-shadow 0.15s ease;
  }

  .card:hover {
    border-color: var(--border-strong);
    transform: translateY(-2px);
    box-shadow: var(--shadow);
  }

  .card.error {
    --accent: var(--error);
    --accent-soft: var(--error-soft);
  }
  .card.warning {
    --accent: var(--warning);
    --accent-soft: var(--warning-soft);
  }
  .card.event {
    --accent: var(--info);
    --accent-soft: var(--info-soft);
  }
  .card.identify {
    --accent: var(--success);
    --accent-soft: var(--success-soft);
  }
  .card.breadcrumb {
    --accent: var(--primary);
    --accent-soft: var(--primary-soft);
  }

  .card-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 0 4px var(--accent-soft);
    flex: none;
  }

  h3 {
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -0.01em;
    flex: 1;
  }

  .tag {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--accent);
    background: var(--accent-soft);
    padding: 2px 7px;
    border-radius: var(--radius-pill);
    flex: none;
  }

  .desc {
    color: var(--text-muted);
    font-size: 12.5px;
    line-height: 1.5;
    flex: 1;
  }

  .run {
    align-self: flex-start;
    padding: 7px 14px;
    border-radius: var(--radius);
    font-size: 12.5px;
    font-weight: 600;
    color: var(--accent);
    background: var(--accent-soft);
    border: 1px solid color-mix(in srgb, var(--accent) 32%, transparent);
    transition: background 0.15s ease, filter 0.15s ease;
  }

  .run:hover:not(:disabled) {
    filter: brightness(1.12);
  }

  .run:active:not(:disabled) {
    transform: translateY(1px);
  }

  .run:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
