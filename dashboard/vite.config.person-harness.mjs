import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the person Activity timeline's time modes and JSON export.
 *
 * Exists because the behaviour added here is a function of the *spacing*
 * between entries, and no seeded database reliably contains a person whose
 * activity spans milliseconds, minutes, and weeks in one window — which is
 * exactly what separates a correct offset column from one that reads
 * "86400.00 s" or an em dash on every row. The fixture below is built to hit
 * every tier, plus the two rows that must NOT render a number.
 *
 * **Stubbed at the HTTP layer, not the module layer**, for the reason recorded
 * in `vite.config.search-harness.mjs`: a `resolve.alias` on `lib/api/client`
 * matches the import specifier before resolution, so it silently misses the
 * relative imports and the real client goes to the :8090 pin in the committed
 * `static/config.js`. Answering the request instead keeps axios, the
 * interceptors, `CachedView`, `getPerson` and the whole page as the real code,
 * with only the server on the far end canned.
 */
const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'web', ingest_enabled: true, platform: null, store_environment_id: null };

/**
 * The gap ladder, newest first. Every value is chosen to land in a different
 * branch of `formatOffset`, so one screenshot proves the whole tier table:
 *
 *   30d, 25h, 1h, 90s, 962ms, 2ms, and a tie at 0ms.
 */
const T = (iso) => new Date(iso).toISOString();
const EVENTS = [
  { at: '2026-08-16T10:00:00.000Z', name: 'checkout_completed', props: { plan: 'pro', amount: 49 } },
  { at: '2026-07-17T10:00:00.000Z', name: '$screen', props: { screen: 'Settings' } },
  { at: '2026-07-16T09:00:00.000Z', name: 'profile_updated', props: null },
  { at: '2026-07-16T08:00:00.000Z', name: '$screen', props: { screen: 'Profile' } },
  { at: '2026-07-16T07:58:30.000Z', name: 'search_performed', props: { query: 'invoices' } },
  { at: '2026-07-16T07:58:29.038Z', name: 'app_opened', props: null },
  // Same millisecond as the row below it: a measured zero gap, not a missing one.
  { at: '2026-07-16T07:58:29.036Z', name: 'session_started', props: null },
];
const ERRORS = [
  { at: '2026-07-16T07:58:29.036Z', type: 'TypeError', value: "Cannot read property 'id' of undefined", level: 'error' },
];

function personProfile(distinctId) {
  return {
    distinct_id: distinctId,
    user: {
      distinct_id: distinctId,
      properties: { email: distinctId, plan: 'pro', signup_source: 'organic' },
      first_seen: T('2026-06-01T12:00:00Z'),
      last_seen: T('2026-08-16T10:00:00Z'),
      events_count: EVENTS.length,
      errors_count: ERRORS.length,
      sessions_count: 3,
    },
    events: EVENTS.map((e, i) => ({
      id: `ev-${i}`,
      name: e.name,
      distinct_id: distinctId,
      session_id: `sess-${i % 3}`,
      properties: e.props,
      screen: e.props?.screen ?? null,
      occurred_at: T(e.at),
    })),
    errors: ERRORS.map((e, i) => ({
      id: `err-${i}`,
      issue_id: `issue-${i}`,
      level: e.level,
      exception_type: e.type,
      exception_value: e.value,
      message: null,
      release: '1.4.2',
      session_id: 'sess-0',
      distinct_id: distinctId,
      occurred_at: T(e.at),
      stacktrace: null,
      tags: null,
    })),
  };
}

/** A person with no recorded activity — the empty state must hide both buttons. */
const EMPTY_PROFILE = (distinctId) => ({
  distinct_id: distinctId,
  user: { distinct_id: distinctId, properties: null, first_seen: T('2026-08-01T00:00:00Z'), last_seen: T('2026-08-01T00:00:00Z'), events_count: 0, errors_count: 0, sessions_count: 0 },
  events: [],
  errors: [],
});

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./person-harness', import.meta.url));
  return {
    name: 'person-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path] = (req.url ?? '').split('?');

        // `static/config.js` is committed with `apiBaseUrl` pinned to :8090 and,
        // because it assigns a runtime global, it outranks any build-time env
        // var. Point it at this origin instead.
        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3022',
              ingestBaseUrl: 'http://localhost:3022',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // Wide grants: permission gating is not what this harness verifies,
          // and a locked-down fixture would hide the page behind an access
          // error instead of failing loudly.
          return json(res, { permissions: ['*'], grants: [{ org_id: 'org1', project_id: null, app_id: null, environment_id: null, permissions: ['*'] }] });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);

        const person = path?.match(/^\/v1\/apps\/app1\/persons\/(.+)$/);
        if (person) {
          const distinctId = decodeURIComponent(person[1]);
          return json(res, distinctId.startsWith('quiet') ? EMPTY_PROFILE(distinctId) : personProfile(distinctId));
        }

        // Anything else the shell asks for on boot. Logged rather than silently
        // answered, so a fixture this harness forgot shows up in the terminal
        // instead of as an empty page.
        if (path?.startsWith('/v1/')) {
          console.log(`[person-harness] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./person-harness', import.meta.url)),
  server: { port: 3022, strictPort: true },
});
