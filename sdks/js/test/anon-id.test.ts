import { gunzipSync } from 'node:zlib';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  ANON_ID_KEY,
  getAnonymousId,
  getLastIdentified,
  getSessionId,
  hashIdentity,
  LAST_IDENTIFIED_KEY,
  resetAnonymousId,
  resetIdentity,
  rotateSessionId,
  setLastIdentified,
} from '../src/identity.js';
import * as Sauron from '../src/index.js';

/** Minimal writable localStorage stand-in; the SDK probes before using one. */
function installStorage(): Map<string, string> {
  const map = new Map<string, string>();
  (globalThis as Record<string, unknown>).localStorage = {
    getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  };
  return map;
}

describe('anonymous id', () => {
  let store: Map<string, string>;

  beforeEach(() => {
    store = installStorage();
    resetIdentity();
  });

  it('persists across page loads instead of being re-minted in memory', () => {
    const first = getAnonymousId();
    expect(store.get(ANON_ID_KEY)).toBe(first);
    // A fresh page load: the in-memory cache is gone, storage is not.
    resetIdentity();
    expect(getAnonymousId()).toBe(first);
  });

  it('keeps the anon_ prefix so existing data stays recognisable', () => {
    expect(getAnonymousId()).toMatch(/^anon_/);
  });

  it('resetAnonymousId mints a new one and persists it', () => {
    const first = getAnonymousId();
    const second = resetAnonymousId();
    expect(second).not.toBe(first);
    expect(store.get(ANON_ID_KEY)).toBe(second);
    expect(getAnonymousId()).toBe(second);
  });

  it('degrades to a per-process id with no writable storage', () => {
    delete (globalThis as Record<string, unknown>).localStorage;
    resetIdentity();
    const a = getAnonymousId();
    expect(a).toMatch(/^anon_/);
    expect(getAnonymousId()).toBe(a);
  });
});

/**
 * Minimal writable Storage stand-in, installed onto `globalThis` under `name`.
 *
 * The test environment here is vitest's `node` environment (see
 * vitest.config.ts), which — unlike jsdom — provides neither `localStorage`
 * nor `sessionStorage` as real globals, so both must be stubbed before any
 * code (SDK or test) references them.
 *
 * Coerces keys/values through `String()`, same as a real `Storage` (which
 * applies `ToString` on every write): a naive `Map`-backed stub that stores
 * raw references instead would silently hide any bug caused by that
 * coercion — e.g. a numeric id compared against its own persisted, now-string,
 * form.
 */
function installWebStorage(name: 'localStorage' | 'sessionStorage'): Map<string, string> {
  const map = new Map<string, string>();
  (globalThis as Record<string, unknown>)[name] = {
    getItem: (k: string) => (map.has(String(k)) ? (map.get(String(k)) as string) : null),
    setItem: (k: string, v: string) => void map.set(String(k), String(v)),
    removeItem: (k: string) => void map.delete(String(k)),
    clear: () => void map.clear(),
  };
  return map;
}

/**
 * A `localStorage` stand-in whose 1-byte writability probe (see `webStorage`
 * in `identity.ts`) succeeds — so storage is reported present/writable — but
 * whose write to `blockedKey` specifically throws, simulating a quota error
 * that only bites on the real write, not the SDK's own probe.
 */
function installQuotaLimitedStorage(blockedKey: string): Map<string, string> {
  const map = new Map<string, string>();
  (globalThis as Record<string, unknown>).localStorage = {
    getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
    setItem: (k: string, v: string) => {
      if (k === blockedKey) throw new Error('QuotaExceededError');
      map.set(k, v);
    },
    removeItem: (k: string) => void map.delete(k),
    clear: () => void map.clear(),
  };
  return map;
}

interface CapturedRequest {
  headers: Record<string, string>;
  body: string;
}

/** Decode an envelope body the SDK's transport may (or may not) have gzipped. */
function decodeEnvelopeBody(body: string, headers: Record<string, string>): string {
  return headers['Content-Encoding'] === 'gzip'
    ? gunzipSync(body as unknown as Uint8Array).toString('utf8')
    : body;
}

/**
 * Stub `globalThis.fetch` to capture outbound requests instead of sending
 * them, and return the array they land in. Must be called BEFORE `init()` —
 * the client captures the native `fetch` at construction time, so a stub
 * installed after would never be used (see wire-fixture.test.ts).
 */
function stubFetch(): CapturedRequest[] {
  const captured: CapturedRequest[] = [];
  globalThis.fetch = (async (_url: unknown, reqInit: RequestInit) => {
    captured.push({
      headers: (reqInit.headers ?? {}) as Record<string, string>,
      body: reqInit.body as unknown as string,
    });
    return new Response(null, { status: 202 });
  }) as unknown as typeof fetch;
  return captured;
}

