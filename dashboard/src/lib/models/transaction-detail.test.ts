import { describe, expect, it } from 'vitest';
import { detailRows, isTruncated, truncatedBytesLabel } from './transaction-detail';
import type { Transaction } from './index';

function span(over: Partial<Transaction> = {}): Transaction {
  return {
    id: 'tx-1',
    app_id: 'app-1',
    environment_id: null,
    name: 'wallet_payment_history',
    op: 'http',
    duration_ms: 1234,
    status: 'ok',
    http_method: 'GET',
    http_status: 200,
    url: 'https://api.example.com/wallet/history?page=1',
    distinct_id: 'user-9',
    session_id: 'sess-7',
    device_key: 'dev-3',
    release: '1.4.0',
    ip_address: '10.0.0.0',
    occurred_at: '2026-08-19T10:00:00Z',
    received_at: '2026-08-19T10:00:04Z',
    workflow_id: null,
    workflow_name: null,
    restored_pin_id: null,
    finished_at: '2026-08-19T10:00:01Z',
    tags: {},
    extra: {},
    ...over,
  } as Transaction;
}

const labels = (t: Transaction) => detailRows(t).map((r) => r.label);
const valueOf = (t: Transaction, label: string) =>
  detailRows(t).find((r) => r.label === label)?.value;
const hrefOf = (t: Transaction, label: string) =>
  detailRows(t).find((r) => r.label === label)?.href;

describe('detailRows', () => {
  /**
   * The whole point of hand-writing the list is deciding what NOT to show.
   * `ip_address` is masked and permission-nulled by the API, so a row for it is
   * either a truncated address or a blank that reads as "no IP recorded"; the
   * three internal ids are plumbing. A key-walk implementation passes every
   * other test in this file and fails only this one.
   */
  it('omits the fields the panel deliberately does not show', () => {
    const shown = labels(span());
    for (const absent of ['IP address', 'App id', 'Environment id', 'Restored pin id']) {
      expect(shown).not.toContain(absent);
    }
    expect(shown).toContain('Transaction id');
  });

  it('renders both timestamps, so a queued or clock-skewed span is legible', () => {
    // The gap between them is the fact worth seeing; dropping either hides it.
    const t = span({ occurred_at: '2026-08-19T08:00:00Z', received_at: '2026-08-19T11:30:00Z' });
    expect(valueOf(t, 'Occurred at')).toBe('2026-08-19T08:00:00Z');
    expect(valueOf(t, 'Received at')).toBe('2026-08-19T11:30:00Z');
  });

  it('distinguishes an absent field from a zero-ish one', () => {
    // `http_status: 0` is falsy; a `||`-based guard would blank it and claim the
    // span carried no status. Only `== null` gets this right.
    expect(valueOf(span({ http_status: 0 }), 'HTTP status')).toBe('0');
    expect(valueOf(span({ http_status: null }), 'HTTP status')).toBeNull();
    expect(valueOf(span({ duration_ms: 0 }), 'Duration')).toBe('0 ms');
  });

  it('links User, Session and Device to their own pages, url-encoding the id', () => {
    const t = span({ distinct_id: 'user a/b', session_id: 'sess a/b', device_key: 'dev a/b' });
    expect(hrefOf(t, 'User')).toBe('#/persons/user%20a%2Fb');
    expect(hrefOf(t, 'Session')).toBe('#/sessions/sess%20a%2Fb');
    expect(hrefOf(t, 'Device')).toBe('#/devices/dev%20a%2Fb');
  });

  it('leaves an absent id unlinked rather than linking to an empty route', () => {
    // `#/persons/` with no id is a route that loads and then fails to find
    // anything — worse than plain text, because it looks like a working link.
    const t = span({ distinct_id: null, session_id: null, device_key: null });
    expect(hrefOf(t, 'User')).toBeUndefined();
    expect(hrefOf(t, 'Session')).toBeUndefined();
    expect(hrefOf(t, 'Device')).toBeUndefined();
  });

  it('gives URL the full-width row, and nothing else', () => {
    // The one field with no upper bound. If a second row picks up `wide`, the
    // two-column grid degrades to one column of mostly whitespace.
    const wide = detailRows(span()).filter((r) => r.wide).map((r) => r.label);
    expect(wide).toEqual(['URL']);
  });
});

describe('isTruncated', () => {
  it('is true only for the SDK marker, not for any populated extra', () => {
    expect(isTruncated(span({ extra: { _truncated: true, _bytes: 20_000 } }))).toBe(true);
    expect(isTruncated(span({ extra: { order_id: 7 } }))).toBe(false);
    expect(isTruncated(span({ extra: {} }))).toBe(false);
    expect(isTruncated(span({ extra: null }))).toBe(false);
  });

  it('does not treat a truthy non-boolean marker as truncation', () => {
    // A developer's own `_truncated: "yes"` key is data, not the SDK's signal.
    expect(isTruncated(span({ extra: { _truncated: 'yes' } }))).toBe(false);
  });
});

describe('truncatedBytesLabel', () => {
  it('reports a negative byte count as a serialization failure, not "-1 bytes"', () => {
    expect(truncatedBytesLabel(span({ extra: { _truncated: true, _bytes: -1 } }))).toBe(
      'the value could not be serialized',
    );
  });

  it('thousands-separates a real byte count', () => {
    expect(truncatedBytesLabel(span({ extra: { _truncated: true, _bytes: 20_480 } }))).toBe(
      '20,480 bytes',
    );
  });

  it('falls back when the marker carries no byte count at all', () => {
    expect(truncatedBytesLabel(span({ extra: { _truncated: true } }))).toBe(
      'the value could not be serialized',
    );
  });
});
