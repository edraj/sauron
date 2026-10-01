import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createContext, runInContext } from 'node:vm';
import { gunzipSync } from 'node:zlib';
import { parse } from 'acorn';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as api from '../src/index.js';

/**
 * The script-tag builds (`scripts/build-global.mjs`) go through their own
 * pipeline — esbuild, then SWC for the ES5 file — so nothing else in this
 * suite, which imports `src/`, says anything about them. Build both into a temp
 * dir and run each in a fresh VM realm, the way a page runs a classic script.
 *
 * The GTM loader snippet is read from the README as written, so the snippet
 * people paste and the replay in `src/global.ts` are tested together.
 */

const FILES = ['sauron.min.js', 'sauron.es5.min.js'] as const;
const code = {} as Record<(typeof FILES)[number], string>;
let outdir = '';

const DSN = 'https://pk_test@ingest.example.com/42';

/** The `<script>` body of the README's GTM loader block. */
const LOADER = (() => {
  const readme = readFileSync(fileURLToPath(new URL('../README.md', import.meta.url)), 'utf8');
  const block =
    /<!-- test\/global-bundle\.test\.ts runs this snippet[^>]*-->\s*```html\n<script>\n([\s\S]*?)<\/script>\n```/.exec(
      readme,
    );
  if (!block) throw new Error('README: no GTM loader block after its marker comment');
  return block[1];
})();

beforeAll(() => {
  outdir = mkdtempSync(join(tmpdir(), 'sauron-global-'));
  const script = fileURLToPath(new URL('../scripts/build-global.mjs', import.meta.url));
  execFileSync(process.execPath, [script, '--outdir', outdir], { stdio: 'pipe' });
  for (const file of FILES) code[file] = readFileSync(join(outdir, file), 'utf8');
}, 60_000);

afterAll(() => {
  if (outdir) rmSync(outdir, { recursive: true, force: true });
});

/** A fresh realm whose `self` and `window` are its own global, like a page. */
function page(globals: Record<string, unknown> = {}) {
  const sandbox: Record<string, unknown> = { ...globals };
  sandbox.self = sandbox;
  sandbox.window = sandbox;
  const context = createContext(sandbox);
  return { sandbox, run: (src: string): unknown => runInContext(src, context) };
}

interface Request {
  url: string;
  init: { headers: Record<string, string>; body: Uint8Array | string };
}

/**
 * A page that records what the SDK posts, with the DOM the loader touches:
 * `document.createElement` / `head.appendChild` for its `<script>`, which is
 * recorded rather than fetched — a test "loads" it by running the bundle.
 */
