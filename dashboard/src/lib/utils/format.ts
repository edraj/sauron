// Small presentation helpers shared across pages and components.
import type { IconName } from '../components/ui/Icon.svelte';
import { ingestBaseUrl } from '../config/env';

// --- app types ------------------------------------------------------------

/**
 * Lucide icon name for an app_type, used in switchers and app lists. Lucide has
 * no brand marks, so the mobile platforms share the generic `smartphone` glyph.
 */
export function appTypeIcon(type: string): IconName {
  switch (type) {
    case 'web':
      return 'globe';
    case 'flutter':
    case 'ios':
    case 'android':
    case 'react_native':
      return 'smartphone';
    case 'node':
      return 'server';
    case 'python':
      return 'braces';
    case 'csharp':
      return 'hash';
    default:
      return 'package';
  }
}

/** Human label for an app_type. */
export function appTypeLabel(type: string): string {
  switch (type) {
    case 'web':
      return 'Web';
    case 'flutter':
      return 'Flutter';
    case 'ios':
      return 'iOS';
    case 'android':
      return 'Android';
    case 'react_native':
      return 'React Native';
    case 'node':
      return 'Node.js';
    case 'python':
      return 'Python';
    case 'csharp':
      return 'C#';
    default:
      return type;
  }
}

/** The selectable app types, in menu order. */
export const APP_TYPES: { value: string; label: string }[] = [
  { value: 'web', label: 'Web' },
  { value: 'flutter', label: 'Flutter' },
  { value: 'ios', label: 'iOS' },
  { value: 'android', label: 'Android' },
  { value: 'react_native', label: 'React Native' },
  { value: 'node', label: 'Node.js' },
  { value: 'python', label: 'Python' },
  { value: 'csharp', label: 'C#' },
];

/**
 * Build the ingest DSN for an environment:
 * `http(s)://<public_key>@<ingest_host>/<environment_id>`.
 *
 * The ingest edge authenticates on the key alone and discards this path segment,
 * so the id is documentation rather than routing — but it should name the thing
 * the key actually belongs to.
 */
export function buildDsn(publicKey: string, environmentId: string): string {
  try {
    const u = new URL(ingestBaseUrl);
    return `${u.protocol}//${publicKey}@${u.host}/${environmentId}`;
  } catch {
    // `ingestBaseUrl` failed to parse as a URL, so there is no working DSN to
    // build. This string is not a valid DSN — no SDK can parse it (no scheme,
    // userinfo stuffed into the path) — it exists only so the malformed
    // `INGEST_BASE_URL` / `VITE_INGEST_BASE_URL` value is visible in the UI as
    // a diagnostic, instead of the function throwing or returning nothing.
    return `${ingestBaseUrl}/${publicKey}@${environmentId}`;
  }
}


const RELATIVE_UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ['year', 60 * 60 * 24 * 365],
  ['month', 60 * 60 * 24 * 30],
  ['week', 60 * 60 * 24 * 7],
  ['day', 60 * 60 * 24],
  ['hour', 60 * 60],
  ['minute', 60],
  ['second', 1],
];

const rtf = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

/** "3 minutes ago", "just now", "in 2 hours". */
export function relativeTime(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const then = new Date(input).getTime();
  if (Number.isNaN(then)) return '—';
  const diffSeconds = (then - Date.now()) / 1000;
  const abs = Math.abs(diffSeconds);
  if (abs < 5) return 'just now';
  for (const [unit, secs] of RELATIVE_UNITS) {
    if (abs >= secs || unit === 'second') {
      return rtf.format(Math.round(diffSeconds / secs), unit);
    }
  }
  return 'just now';
}

