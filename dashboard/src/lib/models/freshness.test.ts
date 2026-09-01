import { describe, expect, it } from 'vitest';
import { approx, combineFreshness, envelopeStatus, rollupChip, viewFreshness } from './freshness';

const NOW = new Date('2026-08-25T12:00:00Z');

describe('rollupChip', () => {
  it('is null while rollups are not serving the app', () => {
    expect(rollupChip(null, NOW)).toBeNull();
    expect(rollupChip({ ready: false, as_of: NOW.toISOString(), sessions_as_of: null }, NOW)).toBeNull();
    expect(rollupChip({ ready: true, as_of: null, sessions_as_of: null }, NOW)).toBeNull();
    expect(rollupChip({ ready: true, as_of: 'garbage', sessions_as_of: null }, NOW)).toBeNull();
  });

  it('labels a fresh watermark neutrally and carries the disclosure in the title', () => {
    const chip = rollupChip(
      { ready: true, as_of: '2026-08-25T11:59:30Z', sessions_as_of: null },
      NOW,
    );
    expect(chip).not.toBeNull();
    expect(chip?.tone).toBe('neutral');
    expect(chip?.label.length).toBeGreaterThan(0);
    expect(chip?.title).toContain('2026-08-25T11:59:30Z');
  });

  it('turns warning-toned when the watermark lags more than five minutes', () => {
    const chip = rollupChip(
      { ready: true, as_of: '2026-08-25T11:54:00Z', sessions_as_of: null },
      NOW,
    );
    expect(chip?.tone).toBe('warning');
  });
});

describe('approx', () => {
  it('prefixes only while rollups are active', () => {
    expect(approx('1,234', true)).toBe('≈1,234');
    expect(approx('1,234', false)).toBe('1,234');
  });
});

describe('viewFreshness', () => {
  const now = new Date('2026-08-31T14:00:00Z');

  it('shows nothing when there is no timestamp of either kind', () => {
    expect(viewFreshness({}, now)).toBeNull();
    expect(viewFreshness({ computedAt: null, fetchedAt: null }, now)).toBeNull();
  });

  /**
   * The honesty rule, and the reason this is a function rather than a template
   * expression. `/active-users` can serve a report Redis has held for hours;
   * the browser fetched that answer seconds ago. Showing the LOCAL stamp there
   * would date a three-hour-old number to "just now" — confidently wrong, in
   * the one place the user is looking to find out how old it is.
   */
  it('prefers the server stamp over the local one', () => {
    const v = viewFreshness(
      {
        computedAt: '2026-08-31T11:00:00Z', // 3h old
        fetchedAt: now.getTime() - 2000, // fetched 2s ago
      },
      now,
    );
    expect(v!.source).toBe('server');
    // Three hours is NOT a warning: `/active-users` serves up to that by
    // design, and a threshold inside the normal range lights permanently on a
    // page behaving exactly as intended. See `DEFAULT_STALE_AFTER_MS`.
    expect(v!.tone).toBe('neutral');
  });

  it('falls back to the local stamp when the endpoint discloses none', () => {
    const v = viewFreshness({ fetchedAt: now.getTime() - 60_000 }, now);
    expect(v!.source).toBe('local');
    expect(v!.tone).toBe('neutral');
  });

  /**
   * A malformed server stamp must not render "as of Invalid Date"; the local
   * clock is a worse answer than the server's but a far better one than NaN.
   */
  it('falls back to local when the server stamp is unparseable', () => {
    const v = viewFreshness({ computedAt: 'not-a-date', fetchedAt: now.getTime() }, now);
    expect(v!.source).toBe('local');
    expect(v!.label).not.toMatch(/invalid/i);
  });

  it('reports a refresh in flight independently of age', () => {
    const fresh = viewFreshness({ fetchedAt: now.getTime(), revalidating: true }, now);
    expect(fresh!.updating).toBe(true);
    expect(fresh!.tone).toBe('neutral');
  });

  it('turns warning once past the staleness threshold', () => {
    const t = now.getTime();
    expect(viewFreshness({ fetchedAt: t - 60_000, staleAfterMs: 120_000 }, now)!.tone).toBe(
      'neutral',
    );
    expect(viewFreshness({ fetchedAt: t - 180_000, staleAfterMs: 120_000 }, now)!.tone).toBe(
      'warning',
    );
  });
});

describe('combineFreshness', () => {
  it('is null until at least one view has data', () => {
    expect(combineFreshness([])).toBeNull();
    expect(combineFreshness([{ fetchedAt: null }, { fetchedAt: null }])).toBeNull();
  });

  /**
   * A page is only as fresh as its stalest section. Reporting the NEWEST stamp
   * would let one cheap section that just refreshed vouch for four expensive
   * ones still showing hour-old numbers.
   */
  it('reports the oldest stamp across the page', () => {
    const got = combineFreshness([
      { fetchedAt: 5_000 },
      { fetchedAt: 1_000 },
      { fetchedAt: 9_000 },
    ]);
    expect(got!.fetchedAt).toBe(1_000);
  });

  it('ignores sections that have not loaded yet', () => {
    const got = combineFreshness([{ fetchedAt: null }, { fetchedAt: 7_000 }]);
    expect(got!.fetchedAt).toBe(7_000);
  });

  it('is updating while any section is revalidating', () => {
    expect(
      combineFreshness([
        { fetchedAt: 1, revalidating: false },
        { fetchedAt: 2, revalidating: true },
      ])!.revalidating,
    ).toBe(true);
    expect(
      combineFreshness([{ fetchedAt: 1, revalidating: false }])!.revalidating,
    ).toBe(false);
  });
});

describe('envelopeStatus', () => {
  it('is computing with nothing to show on a cold read', () => {
    const s = envelopeStatus({ state: 'computing', hasData: false });
    expect(s).toEqual({ error: null, computing: true, shouldPoll: true });
  });

  /**
   * The bug this exists to stop coming back. A failed server-side recompute
   * returns HTTP 200 with `{state:"computing", data:null, error:"…"}`. Read
   * only through the transport error, that is a success with nothing in it —
   * so the page renders a skeleton and polls forever while the reason sits
   * unread in the payload. The server also suppresses re-enqueue behind a
   * failure backoff, so nothing is even retrying.
   */
  it('surfaces a failed recompute and stops polling', () => {
    const s = envelopeStatus({
      state: 'computing',
      hasData: false,
      error: 'relation "event_users" does not exist',
    });
    expect(s.error).toBe('relation "event_users" does not exist');
    expect(s.shouldPoll).toBe(false);
  });

  it('keeps stale data on screen when a refresh fails', () => {
    const s = envelopeStatus({ state: 'stale', hasData: true, error: 'timed out' });
    expect(s.error).toBe('timed out');
    expect(s.computing).toBe(false);
    expect(s.shouldPoll).toBe(false);
  });

  it('prefers the transport error, which means the read never landed', () => {
    const s = envelopeStatus({ state: 'computing', hasData: false, viewError: '503' });
    expect(s.error).toBe('503');
    expect(s.shouldPoll).toBe(false);
  });

  it('does not poll once the data has arrived', () => {
    expect(envelopeStatus({ state: 'fresh', hasData: true }).shouldPoll).toBe(false);
  });

  /** An endpoint that returns no envelope at all is not "computing". */
  it('treats a missing envelope as nothing to wait for', () => {
    expect(envelopeStatus({ hasData: true })).toEqual({
      error: null,
      computing: false,
      shouldPoll: false,
    });
  });
});
