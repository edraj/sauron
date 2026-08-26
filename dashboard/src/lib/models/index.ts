// TypeScript interfaces mirroring the Sauron API contract.
// Shapes were verified against the live backend at http://localhost:8090.
// Hierarchy: Org → Project (grouping) → App (holds the DSN). Signals live under apps.

import type { ScopeRef } from './scope-tree';

// ---------------------------------------------------------------------------
// Auth & user
// ---------------------------------------------------------------------------

export interface User {
  id: string;
  email: string;
  name: string | null;
  last_login_at: string | null;
  created_at: string;
  updated_at: string;
  must_change_password: boolean;
  is_active: boolean;
}

export interface AuthTokens {
  access_token: string;
  refresh_token: string;
  expires_at: number;
}

export interface AuthSession extends AuthTokens {
  user: User;
}

export interface RefreshResponse extends AuthTokens {}

export interface LoginPayload {
  email: string;
  password: string;
}

export interface RegisterPayload {
  email: string;
  password: string;
  name?: string;
  org_name: string;
}

// ---------------------------------------------------------------------------
// Orgs, projects & apps
// ---------------------------------------------------------------------------

export interface Organization {
  id: string;
  name: string;
  slug: string;
  created_at: string;
  updated_at: string;
  /**
   * Projects in this org the current user can reach, counted server-side with
   * the same rule `/v1/orgs/{id}/projects` lists by.
   *
   * Carried on the org rather than fetched per org because the shell needs it
   * for EVERY org at once: onboarding is only correct when the user has no
   * reachable project anywhere, and answering that from per-org calls costs one
   * request per org on every cold load.
   */
  project_count: number;
  /**
   * Whether this user may create a project in this org.
   *
   * Server-sent because `/access` is fetched for the current org only, so the
   * org picker has no way to evaluate it for the other rows. Together with
   * `project_count` it distinguishes a dead-end org (nothing to see, nothing
   * to create — lock it) from an empty org the member can start work in.
   */
  can_create_project: boolean;
}

// A project is now a pure grouping container within an org. It no longer holds
// the DSN — that lives on apps.
export interface Project {
  id: string;
  org_id: string;
  name: string;
  slug: string;
  created_at: string;
  updated_at: string;
}

// The kinds of app the ingest gateway understands. `app_type` drives the icon.
export type AppType =
  | 'web'
  | 'flutter'
  | 'ios'
  | 'android'
  | 'react_native'
  | 'node'
  | 'python'
  | 'csharp';

// An app holds the public key / DSN and is the scope signals are reported under.
export interface App {
  id: string;
  project_id: string;
  name: string;
  slug: string;
  app_type: AppType;
  ingest_enabled: boolean;
  // Retained by the API for backwards compat; not surfaced in the UI.
  platform?: string | null;
  /**
   * The environment whose build ships to the app stores, or `null`.
   *
   * An `AppEnvironment` (enrollment) id — the same id the environment switcher
   * carries — so the Overview gate is a plain `===` against
   * `sessionStore.currentEnvironmentId`. It decides where the store section is
   * SHOWN; it does not partition the numbers, because Google and Apple report
   * per package/bundle and have no environment dimension at all.
   */
  store_environment_id: string | null;
  created_at: string;
  updated_at: string;
}

// ---------------------------------------------------------------------------
// Environments live at two levels, and the split is load-bearing.
//
//  1. The CATALOGUE (`ProjectEnvironment`, table `environments`) is owned by a
//     project. It is where a name is *defined* — "we ship to dev, staging,
//     production" — and every app in the project shares those names. It holds
//     no key and no ingest switch.
//
//  2. The ENROLLMENT (`AppEnvironment`, table `app_environments`) is one app's
//     membership in one catalogue environment. It holds the ingest key and the
//     switches that are genuinely per-app (muted / default).
//
// Enrollments are auto-created on both sides: adding an environment to a
// project enrolls every app in it, and creating an app enrolls it in every
// live environment of its project. Neither level is ever created by hand from
// the other's endpoint.
// ---------------------------------------------------------------------------

/**
 * A catalogue entry: an environment *name* as the project defines it, shared
 * by every app in that project. Returned by
 * `GET|POST /v1/projects/{project_id}/environments` and by
 * `PATCH|DELETE /v1/environments/{env_id}`.
 *
 * Renaming or retiring one of these is a PROJECT-WIDE action — it changes what
 * every app in the project sees.
 */
