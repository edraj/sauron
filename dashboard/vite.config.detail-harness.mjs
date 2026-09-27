import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the detail pages' section-by-section loading.
 *
 * Each detail page reads its record in one request and every heavy block in
 * another (`…/summary`, `…/series`, `…/latest-event`, `…/sessions`, …), so the
 * header can paint off a keyed lookup while the expensive cards show a
 * skeleton. What that looks like depends entirely on the sections answering at
 * DIFFERENT speeds, which no local database does: there, all of them land
 * within a millisecond and the page appears whole.
 *
 * So the stub below answers the light section at once and holds the heavy ones
 * back by `?slow=<ms>` (read off the page URL via `Referer`), and can fail one
 * of them with `?fail=<section>`.
 *
 * **Stubbed at the HTTP layer, not the module layer**, for the reason recorded
 * in `vite.config.search-harness.mjs`: answering the request keeps axios, the
 * interceptors, `CachedView`, the API client and the pages as the real code.
 */
const PORT = 3047;

const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'web', ingest_enabled: true, platform: null, store_environment_id: null };

const NOW = Date.parse('2026-09-27T10:00:00Z');
const ago = (ms) => new Date(NOW - ms).toISOString();
const HOUR = 3600_000;
const DAY = 24 * HOUR;

// --- issue -------------------------------------------------------------------

const ISSUE = {
  id: 'issue-1',
  app_id: 'app1',
  fingerprint: 'f3a9c1d27b6e4a10f3a9c1d27b6e4a10',
  type: 'TypeError',
  title: "TypeError: Cannot read properties of undefined (reading 'total')",
  culprit: 'CheckoutSummary.render',
  level: 'error',
  status: 'unresolved',
  first_seen: ago(12 * DAY),
  last_seen: ago(2 * HOUR),
  times_seen: 1843,
  users_seen: 212,
  assignee_id: null,
  created_at: ago(12 * DAY),
  updated_at: ago(2 * HOUR),
};

/** What the record route serves; replaced by a `PATCH` of the status. */
let issue = ISSUE;

const SERIES = Array.from({ length: 30 }, (_, i) => ({
  bucket: ago((29 - i) * DAY),
  count: Math.round(40 + 35 * Math.sin(i / 3) + (i % 7 === 0 ? 60 : 0)),
}));

const LATEST_EVENT = {
  id: 'err-1',
  app_id: 'app1',
  issue_id: 'issue-1',
  level: 'error',
  message: null,
  exception_type: 'TypeError',
  exception_value: "Cannot read properties of undefined (reading 'total')",
  release: 'web@1.4.2',
  screen: 'Checkout',
  distinct_id: 'ana@example.com',
  session_id: 'sess-1',
  device_key: 'pixel-8/android-15',
  occurred_at: ago(2 * HOUR),
  symbolication_status: 'symbolicated',
  debug_meta: null,
  stacktrace: [
    { function: 'render', filename: 'src/checkout/CheckoutSummary.tsx', lineno: 88, colno: 21, in_app: true },
    { function: 'commitRoot', filename: 'node_modules/react-dom/index.js', lineno: 2210, colno: 9, in_app: false },
  ],
  stacktrace_symbolicated: null,
  breadcrumbs: [
    { timestamp: ago(2 * HOUR + 4000), category: 'navigation', message: '/cart → /checkout', level: 'info' },
    { timestamp: ago(2 * HOUR + 1500), category: 'http', message: 'GET /api/cart 200', level: 'info' },
  ],
  context: { os: { name: 'Android', version: '15' }, device: { family: 'Pixel', model: 'Pixel 8' } },
  contexts: { app: { build: '412', locale: 'en-US' } },
  extra: { cart_items: 3 },
  tags: { release: 'web@1.4.2', environment: 'production', region: 'eu-west-1' },
  event_user: { email: 'ana@example.com' },
};

// --- device ------------------------------------------------------------------

