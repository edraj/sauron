import { describe, expect, it } from 'vitest';
import { selectableReleases } from './release-switcher';
import type { AppRelease } from './index';

function rel(release: string): AppRelease {
  return {
    release,
    environment_ids: [null],
    first_seen_at: '2026-01-01T00:00:00Z',
    last_seen_at: '2026-01-01T00:00:00Z',
  };
}

function names(...releases: string[]): string[] {
  return selectableReleases(releases.map(rel));
}

describe('selectableReleases', () => {
  it('keeps ordinary releases in the order the API returned them', () => {
    // Order is `last_seen_at DESC` server-side; re-sorting here would put the
    // release you just shipped somewhere down the menu.
    expect(names('2.0.0', '1.9.0', '1.4.0')).toEqual(['2.0.0', '1.9.0', '1.4.0']);
  });

  it('trims surrounding whitespace', () => {
    expect(names('  1.4.0  ', '\t2.0.0\n')).toEqual(['1.4.0', '2.0.0']);
  });

  it('drops a blank or whitespace-only release', () => {
    // Un-droppable in the UI: a row with no visible label, identical to every
    // other blank one.
    expect(names('1.4.0', '', '   ', '\t')).toEqual(['1.4.0']);
  });

  it('drops a release literally named "none"', () => {
    // `'none'` is the wire literal for "no release" and the id of the menu's
    // own "Unknown release" entry; two items with `id === 'none'` make
    // SwitcherMenu's keyed `{#each}` throw and take the whole topbar down.
    expect(names('1.4.0', 'none', '2.0.0')).toEqual(['1.4.0', '2.0.0']);
  });

  it('drops a padded "none" too — trimming happens first', () => {
    expect(names(' none ', '1.4.0')).toEqual(['1.4.0']);
  });

  it('de-duplicates, keeping the first occurrence', () => {
    // Defence in depth only: `routes/releases.rs` folds the catalogue through
    // a BTreeMap keyed on the release, so exact duplicates do not reach us.
    // The case that DOES is the next test.
    expect(names('1.4.0', '2.0.0', '1.4.0')).toEqual(['1.4.0', '2.0.0']);
  });

  it('de-duplicates values that collide only once trimmed', () => {
    // The real reason the dedupe exists: ' 1.4.0' and '1.4.0' are distinct
    // BTreeMap keys server-side and become the same item.id once we trim.
    expect(names('1.4.0', ' 1.4.0')).toEqual(['1.4.0']);
  });

  it('returns an empty list for an app with no usable releases', () => {
    expect(names()).toEqual([]);
    expect(names('', 'none')).toEqual([]);
  });

  it('does not mutate or reorder its input', () => {
    const input = [rel('2.0.0'), rel('none'), rel('1.4.0')];
    const copy = input.map((r) => ({ ...r }));
    selectableReleases(input);
    expect(input).toEqual(copy);
  });
});
