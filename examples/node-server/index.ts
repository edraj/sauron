/**
 * Minimal server-side example for the @sauron/node SDK.
 *
 * Shows the full lifecycle: init from a DSN, identify a user, track an
 * analytics event, capture an exception in a try/catch, then flush + close on
 * shutdown. Reads the DSN from the SAURON_DSN environment variable.
 *
 *   SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
 */
import {
  init,
  identify,
  track,
  captureException,
  captureMessage,
  flush,
  close,
} from '@sauron/node';

const dsn = process.env.SAURON_DSN;
if (!dsn) {
  console.error(
    'Set SAURON_DSN, e.g. SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start',
  );
  process.exit(1);
}

async function main(): Promise<void> {
  // 1. Initialize the global client. Throws a typed DsnError on a bad DSN.
  init({
    dsn: dsn!,
    environment: process.env.NODE_ENV ?? 'development',
    release: process.env.SAURON_RELEASE ?? '1.0.0',
  });

  // 2. Associate traits with a user.
  const distinctId = 'user-42';
  identify(distinctId, { plan: 'pro', email: 'ada@example.com' });

  // 3. Track a product-analytics event (distinctId is required).
  track('order_completed', distinctId, { total: 42.5, currency: 'USD' });

  // 4. Capture an exception from real work.
  try {
    throw new Error('checkout failed: payment gateway timeout');
  } catch (err) {
    captureException(err, {
      user: { id: distinctId },
      tags: { area: 'checkout' },
    });
  }

  captureMessage('nightly job finished', 'info');

  // 5. Flush buffered items now (optional — close() also flushes).
  await flush();

  // 6. On shutdown: flush any remainder and stop the background timer.
  await close();

  console.log('Sent identify + track + exception; flushed and closed.');
}

main().catch((err: unknown) => {
  console.error('example failed:', err);
  process.exit(1);
});
