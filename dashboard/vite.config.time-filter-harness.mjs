import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the list time filter on Devices, Users and Sessions.
 *
 * Exists because the backend half of this feature is not built (the machine ran
 * out of disk), so there is no API that honours `time_field`/`from`/`to`. The
 * stub below honours them itself. That makes this harness verify the whole
 * FRONTEND contract rather than merely that the control paints:
 *
 *   - the parameters actually reach the wire, under the right names, and
 *     `since_days` is dropped whenever a bound is present;
 *   - the `viewKey` changes with the window, so a filter change repaints from
 *     the network instead of silently serving the previous window from cache;
 *   - the URL round-trips, including alongside the Devices drill-down key;
 *   - the Devices URL-write effect does not loop with the `groupKey` sync
 *     effect that reads the string it writes.
 *
 * What it CANNOT verify is the SQL — whether `first_seen` really selects
 * different rows in Postgres. That needs the backend built.
 *
 * **Stubbed at the HTTP layer, not the module layer**, per
 * `vite.config.person-harness.mjs`: a `resolve.alias` on `lib/api/client`
 * matches the import specifier before resolution, so it misses the relative
 * imports and the real client goes to the `static/config.js` :8090 pin.
 * Answering the request keeps axios, the interceptors, `CachedView` and the
 * real page code in the path, with only the far end canned.
 */
const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'web', ingest_enabled: true, platform: null, store_environment_id: null };

const DAY = 86_400_000;
const NOW = Date.parse('2026-08-16T12:00:00.000Z');
const ago = (d) => new Date(NOW - d * DAY).toISOString();

/**
 * The fixture's whole point is that `first_seen` and `last_seen` DISAGREE.
 *
 * `loyal` was first seen 200 days ago and is still active; `newbie` arrived
 * yesterday. A window on `last_seen` admits both, a window on `first_seen`
 * admits only `newbie`. A fixture where the two orderings correlate cannot tell
 * a working field selector from one that ignores the parameter — which is the
 * single thing this harness exists to check.
 */
const ROWS = [
  { key: 'loyal', first_seen: ago(200), last_seen: ago(1) },
  { key: 'newbie', first_seen: ago(1), last_seen: ago(1) },
  { key: 'dormant', first_seen: ago(300), last_seen: ago(120) },
];

/** Apply the window the request asked for. This is the stub's real job. */
function windowed(url) {
  const q = new URLSearchParams(url.split('?')[1] ?? '');
  const col = q.get('time_field') ?? 'last_seen';
  const from = q.get('from');
  const to = q.get('to');
  const sinceDays = q.get('since_days');

  // Mirrors `resolve_time_filter`'s precedence: explicit bounds win outright
  // and `since_days` is not consulted at all when either is present.
  let lo;
  let hi = null;
  if (from || to) {
    hi = to ? Date.parse(to) : null;
    lo = from ? Date.parse(from) : (hi ?? NOW) - 365 * DAY;
  } else {
    lo = NOW - Number(sinceDays ?? 30) * DAY;
  }
  // Half-open: `lo <= col < hi`.
  return ROWS.filter((r) => {
    const v = Date.parse(r[col]);
    return v >= lo && (hi === null || v < hi);
  });
}

function deviceRow(r, i) {
  return {
    id: `dev-${i}`, device_key: r.key, family: 'Pixel', model: '8 Pro', os_name: 'Android',
    os_version: '15', arch: 'arm64', browser: 'Chrome', last_distinct_id: `${r.key}@example.com`,
    first_seen: r.first_seen, last_seen: r.last_seen,
    events_count: 120, errors_count: 3, sessions_count: 12,
  };
}
function groupRow(r, i) {
  return {
    family: 'Pixel', model: `8 Pro (${r.key})`, os_name: 'Android', os_version: '15',
    device_count: i + 1, events_count: 120, errors_count: 3, sessions_count: 12,
    first_seen: r.first_seen, last_seen: r.last_seen,
  };
}
function personRow(r) {
  return {
    distinct_id: `${r.key}@example.com`, properties: { plan: 'pro' },
    first_seen: r.first_seen, last_seen: r.last_seen,
    events_count: 120, errors_count: 3, sessions_count: 12,
  };
}
function sessionRow(r, i) {
  return {
    id: `s-${i}`, session_id: `sess-${r.key}`, distinct_id: `${r.key}@example.com`,
    device_key: r.key, started_at: r.first_seen, last_event_at: r.last_seen,
    duration_ms: 45_000, events_count: 20, errors_count: 0, release: '1.4.2',
    context: {}, crashed: false,
  };
}