function browserPage() {
  const requests: Request[] = [];
  const scripts: Array<{ src?: string; async?: boolean }> = [];
  const thrown: unknown[] = [];
  const listeners: Record<string, Array<(event: unknown) => void>> = {};
  const p = page({
    fetch: (url: string, init: Request['init']) => {
      requests.push({ url, init });
      return Promise.resolve({ status: 200, headers: { get: () => null } });
    },
    addEventListener: (type: string, listener: (event: unknown) => void) => {
      (listeners[type] = listeners[type] ?? []).push(listener);
    },
    removeEventListener: (type: string, listener: (event: unknown) => void) => {
      listeners[type] = (listeners[type] ?? []).filter((l) => l !== listener);
    },
    document: {
      createElement: () => ({}),
      head: { appendChild: (script: { src?: string; async?: boolean }) => scripts.push(script) },
    },
    // Real timers, but an error thrown from one is recorded instead of
    // crashing the run: that is how a queued call's error surfaces.
    setTimeout: (fn: () => void, ms?: number) =>
      setTimeout(() => {
        try {
          fn();
        } catch (err) {
          thrown.push(err);
        }
      }, ms),
    clearTimeout,
    setInterval,
    clearInterval,
    TextEncoder,
    URL,
  });

  /** close() the SDK, then decode every envelope it posted. */
  async function sent() {
    expect(await (p.run('Sauron.close(5000)') as Promise<boolean>)).toBe(true);
    return requests.map(({ init }) =>
      JSON.parse(typeof init.body === 'string' ? init.body : gunzipSync(init.body).toString('utf8')),
    );
  }

  /**
   * An uncaught error, delivered as a browser does: to the `onerror` property
   * handler, then to every `error` listener. `errorSrc` builds the thrown value
   * inside the page; a cross-origin script's error arrives with none, and no
   * location either.
   */
  function pageError(errorSrc: string, message?: string): void {
    const error = p.run(errorSrc) as Error | null;
    const event = {
      type: 'error',
      message: message ?? `Uncaught ${String(error)}`,
      filename: error ? 'https://shop.example/app.js' : '',
      lineno: error ? 3 : 0,
      colno: error ? 7 : 0,
      error,
    };
    const onerror = p.sandbox.onerror as OnErrorEventHandlerNonNull | undefined;
    onerror?.(event.message, event.filename, event.lineno, event.colno, error ?? undefined);
    for (const listener of listeners.error ?? []) listener(event);
  }

  /** An unhandled rejection, delivered as a browser does. */
  function pageRejection(reasonSrc: string): void {
    const event = { type: 'unhandledrejection', reason: p.run(reasonSrc) };
    const handler = p.sandbox.onunhandledrejection as ((event: unknown) => void) | undefined;
    handler?.(event);
    for (const listener of listeners.unhandledrejection ?? []) listener(event);
  }

  return { ...p, requests, scripts, thrown, listeners, sent, pageError, pageRejection };
}

/** Every item posted, across envelopes. */
async function items(p: ReturnType<typeof browserPage>): Promise<Array<Record<string, any>>> {
  return (await p.sent()).flatMap((envelope: { items: Array<Record<string, any>> }) => envelope.items);
}

describe('sauron.es5.min.js', () => {
  it('parses as ES5', () => {
    // What a GTM Custom HTML tag checks before it lets the container publish.
    expect(() => parse(code['sauron.es5.min.js'], { ecmaVersion: 5, sourceType: 'script' })).not.toThrow();
  });
});

describe('README GTM loader', () => {
  it('parses as ES5', () => {
    // It is pasted into the tag itself, so GTM checks it too.
    expect(() => parse(LOADER, { ecmaVersion: 5, sourceType: 'script' })).not.toThrow();
  });

  it('stubs every facade method except the getters', () => {
    // A method missing here is a TypeError in any tag that calls it before
    // the file loads.
    const p = browserPage();
    p.run(LOADER);
    const getters = ['getScreen', 'getWorkflow', 'getClient'];
    expect(Object.keys(p.sandbox.Sauron as object).filter((key) => key !== 'q').sort()).toEqual(
      Object.keys(api.Sauron)
        .filter((key) => !getters.includes(key))
        .sort(),
    );
  });

  it('injects the file once, however often the tag fires', () => {
    const p = browserPage();
    p.run(LOADER);
    p.run(LOADER);
    expect(p.scripts).toHaveLength(1);
    expect(p.scripts[0].async).toBe(true);
    expect(p.scripts[0].src).toMatch(/\/dist\/sauron\.min\.js$/);
    expect(p.listeners.error).toHaveLength(1);
    expect(p.listeners.unhandledrejection).toHaveLength(1);
    const queue = (p.sandbox.Sauron as { q: unknown[][] }).q;
    expect(queue.map(([name]) => name)).toEqual(['init', 'init']);
  });

  it('queues page errors until the file loads, up to 100 entries', () => {
    // A page stuck in an error loop, with the file never arriving, must not
    // grow the queue without bound.
    const p = browserPage();
    p.run(LOADER);
    p.pageRejection("new Error('rejected')");
    for (let i = 0; i < 150; i++) p.pageError(`new Error('error ${i}')`);
    const queue = (p.sandbox.Sauron as { q: unknown[][] }).q;
    expect(queue).toHaveLength(100);
    expect(queue.slice(0, 3).map(([name]) => name)).toEqual(['init', '$unhandledrejection', '$error']);
  });
});

