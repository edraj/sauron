import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the list toolbars, the sessions filter chips, and the
 * timeline's issue quick view.
 *
 * Exists because none of the four changes can fail in a way the static gates
 * see. `svelte-check` is happy with a search box that queries nothing, a chip
 * whose encoded value axios drops before it reaches the wire, a modal that
 * mounts with every field empty, and a duplicate card removed from the wrong
 * column. All four are visible here in one pass, and the request log below
 * makes the chip's wire format checkable rather than assumed.
 *
 * **Stubbed at the HTTP layer, not the module layer**, as
 * `vite.config.timeline-filter-harness.mjs` records: a `resolve.alias` on
 * `lib/api/client` matches the import specifier before resolution, so it misses
 * the relative imports and the real client goes to the :8090 pin in the
 * committed `static/config.js`. Answering the request keeps axios, the
 * interceptors, `CachedView` and the pages themselves as the real code.
 */
/** Every permission the dashboard gates on; this harness verifies layout, not
    RBAC. Mirrors the `Permission` union in `src/lib/models/index.ts`. */
const ALL_PERMS = [
  'issue:read', 'issue:write', 'event:read', 'funnel:write', 'artifact:write', 'source:read',
  'monitor:read', 'monitor:write', 'app:read', 'app:create', 'app:update', 'app:delete',
  'env:read', 'env:create', 'env:update', 'env:delete', 'env:rotate_key',
  'project:read', 'project:create', 'project:update', 'project:delete',
  'member:read', 'member:manage', 'member:credential', 'role:manage', 'org:manage',
  'alert:read', 'alert:write', 'pii:read', 'pii:manage',
];

const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'mobile', ingest_enabled: true, platform: 'android', store_environment_id: null };

const DISTINCT = 'EQevrjcvy+sa6eIl5g7j4l5gwNIZuvt3YnQavcrALF4=';
const DEVICE = 'a50fd201-2683-4361-af8a-5d23e4ba1888';
const SESSION_ID = 'sess-harness-01';

/** Every field the quick view claims to show, so an empty section is a defect
    rather than an empty fixture. */
const ERROR_EVENT = {
  id: 'err-1',
  app_id: 'app1',
  environment_id: null,
  issue_id: 'issue-1',
  fingerprint: 'fp-harness-0001',
  level: 'error',
  message: null,
  exception_type: 'DioException',
  exception_value: 'This email address is not valid or cannot receive emails',
  title: null,
  culprit: 'AuthRepository.register (auth_repository.dart)',
  stacktrace: [
    { function: 'AuthRepository.register', filename: 'lib/data/auth_repository.dart', lineno: 128, colno: 7, in_app: true },
    { function: 'RegisterCubit.submit', filename: 'lib/logic/register_cubit.dart', lineno: 64, colno: 12, in_app: true },
    { function: '_rootRunUnary', filename: 'dart:async/zone.dart', lineno: 1407, colno: 47, in_app: false },
  ],
  breadcrumbs: [],
  context: {
    app: { build: '31099', version: '3.1.21' },
    device: { arch: 'arm64-v8a', device_id: DEVICE, family: 'Xiaomi', model: '25062PC34G' },
    device_key: DEVICE,
    os: { name: 'Android', version: '16' },
    runtime: { name: 'Dart', version: '3.12' },
    user: { email: null, id: DISTINCT, ip_address: null, traits: { signUpMethod: 'Started Join oodi' }, username: null },
  },
  tags: {
    app_version: '3.1.21',
    build_mode: 'release',
    build_number: '31099',
    environment: 'PROD',
    os: 'android',
    platform: 'mobile',
  },
  contexts: { app_env: { name: 'PROD' } },
  extra: {
    app_version: '3.1.21',
    code: 10007,
    context: null,
    description: 'somethingWentWrong',
    env: 'PROD',
    os_name: 'android',
    possible_waf_issue: false,
    title: 'This email address is not valid or cannot receive emails',
    type: 'client_error',
  },
  release: '3.1.21',
  distinct_id: DISTINCT,
  event_user: { id: DISTINCT, email: 'ana@example.com', username: null, ip_address: null, traits: null },
  sdk: { name: 'sauron.flutter', version: '1.4.0' },
  ip_address: null,
  screen: 'HomeRoute',
  session_id: SESSION_ID,
  device_key: DEVICE,
  occurred_at: '2026-08-16T10:00:19.400Z',
  received_at: '2026-08-16T10:00:19.900Z',
  stacktrace_symbolicated: null,
  symbolication_status: 'symbolicated',
  debug_meta: null,
};

const TIMELINE = [
  {
    kind: 'event',
    at: '2026-08-16T10:00:00.000Z',
    event: { id: 'ev-1', name: '$screen', distinct_id: DISTINCT, session_id: SESSION_ID, properties: { screen: 'HomeRoute' }, screen: 'HomeRoute', occurred_at: '2026-08-16T10:00:00.000Z' },
  },
  { kind: 'error', at: ERROR_EVENT.occurred_at, error: ERROR_EVENT },
];

