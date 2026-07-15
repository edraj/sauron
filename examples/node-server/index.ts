/**
 * Minimal server-side example for the @sauron/node SDK (v0.3.0).
 *
 * Demonstrates the per-request pattern the 0.3.0 surface unlocks:
 *   - withScope()      — an isolated scope per request; a user + tag set inside
 *                        it never leak into a concurrent request.
 *   - addBreadcrumb()  — a trail leading up to a deliberately-captured exception
 *                        (the scoped user, tag and crumbs attach automatically).
 *   - trackTransaction — one timed operation recorded as a performance item.
 *   - flush() / close()— drain buffered items and stop the timer before exit.
 *
 * The DSN comes from SAURON_DSN. With it unset the client is never initialized,
 * every dispatch call is a no-op, and the process still exits 0 (disabled mode).
 *
 *   SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
 */
import {
  init,
  identify,
  track,
  captureException,
  trackTransaction,
  addBreadcrumb,
  withScope,
  setUser,
  setTag,
  flush,
  close,
} from '@sauron/node';

const dsn = process.env.SAURON_DSN;
const distinctId = 'user-42';

/** Simulate handling one HTTP request under its own isolated scope. */
function handleCheckout(): void {
  // withScope layers an isolated child scope for the life of this callback —
  // a concurrent request never observes this user/tag/breadcrumbs.
  withScope(() => {
    // 1. Attribute everything in this scope to a user + a tag.
    setUser({ id: distinctId, email: 'ada@example.com' });
    setTag('route', 'POST /checkout');

    // 2. Leave breadcrumbs on the path to the failure.
    addBreadcrumb({ category: 'auth', message: 'user authenticated', level: 'info' });
    addBreadcrumb({
      category: 'payment',
      message: 'charging card',
      level: 'info',
      data: { amount: 42.5, currency: 'USD' },
    });

    // 3. Capture a deliberately-thrown exception. The scoped user, tag and the
    //    breadcrumbs above are attached to the error item automatically.
    try {
      throw new Error('checkout failed: payment gateway timeout');
    } catch (err) {
      captureException(err, { tags: { area: 'checkout' } });
    }
  });
}

async function main(): Promise<void> {
  if (dsn) {
    // Initialize the global client. Throws a typed DsnError on a bad DSN.
    init({
      dsn,
      environment: process.env.NODE_ENV ?? 'development',
      release: process.env.SAURON_RELEASE ?? '1.0.0',
    });
  } else {
    console.log('SAURON_DSN unset — running in disabled mode (every call is a no-op).');
  }

  // Product analytics: associate traits, then track an event.
  identify(distinctId, { plan: 'pro', email: 'ada@example.com' });
  track('order_completed', distinctId, { total: 42.5, currency: 'USD' });

  // Time the request and record it as a performance transaction.
  const startedAt = Date.now();
  handleCheckout();
  trackTransaction({
    name: 'POST /checkout',
    op: 'http',
    http_method: 'POST',
    http_status: 500,
    duration_ms: Date.now() - startedAt,
    distinct_id: distinctId,
  });

  // Flush buffered items now (optional — close() flushes too), then stop the
  // background timer before the process exits.
  await flush();
  await close();

  console.log('Done: scope + breadcrumbs + exception + transaction; flushed and closed.');
}

main().catch((err: unknown) => {
  console.error('example failed:', err);
  process.exit(1);
});
