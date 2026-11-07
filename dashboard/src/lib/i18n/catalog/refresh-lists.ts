import type { Message } from '../types';

/**
 * Strings added by the Quiet refresh for the lists pages (batch A).
 *
 * A separate file so the four page batches, done in parallel, never edit the
 * same catalog file. Keys follow the usual domain prefixes; a key must still
 * be unique across every catalog file (catalog.test.ts).
 */
export const refreshA = {
  // --- page titles that used to borrow another context's key -------------
  'events.title': { en: 'Events', ar: 'الأحداث' },
  'sessions.title': { en: 'Sessions', ar: 'الجلسات' },

  // --- {things}/{thing} for the shared empty/error wordings ----------------
  // Lowercase plurals, read inside "No {things} match these filters".
  'issues.things': { en: 'exceptions', ar: 'استثناءات' },
  'events.things': { en: 'events', ar: 'أحداث' },
  'sessions.things': { en: 'sessions', ar: 'جلسات' },
  'screens.things': { en: 'screens', ar: 'شاشات' },
  'devices.things': { en: 'devices', ar: 'أجهزة' },
  'overview.things.issues': { en: 'issues', ar: 'استثناءات' },
  'overview.things.errors': { en: 'errors', ar: 'أخطاء' },

  // --- overview ------------------------------------------------------------
  'overview.subtitle': {
    en: 'Health and activity at a glance for {range}.',
    ar: 'نظرة سريعة على صحة التطبيق ونشاطه خلال {range}.',
  },
  'overview.range.last': { en: 'the last {n} days', ar: 'آخر {n} يومًا' },
  'overview.computing': { en: 'Computing…', ar: 'جارٍ الحساب…' },
  'overview.recompute': { en: 'Recompute now', ar: 'أعد الحساب الآن' },
  'overview.stat.errorRateSub': { en: '{pct} error rate', ar: 'معدل الأخطاء {pct}' },
  'overview.stat.newUserShare': { en: '{pct} of users', ar: '{pct} من المستخدمين' },
  'overview.stat.crashedSub': { en: '{n} crashed', ar: '{n} متعطلة' },
  'overview.stat.errorRateFormula': { en: 'errors / events', ar: 'الأخطاء / الأحداث' },
  'overview.empty.activeUsers': {
    en: 'No identified activity in this time range',
    ar: 'لا يوجد نشاط لمستخدمين معروفين في هذا النطاق الزمني',
  },
  'overview.partialDays.one': { en: '1 day omitted: {reason}', ar: 'استُبعد يوم واحد: {reason}' },
  'overview.partialDays.many': { en: '{n} days omitted: {reason}', ar: 'استُبعدت {n} أيام: {reason}' },

  // --- exceptions list -----------------------------------------------------
  'issues.error.load': { en: "Couldn't load exceptions", ar: 'تعذّر تحميل الاستثناءات' },
  'issues.empty.unresolved': { en: 'No unresolved exceptions', ar: 'لا توجد استثناءات غير محلولة' },
  'issues.empty.unresolvedBody': { en: 'Your app is behaving.', ar: 'تطبيقك يعمل بشكل سليم.' },
  'issues.blocked.title': { en: 'This filter needs more access', ar: 'يحتاج هذا المرشّح إلى صلاحيات أوسع' },
  'issues.blocked.removeOne': { en: 'Remove {filters} filter', ar: 'إزالة مرشّح {filters}' },
  'issues.blocked.removeMany': { en: 'Remove {filters} filters', ar: 'إزالة مرشّحات {filters}' },

  // --- events --------------------------------------------------------------
  'events.filterHintTail': {
    en: '(key = value); the search box also matches tag and payload content.',
    ar: '(مفتاح = قيمة)؛ يطابق مربع البحث أيضًا محتوى الوسوم والحمولة.',
  },

  // --- sessions ------------------------------------------------------------
  'sessions.error.engagement': {
    en: "Couldn't load session engagement",
    ar: 'تعذّر تحميل تفاعل الجلسات',
  },
  'explore.range.last': { en: 'last {n}d', ar: 'آخر {n} يوم' },

  // --- users ---------------------------------------------------------------
  'users.error.audience': { en: "Couldn't load audience stats", ar: 'تعذّر تحميل إحصاءات الجمهور' },
  'users.empty.match': {
    en: 'No users match these filters',
    ar: 'لا يوجد مستخدمون يطابقون عوامل التصفية هذه',
  },
  'users.empty.range': {
    en: 'No users in this time range',
    ar: 'لا يوجد مستخدمون في هذا النطاق الزمني',
  },
  'users.range.allTime': { en: 'all time', ar: 'كل الأوقات' },
  'users.range.day': { en: '24h', ar: '24 ساعة' },
  'users.range.week': { en: '7-day', ar: '7 أيام' },
  'users.range.month': { en: '30-day', ar: '30 يومًا' },
  'users.moreTraits.one': { en: '1 more trait', ar: 'سمة أخرى واحدة' },
  'users.moreTraits.many': { en: '{n} more traits', ar: '{n} سمات أخرى' },

  // --- workflows -----------------------------------------------------------
  'workflows.empty.match': {
    en: 'No workflows match these filters',
    ar: 'لا يوجد سير عمل يطابق عوامل التصفية هذه',
  },
  'workflows.empty.range': {
    en: 'No workflows in this time range',
    ar: 'لا يوجد سير عمل في هذا النطاق الزمني',
  },

  // --- devices -------------------------------------------------------------
  'devices.column.os': { en: 'OS', ar: 'نظام التشغيل' },

  // --- shared list controls ------------------------------------------------
  'search.placeholder': { en: 'Search…', ar: 'بحث…' },
  'search.suggestionsUnavailable': { en: 'Suggestions unavailable', ar: 'الاقتراحات غير متاحة' },
  'search.schemaErrorHint': {
    en: '{error} — you can still type a query.',
    ar: '{error} — لا يزال بإمكانك كتابة استعلام.',
  },
  'timeFilter.days': { en: 'days', ar: 'أيام' },
  'timeFilter.to': { en: 'to', ar: 'إلى' },
  'timeFilter.toLabel': { en: 'To', ar: 'إلى' },
  'issueQuickView.noContext': { en: 'No context', ar: 'لا يوجد سياق' },
} as const satisfies Record<string, Message>;
