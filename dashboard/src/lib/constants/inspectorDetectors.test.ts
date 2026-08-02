import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { DETECTORS } from './inspectorDetectors';

// Same contract pattern as `permissions.test.ts` reading `rbac.rs`: parse the
// detector ids straight out of the backend source rather than comparing
// against a hand-copied list.
//
// The failure this closes is silent in BOTH directions and invisible at
// runtime. `parse_detectors` in detect.rs deliberately DROPS unknown ids ("an
// unknown id is a downgrade artifact, not a reason to fail the scan"), so a
// detector this file misspells is accepted with a 200, stored, and simply
// never matches — the scan finishes `succeeded` with `coverage='full'` and
// fewer findings than it should. A confident false negative is, in that file's
// own words, the worst thing this feature can emit. A detector added to the
// backend and forgotten here is the same bug with the checkbox missing.
const DETECT_RS_PATH = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  '../../../../backend/crates/sauron-inspector/src/detect.rs',
);

/** Parse the `Detector::X => "id",` arms of `Detector::id()`. */
function parseBackendDetectorIds(): string[] {
  let source: string;
  try {
    source = fs.readFileSync(DETECT_RS_PATH, 'utf-8');
  } catch (err) {
    throw new Error(
      `inspectorDetectors.test.ts could not read the backend detector source it validates ` +
        `against at "${DETECT_RS_PATH}" (${err instanceof Error ? err.message : String(err)}). ` +
        `This test must fail rather than silently skip when that file is missing or moved.`,
    );
  }

  const marker = 'pub fn id(self) -> &\'static str {';
  const start = source.indexOf(marker);
  if (start === -1) {
    throw new Error(`could not find "${marker}" in ${DETECT_RS_PATH}`);
  }
  let depth = 1;
  let i = start + marker.length;
  const bodyStart = i;
  for (; i < source.length && depth > 0; i++) {
    if (source[i] === '{') depth++;
    else if (source[i] === '}') depth--;
  }
  if (depth !== 0) {
    throw new Error(`unbalanced braces while parsing "Detector::id" in ${DETECT_RS_PATH}`);
  }
  const body = source.slice(bodyStart, i - 1);

  const ids = [...body.matchAll(/Detector::\w+\s*=>\s*"([a-z0-9_]+)"/g)].map((m) => m[1]);
  if (ids.length === 0) {
    throw new Error(`parsed zero detector arms out of "Detector::id" in ${DETECT_RS_PATH}`);
  }
  return ids;
}

describe('DETECTORS', () => {
  it('offers exactly the ids the backend knows', () => {
    // Set equality in both directions: an id here the backend drops silently
    // disables a checkbox, an id there missing here hides a detector.
    expect([...DETECTORS.map((d) => d.id)].sort()).toEqual([...parseBackendDetectorIds()].sort());
  });

  it('matches the backend ALL_DETECTORS arity', () => {
    const source = fs.readFileSync(DETECT_RS_PATH, 'utf-8');
    const m = source.match(/pub const ALL_DETECTORS:\s*\[Detector;\s*(\d+)\]/);
    if (!m) throw new Error(`could not find ALL_DETECTORS in ${DETECT_RS_PATH}`);
    expect(DETECTORS).toHaveLength(Number(m[1]));
  });

  it('labels and hints every detector', () => {
    for (const d of DETECTORS) {
      expect(d.label.trim()).not.toBe('');
      expect(d.hint.trim()).not.toBe('');
    }
  });

  it('has no duplicate ids', () => {
    expect(new Set(DETECTORS.map((d) => d.id)).size).toBe(DETECTORS.length);
  });
});
