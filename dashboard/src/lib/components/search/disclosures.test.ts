import { describe, it, expect } from 'vitest';
import { disclosuresFor } from './disclosures';

describe('disclosuresFor', () => {
  it('says nothing when nothing was narrowed', () => {
    expect(disclosuresFor(null, null)).toEqual([]);
    expect(disclosuresFor(null, true)).toEqual([]);
  });

  it('names the window ACTUALLY SERVED and the reason it bound', () => {
    const msgs = disclosuresFor(
      {
        field: 'last_seen',
        to: '30d',
        reason: 'unindexed predicate requires a bounded time window',
      },
      null,
    );
    expect(msgs).toHaveLength(1);
    expect(msgs[0].text).toContain('30d');
    expect(msgs[0].text).toContain('unindexed predicate');
    expect(msgs[0].tone).toBe('warning');
  });

  it('reports a narrowed payload search only when it is false', () => {
    // `null` means no search ran; `true` means it ran in full. Only `false` —
    // "it ran and quietly matched less than you think" — is worth a line.
    expect(disclosuresFor(null, false)).toHaveLength(1);
    expect(disclosuresFor(null, false)[0].text).toContain('event:read');
    expect(disclosuresFor(null, true)).toHaveLength(0);
    expect(disclosuresFor(null, null)).toHaveLength(0);
  });

  it('renders both when both are true', () => {
    const msgs = disclosuresFor({ field: 'occurred_at', to: '7d', reason: 'bounded window' }, false);
    expect(msgs).toHaveLength(2);
  });

  it('treats undefined exactly as null, so a page that omits a prop is silent', () => {
    expect(disclosuresFor(undefined, undefined)).toEqual([]);
  });
});
