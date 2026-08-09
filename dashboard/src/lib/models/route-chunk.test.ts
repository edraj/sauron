import { describe, expect, it } from 'vitest';
import { chunkErrorMessage, loadRouteChunk } from './route-chunk';

// What this file may and may not assert.
//
// `loadRouteChunk` takes an injected loader, so a test can make that loader do
// anything — including succeed on a second call. An earlier version of this
// suite used exactly that to assert "retries once and succeeds — the commonest
// cause is transient". It passed, and it was worse than no test: driving a real
// browser showed a failed `import()` is recorded in the module map AS FAILED, so
// a second `import()` of the same specifier replays the cached rejection without
// touching the network. The suite was licensing belief in a recovery path the
// platform does not have.
//
// So the rule here is: assert only what a real chunk load can do. Concretely
// that means one invocation, one outcome, and never a rejection.
describe('loadRouteChunk', () => {
  it('returns the module on success', async () => {
    let calls = 0;
    const outcome = await loadRouteChunk(async () => {
      calls++;
      return { default: 'Page' };
    });

    expect(outcome.status).toBe('loaded');
    expect(outcome.status === 'loaded' && outcome.module).toEqual({ default: 'Page' });
    expect(calls).toBe(1);
  });

  it('invokes the loader exactly ONCE on failure — no retry', async () => {
    // The behaviour a browser actually has. A second `import()` of a failed
    // specifier does not reach the network, so a retry here could only ever
    // replay the same rejection while delaying the error state.
    let calls = 0;
    const outcome = await loadRouteChunk(async () => {
      calls++;
      throw new Error('Failed to fetch dynamically imported module: /assets/Login-a1b2c3.js');
    });

    expect(calls).toBe(1);
    expect(outcome.status).toBe('failed');
  });

  it('reports the reason instead of hanging', async () => {
    // The regression this file exists for: before it, the rejection went
    // unobserved and the router sat on its loading component indefinitely.
    const outcome = await loadRouteChunk(async () => {
      throw new Error('Failed to fetch dynamically imported module: /assets/Login-a1b2c3.js');
    });

    expect(outcome.status).toBe('failed');
    // The chunk URL survives into the copy — with a stale deploy it is the one
    // fact that identifies the cause.
    expect(outcome.status === 'failed' && outcome.message).toContain('Login-a1b2c3.js');
  });

  it('never rejects, even when the loader throws synchronously', async () => {
    const outcome = await loadRouteChunk(() => {
      throw new Error('sync boom');
    });
    expect(outcome.status).toBe('failed');
    expect(outcome.status === 'failed' && outcome.message).toBe('sync boom');
  });

  it('resolves without waiting — the error state is immediate', async () => {
    // The removed auto-retry paid a 350 ms pause before every failure, so the
    // spinner outlasted the knowledge that the load had failed. Nothing in the
    // path may sleep now.
    const started = Date.now();
    const outcome = await loadRouteChunk(async () => {
      throw new Error('nope');
    });
    expect(outcome.status).toBe('failed');
    expect(Date.now() - started).toBeLessThan(50);
  });
});

describe('chunkErrorMessage', () => {
  it('prefers the error message', () => {
    expect(chunkErrorMessage(new Error('boom'))).toBe('boom');
  });

  it('accepts a thrown string', () => {
    expect(chunkErrorMessage('boom')).toBe('boom');
  });

  it('never returns an empty string — a blank error state reads as a hang', () => {
    expect(chunkErrorMessage(new Error(''))).toBe('The page could not be downloaded.');
    expect(chunkErrorMessage(new Error('   '))).toBe('The page could not be downloaded.');
    expect(chunkErrorMessage(undefined)).toBe('The page could not be downloaded.');
    expect(chunkErrorMessage(null)).toBe('The page could not be downloaded.');
    expect(chunkErrorMessage({})).toBe('The page could not be downloaded.');
  });
});
