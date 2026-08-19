import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type {
  AnalyticsEvent,
  ErrorEvent,
  ScreenDeviceRow,
  ScreenUserRow,
} from '../models';

/**
 * The four collapsible sections on the screen detail page.
 *
 * These are their own endpoints rather than `?filter=screen:eq:…` on the
 * Events/Devices/Users lists, because the query language reaches none of them:
 * the `screen` dimension is scoped to Issues+Occurrences, there is no app-wide
 * occurrences route for it to land on, and "devices/users on a screen" is an
 * aggregate over a different table rather than a column filter at all.
 *
 * All four page by the house over-fetch probe — request `limit + 1`, render
 * `limit`, and read the surplus row as "there is a page after this one". See
 * `overFetched`. None has a count endpoint, so `Pagination` is given
 * `total={null}` and shows how far the walk has gone rather than a page count.
 */
export interface ScreenSectionParams {
  /** The screen name, unencoded. Blank is a 400 from the server. */
  name: string;
  /** Rows to RENDER; the request asks for one more. */
  limit: number;
  offset: number;
  /** Server default is 30, clamped to 1..365. */
  sinceDays?: number;
}

/**
 * The shared query string.
 *
 * Built into the URL here rather than handed to axios as a `params` object.
 * That is the deliberate half of a trap this codebase has already paid for
 * once: axios serialises an array-valued param as `name[]=`, a spelling the
 * backend's extractor ignores, so the filter is dropped silently and the list
 * returns unnarrowed. None of these params is an array today, but writing the
 * query string by hand keeps the wire format visible at the call site instead
 * of delegating it to a serialiser whose defaults can surprise.
 *
 * `environment_id` is deliberately absent — the request interceptor in
 * `client.ts` attaches it, and these URLs are app-scoped telemetry reads that
 * it correctly scopes by construction (see `scope.ts`).
 */
function sectionQuery(opts: ScreenSectionParams): string {
  const p = new URLSearchParams();
  p.set('name', opts.name);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  p.set('limit', String(opts.limit + 1));
  p.set('offset', String(opts.offset));
  return p.toString();
}

async function section<T>(
  appId: string,
  path: string,
  opts: ScreenSectionParams,
): Promise<ListPage<T>> {
  const { data } = await api.get<T[]>(
    `/v1/apps/${appId}/screens/${path}?${sectionQuery(opts)}`,
  );
  return overFetched(data, opts.limit);
}

/**
 * A screen's analytics events, most recent first.
 *
 * Excludes the synthetic `$screen` view rows the mobile SDKs emit — those are
 * the `Views` stat tile above the card, not events. The Event Explorer applies
 * the same exclusion.
 */
export function listScreenEvents(
  appId: string,
  opts: ScreenSectionParams,
): Promise<ListPage<AnalyticsEvent>> {
  return section<AnalyticsEvent>(appId, 'events', opts);
}

/**
 * A screen's exceptions, most recent first.
 *
 * The server redacts rather than refuses for a caller without `issue:read` /
 * `source:read`, so a row can arrive with its body or source context stripped.
 * The page hides this card outright for a role lacking `issue:read`, which is
 * why the redacted shape is not rendered specially here.
 */
export function listScreenExceptions(
  appId: string,
  opts: ScreenSectionParams,
): Promise<ListPage<ErrorEvent>> {
  return section<ErrorEvent>(appId, 'exceptions', opts);
}

/** The devices seen on a screen, most recently active first. */
export function listScreenDevices(
  appId: string,
  opts: ScreenSectionParams,
): Promise<ListPage<ScreenDeviceRow>> {
  return section<ScreenDeviceRow>(appId, 'devices', opts);
}

/** The users seen on a screen, most recently active first. */
export function listScreenUsers(
  appId: string,
  opts: ScreenSectionParams,
): Promise<ListPage<ScreenUserRow>> {
  return section<ScreenUserRow>(appId, 'users', opts);
}
