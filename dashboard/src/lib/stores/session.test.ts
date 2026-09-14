import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { App, AppEnvironment, AppRelease } from '../models';

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
vi.mock('../api/releases', () => ({
  listReleases: vi.fn(),
}));

import { listApps } from '../api/apps';
import { listEnvironments } from '../api/environments';
import { listReleases } from '../api/releases';
import { listProjects } from '../api/projects';
import { getAccess, listOrgs } from '../api/orgs';
import { sessionStore } from './session.svelte';

const mockListEnvironments = vi.mocked(listEnvironments);
const mockListReleases = vi.mocked(listReleases);
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
    store_environment_id: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  };
}

function makeEnv(id: string, overrides: Partial<AppEnvironment> = {}): AppEnvironment {
  return {
    // `id` is the ENROLLMENT id — the one the store selects on, persists and
    // sends as `?environment_id=`. `environment_id` names the project-level
    // catalogue entry this app is enrolled in, and is deliberately different
    // here so a test that confuses the two fails.
    id,
    app_id: 'app-1',
    environment_id: `cat-${id}`,
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

function makeRelease(release: string, overrides: Partial<AppRelease> = {}): AppRelease {
  return {
    release,
    environment_ids: [null],
    first_seen_at: '2026-01-01T00:00:00Z',
    last_seen_at: '2026-01-01T00:00:00Z',
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
  sessionStore.releases = [];
  sessionStore.releasesError = false;
  sessionStore.currentOrgId = null;
  sessionStore.currentProjectId = null;
  sessionStore.currentAppId = null;
  sessionStore.currentEnvId = null;
  sessionStore.currentRelease = null;
  sessionStore.access = null;
  sessionStore.loaded = false;
  sessionStore.loading = false;
  // `environmentsLoadAttemptedFor` is a private bookkeeping field (not part
  // of the public store surface) that the singleton would otherwise carry
  // over between tests, unlike every other field reset above.
  (sessionStore as unknown as { environmentsLoadAttemptedFor: string | null }).environmentsLoadAttemptedFor =
    null;
  // Same reasoning for the release equivalent.
  (sessionStore as unknown as { releasesLoadAttemptedFor: string | null }).releasesLoadAttemptedFor = null;
  // Same reasoning for `loadPromise` (Task 15, Item 3) — every test that sets
  // it awaits `load()` to completion, which always resets it to `null` itself
  // (success or failure), but reset it here too rather than rely on that.
  (sessionStore as unknown as { loadPromise: Promise<void> | null }).loadPromise = null;
}

beforeEach(() => {
  vi.stubGlobal('window', { localStorage: new FakeStorage() } as unknown as Window & typeof globalThis);
  resetStore();
  vi.clearAllMocks();
  // Default to no releases so every pre-existing test that reaches
  // `loadAppReleases` via `setApp`/`setOrg`/`setProject`/`load()` without
  // caring about releases doesn't have to mock it explicitly — mirrors the
  // "genuinely empty" success path `loadAppEnvironments` already handles.
  mockListReleases.mockResolvedValue([]);
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
      { id: 'org-1', name: 'Org 1', slug: 'org-1', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z', project_count: 1, can_create_project: true },
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

  // The whole point of the split: ~20 telemetry pages read rollups with no
  // release dimension, and folding the release into `scopeKey` made every one
  // of them re-fetch an identical payload on each switch of the Topbar's
  // release picker.
  it('does NOT change when only the release changes', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
    sessionStore.currentRelease = null;
    const base = sessionStore.scopeKey;

    sessionStore.setRelease('1.4.0');

    expect(sessionStore.scopeKey).toBe(base);
  });
});

describe('scopeKeyWithRelease', () => {
  it('changes when the release changes but the app and environment do not', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
    sessionStore.currentRelease = null;
    const base = sessionStore.scopeKeyWithRelease;
    expect(base).toBe('app-1:all:all');

    sessionStore.setRelease('1.4.0');

    expect(sessionStore.scopeKeyWithRelease).not.toBe(base);
    expect(sessionStore.scopeKeyWithRelease).toBe('app-1:all:1.4.0');
  });

  // Prefixed by `scopeKey`, so a release-aware page still re-fetches on an
  // app or environment switch — it does not trade one dimension for another.
  it('still changes when the environment or the app changes', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
    sessionStore.currentRelease = '1.4.0';
    expect(sessionStore.scopeKeyWithRelease).toBe('app-1:all:1.4.0');

    sessionStore.setEnvironment('env-1');
    expect(sessionStore.scopeKeyWithRelease).toBe('app-1:env-1:1.4.0');

    sessionStore.currentAppId = 'app-2';
    expect(sessionStore.scopeKeyWithRelease).toBe('app-2:env-1:1.4.0');
  });

  it('spells "all releases" as the `all` sentinel, distinct from a release named "none"', () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = 'env-1';
    sessionStore.currentRelease = null;
    expect(sessionStore.scopeKeyWithRelease).toBe('app-1:env-1:all');

    sessionStore.currentRelease = 'none';
    expect(sessionStore.scopeKeyWithRelease).toBe('app-1:env-1:none');
  });
});