describe.each(FILES)('%s', (file) => {
  it('defines Sauron and no other global', () => {
    // SWC's ES5 helpers land at the top level unless the build wraps them —
    // one-letter globals, including `_`, lodash's.
    const p = page();
    const before = Object.keys(p.sandbox);
    p.run(code[file]);
    expect(Object.keys(p.sandbox).filter((key) => !before.includes(key))).toEqual(['Sauron']);
  });

  it('exposes exactly what the package entry point exports', () => {
    const p = page();
    p.run(code[file]);
    const Sauron = p.sandbox.Sauron as Record<string, unknown>;
    expect(Object.keys(Sauron).sort()).toEqual(Object.keys(api).sort());
    expect(Sauron.SDK_VERSION).toBe(api.SDK_VERSION);
  });

  it('keeps the first copy when loaded twice', () => {
    const p = page();
    p.run(code[file]);
    const first = p.sandbox.Sauron;
    p.run(code[file]);
    expect(p.sandbox.Sauron).toBe(first);
  });

  it('keeps DsnError an Error subclass', () => {
    // An ES5 class extending a builtin loses its prototype unless the
    // compiler wraps the super call; `instanceof DsnError` then goes false.
    const p = page();
    p.run(code[file]);
    const result = p.run(
      'var e = new Sauron.DsnError("x");' +
        'JSON.stringify([e instanceof Sauron.DsnError, e instanceof Error, e.name, e.message])',
    );
    expect(JSON.parse(result as string)).toEqual([true, true, 'DsnError', '[sauron] invalid DSN: x']);
  });

  it('bundles only the gzip half of fflate', () => {
    // One of fflate's unzip errors: present iff the whole library came along.
    // (A boolean, so a failure doesn't print the entire bundle.)
    expect(code[file].includes('invalid zip data'), 'all of fflate is bundled').toBe(false);
  });

  it('delivers a gzipped envelope', async () => {
    const p = browserPage();
    p.run(code[file]);
    p.run(`Sauron.init({ dsn: '${DSN}', release: 'web@1.0.0' })`);
    // Over 1 KB, so the body is compressed — and this realm has no
    // CompressionStream, so it goes through the bundled fflate.
    p.run("Sauron.track('checkout_completed', { note: new Array(2001).join('x') })");
    p.run("Sauron.captureException(new Error('boom'))");
    const [envelope] = await p.sent();

    expect(p.requests).toHaveLength(1);
    const [{ url, init }] = p.requests;
    expect(url).toBe('https://ingest.example.com/api/42/envelope');
    expect(init.headers['Content-Encoding']).toBe('gzip');
    expect(envelope.header.sdk).toEqual({ name: 'sauron.javascript', version: api.SDK_VERSION });
    expect(envelope.header.release).toBe('web@1.0.0');
    const event = envelope.items.find((item: { type: string }) => item.type === 'event');
    expect(event.name).toBe('checkout_completed');
    expect(event.properties.note).toHaveLength(2000);
    const error = envelope.items.find((item: { type: string }) => item.type === 'error');
    expect(error.exception).toMatchObject({ type: 'Error', value: 'boom' });
    expect(error.exception.stacktrace.length).toBeGreaterThan(0);
  });

  it('replays what the README loader queued, then takes over', async () => {
    const p = browserPage();
    p.run(LOADER);
    // Other tags, before the file has arrived.
    p.run("Sauron.setTag('tier', 'pro'); Sauron.track('queued'); Sauron.captureMessage('queued message')");
    const stub = p.sandbox.Sauron as { track(name: string): void };

    p.run(code[file]); // the <script> the loader added arrives

    expect(p.sandbox.Sauron).not.toBe(stub);
    expect((p.sandbox.Sauron as Record<string, unknown>).SDK_VERSION).toBe(api.SDK_VERSION);
    // A tag that held on to the stand-in still reaches the SDK.
    stub.track('via_old_reference');
    const posted = await items(p);
    const events = posted.filter((item) => item.type === 'event');
    expect(events.map((event) => [event.name, event.tags?.tier])).toEqual([
      ['queued', 'pro'],
      ['via_old_reference', 'pro'],
    ]);
    expect(posted.find((item) => item.type === 'error')?.message).toBe('queued message');
  });

  it('reports page errors the loader queued, then leaves errors to its own handlers', async () => {
    const p = browserPage();
    p.run(LOADER);
    p.run("Sauron.setTag('tier', 'pro')");
    // The page, before the file has arrived.
    p.pageError("new TypeError('x is not a function')");
    p.pageRejection("new Error('rejected early')");
    p.pageError('null', 'Script error.'); // a cross-origin script's error
    const stub = p.sandbox.Sauron as { q: { push(entry: unknown): void } };

    p.run(code[file]);

    p.pageError("new Error('after load')");
    // Only the SDK's handler reports it: the loader's listener stepped aside…
    expect(p.listeners.error).toEqual([]);
    expect(p.listeners.unhandledrejection).toEqual([]);
    // …and an error pushed onto the stand-in's old queue is ignored.
    stub.q.push(['$error', [{ type: 'error', message: 'late', error: p.run("new Error('late')") }]]);

    const errors = (await items(p)).filter((item) => item.type === 'error');
    expect(
      errors.map(({ exception, tags }) => [
        exception.type,
        exception.value,
        exception.mechanism.type,
        exception.mechanism.handled,
        tags?.tier,
      ]),
    ).toEqual([
      ['TypeError', 'x is not a function', 'onerror', false, 'pro'],
      ['Error', 'rejected early', 'onunhandledrejection', false, 'pro'],
      ['Error', 'Script error.', 'onerror', false, 'pro'],
      ['Error', 'after load', 'onerror', false, 'pro'],
    ]);
    expect(errors[0].exception.stacktrace.length).toBeGreaterThan(0);
  });

  it('gives a stackless capture its call site, but only when there is one', async () => {
    // Minification and the ES5 rewrite must not change which frame is dropped.
    const p = browserPage();
    p.run(LOADER);
    p.run("Sauron.captureException('queued before load')"); // replayed: no call site
    p.run(code[file]);
    p.run("function checkoutFails() { Sauron.captureException('payment failed'); } checkoutFails();");
    p.pageError('null', 'Script error.');
    const stacks = Object.fromEntries(
      (await items(p))
        .filter((item) => item.type === 'error')
        .map(({ exception }) => [
          exception.value,
          exception.stacktrace.map((frame: { function: string | null }) => frame.function),
        ]),
    );
    expect(stacks['queued before load']).toEqual([]);
    expect(stacks['payment failed'][stacks['payment failed'].length - 1]).toBe('checkoutFails');
    expect(stacks['Script error.']).toEqual([]);
  });

  it('replays init() ahead of the calls queued before it', async () => {
    // A tag that fires before the loader's own init() call.
    const p = browserPage();
    p.run(
      "self.Sauron = { q: [['track', ['before_init']], " +
        `['init', [{ dsn: '${DSN}', release: 'web@1.0.0' }]]] }`,
    );
    p.run(code[file]);
    expect((await items(p)).map((item) => item.name)).toEqual(['before_init']);
  });

  it('surfaces a queued call that throws, and still runs the rest', async () => {
    const p = browserPage();
    p.run(
      "self.Sauron = { q: [['init', [{ dsn: 'not a dsn', release: 'web@1.0.0' }]], " +
        `['init', [{ dsn: '${DSN}', release: 'web@1.0.0' }]], ` +
        "['track', ['after_a_bad_init']]] }",
    );
    p.run(code[file]);
    expect((await items(p)).map((item) => item.name)).toEqual(['after_a_bad_init']);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(p.thrown.map((err) => (err as Error).name)).toEqual(['DsnError']);
  });
});
