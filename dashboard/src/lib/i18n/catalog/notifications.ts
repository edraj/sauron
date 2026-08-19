import type { Message } from '../types';

/**
 * Personal notification subscriptions — the card on the account page and the
 * dialog that creates one.
 */
export const notifications = {
  'notif.title': { en: 'Notifications', ar: 'الإشعارات' },
  'notif.new': { en: 'New subscription', ar: 'اشتراك جديد' },
  'notif.empty.title': { en: 'No personal notifications yet', ar: 'لا توجد إشعارات شخصية بعد' },
  'notif.empty.body': {
    en: 'Subscribe yourself to uptime or error notifications for a project or app. Only you see and control these.',
    ar: 'اشترك في إشعارات التوافر أو الأخطاء لمشروع أو تطبيق. أنت وحدك من يراها ويتحكم بها.',
  },

  // --- table columns -------------------------------------------------------
  'notif.column.notifyAbout': { en: 'Notify about', ar: 'الإشعار عن' },
  'notif.column.environments': { en: 'Environments', ar: 'البيئات' },
  'notif.column.delivery': { en: 'Delivery', ar: 'التسليم' },
  'notif.column.quietHours': { en: 'Quiet hours', ar: 'ساعات الهدوء' },
  'notif.column.state': { en: 'State', ar: 'الحالة' },
  'notif.state.off': { en: 'Off', ar: 'موقوف' },
  'notif.state.offAccessRemoved': { en: 'Off — access removed', ar: 'موقوف — أُزيلت الصلاحية' },

  // --- delete confirmation -------------------------------------------------
  'notif.delete.title': { en: 'Delete subscription', ar: 'حذف الاشتراك' },
  'notif.delete.body': {
    en: 'You will stop receiving these notifications. This does not affect anyone else.',
    ar: 'ستتوقف عن تلقي هذه الإشعارات. لا يؤثر ذلك على أي شخص آخر.',
  },

  // --- dialog: what to be notified about -----------------------------------
  'notif.dialog.notifyMeAbout': { en: 'Notify me about', ar: 'أَبلِغني عن' },
  'notif.dialog.newIssue': { en: 'A new issue appears', ar: 'ظهور استثناء جديد' },
  'notif.dialog.regression': { en: 'A resolved issue regresses', ar: 'عودة استثناء بعد حلّه' },
  'notif.dialog.errorRate': { en: 'Error rate increasing', ar: 'ارتفاع معدل الأخطاء' },
  'notif.dialog.monitor': { en: 'A monitor goes down or recovers', ar: 'تعطّل مراقب أو تعافيه' },

  // --- dialog: scope and environments --------------------------------------
  'notif.dialog.scope': { en: 'Scope', ar: 'النطاق' },
  'notif.dialog.pickScopeFirst': {
    en: 'Pick a scope to choose environments.',
    ar: 'اختر نطاقًا لتحديد البيئات.',
  },
  'notif.dialog.allEnvironments': {
    en: 'Leave all unticked to be notified about every environment.',
    ar: 'اترك الجميع دون تحديد لتصلك إشعارات عن كل البيئات.',
  },
  'notif.dialog.monitorsProjectWide': {
    en: 'Monitors belong to a whole project, so the environment filter does not apply to uptime.',
    ar: 'تنتمي المراقبات إلى المشروع بأكمله، لذا لا ينطبق مرشّح البيئة على التوافر.',
  },
  'notif.dialog.immutable': {
    en: 'What you are notified about, and where, are fixed when a subscription is created. To change either, delete this one and create a new one.',
    ar: 'يُحدَّد موضوع الإشعار ونطاقه عند إنشاء الاشتراك. لتغيير أيٍّ منهما، احذف هذا الاشتراك وأنشئ آخر.',
  },

  // --- dialog: delivery ----------------------------------------------------
  'notif.dialog.asItHappens': { en: 'As it happens', ar: 'فور وقوعه' },
  'notif.dialog.hourly': { en: 'Hourly summary', ar: 'ملخص كل ساعة' },
  'notif.dialog.daily': { en: 'Daily summary', ar: 'ملخص يومي' },
  'notif.dialog.level': { en: 'Level', ar: 'المستوى' },
  'notif.dialog.anyLevel': { en: 'Any level', ar: 'أي مستوى' },

  // --- dialog: thresholds --------------------------------------------------
  'notif.dialog.throttle': { en: 'Throttle (seconds)', ar: 'التقييد (بالثواني)' },
  'notif.dialog.window': { en: 'Window (seconds)', ar: 'النافذة (بالثواني)' },
  'notif.dialog.minErrors': { en: 'Minimum errors', ar: 'الحد الأدنى للأخطاء' },
  'notif.dialog.increaseFactor': { en: 'Increase factor', ar: 'معامل الزيادة' },

  // --- dialog: quiet hours -------------------------------------------------
  'notif.dialog.quietFrom': { en: 'Quiet from (minute of day)', ar: 'الهدوء من (دقيقة اليوم)' },
  'notif.dialog.quietUntil': { en: 'Quiet until (minute of day)', ar: 'الهدوء حتى (دقيقة اليوم)' },
  'notif.dialog.timezone': { en: 'Timezone', ar: 'المنطقة الزمنية' },
  'notif.dialog.timezoneHint': {
    en: 'IANA name, e.g. Europe/Paris',
    ar: 'اسم IANA، مثل Europe/Paris',
  },
  'notif.dialog.quietHoursHelp': {
    en: 'Quiet hours never drop a notification — they hold it until the window ends, so a night-time outage still reaches you in the morning.',
    ar: 'لا تُسقِط ساعات الهدوء أي إشعار — بل تحتجزه حتى انتهاء النافذة، فيصلك انقطاع الليل صباحًا.',
  },
} as const satisfies Record<string, Message>;
