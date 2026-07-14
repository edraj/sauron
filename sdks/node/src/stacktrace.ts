import type { Frame } from './types.js';

/**
 * V8 (`Error.stack`) parser for Node.
 *
 * Produces neutral frames with the CRASHING FRAME LAST (raw V8 stacks list the
 * crash site first, so the parsed list is reversed). No symbolication is
 * performed — line/column/filename pass through verbatim.
 */

export const MAX_FRAMES = 50;

/** V8 / Node: `    at fn (file:line:col)` or `    at file:line:col`. */
const V8_RE =
  /^\s*at (?:(.+?) )?\(?((?:[a-z][\w.+-]*:\/\/)?[^\s()]+?):(\d+):(\d+)\)?\s*$/i;

function cleanFunction(fn: string | undefined): string | null {
  if (!fn) return null;
  let name = fn.trim();
  name = name.replace(/^async\s+/, '').replace(/^new\s+/, 'new ');
  name = name.replace(/\s+\[as .+\]$/, '');
  if (name === '<anonymous>' || name === '') return null;
  return name;
}

/**
 * Heuristic for whether a frame belongs to first-party ("in app") code:
 * Node internals (`node:` / `internal/`) and `node_modules` dependencies are
 * not in-app; everything else (the app's own files) is.
 */
export function isInAppFrame(filename: string | null): boolean {
  if (!filename) return false;
  if (
    filename === '<anonymous>' ||
    filename.startsWith('node:') ||
    filename.startsWith('internal/') ||
    filename.startsWith('node internal')
  ) {
    return false;
  }
  if (filename.includes('node_modules')) return false;
  return true;
}

function stripFileProtocol(filename: string): string {
  return filename.startsWith('file://') ? filename.slice('file://'.length) : filename;
}

function parseLine(line: string): Frame | null {
  const m = V8_RE.exec(line);
  if (!m) return null;

  const rawFile = m[2] || null;
  const filename = rawFile ? stripFileProtocol(rawFile) : null;
  const lineno = m[3] ? parseInt(m[3], 10) : null;
  const colno = m[4] ? parseInt(m[4], 10) : null;

  return {
    function: cleanFunction(m[1]),
    module: null,
    filename,
    abs_path: filename,
    lineno: lineno !== null && Number.isNaN(lineno) ? null : lineno,
    colno: colno !== null && Number.isNaN(colno) ? null : colno,
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
      if (frames.length >= MAX_FRAMES) break; // keep frames nearest the crash
    }
  }
  // Raw V8 stacks are crash-first; the wire contract wants crash-last.
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
