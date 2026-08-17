import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the session timeline's category + op filter.
 *
 * Exists because the behaviour is a function of WHICH LANES a session happens
 * to contain, and no seeded database reliably holds one session carrying
 * navigation, ordinary events, issues, and transactions across four distinct
 * ops — including the blank-op bucket, which is the case the op chips must not
 * silently drop. The fixtures below are built to hit each of those, plus the
 * two states the strip must react to on its own: a session with an empty lane
 * (chip disabled at 0) and a session with no timeline at all (strip hidden).
 *
 * **Stubbed at the HTTP layer, not the module layer**, as `vite.config.person-
 * harness.mjs` records: a `resolve.alias` on `lib/api/client` matches the
 * import specifier before resolution, so it misses the relative imports and the
 * real client goes to the :8090 pin in the committed `static/config.js`.
 * Answering the request keeps axios, the interceptors, `CachedView`,
 * `getSession` and the page itself as the real code.
 */
const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'web', ingest_enabled: true, platform: null, store_environment_id: null };

const SESSION_ID = 'sess-harness-01';
const DISTINCT = 'ana@example.com';

function ev(at, name, properties = null, screen = null) {
  return {
    kind: 'event',
    at,
    event: { id: `ev-${at}-${name}`, name, distinct_id: DISTINCT, session_id: SESSION_ID, properties, screen, occurred_at: at },
  };
}

function err(at, type, value) {
  return {
    kind: 'error',
    at,
    error: {
      id: `err-${at}`, issue_id: 'issue-1', level: 'error', exception_type: type, exception_value: value,
      message: null, release: '1.4.2', session_id: SESSION_ID, distinct_id: DISTINCT, screen: 'Checkout',
      occurred_at: at, stacktrace: null, stacktrace_symbolicated: null, debug_meta: null,
      symbolication_status: null, tags: { region: 'eu-west-1' }, context: null,
    },
  };
}

function tx(at, name, op, durationMs, over = {}) {
  return {
    kind: 'transaction',
    at,
    transaction: {
      id: `tx-${at}`, app_id: 'app1', environment_id: null, name, op, duration_ms: durationMs,
      status: 'ok', http_method: null, http_status: null, url: null, distinct_id: DISTINCT,
      session_id: SESSION_ID, device_key: 'dev-1', release: '1.4.2', ip_address: null,
      occurred_at: at, received_at: at, workflow_id: null, workflow_name: null,
      restored_pin_id: null, finished_at: new Date(new Date(at).getTime() + durationMs).toISOString(),
      tags: {}, extra: {}, ...over,
    },
  };
}

/**
 * Every lane, and four op buckets — `http`, `db`, `ui`, and one transaction the
 * SDK sent with a BLANK op. That last row is the whole reason the op filter
 * normalizes rather than skips: it must appear as `(none)` and be selectable,
 * not sit under the transaction chip unreachable by any op.
 */
const FULL_TIMELINE = [
  ev('2026-08-16T10:00:00.000Z', '$screen', { screen: 'Home' }, 'Home'),
  ev('2026-08-16T10:00:01.200Z', 'app_opened'),
  tx('2026-08-16T10:00:01.800Z', '/api/session', 'http', 142, { http_method: 'GET', http_status: 200, url: 'https://api.example.com/session' }),
  ev('2026-08-16T10:00:04.000Z', '$screen', { screen: 'Catalogue' }, 'Catalogue'),
  tx('2026-08-16T10:00:04.300Z', 'select products', 'db', 38),
  tx('2026-08-16T10:00:04.900Z', 'render grid', 'ui', 210),
  ev('2026-08-16T10:00:09.500Z', 'search_performed', { query: 'invoices' }, 'Catalogue'),
  tx('2026-08-16T10:00:10.100Z', '/api/search', 'http', 890, { http_method: 'POST', http_status: 200, url: 'https://api.example.com/search' }),
  ev('2026-08-16T10:00:16.000Z', '$screen', { screen: 'Checkout' }, 'Checkout'),
  tx('2026-08-16T10:00:16.400Z', 'legacy span', '', 55),
  tx('2026-08-16T10:00:17.000Z', '/api/checkout', 'http', 2400, { http_method: 'POST', http_status: 500, url: 'https://api.example.com/checkout' }),
  err('2026-08-16T10:00:19.400Z', 'TypeError', "Cannot read property 'id' of undefined"),
  ev('2026-08-16T10:00:21.000Z', 'checkout_failed', { reason: 'server_error' }, 'Checkout'),
];

/** No errors at all: the Issues chip must render 0 and be disabled, not clickable-to-nothing. */
const NO_ISSUES_TIMELINE = FULL_TIMELINE.filter((i) => i.kind !== 'error');

const FIXTURES = {
  full: FULL_TIMELINE,
  'no-issues': NO_ISSUES_TIMELINE,
  // Nothing at all: the strip must hide rather than render four dead chips.
  empty: [],
};

function sessionRow(timeline) {
  return {
    session_id: SESSION_ID,
    app_id: 'app1',
    environment_id: null,
    distinct_id: DISTINCT,
    device_key: 'dev-1',
    release: '1.4.2',
    started_at: '2026-08-16T10:00:00.000Z',
    last_event_at: timeline.length ? timeline[timeline.length - 1].at : '2026-08-16T10:00:00.000Z',
    events_count: timeline.filter((i) => i.kind === 'event').length,
    errors_count: timeline.filter((i) => i.kind === 'error').length,
    context: { os: { name: 'macOS', version: '15.2' }, browser: { name: 'Firefox', version: '141' } },
  };
}

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./timeline-filter-harness', import.meta.url));
  return {
    name: 'timeline-filter-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path] = (req.url ?? '').split('?');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3033',
              ingestBaseUrl: 'http://localhost:3033',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // Wide grants: permission gating is not what this harness verifies.
          return json(res, { permissions: ['*'], grants: [{ org_id: 'org1', project_id: null, app_id: null, environment_id: null, permissions: ['*'] }] });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);

        const session = path?.match(/^\/v1\/apps\/app1\/sessions\/(.+)$/);
        if (session) {
          const key = decodeURIComponent(session[1]);
          const timeline = FIXTURES[key] ?? FIXTURES.full;
          return json(res, { session: sessionRow(timeline), timeline });
        }

        // Logged rather than silently answered, so a fixture this harness forgot
        // shows up in the terminal instead of as an empty page.
        if (path?.startsWith('/v1/')) {
          console.log(`[timeline-filter-harness] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./timeline-filter-harness', import.meta.url)),
  server: { port: 3033, strictPort: true },
});