describe('releases', () => {
  it('persists the release per app and clears it when the stored value is stale', async () => {
    sessionStore.currentAppId = 'app-0';
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockResolvedValue([makeRelease('1.4.0')]);
    window.localStorage.setItem('sauron.release:app-1', '1.4.0');

    await sessionStore.setApp('app-1');

    expect(sessionStore.currentRelease).toBe('1.4.0');
    expect(mockListReleases).toHaveBeenCalledWith('app-1');

    mockListReleases.mockResolvedValue([makeRelease('1.4.0')]);
    window.localStorage.setItem('sauron.release:app-2', '9.9.9');

    await sessionStore.setApp('app-2');

    expect(sessionStore.currentRelease).toBeNull();
    expect(window.localStorage.getItem('sauron.release:app-2')).toBeNull();
  });

  it('setApp clears releases and currentRelease — they belong to the previous app', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentRelease = '1.4.0';
    sessionStore.releases = [makeRelease('1.4.0')];
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockResolvedValue([]);

    await sessionStore.setApp('app-2');

    expect(sessionStore.currentRelease).toBeNull();
    expect(sessionStore.releases).toEqual([]);
  });

  it('setOrg and setProject clear currentRelease along with currentEnvId', async () => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentRelease = '1.4.0';
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([]);

    await sessionStore.setOrg('org-2');

    expect(sessionStore.currentRelease).toBeNull();

    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentRelease = '1.4.0';
    mockListApps.mockResolvedValue([]);

    await sessionStore.setProject('proj-2');

    expect(sessionStore.currentRelease).toBeNull();
  });

  describe('setRelease', () => {
    it('narrows the environment to "all" when the current env has not seen the selected release', () => {
      sessionStore.currentAppId = 'app-1';
      sessionStore.currentEnvId = 'env-1';
      sessionStore.releases = [makeRelease('1.4.0', { environment_ids: ['env-2'] })];

      sessionStore.setRelease('1.4.0');

      expect(sessionStore.currentRelease).toBe('1.4.0');
      expect(sessionStore.currentEnvId).toBeNull();
    });

    it('leaves the environment alone when it has seen the selected release', () => {
      sessionStore.currentAppId = 'app-1';
      sessionStore.currentEnvId = 'env-1';
      sessionStore.releases = [makeRelease('1.4.0', { environment_ids: ['env-1'] })];

      sessionStore.setRelease('1.4.0');

      expect(sessionStore.currentEnvId).toBe('env-1');
    });

    it('leaves "none" (unattributed) untouched regardless of the release', () => {
      sessionStore.currentAppId = 'app-1';
      sessionStore.currentEnvId = 'none';
      sessionStore.releases = [makeRelease('1.4.0', { environment_ids: ['env-2'] })];

      sessionStore.setRelease('1.4.0');

      expect(sessionStore.currentEnvId).toBe('none');
    });

    /**
     * The switcher offers the TRIMMED name (`selectableReleases`), so a padded
     * catalogue row must still be found by the env-fallback lookup. With a raw
     * `x.release === id` compare the row is never found, the guard silently
     * no-ops, and the app is left on an (env, release) pair the release was
     * never seen in. Padded rows only predate the ingest-edge normalisation,
     * but the lookup has to agree with the menu that produced the value.
     */
    it('matches a padded catalogue row by its trimmed name when falling back', () => {
      sessionStore.currentAppId = 'app-1';
      sessionStore.currentEnvId = 'env-a';
      sessionStore.releases = [makeRelease(' 2.0.0', { environment_ids: ['env-b'] })];

      sessionStore.setRelease('2.0.0');

      expect(sessionStore.currentRelease).toBe('2.0.0');
      expect(sessionStore.currentEnvId).toBeNull();
    });

    it('persists the selection per app and clears it via setRelease(null)', () => {
      sessionStore.currentAppId = 'app-1';

      sessionStore.setRelease('1.4.0');
      expect(window.localStorage.getItem('sauron.release:app-1')).toBe('1.4.0');

      sessionStore.setRelease(null);
      expect(sessionStore.currentRelease).toBeNull();
      expect(window.localStorage.getItem('sauron.release:app-1')).toBeNull();
    });
  });
});

