import type { Message } from '../types';

/**
 * Strings added by the Quiet refresh for the auth pages (batch D).
 *
 * A separate file so the four page batches, done in parallel, never edit the
 * same catalog file. Keys follow the usual domain prefixes; a key must still
 * be unique across every catalog file (catalog.test.ts).
 */
export const refreshD = {
  // --- password rules (ResetPassword, ChangePassword) ----------------------
  'auth.password.hint.minLength': { en: 'At least 8 characters.', ar: '8 أحرف على الأقل.' },
  'auth.password.error.tooShort': {
    en: 'Must be at least 8 characters.',
    ar: 'يجب ألا تقل عن 8 أحرف.',
  },
  'auth.password.error.mismatch': {
    en: 'Passwords do not match.',
    ar: 'كلمتا المرور غير متطابقتين.',
  },
  'auth.password.error.reused': {
    en: 'Must be different from your current password.',
    ar: 'يجب أن تختلف عن كلمة المرور الحالية.',
  },
  'auth.reset.toast.updated': {
    en: 'Password updated. Sign in with your new password.',
    ar: 'تم تحديث كلمة المرور. سجّل الدخول بكلمة المرور الجديدة.',
  },
  'auth.change.toast.updated': { en: 'Password updated.', ar: 'تم تحديث كلمة المرور.' },

  // --- onboarding ----------------------------------------------------------
  'auth.onboarding.addAppTo': { en: 'Add an app to {project}', ar: 'أضف تطبيقًا إلى {project}' },
  'auth.onboarding.connectApp': { en: 'Connect {app}', ar: 'اربط {app}' },
  'auth.onboarding.arriving.both': {
    en: 'Errors and analytics events are arriving.',
    ar: 'الأخطاء وأحداث التحليلات تصل الآن.',
  },
  'auth.onboarding.arriving.errors': { en: 'Errors are arriving.', ar: 'الأخطاء تصل الآن.' },
  'auth.onboarding.arriving.events': {
    en: 'Analytics events are arriving.',
    ar: 'أحداث التحليلات تصل الآن.',
  },
  'auth.onboarding.goTo': { en: 'Go to {page}', ar: 'الانتقال إلى {page}' },

  // --- page-level states (PermissionDenied) --------------------------------
  'ui.denied.title': {
    en: "You don't have access to {page}",
    ar: 'ليست لديك صلاحية الوصول إلى {page}',
  },
  'ui.denied.description': {
    en: '{requirement}. Ask an organization owner for access.',
    ar: '{requirement}. اطلب الوصول من أحد مالكي المؤسسة.',
  },
} as const satisfies Record<string, Message>;
