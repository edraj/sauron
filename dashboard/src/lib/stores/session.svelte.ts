import { getAccess, listOrgs } from '../api/orgs';
import { listProjects } from '../api/projects';
import { listApps } from '../api/apps';
import { listEnvironments } from '../api/environments';
import { configureScopeBridge } from '../api/scope';
import type {
  AccessResponse,
  App,
  Environment,
  Organization,
  Permission,
  Project,
} from '../models';

const ORG_KEY = 'sauron.org_id';
const PROJECT_KEY = 'sauron.project_id';
const APP_KEY = 'sauron.app_id';
const ENV_KEY = 'sauron.environment_id';

function readStored(key: string): string | null {
  if (typeof window === 'undefined') return null;
  return window.localStorage.getItem(key);
}

function writeStored(key: string, id: string | null): void {
  if (typeof window === 'undefined') return;
  if (id) window.localStorage.setItem(key, id);
  else window.localStorage.removeItem(key);
}

export interface CanScope {
  org?: string | null;
  project?: string | null;
  app?: string | null;
}

/**
 * Holds the current org → project → app → environment selection plus the
 * lists needed to switch between them, and the access grants for the current
 * org. Selections persist to localStorage so reloads land you back where you
 * were.
 */
class SessionStore {
  orgs = $state<Organization[]>([]);
  projects = $state<Project[]>([]);
  apps = $state<App[]>([]);
  environments = $state<Environment[]>([]);
  // True iff the most recent `listEnvironments` fetch for `currentAppId`
  // failed. Distinct from "loaded and empty" (a real, legitimate state where
  // this stays `false` and `environments` is `[]`) — see
  // `loadAppEnvironments`'s doc comment for why the two must never collapse
  // into the same representation. Cleared at the start of every fetch
  // attempt, so a stale `true` from a previous app never leaks into the next.
  environmentsError = $state(false);

  currentOrgId = $state<string | null>(null);
  currentProjectId = $state<string | null>(null);
  currentAppId = $state<string | null>(null);
  // `null` means "all environments"; the literal string `'none'` means
  // "unattributed" — both map straight onto the backend's `?environment_id=`
  // wire contract, so there is no translation layer anywhere above this.
  currentEnvId = $state<string | null>(null);

  // Access grants for the current org — drives every permission check.
  access = $state<AccessResponse | null>(null);

  loaded = $state(false);
  loading = $state(false);

  constructor() {
    // Wire this store into the axios client's scope bridge (mirrors
    // `configureAuthBridge` in auth.svelte.ts) — `client.ts` must not import
    // this module directly, so it reads the current environment id through
    // this callback instead, registered once here.
    configureScopeBridge({
      getCurrentEnvironmentId: () => this.currentEnvId,
    });
  }

  get currentOrg(): Organization | null {
    return this.orgs.find((o) => o.id === this.currentOrgId) ?? null;
  }

  get currentProject(): Project | null {
    return this.projects.find((p) => p.id === this.currentProjectId) ?? null;
  }

  get currentApp(): App | null {
    return this.apps.find((a) => a.id === this.currentAppId) ?? null;
  }

  get currentEnvironment(): Environment | null {
    return this.environments.find((e) => e.id === this.currentEnvId) ?? null;
  }

  /// Changes whenever the data on screen should be refetched. Telemetry pages key
  /// their effects on this rather than on `currentAppId` alone: an effect that
  /// tracks only the app will not re-run when the environment changes, leaving
  /// the previous environment's data on screen. That exact bug shipped once in
  /// Docs.svelte and was caught in review; here there would be 24 chances for it.
  get scopeKey(): string {
    return `${this.currentAppId ?? ''}:${this.currentEnvId ?? 'all'}`;
  }

