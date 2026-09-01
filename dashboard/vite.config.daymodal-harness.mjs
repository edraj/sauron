import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the Performance → day-detail modal.
 *
 * Reuses `perfdrill-harness/` as its root (same real Performance page behind
 * the real router); only the series fixture and the port differ.
 *
 * The whole feature is invisible to the static gates. `svelte-check` is happy
 * with a row that opens a modal which requests the wrong operation, a filter
 * whose encoding the Transactions page cannot parse back, a modal that shows
 * the PREVIOUS row's spans while the new request is in flight, and a detail
 * panel whose line breaking is suppressed by the table around it. The unit
 * suite cannot see any of them either — they are all wire format and render.
 *
 * **Stubbed at the HTTP layer, not the module layer**, as
 * `vite.config.listui-harness.mjs` records: a `resolve.alias` on `lib/api/*`
 * matches the import specifier before resolution, so it misses relative imports
 * and the real client goes to the :8090 pin in the committed `static/config.js`.
 * Answering the request keeps axios, its interceptors and the pages themselves
 * as the real code.
 *
 * The transactions stub EVALUATES the `filter=` chips (`applyChips`) rather
 * than returning a fixed list. That is the point of the harness: a modal that
 * sent no filter at all, or sent one the server would reject, paints an
 * identical-looking table off an unfiltered response.
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
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'mobile', ingest_enabled: true, platform: 'android', store_environment_id: null };

/**
 * The two names from the screenshot, plus the cases that catch a hand-rolled
 * encoder: a name with a SPACE, and one carrying a `:` and a `/` — the two
 * characters `field:op:value` uses as structure.
 *
 * `get gamification prize` appears TWICE under different ops. That pair is the
 * fixture that fails a drill-down filtering on `name` alone: it would return
 * 9 + 4 spans for a row that counts 9.
 */
const OPERATIONS = [
  { name: 'get gamification prize', op: 'http', count: 9, p50: 120, p75: 400, p95: 1850, p99: 4200, avg: 380, error_rate: 0.11 },
  { name: 'wallet_payment_history', op: 'http', count: 24, p50: 90, p75: 150, p95: 320, p99: 900, avg: 140, error_rate: 0.0 },
  { name: 'get gamification prize', op: 'custom', count: 4, p50: 12, p75: 20, p95: 44, p99: 61, avg: 18, error_rate: 0.0 },
  { name: 'GET /v1/orders?status=open:pending', op: 'http', count: 6, p50: 210, p75: 300, p95: 640, p99: 1200, avg: 260, error_rate: 0.33 },
  { name: 'HomeRoute', op: 'screen_load', count: 31, p50: 310, p75: 520, p95: 980, p99: 1400, avg: 400, error_rate: 0.0 },
];

/**
 * HOURLY buckets, which is what the endpoint actually returns — the daily
 * fixture the drill-down harness uses would hide the whole point of the day
 * modal, because every bar would already be a day.
 *
 * Deliberately sparse and deliberately uneven: hours 0–1 and 12–16 of the
 * 26th record nothing at all, so the latency line has to BREAK across them
 * while the throughput line sits honestly on the floor. Two adjacent hours on
 * the 27th are the only measured ones, so the "lone point" marker has a case
 * to render. A dense, complete fixture would pass a chart that silently draws
 * 0 ms through every empty hour.
 */
const HOURS_BY_DAY = {
  '25': [3, 4, 5, 9, 10, 14, 15, 16, 20, 21],
  '26': [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 17, 18, 19, 20, 21, 22, 23],
  '27': [8, 9],
};
const SERIES = Object.entries(HOURS_BY_DAY).flatMap(([day, hours]) =>
  hours.map((h) => ({
    bucket: `2026-08-${day}T${String(h).padStart(2, '0')}:00:00Z`,
    p50: 60 + ((h * 13) % 90),
    p95: 180 + ((h * 137) % 1400),
    throughput: 5 + ((h * 29) % 140),
  })),
);

