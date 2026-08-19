import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the Arabic locale and RTL layout.
 *
 * Everything the static gates can see about this feature is already green:
 * `svelte-check` type-checks a catalogue whose Arabic is verbatim English,
 * and vitest asserts `t()` returns the right string without ever laying out a
 * page. What neither can see is the part that actually breaks — a sidebar that
 * stays on the left because one `margin-left` survived the sweep, a stack
 * trace whose frames reorder under bidi, a "next page" chevron pointing back,
 * or Arabic rendered in a font with no Arabic glyphs.
 *
 * Stubbed at the HTTP layer, not the module layer, for the reason
 * `vite.config.listui-harness.mjs` records: a `resolve.alias` on `lib/api/*`
 * matches the import specifier before resolution, so it misses relative
 * imports and the real client goes to the :8090 pin in the committed
 * `static/config.js`.
 *
 * `?lang=ar` seeds the locale before the store's constructor runs; see
 * `index.html`.
 */
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
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'flutter', ingest_enabled: true, platform: 'android', store_environment_id: null };

/**
 * An Arabic display name is deliberate: `initials()` used to strip every
 * non-ASCII character and render "?" for exactly this user.
 */
const ME = {
  id: 'user1',
  name: 'محمد العربي',
  email: 'mohamed@example.com',
  last_login_at: '2026-08-19T09:14:00Z',
  is_active: true,
};

/**
 * Sessions covering both table branches — live rows and revoked ones — plus
 * the fixtures that make RTL failures visible rather than plausible:
 * a long LTR user-agent string, an IPv6 address, and one row per revoke
 * reason so the reason column is exercised.
 */
