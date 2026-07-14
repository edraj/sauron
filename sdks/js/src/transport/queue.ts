import { byteLength } from '../utils.js';

/** The minimal storage surface we need (a subset of `Storage`). */
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const QUEUE_KEY = 'sauron:queue:v1';

/** Return `localStorage` if it is present and writable, else `null`. */
export function defaultStorage(): StorageLike | null {
  try {
    const ls = (globalThis as { localStorage?: StorageLike }).localStorage;
    if (!ls) return null;
    const probe = '__sauron_probe__';
    ls.setItem(probe, '1');
    ls.removeItem(probe);
    return ls;
  } catch {
    // Storage disabled (private mode, blocked cookies, SSR, ...).
    return null;
  }
}

/**
 * A FIFO offline queue backed by a single `localStorage` key. Each entry is a
 * serialized envelope JSON string. The queue is byte-capped: when enqueuing
 * would exceed `maxBytes`, the OLDEST entries are evicted first.
 */
export class OfflineQueue {
  private readonly maxBytes: number;
  private readonly storage: StorageLike | null;
  private readonly key: string;

  constructor(maxBytes: number, storage: StorageLike | null, key: string = QUEUE_KEY) {
    this.maxBytes = Math.max(0, maxBytes);
    this.storage = storage;
    this.key = key;
  }

  /** True when a real backing store is available. */
  get available(): boolean {
    return this.storage !== null;
  }

  private read(): string[] {
    if (!this.storage) return [];
    try {
      const raw = this.storage.getItem(this.key);
      if (!raw) return [];
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) return [];
      return parsed.filter((x): x is string => typeof x === 'string');
    } catch {
      return [];
    }
  }

  private write(entries: string[]): void {
    if (!this.storage) return;
    try {
      if (entries.length === 0) {
        this.storage.removeItem(this.key);
      } else {
        this.storage.setItem(this.key, JSON.stringify(entries));
      }
    } catch {
      // Quota exceeded or storage vanished — drop silently.
    }
  }

  /** Drop oldest entries until the total fits under `maxBytes`. Keeps >= 1. */
  private evict(entries: string[]): void {
    let total = 0;
    for (const e of entries) total += byteLength(e);
    while (entries.length > 1 && total > this.maxBytes) {
      const removed = entries.shift();
      if (removed === undefined) break;
      total -= byteLength(removed);
    }
  }

  /** Append a payload to the tail (newest), evicting from the head if needed. */
  enqueue(payload: string): void {
    if (!this.storage) return;
    const entries = this.read();
    entries.push(payload);
    this.evict(entries);
    this.write(entries);
  }

  /** Remove and return every entry, oldest first. */
  drain(): string[] {
    const entries = this.read();
    if (entries.length) this.write([]);
    return entries;
  }

  /** Non-destructive read of the current entries, oldest first. */
  peek(): string[] {
    return this.read();
  }

  /** Number of queued entries. */
  size(): number {
    return this.read().length;
  }

  /** Total UTF-8 byte size of the queue. */
  byteSize(): number {
    let total = 0;
    for (const e of this.read()) total += byteLength(e);
    return total;
  }

  clear(): void {
    this.write([]);
  }
}
