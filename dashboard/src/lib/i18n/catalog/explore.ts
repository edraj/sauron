import type { Message } from '../types';

/** Sessions, Users/People, Devices, Screens, Transactions, and Workflows. */
export const explore = {
  // --- sessions ------------------------------------------------------------
  'sessions.subtitle': {
    en: 'User sessions — activity, duration and errors over time.',
    ar: 'جلسات المستخدمين — النشاط والمدة والأخطاء عبر الزمن.',
  },
  'sessions.stat.avg': { en: 'Avg session', ar: 'متوسط الجلسة' },
  'sessions.stat.median': { en: 'Median session', ar: 'وسيط الجلسة' },
  'sessions.stat.crashed': { en: 'Crashed', ar: 'متعطلة' },
  'sessions.column.session': { en: 'Session', ar: 'الجلسة' },
  'sessions.column.user': { en: 'User', ar: 'المستخدم' },
  'sessions.column.device': { en: 'Device', ar: 'الجهاز' },
  'sessions.column.started': { en: 'Started', ar: 'البداية' },
  'sessions.timeField.lastActivity': { en: 'Last activity', ar: 'آخر نشاط' },
  'sessions.column.duration': { en: 'Duration', ar: 'المدة' },
  'sessions.column.events': { en: 'Events', ar: 'الأحداث' },
  'sessions.column.errors': { en: 'Errors', ar: 'الأخطاء' },
  'sessions.card.engagement': { en: 'Session engagement', ar: 'تفاعل الجلسات' },
  'sessions.card.avgPerDay': {
    en: 'Average session duration per day',
    ar: 'متوسط مدة الجلسة يوميًا',
  },
  'sessions.card.distribution': {
    en: 'Session length distribution',
    ar: 'توزيع أطوال الجلسات',
  },
  'sessions.exportTitle': {
    en: 'Download visible sessions as CSV',
    ar: 'تنزيل الجلسات المعروضة بصيغة CSV',
  },
  'sessions.error.load': { en: "Couldn't load sessions", ar: 'تعذّر تحميل الجلسات' },
  'sessions.empty.noMatches': { en: 'No matches', ar: 'لا توجد نتائج مطابقة' },

  // --- session detail ------------------------------------------------------
  'session.backToList': { en: 'Back to sessions', ar: 'العودة إلى الجلسات' },
  'session.card.timeline': { en: 'Timeline', ar: 'المخطط الزمني' },
  'session.card.context': { en: 'Session context', ar: 'سياق الجلسة' },
  'session.noContext': {
    en: 'No context recorded for this session.',
    ar: 'لم يُسجَّل أي سياق لهذه الجلسة.',
  },
  'session.inBetweenTransaction': { en: 'In between transaction', ar: 'معاملة بينية' },
  'session.downloadTitle': {
    en: 'Download session timeline and context as JSON',
    ar: 'تنزيل المخطط الزمني وسياق الجلسة بصيغة JSON',
  },
  'session.error.load': { en: "Couldn't load session", ar: 'تعذّر تحميل الجلسة' },
  'session.notFound.title': { en: 'Session not found', ar: 'الجلسة غير موجودة' },
  'session.notFound.body': {
    en: 'This session no longer exists, or it never reached this app.',
    ar: 'لم تعد هذه الجلسة موجودة، أو أنها لم تصل إلى هذا التطبيق أصلاً.',
  },

  // --- users / audience ----------------------------------------------------
  'users.title': { en: 'Users', ar: 'المستخدمون' },
  'users.subtitle': {
    en: 'Identified & anonymous people seen by this app — search by distinct ID or trait.',
    ar: 'الأشخاص المعروفون والمجهولون الذين رصدهم هذا التطبيق — ابحث بالمعرّف المميز أو بإحدى السمات.',
  },
  'users.audience': { en: 'Audience', ar: 'الجمهور' },
  'users.people': { en: 'People', ar: 'الأشخاص' },
  'users.onePerDistinctId': { en: 'One row per distinct ID.', ar: 'صف واحد لكل معرّف مميز.' },
  'users.thisAppOnly': { en: 'This app only.', ar: 'هذا التطبيق فقط.' },
  'users.combinedActive': { en: 'Combined active users', ar: 'إجمالي المستخدمين النشطين' },
  'users.stat.total': { en: 'Total users', ar: 'إجمالي المستخدمين' },
  'users.stat.active': { en: 'Active', ar: 'نشطون' },
  'users.stat.new': { en: 'New', ar: 'جدد' },
  'users.stat.stickiness': { en: 'Stickiness', ar: 'معدل الالتصاق' },
  'users.column.traits': { en: 'Traits', ar: 'السمات' },
  'users.card.activePerDay': { en: 'Active users per day', ar: 'المستخدمون النشطون يوميًا' },
  'users.search': { en: 'Search users…', ar: 'البحث في المستخدمين…' },
  'users.error.load': { en: "Couldn't load users", ar: 'تعذّر تحميل المستخدمين' },
  'users.loading.users': { en: 'Loading users', ar: 'جارٍ تحميل المستخدمين' },
  'users.loading.stats': { en: 'Loading audience stats', ar: 'جارٍ تحميل إحصاءات الجمهور' },
  'users.loading.chart': { en: 'Loading activity chart', ar: 'جارٍ تحميل مخطط النشاط' },

  // --- person profile ------------------------------------------------------
  'person.title': { en: 'Profile', ar: 'الملف الشخصي' },
  'person.backToEvents': { en: 'Back to events', ar: 'العودة إلى الأحداث' },
  'person.card.identity': { en: 'Identity', ar: 'الهوية' },
  'person.card.traits': { en: 'Traits', ar: 'السمات' },
  'person.card.timeline': { en: 'Activity timeline', ar: 'المخطط الزمني للنشاط' },
  'person.distinctId': { en: 'Distinct ID', ar: 'المعرّف المميز' },
  'person.anonymous': { en: 'Anonymous', ar: 'مجهول' },
  'person.anonymousNote': {
    en: 'Anonymous — no persisted profile record.',
    ar: 'مجهول — لا يوجد سجل ملف شخصي محفوظ.',
  },
  'person.noTraits': { en: 'No traits recorded', ar: 'لم تُسجَّل أي سمات' },
  'person.downloadTitle': {
    en: "Download this person's activity timeline as JSON",
    ar: 'تنزيل المخطط الزمني لنشاط هذا الشخص بصيغة JSON',
  },
  'person.error.load': { en: "Couldn't load person", ar: 'تعذّر تحميل بيانات الشخص' },
  'person.empty.title': { en: 'No activity', ar: 'لا يوجد نشاط' },
  'person.empty.body': {
    en: 'This person has no recorded events or errors.',
    ar: 'لا توجد أحداث أو أخطاء مسجّلة لهذا الشخص.',
  },

  // --- devices -------------------------------------------------------------
  'devices.title': { en: 'Devices', ar: 'الأجهزة' },
  'devices.subtitle': {
    en: 'Fleet-wide hardware, OS and browser breakdown across your users.',
    ar: 'توزيع العتاد ونظام التشغيل والمتصفح عبر مستخدميك.',
  },
  'devices.all': { en: 'All devices', ar: 'كل الأجهزة' },
  'devices.search': { en: 'Search devices…', ar: 'البحث في الأجهزة…' },
  'devices.exportTitle': {
    en: 'Download visible devices as CSV',
    ar: 'تنزيل الأجهزة المعروضة بصيغة CSV',
  },
  'devices.error.load': { en: "Couldn't load devices", ar: 'تعذّر تحميل الأجهزة' },

  // --- device detail -------------------------------------------------------
  'device.backToList': { en: 'Back to devices', ar: 'العودة إلى الأجهزة' },
  'device.notFound': { en: 'Device not found', ar: 'الجهاز غير موجود' },
  'device.copyKey': { en: 'Copy key', ar: 'نسخ المفتاح' },
  'device.card.hardware': { en: 'Hardware & OS', ar: 'العتاد ونظام التشغيل' },
  'device.card.sessions': { en: 'Recent sessions', ar: 'الجلسات الأخيرة' },
  'device.card.crashes': { en: 'Crash history', ar: 'سجل الأعطال' },
  'device.card.performance': { en: 'Performance profile', ar: 'ملف الأداء' },
  'device.field.family': { en: 'Family', ar: 'العائلة' },
  'device.field.model': { en: 'Model', ar: 'الطراز' },
  'device.field.osVersion': { en: 'OS version', ar: 'إصدار النظام' },
  'device.field.browser': { en: 'Browser', ar: 'المتصفح' },
  'device.field.arch': { en: 'Arch', ar: 'المعمارية' },
  'device.field.lastUser': { en: 'Last user', ar: 'آخر مستخدم' },
  'device.empty.sessions': {
    en: 'No sessions recorded for this device.',
    ar: 'لم تُسجَّل أي جلسات لهذا الجهاز.',
  },
  'device.empty.crashes': {
    en: 'No crashes reported on this device.',
    ar: 'لم يُبلَّغ عن أعطال في هذا الجهاز.',
  },
  'device.empty.performance': { en: 'No performance data yet.', ar: 'لا توجد بيانات أداء بعد.' },

  // --- shared column headings ---------------------------------------------
  'explore.column.count': { en: 'Count', ar: 'العدد' },
  'explore.column.events': { en: 'Events', ar: 'الأحداث' },
  'explore.column.errors': { en: 'Errors', ar: 'الأخطاء' },
  'explore.column.sessions': { en: 'Sessions', ar: 'الجلسات' },
  'explore.column.duration': { en: 'Duration', ar: 'المدة' },
  'explore.column.started': { en: 'Started', ar: 'البداية' },
  'explore.column.firstSeen': { en: 'First seen', ar: 'أول ظهور' },
  'explore.column.lastSeen': { en: 'Last seen', ar: 'آخر ظهور' },
  'explore.downloadJson': { en: 'Download JSON', ar: 'تنزيل JSON' },
  'explore.exportCsv': { en: 'Export CSV', ar: 'تصدير CSV' },
  // --- screens -------------------------------------------------------------
  'screens.title': { en: 'Screens', ar: 'الشاشات' },
  'screens.subtitle': {
    en: 'Views, engagement and errors per screen.',
    ar: 'المشاهدات والتفاعل والأخطاء لكل شاشة.',
  },
  'screens.column.screen': { en: 'Screen', ar: 'الشاشة' },
  'screens.column.views': { en: 'Views', ar: 'المشاهدات' },
  'screens.column.avgDwell': { en: 'Avg dwell', ar: 'متوسط زمن البقاء' },
  'screens.column.exceptions': { en: 'Exceptions', ar: 'الاستثناءات' },
  'screens.search': { en: 'Search screens…', ar: 'البحث في الشاشات…' },
  'screens.error.load': { en: "Couldn't load screens", ar: 'تعذّر تحميل الشاشات' },
  'screens.empty.title': { en: 'No screens yet', ar: 'لا توجد شاشات بعد' },

  // --- screen detail -------------------------------------------------------
  'screen.backToList': { en: 'Back to screens', ar: 'العودة إلى الشاشات' },
  'screen.notFound': { en: 'Screen not found', ar: 'الشاشة غير موجودة' },
  'screen.error.load': { en: "Couldn't load screen", ar: 'تعذّر تحميل الشاشة' },
  'screen.empty.body': {
    en: 'No data for this screen in the selected range.',
    ar: 'لا توجد بيانات لهذه الشاشة في النطاق المحدد.',
  },
  'screen.stat.totalDwell': { en: 'Total dwell', ar: 'إجمالي زمن البقاء' },
  'screen.viewsHere': { en: 'Views here', ar: 'المشاهدات هنا' },
  'screen.eventsHere': { en: 'Events here', ar: 'الأحداث هنا' },
  'screen.exceptionsHere': { en: 'Exceptions here', ar: 'الاستثناءات هنا' },
  'screen.firstSeenHere': { en: 'First seen here', ar: 'أول ظهور هنا' },
  'screen.deviceKey': { en: 'Device key', ar: 'مفتاح الجهاز' },
  'screen.distinctId': { en: 'Distinct id', ar: 'المعرّف المميز' },
  'screen.culprit': { en: 'Culprit', ar: 'الموضع المسبِّب' },
  'screen.message': { en: 'Message', ar: 'الرسالة' },

  // --- transactions --------------------------------------------------------
  'transactions.title': { en: 'Transactions', ar: 'المعاملات' },
  'transactions.subtitle': {
    en: 'Individual timed operations. The',
    ar: 'عمليات مُوقَّتة فردية. أما',
  },
  'transactions.card.spans': { en: 'Spans', ar: 'المقاطع' },
  'transactions.error.load': { en: "Couldn't load transactions", ar: 'تعذّر تحميل المعاملات' },
  'transactions.empty.title': { en: 'No transactions', ar: 'لا توجد معاملات' },
  'transactions.empty.body': {
    en: 'Nothing matched this query in the selected window. Record one with trackTransaction() in any Sauron SDK.',
    ar: 'لا شيء يطابق هذا الاستعلام في النافذة المحددة. سجّل معاملة عبر trackTransaction() في أي حزمة تطوير من Sauron.',
  },

  // --- workflows -----------------------------------------------------------
  'workflows.title': { en: 'Workflows', ar: 'سير العمل' },
  'workflows.subtitle': {
    en: 'Named, bounded spans of activity your app reports.',
    ar: 'مقاطع نشاط مسمّاة ومحدودة يبلّغ عنها تطبيقك.',
  },
  'workflows.column.workflow': { en: 'Workflow', ar: 'سير العمل' },
  'workflows.column.median': { en: 'Median', ar: 'الوسيط' },
  'workflows.stat.started': { en: 'Started', ar: 'بدأت' },
  'workflows.stat.completed': { en: 'Completed', ar: 'اكتملت' },
  'workflows.stat.abandoned': { en: 'Abandoned', ar: 'مهجورة' },
  'workflows.stat.cancelled': { en: 'Cancelled', ar: 'ملغاة' },
  'workflows.stat.completionRate': { en: 'Completion rate', ar: 'معدل الإكمال' },
  'workflows.search': { en: 'Search workflows…', ar: 'البحث في سير العمل…' },
  'workflows.error.load': { en: "Couldn't load workflows", ar: 'تعذّر تحميل سير العمل' },
  'workflows.empty.title': { en: 'No workflows yet', ar: 'لا يوجد سير عمل بعد' },

  // --- issue detail --------------------------------------------------------
  'issue.backToList': { en: 'Back to issues', ar: 'العودة إلى الاستثناءات' },
  'issue.card.overview': { en: 'Overview', ar: 'نظرة عامة' },
  'issue.card.latestEvent': { en: 'Latest event', ar: 'أحدث حدث' },
  'issue.card.eventsOverTime': { en: 'Events over time', ar: 'الأحداث عبر الزمن' },
  'issue.card.breadcrumbs': { en: 'Breadcrumbs', ar: 'خطوات التتبّع' },
  'issue.field.type': { en: 'Type', ar: 'النوع' },
  'issue.field.release': { en: 'Release', ar: 'الإصدار' },
  'issue.field.fingerprint': { en: 'Fingerprint', ar: 'البصمة' },
  'issue.field.usersAffected': { en: 'Users affected', ar: 'المستخدمون المتأثرون' },
  'issue.field.occurred': { en: 'Occurred', ar: 'وقع في' },
  'issue.action.resolve': { en: 'Resolve', ar: 'حلّ' },
  'issue.action.unresolve': { en: 'Unresolve', ar: 'إلغاء الحل' },
  'issue.action.ignore': { en: 'Ignore', ar: 'تجاهل' },
  'issue.error.load': { en: "Couldn't load issue", ar: 'تعذّر تحميل الاستثناء' },
  'issue.empty.contexts': { en: 'No contexts', ar: 'لا توجد سياقات' },
  'issue.empty.extra': { en: 'No additional data', ar: 'لا توجد بيانات إضافية' },
  'issue.empty.payload': {
    en: 'No event payload available for this issue.',
    ar: 'لا توجد حمولة حدث متاحة لهذا الاستثناء.',
  },
  'issue.empty.filtered': {
    en: 'No occurrences match this filter.',
    ar: 'لا توجد تكرارات تطابق هذا المرشّح.',
  },
  'issue.acrossRange': {
    en: 'Across the selected range and filters',
    ar: 'عبر النطاق والمرشّحات المحددة',
  },

  // --- device tables -------------------------------------------------------
  'devices.unknownDevice': { en: 'Unknown device', ar: 'جهاز غير معروف' },
  'devices.column.browserArch': { en: 'Browser / Arch', ar: 'المتصفح / المعمارية' },
  'issue.stale.body': {
    en: 'Nothing left on this page — these occurrences have gone since it was loaded. Go back for the ones that are still here.',
    ar: 'لم يتبقَّ شيء في هذه الصفحة — اختفت هذه التكرارات منذ تحميلها. ارجع للخلف لرؤية ما تبقّى منها.',
  },
  'transactions.aggregatedBy': {
    en: 'page aggregates these by operation.',
    ar: 'تجمّع هذه المعاملات حسب العملية.',
  },
} as const satisfies Record<string, Message>;
