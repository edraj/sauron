import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { computeScopeParams, shouldScopeUrl } from './scope';

describe('shouldScopeUrl', () => {
  it('scopes an ordinary telemetry read', () => {
    expect(shouldScopeUrl('/v1/apps/app-1/events')).toBe(true);
    expect(shouldScopeUrl('/v1/apps/app-1/issues')).toBe(true);
    expect(shouldScopeUrl('/v1/apps/app-1/sessions')).toBe(true);
  });

  it('scopes POST /v1/apps/{id}/funnel (compute, singular) but not GET .../funnels (saved, plural)', () => {
    // These two differ only by a trailing "s" and both matter: `compute` is
    // a live telemetry read (backend calls `read_scope`); the saved-funnels
    // CRUD is app-wide configuration (backend calls `reject_environment_id`
    // and 400s if the parameter rides along at all).
    expect(shouldScopeUrl('/v1/apps/app-1/funnel')).toBe(true);
    expect(shouldScopeUrl('/v1/apps/app-1/funnels')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/funnels/funnel-1')).toBe(false);
  });

  it('does not scope any of the app-configuration exclusions', () => {
    expect(shouldScopeUrl('/v1/apps/app-1/environments')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/first-event')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/artifacts')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/artifacts/artifact-1')).toBe(false);
  });

  it('does not scope the three cross-tier timeseries endpoints', () => {
    // `analytics::error_timeseries` / `event_timeseries` /
    // `transaction_timeseries` reject any `environment_id` at all — cold
    // storage is not partitioned by environment yet. Regression guard for the
    // review finding that these three rejected the parameter via an inline
    // `raw_environment_id(..).is_some()` check instead of a
    // `reject_environment_id*` call, so the reconciliation grep this module's
    // comment prescribes couldn't see them and this matcher fell through to
    // `true` (scoped) — attaching `environment_id` and getting a 400 back —
    // for every one of them, dormant only because the dashboard never calls
    // these endpoints.
    expect(shouldScopeUrl('/v1/apps/app-1/errors/timeseries')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/events/timeseries')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/transactions/timeseries')).toBe(false);
  });

  it('does not scope the bare getApp endpoint, but does scope its sub-resources', () => {
    expect(shouldScopeUrl('/v1/apps/app-1')).toBe(false);
    expect(shouldScopeUrl('/v1/apps/app-1/')).toBe(false);
    // Regression guard for the matcher itself: a bare-app match must not
    // swallow every other `/v1/apps/{id}/...` read too.
    expect(shouldScopeUrl('/v1/apps/app-1/events')).toBe(true);
  });

  it('is false for an undefined url', () => {
    expect(shouldScopeUrl(undefined)).toBe(false);
  });

  // ---------------------------------------------------------------------
  // Regression matrix for the Critical this module was rewritten to fix:
  // the old rule was an opt-OUT list (scope everything except a few
  // substrings), so any route family the list's author didn't know about
  // got `environment_id` attached by default and 400'd. A grep of the
  // backend for `reject_environment_id` turned up five call sites; the old
  // list only covered two of them (funnels.rs, artifacts.rs). These are the
  // three it missed — monitors, notifications/alerts, admin — none of which
  // live under `/v1/apps/{id}/...`, so under the new opt-IN rule they are
  // unscoped by construction rather than by remembering to list them.
  // ---------------------------------------------------------------------
  it('does not scope monitors, alerting or admin routes (the routes the old opt-out list missed)', () => {
    expect(shouldScopeUrl('/v1/projects/proj-1/monitors')).toBe(false);
    expect(shouldScopeUrl('/v1/monitors/mon-1')).toBe(false);
    expect(shouldScopeUrl('/v1/monitors/mon-1/checks')).toBe(false);
    expect(shouldScopeUrl('/v1/monitors/mon-1/incidents')).toBe(false);
    expect(shouldScopeUrl('/v1/orgs/org-1/notification-channels')).toBe(false);
    expect(shouldScopeUrl('/v1/notification-channels/chan-1')).toBe(false);
    expect(shouldScopeUrl('/v1/notification-channels/chan-1/test')).toBe(false);
    expect(shouldScopeUrl('/v1/orgs/org-1/alert-rules')).toBe(false);
    expect(shouldScopeUrl('/v1/alert-rules/rule-1')).toBe(false);
    expect(shouldScopeUrl('/v1/orgs/org-1/alert-events')).toBe(false);
    expect(shouldScopeUrl('/v1/alert-meta')).toBe(false);
    expect(shouldScopeUrl('/v1/admin/storage')).toBe(false);
  });

  it('does not scope auth or bare environments routes', () => {
    expect(shouldScopeUrl('/v1/auth/login')).toBe(false);
    expect(shouldScopeUrl('/v1/me')).toBe(false);
    expect(shouldScopeUrl('/v1/environments/env-1')).toBe(false);
    expect(shouldScopeUrl('/v1/environments/env-1/rotate-key')).toBe(false);
  });
});