const DISTINCT = 'EQevrjcvy+sa6eIl5g7j4l5gwNIZuvt3YnQavcrALF4=';
const DEVICE = 'a50fd201-2683-4361-af8a-5d23e4ba1888';

/**
 * Spans for every operation above, with the durations that make the ordering
 * checkable: the slowest span of `get gamification prize`/`http` is 4,180 ms
 * and its most recent is 12 ms, so "Slowest" and "Most recent" cannot both
 * put the same row on top.
 */
function spansFor(name, op, n, seed) {
  return Array.from({ length: n }, (_, i) => {
    const slow = i === 0;
    return {
      id: `tx-${seed}-${i + 1}`,
      app_id: 'app1',
      environment_id: null,
      name,
      op,
      // Descending duration, ASCENDING time — so the two sort modes disagree.
      duration_ms: slow ? 4180 : Math.max(12, 900 - i * 120),
      status: i % 4 === 0 ? 'error' : 'ok',
      http_method: op === 'http' ? 'GET' : null,
      http_status: op === 'http' ? (i % 4 === 0 ? 500 : 200) : null,
      url: op === 'http'
        ? `https://api.example.com/v1/${name.replace(/[^a-z_]/gi, '')}?page=${i + 1}&include=meta,totals&locale=en-GB&trace=${'a'.repeat(60)}`
        : null,
      distinct_id: DISTINCT,
      session_id: `sess-${seed}-${String(i + 1).padStart(2, '0')}`,
      device_key: DEVICE,
      release: '3.1.21',
      ip_address: null,
      occurred_at: `2026-08-${String(6 + i).padStart(2, '0')}T10:0${i % 10}:19.400Z`,
      received_at: `2026-08-${String(6 + i).padStart(2, '0')}T10:0${i % 10}:22.900Z`,
      workflow_id: null,
      workflow_name: i === 1 ? 'checkout' : null,
      restored_pin_id: null,
      finished_at: `2026-08-${String(6 + i).padStart(2, '0')}T10:0${i % 10}:23.900Z`,
      tags: i === 2 ? { tier: 'premium', environment: 'PROD' } : {},
      // The SDK truncation marker on one span, so that banner renders somewhere.
      extra: i === 1 ? { _truncated: true, _bytes: 20480 } : i === 2 ? { order_id: 7781, retries: 2 } : {},
    };
  });
}

const ALL_SPANS = OPERATIONS.flatMap((o, idx) => spansFor(o.name, o.op, o.count, idx + 1));

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  res.end(JSON.stringify(body));
}

/**
 * Evaluate the `filter=` chips the way the server would.
 *
 * Deliberately strict about the encoding: the value is `decodeURIComponent`d
 * exactly once, matching `parseFilters`. A modal that sent a raw, unencoded
 * name would still "work" against a lenient stub and then return nothing in
 * production the moment a name contained a `:`.
 */
function applyChips(rows, params) {
  const chips = params.getAll('filter');
  if (chips.length === 0) return rows;
  return rows.filter((row) =>
    chips.every((chip) => {
      const i1 = chip.indexOf(':');
      const i2 = chip.indexOf(':', i1 + 1);
      if (i1 < 0 || i2 < 0) return false;
      const field = chip.slice(0, i1);
      const op = chip.slice(i1 + 1, i2);
      const value = decodeURIComponent(chip.slice(i2 + 1));
      const actual = { name: row.name, op: row.op, url: row.url, session: row.session_id, distinctId: row.distinct_id }[field];
      if (actual == null) return false;
      if (op === 'eq') return String(actual) === value;
      if (op === 'neq') return String(actual) !== value;
      if (op === 'contains') return String(actual).toLowerCase().includes(value.toLowerCase());
      return false;
    }),
  );
}

const TRANSACTION_SCHEMA = {
  resource: 'transactions',
  variables: [{ prefix: '@tag', description: 'Developer tags', chainable: true }],
  dimensions: [
    { name: 'name', type: 'string', ops: ['=', '!=', 'contains'] },
    { name: 'op', type: 'enum', ops: ['=', '!='], options: ['navigation', 'http', 'resource', 'screen_load', 'custom'] },
    { name: 'url', type: 'string', ops: ['=', '!=', 'contains'] },
    { name: 'session', type: 'string', ops: ['=', '!=', 'contains'] },
    { name: 'distinctId', type: 'string', ops: ['=', '!=', 'contains'] },
  ],
  available_tags: [{ key: 'tier', sample_values: ['premium'] }],
  available_labels: [],
};

