import { describe, it, expect } from 'vitest';
import {
  describeKind,
  describeRecovery,
  shortFingerprint,
  shortMessage,
  statusTone,
  wasAutoRetried,
} from './ingest-failures';

describe('describeRecovery', () => {
  // The reason this function exists. A capped group can replay only what was
  // retained, and an operator reading "Retry" as "recover everything" will
  // believe a mass failure was resolved when most of it is permanently gone.
  it('states the unrecoverable count when the cap was exceeded', () => {
    const r = describeRecovery({ occurrences: 242700, retained: 1000, dropped: 241700 });
    expect(r.level).toBe('partial');
    expect(r.summary).toContain('241,700');
    expect(r.summary).toMatch(/cannot be recovered/);
  });

  it('does not imply a loss when everything was retained', () => {
    const r = describeRecovery({ occurrences: 3, retained: 3, dropped: 0 });
    expect(r.level).toBe('full');
    expect(r.summary).not.toMatch(/cannot be recovered/);
    expect(r.summary).toContain('3 events');
  });

  it('says plainly when nothing can be replayed', () => {
    const r = describeRecovery({ occurrences: 12, retained: 0, dropped: 12 });
    expect(r.level).toBe('none');
    expect(r.summary).toContain('12');
    expect(r.summary).toMatch(/nothing can be replayed/);
  });

  // A single retained event must not read as "1 events".
  it('agrees in number', () => {
    expect(describeRecovery({ occurrences: 1, retained: 1, dropped: 0 }).summary).toContain(
      '1 event',
    );
    expect(describeRecovery({ occurrences: 1, retained: 1, dropped: 0 }).summary).not.toContain(
      '1 events',
    );
  });

  // Defensive: `dropped` is derived server-side with GREATEST(.., 0), but a
  // negative arriving here must not produce a "-5 events cannot be recovered".
  it('treats a non-positive dropped count as no loss', () => {
    const r = describeRecovery({ occurrences: 2, retained: 5, dropped: -3 });
    expect(r.level).toBe('full');
  });
});

describe('describeKind', () => {
  it('labels the known slugs', () => {
    expect(describeKind('decode')).toBe('Malformed payload');
    expect(describeKind('db_fk_violation')).toBe('Unknown reference');
  });

  // A new backend slug must show through rather than render as blank.
  it('falls back to the raw slug', () => {
    expect(describeKind('something_new')).toBe('something_new');
  });
});

describe('wasAutoRetried', () => {
  it('matches the classifier: only transient kinds were retried', () => {
    expect(wasAutoRetried('db_contention')).toBe(true);
    expect(wasAutoRetried('db_unavailable')).toBe(true);
    expect(wasAutoRetried('redis')).toBe(true);
    expect(wasAutoRetried('decode')).toBe(false);
    expect(wasAutoRetried('unknown')).toBe(false);
  });
});

describe('statusTone', () => {
  it('maps each status', () => {
    // `error`, not `danger`: Badge's Tone union has no `danger`, and an
    // unknown tone renders as unstyled text rather than failing loudly.
    expect(statusTone('failed')).toBe('error');
    expect(statusTone('requeued')).toBe('warning');
    expect(statusTone('resolved')).toBe('success');
    expect(statusTone('who-knows')).toBe('neutral');
  });
});

describe('shortMessage', () => {
  it('collapses whitespace and truncates', () => {
    expect(shortMessage('a\n  b\tc')).toBe('a b c');
    expect(shortMessage('x'.repeat(200)).length).toBe(120);
  });

  it('leaves short messages untouched', () => {
    expect(shortMessage('too long?')).toBe('too long?');
  });
});

describe('shortFingerprint', () => {
  it('takes the first 8 characters', () => {
    expect(shortFingerprint('0123456789abcdef')).toBe('01234567');
  });
});
