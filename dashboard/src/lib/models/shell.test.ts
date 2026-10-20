import { describe, expect, it } from 'vitest';
import { PAGE_ACCESS } from './page-access';
import {
  FULLY_RELEASE_AWARE,
  RELEASE_AWARE,
  SHELL_FLAGS,
  resolveShell,
  showsReleaseNote,
} from './shell';

describe('SHELL_FLAGS ↔ PAGE_ACCESS parity', () => {
  /**
   * Both directions, deliberately. A PAGE_ACCESS key with no shell row would
   * render that page BARE — no sidebar, no topbar — which is exactly the
   * regression the hoist exists to prevent; a shell row with no PAGE_ACCESS
   * key is dead weight that `findPageAccessKey` can never resolve to, so it
   * would sit green while covering nothing.
   */
  it('every PAGE_ACCESS key has a shell decision', () => {
    for (const key of Object.keys(PAGE_ACCESS)) {
      expect(key in SHELL_FLAGS, `SHELL_FLAGS is missing '${key}'`).toBe(true);
    }
  });

  it('every shell key is a real PAGE_ACCESS key', () => {
    for (const key of Object.keys(SHELL_FLAGS)) {
      expect(key in PAGE_ACCESS, `SHELL_FLAGS has stray key '${key}'`).toBe(true);
    }
  });
});

describe('resolveShell', () => {
  it('detail routes inherit their list page via the prefix match', () => {
    expect(resolveShell('/issues/0192aa41')).toEqual({ requireProject: true, requireApp: true });
    expect(resolveShell('/sessions/abc?x=1')).toEqual({ requireProject: true, requireApp: true });
  });

  /**
   * The flags each page passed BEFORE the hoist, spot-checked at the corners:
   * AppShell defaulted `requireProject: true` while AdminShell forwarded
   * `false`, so these are the rows a mechanical "all admin pages are alike"
   * rewrite would get wrong.
   */
  it('preserves the pre-hoist per-page flags at the defaults corners', () => {
    expect(resolveShell('/active-users')).toEqual({ requireProject: true, requireApp: false });
    expect(resolveShell('/admin/environments')).toEqual({
      requireProject: true,
      requireApp: false,
    });
    expect(resolveShell('/admin/privacy')).toEqual({ requireProject: false, requireApp: true });
    expect(resolveShell('/admin/purge')).toEqual({ requireProject: false, requireApp: true });
    expect(resolveShell('/docs')).toEqual({ requireProject: false, requireApp: false });
  });

  it('leaves the bare pages bare', () => {
    // Own layouts: wrapping them would be a visible regression, not a default.
    expect(resolveShell('/onboarding')).toBeNull();
    // No PAGE_ACCESS key at all → no shell (auth flows, legacy redirects, '*').
    expect(resolveShell('/login')).toBeNull();
    expect(resolveShell('/reset-password')).toBeNull();
    expect(resolveShell('/members')).toBeNull();
    expect(resolveShell('/no-such-page')).toBeNull();
  });
});

describe('RELEASE_AWARE ↔ PAGE_ACCESS parity', () => {
  it('every PAGE_ACCESS key says whether it honors the release switcher', () => {
    for (const key of Object.keys(PAGE_ACCESS)) {
      expect(key in RELEASE_AWARE, `RELEASE_AWARE is missing '${key}'`).toBe(true);
    }
    for (const key of Object.keys(RELEASE_AWARE)) {
      expect(key in PAGE_ACCESS, `RELEASE_AWARE has stray key '${key}'`).toBe(true);
    }
  });

  it('exactly the four searched list pages and Users are release-aware', () => {
    const aware = Object.entries(RELEASE_AWARE)
      .filter(([, v]) => v)
      .map(([k]) => k)
      .sort();
    expect(aware).toEqual(['/events', '/issues', '/sessions', '/transactions', '/users'].sort());
  });

  it('the person detail page stays release-blind (one person is one person)', () => {
    expect(RELEASE_AWARE['/persons']).toBe(false);
  });
});

describe('FULLY_RELEASE_AWARE', () => {
  /**
   * A page whose charts and totals follow the release necessarily keys on it,
   * and only an envAware telemetry page has charts and totals to speak of. A
   * stray entry would silently suppress the note on a page whose aggregates
   * still include every release — the exact mis-read the note prevents.
   */
  it('is a subset of the release-aware, envAware PAGE_ACCESS keys', () => {
    expect(FULLY_RELEASE_AWARE.size).toBeGreaterThan(0);
    for (const key of FULLY_RELEASE_AWARE) {
      expect(key in PAGE_ACCESS, `'${key}' is not a PAGE_ACCESS key`).toBe(true);
      expect(RELEASE_AWARE[key], `'${key}' is not RELEASE_AWARE`).toBe(true);
      expect(PAGE_ACCESS[key]?.envAware, `'${key}' is not envAware`).toBe(true);
    }
  });

  it('is exactly Users today', () => {
    expect([...FULLY_RELEASE_AWARE].sort()).toEqual(['/users']);
  });
});

