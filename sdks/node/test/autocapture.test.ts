import { describe, it, expect, beforeEach } from 'vitest';

import { SauronClient } from '../src/client.js';
import { installAutoCapture, installShutdownHooks } from '../src/autocapture.js';
import type { Envelope, FetchLike, InitOptions, ProcessLike } from '../src/types.js';
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

/** A minimal in-memory stand-in for Node's `process` (no real listeners/exit). */
class FakeProcess implements ProcessLike {
  readonly listenersMap = new Map<string, Array<(...args: unknown[]) => void>>();
  readonly exitCalls: number[] = [];

  on(event: string, listener: (...args: unknown[]) => void): this {
    const list = this.listenersMap.get(event) ?? [];
    list.push(listener);
    this.listenersMap.set(event, list);
    return this;
  }

  removeListener(event: string, listener: (...args: unknown[]) => void): this {
    const list = this.listenersMap.get(event);
    if (list) this.listenersMap.set(event, list.filter((l) => l !== listener));
    return this;
  }

  listeners(event: string): Array<(...args: unknown[]) => void> {
    return (this.listenersMap.get(event) ?? []).slice();
  }

  exit(code = 0): void {
    this.exitCalls.push(code);
  }

  emit(event: string, ...args: unknown[]): void {
    for (const l of this.listeners(event)) l(...args);
  }
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('installAutoCapture', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('registers uncaughtException + unhandledRejection listeners', () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installAutoCapture(client, { process: proc });
    expect(proc.listeners('uncaughtException')).toHaveLength(1);
    expect(proc.listeners('unhandledRejection')).toHaveLength(1);
  });

  it('captures an uncaught exception with mechanism.handled=false', async () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installAutoCapture(client, { process: proc });

    proc.emit('uncaughtException', new Error('boom'));
    await client.flush();

    const items = fake.calls.flatMap((c) => c.envelope.items) as any[];
    const err = items.find((i) => i.type === 'error');
    expect(err).toBeDefined();
    expect(err.exception.mechanism.handled).toBe(false);
    expect(err.level).toBe('fatal');
  });

  it('preserves the default crash exit when it is the sole handler', async () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installAutoCapture(client, { process: proc });

    proc.emit('uncaughtException', new Error('fatal'));
    await client.flush();
    await tick();

    expect(proc.exitCalls).toContain(1);
  });

  it('does not exit when another uncaughtException handler is present', async () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    proc.on('uncaughtException', () => {
      /* a competing handler owns the crash */
    });
    installAutoCapture(client, { process: proc });

    proc.emit('uncaughtException', new Error('shared'));
    await client.flush();
    await tick();

    expect(proc.exitCalls).toHaveLength(0);
  });

  it('captures an unhandled rejection reason with handled=false', async () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installAutoCapture(client, { process: proc });

    proc.emit('unhandledRejection', new Error('rejected'), Promise.resolve());
    await client.flush();

    const items = fake.calls.flatMap((c) => c.envelope.items) as any[];
    const err = items.find((i) => i.type === 'error');
    expect(err).toBeDefined();
    expect(err.exception.mechanism.handled).toBe(false);
    expect(err.exception.value).toBe('rejected');
    // A rejection alone must not terminate the process.
    expect(proc.exitCalls).toHaveLength(0);
  });

  it('is idempotent — installing twice does not double-register', () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installAutoCapture(client, { process: proc });
    installAutoCapture(client, { process: proc });
    expect(proc.listeners('uncaughtException')).toHaveLength(1);
    expect(proc.listeners('unhandledRejection')).toHaveLength(1);
  });

  it('returns an uninstaller that removes the listeners', () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    const uninstall = installAutoCapture(client, { process: proc });
    uninstall();
    expect(proc.listeners('uncaughtException')).toHaveLength(0);
    expect(proc.listeners('unhandledRejection')).toHaveLength(0);
  });
});

describe('installShutdownHooks', () => {
  let fake: ReturnType<typeof makeFakeFetch>;

  beforeEach(() => {
    fake = makeFakeFetch();
  });

  it('registers beforeExit/SIGTERM/SIGINT listeners', () => {
    const client = newClient(fake.fetchImpl);
    const proc = new FakeProcess();
    installShutdownHooks(client, { process: proc });
    expect(proc.listeners('beforeExit')).toHaveLength(1);
    expect(proc.listeners('SIGTERM')).toHaveLength(1);
    expect(proc.listeners('SIGINT')).toHaveLength(1);
  });

  it('closes the client on SIGTERM then exits with the conventional code', async () => {
    const client = newClient(fake.fetchImpl);
    let closed = 0;
    const realClose = client.close.bind(client);
    client.close = async () => {
      closed += 1;
      return realClose();
    };
    const proc = new FakeProcess();
    installShutdownHooks(client, { process: proc });

    proc.emit('SIGTERM', 'SIGTERM');
    await tick();

    expect(closed).toBe(1);
    expect(proc.exitCalls).toContain(143);
  });

  it('closes the client on beforeExit without forcing an exit', async () => {
    const client = newClient(fake.fetchImpl);
    let closed = 0;
    const realClose = client.close.bind(client);
    client.close = async () => {
      closed += 1;
      return realClose();
    };
    const proc = new FakeProcess();
    installShutdownHooks(client, { process: proc });

    proc.emit('beforeExit', 0);
    await tick();

    expect(closed).toBe(1);
    expect(proc.exitCalls).toHaveLength(0);
  });
});

describe('client wiring (opt-in only)', () => {
  const fake = makeFakeFetch();

  it('installs process hooks only when opted in, and removes them on close', async () => {
    const beforeUncaught = process.listenerCount('uncaughtException');
    const beforeSigterm = process.listenerCount('SIGTERM');

    const off = newClient(fake.fetchImpl, {
      autoCaptureUnhandled: false,
      autoShutdown: false,
    });
    expect(process.listenerCount('uncaughtException')).toBe(beforeUncaught);
    expect(process.listenerCount('SIGTERM')).toBe(beforeSigterm);
    await off.close();

    const on = newClient(fake.fetchImpl, {
      autoCaptureUnhandled: true,
      autoShutdown: true,
    });
    expect(process.listenerCount('uncaughtException')).toBe(beforeUncaught + 1);
    expect(process.listenerCount('SIGTERM')).toBe(beforeSigterm + 1);

    await on.close();
    expect(process.listenerCount('uncaughtException')).toBe(beforeUncaught);
    expect(process.listenerCount('SIGTERM')).toBe(beforeSigterm);
  });
});
