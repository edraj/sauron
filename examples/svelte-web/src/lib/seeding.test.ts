/**
 * Tests for the seeding planner + driver. Runs on Node's built-in test runner
 * with native TypeScript stripping — no SDK, no browser globals:
 *
 *   node --test src/lib/seeding.test.ts
 *
 * `seeding.ts` is deliberately SDK-free (it takes an injected sink), so these
 * exercise the *real* planning/emission logic that fans out to the dashboard.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  makeRng,
  planVisitor,
  runSeeding,
  PRESETS,
  MAX_VISITORS,
  type SeedOp,
  type SeedingSink,
  type SeedUser,
} from './seeding.ts';

/** Flatten every op across a planned run. */
function allOps(runId: string, visitors: number): SeedOp[] {
  const rng = makeRng(runId);
  const ops: SeedOp[] = [];
  for (let i = 0; i < visitors; i++) ops.push(...planVisitor(rng, i, runId).ops);
  return ops;
}

test('makeRng is deterministic for a given seed', () => {
  const a = makeRng('seed-1');
  const b = makeRng('seed-1');
  assert.deepEqual([a(), a(), a()], [b(), b(), b()]);
});

test('planVisitor is deterministic and ids are namespaced by run', () => {
  const a = planVisitor(makeRng('run-x'), 7, 'run-x');
  const b = planVisitor(makeRng('run-x'), 7, 'run-x');
  assert.deepEqual(a, b);
  assert.equal(a.id, 'seed_run-x_7');
  assert.ok(a.ops.length > 0);
});

test('errors span every level from debug to fatal', () => {
  const ops = allOps('levels', 250);
  const levels = new Set(ops.filter((o) => o.t === 'error').map((o) => (o as Extract<SeedOp, { t: 'error' }>).level));
  for (const lvl of ['debug', 'info', 'warning', 'error', 'fatal']) {
    assert.ok(levels.has(lvl as never), `expected some errors at level ${lvl}`);
  }
});

test('fingerprints include both grouped (stable) and unique (long-tail) issues', () => {
  const errs = allOps('fp', 250).filter((o) => o.t === 'error') as Extract<SeedOp, { t: 'error' }>[];
  const grouped = errs.filter((e) => e.fingerprint?.length === 1);
  const unique = errs.filter((e) => e.fingerprint?.length === 2);
  assert.ok(grouped.length > 0, 'expected grouped (stable-fingerprint) errors');
  assert.ok(unique.length > 0, 'expected unique (per-occurrence) errors');

  // A grouped stem should actually repeat (→ one issue, many occurrences).
  const counts = new Map<string, number>();
  for (const e of grouped) counts.set(e.fingerprint![0], (counts.get(e.fingerprint![0]) ?? 0) + 1);
  assert.ok([...counts.values()].some((c) => c > 3), 'expected at least one high-count grouped issue');
});

test('there is a real mix of big vs small payloads, and tagged vs untagged errors', () => {
  const ops = allOps('mix', 250);
  const errs = ops.filter((o) => o.t === 'error') as Extract<SeedOp, { t: 'error' }>[];
  const evts = ops.filter((o) => o.t === 'event') as Extract<SeedOp, { t: 'event' }>[];

  assert.ok(errs.some((e) => e.big) && errs.some((e) => !e.big), 'expected big AND small errors');
  assert.ok(errs.some((e) => e.tagged) && errs.some((e) => !e.tagged), 'expected tagged AND untagged errors');
  assert.ok(evts.some((e) => e.big) && evts.some((e) => !e.big), 'expected big AND small events');

  // Big errors carry the multi-KB state snapshot as a tag value.
  const tagOps = ops.filter((o) => o.t === 'tags') as Extract<SeedOp, { t: 'tags' }>[];
  assert.ok(
    tagOps.some((o) => o.tags && 'state_snapshot' in o.tags && o.tags.state_snapshot.length > 1000),
    'expected a big state_snapshot tag payload',
  );
});

test('every event carries categorical props (plan/source/region/ab_variant)', () => {
  const evts = allOps('props', 120).filter((o) => o.t === 'event') as Extract<SeedOp, { t: 'event' }>[];
  assert.ok(evts.length > 0);
  for (const e of evts) {
    for (const key of ['plan', 'source', 'region', 'ab_variant']) {
      assert.ok(key in e.properties, `event ${e.name} missing categorical prop ${key}`);
    }
  }
});

/* ------------------------------------------------------------- driver tests */

interface RecordedErr {
  name: string;
  message: string;
  level: string;
  fingerprint: string[] | null;
  taggedAtCapture: boolean;
  bigTag: boolean;
  callContexts?: Record<string, Record<string, unknown>>;
}