const SESSIONS = [
  { id: 's1', current: true, ip: '196.61.24.7', browser: 'Chrome 128', os: 'Linux', user_agent: 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36', created_at: '2026-08-19T08:00:00Z', last_used_at: '2026-08-19T09:20:00Z', revoked_at: null, revoked_reason: null },
  { id: 's2', current: false, ip: '2001:db8:85a3::8a2e:370:7334', browser: 'Safari 17.5', os: 'iOS 17.5', user_agent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) Safari/605.1.15', created_at: '2026-08-18T11:30:00Z', last_used_at: '2026-08-19T07:05:00Z', revoked_at: null, revoked_reason: null },
  { id: 's3', current: false, ip: '10.0.4.19', browser: 'Safari 17.5', os: 'macOS 14.5', user_agent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) Safari/17.5', created_at: '2026-08-12T09:00:00Z', last_used_at: '2026-08-14T18:42:00Z', revoked_at: '2026-08-15T10:00:00Z', revoked_reason: 'user_revoked_others' },
  { id: 's4', current: false, ip: '172.16.9.2', browser: 'Edge 126', os: 'Windows 10', user_agent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edge/126.0', created_at: '2026-08-01T09:00:00Z', last_used_at: '2026-08-02T08:00:00Z', revoked_at: '2026-08-03T12:00:00Z', revoked_reason: 'reuse' },
];


const ISSUES = [
  { id: 'i1', app_id: 'app1', title: 'NullPointerException', culprit: 'com.example.CartActivity.onCreate', level: 'error', status: 'unresolved', times_seen: 412, users_seen: 87, first_seen: '2026-08-01T09:00:00Z', last_seen: '2026-08-19T08:40:00Z', environment_id: null, exception_type: 'NullPointerException', message: 'Attempt to read from null array' },
  { id: 'i2', app_id: 'app1', title: 'TimeoutException', culprit: 'lib/api/client.dart:88', level: 'fatal', status: 'unresolved', times_seen: 96, users_seen: 31, first_seen: '2026-08-10T11:00:00Z', last_seen: '2026-08-19T07:10:00Z', environment_id: null, exception_type: 'TimeoutException', message: 'Future not completed within 30s' },
  { id: 'i3', app_id: 'app1', title: 'RangeError', culprit: 'lib/widgets/list.dart:204', level: 'warning', status: 'resolved', times_seen: 7, users_seen: 3, first_seen: '2026-08-14T15:00:00Z', last_seen: '2026-08-16T12:00:00Z', environment_id: null, exception_type: 'RangeError', message: 'index out of range' },
];

const SESSION_ROWS = [
  { id: 'sess-1', app_id: 'app1', distinct_id: 'user-9931', device_key: 'a50fd201-2683-4361-af8a-5d23e4ba1888', started_at: '2026-08-19T08:00:00Z', ended_at: '2026-08-19T08:14:00Z', duration_ms: 840000, events_count: 42, errors_count: 2, environment_id: null, release: '3.1.21', device_family: 'Pixel', device_model: 'Pixel 8', os_name: 'Android', os_version: '15' },
  { id: 'sess-2', app_id: 'app1', distinct_id: null, device_key: 'b71ee902-1122-4aa1-9f3a-77c1e4bb2210', started_at: '2026-08-19T06:20:00Z', ended_at: '2026-08-19T06:22:00Z', duration_ms: 120000, events_count: 5, errors_count: 0, environment_id: null, release: '3.1.20', device_family: 'iPhone', device_model: 'iPhone 15', os_name: 'iOS', os_version: '17.5' },
];

const MONITORS = [
  { id: 'm1', project_id: 'proj1', name: 'API health check', kind: 'http', target: 'https://api.example.com/healthz', method: 'GET', interval_seconds: 60, enabled: true, status: 'up', last_checked_at: '2026-08-19T09:19:00Z', last_latency_ms: 84, uptime_24h: 0.9993, webhook_url: null, expected_status: 200, timeout_ms: 5000, failure_threshold: 2, recovery_threshold: 2 },
  { id: 'm2', project_id: 'proj1', name: 'Primary database', kind: 'tcp', target: 'db.example.com:5432', method: null, interval_seconds: 300, enabled: true, status: 'down', last_checked_at: '2026-08-19T09:15:00Z', last_latency_ms: null, uptime_24h: 0.8712, webhook_url: 'https://hooks.example.com/x', expected_status: null, timeout_ms: 5000, failure_threshold: 2, recovery_threshold: 2 },
];

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./i18n-harness', import.meta.url));
  return {
    name: 'i18n-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path, qs] = (req.url ?? '').split('?');
        const params = new URLSearchParams(qs ?? '');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3040',
              ingestBaseUrl: 'http://localhost:3040',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          return json(res, {
            permissions: ALL_PERMS,
            grants: [{ scope_type: 'org', scope_id: 'org1', permissions: ALL_PERMS }],
          });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);
        if (path === '/v1/me') return json(res, ME);

        if (path === '/v1/me/sessions') {
          const includeRevoked = params.get('include_revoked') === '1';
          return json(res, includeRevoked ? SESSIONS : SESSIONS.filter((s) => s.revoked_at === null));
        }
        if (path === '/v1/me/notification-subscriptions') return json(res, []);

        if (path === '/v1/apps/app1/issues') {
          return json(res, { data: ISSUES, total: ISSUES.length, total_is_capped: false, next_cursor: null, clamped: null });
        }
        if (path === '/v1/apps/app1/issues/stats') {
          // `series` is what `<TimeSeriesChart data={stats.series}>` reads; omitting
          // it makes the component dereference `undefined.length`.
          return json(res, {
            total: 3, unresolved: 2, resolved: 1, ignored: 0, fatal: 1, error: 1, warning: 1,
            series: Array.from({ length: 14 }, (_, i) => ({
              bucket: `2026-08-${String(i + 6).padStart(2, '0')}T00:00:00Z`,
              count: 5 + ((i * 7) % 23),
            })),
          });
        }
        if (path === '/v1/apps/app1/sessions') {
          return json(res, { data: SESSION_ROWS, total: SESSION_ROWS.length, total_is_capped: false, next_cursor: null, clamped: null });
        }
        if (path === '/v1/apps/app1/sessions/stats') {
          return json(res, { sessions: 2, avg_duration_ms: 480000, median_duration_ms: 480000, crashed: 1 });
        }
        if (path === '/v1/projects/proj1/monitors') return json(res, MONITORS);

        if (path?.startsWith('/v1/')) {
          console.log(`[i18n] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./i18n-harness', import.meta.url)),
  server: { port: 3040, strictPort: true },
});
