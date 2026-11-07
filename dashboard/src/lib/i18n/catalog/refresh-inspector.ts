import type { Message } from '../types';

/**
 * Strings moved out of shared inspector code (models/constants) into the catalog,
 * so the Arabic UI no longer shows English from those helpers.
 *
 * Technical tokens — table and column names, env vars, Redis keys, commands —
 * stay in Latin script inside the Arabic sentences: they name things the reader
 * has to find or type verbatim.
 */
export const refreshInspector = {
  // --- Detectors (constants/inspectorDetectors.ts) ---------------------------
  'inspector.detector.email': { en: 'Email address', ar: 'عنوان البريد الإلكتروني' },
  'inspector.detector.phoneE164': { en: 'Phone (E.164)', ar: 'رقم الهاتف (E.164)' },
  'inspector.detector.ipv4': { en: 'IPv4 address', ar: 'عنوان IPv4' },
  'inspector.detector.ipv6': { en: 'IPv6 address', ar: 'عنوان IPv6' },
  'inspector.detector.jwt': { en: 'JWT', ar: 'رمز JWT' },
  'inspector.detector.iban': { en: 'IBAN', ar: 'رقم IBAN' },
  'inspector.detector.ssnUs': { en: 'US SSN', ar: 'رقم الضمان الاجتماعي الأمريكي (SSN)' },
  'inspector.detector.creditCard': { en: 'Credit card', ar: 'بطاقة ائتمان' },
  'inspector.detectorHint.email': {
    en: 'requires a dot-separated host',
    ar: 'يتطلب اسم مضيف مفصولًا بنقاط',
  },
  'inspector.detectorHint.phoneE164': {
    en: 'international form only',
    ar: 'بالصيغة الدولية فقط',
  },
  'inspector.detectorHint.jwt': {
    en: 'A signed three-segment token',
    ar: 'رمز موقَّع من ثلاثة مقاطع',
  },
  'inspector.detectorHint.iban': {
    en: 'Bank account number, checksum-validated',
    ar: 'رقم حساب مصرفي، يُتحقَّق منه بمجموع اختباري',
  },
  'inspector.detectorHint.creditCard': {
    en: 'Luhn-validated, so order ids do not match',
    ar: 'يُتحقَّق منه بخوارزمية Luhn، لذا لا تُطابَق أرقام الطلبات',
  },

  // --- Schedule (constants/inspectorSchedules.ts, models/inspector-schedule.ts)
  'inspector.weekday.sun': { en: 'Sun', ar: 'الأحد' },
  'inspector.weekday.mon': { en: 'Mon', ar: 'الاثنين' },
  'inspector.weekday.tue': { en: 'Tue', ar: 'الثلاثاء' },
  'inspector.weekday.wed': { en: 'Wed', ar: 'الأربعاء' },
  'inspector.weekday.thu': { en: 'Thu', ar: 'الخميس' },
  'inspector.weekday.fri': { en: 'Fri', ar: 'الجمعة' },
  'inspector.weekday.sat': { en: 'Sat', ar: 'السبت' },
  'inspector.scheduleDesc.none': { en: 'No scheduled runs', ar: 'لا توجد عمليات تشغيل مجدولة' },
  'inspector.scheduleDesc.daily': {
    en: 'Every day at {time} ({tz})',
    ar: 'كل يوم عند الساعة {time} ({tz})',
  },
  'inspector.scheduleDesc.days': {
    en: 'Every {days} at {time} ({tz})',
    ar: 'كل {days} عند الساعة {time} ({tz})',
  },
  /** Joins the weekday names inside `scheduleDesc.days`. */
  'inspector.scheduleDesc.sep': { en: ', ', ar: '، ' },

  // --- Findings (models/inspector-findings.ts) --------------------------------
  'inspector.atLeast': { en: 'at least {n}', ar: 'ما لا يقل عن {n}' },
  'inspector.badge.recurring.label': { en: 'recurring', ar: 'متكرر' },
  'inspector.badge.recurring.title': {
    en: 'This row is rewritten by every matching event, so an at-rest mask will be undone by the next event. Forward enforcement is what covers it.',
    ar: 'يُعاد كتابة هذا الصف مع كل حدث مطابق، لذا سيُلغي الحدث التالي أي إخفاء للبيانات المخزّنة. ما يغطيه هو التطبيق المستقبلي.',
  },
  'inspector.badge.neverAgesOut.label': { en: 'never ages out', ar: 'لا يتقادم أبدًا' },
  'inspector.badge.neverAgesOut.title': {
    en: 'This row lives in the default partition, which is never exported to cold storage and never dropped. It is the longest-lived copy in the system.',
    ar: 'يقع هذا الصف في القسم الافتراضي، الذي لا يُصدَّر أبدًا إلى التخزين البارد ولا يُحذف أبدًا. إنه أطول النسخ بقاءً في النظام.',
  },
  'inspector.badge.notMaskable.label': { en: 'not maskable', ar: 'غير قابل للإخفاء' },
  'inspector.badge.notMaskable.devices': {
    en: 'Every devices column is COALESCE(EXCLUDED.x, devices.x), so a mask would report success and be overwritten by the next event from that device.',
    ar: 'كل عمود في devices هو COALESCE(EXCLUDED.x, devices.x)، لذا سيُبلغ الإخفاء عن نجاحه ثم يُستبدل بالحدث التالي من ذلك الجهاز.',
  },
  'inspector.badge.notMaskable.identities': {
    en: 'alias_id and distinct_id ARE the identity graph. Masking them merges every masked person into one rather than redacting anyone.',
    ar: 'الحقلان alias_id و distinct_id هما مخطط الهوية نفسه. إخفاؤهما يدمج كل الأشخاص المُخفَين في شخص واحد بدلًا من حجب بيانات أي منهم.',
  },
  'inspector.badge.notMaskable.workflows': {
    en: 'cancel_reason is derived server-side from an analytics event; mask analytics_events.properties instead, which is where the bytes arrive.',
    ar: 'يُشتق cancel_reason على الخادم من حدث تحليلات؛ أخفِ analytics_events.properties بدلًا منه، فهناك تصل البيانات.',
  },
  'inspector.badge.noEnvironment.label': { en: 'no environment', ar: 'بلا بيئة' },
  'inspector.badge.noEnvironment.title': {
    en: 'The platform could not attribute this row to an environment.',
    ar: 'تعذّر على المنصة نسب هذا الصف إلى بيئة.',
  },
  'inspector.badge.appWide.label': { en: 'app-wide table', ar: 'جدول على مستوى التطبيق' },
  'inspector.badge.appWide.title': {
    en: 'This table has no environment column at all, so the finding covers the whole app.',
    ar: 'لا يحتوي هذا الجدول على عمود بيئة إطلاقًا، لذا تشمل النتيجة التطبيق بأكمله.',
  },
  'inspector.badge.detector.title': {
    en: 'Matched by value shape, not by key name.',
    ar: 'طوبِق حسب شكل القيمة، لا حسب اسم المفتاح.',
  },

  // --- Policy form + mask dialog (models/inspector.ts) -----------------------
  'inspector.blocked.noTarget': {
    en: 'Choose the target this policy covers.',
    ar: 'اختر الهدف الذي تشمله هذه السياسة.',
  },
  'inspector.blocked.noMatchers': {
    en: 'Add at least one tracked key or enable one detector — a policy with neither scans nothing and reports no findings, which reads as "clean".',
    ar: 'أضف مفتاحًا متتبَّعًا واحدًا على الأقل أو فعّل كاشفًا واحدًا — فالسياسة الخالية من كليهما لا تفحص شيئًا ولا تُبلغ عن أي نتائج، ما يبدو وكأنه «نظيف».',
  },
  'inspector.describeTarget.wholeValue': { en: 'the whole value', ar: 'القيمة كاملة' },

  // --- What a mask does not reach (UNREACHABLE_COPY) -------------------------
  'inspector.unreachable.headline.what': {
    en: 'Masking rewrites rows in hot Postgres only.',
    ar: 'لا يعيد الإخفاء كتابة الصفوف إلا في قاعدة Postgres الساخنة.',
  },
  'inspector.unreachable.headline.why': {
    en: 'Everything below still holds the original bytes, or is outside this product’s reach.',
    ar: 'كل ما يلي لا يزال يحتفظ بالبيانات الأصلية، أو يقع خارج نطاق هذا المنتج.',
  },
  'inspector.unreachable.headline.bounded': {
    en: 'Read the rows below before confirming.',
    ar: 'اقرأ الصفوف أدناه قبل التأكيد.',
  },
  'inspector.unreachable.coldParquet.what': { en: 'Cold Parquet', ar: 'ملفات Parquet الباردة' },
  'inspector.unreachable.coldParquet.why': {
    en: 'The partition was exported before the mask ran. Parquet is immutable and, after the drop, the only copy.',
    ar: 'صُدِّر القسم قبل تنفيذ الإخفاء. ملفات Parquet غير قابلة للتعديل، وبعد الحذف تصبح النسخة الوحيدة.',
  },
  'inspector.unreachable.coldParquet.bounded': { en: 'Nothing. Permanent.', ar: 'لا شيء. دائم.' },
  'inspector.unreachable.beyondHot.what': {
    en: 'Postgres rows older than TIER_HOT_DAYS',
    ar: 'صفوف Postgres الأقدم من TIER_HOT_DAYS',
  },
  'inspector.unreachable.beyondHot.why': {
    en: 'The retro-mask deliberately stops at the hot boundary.',
    ar: 'يتوقف الإخفاء بأثر رجعي عمدًا عند حدّ الطبقة الساخنة.',
  },
  'inspector.unreachable.beyondHot.bounded': {
    en: 'The tier drop, which destroys the row entirely.',
    ar: 'حذف الطبقة، الذي يُتلف الصف بالكامل.',
  },
  'inspector.unreachable.ingestStream.what': {
    en: 'The Redis ingest stream',
    ar: 'تدفق الاستقبال في Redis',
  },
  'inspector.unreachable.ingestStream.why': {
    en: 'sauron:ingest:stream holds the full serialized job.',
    ar: 'يحتفظ sauron:ingest:stream بالمهمة المُسلسَلة كاملة.',
  },
  'inspector.unreachable.ingestStream.bounded': {
    en: 'XADD … MAXLEN ~ 1000000.',
    ar: 'XADD … MAXLEN ~ 1000000 (الحد الأقصى لطول التدفق).',
  },
  'inspector.unreachable.failedIngest.what': { en: 'Failed ingest', ar: 'الاستقبال الفاشل' },
  'inspector.unreachable.failedIngest.why': {
    en: 'Events that fail to persist are retained as masked copies — in ingest_failures, and in sauron:ingest:dlq when even that write fails.',
    ar: 'تُحفظ الأحداث التي يتعذّر حفظها كنسخ مُخفاة — في ingest_failures، وفي sauron:ingest:dlq عندما تفشل حتى تلك الكتابة.',
  },
  'inspector.unreachable.failedIngest.bounded': {
    en: 'INGEST_FAILURE_RETENTION_DAYS (30d) and INGEST_DLQ_RETENTION_HOURS (7d).',
    ar: 'INGEST_FAILURE_RETENTION_DAYS (30 يومًا) و INGEST_DLQ_RETENTION_HOURS (7 أيام).',
  },
  'inspector.unreachable.breadcrumbs.what': {
    en: 'Per-person breadcrumbs in Redis',
    ar: 'خطوات التتبّع لكل شخص في Redis',
  },
  'inspector.unreachable.breadcrumbs.why': {
    en: 'Up to 100 batches are buffered per person before an error arrives.',
    ar: 'تُخزَّن مؤقتًا حتى 100 دفعة لكل شخص قبل وصول خطأ.',
  },
  'inspector.unreachable.breadcrumbs.bounded': {
    en: 'A 1800 s TTL.',
    ar: 'مدة صلاحية (TTL) قدرها 1800 ثانية.',
  },
  'inspector.unreachable.alertEvents.what': {
    en: 'alert_events.title / .body',
    ar: 'الحقلان alert_events.title / .body',
  },
  'inspector.unreachable.alertEvents.why': {
    en: 'They embed the issue title verbatim.',
    ar: 'يتضمّنان عنوان الاستثناء حرفيًا.',
  },
  'inspector.unreachable.alertEvents.bounded': {
    en: 'ALERT_EVENT_RETENTION_DAYS (90).',
    ar: 'ALERT_EVENT_RETENTION_DAYS (90 يومًا).',
  },
  'inspector.unreachable.deliveredAlerts.what': {
    en: 'Already-delivered alerts',
    ar: 'التنبيهات المُرسَلة بالفعل',
  },
  'inspector.unreachable.deliveredAlerts.why': {
    en: 'Email, Slack, Discord, Matrix, Telegram and webhook messages are gone from our control the moment they send.',
    ar: 'رسائل البريد الإلكتروني و Slack و Discord و Matrix و Telegram و webhook تخرج عن سيطرتنا لحظة إرسالها.',
  },
  'inspector.unreachable.deliveredAlerts.bounded': { en: 'Nothing.', ar: 'لا شيء.' },
  'inspector.unreachable.eventUsers.what': {
    en: 'event_users.properties',
    ar: 'العمود event_users.properties',
  },
  'inspector.unreachable.eventUsers.why': {
    en: 'The identify() write merges with ||, which never removes keys. An at-rest mask is undone by the next identify().',
    ar: 'تدمج كتابة identify() القيم باستخدام ||، الذي لا يحذف المفاتيح أبدًا. يُلغي استدعاء identify() التالي أي إخفاء للبيانات المخزّنة.',
  },
  'inspector.unreachable.eventUsers.bounded': {
    en: 'Forward enforcement only, and only for keys in the mask set.',
    ar: 'التطبيق المستقبلي فقط، وللمفاتيح الموجودة في مجموعة الإخفاء فقط.',
  },
  'inspector.unreachable.devices.what': { en: 'devices.*', ar: 'جميع أعمدة devices.*' },
  'inspector.unreachable.devices.why': {
    en: 'Every column is COALESCE(EXCLUDED.x, devices.x) — a non-null incoming value always wins, and there is no wire field to enforce on.',
    ar: 'كل عمود هو COALESCE(EXCLUDED.x, devices.x) — القيمة الواردة غير الفارغة تفوز دائمًا، ولا يوجد حقل في البيانات المُرسَلة يمكن التطبيق عليه.',
  },
  'inspector.unreachable.devices.bounded': {
    en: 'Not offered: devices is not maskable at all.',
    ar: 'غير متاح: الجدول devices غير قابل للإخفاء إطلاقًا.',
  },
  'inspector.unreachable.sourceLines.what': {
    en: 'Symbolicated source lines',
    ar: 'أسطر المصدر المُرمَّزة',
  },
  'inspector.unreachable.sourceLines.why': {
    en: 'Frames carry context_line / pre_context / post_context — verbatim customer source. Masking a JSON path never touches them.',
    ar: 'تحمل الإطارات context_line / pre_context / post_context — وهي شيفرة مصدر العميل حرفيًا. إخفاء مسار JSON لا يمسّها أبدًا.',
  },
  'inspector.unreachable.sourceLines.bounded': {
    en: 'Redacted from responses only, for callers without source:read.',
    ar: 'تُحجب من الاستجابات فقط، للمستدعين الذين لا يملكون source:read.',
  },
  'inspector.unreachable.backups.what': {
    en: 'Backups, WAL, replicas',
    ar: 'النسخ الاحتياطية وسجل WAL والنسخ المتماثلة',
  },
  'inspector.unreachable.backups.why': {
    en: 'Out of the product’s reach entirely.',
    ar: 'خارج نطاق المنتج تمامًا.',
  },
  'inspector.unreachable.backups.bounded': { en: 'Operator policy.', ar: 'سياسة المشغّل.' },
  'inspector.unreachable.activeUsers.what': {
    en: 'The active-users report stops identifying anyone new through that key',
    ar: 'يتوقف تقرير المستخدمين النشطين عن تعريف أي شخص جديد عبر ذلك المفتاح',
  },
  'inspector.unreachable.activeUsers.why': {
    en: 'The enforcer runs before the active-users pipeline stamps identified_at, so masking a key an app sends as context.user.id means the equality test never passes again. Nobody already stamped is un-identified, but everyone first seen afterwards arrives as a guest and never merges across apps, so the identified share decays with no discontinuity to notice.',
    ar: 'يعمل المُنفِّذ قبل أن يختم مسار المستخدمين النشطين الحقل identified_at، لذا فإن إخفاء مفتاح يرسله التطبيق بوصفه context.user.id يعني أن اختبار التطابق لن ينجح مجددًا. لا يفقد أحد ممن خُتموا مسبقًا تعريفه، لكن كل من يُرى لأول مرة بعد ذلك يصل بوصفه ضيفًا ولا يُدمج أبدًا عبر التطبيقات، فتتراجع نسبة المُعرَّفين تدريجيًا دون أي انقطاع يلفت الانتباه.',
  },
  'inspector.unreachable.activeUsers.bounded': {
    en: 'Nothing. The bytes are gone, so it cannot be recomputed later.',
    ar: 'لا شيء. البيانات زالت، لذا لا يمكن إعادة حسابها لاحقًا.',
  },
} as const satisfies Record<string, Message>;