  // -------------------------------------------------------------------------
  // Permission check
  //
  // True iff any grant for the current org matches one of the supplied scopes
  // (falling back to the current selection) and contains `perm`. An org-scoped
  // grant cascades to every project/app beneath it.
  // -------------------------------------------------------------------------
  can(perm: Permission, scope: CanScope = {}): boolean {
    if (!this.access) return false;
    const org = scope.org ?? this.currentOrgId ?? undefined;
    const project = scope.project ?? this.currentProjectId ?? undefined;
    const app = scope.app ?? this.currentAppId ?? undefined;
    return this.access.grants.some((g) => {
      const scopeMatch =
        (g.scope_type === 'org' && g.scope_id === org) ||
        (g.scope_type === 'project' && g.scope_id === project) ||
        (g.scope_type === 'app' && g.scope_id === app);
      return scopeMatch && g.permissions.includes(perm);
    });
  }

  // -------------------------------------------------------------------------
  // Loading
  // -------------------------------------------------------------------------

  /** Load orgs + the current org's access/projects/apps. Caches after first call. */
  async load(force = false): Promise<void> {
    if (this.loaded && !force) return;
    this.loading = true;
    try {
      const orgs = await listOrgs();
      this.orgs = orgs;
      if (orgs.length === 0) {
        this.projects = [];
        this.apps = [];
        this.environments = [];
        this.access = null;
        this.currentOrgId = null;
        this.currentProjectId = null;
        this.currentAppId = null;
        this.currentEnvId = null;
        this.loaded = true;
        return;
      }
      const stored = readStored(ORG_KEY);
      this.currentOrgId = stored && orgs.some((o) => o.id === stored) ? stored : orgs[0].id;
      writeStored(ORG_KEY, this.currentOrgId);
      await this.loadOrgScope(this.currentOrgId);
      this.loaded = true;
    } finally {
      this.loading = false;
    }
  }

  /** Load access + projects for an org, then resolve the current project + apps. */
  private async loadOrgScope(orgId: string): Promise<void> {
    const [access, projects] = await Promise.all([
      getAccess(orgId).catch(() => null),
      listProjects(orgId).catch(() => [] as Project[]),
    ]);
    this.access = access;
    this.projects = projects;
    this.resolveCurrentProject();
    if (this.currentProjectId) {
      await this.loadProjectApps(this.currentProjectId);
    } else {
      this.apps = [];
      this.currentAppId = null;
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
    }
  }

  private resolveCurrentProject(): void {
    const stored = readStored(PROJECT_KEY);
    if (stored && this.projects.some((p) => p.id === stored)) {
      this.currentProjectId = stored;
    } else if (this.projects.length > 0) {
      this.currentProjectId = this.projects[0].id;
      writeStored(PROJECT_KEY, this.currentProjectId);
    } else {
      this.currentProjectId = null;
      writeStored(PROJECT_KEY, null);
    }
  }

  private async loadProjectApps(projectId: string): Promise<void> {
    this.apps = await listApps(projectId).catch(() => [] as App[]);
    this.resolveCurrentApp();
    if (this.currentAppId) {
      await this.loadAppEnvironments(this.currentAppId);
    } else {
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
    }
  }

  private resolveCurrentApp(): void {
    const stored = readStored(APP_KEY);
    if (stored && this.apps.some((a) => a.id === stored)) {
      this.currentAppId = stored;
    } else if (this.apps.length > 0) {
      this.currentAppId = this.apps[0].id;
      writeStored(APP_KEY, this.currentAppId);
    } else {
      this.currentAppId = null;
      writeStored(APP_KEY, null);
    }
  }