// ---------------------------------------------------------------------------
// Stale continuations.
//
// `setApp` starts two fetches and awaits them; nothing cancels an in-flight
// one. A user clicking through the app switcher faster than the network
// answers therefore has two loads in flight for two DIFFERENT apps, and the
// OLDER one can land last. Without a guard after each `await`, that older
// continuation writes its app's environments/releases over the newer app's —
// the picker then lists environments that belong to an app you are no longer
// looking at, and every request the interceptor scopes goes out with an
// `environment_id` the current app does not own (a 403, or worse, a silent
// cross-app read if the id happens to be reachable).
//
// The guard is a generation counter (`loadGen`), not an app-id comparison,
// because the two loads in flight need not be for two different apps: A→B→A
// leaves two `app-1` loads racing while `currentAppId` reads `'app-1'` for
// both. See the A→B→A test below.
// ---------------------------------------------------------------------------
describe('rapid app switching', () => {
  /** A `listX` mock whose `app-1` answer is held open until the test releases it. */
  function gated<T>(slow: T, fast: T) {
    let open!: () => void;
    const gate = new Promise<void>((resolve) => {
      open = resolve;
    });
    const impl = async (appId: string): Promise<T> => {
      if (appId === 'app-1') {
        await gate;
        return slow;
      }
      return fast;
    };
    return { impl, open };
  }

  it('a stale loadAppReleases continuation is discarded, not applied', async () => {
    const oldReleases = [makeRelease('1.0.0')];
    const newReleases = [makeRelease('2.0.0')];
    const { impl, open } = gated(oldReleases, newReleases);
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockImplementation(impl);

    const first = sessionStore.setApp('app-1');
    const second = sessionStore.setApp('app-2');
    await second;
    expect(sessionStore.releases).toEqual(newReleases);

    // app-1's fetch answers only now, long after the user moved on.
    open();
    await first;

    expect(sessionStore.currentAppId).toBe('app-2');
    expect(sessionStore.releases).toEqual(newReleases);
  });

  it('a stale loadAppEnvironments continuation is discarded, not applied', async () => {
    const oldEnvs = [makeEnv('env-old', { is_default: true })];
    const newEnvs = [makeEnv('env-new', { is_default: true })];
    const { impl, open } = gated(oldEnvs, newEnvs);
    mockListReleases.mockResolvedValue([]);
    mockListEnvironments.mockImplementation(impl);

    const first = sessionStore.setApp('app-1');
    const second = sessionStore.setApp('app-2');
    await second;
    expect(sessionStore.currentEnvId).toBe('env-new');

    open();
    await first;

    expect(sessionStore.environments).toEqual(newEnvs);
    // The selection is the part that actually reaches the wire, so it matters
    // more than the list: `resolveCurrentEnvironment` running for the stale
    // app is what would put `env-old` into every scoped request.
    expect(sessionStore.currentEnvId).toBe('env-new');
  });

  /**
   * The case an `appId`-identity guard cannot see. Going A→B→A leaves TWO
   * `app-1` loads in flight; when the first one finally answers,
   * `currentAppId` is `'app-1'` again, so `this.currentAppId !== appId` is
   * false and the oldest response is applied over the newest. Only a
   * generation counter (`loadGen`, bumped by every `setApp`) distinguishes
   * "this app" from "this load".
   */
  it('a same-app A→B→A first continuation does not overwrite the second A result', async () => {
    const firstA = [makeRelease('1.0.0')];
    const secondA = [makeRelease('3.0.0')];
    const bList = [makeRelease('2.0.0')];

    // Hold app-1's FIRST answer open; its second call resolves immediately.
    let open!: () => void;
    const gate = new Promise<void>((resolve) => {
      open = resolve;
    });
    let aCalls = 0;
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockImplementation(async (appId: string) => {
      if (appId !== 'app-1') return bList;
      aCalls += 1;
      if (aCalls === 1) {
        await gate;
        return firstA;
      }
      return secondA;
    });

    const first = sessionStore.setApp('app-1');
    await sessionStore.setApp('app-2');
    await sessionStore.setApp('app-1');

    expect(aCalls).toBe(2);
    expect(sessionStore.releases).toEqual(secondA);

    // The very first app-1 fetch answers last.
    open();
    await first;

    expect(sessionStore.currentAppId).toBe('app-1');
    expect(sessionStore.releases).toEqual(secondA);
  });

  it('a stale FAILED load does not raise an error flag over the newer app', async () => {
    // The failure path writes state too (`releasesError`, and it clears the
    // attempted-for stamp so a retry is possible). Both belong to the app that
    // failed — applied to the app now on screen they show an error banner for
    // a load that succeeded, and re-open the door to a duplicate fetch.
    let reject!: (err: Error) => void;
    const failing = new Promise<AppRelease[]>((_, r) => {
      reject = r;
    });
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockImplementation(async (appId: string) =>
      appId === 'app-1' ? failing : [makeRelease('2.0.0')],
    );

    const first = sessionStore.setApp('app-1');
    const second = sessionStore.setApp('app-2');
    await second;

    reject(new Error('network'));
    await first;

    expect(sessionStore.releasesError).toBe(false);
    expect(sessionStore.releases).toEqual([makeRelease('2.0.0')]);
  });
});

