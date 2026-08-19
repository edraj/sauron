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
} as const satisfies Record<string, Message>;
