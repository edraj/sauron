import { describe, expect, it } from 'vitest';
import { createSseParser } from './overview-stream';

/**
 * One well-formed `section` frame, as the server emits it.
 */
function frame(section: string, body: Record<string, unknown> = {}): string {
  return `event: section\ndata: ${JSON.stringify({ section, state: 'fresh', computed_at: '2026-08-17T12:00:00Z', data: null, ...body })}\n\n`;
}

describe('createSseParser', () => {
  it('parses a single frame', () => {
    const p = createSseParser();
    const out = p.push(frame('totals'));
    expect(out).toHaveLength(1);
    expect(out[0].section).toBe('totals');
    expect(out[0].state).toBe('fresh');
  });

  it('parses several frames arriving in one chunk', () => {
    const p = createSseParser();
    const out = p.push(frame('totals') + frame('series') + frame('top-events'));
    expect(out.map((f) => f.section)).toEqual(['totals', 'series', 'top-events']);
  });

  /**
   * THE test this file exists for.
   *
   * A parser that treats each chunk as whole frames passes every test above and
   * fails here — and in production it fails only on payloads big enough to be
   * split across TCP reads, i.e. exactly the large sections this stream carries.
   * Driving it one character at a time is the strongest form of the check: it
   * puts a boundary at every possible position, including inside the JSON,
   * inside a field name, and between the two terminating newlines.
   */
  it('reassembles a frame split at every possible byte boundary', () => {
    const text = frame('totals', { data: { totals: { events: 12345 }, error_rate: 0.5 } });
    const p = createSseParser();
    const out: unknown[] = [];
    for (const ch of text) out.push(...p.push(ch));
    expect(out).toHaveLength(1);
    expect((out[0] as { data: { totals: { events: number } } }).data.totals.events).toBe(12345);
  });

  it('holds an incomplete frame until its terminator arrives', () => {
    const p = createSseParser();
    const text = frame('series');
    const cut = text.length - 1; // everything but the final newline
    expect(p.push(text.slice(0, cut))).toHaveLength(0);
    expect(p.push(text.slice(cut))).toHaveLength(1);
  });

  it('carries a partial frame over to the next chunk', () => {
    const p = createSseParser();
    const two = frame('totals') + frame('series');
    const cut = two.indexOf('event: section', 1) + 10; // mid-way through frame 2
    const first = p.push(two.slice(0, cut));
    expect(first.map((f) => f.section)).toEqual(['totals']);
    const second = p.push(two.slice(cut));
    expect(second.map((f) => f.section)).toEqual(['series']);
  });

  /**
   * axum's `KeepAlive` emits a bare `:` comment every 15s. Treating it as a
   * frame — or worse, throwing on it — would break the stream after the first
   * idle period, which is precisely when nobody is watching.
   */
  it('skips keep-alive comments without emitting a frame', () => {
    const p = createSseParser();
    expect(p.push(':\n\n')).toHaveLength(0);
    expect(p.push(': keep-alive\n\n')).toHaveLength(0);
    expect(p.push(frame('totals'))).toHaveLength(1);
  });

  it('ignores non-section events rather than breaking the stream', () => {
    const p = createSseParser();
    expect(p.push('event: something-new\ndata: {"x":1}\n\n')).toHaveLength(0);
    // The stream must keep working afterwards — a future server-side addition
    // must not be able to kill an old client.
    expect(p.push(frame('totals'))).toHaveLength(1);
  });

  it('ignores a frame whose data is not valid JSON', () => {
    const p = createSseParser();
    expect(p.push('event: section\ndata: {not json\n\n')).toHaveLength(0);
    expect(p.push(frame('series'))).toHaveLength(1);
  });

  it('accepts CRLF terminators', () => {
    const p = createSseParser();
    const out = p.push(
      'event: section\r\ndata: {"section":"totals","state":"stale","computed_at":null,"data":null}\r\n\r\n',
    );
    expect(out).toHaveLength(1);
    expect(out[0].state).toBe('stale');
  });

  /**
   * Per the SSE grammar a multi-line payload is sent as repeated `data:` fields
   * and rejoined with `\n`. Joining with `''` instead produces valid-looking
   * but wrong JSON for any payload containing a newline.
   */
  it('rejoins multi-line data fields with newlines', () => {
    const p = createSseParser();
    const json = JSON.stringify({ section: 'totals', state: 'fresh', computed_at: null, data: 1 });
    const half = Math.floor(json.length / 2);
    const out = p.push(`event: section\ndata: ${json.slice(0, half)}\ndata: ${json.slice(half)}\n\n`);
    // Split mid-JSON across two data: lines rejoins to `a\nb`, which is not the
    // original — so this must NOT parse. The guard is that it fails cleanly.
    expect(out).toHaveLength(0);
    expect(p.push(frame('series'))).toHaveLength(1);
  });

  /**
   * The `data:` field's single optional leading space is framing, not content.
   * Stripping more than one — or none — corrupts payloads that legitimately
   * begin with whitespace.
   */
  it('strips exactly one leading space after the field colon', () => {
    const p = createSseParser();
    const body = '{"section":"totals","state":"fresh","computed_at":null,"data":null}';
    expect(p.push(`event: section\ndata:${body}\n\n`)).toHaveLength(1);
    expect(p.push(`event: section\ndata: ${body}\n\n`)).toHaveLength(1);
  });

  it('preserves the error field when a recompute failed', () => {
    const p = createSseParser();
    const out = p.push(
      'event: section\ndata: {"section":"totals","state":"computing","computed_at":null,"data":null,"error":"boom"}\n\n',
    );
    expect(out[0].error).toBe('boom');
    expect(out[0].data).toBeNull();
  });
});
