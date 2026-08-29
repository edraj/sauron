/**
 * Computed reading of the retention data — the "so what" a product person
 * would otherwise derive by squinting at the grid and the lifecycle bars.
 *
 * Pure and framework-free (see `retention.ts`'s header for why): every
 * statement is a key into the i18n catalog plus preformatted params, so the
 * page renders them with `t()` and this file stays testable without a DOM.
 *
 * Each insight states a FACT computed from the loaded data, never a guess:
 * anything without enough data to support it is skipped rather than hedged.
 */
import type { Cohort, LifecyclePoint } from './retention';
import { retentionRate } from './retention';

export type InsightTone = 'bad' | 'warn' | 'good' | 'info';

export interface Insight {
  tone: InsightTone;
  /** i18n key under `retention.insight.*`. */
  key: string;
  params?: Record<string, string>;
  /**
   * i18n key for the recommended next step, under `retention.action.*`.
   *
   * Required, not optional: a finding without an action is the "so what?"
   * problem this card exists to solve, and making the field optional is how
   * one silently ships without one. `actionFor` derives it from the finding's
   * own name so the two cannot drift.
   */
  actionKey: string;
  /**
   * Where to go to act on it. Absent when the next step is on THIS page (the
   * error-split toggle) — a link is only offered when leaving actually helps.
   */
  link?: InsightLink;
}

export interface InsightLink {
  /** A route from `PAGE_ACCESS` — the page that answers the next question. */
  route: string;
  /** i18n key for the link text, under `retention.actionLink.*`. */
  labelKey: string;
}

/**
 * Which page each finding sends you to, or `null` when the next step is on
 * the Retention page itself.
 *
 * A table rather than per-branch literals: it is the one place to check that
 * every finding has an action, and `retention-insights.test.ts` asserts every
 * route here exists in `PAGE_ACCESS` — a typo'd route would otherwise render
 * a link that 404s to the router's catch-all.
 */
const ACTION_LINKS: Record<string, string | null> = {
  // The three day-1 branches all send you to the ERROR SPLIT on this page
  // rather than off to another one. Only one of the three is ever emitted, so
  // they never appear together, and the split is the only control that redraws
  // these exact cohorts by period-0 error exposure — the check no other cohort
  // tool in the product can make.
  day1Down: null,
  day1Up: null,
  day1Flat: null,
  churnReplace: '/users',
  quickGood: '/users',
  // The next step is the errors column of the at-risk table below — on this
  // page, no new query.
  quickBad: null,
  // Events, not Exceptions: "nobody was active" already means no
  // person-attributed events, so the discriminating question is whether ANY
  // traffic arrived that period (an ingest gap) or genuinely none did.
  cliff: '/events',
  bestCohort: '/journeys',
};

/** The short name of a finding: `retention.insight.cliff` -> `cliff`. */
function shortName(key: string): string {
  return key.slice(key.lastIndexOf('.') + 1);
}

/**
 * Attach the recommendation to a finding.
 *
 * Keys are derived from the finding's own name — `retention.insight.cliff`
 * yields `retention.action.cliff` and `retention.actionLink.cliff` — so a new
 * finding cannot ship with another finding's advice attached to it.
 */
function withAction(insight: Omit<Insight, 'actionKey' | 'link'>): Insight {
  const name = shortName(insight.key);
  const route = ACTION_LINKS[name] ?? null;
  return {
    ...insight,
    actionKey: `retention.action.${name}`,
    ...(route === null ? {} : { link: { route, labelKey: `retention.actionLink.${name}` } }),
  };
}

/**
 * The link to render for a finding, or `null` when there is none to offer.
 *
 * `canAccess` is injected rather than imported so this stays testable on the
 * node environment (the same reason `cellLabel` takes its formatter) — and so
 * the gating is covered by a test at all. A user without the permission for
 * the target page keeps the ADVICE and loses only the shortcut: hiding the
 * whole recommendation would withhold the analysis over a missing grant,
 * while linking anyway lands them on a permission-denied screen.
 */
export function insightLink(
  insight: Insight,
  canAccess: (route: string) => boolean,
): InsightLink | null {
  if (!insight.link) return null;
  return canAccess(insight.link.route) ? insight.link : null;
}

const TONE_ORDER: Record<InsightTone, number> = { bad: 0, warn: 1, good: 2, info: 3 };

function pct(x: number): string {
  return `${Math.round(x * 100)}%`;
}

