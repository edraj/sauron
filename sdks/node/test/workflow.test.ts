import { describe, it, expect, afterEach, vi } from 'vitest';

/**
 * Seam for forcing `randomUUID()` to throw, so the "mint the replacement
 * BEFORE superseding" ordering in `startWorkflow`'s force path can actually be
 * verified rather than assumed. `vi.mock` is hoisted and file-wide, so the
 * override is a pass-through unless a test explicitly arms it.
 */
const mint = vi.hoisted(() => ({ shouldThrow: false }));
vi.mock('node:crypto', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:crypto')>();
  return {
    ...actual,
    randomUUID: () => {
      if (mint.shouldThrow) throw new Error('mint failed');
      return actual.randomUUID();
    },
  };
});

import { SauronClient } from '../src/client.js';
import { getGlobalScope, withScope } from '../src/scope.js';
import type { Transport } from '../src/transport.js';
import { getWorkflow, normalizeReason, normalizeWorkflowName } from '../src/workflow.js';
import type { EnvelopeItem, FetchLike } from '../src/types.js';

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

/**
 * A client whose `beforeSend` captures every dispatched item BEFORE it is
 * JSON-serialized. This is deliberate: `JSON.stringify` drops `undefined`-
 * valued keys on its own, so a round-tripped item can't distinguish "key
 * omitted" from "key present but undefined" — exactly the regression this
 * suite must catch (per the omission trap: Vitest's `toEqual` also treats an
 * `undefined`-valued key as equivalent to an absent one). Reading the raw,
 * pre-serialization object with the `in` operator is the only way to prove
 * real omission.
 *
 * `options.throwOnEvent`, when set, arms a throw for the matching item's
 * `name` — but NOT via `beforeSend`. `beforeSend` is guarded by `dispatch()`
 * (a throwing user hook must not break the emit path or drop telemetry), so a
 * throw planted there would be swallowed and could no longer exercise the
 * "internal exception mid-emit" windows below. Instead the throw is planted
 * on `Transport.enqueue`, which sits immediately after `beforeSend` inside
 * `dispatch()` and is NOT a user-supplied hook — it reproduces the exact same
 * "something blows up while emitting this item" condition those tests need,
 * from a seam the fix does not (and should not) guard.
 */
function newClient(options: { throwOnEvent?: string } = {}) {
  const items: Array<Record<string, any>> = [];
  const fetchImpl: FetchLike = async () => ({ status: 200, ok: true });
  const client = new SauronClient({
    dsn: DSN,
    flushInterval: 0,
    fetchImpl,
    // Purely a capture role now (see the doc comment above) — every
    // dispatched item lands here unconditionally, throw or no throw.
    beforeSend: (item: EnvelopeItem) => {
      items.push(item as unknown as Record<string, any>);
      return item;
    },
  });
  const transport = (client as unknown as { transport: Transport }).transport;
  const originalEnqueue = transport.enqueue.bind(transport);
  transport.enqueue = (item: EnvelopeItem): void => {
    const record = item as unknown as Record<string, any>;
    if (options.throwOnEvent && record.name === options.throwOnEvent) {
      throw new Error(`enqueue blew up on ${options.throwOnEvent}`);
    }
    originalEnqueue(item);
  };
  return { client, items, last: () => items[items.length - 1] };
}