export interface ProjectEnvironment {
  id: string;
  project_id: string;
  name: string;
  /** Non-null once retired: cascades to every app's enrollment in it. */
  retired_at: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * One app's enrollment in one catalogue environment, as the mutation
 * endpoints return it — `PATCH|DELETE /v1/app-environments/{id}` and
 * `POST /v1/app-environments/{id}/rotate-key`.
 *
 * `id` is the ENROLLMENT id, not the catalogue id: it is the id that appears
 * in a DSN, that env-scoped RBAC grants name, and that `?environment_id=`
 * filters on. `environment_id` points at the `ProjectEnvironment` this row
 * enrolls into. These endpoints return the bare row with no name joined on —
 * the name lives on the catalogue row, which is exactly the drift this model
 * removed.
 */
export interface AppEnvironmentRow {
  id: string;
  app_id: string;
  /** The `ProjectEnvironment.id` this enrollment belongs to. */
  environment_id: string;
  /** Non-secret, write-only ingest credential. Safe to render. */
  public_key: string;
  ingest_enabled: boolean;
  is_default: boolean;
  /** Non-null once retired: ingest is off and it is hidden from pickers. */
  retired_at: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * An enrollment joined to its catalogue name — what
 * `GET /v1/apps/{app_id}/environments` returns and what every picker, DSN
 * table and env-scope label in the dashboard renders.
 */
export interface AppEnvironment extends AppEnvironmentRow {
  name: string;
}

export interface FirstEventStatus {
  received: boolean;
  /** Presence flags, not counts — the API does an existence check only. */
  errors: boolean;
  events: boolean;
}

/**
 * One row of GET /v1/me/sessions — a login of the current user that has
 * survived refresh-token rotation.
 *
 * NOT `AuthSession`: that name is taken above by the login *response*, and
 * shadowing it compiles while silently changing the auth store's types.
 *
 * `revoked_by` is deliberately absent — the API never serializes it, because it
 * would tell a member which admin signed them out.
 */
export interface AccountSession {
  id: string;
  created_at: string;
  last_used_at: string;
  expires_at: string;
  /** Marked server-side; the dashboard has no JWT decoder and should not gain one. */
  current: boolean;
  user_agent: string | null;
  browser: string | null;
  os: string | null;
  device_kind: string | null;
  ip: string | null;
  /** Only ever set on rows returned with `?include_revoked=1`. */
  revoked_at: string | null;
  revoked_reason: string | null;
}

// ---------------------------------------------------------------------------
// Access control (RBAC)
// ---------------------------------------------------------------------------

export type ScopeType = 'org' | 'project' | 'app' | 'env';

// Known permission strings. `(string & {})` keeps autocomplete while tolerating
// any future permission the backend introduces.
export type Permission =
  | 'issue:read'
  | 'issue:write'
  | 'event:read'
  | 'funnel:write'
  | 'artifact:write'
  | 'source:read'
  | 'monitor:read'
  | 'monitor:write'
  | 'app:read'
  | 'app:create'
  | 'app:update'
  | 'app:delete'
  | 'env:read'
  | 'env:create'
  | 'env:update'
  | 'env:delete'
  | 'env:rotate_key'
  | 'project:read'
  | 'project:create'
  | 'project:update'
  | 'project:delete'
  | 'member:read'
  | 'member:manage'
  | 'member:credential'
  | 'role:manage'
  | 'org:manage'
  | 'alert:read'
  | 'alert:write'
  | 'pii:read'
  | 'pii:manage'
  | (string & {});

// One entry in the `grants` array of GET /v1/orgs/{org}/access — the scoped set
// of permissions the current user holds.
export interface GrantView {
  scope_type: ScopeType;
  scope_id: string;
  permissions: Permission[];
}

export interface AccessResponse {
  // Flattened org-level permissions (convenience — gating uses `grants`).
  permissions: Permission[];
  grants: GrantView[];
}

export interface Role {
  id: string;
  org_id: string | null;
  name: string;
  description: string | null;
  is_system: boolean;
  permissions: Permission[];
  created_at?: string;
}

// A row from GET /v1/orgs/{org}/members — a materialized grant with the
// resolved user + role.
export interface MemberGrant {
  id: string;
  user_id: string;
  email: string;
  name: string | null;
  role_id: string;
  role_name: string;
  scope_type: ScopeType;
  scope_id: string;
  is_active: boolean;
  /** Non-null while an admin-forced reset is outstanding. Comes from
      `GET /v1/orgs/{org}/members`, the only place the dashboard learns anything
      about a member's account state. */
  credentials_invalidated_at: string | null;
}

/**
 * One person, with every grant they hold in the org.
 *
 * The API returns one row per grant. The table renders one row per person:
 * deactivation and editing are per-account, so a member with three grants
 * would otherwise show three identical Deactivate buttons.
 */
export interface Member {
  user_id: string;
  email: string;
  name: string | null;
  is_active: boolean;
  /** Non-null while an admin-forced reset is outstanding. Comes from
      `GET /v1/orgs/{org}/members`, the only place the dashboard learns anything
      about a member's account state. */
  credentials_invalidated_at: string | null;
  grants: MemberGrant[];
}

export interface CreateMemberPayload {
  email: string;
  name: string;
  role_id: string;
  scopes: ScopeRef[];
}

export interface CreateMemberResult {
  user_id: string;
  grant_ids: string[];
  /** First of `grant_ids`. The API still emits it so an older dashboard build
      keeps working after a partial upgrade; nothing here reads it. */
  grant_id?: string;
  temp_password: string;
}

export interface MemberPasswordResetResult {
  ok: boolean;
  action: 'reset' | 'cancel';
  /** RFC 3339 when the link expires; null for `cancel`. Never a token — the
      server refuses to return the link under any condition. */
  expires_at: string | null;
}

/** One entry in the scope picker: the org, a project, or an app. */
export interface ScopeOption {
  key: string; // `${scope_type}:${scope_id}`
  label: string;
  scope_type: ScopeType;
  scope_id: string;
}

export interface UpdateGrantPayload {
  role_id?: string;
  scope_type?: ScopeType;
  scope_id?: string;
}

export interface UpdateRolePayload {
  name?: string;
  description?: string;
  permissions?: Permission[];
}

/**
 * The batch form (`scopes`) is what the picker sends. The singular pair is the
 * legacy shape the API still accepts, and is kept in the union because
 * EditMemberDialog adds exactly one grant at a time.
 */
export type CreateGrantPayload = { email: string; role_id: string } & (
  | { scopes: ScopeRef[] }
  | { scope_type: ScopeType; scope_id: string }
);

export interface CreateRolePayload {
  name: string;
  description?: string;
  permissions: Permission[];
}

/** Collapse the flat grant list into one entry per person, preserving order. */
export function groupMembers(grants: MemberGrant[]): Member[] {
  const byUser = new Map<string, Member>();
  for (const g of grants) {
    const existing = byUser.get(g.user_id);
    if (existing) {
      existing.grants.push(g);
    } else {
      byUser.set(g.user_id, {
        user_id: g.user_id,
        email: g.email,
        name: g.name,
        is_active: g.is_active,
        credentials_invalidated_at: g.credentials_invalidated_at,
        grants: [g],
      });
    }
  }
  return [...byUser.values()];
}

// ---------------------------------------------------------------------------
// Issues & error events
// ---------------------------------------------------------------------------

export type IssueLevel = 'debug' | 'info' | 'warning' | 'error' | 'fatal' | string;
export type IssueStatus = 'unresolved' | 'resolved' | 'ignored';

export interface Issue {
  id: string;
  app_id: string;
  fingerprint: string;
  type: string;
  title: string;
  culprit: string | null;
  level: IssueLevel;
  status: IssueStatus;
  first_seen: string;
  last_seen: string;
  times_seen: number;
  users_seen: number;
  assignee_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface Frame {
  function?: string | null;
  module?: string | null;
  filename?: string | null;
  abs_path?: string | null;
  lineno?: number | null;
  colno?: number | null;
  in_app?: boolean | null;
}

// A frame after server-side symbolication: original file/function/line plus
// optional source context. Extends Frame so it renders through the same view.
export interface SymbolicatedFrame extends Frame {
  symbolicated: boolean;
  context_line?: string | null;
  pre_context?: string[];
  post_context?: string[];
  context_start_line?: number | null;
}

export type SymbolicationStatus =
  | 'pending'
  | 'symbolicated'
  | 'partial'
  | 'no_artifacts'
  | 'not_applicable'
  | 'failed';

export interface Breadcrumb {
  type: string;
  category?: string | null;
  message?: string | null;
  level?: string | null;
  timestamp: string;
  data?: Record<string, unknown> | null;
}

// The user embedded inside an error event (context.user / event_user).
export interface EventUser {
  id?: string | null;
  email?: string | null;
  username?: string | null;
  ip_address?: string | null;
  traits?: Record<string, unknown> | null;
}

export interface ErrorEvent {
  id: string;
  app_id: string;
  environment_id: string | null;
  issue_id: string;
  fingerprint: string;
  level: IssueLevel;
  message: string | null;
  exception_type: string | null;
  exception_value: string | null;
  title?: string | null;
  culprit?: string | null;
  stacktrace: Frame[];
  breadcrumbs: Breadcrumb[];
  context: Record<string, unknown> | null;
  tags: Record<string, unknown> | null;
  // Developer-attached scopes (distinct from the machine-owned `context` above).
  // Omitted by SDKs when empty, so treat as optional on the wire.
  contexts?: Record<string, unknown> | null;
  extra?: Record<string, unknown> | null;
  release: string | null;
  distinct_id: string | null;
  event_user: EventUser | null;
  sdk: unknown;
  ip_address: string | null;
  screen?: string | null;
  session_id: string | null;
  device_key: string | null;
  occurred_at: string;
  received_at: string;
  stacktrace_symbolicated?: SymbolicatedFrame[] | null;
  symbolication_status?: SymbolicationStatus | null;
  debug_meta?: DartDebugMeta | null;
}

// Dart (Flutter AOT) debug header stored on the event; carries the verbatim
// obfuscated trace for display when no symbols are uploaded yet.
export interface DartDebugMeta {
  build_id?: string | null;
  isolate_dso_base?: string | null;
  arch?: string | null;
  os?: string | null;
  raw_stacktrace?: string | null;
}

export interface SeriesPoint {
  bucket: string;
  count: number;
  segments?: { count: number; color?: string; label?: string }[];
}

export interface IssueDetail extends Issue {
  latest_event: ErrorEvent | null;
  series: SeriesPoint[];
}

/**
 * Occurrence totals for one issue under the active occurrence filters.
 *
 * Distinct from `Issue.times_seen`/`users_seen`, which are all-time and
 * app-wide (`users_seen` is a HyperLogLog estimate); these are exact counts
 * over the selected range and filters, so the two will legitimately differ.
 */
export interface IssueEventStats {
  events: number;
  users: number;
  sessions: number;
  /**
   * Whether the free-text term was matched against the event payload
   * (`contexts`/`extra`/`tags`) as well as the message and exception fields.
   *
   * **Three states, and the third is the point.** `null` means no free-text
   * search ran at all; `false` that one ran but the payload columns were
   * excluded because this member lacks `event:read`; `true` that it ran over
   * everything. Collapsing `null` into `false` would claim a narrowing on every
   * unfiltered request — the same "absent is not empty is not false" trap
   * `environmentsError` and `accessError` exist to avoid.
   *
   * `false` is the one worth surfacing: the member's search silently matched
   * less than they think it did, and nothing else on screen says so.
   */
  payload_searched: boolean | null;
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

export interface TopEvent {
  name: string;
  count: number;
}

export interface AnalyticsEvent {
  id: string;
  app_id?: string;
  environment_id?: string | null;
  name: string;
  distinct_id: string;
  properties: Record<string, unknown> | null;
  // Developer-attached scopes (distinct from the machine-owned `context` below).
  // Omitted by SDKs when empty, so treat as optional on the wire.
  tags?: Record<string, unknown> | null;
  contexts?: Record<string, unknown> | null;
  extra?: Record<string, unknown> | null;
  context?: Record<string, unknown> | null;
  session_id?: string | null;
  release?: string | null;
  ip_address?: string | null;
  occurred_at: string;
  received_at?: string;
  device_key?: string | null;
  screen?: string | null;
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface Session {
  id: string;
  app_id: string;
  session_id: string;
  distinct_id: string | null;
  device_key: string | null;
  started_at: string;
  last_event_at: string;
  events_count: number;
  errors_count: number;
  context: Record<string, unknown> | null;
  release: string | null;
  environment_id: string | null;
  ip_address: string | null;
  created_at: string;
  updated_at: string;
}

// A performance transaction (one timed operation).
export interface Transaction {
  id: string;
  app_id: string;
  environment_id: string | null;
  name: string;
  op: string;
  duration_ms: number;
  status: string | null;
  http_method: string | null;
  http_status: number | null;
  url: string | null;
  distinct_id: string | null;
  session_id: string | null;
  device_key: string | null;
  release: string | null;
  ip_address: string | null;
  occurred_at: string;
  received_at: string;
  workflow_id: string | null;
  workflow_name: string | null;
  restored_pin_id: string | null;
  finished_at: string | null;
  /**
   * Developer-supplied flat string tags, set per-call on `trackTransaction`.
   *
   * `null` means WITHHELD, not empty — `strip_transaction_body` nulls this for
   * a caller without `event:read`. An empty object means the span carried
   * none. Rendering code must not conflate the two.
   */
  tags: Record<string, string> | null;
  /**
   * Developer-supplied freeform JSON — the request body, the response body, a
   * retry count. `null` means withheld, for the reason on {@link tags}.
   *
   * May contain `{ _truncated: true, _bytes: N }` in place of the payload when
   * the SDK capped it (16 KB serialized). That marker is data, not an error:
   * the span is real and its timing is accurate.
   */
  extra: Record<string, unknown> | null;
}

// One entry on the session timeline — a discriminated union keyed by `kind`.
export type TimelineItem =
  | { kind: 'event'; at: string; event: AnalyticsEvent }
  | { kind: 'error'; at: string; error: ErrorEvent }
  | { kind: 'transaction'; at: string; transaction: Transaction };

export interface SessionDetail {
  session: Session;
  timeline: TimelineItem[];
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

export interface DeviceRow {
  id: string;
  device_key: string;
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
  arch: string | null;
  browser: string | null;
  last_distinct_id: string | null;
  first_seen: string;
  last_seen: string;
  events_count: number;
  errors_count: number;
  sessions_count: number;
}

/**
 * One row per (family, model, os_name, os_version) — the Devices inventory's
 * default shape. No `last_distinct_id`, `browser` or `arch`: none has a single
 * value across a group. All four are on `DeviceRow`, in the drill-down.
 */
export interface DeviceGroupRow {
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
  device_count: number;
  events_count: number;
  errors_count: number;
  sessions_count: number;
  first_seen: string;
  last_seen: string;
}

export interface DeviceDetail {
  // Environment-scoped, not the raw `devices` row — see the backend's
  // `get_device` doc comment (backend/crates/sauron-db/src/repo.rs).
  // events_count/errors_count come from the durable devices columns under
  // the "all environments" scope and from an environment-scoped LATERAL
  // otherwise, matching sessions/errors/perf below rather than showing
  // cross-environment, all-time totals above a scoped list.
  device: DeviceRow;
  sessions: Session[];
  errors: ErrorEvent[];
  perf: PerfSummaryRow[];
}

// ---------------------------------------------------------------------------
// Users Explorer
// ---------------------------------------------------------------------------

export interface PersonRow {
  distinct_id: string;
  properties: Record<string, unknown> | null;
  first_seen: string;
  last_seen: string;
  events_count: number;
  errors_count: number;
  sessions_count: number;
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

export interface OverviewTotals {
  events: number;
  errors: number;
  sessions: number;
  users: number;
  new_users: number;
  crashed_sessions: number;
}

export interface Overview {
  totals: OverviewTotals;
  error_rate: number;
  /**
   * `null` when the rate cannot be measured — NOT a fallback.
   *
   * Either the window holds no sessions, or it holds errors whose SDK never
   * reported `mechanism.handled` (node, python and csharp default their
   * uncaught-error capture OFF). Rendering 100% for those would state
   * "crash-free" about an app that may be crashing constantly.
   */
  crash_free_sessions: number | null;
  events_series: SeriesPoint[];
  errors_series: SeriesPoint[];
  top_issues: Issue[];
  top_events: TopEvent[];
}

// ---------------------------------------------------------------------------
// Exceptions dashboard stats
// ---------------------------------------------------------------------------

export interface IssueStats {
  total: number;
  unresolved: number;
  resolved: number;
  ignored: number;
  fatal: number;
  error: number;
  warning: number;
  info: number;
  series: SeriesPoint[];
}

// ---------------------------------------------------------------------------
// Funnels
// ---------------------------------------------------------------------------

export interface FunnelStep {
  name: string;
  count: number;
  conv_from_start: number;
  conv_from_prev: number;
}

export interface FunnelResult {
  total_entered: number;
  steps: FunnelStep[];
}

export interface SavedFunnel {
  id: string;
  app_id: string;
  name: string;
  description?: string | null;
  steps: string[];
  created_by_name?: string | null;
  created_at: string;
  updated_at: string;
}

// ---------------------------------------------------------------------------
// Journeys (step-indexed Sankey)
// ---------------------------------------------------------------------------

export interface JourneyNode {
  step: number;
  event: string;
  count: number;
}

export interface JourneyLink {
  from_step: number;
  from_event: string;
  to_event: string;
  count: number;
}

export interface Journey {
  depth: number;
  nodes: JourneyNode[];
  links: JourneyLink[];
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

export type TransactionOp = 'navigation' | 'http' | 'resource' | 'screen_load' | 'custom' | string;

export interface PerfSummaryRow {
  name: string;
  op: string;
  count: number;
  p50: number;
  p75: number;
  p95: number;
  p99: number;
  avg: number;
  error_rate: number;
}

export interface PerfSeriesPoint {
  bucket: string;
  p50: number;
  p95: number;
  throughput: number;
}

export interface PersonProfile {
  distinct_id: string;
  // `PersonRow`, not a raw persisted-row type — see the backend's
  // `repo::get_event_user` doc comment (backend/crates/sauron-db/src/repo.rs).
  // `first_seen`/`last_seen` are environment-scoped, matching the events/
  // errors lists on the same page, not the app-wide identity record.
  user: PersonRow | null;
  events: AnalyticsEvent[];
  errors: ErrorEvent[];
}

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------
// Audience & session analytics
// ---------------------------------------------------------------------------

export interface UserStats {
  total_users: number;
  active_in_range: number;
  new_in_range: number;
  dau: number;
  wau: number;
  mau: number;
  avg_session_ms: number;
  median_session_ms: number;
}

export interface UserSeriesPoint {
  bucket: string;
  active: number;
  new_users: number;
}

export interface UsersAnalytics {
  stats: UserStats;
  stickiness: number;
  series: UserSeriesPoint[];
}

export interface SessionStats {
  sessions: number;
  crashed: number;
  avg_session_ms: number;
  median_session_ms: number;
}

export interface SeriesAvgPoint {
  bucket: string;
  avg_ms: number;
}

export interface HistoBucket {
  bucket: string;
  count: number;
}

export interface SessionsAnalytics {
  stats: SessionStats;
  duration_series: SeriesAvgPoint[];
  duration_histogram: HistoBucket[];
}

// ---------------------------------------------------------------------------

export interface ApiErrorEnvelope {
  error: {
    code: string;
    message: string;
  };
}

export interface NormalizedError {
  status: number;
  code: string;
  message: string;
  isNetwork: boolean;
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

export interface ScreenRow {
  screen: string;
  views: number;
  events: number;
  exceptions: number;
  users: number;
  avg_dwell_ms: number;
}

export interface ScreenStats extends ScreenRow {
  total_dwell_ms: number;
}

/**
 * The screen detail header. Stats only — the row lists come from the four
 * paged section endpoints (see `api/screen-sections.ts`).
 *
 * `recent_events` / `recent_exceptions` were removed from both this type and
 * the wire response once the collapsible sections replaced the static cards:
 * they were 20 events plus 20 full `ErrorEvent`s (stacktrace, breadcrumbs)
 * fetched, permission-gated and serialized on every page load with no reader.
 */
export interface ScreenDetail {
  stats: ScreenStats;
}

/**
 * One user who produced signal on a screen, as the Users section of the screen
 * detail page lists them.
 *
 * **Every count here is scoped to that one screen**, which is why each carries
 * an `_on_screen` suffix rather than matching `PersonRow`'s bare
 * `events_count`/`errors_count`. The two are not interchangeable: a person with
 * 900 lifetime events may have 3 on this screen, and rendering the lifetime
 * number under a per-screen heading is the misreading the suffix exists to
 * prevent. The lifetime totals live on the person's own page, behind the row's
 * link to `/persons/:distinct_id`.
 */
export interface ScreenUserRow {
  distinct_id: string;
  properties: Record<string, unknown> | null;
  views_on_screen: number;
  events_on_screen: number;
  exceptions_on_screen: number;
  first_seen_on_screen: string;
  last_seen_on_screen: string;
}

/**
 * One device seen on a screen. Same `_on_screen` rule as {@link ScreenUserRow}.
 *
 * The descriptive fields (`family` … `browser`) are joined from the device
 * record and are lifetime facts, not per-screen ones — they describe the
 * device itself, so no suffix applies. They are nullable because the join is a
 * LEFT join: a device can appear in the event stream before its inventory row
 * is written.
 */
export interface ScreenDeviceRow {
  device_key: string;
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
  arch: string | null;
  browser: string | null;
  views_on_screen: number;
  events_on_screen: number;
  exceptions_on_screen: number;
  first_seen_on_screen: string;
  last_seen_on_screen: string;
}

// ---------------------------------------------------------------------------
// Workflows — named, bounded spans of activity an app can declare via
// startWorkflow()/endWorkflow(). Entirely optional: an app that never calls
// them has no rows anywhere below.
// ---------------------------------------------------------------------------

/** One row per workflow name — the rollup `GET /v1/apps/{app_id}/workflows` returns. */
export interface WorkflowRow {
  name: string;
  started: number;
  completed: number;
  cancelled: number;
  abandoned: number;
  active: number;
  unique_users: number;
  /** `null` when no run in the window has finished yet — duration describes finished runs only. */
  median_duration_ms: number | null;
  p95_duration_ms: number | null;
  last_seen: string;
}

/**
 * The effective status of a workflow run, derived server-side at read time —
 * never stored. An `active` run with no activity for 30 minutes reads as
 * `abandoned`; a later stamped event revives it. Never compute staleness
 * client-side.
 */
export type WorkflowStatus = 'active' | 'completed' | 'cancelled' | 'abandoned';

/** One individual run of a workflow name — a row from `.../workflows/{name}/runs`. */
export interface WorkflowRun {
  workflow_id: string;
  session_id: string | null;
  distinct_id: string | null;
  status: WorkflowStatus;
  started_at: string;
  ended_at: string | null;
  duration_ms: number | null;
  events_count: number;
  errors_count: number;
}

/** One workflow span within a session — feeds the session timeline lane. */
export interface WorkflowSpan {
  workflow_id: string;
  name: string;
  status: WorkflowStatus;
  started_at: string;
  ended_at: string | null;
}

// ---------------------------------------------------------------------------
// Uptime Monitoring
// ---------------------------------------------------------------------------

export type MonitorStatus = 'unknown' | 'up' | 'down' | 'paused';

export interface MonitorListItem {
  id: string;
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  status: MonitorStatus;
  enabled: boolean;
  last_response_time_ms: number | null;
  last_checked_at: string | null;
  uptime_24h: number | null;
}

export interface Monitor {
  id: string;
  project_id: string;
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  method: string;
  /** Probe settings minus `headers` — see `probe_header_names`. */
  config: Record<string, unknown>;
  interval_seconds: number;
  timeout_ms: number;
  failure_threshold: number;
  recovery_threshold: number;
  /**
   * The webhook URL itself is never sent: it is a bearer-equivalent capability
   * URL and `monitor:read` (which Viewer holds) gates this payload, so the API
   * redacts it at the serializer and exposes only its existence. Same for the
   * probe's request headers — names only, never values, because they carry
   * `Authorization`/`X-Api-Key` straight into the outbound probe. To change
   * either, PATCH the new value; there is nothing to pre-fill.
   */
  has_webhook: boolean;
  probe_header_names: string[];
  enabled: boolean;
  status: MonitorStatus;
  last_checked_at: string | null;
  next_check_at: string;
  created_at: string;
}

export interface MonitorIncident {
  id: string;
  monitor_id: string;
  started_at: string;
  resolved_at: string | null;
  cause: string;
  last_error: string | null;
}

export interface MonitorDetail {
  monitor: Monitor;
  uptime: { h24: number | null; d7: number | null; d30: number | null };
  incidents: MonitorIncident[];
  // Alert rules pinned to this monitor via alert_rules.monitor_id, which is
  // ON DELETE CASCADE — deleting the monitor deletes these too. Surfaced
  // here so the delete confirmation can disclose it before the delete call.
  pinned_alert_rules: number;
}

export interface MonitorCheck {
  checked_at: string;
  up: boolean;
  response_time_ms: number | null;
  status_code: number | null;
  error: string | null;
}

// ---------------------------------------------------------------------------
// Alerting: notification channels, rules, delivery history
// ---------------------------------------------------------------------------

export type ChannelKind = 'email' | 'slack' | 'discord' | 'matrix' | 'telegram' | 'webhook';

export type TriggerType =
  | 'monitor_down'
  | 'monitor_up'
  | 'issue_new'
  | 'issue_regression'
  | 'error_threshold'
  | 'error_spike'
  | 'event_threshold'
  | 'perf_degradation';

export type AlertSeverity = 'info' | 'warning' | 'critical';

export interface NotificationChannel {
  id: string;
  org_id: string;
  name: string;
  kind: ChannelKind;
  /**
   * A REDACTED projection of the channel's settings, not the stored config.
   *
   * The stored value is encrypted at rest and is a credential in its own right
   * for some kinds — a generic webhook's `url` and its arbitrary `headers` map
   * (where an `Authorization: Bearer …` lives), and a Slack/Discord
   * `webhook_url`, which *is* the credential. The API returns presence flags
   * and a path-less origin in their place (`has_url`, `url_origin`,
   * `header_names`, `has_webhook_url`) alongside the genuinely non-secret
   * fields (SMTP host/port/from/to, Matrix homeserver/room, Telegram chat id).
   *
   * `null` when the server could not decrypt the row — see `config_error`.
   */
  config: Record<string, unknown> | null;
  /**
   * The row's stored payload could not be decrypted, i.e. `NOTIFY_SECRET_KEY`
   * no longer matches the key it was written with. Reads degrade per row rather
   * than failing the page, so one broken channel can still be deleted; writes to
   * it are refused outright.
   */
  config_error: boolean;
  enabled: boolean;
  /** Whether a secret bundle is stored. The secret itself is never returned. */
  has_secret: boolean;
  created_at: string;
  updated_at: string;
}

export interface AlertRule {
  id: string;
  org_id: string;
  project_id: string | null;
  app_id: string | null;
  /** Set only for `monitor_down`/`monitor_up` rules pinned to one monitor; null means every monitor in scope. */
  monitor_id: string | null;
  name: string;
  trigger_type: TriggerType;
  enabled: boolean;
  conditions: AlertConditions;
  severity: AlertSeverity;
  throttle_seconds: number;
  message_template: string | null;
  last_evaluated_at: string | null;
  created_at: string;
  updated_at: string;
  channel_ids: string[];
}

export interface AlertConditions {
  comparator?: 'gte' | 'gt' | 'lte' | 'lt' | 'eq';
  threshold?: number;
  window_seconds?: number;
  spike_factor?: number;
  metric?: string;
  filters?: {
    level?: string;
    environment?: string;
    event_name?: string;
    tag_key?: string;
    tag_value?: string;
    op?: string;
  };
}

export interface AlertEvent {
  id: string;
  org_id: string;
  rule_id: string | null;
  channel_id: string | null;
  trigger_type: string;
  dedup_key: string;
  status: 'sent' | 'failed' | 'throttled' | 'skipped';
  title: string;
  body: string;
  error: string | null;
  attempts: number;
  created_at: string;
}

export interface AlertMeta {
  channel_kinds: ChannelKind[];
  trigger_types: { key: TriggerType; metric: boolean }[];
  comparators: string[];
  severities: AlertSeverity[];
  metrics: string[];
  template_vars: Record<string, string[]>;
  subscription_kinds: SubscriptionKindMeta[];
}

export type SubscriptionKind =
  | 'uptime'
  | 'error_spike'
  | 'error_new_issue'
  | 'error_regression';

export type SubscriptionDelivery = 'immediate' | 'hourly' | 'daily';

export interface SubscriptionConditions {
  window_seconds: number;
  factor: number;
  min_count: number;
  level: string | null;
}

export interface NotificationSubscription {
  id: string;
  scope_type: 'project' | 'app';
  scope_id: string;
  /** Best effort: `scope_id` has no foreign key, so a row can outlive its target. */
  scope_name: string | null;
  project_id: string | null;
  kind: SubscriptionKind;
  enabled: boolean;
  disabled_reason: 'unsubscribed' | 'access_revoked' | null;
  /** CATALOGUE environment ids (`environments.id`), never enrollment ids. */
  environment_ids: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  /** What the user will actually get once the per-hour cap is applied. */
  effective_delivery: SubscriptionDelivery;
  throttle_seconds: number;
  quiet_start_min: number | null;
  quiet_end_min: number | null;
  quiet_tz: string;
  created_at: string;
}

export interface NotificationQueueItem {
  id: string;
  kind: SubscriptionKind;
  severity: AlertSeverity;
  title: string | null;
  body: string | null;
  link: string | null;
  status: string;
  occurred_at: string;
  sent_at: string | null;
}

export interface SubscriptionKindMeta {
  key: SubscriptionKind;
  scope_types: ('project' | 'app')[];
  env_filter: boolean;
  defaults: Partial<SubscriptionConditions>;
  clamps: Record<string, [number, number]>;
}

// ---------------------------------------------------------------------------
// Combined active users (project-scoped)
// ---------------------------------------------------------------------------

export interface ReportWindow {
  from: string;
  to: string;
}

export interface ActiveUserPoint {
  /** A UTC calendar day, `YYYY-MM-DD`. Never a timestamp. */
  day: string;
  active_total: number;
  active_identified: number;
  active_guest: number;
}

export interface SelectionView {
  app_id: string;
  app_name: string;
  /**
   * The filter the server ACTUALLY applied: `all` | `one` | `subset` |
   * `unattributed`. `subset` means the caller's grants reach only some of the
   * app's environments, so the number covers fewer environments than the
   * picker's "All environments" suggests — the page must say so.
   */
  resolved: 'all' | 'one' | 'subset' | 'unattributed';
  environment_ids: string[];
  environment_labels: string[];
}

export interface ActiveUsersReport {
  requested: ReportWindow;
  effective: ReportWindow;
  truncated: boolean;
  /** A full sentence, rendered verbatim. */
  truncation_reason: string | null;
  selections: SelectionView[];
  series: ActiveUserPoint[];
  /** The last COMPLETE UTC day, or null when the window contains only today. */
  latest: ActiveUserPoint | null;
  /**
   * When the numbers were computed. The server serves this report from a
   * ~1h serve-stale cache, so a page that painted instantly can be showing
   * hour-old numbers — the stamp is the disclosure, same contract as the
   * overview's `computed_at`. Optional: absent from reports cached by older
   * server builds.
   */
  computed_at?: string | null;
}

// ---------------------------------------------------------------------------
// PII inspector
// ---------------------------------------------------------------------------

export interface InspectorTrackedKey {
  key: string;
  scope: 'any' | 'top';
}

export interface InspectorPolicy {
  id: string;
  org_id: string;
  target_type: 'project' | 'app' | 'app_env';
  target_id: string;
  enabled: boolean;
  tracked_keys: InspectorTrackedKey[];
  detectors: string[];
  scan_columns: string[] | null;
  rollups: string[];
  window_days: number;
  schedule_enabled: boolean;
  /** 7-bit weekday mask; bit 0 is Sunday, matching Postgres's EXTRACT(DOW). */
  schedule_days: number;
  /** `HH:MM` local wall clock. */
  schedule_time: string;
  schedule_tz: string;
  next_run_at: string | null;
  last_run_at: string | null;
  last_scan_id: string | null;
  last_skip_reason: string;
  created_at: string;
  updated_at: string;
}

export interface InspectorScan {
  id: string;
  policy_id: string;
  org_id: string;
  trigger_type: 'scheduled' | 'manual';
  status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
  coverage: 'full' | 'partial';
  coverage_note: string;
  window_from: string;
  window_to: string;
  units_total: number;
  units_done: number;
  rows_scanned: number;
  findings_count: number;
  findings_reaped_at: string | null;
  attempts: number;
  cancel_requested_at: string | null;
  error: string;
  started_at: string | null;
  finished_at: string | null;
  created_at: string;
}

export interface InspectorFinding {
  id: string;
  scan_id: string;
  org_id: string;
  app_id: string;
  environment_id: string | null;
  env_scope: 'enrollment' | 'unattributed' | 'no_env_column';
  source_table: string;
  source_column: string;
  key_path: string;
  matched_key: string;
  detector: string;
  value_type: string;
  match_count: number;
  match_count_exact: boolean;
  /** Shape-only. NEVER the value — the findings table has no value column. */
  sample_preview: string;
  sample_row_id: string | null;
  sample_occurred_at: string | null;
  partition_kind: 'ranged' | 'default' | 'rollup';
  first_seen_at: string | null;
  last_seen_at: string | null;
  created_at: string;
}

export interface InspectorMaskAction {
  id: string;
  org_id: string;
  app_id: string;
  kind: 'preview' | 'mask';
  finding_id: string | null;
  scan_id: string | null;
  targets: { table: string; column: string; path: string }[];
  status:
    | 'preview'
    | 'previewed'
    | 'pending'
    | 'running'
    | 'cancelling'
    | 'done'
    | 'failed'
    | 'cancelled';
  requested_by_email: string;
  cancelled_by_email: string;
  cancelled_at: string | null;
  requested_at: string;
  previewed_at: string | null;
  confirmed_at: string | null;
  started_at: string | null;
  finished_at: string | null;
  confirm_source: string;
  estimated_rows: number;
  rows_scanned: number;
  rows_masked: number;
  cold_rows_skipped: number;
  cold_boundary_at: string | null;
  phase: string;
  vacuum_advised: boolean;
  error: string;
}

export interface InspectorMaskedKey {
  id: string;
  app_id: string;
  target_table: string;
  target_column: string;
  json_path: string;
  created_at: string;
  source_action_id: string | null;
}

export interface EffectivePolicy {
  policy: InspectorPolicy | null;
  masked_keys: InspectorMaskedKey[];
  /** Read from the server, never hardcoded — the UI states this number. */
  enforcement_latency_secs: number;
  hot_window_days: number;
}

export interface FindingsPage {
  findings: InspectorFinding[];
  coverage: 'full' | 'partial';
  coverage_note: string;
  detection_caveat: string;
}

export interface RevealResult {
  path: string;
  value: unknown;
  type: string;
}

export interface MaskPreviewStart {
  action: InspectorMaskAction;
  app_slug: string;
  preview_ttl_secs: number;
  mask_max_rows: number;
  enforcement_latency_secs: number;
}

/** GET /v1/apps/{id}/rollups/status — the freshness chip's source. */
export interface RollupStatus {
  ready: boolean;
  as_of: string | null;
  sessions_as_of: string | null;
}
