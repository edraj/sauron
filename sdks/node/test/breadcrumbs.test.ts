import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import { withScope } from '../src/scope.js';
import type { Envelope, FetchLike, InitOptions } from '../src/types.js';
import { bodyToString } from './helpers.js';

interface Captured {
  envelope: Envelope;
}

function makeFakeFetch() {
  const calls: Captured[] = [];
  const fetchImpl: FetchLike = async (_url, init) => {
    calls.push({ envelope: JSON.parse(bodyToString(init)) as Envelope });
    return { status: 200, ok: true };
  };
  return { fetchImpl, calls };
}

const DSN = 'https://pub_key_abc@ingest.sauron.dev/99';

function newClient(fetchImpl: FetchLike, overrides: Partial<InitOptions> = {}) {
  return new SauronClient({ dsn: DSN, flushInterval: 0, fetchImpl, ...overrides });
}

describe('breadcrumbs', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('attaches an added breadcrumb to a captured error', async () => {
    const client = newClient(fake.fetchImpl);
    await withScope(async () => {
      client.addBreadcrumb({ message: 'x', category: 'ui' });
      client.captureException(new Error('boom'));
      await client.flush();
    });

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.type).toBe('error');
    expect(item.breadcrumbs).toHaveLength(1);
    expect(item.breadcrumbs[0].message).toBe('x');
    expect(item.breadcrumbs[0].category).toBe('ui');
  });

  it('stamps an ISO timestamp on breadcrumbs', async () => {
    const client = newClient(fake.fetchImpl);
    await withScope(async () => {
      client.addBreadcrumb({ message: 'stamped' });
      client.captureException(new Error('boom'));
      await client.flush();
    });

    const [item] = fake.calls[0].envelope.items as any[];
    const ts = item.breadcrumbs[0].timestamp;
    expect(typeof ts).toBe('string');
    expect(new Date(ts).toISOString()).toBe(ts);
  });

  it('drops a breadcrumb when beforeBreadcrumb returns null', async () => {
    const client = newClient(fake.fetchImpl, {
      beforeBreadcrumb: (crumb) => (crumb.message === 'secret' ? null : crumb),
    });
    await withScope(async () => {
      client.addBreadcrumb({ message: 'secret' });
      client.addBreadcrumb({ message: 'kept' });
      client.captureException(new Error('boom'));
      await client.flush();
    });

    const [item] = fake.calls[0].envelope.items as any[];
    expect(item.breadcrumbs.map((b: any) => b.message)).toEqual(['kept']);
  });
});