describe('showsReleaseNote', () => {
  it('shows on an env-scoped telemetry page that does not filter by release', () => {
    expect(showsReleaseNote('/overview')).toBe(true);
  });

  /**
   * The note ALSO shows on release-aware pages: its sentence is about charts
   * and totals, and those stay aggregate on `/issues` and `/sessions` too
   * (their stat tiles / timeseries read release-blind rollups — see
   * `RELEASE_AWARE`'s doc comment). Suppressing it there would leave exactly
   * the pages that mix a narrowing list with aggregate widgets unexplained.
   */
  it('shows on the release-aware list pages too (their charts stay aggregate)', () => {
    expect(showsReleaseNote('/issues')).toBe(true);
    expect(showsReleaseNote('/issues/abc')).toBe(true);
    expect(showsReleaseNote('/sessions')).toBe(true);
  });

  /**
   * Users is the one page where the sentence is false: its tiles, chart, list
   * and count all read release-keyed rollups, so "charts and totals include
   * all releases" would contradict every number under it.
   */
  it('hides on Users, where the charts and totals follow the release too', () => {
    expect(showsReleaseNote('/users')).toBe(false);
    expect(showsReleaseNote('/users?time_field=first_seen')).toBe(false);
    // The person detail page is NOT part of that: its own widgets ignore the
    // switcher, so it keeps the note like every other envAware page.
    expect(PAGE_ACCESS['/persons']?.envAware).toBe(true);
    expect(showsReleaseNote('/persons/bob')).toBe(true);
  });

  it('hides on pages with no telemetry at all', () => {
    expect(showsReleaseNote('/admin/storage')).toBe(false);
    expect(showsReleaseNote('/docs')).toBe(false);
    expect(showsReleaseNote('/account')).toBe(false);
  });

  it('hides on an env-blind page (app-wide config, not telemetry)', () => {
    // `/funnels` omits `envAware` entirely (saved funnels are app-wide
    // config — see `page-access.ts`), so the gate must treat absent as false.
    expect(PAGE_ACCESS['/funnels']?.envAware).not.toBe(true);
    expect(showsReleaseNote('/funnels')).toBe(false);
  });

  it('hides on an unknown path (fails closed, unlike resolveShell)', () => {
    expect(showsReleaseNote('/no-such-page')).toBe(false);
  });

  /**
   * Every page the note shows on must be a `PAGE_ACCESS` row that is
   * `envAware` — it reads env-scoped telemetry, so it has charts/totals a
   * release could plausibly have narrowed and didn't — and must not be one
   * whose charts and totals DO follow the release. `RELEASE_AWARE` is
   * deliberately NOT part of this gate (see `showsReleaseNote`). Catches a
   * rewrite that stops reading the tables it gates on.
   */
  it('every page the note shows on is envAware and not fully release-aware', () => {
    let shown = 0;
    for (const key of Object.keys(PAGE_ACCESS)) {
      if (showsReleaseNote(key)) {
        shown += 1;
        expect(PAGE_ACCESS[key]?.envAware, `${key} is not envAware`).toBe(true);
        expect(FULLY_RELEASE_AWARE.has(key), `${key} is fully release-aware`).toBe(false);
      }
    }
    // Guards against the sweep passing vacuously if the note stopped showing
    // anywhere at all.
    expect(shown).toBeGreaterThan(5);
  });

  /**
   * The converse direction: every envAware page gets the note, release-aware
   * or not, unless it is fully release-aware. Without this, re-adding a
   * blanket `RELEASE_AWARE` exclusion would still satisfy the sweep above.
   */
  it('shows on EVERY envAware page except the fully release-aware ones', () => {
    const listOnlyAware = Object.keys(PAGE_ACCESS).filter(
      (k) =>
        PAGE_ACCESS[k]?.envAware === true &&
        RELEASE_AWARE[k] === true &&
        !FULLY_RELEASE_AWARE.has(k),
    );
    // The release-aware LIST pages must still be in the sweep — they are the
    // ones a blanket exclusion would wrongly silence.
    expect(listOnlyAware.length).toBeGreaterThan(0);
    for (const key of Object.keys(PAGE_ACCESS)) {
      if (PAGE_ACCESS[key]?.envAware === true && !FULLY_RELEASE_AWARE.has(key)) {
        expect(showsReleaseNote(key), `${key} is envAware but shows no note`).toBe(true);
      }
    }
  });
});
