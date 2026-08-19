import type { Message } from '../types';

/** Overview, Exceptions (issues), Performance, and the Events stream. */
export const monitor = {
  // --- overview ------------------------------------------------------------
  'overview.title': { en: 'Overview', ar: 'نظرة عامة' },
  'overview.stat.events': { en: 'Events', ar: 'الأحداث' },
  'overview.stat.errors': { en: 'Errors', ar: 'الأخطاء' },
  'overview.stat.users': { en: 'Users', ar: 'المستخدمون' },
  'overview.stat.newUsers': { en: 'New users', ar: 'مستخدمون جدد' },
  'overview.stat.sessions': { en: 'Sessions', ar: 'الجلسات' },
  'overview.stat.crashFree': { en: 'Crash-free sessions', ar: 'جلسات بلا أعطال' },
  'overview.card.activeUsers': { en: 'Active users', ar: 'المستخدمون النشطون' },
  'overview.card.errorsOverTime': { en: 'Errors over time', ar: 'الأخطاء عبر الزمن' },
  'overview.card.eventVolume': { en: 'Event volume', ar: 'حجم الأحداث' },
  'overview.card.topEvents': { en: 'Top events', ar: 'أبرز الأحداث' },
  'overview.card.topIssues': { en: 'Top issues', ar: 'أبرز الاستثناءات' },
  'overview.timesSeen': { en: 'times seen', ar: 'مرات الظهور' },
  'overview.error.totals': { en: "Couldn't load totals", ar: 'تعذّر تحميل الإجماليات' },
  'overview.error.chart': { en: "Couldn't load chart", ar: 'تعذّر تحميل الرسم البياني' },
  'overview.error.issues': { en: "Couldn't load issues", ar: 'تعذّر تحميل الاستثناءات' },
  'overview.error.activeUsers': {
    en: "Couldn't load active users",
    ar: 'تعذّر تحميل المستخدمين النشطين',
  },
  'overview.loading.totals': { en: 'Loading totals', ar: 'جارٍ تحميل الإجماليات' },
  'overview.loading.activeUsers': {
    en: 'Loading active users',
    ar: 'جارٍ تحميل المستخدمين النشطين',
  },
  'overview.loading.errorsOverTime': {
    en: 'Loading errors over time',
    ar: 'جارٍ تحميل الأخطاء عبر الزمن',
  },
  'overview.loading.eventVolume': { en: 'Loading event volume', ar: 'جارٍ تحميل حجم الأحداث' },
  'overview.loading.topEvents': { en: 'Loading top events', ar: 'جارٍ تحميل أبرز الأحداث' },
  'overview.loading.topIssues': { en: 'Loading top issues', ar: 'جارٍ تحميل أبرز الاستثناءات' },
  'overview.empty.issues': { en: 'No issues', ar: 'لا توجد استثناءات' },
  'overview.empty.issuesBody': {
    en: 'No errors have been grouped into issues yet.',
    ar: 'لم تُجمَّع أي أخطاء في استثناءات بعد.',
  },

  // --- exceptions (issues) -------------------------------------------------
  'issues.title': { en: 'Exceptions', ar: 'الاستثناءات' },
  'issues.subtitle': {
    en: 'Grouped errors across your app, most recent first.',
    ar: 'أخطاء مجمَّعة عبر تطبيقك، الأحدث أولاً.',
  },
  'issues.column.issue': { en: 'Issue', ar: 'الاستثناء' },
  'issues.column.level': { en: 'Level', ar: 'المستوى' },
  'issues.column.lastSeen': { en: 'Last seen', ar: 'آخر ظهور' },
  'issues.stat.total': { en: 'Total', ar: 'الإجمالي' },
  'issues.stat.unresolved': { en: 'Unresolved', ar: 'غير محلولة' },
  'issues.stat.resolved': { en: 'Resolved', ar: 'محلولة' },
  'issues.stat.ignored': { en: 'Ignored', ar: 'متجاهَلة' },
  'issues.stat.fatal': { en: 'Fatal', ar: 'قاتلة' },
  'issues.stat.error': { en: 'Error', ar: 'خطأ' },
  'issues.stat.warning': { en: 'Warning', ar: 'تحذير' },
  'issues.occurrences': { en: 'Occurrences', ar: 'التكرارات' },
  'issues.empty.title': { en: 'No issues here', ar: 'لا توجد استثناءات هنا' },

  // --- stale-page recovery (shared by Issues and Events) -------------------
  'list.stale.title': { en: 'Nothing left on this page', ar: 'لم يتبقَّ شيء في هذه الصفحة' },
  'list.stale.backAPage': { en: 'Back a page', ar: 'صفحة للخلف' },
  'list.stale.issuesBody': {
    en: 'These rows have gone since the previous page was loaded. Go back for the ones that are still here.',
    ar: 'اختفت هذه الصفوف منذ تحميل الصفحة السابقة. ارجع للخلف لرؤية ما تبقّى منها.',
  },
  'list.stale.eventsBody': {
    en: 'These events have gone since the previous page was loaded — the stream moved on, or they fell out of retention. Go back for the ones that are still here.',
    ar: 'اختفت هذه الأحداث منذ تحميل الصفحة السابقة — إما لتقدّم البثّ أو لخروجها من مدة الاحتفاظ. ارجع للخلف لرؤية ما تبقّى منها.',
  },

  // --- performance ---------------------------------------------------------
  'perf.title': { en: 'Performance', ar: 'الأداء' },
  'perf.subtitle': {
    en: 'Application performance monitoring — latency, throughput, and error rates by operation.',
    ar: 'مراقبة أداء التطبيق — زمن الاستجابة والإنتاجية ومعدلات الأخطاء حسب العملية.',
  },
  'perf.stat.throughput': { en: 'Throughput', ar: 'الإنتاجية' },
  'perf.stat.p95': { en: 'p95 latency', ar: 'زمن الاستجابة p95' },
  'perf.stat.errorRate': { en: 'Error rate', ar: 'معدل الأخطاء' },
  'perf.column.name': { en: 'Name', ar: 'الاسم' },
  'perf.column.avg': { en: 'Avg', ar: 'المتوسط' },
  'perf.card.operations': { en: 'Operations', ar: 'العمليات' },
  'perf.card.latencyOverTime': { en: 'Latency over time', ar: 'زمن الاستجابة عبر الزمن' },
  'perf.card.throughputOverTime': { en: 'Throughput over time', ar: 'الإنتاجية عبر الزمن' },
  'perf.operationFilter': { en: 'Operation filter', ar: 'مرشّح العمليات' },
  'perf.error.load': { en: "Couldn't load performance", ar: 'تعذّر تحميل بيانات الأداء' },
  'perf.empty.title': { en: 'No performance data yet', ar: 'لا توجد بيانات أداء بعد' },
  'perf.empty.body': {
    en: 'Once your SDK sends transactions — navigations, HTTP calls, screen loads and custom spans — their latency and throughput will show up here.',
    ar: 'بمجرد أن ترسل حزمة التطوير معاملات — تنقلات أو طلبات HTTP أو تحميل شاشات أو مقاطع مخصصة — سيظهر زمن استجابتها وإنتاجيتها هنا.',
  },

  // --- events --------------------------------------------------------------
  'events.subtitle': {
    en: 'Product analytics — event volume, top events and raw stream.',
    ar: 'تحليلات المنتج — حجم الأحداث وأبرزها والبثّ الخام.',
  },
  'events.card.stream': { en: 'Event stream', ar: 'بثّ الأحداث' },
  'events.clickToFilter': {
    en: 'Click an event to filter the chart and stream.',
    ar: 'انقر على حدث لتصفية الرسم البياني والبثّ.',
  },
  'events.column.event': { en: 'Event', ar: 'الحدث' },
  'events.column.time': { en: 'Time', ar: 'الوقت' },
  'events.column.user': { en: 'User', ar: 'المستخدم' },
  'events.column.session': { en: 'Session', ar: 'الجلسة' },
  'events.filterBy': { en: 'Filter by', ar: 'تصفية حسب' },
  'events.tag': { en: 'Tag', ar: 'الوسم' },
  'events.properties': { en: 'Properties', ar: 'الخصائص' },
  'events.noProperties': { en: 'No properties on this event.', ar: 'لا توجد خصائص على هذا الحدث.' },
  'events.error.load': { en: "Couldn't load events", ar: 'تعذّر تحميل الأحداث' },
  'events.error.analytics': { en: "Couldn't load analytics", ar: 'تعذّر تحميل التحليلات' },
  'events.empty.none': { en: 'No events', ar: 'لا توجد أحداث' },
  'events.empty.title': { en: 'No events yet', ar: 'لا توجد أحداث بعد' },
  'events.empty.body': {
    en: 'Send events from your SDK to see them here.',
    ar: 'أرسل أحداثًا من حزمة التطوير لتظهر هنا.',
  },
} as const satisfies Record<string, Message>;
