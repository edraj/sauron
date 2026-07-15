// TypeScript interfaces mirroring the Sauron API contract.
// Shapes were verified against the live backend at http://localhost:8090.
// Hierarchy: Org → Project (grouping) → App (holds the DSN). Signals live under apps.

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
  public_key: string;
  ingest_enabled: boolean;
  // Retained by the API for backwards compat; not surfaced in the UI.
  platform?: string | null;
  created_at: string;
  updated_at: string;
}

export interface Environment {
  id: string;
  app_id: string;
  name: string;
  created_at: string;
}

export interface FirstEventStatus {
  received: boolean;
  errors: number;
  events: number;
}

// ---------------------------------------------------------------------------
// Access control (RBAC)
// ---------------------------------------------------------------------------

export type ScopeType = 'org' | 'project' | 'app';

// Known permission strings. `(string & {})` keeps autocomplete while tolerating
// any future permission the backend introduces.
export type Permission =
  | 'issue:read'
  | 'issue:write'
  | 'event:read'
  | 'funnel:write'
  | 'app:read'
  | 'app:create'
  | 'app:update'
  | 'app:delete'
  | 'app:rotate_key'
  | 'project:read'
  | 'project:create'
  | 'project:update'
  | 'project:delete'
  | 'member:read'
  | 'member:manage'
  | 'role:manage'
  | 'org:manage'
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
}

export interface CreateGrantPayload {
  email: string;
  role_id: string;
  scope_type: ScopeType;
  scope_id: string;
}

export interface CreateRolePayload {
  name: string;
  description?: string;
  permissions: Permission[];
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
  stacktrace: Frame[];
  breadcrumbs: Breadcrumb[];
  context: Record<string, unknown> | null;
  tags: Record<string, unknown> | null;
  release: string | null;
  distinct_id: string | null;
  event_user: EventUser | null;
  sdk: unknown;
  ip_address: string | null;
  screen?: string | null;
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
}

export interface IssueDetail extends Issue {
  latest_event: ErrorEvent | null;
  series: SeriesPoint[];
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

export interface TopEvent {
  name: string;
  count: number;
}

// The persisted person record returned by the persons endpoint.
export interface Person {
  id: string;
  app_id: string;
  distinct_id: string;
  properties: Record<string, unknown> | null;
  first_seen: string;
  last_seen: string;
  created_at?: string;
  updated_at?: string;
}

export interface AnalyticsEvent {
  id: string;
  app_id?: string;
  environment_id?: string | null;
  name: string;
  distinct_id: string;
  properties: Record<string, unknown> | null;
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

export interface Device {
  id: string;
  app_id: string;
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
  created_at: string;
  updated_at: string;
}

export interface DeviceDetail {
  device: Device;
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
  crash_free_sessions: number;
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
  user: Person | null;
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

export interface ScreenDetail {
  stats: ScreenStats;
  recent_events: AnalyticsEvent[];
  recent_exceptions: ErrorEvent[];
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
  config: Record<string, unknown>;
  interval_seconds: number;
  timeout_ms: number;
  failure_threshold: number;
  recovery_threshold: number;
  webhook_url: string | null;
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
}

export interface MonitorCheck {
  checked_at: string;
  up: boolean;
  response_time_ms: number | null;
  status_code: number | null;
  error: string | null;
}
