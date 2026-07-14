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
 * Build the ingest DSN for an app: http(s)://<public_key>@<ingest_host>/<app_id>.
 * Falls back to a path form if the ingest base URL can't be parsed.
 */
export function buildDsn(publicKey: string, appId: string): string {
  try {
    const u = new URL(ingestBaseUrl);
    return `${u.protocol}//${publicKey}@${u.host}/${appId}`;
  } catch {
    return `${ingestBaseUrl}/${publicKey}/${appId}`;
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

export function formatTime(input: string | number | Date | null | undefined): string {
  if (input === null || input === undefined) return '—';
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

/** Compact number: 1_234 -> "1.2k". */
export function compactNumber(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) return '0';
  return new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 }).format(
    value,
  );
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
