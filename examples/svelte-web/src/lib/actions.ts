/**
 * The demo's action catalog. Each entry maps a button in the UI to a real call
 * into `@sauron/browser`, plus a short client-side echo into the activity log.
 *
 * Note: the "throw" / "reject" actions deliberately produce *genuine* uncaught
 * failures — the SDK's global `window.onerror` / `onunhandledrejection`
 * handlers (installed by `Sauron.init`) are what capture them, exactly as they
 * would in a real app. Nothing here calls captureException for those two.
 */
import { Sauron } from '@sauron/browser';
import { activity, config } from './store.svelte';

export type ActionCategory = 'error' | 'warning' | 'event' | 'identify' | 'breadcrumb';

export interface DemoAction {
  id: string;
  title: string;
  description: string;
  category: ActionCategory;
  cta: string;
  run: () => void;
}

function randomCartValue(): number {
  return Math.round((19 + Math.random() * 480) * 100) / 100;
}

// Local toggle so repeated clicks alternate screens (setScreen de-dupes when
// the name is unchanged, so we flip between two to keep emitting views).
let currentScreen: 'Home' | 'Checkout' = 'Home';

export const actions: DemoAction[] = [
  {
    id: 'uncaught-type-error',
    title: 'Throw uncaught TypeError',
    description:
      'Throws from inside a setTimeout so the browser reports a genuine uncaught error. The SDK’s window.onerror handler captures it automatically.',
    category: 'error',
    cta: 'Throw uncaught',
    run() {
      activity.push(
        'error',
        'Scheduled an uncaught TypeError',
        'Thrown in setTimeout → captured by window.onerror (mechanism.handled = false)',
      );
      setTimeout(() => {
        // The element does not exist, so `target` is null at runtime and the
        // non-null assertion lets the property access throw a real TypeError.
        const target = document.querySelector('#sauron-nonexistent-node');
        target!.dispatchEvent(new Event('demo'));
      }, 0);
    },
  },
  {
    id: 'unhandled-rejection',
    title: 'Unhandled promise rejection',
    description:
      'Rejects a Promise with no .catch(). The SDK’s onunhandledrejection handler picks it up — a common source of "silent" production errors.',
    category: 'error',
    cta: 'Reject promise',
    run() {
      activity.push(
        'error',
        'Fired an unhandled promise rejection',
        'Promise.reject(...) with no catch → captured by onunhandledrejection',
      );
      // Intentionally not awaited / not caught.
      void Promise.reject(new Error('Payment provider timed out (demo unhandled rejection)'));
    },
  },
  {
    id: 'capture-exception',
    title: 'captureException (handled)',
    description:
      'Throws and catches a synthetic Error, then reports it via Sauron.captureException(). Carries a full stacktrace with mechanism.handled = true.',
    category: 'error',
    cta: 'captureException',
    run() {
      try {
        throw new Error('Synthetic handled error from the demo checkout flow');
      } catch (err) {
        Sauron.captureException(err);
      }
      activity.push(
        'error',
        'Sauron.captureException(err)',
        'Handled Error with a real stacktrace',
      );
    },
  },
  {
    id: 'capture-message',
    title: 'captureMessage (warning)',
    description:
      'Sends a plain string at "warning" level with Sauron.captureMessage(). Useful for noteworthy-but-non-fatal conditions.',
    category: 'warning',
    cta: 'captureMessage',
    run() {
      Sauron.captureMessage('Cache hit-rate dropped below threshold (demo warning)', 'warning');
      activity.push(
        'warning',
        'Sauron.captureMessage(msg, "warning")',
        '“Cache hit-rate dropped below threshold”',
      );
    },
  },
  {
    id: 'track-checkout',
    title: 'track: checkout_completed',
    description:
      'Records a product-analytics event with a random cart value and a couple of properties — the PostHog-style track() API.',
    category: 'event',
    cta: 'track checkout',
    run() {
      const cartValue = randomCartValue();
      const items = 1 + Math.floor(Math.random() * 4);
      Sauron.track('checkout_completed', { cart_value: cartValue, currency: 'USD', items });
      activity.push(
        'event',
        'track: checkout_completed',
        `cart_value = $${cartValue.toFixed(2)} · items = ${items}`,
      );
    },
  },
  {
    id: 'track-page-view',
    title: 'track: page_viewed',
    description:
      'Records a simple page-view event with the current path, title and referrer. The bread-and-butter of product analytics.',
    category: 'event',
    cta: 'track page view',
    run() {
      Sauron.track('page_viewed', {
        path: '/demo',
        title: document.title,
        referrer: document.referrer || 'direct',
      });
      activity.push('event', 'track: page_viewed', 'path = /demo');
    },
  },
  {
    id: 'set-screen',
    title: 'setScreen (navigate)',
    description:
      'Switches the current screen with the v0.2.0 Sauron.setScreen() API. Emits a $screen view and attributes later events/errors to it — toggles Home ⇄ Checkout on each click.',
    category: 'event',
    cta: 'Change screen',
    run() {
      currentScreen = currentScreen === 'Home' ? 'Checkout' : 'Home';
      Sauron.setScreen(currentScreen);
      activity.push(
        'event',
        `setScreen: ${currentScreen}`,
        `$screen view emitted · getScreen() → ${Sauron.getScreen() ?? 'null'}`,
      );
    },
  },
  {
    id: 'identify',
    title: 'identify',
    description:
      'Associates the session with a known user using the Distinct ID from the header, plus some example traits (plan, source).',
    category: 'identify',
    cta: 'identify user',
    run() {
      const distinctId = config.distinctId.trim() || 'user_demo_1';
      Sauron.identify(distinctId, {
        plan: 'pro',
        signup_source: 'web-demo',
        is_demo: true,
      });
      activity.push(
        'identify',
        `identify: ${distinctId}`,
        'traits: { plan: "pro", signup_source: "web-demo" }',
      );
    },
  },
  {
    id: 'breadcrumb-then-throw',
    title: 'addBreadcrumb → throw',
    description:
      'Leaves a trail of breadcrumbs, then captures an error. On the issue you’ll see the breadcrumbs that led up to the crash.',
    category: 'breadcrumb',
    cta: 'Breadcrumb + throw',
    run() {
      Sauron.addBreadcrumb({
        category: 'ui.click',
        message: 'User clicked “Complete order”',
        level: 'info',
        data: { button: 'complete-order' },
      });
      Sauron.addBreadcrumb({
        type: 'http',
        category: 'http',
        message: 'POST /api/checkout → 500',
        level: 'error',
        data: { method: 'POST', url: '/api/checkout', status: 500 },
      });
      try {
        throw new Error('Checkout failed after breadcrumb trail (demo)');
      } catch (err) {
        Sauron.captureException(err);
      }
      activity.push(
        'breadcrumb',
        'Added 2 breadcrumbs, then captured an error',
        'The error is delivered with its breadcrumb trail attached',
      );
    },
  },
];