describe('resolveCurrentRelease', () => {
  it('does not touch localStorage when the app has no stored release', async () => {
    // `writeStored(key, null)` is a `removeItem`, and the overwhelmingly
    // common case — an app whose release was never pinned — hit it on every
    // single app switch and every bootstrap.
    const removeItem = vi.spyOn(window.localStorage, 'removeItem');
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockResolvedValue([makeRelease('1.4.0')]);

    await sessionStore.setApp('app-9');

    expect(sessionStore.currentRelease).toBeNull();
    expect(
      removeItem.mock.calls.filter(([k]) => k === 'sauron.release:app-9'),
      'a release that was never stored has nothing to clear',
    ).toEqual([]);
    removeItem.mockRestore();
  });

  it('still clears a stored release that no longer exists', async () => {
    window.localStorage.setItem('sauron.release:app-9', '0.0.1-gone');
    mockListEnvironments.mockResolvedValue([]);
    mockListReleases.mockResolvedValue([makeRelease('1.4.0')]);

    await sessionStore.setApp('app-9');

    expect(sessionStore.currentRelease).toBeNull();
    expect(window.localStorage.getItem('sauron.release:app-9')).toBeNull();
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
      if (sessionStore.currentAppId && sessionStore.releases.length === 0) {
        await sessionStore.ensureReleasesLoaded();
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

  // Regression for the `setApp` sequential-await bug: `loadAppEnvironments`
  // stamps `environmentsLoadAttemptedFor` synchronously, before its own
  // network round trip, but `loadAppReleases` was only ever invoked (and so
  // only ever stamped `releasesLoadAttemptedFor`) *after* that round trip
  // completed. That left a window, for the whole `listEnvironments` await,
  // where the Topbar effect's `ensureReleasesLoaded()` guard saw
  // `releasesLoadAttemptedFor !== 'app-2'` and fired a second, concurrent
  // `listReleases('app-2')` alongside `setApp`'s own. Fails against
  // sequential `await loadAppEnvironments(id); await loadAppReleases(id);`
  // and passes once both loads are started before either is awaited
  // (`Promise.all`).
  it('issues exactly one releases load for an ordinary app switch', async () => {
    sessionStore.currentAppId = 'app-1';
    sessionStore.environments = [makeEnv('env-1', { app_id: 'app-1', is_default: true })];
    sessionStore.releases = [makeRelease('1.0.0')];
    const newDefault = makeEnv('env-2', { app_id: 'app-2', is_default: true });
    mockListEnvironments.mockResolvedValue([newDefault]);
    mockListReleases.mockResolvedValue([makeRelease('2.0.0')]);

    const setAppDone = sessionStore.setApp('app-2');
    const effectDone = simulateTopbarEffectFlush();
    await Promise.all([setAppDone, effectDone]);

    expect(mockListReleases).toHaveBeenCalledTimes(1);
    expect(mockListReleases).toHaveBeenCalledWith('app-2');
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

// ---------------------------------------------------------------------------
// Task 15, Item 3 (`s2-task-13-dupe-fetch-fix.md`'s "out-of-scope finding"):
// `App.svelte`'s post-auth redirect (`push('/issues')`, which mounts a layout
// whose `onMount` calls `load()`) and `Login.svelte`'s own forced
// `load(true)` right after a successful sign-in can both fire within the same
// render pass, each starting a full bootstrap chain (`listOrgs` →
// `getAccess`/`listProjects` → …) concurrently. As with the Topbar-effect
// race above, asserting only the end state passes even with the bug present
// — a second, concurrent bootstrap is idempotent — so these assert the call
// count instead.
// ---------------------------------------------------------------------------
describe('load() in-flight idempotency (fresh-login double bootstrap)', () => {
  function mockBootstrapChain() {
    mockListOrgs.mockResolvedValue([
      {
        id: 'org-1',
        name: 'Org 1',
        slug: 'org-1',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
        project_count: 1,
        can_create_project: true,
      },
    ]);
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([]);
  }

  it('collapses a concurrent plain call and forced call into a single chain', async () => {
    mockBootstrapChain();

    // Models the race exactly: `fromAppShellMount` stands in for the layout
    // `onMount` fired by App.svelte's redirect; `fromLoginSubmit` stands in
    // for Login.svelte's own `await sessionStore.load(true)` — both start
    // before either has resolved.
    const fromAppShellMount = sessionStore.load();
    const fromLoginSubmit = sessionStore.load(true);
    await Promise.all([fromAppShellMount, fromLoginSubmit]);

    expect(mockListOrgs).toHaveBeenCalledTimes(1);
    expect(sessionStore.loaded).toBe(true);
  });

  it('collapses three overlapping callers, not just two', async () => {
    mockBootstrapChain();

    await Promise.all([sessionStore.load(), sessionStore.load(), sessionStore.load(true)]);

    expect(mockListOrgs).toHaveBeenCalledTimes(1);
  });

  it('starts a genuinely new chain once the in-flight one has settled', async () => {
    mockBootstrapChain();

    await sessionStore.load();
    expect(mockListOrgs).toHaveBeenCalledTimes(1);

    // Not concurrent this time — the first call fully settled first, so the
    // in-flight guard must not still be latched.
    await sessionStore.load(true);
    expect(mockListOrgs).toHaveBeenCalledTimes(2);
  });

  it('lets a later caller retry after an in-flight chain fails', async () => {
    mockListOrgs.mockRejectedValueOnce(new Error('network blip'));

    await expect(sessionStore.load()).rejects.toThrow('network blip');
    expect(sessionStore.loaded).toBe(false);

    mockBootstrapChain();
    await sessionStore.load();

    expect(mockListOrgs).toHaveBeenCalledTimes(2);
    expect(sessionStore.loaded).toBe(true);
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

// ---------------------------------------------------------------------------
// can(): the client mirror of the backend's `grant_applies` /
// `effective_permissions` (sauron-auth/src/rbac.rs), extended to the fourth
// (env) level. These pin the cascade in both directions: a grant at env, app,
// project, or org level all satisfy an env-scoped check (down), but an env
// grant must NEVER satisfy an app/project/org-level check (up) — that
// direction is the actual security-relevant risk (a UI that shows a button
// which then 403s), not the other one.
// ---------------------------------------------------------------------------
describe('can() — environment-scoped checks', () => {
  beforeEach(() => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
  });

  it('an env grant satisfies a check for that same environment only', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };

    expect(sessionStore.can('issue:read', { env: 'env-1' })).toBe(true);
    expect(sessionStore.can('issue:read', { env: 'env-2' })).toBe(false);
  });

  it('an app grant cascades down to satisfy an env-scoped check', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['issue:read'] }],
    };

    expect(sessionStore.can('issue:read', { env: 'env-1' })).toBe(true);
  });

  it('project and org grants also cascade down to an env-scoped check', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'project', scope_id: 'proj-1', permissions: ['issue:read'] }],
    };
    expect(sessionStore.can('issue:read', { env: 'env-1' })).toBe(true);

    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'org', scope_id: 'org-1', permissions: ['issue:read'] }],
    };
    expect(sessionStore.can('issue:read', { env: 'env-1' })).toBe(true);
  });

  it('an env grant does NOT satisfy an app-level check — a narrower grant can never satisfy a wider one', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };

    // No `env` in the scope at all — an ordinary app-level check.
    expect(sessionStore.can('issue:read', { app: 'app-1' })).toBe(false);
  });

  it('omitting `env` never matches an env grant, even when currentEnvId equals it', () => {
    // `env` is deliberately not defaulted from `currentEnvId` — see can()'s
    // doc comment. Most call sites never pass `env` at all, and if it
    // defaulted from the current selection, any env-scoped grant naming the
    // selected environment would leak into every one of those unrelated
    // checks.
    sessionStore.currentEnvId = 'env-1';
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['app:update'] }],
    };

    expect(sessionStore.can('app:update', { app: 'app-1' })).toBe(false);
  });

  it('`null` (all environments) and "none" (unattributed) never match an env grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };

    expect(sessionStore.can('issue:read', { env: null })).toBe(false);
    expect(sessionStore.can('issue:read', { env: 'none' })).toBe(false);
  });

  it('an env grant on a different environment does not satisfy the check', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-2', permissions: ['issue:read'] }],
    };

    expect(sessionStore.can('issue:read', { env: 'env-1' })).toBe(false);
  });

  it('a permission absent from the matched env grant still denies', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };

    expect(sessionStore.can('issue:write', { env: 'env-1' })).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// can(`level`): the UP direction of the cascade, which the block above pins
