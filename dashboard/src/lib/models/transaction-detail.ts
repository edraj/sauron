import { formatNumber } from '../i18n';
/**
 * The label/value rows that describe a single span, and the SDK-truncation
 * probe that goes with them.
 *
 * Extracted from `Transactions.svelte` when the Performance drill-down modal
 * grew the same panel. Two hand-written copies of "which fields of a span are
 * safe to show" is how one of them starts rendering `ip_address` — the field
 * the API masks — six months after the other decided not to. There is one list
 * and both surfaces read it.
 *
 * Lives in `models/` rather than beside the component for the reason
 * `performance-sort.ts` gives: vitest runs on the node environment and cannot
 * import a `.svelte` file, so a list written inline in the markup is
 * untestable — and the interesting mistakes here are omissions, which a test
 * over the row set can see and a compiler cannot.
 */
import type { Transaction } from './index';

export interface DetailRow {
  label: string;
  /** `null` renders an em dash — the field is genuinely absent on this span. */
  value: string | null;
  href?: string;
  mono?: boolean;
  /**
   * Span the whole panel instead of taking one grid column.
   *
   * For the one field with no useful upper bound. A query string with a dozen
   * parameters wraps to eight lines in a third of the panel and to two across
   * all of it; every other field here is a uuid, a timestamp or a short label,
   * all of which fit the narrow column (measured: uuid 271px, ISO timestamp
   * 203px, value column 283px).
   */
  wide?: boolean;
}

/**
 * Every stored field of a span, as label/value pairs for the detail panel.
 *
 * Deliberately hand-written rather than iterating the object's keys. A
 * key-walk would render `id`, `app_id` and `restored_pin_id` beside `url`
 * with equal weight, invent labels from column names, and — the part that
 * matters — silently start displaying whatever column the table grows next,
 * including one nobody decided was safe to show. This list is the decision.
 *
 * `ip_address` is omitted on purpose: the API already masks it
 * (`serialize_masked_ip`) and nulls it for a caller without `event:read`, so
 * the value here is at best a truncated address and at worst a blank field
 * that reads as "no IP recorded".
 */
export function detailRows(t: Transaction): DetailRow[] {
  return [
    { label: 'Name', value: t.name, mono: true },
    { label: 'Operation', value: t.op },
    { label: 'Duration', value: `${formatNumber(t.duration_ms)} ms` },
    { label: 'Status', value: t.status },
    { label: 'HTTP method', value: t.http_method },
    { label: 'HTTP status', value: t.http_status == null ? null : String(t.http_status) },
    { label: 'URL', value: t.url, mono: true, wide: true },
    {
      label: 'User',
      value: t.distinct_id,
      href: t.distinct_id ? `#/persons/${encodeURIComponent(t.distinct_id)}` : undefined,
      mono: true,
    },
    {
      label: 'Session',
      value: t.session_id,
      href: t.session_id ? `#/sessions/${encodeURIComponent(t.session_id)}` : undefined,
      mono: true,
    },
    {
      label: 'Device',
      value: t.device_key,
      href: t.device_key ? `#/devices/${encodeURIComponent(t.device_key)}` : undefined,
      mono: true,
    },
    { label: 'Release', value: t.release, mono: true },
    { label: 'Workflow', value: t.workflow_name },
    { label: 'Occurred at', value: t.occurred_at, mono: true },
    // Both timestamps, always. The GAP between them is the interesting
    // number on a mobile SDK — a span that occurred hours before it arrived
    // came out of an offline queue, or off a device with a skewed clock, and
    // either fact changes how you read the one above.
    { label: 'Received at', value: t.received_at, mono: true },
    { label: 'Finished at', value: t.finished_at, mono: true },
    { label: 'Transaction id', value: t.id, mono: true },
  ];
}

/** The SDK capped this payload — the span is real, the blob is a marker. */
export function isTruncated(t: Transaction): boolean {
  return t.extra?._truncated === true;
}

/**
 * How many bytes the SDK dropped, as prose, or `null` when it could not say.
 *
 * A negative `_bytes` is the SDK's marker for "this value could not be
 * serialized at all", which is a different fact from "it was too big" and
 * reads wrong as `-1 bytes`.
 */
export function truncatedBytesLabel(t: Transaction): string {
  const bytes = t.extra?._bytes;
  if (typeof bytes !== 'number' || bytes < 0) return 'the value could not be serialized';
  return `${formatNumber(bytes)} bytes`;
}
