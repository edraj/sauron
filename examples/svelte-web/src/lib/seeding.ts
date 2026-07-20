/**
 * Seeding engine — the "Seed demo data" button.
 *
 * Where the cohort {@link import('./showcase') showcase} lights up Funnels /
 * Journeys / Performance, seeding fills the **observability + analytics** side:
 * it drives the SDK through a bulk, deliberately *mixed* stream of **errors and
 * events** so the dashboard's **Issues**, **Events**, **Users**, **Screens** and
 * **Sessions** screens have realistic, varied data to demo:
 *
 *  - **Errors** across every level (`debug…fatal`), with controlled grouping
 *    (stable fingerprints → a few high-count issues; unique fingerprints → a
 *    long tail of one-offs), some carrying **tags**, some a **big payload**
 *    (multi-KB state snapshot + a rich breadcrumb trail), some bare.
 *  - **Events** with a mix of **big** property payloads (cart, experiments,
 *    metadata) and **small** ones, plus categorical props (plan/region/…).
 *  - All attributed across many synthetic end-users and several screens.
 *
 * Like `showcase.ts` this module is **SDK-free**: it plans against an injected
 * {@link SeedingSink} so the planning/emission logic is unit-testable under
 * plain Node (see `seeding.test.ts`). The Sauron-backed sink lives in the UI
 * layer (`sauron.ts`).
 */
import type { BreadcrumbInput, Level } from '@sauron/browser';

/**
 * Deterministic PRNG (mulberry32 seeded via FNV-1a) — same seed, same run.
 * Inlined (rather than imported from `showcase.ts`) so this module has no
 * relative runtime imports and runs as-is under `node --test`.
 */
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

/** Screens a synthetic visitor moves through — stamped on their signals. */
export const SCREENS = ['Home', 'Product', 'Cart', 'Checkout', 'Search', 'Settings', 'Account'] as const;

/** Volume presets → number of synthetic visitors to simulate. */
export const PRESETS = {
  small: { label: 'Small', visitors: 12 },
  medium: { label: 'Medium', visitors: 60 },
  large: { label: 'Large', visitors: 220 },
} as const;
export type PresetKey = keyof typeof PRESETS;

export const MAX_VISITORS = 500;

/* --------------------------------------------------------------- sink shape */

/** A synthetic user as the sink sees it. */
export interface SeedUser {
  id: string;
  traits?: Record<string, unknown>;
}

/**
 * The SDK surface the seeding driver needs — injected so this module stays
 * testable. `setTags(null)` clears scope tags; `captureException` takes a real
 * `Error` plus a `{ level, fingerprint }` hint, mirroring `@sauron/browser`.
 */
export interface SeedingSink {
  setUser(user: SeedUser | null): void;
  setScreen(name: string): void;
  /** Replace the current scope tags (lifted onto subsequent errors). */
  setTags(tags: Record<string, string> | null): void;
  /** Set (replace) a named scope context block. */
  setContext(name: string, block: Record<string, unknown>): void;
  /** Set a freeform scope extra value. */
  setExtra(key: string, value: unknown): void;
  addBreadcrumb(crumb: BreadcrumbInput): void;
  clearBreadcrumbs(): void;
  captureException(
    error: Error,
    hint: {
      level: Level;
      fingerprint: string[] | null;
      contexts?: Record<string, Record<string, unknown>>;
      extra?: Record<string, unknown>;
    },
  ): void;
  captureMessage(message: string, level: Level, hint: { fingerprint: string[] | null }): void;
  track(
    name: string,
    properties?: Record<string, unknown>,
    meta?: {
      tags?: Record<string, string>;
      contexts?: Record<string, Record<string, unknown>>;
      extra?: Record<string, unknown>;
    },
  ): void;
  flush(): Promise<void>;
}

/* ----------------------------------------------------------- small PRNG util */

function pick<T>(rng: () => number, arr: readonly T[]): T {
  return arr[Math.floor(rng() * arr.length)];
}
function chance(rng: () => number, p: number): boolean {
  return rng() < p;
}
function int(rng: () => number, lo: number, hi: number): number {
  return lo + Math.floor(rng() * (hi - lo + 1));
}
/** Pick from `[value, weight]` pairs proportionally to weight. */
function weighted<T>(rng: () => number, entries: readonly (readonly [T, number])[]): T {
  const total = entries.reduce((s, [, w]) => s + w, 0);
  let r = rng() * total;
  for (const [value, w] of entries) {
    r -= w;
    if (r <= 0) return value;
  }
  return entries[entries.length - 1][0];
}
/** A short deterministic id fragment. */
function frag(rng: () => number): string {
  return Math.floor(rng() * 0xffffff).toString(36).padStart(4, '0');
}
function clamp(n: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, n));
}