const DEVICE = {
  id: 'dev-1',
  device_key: 'pixel-8/android-15',
  family: 'Pixel',
  model: 'Pixel 8',
  os_name: 'Android',
  os_version: '15',
  arch: 'arm64',
  browser: null,
  last_distinct_id: 'ana@example.com',
  first_seen: ago(40 * DAY),
  last_seen: ago(HOUR),
  events_count: 9120,
  errors_count: 37,
  sessions_count: 4,
};

const DEVICE_SESSIONS = Array.from({ length: 4 }, (_, i) => ({
  id: `s-${i}`,
  app_id: 'app1',
  session_id: `sess-${i + 1}`,
  distinct_id: 'ana@example.com',
  device_key: DEVICE.device_key,
  started_at: ago((i + 1) * DAY),
  last_event_at: ago((i + 1) * DAY - 14 * 60_000),
  events_count: 120 - i * 17,
  errors_count: i === 1 ? 3 : 0,
  context: null,
  release: 'web@1.4.2',
  environment_id: null,
  ip_address: null,
  created_at: ago((i + 1) * DAY),
  updated_at: ago((i + 1) * DAY),
}));

const DEVICE_ERRORS = [
  { ...LATEST_EVENT, id: 'err-d1', occurred_at: ago(3 * HOUR) },
  { ...LATEST_EVENT, id: 'err-d2', exception_type: 'RangeError', exception_value: 'Invalid array length', occurred_at: ago(2 * DAY) },
];

const DEVICE_PERF = [
  { name: '/checkout', op: 'navigation', count: 310, p50: 420, p75: 610, p95: 1480, p99: 2900, avg: 530, error_rate: 0.01 },
  { name: 'GET /api/cart', op: 'http', count: 1204, p50: 88, p75: 130, p95: 340, p99: 910, avg: 112, error_rate: 0 },
];

// --- monitor -----------------------------------------------------------------

const MONITOR = {
  id: 'mon-1',
  project_id: 'proj1',
  name: 'Checkout API',
  kind: 'http',
  target: 'https://api.example.com/health',
  method: 'GET',
  config: { expected_status: 200 },
  interval_seconds: 60,
  timeout_ms: 10000,
  failure_threshold: 3,
  recovery_threshold: 1,
  has_webhook: true,
  probe_header_names: ['Authorization'],
  enabled: true,
  status: 'up',
  last_checked_at: ago(30_000),
  next_check_at: ago(-30_000),
  created_at: ago(90 * DAY),
};

const MONITOR_CHECKS = Array.from({ length: 80 }, (_, i) => ({
  checked_at: ago(i * 60_000),
  up: i !== 12 && i !== 13,
  status_code: i === 12 || i === 13 ? 503 : 200,
  response_time_ms: 90 + ((i * 37) % 160),
  error: i === 12 || i === 13 ? 'upstream returned 503' : null,
}));

const MONITOR_INCIDENTS = [
  { id: 'inc-1', monitor_id: 'mon-1', started_at: ago(13 * 60_000), resolved_at: ago(11 * 60_000), cause: 'status 503', last_error: 'upstream returned 503' },
  { id: 'inc-2', monitor_id: 'mon-1', started_at: ago(9 * DAY), resolved_at: ago(9 * DAY - 25 * 60_000), cause: 'timeout', last_error: null },
];

// --- plumbing ----------------------------------------------------------------

function pageParam(req, name) {
  try {
    return new URL(req.headers.referer ?? '').searchParams.get(name);
  } catch {
    return null;
  }
}

