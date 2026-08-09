import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Shared writer for `sdks/wire-fixtures/<sdk>.json` — the envelopes the backend's
 * `sdk_wire_conformance` test feeds through the real `serde` deserializer.
 *
 * ## What is pinned, and why
 *
 * A fixture is only useful if regenerating it is a NO-OP. Two things follow from
 * that, and the second one is the whole reason this file grew:
 *
 * 1. The intrinsically dynamic fields (`timestamp`, `event_id`, …) are pinned.
 * 2. So is everything **the toolchain supplies rather than the SDK**. The
 *    fixture is written from inside vitest running on Node, so without this the
 *    file recorded vitest's internal frame names (`chunk-hooks.js`, `runSuite`,
 *    `processTicksAndRejections`), the host kernel version and the Node version.
 *    Bumping vitest or Node then rewrote a committed file **with no wire change
 *    at all** — which makes a CI diff gate permanently noisy and trains
 *    reviewers to wave fixture diffs through, and means the suites dirty a
 *    tracked file on machines that only ran the tests.
 *
 * What is deliberately NOT normalized is the part that actually proves
 * something: item shape, key set, nullability, and the frame COUNT. A frame's
 * identity strings say nothing about whether the envelope deserializes; that
 * there are 9 frames, each with these keys and these nulls, does.
 *
 * Residual: frame count and null-vs-string per frame still come from the test
 * runner, so a vitest/Node MAJOR change can legitimately move them. That is
 * deterministic per lockfile — CI and a developer both run `npm ci` — so it
 * shows up only alongside a dependency bump, where a fixture diff is expected
 * and reviewable. See `sdks/wire-fixtures/README.md`.
 */

const FIXED = {
  timestamp: '2026-07-12T10:30:00.123Z',
  eventId: '0123456789abcdef0123456789abcdef',
  sessionId: 'sess_fixture',
  deviceId: '3f2504e0-4f89-41d3-9a0c-0305e82c3301',
  workflowId: 'wf_fixture',
  lineno: 42,
  colno: 13,
} as const;

/**
 * Stack-frame identity. These come from wherever the test happened to run, not
 * from the SDK: under vitest they are the runner's own internals.
 */
const FRAME_IDENTITY: Record<string, string> = {
  function: '<fn>',
  module: '<module>',
  filename: '<file>',
  abs_path: '<file>',
};

/**
 * `<parent>.<key>` paths whose value is host- or toolchain-derived.
 *
 * `context.device` / `.os` / `.runtime` are free-form `serde_json::Value` on the
 * wire (`envelope.rs`: `pub os: serde_json::Value`), so their CONTENTS prove
 * nothing about conformance — while `os.version` is the host kernel string and
 * `runtime.version` is the Node version, both of which differ on every machine.
 * `runtime.name` is absent on purpose: it is an SDK constant, not a host value.
 */
const HOST_DERIVED = new Set([
  'os.name',
  'os.version',
  'runtime.version',
  'device.family',
  'device.model',
  'device.arch',
]);

function normalizeValue(key: string, parentKey: string, value: unknown): unknown {
  if (typeof value === 'string') {
    if (HOST_DERIVED.has(`${parentKey}.${key}`)) return '<host>';
    if (key in FRAME_IDENTITY) return FRAME_IDENTITY[key];
    switch (key) {
      case 'timestamp':
      case 'sent_at':
        return FIXED.timestamp;
      case 'event_id':
        return FIXED.eventId;
      case 'session_id':
        return FIXED.sessionId;
      case 'device_id':
        return FIXED.deviceId;
      case 'workflow_id':
        return FIXED.workflowId;
      case 'raw_stacktrace':
        return '<normalized>';
      case 'build_id':
      case 'isolate_dso_base':
        return '<normalized>';
      default:
        return value;
    }
  }
  if (typeof value === 'number') {
    if (key === 'lineno') return FIXED.lineno;
    if (key === 'colno') return FIXED.colno;
  }
  // `null` is left alone everywhere: nullability is part of what the fixture
  // proves, so a null must never be papered over with a placeholder.
  return value;
}

function normalize(node: unknown, key = '', parentKey = ''): unknown {
  // Array children keep their container's key AND its parent, so a frame inside
  // `stacktrace: [...]` is still seen as living under `stacktrace`.
  if (Array.isArray(node)) return node.map((n) => normalize(n, key, parentKey));
  if (node !== null && typeof node === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
      out[k] = normalize(v, k, key);
    }
    return out;
  }
  return normalizeValue(key, parentKey, node);
}

/** Write one captured envelope as this SDK's committed wire fixture. */
export function writeWireFixture(sdk: string, envelope: unknown): void {
  const path = fileURLToPath(new URL(`../../wire-fixtures/${sdk}.json`, import.meta.url));
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(normalize(envelope), null, 2)}\n`, 'utf8');
}