describe('computeScopeParams', () => {
  it('adds environment_id for a telemetry url when an environment is selected', () => {
    expect(computeScopeParams('/v1/apps/app-1/events', 'env-1')).toEqual({
      environment_id: 'env-1',
    });
  });

  it('passes the literal "none" (unattributed) straight through', () => {
    expect(computeScopeParams('/v1/apps/app-1/events', 'none')).toEqual({
      environment_id: 'none',
    });
  });

  it('adds nothing for an app-configuration exclusion, even with an environment selected', () => {
    expect(computeScopeParams('/v1/apps/app-1/environments', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/apps/app-1/funnels', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/apps/app-1/artifacts', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/apps/app-1/first-event', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/apps/app-1', 'env-1')).toBeUndefined();
  });

  it('adds nothing for routes outside /v1/apps/{id}/..., even with an environment selected', () => {
    // These are the exact URLs that a 400 was reaching production users on:
    // the opt-out list had no entry for any of them, so the old rule scoped
    // them by default. The opt-in rule leaves them unscoped by default.
    expect(computeScopeParams('/v1/monitors/mon-1/checks', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/orgs/org-1/alert-rules', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/alert-meta', 'env-1')).toBeUndefined();
    expect(computeScopeParams('/v1/admin/storage', 'env-1')).toBeUndefined();
  });

  it('adds nothing — not an empty-string parameter — when currentEnvId is null ("all")', () => {
    const result = computeScopeParams('/v1/apps/app-1/events', null);
    expect(result).toBeUndefined();
    // Guard the exact failure mode Task 10's review caught: a present-but-empty
    // `environment_id` is a hard 400 on the backend, not a synonym for "all".
    expect(result).not.toEqual({ environment_id: '' });
  });
});

// ---------------------------------------------------------------------------
// Guard: every telemetry page's data-loading effects must key on `scopeKey`,
// not just `currentAppId`. This test doesn't (can't, from here) prove every
// *effect* does it — it parses each page's source and asserts the string
// `scopeKey` appears somewhere in it, which is enough to catch the actual
// failure mode: a page added to (or missed from) this list that never
// touches `scopeKey` at all and so never re-fetches on an environment switch.
// Precedent: `../models/permissions.test.ts` already parses source off disk
// for the same reason (there, Rust; here, Svelte).
//
// Task 14 (F6): the target set below used to be a hand-written
// `TELEMETRY_PAGES` array that was — by construction — exactly the set of
// pages containing the string "scopeKey". That made the `missing` assertion
// `[]` for free and the companion "list is non-empty" assertion a tautology
// over a `const` defined three lines above: the array could never fail no
// matter what any page's source said. The target set is now DERIVED from the
// filesystem (every `.svelte` file under `dashboard/src/pages/`) minus an
// explicit, individually-justified non-telemetry allow-list, so a page added
// to `pages/` tomorrow that is not on the allow-list is automatically
// enforced — the developer must consciously either wire it to `scopeKey` or
// add it to the allow-list with a reason, rather than the test silently
// growing or shrinking around whatever already happens to be true.
// ---------------------------------------------------------------------------

const PAGES_DIR = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../pages');

