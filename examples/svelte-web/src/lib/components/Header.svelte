<script lang="ts">
  import { config, initStatus } from '../store.svelte';
  import { connect } from '../sauron';

  const statusLabel: Record<string, string> = {
    idle: 'Idle',
    connecting: 'Connecting',
    ready: 'Connected',
    error: 'Error',
  };
</script>

<header class="hero">
  <div class="brand">
    <svg class="eye" viewBox="0 0 32 32" role="img" aria-label="Sauron">
      <defs>
        <radialGradient id="hf" cx="50%" cy="52%" r="62%">
          <stop offset="0%" stop-color="#fff7d6" />
          <stop offset="20%" stop-color="#ffd451" />
          <stop offset="48%" stop-color="#ff8f22" />
          <stop offset="76%" stop-color="#f14e0b" />
          <stop offset="100%" stop-color="#a81a03" />
        </radialGradient>
      </defs>
      <rect width="32" height="32" rx="8" fill="#0c0a09" />
      <path d="M2 16C8 6.5 24 6.5 30 16 24 25.5 8 25.5 2 16Z" fill="url(#hf)" />
      <path d="M16 7.2c2.4 3.8 2.4 13.8 0 17.6-2.4-3.8-2.4-13.8 0-17.6Z" fill="#0c0a09" />
    </svg>
    <div>
      <h1>Sauron — Web SDK Demo</h1>
      <p class="sub">
        Push errors &amp; product events to a live Sauron ingest gateway with
        <code>@sauron/browser</code>.
      </p>
    </div>
    <span class="status {initStatus.state}" title={initStatus.message}>
      <span class="pulse" aria-hidden="true"></span>
      {statusLabel[initStatus.state] ?? initStatus.state}
    </span>
  </div>

  <div class="config">
    <label class="field dsn">
      <span>DSN</span>
      <input
        type="text"
        spellcheck="false"
        autocomplete="off"
        bind:value={config.dsn}
        placeholder="http://pk_...@host/project-id"
      />
    </label>
    <label class="field">
      <span>Environment</span>
      <input type="text" spellcheck="false" bind:value={config.environment} placeholder="demo" />
    </label>
    <label class="field">
      <span>Release</span>
      <input
        type="text"
        spellcheck="false"
        bind:value={config.release}
        placeholder="web-demo@0.1.0"
      />
    </label>
    <label class="field">
      <span>Distinct ID</span>
      <input
        type="text"
        spellcheck="false"
        bind:value={config.distinctId}
        placeholder="user_demo_1"
      />
    </label>
    <div class="actions">
      <button class="primary" onclick={() => connect()}>
        {initStatus.state === 'ready' ? 'Reconnect' : 'Init'}
      </button>
      <button class="ghost" onclick={() => config.reset()} title="Restore the dev-default DSN">
        Reset
      </button>
    </div>
  </div>

  <p class="status-line {initStatus.state}">{initStatus.message}</p>
</header>

<style>
  .hero {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 22px;
    background: linear-gradient(180deg, var(--surface-2), var(--surface));
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  .eye {
    flex: none;
    width: 36px;
    height: 36px;
    border-radius: 8px;
    box-shadow: 0 0 22px rgba(255, 110, 30, 0.4);
  }

  h1 {
    font-size: 19px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }

  .sub {
    color: var(--text-muted);
    font-size: 13px;
    margin-top: 2px;
  }
  .sub code {
    color: var(--text);
    background: var(--surface-3);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    font-size: 12px;
  }

  .status {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
    font-weight: 600;
    padding: 5px 12px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text-muted);
    white-space: nowrap;
  }
  .status .pulse {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--neutral);
  }
  .status.ready {
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 40%, transparent);
    background: var(--success-soft);
  }
  .status.ready .pulse {
    background: var(--success);
    box-shadow: 0 0 0 4px var(--success-soft);
    animation: pulse 1.8s ease-in-out infinite;
  }
  .status.connecting {
    color: var(--warning);
    background: var(--warning-soft);
  }
  .status.connecting .pulse {
    background: var(--warning);
  }
  .status.error {
    color: var(--error);
    background: var(--error-soft);
  }
  .status.error .pulse {
    background: var(--error);
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  .config {
    display: grid;
    grid-template-columns: minmax(0, 2.4fr) repeat(3, minmax(0, 1fr)) auto;
    gap: 12px;
    align-items: end;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  .field > span {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
  }
  .field input {
    width: 100%;
    padding: 9px 11px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    font-size: 12.5px;
  }
  .field.dsn input {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .field input:focus {
    border-color: var(--primary-border);
    outline: none;
    box-shadow: 0 0 0 3px var(--primary-soft);
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  .primary {
    padding: 9px 18px;
    font-weight: 600;
    font-size: 13px;
    color: var(--primary-contrast);
    background: var(--primary);
    border-radius: var(--radius);
    white-space: nowrap;
  }
  .primary:hover {
    background: var(--primary-hover);
  }
  .primary:active {
    background: var(--primary-active);
  }

  .ghost {
    padding: 9px 14px;
    font-size: 13px;
    color: var(--text-muted);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .ghost:hover {
    color: var(--text);
    border-color: var(--border-strong);
  }

  .status-line {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .status-line.ready {
    color: var(--success);
  }
  .status-line.error {
    color: var(--error);
  }

  @media (max-width: 760px) {
    .config {
      grid-template-columns: 1fr 1fr;
    }
    .field.dsn {
      grid-column: 1 / -1;
    }
    .actions {
      grid-column: 1 / -1;
    }
    .primary {
      flex: 1;
    }
  }
</style>
