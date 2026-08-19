import type { Message, PluralMessage } from '../types';

/**
 * Strings shared across pages: verbs on buttons, table furniture, and the
 * handful of sentences every list page renders.
 *
 * Keys are dotted and domain-prefixed. `common.*` is for wording that is
 * genuinely generic — if a string only reads correctly on one page, it belongs
 * in that page's file instead, where the surrounding context is visible to
 * whoever revises the Arabic.
 */
export const common = {
  // --- verbs ---------------------------------------------------------------
  'common.save': { en: 'Save', ar: 'حفظ' },
  'common.cancel': { en: 'Cancel', ar: 'إلغاء' },
  'common.delete': { en: 'Delete', ar: 'حذف' },
  'common.edit': { en: 'Edit', ar: 'تعديل' },
  'common.close': { en: 'Close', ar: 'إغلاق' },
  'common.confirm': { en: 'Confirm', ar: 'تأكيد' },
  'common.apply': { en: 'Apply', ar: 'تطبيق' },
  'common.reset': { en: 'Reset', ar: 'إعادة تعيين' },
  'common.clear': { en: 'Clear', ar: 'مسح' },
  'common.remove': { en: 'Remove', ar: 'إزالة' },
  'common.create': { en: 'Create', ar: 'إنشاء' },
  'common.view': { en: 'View', ar: 'عرض' },
  'common.search': { en: 'Search', ar: 'بحث' },
  'common.refresh': { en: 'Refresh', ar: 'تحديث' },
  'common.retry': { en: 'Retry', ar: 'إعادة المحاولة' },
  'common.copy': { en: 'Copy', ar: 'نسخ' },
  'common.copied': { en: 'Copied', ar: 'تم النسخ' },
  'common.copyToClipboard': { en: 'Copy to clipboard', ar: 'نسخ إلى الحافظة' },
  'common.download': { en: 'Download', ar: 'تنزيل' },
  'common.upload': { en: 'Upload', ar: 'رفع' },
  'common.select': { en: 'Select', ar: 'اختيار' },
  'common.done': { en: 'Done', ar: 'تم' },
  'common.dismiss': { en: 'Dismiss', ar: 'تجاهل' },
  'common.collapse': { en: 'Collapse', ar: 'طي' },
  'common.showMore': { en: 'Show more', ar: 'عرض المزيد' },
  'common.showLess': { en: 'Show less', ar: 'عرض أقل' },

  // --- navigation ----------------------------------------------------------
  'common.previous': { en: 'Previous', ar: 'السابق' },
  'common.back': { en: 'Back', ar: 'رجوع' },
  'common.backTo': { en: 'Back to {target}', ar: 'العودة إلى {target}' },

  // --- state ---------------------------------------------------------------
  'common.loading': { en: 'Loading…', ar: 'جارٍ التحميل…' },
  'common.saving': { en: 'Saving…', ar: 'جارٍ الحفظ…' },
  'common.noResults': { en: 'No results', ar: 'لا توجد نتائج' },
  'common.noData': { en: 'No data', ar: 'لا توجد بيانات' },
  'common.none': { en: 'None', ar: 'لا شيء' },
  'common.all': { en: 'All', ar: 'الكل' },
  'common.unknown': { en: 'Unknown', ar: 'غير معروف' },
  'common.somethingWentWrong': { en: 'Something went wrong', ar: 'حدث خطأ ما' },

  // --- fields --------------------------------------------------------------
  'common.name': { en: 'Name', ar: 'الاسم' },
  'common.email': { en: 'Email', ar: 'البريد الإلكتروني' },
  'common.password': { en: 'Password', ar: 'كلمة المرور' },
  'common.status': { en: 'Status', ar: 'الحالة' },
  'common.actions': { en: 'Actions', ar: 'الإجراءات' },
  'common.details': { en: 'Details', ar: 'التفاصيل' },
  'common.key': { en: 'Key', ar: 'المفتاح' },
  'common.total': { en: 'Total', ar: 'الإجمالي' },
  'common.yes': { en: 'Yes', ar: 'نعم' },
  'common.no': { en: 'No', ar: 'لا' },
} as const satisfies Record<string, Message>;

/**
 * Count-dependent shared strings.
 *
 * `{n}` is substituted with the count already run through the locale's number
 * formatter, so the caller passes a raw number and still gets thousands
 * separators.
 */
export const commonPlurals = {
  'common.plural.event': {
    en: { one: '{n} event', other: '{n} events' },
    ar: {
      zero: 'لا أحداث',
      one: 'حدث واحد',
      two: 'حدثان',
      few: '{n} أحداث',
      many: '{n} حدثًا',
      other: '{n} حدث',
    },
  },
  'common.plural.session': {
    en: { one: '{n} session', other: '{n} sessions' },
    ar: {
      zero: 'لا جلسات',
      one: 'جلسة واحدة',
      two: 'جلستان',
      few: '{n} جلسات',
      many: '{n} جلسة',
      other: '{n} جلسة',
    },
  },
  'common.plural.user': {
    en: { one: '{n} user', other: '{n} users' },
    ar: {
      zero: 'لا مستخدمين',
      one: 'مستخدم واحد',
      two: 'مستخدمان',
      few: '{n} مستخدمين',
      many: '{n} مستخدمًا',
      other: '{n} مستخدم',
    },
  },
  'common.plural.device': {
    en: { one: '{n} device', other: '{n} devices' },
    ar: {
      zero: 'لا أجهزة',
      one: 'جهاز واحد',
      two: 'جهازان',
      few: '{n} أجهزة',
      many: '{n} جهازًا',
      other: '{n} جهاز',
    },
  },
  'common.plural.issue': {
    en: { one: '{n} issue', other: '{n} issues' },
    ar: {
      zero: 'لا استثناءات',
      one: 'استثناء واحد',
      two: 'استثناءان',
      few: '{n} استثناءات',
      many: '{n} استثناءً',
      other: '{n} استثناء',
    },
  },
  'common.plural.occurrence': {
    en: { one: '{n} occurrence', other: '{n} occurrences' },
    ar: {
      zero: 'لا تكرارات',
      one: 'تكرار واحد',
      two: 'تكراران',
      few: '{n} تكرارات',
      many: '{n} تكرارًا',
      other: '{n} تكرار',
    },
  },
  'common.plural.transaction': {
    en: { one: '{n} transaction', other: '{n} transactions' },
    ar: {
      zero: 'لا معاملات',
      one: 'معاملة واحدة',
      two: 'معاملتان',
      few: '{n} معاملات',
      many: '{n} معاملة',
      other: '{n} معاملة',
    },
  },
  'common.plural.result': {
    en: { one: '{n} result', other: '{n} results' },
    ar: {
      zero: 'لا نتائج',
      one: 'نتيجة واحدة',
      two: 'نتيجتان',
      few: '{n} نتائج',
      many: '{n} نتيجة',
      other: '{n} نتيجة',
    },
  },
} as const satisfies Record<string, PluralMessage>;
