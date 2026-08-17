import { apiBaseUrl } from '../config/env';
import { currentAccessToken, refreshAccessToken } from './client';
import { currentEnvironmentId } from './scope';
import type { OverviewEnvelope, OverviewSectionName } from './overview';

/**
 * The push half of the Overview cache.
 *
 * The five section endpoints now answer instantly from Redis — `fresh`, `stale`
 * or `computing` — and never run their aggregate on the request path. The
 * aggregate runs server-side in the background and its result arrives here.
 *
 * ## Why `fetch()` and not `EventSource`
 *
 * `EventSource` cannot set request headers, and this API authenticates with
 * `Authorization: Bearer`. The two usual workarounds are both rejected:
 *
 * - **Token in the query string.** A live JWT would be written into every
 *   access log, proxy log and `Referer` header along the path. Query strings
 *   are the one part of a URL that is logged everywhere by default.
 * - **Cookie auth for this route only.** Would open a CSRF surface on an API
 *   that currently has none, to save the ~60 lines below.
 *
 * So the stream is read as a plain `fetch()` body and the frames are parsed
 * here. The cost is this file; the benefit is that the token stays in a header
 * and the 401-refresh path is reused rather than reimplemented.
 *
 * ## The frame format
 *
 * Standard SSE: `event:` and `data:` lines, frames separated by a blank line,
 * `\n\n`. Only `section` events carry a payload; axum's keep-alive emits bare
 * `:` comment lines every 15s, which parse to nothing and are skipped.
 */

/** One section's state, as pushed by the server. */
export interface SectionFrame extends OverviewEnvelope<unknown> {
  section: OverviewSectionName;
}

export interface StreamHandle {
  /** Idempotent. Safe to call from an effect teardown. */
  close(): void;
}

/**
 * Open the Overview stream for one app / environment / window.
 *
 * Returns a handle immediately; frames arrive on `onSection` until `close()` is
 * called or the connection drops. `onError` fires on a connection failure, not
 * on a section whose recompute failed — the latter arrives as a normal frame
 * carrying an `error` field, because the section still has a state worth
 * rendering.
 *
 * Reconnection is deliberately NOT built in. The server sends a full snapshot
 * of every section on connect, so a caller that wants to recover simply opens a
 * new stream and converges; a retry loop in here would have to duplicate that
 * decision without knowing whether the page is still mounted.
 */
export function openOverviewStream(
  appId: string,
  sinceDays: number,
  handlers: {
    onSection: (frame: SectionFrame) => void;
    onError?: (err: unknown) => void;
    onOpen?: () => void;
  },
): StreamHandle {
  const controller = new AbortController();
  let closed = false;

  const close = () => {
    if (closed) return;
    closed = true;
    controller.abort();
  };

  void (async () => {
    try {
      const res = await fetchStream(appId, sinceDays, controller.signal);
      if (!res.body) throw new Error('overview stream: response had no body');
      handlers.onOpen?.();
      await readFrames(res.body, (frame) => {
        if (!closed) handlers.onSection(frame);
      });
    } catch (err) {
      // An abort is the caller closing us, not a failure. Reporting it would
      // make every navigation look like a broken stream.
      if (closed || (err instanceof DOMException && err.name === 'AbortError')) return;
      handlers.onError?.(err);
    }
  })();

  return { close };
}

/**
 * Issue the request, retrying ONCE through the token refresh on a 401.
 *
 * Mirrors the axios response interceptor rather than sharing it: the
 * interceptor operates on axios config objects and replays an axios request,
 * neither of which exists here. What must not drift is the single-flight
 * refresh, and that is shared — `refreshAccessToken()` parks on the same
 * promise every axios 401 does, so a page that opens a stream and fires five
 * requests at an expired token still performs exactly one refresh.
 */
