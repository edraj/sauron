/**
 * The topbar scope (org → project → app → release → environment) as URL query
 * parameters, so a copied link opens on exactly the view it was copied from.
 *
 * It lives in the page's REAL query string — `/?org=…&app=…#/users?from=…` —
 * and never in the hash's. Every list page rewrites its own hash query with
 * svelte-spa-router's `replace()` from its own state (filters, window, sort),
 * which drops any parameter that page does not own; scope parameters in the
 * hash would be erased on the first filter change. The router never reads or
 * writes `location.search`, so the two halves cannot clobber each other, and a
 * hash navigation (`href="#/persons/…"`) keeps the search part as it is.
 *
 * Ids, not names or slugs, for org / project / app / environment: they survive
 * a rename (environments are renamed in place, keeping their id), and a slug
 * is only unique inside its parent. The release has no id — its name IS its
 * key, the same string `?release=` carries to the API.
 */

/** Every parameter this module owns, in breadcrumb order. */
export const SCOPE_PARAMS = ['org', 'project', 'app', 'release', 'env'] as const;

/** The store's current selection. `null` env / release = "all". */
export interface ScopeSelection {
  org: string | null;
  project: string | null;
  app: string | null;
  /** `null` = all releases; `'none'` = rows with no release. */
  release: string | null;
  /** `null` = all environments; `'none'` = unattributed. */
  env: string | null;
}

/**
 * The scope a link names. `undefined` = the link says nothing about that level
 * (fall back to the remembered selection); `null` = "all".
 */
export interface ScopeSeed {
  org?: string;
  project?: string;
  app?: string;
  release?: string | null;
  env?: string | null;
}

/**
 * Read a link's scope out of `location.search`, or `null` when it names none.
 *
 * "All" is spelled by ABSENCE, not by a sentinel value: a link that names an
 * app but no `env` means all environments (and likewise for `release`). A
 * sentinel such as `release=all` would collide with a release literally named
 * `all`, the way `'none'` already does. Absence can only mean "all" once the
 * link names an app, though — a link without one says nothing about either,
 * so the remembered selection stands.
 */
export function parseScopeSearch(search: string): ScopeSeed | null {
  const p = new URLSearchParams(search);
  const read = (k: string): string | undefined => {
    const v = p.get(k);
    return v === null || v === '' ? undefined : v;
  };
  const seed: ScopeSeed = {};
  const org = read('org');
  const project = read('project');
  const app = read('app');
  if (org !== undefined) seed.org = org;
  if (project !== undefined) seed.project = project;
  if (app !== undefined) seed.app = app;
  const release = read('release');
  const env = read('env');
  if (release !== undefined) seed.release = release;
  else if (app !== undefined) seed.release = null;
  if (env !== undefined) seed.env = env;
  else if (app !== undefined) seed.env = null;
  return Object.keys(seed).length > 0 ? seed : null;
}

/**
 * `search` with the scope parameters set to `scope`: `''` or `'?…'`, ready to
 * compare against `location.search` and to pass to `history.replaceState`.
 *
 * Scope parameters come first, in breadcrumb order, so the address bar reads
 * like the topbar; any parameter this module does not own is kept, after them.
 * `null` levels are omitted — see `parseScopeSearch` for why absence is "all".
 */
export function scopeSearch(search: string, scope: ScopeSelection): string {
  const foreign = new URLSearchParams(search);
  for (const k of SCOPE_PARAMS) foreign.delete(k);
  const out = new URLSearchParams();
  for (const k of SCOPE_PARAMS) {
    const v = scope[k];
    if (v) out.set(k, v);
  }
  for (const [k, v] of foreign) out.append(k, v);
  const qs = out.toString();
  return qs ? `?${qs}` : '';
}

/**
 * Which levels of `seed` the store could NOT honour — the link named something
 * this member cannot open here (another org's app, a retired environment, a
 * release the app never reported). Empty when the link landed as written.
 *
 * Compared against what the store resolved to, rather than tracked inside each
 * resolver, so there is one definition of "honoured" and it is the visible one.
 */
export function unmetScope(seed: ScopeSeed, current: ScopeSelection): (keyof ScopeSelection)[] {
  const missed: (keyof ScopeSelection)[] = [];
  for (const k of SCOPE_PARAMS) {
    const want = seed[k];
    if (want === undefined) continue;
    if (want !== current[k]) missed.push(k);
  }
  return missed;
}
