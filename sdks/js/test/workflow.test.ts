import { describe, it, expect, beforeEach } from 'vitest';
import { getClient, init } from '../src/client.js';
import {
  cancelWorkflow,
  endWorkflow,
  identify,
  startWorkflow,
  track,
  trackTransaction,
} from '../src/api/product.js';
import { captureException, captureMessage } from '../src/api/capture.js';
import { getWorkflow, resetWorkflow } from '../src/workflow.js';
import type { EnvelopeItem } from '../src/types.js';

let items: any[] = [];

function lastItem(): any {
  return items[items.length - 1];
}

function lastItems(n: number): any[] {
  return items.slice(items.length - n);
}

function itemCount(): number {
  return items.length;
}

describe('workflows', () => {
  beforeEach(() => {
    resetWorkflow();
    items = [];
    init({
      dsn: 'https://pk_test@localhost:9/1',
      beforeSend: (i: EnvelopeItem) => {
        items.push(i);
        return null;
      },
    });
  });

  it('start returns ok and emits $workflow_start stamped with the new workflow', () => {
    const r = startWorkflow('checkout');
    expect(r.status).toBe('ok');
    expect(r.workflowId).toBeTruthy();
    const item = lastItem();
    expect(item.name).toBe('$workflow_start');
    expect(item.workflow_name).toBe('checkout');
    expect(item.workflow_id).toBe(r.workflowId);
    expect(item.properties.workflow_name).toBe('checkout');
  });

  it('stamps subsequent track calls with the active workflow', () => {
    const r = startWorkflow('checkout');
    track('add_to_cart');
    expect(lastItem().workflow_id).toBe(r.workflowId);
    expect(lastItem().workflow_name).toBe('checkout');
  });

  // The `in` operator, not `toEqual`: `toEqual` treats a key present with an
  // `undefined` value as equivalent to an absent key, so it would wave through
  // exactly the regression these assertions exist to catch. Asserted on EVERY
  // stamped item type — a leaf path that regressed to `workflow_id: undefined`
  // would otherwise be invisible, since the other paths only assert positively.
  it('omits the fields entirely on an event when no workflow is active', () => {
    track('plain');
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('omits the fields entirely on an error when no workflow is active', () => {
    captureException(new Error('boom'));
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('omits the fields entirely on a message when no workflow is active', () => {
    captureMessage('just so you know');
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('omits the fields entirely on a transaction when no workflow is active', () => {
    trackTransaction({ name: 'load', op: 'navigation', durationMs: 12 });
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('never stamps an identify item, which has no workflow columns server-side', () => {
    startWorkflow('checkout');
    identify('u_123');
    expect(lastItem().type).toBe('identify');
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('start while active returns already_active and changes nothing', () => {
    const first = startWorkflow('onboarding');
    const before = itemCount();
    const second = startWorkflow('checkout');
    expect(second.status).toBe('already_active');
    expect(itemCount()).toBe(before);
    expect(getWorkflow()!.workflowId).toBe(first.workflowId);
  });

  it('force cancels the old with reason superseded then starts the new', () => {
    const first = startWorkflow('onboarding');
    const second = startWorkflow('checkout', { force: true });
    expect(second.status).toBe('ok');
    const [cancel, start] = lastItems(2);
    expect(cancel.name).toBe('$workflow_cancel');
    expect(cancel.workflow_id).toBe(first.workflowId);
    expect(cancel.properties.reason).toBe('superseded');
    expect(start.name).toBe('$workflow_start');
    expect(start.workflow_id).toBe(second.workflowId);
  });

  it('end emits $workflow_end with duration_ms and clears state', () => {
    startWorkflow('checkout');
    expect(endWorkflow().status).toBe('ok');
    expect(lastItem().name).toBe('$workflow_end');
    expect(typeof lastItem().properties.duration_ms).toBe('number');
    expect(getWorkflow()).toBeNull();
  });

  it('end with a mismatched name is a no-op returning name_mismatch', () => {
    startWorkflow('checkout');
    const before = itemCount();
    expect(endWorkflow('onboarding').status).toBe('name_mismatch');
    expect(itemCount()).toBe(before);
    expect(getWorkflow()).not.toBeNull();
  });

  it('end with no active workflow returns not_active', () => {
    expect(endWorkflow().status).toBe('not_active');
  });

  it('cancel defaults reason to user and caps a long reason at 120 chars', () => {
    startWorkflow('checkout');
    cancelWorkflow(undefined, { reason: 'x'.repeat(300) });
    expect(lastItem().properties.reason).toHaveLength(120);
  });

  it('cancel with no options defaults the reason to user', () => {
    startWorkflow('checkout');
    cancelWorkflow();
    expect(lastItem().name).toBe('$workflow_cancel');
    expect(lastItem().properties.reason).toBe('user');
  });

  it('rejects an empty or over-long name without starting anything', () => {
    expect(startWorkflow('   ').status).toBe('invalid_name');
    expect(startWorkflow('n'.repeat(121)).status).toBe('invalid_name');
    expect(getWorkflow()).toBeNull();
  });

  it('trims the name', () => {
    startWorkflow('  checkout  ');
    expect(getWorkflow()!.name).toBe('checkout');
  });

  it('stamps captureException, captureMessage and trackTransaction too', () => {
    const r = startWorkflow('checkout');

    captureException(new Error('boom'));
    expect(lastItem().workflow_id).toBe(r.workflowId);
    expect(lastItem().workflow_name).toBe('checkout');

    // captureMessage builds its ErrorItem inline rather than via
    // buildErrorItem, so it is the path most likely to be missed.
    captureMessage('heads up');
    expect(lastItem().workflow_id).toBe(r.workflowId);
    expect(lastItem().workflow_name).toBe('checkout');

    trackTransaction({ name: 'load', op: 'navigation', durationMs: 12 });
    expect(lastItem().workflow_id).toBe(r.workflowId);
    expect(lastItem().workflow_name).toBe('checkout');
  });

  it('returns disabled for every call once the client is torn down', () => {
    getClient()!.teardown();
    expect(startWorkflow('checkout').status).toBe('disabled');
    expect(endWorkflow().status).toBe('disabled');
    expect(cancelWorkflow().status).toBe('disabled');
    expect(getWorkflow()).toBeNull();
  });

  /*
   * The returned status must never disagree with the actual state.
   * `disabled` is documented as "nothing changed", so a well-behaved caller
   * does not retry on it — if a failed lifecycle emit returned `disabled`
   * while leaving the workflow set (or cleared), the SDK would be desynced
   * from the app for the rest of the session with no way for the caller to
   * notice. Unreachable in the browser today (the transport swallows its own
   * failures), but reachable in the Node/Python/C# ports, so the invariant is
   * pinned here in the reference implementation.
   */
  function withThrowingCapture(body: () => void): void {
    const client = getClient()! as unknown as { captureItem: unknown };
    const original = client.captureItem;
    client.captureItem = () => {
      throw new Error('transport exploded');
    };
    try {
      body();
    } finally {
      client.captureItem = original;
    }
  }

  it('still clears state and reports ok when the closing emit throws', () => {
    startWorkflow('checkout');
    withThrowingCapture(() => {
      // `ok` is truthful precisely because the state clear is in a `finally`.
      expect(endWorkflow().status).toBe('ok');
      expect(getWorkflow()).toBeNull();
    });
  });

  it('still clears state and reports ok when the cancel emit throws', () => {
    startWorkflow('checkout');
    withThrowingCapture(() => {
      expect(cancelWorkflow().status).toBe('ok');
      expect(getWorkflow()).toBeNull();
    });
  });

  it('reports ok and keeps the workflow live when $workflow_start fails to emit', () => {
    withThrowingCapture(() => {
      const r = startWorkflow('checkout');
      // The workflow IS live and stamping IS active, so `disabled` would be a
      // lie and would also throw away the only copy of the id.
      expect(r.status).toBe('ok');
      expect(r.workflowId).toBeTruthy();
      expect(getWorkflow()!.workflowId).toBe(r.workflowId);
    });
    // Stamping really is live afterwards — not just the getter agreeing.
    track('after');
    expect(lastItem().workflow_name).toBe('checkout');
  });
});
