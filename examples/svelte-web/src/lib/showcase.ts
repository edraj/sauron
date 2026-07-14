/**
 * Cohort simulator — the "Run showcase" engine.
 *
 * Drives the SDK through a synthetic but realistic e-commerce cohort so the
 * dashboard's **Funnels**, **Journeys** and **Performance** screens light up
 * with non-trivial data. A single demo user clicking buttons is one
 * `distinct_id` → a flat, single-path funnel; a cohort of synthetic users
 * (switched via `setUser`) produces real drop-off and a branching Sankey.
 *
 * This module is deliberately **SDK-free**: the driver takes an injected
 * {@link ShowcaseSink}, so the planning + emission logic is unit-testable under
 * plain Node (see `showcase.test.ts`). The real Sauron-backed sink is built in
 * the UI layer.
 */
import type { TransactionInput } from '@sauron/browser';

/** The ordered funnel. Reuses `checkout_completed` (the manual demo button). */
export const FUNNEL_STEPS: readonly string[] = [
  'product_viewed',
  'product_added_to_cart',
  'checkout_started',
  'payment_info_entered',
  'checkout_completed',
];

/** Cumulative retention per funnel step — the target drop-off shape. */
const RETENTION: readonly number[] = [1.0, 0.65, 0.43, 0.28, 0.2];

/** Known transaction ops (mirror of the SDK's `TransactionOp`). */
export const TRANSACTION_OPS: readonly string[] = [
  'navigation',
  'http',
  'resource',
  'screen_load',
  'custom',
];

export const DEFAULT_USERS = 120;
export const MAX_USERS = 500;

export interface PlannedEvent {
  kind: 'event';
  name: string;
  properties?: Record<string, unknown>;
}
export interface PlannedTxn {
  kind: 'txn';
  input: TransactionInput;
}
export type PlannedAction = PlannedEvent | PlannedTxn;

export interface UserPlan {
  id: string;
  /** How many funnel steps this user reached (1..FUNNEL_STEPS.length). */
  reached: number;
  actions: PlannedAction[];
}

/** A user identity as the sink sees it (subset of the SDK user). */
export interface SinkUser {
  id: string;
  traits?: Record<string, unknown>;
}

/** The SDK surface the driver needs — injected so this module stays testable. */
export interface ShowcaseSink {
  getUser(): SinkUser | null;
  setUser(user: SinkUser | null): void;
  track(name: string, properties?: Record<string, unknown>): void;
  trackTransaction(input: TransactionInput): void;
  flush(): Promise<void>;
}

export interface ShowcaseOptions {
  users?: number;
  runId?: string;
  /** Yield to the event loop (and report progress) every N users. */
  yieldEvery?: number;
}

export interface ShowcaseProgress {
  done: number;
  total: number;
  events: number;
  transactions: number;
}

export interface ShowcaseSummary {
  users: number;
  events: number;
  transactions: number;
  funnel: { name: string; count: number }[];
}