const SESSION_ROW = {
  id: 'sess-row-1',
  app_id: 'app1',
  session_id: SESSION_ID,
  distinct_id: DISTINCT,
  device_key: DEVICE,
  started_at: '2026-08-16T10:00:00.000Z',
  last_event_at: '2026-08-16T10:04:30.000Z',
  events_count: 7,
  errors_count: 1,
  context: { os: { name: 'Android', version: '16' }, device: { family: 'Xiaomi', model: '25062PC34G' } },
  release: '3.1.21',
  environment_id: null,
  ip_address: null,
  created_at: '2026-08-16T10:00:00.000Z',
  updated_at: '2026-08-16T10:04:30.000Z',
};

function sessionRows(n) {
  return Array.from({ length: n }, (_, i) => ({
    ...SESSION_ROW,
    id: `sess-row-${i + 1}`,
    session_id: i === 0 ? SESSION_ID : `sess-harness-${String(i + 1).padStart(2, '0')}`,
    events_count: 3 + i,
    errors_count: i % 3 === 0 ? 1 : 0,
  }));
}

const PERSONS = Array.from({ length: 12 }, (_, i) => ({
  distinct_id: i === 0 ? DISTINCT : `user-${String(i + 1).padStart(3, '0')}`,
  properties: { plan: i % 2 === 0 ? 'pro' : 'free' },
  first_seen: '2026-07-20T09:00:00.000Z',
  last_seen: '2026-08-16T10:04:30.000Z',
  events_count: 40 - i,
  errors_count: i % 4,
  sessions_count: 5 + i,
}));

const SESSIONS_SUMMARY = {
  stats: { sessions: 232900, crashed: 160000, avg_session_ms: 1330000, median_session_ms: 33000 },
  duration_series: Array.from({ length: 14 }, (_, i) => ({
    bucket: `2026-08-${String(i + 3).padStart(2, '0')}T00:00:00Z`,
    avg_ms: 40000 + i * 9000,
  })),
  duration_histogram: [
    { bucket: '<10s', count: 70647 },
    { bucket: '10-60s', count: 69783 },
    { bucket: '1-5m', count: 53106 },
    { bucket: '5-30m', count: 26268 },
    { bucket: '30m+', count: 13078 },
  ],
};

const USERS_SUMMARY = {
  stats: { total_users: 48210, active_in_range: 12904, new_in_range: 3120, dau: 1840, wau: 8210, mau: 12904, avg_session_ms: 1330000, median_session_ms: 33000 },
  stickiness: 0.1426,
  series: Array.from({ length: 14 }, (_, i) => ({
    bucket: `2026-08-${String(i + 3).padStart(2, '0')}T00:00:00Z`,
    active: 900 + i * 60,
    new_users: 120 + i * 5,
  })),
};

const ISSUE = {
  id: 'issue-1',
  app_id: 'app1',
  fingerprint: 'fp-harness-0001',
  type: 'DioException',
  title: 'DioException: This email address is not valid or cannot receive emails',
  culprit: 'AuthRepository.register (auth_repository.dart)',
  level: 'error',
  status: 'unresolved',
  first_seen: '2026-08-01T08:00:00.000Z',
  last_seen: '2026-08-16T10:00:19.400Z',
  times_seen: 4210,
  users_seen: 890,
  assignee_id: null,
  created_at: '2026-08-01T08:00:00.000Z',
  updated_at: '2026-08-16T10:00:19.400Z',
};

/** The sessions schema, so the search box builds its own placeholder and the
    autocomplete has dimensions to offer — the chips must agree with these. */
const SCHEMAS = {
  sessions: {
    resource: 'sessions',
    variables: [{ prefix: '@context', description: 'Device/runtime context', chainable: true }],
    dimensions: [
      { name: 'session', type: 'string', ops: ['=', '!=', 'contains'] },
      { name: 'distinctId', type: 'string', ops: ['=', '!=', 'contains'], aliases: ['distinct_id'] },
      { name: 'deviceKey', type: 'string', ops: ['=', '!='] },
      { name: 'release', type: 'string', ops: ['=', '!=', 'contains'] },
      { name: 'eventsCount', type: 'integer', ops: ['=', '>', '<'] },
      { name: 'errorsCount', type: 'integer', ops: ['=', '>', '<'] },
      { name: 'duration', type: 'duration', ops: ['>', '<'] },
      { name: 'startedAt', type: 'timestamp', ops: ['>', '<'] },
    ],
    available_tags: [],
    available_labels: [],
  },
  // Issue detail's occurrence list asks for this one. Stubbed so its search box
  // builds a real placeholder — and so a 400 in this harness's log always means
  // a defect rather than a fixture nobody wrote.
  occurrences: {
    resource: 'occurrences',
    variables: [{ prefix: '@tag', description: 'Developer tags', chainable: true }],
    dimensions: [
      { name: 'level', type: 'enum', ops: ['=', '!='], options: ['debug', 'info', 'warning', 'error', 'fatal'] },
      { name: 'release', type: 'string', ops: ['=', '!=', 'contains'] },
      { name: 'workflow', type: 'string', ops: ['=', '!=', 'contains'] },
    ],
    available_tags: [{ key: 'environment', sample_values: ['PROD'] }],
    available_labels: [],
  },
};

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