/** Every page component's name (no extension) in `dashboard/src/pages/`. */
function listAllPageNames(): string[] {
  const suffix = '.svelte';
  return fs
    .readdirSync(PAGES_DIR)
    .filter((f: string) => f.endsWith(suffix))
    .map((f: string) => f.slice(0, -suffix.length));
}

function readPageSource(name: string): string {
  const file = path.join(PAGES_DIR, `${name}.svelte`);
  try {
    return fs.readFileSync(file, 'utf-8');
  } catch (err) {
    throw new Error(
      `scope.test.ts could not read page "${file}" (${
        err instanceof Error ? err.message : String(err)
      }). This test must fail rather than silently skip a page that was renamed or moved.`,
    );
  }
}

/**
 * Pages that are genuinely not app-scoped telemetry reads — org/account/auth
 * chrome, or a resource with no `environment_id` dimension at all. Each entry
 * carries its own reason: a page landing here for the wrong reason is exactly
 * how F6 (and the four backend `environment_id` regressions before it)
 * happened — a hand-maintained list nobody re-checks against the thing it
 * claims to describe.
 */
const NON_TELEMETRY_PAGES = new Set([
  'Alerts', // org-scoped alert rules/channels, not app-scoped telemetry
  'ChangePassword', // account settings, no app/environment dimension
  'Docs', // static documentation
  'Login', // pre-auth
  'Members', // org membership management
  'MonitorDetail', // uptime monitor detail, project-scoped not environment-scoped
  'Monitors', // uptime monitors list, project-scoped not environment-scoped
  // Polls `apps::first_event` by the app's own id, deliberately NOT scoped to
  // sessionStore.currentEnvId — see scope.ts's UI_ONLY_EXCLUSIONS comment.
  'Onboarding',
  'Projects', // org-scoped project list
  'Register', // pre-auth
  'SettingsApp', // app configuration (name, environments, keys), not telemetry
  'SourceMaps', // symbol artifacts — app-wide config; backend rejects environment_id
  'Storage', // admin storage report, org-scoped
]);

const ALL_PAGES = listAllPageNames();
const TELEMETRY_PAGES = ALL_PAGES.filter((name) => !NON_TELEMETRY_PAGES.has(name));

describe('telemetry pages observe scopeKey', () => {
  it('every page not on the non-telemetry allow-list references sessionStore.scopeKey somewhere in its effects', () => {
    const missing = TELEMETRY_PAGES.filter((name) => !readPageSource(name).includes('scopeKey'));
    expect(missing, `pages missing a scopeKey reference: ${missing.join(', ')}`).toEqual([]);
  });

  it('the derived set is non-empty and strictly smaller than the full page list (guards the parser against silently matching nothing, or the allow-list against silently excluding everything)', () => {
    expect(TELEMETRY_PAGES.length).toBeGreaterThan(0);
    expect(TELEMETRY_PAGES.length).toBeLessThan(ALL_PAGES.length);
  });

  it('every allow-listed page name still exists on disk (catches a renamed/deleted page going silently unenforced)', () => {
    const present = new Set(ALL_PAGES);
    const stale = [...NON_TELEMETRY_PAGES].filter((name) => !present.has(name));
    expect(stale, `allow-listed page(s) no longer on disk: ${stale.join(', ')}`).toEqual([]);
  });

  // FunnelBuilder is the one page whose *recompute* effect tracks
  // `currentEnvId` directly instead of `scopeKey` — an intentional exception,
  // not a gap this suite should paper over with a bare skip. It still passes
  // the coarse "somewhere in the file" check above, because its sibling
  // `loadEvents` effect keys on `scopeKey` for the app+environment switch
  // case. But the recompute effect specifically must NOT track `scopeKey`:
  // that would fold `currentAppId` back in and refire on every app switch —
  // the exact thing its `untrack()` exists to prevent (see the effect's own
  // comment in FunnelBuilder.svelte). Asserted here, by name, so the reason
  // lives in the test rather than as a silent exclusion.
  it('FunnelBuilder tracks currentEnvId (not scopeKey) in its recompute effect, by design', () => {
    const src = readPageSource('FunnelBuilder');
    expect(src).toContain('sessionStore.currentEnvId');
  });
});