/* --------------------------------------------------------- error archetypes */

/**
 * `kind: 'exception'` → captured via `captureException(new Error)` (real type +
 * stack); `kind: 'message'` → captured via `captureMessage` (no stack).
 * `unique: true` → a per-occurrence fingerprint, so each occurrence becomes its
 * own issue (the long tail). Otherwise a stable fingerprint groups them.
 */
interface ErrorArchetype {
  key: string;
  kind: 'exception' | 'message';
  name: string;
  message: string | ((rng: () => number) => string);
  level: Level;
  weight: number;
  unique?: boolean;
  /** Fraction of occurrences that carry a big state-snapshot payload. */
  bigChance?: number;
}

const ERROR_ARCHETYPES: readonly ErrorArchetype[] = [
  { key: 'type-undefined', kind: 'exception', name: 'TypeError', message: "Cannot read properties of undefined (reading 'items')", level: 'error', weight: 30, bigChance: 0.35 },
  { key: 'fetch-failed', kind: 'exception', name: 'NetworkError', message: 'Failed to fetch /api/checkout', level: 'error', weight: 22, bigChance: 0.2 },
  { key: 'chunk-load', kind: 'exception', name: 'ChunkLoadError', message: (r) => `Loading chunk ${int(r, 10, 90)} failed`, level: 'error', weight: 14 },
  { key: 'payment-declined', kind: 'exception', name: 'PaymentDeclinedError', message: 'Card declined: insufficient_funds', level: 'error', weight: 10, bigChance: 0.6 },
  { key: 'timeout', kind: 'exception', name: 'TimeoutError', message: 'Request timed out after 30000ms', level: 'warning', weight: 12 },
  { key: 'validation', kind: 'exception', name: 'ValidationError', message: 'email is required', level: 'warning', weight: 9 },
  { key: 'reference', kind: 'exception', name: 'ReferenceError', message: 'analytics is not defined', level: 'error', weight: 7 },
  { key: 'stack-overflow', kind: 'exception', name: 'RangeError', message: 'Maximum call stack size exceeded', level: 'fatal', weight: 4 },
  { key: 'quota', kind: 'exception', name: 'QuotaExceededError', message: 'localStorage quota exceeded', level: 'warning', weight: 5 },
  { key: 'cors', kind: 'exception', name: 'SecurityError', message: 'Blocked a frame with origin from accessing a cross-origin frame', level: 'error', weight: 5 },
  { key: 'render-crash', kind: 'exception', name: 'RenderingError', message: 'Rendering component tree failed (out of memory)', level: 'fatal', weight: 3, bigChance: 0.8 },
  // Unique per occurrence → long tail of singleton issues.
  { key: 'unhandled-rejection', kind: 'exception', name: 'UnhandledRejection', message: (r) => `Promise rejected in module ${pick(r, ['cart', 'auth', 'search', 'media', 'billing'])}#${frag(r)}`, level: 'error', weight: 8, unique: true },
  // Message-style (no stack) across info/debug/warning.
  { key: 'ws-retry', kind: 'message', name: 'WebSocket', message: 'WebSocket disconnected — retrying in 5s', level: 'warning', weight: 6 },
  { key: 'slow-resource', kind: 'message', name: 'Perf', message: (r) => `Slow resource load: hero.jpg (${int(r, 1200, 4800)}ms)`, level: 'info', weight: 6 },
  { key: 'flag-fallback', kind: 'message', name: 'FeatureFlags', message: 'Flag "new_checkout" evaluated to fallback (config unavailable)', level: 'debug', weight: 5 },
  { key: 'deprecated-api', kind: 'message', name: 'Deprecation', message: 'Use of deprecated API `KeyboardEvent.keyCode`', level: 'debug', weight: 4 },
];

/* ---------------------------------------------------------------- event catalog */

interface EventArchetype {
  name: string;
  weight: number;
  /** Fraction of occurrences that carry a big property payload. */
  bigChance?: number;
  props?: (rng: () => number) => Record<string, unknown>;
}

