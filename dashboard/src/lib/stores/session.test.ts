import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { App, Environment } from '../models';

vi.mock('../api/orgs', () => ({
  getAccess: vi.fn(),
  listOrgs: vi.fn(),
}));
vi.mock('../api/projects', () => ({
  listProjects: vi.fn(),
}));
vi.mock('../api/apps', () => ({
  listApps: vi.fn(),
}));
vi.mock('../api/environments', () => ({
  listEnvironments: vi.fn(),
}));

import { listApps } from '../api/apps';
import { listEnvironments } from '../api/environments';
import { listProjects } from '../api/projects';
import { getAccess, listOrgs } from '../api/orgs';
import { sessionStore } from './session.svelte';

const mockListEnvironments = vi.mocked(listEnvironments);
const mockListApps = vi.mocked(listApps);
const mockListProjects = vi.mocked(listProjects);
const mockGetAccess = vi.mocked(getAccess);
const mockListOrgs = vi.mocked(listOrgs);

// ---------------------------------------------------------------------------
// A minimal in-memory localStorage so the store's persistence branch
// (`typeof window === 'undefined'` guards) actually exercises, rather than
// silently no-op'ing the way it would under plain Node with no `window`.
// ---------------------------------------------------------------------------
class FakeStorage implements Storage {
  private map = new Map<string, string>();
  get length() {
    return this.map.size;
  }
  clear(): void {
    this.map.clear();
  }
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  key(index: number): string | null {
    return Array.from(this.map.keys())[index] ?? null;
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
}

const ENV_KEY = 'sauron.environment_id';
const APP_KEY = 'sauron.app_id';
const PROJECT_KEY = 'sauron.project_id';
const ORG_KEY = 'sauron.org_id';

function makeApp(id: string, overrides: Partial<App> = {}): App {
  return {
    id,
    project_id: 'proj-1',
    name: id,
    slug: id,
    app_type: 'web',
    ingest_enabled: true,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  };
}

function makeEnv(id: string, overrides: Partial<Environment> = {}): Environment {
  return {
    id,
    app_id: 'app-1',
    name: id,
    created_at: '2026-01-01T00:00:00Z',
    public_key: `pk_${id}`,
    ingest_enabled: true,
    is_default: false,
    retired_at: null,
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  };
}

/** Reset the singleton store back to a blank slate before every test. */
function resetStore() {
  sessionStore.orgs = [];
  sessionStore.projects = [];
  sessionStore.apps = [];
  sessionStore.environments = [];
  sessionStore.environmentsError = false;
  sessionStore.currentOrgId = null;
  sessionStore.currentProjectId = null;
  sessionStore.currentAppId = null;
  sessionStore.currentEnvId = null;
  sessionStore.access = null;
  sessionStore.loaded = false;
  sessionStore.loading = false;
  // `environmentsLoadAttemptedFor` is a private bookkeeping field (not part
  // of the public store surface) that the singleton would otherwise carry
  // over between tests, unlike every other field reset above.
  (sessionStore as unknown as { environmentsLoadAttemptedFor: string | null }).environmentsLoadAttemptedFor =
    null;
}

beforeEach(() => {
  vi.stubGlobal('window', { localStorage: new FakeStorage() } as unknown as Window & typeof globalThis);
  resetStore();
  vi.clearAllMocks();
});

describe('resolveCurrentEnvironment (via setApp)', () => {
  it('picks the is_default environment rather than [0]', async () => {
    sessionStore.currentAppId = 'app-0';
    const notDefault = makeEnv('env-a', { is_default: false });
    const isDefault = makeEnv('env-b', { is_default: true });
    mockListEnvironments.mockResolvedValue([notDefault, isDefault]);

    await sessionStore.setApp('app-1');

    expect(sessionStore.currentEnvId).toBe('env-b');
    // Active-only: the store's picker must never call out for retired rows.
    expect(mockListEnvironments).toHaveBeenCalledWith('app-1');
  });

  it('falls back to null when the app has no environments at all', async () => {
    sessionStore.currentAppId = 'app-0';
    mockListEnvironments.mockResolvedValue([]);

    await sessionStore.setApp('app-1');

    expect(sessionStore.currentEnvId).toBeNull();
    expect(sessionStore.environments).toEqual([]);
  });

  it('honors a previously stored id on a fresh resolution (e.g. a page reload)', async () => {
    // `setOrg`/`setProject`/`setApp` deliberately clear downstream storage —
    // that's the "belongs to the previous X" invalidation under test
    // elsewhere. A reload is different: nothing has switched, so `load()`
    // must read every stored id back rather than clearing any of them.
    window.localStorage.setItem(ORG_KEY, 'org-1');
    window.localStorage.setItem(PROJECT_KEY, 'proj-1');
    window.localStorage.setItem(APP_KEY, 'app-1');
    window.localStorage.setItem(ENV_KEY, 'env-b');

    mockListOrgs.mockResolvedValue([
      { id: 'org-1', name: 'Org 1', slug: 'org-1', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' },
    ]);
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([
      { id: 'proj-1', org_id: 'org-1', name: 'P1', slug: 'p1', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' },
    ]);
    mockListApps.mockResolvedValue([makeApp('app-1')]);
    const a = makeEnv('env-a', { is_default: true });
    const b = makeEnv('env-b', { is_default: false });
    mockListEnvironments.mockResolvedValue([a, b]);

    await sessionStore.load();

    expect(sessionStore.currentEnvId).toBe('env-b');
  });
});

describe('setApp', () => {
  it('clears the stored environment — it belongs to the previous app', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.environments = [makeEnv('env-1', { app_id: 'app-1', is_default: true })];
    window.localStorage.setItem(APP_KEY, 'app-1');
    window.localStorage.setItem(ENV_KEY, 'env-1');

    const newDefault = makeEnv('env-2', { app_id: 'app-2', is_default: true });
    mockListEnvironments.mockResolvedValue([newDefault]);

    await sessionStore.setApp('app-2');

    // The previous app's environment id must never leak into the new app's
    // scope — either transiently in localStorage or in the final state.
    expect(sessionStore.currentEnvId).not.toBe('env-1');
    expect(sessionStore.currentEnvId).toBe('env-2');
    expect(window.localStorage.getItem(ENV_KEY)).not.toBe('env-1');
  });

  it('is a no-op when selecting the app that is already current', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';

    await sessionStore.setApp('app-1');

    expect(mockListEnvironments).not.toHaveBeenCalled();
    expect(sessionStore.currentEnvId).toBe('env-1');
  });
});

describe('setOrg / setProject clear the stored environment', () => {
  it('setOrg clears currentEnvId and the persisted key', async () => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentEnvId = 'env-1';
    window.localStorage.setItem(ENV_KEY, 'env-1');

    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([]);

    await sessionStore.setOrg('org-2');

    expect(sessionStore.currentEnvId).toBeNull();
    expect(window.localStorage.getItem(ENV_KEY)).toBeNull();
  });

  it('setProject clears currentEnvId and the persisted key', async () => {
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentEnvId = 'env-1';
    window.localStorage.setItem(ENV_KEY, 'env-1');

    mockListApps.mockResolvedValue([]);

    await sessionStore.setProject('proj-2');

    expect(sessionStore.currentEnvId).toBeNull();
    expect(window.localStorage.getItem(ENV_KEY)).toBeNull();
  });
});

