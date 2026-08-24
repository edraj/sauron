import { api } from './client';
import { toParams, type DateRangeValue } from '../models/date-range';

/**
 * Row counts for the four offset-paged lists that have no total of their own.
 *
 * Screens, Devices, Users and Workflows page by a `limit + 1` over-fetch probe,
 * which answers "is there another page" and nothing else — so their pagers
 * could only ever offer Prev/Next. These endpoints supply the page count that
 * turns those into numbered strips.
 *
 * ## Why a second request rather than a `total` on the list response
 *
 * Two of the four counts run over `list_persons` / `list_devices` — the
 * hand-written SQL behind the worst timeouts this project has had. Folding the
 * count into the list would put that cost on the latency path of the table
 * itself. On its own request it delays the page strip and nothing else: rows
 * paint at the speed they always did, and the numbers resolve a beat later.
 *
 * It also keeps the list responses' shape unchanged, so nothing that reads them
 * had to be touched.
 *
 * ## Call these WITHOUT the page parameters
 *
 * Pass the predicate — window, search, environment — and omit `limit`,
 * `offset` and `sort`. The server ignores them, but a request that sent them
 * would read like it described a page.
 */
export interface CountEnvelope {
  total: number;
  /**
   * The server stopped counting at its cap (10,000), so `total` means "at
   * least this many". Render as a `+`; never treat the number as exact when
   * this is set.
   */
  total_is_capped: boolean;
}

/**
 * The predicate half each count accepts — deliberately a subset of its list's
 * query, with the page fields left out.
 *
 * `environment_id` is absent here for the same reason it is absent from every
 * list client: the axios interceptor adds it from the session scope, so a
 * caller cannot accidentally count one environment while displaying another.
 */
export interface CountParams {
  /** The date-range picker's window, encoded by `date-range`'s `toParams`. */
  range?: DateRangeValue;
  /** Free-text filter. Sent as `q` for screens, `search` for the other three. */
  search?: string;
  /** `time_field`/`from`/`to`, already encoded by `models/time-filter`'s `toRecord`. */
  window?: Record<string, string>;
}

function withPredicate(p: URLSearchParams, opts: CountParams): URLSearchParams {
  if (opts.range) {
    for (const [k, v] of Object.entries(toParams(opts.range))) p.set(k, v);
  }
  for (const [k, v] of Object.entries(opts.window ?? {})) {
    if (v) p.set(k, v);
  }
  return p;
}

export async function countScreens(appId: string, opts: CountParams = {}): Promise<CountEnvelope> {
  const p = withPredicate(new URLSearchParams(), opts);
  // Screens names its free-text parameter `q`; the other three use `search`.
  // Mirrored from each list client rather than unified, because the count has
  // to match the list it captions, not a tidier convention.
  if (opts.search) p.set('q', opts.search);
  const { data } = await api.get<CountEnvelope>(`/v1/apps/${appId}/counts/screens?${p}`);
  return data;
}

export async function countWorkflows(
  appId: string,
  opts: CountParams = {},
): Promise<CountEnvelope> {
  const p = withPredicate(new URLSearchParams(), opts);
  if (opts.search) p.set('search', opts.search);
  const { data } = await api.get<CountEnvelope>(`/v1/apps/${appId}/counts/workflows?${p}`);
  return data;
}

export async function countPersons(appId: string, opts: CountParams = {}): Promise<CountEnvelope> {
  const p = withPredicate(new URLSearchParams(), opts);
  if (opts.search) p.set('search', opts.search);
  const { data } = await api.get<CountEnvelope>(`/v1/apps/${appId}/counts/persons?${p}`);
  return data;
}

/**
 * Devices, for whichever of the two shapes the same parameters would list.
 *
 * `grouped` picks between them and must match what the table is rendering: the
 * default inventory is one row per descriptor tuple, the drill-down is one row
 * per device, and the two totals differ by a large factor. The server reads the
 * same `group` sentinel its list routes read.
 */
export async function countDevices(
  appId: string,
  opts: CountParams & {
    grouped: boolean;
    family?: string;
    model?: string;
    osName?: string;
    osVersion?: string;
  },
): Promise<CountEnvelope> {
  const p = withPredicate(new URLSearchParams(), opts);
  if (opts.search) p.set('search', opts.search);
  if (!opts.grouped) {
    // Any non-empty value turns the drill-down on; "1" is what the list sends.
    p.set('group', '1');
    if (opts.family) p.set('family', opts.family);
    if (opts.model) p.set('model', opts.model);
    if (opts.osName) p.set('os_name', opts.osName);
    if (opts.osVersion) p.set('os_version', opts.osVersion);
  }
  const { data } = await api.get<CountEnvelope>(`/v1/apps/${appId}/counts/devices?${p}`);
  return data;
}
