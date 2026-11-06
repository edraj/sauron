import { describe, expect, it } from 'vitest';
import { labelFor } from '../i18n';
import { LEVEL_KEYS, STATUS_KEYS } from './issue-labels';

describe('issue labels', () => {
  it('names every issue level the API sends', () => {
    expect(['fatal', 'error', 'warning', 'info', 'debug'].map((l) => labelFor(LEVEL_KEYS, l))).toEqual([
      'Fatal',
      'Error',
      'Warning',
      'Info',
      'Debug',
    ]);
  });

  it('names every issue status the API sends', () => {
    expect(['unresolved', 'resolved', 'ignored'].map((s) => labelFor(STATUS_KEYS, s))).toEqual([
      'Unresolved',
      'Resolved',
      'Ignored',
    ]);
  });
});
