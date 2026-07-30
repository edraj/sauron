/**
 * Minimal server-side example for the @edraj/sauron-node SDK (v0.3.0).
 *
 * Demonstrates the per-request pattern the 0.3.0 surface unlocks:
 *   - withScope()      — an isolated scope per request; a user + tag set inside
 *                        it never leak into a concurrent request.
 *   - addBreadcrumb()  — a trail leading up to a deliberately-captured exception
 *                        (the scoped user, tag and crumbs attach automatically).
 *   - trackTransaction — one timed operation recorded as a performance item.
 *   - startWorkflow() / endWorkflow() / cancelWorkflow() — bound a named span
 *                        of a request handler; every event/error/transaction
 *                        captured while it is active is stamped with it. The
 *                        active workflow is request-scoped (AsyncLocalStorage,
 *                        same mechanism as the scope above), so two concurrent
 *                        checkouts never see each other's workflow.
 *   - flush() / close()— drain buffered items and stop the timer before exit.
 *
 * The DSN comes from SAURON_DSN. With it unset the client is never initialized,
 * every dispatch call is a no-op, and the process still exits 0 (disabled mode).
 *
 *   SAURON_DSN="https://<public_key>@<host>/<environment_id>" npm start
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
  startWorkflow,
  endWorkflow,
  cancelWorkflow,
  flush,
  close,
} from '@edraj/sauron-node';

const dsn = process.env.SAURON_DSN;
const distinctId = 'user-42';

/**
 * Simulate handling one HTTP request under its own isolated scope, with the
 * whole handler bounded by a "checkout" workflow: the events tracked and the
 * exception captured inside it are all stamped with the same workflow id, so
 * the dashboard can group them as one span even though the payment failure
 * itself is handled (not fatal to the request).
 */
function handleCheckout(): void {
  // withScope layers an isolated child scope for the life of this callback —
  // a concurrent request never observes this user/tag/breadcrumbs/workflow.
  withScope(() => {
    // 1. Attribute everything in this scope to a user + a tag.
    setUser({ id: distinctId, email: 'ada@example.com' });
    setTag('route', 'POST /checkout');

    // 2. Bound the span. Every track()/captureException() below, until
    //    endWorkflow(), is stamped with this workflow's id and name.
    startWorkflow('checkout');

    // 3. Leave breadcrumbs on the path to the failure, and track progress as
    //    ordinary analytics events — both land inside the workflow.
    addBreadcrumb({ category: 'auth', message: 'user authenticated', level: 'info' });
    track('checkout_started', distinctId, { total: 42.5, currency: 'USD' });
    addBreadcrumb({
      category: 'payment',
      message: 'charging card',
      level: 'info',
      data: { amount: 42.5, currency: 'USD' },
    });

    // 4. Capture a deliberately-thrown exception. The scoped user, tag and the
    //    breadcrumbs above are attached to the error item automatically, and
    //    (per step 2) so is the active workflow.
    try {
      throw new Error('checkout failed: payment gateway timeout');
    } catch (err) {
      captureException(err, { tags: { area: 'checkout' } });
    }

    // 5. The gateway timeout was handled (retried at the payment provider),
    //    so the checkout flow itself still completes — end the workflow
    //    rather than cancel it. endWorkflow() stamps $workflow_end with
    //    duration_ms and clears it; nothing captured after this point carries
    //    the workflow any more.
    track('checkout_completed', distinctId, { total: 42.5, currency: 'USD' });
    endWorkflow('checkout');
  });
}

/**
 * A second request, run under its own scope, demonstrating the cancel path:
 * a validation failure means this checkout can never complete, so the
 * workflow is explicitly cancelled (with a reason) instead of ended.
 */
function handleCheckoutValidationFailure(): void {
  withScope(() => {
    setUser({ id: distinctId, email: 'ada@example.com' });
    setTag('route', 'POST /checkout');
    startWorkflow('checkout');

    track('checkout_started', distinctId, { total: -5, currency: 'USD' });
    try {
      throw new Error('checkout failed: negative order total');
    } catch (err) {
      captureException(err, { tags: { area: 'checkout', validation: 'total' } });
    }

    // This checkout can't be retried into success — cancel rather than end.
    // cancelWorkflow stamps $workflow_cancel with duration_ms and the given
    // reason (trimmed/capped at 120 chars; defaults to 'user' if omitted).
    cancelWorkflow('checkout', { reason: 'invalid_order_total' });
  });
}

async function main(): Promise<void> {
  if (dsn) {
    // Initialize the global client. Throws a typed DsnError on a bad DSN.
    init({
      dsn,
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

  // A second, independent request — its own scope and its own workflow,
  // ended via the cancel path instead.
  handleCheckoutValidationFailure();

  // Flush buffered items now (optional — close() flushes too), then stop the
  // background timer before the process exits.
  await flush();
  await close();

  console.log(
    'Done: scope + breadcrumbs + exception + transaction + workflow (end and cancel); flushed and closed.',
  );
}

main().catch((err: unknown) => {
  console.error('example failed:', err);
  process.exit(1);
});