const EVENT_ARCHETYPES: readonly EventArchetype[] = [
  { name: 'page_viewed', weight: 30, props: (r) => ({ path: pick(r, ['/', '/product', '/cart', '/search', '/settings']), referrer: pick(r, ['direct', 'google', 'twitter', 'email']) }) },
  { name: 'product_viewed', weight: 20, bigChance: 0.4, props: (r) => ({ product_id: `sku_${int(r, 1, 60)}`, price: Math.round(rn(r, 9, 499) * 100) / 100 }) },
  { name: 'button_clicked', weight: 16, props: (r) => ({ label: pick(r, ['Add to cart', 'Checkout', 'Save', 'Share', 'Upgrade']) }) },
  { name: 'search_performed', weight: 12, props: (r) => ({ query: pick(r, ['wireless headphones', 'usb-c cable', 'mechanical keyboard', '4k monitor']), results: int(r, 0, 240) }) },
  { name: 'filter_applied', weight: 8, bigChance: 0.5, props: (r) => ({ filters: sampleFilters(r) }) },
  { name: 'video_played', weight: 8, props: (r) => ({ video_id: `vid_${int(r, 1, 30)}`, autoplay: chance(r, 0.3) }) },
  { name: 'video_completed', weight: 5, props: (r) => ({ video_id: `vid_${int(r, 1, 30)}`, watch_ms: int(r, 5000, 240000) }) },
  { name: 'file_uploaded', weight: 5, bigChance: 0.6, props: (r) => ({ filename: `report-${frag(r)}.pdf`, bytes: int(r, 12_000, 8_400_000), meta: sampleMetadata(r) }) },
  { name: 'settings_changed', weight: 6, props: (r) => ({ setting: pick(r, ['theme', 'notifications', 'language', 'timezone']), value: pick(r, ['on', 'off', 'dark', 'en', 'UTC']) }) },
  { name: 'notification_clicked', weight: 5, props: (r) => ({ channel: pick(r, ['push', 'email', 'in_app']) }) },
  { name: 'invite_sent', weight: 4, props: (r) => ({ method: pick(r, ['email', 'link']), count: int(r, 1, 8) }) },
  { name: 'export_generated', weight: 4, bigChance: 0.7, props: (r) => ({ format: pick(r, ['csv', 'xlsx', 'json']), rows: int(r, 10, 50_000), spec: sampleMetadata(r) }) },
  { name: 'checkout_completed', weight: 6, bigChance: 0.5, props: (r) => ({ cart_value: Math.round(rn(r, 19, 640) * 100) / 100, currency: 'USD', cart: sampleCart(r) }) },
  { name: 'subscription_upgraded', weight: 3, props: (r) => ({ from: 'free', to: pick(r, ['pro', 'team', 'enterprise']) }) },
  { name: 'theme_toggled', weight: 4, props: (r) => ({ theme: pick(r, ['dark', 'light']) }) },
  { name: 'help_opened', weight: 3, props: (r) => ({ topic: pick(r, ['billing', 'shipping', 'returns', 'account']) }) },
  { name: 'comment_posted', weight: 3, bigChance: 0.4, props: (r) => ({ length: int(r, 4, 480), thread_id: `t_${int(r, 1, 200)}` }) },
  { name: 'feature_used', weight: 8, props: (r) => ({ feature: pick(r, ['bulk_edit', 'keyboard_shortcuts', 'dark_mode', 'api_token']) }) },
];

/** A right-skewed random in [min, max]. */
function rn(rng: () => number, min: number, max: number, k = 1.8): number {
  return min + (max - min) * Math.pow(rng(), k);
}

/* --------------------------------------------------- big-payload generators */

/** ~2–6 KB "state snapshot" attached to big-payload errors as a tag value. */
function bigStateSnapshot(rng: () => number): string {
  const snapshot = {
    route: pick(rng, ['/checkout', '/cart', '/product/42']),
    feature_flags: Object.fromEntries(Array.from({ length: int(rng, 12, 22) }, (_, i) => [`flag_${i}`, chance(rng, 0.5)])),
    experiments: Object.fromEntries(Array.from({ length: int(rng, 6, 12) }, (_, i) => [`exp_${i}`, pick(rng, ['control', 'variant_a', 'variant_b'])])),
    cart: sampleCart(rng),
    last_request: {
      url: '/api/checkout',
      method: 'POST',
      status: pick(rng, [500, 502, 504, 400]),
      body: 'x'.repeat(int(rng, 400, 1400)),
      headers: { 'x-request-id': frag(rng) + frag(rng), 'content-type': 'application/json' },
    },
    breadcrumbs_hint: 'see attached trail',
  };
  return JSON.stringify(snapshot);
}

