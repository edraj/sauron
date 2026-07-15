/**
 * Bounded in-memory send buffer with opt-in disk persistence.
 *
 * Items accumulate here between flushes. The buffer is byte-capped: when a push
 * would exceed `maxBytes`, the oldest items are dropped first (drop-oldest ring)
 * so a stalled ingest can never grow memory without bound.
 *
 * When `offlineDir` is set, every queued item is also written to a FIFO file so
 * that a crash mid-outage doesn't lose it: a fresh instance reloads the pending
 * files on construction (at-least-once across restarts). Files are deleted only
 * once their batch is committed (delivered or intentionally dropped). Disk
 * persistence is off by default — servers shouldn't be forced into filesystem
 * assumptions (ephemeral containers).
 *
 * The flush cycle is: `push` (many) → `drain` (take a batch, moving it "in
 * flight") → `commit` (delivered/dropped: forget it, delete its files) or
 * `restore` (send failed: put it back at the head, keep its files).
 */

import { existsSync, mkdirSync, readFileSync, readdirSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import type { EnvelopeItem } from './types.js';

export interface BoundedQueueOptions {
  /** Drop-oldest byte cap for the in-memory buffer. */
  maxBytes: number;
  /** When set, persist pending items here (FIFO) for at-least-once across restarts. */
  offlineDir?: string | null;
}

interface Entry {
  item: EnvelopeItem;
  /** Byte size of the serialized item (drives the byte cap). */
  size: number;
  /** Absolute path of the on-disk copy, or `null` when memory-only. */
  file: string | null;
}

const FILE_SUFFIX = '.env.json';

function pad(seq: number): string {
  return String(seq).padStart(16, '0');
}

export class BoundedQueue {
  private entries: Entry[] = [];
  /** The batch currently handed to the transport, awaiting commit/restore. */
  private inflight: Entry[] = [];
  private byteCount = 0;
  private seq = 0;
  private readonly maxBytes: number;
  private readonly offlineDir: string | null;

  constructor(options: BoundedQueueOptions) {
    this.maxBytes = Math.max(0, options.maxBytes);
    this.offlineDir = options.offlineDir ?? null;
    if (this.offlineDir) {
      mkdirSync(this.offlineDir, { recursive: true });
      this.reload();
    }
  }

  /** Reload persisted items (FIFO by filename) from a previous run. */
  private reload(): void {
    const dir = this.offlineDir;
    if (!dir || !existsSync(dir)) return;
    const files = readdirSync(dir)
      .filter((f) => f.endsWith(FILE_SUFFIX))
      .sort();
    for (const name of files) {
      const full = join(dir, name);
      try {
        const raw = readFileSync(full, 'utf8');
        const item = JSON.parse(raw) as EnvelopeItem;
        this.entries.push({ item, size: Buffer.byteLength(raw), file: full });
        this.byteCount += Buffer.byteLength(raw);
      } catch {
        // Corrupt/partial file — drop it so it can't wedge the queue.
        this.tryUnlink(full);
      }
      const seqPart = Number.parseInt(name, 10);
      if (Number.isFinite(seqPart) && seqPart >= this.seq) this.seq = seqPart + 1;
    }
    this.evict();
  }

  /** Enqueue an item; persists it when offline, then enforces the byte cap. */
  push(item: EnvelopeItem): void {
    const raw = JSON.stringify(item);
    const size = Buffer.byteLength(raw);
    let file: string | null = null;
    if (this.offlineDir) {
      file = join(this.offlineDir, `${pad(this.seq++)}${FILE_SUFFIX}`);
      try {
        writeFileSync(file, raw);
      } catch {
        file = null; // best-effort persistence; never block the send path
      }
    }
    this.entries.push({ item, size, file });
    this.byteCount += size;
    this.evict();
  }

  /** Drop oldest entries until the byte cap is satisfied. */
  private evict(): void {
    while (this.byteCount > this.maxBytes && this.entries.length > 0) {
      const dropped = this.entries.shift() as Entry;
      this.byteCount -= dropped.size;
      if (dropped.file) this.tryUnlink(dropped.file);
    }
  }

  /**
   * Take the whole queued batch, moving it "in flight". The transport must
   * later {@link commit} it (delivered/dropped) or {@link restore} it (failed).
   */
  drain(): EnvelopeItem[] {
    const batch = this.entries;
    this.entries = [];
    this.byteCount = 0;
    if (batch.length > 0) this.inflight.push(...batch);
    return batch.map((e) => e.item);
  }

  /** Delivered (or intentionally dropped): forget the in-flight batch and delete its files. */
  commit(): void {
    for (const e of this.inflight) {
      if (e.file) this.tryUnlink(e.file);
    }
    this.inflight = [];
  }

  /** Send failed: return the in-flight batch to the head of the queue, keeping its files. */
  restore(): void {
    if (this.inflight.length === 0) return;
    this.entries = [...this.inflight, ...this.entries];
    for (const e of this.inflight) this.byteCount += e.size;
    this.inflight = [];
    this.evict();
  }

  private tryUnlink(file: string): void {
    try {
      unlinkSync(file);
    } catch {
      // Already gone / never written — ignore.
    }
  }

  /** Bytes currently buffered (excludes the in-flight batch). */
  get bytes(): number {
    return this.byteCount;
  }

  /** Number of queued items (excludes the in-flight batch). */
  get size(): number {
    return this.entries.length;
  }
}
