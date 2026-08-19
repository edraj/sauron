import type { Message } from '../types';

/**
 * Strings belonging to the shared components under `lib/components/` — the
 * pager, search box, time filter, timeline, and the cards and modals reused
 * across pages.
 *
 * Separate from `common` because these are not generic vocabulary: they read
 * correctly only in the widget that owns them, and whoever revises the Arabic
 * needs to know which widget that is.
 */
export const ui = {
  // --- pagination ----------------------------------------------------------
  'ui.pager.prev': { en: 'Prev', ar: 'السابق' },
  'ui.pager.next': { en: 'Next', ar: 'التالي' },
  'ui.pager.page': { en: 'Page {n}', ar: 'صفحة {n}' },
  'ui.pager.pageOf': { en: 'Page {page} of {total}', ar: 'صفحة {page} من {total}' },
  'ui.pager.range': { en: '{range} of {total}', ar: '{range} من {total}' },
  'ui.pager.endOfResults': { en: 'End of results', ar: 'نهاية النتائج' },

  // --- route chrome --------------------------------------------------------
  'ui.route.loading': { en: 'Loading page…', ar: 'جارٍ تحميل الصفحة…' },
  'ui.route.errorTitle': { en: 'This page didn’t load', ar: 'تعذّر تحميل هذه الصفحة' },
  'ui.route.reload': { en: 'Reload Sauron', ar: 'إعادة تحميل Sauron' },

  // --- search --------------------------------------------------------------
  'ui.search.clear': { en: 'Clear search', ar: 'مسح البحث' },
  'ui.search.submit': { en: 'Search (Enter)', ar: 'بحث (Enter)' },
  'ui.search.pending': {
    en: 'Press Enter or click Search to run this query.',
    ar: 'اضغط Enter أو انقر «بحث» لتشغيل هذا الاستعلام.',
  },

  // --- time filter ---------------------------------------------------------
  'ui.time.custom': { en: 'Custom…', ar: 'مخصص…' },
  'ui.time.from': { en: 'From', ar: 'من' },
  'ui.time.numberOfDays': { en: 'Number of days', ar: 'عدد الأيام' },
  'ui.time.comparison': { en: 'Time comparison', ar: 'مقارنة زمنية' },
  'ui.time.field': { en: 'Time field', ar: 'حقل الوقت' },
  'ui.time.range': { en: 'Time range', ar: 'النطاق الزمني' },
  'ui.time.localTimezone': {
    en: 'Times are entered in your local timezone',
    ar: 'تُدخل الأوقات بتوقيتك المحلي',
  },

  // --- environment picker --------------------------------------------------
  'ui.env.all': { en: 'All environments', ar: 'كل البيئات' },
  'ui.env.unattributed': { en: 'Unattributed', ar: 'غير منسوب' },
  'ui.env.unattributedNeedsAccess': {
    en: 'Unattributed (needs app-wide access)',
    ar: 'غير منسوب (يتطلب صلاحية على مستوى التطبيق)',
  },
  'ui.env.partialAccess': {
    en: "Your access reaches only some of this app's environments.",
    ar: 'صلاحيتك تشمل بعض بيئات هذا التطبيق فقط.',
  },
  'ui.env.noApps': { en: 'No apps in this project.', ar: 'لا توجد تطبيقات في هذا المشروع.' },

  // --- empty / fallback states --------------------------------------------
  'ui.empty.breadcrumbs': { en: 'No breadcrumbs recorded.', ar: 'لم تُسجَّل أي خطوات تتبّع.' },
  'ui.empty.stacktrace': { en: 'No stacktrace on this event.', ar: 'لا يوجد تتبّع للمكدس في هذا الحدث.' },
  'ui.empty.notLoaded': { en: 'Nothing loaded yet.', ar: 'لم يُحمَّل شيء بعد.' },
  'ui.empty.journeys': {
    en: 'Not enough event data to map journeys in this range.',
    ar: 'لا توجد بيانات أحداث كافية لرسم الرحلات في هذا النطاق.',
  },
  'ui.tryAgain': { en: 'Try again', ar: 'حاول مرة أخرى' },

  // --- event / span detail sections ---------------------------------------
  'ui.section.tags': { en: 'Tags', ar: 'الوسوم' },
  'ui.section.extra': { en: 'Additional data', ar: 'بيانات إضافية' },
  'ui.section.context': { en: 'Context', ar: 'السياق' },
  'ui.section.contexts': { en: 'Contexts', ar: 'السياقات' },
  'ui.section.stacktrace': { en: 'Stacktrace', ar: 'تتبّع المكدس' },
  'ui.section.span': { en: 'Span', ar: 'المقطع' },
  'ui.section.screen': { en: 'Screen', ar: 'الشاشة' },
  'ui.section.device': { en: 'Device', ar: 'الجهاز' },
  'ui.section.affectedUser': { en: 'Affected user', ar: 'المستخدم المتأثر' },
  'ui.viewMore': { en: 'View more', ar: 'عرض المزيد' },
  'ui.viewIssue': { en: 'View issue', ar: 'عرض الاستثناء' },
  'ui.errorDetails': { en: 'Error details', ar: 'تفاصيل الخطأ' },
  'ui.countingOccurrences': { en: 'Counting occurrences…', ar: 'جارٍ حساب التكرارات…' },
  'ui.occurrenceCountsUnavailable': {
    en: 'Occurrence counts unavailable',
    ar: 'أعداد التكرارات غير متاحة',
  },
  'ui.acrossLast30Days': { en: 'Across the last 30 days', ar: 'خلال آخر 30 يومًا' },

  // --- timeline ------------------------------------------------------------
  'ui.timeline.inBetween': { en: 'In between', ar: 'بين الأحداث' },
  'ui.timeline.sliceToTransaction': {
    en: 'Slice timeline to this transaction',
    ar: 'قصر المخطط الزمني على هذه المعاملة',
  },
  'ui.timeline.filterByCategory': {
    en: 'Filter timeline by category',
    ar: 'تصفية المخطط الزمني حسب الفئة',
  },
  'ui.timeline.filterByOp': {
    en: 'Filter transactions by op',
    ar: 'تصفية المعاملات حسب العملية',
  },

  // --- operation transactions modal ---------------------------------------
  'ui.opModal.duration': { en: 'Duration', ar: 'المدة' },
  'ui.opModal.when': { en: 'When', ar: 'الوقت' },
  'ui.opModal.session': { en: 'Session', ar: 'الجلسة' },
  'ui.opModal.slowest': { en: 'Slowest', ar: 'الأبطأ' },
  'ui.opModal.mostRecent': { en: 'Most recent', ar: 'الأحدث' },
  'ui.opModal.order': { en: 'Order', ar: 'الترتيب' },
  'ui.opModal.expand': { en: 'Expand', ar: 'توسيع' },
  'ui.opModal.openInTransactions': { en: 'Open in Transactions', ar: 'فتح في المعاملات' },
  'ui.opModal.loadError': { en: "Couldn't load transactions", ar: 'تعذّر تحميل المعاملات' },
  'ui.opModal.emptyTitle': { en: 'No spans in this window', ar: 'لا توجد مقاطع في هذه النافذة' },
  'ui.opModal.emptyBody': {
    en: 'This operation was aggregated over a different window than the one selected, or its spans have since rotated to cold storage.',
    ar: 'جُمِّعت هذه العملية عبر نافذة زمنية مختلفة عن المحددة، أو أن مقاطعها انتقلت منذ ذلك الحين إلى التخزين البارد.',
  },
  'ui.opModal.noSession': {
    en: 'This span was recorded without a session',
    ar: 'سُجِّل هذا المقطع دون جلسة',
  },

  // --- transaction detail panel -------------------------------------------

  // --- app store section ---------------------------------------------------
  'ui.store.title': { en: 'App store installs', ar: 'عمليات التثبيت من المتاجر' },
  'ui.store.installs': { en: 'Installs', ar: 'عمليات التثبيت' },
  'ui.store.uninstalls': { en: 'Uninstalls', ar: 'عمليات إلغاء التثبيت' },
  'ui.store.netChange': { en: 'Net change', ar: 'صافي التغيّر' },
  'ui.store.appStore': { en: 'App Store', ar: 'App Store' },
  'ui.store.play': { en: 'Play', ar: 'Play' },
  'ui.store.preparing': {
    en: 'App Store is still preparing this report. Apple usually takes 24–48 hours after setup.',
    ar: 'لا يزال App Store يجهّز هذا التقرير. تستغرق Apple عادةً من 24 إلى 48 ساعة بعد الإعداد.',
  },

  // --- panel-scope captions ------------------------------------------------
  // The sentence a panel shows when its request carried less of the page's
  // query than the list beside it. See `models/panel-scope.ts`.
  'panel.control.filter': { en: 'filter', ar: 'المرشّح' },
  'panel.control.filters': { en: 'filters', ar: 'المرشّحات' },
  'panel.control.search': { en: 'search', ar: 'البحث' },
  'panel.control.dateRange': { en: 'date range', ar: 'النطاق الزمني' },
  'panel.subject.totals': { en: 'these totals', ar: 'هذه الإجماليات' },
  'panel.subject.chart': { en: 'this chart', ar: 'هذا الرسم البياني' },
  'panel.subject.list': { en: 'this list', ar: 'هذه القائمة' },
  // English needs `don't` / `doesn't` to agree with the joined subject; Arabic
  // negates the verb ahead of it and does not inflect for number, so both
  // templates collapse to one sentence there.
  'panel.note.singular': {
    en: "The {controls} doesn't apply to {subject}.",
    ar: 'لا ينطبق {controls} على {subject}.',
  },
  'panel.note.plural': {
    en: "The {controls} don't apply to {subject}.",
    ar: 'لا ينطبق {controls} على {subject}.',
  },
  'panel.note.onlyFilter': {
    en: 'Only the {label} filter applies to {subject}.',
    ar: 'ينطبق مرشّح {label} وحده على {subject}.',
  },
  'panel.note.onlyFilterNoRange': {
    en: "Only the {label} filter applies to {subject} — the date range doesn't.",
    ar: 'ينطبق مرشّح {label} وحده على {subject} — أما النطاق الزمني فلا.',
  },
  // English joins the last pair with "and" and separates the rest with commas;
  // Arabic prefixes every item after the first with "و" and uses no commas.
  'panel.join.and': { en: 'and', ar: 'و' },

  // --- issue level & status values ----------------------------------------
  // Rendered from enum values the API returns, so the badges used to print the
  // wire value verbatim. Mapped rather than translated in place because the
  // value is also the sort key and the filter chip's payload — only the label
  // moves.
  'level.fatal': { en: 'fatal', ar: 'قاتل' },
  'level.error': { en: 'error', ar: 'خطأ' },
  'level.warning': { en: 'warning', ar: 'تحذير' },
  'level.info': { en: 'info', ar: 'معلومة' },
  'level.debug': { en: 'debug', ar: 'تنقيح' },
  'status.unresolved': { en: 'unresolved', ar: 'غير محلول' },
  'status.resolved': { en: 'resolved', ar: 'محلول' },
  'status.ignored': { en: 'ignored', ar: 'متجاهَل' },

  // --- filter bar & time modes --------------------------------------------
  'filter.addFilter': { en: '+ Add filter', ar: '+ إضافة مرشّح' },
  'ui.time.mode.last': { en: 'in the last', ar: 'خلال آخر' },
  'ui.time.mode.after': { en: 'after', ar: 'بعد' },
  'ui.time.mode.before': { en: 'before', ar: 'قبل' },
  'ui.time.mode.between': { en: 'between', ar: 'بين' },
  // Range presets are unit abbreviations — "24h", "7d" — and stay Latin for the
  // same reason the digits do. Only "All" is a word.
  'ui.range.all': { en: 'All', ar: 'الكل' },
  'ui.chart.total': { en: '{n} total', ar: 'الإجمالي {n}' },

  // --- filter field labels -------------------------------------------------
  // Display names for the chip registry in `components/filters/filters.ts`.
  // The registry's `key` is the wire identifier and never moves — the
  // backend-parity tests compare on it — so only these labels are translated.
  'filter.field.culprit': { en: 'Culprit', ar: 'الموضع المسبِّب' },
  'filter.field.device': { en: 'Device', ar: 'الجهاز' },
  'filter.field.errors': { en: 'Errors', ar: 'الأخطاء' },
  'filter.field.event': { en: 'Event', ar: 'الحدث' },
  'filter.field.events': { en: 'Events', ar: 'الأحداث' },
  'filter.field.level': { en: 'Level', ar: 'المستوى' },
  'filter.field.method': { en: 'Method', ar: 'الطريقة' },
  'filter.field.name': { en: 'Name', ar: 'الاسم' },
  'filter.field.op': { en: 'Op', ar: 'العملية' },
  'filter.field.release': { en: 'Release', ar: 'الإصدار' },
  'filter.field.screen': { en: 'Screen', ar: 'الشاشة' },
  'filter.field.session': { en: 'Session', ar: 'الجلسة' },
  'filter.field.status': { en: 'Status', ar: 'الحالة' },
  'filter.field.statusCode': { en: 'Status code', ar: 'رمز الحالة' },
  'filter.field.tag': { en: 'Tag', ar: 'الوسم' },
  'filter.field.type': { en: 'Type', ar: 'النوع' },
  'filter.field.url': { en: 'URL', ar: 'الرابط' },
  'filter.field.user': { en: 'User', ar: 'المستخدم' },
  'filter.field.users': { en: 'Users', ar: 'المستخدمون' },
  'filter.field.workflow': { en: 'Workflow', ar: 'سير العمل' },
  // `=`, `≠`, `>` and `<` are symbols and stay; only the word-shaped operator
  // needs translating.
  'filter.op.contains': { en: 'contains', ar: 'يحتوي على' },

  // --- monitor status pill -------------------------------------------------
  'monitor.status.up': { en: 'Up', ar: 'يعمل' },
  'monitor.status.down': { en: 'Down', ar: 'متوقف' },
  'monitor.status.paused': { en: 'Paused', ar: 'موقوف مؤقتًا' },
  'monitor.status.unknown': { en: 'Pending', ar: 'قيد الانتظار' },
} as const satisfies Record<string, Message>;
