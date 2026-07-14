/**
 * Thin wrapper around `@sauron/browser` that ties the SDK to the reactive
 * config/status stores. `connect()` (re)initializes the SDK from the current
 * config — used both on first mount and whenever the user edits the DSN and
 * clicks "Init / Reconnect".
 */
import { Sauron } from '@sauron/browser';
import { activity, config, initStatus } from './store.svelte';
import type { ShowcaseSink } from './showcase';

/** True once the SDK has an active client. */
export function isConnected(): boolean {
  return Sauron.getClient() !== null;
}

/**
 * A {@link ShowcaseSink} backed by the live `@sauron/browser` client. The
 * cohort simulator switches identities through `setUser` (not `identify`) so
 * each synthetic user's events keep their own `distinct_id` — real funnel
 * drop-off, no person-aliasing.
 */
export function sauronSink(): ShowcaseSink {
  return {
    getUser() {
      const user = Sauron.getClient()?.getScope().getUser();
      return user?.id ? { id: user.id, traits: user.traits } : null;
    },
    setUser(user) {
      Sauron.setUser(user);
    },
    track(name, properties) {
      Sauron.track(name, properties);
    },
    trackTransaction(input) {
      Sauron.trackTransaction(input);
    },
    async flush() {
      await Sauron.flush(4000);
    },
  };
}

/**
 * (Re)initialize the SDK from the current config. Idempotent-safe: if a client
 * already exists we flush + tear it down first so a brand-new DSN takes effect.
 */
export async function connect(): Promise<void> {
  const dsn = config.dsn.trim();
  if (!dsn) {
    initStatus.set('error', 'DSN is empty — paste one to connect.');
    return;
  }

  initStatus.set('connecting', 'Initializing…');

  try {
    // Tear down any previous client so a changed DSN is honored.
    if (Sauron.getClient()) {
      await Sauron.close(1500);
    }

    Sauron.init({
      dsn,
      environment: config.environment.trim() || 'demo',
      release: config.release.trim() || undefined,
      // Flush a little more eagerly than the 5s default so freshly-clicked
      // actions show up in the dashboard within a couple of seconds.
      transport: { flushIntervalMs: 3000 },
    });

    config.persist();

    const host = safeHost(dsn);
    initStatus.set('ready', `Connected · ${host}`);
    activity.push(
      'system',
      'Sauron.init()',
      `env=${config.environment.trim() || 'demo'}` +
        (config.release.trim() ? ` · release=${config.release.trim()}` : ''),
    );

    // v0.2.0 screen API — declare the initial screen right after init. This
    // emits a `$screen` view and tags every later event/error with `Home`
    // until `setScreen(...)` is called again (see the "setScreen" action).
    Sauron.setScreen('Home');
    activity.push(
      'system',
      "Sauron.setScreen('Home')",
      `$screen view emitted · getScreen() → ${Sauron.getScreen() ?? 'null'}`,
    );
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    initStatus.set('error', message);
    activity.push('system', 'Sauron.init() failed', message);
  }
}

function safeHost(dsn: string): string {
  try {
    return new URL(dsn).host;
  } catch {
    return 'ingest';
  }
}
