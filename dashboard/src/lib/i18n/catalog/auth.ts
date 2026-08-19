import type { Message } from '../types';

/** Sign-in, registration, password reset, and first-run onboarding. */
export const auth = {
  // --- sign in -------------------------------------------------------------
  'auth.login.subtitle': {
    en: 'Welcome back. Watch every error and event.',
    ar: 'أهلاً بعودتك. راقب كل خطأ وكل حدث.',
  },
  'auth.login.submit': { en: 'Sign in', ar: 'تسجيل الدخول' },
  'auth.login.forgot': { en: 'Forgot your password?', ar: 'نسيت كلمة المرور؟' },
  'auth.login.newHere': { en: 'New to Sauron?', ar: 'جديد على Sauron؟' },
  'auth.login.createAccount': { en: 'Create an account', ar: 'إنشاء حساب' },
  'auth.login.adminReset': {
    en: 'An administrator reset the password for this account. We have emailed',
    ar: 'أعاد أحد المسؤولين تعيين كلمة المرور لهذا الحساب. أرسلنا رسالة إلى',
  },
  'auth.login.checkSpam': {
    en: 'Nothing arrived? Check your spam folder, or ask the administrator to send it again.',
    ar: 'لم تصلك رسالة؟ تحقق من مجلد البريد غير المرغوب، أو اطلب من المسؤول إعادة الإرسال.',
  },

  // --- register ------------------------------------------------------------
  'auth.register.title': { en: 'Create your account', ar: 'أنشئ حسابك' },
  'auth.register.subtitle': {
    en: 'Spin up a workspace in seconds.',
    ar: 'أنشئ مساحة عمل في ثوانٍ.',
  },
  'auth.register.submit': { en: 'Create account', ar: 'إنشاء الحساب' },
  'auth.register.haveAccount': { en: 'Already have an account?', ar: 'لديك حساب بالفعل؟' },
  'auth.register.workEmail': { en: 'Work email', ar: 'بريد العمل' },
  'auth.register.orgName': { en: 'Organization name', ar: 'اسم المؤسسة' },
  'auth.register.minChars': { en: 'At least 8 characters', ar: '8 أحرف على الأقل' },

  // --- forgot password -----------------------------------------------------
  'auth.forgot.title': { en: 'Reset your password', ar: 'إعادة تعيين كلمة المرور' },
  'auth.forgot.subtitle': {
    en: "We'll email you a link to choose a new one.",
    ar: 'سنرسل إليك رابطًا لاختيار كلمة مرور جديدة.',
  },
  'auth.forgot.submit': { en: 'Email me a link', ar: 'أرسل لي رابطًا' },
  'auth.forgot.backToSignIn': { en: 'Back to sign in', ar: 'العودة إلى تسجيل الدخول' },
  'auth.forgot.sentDetail': {
    en: 'If an account exists for that address, we have sent a link to reset the password. The link expires in 1 hour.',
    ar: 'إذا كان هناك حساب مرتبط بهذا العنوان، فقد أرسلنا رابطًا لإعادة تعيين كلمة المرور. تنتهي صلاحية الرابط خلال ساعة واحدة.',
  },
  'auth.forgot.unsupportedDetail': {
    en: 'This server does not support password reset yet — ask an administrator to finish the upgrade.',
    ar: 'لا يدعم هذا الخادم إعادة تعيين كلمة المرور بعد — اطلب من أحد المسؤولين إكمال الترقية.',
  },
  'auth.forgot.retry': {
    en: 'Nothing arrived? Check your spam folder, then try again in a little while.',
    ar: 'لم تصلك رسالة؟ تحقق من مجلد البريد غير المرغوب، ثم أعد المحاولة بعد قليل.',
  },

  // --- reset password ------------------------------------------------------
  'auth.reset.title': { en: 'Choose a new password', ar: 'اختر كلمة مرور جديدة' },
  'auth.reset.submit': { en: 'Set new password', ar: 'تعيين كلمة المرور' },
  'auth.reset.requestNew': { en: 'Email me a new link', ar: 'أرسل لي رابطًا جديدًا' },
  'auth.reset.invalidLink': {
    en: 'This reset link is invalid or has expired — request a new one.',
    ar: 'رابط إعادة التعيين غير صالح أو منتهي الصلاحية — اطلب رابطًا جديدًا.',
  },

  // --- change password -----------------------------------------------------
  'auth.change.title': { en: 'Choose a password', ar: 'اختر كلمة مرور' },
  'auth.change.temporary': {
    en: 'Your account was created with a temporary password. Choose your own before continuing.',
    ar: 'أُنشئ حسابك بكلمة مرور مؤقتة. اختر كلمة المرور الخاصة بك قبل المتابعة.',
  },
  'auth.change.submit': { en: 'Update password', ar: 'تحديث كلمة المرور' },
  'auth.password.current': { en: 'Current password', ar: 'كلمة المرور الحالية' },
  'auth.password.new': { en: 'New password', ar: 'كلمة المرور الجديدة' },
  'auth.password.confirm': { en: 'Confirm new password', ar: 'تأكيد كلمة المرور الجديدة' },

  // --- onboarding ----------------------------------------------------------
  'auth.onboarding.step1': { en: 'Step 1 of 3', ar: 'الخطوة 1 من 3' },
  'auth.onboarding.step2': { en: 'Step 2 of 3', ar: 'الخطوة 2 من 3' },
  'auth.onboarding.step3': { en: 'Step 3 of 3', ar: 'الخطوة 3 من 3' },
  'auth.onboarding.createProject': { en: 'Create your first project', ar: 'أنشئ مشروعك الأول' },
  'auth.onboarding.projectHelp': {
    en: "A project groups related apps — think a product or a team. You'll add an app next.",
    ar: 'يجمع المشروع التطبيقات المرتبطة — كمنتج أو فريق. ستضيف تطبيقًا بعد ذلك.',
  },
  'auth.onboarding.projectName': { en: 'Project name', ar: 'اسم المشروع' },
  'auth.onboarding.createProjectBtn': { en: 'Create project', ar: 'إنشاء المشروع' },
  'auth.onboarding.addApp': { en: 'Add an app to', ar: 'أضف تطبيقًا إلى' },
  'auth.onboarding.appHelp': {
    en: 'An app holds the DSN your SDK reports to. Pick the platform it runs on.',
    ar: 'يحمل التطبيق عنوان DSN الذي ترسل إليه حزمة التطوير. اختر المنصة التي يعمل عليها.',
  },
  'auth.onboarding.appName': { en: 'App name', ar: 'اسم التطبيق' },
  'auth.onboarding.appType': { en: 'App type', ar: 'نوع التطبيق' },
  'auth.onboarding.createApp': { en: 'Create app', ar: 'إنشاء التطبيق' },
  'auth.onboarding.connect': { en: 'Connect', ar: 'الربط' },
  'auth.onboarding.connectHelp': {
    en: "Initialize the SDK with your DSN. We'll light up as soon as the first event arrives.",
    ar: 'هيّئ حزمة التطوير باستخدام DSN الخاص بك. سنبدأ العمل فور وصول أول حدث.',
  },
  'auth.onboarding.yourDsn': { en: 'Your DSN', ar: 'عنوان DSN الخاص بك' },
  'auth.onboarding.installSnippet': { en: 'Install snippet', ar: 'مقتطف التثبيت' },
  'auth.onboarding.waiting': {
    en: 'Waiting for your first event…',
    ar: 'في انتظار حدثك الأول…',
  },
  'auth.onboarding.polling': {
    en: 'Send an error or event from your app. Polling every 3s.',
    ar: 'أرسل خطأً أو حدثًا من تطبيقك. يجري الفحص كل 3 ثوانٍ.',
  },
  'auth.onboarding.received': { en: 'First event received!', ar: 'وصل أول حدث!' },
  'auth.onboarding.goToIssues': { en: 'Go to Issues', ar: 'الانتقال إلى الاستثناءات' },
  'auth.onboarding.skip': { en: 'Skip for now', ar: 'تخطٍّ الآن' },
  'auth.onboarding.settingUp': {
    en: 'Setting up your environment…',
    ar: 'جارٍ إعداد بيئتك…',
  },

  // --- unsubscribe ---------------------------------------------------------
  'auth.unsubscribe.title': { en: 'Unsubscribe', ar: 'إلغاء الاشتراك' },
  'auth.unsubscribe.done': {
    en: 'That subscription is now off. You will not receive those notifications again.',
    ar: 'أُوقف هذا الاشتراك. لن تصلك تلك الإشعارات مرة أخرى.',
  },
  'auth.unsubscribe.reenable': {
    en: 'You can turn it back on at any time from your account page.',
    ar: 'يمكنك تفعيله مجددًا في أي وقت من صفحة حسابك.',
  },
  'auth.unsubscribe.missingToken': {
    en: 'This link is missing its token. Open it directly from the notification email.',
    ar: 'هذا الرابط ينقصه الرمز. افتحه مباشرةً من رسالة الإشعار.',
  },
  'auth.unsubscribe.manage': { en: 'Manage subscriptions', ar: 'إدارة الاشتراكات' },

  // --- shared --------------------------------------------------------------
  'auth.signOut': { en: 'Sign out', ar: 'تسجيل الخروج' },
  'auth.placeholder.email': { en: 'you@company.com', ar: 'you@company.com' },
  'auth.placeholder.personName': { en: 'Ada Lovelace', ar: 'أحمد بن بلة' },
  'auth.placeholder.orgName': { en: 'Acme Inc.', ar: 'شركة المثال' },
  'auth.placeholder.projectName': { en: 'Payments', ar: 'المدفوعات' },
  'auth.placeholder.appName': { en: 'Web App', ar: 'تطبيق الويب' },
} as const satisfies Record<string, Message>;