/** Rows the chips are supposed to narrow to, evaluated HERE so the harness can
    show that the parameter arrived — a stub that ignores `filter=` would paint
    a filtered-looking page off an unfiltered response. */
function applyChips(rows, params) {
  const chips = params.getAll('filter');
  if (chips.length === 0) return rows;
  return rows.filter((row) =>
    chips.every((chip) => {
      const [field, op, rawValue] = chip.split(':');
      const value = decodeURIComponent(rawValue ?? '');
      const actual = {
        session: row.session_id,
        distinctId: row.distinct_id,
        deviceKey: row.device_key,
        release: row.release,
        eventsCount: row.events_count,
        errorsCount: row.errors_count,
      }[field];
      if (actual == null) return false;
      switch (op) {
        case 'eq':
          return String(actual) === value;
        case 'neq':
          return String(actual) !== value;
        case 'contains':
          return String(actual).toLowerCase().includes(value.toLowerCase());
        case 'gt':
          return Number(actual) > Number(value);
        case 'lt':
          return Number(actual) < Number(value);
        default:
          return false;
      }
    }),
  );
}

function stubApi() {
  const root = fileURLToPath(new URL('./listui-harness', import.meta.url));
  return {
    name: 'listui-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path, qs] = (req.url ?? '').split('?');
        const params = new URLSearchParams(qs ?? '');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3037',
              ingestBaseUrl: 'http://localhost:3037',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // Wide grants: permission gating is not what this harness verifies.
          // Shape matters — `sessionStore.can()` matches on `scope_type` /
          // `scope_id` and needs the literal permission strings, so a `['*']`
          // grant leaves every page on its "you don't have access" gate.
          return json(res, {
            permissions: ALL_PERMS,
            grants: [{ scope_type: 'org', scope_id: 'org1', permissions: ALL_PERMS }],
          });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);

        if (path === '/v1/apps/app1/search/schema') {
          const ctx = params.get('context') ?? 'sessions';
          const body = SCHEMAS[ctx];
          if (!body) {
            res.statusCode = 400;
            return json(res, { error: { code: 'bad_request', message: `invalid context: ${ctx}` } });
          }
          return json(res, body);
        }

        if (path === '/v1/apps/app1/sessions/summary') return json(res, SESSIONS_SUMMARY);
        if (path === '/v1/apps/app1/users/summary') return json(res, USERS_SUMMARY);

        const sessionDetail = path?.match(/^\/v1\/apps\/app1\/sessions\/(.+)$/);
        if (sessionDetail) {
          return json(res, { session: SESSION_ROW, timeline: TIMELINE });
        }

        if (path === '/v1/apps/app1/sessions') {
          // Logged so the chip's WIRE FORMAT is checkable: repeated `filter=`,
          // not `filters[]=`, is the whole reason `listSessions` builds this
          // query string itself instead of handing the array to axios.
          console.log(`[listui-harness] sessions ${req.url}`);
          const rows = applyChips(sessionRows(12), params);
          return json(res, {
            data: rows,
            total: rows.length,
            total_is_capped: false,
            next_cursor: null,
            clamped: null,
          });
        }

        if (path === '/v1/apps/app1/persons') {
          console.log(`[listui-harness] persons ${req.url}`);
          const search = (params.get('search') ?? '').toLowerCase();
          const rows = search
            ? PERSONS.filter((p) => p.distinct_id.toLowerCase().includes(search))
            : PERSONS;
          return json(res, rows);
        }

        if (path === '/v1/apps/app1/issues/issue-1/events/stats') {
          console.log(`[listui-harness] occurrence stats ${req.url}`);
          return json(res, { events: 4210, users: 890, sessions: 1204, payload_searched: null });
        }
        if (path === '/v1/apps/app1/issues/issue-1/events') {
          return json(res, { data: [ERROR_EVENT], total: 1, total_is_capped: false, next_cursor: null, clamped: null });
        }
        if (path === '/v1/apps/app1/issues/issue-1') {
          return json(res, { ...ISSUE, latest_event: ERROR_EVENT, series: [] });
        }

        // Logged rather than silently answered, so a route this harness forgot
        // shows up in the terminal instead of as an empty page.
        if (path?.startsWith('/v1/')) {
          console.log(`[listui-harness] unstubbed ${req.method} ${path}`);
          return json(res, []);
        }

        if (path === '/' || path === '/index.html') {
          res.setHeader('Content-Type', 'text/html');
          res.end(readFileSync(`${root}/index.html`, 'utf8'));
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig({
  plugins: [stubApi(), svelte()],
  root: fileURLToPath(new URL('./listui-harness', import.meta.url)),
  server: { port: 3037, strictPort: true },
});
