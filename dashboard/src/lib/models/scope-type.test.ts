import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import { readdirSync, readFileSync } from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';

// Read the backend migrations rather than a hand-copied list, for the same
// reason permissions.test.ts reads rbac.rs: a copy only catches drift the
// frontend introduces, never drift the backend introduces.
const MIGRATIONS = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  '../../../../backend/migrations',
);

/** The scope types the live CHECK constraint accepts, newest migration wins. */
function backendScopeTypes(): string[] {
  const dirs = readdirSync(MIGRATIONS).sort();
  let found: string[] | null = null;
  for (const d of dirs) {
    const p = path.join(MIGRATIONS, d, 'up.sql');
    let sql: string;
    try {
      sql = readFileSync(p, 'utf8');
    } catch {
      continue;
    }
    // Strip `--` comments BEFORE matching, and require the migration to name
    // `role_grants` in actual SQL. Without both halves this scan reads the
    // wrong table: 2026-08-01-000037 creates `notification_subscriptions` with
    // its own `CHECK (scope_type IN ('project','app'))` and merely *mentions*
    // role_grants in two prose comments, so it sorts last, qualifies, and this
    // test fails claiming the backend dropped 'org' and 'env' from role_grants
    // — a false alarm about a migration that never touched role_grants.
    const code = sql.replace(/--[^\n]*/g, '');
    const m = code.match(/CHECK\s*\(\s*scope_type\s+IN\s*\(([^)]*)\)/i);
    if (m && code.includes('role_grants')) {
      found = [...m[1].matchAll(/'([a-z]+)'/g)].map((x) => x[1]);
    }
  }
  if (!found) throw new Error('no scope_type CHECK constraint found in migrations');
  return found;
}

describe('ScopeType mirrors the backend CHECK constraint', () => {
  it('accepts exactly the scope types role_grants does', () => {
    expect(backendScopeTypes().sort()).toEqual(
      ['app', 'env', 'org', 'project'].sort(),
    );
  });
});