// only for env grants. `level` names how far down the tree the check is
// allowed to look, mirroring which ids the matching backend helper passes to
// `has_permission`: `authorize_org` resolves at `(org, None, None, None)`, so
// a project- or app-scoped grant carrying `member:manage` can never satisfy
// it. Before `level` existed, `can()` ORed all three and lit UI the server
// answers with 403 — the exact wrong-direction permissiveness the store's own
// doc comment says it must never have.
// ---------------------------------------------------------------------------
describe('can() — scope level truncation', () => {
  beforeEach(() => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
    sessionStore.currentEnvId = null;
  });

  it('level org ignores a project grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'project', scope_id: 'proj-1', permissions: ['member:manage'] }],
    };

    // Default level is 'app', which is today's (looser) behaviour — pinned so
    // the ~40 pre-existing call sites are provably unaffected by this change.
    expect(sessionStore.can('member:manage')).toBe(true);
    expect(sessionStore.can('member:manage', { level: 'org' })).toBe(false);
  });

  it('level org ignores an app grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['org:manage'] }],
    };

    expect(sessionStore.can('org:manage', { level: 'org' })).toBe(false);
  });

  it('level project ignores an app grant but honours a project grant', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['monitor:read'] }],
    };
    expect(sessionStore.can('monitor:read', { level: 'project' })).toBe(false);

    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'project', scope_id: 'proj-1', permissions: ['monitor:read'] }],
    };
    expect(sessionStore.can('monitor:read', { level: 'project' })).toBe(true);
  });

  it('an org grant satisfies every level', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'org', scope_id: 'org-1', permissions: ['issue:read'] }],
    };

    for (const level of ['org', 'project', 'app'] as const) {
      expect(sessionStore.can('issue:read', { level })).toBe(true);
    }
    expect(sessionStore.can('issue:read', { level: 'env', env: 'env-1' })).toBe(true);
  });

  it('an env grant satisfies no level above env', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    };

    for (const level of ['org', 'project', 'app'] as const) {
      expect(sessionStore.can('issue:read', { level, env: 'env-1' })).toBe(false);
    }
    expect(sessionStore.can('issue:read', { level: 'env', env: 'env-1' })).toBe(true);
  });

  it('null access denies at every level', () => {
    sessionStore.access = null;

    for (const level of ['org', 'project', 'app', 'env'] as const) {
      expect(sessionStore.can('issue:read', { level, env: 'env-1' })).toBe(false);
    }
  });
});

