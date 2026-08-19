import { afterEach, describe, expect, it } from 'vitest';
import { t, tn, formatNumber } from './index';
import { localeStore } from './locale.svelte';
import { intlTag, isRtl, isLocale, LOCALES, LOCALE_LABELS } from './types';

afterEach(() => localeStore.set('en'));

describe('t()', () => {
  it('returns the active locale’s text', () => {
    localeStore.set('en');
    expect(t('common.save')).toBe('Save');
    localeStore.set('ar');
    expect(t('common.save')).toBe('حفظ');
  });

  it('substitutes named placeholders', () => {
    localeStore.set('en');
    expect(t('common.backTo', { target: 'Issues' })).toBe('Back to Issues');
    localeStore.set('ar');
    expect(t('common.backTo', { target: 'الاستثناءات' })).toBe('العودة إلى الاستثناءات');
  });

  it('leaves an unsupplied placeholder written rather than printing undefined', () => {
    // A missing parameter is a caller bug. Rendering "Back to {target}" makes
    // it obvious in review; rendering "Back to undefined" looks like data.
    expect(t('common.backTo')).toBe('Back to {target}');
  });

  it('substitutes a numeric parameter', () => {
    expect(t('common.backTo', { target: 42 })).toBe('Back to 42');
  });
});

describe('tn()', () => {
  it('selects English singular and plural', () => {
    localeStore.set('en');
    expect(tn('common.plural.event', 1)).toBe('1 event');
    expect(tn('common.plural.event', 5)).toBe('5 events');
  });

  it('formats the count with thousands separators', () => {
    localeStore.set('en');
    expect(tn('common.plural.event', 1234)).toBe('1,234 events');
  });

  /**
   * The reason `PluralMessage` demands six Arabic forms. Each of these
   * numbers lands in a different CLDR category and takes a different word;
   * an `n === 1 ? singular : plural` scheme gets five of the six wrong.
   */
  it('selects the right Arabic form for each category', () => {
    localeStore.set('ar');
    expect(tn('common.plural.event', 0)).toBe('لا أحداث'); // zero
    expect(tn('common.plural.event', 1)).toBe('حدث واحد'); // one
    expect(tn('common.plural.event', 2)).toBe('حدثان'); // two
    expect(tn('common.plural.event', 3)).toBe('3 أحداث'); // few
    expect(tn('common.plural.event', 11)).toBe('11 حدثًا'); // many
    expect(tn('common.plural.event', 100)).toBe('100 حدث'); // other
  });
});

describe('number formatting', () => {
  /**
   * The decision that makes this an observability dashboard rather than a
   * localised brochure: Arabic keeps Western digits, so counts, durations and
   * IDs stay comparable with server logs and pasteable into issue trackers.
   *
   * Bare `'ar'` does NOT guarantee this — an engine resolving toward `ar-EG`
   * yields `١٬٢٣٤`. `intlTag` pins `-u-nu-latn` precisely to stop that, and
   * this is the test that would catch its removal.
   */
  it('keeps Western digits in Arabic', () => {
    localeStore.set('ar');
    expect(formatNumber(1234567)).toMatch(/^[\d,.٬˙]*$/);
    expect(formatNumber(1234567)).not.toMatch(/[٠-٩]/);
  });

  it('pins the Latin numbering system on the Arabic tag', () => {
    expect(intlTag('ar')).toBe('ar-u-nu-latn');
    expect(intlTag('en')).toBe('en');
  });
});

describe('locale helpers', () => {
  it('marks only Arabic as right-to-left', () => {
    expect(isRtl('ar')).toBe(true);
    expect(isRtl('en')).toBe(false);
  });

  it('narrows unknown values', () => {
    expect(isLocale('ar')).toBe(true);
    expect(isLocale('fr')).toBe(false);
    expect(isLocale(null)).toBe(false);
    expect(isLocale(undefined)).toBe(false);
  });

  it('labels every locale in its own script', () => {
    for (const locale of LOCALES) {
      expect(LOCALE_LABELS[locale]).toBeTruthy();
    }
    // Written in the language it names, so a user stranded in the wrong
    // locale can still recognise the way out.
    expect(LOCALE_LABELS.ar).toBe('العربية');
  });
});
