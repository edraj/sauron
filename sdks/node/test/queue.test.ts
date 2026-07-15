import { describe, it, expect, afterEach } from 'vitest';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { BoundedQueue } from '../src/queue.js';
import type { EnvelopeItem } from '../src/types.js';

const dirs: string[] = [];
function tempDir(): string {
  const d = mkdtempSync(join(tmpdir(), 'sauron-queue-'));
  dirs.push(d);
  return d;
}

afterEach(() => {
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

/** A padded event item of roughly `pad` bytes. */
function item(id: string, pad = 0): EnvelopeItem {
  return {
    type: 'event',
    name: id,
    distinct_id: 'u1',
    properties: { blob: 'x'.repeat(pad) },
    timestamp: '2026-07-15T00:00:00.000Z',
    session_id: null,
    screen: null,
  };
}

function names(drained: EnvelopeItem[]): string[] {
  return drained.map((i) => (i as { name: string }).name);
}

describe('BoundedQueue (memory)', () => {
  it('drops the oldest item when over maxQueueBytes and keeps bytes <= max', () => {
    // Each padded item is ~200 bytes; cap holds ~2 of them.
    const one = JSON.stringify(item('probe', 200));
    const per = Buffer.byteLength(one);
    const q = new BoundedQueue({ maxBytes: per * 2 + 10 });

    q.push(item('a', 200));
    q.push(item('b', 200));
    q.push(item('c', 200)); // forces the oldest ('a') out
    q.push(item('d', 200)); // forces 'b' out

    expect(q.bytes).toBeLessThanOrEqual(per * 2 + 10);
    expect(names(q.drain())).toEqual(['c', 'd']);
  });

  it('drains everything and reports zero bytes afterward', () => {
    const q = new BoundedQueue({ maxBytes: 1_000_000 });
    q.push(item('a'));
    q.push(item('b'));
    expect(q.size).toBe(2);
    expect(names(q.drain())).toEqual(['a', 'b']);
    expect(q.bytes).toBe(0);
    expect(q.size).toBe(0);
  });

  it('is a no-op for commit/restore with nothing in flight', () => {
    const q = new BoundedQueue({ maxBytes: 1_000_000 });
    expect(() => {
      q.commit();
      q.restore();
    }).not.toThrow();
  });

  it('restore puts an un-acked batch back at the head of the queue', () => {
    const q = new BoundedQueue({ maxBytes: 1_000_000 });
    q.push(item('a'));
    q.push(item('b'));
    const drained = q.drain();
    expect(names(drained)).toEqual(['a', 'b']);
    q.push(item('c')); // arrives while the batch is in flight
    q.restore(); // send failed → put a,b back ahead of c
    expect(names(q.drain())).toEqual(['a', 'b', 'c']);
  });
});

describe('BoundedQueue (disk persistence)', () => {
  it('persists pushed items and a fresh instance reloads them (at-least-once)', () => {
    const dir = tempDir();
    const q1 = new BoundedQueue({ maxBytes: 1_000_000, offlineDir: dir });
    q1.push(item('a'));
    q1.push(item('b'));
    expect(readdirSync(dir).length).toBe(2);

    // Simulate a crash before delivery: a brand-new instance reloads from disk.
    const q2 = new BoundedQueue({ maxBytes: 1_000_000, offlineDir: dir });
    expect(q2.size).toBe(2);
    expect(names(q2.drain())).toEqual(['a', 'b']);
  });

  it('deletes delivered items from disk on commit', () => {
    const dir = tempDir();
    const q = new BoundedQueue({ maxBytes: 1_000_000, offlineDir: dir });
    q.push(item('a'));
    q.push(item('b'));
    q.drain();
    q.commit(); // delivered
    expect(readdirSync(dir).length).toBe(0);
  });

  it('keeps disk files when a batch is restored (send failed)', () => {
    const dir = tempDir();
    const q = new BoundedQueue({ maxBytes: 1_000_000, offlineDir: dir });
    q.push(item('a'));
    q.drain();
    q.restore();
    expect(readdirSync(dir).length).toBe(1);
    // And the reloaded item is still drainable.
    const fresh = new BoundedQueue({ maxBytes: 1_000_000, offlineDir: dir });
    expect(names(fresh.drain())).toEqual(['a']);
  });

  it('removes evicted files from disk when over the byte cap', () => {
    const dir = tempDir();
    const per = Buffer.byteLength(JSON.stringify(item('probe', 200)));
    const q = new BoundedQueue({ maxBytes: per * 2 + 10, offlineDir: dir });
    q.push(item('a', 200));
    q.push(item('b', 200));
    q.push(item('c', 200)); // evicts 'a' → its file must be unlinked
    expect(readdirSync(dir).length).toBe(2);
    expect(names(q.drain())).toEqual(['b', 'c']);
  });
});
