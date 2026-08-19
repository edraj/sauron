/**
 * Locale identifiers, message shapes, and the pure helpers that derive
 * direction and `Intl` tags from a locale.
 *
 * Deliberately rune-free so that `formatters.ts`, the catalogue, and the tests
 * can import these without pulling in the `$state` store.
 */

/** The locales the dashboard ships. */
export type Locale = 'en' | 'ar';

/** Selectable locales, in switcher order. */
export const LOCALES = ['en', 'ar'] as const satisfies readonly Locale[];

/**
 * The switcher's own labels.
 *
 * Each is written in its own script rather than translated, so a user who
 * lands in the wrong language can still find their way out: someone who reads
 * only Arabic needs to recognise "العربية", not the Arabic word for "Arabic"
 * rendered in English.
 */
export const LOCALE_LABELS: Record<Locale, string> = {
  en: 'English',
  ar: 'العربية',
};

/** Right-to-left locales. Arabic is the only one today. */
const RTL_LOCALES: ReadonlySet<string> = new Set<Locale>(['ar']);

export function isRtl(locale: Locale): boolean {
  return RTL_LOCALES.has(locale);
}

/**
 * The BCP-47 tag handed to `Intl`.
 *
 * Arabic pins `-u-nu-latn` — the Latin numbering system — on purpose. Bare
 * `'ar'` lets the engine choose, and one resolving toward `ar-EG` renders
 * Arabic-Indic digits (`١٢٣`). This is an observability dashboard: counts,
 * durations, and IDs get compared against server logs and pasted into issue
 * trackers, so the digits must stay Western whatever the engine decides.
 *
 * Node's ICU happens to return Latin digits for bare `'ar'`, which is exactly
 * why this cannot be left implicit — a unit test would pass while browsers
 * disagreed.
 */
export function intlTag(locale: Locale): string {
  return locale === 'ar' ? 'ar-u-nu-latn' : 'en';
}

export function isLocale(value: unknown): value is Locale {
  return value === 'en' || value === 'ar';
}

/**
 * One translatable string, both languages side by side.
 *
 * Co-location is what makes a missing translation a *compile* error instead of
 * a runtime fallback nobody notices: the type requires both fields, so an
 * English string cannot be added without filling the Arabic slot. It also puts
 * the two variants on adjacent lines for whoever revises the wording later.
 */
export type Message = Record<Locale, string>;

/**
 * A count-dependent string.
 *
 * English needs two forms. Arabic needs all six CLDR categories, because
 * `Intl.PluralRules('ar')` genuinely returns `zero`, `one`, `two`, `few`,
 * `many`, and `other` — for 0, 1, 2, 3, 11, and 100 respectively. Supplying
 * only `one`/`other` for Arabic yields text that is grammatically wrong for
 * most inputs, so the type demands the full set.
 */
export interface PluralMessage {
  en: { one: string; other: string };
  ar: {
    zero: string;
    one: string;
    two: string;
    few: string;
    many: string;
    other: string;
  };
}
