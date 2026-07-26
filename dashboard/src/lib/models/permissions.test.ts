import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { ALL_PERMISSIONS, PERMISSION_GROUPS, PERMISSION_LABELS } from './permissions';

// This test parses perm::ALL straight out of the backend source instead of
// comparing against a hand-copied list. A hand-copied list only catches drift
// introduced on the frontend: someone adds a permission to perm::ALL in
// backend/crates/sauron-auth/src/rbac.rs, forgets the dashboard, and a
// duplicated-by-hand list here would stay green while the checkbox grid
// silently strips that permission from every role on first save. Reading the
// real file closes that gap.
const RBAC_RS_PATH = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  '../../../../backend/crates/sauron-auth/src/rbac.rs',
);

/** Slice out the body of `pub mod perm { ... }`, matching braces so nested
 * `{}` (none today, but be safe) don't truncate the block early. */
function extractPermModuleBody(source: string): string {
  const marker = 'pub mod perm {';
  const start = source.indexOf(marker);
  if (start === -1) {
    throw new Error(`could not find "${marker}" in ${RBAC_RS_PATH}`);
  }
  let depth = 1;
  let i = start + marker.length;
  const bodyStart = i;
  for (; i < source.length && depth > 0; i++) {
    if (source[i] === '{') depth++;
    else if (source[i] === '}') depth--;
  }
  if (depth !== 0) {
    throw new Error(`unbalanced braces while parsing "pub mod perm" in ${RBAC_RS_PATH}`);
  }
  return source.slice(bodyStart, i - 1);
}

/** Parse every `pub const NAME: &str = "value";` declaration into a map. */
function parseConstStrings(moduleBody: string): Map<string, string> {
  const map = new Map<string, string>();
  const re = /pub const ([A-Z0-9_]+):\s*&str\s*=\s*"([^"]*)";/g;
  for (const m of moduleBody.matchAll(re)) {
    map.set(m[1], m[2]);
  }
  return map;
}

/** Parse the declared length and the ordered identifier list out of
 * `pub const ALL: [&str; N] = [ ... ];`. */
function parseAllDeclaration(moduleBody: string): { declaredLength: number; identifiers: string[] } {
  const re = /pub const ALL:\s*\[&str;\s*(\d+)\]\s*=\s*\[([\s\S]*?)\];/;
  const m = moduleBody.match(re);
  if (!m) {
    throw new Error(`could not find "pub const ALL: [&str; N] = [...]" in ${RBAC_RS_PATH}`);
  }
  const declaredLength = Number(m[1]);
  const identifiers = m[2].match(/[A-Z0-9_]+/g) ?? [];
  return { declaredLength, identifiers };
}

function loadBackendCatalog(): { declaredLength: number; permissions: string[] } {
  let source: string;
  try {
    source = fs.readFileSync(RBAC_RS_PATH, 'utf-8');
  } catch (err) {
    throw new Error(
      `permissions.test.ts could not read the backend RBAC source it validates against ` +
        `at "${RBAC_RS_PATH}" (${err instanceof Error ? err.message : String(err)}). ` +
        `This test must fail rather than silently skip when that file is missing or moved.`,
    );
  }

  const moduleBody = extractPermModuleBody(source);
  const constants = parseConstStrings(moduleBody);
  const { declaredLength, identifiers } = parseAllDeclaration(moduleBody);

  const permissions = identifiers.map((name) => {
    const value = constants.get(name);
    if (value === undefined) {
      throw new Error(
        `perm::ALL in ${RBAC_RS_PATH} references "${name}", but no ` +
          `"pub const ${name}: &str = ...;" declaration was found to resolve it.`,
      );
    }
    return value;
  });

  return { declaredLength, permissions };
}

const BACKEND_CATALOG = loadBackendCatalog();

describe('permission catalog', () => {
  it('parses a well-formed, non-empty backend catalog', () => {
    // Guards the parser itself: a regex that silently matched nothing (e.g.
    // because the file's shape changed) must not pass vacuously, and a stale
    // `[&str; N]` length annotation must be caught too.
    expect(BACKEND_CATALOG.permissions.length).toBeGreaterThan(0);
    expect(BACKEND_CATALOG.declaredLength).toBe(BACKEND_CATALOG.permissions.length);
  });

  it('matches the backend catalog exactly, in order', () => {
    expect(ALL_PERMISSIONS).toEqual(BACKEND_CATALOG.permissions);
  });

  it('groups every permission exactly once', () => {
    const grouped = PERMISSION_GROUPS.flatMap((g) => g.permissions);
    expect([...grouped].sort()).toEqual([...ALL_PERMISSIONS].sort());
    expect(new Set(grouped).size).toBe(grouped.length);
  });

  it('labels every permission', () => {
    for (const p of ALL_PERMISSIONS) {
      expect(PERMISSION_LABELS[p], `missing label for ${p}`).toBeTruthy();
    }
  });
});
