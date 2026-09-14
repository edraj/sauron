import { getAccess, listOrgs } from '../api/orgs';
import { listProjects } from '../api/projects';
import { listApps } from '../api/apps';
import { listEnvironments } from '../api/environments';
import { listReleases } from '../api/releases';
import { configureScopeBridge } from '../api/scope';
import { selectableReleases } from '../models/release-switcher';
import type {
  AccessResponse,
  App,
  AppEnvironment,
  AppRelease,
  Organization,
  Permission,
  Project,
} from '../models';

const ORG_KEY = 'sauron.org_id';
const PROJECT_KEY = 'sauron.project_id';
const APP_KEY = 'sauron.app_id';
const ENV_KEY = 'sauron.environment_id';
const RELEASE_KEY_PREFIX = 'sauron.release:';
const releaseKey = (appId: string) => RELEASE_KEY_PREFIX + appId;

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

  releases = $state<AppRelease[]>([]);
  releasesError = $state(false);
  // `null` = all releases; the literal `'none'` = rows with no release.
  // Persisted PER APP (`sauron.release:{appId}`), unlike the environment,
  // because a release name is meaningless across apps.
  currentRelease = $state<string | null>(null);

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
      getCurrentRelease: () => this.currentRelease,
    });
  }

  get currentOrg(): Organization | null {
    return this.orgs.find((o) => o.id === this.currentOrgId) ?? null;
  }

  /**
   * Projects this user can reach across EVERY org, not just the current one.
   *
   * The shell asks this before offering onboarding. Asking "does the current
   * org have projects" instead is what stranded a member who holds a grant in
   * one org and lands on another: the empty org answered "no projects", and
   * onboarding is a page with no org switcher on it, so there was no way back.
   */
  get reachableProjectCount(): number {
    return this.orgs.reduce((n, o) => n + (o.project_count ?? 0), 0);
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
  ///
  /// Two segments — app and environment — because those are the dimensions
  /// EVERY telemetry read is narrowed by. The release is deliberately NOT here:
  /// only five list routes accept `?release=` (`api/scope.ts`'s
  /// `RELEASE_SCOPED_URL`), so folding it in made all 24 pages re-fetch on every
  /// release switch, and ~20 of them re-rendered a byte-identical rollup.
  /// Release-aware pages use `scopeKeyWithRelease` below instead.
  get scopeKey(): string {
    return `${this.currentAppId ?? ''}:${this.currentEnvId ?? 'all'}`;
  }

  /// `scopeKey` plus the selected release, for the pages whose LIST requests the
  /// axios interceptor actually attaches `release=` to — Issues (and its
  /// occurrences table), Events, Sessions and Transactions. Everything else,
  /// including the aggregate side widgets ON those same pages, stays on
  /// `scopeKey`; `models/release-scope-key-parity.test.ts` enforces both
  /// directions off `RELEASE_AWARE`.
  ///
  /// The name contains `scopeKey` on purpose: `api/scope.test.ts`'s
  /// "telemetry pages observe scopeKey" guard is a source scan for that
  /// substring, and a page that swapped to a differently-named getter would
  /// otherwise drop out of it silently.
  get scopeKeyWithRelease(): string {
    return `${this.scopeKey}:${this.currentRelease ?? 'all'}`;
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

  /**
   * Whether ANY environment-scoped grant carries `perm`.
   *
   * The "all environments" arm of `canAccessPage`, and deliberately not part of
   * `can()`: `can()` asks about one NAMED environment, which is the right
   * question when the picker is on a specific one and the wrong question when
   * it is on "all". `resolve_env_filter` (rbac.rs) answers `EnvFilter::All` for
   * an environment-scoped caller with `Ok(Subset(readable))` rather than a
   * denial — the server narrows the read to the environments they hold instead
   * of refusing it — so the gate has to admit them without knowing which one
   * they will end up reading.
   *
   * Org/project/app grants are ignored on purpose. `canAccessPage` asks `can()`
   * first, so folding them in here would make the two arms overlap and obscure
   * which one admitted the member. It also keeps this function's name honest.
   */
  canAtAnyEnv(perm: Permission): boolean {
    if (!this.access) return false;
    return this.access.grants.some(
      (g) => g.scope_type === 'env' && g.permissions.includes(perm),
    );
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
        this.beginAppScopeChange();
        this.projects = [];
        this.apps = [];
        this.environments = [];
        this.releases = [];
        this.access = null;
        this.accessError = false;
        this.currentOrgId = null;
        this.currentProjectId = null;
        this.currentAppId = null;
        this.currentEnvId = null;
        this.currentRelease = null;
        this.loaded = true;
        return;
      }
      const stored = readStored(ORG_KEY);
      // A stored org still wins — switching orgs is an explicit choice and
      // landing somewhere else would undo it. Without one, prefer the first org
      // that actually HAS a reachable project: `orgs[0]` is creation-ordered,
      // so blindly taking it drops a member onto an empty org while their
      // projects sit one org over.
      this.currentOrgId =
        stored && orgs.some((o) => o.id === stored)
          ? stored
          : (orgs.find((o) => o.project_count > 0) ?? orgs[0]).id;
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
      this.beginAppScopeChange();
      this.apps = [];
      this.currentAppId = null;
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
      this.releases = [];
      this.currentRelease = null;
      this.releasesError = false;
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
    // Unconditionally, not just on the empty branch below: this re-derives the
    // whole app scope, so whatever was in flight for the previous one is void
    // either way. `setProject`/`setOrg` already bumped before calling, but
    // `load(force: true)` reaches here without one — and it can re-resolve to
    // the SAME app, which is exactly the case an app-id comparison cannot
    // catch (see `loadGen`).
    this.beginAppScopeChange();
    this.apps = await listApps(projectId).catch(() => [] as App[]);
    this.resolveCurrentApp();
    if (this.currentAppId) {
      // Start both loads before awaiting either, so both
      // `environmentsLoadAttemptedFor` / `releasesLoadAttemptedFor` stamps
      // land synchronously in the same tick — see `setApp`'s identical
      // `Promise.all` for why a sequential `await`/`await` here would let
      // the Topbar effect's `ensureReleasesLoaded()` guard see a released
      // load as not-yet-attempted and fire a duplicate `listReleases`.
      await Promise.all([
        this.loadAppEnvironments(this.currentAppId),
        this.loadAppReleases(this.currentAppId),
      ]);
    } else {
      // No second `beginAppScopeChange()` — the one at the top of this method
      // already covers this branch.
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
      this.releases = [];
      this.currentRelease = null;
      this.releasesError = false;
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
   * this one failed load. So on failure: do not touch `environments` or
   * `currentEnvId` (do not call `resolveCurrentEnvironment` at all — there is
   * nothing new to reconcile against), set `environmentsError` so the UI can
   * react, and clear `environmentsLoadAttemptedFor` so the failure is
   * retryable.
   *
   * Note what "do not touch" does and does not buy, per caller. On the
   * `setApp` path it buys nothing observable: `setApp` has ALREADY cleared
   * `currentEnvId` to `null` and `environments` to `[]` synchronously before
   * calling here (the previous app's environment must not be sent for the new
   * app), so a failed load lands the user on "all environments" regardless —
   * what this code avoids is only the extra harm of `resolveCurrentEnvironment`
   * writing that `null` through to `localStorage`. The claim that a selection
   * SURVIVES a failed load holds on the self-heal path
   * (`ensureEnvironmentsLoaded`, the Topbar's retry effect), where the app did
   * not change and `currentEnvId` is whatever the user picked: there a failure
   * leaves the picker empty but the scoping intact, and the next successful
   * retry re-populates it.
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
    const gen = this.loadGen;
    this.environmentsLoadAttemptedFor = appId;
    this.environmentsError = false;
    let fetched: AppEnvironment[];
    try {
      fetched = await listEnvironments(appId);
    } catch {
      if (this.isStale(gen)) return;
      this.environmentsError = true;
      this.environmentsLoadAttemptedFor = null;
      return;
    }
    if (this.isStale(gen)) return;
    this.environments = fetched;
    this.resolveCurrentEnvironment();
  }

  /**
   * Monotonic counter identifying the current app-scope "generation".
   *
   * Bumped by `beginAppScopeChange()` at the head of EVERY path that clears
   * app-scoped state (`setApp`, `setOrg`, `setProject`, `removeApp`,
   * `removeProject`, `reset`, and `performLoad`'s no-orgs branch). Each loader
   * captures it synchronously before its first `await` and refuses to write
   * anything if it has moved on by the time the fetch resolves.
   *
   * This replaces an earlier `isStale(appId)` that compared the load's `appId`
   * against `currentAppId`. That check could not see a same-app A→B→A
   * sequence: by the time the FIRST A load resolved, `currentAppId` was `'A'`
   * again, so the check passed and the oldest response overwrote the newest
   * one — exactly the bug the guard was there to prevent, and the easiest one
   * to hit by double-clicking back to where you started. Identity of the app
   * is not the question; "is this continuation still the one we are waiting
   * for" is, and only a counter answers that.
   */
  private loadGen = 0;

  /**
   * Invalidate every in-flight app-scoped load. Call synchronously, BEFORE
   * clearing state and before starting the replacement loads, so the new
   * loaders capture the new generation and the old ones are already stale.
   */
  private beginAppScopeChange(): void {
    this.loadGen += 1;
  }

  /**
   * Whether an in-flight load still speaks for the app scope on screen.
   *
   * Nothing cancels a fetch when the user switches apps, so a rapid double
   * `setApp` leaves two loads in flight and the OLDER one may land last. Every
   * continuation past an `await` in this file checks this before writing
   * anything — the result, the `…Error` flag, and the `…LoadAttemptedFor`
   * stamp alike: applied to the newer scope they would list another app's
   * environments in the picker (and then send that app's `environment_id` on
   * every scoped request), or raise an error banner over a load that
   * succeeded.
   */
  private isStale(gen: number): boolean {
    return gen !== this.loadGen;
  }

  // Tracks which app id a releases load has *completed* for, mirroring
  // `environmentsLoadAttemptedFor` above — same reasoning: a genuinely-empty
  // result (an app with no releases yet) must not retrigger a fetch on every
  // reactive read of `releases`, and a load already in flight must not be
  // duplicated. Rolled back to `null` on failure so a retry is possible.
  private releasesLoadAttemptedFor: string | null = null;

  /**
   * The releases the caller may see for `appId`, mirroring
   * `loadAppEnvironments` in every particular: stamped synchronously before
   * the fetch, `releasesError` cleared at the start of every attempt, and on
   * failure neither `releases` nor `currentRelease` is written — in
   * particular `resolveCurrentRelease` is not called, so a failed list fetch
   * never widens a selection to "all" and never persists that widening (see
   * `loadAppEnvironments`'s doc comment for why a failed list fetch must not
   * behave like a fail-open scoping fallback).
   *
   * As there, "not written" is not the same as "preserved", and which one you
   * get depends on the caller. `setApp` clears `releases` to `[]` and
   * `currentRelease` to `null` up front (release names are per-app), so a
   * failed load on that path lands on "all releases" no matter what this
   * method does — only the persisted per-app selection under `releaseKey` is
   * spared. The selection genuinely survives only on the self-heal path
   * (`ensureReleasesLoaded`), where the app did not change.
   */
  private async loadAppReleases(appId: string): Promise<void> {
    const gen = this.loadGen;
    this.releasesLoadAttemptedFor = appId;
    this.releasesError = false;
    let fetched: AppRelease[];
    try {
      fetched = await listReleases(appId);
    } catch {
      if (this.isStale(gen)) return;
      this.releasesError = true;
      this.releasesLoadAttemptedFor = null;
      return;
    }
    if (this.isStale(gen)) return;
    this.releases = fetched;
    this.resolveCurrentRelease(appId);
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

  /** The release equivalent of `ensureEnvironmentsLoaded` — same guard shape. */
  async ensureReleasesLoaded(): Promise<void> {
    const appId = this.currentAppId;
    if (!appId) return;
    if (this.releases.length > 0) return;
    if (this.releasesLoadAttemptedFor === appId) return;
    await this.loadAppReleases(appId);
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

  /**
   * The release equivalent of `resolveCurrentEnvironment`. There is no
   * "default release" concept to fall back to (unlike environments, which
   * always have exactly one live default) — an unresolvable stored value just
   * falls back to `null` ("all releases").
   *
   * Validated through `selectableReleases`, the same function the topbar menu
   * is built from, so "restorable" and "offerable" cannot drift: a stored
   * value the switcher would refuse to render (blank, or padded differently
   * from the catalogue row) must not survive a reload as an invisible active
   * filter.
   */
  private resolveCurrentRelease(appId: string): void {
    const stored = readStored(releaseKey(appId));
    if (stored && (stored === 'none' || selectableReleases(this.releases).includes(stored))) {
      this.currentRelease = stored;
      return;
    }
    this.currentRelease = null;
    // Only when there was actually something to clear. `writeStored(_, null)`
    // is a `removeItem`, and the common case by far is an app whose release
    // was never pinned — writing to `localStorage` on every app switch and
    // every bootstrap to delete a key that does not exist.
    if (stored !== null) writeStored(releaseKey(appId), null);
  }

  // -------------------------------------------------------------------------
  // Switching
  // -------------------------------------------------------------------------

  async setOrg(id: string): Promise<void> {
    if (id === this.currentOrgId) return;
    this.beginAppScopeChange();
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
    this.currentRelease = null;
    this.projects = [];
    this.apps = [];
    this.environments = [];
    this.releases = [];
    await this.loadOrgScope(id);
  }

  async setProject(id: string): Promise<void> {
    if (id === this.currentProjectId) return;
    this.beginAppScopeChange();
    this.currentProjectId = id;
    writeStored(PROJECT_KEY, id);
    writeStored(APP_KEY, null);
    writeStored(ENV_KEY, null);
    this.currentAppId = null;
    this.currentEnvId = null;
    this.currentRelease = null;
    this.apps = [];
    this.environments = [];
    this.releases = [];
    await this.loadProjectApps(id);
  }

  async setApp(id: string): Promise<void> {
    if (id === this.currentAppId) return;
    // Before anything else: any load still in flight for the app we are
    // leaving must not land on this one. Note this matters even when the user
    // comes back to an app they just left (A→B→A) — see `loadGen`.
    this.beginAppScopeChange();
    this.currentAppId = id;
    writeStored(APP_KEY, id);
    // The environment belongs to the previous app — carrying it over would
    // send another app's environment id to the API.
    writeStored(ENV_KEY, null);
    this.currentEnvId = null;
    this.environments = [];
    // The release belongs to the previous app too — release names are
    // per-app (`releaseKey`), so this only clears in-memory state; the old
    // app's own persisted selection is left alone for when it becomes
    // current again.
    this.currentRelease = null;
    this.releases = [];
    // Start both loads before awaiting either (rather than
    // `await loadAppEnvironments(id); await loadAppReleases(id);`), so both
    // `environmentsLoadAttemptedFor` and `releasesLoadAttemptedFor` are
    // stamped synchronously in the same tick. `loadAppEnvironments` stamps
    // its marker before its own network round trip, so a sequential await
    // here would let the Topbar effect's `ensureEnvironmentsLoaded()` guard
    // close (satisfied) while `ensureReleasesLoaded()`'s guard was still open
    // (not yet attempted) for the whole environments round trip — the effect
    // would then fire a second, concurrent `listReleases` alongside this
    // one's.
    await Promise.all([this.loadAppEnvironments(id), this.loadAppReleases(id)]);
  }

  setEnvironment(id: string | null): void {
    this.currentEnvId = id;
    writeStored(ENV_KEY, id);
  }

  /**
   * `null` = all releases; the literal `'none'` = rows with no release.
   * Persisted per app, unlike the environment (see `currentRelease`'s field
   * comment).
   */
  setRelease(id: string | null): void {
    this.currentRelease = id;
    if (this.currentAppId) writeStored(releaseKey(this.currentAppId), id);
    // A release narrows the env list (Topbar); if the current env is not in
    // it, fall back to "all" rather than sending an impossible pair. `'none'`
    // (unattributed) is left untouched — it does not name a row in
    // `environment_ids` the way a real environment id does, so it can never
    // be "not in" a release's set in any meaningful sense.
    if (id !== null && this.currentEnvId !== null && this.currentEnvId !== 'none') {
      // `id` is whatever the switcher offered, i.e. a name already put through
      // `selectableReleases`'s trim — so match the raw catalogue row on its
      // trimmed name (same lookup as `Topbar`'s `visibleEnvs`). Padded rows can
      // only predate the ingest-edge normalisation; a raw `===` would leave the
      // env pointing at an environment the selected release was never seen in.
      const r = this.releases.find((x) => x.release.trim() === id);
      if (r && !r.environment_ids.includes(this.currentEnvId)) this.setEnvironment(null);
    }
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
      this.beginAppScopeChange();
      this.resolveCurrentProject();
      this.apps = [];
      this.currentAppId = null;
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
      this.releases = [];
      this.currentRelease = null;
      this.releasesError = false;
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
      this.beginAppScopeChange();
      this.resolveCurrentApp();
      // The removed app's environments no longer apply. Whichever app
      // `resolveCurrentApp` landed on (or none) gets its own environments
      // loaded the next time it becomes current via `setApp`/`loadProjectApps`.
      this.environments = [];
      this.currentEnvId = null;
      this.environmentsError = false;
      writeStored(ENV_KEY, null);
      // Same reasoning for releases — they belong to the removed app.
      this.releases = [];
      this.currentRelease = null;
      this.releasesError = false;
    }
  }

  reset(): void {
    this.beginAppScopeChange();
    this.orgs = [];
    this.projects = [];
    this.apps = [];
    this.environments = [];
    this.environmentsError = false;
    this.releases = [];
    this.releasesError = false;
    this.access = null;
    this.accessError = false;
    this.currentOrgId = null;
    this.currentProjectId = null;
    this.currentAppId = null;
    this.currentEnvId = null;
    this.currentRelease = null;
    this.loaded = false;
    writeStored(ORG_KEY, null);
    writeStored(PROJECT_KEY, null);
    writeStored(APP_KEY, null);
    writeStored(ENV_KEY, null);
  }
}

export const sessionStore = new SessionStore();
