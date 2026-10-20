import { describe, expect, it } from 'vitest';
import {
  DEFAULT_RELEASE_MODE,
  RELEASE_MODES,
  effectiveReleaseMode,
  isReleaseRollupPending,
  parseReleaseMode,
  releaseModeParams,
  writeReleaseModeParam,
} from './release-users';

describe('parseReleaseMode', () => {
  it('reads the one non-default value', () => {
    expect(parseReleaseMode('current')).toBe('current');
    expect(parseReleaseMode('used')).toBe('used');
  });

  it('degrades anything else to the default rather than sending a 400', () => {
    // The server 400s an unknown `release_mode`; a hand-edited link must not
    // turn into an error card.
    for (const raw of [null, undefined, '', 'CURRENT', 'latest', ' current']) {
      expect(parseReleaseMode(raw), String(raw)).toBe(DEFAULT_RELEASE_MODE);
    }
    expect(DEFAULT_RELEASE_MODE).toBe('used');
  });
});

describe('effectiveReleaseMode', () => {
  it('is the chosen mode while a release is picked', () => {
    expect(effectiveReleaseMode('current', '1.4.0')).toBe('current');
    expect(effectiveReleaseMode('used', '1.4.0')).toBe('used');
    // The Unknown-release sentinel is a picked release like any other.
    expect(effectiveReleaseMode('current', 'none')).toBe('current');
  });

  it('is null with no release, whatever was chosen', () => {
    expect(effectiveReleaseMode('current', null)).toBeNull();
    expect(effectiveReleaseMode('used', null)).toBeNull();
  });
});

describe('releaseModeParams', () => {
  it('sends the mode explicitly while a release is picked', () => {
    expect(releaseModeParams('current')).toEqual({ release_mode: 'current' });
    expect(releaseModeParams('used')).toEqual({ release_mode: 'used' });
  });

  it('sends nothing without a release, so the request is the pre-release one', () => {
    expect(releaseModeParams(null)).toEqual({});
    expect(releaseModeParams(effectiveReleaseMode('current', null))).toEqual({});
  });
});

describe('writeReleaseModeParam', () => {
  it('writes only a non-default mode that is in force', () => {
    expect(writeReleaseModeParam(new URLSearchParams('since_days=7'), 'current').toString()).toBe(
      'since_days=7&release_mode=current',
    );
    expect(writeReleaseModeParam(new URLSearchParams('since_days=7'), 'used').toString()).toBe(
      'since_days=7',
    );
    expect(writeReleaseModeParam(new URLSearchParams(), null).toString()).toBe('');
  });

  it('removes a stale parameter once the mode is no longer in force', () => {
    const p = new URLSearchParams('release_mode=current&time_field=first_seen');
    expect(writeReleaseModeParam(p, null).toString()).toBe('time_field=first_seen');
  });

  it('round-trips through parseReleaseMode', () => {
    for (const mode of RELEASE_MODES) {
      const p = writeReleaseModeParam(new URLSearchParams(), mode);
      expect(parseReleaseMode(p.get('release_mode'))).toBe(mode);
    }
  });
});

describe('isReleaseRollupPending', () => {
  it('matches the backend code exactly', () => {
    expect(isReleaseRollupPending('release_rollup_pending')).toBe(true);
  });

  it('does not match an outage or a missing code', () => {
    // A generic 503 (`http_error` from an envelope-less body, or a real
    // `unavailable`) is an error card, never "still building".
    for (const code of ['http_error', 'unavailable', 'rollup_pending', '', null, undefined]) {
      expect(isReleaseRollupPending(code), String(code)).toBe(false);
    }
  });
});
