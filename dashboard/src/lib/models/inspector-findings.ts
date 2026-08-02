// Grouping, count rendering and badge logic for the Findings tab. Pure.

export interface FindingView {
  id: string;
  app_id: string;
  environment_id: string | null;
  env_scope: string;
  source_table: string;
  source_column: string;
  key_path: string;
  matched_key: string;
  detector: string;
  value_type: string;
  match_count: number;
  match_count_exact: boolean;
  sample_preview: string;
  partition_kind: string;
  last_seen_at: string | null;
}

export interface FindingBadge {
  label: string;
  /** The tooltip. Every badge explains a consequence, not a category. */
  title: string;
}

/** Tables a scan reaches but a mask is never offered for. */
const SCAN_ONLY = new Set(['devices', 'identities', 'workflows']);

export function formatMatchCount(n: number, exact: boolean): string {
  const s = n.toLocaleString();
  // A truncated unit makes every count a LOWER BOUND; rendering it as an
  // exact number would be a quiet lie on a privacy report.
  return exact ? s : `at least ${s}`;
}

export function findingBadges(f: FindingView): FindingBadge[] {
  const out: FindingBadge[] = [];
  if (f.partition_kind === 'rollup') {
    out.push({
      label: 'recurring',
      title:
        'This row is rewritten by every matching event, so an at-rest mask will be undone by the next event. Forward enforcement is what covers it.',
    });
  }
  if (f.partition_kind === 'default') {
    out.push({
      label: 'never ages out',
      title:
        'This row lives in the default partition, which is never exported to cold storage and never dropped. It is the longest-lived copy in the system.',
    });
  }
  if (SCAN_ONLY.has(f.source_table)) {
    out.push({
      label: 'not maskable',
      title:
        f.source_table === 'devices'
          ? 'Every devices column is COALESCE(EXCLUDED.x, devices.x), so a mask would report success and be overwritten by the next event from that device.'
          : f.source_table === 'identities'
            ? 'alias_id and distinct_id ARE the identity graph. Masking them merges every masked person into one rather than redacting anyone.'
            : 'cancel_reason is derived server-side from an analytics event; mask analytics_events.properties instead, which is where the bytes arrive.',
    });
  }
  if (f.env_scope === 'unattributed') {
    out.push({
      label: 'no environment',
      title: 'The platform could not attribute this row to an environment.',
    });
  }
  if (f.env_scope === 'no_env_column') {
    out.push({
      label: 'app-wide table',
      title: 'This table has no environment column at all, so the finding covers the whole app.',
    });
  }
  if (f.detector !== '') {
    out.push({ label: f.detector, title: 'Matched by value shape, not by key name.' });
  }
  return out;
}

export interface FindingGroup {
  key: string;
  table: string;
  column: string;
  total: number;
  findings: FindingView[];
}

export function groupFindings(rows: FindingView[]): FindingGroup[] {
  const byKey = new Map<string, FindingGroup>();
  for (const f of rows) {
    const key = `${f.source_table}.${f.source_column}`;
    let g = byKey.get(key);
    if (!g) {
      g = { key, table: f.source_table, column: f.source_column, total: 0, findings: [] };
      byKey.set(key, g);
    }
    g.findings.push(f);
    g.total += f.match_count;
  }
  const groups = [...byKey.values()];
  for (const g of groups) {
    g.findings.sort((a, b) => b.match_count - a.match_count || a.key_path.localeCompare(b.key_path));
  }
  groups.sort((a, b) => b.total - a.total || a.key.localeCompare(b.key));
  return groups;
}
