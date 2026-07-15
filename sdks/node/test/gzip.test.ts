import { describe, it, expect, beforeEach } from 'vitest';
import { gunzipSync } from 'node:zlib';

import { maybeGzip } from '../src/gzip.js';
import { SauronClient } from '../src/client.js';
import type { Envelope, FetchLike } from '../src/types.js';

describe('maybeGzip', () => {
  it('gzips a body over the threshold and sets Content-Encoding', () => {
    const body = 'x'.repeat(2000);
    const { body: out, headers } = maybeGzip(body, 1024);
    expect(Buffer.isBuffer(out)).toBe(true);
    expect(headers['Content-Encoding']).toBe('gzip');
    // Round-trips back to the original JSON string.
    expect(gunzipSync(out as Buffer).toString()).toBe(body);
    // The gzipped payload is actually smaller than the input.
    expect((out as Buffer).byteLength).toBeLessThan(Buffer.byteLength(body));
  });

  it('passes a sub-threshold body through untouched with no header', () => {
    const body = 'small';
    const { body: out, headers } = maybeGzip(body, 1024);
    expect(out).toBe(body);
    expect(typeof out).toBe('string');
    expect(headers).toEqual({});
    expect('Content-Encoding' in headers).toBe(false);
  });

  it('uses byte length (not char length) for the threshold check', () => {
    // 600 multi-byte chars = 1800 bytes > 1024, even though length is 600.
    const body = '€'.repeat(600);
    expect(body.length).toBeLessThan(1024);
    const { headers } = maybeGzip(body, 1024);
    expect(headers['Content-Encoding']).toBe('gzip');
  });

  it('treats a body exactly at the threshold as passthrough', () => {
    const body = 'a'.repeat(1024);
    const { body: out, headers } = maybeGzip(body, 1024);
    expect(out).toBe(body);
    expect(headers).toEqual({});
  });
});

/** A fake fetch that decodes gzip when the header is present. */
function makeDecodingFetch(status = 200) {
  const calls: { headers: Record<string, string>; envelope: Envelope }[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    const raw =
      init.headers['Content-Encoding'] === 'gzip'
        ? gunzipSync(init.body as Uint8Array).toString()
        : (init.body as string);
    calls.push({ headers: init.headers, envelope: JSON.parse(raw) as Envelope });
    return { status, ok: status >= 200 && status < 300 };
  };
  return { fetchImpl, calls };
}

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

describe('transport gzip wiring', () => {
  let fake: ReturnType<typeof makeDecodingFetch>;

  beforeEach(() => {
    fake = makeDecodingFetch();
  });

  it('gzips a large envelope and the ingest still decodes it', async () => {
    const client = new SauronClient({
      dsn: DSN,
      flushInterval: 0,
      fetchImpl: fake.fetchImpl,
      gzipThresholdBytes: 16,
    });
    client.track('big_event', 'u1', { blob: 'y'.repeat(500) });
    await client.flush();

    expect(fake.calls).toHaveLength(1);
    expect(fake.calls[0].headers['Content-Encoding']).toBe('gzip');
    expect(fake.calls[0].headers['Content-Type']).toBe('application/json');
    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('event');
    expect(item.name).toBe('big_event');
  });

  it('leaves a small envelope uncompressed by default', async () => {
    const client = new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl: fake.fetchImpl });
    client.track('small', 'u1');
    await client.flush();

    expect(fake.calls).toHaveLength(1);
    expect('Content-Encoding' in fake.calls[0].headers).toBe(false);
  });
});
