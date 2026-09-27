import { describe, expect, it } from 'vitest';
// `?raw` rather than `node:fs` — see the note in
// `components/filters/filter-registry-parity.test.ts`.
import mainRs from '../../../../backend/bins/sauron-api/src/main.rs?raw';
import issuesTs from './issues.ts?raw';
import sessionsTs from './sessions.ts?raw';
import personsTs from './persons.ts?raw';
import devicesTs from './devices.ts?raw';
import monitorsTs from './monitors.ts?raw';
import issuePage from '../../pages/IssueDetail.svelte?raw';
import sessionPage from '../../pages/SessionDetail.svelte?raw';
import personPage from '../../pages/PersonProfile.svelte?raw';
import devicePage from '../../pages/DeviceDetail.svelte?raw';
import monitorPage from '../../pages/MonitorDetail.svelte?raw';

/**
 * The detail pages read their sections from routes of their own, so the header
 * can paint off a keyed lookup while the heavy cards show a skeleton.
 *
 * Three things have to stay true for that to keep working, and nothing else
 * checks any of them:
 *
 *  1. every section the dashboard requests is a route the API serves — a typo
 *     here is a card that 404s into an error state on every visit;
 *  2. every section route the API serves has a caller — the
 *     backend-plus-typed-client-plus-zero-call-sites shape that ships green and
 *     unreachable;
 *  3. no page has quietly gone back to the composite route, which would bring
 *     back the whole-page wait with every test still passing.
 */
interface Section {
  /** The route as `main.rs` registers it. */
  route: string;
  /** The same path as the client writes it, in a template literal. */
  client: string;
  /** The exported function that requests it. */
  fn: string;
  source: string;
  page: string;
}

const SECTIONS: Section[] = [
  { route: '/v1/apps/{app_id}/issues/{issue_id}/summary', client: '/issues/${issueId}/summary`', fn: 'getIssueSummary', source: issuesTs, page: issuePage },
  { route: '/v1/apps/{app_id}/issues/{issue_id}/latest-event', client: '/issues/${issueId}/latest-event`', fn: 'getIssueLatestEvent', source: issuesTs, page: issuePage },
  { route: '/v1/apps/{app_id}/issues/{issue_id}/series', client: '/issues/${issueId}/series`', fn: 'getIssueSeries', source: issuesTs, page: issuePage },
  { route: '/v1/apps/{app_id}/sessions/{session_id}/summary', client: '/sessions/${encodeURIComponent(sessionId)}/summary`', fn: 'getSessionSummary', source: sessionsTs, page: sessionPage },
  { route: '/v1/apps/{app_id}/sessions/{session_id}/timeline', client: '/sessions/${encodeURIComponent(sessionId)}/timeline`', fn: 'getSessionTimeline', source: sessionsTs, page: sessionPage },
  { route: '/v1/apps/{app_id}/persons/{distinct_id}/summary', client: '/persons/${encodeURIComponent(distinctId)}/summary`', fn: 'getPersonSummary', source: personsTs, page: personPage },
  { route: '/v1/apps/{app_id}/persons/{distinct_id}/timeline', client: '/persons/${encodeURIComponent(distinctId)}/timeline`', fn: 'getPersonTimeline', source: personsTs, page: personPage },
  { route: '/v1/apps/{app_id}/device/summary', client: '/device/summary`', fn: 'getDeviceSummary', source: devicesTs, page: devicePage },
  { route: '/v1/apps/{app_id}/device/sessions', client: '/device/sessions`', fn: 'getDeviceSessions', source: devicesTs, page: devicePage },
  { route: '/v1/apps/{app_id}/device/errors', client: '/device/errors`', fn: 'getDeviceErrors', source: devicesTs, page: devicePage },
  { route: '/v1/apps/{app_id}/device/perf', client: '/device/perf`', fn: 'getDevicePerf', source: devicesTs, page: devicePage },
  { route: '/v1/monitors/{monitor_id}/summary', client: '/v1/monitors/${id}/summary`', fn: 'getMonitorSummary', source: monitorsTs, page: monitorPage },
  { route: '/v1/monitors/{monitor_id}/uptime', client: '/v1/monitors/${id}/uptime`', fn: 'getMonitorUptime', source: monitorsTs, page: monitorPage },
  { route: '/v1/monitors/{monitor_id}/incidents', client: '/v1/monitors/${id}/incidents`', fn: 'getMonitorIncidents', source: monitorsTs, page: monitorPage },
];

/** Every route path `main.rs` registers, as written. */
const REGISTERED = new Set(Array.from(mainRs.matchAll(/"(\/v1\/[^"]+)"/g), (m) => m[1]));

describe('detail page sections ↔ API routes', () => {
  it('found the router', () => {
    // Guards every assertion below against a vacuous pass: if `main.rs` moved
    // or the route spelling changed, the set is empty and nothing would match.
    expect(REGISTERED.size).toBeGreaterThan(100);
  });

  for (const s of SECTIONS) {
    it(`${s.fn} requests a route the API serves, and its page calls it`, () => {
      expect(REGISTERED, `main.rs does not register ${s.route}`).toContain(s.route);
      expect(s.source, `${s.fn} is not exported`).toContain(`export async function ${s.fn}(`);
      expect(s.source, `no request for ${s.client}`).toContain(s.client);
      expect(s.page, `the page never calls ${s.fn}`).toContain(`${s.fn}(`);
    });
  }

  it('every section route the API registers has a caller here', () => {
    // The section routes are the ones sitting one segment below a detail
    // route. Derived from the router rather than listed, so a section added on
    // the backend alone fails here instead of shipping unreachable.
    const SECTION_OF = [
      /^\/v1\/apps\/\{app_id\}\/issues\/\{issue_id\}\/(summary|latest-event|series)$/,
      /^\/v1\/apps\/\{app_id\}\/sessions\/\{session_id\}\/(summary|timeline)$/,
      /^\/v1\/apps\/\{app_id\}\/persons\/\{distinct_id\}\/[^/]+$/,
      /^\/v1\/apps\/\{app_id\}\/device\/[^/]+$/,
      /^\/v1\/monitors\/\{monitor_id\}\/(summary|uptime|incidents)$/,
    ];
    const served = [...REGISTERED].filter((r) => SECTION_OF.some((re) => re.test(r)));
    expect(served.sort()).toEqual(SECTIONS.map((s) => s.route).sort());
  });

  it('no detail page reads the composite route any more', () => {
    // The composite stays on the API for other callers. A page going back to
    // it would restore the whole-page wait and fail nothing else.
    for (const [name, page] of [
      ['IssueDetail', issuePage],
      ['SessionDetail', sessionPage],
      ['PersonProfile', personPage],
      ['DeviceDetail', devicePage],
      ['MonitorDetail', monitorPage],
    ] as const) {
      for (const composite of ['getIssue(', 'getSession(', 'getPerson(', 'getDevice(', 'getMonitor(']) {
        expect(page, `${name} calls ${composite}…)`).not.toContain(composite);
      }
    }
  });
});