  /**
   * Active environments only — a retired one must never be selectable.
   *
   * Records `environmentsLoadAttemptedFor` synchronously, before the network
   * round-trip, so any concurrent reader of `environments` (namely the
   * Topbar's self-heal effect below) can tell a load for this app is already
   * under way rather than starting a second one alongside it. `setApp` /
   * `loadProjectApps` / `load()` all clear `environments` to `[]` and then
   * call this method without any intervening `await`, so the flag is in
   * place before the effect's next flush ever sees the emptied array.
   *
   * On failure this must NOT behave like `routes/scope.rs`'s own opposite: that
   * module's doc comment states its rule as "a malformed value must be a 400,
   * not a silent fallback to `All` — falling back would show the caller MORE
   * data than they asked for, which is the wrong direction to fail on a
   * scoping parameter." A failed *list* fetch says nothing about whether the
   * previously-selected environment still exists — only that the list
   * couldn't be fetched right now — so widening `currentEnvId` to `null`
   * ("all environments") here would be exactly that wrong-direction fallback,
   * and worse: `resolveCurrentEnvironment` would then persist the `null` to
   * `localStorage`, destroying the selection permanently rather than just for
   * this one failed load. So on failure: leave `environments` and
   * `currentEnvId` exactly as they were (do not call
   * `resolveCurrentEnvironment` at all — there is nothing new to reconcile
   * against), set `environmentsError` so the UI can react, and clear
   * `environmentsLoadAttemptedFor` so the failure is retryable.
   *
   * That last part is what keeps this from colliding with
   * `ensureEnvironmentsLoaded`'s guard: a genuinely-empty successful load
   * (an app with zero environments) sets `environmentsLoadAttemptedFor` and
   * leaves it set, so the guard correctly refuses to refetch forever. A
   * failed load must not be indistinguishable from that — clearing the
   * marker here is what tells the guard "this app's load never actually
   * completed, a retry is still warranted."
   */
  private async loadAppEnvironments(appId: string): Promise<void> {
    this.environmentsLoadAttemptedFor = appId;
    this.environmentsError = false;
    let fetched: Environment[];
    try {
      fetched = await listEnvironments(appId);
    } catch {
      this.environmentsError = true;
      this.environmentsLoadAttemptedFor = null;
      return;
    }
    this.environments = fetched;
    this.resolveCurrentEnvironment();
  }

  // Tracks which app id a load has *completed* for (see
  // `loadAppEnvironments`), so a genuinely-empty result (an app with zero
  // environments) doesn't retrigger a fetch on every reactive read of
  // `environments`, and so a load already in flight isn't duplicated. Set
  // synchronously before the fetch starts (so an in-flight load is visible
  // immediately), but rolled back to `null` if that fetch fails —
  // `loadAppEnvironments` is what tells the two states apart; this field on
  // its own cannot distinguish "succeeded with zero rows" (stays set, must
  // not retry) from "failed" (cleared, must be retryable) without that help.
  private environmentsLoadAttemptedFor: string | null = null;

  /**
   * `removeApp` clears `environments`/`currentEnvId` synchronously when the
   * removed app was current, but does not reload the replacement app's
   * environments (that would require `removeApp` to become async). And
   * `setApp` has a same-id no-op guard, so `setApp(currentAppId)` cannot be
   * used to force a reload either. Callers — namely the Topbar switcher —
   * that observe `currentAppId` set but `environments` empty should call
   * this instead of assuming the two are always in step.
   *
   * Also the retry path after a failed load: `loadAppEnvironments` clears
   * `environmentsLoadAttemptedFor` on failure specifically so this method's
   * guard lets a subsequent call through instead of refusing forever.
   */
  async ensureEnvironmentsLoaded(): Promise<void> {
    const appId = this.currentAppId;
    if (!appId) return;
    if (this.environments.length > 0) return;
    if (this.environmentsLoadAttemptedFor === appId) return;
    await this.loadAppEnvironments(appId);
  }

  private resolveCurrentEnvironment(): void {
    const stored = readStored(ENV_KEY);
    // `'none'` (Unattributed) is always a valid selection — it does not name
    // a row in `this.environments` the way a real environment id does.
    if (stored && (stored === 'none' || this.environments.some((e) => e.id === stored))) {
      this.currentEnvId = stored;
      return;
    }
    // No stored selection (or one that no longer applies): land on the app's
    // default environment, never on `environments[0]` — index order carries
    // no meaning and Slice 1 guarantees exactly one live default per app.
    const def = this.environments.find((e) => e.is_default);
    this.currentEnvId = def ? def.id : null;
    writeStored(ENV_KEY, this.currentEnvId);
  }

  // -------------------------------------------------------------------------
  // Switching
  // -------------------------------------------------------------------------

