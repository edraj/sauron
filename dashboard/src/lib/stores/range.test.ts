import { beforeEach, describe, expect, it, vi } from 'vitest';
import { MAX_RELATIVE_DAYS, lastDays, type DateRangeValue } from '../models/date-range';
import { CURRENT_KEY, SAVED_KEY, SAVED_MAX, RangeStore } from './range.svelte';

/** A minimal in-memory localStorage so the persistence branch runs for real. */
class FakeStorage {
  map = new Map<string, string>();
  getItem(k: string) {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.map.set(k, v);
  }
  removeItem(k: string) {
    this.map.delete(k);
  }
}

function withStorage(storage: unknown): FakeStorage {
  vi.stubGlobal('window', { localStorage: storage } as unknown as Window & typeof globalThis);
  return storage as FakeStorage;
}

const AUG12: DateRangeValue = {
  kind: 'absolute',
  preset: 'day',
  from: '2026-08-12T00:00:00.000Z',
  to: '2026-08-13T00:00:00.000Z',
};

beforeEach(() => {
  vi.unstubAllGlobals();
});

describe('the shared selection', () => {
  /**
   * The rule that keeps first load byte-identical to before this feature:
   * pages do NOT share a default (Overview starts at 30 days, Issues at its
   * widest), so until the user chooses something each page keeps its own.
   */
  it('is absent until the user chooses, so pages keep their own defaults', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    expect(s.value).toBeNull();
    expect(s.effective(3650)).toEqual(lastDays(3650));
    expect(s.effective(30)).toEqual(lastDays(30));
  });

  it('applies everywhere once chosen', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.set(lastDays(7));
    expect(s.effective(3650)).toEqual(lastDays(7));
    expect(s.effective(30)).toEqual(lastDays(7));
  });

  it('persists and restores across a reload', () => {
    const store = withStorage(new FakeStorage());
    new RangeStore().set(AUG12);
    expect(store.getItem(CURRENT_KEY)).toContain('2026-08-12');
    expect(new RangeStore().value).toEqual(AUG12);
  });

  it('clear() returns pages to their own defaults', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.set(AUG12);
    s.clear();
    expect(s.value).toBeNull();
    expect(s.effective(30)).toEqual(lastDays(30));
  });
});

describe('reading a hostile store', () => {
  /**
   * These values outlive the code that wrote them. Anything unreadable falls
   * back to "no choice yet" rather than throwing — a bad preference must not
   * break every page in the app.
   */
  it('drops a corrupt, mistyped or stale entry', () => {
    for (const raw of [
      'not json',
      '[]',
      '{"kind":"nope"}',
      '{"kind":"last","days":0}',
      `{"kind":"last","days":${MAX_RELATIVE_DAYS + 1}}`,
      '{"kind":"absolute","preset":"day","from":"2026-08-05T00:00:00.000Z","to":"2026-08-01T00:00:00.000Z"}',
    ]) {
      const store = withStorage(new FakeStorage());
      store.setItem(CURRENT_KEY, raw);
      expect(new RangeStore().value, raw).toBeNull();
    }
  });

  /** Private-mode Safari throws from `localStorage` on read as well as write. */
  it('survives localStorage throwing', () => {
    withStorage({
      getItem() {
        throw new Error('SecurityError');
      },
      setItem() {
        throw new Error('QuotaExceeded');
      },
      removeItem() {
        throw new Error('SecurityError');
      },
    });
    const s = new RangeStore();
    expect(s.value).toBeNull();
    // The write still updates the in-memory value — losing persistence must
    // not lose the click.
    s.set(lastDays(7));
    expect(s.value).toEqual(lastDays(7));
  });
});

describe('saved ranges', () => {
  it('saves, lists newest first, and restores', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.save('August 12', AUG12);
    s.save('Last week', { ...AUG12, preset: 'week', to: '2026-08-19T00:00:00.000Z' });
    expect(s.saved.map((r) => r.name)).toEqual(['Last week', 'August 12']);
    expect(new RangeStore().saved).toHaveLength(2);
  });

  it('only saves absolute ranges — a preset needs no name', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.save('Last 7', lastDays(7));
    expect(s.saved).toHaveLength(0);
  });

  it('refuses a blank name and trims the rest', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.save('   ', AUG12);
    expect(s.saved).toHaveLength(0);
    s.save('  Trimmed  ', AUG12);
    expect(s.saved[0].name).toBe('Trimmed');
  });

  it('removes by id', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    s.save('One', AUG12);
    const id = s.saved[0].id;
    s.remove(id);
    expect(s.saved).toHaveLength(0);
    expect(new RangeStore().saved).toHaveLength(0);
  });

  /**
   * Unbounded growth in localStorage is a quota error waiting to happen, and
   * the quota error lands on an unrelated write.
   */
  it('caps the list, dropping the oldest', () => {
    withStorage(new FakeStorage());
    const s = new RangeStore();
    for (let i = 0; i < SAVED_MAX + 5; i++) s.save(`r${i}`, AUG12);
    expect(s.saved).toHaveLength(SAVED_MAX);
    expect(s.saved[0].name).toBe(`r${SAVED_MAX + 4}`);
    expect(s.saved.some((r) => r.name === 'r0')).toBe(false);
  });

  it('drops individually-invalid saved entries without discarding the rest', () => {
    const store = withStorage(new FakeStorage());
    store.setItem(
      SAVED_KEY,
      JSON.stringify([
        { id: 'a', name: 'good', kind: 'absolute', preset: 'day', from: AUG12.from, to: AUG12.to },
        { id: 'b', name: 'inverted', kind: 'absolute', preset: 'day', from: AUG12.to, to: AUG12.from },
        { id: 'c', name: '', kind: 'absolute', preset: 'day', from: AUG12.from, to: AUG12.to },
        'not an object',
      ]),
    );
    const s = new RangeStore();
    expect(s.saved.map((r) => r.name)).toEqual(['good']);
  });

  it('drops a saved list that is not a list at all', () => {
    const store = withStorage(new FakeStorage());
    store.setItem(SAVED_KEY, '{"nope":true}');
    expect(new RangeStore().saved).toEqual([]);
  });
});
