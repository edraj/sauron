import { intlTag, type Locale } from './types';

/**
 * Memoized `Intl` instances, keyed by locale.
 *
 * Constructing an `Intl` formatter resolves locale data and is the expensive
 * part; `.format()` on an existing one is cheap. `format.ts` hoisted single
 * instances to module scope for exactly that reason — a 50-row table was
 * building one formatter per numeric cell and discarding all of them.
 *
 * That hoist was only sound while the locale was the hardcoded `'en'`. Now
 * that it moves at runtime, a module-level constant would pin every table to
 * whichever locale happened to be active at *import* time, and switching
 * language would silently leave the numbers behind. Keying a small cache by
 * locale keeps the optimisation without freezing the locale — the map holds at
 * most one entry per shipped language.
 */

const relativeCache = new Map<Locale, Intl.RelativeTimeFormat>();
const compactCache = new Map<Locale, Intl.NumberFormat>();
const decimalCache = new Map<Locale, Intl.NumberFormat>();
const pluralCache = new Map<Locale, Intl.PluralRules>();

export function relativeFormat(locale: Locale): Intl.RelativeTimeFormat {
  let f = relativeCache.get(locale);
  if (!f) {
    f = new Intl.RelativeTimeFormat(intlTag(locale), { numeric: 'auto' });
    relativeCache.set(locale, f);
  }
  return f;
}

export function compactFormat(locale: Locale): Intl.NumberFormat {
  let f = compactCache.get(locale);
  if (!f) {
    f = new Intl.NumberFormat(intlTag(locale), {
      notation: 'compact',
      maximumFractionDigits: 1,
    });
    compactCache.set(locale, f);
  }
  return f;
}

export function decimalFormat(locale: Locale): Intl.NumberFormat {
  let f = decimalCache.get(locale);
  if (!f) {
    f = new Intl.NumberFormat(intlTag(locale));
    decimalCache.set(locale, f);
  }
  return f;
}

/**
 * Plural category selector.
 *
 * Built from the *language* tag rather than the numbering-system-pinned one —
 * `ar-u-nu-latn` and `ar` select identically, but the plain tag is what the
 * category names document.
 */
export function pluralRules(locale: Locale): Intl.PluralRules {
  let r = pluralCache.get(locale);
  if (!r) {
    r = new Intl.PluralRules(locale);
    pluralCache.set(locale, r);
  }
  return r;
}