function sampleCart(rng: () => number): Array<Record<string, unknown>> {
  return Array.from({ length: int(rng, 3, 18) }, () => ({
    sku: `sku_${int(rng, 1, 999)}`,
    name: pick(rng, ['Wireless Headphones', 'USB-C Cable', 'Mechanical Keyboard', '4K Monitor', 'Laptop Stand', 'Webcam']),
    qty: int(rng, 1, 5),
    price: Math.round(rn(rng, 9, 499) * 100) / 100,
    attributes: { color: pick(rng, ['black', 'white', 'silver']), warranty: chance(rng, 0.4) },
  }));
}

function sampleFilters(rng: () => number): Record<string, unknown> {
  return {
    category: pick(rng, ['audio', 'accessories', 'displays', 'input']),
    price_range: [int(rng, 0, 100), int(rng, 100, 900)],
    brands: Array.from({ length: int(rng, 1, 6) }, () => pick(rng, ['acme', 'globex', 'initech', 'umbrella', 'stark'])),
    in_stock: chance(rng, 0.7),
    sort: pick(rng, ['relevance', 'price_asc', 'price_desc', 'newest']),
  };
}

function sampleMetadata(rng: () => number): Record<string, unknown> {
  return {
    generated_at: '2026-07-20T00:00:00Z',
    columns: Array.from({ length: int(rng, 6, 20) }, (_, i) => `col_${i}`),
    options: { include_headers: chance(rng, 0.8), delimiter: ',', encoding: 'utf-8' },
    notes: 'x'.repeat(int(rng, 200, 900)),
  };
}

function errorTags(rng: () => number, screen: string): Record<string, string> {
  const tags: Record<string, string> = {
    feature: pick(rng, ['checkout', 'search', 'media', 'auth', 'settings']),
    browser: pick(rng, ['chrome', 'safari', 'firefox', 'edge']),
    region: pick(rng, ['us-east', 'us-west', 'eu-central', 'ap-south']),
    screen,
  };
  if (chance(rng, 0.5)) tags.customer_tier = pick(rng, ['free', 'pro', 'enterprise']);
  return tags;
}

function categoricalProps(rng: () => number): Record<string, unknown> {
  return {
    plan: pick(rng, ['free', 'pro', 'team', 'enterprise']),
    source: pick(rng, ['web', 'ios', 'android']),
    region: pick(rng, ['us-east', 'us-west', 'eu-central', 'ap-south']),
    ab_variant: pick(rng, ['control', 'variant_a', 'variant_b']),
  };
}

/* ------------------------------------------------------------------- planning */

/** One replayable operation against the sink. */
export type SeedOp =
  | { t: 'screen'; name: string }
  | { t: 'tags'; tags: Record<string, string> | null }
  | { t: 'breadcrumb'; crumb: BreadcrumbInput }
  | { t: 'clearBreadcrumbs' }
  | { t: 'error'; kind: 'exception' | 'message'; name: string; message: string; level: Level; fingerprint: string[] | null; big: boolean; tagged: boolean; contexts?: Record<string, Record<string, unknown>>; extra?: Record<string, unknown> }
  | {
      t: 'event';
      name: string;
      properties: Record<string, unknown>;
      big: boolean;
      tags?: Record<string, string>;
      contexts?: Record<string, Record<string, unknown>>;
      extra?: Record<string, unknown>;
    };

export interface VisitorPlan {
  id: string;
  traits: Record<string, unknown>;
  ops: SeedOp[];
}