describe('removeApp', () => {
  it('clears the stored environment when the removed app was current', () => {
    sessionStore.apps = [makeApp('app-1')];
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.environments = [makeEnv('env-1')];
    window.localStorage.setItem(APP_KEY, 'app-1');
    window.localStorage.setItem(ENV_KEY, 'env-1');

    sessionStore.removeApp('app-1');

    expect(sessionStore.currentEnvId).toBeNull();
    expect(sessionStore.environments).toEqual([]);
    expect(window.localStorage.getItem(ENV_KEY)).toBeNull();
  });

  it('leaves the stored environment alone when a different app was removed', () => {
    sessionStore.apps = [makeApp('app-1'), makeApp('app-2')];
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.environments = [makeEnv('env-1')];
    window.localStorage.setItem(ENV_KEY, 'env-1');

    sessionStore.removeApp('app-2');

    expect(sessionStore.currentEnvId).toBe('env-1');
    expect(window.localStorage.getItem(ENV_KEY)).toBe('env-1');
  });
});

describe('reset', () => {
  it('clears the stored environment along with the other three keys', () => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.environments = [makeEnv('env-1')];
    window.localStorage.setItem(ORG_KEY, 'org-1');
    window.localStorage.setItem(PROJECT_KEY, 'proj-1');
    window.localStorage.setItem(APP_KEY, 'app-1');
    window.localStorage.setItem(ENV_KEY, 'env-1');

    sessionStore.reset();

    expect(sessionStore.currentEnvId).toBeNull();
    expect(sessionStore.environments).toEqual([]);
    expect(window.localStorage.getItem(ENV_KEY)).toBeNull();
    // The pre-existing three stay covered too — a regression here would be
    // just as bad as missing the new one.
    expect(window.localStorage.getItem(ORG_KEY)).toBeNull();
    expect(window.localStorage.getItem(PROJECT_KEY)).toBeNull();
    expect(window.localStorage.getItem(APP_KEY)).toBeNull();
  });
});

