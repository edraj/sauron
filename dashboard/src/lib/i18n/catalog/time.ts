import type { Message } from '../types';

/**
 * Time wording that `Intl` does not supply.
 *
 * `Intl.RelativeTimeFormat` handles "3 minutes ago" / "قبل 3 دقائق" natively,
 * so only the strings it has no concept of live here — the sub-five-second
 * case, and the labels around the range picker.
 */
export const time = {
  'time.justNow': { en: 'just now', ar: 'الآن' },
  'time.never': { en: 'Never', ar: 'أبدًا' },
  'time.relative': { en: 'Relative', ar: 'نسبي' },
  'time.absolute': { en: 'Absolute', ar: 'مطلق' },
  'time.toggleFormat': { en: 'Toggle time format', ar: 'تبديل تنسيق الوقت' },
  // `{shown}` is the timestamp as currently rendered; the rest tells the user
  // what clicking swaps it to.
  'time.showExact': { en: '{shown} — click to show exact time', ar: '{shown} — انقر لعرض الوقت المضبوط' },
  'time.showRelative': { en: '{shown} — click to show relative time', ar: '{shown} — انقر لعرض الوقت النسبي' },

  'time.range.last15m': { en: 'Last 15 minutes', ar: 'آخر 15 دقيقة' },
  'time.range.last1h': { en: 'Last hour', ar: 'آخر ساعة' },
  'time.range.last24h': { en: 'Last 24 hours', ar: 'آخر 24 ساعة' },
  'time.range.last7d': { en: 'Last 7 days', ar: 'آخر 7 أيام' },
  'time.range.last30d': { en: 'Last 30 days', ar: 'آخر 30 يومًا' },
  'time.range.last90d': { en: 'Last 90 days', ar: 'آخر 90 يومًا' },
  'time.range.custom': { en: 'Custom range', ar: 'نطاق مخصص' },
  'time.to': { en: 'To', ar: 'إلى' },
} as const satisfies Record<string, Message>;