/** Deterministic PRNG (mulberry32 seeded via FNV-1a) — same seed, same cohort. */
export function makeRng(seed: string | number): () => number {
  let h = 2166136261 >>> 0;
  const s = String(seed);
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  let a = h >>> 0;
  return function next(): number {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A right-skewed latency in ms: most values low, a long tail toward `max`. */
function skewed(rng: () => number, min: number, max: number, k = 2.2): number {
  return Math.round(min + (max - min) * Math.pow(rng(), k));
}

/** Plan one synthetic user's actions. Pure — no SDK calls. */
export function planUser(rng: () => number, index: number, runId: string): UserPlan {
  // How far down the funnel this user gets, honoring the retention curve.
  let reached = 1;
  for (let i = 1; i < FUNNEL_STEPS.length; i++) {
    const cond = RETENTION[i] / RETENTION[i - 1];
    if (rng() < cond) reached++;
    else break;
  }

  const actions: PlannedAction[] = [];
  const event = (name: string, properties?: Record<string, unknown>) =>
    actions.push({ kind: 'event', name, properties });
  const txn = (input: TransactionInput) => actions.push({ kind: 'txn', input });

  // Entry: some users arrive via search.
  if (rng() < 0.4) event('search_performed', { query: 'wireless headphones' });

  // Landing: route load + product list fetch (+ sometimes a bundle resource).
  txn({ name: 'GET /products', op: 'navigation', durationMs: skewed(rng, 200, 1400) });
  txn({
    name: 'GET /api/products',
    op: 'http',
    durationMs: skewed(rng, 80, 1800),
    httpMethod: 'GET',
    httpStatus: rng() < 0.03 ? 500 : 200,
    status: null,
    url: '/api/products',
  });
  if (rng() < 0.5) {
    txn({ name: 'app.bundle.js', op: 'resource', durationMs: skewed(rng, 40, 600) });
  }

  // Step 0 — everyone views a product.
  event(FUNNEL_STEPS[0], { product_id: `sku_${index % 40}` });
  if (rng() < 0.3) event('viewed_recommendations');

  // Step 1 — add to cart.
  if (reached >= 2) event(FUNNEL_STEPS[1], { product_id: `sku_${index % 40}` });

  // Step 2 — start checkout (+ the checkout API call).
  if (reached >= 3) {
    event(FUNNEL_STEPS[2]);
    txn({
      name: 'POST /api/checkout',
      op: 'http',
      durationMs: skewed(rng, 150, 2600),
      httpMethod: 'POST',
      httpStatus: rng() < 0.04 ? 500 : 200,
      status: null,
      url: '/api/checkout',
    });
    if (rng() < 0.2) event('applied_coupon', { code: 'SAVE10' });
  }

  // Step 3 — enter payment.
  if (reached >= 4) event(FUNNEL_STEPS[3]);

  // Step 4 — order complete.
  if (reached >= 5) {
    const value = Math.round((19 + rng() * 480) * 100) / 100;
    event(FUNNEL_STEPS[4], { cart_value: value, currency: 'USD' });
  }

  return { id: `sim_${runId}_${index}`, reached, actions };
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, n));
}

let runCounter = 0;
function defaultRunId(): string {
  runCounter += 1;
  return `run${runCounter}`;
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/**
 * Drive the sink through a cohort of `users` synthetic users. Restores the
 * pre-run identity and flushes on the way out, even if something throws.
 */
export async function runShowcase(
  sink: ShowcaseSink,
  opts: ShowcaseOptions = {},
  onProgress?: (p: ShowcaseProgress) => void,
): Promise<ShowcaseSummary> {
  const total = clamp(Math.floor(opts.users ?? DEFAULT_USERS), 1, MAX_USERS);
  const runId = opts.runId ?? defaultRunId();
  const yieldEvery = Math.max(1, opts.yieldEvery ?? 12);
  const rng = makeRng(runId);

  const previousUser = sink.getUser();
  const funnel = FUNNEL_STEPS.map((name) => ({ name, count: 0 }));
  let events = 0;
  let transactions = 0;

  try {
    for (let i = 0; i < total; i++) {
      const plan = planUser(rng, i, runId);
      sink.setUser({ id: plan.id, traits: { plan: 'sim', cohort: runId } });

      for (const action of plan.actions) {
        if (action.kind === 'event') {
          sink.track(action.name, action.properties);
          events++;
          const step = FUNNEL_STEPS.indexOf(action.name);
          if (step >= 0) funnel[step].count++;
        } else {
          sink.trackTransaction(action.input);
          transactions++;
        }
      }

      const last = i === total - 1;
      if (onProgress && (i % yieldEvery === 0 || last)) {
        onProgress({ done: i + 1, total, events, transactions });
      }
      if (!last && (i + 1) % yieldEvery === 0) await tick();
    }
  } finally {
    sink.setUser(previousUser);
    await sink.flush();
  }

  return { users: total, events, transactions, funnel };
}