describe('scopeKey', () => {
  it('changes when the environment changes but the app does not', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
    const base = sessionStore.scopeKey;
    expect(base).toBe('app-1:all');

    sessionStore.setEnvironment('env-1');

    expect(sessionStore.scopeKey).not.toBe(base);
    expect(sessionStore.scopeKey).toBe('app-1:env-1');
  });

  it('changes when the app changes but the environment does not', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    const base = sessionStore.scopeKey;
    expect(base).toBe('app-1:env-1');

    sessionStore.currentAppId = 'app-2';

    expect(sessionStore.scopeKey).not.toBe(base);
    expect(sessionStore.scopeKey).toBe('app-2:env-1');
  });

  it('treats "none" (unattributed) as distinct from "all"', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
    const all = sessionStore.scopeKey;

    sessionStore.setEnvironment('none');

    expect(sessionStore.scopeKey).not.toBe(all);
    expect(sessionStore.scopeKey).toBe('app-1:none');
  });
});

describe('ensureEnvironmentsLoaded', () => {
  it('loads when currentAppId is set but environments is empty (the removeApp gap)', async () => {
    // Mirrors what removeApp leaves behind: the replacement app is current,
    // but its environments were never fetched.
    sessionStore.currentAppId = 'app-2';
    sessionStore.environments = [];
    const def = makeEnv('env-2', { app_id: 'app-2', is_default: true });
    mockListEnvironments.mockResolvedValue([def]);

    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).toHaveBeenCalledWith('app-2');
    expect(sessionStore.environments).toEqual([def]);
    expect(sessionStore.currentEnvId).toBe('env-2');
  });

  it('is a no-op when environments are already populated', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.environments = [makeEnv('env-1')];

    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).not.toHaveBeenCalled();
  });

  it('is a no-op when there is no current app', async () => {
    sessionStore.currentAppId = null;
    sessionStore.environments = [];

    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).not.toHaveBeenCalled();
  });

  it('does not repeatedly call the API when a genuinely-empty result recurs for the same app', async () => {
    sessionStore.currentAppId = 'app-3';
    sessionStore.environments = [];
    mockListEnvironments.mockResolvedValue([]);

    await sessionStore.ensureEnvironmentsLoaded();
    await sessionStore.ensureEnvironmentsLoaded();
    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).toHaveBeenCalledTimes(1);
  });

  it('attempts again for a different app after a genuinely-empty result', async () => {
    sessionStore.currentAppId = 'app-3';
    sessionStore.environments = [];
    mockListEnvironments.mockResolvedValue([]);
    await sessionStore.ensureEnvironmentsLoaded();

    sessionStore.currentAppId = 'app-4';
    sessionStore.environments = [];
    const def = makeEnv('env-4', { app_id: 'app-4', is_default: true });
    mockListEnvironments.mockResolvedValue([def]);
    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).toHaveBeenCalledTimes(2);
    expect(sessionStore.currentEnvId).toBe('env-4');
  });
});

