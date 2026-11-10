import type { Message } from '../types';

/** The `/account` page — profile details, language, and active sessions. */
export const account = {
  'account.title': { en: 'Account', ar: 'الحساب' },
  'account.subtitle': {
    en: 'Your profile and the devices signed in to it.',
    ar: 'ملفك الشخصي والأجهزة المسجّل دخولها إليه.',
  },
  'account.sessions.proxyNote': {
    en: 'All sessions show the same address — the API is behind a proxy and {setting} is not set.',
    ar: 'تعرض كل الجلسات العنوان نفسه — واجهة البرمجة خلف وسيط، ولم يُضبط {setting}.',
  },

  // --- profile card --------------------------------------------------------
  'account.profile.title': { en: 'Profile', ar: 'الملف الشخصي' },
  'account.profile.name': { en: 'Name', ar: 'الاسم' },
  'account.profile.email': { en: 'Email', ar: 'البريد الإلكتروني' },
  'account.profile.lastSignIn': { en: 'Last sign-in', ar: 'آخر تسجيل دخول' },
  'account.profile.changePassword': { en: 'Change password', ar: 'تغيير كلمة المرور' },

  // --- language ------------------------------------------------------------
  'account.language.label': { en: 'Language', ar: 'اللغة' },
  'account.language.hint': {
    en: 'Applies to this browser only.',
    ar: 'ينطبق على هذا المتصفح فقط.',
  },
  'account.language.switchTo': { en: 'Switch to {language}', ar: 'التبديل إلى {language}' },

  // --- sessions card -------------------------------------------------------
  'account.sessions.title': { en: 'Active sessions', ar: 'الجلسات النشطة' },
  'account.sessions.showRevoked': { en: 'Show recent sign-outs', ar: 'عرض عمليات الخروج الأخيرة' },
  'account.sessions.hideRevoked': { en: 'Hide recent sign-outs', ar: 'إخفاء عمليات الخروج الأخيرة' },
  'account.sessions.signOutOthers': { en: 'Sign out other devices', ar: 'تسجيل الخروج من الأجهزة الأخرى' },
  'account.sessions.empty.title': { en: 'No active sessions', ar: 'لا توجد جلسات نشطة' },
  'account.sessions.empty.description': {
    en: 'Sign in again to see this device.',
    ar: 'سجّل الدخول مرة أخرى لرؤية هذا الجهاز.',
  },
  'account.sessions.reloadHint': {
    en: 'Reload the dashboard to manage your devices.',
    ar: 'أعد تحميل لوحة التحكم لإدارة أجهزتك.',
  },
  'account.sessions.column.device': { en: 'Device', ar: 'الجهاز' },
  'account.sessions.column.ip': { en: 'IP', ar: 'عنوان IP' },
  'account.sessions.column.signedIn': { en: 'Signed in', ar: 'وقت الدخول' },
  'account.sessions.column.lastUsed': { en: 'Last used', ar: 'آخر استخدام' },
  'account.sessions.current': { en: 'This device', ar: 'هذا الجهاز' },
  'account.sessions.revoke': { en: 'Sign out', ar: 'تسجيل الخروج' },
  'account.sessions.signedOut': { en: 'Signed out', ar: 'تم تسجيل الخروج' },

  // --- why a revoked session ended ----------------------------------------
  'account.sessions.reason.logout': { en: 'Logged out', ar: 'تسجيل خروج' },
  'account.sessions.reason.userRevoked': {
    en: 'Signed out from your account page',
    ar: 'تسجيل الخروج من صفحة الحساب',
  },
  'account.sessions.reason.userRevokedOthers': {
    en: 'Signed out with "other devices"',
    ar: 'تسجيل الخروج عبر «الأجهزة الأخرى»',
  },
  'account.sessions.reason.adminRevoked': {
    en: 'Signed out by an administrator',
    ar: 'تم تسجيل الخروج بواسطة مسؤول',
  },
  'account.sessions.reason.passwordChanged': {
    en: 'Password changed',
    ar: 'تم تغيير كلمة المرور',
  },
  'account.sessions.reason.deactivated': {
    en: 'Account deactivated',
    ar: 'تم تعطيل الحساب',
  },
  'account.sessions.reason.reuse': {
    en: 'Security: token replay detected',
    ar: 'أمان: تم رصد إعادة استخدام رمز الدخول',
  },
  'account.sessions.reason.ended': { en: 'Ended', ar: 'انتهت' },

  // --- confirmations -------------------------------------------------------
  'account.confirm.revokeOne.title': { en: 'Sign out this device', ar: 'تسجيل الخروج من هذا الجهاز' },
  'account.confirm.revokeOne.body': {
    en: '{device} will be signed out within a few seconds and will have to log in again.',
    ar: 'سيتم تسجيل الخروج من {device} خلال ثوانٍ وسيلزم تسجيل الدخول مجددًا.',
  },
  'account.confirm.revokeOne.fallbackDevice': { en: 'That device', ar: 'ذلك الجهاز' },
  'account.confirm.revokeAll.title': {
    en: 'Sign out other devices',
    ar: 'تسجيل الخروج من الأجهزة الأخرى',
  },
  'account.confirm.revokeAll.body': {
    en: 'Every device except this one will be signed out. You will stay logged in here.',
    ar: 'سيتم تسجيل الخروج من كل الأجهزة عدا هذا الجهاز. ستبقى مسجّلاً هنا.',
  },

  // --- API keys (MCP) ------------------------------------------------------
  'account.apiKeys.title': { en: 'API keys', ar: 'مفاتيح API' },
  'account.apiKeys.thing': { en: 'API keys', ar: 'مفاتيح API' },
  'account.apiKeys.explainer': {
    en: 'Keys let Claude Code and other MCP clients read your Sauron data with your permissions. Read-only. Your organization\'s admins can turn this off.',
    ar: 'تتيح المفاتيح لـ Claude Code وعملاء MCP الآخرين قراءة بيانات Sauron الخاصة بك بصلاحياتك. للقراءة فقط. يمكن لمسؤولي مؤسستك إيقاف هذه الميزة.',
  },
  'account.apiKeys.revocationNote': {
    en: 'Changing your password does not revoke keys; an admin ending your sessions does.',
    ar: 'تغيير كلمة المرور لا يلغي المفاتيح؛ أما إنهاء المسؤول لجلساتك فيلغيها.',
  },
  'account.apiKeys.create': { en: 'Create key', ar: 'إنشاء مفتاح' },
  'account.apiKeys.empty.title': { en: 'No API keys', ar: 'لا توجد مفاتيح API' },
  'account.apiKeys.empty.description': {
    en: 'Create a key to connect Claude Code or another MCP client.',
    ar: 'أنشئ مفتاحًا لربط Claude Code أو أي عميل MCP آخر.',
  },
  'account.apiKeys.column.name': { en: 'Name', ar: 'الاسم' },
  'account.apiKeys.column.key': { en: 'Key', ar: 'المفتاح' },
  'account.apiKeys.column.created': { en: 'Created', ar: 'تاريخ الإنشاء' },
  'account.apiKeys.column.lastUsed': { en: 'Last used', ar: 'آخر استخدام' },
  'account.apiKeys.column.expires': { en: 'Expires', ar: 'ينتهي' },
  'account.apiKeys.never': { en: 'Never', ar: 'أبدًا' },
  'account.apiKeys.expired': { en: 'Expired', ar: 'منتهي' },
  'account.apiKeys.revoke': { en: 'Revoke', ar: 'إلغاء' },
  'account.apiKeys.expiry.30d': { en: '30 days', ar: '30 يومًا' },
  'account.apiKeys.expiry.90d': { en: '90 days', ar: '90 يومًا' },
  'account.apiKeys.expiry.1y': { en: '1 year', ar: 'سنة واحدة' },
  'account.apiKeys.expiry.never': { en: 'Never expires', ar: 'لا ينتهي أبدًا' },
  'account.apiKeys.dialog.title': { en: 'Create API key', ar: 'إنشاء مفتاح API' },
  'account.apiKeys.dialog.name': { en: 'Name', ar: 'الاسم' },
  'account.apiKeys.dialog.namePlaceholder': { en: 'e.g. Laptop, Claude Code', ar: 'مثال: الحاسوب المحمول، Claude Code' },
  'account.apiKeys.dialog.nameHint': { en: '1 to 64 characters.', ar: 'من 1 إلى 64 حرفًا.' },
  'account.apiKeys.dialog.expiry': { en: 'Expires', ar: 'ينتهي' },
  'account.apiKeys.dialog.submit': { en: 'Create key', ar: 'إنشاء المفتاح' },
  'account.apiKeys.reveal.title': { en: 'Your new API key', ar: 'مفتاح API الجديد' },
  'account.apiKeys.reveal.warning': {
    en: 'Copy this key now. It is shown only once and cannot be retrieved later.',
    ar: 'انسخ هذا المفتاح الآن. يظهر مرة واحدة فقط ولا يمكن استرجاعه لاحقًا.',
  },
  'account.apiKeys.reveal.commandLabel': {
    en: 'Add it to Claude Code with this command:',
    ar: 'أضفه إلى Claude Code بهذا الأمر:',
  },
  'account.apiKeys.reveal.tokenLabel': { en: 'Key only', ar: 'المفتاح فقط' },
  'account.apiKeys.toast.revoked': { en: 'API key revoked', ar: 'تم إلغاء مفتاح API' },
  'account.apiKeys.confirm.title': { en: 'Revoke API key', ar: 'إلغاء مفتاح API' },
  'account.apiKeys.confirm.body': {
    en: 'Anything using "{name}" will stop working immediately. This cannot be undone.',
    ar: 'سيتوقف كل ما يستخدم "{name}" عن العمل فورًا. لا يمكن التراجع عن هذا الإجراء.',
  },
} as const satisfies Record<string, Message>;