function breadcrumbTrail(rng: () => number, screen: string, count: number): BreadcrumbInput[] {
  const trail: BreadcrumbInput[] = [
    { type: 'navigation', category: 'navigation', message: `Navigated to ${screen}`, level: 'info', data: { from: pick(rng, SCREENS), to: screen } },
  ];
  for (let i = 0; i < count; i++) {
    const roll = rng();
    if (roll < 0.4) {
      trail.push({ category: 'ui.click', message: `Clicked ${pick(rng, ['Add to cart', 'Apply coupon', 'Pay now', 'Retry'])}`, level: 'info', data: { x: int(rng, 0, 1280), y: int(rng, 0, 800) } });
    } else if (roll < 0.75) {
      const status = pick(rng, [200, 200, 200, 500, 429]);
      trail.push({ type: 'http', category: 'http', message: `${pick(rng, ['GET', 'POST'])} /api/${pick(rng, ['cart', 'checkout', 'products'])} → ${status}`, level: status >= 500 ? 'error' : 'info', data: { status, duration_ms: int(rng, 20, 2400) } });
    } else {
      trail.push({ category: 'console', message: pick(rng, ['warn: retrying request', 'debug: cache miss', 'info: state hydrated']), level: 'debug', data: null });
    }
  }
  return trail;
}

/** Plan one synthetic visitor's session — a mix of events and errors. Pure. */
export function planVisitor(rng: () => number, index: number, runId: string): VisitorPlan {
  const id = `seed_${runId}_${index}`;
  const traits = { cohort: runId, plan: pick(rng, ['free', 'pro', 'team', 'enterprise']) };
  const ops: SeedOp[] = [];

  const eventCount = int(rng, 3, 14);
  const errorCount = weighted(rng, [[0, 3], [1, 5], [2, 4], [3, 2], [4, 1]]);
  let screen = 'Home';
  ops.push({ t: 'screen', name: screen });

  // Interleave events and errors across a few screen changes.
  const totalSteps = eventCount + errorCount;
  let eventsLeft = eventCount;
  let errorsLeft = errorCount;

  for (let s = 0; s < totalSteps; s++) {
    if (chance(rng, 0.25)) {
      screen = pick(rng, SCREENS);
      ops.push({ t: 'screen', name: screen });
    }

    const doError = errorsLeft > 0 && (eventsLeft === 0 || chance(rng, errorsLeft / (eventsLeft + errorsLeft)));
    if (doError) {
      errorsLeft--;
      pushErrorOps(ops, rng, screen);
    } else if (eventsLeft > 0) {
      eventsLeft--;
      const arch = weighted(rng, EVENT_ARCHETYPES.map((a) => [a, a.weight] as const));
      const big = chance(rng, arch.bigChance ?? 0);
      const properties: Record<string, unknown> = { ...(arch.props?.(rng) ?? {}), ...categoricalProps(rng) };
      if (big) properties.metadata = sampleMetadata(rng);
      // Per-call metadata scopes on the event itself — these override the
      // per-visitor scope defaults (session context / cohort extra) so seeded
      // analytics shows the override path too: a `surface` tag on ~half, and a
      // richer funnel context + value extra on big events.
      const eventTags = chance(rng, 0.5)
        ? { surface: screen, source: pick(rng, ['web', 'email', 'push']) }
        : undefined;
      const eventContexts = big
        ? { funnel: { step: pick(rng, ['view', 'add_to_cart', 'checkout', 'purchase']), variant: pick(rng, ['A', 'B']) } }
        : undefined;
      const eventExtra = big
        ? { value_cents: int(rng, 100, 50000), experiment: pick(rng, ['pricing_v2', 'onboarding_v3']) }
        : undefined;
      ops.push({ t: 'event', name: arch.name, properties, big, tags: eventTags, contexts: eventContexts, extra: eventExtra });
    }
  }

  return { id, traits, ops };
}

function pushErrorOps(ops: SeedOp[], rng: () => number, screen: string): void {
  const arch = weighted(rng, ERROR_ARCHETYPES.map((a) => [a, a.weight] as const));
  const big = chance(rng, arch.bigChance ?? 0);
  const tagged = big || chance(rng, 0.55);

  // A big-payload error gets a fresh, rich breadcrumb trail.
  if (big) {
    ops.push({ t: 'clearBreadcrumbs' });
    for (const crumb of breadcrumbTrail(rng, screen, int(rng, 6, 11))) ops.push({ t: 'breadcrumb', crumb });
  } else if (chance(rng, 0.4)) {
    for (const crumb of breadcrumbTrail(rng, screen, int(rng, 1, 3))) ops.push({ t: 'breadcrumb', crumb });
  }

  // Tags (some errors carry them, some don't). Big errors also carry the
  // multi-KB state snapshot as a tag value.
  if (tagged) {
    const tags = errorTags(rng, screen);
    if (big) tags.state_snapshot = bigStateSnapshot(rng);
    ops.push({ t: 'tags', tags });
  } else {
    ops.push({ t: 'tags', tags: null });
  }

  const message = typeof arch.message === 'function' ? arch.message(rng) : arch.message;
  const fingerprint = arch.unique ? [arch.key, frag(rng)] : [arch.key];
  const contexts = big ? { failure: { screen, code: pick(rng, ['E_TIMEOUT', 'E_5XX', 'E_OOM']) } } : undefined;
  const extra = big ? { attempt: int(rng, 1, 4), degraded: chance(rng, 0.5) } : undefined;
  ops.push({ t: 'error', kind: arch.kind, name: arch.name, message, level: arch.level, fingerprint, big, tagged, contexts, extra });
}

