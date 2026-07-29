// Environment scoping for the axios request interceptor in `client.ts`.
//
// Split into its own module for the same reason auth has its own bridge in
// `client.ts`: `client.ts` must not import a store directly, because
// `sessionStore` (transitively, via `../api/{orgs,apps,environments}`)
// imports `client.ts` itself — a module-level `import { sessionStore }` here
// would recreate that `store -> api -> client -> store` cycle one hop later
// (`client.ts -> scope.ts -> session.svelte.ts -> apps.ts -> client.ts`).
//
// So this module takes the same approach as `configureAuthBridge`: it holds
// no store import, only a bridge interface the store wires itself into once,
// at construction (see `SessionStore`'s constructor in `session.svelte.ts`).

export interface ScopeBridge {
  /**
   * The currently selected environment id, or `null` for "all environments".
   * The literal string `'none'` means "unattributed" and is a real id as far
   * as this module is concerned — it is passed straight through to the wire.
   */
  getCurrentEnvironmentId(): string | null;
}

const noopScopeBridge: ScopeBridge = {
  getCurrentEnvironmentId: () => null,
};

let bridge: ScopeBridge = noopScopeBridge;

export function configureScopeBridge(next: ScopeBridge): void {
  bridge = next;
}

/** Reads the current environment id through the bridge. `null` = all environments. */
export function currentEnvironmentId(): string | null {
  return bridge.getCurrentEnvironmentId();
}

// ---------------------------------------------------------------------------
// The rule is opt-IN, not opt-out.
//
// This module used to work the other way around: scope everything except a
// short list of URL substrings (`/environments`, `/first-event`, `/funnels`,
// `/artifacts`, plus a bare-`/v1/apps/{id}` regex). An opt-out list fails
// *open* — anything the list's author forgot, or any route family added
// later, gets `environment_id` attached by default. That is exactly what
// happened: a grep of `backend/bins/sauron-api/src/routes/` for
// `reject_environment_id` (the backend's hard-`400`-on-any-value guard, see
// `routes/scope.rs`) turns up five call sites, and the opt-out list only
// accounted for two of them (`funnels.rs`, `artifacts.rs`):
//
//   - `admin.rs`         -> GET  /v1/admin/storage
//   - `artifacts.rs`     -> already covered (app-scoped, see below)
//   - `funnels.rs`       -> already covered (app-scoped, see below)
//   - `monitors.rs`      -> /v1/projects/{id}/monitors,
//                           /v1/monitors/{id}, /v1/monitors/{id}/checks,
//                           /v1/monitors/{id}/incidents
//   - `notifications.rs` -> /v1/orgs/{id}/notification-channels,
//                           /v1/notification-channels/{id}[/test],
//                           /v1/orgs/{id}/alert-rules, /v1/alert-rules/{id},
//                           /v1/orgs/{id}/alert-events, /v1/alert-meta
//
// None of the monitors/notifications/admin URLs matched the opt-out
// substrings, so every request on Settings > Alerting, Uptime > Monitors and
// Admin > Storage was getting `environment_id` attached and rejected with a
// 400 — not an edge case, since `sessionStore.currentEnvId` is non-null by
// default (`resolveCurrentEnvironment` auto-selects the app's default
// environment).
//
// The fix inverts the default: only telemetry reads under
// `/v1/apps/{app_id}/...` are eligible for scoping at all. Everything else —
// `/v1/monitors/...`, `/v1/projects/...`, `/v1/orgs/...`, `/v1/admin/...`,
// `/v1/auth/...`, `/v1/environments/...` (and any route family added after
// this comment was written) — is unscoped *by construction*, with no list to
// forget to update. A new non-telemetry route is safe by default; a new
// telemetry route under `/v1/apps/{app_id}/...` is scoped by default and
// must be added to the exclusion list below only if the backend actually
// rejects the parameter (verify against `reject_environment_id` call sites,
// don't guess from the URL shape).
const APP_SCOPED_URL = /^\/v1\/apps\/[^/]+(?:\/.*)?$/;