describe('workflow', () => {
  afterEach(() => {
    // The global scope is a module-wide singleton shared across clients/tests
    // in this file; reset it so a leftover workflow can't bleed into the next
    // test the way a module-global implementation would leak across requests.
    getGlobalScope().data.workflow = null;
  });

  describe('normalizeWorkflowName / normalizeReason', () => {
    it('rejects empty and over-long names', () => {
      expect(normalizeWorkflowName('')).toBeNull();
      expect(normalizeWorkflowName('   ')).toBeNull();
      expect(normalizeWorkflowName('x'.repeat(121))).toBeNull();
      expect(normalizeWorkflowName(123)).toBeNull();
      expect(normalizeWorkflowName('x'.repeat(120))).toBe('x'.repeat(120));
      expect(normalizeWorkflowName('  checkout  ')).toBe('checkout');
    });

    it('defaults reason to user and caps a long reason at 120 chars', () => {
      expect(normalizeReason(undefined)).toBe('user');
      expect(normalizeReason('   ')).toBe('user');
      expect(normalizeReason(42)).toBe('user');
      expect(normalizeReason('  custom  ')).toBe('custom');
      expect(normalizeReason('x'.repeat(300))).toHaveLength(120);
    });
  });

  describe('startWorkflow', () => {
    it('emits $workflow_start stamped with the new workflow', () => {
      const { client, last } = newClient();
      const result = client.startWorkflow('checkout');

      expect(result.status).toBe('ok');
      expect(typeof result.workflowId).toBe('string');

      const item = last();
      expect(item.type).toBe('event');
      expect(item.name).toBe('$workflow_start');
      expect(item.workflow_id).toBe(result.workflowId);
      expect(item.workflow_name).toBe('checkout');
      // Also present in properties (server hand-rolled-client fallback).
      expect(item.properties.workflow_id).toBe(result.workflowId);
      expect(item.properties.workflow_name).toBe('checkout');

      expect(getWorkflow()).toEqual({
        workflowId: result.workflowId,
        name: 'checkout',
        startedAt: expect.any(String),
      });
    });

    it('mints a fresh UUID per call, never deterministic', () => {
      const { client } = newClient();
      const a = client.startWorkflow('a');
      client.endWorkflow();
      const b = client.startWorkflow('a');
      expect(a.workflowId).not.toBe(b.workflowId);
    });

    it('rejects empty and over-long names, emitting nothing', () => {
      const { client, items } = newClient();
      expect(client.startWorkflow('')).toEqual({ status: 'invalid_name' });
      expect(client.startWorkflow('   ')).toEqual({ status: 'invalid_name' });
      expect(client.startWorkflow('x'.repeat(121))).toEqual({ status: 'invalid_name' });
      expect(items).toHaveLength(0);
      expect(getWorkflow()).toBeNull();
    });

    it('start while active returns already_active and emits nothing', () => {
      const { client, items } = newClient();
      client.startWorkflow('a');
      const before = items.length;
      const result = client.startWorkflow('b');
      expect(result).toEqual({ status: 'already_active' });
      expect(items).toHaveLength(before);
      expect(getWorkflow()?.name).toBe('a');
    });

    it('force cancels the active workflow with reason superseded then starts the new one', () => {
      const { client, items } = newClient();
      const a = client.startWorkflow('a');
      const b = client.startWorkflow('b', { force: true });

      expect(b.status).toBe('ok');
      expect(b.workflowId).not.toBe(a.workflowId);

      const names = items.map((i) => i.name);
      expect(names).toEqual(['$workflow_start', '$workflow_cancel', '$workflow_start']);

      const cancelItem = items[1];
      expect(cancelItem.workflow_id).toBe(a.workflowId);
      expect(cancelItem.workflow_name).toBe('a');
      expect(cancelItem.properties.reason).toBe('superseded');

      const secondStart = items[2];
      expect(secondStart.workflow_id).toBe(b.workflowId);
      expect(secondStart.workflow_name).toBe('b');

      expect(getWorkflow()?.name).toBe('b');
    });

    it('returns disabled once the transport has auto-disabled itself, emitting nothing', async () => {
      const fetchImpl: FetchLike = async () => ({ status: 401, ok: false });
      const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl });
      client.track('trigger', 'u1');
      await client.flush(); // 401 -> transport.disabled = true

      expect(client.isEnabled()).toBe(false);
      expect(client.startWorkflow('checkout')).toEqual({ status: 'disabled' });
      expect(getWorkflow()).toBeNull();
    });
  });

  describe('endWorkflow', () => {
    it('emits $workflow_end with duration_ms and clears the scope field', () => {
      const { client, last } = newClient();
      const started = client.startWorkflow('checkout');
      const result = client.endWorkflow();

      expect(result).toEqual({ status: 'ok', workflowId: started.workflowId });

      const item = last();
      expect(item.name).toBe('$workflow_end');
      expect(item.workflow_id).toBe(started.workflowId);
      expect(item.workflow_name).toBe('checkout');
      // Also mirrored into properties (server hand-rolled-client fallback),
      // same as $workflow_start.
      expect(item.properties.workflow_id).toBe(started.workflowId);
      expect(item.properties.workflow_name).toBe('checkout');
      expect(typeof item.properties.duration_ms).toBe('number');
      expect(item.properties.duration_ms).toBeGreaterThanOrEqual(0);
      // end never carries a reason.
      expect('reason' in item.properties).toBe(false);

      expect(getWorkflow()).toBeNull();
    });

    it('accepts the matching explicit name', () => {
      const { client } = newClient();
      client.startWorkflow('checkout');
      expect(client.endWorkflow('checkout').status).toBe('ok');
      expect(getWorkflow()).toBeNull();
    });

    it('with a mismatched name returns name_mismatch and is a no-op', () => {
      const { client, items } = newClient();
      client.startWorkflow('checkout');
      const before = items.length;

      expect(client.endWorkflow('shipping')).toEqual({ status: 'name_mismatch' });
      expect(items).toHaveLength(before);
      expect(getWorkflow()?.name).toBe('checkout');
    });

    it('an explicit name that fails normalization returns name_mismatch, not invalid_name', () => {
      const { client } = newClient();
      client.startWorkflow('checkout');
      expect(client.endWorkflow('   ')).toEqual({ status: 'name_mismatch' });
      expect(client.endWorkflow('x'.repeat(121))).toEqual({ status: 'name_mismatch' });
      expect(getWorkflow()?.name).toBe('checkout');
    });

    it('with none active returns not_active', () => {
      const { client } = newClient();
      expect(client.endWorkflow()).toEqual({ status: 'not_active' });
    });
  });

  describe('cancelWorkflow', () => {
    it('emits $workflow_cancel, defaulting reason to user', () => {
      const { client, last } = newClient();
      client.startWorkflow('checkout');
      const result = client.cancelWorkflow();

      expect(result.status).toBe('ok');
      const item = last();
      expect(item.name).toBe('$workflow_cancel');
      expect(item.properties.reason).toBe('user');
      expect(getWorkflow()).toBeNull();
    });

    it('caps a long reason at 120 chars', () => {
      const { client, last } = newClient();
      client.startWorkflow('checkout');
      client.cancelWorkflow(undefined, { reason: 'x'.repeat(300) });
      expect(last().properties.reason).toHaveLength(120);
    });

    it('with none active returns not_active', () => {
      const { client } = newClient();
      expect(client.cancelWorkflow()).toEqual({ status: 'not_active' });
    });

    it('with a mismatched name returns name_mismatch and is a no-op', () => {
      const { client, items } = newClient();
      client.startWorkflow('checkout');
      const before = items.length;
      expect(client.cancelWorkflow('other')).toEqual({ status: 'name_mismatch' });
      expect(items).toHaveLength(before);
      expect(getWorkflow()?.name).toBe('checkout');
    });
  });

  describe('stamping every capture path', () => {
    it('stamps track, captureException, captureMessage and trackTransaction while active', () => {
      const { client, items } = newClient();
      const started = client.startWorkflow('checkout');
      items.length = 0; // drop the $workflow_start item itself

      client.track('add_to_cart', 'user-1');
      client.captureException(new Error('boom'));
      client.captureMessage('note'); // built inline — the JS reference's trap site
      client.trackTransaction({ name: 'charge', duration_ms: 5 });

      expect(items).toHaveLength(4);
      for (const item of items) {
        expect(item.workflow_id).toBe(started.workflowId);
        expect(item.workflow_name).toBe('checkout');
      }
    });

    it('omits both keys on track, captureException, captureMessage and trackTransaction when no workflow is active', () => {
      const { client, items } = newClient();

      client.track('add_to_cart', 'user-1');
      client.captureException(new Error('boom'));
      client.captureMessage('note');
      client.trackTransaction({ name: 'charge', duration_ms: 5 });

      expect(items).toHaveLength(4);
      for (const item of items) {
        // The `in` operator, not `toEqual`/`toBeUndefined` — those treat an
        // undefined-valued key as equivalent to an absent one and would miss
        // a regression to `workflow_id: undefined`.
        expect('workflow_id' in item).toBe(false);
        expect('workflow_name' in item).toBe(false);
      }
    });

    it('lifecycle events carry an EMPTY distinct_id when no user is identified', () => {
      // Empty (not a sentinel) is load-bearing: the pipeline maps an empty
      // distinct_id to SQL NULL on the workflows row, and the dashboard's
      // COUNT(DISTINCT distinct_id) AS unique_users skips NULLs. A sentinel
      // would collapse every anonymous run into one fake "user".
      const { client, items } = newClient();
      client.startWorkflow('guest_checkout');
      client.endWorkflow();

      const lifecycle = items.filter((i) => String(i.name).startsWith('$workflow_'));
      expect(lifecycle.map((i) => i.name)).toEqual(['$workflow_start', '$workflow_end']);
      for (const item of lifecycle) {
        expect(item.distinct_id).toBe('');
      }
    });

    it('lifecycle events use the scoped user id when one IS identified', () => {
      const { client, items } = newClient();
      withScope((scope) => {
        scope.setUser({ id: 'user-77' });
        client.startWorkflow('checkout');
        client.cancelWorkflow();
      });

      const lifecycle = items.filter((i) => String(i.name).startsWith('$workflow_'));
      expect(lifecycle).toHaveLength(2);
      for (const item of lifecycle) {
        expect(item.distinct_id).toBe('user-77');
      }
    });

    it('an ordinary track() with an empty distinct id is still dropped (guard unchanged)', () => {
      const { client, items } = newClient();
      client.startWorkflow('checkout');
      items.length = 0;

      client.track('manual_event', '');

      expect(items).toHaveLength(0);
    });

    it('never stamps identify items (server has no workflow columns for them)', () => {
      const { client, last } = newClient();
      client.startWorkflow('checkout');
      client.identify('user-1', { plan: 'pro' });

      const item = last();
      expect(item.type).toBe('identify');
      expect('workflow_id' in item).toBe(false);
      expect('workflow_name' in item).toBe(false);
    });
  });

  describe('request isolation (AsyncLocalStorage, not a module global)', () => {
    it('does NOT leak a workflow across concurrent async contexts', async () => {
      const { client } = newClient();
      const seen: Record<string, string | undefined> = {};

      await Promise.all([
        withScope(async () => {
          client.startWorkflow('a');
          await new Promise((r) => setTimeout(r, 10));
          client.track('from_a', 'user-a');
          seen.a = getWorkflow()?.name;
        }),
        withScope(async () => {
          client.startWorkflow('b');
          client.track('from_b', 'user-b');
          seen.b = getWorkflow()?.name;
        }),
      ]);

      expect(seen.a).toBe('a');
      expect(seen.b).toBe('b');
    });

    it('stamps each concurrent context capture with its OWN workflow, never the other', async () => {
      const { client, items } = newClient();
      const captured: Record<string, any> = {};

      await Promise.all([
        withScope(async () => {
          client.startWorkflow('a');
          await new Promise((r) => setTimeout(r, 10));
          client.track('from_a', 'user-a');
        }),
        withScope(async () => {
          client.startWorkflow('b');
          client.track('from_b', 'user-b');
        }),
      ]);

      for (const item of items) {
        if (item.name === 'from_a') captured.a = item;
        if (item.name === 'from_b') captured.b = item;
      }

      expect(captured.a.workflow_name).toBe('a');
      expect(captured.b.workflow_name).toBe('b');
      expect(captured.a.workflow_id).not.toBe(captured.b.workflow_id);
    });

    it('a workflow started inside withScope does not survive it', async () => {
      const { client } = newClient();
      await withScope(async () => {
        client.startWorkflow('inner');
        expect(getWorkflow()?.name).toBe('inner');
      });
      expect(getWorkflow()).toBeNull();
    });
  });

  describe('internal exceptions never leave half-mutated state', () => {
    afterEach(() => {
      mint.shouldThrow = false;
    });

    it('close-emit throws: endWorkflow still returns ok and STILL clears the workflow', () => {
      // Window 1 — guards the `finally`-clear in closeWorkflow. Without the
      // `finally`, the throw skips the clear and the workflow stays active
      // forever while the caller is told `ok`.
      const { client } = newClient({ throwOnEvent: '$workflow_end' });
      const started = client.startWorkflow('checkout');
      expect(started.status).toBe('ok');

      const result = client.endWorkflow();

      expect(result).toEqual({ status: 'ok', workflowId: started.workflowId });
      expect(getWorkflow()).toBeNull();
    });

    it('close-emit throws: cancelWorkflow behaves the same', () => {
      const { client } = newClient({ throwOnEvent: '$workflow_cancel' });
      const started = client.startWorkflow('checkout');

      expect(client.cancelWorkflow()).toEqual({ status: 'ok', workflowId: started.workflowId });
      expect(getWorkflow()).toBeNull();
    });

    it('start-emit throws AFTER state was set: returns ok with the id, workflow stays live', () => {
      // Window 2 — a lost $workflow_start is recoverable server-side (the row
      // materializes from the next stamped item); a lost local id is not. So
      // this must NOT degrade to `disabled`.
      const { client } = newClient({ throwOnEvent: '$workflow_start' });

      const result = client.startWorkflow('checkout');

      expect(result.status).toBe('ok');
      expect(typeof result.workflowId).toBe('string');
      expect(getWorkflow()).toEqual({
        workflowId: result.workflowId,
        name: 'checkout',
        startedAt: expect.any(String),
      });
    });

    it('a workflow that survived a failed start still stamps subsequent captures', () => {
      const { client, items } = newClient({ throwOnEvent: '$workflow_start' });
      const started = client.startWorkflow('checkout');
      items.length = 0;

      client.captureMessage('after a failed start');

      expect(items).toHaveLength(1);
      expect(items[0].workflow_id).toBe(started.workflowId);
      expect(items[0].workflow_name).toBe('checkout');
    });

    it('force-path mint throws: NOTHING happens — no cancel on the wire, old workflow intact', () => {
      // Window 3 — the C1 regression. The replacement id/timestamp must be
      // minted BEFORE the supersede emit. If they are minted after, the old
      // workflow's $workflow_cancel is already dispatched when the mint
      // throws, yet `disabled` is returned and scope.data.workflow still holds
      // the old one — so the caller's later endWorkflow() emits a SECOND
      // terminal event for a workflow the server already recorded cancelled.
      const { client, items } = newClient();
      const original = client.startWorkflow('checkout');
      expect(original.status).toBe('ok');
      items.length = 0;

      mint.shouldThrow = true;
      const result = client.startWorkflow('replacement', { force: true });
      mint.shouldThrow = false;

      // `disabled` must mean literally nothing happened.
      expect(result).toEqual({ status: 'disabled' });
      expect(items).toHaveLength(0); // no $workflow_cancel was dispatched
      expect(getWorkflow()).toEqual({
        workflowId: original.workflowId,
        name: 'checkout',
        startedAt: expect.any(String),
      });

      // And the old workflow can still be ended exactly once, normally.
      const ended = client.endWorkflow('checkout');
      expect(ended).toEqual({ status: 'ok', workflowId: original.workflowId });
      expect(items.map((i) => i.name)).toEqual(['$workflow_end']);
    });

    it('mint throws on a plain (non-force) start: disabled, nothing emitted, nothing set', () => {
      const { client, items } = newClient();

      mint.shouldThrow = true;
      const result = client.startWorkflow('checkout');
      mint.shouldThrow = false;

      expect(result).toEqual({ status: 'disabled' });
      expect(items).toHaveLength(0);
      expect(getWorkflow()).toBeNull();
    });
  });

  describe('teardown', () => {
    it('close() clears (never auto-cancels) a workflow left on the global scope', async () => {
      const { client, items } = newClient();
      client.startWorkflow('checkout');
      const before = items.length;

      await client.close();

      // No $workflow_cancel/$workflow_end was emitted by close() itself.
      expect(items).toHaveLength(before);
      expect(getWorkflow()).toBeNull();
    });
  });
});
