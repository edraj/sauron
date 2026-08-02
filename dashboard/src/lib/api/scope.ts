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
// prefix of the former) on all of them. This array is load-bearing in BOTH
// directions now, not just backend-to-dashboard: Task 14's
// `backend/bins/sauron-api/tests/http_env_scoping.rs` reads this exact array
// out of this file's source (the same way `permissions.test.ts` reads
// `rbac.rs` in the other direction) and asserts it equals the set of
// app-scoped GETs that actually 400 on a valid `environment_id` when driven
// over HTTP — so an entry added here without a matching backend
// `reject_environment_id*` call, or a backend rejection added without a
// matching entry here, now fails a test instead of drifting silently. Each
// entry below is one of the `reject_environment_id*` call-site groups,
// verified directly against `backend/bins/sauron-api/src/routes/` and
// `main.rs`'s route table:
//
//  - `/v1/apps/{id}` bare (`apps::get_app`) — app metadata (name,
//    ingest_enabled) has no environment dimension. Calls
//    `reject_environment_id`; a 400 without this exclusion.
//
//  - `/v1/apps/{id}/environments` (`environments::list_environments`) — this
//    IS the environment list; scoping the request that enumerates
//    environments to one of them is circular. Calls `reject_environment_id`;
//    a 400 without this exclusion.
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
//
// `get_app` and `list_environments` used to parse no `environment_id` at all
// (an extra query param was silently dropped, not rejected) — Task 14's
// router-enumeration test caught that as the exact defect class this whole
// module exists to prevent (a 200 on a malformed `environment_id` looks like
// a filter was applied and wasn't), so both now call `reject_environment_id`
// too. This array's comment used to call those two "precautionary rather
// than required"; that is no longer true, and the wording above has been
// corrected rather than left stale.
//
//  - `/v1/apps/{id}/inspector/policy` (`inspector::effective_policy`) — the
//    PII inspector is APP-scoped. Findings carry their own environment
//    dimension inside the payload (`env_scope` plus `environment_id`), and
//    MASKING cannot be limited to one environment at all: the pipeline
//    enforcer keys on `app_id` alone, and a policy that masks in prod but not
//    staging is a footgun that produces exactly the leak the feature exists to
//    prevent. It calls `reject_environment_id_with_message` with that reason.
//
//  - `/v1/apps/{id}/inspector/mask-actions`
//    (`inspector::list_app_mask_actions`) and
//    `/v1/apps/{id}/inspector/masked-keys`
//    (`inspector::list_app_masked_keys`) — the same app-scoped rule, the same
//    message, the same helper. A mask action's `targets` name a table, a
//    column and a json path and nothing else, and `inspector_masked_keys` has
//    no environment column at all, so there is no environment dimension to
//    narrow on: accepting the parameter would report a filter that was never
//    applied. Added in the same change that MOUNTED both routes, because the
//    Rust contract test enumerates `main.rs` and an entry added here ahead of
//    its route fails that test exactly as loudly as a missing one does.
//    `POST /v1/apps/{id}/inspector/mask-preview` rejects `environment_id` too
//    and is deliberately NOT listed HERE: `http_env_scoping.rs`'s
//    `app_scoped_get_route_templates()` only collects `.route(...)` calls
//    containing `get(`, and
//    `the_backend_rejection_set_matches_the_dashboard_exclusion_list` asserts
//    that collected set EQUALS this array. A POST-only path here would sit in
//    `expected` and could never appear in `rejecting`, so the contract test
//    would fail on a perfectly correct handler. Every entry that IS listed has
//    a GET. It is excluded from SCOPING all the same, one array down — see
//    `NON_GET_BACKEND_REJECTIONS`.
const BACKEND_REJECTS_ENVIRONMENT_ID: RegExp[] = [
  /^\/v1\/apps\/[^/]+\/?(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/environments(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/funnels(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/artifacts(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/errors\/timeseries(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/events\/timeseries(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/transactions\/timeseries(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/inspector\/policy(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/inspector\/mask-actions(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/inspector\/masked-keys(?:[/?].*)?$/,
];

// `/v1/apps/{id}/first-event` (`apps::first_event`) is different in kind from
// the array above: the backend *does* support scoping it for real
// (`read_scope`, not `reject_environment_id` — it would happily narrow on a
// valid `environment_id`). It is excluded here anyway, for a reason specific
// to the ONE call site that hits it: `Onboarding.svelte` polls a
// just-created app by that app's own id, not necessarily
// `sessionStore.currentAppId` — the store's `currentEnvId` can belong to a
// different app entirely. Attaching it blindly would scope the poll to an
// environment that doesn't belong to the app being onboarded, silently
// under-reporting "no events yet" forever.
//
// Kept as a SEPARATE array from `BACKEND_REJECTS_ENVIRONMENT_ID` on purpose:
// that array is checked against the backend's actual rejection set by
// `http_env_scoping.rs` (see the comment above it), and `first-event` is not
// part of that correspondence — folding it in would make the Rust test
// demand the backend reject a route it correctly narrows on.
const UI_ONLY_EXCLUSIONS: RegExp[] = [/^\/v1\/apps\/[^/]+\/first-event(?:[/?].*)?$/];

// App-scoped routes the backend rejects `environment_id` on that have NO `get(`
// handler, so they cannot live in `BACKEND_REJECTS_ENVIRONMENT_ID` without
// failing `http_env_scoping.rs`'s equality assertion (see the note at the end of
// that array's comment). They still have to be excluded from scoping, because
// the exclusion list is what `shouldScopeUrl` reads — being absent from it is
// not "unchecked", it is "scoped".
//
// Measured, before this array existed: with an environment selected (the
// default — `resolveCurrentEnvironment` auto-selects one), the MaskDialog's
// first call went out as
// `POST /v1/apps/{id}/inspector/mask-preview?environment_id=…` and came back
// 400 "the inspector is app-scoped; masking cannot be limited to one
// environment". The dialog then sat on "Counting affected rows…" forever with
// an empty app slug, so masking was unreachable from the UI entirely.
const NON_GET_BACKEND_REJECTIONS: RegExp[] = [
  /^\/v1\/apps\/[^/]+\/inspector\/mask-preview(?:[/?].*)?$/,
];

const APP_CONFIG_SUBPATHS: RegExp[] = [
  ...BACKEND_REJECTS_ENVIRONMENT_ID,
  ...UI_ONLY_EXCLUSIONS,
  ...NON_GET_BACKEND_REJECTIONS,
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

// ---------------------------------------------------------------------------
// Project-scoped routes.
//
// `APP_SCOPED_URL` above only matches `/v1/apps/...`, so nothing under
// `/v1/projects/...` is ever scoped by the interceptor — that part is safe by
// construction and this array changes no behaviour here. What it does is close
// the same two-directional gap for a NEW route family: the active-users
// endpoints are the first telemetry reads outside `/v1/apps/{id}/…`, so they
// sit outside the only mechanised check that a telemetry GET resolves
// environment scoping rather than accepting-and-ignoring it.
// `backend/bins/sauron-api/tests/http_env_scoping.rs` reads THIS array's
// literal source and asserts it equals the set of project-scoped GETs that
// actually 400 on a valid `environment_id`.
//
// Only the rejecting routes belong here. `/v1/projects/{id}` and
// `/v1/projects/{id}/apps` neither narrow nor reject — they are ordinary
// configuration reads with no environment dimension and no `Query` field for
// one — so listing them would make the Rust test demand a rejection the
// backend does not perform.
export const PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID: RegExp[] = [
  /^\/v1\/projects\/[^/]+\/active-users(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/active-users\.csv(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/environments(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/monitors(?:[/?].*)?$/,
];