// Within `/v1/apps/{app_id}/...`, these sub-resources are app *configuration*
// rather than telemetry, and the backend calls `reject_environment_id` (or
// `reject_environment_id_with_message` — same grep target, its name is a
// prefix of the former) on all of them — so, unlike the rest of the module's
// new default-safe posture, this remaining exclusion list is load-bearing and
// must stay reconciled against the backend by hand. Each entry below is one
// of the six `reject_environment_id*` call-site groups (or an
// already-forgiving one kept for symmetry), verified directly against
// `backend/bins/sauron-api/src/routes/` and `main.rs`'s route table:
//
//  - `/v1/apps/{id}` bare (`apps::get_app`) — app metadata (name,
//    ingest_enabled) has no environment dimension. The handler doesn't parse
//    a Query extractor at all, so an extra `environment_id` would just be
//    ignored today, not rejected — excluded anyway so that stays true by
//    construction rather than by accident.
//
//  - `/v1/apps/{id}/environments` (`environments::list_environments`) — this
//    IS the environment list; scoping the request that enumerates
//    environments to one of them is circular. Like `get_app`, the handler
//    doesn't parse `environment_id` at all, so this is precautionary rather
//    than required.
//
//  - `/v1/apps/{id}/first-event` (`apps::first_event`) — the backend *does*
//    support scoping this one for real (`read_scope`, not
//    `reject_environment_id`). Excluded anyway: the only call site
//    (Onboarding.svelte) polls a just-created app by that app's own id, not
//    necessarily `sessionStore.currentAppId` — the store's `currentEnvId`
//    can belong to a different app entirely. Attaching it blindly would
//    scope the poll to an environment that doesn't belong to the app being
//    onboarded, silently under-reporting "no events yet" forever.
//
//  - `/v1/apps/{id}/funnels` (`funnels::list_saved` / `create_saved` /
//    `update_saved` / `delete_saved`) — saved funnel *definitions* (plural,
//    app-wide config: name/description/step list). Calls
//    `reject_environment_id`; a 400 without this exclusion.
//    NOT the same route as `/v1/apps/{id}/funnel` (singular, `POST`,
//    `funnels::compute`) — that one computes live counts for a chosen date
//    range and calls `read_scope`, i.e. it IS telemetry and must stay
//    scoped. The two differ by exactly the trailing "s", so this is matched
//    as a literal path segment, not a word-in-URL substring, to avoid a
//    plural match accidentally swallowing the singular. Verified against the
//    route table in `main.rs` rather than pattern-matched from the word.
//
//  - `/v1/apps/{id}/artifacts` (`artifacts::list` / `upload` / `delete`) —
//    symbol artifacts are app-wide config, not telemetry. Calls
//    `reject_environment_id`; a 400 without this exclusion.
//
//  - `/v1/apps/{id}/errors/timeseries`, `/v1/apps/{id}/events/timeseries`,
//    `/v1/apps/{id}/transactions/timeseries` (`analytics::error_timeseries` /
//    `event_timeseries` / `transaction_timeseries`) — cross-tier reads that
//    route across hot Postgres and cold Parquet; cold storage is not
//    partitioned by environment yet, so these reject any `environment_id` at
//    all rather than scoping only the hot half. Each calls
//    `reject_environment_id_with_message` (a `reject_environment_id*` call
//    site like every other entry above — the grep below finds it because its
//    name is a prefix of `reject_environment_id`), which preserves their own
//    reason string instead of the generic one. Previously these three
//    hand-rolled an inline `raw_environment_id(..).is_some()` check instead of
//    calling through `routes::scope` at all, which is exactly what made the
//    grep below miss them — fixed at the source rather than patched here only.
const APP_CONFIG_SUBPATHS: RegExp[] = [
  /^\/v1\/apps\/[^/]+\/?(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/environments(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/first-event(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/funnels(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/artifacts(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/errors\/timeseries(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/events\/timeseries(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/transactions\/timeseries(?:[/?].*)?$/,
];

/** True if `url` is a read that environment scoping applies to at all. */
export function shouldScopeUrl(url: string | undefined): boolean {
  if (!url) return false;
  if (!APP_SCOPED_URL.test(url)) return false;
  return !APP_CONFIG_SUBPATHS.some((re) => re.test(url));
}

/**
 * The query params the interceptor should merge onto a request, or
 * `undefined` when it should add nothing at all.
 *
 * Two independent reasons to add nothing, both load-bearing:
 *  - `url` is not an app-scoped telemetry read (see above).
 *  - `envId` is `null` ("all environments") — the parameter must be omitted
 *    entirely rather than sent empty. The backend treats a *present* but
 *    empty `?environment_id=` as a `400`, not "all" (that silent-widening
 *    bug was a Critical in an earlier review); sending nothing is the only
 *    correct way to ask for every environment.
 */
export function computeScopeParams(
  url: string | undefined,
  envId: string | null,
): Record<string, string> | undefined {
  if (!shouldScopeUrl(url)) return undefined;
  if (envId === null) return undefined;
  return { environment_id: envId };
}
