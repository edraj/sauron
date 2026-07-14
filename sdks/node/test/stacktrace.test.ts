import { describe, it, expect } from 'vitest';

import { parseStackString, parseError, isInAppFrame } from '../src/stacktrace.js';

const V8_STACK = `Error: boom
    at deepest (/app/src/service.js:10:15)
    at middle (/app/src/handler.js:22:5)
    at Object.<anonymous> (/app/index.js:3:1)`;

describe('parseStackString', () => {
  it('parses V8 frames with the crash frame LAST', () => {
    const frames = parseStackString(V8_STACK);
    expect(frames).toHaveLength(3);
    // Raw stack lists `deepest` first (crash site); wire wants it last.
    expect(frames[frames.length - 1]).toMatchObject({
      function: 'deepest',
      filename: '/app/src/service.js',
      lineno: 10,
      colno: 15,
      in_app: true,
    });
    expect(frames[0]).toMatchObject({
      function: 'Object.<anonymous>',
      filename: '/app/index.js',
      lineno: 3,
      colno: 1,
    });
  });

  it('emits the full frame shape (module/abs_path/in_app)', () => {
    const [frame] = parseStackString('    at fn (/app/a.js:1:2)');
    expect(frame).toEqual({
      function: 'fn',
      module: null,
      filename: '/app/a.js',
      abs_path: '/app/a.js',
      lineno: 1,
      colno: 2,
      in_app: true,
    });
  });

  it('strips the file:// protocol from ESM frames', () => {
    const [frame] = parseStackString('    at fn (file:///app/mod.mjs:5:9)');
    expect(frame.filename).toBe('/app/mod.mjs');
    expect(frame.in_app).toBe(true);
  });

  it('returns an empty array for empty/undefined input', () => {
    expect(parseStackString(undefined)).toEqual([]);
    expect(parseStackString(null)).toEqual([]);
    expect(parseStackString('')).toEqual([]);
  });

  it('parses a real Error via parseError', () => {
    const frames = parseError(new Error('real'));
    expect(frames.length).toBeGreaterThan(0);
    // The crash frame (this test function) is last.
    expect(frames[frames.length - 1].in_app).toBe(true);
  });
});

describe('isInAppFrame', () => {
  it('treats app files as in-app', () => {
    expect(isInAppFrame('/app/src/service.js')).toBe(true);
  });
  it('treats node internals as not in-app', () => {
    expect(isInAppFrame('node:internal/process/task_queues')).toBe(false);
    expect(isInAppFrame('internal/main/run_main_module.js')).toBe(false);
  });
  it('treats node_modules as not in-app', () => {
    expect(isInAppFrame('/app/node_modules/express/lib/router.js')).toBe(false);
  });
  it('treats null/anonymous as not in-app', () => {
    expect(isInAppFrame(null)).toBe(false);
    expect(isInAppFrame('<anonymous>')).toBe(false);
  });
});
