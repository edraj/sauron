import type { Frame } from '../types.js';

/**
 * Cross-browser `Error.stack` parser.
 *
 * Produces neutral frames with the CRASHING FRAME LAST (raw stacks list the
 * crash site first, so the parsed list is reversed). Depth is capped at
 * {@link MAX_FRAMES}, keeping the frames nearest the crash. No symbolication is
 * performed — line/column/filename are passed through verbatim.
 */

export const MAX_FRAMES = 50;

/** V8 / Chrome / Node / Edge: `    at fn (file:line:col)` or `    at file:line:col`. */
const V8_RE =
  /^\s*at (?:(.+?) )?\(?((?:[a-z][\w.+-]*:\/\/)?[^\s()]+?):(\d+):(\d+)\)?\s*$/i;

/**
 * Firefox / Safari: `fn@file:line:col`, `@file:line:col`, `fn/<@file:line:col`.
 * The function name may contain spaces (Safari's `global code`, `module code`).
 */
const GECKO_RE =
  /^\s*(?:([^@]*?)@)?((?:[a-z][\w.+-]*:\/\/)?[^@\s]+?):(\d+):(\d+)\s*$/i;

function cleanFunction(fn: string | undefined): string | null {
  if (!fn) return null;
  let name = fn.trim();
  // V8 async / constructor prefixes and Safari's trailing markers.
  name = name.replace(/^async\s+/, '').replace(/^new\s+/, 'new ');
  name = name.replace(/\s+\[as .+\]$/, '');
  if (name === '<anonymous>' || name === '') return null;
  return name;
}

/**
 * Heuristic for whether a frame belongs to first-party ("in app") code:
 * same-origin URLs and bare/relative paths are in-app; cross-origin URLs
 * (typically CDN or third-party scripts) and internal frames are not.
 */
export function isInAppFrame(filename: string | null): boolean {
  if (!filename) return false;
  if (filename === '<anonymous>' || filename.startsWith('node:') || filename.startsWith('internal/')) {
    return false;
  }
  const hasProtocol = /^[a-z][\w.+-]*:\/\//i.test(filename);
  if (!hasProtocol) {
    // A bare or relative path (e.g. "app.js", "/static/app.js").
    return true;
  }
  const origin = getOrigin();
  if (origin && filename.startsWith(origin)) {
    return true;
  }
  // Cross-origin absolute URL — treat as third-party / vendor.
  return false;
}

function getOrigin(): string {
  const loc = (globalThis as { location?: { origin?: string } }).location;
  return loc?.origin ?? '';
}

function parseLine(line: string): Frame | null {
  let m = V8_RE.exec(line);
  if (!m) m = GECKO_RE.exec(line);
  if (!m) return null;

  const filename = m[2] || null;
  const lineno = m[3] ? parseInt(m[3], 10) : null;
  const colno = m[4] ? parseInt(m[4], 10) : null;

  return {
    function: cleanFunction(m[1]),
    filename,
    lineno: Number.isNaN(lineno) ? null : lineno,
    colno: Number.isNaN(colno) ? null : colno,
    in_app: isInAppFrame(filename),
  };
}

/**
 * Parse a raw `Error.stack` string into normalized frames (crash frame last).
 * Non-frame lines (the `Error: message` header, `[native code]`, etc.) are
 * skipped.
 */
export function parseStackString(stack: string | undefined | null): Frame[] {
  if (!stack) return [];
  const frames: Frame[] = [];
  const lines = stack.split('\n');
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '');
    if (!line.trim()) continue;
    const frame = parseLine(line);
    if (frame) {
      frames.push(frame);
      if (frames.length >= MAX_FRAMES) break; // keep the frames nearest the crash
    }
  }
  // Raw stacks are crash-first; the wire contract wants crash-last.
  frames.reverse();
  return frames;
}

/** Parse an `Error`-like object's `.stack`. */
export function parseError(err: unknown): Frame[] {
  if (err && typeof err === 'object' && 'stack' in err) {
    const stack = (err as { stack?: unknown }).stack;
    if (typeof stack === 'string') return parseStackString(stack);
  }
  return [];
}