  async setOrg(id: string): Promise<void> {
    if (id === this.currentOrgId) return;
    this.currentOrgId = id;
    writeStored(ORG_KEY, id);
    // Downstream selections belong to the previous org — clear them so the new
    // org resolves to its own first project/app/environment.
    writeStored(PROJECT_KEY, null);
    writeStored(APP_KEY, null);
    writeStored(ENV_KEY, null);
    this.currentProjectId = null;
    this.currentAppId = null;
    this.currentEnvId = null;
    this.projects = [];
    this.apps = [];
    this.environments = [];
    await this.loadOrgScope(id);
  }

  async setProject(id: string): Promise<void> {
    if (id === this.currentProjectId) return;
    this.currentProjectId = id;
    writeStored(PROJECT_KEY, id);
    writeStored(APP_KEY, null);
    writeStored(ENV_KEY, null);
    this.currentAppId = null;
    this.currentEnvId = null;
    this.apps = [];
    this.environments = [];
    await this.loadProjectApps(id);
  }

  async setApp(id: string): Promise<void> {
    if (id === this.currentAppId) return;
    this.currentAppId = id;
    writeStored(APP_KEY, id);
    // The environment belongs to the previous app — carrying it over would
    // send another app's environment id to the API.
    writeStored(ENV_KEY, null);
    this.currentEnvId = null;
    this.environments = [];
    await this.loadAppEnvironments(id);
  }

  setEnvironment(id: string | null): void {
    this.currentEnvId = id;
    writeStored(ENV_KEY, id);
  }

  /** Select a project + app together (used when jumping from lists). */
  async selectApp(projectId: string, appId: string): Promise<void> {
    if (projectId !== this.currentProjectId) {
      await this.setProject(projectId);
    }
    await this.setApp(appId);
  }

  // -------------------------------------------------------------------------
  // Local list mutation (create/update flows)
  // -------------------------------------------------------------------------

  upsertProject(project: Project, select = true): void {
    const idx = this.projects.findIndex((p) => p.id === project.id);
    if (idx >= 0) this.projects[idx] = project;
    else this.projects = [...this.projects, project];
    if (!this.currentOrgId) this.currentOrgId = project.org_id;
    if (select) {
      this.currentProjectId = project.id;
      writeStored(PROJECT_KEY, project.id);
    }
  }

  removeProject(projectId: string): void {
    this.projects = this.projects.filter((p) => p.id !== projectId);
    if (this.currentProjectId === projectId) {
      this.resolveCurrentProject();
      this.apps = [];
      this.currentAppId = null;
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
    }
  }

  upsertApp(app: App, select = true): void {
    // Only track in the local list when it belongs to the current project.
    if (app.project_id === this.currentProjectId) {
      const idx = this.apps.findIndex((a) => a.id === app.id);
      if (idx >= 0) this.apps[idx] = app;
      else this.apps = [...this.apps, app];
    }
    // Fire-and-forget: callers here are synchronous create/update flows that
    // don't depend on the newly-selected app's environments being loaded yet.
    if (select) void this.setApp(app.id);
  }

  removeApp(appId: string): void {
    this.apps = this.apps.filter((a) => a.id !== appId);
    if (this.currentAppId === appId) {
      this.resolveCurrentApp();
      // The removed app's environments no longer apply. Whichever app
      // `resolveCurrentApp` landed on (or none) gets its own environments
      // loaded the next time it becomes current via `setApp`/`loadProjectApps`.
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
      writeStored(ENV_KEY, null);
    }
  }

  reset(): void {
    this.orgs = [];
    this.projects = [];
    this.apps = [];
    this.environments = [];
    this.environmentsError = false;
    this.access = null;
    this.currentOrgId = null;
    this.currentProjectId = null;
    this.currentAppId = null;
    this.currentEnvId = null;
    this.loaded = false;
    writeStored(ORG_KEY, null);
    writeStored(PROJECT_KEY, null);
    writeStored(APP_KEY, null);
    writeStored(ENV_KEY, null);
  }
}

export const sessionStore = new SessionStore();