/* -------------------------------------------------------------------- driver */

export interface SeedOptions {
  visitors?: number;
  runId?: string;
  /** Yield to the event loop (and report progress) every N visitors. */
  yieldEvery?: number;
}

export interface SeedProgress {
  done: number;
  total: number;
  errors: number;
  events: number;
}

export interface SeedSummary {
  visitors: number;
  errors: number;
  events: number;
  /** Distinct fingerprints seen — approximates the number of dashboard issues. */
  issues: number;
  bigPayloads: number;
  taggedErrors: number;
  levels: Record<Level, number>;
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

let runCounter = 0;
function defaultRunId(): string {
  runCounter += 1;
  return `seed${runCounter}`;
}

/**
 * Drive the sink through `visitors` synthetic sessions, emitting a mixed stream
 * of errors and events. Restores the previous identity and flushes on the way
 * out, even if something throws.
 */
export async function runSeeding(
  sink: SeedingSink,
  opts: SeedOptions = {},
  onProgress?: (p: SeedProgress) => void,
): Promise<SeedSummary> {
  const total = clamp(Math.floor(opts.visitors ?? PRESETS.medium.visitors), 1, MAX_VISITORS);
  const runId = opts.runId ?? defaultRunId();
  const yieldEvery = Math.max(1, opts.yieldEvery ?? 5);
  const rng = makeRng(runId);

  const fingerprints = new Set<string>();
  const levels: Record<Level, number> = { debug: 0, info: 0, warning: 0, error: 0, fatal: 0 };
  let errors = 0;
  let events = 0;
  let bigPayloads = 0;
  let taggedErrors = 0;

  try {
    for (let i = 0; i < total; i++) {
      const plan = planVisitor(rng, i, runId);
      sink.setUser({ id: plan.id, traits: plan.traits });
      sink.setContext('session', { visitor: plan.id, plan: plan.traits.plan, cohort: runId });
      sink.setExtra('cohort', runId);

      for (const op of plan.ops) {
        switch (op.t) {
          case 'screen':
            sink.setScreen(op.name);
            break;
          case 'tags':
            sink.setTags(op.tags);
            break;
          case 'breadcrumb':
            sink.addBreadcrumb(op.crumb);
            break;
          case 'clearBreadcrumbs':
            sink.clearBreadcrumbs();
            break;
          case 'event':
            sink.track(op.name, op.properties, {
              tags: op.tags,
              contexts: op.contexts,
              extra: op.extra,
            });
            events++;
            if (op.big) bigPayloads++;
            break;
          case 'error': {
            if (op.kind === 'exception') {
              const err = new Error(op.message);
              err.name = op.name;
              sink.captureException(err, {
                level: op.level,
                fingerprint: op.fingerprint,
                contexts: op.contexts,
                extra: op.extra,
              });
            } else {
              sink.captureMessage(op.message, op.level, { fingerprint: op.fingerprint });
            }
            errors++;
            levels[op.level]++;
            if (op.big) bigPayloads++;
            if (op.tagged) taggedErrors++;
            if (op.fingerprint) fingerprints.add(op.fingerprint.join('|'));
            break;
          }
        }
      }

      const last = i === total - 1;
      if (onProgress && (i % yieldEvery === 0 || last)) {
        onProgress({ done: i + 1, total, errors, events });
      }
      if (!last && (i + 1) % yieldEvery === 0) await tick();
    }
  } finally {
    // Leave the scope clean for any subsequent manual actions.
    sink.setTags(null);
    sink.clearBreadcrumbs();
    sink.setUser(null);
    await sink.flush();
  }

  return { visitors: total, errors, events, issues: fingerprints.size, bigPayloads, taggedErrors, levels };
}