// Captured once, before any test stubs `globalThis.fetch` — this is the TRUE
// original, restored explicitly in `afterEach` below (see the comment there
// for why that restore can't live inside the test itself).
const originalFetch = globalThis.fetch;

describe('identity switch', () => {
  beforeEach(() => {
    installWebStorage('localStorage');
    installWebStorage('sessionStorage');
    localStorage.clear();
    sessionStorage.clear();
    resetIdentity();
  });

  afterEach(() => {
    // Order matters. `teardown()` → `unpatchAll()` resets `globalThis.fetch`
    // to whatever was installed at `Sauron.init()` time — which, in the wire
    // tests below, is OUR stub, not the true original — so restoring the true
    // original must happen AFTER `teardown()`, never inside the test's own
    // `finally` (which would run first and just get overwritten back to the
    // stub). `wire-fixture.test.ts` gets this order right; mirror it here.
    Sauron.getClient()?.teardown();
    globalThis.fetch = originalFetch;
  });

  it('rotates the session id so a session never spans two people', () => {
    const first = getSessionId();
    const second = rotateSessionId();
    expect(second).not.toBe(first);
    expect(getSessionId()).toBe(second);
    expect(sessionStorage.getItem('sauron.session_id')).toBe(second);
  });

  it('remembers the last identified user across reloads', () => {
    expect(getLastIdentified()).toBeNull();
    setLastIdentified('u-42');
    // A fresh page load: the in-memory cache is gone, storage is not. Without
    // this reset, a memory-only implementation of getLastIdentified() would
    // also pass, defeating the entire point of persisting the key.
    resetIdentity();
    expect(getLastIdentified()).toBe('u-42');
    // The STORED bytes carry the format tag; `getLastIdentified()` strips it.
    // Pinned literally rather than built from LAST_IDENTIFIED_FORMAT, because
    // the Flutter SDK writes this exact string under this exact key and the
    // two must stay byte-compatible — a test that recomputes the prefix from
    // the constant would follow a one-sided change instead of catching it.
    expect(localStorage.getItem(LAST_IDENTIFIED_KEY)).toBe('v1:u-42');
  });

  // Without a format tag, a stored digest in a shape this build no longer
  // produces is indistinguishable from a DIFFERENT person's digest: both
  // compare unequal, both read as a switch. `hashIdentity` already changed
  // width once (8 hex digits -> 16), so this is the shipped-and-widened case,
  // not a hypothetical — and it would rotate the anon id and session for
  // every returning user on their next identify(), once, silently.
  // Each fixture is a value that would compare UNEQUAL to `hashIdentity`'s
  // current output for the person identifying below — the untagged one is
  // deliberately sara's digest, not ahmed's, so the behavioural assertions
  // fail (not just the `toBeNull()` one) when the tag check is removed.
  it.each([
    ['an untagged (pre-v1) value', 'b8f66470861ed579'],
    ['a newer format tag', 'v2:whatever-that-turns-out-to-be'],
    ['an empty payload behind a known tag', 'v1:'],
  ])('reads %s as "no previous identity", not as a switch', (_label, raw) => {
    localStorage.setItem(LAST_IDENTIFIED_KEY, raw);
    resetIdentity();

    expect(getLastIdentified()).toBeNull();

    const client = Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0 },
    });
    client.getDistinctId();
    const anonBefore = getAnonymousId();
    const alias = client.prepareIdentify('ahmed');

    expect(alias).toBe(anonBefore);
    expect(getAnonymousId()).toBe(anonBefore);
    // …and the unreadable entry is replaced with a readable one, so this
    // degrades for exactly one identify rather than permanently.
    expect(localStorage.getItem(LAST_IDENTIFIED_KEY)).toBe(`v1:${hashIdentity('ahmed')}`);
  });

  it('falls back to the in-memory value when the real write silently fails (quota)', () => {
    installQuotaLimitedStorage(LAST_IDENTIFIED_KEY);

    setLastIdentified('u-7');
    // The real write failed — storage never actually holds the value...
    expect(localStorage.getItem(LAST_IDENTIFIED_KEY)).toBeNull();
    // ...but getLastIdentified() must still see it, from the in-memory cache
    // that `setLastIdentified` updates unconditionally.
    expect(getLastIdentified()).toBe('u-7');
  });

  it('mints a fresh anon id and rotates the session when a different user identifies', () => {
    const client = Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0 },
    });

    const ahmedAnon = client.getDistinctId(); // marks the anon id used
    expect(client.prepareIdentify('ahmed')).toBe(ahmedAnon);

    const sessionBeforeSwitch = getSessionId();

    // Logout was never wired. Sara browses, then logs in.
    client.getDistinctId();
    const saraAlias = client.prepareIdentify('sara');

    expect(saraAlias).toBeNull();
    expect(getAnonymousId()).not.toBe(ahmedAnon);
    // The session must not survive the switch either — otherwise one
    // `sessions` row could still end up representing both ahmed and sara.
    expect(getSessionId()).not.toBe(sessionBeforeSwitch);
  });

  it('treats a numeric id as stable across identify calls (Storage coerces it to a string)', () => {
    const client = Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0 },
    });

    client.getDistinctId();
    const first = client.prepareIdentify(42 as unknown as string);
    expect(first).not.toBeNull();

    // The SAME numeric id again — must not be misread as a switch just
    // because the round trip through Storage turned it into the string "42".
    client.getDistinctId();
    const second = client.prepareIdentify(42 as unknown as string);

    expect(second).toBe(first);
    expect(getLastIdentified()).toBe(hashIdentity('42'));
  });

  it('detects a real switch away from an empty-string identity (not falsy-skipped)', () => {
    const client = Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0 },
    });

    client.getDistinctId();
    const emptyAlias = client.prepareIdentify('');
    // First-ever identify (even with an empty id) is not itself a switch.
    expect(emptyAlias).not.toBeNull();

    const anonAfterEmpty = getAnonymousId();
    client.getDistinctId();
    const bobAlias = client.prepareIdentify('bob');

    // '' is a falsy STRING, not "no identity yet" (that's `null`) — a later,
    // real switch away from it must still be detected as one.
    expect(bobAlias).toBeNull();
    expect(getAnonymousId()).not.toBe(anonAfterEmpty);
  });

  it('reset() clears the last identified user and rotates the session id', () => {
    const client = Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0 },
    });

    client.getDistinctId();
    Sauron.identify('ahmed');
    expect(getLastIdentified()).toBe(hashIdentity('ahmed'));

    const sessionBefore = getSessionId();
    client.reset();

    expect(getLastIdentified()).toBeNull();
    expect(localStorage.getItem(LAST_IDENTIFIED_KEY)).toBeNull();
    expect(getSessionId()).not.toBe(sessionBefore);
  });

  it('the public identify() sends the real anon id for the first user and null for a switch', async () => {
    const captured = stubFetch();

    Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0, maxBatch: 1000 },
    });

    Sauron.track('page_view'); // marks the anon id used
    Sauron.identify('ahmed');
    expect(await Sauron.flush(5000)).toBe(true);

    // Logout was never wired. Sara browses under ahmed's device, then logs in.
    Sauron.track('page_view');
    Sauron.identify('sara');
    expect(await Sauron.flush(5000)).toBe(true);

    expect(captured).toHaveLength(2);
    const envelope1 = JSON.parse(decodeEnvelopeBody(captured[0].body, captured[0].headers)) as {
      items: Array<Record<string, unknown>>;
    };
    const envelope2 = JSON.parse(decodeEnvelopeBody(captured[1].body, captured[1].headers)) as {
      items: Array<Record<string, unknown>>;
    };
    const identify1 = envelope1.items.find((i) => i.type === 'identify');
    const identify2 = envelope2.items.find((i) => i.type === 'identify');

    expect(identify1?.anonymous_id).toEqual(expect.stringMatching(/^anon_/));
    expect(identify2?.anonymous_id).toBeNull();
  });

  it('coerces a numeric id to a string before it reaches the wire', async () => {
    const captured = stubFetch();

    Sauron.init({
      dsn: 'https://pub@example.test/1',
      transport: { flushIntervalMs: 0, maxBatch: 1000 },
    });

    // A plain-JS caller can pass a number — nothing at runtime enforces the
    // TS `id: string` signature (`Sauron.identify(user.id)` is common).
    Sauron.identify(123 as unknown as string);
    expect(await Sauron.flush(5000)).toBe(true);

    expect(captured).toHaveLength(1);
    const envelope = JSON.parse(decodeEnvelopeBody(captured[0].body, captured[0].headers)) as {
      items: Array<Record<string, unknown>>;
    };
    const identifyItem = envelope.items.find((i) => i.type === 'identify');

    // Must be the STRING "123", never the JSON number 123: `distinct_id` is a
    // non-`Option` Rust `String` on the wire, so a number there fails to
    // deserialize and rejects the WHOLE envelope (400 invalid_envelope),
    // taking every other item batched alongside it down too.
    expect(identifyItem?.distinct_id).toBe('123');
    expect(typeof identifyItem?.distinct_id).toBe('string');
  });

  it('restores the true fetch after a fetch-stubbing test tears down', () => {
    // Regression coverage for the `afterEach` ordering itself, placed AFTER
    // the two fetch-stubbing tests above so it actually observes their
    // teardown. `teardown()`'s `unpatchAll()` resets `globalThis.fetch` to
    // whatever was installed at `init()` time — a STUB, in those two tests —
    // so if the explicit restore ran before `teardown()` (or inside the
    // test's own `finally`, which runs before `afterEach`) instead of after
    // it, this file would silently end with the stub still installed.
    expect(globalThis.fetch).toBe(originalFetch);
  });
});
