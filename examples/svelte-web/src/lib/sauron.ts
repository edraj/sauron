/**
 * Thin wrapper around `@sauron/browser` that ties the SDK to the reactive
 * config/status stores. `connect()` (re)initializes the SDK from the current
 * config — used both on first mount and whenever the user edits the DSN and
 * clicks "Init / Reconnect".
 */
import { Sauron } from '@sauron/browser';
import { activity, config, initStatus } from './store.svelte';
import type { ShowcaseSink } from './showcase';
import type { SeedingSink } from './seeding';

/** True once the SDK has an active client. */
export function isConnected(): boolean {
  return Sauron.getClient() !== null;
}

/**
 * Capture one hand-crafted error that exercises the metadata scopes E2E: a
 * scope-level context/extra (via setContext/setExtra) PLUS per-call overrides on
 * the capture itself — proving the SDK merges scope ⊕ call before send. The
 * per-call `order` block replaces the same-named scope block; `feature` tag and
 * `attempt` extra are per-call-only.
 */
export function captureExampleError(): void {
  if (!isConnected()) return;
  Sauron.setContext('order', { id: 4242, total: 99.5, currency: 'USD' });
  Sauron.setExtra('cart_size', 3);
  Sauron.captureException(new Error('Checkout failed at payment step'), {
    level: 'error',
    tags: { feature: 'checkout' },
    contexts: { order: { id: 4242, step: 'payment' } },
    extra: { attempt: 2, gateway: 'stripe' },
  });
  activity.push('error', 'captureException()', 'order+extra scopes attached (scope ⊕ per-call override)');
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
 * A {@link SeedingSink} backed by the live `@sauron/browser` client. Errors and
 * events are the focus, so this sink additionally drives per-signal **tags**
 * (lifted onto errors) and the **breadcrumb** trail via the client's `Scope` —
 * exactly where the SDK reads them from at capture time.
 */
export function seedingSink(): SeedingSink {
  const scope = () => Sauron.getClient()?.getScope() ?? null;
  return {
    setUser(user) {
      Sauron.setUser(user);
    },
    setScreen(name) {
      Sauron.setScreen(name);
    },
    setTags(tags) {
      const s = scope();
      if (!s) return;
      // Replace the scope's tags wholesale so "some errors carry tags, some
      // don't" is honored per capture.
      for (const key of Object.keys(s.tags)) delete s.tags[key];
      if (tags) for (const [key, value] of Object.entries(tags)) s.setTag(key, value);
    },
    setContext(name, block) {
      scope()?.setContext(name, block);
    },
    setExtra(key, value) {
      scope()?.setExtra(key, value);
    },
    addBreadcrumb(crumb) {
      Sauron.addBreadcrumb(crumb);
    },
    clearBreadcrumbs() {
      scope()?.clearBreadcrumbs();
    },
    captureException(error, hint) {
      Sauron.captureException(error, hint);
    },
    captureMessage(message, level, hint) {
      Sauron.captureMessage(message, level, hint);
    },
    track(name, properties, meta) {
      Sauron.track(name, properties, meta);
    },
    async flush() {
      await Sauron.flush(6000);
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
      // Default metadata scopes — seeded into the global scope, lifted onto
      // every error / message / track() from here on.
      tags: { app: 'svelte-web', surface: 'demo' },
      contexts: { app: { name: 'sauron-web-demo', framework: 'svelte' } },
      extra: { initialized_at: new Date().toISOString() },
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
