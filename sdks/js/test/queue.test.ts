import { beforeEach, describe, expect, it } from 'vitest';
import { OfflineQueue, type StorageLike } from '../src/transport/queue';

/** In-memory localStorage stand-in. */
class MemoryStorage implements StorageLike {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? (this.map.get(key) as string) : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
}

let storage: MemoryStorage;

beforeEach(() => {
  storage = new MemoryStorage();
});

describe('OfflineQueue - FIFO', () => {
  it('drains entries oldest-first', () => {
    const q = new OfflineQueue(1_000_000, storage);
    q.enqueue('one');
    q.enqueue('two');
    q.enqueue('three');
    expect(q.size()).toBe(3);
    expect(q.drain()).toEqual(['one', 'two', 'three']);
    expect(q.size()).toBe(0);
  });

  it('peek is non-destructive', () => {
    const q = new OfflineQueue(1_000_000, storage);
    q.enqueue('a');
    q.enqueue('b');
    expect(q.peek()).toEqual(['a', 'b']);
    expect(q.peek()).toEqual(['a', 'b']);
    expect(q.size()).toBe(2);
  });

  it('persists across queue instances via storage', () => {
    new OfflineQueue(1_000_000, storage).enqueue('persisted');
    const other = new OfflineQueue(1_000_000, storage);
    expect(other.peek()).toEqual(['persisted']);
  });
});

describe('OfflineQueue - byte cap eviction', () => {
  it('evicts the OLDEST entries when the byte cap is exceeded', () => {
    // Each payload is 100 bytes; cap holds ~2 of them.
    const payload = (tag: string): string => tag + 'x'.repeat(99); // 100 bytes
    const q = new OfflineQueue(250, storage);

    q.enqueue(payload('1'));
    q.enqueue(payload('2'));
    expect(q.size()).toBe(2); // 200 bytes, under cap

    q.enqueue(payload('3')); // 300 bytes -> evict oldest
    const remaining = q.peek();
    expect(remaining).toHaveLength(2);
    expect(remaining[0].startsWith('2')).toBe(true);
    expect(remaining[1].startsWith('3')).toBe(true);
    expect(q.byteSize()).toBeLessThanOrEqual(250);
  });

  it('keeps at least the newest entry even if it alone exceeds the cap', () => {
    const q = new OfflineQueue(50, storage);
    q.enqueue('x'.repeat(200)); // 200 bytes > 50 cap
    expect(q.size()).toBe(1);
    expect(q.peek()[0]).toHaveLength(200);
  });

  it('maintains FIFO order through repeated eviction', () => {
    const q = new OfflineQueue(250, storage);
    const p = (tag: string): string => tag + 'x'.repeat(99);
    for (const tag of ['1', '2', '3', '4', '5']) q.enqueue(p(tag));
    const tags = q.peek().map((e) => e[0]);
    expect(tags).toEqual(['4', '5']);
  });
});

describe('OfflineQueue - no storage', () => {
  it('is a no-op when storage is unavailable', () => {
    const q = new OfflineQueue(1000, null);
    expect(q.available).toBe(false);
    q.enqueue('ignored');
    expect(q.size()).toBe(0);
    expect(q.drain()).toEqual([]);
  });
});