function fakeSink() {
  const state = {
    setUserCalls: [] as (SeedUser | null)[],
    screens: [] as string[],
    tagSets: [] as (Record<string, string> | null)[],
    breadcrumbs: 0,
    clears: 0,
    errors: [] as RecordedErr[],
    events: [] as {
      name: string;
      keys: string[];
      meta?: {
        tags?: Record<string, string>;
        contexts?: Record<string, Record<string, unknown>>;
        extra?: Record<string, unknown>;
      };
    }[],
    flushes: 0,
    currentTags: null as Record<string, string> | null,
    contextSets: [] as { name: string; block: Record<string, unknown> }[],
    extraSets: [] as { key: string; value: unknown }[],
  };
  const record = (
    name: string,
    message: string,
    level: string,
    fingerprint: string[] | null,
    callContexts?: Record<string, Record<string, unknown>>,
  ) => {
    state.errors.push({
      name,
      message,
      level,
      fingerprint,
      taggedAtCapture: !!state.currentTags && Object.keys(state.currentTags).length > 0,
      bigTag: !!state.currentTags && 'state_snapshot' in state.currentTags,
      callContexts,
    });
  };
  const sink: SeedingSink = {
    setUser: (u) => state.setUserCalls.push(u),
    setScreen: (n) => state.screens.push(n),
    setTags: (t) => {
      state.currentTags = t;
      state.tagSets.push(t);
    },
    setContext: (name, block) => state.contextSets.push({ name, block }),
    setExtra: (key, value) => state.extraSets.push({ key, value }),
    addBreadcrumb: () => {
      state.breadcrumbs++;
    },
    clearBreadcrumbs: () => {
      state.clears++;
    },
    captureException: (err, hint) => record(err.name, err.message, hint.level, hint.fingerprint, hint.contexts),
    captureMessage: (msg, level, hint) => record('', msg, level, hint.fingerprint),
    track: (name, props, meta) =>
      state.events.push({ name, keys: Object.keys(props ?? {}), meta }),
    flush: async () => {
      state.flushes++;
    },
  };
  return { sink, state };
}

test('runSeeding flushes once, restores a clean scope, and reports a consistent summary', async () => {
  const { sink, state } = fakeSink();
  const summary = await runSeeding(sink, { visitors: 40, runId: 'drive' });

  assert.equal(state.flushes, 1, 'should flush exactly once');
  assert.equal(state.setUserCalls.at(-1), null, 'should reset the user at the end');
  assert.equal(state.tagSets.at(-1), null, 'should clear tags at the end');

  assert.equal(summary.visitors, 40);
  assert.equal(summary.errors, state.errors.length, 'summary.errors matches captured');
  assert.equal(summary.events, state.events.length, 'summary.events matches tracked');
  assert.ok(summary.errors > 0 && summary.events > 0);
  assert.ok(summary.issues > 1, 'expected multiple distinct issues');
  assert.ok(summary.bigPayloads > 0, 'expected some big payloads');
  assert.ok(summary.taggedErrors > 0, 'expected some tagged errors');

  // Level tallies in the summary add up to the error count.
  const totalLeveled = Object.values(summary.levels).reduce((a, b) => a + b, 0);
  assert.equal(totalLeveled, summary.errors);

  // Big errors were tagged with the state snapshot at capture time.
  assert.ok(state.errors.some((e) => e.bigTag), 'expected a captured error with a big state_snapshot tag');
  assert.ok(state.errors.some((e) => !e.taggedAtCapture), 'expected some untagged errors');
});

test('runSeeding is deterministic for a given runId', async () => {
  const a = await runSeeding(fakeSink().sink, { visitors: 30, runId: 'same' });
  const b = await runSeeding(fakeSink().sink, { visitors: 30, runId: 'same' });
  assert.deepEqual(a, b);
});

test('runSeeding clamps the visitor count to MAX_VISITORS', async () => {
  const summary = await runSeeding(fakeSink().sink, { visitors: 100000, runId: 'clamp' });
  assert.equal(summary.visitors, MAX_VISITORS);
});

test('preset visitor counts are ordered small < medium < large', () => {
  assert.ok(PRESETS.small.visitors < PRESETS.medium.visitors);
  assert.ok(PRESETS.medium.visitors < PRESETS.large.visitors);
});

test('runSeeding drives scope contexts/extra and per-call contexts on big errors', async () => {
  const { sink, state } = fakeSink();
  await runSeeding(sink, { visitors: 40, runId: 'meta' });
  assert.ok(state.contextSets.length > 0, 'expected per-visitor scope contexts');
  assert.ok(state.extraSets.length > 0, 'expected per-visitor scope extra');
  assert.ok(
    state.errors.some((e) => e.callContexts && Object.keys(e.callContexts).length > 0),
    'expected at least one error captured with per-call contexts',
  );
  // Analytics events also carry per-call metadata (tags on ~half; funnel
  // context + value extra on big events) — the override path, not just scope-lift.
  assert.ok(
    state.events.some((e) => e.meta?.tags && Object.keys(e.meta.tags).length > 0),
    'expected at least one event tracked with per-call tags',
  );
  assert.ok(
    state.events.some((e) => e.meta?.contexts && Object.keys(e.meta.contexts).length > 0),
    'expected at least one big event tracked with per-call contexts',
  );
});
