import type { Message } from '../types';

/**
 * Strings added by the Quiet refresh for the admin pages (batch C).
 *
 * A separate file so the four page batches, done in parallel, never edit the
 * same catalog file. Keys follow the usual domain prefixes; a key must still
 * be unique across every catalog file (catalog.test.ts).
 */
export const refreshC = {
  // --- Account ---------------------------------------------------------------
  'account.sessions.thing': { en: 'sessions', ar: 'الجلسات' },
  'account.sessions.revokedOne': {
    en: 'That device will be signed out within a few seconds.',
    ar: 'سيُسجَّل خروج ذلك الجهاز خلال ثوانٍ.',
  },
  'account.sessions.revokedOthersOne': {
    en: 'One other device signed out.',
    ar: 'تم تسجيل خروج جهاز آخر واحد.',
  },
  'account.sessions.revokedOthersMany': {
    en: '{n} other devices signed out.',
    ar: 'تم تسجيل خروج {n} من الأجهزة الأخرى.',
  },

  // --- Account: notification subscriptions ------------------------------------
  'notif.thing': { en: 'notifications', ar: 'الإشعارات' },
  'notif.edit': { en: 'Edit subscription', ar: 'تعديل الاشتراك' },
  'notif.kind.uptime': { en: 'Uptime', ar: 'التوفّر' },
  'notif.kind.errorSpike': { en: 'Error rate increasing', ar: 'ارتفاع معدل الأخطاء' },
  'notif.kind.newIssue': { en: 'New issue', ar: 'استثناء جديد' },
  'notif.kind.regression': { en: 'Issue regressed', ar: 'عودة استثناء' },
  'notif.capped': { en: 'Capped', ar: 'مُقيَّد' },
  'notif.state.on': { en: 'On', ar: 'مُفعَّل' },
  'notif.quietFromHint': { en: 'e.g. 1320 = 22:00', ar: 'مثال: 1320 = 22:00' },
  'notif.quietUntilHint': { en: 'e.g. 360 = 06:00', ar: 'مثال: 360 = 06:00' },
  'notif.toast.saved': { en: 'Subscription saved', ar: 'تم حفظ الاشتراك' },
  'notif.toast.saveFailed': { en: 'Couldn’t save the subscription', ar: 'تعذّر حفظ الاشتراك' },
  'notif.toast.enabled': { en: 'Subscription enabled', ar: 'تم تفعيل الاشتراك' },
  'notif.toast.disabled': { en: 'Subscription disabled', ar: 'تم تعطيل الاشتراك' },
  'notif.toast.updateFailed': { en: 'Couldn’t update the subscription', ar: 'تعذّر تحديث الاشتراك' },
  'notif.toast.deleted': { en: 'Subscription deleted', ar: 'تم حذف الاشتراك' },
  'notif.toast.deleteFailed': { en: 'Couldn’t delete the subscription', ar: 'تعذّر حذف الاشتراك' },

  // --- Alerts ----------------------------------------------------------------
  'alerts.thing': { en: 'alerts', ar: 'التنبيهات' },
  'alerts.editChannel': { en: 'Edit channel', ar: 'تعديل القناة' },
  'alerts.editRule': { en: 'Edit alert rule', ar: 'تعديل قاعدة التنبيه' },
  'alerts.newRuleTitle': { en: 'New alert rule', ar: 'قاعدة تنبيه جديدة' },
  'alerts.createChannelFirst': { en: 'Create a channel first', ar: 'أنشئ قناة أولًا' },
  'alerts.editChannelNote': {
    en: 'Type is fixed after creation. Secrets are never returned — leave a credential field blank to keep what is stored, or type a new value to replace it.',
    ar: 'لا يمكن تغيير النوع بعد الإنشاء. لا تُعاد الأسرار أبدًا — اترك حقل بيانات الاعتماد فارغًا للإبقاء على القيمة المخزّنة، أو اكتب قيمة جديدة لاستبدالها.',
  },
  'alerts.editWebhookNote': {
    en: 'The destination URL is stored encrypted and is not shown; retype it to change it.',
    ar: 'يُخزَّن رابط الوجهة مشفّرًا ولا يُعرض؛ أعد كتابته لتغييره.',
  },
  'alerts.field.url': { en: 'URL', ar: 'الرابط' },
  'alerts.field.thresholdMs': { en: 'Threshold (ms)', ar: 'الحد (بالمللي ثانية)' },
  'alerts.field.thresholdCount': { en: 'Threshold (count)', ar: 'الحد (العدد)' },
  'alerts.cmp.atLeast': { en: 'is at least', ar: 'على الأقل' },
  'alerts.cmp.atMost': { en: 'is at most', ar: 'على الأكثر' },
  'alerts.cmp.equals': { en: 'equals', ar: 'يساوي' },
  'alerts.placeholders': { en: 'placeholders.', ar: 'كعناصر نائبة.' },
  'alerts.availableVars': { en: 'Available: {vars}', ar: 'المتاح: {vars}' },
  'alerts.secretStored': { en: 'Stored', ar: 'مخزَّن' },
  'alerts.state.enabled': { en: 'Enabled', ar: 'مُفعَّل' },
  'alerts.state.disabled': { en: 'Disabled', ar: 'معطَّل' },
  'alerts.enable': { en: 'Enable', ar: 'تفعيل' },
  'alerts.disable': { en: 'Disable', ar: 'تعطيل' },
  'alerts.configErrorTitle': {
    en: 'This channel’s stored payload cannot be decrypted, so it cannot be edited',
    ar: 'تعذّر فك تشفير البيانات المخزّنة لهذه القناة، لذا لا يمكن تعديلها',
  },
  'alerts.pinnedMonitor': { en: 'pinned monitor', ar: 'مراقب مثبّت' },
  'alerts.pinnedMonitorOutside': {
    en: 'Pinned monitor (outside the selected project)',
    ar: 'مراقب مثبّت (خارج المشروع المحدد)',
  },
  'alerts.testDelivered': {
    en: 'Test notification delivered to “{name}”.',
    ar: 'تم تسليم الإشعار التجريبي إلى «{name}».',
  },
  'alerts.testFailed': {
    en: 'Test to “{name}” failed: {error}',
    ar: 'فشل الاختبار إلى «{name}»: {error}',
  },
  'alerts.unknownError': { en: 'unknown error', ar: 'خطأ غير معروف' },
  'alerts.trigger.monitorDown': { en: 'Monitor goes down', ar: 'تعطّل المراقب' },
  'alerts.trigger.monitorUp': { en: 'Monitor recovers', ar: 'تعافي المراقب' },
  'alerts.trigger.issueNew': { en: 'New issue appears', ar: 'ظهور مشكلة جديدة' },
  'alerts.trigger.issueRegression': { en: 'Resolved issue regresses', ar: 'عودة مشكلة محلولة' },
  'alerts.trigger.errorThreshold': { en: 'Error count crosses threshold', ar: 'تجاوز عدد الأخطاء الحد' },
  'alerts.trigger.errorSpike': { en: 'Error rate spikes', ar: 'ارتفاع حاد في معدل الأخطاء' },
  'alerts.trigger.eventThreshold': { en: 'Event count crosses threshold', ar: 'تجاوز عدد الأحداث الحد' },
  'alerts.trigger.perfDegradation': { en: 'Latency degrades', ar: 'تدهور زمن الاستجابة' },
  'alerts.delivery.sent': { en: 'Sent', ar: 'مُرسَل' },
  'alerts.delivery.failed': { en: 'Failed', ar: 'فشل' },
  'alerts.delivery.throttled': { en: 'Throttled', ar: 'مُقيَّد' },
  'alerts.delivery.skipped': { en: 'Skipped', ar: 'متخطّى' },
  'alerts.confirmDelete.channelTitle': { en: 'Delete channel?', ar: 'حذف القناة؟' },
  'alerts.confirmDelete.ruleTitle': { en: 'Delete alert rule?', ar: 'حذف قاعدة التنبيه؟' },
  'alerts.confirmDelete.body': {
    en: '“{name}” will be removed. This can’t be undone.',
    ar: 'ستُزال «{name}». لا يمكن التراجع عن ذلك.',
  },
  'alerts.confirmDelete.channelAction': { en: 'Delete channel', ar: 'حذف القناة' },
  'alerts.confirmDelete.ruleAction': { en: 'Delete rule', ar: 'حذف القاعدة' },

  // --- Environments ------------------------------------------------------------
  'environments.thing': { en: 'environments', ar: 'البيئات' },
  'environments.subtitle': {
    en: "Defined by {project} and shared by every app in it — creating, renaming or retiring one below changes it for all of them. Each app's ingest key, mute switch and default stay its own, set per app below.",
    ar: 'تُعرَّف في {project} وتتشاركها كل تطبيقاته — إنشاء بيئة أو إعادة تسميتها أو إيقافها أدناه يغيّرها لها جميعًا. يبقى مفتاح الاستقبال ومفتاح الكتم والإعداد الافتراضي خاصًا بكل تطبيق، ويُضبط لكل تطبيق أدناه.',
  },
  'environments.subtitleNoProject': {
    en: "Defined by this project and shared by every app in it — creating, renaming or retiring one below changes it for all of them. Each app's ingest key, mute switch and default stay its own, set per app below.",
    ar: 'تُعرَّف في هذا المشروع وتتشاركها كل تطبيقاته — إنشاء بيئة أو إعادة تسميتها أو إيقافها أدناه يغيّرها لها جميعًا. يبقى مفتاح الاستقبال ومفتاح الكتم والإعداد الافتراضي خاصًا بكل تطبيق، ويُضبط لكل تطبيق أدناه.',
  },
  'environments.thisApp': { en: 'this app', ar: 'هذا التطبيق' },
  'environments.created': { en: 'created', ar: 'أُنشئت' },
  'environments.retiredAt': { en: 'retired', ar: 'أُوقفت' },
  'environments.retiredCount': { en: '{n} retired', ar: 'الموقوفة: {n}' },
  'environments.muteIngest': { en: 'Mute ingest', ar: 'كتم الاستقبال' },
  'environments.resumeIngest': { en: 'Resume ingest', ar: 'استئناف الاستقبال' },
  'environments.someFailedAre': { en: 'are', ar: 'مسجَّلة فعلًا' },
  'environments.createAction': { en: 'Create environment', ar: 'إنشاء البيئة' },
  'environments.renameAction': { en: 'Rename environment', ar: 'إعادة تسمية البيئة' },
  'environments.retireAction': { en: 'Retire environment', ar: 'إيقاف البيئة' },
  'environments.confirmRotateBody': {
    en: 'Anything {app} reports to “{name}” stops until its DSN is updated. There is no grace period.',
    ar: 'يتوقف كل ما يرسله {app} إلى «{name}» حتى يُحدَّث الـ DSN الخاص به. لا توجد فترة سماح.',
  },
  'environments.confirmRetireBody': {
    en: '“{name}” will be removed from every app in this project. All of their keys for it stop working immediately and it leaves every picker. Existing data stays queryable and is archived to cold storage on the normal schedule. This can’t be undone.',
    ar: 'ستُزال «{name}» من كل تطبيق في هذا المشروع. تتوقف كل مفاتيحها عن العمل فورًا وتختفي من كل قائمة اختيار. تبقى البيانات الحالية قابلة للاستعلام وتُؤرشَف في التخزين البارد وفق الجدول المعتاد. لا يمكن التراجع عن ذلك.',
  },
  'environments.toast.created': {
    en: '“{name}” added to every app in this project.',
    ar: 'أُضيفت «{name}» إلى كل تطبيق في هذا المشروع.',
  },
  'environments.toast.createdReload': {
    en: '“{name}” added to every app in this project. Reload to see its ingest keys.',
    ar: 'أُضيفت «{name}» إلى كل تطبيق في هذا المشروع. أعد التحميل لرؤية مفاتيح الاستقبال.',
  },
  'environments.toast.renamed': {
    en: 'Renamed to “{name}” for every app in this project.',
    ar: 'أُعيدت التسمية إلى «{name}» لكل تطبيق في هذا المشروع.',
  },
  'environments.toast.muted': { en: 'Ingest muted for {app}.', ar: 'تم كتم الاستقبال لـ {app}.' },
  'environments.toast.resumed': { en: 'Ingest resumed for {app}.', ar: 'استُؤنف الاستقبال لـ {app}.' },
  'environments.toast.promoted': {
    en: '“{name}” is now the default environment for {app}.',
    ar: 'أصبحت «{name}» البيئة الافتراضية لـ {app}.',
  },
  'environments.toast.rotated': {
    en: 'Key rotated for {app}. Update this environment’s DSN everywhere it’s configured.',
    ar: 'تم تدوير المفتاح لـ {app}. حدّث الـ DSN لهذه البيئة في كل مكان مُهيّأ فيه.',
  },
  'environments.toast.retired': {
    en: '“{name}” retired project-wide. Its data stays queryable.',
    ar: 'أُوقفت «{name}» على مستوى المشروع. تبقى بياناتها قابلة للاستعلام.',
  },

  // --- Members -------------------------------------------------------------------
  'members.thing': { en: 'members', ar: 'الأعضاء' },
  'members.new': { en: 'New member', ar: 'عضو جديد' },
  'members.subtitle': { en: 'People with access to {org}.', ar: 'الأشخاص الذين لديهم صلاحية الوصول إلى {org}.' },
  'members.thisOrganization': { en: 'this organization', ar: 'هذه المؤسسة' },
  'members.toast.granted': { en: 'Access granted.', ar: 'تم منح الصلاحية.' },
  'members.toast.removed': { en: 'Access removed.', ar: 'تمت إزالة الصلاحية.' },
  'members.toast.updated': { en: 'Access updated.', ar: 'تم تحديث الصلاحية.' },
  'members.toast.deactivated': { en: '{email} can no longer sign in.', ar: 'لم يعد بإمكان {email} تسجيل الدخول.' },
  'members.toast.reactivated': { en: '{email} can sign in again.', ar: 'يمكن لـ {email} تسجيل الدخول مجددًا.' },
  'members.toast.resetSent': {
    en: '{email} has been emailed a link to set a new password.',
    ar: 'أُرسل إلى {email} رابط لتعيين كلمة مرور جديدة.',
  },
  'members.toast.resetCancelled': {
    en: '{email} can sign in with their existing password again.',
    ar: 'يمكن لـ {email} تسجيل الدخول بكلمة المرور الحالية مجددًا.',
  },
  'members.toast.emailRequested': {
    en: 'Nothing has changed yet. {to} must open the emailed link to confirm, and {from} has been told a change was requested so they can stop it.',
    ar: 'لم يتغيّر شيء بعد. يجب أن يفتح {to} الرابط المرسَل للتأكيد، وقد أُبلغ {from} بطلب التغيير ليتمكّن من إيقافه.',
  },
  'members.toast.emailWithdrawn': {
    en: 'The pending email change for {email} was withdrawn.',
    ar: 'سُحب تغيير البريد المعلّق لـ {email}.',
  },
  'members.toast.signedOutOne': { en: '{email} was signed out of 1 device.', ar: 'سُجّل خروج {email} من جهاز واحد.' },
  'members.toast.signedOutMany': {
    en: '{email} was signed out of {n} devices.',
    ar: 'سُجّل خروج {email} من {n} من الأجهزة.',
  },
  'members.confirmDeactivateBody': {
    en: '{email} will be signed out of every device and won’t be able to sign in until reactivated. Their access grants are kept.',
    ar: 'سيُسجَّل خروج {email} من كل الأجهزة ولن يتمكّن من تسجيل الدخول حتى يُعاد تفعيله. تُحفظ صلاحياته.',
  },
  'members.confirmSignOutBody': {
    en: '{name} will be signed out on every device and will have to log in again. Their account stays active.',
    ar: 'سيُسجَّل خروج {name} من كل الأجهزة وسيتعيّن عليه تسجيل الدخول مجددًا. يبقى حسابه نشطًا.',
  },

  // --- Member dialogs --------------------------------------------------------------
  'members.created': { en: 'Member created', ar: 'تم إنشاء العضو' },
  'members.reset.title': { en: 'Reset this member’s password?', ar: 'إعادة تعيين كلمة مرور هذا العضو؟' },
  'members.reset.cancelTitle': { en: 'Cancel the password reset?', ar: 'إلغاء إعادة تعيين كلمة المرور؟' },
  'members.reset.lockout': {
    en: '{email} will not be able to sign in until they use the emailed link.',
    ar: 'لن يتمكّن {email} من تسجيل الدخول حتى يستخدم الرابط المرسَل.',
  },
  'members.reset.cancelLead': {
    en: '{email} will be able to sign in with their existing password again.',
    ar: 'سيتمكّن {email} من تسجيل الدخول بكلمة المرور الحالية مجددًا.',
  },
  'members.reset.action': { en: 'Reset password', ar: 'إعادة تعيين كلمة المرور' },
  'members.reset.cancelAction': { en: 'Cancel reset', ar: 'إلغاء إعادة التعيين' },
  'members.edit.title': { en: 'Edit access', ar: 'تعديل الصلاحية' },
  'members.edit.titleFor': { en: 'Edit access — {name}', ar: 'تعديل الصلاحية — {name}' },
  'members.edit.unknownRole': { en: 'Unknown role', ar: 'دور غير معروف' },
  'members.edit.roleOn': { en: '{role} on {scope}', ar: '{role} على {scope}' },
  'members.edit.and': { en: ' and ', ar: ' و' },
  'members.edit.overlap': {
    en: '{names} both cover {org} — permissions add up.',
    ar: 'يغطي {names} كلاهما {org} — تُجمع الصلاحيات.',
  },
  'members.edit.applying': { en: 'Applying {done} of {total}…', ar: 'جارٍ تطبيق {done} من {total}…' },
  'members.edit.stepGrant': { en: 'Grant {role} on {scope}', ar: 'منح {role} على {scope}' },
  'members.edit.stepRemove': { en: 'Remove {role} on {scope}', ar: 'إزالة {role} من {scope}' },
  'members.edit.keptBecause': {
    en: 'Kept, because some new access could not be granted.',
    ar: 'أُبقي عليه، لأن بعض الصلاحيات الجديدة تعذّر منحها.',
  },
  'members.edit.unreachable': {
    en: 'Stopped — the API is unreachable.',
    ar: 'توقّف — تعذّر الوصول إلى الواجهة البرمجية.',
  },
  'members.edit.alreadyRemoved': { en: 'Already removed.', ar: 'أُزيل بالفعل.' },
  'members.edit.cannotRemove': {
    en: 'You don’t hold every permission in “{role}” at this scope, so you can’t remove this grant.',
    ar: 'لا تملك كل صلاحيات «{role}» في هذا النطاق، لذا لا يمكنك إزالة هذه الصلاحية.',
  },
  'members.edit.somethingWrong': { en: 'Something went wrong', ar: 'حدث خطأ ما' },
  'members.edit.step.done': { en: 'Done', ar: 'تم' },
  'members.edit.step.failed': { en: 'Failed', ar: 'فشل' },
  'members.edit.step.kept': { en: 'Kept', ar: 'أُبقي' },
  'members.edit.noAccess': {
    en: 'This member holds no access in {org}. Add a role to give them some.',
    ar: 'لا يملك هذا العضو أي صلاحية في {org}. أضف دورًا لمنحه بعضها.',
  },
  'members.edit.adding': { en: 'Adding: {list}.', ar: 'إضافة: {list}.' },
  'members.edit.removing': { en: 'Removing: {list}.', ar: 'إزالة: {list}.' },
  'members.edit.destructive': {
    en: 'This removes every grant for {email} in {org}. They will disappear from this list and lose access to this organization. Their account is not deleted — it stays active and they can still sign in.',
    ar: 'يزيل هذا كل صلاحيات {email} في {org}. سيختفي من هذه القائمة ويفقد الوصول إلى هذه المؤسسة. لا يُحذف حسابه — يبقى نشطًا ويمكنه تسجيل الدخول.',
  },
  'members.edit.removeAll': { en: 'Remove all access', ar: 'إزالة كل الصلاحيات' },
  'members.scope.org': { en: 'Organization', ar: 'المؤسسة' },
  'members.scope.project': { en: 'Project: {name}', ar: 'المشروع: {name}' },
  'members.scope.app': { en: 'App: {name}', ar: 'التطبيق: {name}' },
  'members.scope.env': { en: 'Env: {name}', ar: 'البيئة: {name}' },
  'members.removeScopeAccess': { en: 'Remove {scope} access', ar: 'إزالة صلاحية {scope}' },
  'members.menu.cancelReset': { en: 'Cancel password reset', ar: 'إلغاء إعادة تعيين كلمة المرور' },
  'members.menu.signOutAll': { en: 'Sign out all devices', ar: 'تسجيل الخروج من كل الأجهزة' },
  'members.menu.reactivate': { en: 'Reactivate', ar: 'إعادة التفعيل' },
  'members.tree.expand': { en: 'Expand {name}', ar: 'توسيع {name}' },
  'members.tree.collapse': { en: 'Collapse {name}', ar: 'طي {name}' },
  'roles.picker.appliesToOne': {
    en: 'Applies to the 1 shown permission only',
    ar: 'ينطبق على الصلاحية المعروضة فقط',
  },
  'roles.picker.appliesToMany': {
    en: 'Applies to the {n} shown permissions only',
    ar: 'ينطبق على {n} من الصلاحيات المعروضة فقط',
  },

  // --- Roles -------------------------------------------------------------------------
  'roles.thing': { en: 'roles', ar: 'الأدوار' },
  'roles.create': { en: 'Create role', ar: 'إنشاء الدور' },
  'roles.saveChanges': { en: 'Save changes', ar: 'حفظ التغييرات' },
  'roles.copyOf': { en: 'Copy of {name}', ar: 'نسخة من {name}' },
  'roles.dialog.view': { en: 'Role: {name}', ar: 'الدور: {name}' },
  'roles.dialog.edit': { en: 'Edit {name}', ar: 'تعديل {name}' },
  'roles.dialog.copy': { en: 'New role from {name}', ar: 'دور جديد من {name}' },
  'roles.editImpactOne': {
    en: '1 member holds this role. Saving changes their access immediately.',
    ar: 'عضو واحد يحمل هذا الدور. الحفظ يغيّر صلاحياته فورًا.',
  },
  'roles.editImpactMany': {
    en: '{n} members hold this role. Saving changes their access immediately.',
    ar: '{n} من الأعضاء يحملون هذا الدور. الحفظ يغيّر صلاحياتهم فورًا.',
  },
  'roles.deleteTitle': { en: 'Delete “{name}”?', ar: 'حذف «{name}»؟' },
  'roles.deleteImpactOne': {
    en: '1 member will lose this access immediately. This can’t be undone.',
    ar: 'سيفقد عضو واحد هذه الصلاحية فورًا. لا يمكن التراجع عن ذلك.',
  },
  'roles.deleteImpactMany': {
    en: '{n} members will lose this access immediately. This can’t be undone.',
    ar: 'سيفقد {n} من الأعضاء هذه الصلاحية فورًا. لا يمكن التراجع عن ذلك.',
  },
  'roles.typeToConfirm': { en: 'Type “{name}” to confirm', ar: 'اكتب «{name}» للتأكيد' },
  'roles.subtitle': {
    en: 'Permission sets that can be granted to members of {org}.',
    ar: 'مجموعات صلاحيات يمكن منحها لأعضاء {org}.',
  },
  'roles.system': { en: 'System', ar: 'نظامي' },
  'roles.actionsFor': { en: 'Actions for {name}', ar: 'إجراءات {name}' },
  'roles.toast.saved': { en: 'Role “{name}” saved.', ar: 'تم حفظ الدور «{name}».' },
  'roles.toast.deleted': { en: 'Deleted “{name}”.', ar: 'تم حذف «{name}».' },
  'roles.toast.deletedRevokedOne': {
    en: 'Deleted “{name}” and revoked 1 grant.',
    ar: 'تم حذف «{name}» وإلغاء صلاحية واحدة.',
  },
  'roles.toast.deletedRevokedMany': {
    en: 'Deleted “{name}” and revoked {n} grants.',
    ar: 'تم حذف «{name}» وإلغاء {n} من الصلاحيات.',
  },

  // --- Projects --------------------------------------------------------------------
  'projects.appsThing': { en: 'apps', ar: 'التطبيقات' },
  'projects.newApp': { en: 'New app', ar: 'تطبيق جديد' },
  'projects.ingesting': { en: 'Ingesting', ar: 'يستقبل' },
  'projects.paused': { en: 'Paused', ar: 'متوقف مؤقتًا' },
  'projects.confirmDeleteTitle': { en: 'Delete project?', ar: 'حذف المشروع؟' },
  'projects.confirmDeleteBody': {
    en: '“{name}” and every app beneath it will be deleted. This can’t be undone.',
    ar: 'سيُحذف «{name}» وكل تطبيق تحته. لا يمكن التراجع عن ذلك.',
  },
  'projects.deleteAction': { en: 'Delete project', ar: 'حذف المشروع' },
  'projects.toast.created': { en: 'Project created.', ar: 'تم إنشاء المشروع.' },
  'projects.toast.renamed': { en: 'Project renamed.', ar: 'تمت إعادة تسمية المشروع.' },
  'projects.toast.deleted': { en: 'Project deleted.', ar: 'تم حذف المشروع.' },
  'projects.toast.appCreated': { en: 'App created.', ar: 'تم إنشاء التطبيق.' },

  // --- App settings ------------------------------------------------------------------
  'settings.thing': { en: 'app settings', ar: 'إعدادات التطبيق' },
  'settings.ingestOnBody': {
    en: 'This app is accepting events. Disable to stop ingesting without deleting the app.',
    ar: 'يستقبل هذا التطبيق الأحداث. عطّل الاستقبال لإيقافه دون حذف التطبيق.',
  },
  'settings.ingestOffBody': {
    en: 'Ingest is paused. Enable to resume accepting events.',
    ar: 'الاستقبال متوقف مؤقتًا. فعّله لاستئناف استقبال الأحداث.',
  },
  'settings.disableIngest': { en: 'Disable ingest', ar: 'تعطيل الاستقبال' },
  'settings.enableIngest': { en: 'Enable ingest', ar: 'تفعيل الاستقبال' },
  'settings.confirmDeleteTitle': { en: 'Delete app?', ar: 'حذف التطبيق؟' },
  'settings.confirmDeleteBody': {
    en: '“{name}” and all its data will be deleted. This can’t be undone.',
    ar: 'سيُحذف «{name}» وكل بياناته. لا يمكن التراجع عن ذلك.',
  },
  'settings.toast.ingestEnabled': { en: 'Ingest enabled.', ar: 'تم تفعيل الاستقبال.' },
  'settings.toast.ingestDisabled': { en: 'Ingest disabled.', ar: 'تم تعطيل الاستقبال.' },
  'settings.toast.appDeleted': { en: 'App deleted.', ar: 'تم حذف التطبيق.' },

  // --- Overview store section (StoreSection) -----------------------------------------
  'ui.store.thing': { en: 'store metrics', ar: 'مقاييس المتجر' },
  'ui.store.pendingOne': { en: '1 day not yet published by the store.', ar: 'يوم واحد لم ينشره المتجر بعد.' },
  'ui.store.pendingMany': {
    en: '{n} days not yet published by the store.',
    ar: '{n} من الأيام لم ينشرها المتجر بعد.',
  },

  // --- Inspector mask dialog -----------------------------------------------------------
  'mask.typeChanges': {
    en: 'and the key is kept. The TYPE changes, so arithmetic, containment filters and range comparisons stop working for masked rows.',
    ar: 'ويُحتفظ بالمفتاح. يتغيّر النوع، فتتوقف العمليات الحسابية ومرشّحات الاحتواء ومقارنات النطاق عن العمل للصفوف المُخفاة.',
  },
  'mask.eventUserA': { en: ', which backs the', ar: '، الذي يدعم بُعد البحث' },
  'mask.eventUserB': {
    en: 'search dimension. Masked rows will silently stop matching those queries.',
    ar: '. ستتوقف الصفوف المُخفاة بصمت عن مطابقة تلك الاستعلامات.',
  },
  'mask.rows': { en: '{n} rows', ar: '{n} صف' },
  'mask.coldSkipped': {
    en: '{n} row(s) already in cold storage, skipped',
    ar: '{n} صف في التخزين البارد بالفعل، تم تخطّيها',
  },
  'mask.previewIncomplete': {
    en: 'The preview did not complete: {reason}',
    ar: 'لم تكتمل المعاينة: {reason}',
  },
  'mask.boundedBy': { en: 'Bounded by: {what}', ar: 'محدود بـ: {what}' },
  'mask.typeSlug': { en: 'Type the app slug ({slug}) to confirm', ar: 'اكتب معرّف التطبيق ({slug}) للتأكيد' },
  'mask.expired': {
    en: 'This preview is older than {n} seconds and no longer counts as fresh. Close this dialog and start again — the count would be stale for a decision that can’t be undone.',
    ar: 'هذه المعاينة أقدم من {n} ثانية ولم تعد حديثة. أغلق هذا الحوار وابدأ من جديد — سيكون العدد قديمًا لقرار لا يمكن التراجع عنه.',
  },
  'mask.confirmAction': { en: 'Mask permanently in hot Postgres', ar: 'إخفاء نهائي في Postgres الساخن' },
  'mask.toast.queued': {
    en: 'Mask queued. New events are masked within about {n} seconds.',
    ar: 'أُدرج الإخفاء في الطابور. تُخفى الأحداث الجديدة خلال {n} ثانية تقريبًا.',
  },

  // --- App settings: store connections -------------------------------------------
  'stores.thing': { en: 'store connections', ar: 'اتصالات المتاجر' },
  'stores.field.packageName': { en: 'Package name', ar: 'اسم الحزمة' },
  'stores.field.packageNameHint': { en: 'e.g. com.example.app', ar: 'مثال: com.example.app' },
  'stores.field.bucket': { en: 'Reports bucket', ar: 'حاوية التقارير' },
  'stores.field.bucketHint': {
    en: 'From Play Console → Download reports. A gs:// prefix is fine.',
    ar: 'من Play Console ← تنزيل التقارير. لا بأس ببادئة gs://.',
  },
  'stores.field.bundleId': { en: 'Bundle ID', ar: 'معرّف الحزمة' },
  'stores.field.appleAppId': { en: 'Apple app ID', ar: 'معرّف تطبيق Apple' },
  'stores.field.appleAppIdHint': { en: 'The numeric App Store id.', ar: 'المعرّف الرقمي في App Store.' },
  'stores.field.issuerId': { en: 'API key issuer ID', ar: 'معرّف مُصدِر مفتاح API' },
  'stores.field.keyId': { en: 'API key ID', ar: 'معرّف مفتاح API' },
  'stores.field.vendorNumber': { en: 'Vendor number', ar: 'رقم البائع' },
  'stores.secret.playLabel': { en: 'Service account JSON', ar: 'ملف JSON لحساب الخدمة' },
  'stores.secret.playHint': {
    en: 'The full JSON key file for a service account with read access to the reports bucket.',
    ar: 'ملف مفتاح JSON الكامل لحساب خدمة لديه صلاحية قراءة حاوية التقارير.',
  },
  'stores.secret.appleLabel': { en: 'App Store Connect key (.p8)', ar: 'مفتاح App Store Connect (.p8)' },
  'stores.secret.appleHint': {
    en: 'The contents of the AuthKey_XXXX.p8 file, including the BEGIN/END lines.',
    ar: 'محتوى ملف AuthKey_XXXX.p8، بما في ذلك سطرا BEGIN/END.',
  },
  'stores.secret.storedPlaceholder': {
    en: 'Stored — paste a new value to replace it',
    ar: 'مخزَّن — الصق قيمة جديدة لاستبداله',
  },
  'stores.secret.keepHint': {
    en: 'Leave blank to keep the stored credential.',
    ar: 'اتركه فارغًا للإبقاء على بيانات الاعتماد المخزّنة.',
  },
  'stores.status.notConfigured': { en: 'Not configured.', ar: 'غير مُهيّأ.' },
  'stores.status.failed': { en: 'Last sync failed.', ar: 'فشلت آخر مزامنة.' },
  'stores.status.neverSynced': { en: 'Waiting for the first sync.', ar: 'بانتظار أول مزامنة.' },
  'stores.status.pending': {
    en: 'App Store is preparing this report. Apple usually takes 24–48 hours after setup.',
    ar: 'يُعِدّ App Store هذا التقرير. تستغرق Apple عادةً 24–48 ساعة بعد الإعداد.',
  },
  'stores.status.synced': { en: 'Last synced {when}.', ar: 'آخر مزامنة {when}.' },
  'stores.status.syncedPlain': { en: 'Synced.', ar: 'تمت المزامنة.' },
  'stores.confirmRemoveTitle': { en: 'Remove {store} credentials?', ar: 'إزالة بيانات اعتماد {store}؟' },
  'stores.removeAction': { en: 'Remove credentials', ar: 'إزالة بيانات الاعتماد' },
  'stores.toast.saved': { en: '{store} settings saved.', ar: 'تم حفظ إعدادات {store}.' },
  'stores.toast.removed': { en: '{store} credentials removed.', ar: 'أُزيلت بيانات اعتماد {store}.' },
  'stores.toast.queued': {
    en: 'Queued. The sync service will pick this up on its next pass.',
    ar: 'أُدرج في الطابور. ستلتقطه خدمة المزامنة في مرورها التالي.',
  },
  'stores.toast.envSet': { en: 'Store environment set.', ar: 'تم تعيين بيئة المتجر.' },
  'stores.toast.envCleared': { en: 'Store environment cleared.', ar: 'تم مسح بيئة المتجر.' },

  // --- Source maps ---------------------------------------------------------------------
  'sourcemaps.thing': { en: 'source maps', ar: 'خرائط المصدر' },
  'sourcemaps.kind.jsSourcemap': { en: 'JavaScript source map', ar: 'خريطة مصدر JavaScript' },
  'sourcemaps.kind.dartSymbols': {
    en: 'Dart symbols (Flutter) — stack frames',
    ar: 'رموز Dart (Flutter) — إطارات المكدس',
  },
  'sourcemaps.kind.dartObfuscationMap': {
    en: 'Dart obfuscation map — class names',
    ar: 'خريطة تعمية Dart — أسماء الأصناف',
  },
  'sourcemaps.form.jsSourcemap': { en: 'Upload a source map', ar: 'رفع خريطة مصدر' },
  'sourcemaps.form.dartSymbols': { en: 'Upload Dart symbols', ar: 'رفع رموز Dart' },
  'sourcemaps.form.dartObfuscationMap': {
    en: 'Upload a Dart obfuscation map',
    ar: 'رفع خريطة تعمية Dart',
  },
  'sourcemaps.file.jsSourcemap': { en: 'Source map (.map)', ar: 'خريطة المصدر (.map)' },
  'sourcemaps.file.dartSymbols': { en: 'Symbol file (.symbols)', ar: 'ملف الرموز (.symbols)' },
  'sourcemaps.file.dartObfuscationMap': { en: 'Obfuscation map (JSON)', ar: 'خريطة التعمية (JSON)' },
  'sourcemaps.unknownPlatform': { en: 'unknown platform', ar: 'منصة غير معروفة' },
  'sourcemaps.gap.needsMap': {
    en: 'Stack frames resolve. Exception class names do not — upload this build’s obfuscation map.',
    ar: 'تُحَل إطارات المكدس، أما أسماء أصناف الاستثناءات فلا — ارفع خريطة التعمية لهذه البنية.',
  },
  'sourcemaps.gap.needsSymbols': {
    en: 'Exception class names resolve. Stack frames do not — upload this build’s symbol files.',
    ar: 'تُحَل أسماء أصناف الاستثناءات، أما إطارات المكدس فلا — ارفع ملفات الرموز لهذه البنية.',
  },
  'sourcemaps.gap.complete': {
    en: 'Frames and class names both resolve.',
    ar: 'تُحَل الإطارات وأسماء الأصناف كلاهما.',
  },
  'sourcemaps.confirmDelete.title': { en: 'Delete source map?', ar: 'حذف خريطة المصدر؟' },
  'sourcemaps.confirmDelete.body': {
    en: 'Traces for release {release} will no longer be de-obfuscated. This can’t be undone.',
    ar: 'لن يُفك تشويش التتبعات للإصدار {release} بعد الآن. لا يمكن التراجع عن ذلك.',
  },
  'sourcemaps.confirmDelete.bodyNoRelease': {
    en: 'Traces for that release will no longer be de-obfuscated. This can’t be undone.',
    ar: 'لن يُفك تشويش التتبعات لذلك الإصدار بعد الآن. لا يمكن التراجع عن ذلك.',
  },
  'sourcemaps.confirmDelete.action': { en: 'Delete source map', ar: 'حذف خريطة المصدر' },

  // --- Storage ---------------------------------------------------------------------------
  'storage.thing': { en: 'storage', ar: 'التخزين' },
  'storage.policyThing': { en: 'the rotation policy', ar: 'سياسة التدوير' },
  'storage.subtitle.actual': {
    en: 'Actual storage on disk across the deployment, with per-app hot/cold record counts. Postgres figures include indexes and overhead.',
    ar: 'التخزين الفعلي على القرص عبر النشر كله، مع أعداد السجلات الساخنة والباردة لكل تطبيق. تشمل أرقام Postgres الفهارس والحمل الإضافي.',
  },
  'storage.subtitle.estimated': {
    en: 'Estimated storage across the organizations you manage, with per-app hot/cold record counts.',
    ar: 'التخزين التقديري عبر المؤسسات التي تديرها، مع أعداد السجلات الساخنة والباردة لكل تطبيق.',
  },
  'storage.stat.database': { en: 'Database (Postgres)', ar: 'قاعدة البيانات (Postgres)' },
  'storage.stat.estimated': { en: 'Estimated size', ar: 'الحجم التقديري' },
  'storage.days': { en: '{n} days', ar: '{n} يوم' },
  'storage.daysShort': { en: '{n}d', ar: '{n} ي' },
  'storage.daysDefault': { en: '{value} (default)', ar: '{value} (افتراضي)' },
  'storage.retentionOffShort': { en: 'off', ar: 'متوقف' },
  'storage.revertTo': { en: 'Revert to default ({value})', ar: 'العودة إلى الافتراضي ({value})' },
  'storage.hotDaysInvalid': {
    en: 'Must be a whole number of days, at least {min}. A smaller value would put the cutoff at or after now and tier partitions that are still being written to.',
    ar: 'يجب أن يكون عددًا صحيحًا من الأيام، {min} على الأقل. القيمة الأصغر تضع حد القطع عند الآن أو بعده فتنقل أقسامًا ما زالت قيد الكتابة.',
  },
  'storage.lowerWarning': {
    en: 'Lowering this is one-way. On its next cycle the tier worker will export and then drop everything between {from} and {to} days old. Raising the number afterwards does not bring it back into Postgres — that needs a restore from cold.',
    ar: 'خفض هذه القيمة باتجاه واحد. في دورته التالية سيصدّر عامل الطبقات ثم يحذف كل ما يتراوح عمره بين {from} و{to} يومًا. رفع الرقم بعد ذلك لا يعيده إلى Postgres — يتطلب ذلك استعادة من الطبقة الباردة.',
  },
  'storage.retentionInvalid': {
    en: 'Must be 0 (off) or a whole number of days, at least {min}. Sessions younger than that are never retention-dropped.',
    ar: 'يجب أن يكون 0 (متوقف) أو عددًا صحيحًا من الأيام، {min} على الأقل. الجلسات الأحدث من ذلك لا تُحذف بسبب مدة الاحتفاظ أبدًا.',
  },
  'storage.retentionWarning': {
    en: 'This deletes raw sessions older than {n} days on the next daily pass — permanently. Sessions have no cold copy: past-retention days survive only as aggregates, so session lists and drill-downs stop at the retention window. Raising the number afterwards does not bring them back.',
    ar: 'يحذف هذا الجلسات الخام الأقدم من {n} يومًا في المرور اليومي التالي — نهائيًا. لا توجد نسخة باردة للجلسات: الأيام التي تتجاوز مدة الاحتفاظ تبقى كمجاميع فقط، فتتوقف قوائم الجلسات والتفاصيل عند نافذة الاحتفاظ. رفع الرقم بعد ذلك لا يعيدها.',
  },
  'storage.followsImmediately': {
    en: 'Applies without a restart: {list}. Still reading start-time configuration, and so able to disagree about where the boundary is until restarted:',
    ar: 'يُطبَّق دون إعادة تشغيل: {list}. ما زال يقرأ إعدادات وقت البدء، وقد يختلف حول موضع الحد حتى إعادة التشغيل:',
  },
  'storage.pin.expired': { en: 'expired {date}', ar: 'انتهى في {date}' },
  'storage.pin.until': { en: 'until {date}', ar: 'حتى {date}' },
  'storage.pin.expiresSoon': {
    en: 'Expires in {n} day(s). The restored rows will be deleted from Postgres; the Parquet copy is not touched.',
    ar: 'ينتهي خلال {n} يوم. ستُحذف الصفوف المستعادة من Postgres؛ لا تُمس نسخة Parquet.',
  },
  'storage.pin.release': { en: 'Release now', ar: 'تحرير الآن' },
  'storage.keepFor': { en: 'Keep for ({min}–{max} days)', ar: 'الاحتفاظ لمدة ({min}–{max} يومًا)' },
  'storage.restore': { en: 'Restore', ar: 'استعادة' },
  'storage.job.queued': { en: 'Queued', ar: 'في الطابور' },
  'storage.job.running': { en: 'Running', ar: 'قيد التشغيل' },
  'storage.job.succeeded': { en: 'Succeeded', ar: 'نجحت' },
  'storage.job.failed': { en: 'Failed', ar: 'فشلت' },
  'storage.job.cancelled': { en: 'Cancelled', ar: 'أُلغيت' },
  'storage.job.progress': { en: '{done} of {total} rows', ar: '{done} من {total} صف' },
  'storage.job.done': {
    en: 'Restored {n} rows. Held until {date}.',
    ar: 'استُعيد {n} صف. محفوظة حتى {date}.',
  },
  'storage.coldFiles': { en: 'Cold Parquet files ({n})', ar: 'ملفات Parquet الباردة ({n})' },
  'storage.coldFilesTruncated': {
    en: 'Showing the first {shown} of {total} files.',
    ar: 'يُعرض أول {shown} من {total} ملف.',
  },

  // --- Purge -------------------------------------------------------------------------------
  'purge.thing': { en: 'purge options', ar: 'خيارات التطهير' },
  'purge.step2': { en: '2 — Environments', ar: '2 — البيئات' },
  'purge.to': { en: 'To', ar: 'إلى' },
  'purge.preview': { en: 'Preview', ar: 'معاينة' },
  'purge.requestFailed': { en: 'Request failed', ar: 'فشل الطلب' },
  'purge.class.raw': { en: 'Raw', ar: 'خام' },
  'purge.class.rollup': { en: 'Rollup', ar: 'تجميعي' },
  'purge.status.previewing': { en: 'Counting', ar: 'جارٍ العد' },
  'purge.status.previewed': { en: 'Previewed', ar: 'تمت المعاينة' },
  'purge.status.pending': { en: 'Pending', ar: 'معلّق' },
  'purge.status.running': { en: 'Running', ar: 'قيد التشغيل' },
  'purge.status.cancelling': { en: 'Cancelling', ar: 'جارٍ الإلغاء' },
  'purge.status.done': { en: 'Done', ar: 'اكتمل' },
  'purge.status.failed': { en: 'Failed', ar: 'فشل' },
  'purge.status.cancelled': { en: 'Cancelled', ar: 'أُلغي' },
  'purge.phase.idle': { en: 'Idle', ar: 'خامل' },
  'purge.phase.counting': { en: 'Counting', ar: 'جارٍ العد' },
  'purge.phase.delete': { en: 'Deleting', ar: 'جارٍ الحذف' },
  'purge.phase.recompute': { en: 'Recomputing', ar: 'جارٍ إعادة الحساب' },
  'purge.phase.finished': { en: 'Finished', ar: 'انتهى' },
  'purge.coldSurvive': { en: '{n} rows will survive.', ar: 'سيبقى {n} صف.' },
  'purge.coldRotated': {
    en: 'They have already rotated to cold storage',
    ar: 'لقد انتقلت بالفعل إلى التخزين البارد',
  },
  'purge.coldBefore': { en: 'anything before', ar: 'كل ما قبل' },
  'purge.coldStays': {
    en: 'and cannot be deleted. Counters are still recomputed against them, so the numbers stay truthful — but this data stays in your charts.',
    ar: 'ولا يمكن حذفها. تُعاد حساب العدادات مقابلها، فتبقى الأرقام صحيحة — لكن هذه البيانات تبقى في رسومك البيانية.',
  },
  'purge.rollupProgress': {
    en: 'Recomputed {recomputed} rollups, removed {removed} with nothing left.',
    ar: 'أُعيد حساب {recomputed} من التجميعات، وأُزيل {removed} لم يبقَ فيها شيء.',
  },
  'purge.confirmBody': {
    en: 'This deletes the rows above and can’t be undone.',
    ar: 'يحذف هذا الصفوف أعلاه ولا يمكن التراجع عنه.',
  },
  'purge.purgeRows': { en: 'Purge {n} rows', ar: 'تطهير {n} صف' },

  // --- Ingest failures -------------------------------------------------------------------
  'failures.thing': { en: 'ingest failures', ar: 'إخفاقات الاستقبال' },
  'failures.things': { en: 'failure groups', ar: 'مجموعات إخفاق' },
  'failures.empty.title': { en: 'No failing ingest', ar: 'لا توجد إخفاقات في الاستقبال' },
  'failures.empty.body': {
    en: 'Every event the edge accepted has been persisted.',
    ar: 'حُفظ كل حدث قبلته الحافة.',
  },
  'failures.lost': { en: '−{n} lost', ar: '−{n} مفقود' },
  'failures.unknownApp': { en: 'Unknown', ar: 'غير معروف' },
  'failures.autoRetriedYes': { en: 'Yes, 3 attempts', ar: 'نعم، 3 محاولات' },
  'failures.autoRetriedNo': { en: 'No', ar: 'لا' },
  'failures.attempts': { en: '{n} attempts', ar: '{n} محاولة' },
  'failures.notice.requeued': { en: 'Re-queued {n} event(s).', ar: 'أُعيد إدراج {n} حدث في الطابور.' },
  'failures.notice.requeueFailed': {
    en: '{n} could not be re-queued.',
    ar: 'تعذّر إعادة إدراج {n} في الطابور.',
  },
  'failures.notice.unrecoverable': {
    en: '{n} were never retained and cannot be recovered.',
    ar: 'لم يُحتفظ بـ {n} أبدًا ولا يمكن استردادها.',
  },
  'failures.notice.dropped': { en: 'Dropped {kind} permanently.', ar: 'أُسقط {kind} نهائيًا.' },
  'failures.confirmDropBodyOne': {
    en: 'This permanently deletes 1 retained payload for {kind}. It can’t be recovered afterwards.',
    ar: 'يحذف هذا نهائيًا حمولة واحدة محتفظًا بها لـ {kind}. لا يمكن استردادها بعد ذلك.',
  },
  'failures.confirmDropBodyMany': {
    en: 'This permanently deletes {n} retained payloads for {kind}. They can’t be recovered afterwards.',
    ar: 'يحذف هذا نهائيًا {n} من الحمولات المحتفظ بها لـ {kind}. لا يمكن استردادها بعد ذلك.',
  },

  // --- Wall of shame (audit trail) ---------------------------------------------------------
  'audit.thing': { en: 'the audit trail', ar: 'سجل التدقيق' },
  'audit.things': { en: 'actions', ar: 'إجراءات' },
  'audit.exportNothing': {
    en: 'Nothing to export with these filters',
    ar: 'لا شيء للتصدير بهذه المرشّحات',
  },
  'audit.exportTitle': { en: 'Download this view as CSV', ar: 'تنزيل هذا العرض بصيغة CSV' },
  'audit.exportFailed': { en: 'Export failed: {error}', ar: 'فشل التصدير: {error}' },
  'audit.privacy': { en: 'Privacy', ar: 'الخصوصية' },
  'audit.whatChanged': { en: 'What changed', ar: 'ما الذي تغيّر' },
  'audit.noDiffInspector': {
    en: 'Recorded by the PII inspector. The Privacy page has the full detail.',
    ar: 'سجّله فاحص البيانات الشخصية. تحتوي صفحة الخصوصية على التفاصيل الكاملة.',
  },
  'audit.noDiff': {
    en: 'This action has no before/after values — either nothing comparable changed, or the values are credentials that are deliberately never recorded.',
    ar: 'لا توجد قيم قبل/بعد لهذا الإجراء — إما أن شيئًا قابلًا للمقارنة لم يتغيّر، أو أن القيم بيانات اعتماد لا تُسجَّل عمدًا.',
  },

  // --- purge: data kinds, by their wire slug --------------------------------
  'purge.kind.error_events': { en: 'Error events', ar: 'أحداث الأخطاء' },
  'purge.kind.analytics_events': { en: 'Analytics events', ar: 'أحداث التحليلات' },
  'purge.kind.transactions': { en: 'Transactions', ar: 'المعاملات' },
  'purge.kind.inspector': { en: 'Privacy inspector findings', ar: 'نتائج فاحص الخصوصية' },
  'purge.kind.issues': { en: 'Issues', ar: 'المشكلات' },
  'purge.kind.sessions': { en: 'Sessions', ar: 'الجلسات' },
  'purge.kind.devices': { en: 'Devices', ar: 'الأجهزة' },
  'purge.kind.persons': { en: 'Users', ar: 'المستخدمون' },
  'purge.kind.workflows': { en: 'Workflows', ar: 'سير العمل' },
  'purge.column.kind': { en: 'Data', ar: 'البيانات' },
  'purge.confirmLabel': { en: 'Type the app slug to confirm', ar: 'اكتب المعرّف المختصر للتطبيق للتأكيد' },

  // --- members: removing one scope from a grant -----------------------------
  'members.removeScope.title': { en: 'Remove {scope} access?', ar: 'إزالة صلاحية {scope}؟' },
  'members.removeScope.body': {
    en: '{member} loses access to {scope} straight away. You can grant it again later.',
    ar: 'سيفقد {member} الوصول إلى {scope} فورًا. يمكنك منحه الصلاحية مجددًا لاحقًا.',
  },
  'members.removeScope.confirm': { en: 'Remove access', ar: 'إزالة الصلاحية' },
} as const satisfies Record<string, Message>;
