import type { Message } from '../types';

/**
 * Strings moved out of shared models code (models/constants) into the catalog,
 * so the Arabic UI no longer shows English from those helpers.
 */
export const refreshModels = {
  // --- page-access: page titles and lock tooltips ----------------------------
  // Most page titles reuse `nav.*` / `admin.nav.*`; Onboarding has no nav item.
  'access.page.onboarding': { en: 'Onboarding', ar: 'الإعداد الأولي' },
  'access.requires': { en: 'Requires: {what}', ar: 'يتطلب: {what}' },
  'access.requiresAt': {
    en: 'Requires: {what} at {level} level',
    ar: 'يتطلب: {what} على مستوى {level}',
  },
  'access.level.org': { en: 'organization', ar: 'المؤسسة' },
  'access.level.project': { en: 'project', ar: 'المشروع' },

  // --- permissions: human labels (English mirrors PERMISSION_LABELS) ---------
  'access.perm.issueRead': { en: 'View issues', ar: 'عرض الاستثناءات' },
  'access.perm.issueWrite': {
    en: 'Resolve, assign, and comment on issues',
    ar: 'حل الاستثناءات وإسنادها والتعليق عليها',
  },
  'access.perm.eventRead': {
    en: 'View raw events and analytics',
    ar: 'عرض الأحداث الخام والتحليلات',
  },
  'access.perm.funnelWrite': { en: 'Create and edit funnels', ar: 'إنشاء مسارات التحويل وتعديلها' },
  'access.perm.artifactWrite': {
    en: 'Upload source maps and debug symbols',
    ar: 'رفع خرائط المصدر ورموز التصحيح',
  },
  'access.perm.sourceRead': {
    en: 'View de-obfuscated source code in stack traces',
    ar: 'عرض الشيفرة المصدرية بعد فك التعمية في تتبّعات المكدس',
  },
  'access.perm.monitorRead': { en: 'View uptime monitors', ar: 'عرض مراقبات التوفّر' },
  'access.perm.monitorWrite': {
    en: 'Create and edit uptime monitors',
    ar: 'إنشاء مراقبات التوفّر وتعديلها',
  },
  'access.perm.appRead': { en: 'View apps', ar: 'عرض التطبيقات' },
  'access.perm.appCreate': { en: 'Create apps', ar: 'إنشاء التطبيقات' },
  'access.perm.appUpdate': { en: 'Edit app settings', ar: 'تعديل إعدادات التطبيق' },
  'access.perm.appDelete': { en: 'Delete apps', ar: 'حذف التطبيقات' },
  'access.perm.envRead': {
    en: 'View environments and their ingest keys',
    ar: 'عرض البيئات ومفاتيح الاستقبال الخاصة بها',
  },
  'access.perm.envCreate': { en: 'Create environments', ar: 'إنشاء البيئات' },
  'access.perm.envUpdate': {
    en: 'Rename environments, mute ingest, change the default',
    ar: 'إعادة تسمية البيئات وكتم الاستقبال وتغيير البيئة الافتراضية',
  },
  'access.perm.envDelete': { en: 'Retire environments', ar: 'إيقاف البيئات نهائيًا' },
  'access.perm.envRotateKey': {
    en: 'Rotate environment ingest keys',
    ar: 'تدوير مفاتيح استقبال البيئات',
  },
  'access.perm.projectRead': { en: 'View projects', ar: 'عرض المشاريع' },
  'access.perm.projectCreate': { en: 'Create projects', ar: 'إنشاء المشاريع' },
  'access.perm.projectUpdate': { en: 'Edit project settings', ar: 'تعديل إعدادات المشروع' },
  'access.perm.projectDelete': { en: 'Delete projects', ar: 'حذف المشاريع' },
  'access.perm.memberRead': { en: 'View members', ar: 'عرض الأعضاء' },
  'access.perm.memberManage': {
    en: 'Add, edit, and deactivate members',
    ar: 'إضافة الأعضاء وتعديلهم وتعطيلهم',
  },
  'access.perm.memberCredential': {
    en: 'Reset passwords and sign out devices',
    ar: 'إعادة تعيين كلمات المرور وتسجيل خروج الأجهزة',
  },
  'access.perm.roleManage': { en: 'Create and edit roles', ar: 'إنشاء الأدوار وتعديلها' },
  'access.perm.orgManage': { en: 'Manage organization settings', ar: 'إدارة إعدادات المؤسسة' },
  'access.perm.alertRead': { en: 'View alert rules and channels', ar: 'عرض قواعد التنبيه وقنواته' },
  'access.perm.alertWrite': {
    en: 'Create and edit alert rules and channels',
    ar: 'إنشاء قواعد التنبيه وقنواته وتعديلها',
  },
  'access.perm.piiRead': {
    en: 'View PII scan findings, the mask audit trail, and reveal single values',
    ar: 'عرض نتائج فحص البيانات الشخصية وسجل تدقيق الإخفاء وكشف قيم منفردة',
  },
  'access.perm.piiManage': {
    en: 'Configure scans and permanently mask values (irreversible)',
    ar: 'ضبط عمليات الفحص وإخفاء القيم نهائيًا (لا يمكن التراجع عنه)',
  },

  // --- transaction-detail: span field labels --------------------------------
  'tx.field.name': { en: 'Name', ar: 'الاسم' },
  'tx.field.operation': { en: 'Operation', ar: 'العملية' },
  'tx.field.duration': { en: 'Duration', ar: 'المدة' },
  'tx.field.status': { en: 'Status', ar: 'الحالة' },
  'tx.field.httpMethod': { en: 'HTTP method', ar: 'طريقة HTTP' },
  'tx.field.httpStatus': { en: 'HTTP status', ar: 'حالة HTTP' },
  'tx.field.url': { en: 'URL', ar: 'عنوان URL' },
  'tx.field.user': { en: 'User', ar: 'المستخدم' },
  'tx.field.session': { en: 'Session', ar: 'الجلسة' },
  'tx.field.device': { en: 'Device', ar: 'الجهاز' },
  'tx.field.release': { en: 'Release', ar: 'الإصدار' },
  'tx.field.workflow': { en: 'Workflow', ar: 'سير العمل' },
  'tx.field.occurredAt': { en: 'Occurred at', ar: 'وقت الحدوث' },
  'tx.field.receivedAt': { en: 'Received at', ar: 'وقت الاستلام' },
  'tx.field.finishedAt': { en: 'Finished at', ar: 'وقت الانتهاء' },
  'tx.field.id': { en: 'Transaction id', ar: 'معرّف المعاملة' },

  // --- monitorIntervals: one key per preset (Arabic number agreement differs
  // for 1, 2, 3–10 and 11+, so a singular/plural pair would misread most of them)
  'monitor.interval.1s': { en: '1 second', ar: 'ثانية واحدة' },
  'monitor.interval.5s': { en: '5 seconds', ar: '5 ثوانٍ' },
  'monitor.interval.15s': { en: '15 seconds', ar: '15 ثانية' },
  'monitor.interval.30s': { en: '30 seconds', ar: '30 ثانية' },
  'monitor.interval.1m': { en: '1 minute', ar: 'دقيقة واحدة' },
  'monitor.interval.3m': { en: '3 minutes', ar: '3 دقائق' },
  'monitor.interval.5m': { en: '5 minutes', ar: '5 دقائق' },
  'monitor.interval.15m': { en: '15 minutes', ar: '15 دقيقة' },
  'monitor.interval.30m': { en: '30 minutes', ar: '30 دقيقة' },
  'monitor.interval.1h': { en: '1 hour', ar: 'ساعة واحدة' },
  'monitor.interval.3h': { en: '3 hours', ar: '3 ساعات' },
  'monitor.interval.6h': { en: '6 hours', ar: '6 ساعات' },
  'monitor.interval.12h': { en: '12 hours', ar: '12 ساعة' },
  'monitor.interval.24h': { en: '24 hours', ar: '24 ساعة' },

  // --- ingest-failures: error kinds and recovery summaries -------------------
  'failures.kind.decode': { en: 'Malformed payload', ar: 'حمولة غير سليمة' },
  'failures.kind.dbContention': { en: 'Database contention', ar: 'تزاحم على قاعدة البيانات' },
  'failures.kind.dbUnavailable': { en: 'Database unavailable', ar: 'قاعدة البيانات غير متاحة' },
  'failures.kind.dbFkViolation': { en: 'Unknown reference', ar: 'مرجع غير معروف' },
  'failures.kind.dbConstraint': { en: 'Constraint violation', ar: 'انتهاك قيد' },
  'failures.kind.redis': { en: 'Redis error', ar: 'خطأ في Redis' },
  'failures.kind.unknown': { en: 'Unclassified', ar: 'غير مصنّف' },
  'failures.recovery.nothing': { en: 'Nothing retained.', ar: 'لم يُحتفظ بأي شيء.' },
  // `{occurrences}` and `{events}` / `{dropped}` arrive already pluralised
  // through `common.plural.*`.
  'failures.recovery.none': {
    en: 'None of {occurrences} were retained — nothing can be replayed.',
    ar: 'لم يُحتفظ بأيٍّ من {occurrences} — لا يمكن إعادة تشغيل أي شيء.',
  },
  'failures.recovery.full': {
    en: 'All {events} can be replayed.',
    ar: 'يمكن إعادة تشغيل {events} بالكامل.',
  },
  'failures.recovery.partial': {
    en: '{retained} of {occurrences} retained — {dropped} cannot be recovered.',
    ar: 'احتُفظ بـ{retained} من أصل {occurrences} — لا يمكن استرداد {dropped}.',
  },

  // --- tier-policy: revert confirmations -------------------------------------
  // The closing sentence of each matches `storage.lowerWarning` /
  // `storage.retentionWarning` word for word, in both languages.
  'storage.revertNote.lower': {
    en: 'Reverting drops the {effective}-day override and puts {configured} days in force. On its next cycle the tier worker will export and then drop everything between {configured} and {effective} days old. Raising the number afterwards does not bring it back into Postgres — that needs a restore from cold.',
    ar: 'العودة إلى الافتراضي تُسقط التجاوز البالغ {effective} يومًا وتجعل {configured} يومًا سارية. في دورته التالية سيصدّر عامل الطبقات ثم يحذف كل ما يتراوح عمره بين {configured} و{effective} يومًا. رفع الرقم بعد ذلك لا يعيده إلى Postgres — يتطلب ذلك استعادة من الطبقة الباردة.',
  },
  'storage.revertNote.retention': {
    en: 'Reverting drops the override (currently {from}) and puts {configured} days in force. On the next daily pass, raw sessions older than {configured} days are deleted permanently — sessions have no cold copy, so past-retention days survive only as aggregates. Raising the number afterwards does not bring them back.',
    ar: 'العودة إلى الافتراضي تُسقط التجاوز (حاليًا {from}) وتجعل {configured} يومًا سارية. في المرور اليومي التالي تُحذف الجلسات الخام الأقدم من {configured} يومًا نهائيًا — لا توجد نسخة باردة للجلسات، فالأيام التي تتجاوز مدة الاحتفاظ تبقى كمجاميع فقط. رفع الرقم بعد ذلك لا يعيدها.',
  },
  'storage.revertNote.days': { en: '{n} days', ar: '{n} يومًا' },

  // --- audit: Wall of Shame action phrases -----------------------------------
  // `{noun}` is one of the `audit.noun.*` values below.
  'audit.verb.create': { en: 'Created {noun}', ar: 'أنشأ {noun}' },
  'audit.verb.update': { en: 'Updated {noun}', ar: 'حدّث {noun}' },
  'audit.verb.delete': { en: 'Deleted {noun}', ar: 'حذف {noun}' },
  'audit.verb.retire': { en: 'Retired {noun}', ar: 'أوقف {noun} نهائيًا' },
  'audit.verb.activate': { en: 'Activated {noun}', ar: 'فعّل {noun}' },
  'audit.verb.deactivate': { en: 'Deactivated {noun}', ar: 'عطّل {noun}' },
  'audit.verb.resetPassword': { en: 'Reset password for {noun}', ar: 'أعاد تعيين كلمة مرور {noun}' },
  'audit.verb.emailChangeRequest': {
    en: 'Requested an email change for {noun}',
    ar: 'طلب تغيير بريد {noun} الإلكتروني',
  },
  'audit.verb.emailChangeApproved': {
    en: 'Confirmed a new email for {noun}',
    ar: 'أكّد بريدًا إلكترونيًا جديدًا لحساب {noun}',
  },
  'audit.verb.emailChangeCancelled': {
    en: 'Cancelled an email change for {noun}',
    ar: 'ألغى تغيير بريد {noun} الإلكتروني',
  },
  'audit.verb.revokeSessions': { en: 'Revoked sessions for {noun}', ar: 'ألغى جلسات {noun}' },
  'audit.verb.rotateKey': { en: 'Rotated ingest key for {noun}', ar: 'دوّر مفتاح استقبال {noun}' },
  'audit.verb.enrollmentUpdate': { en: 'Updated enrollment for {noun}', ar: 'حدّث تسجيل {noun}' },
  'audit.verb.upload': { en: 'Uploaded {noun}', ar: 'رفع {noun}' },
  'audit.verb.upsert': { en: 'Configured {noun}', ar: 'ضبط {noun}' },
  'audit.verb.sync': { en: 'Queued sync for {noun}', ar: 'جدول مزامنة {noun}' },
  'audit.verb.test': { en: 'Sent test through {noun}', ar: 'أرسل اختبارًا عبر {noun}' },
  'audit.verb.reveal': { en: 'Revealed PII in {noun}', ar: 'كشف بيانات شخصية في {noun}' },
  'audit.verb.mask': { en: 'Masked PII in {noun}', ar: 'أخفى بيانات شخصية في {noun}' },
  'audit.verb.maskPreview': {
    en: 'Previewed PII mask for {noun}',
    ar: 'عاين إخفاء البيانات الشخصية في {noun}',
  },
  'audit.verb.release': { en: 'Released {noun}', ar: 'حرّر {noun}' },
  'audit.verb.extend': { en: 'Extended {noun}', ar: 'مدّد {noun}' },
  // Auth actions are about the actor, so they carry no noun.
  'audit.auth.login': { en: 'Signed in', ar: 'سجّل الدخول' },
  'audit.auth.loginFailed': { en: 'Failed sign-in', ar: 'فشل تسجيل الدخول' },
  'audit.auth.logout': { en: 'Signed out', ar: 'سجّل الخروج' },
  'audit.auth.passwordChange': { en: 'Changed own password', ar: 'غيّر كلمة مروره' },
  'audit.noun.org': { en: 'organization', ar: 'مؤسسة' },
  'audit.noun.project': { en: 'project', ar: 'مشروع' },
  'audit.noun.app': { en: 'app', ar: 'تطبيق' },
  'audit.noun.environment': { en: 'environment', ar: 'بيئة' },
  'audit.noun.member': { en: 'member', ar: 'عضو' },
  'audit.noun.role': { en: 'role', ar: 'دور' },
  'audit.noun.grant': { en: 'access grant', ar: 'منح وصول' },
  'audit.noun.alertRule': { en: 'alert rule', ar: 'قاعدة تنبيه' },
  'audit.noun.alertChannel': { en: 'alert channel', ar: 'قناة تنبيه' },
  'audit.noun.monitor': { en: 'monitor', ar: 'مراقبة' },
  'audit.noun.artifact': { en: 'artifact', ar: 'ملف رموز' },
  'audit.noun.store': { en: 'store connection', ar: 'اتصال متجر' },
  'audit.noun.inspectorPolicy': { en: 'privacy policy', ar: 'سياسة خصوصية' },
  'audit.noun.tierPolicy': { en: 'tier policy', ar: 'سياسة طبقات' },
  'audit.noun.tierRestore': { en: 'cold-data restore', ar: 'استعادة بيانات باردة' },
  'audit.noun.tierPin': { en: 'restore pin', ar: 'تثبيت استعادة' },
  'audit.noun.pii': { en: 'data', ar: 'بيانات' },
  'audit.value.yes': { en: 'yes', ar: 'نعم' },
  'audit.value.no': { en: 'no', ar: 'لا' },
  'audit.value.empty': { en: '(empty)', ar: '(فارغ)' },
  'audit.value.none': { en: '(none)', ar: '(لا شيء)' },

  // --- artifact-upload: success toast ----------------------------------------
  'sourcemaps.toast.deduped': { en: 'Already uploaded (deduped)', ar: 'مرفوع مسبقًا (مكرر)' },
  'sourcemaps.toast.obfuscationMap': { en: 'Obfuscation map uploaded', ar: 'تم رفع خريطة التعمية' },
  'sourcemaps.toast.symbols': { en: 'Symbols uploaded', ar: 'تم رفع الرموز' },
  'sourcemaps.toast.sourceMap': { en: 'Source map uploaded', ar: 'تم رفع خريطة المصدر' },
  'sourcemaps.toast.withDebugId': { en: '{base} — debug id {id}', ar: '{base} — معرّف التصحيح {id}' },

  // --- notification-prefs: subscription labels and save blockers -------------
  'notif.scope.project': { en: 'Project “{name}”', ar: 'المشروع «{name}»' },
  'notif.scope.app': { en: 'App “{name}”', ar: 'التطبيق «{name}»' },
  'notif.scope.projectDeleted': { en: 'Project (deleted)', ar: 'مشروع (محذوف)' },
  'notif.scope.appDeleted': { en: 'App (deleted)', ar: 'تطبيق (محذوف)' },
  'notif.quiet.alwaysOn': { en: 'Always on', ar: 'مُفعَّل دائمًا' },
  'notif.problem.pickOne': { en: 'Pick one project or one app.', ar: 'اختر مشروعًا واحدًا أو تطبيقًا واحدًا.' },
  'notif.problem.uptimeProject': {
    en: 'Uptime subscriptions are project-scoped.',
    ar: 'اشتراكات التوفّر تكون على مستوى المشروع فقط.',
  },
  'notif.problem.quietPair': {
    en: 'Set both a quiet-hours start and end, or neither.',
    ar: 'حدّد بداية ساعات الهدوء ونهايتها معًا، أو اترك كليهما.',
  },
  'notif.problem.quietRange': {
    en: 'Quiet hours must be times of day.',
    ar: 'يجب أن تكون ساعات الهدوء أوقاتًا من اليوم.',
  },
  'notif.problem.throttle': {
    en: 'Throttle must be between 0 and {max} seconds.',
    ar: 'يجب أن يكون التقييد بين 0 و{max} ثانية.',
  },
  'notif.problem.timezone': {
    en: 'Pick a timezone for quiet hours.',
    ar: 'اختر منطقة زمنية لساعات الهدوء.',
  },

  // --- account-sessions -------------------------------------------------------
  'account.sessions.browserOnOs': { en: '{browser} on {os}', ar: '{browser} على {os}' },

  // --- route-chunk -------------------------------------------------------------
  'ui.route.chunkFailed': { en: 'The page could not be downloaded.', ar: 'تعذّر تنزيل الصفحة.' },

  // --- scope-tree: one-line selection summary -----------------------------------
  'scope.summary.none': { en: 'Nothing selected yet.', ar: 'لم يُحدَّد شيء بعد.' },
  'scope.summary.fullAccess': { en: 'Full access to {org}', ar: 'وصول كامل إلى {org}' },
  'scope.summary.projectOne': { en: '{n} project', ar: 'مشروع واحد' },
  'scope.summary.projectMany': { en: '{n} projects', ar: '{n} من المشاريع' },
  'scope.summary.appOne': { en: '{n} app', ar: 'تطبيق واحد' },
  'scope.summary.appMany': { en: '{n} apps', ar: '{n} من التطبيقات' },
  'scope.summary.envOne': { en: '{n} environment', ar: 'بيئة واحدة' },
  'scope.summary.envMany': { en: '{n} environments', ar: '{n} من البيئات' },
  'scope.summary.separator': { en: ', ', ar: '، ' },

  // --- time-filter: why Apply is disabled ---------------------------------------
  'timeFilter.error.wholeDays': { en: 'Enter a whole number of days', ar: 'أدخل عددًا صحيحًا من الأيام' },
  'timeFilter.error.minDays': { en: 'The window must be at least 1 day', ar: 'يجب ألا تقل النافذة عن يوم واحد' },
  'timeFilter.error.maxDays': {
    en: 'This list looks back at most {max} days',
    ar: 'تعود هذه القائمة {max} يومًا إلى الوراء كحد أقصى',
  },
  'timeFilter.error.chooseStart': { en: 'Choose a start date', ar: 'اختر تاريخ البداية' },
  'timeFilter.error.chooseEnd': { en: 'Choose an end date', ar: 'اختر تاريخ النهاية' },
  'timeFilter.error.order': {
    en: 'The start must be earlier than the end',
    ar: 'يجب أن تكون البداية قبل النهاية',
  },

  // Permission picker group headings (the English label stays the group's id).
  'access.group.issuesEvents': { en: 'Issues & events', ar: 'المشكلات والأحداث' },
  'access.group.analytics': { en: 'Analytics', ar: 'التحليلات' },
  'access.group.symbolication': { en: 'Symbolication', ar: 'فك الترميز' },
  'access.group.uptime': { en: 'Uptime', ar: 'التوافر' },
  'access.group.apps': { en: 'Apps', ar: 'التطبيقات' },
  'access.group.environments': { en: 'Environments', ar: 'البيئات' },
  'access.group.projects': { en: 'Projects', ar: 'المشاريع' },
  'access.group.alerting': { en: 'Alerting', ar: 'التنبيهات' },
  'access.group.privacy': { en: 'Privacy', ar: 'الخصوصية' },
  'access.group.organization': { en: 'Organization', ar: 'المؤسسة' },
  'access.picker.selectedCount': { en: '{n} of {total} selected', ar: 'تم اختيار {n} من {total}' },

  // Shared charts' empty messages (callers may still pass their own).
  'chart.empty.range': { en: 'No data in this range', ar: 'لا توجد بيانات في هذا النطاق' },
  'chart.empty.sessions': { en: 'No sessions in this range', ar: 'لا توجد جلسات في هذا النطاق' },
  'chart.empty.userActivity': { en: 'No user activity in this range', ar: 'لا يوجد نشاط مستخدمين في هذا النطاق' },
  'chart.empty.store': { en: 'No store activity in this range', ar: 'لا يوجد نشاط في المتجر في هذا النطاق' },
  'access.picker.groupCounts': {
    en: '{shown} of {matching} shown · {checked} of {total} total',
    ar: 'معروض {shown} من {matching} · {checked} من {total} إجمالًا',
  },
  'access.picker.noMatch': { en: 'No permissions match “{q}”.', ar: 'لا توجد صلاحيات تطابق «{q}».' },
} as const satisfies Record<string, Message>;
