import type { Message } from '../types';

/** Active users, Funnels, Journeys, and the uptime Monitors pages. */
export const analyze = {
  // --- active users --------------------------------------------------------
  'activeUsers.title': { en: 'Active users', ar: 'المستخدمون النشطون' },
  'activeUsers.subtitle': {
    en: 'Distinct people per UTC day, combined across the apps you pick. Users are matched across apps by the distinct ID your SDK sends — apps must use the same identifier.',
    ar: 'الأشخاص المميزون لكل يوم بتوقيت UTC، مجمّعين عبر التطبيقات التي تختارها. تجري مطابقة المستخدمين بين التطبيقات عبر المعرّف المميز الذي ترسله حزمة التطوير — ويجب أن تستخدم التطبيقات المعرّف نفسه.',
  },
  'activeUsers.doubleCount': {
    en: 'Two apps that name the same person differently count that person twice in',
    ar: 'التطبيقان اللذان يسمّيان الشخص نفسه بطريقتين مختلفتين يحتسبانه مرتين في',
  },
  'activeUsers.card.perDay': { en: 'Active users per day', ar: 'المستخدمون النشطون يوميًا' },
  'activeUsers.card.apps': { en: 'Apps and environments', ar: 'التطبيقات والبيئات' },
  'activeUsers.stat.apps': { en: 'Apps', ar: 'التطبيقات' },
  'activeUsers.stat.peak': { en: 'Peak', ar: 'الذروة' },
  'activeUsers.stat.identified': { en: 'Identified', ar: 'معروفون' },
  'activeUsers.stat.guests': { en: 'Guests', ar: 'ضيوف' },
  'activeUsers.empty.pickApp': { en: 'Pick an app to begin', ar: 'اختر تطبيقًا للبدء' },
  'activeUsers.empty.pickAppBody': {
    en: "Tick one or more apps above and choose which environment each one's numbers come from.",
    ar: 'حدّد تطبيقًا أو أكثر أعلاه واختر البيئة التي تأتي منها أرقام كل تطبيق.',
  },
  'activeUsers.empty.noDays': { en: 'No days in range', ar: 'لا توجد أيام في النطاق' },
  'activeUsers.empty.noDaysBody': {
    en: 'The selected window contains no complete day of data.',
    ar: 'لا تحتوي النافذة المحددة على يوم كامل من البيانات.',
  },

  // --- funnels -------------------------------------------------------------
  'funnels.title': { en: 'Funnels', ar: 'مسارات التحويل' },
  'funnels.subtitle': {
    en: 'Define an ordered set of events and track conversion & drop-off between steps.',
    ar: 'حدّد مجموعة أحداث مرتّبة وتتبّع التحويل والتسرّب بين الخطوات.',
  },
  'funnels.card.builder': { en: 'Builder', ar: 'المُنشئ' },
  'funnels.card.results': { en: 'Results', ar: 'النتائج' },
  'funnels.card.saved': { en: 'Saved funnels', ar: 'المسارات المحفوظة' },
  'funnels.addStep': { en: 'Add step', ar: 'إضافة خطوة' },
  'funnels.removeStep': { en: 'Remove step', ar: 'إزالة الخطوة' },
  'funnels.eventToAdd': { en: 'Event to add', ar: 'الحدث المراد إضافته' },
  'funnels.compute': { en: 'Compute funnel', ar: 'احتساب المسار' },
  'funnels.needTwoSteps': { en: 'Need at least 2 steps', ar: 'يلزم خطوتان على الأقل' },
  'funnels.addTwoSteps': {
    en: 'Add at least two steps to compute a funnel.',
    ar: 'أضف خطوتين على الأقل لاحتساب المسار.',
  },
  'funnels.computePrompt': {
    en: 'Compute a funnel to see conversion & drop-off.',
    ar: 'احتسب مسارًا لعرض التحويل والتسرّب.',
  },
  'funnels.entered': { en: 'Entered', ar: 'دخلوا' },
  'funnels.overallConversion': { en: 'Overall conversion', ar: 'التحويل الإجمالي' },
  'funnels.saveTemplate': { en: 'Save template', ar: 'حفظ القالب' },
  'funnels.saveAsNew': { en: 'Save as new', ar: 'حفظ كجديد' },
  'funnels.update': { en: 'Update', ar: 'تحديث' },
  'funnels.updating': { en: 'Updating…', ar: 'جارٍ التحديث…' },
  'funnels.description': { en: 'Description', ar: 'الوصف' },
  'funnels.loadThis': { en: 'Load this funnel', ar: 'تحميل هذا المسار' },
  'funnels.deleteTitle': { en: 'Delete funnel?', ar: 'حذف المسار؟' },
  'funnels.search': { en: 'Search funnels…', ar: 'البحث في المسارات…' },
  'funnels.placeholder.name': { en: 'Signup flow', ar: 'مسار التسجيل' },
  'funnels.placeholder.description': {
    en: 'What this funnel tracks…',
    ar: 'ما الذي يتتبّعه هذا المسار…',
  },
  'funnels.error.compute': { en: "Couldn't compute funnel", ar: 'تعذّر احتساب المسار' },
  'funnels.error.events': { en: "Couldn't load events", ar: 'تعذّر تحميل الأحداث' },
  'funnels.empty.title': { en: 'No events yet', ar: 'لا توجد أحداث بعد' },
  'funnels.empty.body': {
    en: 'Send events from your SDK to start building conversion funnels.',
    ar: 'أرسل أحداثًا من حزمة التطوير لتبدأ ببناء مسارات التحويل.',
  },

  // --- journeys ------------------------------------------------------------
  'journeys.title': { en: 'Journeys', ar: 'رحلات المستخدمين' },
  'journeys.subtitle': {
    en: 'Trace how users move through your product, one event at a time.',
    ar: 'تتبّع كيفية تنقّل المستخدمين في منتجك، حدثًا بحدث.',
  },
  'journeys.card.userJourneys': { en: 'User journeys', ar: 'رحلات المستخدمين' },
  'journeys.card.entryPoints': { en: 'Top entry points', ar: 'أبرز نقاط الدخول' },
  'journeys.card.transitions': { en: 'Top transitions', ar: 'أبرز الانتقالات' },
  'journeys.explainer': {
    en: "Each column is the Nth event in a user's session; ribbons show how many users moved from one event to the next.",
    ar: 'كل عمود يمثّل الحدث رقم N في جلسة المستخدم؛ وتوضّح الأشرطة عدد المستخدمين الذين انتقلوا من حدث إلى الذي يليه.',
  },
  'journeys.entryExplainer': {
    en: 'The first event users fire when a session begins.',
    ar: 'أول حدث يطلقه المستخدمون عند بدء الجلسة.',
  },
  'journeys.depth': { en: 'Depth', ar: 'العمق' },
  'journeys.depthLabel': { en: 'Journey depth', ar: 'عمق الرحلة' },
  'journeys.range': { en: 'Range', ar: 'النطاق' },
  'journeys.from': { en: 'From', ar: 'من' },
  'journeys.error.load': { en: "Couldn't load journeys", ar: 'تعذّر تحميل الرحلات' },
  'journeys.empty.entries': { en: 'No entry events in this range.', ar: 'لا توجد أحداث دخول في هذا النطاق.' },
  'journeys.empty.transitions': {
    en: 'No transitions between events yet.',
    ar: 'لا توجد انتقالات بين الأحداث بعد.',
  },
  'journeys.empty.title': {
    en: 'Not enough event data to map journeys',
    ar: 'بيانات الأحداث غير كافية لرسم الرحلات',
  },
  'journeys.empty.body': {
    en: 'Once users trigger a sequence of events in a session, their paths will appear here.',
    ar: 'بمجرد أن يطلق المستخدمون سلسلة من الأحداث في جلسة، ستظهر مساراتهم هنا.',
  },

  // --- monitors ------------------------------------------------------------
  'monitors.subtitle': {
    en: 'Track availability and latency for your HTTP and TCP endpoints.',
    ar: 'تتبّع التوافر وزمن الاستجابة لنقاط النهاية HTTP وTCP.',
  },
  'monitors.new': { en: 'New monitor', ar: 'مراقب جديد' },
  'monitors.create': { en: 'Create monitor', ar: 'إنشاء المراقب' },
  'monitors.column.target': { en: 'Target', ar: 'الهدف' },
  'monitors.column.type': { en: 'Type', ar: 'النوع' },
  'monitors.column.method': { en: 'Method', ar: 'الطريقة' },
  'monitors.column.interval': { en: 'Interval', ar: 'الفترة' },
  'monitors.column.latency': { en: 'Latency', ar: 'زمن الاستجابة' },
  'monitors.column.checked': { en: 'Checked', ar: 'آخر فحص' },
  'monitors.column.uptime': { en: 'Uptime', ar: 'التوافر' },
  'monitors.column.uptime24h': { en: 'Uptime 24h', ar: 'التوافر 24 ساعة' },
  'monitors.http': { en: 'HTTP(S)', ar: 'HTTP(S)' },
  'monitors.hostPort': { en: 'Host & port', ar: 'المضيف والمنفذ' },
  'monitors.webhookUrl': { en: 'Webhook URL', ar: 'رابط Webhook' },
  'monitors.webhookHint': {
    en: 'Optional — notified when this monitor changes state.',
    ar: 'اختياري — يُبلَّغ عند تغيّر حالة هذا المراقب.',
  },
  'monitors.placeholder.name': { en: 'API health check', ar: 'فحص سلامة واجهة البرمجة' },
  'monitors.placeholder.hostPort': { en: 'db.example.com:5432', ar: 'db.example.com:5432' },
  'monitors.empty.title': { en: 'No monitors yet', ar: 'لا توجد مراقبات بعد' },
  'monitors.empty.body': {
    en: 'Add an HTTP or TCP monitor to start tracking uptime, latency, and incidents.',
    ar: 'أضف مراقب HTTP أو TCP لتبدأ تتبّع التوافر وزمن الاستجابة والحوادث.',
  },

  // --- monitor detail ------------------------------------------------------
  'monitor.backToList': { en: 'Back to Uptime', ar: 'العودة إلى التوافر' },
  'monitor.notFound': { en: 'Monitor not found', ar: 'المراقب غير موجود' },
  'monitor.card.recentChecks': { en: 'Recent checks', ar: 'الفحوص الأخيرة' },
  'monitor.card.incidents': { en: 'Incidents', ar: 'الحوادث' },
  'monitor.stat.uptime7d': { en: 'Uptime 7d', ar: 'التوافر 7 أيام' },
  'monitor.stat.uptime30d': { en: 'Uptime 30d', ar: 'التوافر 30 يومًا' },
  'monitor.column.result': { en: 'Result', ar: 'النتيجة' },
  'monitor.column.code': { en: 'Code', ar: 'الرمز' },
  'monitor.column.cause': { en: 'Cause', ar: 'السبب' },
  'monitor.state.ongoing': { en: 'Ongoing', ar: 'جارٍ' },
  'monitor.state.resolved': { en: 'Resolved', ar: 'محلول' },
  'monitor.sort.newest': { en: 'Newest', ar: 'الأحدث' },
  'monitor.sort.oldest': { en: 'Oldest', ar: 'الأقدم' },
  'monitor.checkInterval': { en: 'Check interval', ar: 'فترة الفحص' },
  'monitor.changeInterval': { en: 'Change check interval', ar: 'تغيير فترة الفحص' },
  'monitor.changeIntervalConfirm': { en: 'Change interval', ar: 'تغيير الفترة' },
  'monitor.delete': { en: 'Delete monitor', ar: 'حذف المراقب' },
  'monitor.webhookNote': {
    en: 'A webhook is notified when this monitor changes state',
    ar: 'يُبلَّغ Webhook عند تغيّر حالة هذا المراقب',
  },
  'monitor.empty.checks': { en: 'No checks yet', ar: 'لا توجد فحوص بعد' },
  'monitor.empty.checksBody': {
    en: "This monitor hasn't run a check yet. Results appear here once the prober reports in.",
    ar: 'لم يجرِ هذا المراقب أي فحص بعد. ستظهر النتائج هنا بمجرد أن يبلّغ الفاحص.',
  },
  'monitor.empty.incidents': { en: 'No incidents', ar: 'لا توجد حوادث' },
  'monitor.empty.incidentsBody': {
    en: 'No downtime has been recorded for this monitor.',
    ar: 'لم يُسجَّل أي انقطاع لهذا المراقب.',
  },

  // --- retention ------------------------------------------------------------
  'nav.retention': { en: 'Retention', ar: 'الاحتفاظ' },
  'retention.title': { en: 'Retention', ar: 'الاحتفاظ' },
  'retention.subtitle': {
    en: 'Whether the people who arrived came back.',
    ar: 'ما إذا كان الأشخاص الذين وصلوا قد عادوا.',
  },
  'retention.cohort': { en: 'Cohort', ar: 'المجموعة' },
  'retention.users': { en: 'Users', ar: 'المستخدمون' },
  'retention.action.day1Down': {
    en: 'Turn on the error split and compare day-1 for error-exposed versus error-free period-0 users.',
    ar: 'فعّل تقسيم الأخطاء وقارن اليوم الأول بين من واجهوا خطأ ومن لم يواجهوه في الفترة صفر.',
  },
  'retention.action.day1Up': {
    en: 'Turn on the error split and check whether the gain holds for error-free users too.',
    ar: 'فعّل تقسيم الأخطاء وتحقّق مما إذا كان التحسّن يشمل من لم يواجهوا أخطاءً أيضًا.',
  },
  'retention.action.day1Flat': {
    en: 'Turn on the error split and check whether the flat average hides a gap between grids.',
    ar: 'فعّل تقسيم الأخطاء وتحقّق مما إذا كان المتوسط المستقر يخفي فجوة بين الشبكتين.',
  },
  'retention.action.churnReplace': {
    en: 'Open Users, sort by session count, and check whether returning people are being counted as new.',
    ar: 'افتح المستخدمين، ورتّب حسب عدد الجلسات، وتحقّق مما إذا كان العائدون يُحتسبون كجدد.',
  },
  'retention.action.quickGood': {
    en: 'Open Users and check whether the active-per-day bars grow, not just the new-user bars.',
    ar: 'افتح المستخدمين وتحقّق من نمو أعمدة النشطين يوميًا، لا أعمدة الجدد وحدها.',
  },
  'retention.action.quickBad': {
    en: 'Sort the churn-risk table by errors to see whether those silent users hit errors before leaving.',
    ar: 'رتّب جدول المعرّضين للخطر حسب الأخطاء لترى هل واجه الصامتون أخطاءً قبل مغادرتهم.',
  },
  'retention.action.cliff': {
    en: 'Filter Events to that date and check whether any events at all were received.',
    ar: 'صفِّ الأحداث على ذلك التاريخ وتحقّق مما إذا وصلت أي أحداث على الإطلاق.',
  },
  'retention.action.bestCohort': {
    en: "Open Journeys, set the range to that period, and compare its top paths with a weaker period's.",
    ar: 'افتح الرحلات، واضبط النطاق على تلك الفترة، وقارن أبرز مساراتها بفترة أضعف.',
  },
  'retention.actionLink.churnReplace': { en: 'Users list', ar: 'قائمة المستخدمين' },
  'retention.actionLink.quickGood': { en: 'Users list', ar: 'قائمة المستخدمين' },
  'retention.actionLink.cliff': { en: 'Events explorer', ar: 'مستكشف الأحداث' },
  'retention.actionLink.bestCohort': { en: 'Journeys explorer', ar: 'مستكشف الرحلات' },
  'retention.insights.title': { en: 'Insights', ar: 'الرؤى' },
  'retention.insight.day1Up': {
    en: 'Day-1 retention averages {pct} and is improving — up {delta} between the older and newer half of these cohorts.',
    ar: 'متوسط الاحتفاظ في اليوم الأول {pct} وهو في تحسّن — ارتفع {delta} بين النصف الأقدم والأحدث من هذه الأفواج.',
  },
  'retention.insight.day1Down': {
    en: 'Day-1 retention averages {pct} and is declining — down {delta} between the older and newer half of these cohorts.',
    ar: 'متوسط الاحتفاظ في اليوم الأول {pct} وهو في تراجع — انخفض {delta} بين النصف الأقدم والأحدث من هذه الأفواج.',
  },
  'retention.insight.day1Flat': {
    en: 'Day-1 retention averages {pct}, roughly flat across these cohorts.',
    ar: 'متوسط الاحتفاظ في اليوم الأول {pct}، مستقر تقريبًا عبر هذه الأفواج.',
  },
  'retention.insight.churnReplace': {
    en: '{pct} of each period\u2019s active users are first-timers — activity depends on continuous acquisition rather than a returning base.',
    ar: '{pct} من المستخدمين النشطين في كل فترة هم جدد — يعتمد النشاط على الاستقطاب المستمر لا على قاعدة عائدة.',
  },
  'retention.insight.quickGood': {
    en: 'Gaining {ratio} users (new + resurrected) for every one going dormant.',
    ar: 'مقابل كل مستخدم يخمل، يُكتسب {ratio} مستخدم (جديد + عائد من الخمول).',
  },
  'retention.insight.quickBad': {
    en: 'Only {ratio} users gained (new + resurrected) for every one going dormant — the base is shrinking.',
    ar: 'يُكتسب {ratio} مستخدم فقط (جديد + عائد) مقابل كل مستخدم يخمل — القاعدة تتقلص.',
  },
  'retention.insight.cliff': {
    en: 'On {date} nobody was active while previously-active users went dormant — investigate what happened that period.',
    ar: 'في {date} لم يكن أحد نشطًا بينما خمل مستخدمون كانوا نشطين — راجع ما حدث في تلك الفترة.',
  },
  'retention.insight.bestCohort': {
    en: 'Best cohort: {date}, with {pct} returning the next period — worth comparing what that group experienced.',
    ar: 'أفضل فوج: {date} بعودة {pct} في الفترة التالية — يستحق مقارنة ما مرّ به هذا الفوج.',
  },
  'retention.churn.errors': { en: 'Errors', ar: 'الأخطاء' },
  'retention.churn.sessions': { en: 'Sessions', ar: 'الجلسات' },
  'retention.churn.silentFor': { en: 'Silent for', ar: 'صامت منذ' },
  'retention.churn.tenure': { en: 'Active tenure', ar: 'مدة النشاط' },
  'retention.churn.firstSeen': { en: 'First seen', ar: 'أول ظهور' },
  'retention.churn.nDays': { en: '{n} days', ar: '{n} يومًا' },
  'retention.churn.viewProfile': { en: 'View profile \u2192', ar: '\u2190 عرض الملف الشخصي' },
  'retention.day0Title': {
    en: "Day 0 is each user's first day — the day they were first seen. 100% by definition.",
    ar: 'اليوم 0 هو أول يوم لكل مستخدم — يوم ظهوره لأول مرة. \u200e100% بحكم التعريف.',
  },
  'retention.dayNTitle': {
    en: "{n} days after each user's own first day",
    ar: 'بعد {n} أيام من أول يوم لكل مستخدم',
  },
  'retention.weekNTitle': {
    en: "{n} weeks after each user's own first week",
    ar: 'بعد {n} أسابيع من أول أسبوع لكل مستخدم',
  },
  'retention.legend.periods': {
    en: "Day N counts from each user's own first day, not the calendar. Click any cell to switch between % and user counts.",
    ar: 'يُحسب اليوم N من أول يوم لكل مستخدم، لا من التقويم. انقر أي خلية للتبديل بين النسبة وعدد المستخدمين.',
  },
  'retention.mode.label': { en: 'Cell values', ar: 'قيم الخلايا' },
  'retention.mode.countTitle': {
    en: 'Show absolute user counts instead of percentages',
    ar: 'عرض أعداد المستخدمين بدلاً من النسب المئوية',
  },
  'retention.dayN': { en: 'Day {n}', ar: 'اليوم {n}' },
  'retention.weekN': { en: 'Week {n}', ar: 'الأسبوع {n}' },
  'retention.cellTitle': {
    en: '{users} of {size} returned',
    ar: 'عاد {users} من {size}',
  },
  'retention.legend.empty': {
    en: 'Empty cells are periods that have not elapsed yet — not zero retention.',
    ar: 'الخلايا الفارغة هي فترات لم تنقضِ بعد، وليست احتفاظًا صفريًا.',
  },
  'retention.granularity.day': { en: 'Daily', ar: 'يومي' },
  'retention.granularity.week': { en: 'Weekly', ar: 'أسبوعي' },
  'retention.notReady.title': {
    en: 'Historical retention needs a one-time backfill',
    ar: 'يحتاج الاحتفاظ التاريخي إلى تعبئة أولية لمرة واحدة',
  },
  // No {command} placeholder: the command itself is rendered below in a
  // copyable CodeBlock, so interpolating it here produced "Run  on the server".
  'retention.notReady.body': {
    en: 'Run this on the server to cover data from before this feature was installed. Until then this page has nothing to show for this app.',
    ar: 'شغّل هذا على الخادم لتغطية البيانات السابقة لتثبيت هذه الميزة. حتى ذلك الحين لا يوجد ما يُعرض هنا لهذا التطبيق.',
  },
  'retention.lifecycle.title': { en: 'Lifecycle', ar: 'دورة الحياة' },
  'retention.lifecycle.subtitle': {
    en: 'Is growth real, or churn and replace?',
    ar: 'هل النمو حقيقي أم فقدان واستبدال؟',
  },
  'retention.lifecycle.new': { en: 'New', ar: 'جديد' },
  'retention.lifecycle.returning': { en: 'Returning', ar: 'عائد' },
  'retention.lifecycle.resurrected': { en: 'Resurrected', ar: 'مستعاد' },
  'retention.lifecycle.active': { en: 'Active', ar: 'النشطون' },
  'retention.lifecycle.dormant': { en: 'Dormant', ar: 'خامل' },
  'retention.churn.title': { en: 'At risk', ar: 'معرّضون للفقد' },
  'retention.churn.subtitle': {
    en: 'Active before, silent for {days} days.',
    ar: 'كانوا نشطين سابقًا، وصامتون منذ {days} يومًا.',
  },
  'retention.churn.person': { en: 'Person', ar: 'الشخص' },
  'retention.churn.lastSeen': { en: 'Last seen', ar: 'آخر ظهور' },
  'retention.churn.events': { en: 'Events', ar: 'الأحداث' },
  'retention.errorSplit.toggle': {
    en: 'Compare users who hit an error',
    ar: 'قارن المستخدمين الذين واجهوا خطأ',
  },
  'retention.errorSplit.exposed': { en: 'Hit an error', ar: 'واجهوا خطأ' },
  'retention.errorSplit.clean': { en: 'No error', ar: 'بلا خطأ' },
  'retention.errorSplit.caveat': {
    en: 'An association, not a cause. Exposure is measured in the first period only.',
    ar: 'ارتباط وليس سببًا. يُقاس التعرض في الفترة الأولى فقط.',
  },
  'retention.export': { en: 'Export CSV', ar: 'تصدير CSV' },
  'retention.updating': { en: 'Updating…', ar: 'جارٍ التحديث…' },
  'retention.churn.loadMore': { en: 'Load more', ar: 'تحميل المزيد' },
  'retention.empty.title': { en: 'No cohorts yet', ar: 'لا توجد مجموعات بعد' },
  'retention.empty.body': {
    en: 'Nobody has been first seen inside this window yet.',
    ar: 'لم يظهر أي شخص لأول مرة ضمن هذه النافذة بعد.',
  },
} as const satisfies Record<string, Message>;
