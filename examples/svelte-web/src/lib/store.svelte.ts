/**
 * Reactive app state (Svelte 5 runes).
 *
 *  - `config`      : DSN + environment + release + distinct id, persisted to
 *                    localStorage so a paste survives reloads.
 *  - `initStatus`  : the SDK connection state, shown as a pill in the header.
 *  - `activity`    : a client-side echo log of what the demo asked the SDK to
 *                    do. The SDK itself batches + sends in the background, so
 *                    this panel is purely local feedback — not a network log.
 */

/** Default DSN — the "Web Showcase" app on the Docker Compose ingest (:8081).
 *  Edit it in the UI to point at your own app's DSN (created in the dashboard). */
export const DEFAULT_DSN =
  'http://pk_d636d908cc176ad82f37e1fdf02a1899@localhost:8081/38243955-1332-4395-a062-567dd478fd60';

const LS_KEY = 'sauron-web-demo-config';

interface PersistedConfig {
  dsn: string;
  environment: string;
  release: string;
  distinctId: string;
}

class ConfigStore {
  dsn = $state(DEFAULT_DSN);
  environment = $state('demo');
  release = $state('web-demo@0.1.0');
  distinctId = $state('user_demo_1');

  constructor() {
    try {
      const raw = localStorage.getItem(LS_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<PersistedConfig>;
        if (typeof parsed.dsn === 'string' && parsed.dsn) this.dsn = parsed.dsn;
        if (typeof parsed.environment === 'string') this.environment = parsed.environment;
        if (typeof parsed.release === 'string') this.release = parsed.release;
        if (typeof parsed.distinctId === 'string') this.distinctId = parsed.distinctId;
      }
    } catch {
      /* ignore malformed/unavailable localStorage */
    }
  }

  persist(): void {
    try {
      const snapshot: PersistedConfig = {
        dsn: this.dsn,
        environment: this.environment,
        release: this.release,
        distinctId: this.distinctId,
      };
      localStorage.setItem(LS_KEY, JSON.stringify(snapshot));
    } catch {
      /* ignore */
    }
  }

  reset(): void {
    this.dsn = DEFAULT_DSN;
    this.environment = 'demo';
    this.release = 'web-demo@0.1.0';
    this.distinctId = 'user_demo_1';
    this.persist();
  }
}

export type InitState = 'idle' | 'connecting' | 'ready' | 'error';

class InitStatusStore {
  state = $state<InitState>('idle');
  message = $state('Not initialized');

  set(state: InitState, message: string): void {
    this.state = state;
    this.message = message;
  }
}

export type LogKind = 'system' | 'error' | 'warning' | 'event' | 'identify' | 'breadcrumb';

export interface LogEntry {
  id: number;
  time: string;
  kind: LogKind;
  title: string;
  detail?: string;
}

class ActivityStore {
  entries = $state<LogEntry[]>([]);
  #nextId = 0;

  push(kind: LogKind, title: string, detail?: string): void {
    const entry: LogEntry = {
      id: this.#nextId++,
      time: new Date().toLocaleTimeString([], { hour12: false }),
      kind,
      title,
      detail,
    };
    // Newest first; keep the panel bounded.
    this.entries = [entry, ...this.entries].slice(0, 100);
  }

  clear(): void {
    this.entries = [];
  }
}

export const config = new ConfigStore();
export const initStatus = new InitStatusStore();
export const activity = new ActivityStore();
