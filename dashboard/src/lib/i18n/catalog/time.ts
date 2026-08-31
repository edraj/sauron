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
  // --- the date-range picker -----------------------------------------------
  // `dateRange.*`, distinct from the `time.range.*` labels above: those name
  // the four preset chips, these name the custom-window control that sits
  // beside them.
  'dateRange.custom': { en: 'Custom', ar: 'مخصص' },
  'dateRange.open': { en: 'Choose a custom date range', ar: 'اختر نطاقًا زمنيًا مخصصًا' },
  'dateRange.mode.day': { en: 'Day', ar: 'يوم' },
  'dateRange.mode.week': { en: 'Week', ar: 'أسبوع' },
  'dateRange.mode.month': { en: 'Month', ar: 'شهر' },
  'dateRange.mode.range': { en: 'Range', ar: 'نطاق' },
  'dateRange.prevMonth': { en: 'Previous month', ar: 'الشهر السابق' },
  'dateRange.nextMonth': { en: 'Next month', ar: 'الشهر التالي' },
  'dateRange.selectMonth': { en: 'Select this whole month', ar: 'اختر هذا الشهر بالكامل' },
  'dateRange.selectWeek': { en: 'Select this week', ar: 'اختر هذا الأسبوع' },
  'dateRange.pickStart': { en: 'Pick the first day', ar: 'اختر اليوم الأول' },
  'dateRange.pickEnd': { en: 'Pick the last day', ar: 'اختر اليوم الأخير' },
  'dateRange.saved': { en: 'Saved ranges', ar: 'النطاقات المحفوظة' },
  'dateRange.saveThis': { en: 'Save this range', ar: 'احفظ هذا النطاق' },
  'dateRange.namePrompt': { en: 'Name this range', ar: 'سمِّ هذا النطاق' },
  'dateRange.removeSaved': { en: 'Remove this saved range', ar: 'احذف هذا النطاق المحفوظ' },
  'dateRange.clear': { en: 'Back to a preset', ar: 'العودة إلى نطاق جاهز' },
  // Shown when a would-be selection exceeds what the server will serve. The
  // server refuses rather than narrows an explicit window, so this is what
  // stops the page from 400ing on every request.
  'dateRange.tooWide': {
    en: 'A custom range covers at most {days} days',
    ar: 'النطاق المخصص يغطي {days} يومًا كحد أقصى',
  },
  'dateRange.future': { en: 'Dates after today hold no data yet', ar: 'التواريخ بعد اليوم لا تحتوي على بيانات بعد' },
  // Rollup freshness chip + the ≈ approximation disclosure (docs/approximate-analytics.md).
  // Shared by every cached view's freshness chip. `funnels.updating` and
  // `retention.updating` predate this and say the same thing per page.
  'time.updating': { en: 'Updating…', ar: 'جارٍ التحديث…' },
  'time.asOf': { en: 'as of {time}', ar: 'حتى {time}' },
  'time.approxNote': {
    en: 'Figures marked ≈ are approximate (±~2%), computed from sketches for speed at scale. Unmarked figures are exact.',
    ar: 'الأرقام المميزة بعلامة ≈ تقريبية (±~2%) وتُحسب من ملخصات إحصائية للسرعة على نطاق واسع. الأرقام غير المميزة دقيقة.',
  },
} as const satisfies Record<string, Message>;