// ---------------------------------------------------------------------------
// accessError: "the permission fetch failed" must stay distinguishable from
// "this member genuinely holds no grants". Both leave `access` null and `can()`
// returning false for everything, but only one is a retryable error — and now
// that nav visibility and every button's enabled state derive from `can()`,
// collapsing the two renders a network blip as a wholly convincing "you have
// no permissions" UI. Same invariant `environmentsError` exists for.
// ---------------------------------------------------------------------------
describe('accessError', () => {
  function mockOrgOnly() {
    mockListOrgs.mockResolvedValue([
      {
        id: 'org-1',
        name: 'Org 1',
        slug: 'org-1',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
        project_count: 0,
        can_create_project: true,
      },
    ]);
    mockListProjects.mockResolvedValue([]);
  }

  it('is set when getAccess fails, and access stays null', async () => {
    mockOrgOnly();
    mockGetAccess.mockRejectedValueOnce(new Error('network'));

    await sessionStore.load(true);

    expect(sessionStore.accessError).toBe(true);
    expect(sessionStore.access).toBe(null);
    // The rest of the bootstrap must still complete — a failed access fetch
    // is not a failed load.
    expect(sessionStore.loaded).toBe(true);
  });

  it('is cleared by a later successful fetch', async () => {
    mockOrgOnly();
    mockGetAccess.mockRejectedValueOnce(new Error('network'));
    await sessionStore.load(true);
    expect(sessionStore.accessError).toBe(true);

    mockGetAccess.mockResolvedValueOnce({ permissions: [], grants: [] });
    await sessionStore.load(true);

    expect(sessionStore.accessError).toBe(false);
  });

  it('stays false for a member who legitimately holds no grants', async () => {
    mockOrgOnly();
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });

    await sessionStore.load(true);

    expect(sessionStore.accessError).toBe(false);
    expect(sessionStore.can('issue:read')).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// `canAtAnyEnv` — the "all environments" arm of the page gate.
//
// `resolve_env_filter` (rbac.rs) answers `EnvFilter::All` for an env-scoped
// caller with `Ok(Subset(readable))`, NOT with a denial: the server narrows the
// read to the environments they hold rather than refusing it. So the page gate
// for an env-aware page must admit a member who holds the permission on any
// environment while the picker sits on "all". `can()` cannot answer that — it
// asks about ONE named environment — which is why this is separate.
// ---------------------------------------------------------------------------
describe('canAtAnyEnv()', () => {
  beforeEach(() => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
  });

  it('is true when an env grant carries the permission', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['event:read'] }],
    };
    expect(sessionStore.canAtAnyEnv('event:read')).toBe(true);
  });

  it('is false when the env grant carries a different permission', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['event:read'] }],
    };
    expect(sessionStore.canAtAnyEnv('issue:write')).toBe(false);
  });

  // Deliberately NOT true. This function exists only to answer the env arm of
  // the gate; the app/project/org arms are `can()`'s job and are checked first
  // by `canAccessPage`. Folding them in here would make the two paths overlap
  // and hide which one actually admitted the member.
  it('ignores org, project and app grants', () => {
    sessionStore.access = {
      permissions: [],
      grants: [
        { scope_type: 'org', scope_id: 'org-1', permissions: ['event:read'] },
        { scope_type: 'project', scope_id: 'proj-1', permissions: ['event:read'] },
        { scope_type: 'app', scope_id: 'app-1', permissions: ['event:read'] },
      ],
    };
    expect(sessionStore.canAtAnyEnv('event:read')).toBe(false);
  });

  it('is false while access has not loaded', () => {
    sessionStore.access = null;
    expect(sessionStore.canAtAnyEnv('event:read')).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Reachable projects across orgs.
//
// The bug these cover: `AppShell` decided whether to show onboarding from the
// CURRENT org's project list. A member holding a grant in one org while sitting
// on another empty one was redirected to `/onboarding` — a page that renders no
// Topbar, so it has no org switcher, and whose only exit is signing out, which
// restores the same stored org and lands straight back there.
// ---------------------------------------------------------------------------
describe('reachableProjectCount', () => {
  function org(id: string, project_count: number, can_create_project = true) {
    return {
      id,
      name: id,
      slug: id,
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
      project_count,
      can_create_project,
    };
  }

  it('sums across every org, not just the current one', () => {
    sessionStore.orgs = [org('empty', 0), org('full', 3)];
    expect(sessionStore.reachableProjectCount).toBe(3);
  });

  it('is zero only when no org has a reachable project', () => {
    sessionStore.orgs = [org('a', 0), org('b', 0)];
    expect(sessionStore.reachableProjectCount).toBe(0);
  });

  it('prefers an org that HAS projects over the creation-ordered first one', async () => {
    // `list_orgs_for_user` orders by created_at, so the empty org legitimately
    // comes first. Taking orgs[0] blindly is what stranded the member.
    mockListOrgs.mockResolvedValue([org('empty', 0), org('full', 1)]);
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([
      {
        id: 'proj-1',
        org_id: 'full',
        name: 'P1',
        slug: 'p1',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      },
    ]);
    mockListApps.mockResolvedValue([]);

    await sessionStore.load();

    expect(sessionStore.currentOrgId).toBe('full');
  });

  it('still honours a stored org even when it is empty', async () => {
    // Switching orgs is an explicit choice; overriding it would be its own bug.
    // Safe now only because the empty org renders inside the shell (org switcher
    // present) rather than redirecting to onboarding.
    window.localStorage.setItem(ORG_KEY, 'empty');
    mockListOrgs.mockResolvedValue([org('empty', 0), org('full', 1)]);
    mockGetAccess.mockResolvedValue({ permissions: [], grants: [] });
    mockListProjects.mockResolvedValue([]);
    mockListApps.mockResolvedValue([]);

    await sessionStore.load();

    expect(sessionStore.currentOrgId).toBe('empty');
    // ...and the shell can still tell that projects exist elsewhere, which is
    // what keeps it out of onboarding.
    expect(sessionStore.reachableProjectCount).toBe(1);
  });
});
