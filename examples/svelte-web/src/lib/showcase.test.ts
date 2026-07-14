/**
 * Tests for the cohort-simulation planner + driver. Runs on Node's built-in
 * test runner with native TypeScript stripping — no SDK, no browser globals:
 *
 *   node --test src/lib/showcase.test.ts
 *
 * `showcase.ts` is deliberately SDK-free (it takes an injected sink), so these
 * exercise the *real* planning/emission logic, not a mock of it.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  FUNNEL_STEPS,
  makeRng,
  planUser,
  runShowcase,
  TRANSACTION_OPS,
  type ShowcaseSink,
  type SinkUser,
} from './showcase.ts';

/** Collect every funnel event emitted across many planned users, by step. */
function funnelCounts(runId: string, users: number): number[] {
  const rng = makeRng(runId);
  const counts = new Array(FUNNEL_STEPS.length).fill(0);
  for (let i = 0; i < users; i++) {
    const plan = planUser(rng, i, runId);
    for (const action of plan.actions) {
      if (action.kind === 'event') {
        const step = FUNNEL_STEPS.indexOf(action.name);
        if (step >= 0) counts[step]++;
      }
    }
  }
  return counts;
}

test('makeRng is deterministic for a given seed', () => {
  const a = makeRng('seed-1');
  const b = makeRng('seed-1');
  const seqA = [a(), a(), a()];
  const seqB = [b(), b(), b()];
  assert.deepEqual(seqA, seqB);
  for (const v of seqA) {
    assert.ok(v >= 0 && v < 1, `rng value ${v} out of [0,1)`);
  }
});

test('planUser emits funnel steps as an ordered prefix starting at product_viewed', () => {
  const rng = makeRng('run-x');
  for (let i = 0; i < 200; i++) {
    const plan = planUser(rng, i, 'run-x');
    const emittedSteps = plan.actions
      .filter((a) => a.kind === 'event' && FUNNEL_STEPS.includes(a.name))
      .map((a) => FUNNEL_STEPS.indexOf((a as { name: string }).name));
    // A contiguous prefix 0,1,2,... with no gaps and no repeats.
    assert.deepEqual(
      emittedSteps,
      emittedSteps.map((_, idx) => idx),
      `user ${i} funnel steps not a clean prefix: ${emittedSteps}`,
    );
    assert.ok(plan.reached >= 1 && plan.reached <= FUNNEL_STEPS.length);
    assert.equal(emittedSteps.length, plan.reached);
    assert.equal(plan.id, `sim_run-x_${i}`);
  }
});

test('aggregate funnel counts are monotonically non-increasing and everyone views', () => {
  const users = 300;
  const counts = funnelCounts('cohort-a', users);
  assert.equal(counts[0], users, 'every synthetic user should emit product_viewed');
  for (let i = 1; i < counts.length; i++) {
    assert.ok(counts[i] <= counts[i - 1], `step ${i} (${counts[i]}) > step ${i - 1} (${counts[i - 1]})`);
  }
  // Sanity: there IS real drop-off, not a flat 100% funnel.
  assert.ok(counts.at(-1)! < counts[0], 'funnel should show drop-off');
});

test('every planned transaction has a known op and a positive duration', () => {
  const rng = makeRng('txn-run');
  let seen = 0;
  for (let i = 0; i < 200; i++) {
    for (const action of planUser(rng, i, 'txn-run').actions) {
      if (action.kind === 'txn') {
        seen++;
        assert.ok(
          (TRANSACTION_OPS as readonly string[]).includes(action.input.op ?? 'custom'),
          `unknown op ${action.input.op}`,
        );
        assert.ok(action.input.durationMs > 0, `non-positive duration ${action.input.durationMs}`);
      }
    }
  }
  assert.ok(seen > 0, 'expected some transactions');
});

/** A recording fake sink. */
function fakeSink(initial: SinkUser | null): {
  sink: ShowcaseSink;
  setUserCalls: (SinkUser | null)[];
  tracked: string[];
  txns: number;
  flushes: number;
} {
  const state = { setUserCalls: [] as (SinkUser | null)[], tracked: [] as string[], txns: 0, flushes: 0, user: initial };
  const sink: ShowcaseSink = {
    getUser: () => state.user,
    setUser: (u) => {
      state.user = u;
      state.setUserCalls.push(u);
    },
    track: (name) => state.tracked.push(name),
    trackTransaction: () => {
      state.txns++;
    },
    flush: async () => {
      state.flushes++;
    },
  };
  return {
    sink,
    get setUserCalls() {
      return state.setUserCalls;
    },
    get tracked() {
      return state.tracked;
    },
    get txns() {
      return state.txns;
    },
    get flushes() {
      return state.flushes;
    },
  };
}

test('runShowcase restores the pre-run user (null case) and flushes once', async () => {
  const f = fakeSink(null);
  const summary = await runShowcase(f.sink, { users: 25, runId: 'r1' });
  assert.equal(f.flushes, 1, 'should flush exactly once');
  assert.equal(f.setUserCalls.at(-1), null, 'should restore null user');
  assert.equal(summary.users, 25);
  assert.equal(summary.funnel[0].count, 25, 'summary step 0 == users');
  assert.ok(summary.transactions > 0);
});

test('runShowcase restores a previously-set user object', async () => {
  const original: SinkUser = { id: 'user_demo_1', traits: { plan: 'pro' } };
  const f = fakeSink(original);
  await runShowcase(f.sink, { users: 10, runId: 'r2' });
  assert.deepEqual(f.setUserCalls.at(-1), original, 'should restore the original user');
});

test('runShowcase clamps user count into [1, 500]', async () => {
  const f = fakeSink(null);
  const summary = await runShowcase(f.sink, { users: 100000, runId: 'r3' });
  assert.equal(summary.users, 500);
});
