import { getAccess, listOrgs } from '../api/orgs';
import { listProjects } from '../api/projects';
import { listApps } from '../api/apps';
import { listEnvironments } from '../api/environments';
import { configureScopeBridge } from '../api/scope';
import type {
  AccessResponse,
  App,
  AppEnvironment,
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

/**
 * How far down the scope cascade a check may look, mirroring which ids the
 * matching backend helper passes to `has_permission` (sauron-auth/src/rbac.rs).
 *
 * - `'org'`     — org grants only. `authorize_org` resolves at
 *                 `(org, None, None, None)`, so no narrower grant can satisfy it.
 * - `'project'` — org + project grants (`authorize_project`).
 * - `'app'`     — org + project + app grants (`authorize_app`).
 * - `'env'`     — all four (`authorize_env_read`); needs an explicit `env`.
 *
 * Defaults to `'env'` when the caller passes an explicit `env`, otherwise to
 * `'app'` — so every call site written before this existed keeps its exact
 * previous behaviour, and an explicit `level` always wins over that default.
 */
export type CanLevel = 'org' | 'project' | 'app' | 'env';

export interface CanScope {
  org?: string | null;
  project?: string | null;
  app?: string | null;
  // Deliberately not defaulted from `currentEnvId` the way org/project/app are
  // — see `can()`'s doc comment. Omit it entirely unless the check really is
  // an environment-scoped one.
  env?: string | null;
  level?: CanLevel;
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
  environments = $state<AppEnvironment[]>([]);
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
  // True iff the most recent `getAccess` for the current org failed. Distinct
  // from "loaded and genuinely holds no grants" (a real state where this stays
  // `false` and `access` is an empty grant list) — exactly the distinction
  // `environmentsError` exists for, and for a sharper reason: page visibility
  // and every button's enabled state now derive from `can()`, which answers
  // `false` for everything while `access` is null. Collapsing the two would
  // render a transient network failure as a fully convincing "you have no
  // permissions" dashboard. Cleared at the start of every attempt, so a stale
  // `true` from a previous org never leaks into the next.
  accessError = $state(false);

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

  get currentEnvironment(): AppEnvironment | null {
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
  // (falling back to the current selection for org/project/app) and contains
  // `perm`. This is the client mirror of the backend's `grant_applies` /
  // `effective_permissions` (sauron-auth/src/rbac.rs) — a UI convenience that
  // must never be MORE permissive than the server. Cascade: an org grant
  // satisfies everything below it; a project grant satisfies its apps and
  // their environments (not sibling projects); an app grant satisfies that
  // app and every environment under it; an env grant satisfies only that one
  // environment. A grant narrower than the check being made can never satisfy
  // it — an env grant does NOT satisfy an app/project/org-level check.
  //
  // `env` is the one exception to the "falls back to the current selection"
  // rule. org/project/app default from `currentOrgId`/`currentProjectId`/
  // `currentAppId` because nearly every call site IS asking about the
  // currently-selected org/project/app. That is not true of `env`: the large
  // majority of `can()` calls (app:update, project:create, member:manage, …)
  // are not environment-scoped questions at all, and the backend's own
  // `authorize_org`/`authorize_project`/`authorize_app` always resolve with
  // `env: None` — an env-scoped grant can NEVER satisfy them, no matter which
  // environment happens to be selected. If `env` defaulted from
  // `currentEnvId` here, a narrow env-scoped grant would silently leak into
  // every one of those unrelated checks just because it happened to name the
  // currently-selected environment — exactly the wrong-direction
  // permissiveness this function must never have. A caller that wants an
  // environment-scoped check must ask for one explicitly:
  // `can('issue:read', { env: sessionStore.currentEnvId })`.
  //
  // `null` ("all environments") and the literal string `'none'`
  // ("unattributed") are both not a real environment id, so neither can ever
  // match an env-scoped grant — passing either behaves exactly like omitting
  // `env` (the check falls back to whatever the org/project/app grants alone
  // allow). This mirrors the backend's `effective_permissions_for_filter`,
  // whose `All`/`Unattributed` arms are evaluated at `env: None` for the same
  // reason: a permission held on one environment must not unlock behavior
  // across "all" or "unattributed".
  // -------------------------------------------------------------------------
  can(perm: Permission, scope: CanScope = {}): boolean {
    if (!this.access) return false;
    // An explicit `env` argument IS the caller opting into an env-scoped
    // question, so it defaults the level — that is what every pre-existing
    // `can(p, { env })` call site already meant. An explicit `level` overrides
    // it, which is what lets `{ level: 'org', env }` correctly refuse to match
    // an env grant.
    const level: CanLevel = scope.level ?? (scope.env !== undefined ? 'env' : 'app');
    const org = scope.org ?? this.currentOrgId ?? undefined;
    // A level above a given scope type zeroes that id out, exactly as the
    // backend passes `None` for every scope below the one it authorizes at.
    // Leaving the id populated is what made `can()` more permissive than the
    // server: a project-scoped `member:manage` grant lit a button that
    // `authorize_org` then answered with 403.
    const project =
      level === 'org' ? undefined : (scope.project ?? this.currentProjectId ?? undefined);
    const app =
      level === 'org' || level === 'project'
        ? undefined
        : (scope.app ?? this.currentAppId ?? undefined);
    const env = level === 'env' && scope.env && scope.env !== 'none' ? scope.env : undefined;
    return this.access.grants.some((g) => {
      const scopeMatch =
        (g.scope_type === 'org' && g.scope_id === org) ||
        (g.scope_type === 'project' && g.scope_id === project) ||
        (g.scope_type === 'app' && g.scope_id === app) ||
        (g.scope_type === 'env' && env !== undefined && g.scope_id === env);
      return scopeMatch && g.permissions.includes(perm);
    });
  }

  // -------------------------------------------------------------------------
  // Loading
  // -------------------------------------------------------------------------

  // The promise of a `load()` call currently in flight, or `null` if none is.
  // `App.svelte`'s post-auth redirect (`push('/issues')`, which mounts a
  // layout whose `onMount` calls `load()`) and `Login.svelte`'s own forced
  // `load(true)` right after a successful sign-in can both fire within the
  // same render pass — without this, both would start their own full
  // bootstrap chain (`listOrgs` → `loadOrgScope` → `loadProjectApps` →
  // `loadAppEnvironments`) concurrently, doubling every request in it. Same
  // precedent as `loadAppEnvironments`'s `environmentsLoadAttemptedFor`
  // marker (see its own doc comment): stamped synchronously, in `load()`
  // itself, before the first `await` — assigning it any later would leave a
  // window where a second call still sees `loadPromise` as `null` and starts
  // its own chain anyway.
  private loadPromise: Promise<void> | null = null;

  /** Load orgs + the current org's access/projects/apps. Caches after first call. */
  async load(force = false): Promise<void> {
    if (this.loaded && !force) return;
    if (this.loadPromise) return this.loadPromise;
    this.loadPromise = this.performLoad();
    try {
      await this.loadPromise;
    } finally {
      this.loadPromise = null;
    }
  }

  private async performLoad(): Promise<void> {
    this.loading = true;
    try {
      const orgs = await listOrgs();
      this.orgs = orgs;
      if (orgs.length === 0) {
        this.projects = [];
        this.apps = [];
        this.environments = [];
        this.access = null;
        this.accessError = false;
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
    this.accessError = false;
    const [access, projects] = await Promise.all([
      getAccess(orgId).then(
        (a) => a,
        () => {
          this.accessError = true;
          return null;
        },
      ),
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
   * No reach filtering happens here, and none should be added: `listEnvironments`
   * (`GET /v1/apps/{id}/environments`) is already reach-filtered server-side
   * (`routes/environments.rs::list_environments`, using `reach_for`/`perm::ENV_READ`)
   * — a partial-reach caller gets back only the environments they hold a grant
   * on, a full-reach caller gets the app's complete list. A client-side filter
   * on top of that would be redundant at best and, the moment its rule drifted
   * from the backend's, either hide environments the caller can see or (worse)
   * show ones they can't.
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
    let fetched: AppEnvironment[];
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
    this.accessError = false;
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