// `bucket`, not `day`. Every series component keys its `{#each}` on
// `point.bucket`, so a fixture using the wrong field name gives EVERY point the
// key `undefined` and Svelte throws `each_key_duplicate` — which surfaces as an
// uncaught error attributed to the page under test rather than to the fixture.
const USER_ANALYTICS = {
  stats: { total_users: 3, active_in_range: 2, new_in_range: 1, dau: 1, wau: 2, mau: 3, avg_session_ms: 45000, median_session_ms: 30000 },
  stickiness: 0.33,
  series: [{ bucket: ago(2), count: 1 }, { bucket: ago(1), count: 2 }],
};
// Shape taken from `models/index.ts`'s `SessionsAnalytics`, not guessed:
// `duration_histogram` feeds `DurationHistogram`, which opens with
// `data.reduce(...)`, so a fixture that omits it throws before the page paints
// and the failure looks like a bug in the code under test.
const SESSION_ANALYTICS = {
  stats: { sessions: 3, crashed: 0, avg_session_ms: 45000, median_session_ms: 30000 },
  duration_series: [{ bucket: ago(2), avg_ms: 40000 }, { bucket: ago(1), avg_ms: 50000 }],
  duration_histogram: [
    { bucket: '0-10s', count: 1 },
    { bucket: '10-60s', count: 2 },
  ],
};

/** Everything the three pages under test gate on, spelled out. */
const PERMS = [
  'event:read', 'issue:read', 'app:read', 'project:read', 'org:read',
  'member:read', 'monitor:read', 'source:read',
];

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./time-filter-harness', import.meta.url));
  return {
    name: 'time-filter-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url ?? '';
        const [path] = url.split('?');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3024',
              ingestBaseUrl: 'http://localhost:3024',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // Wide grants on purpose: permission gating is not what this harness
          // checks, and a locked-down fixture hides the page behind an access
          // error instead of failing loudly.
          //
          // ENUMERATED, not `['*']`. `models/permissions.ts` does no wildcard
          // expansion, so a `*` grant reads as the literal permission `*` and
          // `PAGE_ACCESS` denies every page — which is exactly what it did on
          // the first run of this harness.
          // The grant shape is `{ scope_type, scope_id, permissions }` — see
          // `sessionStore.can`. The `{ org_id, project_id, app_id, ... }` shape
          // that `vite.config.person-harness.mjs` uses matches NOTHING here;
          // that harness gets away with it because its page is not in
          // `PAGE_ACCESS`, so nothing ever asked.
          return json(res, { permissions: PERMS, grants: [{ scope_type: 'org', scope_id: 'org1', permissions: PERMS }] });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);

        // Echoed into the terminal so the params actually on the wire are
        // visible without opening devtools — this is the assertion the harness
        // exists to make.
        if (path?.startsWith('/v1/apps/app1/')) {
          console.log(`[time-filter-harness] ${req.method} ${url}`);
        }

        if (path === '/v1/apps/app1/devices') return json(res, windowed(url).map(deviceRow));
        if (path === '/v1/apps/app1/device-groups') return json(res, windowed(url).map(groupRow));
        if (path === '/v1/apps/app1/persons') return json(res, windowed(url).map(personRow));
        if (path === '/v1/apps/app1/users/summary') return json(res, USER_ANALYTICS);
        if (path === '/v1/apps/app1/sessions/summary') return json(res, SESSION_ANALYTICS);
        if (path === '/v1/apps/app1/sessions') {
          // The searched routes answer an envelope, not a bare array.
          const rows = windowed(url).map(sessionRow);
          return json(res, { data: rows, total: rows.length, total_is_capped: false, next_cursor: null, clamped: null });
        }

        if (path?.startsWith('/v1/')) {
          console.log(`[time-filter-harness] UNSTUBBED ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./time-filter-harness', import.meta.url)),
  server: { port: 3024, strictPort: true },
});