function json(res, body, status = 200) {
  res.statusCode = status;
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

/**
 * Answer a HEAVY section: after `?slow=` ms, and as a 500 when `?fail=` names
 * it. The light sections never come through here.
 */
function heavy(req, res, section, body) {
  const slow = Number(pageParam(req, 'slow'));
  const ms = Number.isFinite(slow) && slow > 0 ? Math.min(slow, 30000) : 0;
  const fail = pageParam(req, 'fail') === section;
  const answer = () =>
    fail
      ? json(res, { error: { code: 'internal', message: `the ${section} section failed (harness)` } }, 500)
      : json(res, body);
  if (ms > 0) setTimeout(answer, ms);
  else answer();
}

function stubApi() {
  const root = fileURLToPath(new URL('./detail-harness', import.meta.url));
  return {
    name: 'detail-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path] = (req.url ?? '').split('?');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: `http://localhost:${PORT}`,
              ingestBaseUrl: `http://localhost:${PORT}`,
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // Wide grants: permission gating is not what this harness verifies.
          return json(res, {
            permissions: ['*'],
            grants: [{ scope_type: 'org', scope_id: 'org1', permissions: ['issue:read', 'issue:write', 'event:read', 'monitor:read', 'monitor:write'] }],
          });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);

        // --- issue: the record is light, the other two are not ---------------
        if (path === '/v1/apps/app1/issues/issue-1/summary') return json(res, issue);
        // Triage. STATEFUL on purpose: the page refetches the record after a
        // write, and a stub that kept answering `unresolved` would undo the
        // change on screen and read as the write having failed.
        // `?fail=status` answers 500, which is how the rollback gets checked.
        if (path === '/v1/apps/app1/issues/issue-1' && req.method === 'PATCH') {
          let raw = '';
          req.on('data', (chunk) => (raw += chunk));
          req.on('end', () => {
            if (pageParam(req, 'fail') === 'status') {
              return json(res, { error: { code: 'internal', message: 'the status write failed (harness)' } }, 500);
            }
            let status;
            try {
              status = JSON.parse(raw).status;
            } catch {
              status = undefined;
            }
            if (!['unresolved', 'resolved', 'ignored'].includes(status)) {
              return json(res, { error: { code: 'bad_request', message: 'status must be unresolved, resolved, or ignored' } }, 400);
            }
            issue = { ...issue, status, updated_at: new Date().toISOString() };
            json(res, issue);
          });
          return;
        }
        if (path === '/v1/apps/app1/issues/issue-1/series') {
          return heavy(req, res, 'series', { series: SERIES });
        }
        if (path === '/v1/apps/app1/issues/issue-1/latest-event') {
          return heavy(req, res, 'latest-event', { latest_event: LATEST_EVENT });
        }
        if (path === '/v1/apps/app1/issues/issue-1/events') {
          return heavy(req, res, 'events', {
            data: DEVICE_ERRORS,
            total: DEVICE_ERRORS.length,
            total_is_capped: false,
            next_cursor: null,
            clamped: null,
          });
        }
        if (path === '/v1/apps/app1/issues/issue-1/events/stats') {
          return heavy(req, res, 'events', { events: 1843, users: 212, sessions: 960 });
        }

        // --- device -----------------------------------------------------------
        if (path === '/v1/apps/app1/device/summary') return json(res, { device: DEVICE });
        if (path === '/v1/apps/app1/device/sessions') {
          return heavy(req, res, 'sessions', { sessions: DEVICE_SESSIONS });
        }
        if (path === '/v1/apps/app1/device/errors') {
          return heavy(req, res, 'errors', { errors: DEVICE_ERRORS });
        }
        if (path === '/v1/apps/app1/device/perf') {
          return heavy(req, res, 'perf', { perf: DEVICE_PERF });
        }

        // --- monitor ----------------------------------------------------------
        if (path === '/v1/monitors/mon-1/summary') {
          return json(res, { monitor: MONITOR, pinned_alert_rules: 2 });
        }
        if (path === '/v1/monitors/mon-1/uptime') {
          return heavy(req, res, 'uptime', { uptime: { h24: 99.86, d7: 99.95, d30: 97.4 } });
        }
        if (path === '/v1/monitors/mon-1/incidents') {
          return heavy(req, res, 'incidents', MONITOR_INCIDENTS);
        }
        if (path === '/v1/monitors/mon-1/checks') {
          return heavy(req, res, 'checks', MONITOR_CHECKS);
        }

        // Anything else the pages ask for. Logged rather than silently
        // answered, so a fixture this harness forgot shows up in the terminal
        // instead of as an empty card.
        if (path?.startsWith('/v1/')) {
          console.log(`[detail-harness] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./detail-harness', import.meta.url)),
  server: { port: PORT, strictPort: true },
});
