import type { Message } from '../types';

/**
 * Strings added by the Quiet refresh for the analytics pages (batch A2).
 *
 * A separate file so the page batches, done in parallel, never edit the same
 * catalog file. A key must still be unique across every catalog file.
 */
export const refreshA2 = {
  // --- transactions --------------------------------------------------------
  // `{things}` nouns are lower-case plurals that slot into the shared
  // `common.empty.*` sentences ("No {things} in this time range").
  'transactions.noun': { en: 'transactions', ar: 'معاملات' },
  'transactions.timeField.occurred': { en: 'Occurred', ar: 'وقت الحدوث' },
  'transactions.timeField.received': { en: 'Received', ar: 'وقت الاستلام' },
  'transactions.showDetails': { en: 'Show details', ar: 'عرض التفاصيل' },
  'transactions.hideDetails': { en: 'Hide details', ar: 'إخفاء التفاصيل' },
  'transactions.status.ok': { en: 'OK', ar: 'سليم' },
  'transactions.status.error': { en: 'Error', ar: 'خطأ' },

  // --- performance ---------------------------------------------------------
  'perf.noun': { en: 'operations', ar: 'عمليات' },
  'perf.column.op': { en: 'Op', ar: 'العملية' },
  'perf.op.all': { en: 'All', ar: 'الكل' },
  'perf.op.navigation': { en: 'Navigation', ar: 'تنقّل' },
  'perf.op.screenLoad': { en: 'Screen load', ar: 'تحميل شاشة' },
  'perf.op.resource': { en: 'Resource', ar: 'مورد' },
  'perf.op.custom': { en: 'Custom', ar: 'مخصص' },
  // Tile sub-lines read on from the figure above them ("1.2K transactions"),
  // so they stay lower case.
  'perf.stat.throughputSub': { en: 'transactions', ar: 'معاملة' },
  'perf.stat.operationsSub': { en: 'tracked', ar: 'مُتتبَّعة' },
  'perf.stat.p95Sub': { en: 'slowest operation', ar: 'أبطأ عملية' },
  'perf.stat.errorRateSub': { en: 'weighted by volume', ar: 'مرجّح حسب الحجم' },
  'perf.caption.p95ms': { en: 'p95 latency (ms)', ar: 'زمن الاستجابة p95 (مللي ثانية)' },

  // --- active users --------------------------------------------------------
  'activeUsers.error.load': { en: "Couldn't load active users", ar: 'تعذّر تحميل المستخدمين النشطين' },
  'activeUsers.exported': { en: 'Export downloaded.', ar: 'تم تنزيل الملف المُصدَّر.' },
  'activeUsers.split': {
    en: '{identified} identified / {guests} guests',
    ar: '{identified} معروفون / {guests} ضيوف',
  },
  'activeUsers.noCompleteDay': { en: 'no complete day yet', ar: 'لا يوجد يوم مكتمل بعد' },
  'activeUsers.stat.identifiedSub': { en: 'matched by distinct ID', ar: 'مطابَقون بالمعرّف المميز' },
  'activeUsers.stat.guestsSub': { en: 'anonymous distinct IDs', ar: 'معرّفات مميزة مجهولة' },

  // --- retention -----------------------------------------------------------
  'retention.noun': { en: 'cohorts', ar: 'مجموعات' },
  // Was titled "Retention", repeating the page title right above it.
  'retention.card.cohorts': { en: 'Cohorts', ar: 'المجموعات' },
  'retention.granularity.label': { en: 'Cohort period', ar: 'فترة المجموعة' },
  'retention.error.grid': { en: "Couldn't load retention", ar: 'تعذّر تحميل الاحتفاظ' },
  'retention.error.lifecycle': { en: "Couldn't load lifecycle", ar: 'تعذّر تحميل دورة الحياة' },
  'retention.churn.empty': { en: 'No one at risk', ar: 'لا أحد معرّض للفقد' },

  // --- funnels -------------------------------------------------------------
  'funnels.noun': { en: 'funnels', ar: 'مسارات' },
  'funnels.toast.saved': { en: 'Funnel saved.', ar: 'تم حفظ المسار.' },
  'funnels.toast.updated': { en: 'Funnel updated.', ar: 'تم تحديث المسار.' },
  'funnels.toast.duplicated': { en: 'Funnel duplicated.', ar: 'تم نسخ المسار.' },
  'funnels.toast.deleted': { en: 'Funnel deleted.', ar: 'تم حذف المسار.' },
  'funnels.copyOf': { en: 'Copy of {name}', ar: 'نسخة من {name}' },
  'funnels.duplicate': { en: 'Duplicate', ar: 'نسخ' },
  // A saved funnel always has two steps or more (saving needs two), so the
  // English never meets n = 1; Arabic sidesteps the dual/plural split.
  'funnels.stepCount': { en: '{n} steps', ar: 'الخطوات: {n}' },
  'funnels.empty.noSteps': { en: 'No steps yet', ar: 'لا توجد خطوات بعد' },
  'funnels.empty.notComputed': { en: 'Nothing computed yet', ar: 'لم يُحتسب شيء بعد' },
  'funnels.optional': { en: 'optional', ar: 'اختياري' },
  'funnels.dialog.saveTitle': { en: 'Save funnel template', ar: 'حفظ قالب المسار' },
  'funnels.dialog.editTitle': { en: 'Edit funnel template', ar: 'تعديل قالب المسار' },
  'funnels.dialog.saveChanges': { en: 'Save changes', ar: 'حفظ التغييرات' },
  'funnels.dialog.stepsHint': {
    en: '{n} steps will be saved from the builder.',
    ar: 'سيتم حفظ الخطوات ({n}) من المُنشئ.',
  },
  'funnels.deleteMessage': {
    en: '“{name}” will be permanently removed. This can’t be undone.',
    ar: 'ستتم إزالة «{name}» نهائيًا. لا يمكن التراجع عن ذلك.',
  },

  // --- journeys ------------------------------------------------------------
  'journeys.noun': { en: 'journeys', ar: 'رحلات' },
  'journeys.noun.entries': { en: 'entry events', ar: 'أحداث دخول' },
  'journeys.noun.transitions': { en: 'transitions', ar: 'انتقالات' },
  'journeys.to': { en: 'To', ar: 'إلى' },
  'journeys.stepN': { en: 'step {n}', ar: 'الخطوة {n}' },
  // Follows a count in the entry-points bars: "1,204 users".
  'journeys.valueLabel': { en: 'users', ar: 'مستخدم' },

  // --- monitors ------------------------------------------------------------
  // The page's own title key. It used to borrow `monitors.column.uptime` and
  // so read "Uptime"; the sidebar calls the page "Monitors".
  'monitors.title': { en: 'Monitors', ar: 'المراقبات' },
  'monitors.error.load': { en: "Couldn't load monitors", ar: 'تعذّر تحميل المراقبات' },
  'monitors.url': { en: 'URL', ar: 'الرابط' },

  // --- privacy inspector ---------------------------------------------------
  'inspector.error.load': { en: "Couldn't load the privacy inspector", ar: 'تعذّر تحميل مفتّش الخصوصية' },
  'inspector.tab.policy': { en: 'Policy', ar: 'السياسة' },
  'inspector.tab.audit': { en: 'Audit', ar: 'التدقيق' },
  'inspector.enforcementLatency': {
    en: 'New events are masked within about {n} seconds of a change.',
    ar: 'تُخفى الأحداث الجديدة في غضون {n} ثانية تقريبًا من أي تغيير.',
  },
  'inspector.coveragePartial': { en: 'Coverage is partial: {note}', ar: 'التغطية جزئية: {note}' },
  'inspector.matchCount': { en: '{n} matches', ar: 'المطابقات: {n}' },
  'inspector.wholeValue': { en: '(whole value)', ar: '(القيمة كاملة)' },
  'inspector.scanStatus.queued': { en: 'Queued', ar: 'في الانتظار' },
  'inspector.scanStatus.running': { en: 'Running', ar: 'قيد التشغيل' },
  'inspector.scanStatus.succeeded': { en: 'Succeeded', ar: 'نجح' },
  'inspector.scanStatus.failed': { en: 'Failed', ar: 'فشل' },
  'inspector.scanStatus.cancelled': { en: 'Cancelled', ar: 'أُلغي' },
  'inspector.maskStatus.preview': { en: 'Previewing', ar: 'قيد المعاينة' },
  'inspector.maskStatus.previewed': { en: 'Previewed', ar: 'تمت المعاينة' },
  'inspector.maskStatus.pending': { en: 'Pending', ar: 'قيد الانتظار' },
  'inspector.maskStatus.running': { en: 'Running', ar: 'قيد التشغيل' },
  'inspector.maskStatus.cancelling': { en: 'Cancelling', ar: 'جارٍ الإلغاء' },
  'inspector.maskStatus.done': { en: 'Done', ar: 'اكتمل' },
  'inspector.maskStatus.failed': { en: 'Failed', ar: 'فشل' },
  'inspector.maskStatus.cancelled': { en: 'Cancelled', ar: 'أُلغي' },
  'inspector.coverage.full': { en: 'Full', ar: 'كاملة' },
  'inspector.coverage.partial': { en: 'Partial', ar: 'جزئية' },
  'inspector.targetType.project': { en: 'Project', ar: 'المشروع' },
  'inspector.targetType.app': { en: 'App', ar: 'التطبيق' },
  'inspector.targetType.appEnv': { en: 'Environment', ar: 'البيئة' },
  'inspector.targetHint.project': {
    en: 'Covers every app in this project',
    ar: 'تغطي كل التطبيقات في هذا المشروع',
  },
  'inspector.targetHint.app': {
    en: 'Covers this app, in every environment',
    ar: 'تغطي هذا التطبيق في كل البيئات',
  },
  'inspector.targetHint.appEnv': {
    en: 'Covers one environment of this app',
    ar: 'تغطي بيئة واحدة من هذا التطبيق',
  },
  'inspector.noActiveEnvs': {
    en: 'This app has no active environments',
    ar: 'لا توجد بيئات نشطة لهذا التطبيق',
  },
  'inspector.envDefault': { en: '{name} (default)', ar: '{name} (الافتراضية)' },
  'inspector.thisProject': { en: 'this project', ar: 'هذا المشروع' },
  'inspector.thisApp': { en: 'this app', ar: 'هذا التطبيق' },
  // Around two literal key names in <code>: "`Email` matches `email`;
  // `user_email` does not."
  'inspector.keyExample.matches': { en: 'matches', ar: 'يطابق' },
  'inspector.keyExample.doesNot': { en: 'does not.', ar: 'لا يطابق.' },
  'inspector.separateKeys': {
    en: 'Separate with commas or spaces.',
    ar: 'افصل بينها بفواصل أو مسافات.',
  },
  'inspector.enable': { en: 'Enable', ar: 'تفعيل' },
  'inspector.disable': { en: 'Disable', ar: 'تعطيل' },
  'inspector.topLevel': { en: '(top level)', ar: '(المستوى الأعلى)' },
  'inspector.nextRuns': {
    en: 'Next runs (approximate — the server decides): {runs}',
    ar: 'التشغيلات التالية (تقريبية — يقررها الخادم): {runs}',
  },
  'inspector.lastSkip': { en: 'Last scheduled run: {reason}', ar: 'آخر تشغيل مجدول: {reason}' },
  'inspector.since': { en: 'since', ar: 'منذ' },
  'inspector.scanQueued': { en: 'Scan queued', ar: 'أُضيف الفحص إلى قائمة الانتظار' },
  'inspector.readableByWhy': {
    en: '— deliberately, and affordable precisely because these rows store paths and counts and never a value.',
    ar: '— عن قصد، وهذا ممكن تحديدًا لأن هذه الصفوف تخزّن المسارات والأعداد ولا تخزّن أي قيمة.',
  },
  'inspector.boundedBy': { en: '{why} — bounded by: {bounded}', ar: '{why} — محدود بـ: {bounded}' },

  // --- funnel, activity and journey charts: text built around a value ---------
  'funnels.dropOff': { en: '{pct} drop-off', ar: 'تسرّب {pct}' },
  'funnels.continued': { en: '{n} of {total} continued', ar: 'استمر {n} من {total}' },
  'users.activity.active': { en: 'active', ar: 'نشط' },
  'users.activity.new': { en: 'new', ar: 'جديد' },
  'users.activity.tip': { en: '{active} active · {new} new', ar: '{active} نشط · {new} جديد' },
} as const satisfies Record<string, Message>;
