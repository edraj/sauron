import { formatNumber, t } from '../i18n';
import { DETECTORS } from '../constants/inspectorDetectors';
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
  /** Stable identity for keyed `{#each}`; never rendered. */
  id: string;
  label: string;
  /** The tooltip. Every badge explains a consequence, not a category. */
  title: string;
}

/** Tables a scan reaches but a mask is never offered for. */
const SCAN_ONLY = new Set(['devices', 'identities', 'workflows']);

export function formatMatchCount(n: number, exact: boolean): string {
  const s = formatNumber(n);
  // A truncated unit makes every count a LOWER BOUND; rendering it as an
  // exact number would be a quiet lie on a privacy report.
  return exact ? s : t('inspector.atLeast', { n: s });
}

/**
 * The badges for one finding, labels and tooltips already translated.
 *
 * Calls `t()`, so call it from markup (as the Findings tab does) to follow a
 * language switch rather than caching the result.
 */
export function findingBadges(f: FindingView): FindingBadge[] {
  const out: FindingBadge[] = [];
  if (f.partition_kind === 'rollup') {
    out.push({
      id: 'recurring',
      label: t('inspector.badge.recurring.label'),
      title: t('inspector.badge.recurring.title'),
    });
  }
  if (f.partition_kind === 'default') {
    out.push({
      id: 'never-ages-out',
      label: t('inspector.badge.neverAgesOut.label'),
      title: t('inspector.badge.neverAgesOut.title'),
    });
  }
  if (SCAN_ONLY.has(f.source_table)) {
    out.push({
      id: 'not-maskable',
      label: t('inspector.badge.notMaskable.label'),
      title: t(
        f.source_table === 'devices'
          ? 'inspector.badge.notMaskable.devices'
          : f.source_table === 'identities'
            ? 'inspector.badge.notMaskable.identities'
            : 'inspector.badge.notMaskable.workflows',
      ),
    });
  }
  if (f.env_scope === 'unattributed') {
    out.push({
      id: 'no-environment',
      label: t('inspector.badge.noEnvironment.label'),
      title: t('inspector.badge.noEnvironment.title'),
    });
  }
  if (f.env_scope === 'no_env_column') {
    out.push({
      id: 'app-wide',
      label: t('inspector.badge.appWide.label'),
      title: t('inspector.badge.appWide.title'),
    });
  }
  if (f.detector !== '') {
    // The detector's name in words ("Credit card"); a detector the dashboard
    // does not know yet keeps its wire id rather than disappearing.
    const known = DETECTORS.find((d) => d.id === f.detector);
    out.push({
      id: `detector:${f.detector}`,
      label: known ? t(known.labelKey) : f.detector,
      title: t('inspector.badge.detector.title'),
    });
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
