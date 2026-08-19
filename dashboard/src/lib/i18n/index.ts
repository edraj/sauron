import { localeStore } from './locale.svelte';
import { MESSAGES, PLURALS, type MessageKey, type PluralKey } from './catalog';
import { decimalFormat, pluralRules } from './formatters';
import type { Locale } from './types';

export { localeStore } from './locale.svelte';
export { LOCALES, LOCALE_LABELS, isRtl, intlTag, isLocale, type Locale } from './types';
export type { MessageKey, PluralKey } from './catalog';

/** Substitute `{name}` placeholders. An unmatched name is left as written. */
function interpolate(raw: string, params: Record<string, string | number>): string {
  return raw.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const value = params[name];
    return value === undefined ? whole : String(value);
  });
}

/**
 * Translate `key` into the active locale.
 *
 * Reads `localeStore.locale`, which is `$state` — so every `{t('…')}` in
 * markup re-renders on a language switch without needing a subscription of its
 * own. That is the whole reason this is a plain function rather than a store:
 * Svelte 5's reactivity follows the *read*, wherever it happens.
 *
 * Falls back through Arabic → English → the key itself. The type system
 * already forbids a missing Arabic value, but not an empty one, and an empty
 * string would render as a blank label rather than an obvious defect. English
 * text in an Arabic UI is visible; nothing is not.
 */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const entry = MESSAGES[key] as Record<Locale, string> | undefined;
  const raw = entry?.[localeStore.locale] || entry?.en || key;
  return params ? interpolate(raw, params) : raw;
}

/**
 * Translate a count-dependent string, selecting the grammatical form with
 * `Intl.PluralRules`.
 *
 * `{n}` is pre-bound to the count run through the locale's number formatter,
 * so callers pass a raw number and still get thousands separators. Additional
 * placeholders may be supplied through `params`.
 *
 * The Arabic forms matter more than they look: 1, 2, 3, and 11 take four
 * different words, which is why `PluralMessage` demands all six CLDR
 * categories for `ar` while English gets two.
 */
export function tn(
  key: PluralKey,
  count: number,
  params?: Record<string, string | number>,
): string {
  const locale = localeStore.locale;
  const forms = PLURALS[key]?.[locale] as Record<string, string> | undefined;
  if (!forms) return String(count);
  const category = pluralRules(locale).select(count);
  const raw = forms[category] || forms.other || '';
  return interpolate(raw, { n: decimalFormat(locale).format(count), ...params });
}

/**
 * Format a number in the active locale.
 *
 * Arabic resolves through `ar-u-nu-latn`, so this stays Western-digit — see
 * `intlTag` for why that is pinned rather than left to the engine.
 */
export function formatNumber(value: number): string {
  return decimalFormat(localeStore.locale).format(value);
}

/**
 * Join names into a readable list: "A", "A and B", "A, B and C".
 *
 * The two languages punctuate this differently, and not only in the
 * conjunction: English separates with commas and joins the final pair with
 * "and", while Arabic prefixes every item after the first with و and uses no
 * commas at all. Rendering the English shape with و substituted reads as a
 * comma-spliced list to an Arabic reader, so the shape itself has to switch.
 */
export function joinList(items: string[]): string {
  if (items.length <= 1) return items.join('');
  const and = t('panel.join.and');
  if (localeStore.locale === 'ar') return items.join(` ${and}`);
  return `${items.slice(0, -1).join(', ')} ${and} ${items[items.length - 1]}`;
}