// ---------------------------------------------------------------------------
// F4/F5 whole-branch-review regression: `loadAppEnvironments` used to swallow
// a failed fetch with `.catch(() => [])`, so `environments` became `[]`,
// `resolveCurrentEnvironment` found no match, `currentEnvId` was cleared to
// `null` ("all environments") and that `null` was persisted to localStorage —
// silently *widening* what a scoped viewer sees on nothing more than a
// network blip, the opposite of `routes/scope.rs`'s own fail-closed rule. A
// test that only exercises the happy path (all the `ensureEnvironmentsLoaded`
// tests above) passes just as well with that bug present, so this block
// specifically drives the rejection path.
// ---------------------------------------------------------------------------
describe('loadAppEnvironments failure (store must fail closed, not open)', () => {
  it('leaves currentEnvId (and its persisted value) unchanged when listEnvironments rejects', async () => {
    // Models a page bootstrap: a previously-resolved, still-valid selection
    // in both memory and localStorage, and `environments` not yet
    // (re-)fetched this session.
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.environments = [];
    window.localStorage.setItem(APP_KEY, 'app-1');
    window.localStorage.setItem(ENV_KEY, 'env-1');
    mockListEnvironments.mockRejectedValueOnce(new Error('network blip'));

    await sessionStore.ensureEnvironmentsLoaded();

    expect(sessionStore.currentEnvId).toBe('env-1');
    expect(window.localStorage.getItem(ENV_KEY)).toBe('env-1');
    // The failed fetch says nothing about whether the app genuinely has
    // environments — `environments` must stay whatever it was, not become `[]`.
    expect(sessionStore.environments).toEqual([]);
    expect(sessionStore.environmentsError).toBe(true);
  });

  it('distinguishes a successful empty result (environmentsError stays false) from a failure', async () => {
    sessionStore.currentAppId = 'app-5';
    sessionStore.environments = [];
    mockListEnvironments.mockResolvedValueOnce([]);

    await sessionStore.ensureEnvironmentsLoaded();

    expect(sessionStore.environmentsError).toBe(false);
    expect(sessionStore.environments).toEqual([]);

    sessionStore.currentAppId = 'app-6';
    sessionStore.environments = [];
    mockListEnvironments.mockRejectedValueOnce(new Error('boom'));

    await sessionStore.ensureEnvironmentsLoaded();

    expect(sessionStore.environmentsError).toBe(true);
  });

  it('makes a failed load retryable, unlike a genuinely-empty successful one', async () => {
    sessionStore.currentAppId = 'app-7';
    sessionStore.environments = [];
    mockListEnvironments.mockRejectedValueOnce(new Error('boom'));

    await sessionStore.ensureEnvironmentsLoaded();
    expect(mockListEnvironments).toHaveBeenCalledTimes(1);

    // Same app, called again (e.g. the user re-opens the picker): the prior
    // attempt never actually completed, so this must hit the API again —
    // contrast with `does not repeatedly call the API when a
    // genuinely-empty result recurs for the same app` above, which asserts
    // the opposite for a *successful* empty result.
    const def = makeEnv('env-7', { app_id: 'app-7', is_default: true });
    mockListEnvironments.mockResolvedValueOnce([def]);

    await sessionStore.ensureEnvironmentsLoaded();

    expect(mockListEnvironments).toHaveBeenCalledTimes(2);
    expect(sessionStore.environmentsError).toBe(false);
    expect(sessionStore.currentEnvId).toBe('env-7');
  });
});

