import type { Message } from '../types';

/**
 * Strings added by the Quiet refresh for the details pages (batch B).
 *
 * A separate file so the four page batches, done in parallel, never edit the
 * same catalog file. Keys follow the usual domain prefixes; a key must still
 * be unique across every catalog file (catalog.test.ts).
 */
export const refreshB = {
  // --- shared section states ----------------------------------------------
  'section.loadError': { en: "Couldn't load this section", ar: 'تعذّر تحميل هذا القسم' },
  'section.row.open': { en: 'Open', ar: 'فتح' },

  // --- timeline (session + person) ----------------------------------------
  'timeline.empty.session': {
    en: 'No activity recorded in this session',
    ar: 'لم يُسجَّل أي نشاط في هذه الجلسة',
  },
  'timeline.things.entries': { en: 'entries', ar: 'إدخالات' },
  'timeline.kind.navigation': { en: 'Navigation', ar: 'تنقّل' },
  'timeline.kind.http': { en: 'HTTP', ar: 'طلب HTTP' },
  'timeline.kind.transaction': { en: 'Transaction', ar: 'معاملة' },
  'timeline.kind.event': { en: 'Event', ar: 'حدث' },
  'timeline.kind.error': { en: 'Error', ar: 'خطأ' },
  'timeline.offset.sincePrevious': { en: 'Since the previous entry', ar: 'منذ الإدخال السابق' },
  'timeline.offset.sinceSessionStart': { en: 'Since the session started', ar: 'منذ بدء الجلسة' },
  'timeline.mode.sincePrevious': { en: 'Since previous', ar: 'منذ السابق' },
  // Person and session timelines read archived (Parquet) rows as well as
  // Postgres. When the archive cannot be read the server still answers with
  // what Postgres holds and says so; this is that note.
  'timeline.archiveIncomplete': {
    en: "Some older activity is archived and couldn't be read just now, so only recent rows are listed. Refresh to try again.",
    ar: 'تعذّرت قراءة بعض النشاط الأقدم المؤرشف الآن، لذا تُعرض الصفوف الحديثة فقط. حدّث الصفحة للمحاولة مجددًا.',
  },
  'timeline.category.navigation': { en: 'Navigation', ar: 'التنقّل' },
  'timeline.category.transaction': { en: 'Transactions', ar: 'المعاملات' },
  'timeline.category.event': { en: 'Events', ar: 'الأحداث' },
  'timeline.category.issue': { en: 'Issues', ar: 'الاستثناءات' },
  'timeline.category.noneNavigation': {
    en: 'No navigation in this session',
    ar: 'لا يوجد تنقّل في هذه الجلسة',
  },
  'timeline.category.noneTransaction': {
    en: 'No transactions in this session',
    ar: 'لا توجد معاملات في هذه الجلسة',
  },
  'timeline.category.noneEvent': { en: 'No events in this session', ar: 'لا توجد أحداث في هذه الجلسة' },
  'timeline.category.noneIssue': {
    en: 'No issues in this session',
    ar: 'لا توجد استثناءات في هذه الجلسة',
  },
  'timeline.filter.op': { en: 'Op', ar: 'العملية' },
  'timeline.filter.noOp': { en: '(none)', ar: '(بلا عملية)' },
  'timeline.filter.opAria': { en: 'Op {op}, {n}', ar: 'العملية {op}، {n}' },

  // --- stack traces & symbolication ----------------------------------------
  'stack.showOriginal': { en: 'Show original', ar: 'عرض الأصلي' },
  'stack.showMinified': { en: 'Show minified', ar: 'عرض المُصغَّر' },
  'stack.showFrames': { en: 'Show {n} frames', ar: 'إظهار {n} إطارات' },
  'stack.hideFrames': { en: 'Hide {n} frames', ar: 'إخفاء {n} إطارات' },
  'stack.inApp': { en: 'In app', ar: 'داخل التطبيق' },
  'stack.anonymousFn': { en: '<anonymous>', ar: '<مجهولة>' },
  'stack.unknownFile': { en: '<unknown>', ar: '<غير معروف>' },
  'symbolication.symbolicated': { en: 'Symbolicated', ar: 'مُرمَّز' },
  'symbolication.partial': { en: 'Partially symbolicated', ar: 'مُرمَّز جزئيًا' },
  'symbolication.noSymbols': { en: 'No symbols', ar: 'لا توجد رموز' },
  'symbolication.noSourceMaps': { en: 'No source maps', ar: 'لا توجد خرائط مصدر' },
  'symbolication.pending': { en: 'Pending', ar: 'قيد الانتظار' },
  'symbolication.notApplicable': { en: 'Not applicable', ar: 'غير منطبق' },
  'symbolication.hintSymbols': {
    en: 'Upload debug symbols for this release to see original frames',
    ar: 'ارفع رموز التنقيح لهذا الإصدار لرؤية الإطارات الأصلية',
  },
  'symbolication.hintSourceMaps': {
    en: 'Upload source maps for this release to see original frames',
    ar: 'ارفع خرائط المصدر لهذا الإصدار لرؤية الإطارات الأصلية',
  },

  // --- transactions (span panel, operation modal) -------------------------
  'tx.truncated.bytes': {
    en: 'The SDK capped this payload at 16 KB and sent a marker instead ({bytes} bytes). The span and its timing are accurate; only the attached data was dropped.',
    ar: 'قيّدت حزمة SDK هذه الحمولة عند 16 كيلوبايت وأرسلت علامة بدلًا منها ({bytes} بايت). المقطع وتوقيته دقيقان؛ أُسقطت البيانات المرفقة فقط.',
  },
  'tx.truncated.unserializable': {
    en: 'The SDK capped this payload at 16 KB and sent a marker instead (the value could not be serialized). The span and its timing are accurate; only the attached data was dropped.',
    ar: 'قيّدت حزمة SDK هذه الحمولة عند 16 كيلوبايت وأرسلت علامة بدلًا منها (تعذّر تسلسل القيمة). المقطع وتوقيته دقيقان؛ أُسقطت البيانات المرفقة فقط.',
  },
  'opModal.showing': { en: 'Showing {n} of {total}', ar: 'يُعرض {n} من {total}' },
  'opModal.showDetails': { en: 'Show details', ar: 'إظهار التفاصيل' },
  'opModal.hideDetails': { en: 'Hide details', ar: 'إخفاء التفاصيل' },

  // --- session detail -----------------------------------------------------
  'session.empty.context': {
    en: 'No context recorded for this session',
    ar: 'لم يُسجَّل أي سياق لهذه الجلسة',
  },
  'session.timeMode.sinceStart': { en: 'Since start', ar: 'منذ البدء' },
  'session.timeMode.deltaTitle': {
    en: 'Showing time since the previous entry — click to measure from the session start',
    ar: 'يُعرض الوقت منذ الإدخال السابق — انقر للقياس من بدء الجلسة',
  },
  'session.timeMode.startTitle': {
    en: 'Showing time since the session started — click to measure from the previous entry',
    ar: 'يُعرض الوقت منذ بدء الجلسة — انقر للقياس من الإدخال السابق',
  },
  'session.slice.measureFrom': { en: 'Measure the span from', ar: 'قياس المقطع ابتداءً من' },
  'session.slice.occurredAt': { en: 'Occurred at', ar: 'وقت الحدوث' },
  'session.slice.receivedAt': { en: 'Received at', ar: 'وقت الاستلام' },
  'session.slice.empty': {
    en: 'Nothing else was recorded while this transaction was open',
    ar: 'لم يُسجَّل شيء آخر أثناء انفتاح هذه المعاملة',
  },

  // --- issue detail -------------------------------------------------------
  'issue.notFound.title': { en: 'Issue not found', ar: 'الاستثناء غير موجود' },
  'issue.notFound.body': {
    en: 'This issue no longer exists, or it belongs to another app.',
    ar: 'لم يعد هذا الاستثناء موجودًا، أو أنه يتبع تطبيقًا آخر.',
  },
  'issue.backToExceptions': { en: 'Back to exceptions', ar: 'العودة إلى الاستثناءات' },
  'issue.toast.resolved': { en: 'Issue resolved.', ar: 'تم حلّ الاستثناء.' },
  'issue.toast.ignored': { en: 'Issue ignored.', ar: 'تم تجاهل الاستثناء.' },
  'issue.toast.unresolved': { en: 'Issue marked unresolved.', ar: 'تم وضع الاستثناء كغير محلول.' },
  'issue.empty.context': { en: 'No context', ar: 'لا يوجد سياق' },
  // Breadcrumbs and Context come from the latest event; this covers both "it
  // failed to load" and "the issue has none" without claiming either.
  'issue.empty.needsLatestEvent': {
    en: 'Not available without the latest event',
    ar: 'غير متاح بدون أحدث حدث',
  },
  'issue.empty.payloadTitle': {
    en: 'No event payload for this issue',
    ar: 'لا توجد حمولة حدث لهذا الاستثناء',
  },
  'issue.things.occurrences': { en: 'occurrences', ar: 'تكرارات' },

  // --- device detail ------------------------------------------------------
  'device.notFound.body': {
    en: 'This device no longer exists, or it never reported to this app.',
    ar: 'لم يعد هذا الجهاز موجودًا، أو أنه لم يُبلِغ هذا التطبيق أصلًا.',
  },
  'device.error.load': { en: "Couldn't load device", ar: 'تعذّر تحميل الجهاز' },
  'device.field.os': { en: 'OS', ar: 'نظام التشغيل' },
  'device.column.op': { en: 'Op', ar: 'العملية' },
  'device.empty.sessionsTitle': {
    en: 'No sessions recorded for this device',
    ar: 'لم تُسجَّل أي جلسات لهذا الجهاز',
  },
  'device.empty.performanceTitle': {
    en: 'No performance data for this device',
    ar: 'لا توجد بيانات أداء لهذا الجهاز',
  },
  'device.empty.crashesTitle': {
    en: 'No crashes reported on this device',
    ar: 'لم يُبلَّغ عن أعطال في هذا الجهاز',
  },

  // --- person profile -----------------------------------------------------
  'person.backToUsers': { en: 'Back to users', ar: 'العودة إلى المستخدمين' },
  'person.notFound.title': { en: 'User not found', ar: 'المستخدم غير موجود' },
  'person.notFound.body': {
    en: 'No user with this ID has reported to this app.',
    ar: 'لم يُبلِغ أي مستخدم بهذا المعرّف هذا التطبيق.',
  },
  // "Start" here is the first entry SHOWN — the oldest row of a capped window,
  // not the person's first activity — so it gets its own words.
  'person.timeMode.sinceFirst': { en: 'Since first entry', ar: 'منذ أول إدخال' },
  'person.timeMode.deltaTitle': {
    en: 'Showing time since the previous entry — click to measure from the first entry shown',
    ar: 'يُعرض الوقت منذ الإدخال السابق — انقر للقياس من أول إدخال معروض',
  },
  'person.timeMode.firstTitle': {
    en: 'Showing time since the first entry shown — click to measure from the previous entry',
    ar: 'يُعرض الوقت منذ أول إدخال معروض — انقر للقياس من الإدخال السابق',
  },
  'person.offset.sinceFirst': { en: 'Since the first entry shown', ar: 'منذ أول إدخال معروض' },

  // --- screen detail ------------------------------------------------------
  'screen.empty.events': { en: 'No events on this screen.', ar: 'لا توجد أحداث على هذه الشاشة.' },
  'screen.empty.exceptions': {
    en: 'No exceptions on this screen.',
    ar: 'لا توجد استثناءات على هذه الشاشة.',
  },
  'screen.empty.devices': {
    en: 'No devices seen on this screen.',
    ar: 'لم تُرصد أي أجهزة على هذه الشاشة.',
  },
  'screen.empty.users': {
    en: 'No users seen on this screen.',
    ar: 'لم يُرصد أي مستخدمين على هذه الشاشة.',
  },
  'screen.empty.properties': { en: 'No properties', ar: 'لا توجد خصائص' },
  'screen.open.issue': { en: 'Open issue', ar: 'فتح الاستثناء' },
  'screen.open.device': { en: 'Open device', ar: 'فتح الجهاز' },
  'screen.open.user': { en: 'Open user', ar: 'فتح المستخدم' },
  'screen.issueFallback': { en: 'Issue {id}', ar: 'الاستثناء {id}' },

  // --- monitor detail -----------------------------------------------------
  'monitor.backToMonitors': { en: 'Back to monitors', ar: 'العودة إلى المراقبات' },
  'monitor.notFound.body': {
    en: 'This monitor no longer exists, or it belongs to another project.',
    ar: 'لم يعد هذا المراقب موجودًا، أو أنه يتبع مشروعًا آخر.',
  },
  'monitor.error.load': { en: "Couldn't load monitor", ar: 'تعذّر تحميل المراقب' },
  'monitor.action.pause': { en: 'Pause', ar: 'إيقاف مؤقت' },
  'monitor.action.resume': { en: 'Resume', ar: 'استئناف' },
  'monitor.kind.tcp': { en: 'TCP', ar: 'منفذ TCP' },
  'monitor.meta.webhook': { en: 'Webhook', ar: 'خطّاف ويب' },
  'monitor.meta.headers': { en: 'Headers: {n}', ar: 'الترويسات: {n}' },
  'monitor.meta.probeSends': { en: 'Probe sends: {headers}', ar: 'يرسل الفاحص: {headers}' },
  'monitor.bar.checks': { en: '{n} checks', ar: '{n} فحص' },
  'monitor.empty.checks24h': {
    en: 'No checks in the last 24 hours',
    ar: 'لا توجد فحوص خلال آخر 24 ساعة',
  },
  'monitor.empty.checks24hBody': {
    en: 'Results appear here once the prober reports in.',
    ar: 'ستظهر النتائج هنا بمجرد أن يبلّغ الفاحص.',
  },
  'monitor.confirm.delete': {
    en: "Delete “{name}”? Its check history and incidents will be removed. This can't be undone.",
    ar: 'حذف «{name}»؟ سيُزال سجل فحوصه وحوادثه. لا يمكن التراجع عن ذلك.',
  },
  'monitor.confirm.deleteOneRule': {
    en: "Delete “{name}”? Its check history and incidents will be removed. This will also delete 1 alert rule pinned to it. This can't be undone.",
    ar: 'حذف «{name}»؟ سيُزال سجل فحوصه وحوادثه. سيُحذف معه أيضًا قاعدة تنبيه واحدة مرتبطة به. لا يمكن التراجع عن ذلك.',
  },
  'monitor.confirm.deleteRules': {
    en: "Delete “{name}”? Its check history and incidents will be removed. This will also delete {n} alert rules pinned to it. This can't be undone.",
    ar: 'حذف «{name}»؟ سيُزال سجل فحوصه وحوادثه. ستُحذف معه أيضًا قواعد التنبيه المرتبطة به ({n}). لا يمكن التراجع عن ذلك.',
  },
  'monitor.confirm.interval': {
    en: 'Change the check interval for “{name}” from {from} to {to}? The prober applies the new interval on its next cycle.',
    ar: 'تغيير فترة الفحص لـ«{name}» من {from} إلى {to}؟ يطبّق الفاحص الفترة الجديدة في دورته التالية.',
  },
  // Arabic takes a colon: the title is a heading in the nominative
  // ("المستخدمون"), which cannot follow the verb directly.
  'ui.fetchCard.load': { en: 'Fetch {title}', ar: 'جلب: {title}' },
} as const satisfies Record<string, Message>;