/** Day-1 (first period after joining) rate per cohort, oldest first. */
function dayOneRates(cohorts: Cohort[]): { start: string; rate: number }[] {
  const out: { start: string; rate: number }[] = [];
  for (const c of cohorts) {
    const rate = retentionRate(c.periods[1] ?? null, c.size);
    if (rate !== null) out.push({ start: c.start, rate });
  }
  return out;
}

/**
 * The analysis. `cohorts` may be empty (not ready, no data) and `points`
 * likewise — every metric degrades to "skip", so the caller can render
 * whatever comes back without emptiness checks of its own.
 */
export function retentionInsights(cohorts: Cohort[], points: LifecyclePoint[]): Insight[] {
  const out: Insight[] = [];

  // --- Day-1 retention level and direction --------------------------------
  const d1 = dayOneRates(cohorts);
  if (d1.length >= 2) {
    const avg = d1.reduce((s, r) => s + r.rate, 0) / d1.length;
    const half = Math.floor(d1.length / 2);
    const older = d1.slice(0, half);
    const newer = d1.slice(d1.length - half);
    const mean = (xs: { rate: number }[]) => xs.reduce((s, r) => s + r.rate, 0) / xs.length;
    const olderMean = mean(older);
    const delta = mean(newer) - olderMean;
    // Direction needs BOTH yardsticks: 1.5 absolute points is noise at these
    // sample sizes for high-retention products, but a 3% -> 2% slide — a
    // third of the base, invisible in absolute points — is exactly what a
    // low-retention product needs flagged. Relative change over 20% counts
    // when it moves at least half a point, so a 0.10% -> 0.08% wiggle stays
    // flat.
    const rel = olderMean > 0 ? delta / olderMean : 0;
    const moved = (d: number, r: number) => d > 0.015 || (d > 0.005 && r > 0.2);
    if (moved(delta, rel)) {
      out.push(
        withAction({
          tone: 'good',
          key: 'retention.insight.day1Up',
          params: { pct: pct(avg), delta: pct(delta) },
        }),
      );
    } else if (moved(-delta, -rel)) {
      out.push(
        withAction({
          tone: 'bad',
          key: 'retention.insight.day1Down',
          params: { pct: pct(avg), delta: pct(-delta) },
        }),
      );
    } else {
      out.push(withAction({ tone: 'info', key: 'retention.insight.day1Flat', params: { pct: pct(avg) } }));
    }
  }

  // --- Composition: how much of each period's activity is brand new -------
  const active = points.filter((p) => p.new_users + p.returning_users + p.resurrected_users > 0);
  if (active.length > 0) {
    const newShare =
      active.reduce(
        (s, p) => s + p.new_users / (p.new_users + p.returning_users + p.resurrected_users),
        0,
      ) / active.length;
    if (newShare >= 0.8) {
      out.push(
        withAction({
          tone: 'warn',
          key: 'retention.insight.churnReplace',
          params: { pct: pct(newShare) },
        }),
      );
    }
  }

  // --- Quick ratio: gained vs lost ----------------------------------------
  const gained = points.reduce((s, p) => s + p.new_users + p.resurrected_users, 0);
  const lost = points.reduce((s, p) => s + p.dormant_users, 0);
  if (lost > 0 && gained + lost > 0) {
    const ratio = gained / lost;
    out.push(
      withAction(
        ratio >= 1
          ? {
              tone: 'good',
              key: 'retention.insight.quickGood',
              params: { ratio: ratio.toFixed(1) },
            }
          : {
              tone: 'bad',
              key: 'retention.insight.quickBad',
              params: { ratio: ratio.toFixed(1) },
            },
      ),
    );
  }

  // --- The cliff: a period in which everyone went silent ------------------
  const cliff = points.find(
    (p) => p.new_users + p.returning_users + p.resurrected_users === 0 && p.dormant_users > 0,
  );
  if (cliff) {
    out.push(withAction({ tone: 'bad', key: 'retention.insight.cliff', params: { date: cliff.start } }));
  }

  // --- Best cohort worth copying ------------------------------------------
  const best = d1
    .filter((r) => (cohorts.find((c) => c.start === r.start)?.size ?? 0) >= 5)
    .reduce<{ start: string; rate: number } | null>(
      (m, r) => (m === null || r.rate > m.rate ? r : m),
      null,
    );
  if (best && best.rate > 0) {
    out.push(
      withAction({
        tone: 'info',
        key: 'retention.insight.bestCohort',
        params: { date: best.start, pct: pct(best.rate) },
      }),
    );
  }

  return out.sort((a, b) => TONE_ORDER[a.tone] - TONE_ORDER[b.tone]);
}
