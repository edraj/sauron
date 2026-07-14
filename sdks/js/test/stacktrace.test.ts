import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { MAX_FRAMES, parseStackString } from '../src/stacktrace/parse';

// The in_app heuristic keys off location.origin — pretend we're on example.com.
beforeEach(() => {
  (globalThis as { location?: unknown }).location = {
    origin: 'https://example.com',
    href: 'https://example.com/',
  };
});

afterEach(() => {
  delete (globalThis as { location?: unknown }).location;
});

describe('parseStackString - Chrome / V8', () => {
  const CHROME = [
    'TypeError: x is not a function',
    '    at crash (https://example.com/app.js:42:13)',
    '    at helper (https://example.com/app.js:30:9)',
    '    at https://cdn.other.net/lib.js:5:1',
    '    at Array.forEach (<anonymous>)',
  ].join('\n');

  it('reverses so the crashing frame is LAST', () => {
    const frames = parseStackString(CHROME);
    const crash = frames[frames.length - 1];
    expect(crash.function).toBe('crash');
    expect(crash.filename).toBe('https://example.com/app.js');
    expect(crash.lineno).toBe(42);
    expect(crash.colno).toBe(13);
  });

  it('skips the non-frame header line', () => {
    const frames = parseStackString(CHROME);
    // <anonymous> has no file:line:col, so it is dropped too.
    expect(frames.map((f) => f.filename)).toEqual([
      'https://cdn.other.net/lib.js',
      'https://example.com/app.js',
      'https://example.com/app.js',
    ]);
  });

  it('marks same-origin frames in_app and cross-origin frames not', () => {
    const frames = parseStackString(CHROME);
    const cdn = frames.find((f) => f.filename === 'https://cdn.other.net/lib.js');
    const app = frames.find((f) => f.filename === 'https://example.com/app.js');
    expect(cdn?.in_app).toBe(false);
    expect(app?.in_app).toBe(true);
  });

  it('parses anonymous frames (no function name)', () => {
    const stack = 'Error: boom\n    at https://example.com/app.js:100:5';
    const frames = parseStackString(stack);
    expect(frames).toHaveLength(1);
    expect(frames[0].function).toBeNull();
    expect(frames[0].lineno).toBe(100);
  });
});

describe('parseStackString - Firefox / Gecko', () => {
  const FIREFOX = [
    'crash@https://example.com/app.js:42:13',
    'helper@https://example.com/app.js:30:9',
    '@https://cdn.other.net/lib.js:5:1',
  ].join('\n');

  it('parses fn@file:line:col with crashing frame last', () => {
    const frames = parseStackString(FIREFOX);
    expect(frames).toHaveLength(3);
    const crash = frames[frames.length - 1];
    expect(crash.function).toBe('crash');
    expect(crash.filename).toBe('https://example.com/app.js');
    expect(crash.lineno).toBe(42);
    expect(crash.colno).toBe(13);
  });

  it('treats an empty @-prefixed function as null', () => {
    const frames = parseStackString(FIREFOX);
    const anon = frames.find((f) => f.filename === 'https://cdn.other.net/lib.js');
    expect(anon?.function).toBeNull();
    expect(anon?.in_app).toBe(false);
  });
});

describe('parseStackString - Safari', () => {
  const SAFARI = [
    'crash@https://example.com/app.js:42:13',
    'dispatch@[native code]',
    'global code@https://example.com/index.html:12:20',
  ].join('\n');

  it('parses Safari frames and ignores [native code]', () => {
    const frames = parseStackString(SAFARI);
    // "[native code]" has no line:col and is skipped.
    expect(frames.map((f) => f.function)).toEqual(['global code', 'crash']);
    expect(frames[frames.length - 1].lineno).toBe(42);
  });
});

describe('parseStackString - edge cases', () => {
  it('treats bare/relative filenames as in_app', () => {
    const frames = parseStackString('Error\n    at loadUser (app.js:42:13)');
    expect(frames[0].filename).toBe('app.js');
    expect(frames[0].in_app).toBe(true);
  });

  it('returns [] for empty/undefined input', () => {
    expect(parseStackString(undefined)).toEqual([]);
    expect(parseStackString(null)).toEqual([]);
    expect(parseStackString('')).toEqual([]);
  });

  it(`caps depth at ${MAX_FRAMES} frames, keeping the ones nearest the crash`, () => {
    const lines = ['Error: deep'];
    for (let i = 0; i < 120; i++) {
      lines.push(`    at fn${i} (https://example.com/app.js:${i + 1}:1)`);
    }
    const frames = parseStackString(lines.join('\n'));
    expect(frames).toHaveLength(MAX_FRAMES);
    // Raw frame 0 (fn0, the crash site) is kept and ends up LAST after reversal.
    expect(frames[frames.length - 1].function).toBe('fn0');
    expect(frames[0].function).toBe(`fn${MAX_FRAMES - 1}`);
  });
});