async function fetchStream(
  appId: string,
  sinceDays: number,
  signal: AbortSignal,
): Promise<Response> {
  const url = streamUrl(appId, sinceDays);

  const attempt = (token: string | null) =>
    fetch(url, {
      method: 'GET',
      headers: {
        Accept: 'text/event-stream',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      signal,
    });

  let res = await attempt(currentAccessToken());
  if (res.status === 401) {
    const fresh = await refreshAccessToken();
    res = await attempt(fresh);
  }
  if (!res.ok) {
    throw new Error(`overview stream: HTTP ${res.status}`);
  }
  return res;
}

/**
 * Build the stream URL.
 *
 * `environment_id` is appended HERE rather than inherited, because this request
 * does not pass through the axios interceptor that adds it to every other
 * scoped read. Omitting it would silently open a stream for ALL environments
 * while the page displayed one — the exact class of bug `scope.ts` exists to
 * prevent, and it would present as "the numbers occasionally disagree with the
 * picker" rather than as an error.
 *
 * A null environment omits the parameter entirely rather than sending it empty,
 * matching the wire contract in `scope.ts`.
 */
function streamUrl(appId: string, sinceDays: number): string {
  const url = new URL(`${apiBaseUrl}/v1/apps/${appId}/overview/stream`);
  url.searchParams.set('since_days', String(sinceDays));
  const envId = currentEnvironmentId();
  if (envId) url.searchParams.set('environment_id', envId);
  return url.toString();
}

/**
 * Incremental SSE frame parser.
 *
 * Exported for its tests. The behaviour that matters is that `push` may be
 * called with ARBITRARY splits — a chunk boundary can land mid-JSON, mid-field
 * name, or between the two newlines that terminate a frame — and the parser
 * must carry the remainder over rather than treating each chunk as whole
 * frames. That is the classic SSE bug: it only bites on payloads large enough
 * to be split, so it passes every small-payload test and corrupts exactly the
 * big responses this stream exists to deliver.
 *
 * Stateful by necessity; one instance per connection.
 */
export function createSseParser(): { push(text: string): SectionFrame[] } {
  let buffer = '';
  return {
    push(text: string): SectionFrame[] {
      buffer += text;
      const out: SectionFrame[] = [];
      let sep: number;
      // Servers may use either terminator; axum sends `\n\n`.
      while ((sep = indexOfFrameEnd(buffer)) !== -1) {
        const raw = buffer.slice(0, sep);
        buffer = buffer.slice(sep).replace(/^(\r?\n){2}/, '');
        const frame = parseFrame(raw);
        if (frame) out.push(frame);
      }
      return out;
    },
  };
}

/** Drive {@link createSseParser} from a `fetch` response body. */
async function readFrames(
  body: ReadableStream<Uint8Array>,
  onFrame: (frame: SectionFrame) => void,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  const parser = createSseParser();

  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    // `stream: true` so a multi-byte UTF-8 character split across chunks is
    // held rather than decoded into a replacement character.
    for (const frame of parser.push(decoder.decode(value, { stream: true }))) {
      onFrame(frame);
    }
  }
}

function indexOfFrameEnd(buffer: string): number {
  const lf = buffer.indexOf('\n\n');
  const crlf = buffer.indexOf('\r\n\r\n');
  if (lf === -1) return crlf;
  if (crlf === -1) return lf;
  return Math.min(lf, crlf);
}

/**
 * One frame's text to a `SectionFrame`, or null for anything else.
 *
 * Returns null rather than throwing for keep-alive comments, unknown event
 * types and unparseable data: a stream that died on an unrecognized frame would
 * be broken by any future server-side addition.
 */
function parseFrame(raw: string): SectionFrame | null {
  let event = 'message';
  const dataLines: string[] = [];

  for (const line of raw.split(/\r?\n/)) {
    if (line === '' || line.startsWith(':')) continue; // keep-alive comment
    const colon = line.indexOf(':');
    const field = colon === -1 ? line : line.slice(0, colon);
    // One optional leading space after the colon is part of the framing, per
    // the SSE grammar, and is not data.
    const rest = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '');
    if (field === 'event') event = rest;
    else if (field === 'data') dataLines.push(rest);
  }

  if (event !== 'section' || dataLines.length === 0) return null;
  try {
    // Multi-line `data:` fields rejoin with `\n`, per the SSE grammar.
    return JSON.parse(dataLines.join('\n')) as SectionFrame;
  } catch {
    return null;
  }
}