// ---------------------------------------------------------------------------
// Regression coverage for the double-fetch bug: `setApp` (and, by the same
// mechanism, `loadProjectApps`/`load()`) clears `environments` to `[]`
// *synchronously*, before awaiting its own `loadAppEnvironments` call. The
// Topbar's `$effect` reacts to that same synchronous transition and, seeing
// `currentAppId` set with `environments` empty, calls `ensureEnvironmentsLoaded`
// itself. Svelte defers `$effect` bodies to a microtask rather than running them
// inline, so a `Promise.resolve().then(...)` faithfully models "the effect's
// next flush" without needing to mount the component. Asserting only the end
// state (as the tests above do) passes even with the bug present — a second,
// concurrent `listEnvironments` call is idempotent — so these assert the call
// count instead.
// ---------------------------------------------------------------------------
describe('setApp vs. the Topbar self-heal effect (no duplicate concurrent fetch)', () => {
  /** Mirrors Topbar.svelte's `$effect` body exactly. */
  function simulateTopbarEffectFlush(): Promise<void> {
    return Promise.resolve().then(async () => {
      if (sessionStore.currentAppId && sessionStore.environments.length === 0) {
        await sessionStore.ensureEnvironmentsLoaded();
      }
    });
  }

  it('issues exactly one load for an ordinary app switch', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.environments = [makeEnv('env-1', { app_id: 'app-1', is_default: true })];
    const newDefault = makeEnv('env-2', { app_id: 'app-2', is_default: true });
    mockListEnvironments.mockResolvedValue([newDefault]);

    const setAppDone = sessionStore.setApp('app-2');
    const effectDone = simulateTopbarEffectFlush();
    await Promise.all([setAppDone, effectDone]);

    expect(mockListEnvironments).toHaveBeenCalledTimes(1);
    expect(mockListEnvironments).toHaveBeenCalledWith('app-2');
    expect(sessionStore.currentEnvId).toBe('env-2');
  });

  it('still self-heals exactly once after removeApp switches away without loading', async () => {
    // Sets up the carry-forward gap the effect exists for: app-1 is current
    // with real environments loaded (so `environmentsLoadAttemptedFor` is
    // already 'app-1'), then it's removed and `resolveCurrentApp` lands on
    // app-2, whose environments were never fetched.
    sessionStore.apps = [makeApp('app-1'), makeApp('app-2')];
    sessionStore.currentAppId = 'app-1';
    (sessionStore as unknown as { environmentsLoadAttemptedFor: string | null }).environmentsLoadAttemptedFor =
      'app-1';
    sessionStore.environments = [makeEnv('env-1', { app_id: 'app-1', is_default: true })];
    window.localStorage.setItem(APP_KEY, 'app-1');

    const appTwoDefault = makeEnv('env-2', { app_id: 'app-2', is_default: true });
    mockListEnvironments.mockResolvedValue([appTwoDefault]);

    sessionStore.removeApp('app-1');
    expect(sessionStore.currentAppId).toBe('app-2');
    expect(sessionStore.environments).toEqual([]);

    await simulateTopbarEffectFlush();

    expect(mockListEnvironments).toHaveBeenCalledTimes(1);
    expect(mockListEnvironments).toHaveBeenCalledWith('app-2');
    expect(sessionStore.currentEnvId).toBe('env-2');
  });
});

describe('setEnvironment', () => {
  it('sets the literal string "none" with no translation', () => {
    sessionStore.setEnvironment('none');
    expect(sessionStore.currentEnvId).toBe('none');
    expect(window.localStorage.getItem(ENV_KEY)).toBe('none');
  });

  it('sets null for "all environments" and clears the persisted key', () => {
    window.localStorage.setItem(ENV_KEY, 'env-1');
    sessionStore.setEnvironment(null);
    expect(sessionStore.currentEnvId).toBeNull();
    expect(window.localStorage.getItem(ENV_KEY)).toBeNull();
  });
});
