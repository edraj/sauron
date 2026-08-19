import type { Message } from '../types';

/** Members, roles, projects, environments, and per-app settings. */
export const admin = {
  // --- admin index ---------------------------------------------------------
  'admin.empty.title': { en: 'No admin sections available', ar: 'لا توجد أقسام إدارة متاحة' },
  'admin.empty.body': {
    en: "You don't have access to any admin area in this organization. Ask an organization owner for access.",
    ar: 'ليس لديك صلاحية الوصول إلى أي قسم إداري في هذه المؤسسة. اطلب الصلاحية من أحد مالكي المؤسسة.',
  },

  // --- members -------------------------------------------------------------
  'members.title': { en: 'Members', ar: 'الأعضاء' },
  'members.column.member': { en: 'Member', ar: 'العضو' },
  'members.column.role': { en: 'Role', ar: 'الدور' },
  'members.column.scope': { en: 'Scope', ar: 'النطاق' },
  'members.create': { en: 'Create member', ar: 'إنشاء عضو' },
  'members.grant': { en: 'Grant', ar: 'منح' },
  'members.grantAccess': { en: 'Grant access', ar: 'منح الصلاحية' },
  'members.removeAccess': { en: 'Remove access', ar: 'إزالة الصلاحية' },
  'members.existingAccount': {
    en: 'For someone who already has an account, here or in another org.',
    ar: 'لمن لديه حساب بالفعل، هنا أو في مؤسسة أخرى.',
  },
  'members.deactivated': { en: 'Deactivated', ar: 'معطَّل' },
  'members.resetPending': { en: 'Reset pending', ar: 'إعادة التعيين معلّقة' },
  'members.deactivate': { en: 'Deactivate', ar: 'تعطيل' },
  'members.confirmDeactivate': { en: 'Deactivate member?', ar: 'تعطيل العضو؟' },
  'members.signOutAll': { en: 'Sign out all sessions', ar: 'إنهاء كل الجلسات' },
  'members.placeholder.email': { en: 'teammate@company.com', ar: 'teammate@company.com' },
  'members.placeholder.name': { en: 'Jane Doe', ar: 'فاطمة أحمد' },
  'members.give': { en: 'Give', ar: 'امنح' },

  // --- edit member ---------------------------------------------------------
  'members.edit.addRole': { en: 'Add a role', ar: 'إضافة دور' },
  'members.edit.removeRole': { en: 'Remove this role', ar: 'إزالة هذا الدور' },
  'members.edit.backToEditing': { en: 'Back to editing', ar: 'العودة إلى التحرير' },
  'members.edit.noMember': { en: 'No member selected.', ar: 'لم يُحدَّد أي عضو.' },
  'members.edit.tickReach': {
    en: 'Tick what they can reach. Each role carries its own scope selection.',
    ar: 'حدّد ما يمكنهم الوصول إليه. لكل دور اختيار نطاق خاص به.',
  },
  'members.edit.pickScope': {
    en: 'Pick at least one scope, or remove this role.',
    ar: 'اختر نطاقًا واحدًا على الأقل، أو أزل هذا الدور.',
  },
  'members.edit.lastOrgManage': {
    en: 'Cannot remove the last grant with org:manage — assign it to another member first.',
    ar: 'لا يمكن إزالة آخر صلاحية org:manage — امنحها لعضو آخر أولاً.',
  },
  'members.edit.hiddenScopes': { en: 'Scopes not visible to you', ar: 'نطاقات غير مرئية لك' },
  'members.edit.hiddenKept': {
    en: 'Kept as they are unless you remove them.',
    ar: 'تبقى كما هي ما لم تُزلها.',
  },
  'members.edit.partialFailure': {
    en: 'Some changes were not applied.',
    ar: 'لم تُطبَّق بعض التغييرات.',
  },

  // --- reset password (admin) ---------------------------------------------
  'members.reset.neverMind': { en: 'Never mind', ar: 'تراجع' },

  // --- roles ---------------------------------------------------------------
  'roles.title': { en: 'Roles', ar: 'الأدوار' },
  'roles.new': { en: 'New role', ar: 'دور جديد' },
  'roles.column.permissions': { en: 'Permissions', ar: 'الصلاحيات' },
  'roles.column.description': { en: 'Description', ar: 'الوصف' },
  'roles.empty.title': { en: 'No roles yet', ar: 'لا توجد أدوار بعد' },
  'roles.empty.body': {
    en: 'Create a custom role to define a permission set members can be granted.',
    ar: 'أنشئ دورًا مخصصًا لتحديد مجموعة صلاحيات يمكن منحها للأعضاء.',
  },
  'roles.builtInNote': {
    en: 'Built-in roles cannot be edited. Create a custom role to define your own permission set.',
    ar: 'لا يمكن تعديل الأدوار المدمجة. أنشئ دورًا مخصصًا لتحديد مجموعة صلاحياتك.',
  },
  'roles.delete': { en: 'Delete role', ar: 'حذف الدور' },
  'roles.deleteSafe': {
    en: 'No members hold this role. This cannot be undone.',
    ar: 'لا يحمل أي عضو هذا الدور. لا يمكن التراجع عن هذا الإجراء.',
  },
  'roles.placeholder.name': { en: 'Support', ar: 'الدعم' },
  'roles.placeholder.description': {
    en: 'Read + resolve issues',
    ar: 'قراءة الاستثناءات وحلّها',
  },
  'roles.searchPermissions': { en: 'Search permissions…', ar: 'البحث في الصلاحيات…' },

  // --- scope tree ----------------------------------------------------------
  'scope.label': { en: 'Access scope', ar: 'نطاق الصلاحية' },
  'scope.loadingEnvironments': { en: 'Loading environments…', ar: 'جارٍ تحميل البيئات…' },
  'scope.noEnvironments': { en: 'No environments.', ar: 'لا توجد بيئات.' },
  'scope.noProjects': {
    en: 'No projects yet — the whole org is the only scope.',
    ar: 'لا توجد مشاريع بعد — المؤسسة بأكملها هي النطاق الوحيد.',
  },

  // --- projects ------------------------------------------------------------
  'projects.title': { en: 'Projects', ar: 'المشاريع' },
  'projects.subtitle': {
    en: 'Group your apps by product or team. Each app holds its own DSN.',
    ar: 'اجمع تطبيقاتك حسب المنتج أو الفريق. لكل تطبيق عنوان DSN خاص به.',
  },
  'projects.new': { en: 'New project', ar: 'مشروع جديد' },
  'projects.create': { en: 'Create project', ar: 'إنشاء المشروع' },
  'projects.createApp': { en: 'Create app', ar: 'إنشاء تطبيق' },
  'projects.name': { en: 'Project name', ar: 'اسم المشروع' },
  'projects.appType': { en: 'App type', ar: 'نوع التطبيق' },
  'projects.appName': { en: 'App name', ar: 'اسم التطبيق' },
  'projects.toggleApps': { en: 'Toggle apps', ar: 'إظهار/إخفاء التطبيقات' },
  'projects.noApps': { en: 'No apps in this project yet.', ar: 'لا توجد تطبيقات في هذا المشروع بعد.' },
  'projects.open': { en: 'Open', ar: 'فتح' },
  'projects.settings': { en: 'Settings', ar: 'الإعدادات' },
  'projects.rename': { en: 'Rename', ar: 'إعادة تسمية' },
  'projects.confirmDelete': { en: 'Yes, delete', ar: 'نعم، احذف' },
  'projects.empty.title': { en: 'No projects yet', ar: 'لا توجد مشاريع بعد' },
  'projects.empty.body': {
    en: 'Create a project to start grouping apps.',
    ar: 'أنشئ مشروعًا لتبدأ بتجميع التطبيقات.',
  },

  // --- environments --------------------------------------------------------
  'environments.title': { en: 'Environments', ar: 'البيئات' },
  'environments.new': { en: 'New environment', ar: 'بيئة جديدة' },
  'environments.newTitle': { en: 'New project environment', ar: 'بيئة مشروع جديدة' },
  'environments.renameTitle': { en: 'Rename environment', ar: 'إعادة تسمية البيئة' },
  'environments.makeDefault': { en: 'Make default', ar: 'تعيين كافتراضية' },
  'environments.default': { en: 'Default', ar: 'افتراضية' },
  'environments.muted': { en: 'Muted', ar: 'مكتومة' },
  'environments.retire': { en: 'Retire', ar: 'إيقاف' },
  'environments.retired': { en: 'Retired', ar: 'موقوفة' },
  'environments.rotateKey': { en: 'Rotate key', ar: 'تدوير المفتاح' },
  'environments.rotate': { en: 'Rotate', ar: 'تدوير' },
  'environments.viewApps': { en: 'View apps', ar: 'عرض التطبيقات' },
  'environments.viewEnvironments': { en: 'View environments', ar: 'عرض البيئات' },
  'environments.placeholder': { en: 'staging', ar: 'staging' },
  'environments.mutedNote': {
    en: 'Ingest is off and its key no longer works. Existing data stays queryable.',
    ar: 'الاستقبال متوقف ولم يعد مفتاحها يعمل. تبقى البيانات الحالية قابلة للاستعلام.',
  },
  'environments.noAppsEnrolled': {
    en: 'No apps enrolled in this environment yet.',
    ar: 'لا توجد تطبيقات مسجَّلة في هذه البيئة بعد.',
  },
  'environments.createHint': {
    en: 'Added to every app in this project, each with its own ingest key. Lowercase and short works best — this appears in every filter.',
    ar: 'تُضاف إلى كل تطبيق في هذا المشروع، ولكلٍّ مفتاح استقبال خاص. يُفضَّل اسم قصير بحروف صغيرة — فهو يظهر في كل مرشّح.',
  },
  'environments.renameHint': {
    en: 'This name belongs to the project — renaming it renames it for every app in the project.',
    ar: 'هذا الاسم مملوك للمشروع — إعادة تسميته تغيّره لكل تطبيق في المشروع.',
  },
  'environments.renameTooltip': {
    en: 'Renames this environment for every app in the project',
    ar: 'يعيد تسمية هذه البيئة لكل تطبيق في المشروع',
  },
  'environments.retireTooltip': {
    en: 'Retires this environment for every app in the project',
    ar: 'يوقف هذه البيئة لكل تطبيق في المشروع',
  },
  'environments.confirmRetire': {
    en: 'Retire environment for the whole project?',
    ar: 'إيقاف البيئة للمشروع بأكمله؟',
  },
  'environments.confirmRotate': { en: 'Rotate ingest key?', ar: 'تدوير مفتاح الاستقبال؟' },
  'environments.empty.title': { en: 'No environments yet', ar: 'لا توجد بيئات بعد' },
  'environments.empty.body': {
    en: 'Create one to start separating dev, staging and production traffic. Every app in this project is enrolled automatically, each with its own ingest key.',
    ar: 'أنشئ بيئة لتبدأ بفصل حركة التطوير والاختبار والإنتاج. يُسجَّل كل تطبيق في هذا المشروع تلقائيًا، ولكلٍّ مفتاح استقبال خاص.',
  },
  'environments.notVisible.title': { en: 'No environments visible', ar: 'لا توجد بيئات مرئية' },
  'environments.notVisible.body': {
    en: "The project-wide catalogue needs project-level View environments (env:read), which your role doesn't grant here, and none of your apps are enrolled anywhere you can see. Ask an organization owner for access.",
    ar: 'يتطلب كتالوج المشروع صلاحية «عرض البيئات» (env:read) على مستوى المشروع، وهي غير ممنوحة لدورك هنا، ولا توجد تطبيقات لك مسجَّلة في أي مكان يمكنك رؤيته. اطلب الصلاحية من أحد مالكي المؤسسة.',
  },

  // --- app settings --------------------------------------------------------
  'settings.title': { en: 'App settings', ar: 'إعدادات التطبيق' },
  'settings.card.ingest': { en: 'Ingest', ar: 'الاستقبال' },
  'settings.deleteApp': { en: 'Delete app', ar: 'حذف التطبيق' },
  'settings.deleteWarning': {
    en: "Permanently delete this app and all of its issues and events. This can't be undone.",
    ar: 'حذف نهائي لهذا التطبيق ولكل استثناءاته وأحداثه. لا يمكن التراجع عن هذا الإجراء.',
  },
  'settings.goToProjects': { en: 'Go to Projects', ar: 'الانتقال إلى المشاريع' },
  'settings.noApp.title': { en: 'No app selected', ar: 'لم يُحدَّد أي تطبيق' },
  'settings.noApp.body': {
    en: 'Pick an app from the switcher, or create one from Projects.',
    ar: 'اختر تطبيقًا من المبدّل، أو أنشئ واحدًا من صفحة المشاريع.',
  },

  // --- app store connections ----------------------------------------------
  'stores.title': { en: 'App stores', ar: 'متاجر التطبيقات' },
  'stores.environment': { en: 'Store environment', ar: 'بيئة المتجر' },
  'stores.none': { en: 'None — hide the store section', ar: 'بلا — إخفاء قسم المتجر' },
  'stores.queueSync': { en: 'Queue sync', ar: 'جدولة المزامنة' },
  'stores.removeCredentials': { en: 'Yes, remove credentials', ar: 'نعم، أزل بيانات الاعتماد' },
} as const satisfies Record<string, Message>;