/** Absolute, human date-time for tooltips / detail rows. */
export function formatDateTime(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Absolute date-time down to the second — for rows where the exact instant is
 * the point, like an issue's occurrence list.
 *
 * Deliberately NOT folded into `formatDateTime`: that one backs the summary
 * fields ("First seen", "Last seen") and a dozen tooltips, where a trailing
 * `:07` is noise. Two call sites with genuinely different precision needs.
 */
export function formatDateTimeSeconds(input: string | number | Date | null | undefined): string {
  return absolute(input, {});
}

/**
 * Same instant, plus the viewer's timezone — tooltip-only, where there is room
 * to spell out which clock the column is showing before someone lines these
 * timestamps up against a server log.
 */
export function formatDateTimeZone(input: string | number | Date | null | undefined): string {
  return absolute(input, { timeZoneName: 'short' });
}

/**
 * `yyyy-MM-DD HH:mm:ss` in the viewer's local time — the absolute half of the
 * TimeValue toggle.
 *
 * Deliberately NOT `toLocaleString`: the other three absolute formatters here
 * are locale-formatted ("Aug 6, 2026, 02:15:07 PM"), which is right for prose
 * but wrong for a value someone is lining up against a log line. This one is
 * fixed-width and sortable, so a column of them reads as a column.
 *
 * Local rather than UTC because `relativeTime` and `formatDateTime` are both
 * local: toggling changes precision, never the instant's apparent value.
 */
export function formatTimestamp(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  const p = (n: number) => String(n).padStart(2, '0');
  return (
    `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
    `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
  );
}

function absolute(
  input: string | number | Date | null | undefined,
  extra: Intl.DateTimeFormatOptions,
): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    ...extra,
  });
}

export function formatTime(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

/**
 * Hoisted out of `compactNumber`. Constructing an `Intl.NumberFormat` is the
 * expensive part — it resolves locale data — while `.format()` on an existing
 * one is cheap. This function is called once per numeric cell, so a 50-row
 * table built one formatter per cell and threw all of them away. The locale is
 * the hardcoded 'en' below, so a single module-level instance is safe: there is
 * no per-call input that could select a different one.
 */
const COMPACT_NUMBER_FORMAT = new Intl.NumberFormat('en', {
  notation: 'compact',
  maximumFractionDigits: 1,
});

/** Compact number: 1_234 -> "1.2k". */
export function compactNumber(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) return '0';
  return COMPACT_NUMBER_FORMAT.format(value);
}

export function plural(count: number, singular: string, pluralForm?: string): string {
  const word = count === 1 ? singular : (pluralForm ?? `${singular}s`);
  return `${count.toLocaleString()} ${word}`;
}

/** Stable-ish hue from an arbitrary string (for avatar / person chips). */
export function hueFromString(value: string): number {
  let hash = 0;
  for (let i = 0; i < value.length; i++) {
    hash = (hash << 5) - hash + value.charCodeAt(i);
    hash |= 0;
  }
  return Math.abs(hash) % 360;
}

export function initials(value: string): string {
  const cleaned = value.replace(/[^a-zA-Z0-9]+/g, ' ').trim();
  if (!cleaned) return '?';
  const parts = cleaned.split(' ');
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

// --- latency & durations ---------------------------------------------------

/** "128 ms", "1.28 s", "<1 ms". For API/screen latencies. */
export function formatMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || Number.isNaN(ms)) return '—';
  if (ms < 1) return '<1 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export type LatencyTone = 'success' | 'warning' | 'error';

/** Color bucket for a latency in ms. Green < good, amber < ok, else red. */
export function latencyTone(ms: number, good = 1000, ok = 3000): LatencyTone {
  if (ms < good) return 'success';
  if (ms < ok) return 'warning';
  return 'error';
}

/** Human session/transaction duration: "8.4s", "3m 12s", "1h 04m". */
export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || Number.isNaN(ms) || ms < 0) return '—';
  const s = ms / 1000;
  if (s < 60) return `${s < 10 ? s.toFixed(1) : Math.round(s)}s`;
  const m = Math.floor(s / 60);
  const remS = Math.round(s % 60);
  if (m < 60) return `${m}m ${remS}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, '0')}m`;
}

/** Milliseconds between two ISO timestamps (end - start). */
export function durationBetween(
  start: string | number | Date,
  end: string | number | Date,
): number {
  return new Date(end).getTime() - new Date(start).getTime();
}

/** "12.3%" from a 0..1 ratio. */
export function formatPercent(value: number | null | undefined, digits = 1): string {
  if (value === null || value === undefined || Number.isNaN(value)) return '—';
  return `${(value * 100).toFixed(digits)}%`;
}
