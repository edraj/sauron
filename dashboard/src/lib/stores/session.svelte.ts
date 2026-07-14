import { getAccess, listOrgs } from '../api/orgs';
import { listProjects } from '../api/projects';
import { listApps } from '../api/apps';
import type {
  AccessResponse,
  App,
  Organization,
  Permission,
  Project,
} from '../models';

const ORG_KEY = 'sauron.org_id';
const PROJECT_KEY = 'sauron.project_id';
const APP_KEY = 'sauron.app_id';

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
 * Holds the current org → project → app selection plus the lists needed to
 * switch between them, and the access grants for the current org. Selections
 * persist to localStorage so reloads land you back where you were.
 */
class SessionStore {
  orgs = $state<Organization[]>([]);
  projects = $state<Project[]>([]);
  apps = $state<App[]>([]);

  currentOrgId = $state<string | null>(null);
  currentProjectId = $state<string | null>(null);
  currentAppId = $state<string | null>(null);

  // Access grants for the current org — drives every permission check.
  access = $state<AccessResponse | null>(null);

  loaded = $state(false);
  loading = $state(false);

  get currentOrg(): Organization | null {
    return this.orgs.find((o) => o.id === this.currentOrgId) ?? null;
  }

  get currentProject(): Project | null {
    return this.projects.find((p) => p.id === this.currentProjectId) ?? null;
  }

  get currentApp(): App | null {
    return this.apps.find((a) => a.id === this.currentAppId) ?? null;
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
        this.access = null;
        this.currentOrgId = null;
        this.currentProjectId = null;
        this.currentAppId = null;
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

  // -------------------------------------------------------------------------
  // Switching
  // -------------------------------------------------------------------------

  async setOrg(id: string): Promise<void> {
    if (id === this.currentOrgId) return;
    this.currentOrgId = id;
    writeStored(ORG_KEY, id);
    // Downstream selections belong to the previous org — clear them so the new
    // org resolves to its own first project/app.
    writeStored(PROJECT_KEY, null);
    writeStored(APP_KEY, null);
    this.currentProjectId = null;
    this.currentAppId = null;
    this.projects = [];
    this.apps = [];
    await this.loadOrgScope(id);
  }

  async setProject(id: string): Promise<void> {
    if (id === this.currentProjectId) return;
    this.currentProjectId = id;
    writeStored(PROJECT_KEY, id);
    writeStored(APP_KEY, null);
    this.currentAppId = null;
    this.apps = [];
    await this.loadProjectApps(id);
  }

  setApp(id: string): void {
    this.currentAppId = id;
    writeStored(APP_KEY, id);
  }

  /** Select a project + app together (used when jumping from lists). */
  async selectApp(projectId: string, appId: string): Promise<void> {
    if (projectId !== this.currentProjectId) {
      await this.setProject(projectId);
    }
    this.setApp(appId);
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
    }
  }

  upsertApp(app: App, select = true): void {
    // Only track in the local list when it belongs to the current project.
    if (app.project_id === this.currentProjectId) {
      const idx = this.apps.findIndex((a) => a.id === app.id);
      if (idx >= 0) this.apps[idx] = app;
      else this.apps = [...this.apps, app];
    }
    if (select) this.setApp(app.id);
  }

  removeApp(appId: string): void {
    this.apps = this.apps.filter((a) => a.id !== appId);
    if (this.currentAppId === appId) this.resolveCurrentApp();
  }

  reset(): void {
    this.orgs = [];
    this.projects = [];
    this.apps = [];
    this.access = null;
    this.currentOrgId = null;
    this.currentProjectId = null;
    this.currentAppId = null;
    this.loaded = false;
    writeStored(ORG_KEY, null);
    writeStored(PROJECT_KEY, null);
    writeStored(APP_KEY, null);
  }
}

export const sessionStore = new SessionStore();