let activeUsersCalls = 0;
let storageCalls = 0;

function stubApi() {
  const root = fileURLToPath(new URL('./daymodal-harness', import.meta.url));
  return {
    name: 'perfdrill-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path, qs] = (req.url ?? '').split('?');
        const params = new URLSearchParams(qs ?? '');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3045',
              ingestBaseUrl: 'http://localhost:3045',
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

        if (path === '/v1/apps/app1/search/schema') {
          return json(res, TRANSACTION_SCHEMA);
        }

        // Deliberate latency so a revalidate is OBSERVABLE: without it the
        // stub answers in microseconds and `revalidating` flips back before a
        // frame renders, which is indistinguishable from the indicator being
        // broken.

        // --- project active-users: `computing` first, then the report -------
        // The sequence is the point. A stub that answered with data straight
        // away would pass whether or not the page ever polls, which is the one
        // thing worth checking here.
        if (path === '/v1/projects/proj1/active-users') {
          activeUsersCalls += 1;
          console.log(`[daymodal] active-users call #${activeUsersCalls}`);
          if (activeUsersCalls === 1) {
            return json(res, { state: 'computing', computed_at: null, data: null });
          }
          // Call #2 is a FAILED recompute: HTTP 200, still `computing`, with the
          // reason in `error`. The page must show it and stop polling.
          if (activeUsersCalls === 2) {
            return json(res, {
              state: 'computing',
              computed_at: null,
              data: null,
              error: 'canceling statement due to statement timeout',
            });
          }
          return json(res, {
            state: 'fresh',
            computed_at: '2026-08-31T09:00:00Z',
            data: {
              requested: { from: '2026-08-25T00:00:00Z', to: '2026-09-01T00:00:00Z' },
              effective: { from: '2026-08-25T00:00:00Z', to: '2026-09-01T00:00:00Z' },
              truncated: false,
              truncation_reason: null,
              selections: [
                { app_id: 'app1', app_name: 'Harness App', environment_ids: [], environment_labels: [], resolved: 'all' },
              ],
              series: Array.from({ length: 7 }, (_, i) => ({
                day: `2026-08-${25 + i}`,
                active_total: 100 + i * 10,
                active_identified: 60 + i * 6,
                active_guest: 40 + i * 4,
              })),
              latest: { day: '2026-08-30', active_total: 150, active_identified: 90, active_guest: 60 },
              computed_at: '2026-08-31T09:00:00Z',
            },
          });
        }


        // Shaped stubs for the two detail pages: the catch-all below returns
        // `[]`, and an array where the page expects an object blows up inside
        // TimeSeriesChart / StatusPill for reasons that have nothing to do
        // with the page under test.
        if (path === '/v1/apps/app1/issues/abc') {
          // `IssueDetail` EXTENDS `Issue` — flat, not nested under `issue`.
          return json(res, {
            id: 'abc', app_id: 'app1', fingerprint: 'fp1', type: 'TypeError',
            title: 'TypeError: undefined is not a function', culprit: 'main.dart',
            level: 'error', status: 'unresolved',
            first_seen: '2026-08-20T00:00:00Z', last_seen: '2026-08-30T00:00:00Z',
            times_seen: 42, users_seen: 7, assignee_id: null,
            created_at: '2026-08-20T00:00:00Z', updated_at: '2026-08-30T00:00:00Z',
            latest_event: null,
            series: [{ bucket: '2026-08-29T00:00:00Z', count: 5 }],
          });
        }
        if (path === '/v1/monitors/abc') {
          return json(res, {
            monitor: {
              id: 'abc', project_id: 'proj1', name: 'API health', kind: 'http',
              target: 'https://api.example.com/health', status: 'up',
              interval_seconds: 60, enabled: true, timeout_ms: 5000,
            },
            uptime: { h24: 0.999, d7: 0.997, d30: 0.995 },
            incidents: [],
            pinned_alert_rules: 0,
          });
        }
        if (path === '/v1/monitors/abc/checks') return json(res, []);


        // Wall of Shame. The facets matter: the selects are built from them, and
        // the reported symptom is that picking one does not stick.

        // Admin storage: `computing` first, then the report — so the page's
        // poll-and-converge is what is actually being checked.
        if (path === '/v1/admin/storage') {
          storageCalls += 1;
          console.log(`[daymodal] storage call #${storageCalls}`);
          if (storageCalls === 1) {
            return json(res, { state: 'computing', computed_at: null, data: null });
          }
          return json(res, {
            state: 'fresh',
            computed_at: '2026-08-31T21:00:00Z',
            data: {
              database: {
                total_bytes: 66571993088,
                physical_bytes: 90194313216,
                cold_bytes: 0,
                full_scope: true,
                tables: [
                  { name: 'analytics_events', total_bytes: 66571993088, hot_rows: 63064258, tiered: true },
                  { name: 'error_events', total_bytes: 23622320128, hot_rows: 15923552, tiered: true },
                ],
              },
              apps: [
                {
                  app_id: 'app1',
                  app_name: 'Harness App',
                  project_name: 'Harness Project',
                  org_name: 'Harness Org',
                  tables: [],
                  hot_rows_total: 63064258,
                  cold_rows_total: 0,
                  cold_bytes_total: 0,
                  estimated_hot_bytes_total: 66571993088,
                  cold_files: [],
                  cold_files_total: 0,
                },
              ],
            },
          });
        }

        if (path === '/v1/admin/audit') {
          console.log(`[daymodal] audit ${req.url}`);
          return json(res, {
            entries: [],
            next_cursor: null,
            facets: {
              actors: [{ id: 'u1', label: 'ada@example.com', count: 3 }],
              actions: [{ id: 'app.create', label: 'app.create', count: 2 }],
              projects: [{ id: 'proj1', label: 'Harness Project', count: 5 }],
              apps: [{ id: 'app1', label: 'Harness App', count: 5 }],
              environments: [{ id: 'env1', label: 'production', count: 4 }],
            },
          });
        }

        if (path === '/v1/apps/app1/performance/summary') {
          const op0 = params.get('op');
          setTimeout(() => json(res, op0 ? OPERATIONS.filter((o) => o.op === op0) : OPERATIONS), 1500);
          return;
        }
        if (false) {
          const op = params.get('op');
          return json(res, op ? OPERATIONS.filter((o) => o.op === op) : OPERATIONS);
        }
        if (path === '/v1/apps/app1/performance/series') return json(res, SERIES);

        if (path === '/v1/apps/app1/transactions') {
          // Logged so the WIRE FORMAT is checkable: repeated `filter=`, both
          // chips present, and the sort the mode buttons claim to set.
          console.log(`[daymodal] transactions ${req.url}`);
          let rows = applyChips(ALL_SPANS, params);
          const sort = params.get('sort') ?? 'occurred_at';
          // `-` prefix means ASCENDING on this route; bare means descending.
          const asc = sort.startsWith('-');
          const key = asc ? sort.slice(1) : sort;
          rows = [...rows].sort((a, b) => {
            const av = key === 'duration_ms' ? a.duration_ms : a.occurred_at;
            const bv = key === 'duration_ms' ? b.duration_ms : b.occurred_at;
            const cmp = av < bv ? -1 : av > bv ? 1 : 0;
            return asc ? cmp : -cmp;
          });
          const limit = Number(params.get('limit') ?? 50);
          return json(res, {
            data: rows.slice(0, limit),
            total: rows.length,
            total_is_capped: false,
            next_cursor: null,
            clamped: null,
          });
        }

        if (path?.startsWith('/v1/')) {
          console.log(`[daymodal] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./daymodal-harness', import.meta.url)),
  server: { port: 3045, strictPort: true },
});
