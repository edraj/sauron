import type { Message } from '../types';

/**
 * Sidebar groups, sidebar items, and top-bar controls.
 *
 * The item keys mirror the route they point at, so the mapping back to
 * `Sidebar.svelte` stays obvious. Group keys use the group's English label
 * lowercased, matching the `navCollapseStore` keys already persisted under
 * those names.
 */
export const nav = {
  // --- groups --------------------------------------------------------------
  'nav.group.monitor': { en: 'Monitor', ar: 'المراقبة' },
  'nav.group.uptime': { en: 'Uptime', ar: 'التوافر' },
  'nav.group.explore': { en: 'Explore', ar: 'الاستكشاف' },
  'nav.group.analyze': { en: 'Analyze', ar: 'التحليل' },
  'nav.group.admin': { en: 'Admin', ar: 'الإدارة' },

  // --- items ---------------------------------------------------------------
  'nav.overview': { en: 'Overview', ar: 'نظرة عامة' },
  'nav.issues': { en: 'Exceptions', ar: 'الاستثناءات' },
  'nav.performance': { en: 'Performance', ar: 'الأداء' },
  'nav.monitors': { en: 'Monitors', ar: 'المراقبات' },
  'nav.events': { en: 'Events', ar: 'الأحداث' },
  'nav.transactions': { en: 'Transactions', ar: 'المعاملات' },
  'nav.sessions': { en: 'Sessions', ar: 'الجلسات' },
  'nav.users': { en: 'Users', ar: 'المستخدمون' },
  'nav.devices': { en: 'Devices', ar: 'الأجهزة' },
  'nav.screens': { en: 'Screens', ar: 'الشاشات' },
  'nav.workflows': { en: 'Workflows', ar: 'سير العمل' },
  'nav.activeUsers': { en: 'Active users', ar: 'المستخدمون النشطون' },
  'nav.funnels': { en: 'Funnels', ar: 'مسارات التحويل' },
  'nav.journeys': { en: 'Journeys', ar: 'رحلات المستخدمين' },
  'nav.admin': { en: 'Admin', ar: 'الإدارة' },

  // --- chrome --------------------------------------------------------------
  'nav.tagline': {
    en: 'Observability & product analytics',
    ar: 'المراقبة وتحليلات المنتج',
  },
  'nav.toggleGroup': { en: 'Toggle {group}', ar: 'تبديل {group}' },
  'nav.collapseSidebar': { en: 'Collapse sidebar', ar: 'طي الشريط الجانبي' },
  'nav.expandSidebar': { en: 'Expand sidebar', ar: 'توسيع الشريط الجانبي' },
  'nav.mainNavigation': { en: 'Main navigation', ar: 'التنقل الرئيسي' },

  // --- top bar -------------------------------------------------------------
  'nav.account': { en: 'Account', ar: 'الحساب' },
  'nav.theme.toggle': { en: 'Toggle theme', ar: 'تبديل المظهر' },
  'nav.theme.dark': { en: 'Dark', ar: 'داكن' },
  'nav.theme.light': { en: 'Light', ar: 'فاتح' },
  'nav.selectApp': { en: 'Select app', ar: 'اختيار التطبيق' },
  'nav.selectEnvironment': { en: 'Select environment', ar: 'اختيار البيئة' },
  'nav.noAccess': { en: 'No access', ar: 'لا توجد صلاحية' },
  // --- shell states --------------------------------------------------------
  'shell.workspace.errorTitle': { en: "Couldn't load workspace", ar: 'تعذّر تحميل مساحة العمل' },
  'shell.permissions.errorTitle': { en: "Couldn't load permissions", ar: 'تعذّر تحميل الصلاحيات' },
  'shell.permissions.errorBody': {
    en: "We couldn't check what you have access to, so the dashboard is showing nothing rather than guessing. This is usually temporary.",
    ar: 'تعذّر التحقق من صلاحياتك، لذا لا تعرض لوحة التحكم شيئًا بدلاً من التخمين. عادةً ما تكون هذه المشكلة مؤقتة.',
  },
  'shell.noApps.title': { en: 'No apps available', ar: 'لا توجد تطبيقات متاحة' },
  'shell.noApps.body': {
    en: "You don't have access to any app in this organization yet. Ask an administrator to grant you access.",
    ar: 'ليس لديك صلاحية الوصول إلى أي تطبيق في هذه المؤسسة بعد. اطلب من أحد المسؤولين منحك الصلاحية.',
  },
  'shell.emptyOrg.title': {
    en: 'No projects in this organization',
    ar: 'لا توجد مشاريع في هذه المؤسسة',
  },
  'shell.emptyOrg.body': {
    en: 'This organization has no projects you can see. Switch organizations from the picker above, or create a project here.',
    ar: 'لا توجد مشاريع يمكنك رؤيتها في هذه المؤسسة. بدّل المؤسسة من القائمة أعلاه، أو أنشئ مشروعًا هنا.',
  },
  'shell.emptyOrg.create': { en: 'Create a project', ar: 'إنشاء مشروع' },
  'nav.orgLocked': { en: 'No access', ar: 'لا صلاحية' },
  'shell.adminSections': { en: 'Admin sections', ar: 'أقسام الإدارة' },

  // --- top bar pickers -----------------------------------------------------
  'nav.org': { en: 'Org', ar: 'المؤسسة' },
  'nav.project': { en: 'Project', ar: 'المشروع' },
  'nav.env': { en: 'Env', ar: 'البيئة' },
  'nav.allEnvironments': { en: 'All environments', ar: 'كل البيئات' },
  'nav.unattributed': { en: 'Unattributed', ar: 'غير منسوب' },
  'nav.docs': { en: 'Docs', ar: 'الوثائق' },
  'nav.docsTitle': { en: 'Docs & integration guides', ar: 'الوثائق وأدلة التكامل' },
  'nav.switchToLight': { en: 'Switch to light', ar: 'التبديل إلى الفاتح' },
  'nav.switchToDark': { en: 'Switch to dark', ar: 'التبديل إلى الداكن' },
  'nav.logOut': { en: 'Log out', ar: 'تسجيل الخروج' },
  'nav.switchOrg': { en: 'Switch organization', ar: 'تبديل المؤسسة' },
  'nav.switchProject': { en: 'Switch project', ar: 'تبديل المشروع' },
  'nav.switchApp': { en: 'Switch app', ar: 'تبديل التطبيق' },
  'nav.switchEnvironment': { en: 'Switch environment', ar: 'تبديل البيئة' },
} as const satisfies Record<string, Message>;
