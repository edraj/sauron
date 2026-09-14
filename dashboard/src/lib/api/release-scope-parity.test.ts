import { describe, it, expect } from 'vitest';
// `?raw` rather than `node:fs`, for the same reason
// `filters/filter-registry-parity.test.ts` records: Vite inlines the file at
// transform time, so this needs no `@types/node` and no path juggling
// relative to the test runner's cwd. One directory shallower than that file
// (`api/` vs `components/filters/`), hence one fewer `../`.
import guardRs from '../../../../backend/bins/sauron-api/src/release_guard.rs?raw';
import { RELEASE_SCOPED_URL } from './scope';

/**
 * The client ↔ server release allowlist.
 *
 * This exists because the gap it checks is SILENT IN BOTH DIRECTIONS, exactly
 * like `filter-registry-parity.test.ts`'s reasoning for the filter chips: a
 * client URL the server does not accept `release` on gets a 400 the moment a
 * release is selected; a server-accepting route with no client entry silently
 * shows unfiltered data forever while `sessionStore.currentRelease` sits
 * non-null and every other page looks correct.
 *
 * Reads the Rust source rather than restating it, so the two cannot drift
 * without one of these two tests catching it.
 */
function serverTemplates(): string[] {
  const block = /pub const RELEASE_ACCEPTING_PATHS: &\[&str\] = &\[([\s\S]*?)\n\];/.exec(guardRs);
  if (!block) throw new Error('RELEASE_ACCEPTING_PATHS not found in release_guard.rs — was it renamed?');
  return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/** `/v1/apps/{app_id}/issues/{issue_id}/events` → `/v1/apps/x/issues/y/events` */
function concrete(template: string): string {
  return template.replace(/\{[^}]+\}/g, 'x');
}

describe('release scoping parity', () => {
  it('every server-accepting route is client-scoped', () => {
    for (const tpl of serverTemplates()) {
      const url = concrete(tpl);
      expect(RELEASE_SCOPED_URL.some((re) => re.test(url)), `client does not scope ${tpl}`).toBe(true);
    }
  });

  it('the client scopes nothing the server rejects', () => {
    expect(RELEASE_SCOPED_URL.length).toBe(serverTemplates().length);
  });
});
