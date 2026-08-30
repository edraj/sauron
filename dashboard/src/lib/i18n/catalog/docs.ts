import type { Message } from '../types';

/**
 * The in-dashboard integration guide (`/docs`).
 *
 * Long-form technical prose. Two conventions hold throughout:
 *
 * - Identifiers the reader must type or match verbatim — header names, CLI
 *   flags, SQL fragments, table and column names — live inside `<code>` in the
 *   markup and are deliberately absent from this file. Translating them would
 *   make an example describe something that does not exist.
 * - Product and technology names (Sauron, Postgres, Parquet, Redis, Flutter)
 *   stay in Latin script, which is how they are written in Arabic technical
 *   writing too.
 */
export const docs = {
  // --- page chrome ---------------------------------------------------------
  'docs.title': { en: 'Docs', ar: 'الوثائق' },
  'docs.sections': { en: 'Docs sections', ar: 'أقسام الوثائق' },
  'docs.subtitle': {
    en: 'Integrate Sauron into your web, mobile, and server apps — install, initialize, capture errors, and track product events.',
    ar: 'ادمج Sauron في تطبيقات الويب والهاتف والخوادم لديك — التثبيت والتهيئة والتقاط الأخطاء وتتبّع أحداث المنتج.',
  },

  // --- top-level sections --------------------------------------------------
  'docs.nav.getStarted': { en: 'Get started', ar: 'البدء' },
  'docs.nav.guides': { en: 'Guides', ar: 'الأدلة' },
  'docs.nav.sdks': { en: 'SDKs', ar: 'حزم التطوير' },
  'docs.nav.architecture': { en: 'Architecture', ar: 'البنية' },
  'docs.nav.troubleshooting': { en: 'Troubleshooting', ar: 'استكشاف الأخطاء' },
  'docs.nav.howItWorks': { en: 'How Sauron works', ar: 'كيف يعمل Sauron' },
  'docs.nav.underTheHood': { en: 'Under the hood', ar: 'خلف الكواليس' },
  'docs.nav.sdkInternals': { en: 'SDK internals', ar: 'داخل حزم التطوير' },
  'docs.nav.accessControl': { en: 'Access control', ar: 'التحكم في الوصول' },
  'docs.nav.dataLifecycle': { en: 'Data lifecycle', ar: 'دورة حياة البيانات' },
  'docs.nav.searchFiltering': { en: 'Search & filtering', ar: 'البحث والتصفية' },
  'docs.nav.uptime': { en: 'Uptime monitoring', ar: 'مراقبة التوافر' },
  'docs.nav.errorGrouping': { en: 'Error grouping', ar: 'تجميع الأخطاء' },
  'docs.nav.projectsApps': { en: 'Projects & apps', ar: 'المشاريع والتطبيقات' },
  'docs.nav.analyticsPeople': { en: 'Analytics & people', ar: 'التحليلات والأشخاص' },
  'docs.nav.queriesBehind': { en: 'Queries behind the screens', ar: 'الاستعلامات خلف الشاشات' },

  // --- quickstarts ---------------------------------------------------------
  'docs.quickstarts': { en: 'SDK quickstarts', ar: 'بدايات سريعة لحزم التطوير' },
  'docs.quickstart.web': { en: 'Web quickstart', ar: 'بداية سريعة للويب' },
  'docs.quickstart.flutter': { en: 'Flutter quickstart', ar: 'بداية سريعة لـ Flutter' },
  'docs.quickstart.node': { en: 'Node.js quickstart', ar: 'بداية سريعة لـ Node.js' },
  'docs.quickstart.python': { en: 'Python quickstart', ar: 'بداية سريعة لـ Python' },
  'docs.quickstart.csharp': { en: 'C# quickstart', ar: 'بداية سريعة لـ C#' },
  'docs.addPlatform': { en: 'Add another platform', ar: 'إضافة منصة أخرى' },
  'docs.apiReference': { en: '{language} API reference', ar: 'مرجع واجهة برمجة {language}' },

  // --- get-started steps ---------------------------------------------------
  'docs.step.createApp': { en: 'Create or select an app', ar: 'أنشئ تطبيقًا أو اختره' },
  'docs.step.copyDsn': { en: 'Copy or rotate your DSN', ar: 'انسخ عنوان DSN أو دوّره' },
  'docs.step.verify': { en: 'Verify it works', ar: 'تحقّق من عمله' },
  'docs.step.seeSignals': { en: 'See signals roll in', ar: 'شاهد الإشارات تتدفق' },
  'docs.dsnForApp': { en: 'Showing the DSN for the', ar: 'يُعرض عنوان DSN الخاص بـ' },
  'docs.snippetsUseDsn': {
    en: 'Snippets below use the DSN shown here.',
    ar: 'تستخدم المقتطفات أدناه عنوان DSN المعروض هنا.',
  },
  'docs.snippetsPlaceholder': {
    en: 'Snippets use a placeholder DSN.',
    ar: 'تستخدم المقتطفات عنوان DSN نائبًا.',
  },
  'docs.fireTestEvent': {
    en: 'Fire a test event from your app, then watch it land here. The first event can take a few seconds.',
    ar: 'أطلق حدثًا تجريبيًا من تطبيقك ثم راقب وصوله هنا. وقد يستغرق الحدث الأول بضع ثوانٍ.',
  },
  'docs.errorsToExceptions': { en: 'Errors → Exceptions', ar: 'الأخطاء ← الاستثناءات' },
  'docs.eventsToAnalytics': { en: 'Events → Analytics', ar: 'الأحداث ← التحليلات' },
  'docs.stackTracedGrouped': {
    en: 'Stack-traced and grouped into issues.',
    ar: 'تُتتبَّع آثار مكدسها وتُجمَّع في استثناءات.',
  },
  'docs.fullSurface': {
    en: 'The full public surface per language is in the',
    ar: 'الواجهة العامة الكاملة لكل لغة موجودة في',
  },

  // --- architecture --------------------------------------------------------
  'docs.arch.ingestEdge': { en: 'Ingest edge', ar: 'حافة الاستقبال' },
  'docs.arch.redisStream': { en: 'Redis stream', ar: 'دفق Redis' },
  'docs.arch.workers': { en: 'Workers', ar: 'العمّال' },
  'docs.arch.sdkBatch': { en: 'SDK batch', ar: 'دفعة حزمة التطوير' },
  'docs.arch.everythingSdkSends': {
    en: 'Everything an SDK sends — errors, events, identifies, transactions, breadcrumbs —',
    ar: 'كل ما ترسله حزمة التطوير — أخطاء وأحداث وتعريفات ومعاملات وخطوات تتبّع —',
  },
  'docs.arch.sdkBatchNote': {
    en: 'What every SDK does between your call and the wire. Calls accumulate into one',
    ar: 'ما تفعله كل حزمة تطوير بين استدعائك والإرسال. تتراكم الاستدعاءات في',
  },
  'docs.arch.breadcrumbs': {
    en: "Breadcrumbs don't become rows — they ride ahead of a crash in a capped, expiring Redis list per person, and get attached to the next error for that user.",
    ar: 'لا تصبح خطوات التتبّع صفوفًا — بل تسبق العطل في قائمة Redis محدودة الحجم ومنتهية الصلاحية لكل شخص، وتُرفق بالخطأ التالي لذلك المستخدم.',
  },
  'docs.arch.symbolication': {
    en: 'Minified and ahead-of-time traces are made readable server-side: JavaScript via',
    ar: 'تُجعل الآثار المصغَّرة والمُترجمة مسبقًا قابلة للقراءة على الخادم: JavaScript عبر',
  },

  // --- data lifecycle ------------------------------------------------------
  'docs.lifecycle.hourlyExport': {
    en: 'An hourly job exports whole partitions older than the hot window to Parquet via DuckDB',
    ar: 'تُصدِّر مهمة كل ساعة الأقسام الكاملة الأقدم من النافذة الساخنة إلى Parquet عبر DuckDB',
  },
  'docs.lifecycle.signalsStay': { en: 'Signals stay', ar: 'تبقى الإشارات' },
  'docs.lifecycle.maskingHotOnly': {
    en: 'Masking rewrites rows in hot Postgres only',
    ar: 'يعيد الإخفاء كتابة الصفوف في Postgres الساخن فقط',
  },
  'docs.lifecycle.whatMaskDoes': {
    en: 'What a mask does to a value',
    ar: 'ما الذي يفعله الإخفاء بالقيمة',
  },
  'docs.lifecycle.notRecoverable': {
    en: 'Two things not to get wrong, because neither is recoverable.',
    ar: 'أمران لا يُسمح بالخطأ فيهما، إذ لا يمكن استرجاع أي منهما.',
  },

  // --- access control ------------------------------------------------------
  'docs.rbac.fineGrained': { en: 'Fine-grained RBAC:', ar: 'تحكم دقيق في الوصول بالأدوار:' },
  'docs.rbac.everyUserHas': { en: 'Every signed-in user has an', ar: 'لكل مستخدم مسجّل الدخول' },
  'docs.rbac.memberRowHas': { en: "A member's row has", ar: 'يحمل صف العضو' },
  'docs.rbac.customRoles': { en: 'Custom roles can now be', ar: 'يمكن الآن للأدوار المخصصة أن' },
  'docs.rbac.adminHolding': { en: 'An admin holding', ar: 'المسؤول الذي يملك' },
  'docs.rbac.tempPassword': {
    en: 'That temp password grants nothing except replacing itself: every authenticated',
    ar: 'كلمة المرور المؤقتة تلك لا تمنح شيئًا سوى استبدال نفسها: فكل طلب موثَّق',
  },
  'docs.rbac.account': { en: 'Account', ar: 'الحساب' },
  'docs.rbac.thisDevice': { en: 'This device', ar: 'هذا الجهاز' },
  'docs.rbac.signOutOthers': { en: 'Sign out other devices', ar: 'تسجيل الخروج من الأجهزة الأخرى' },
  'docs.rbac.logOut': { en: 'Log out', ar: 'تسجيل الخروج' },
  'docs.rbac.managePrivacy': { en: 'Manage → Privacy', ar: 'الإدارة ← الخصوصية' },

  // --- search & filtering --------------------------------------------------
  'docs.search.whereEachPage': {
    en: 'Where each page searches',
    ar: 'أين يبحث كل صفحة',
  },
  'docs.search.filterChips': { en: 'Filter chips', ar: 'رقائق التصفية' },
  'docs.search.filtersInUrl': { en: 'Filters live in the URL', ar: 'تعيش المرشّحات في الرابط' },
  'docs.search.freeText': {
    en: 'Free text: what a bare term actually matches',
    ar: 'النص الحر: ما الذي يطابقه المصطلح المجرّد فعلاً',
  },
  'docs.search.bareTerm': { en: 'A term with no', ar: 'المصطلح الذي بلا' },
  'docs.search.operators': { en: 'Query language — operators', ar: 'لغة الاستعلام — المعاملات' },
  'docs.search.variables': { en: 'Query language — variables', ar: 'لغة الاستعلام — المتغيرات' },
  'docs.search.structuredFilters': {
    en: 'Structured filters — Exceptions & Events',
    ar: 'المرشّحات المهيكلة — الاستثناءات والأحداث',
  },
  'docs.search.fieldsPerList': { en: 'Fields, per list', ar: 'الحقول، لكل قائمة' },
  'docs.search.fourBiggestLists': { en: 'The four biggest lists —', ar: 'أكبر أربع قوائم —' },
  'docs.search.readLive': {
    en: 'Read live from this app, so it always matches what the server will accept. These names work in the query box; the chips expose a subset of them.',
    ar: 'تُقرأ مباشرةً من هذا التطبيق، فتطابق دائمًا ما سيقبله الخادم. وتعمل هذه الأسماء في صندوق الاستعلام؛ أما الرقائق فتعرض مجموعة جزئية منها.',
  },
  'docs.search.addressJson': {
    en: 'These address JSON rather than a table column. Which ones a page offers depends on the',
    ar: 'تخاطب هذه بنية JSON لا عمودًا في جدول. وأيّها تتيحه الصفحة يعتمد على',
  },
  'docs.search.tagChip': {
    en: 'The Tag chip — read this before you file a bug',
    ar: 'رقاقة الوسم — اقرأ هذا قبل الإبلاغ عن خلل',
  },
  'docs.search.tagSuggestions': {
    en: 'Tag-key suggestions come from a sample of recent events, so a key you have not sent lately may not be offered by autocomplete. You can still type it — any key is queryable.',
    ar: 'تأتي اقتراحات مفاتيح الوسوم من عيّنة من الأحداث الأخيرة، لذا قد لا يقترح الإكمال التلقائي مفتاحًا لم ترسله مؤخرًا. ويمكنك كتابته على أي حال — فكل مفتاح قابل للاستعلام.',
  },
  'docs.search.example': { en: 'Example: find your error', ar: 'مثال: ابحث عن خطئك' },
  'docs.search.unresolvedCheckout': {
    en: 'Unresolved issues on the checkout path, plus a free-text term. Written as chips it',
    ar: 'الاستثناءات غير المحلولة في مسار الدفع، مع مصطلح نصي حر. وإذا كُتبت كرقائق فإنها',
  },
  'docs.search.thisIsAbout': { en: 'This is about the', ar: 'يتعلق هذا بـ' },
  'docs.search.realTickets': {
    en: 'This is the one that generates real support tickets: switch to',
    ar: 'هذا هو ما يولّد تذاكر دعم حقيقية: بدّل إلى',
  },

  // --- error grouping ------------------------------------------------------
  'docs.grouping.rawCollapse': { en: 'Raw exceptions collapse into', ar: 'تنطوي الاستثناءات الخام في' },
  'docs.grouping.issues': { en: 'Issues', ar: 'الاستثناءات' },

  // --- analytics & people --------------------------------------------------
  'docs.analytics.buildFunnel': { en: 'Build a funnel', ar: 'ابنِ مسار تحويل' },
  'docs.analytics.funnelIs': { en: 'A funnel is an ordered list of', ar: 'مسار التحويل قائمة مرتبة من' },
  'docs.analytics.pickRange': {
    en: 'Pick a date range — it defaults to the last 30 days.',
    ar: 'اختر نطاقًا زمنيًا — والافتراضي هو آخر 30 يومًا.',
  },
  'docs.analytics.compute': { en: 'Compute', ar: 'احتساب' },
  'docs.analytics.inPractice': { en: 'In practice', ar: 'عمليًا' },
  'docs.analytics.harderNumbers': { en: 'The harder numbers are computed', ar: 'تُحتسب الأرقام الأصعب' },
  'docs.analytics.funnelsDistinct': {
    en: 'Funnels — distinct people, in order',
    ar: 'مسارات التحويل — أشخاص مميّزون، بالترتيب',
  },
  'docs.analytics.oneCte': {
    en: 'One CTE per step; each step is matched',
    ar: 'تعبير جدولي واحد لكل خطوة؛ وتُطابَق كل خطوة',
  },
  'docs.analytics.eachStepMatched': {
    en: "Each step is matched per person and only at-or-after the previous step's time, so order",
    ar: 'تُطابَق كل خطوة لكل شخص وعند وقت الخطوة السابقة أو بعده فقط، فالترتيب',
  },
  'docs.analytics.screenDwell': {
    en: 'Screen dwell — gap to the next event',
    ar: 'زمن البقاء على الشاشة — الفجوة حتى الحدث التالي',
  },
  'docs.analytics.dwellExplainer': {
    en: 'Time on a screen is the gap to the next event in that session, capped at 30 minutes.',
    ar: 'زمن البقاء على الشاشة هو الفجوة حتى الحدث التالي في تلك الجلسة، بحدٍّ أقصى 30 دقيقة.',
  },
  'docs.analytics.perfPercentiles': {
    en: 'Performance — interpolated percentiles',
    ar: 'الأداء — المئينات المستكملة',
  },
  'docs.analytics.sessionKeyed': {
    en: 'Keyed on (app, session_id); its span grows to [first seen, last seen] with running event and error counts.',
    ar: 'مفتاحها (app, session_id)؛ ويمتد نطاقها إلى [أول ظهور، آخر ظهور] مع عدّادات جارية للأحداث والأخطاء.',
  },
  'docs.analytics.deviceKeyed': {
    en: "Keyed on a stable device_key — your SDK's persistent install id, else a family|model|os|arch descriptor so web clients still cluster.",
    ar: 'مفتاحها device_key ثابت — وهو معرّف التثبيت الدائم لحزمة التطوير، وإلا فواصف family|model|os|arch كي تظل عملاء الويب متجمّعة.',
  },

  // --- uptime --------------------------------------------------------------
  'docs.uptime.probes': {
    en: 'Active HTTP/TCP probes on a fixed schedule (14 presets, 1 second to 24 hours), each with a timeout and failure/recovery thresholds.',
    ar: 'فحوص HTTP/TCP نشطة وفق جدول ثابت (14 إعدادًا مسبقًا، من ثانية واحدة إلى 24 ساعة)، لكلٍّ منها مهلة وعتبات للإخفاق والتعافي.',
  },
  'docs.uptime.claim': {
    en: 'A prober claims due monitors with a single atomic',
    ar: 'يطالب الفاحص بالمراقبات المستحقة عبر عملية ذرية واحدة',
  },
  'docs.uptime.ssrf': {
    en: "Every target and webhook URL is SSRF-guarded: loopback, private, link-local, CGNAT and cloud-metadata (169.254.169.254) addresses are refused, redirects aren't followed, and response bodies are capped at 1 MiB.",
    ar: 'كل هدف ورابط Webhook محميّ من هجمات SSRF: تُرفض عناوين الاسترجاع والخاصة والمحلية وCGNAT وبيانات السحابة الوصفية (169.254.169.254)، ولا تُتبَّع عمليات إعادة التوجيه، ويُحدّ حجم أجسام الاستجابة بميبي بايت واحد.',
  },
  'docs.uptime.knobs': { en: 'The knobs', ar: 'المقابض' },

  // --- final sentence fragments -------------------------------------------
  // Each of these opens a sentence that is interrupted by a `<b>` or `<code>`
  // element, so the run is translated on its own; see `prose.ts` for the same
  // convention. Keyboard keys ("Enter", "↓") and technology names (Postgres,
  // Parquet, DWARF, Source Map v3) stay Latin in the markup.
  'docs.frag.clickAddFilter': { en: 'Click', ar: 'انقر' },
  'docs.frag.pressArrow': { en: 'Press', ar: 'اضغط' },
  'docs.frag.theEdge': { en: 'The', ar: 'تصادق' },
  'docs.lifecycle.hourlyExportFull': {
    en: 'An hourly job exports whole partitions older than the hot window to Parquet via DuckDB (laid out by app / year / month),',
    ar: 'تُصدِّر مهمة كل ساعة الأقسام الكاملة الأقدم من النافذة الساخنة إلى Parquet عبر DuckDB (مرتَّبة حسب التطبيق / السنة / الشهر)،',
  },
  'docs.arch.everythingLandsOn': {
    en: 'Everything an SDK sends — errors, events, identifies, transactions, breadcrumbs — travels the same path and lands on',
    ar: 'كل ما ترسله حزمة التطوير — أخطاء وأحداث وتعريفات ومعاملات وخطوات تتبّع — يسلك المسار نفسه وينتهي إلى',
  },
  'docs.rbac.tempPasswordFull': {
    en: "That temp password grants nothing except replacing itself: every authenticated endpoint but changing the password and logging out is refused until it's replaced, and first sign-in routes straight to a change-password screen. After that they get a fresh session and normal access.",
    ar: 'كلمة المرور المؤقتة تلك لا تمنح شيئًا سوى استبدال نفسها: إذ تُرفض كل نقطة نهاية موثَّقة عدا تغيير كلمة المرور وتسجيل الخروج حتى يجري استبدالها، ويقود أول تسجيل دخول مباشرةً إلى شاشة تغيير كلمة المرور. وبعد ذلك يحصلون على جلسة جديدة ووصول طبيعي.',
  },
  'docs.analytics.dwellFull': {
    en: "Time on a screen is the gap to the next event in that session, capped at 30 minutes. The inner subquery drops each session's",
    ar: 'زمن البقاء على الشاشة هو الفجوة حتى الحدث التالي في تلك الجلسة، بحدٍّ أقصى 30 دقيقة. ويُسقِط الاستعلام الفرعي الداخلي من كل جلسة',
  },
  'docs.example': { en: 'Example', ar: 'مثال' },
  'docs.analytics.stepOrder.a': {
    en: "Each step is matched per person and only at-or-after the previous step's time, so order matters — call",
    ar: 'تُطابَق كل خطوة لكل شخص وعند وقت الخطوة السابقة أو بعده فقط، فالترتيب مهم — استدعِ',
  },
  'docs.analytics.stepOrder.b': {
    en: 'so events attribute to the same person. Only event names seen in the selected window appear in the picker.',
    ar: 'كي تُنسب الأحداث إلى الشخص نفسه. ولا تظهر في المُنتقي إلا أسماء الأحداث المرصودة في النافذة المحددة.',
  },
  'docs.search.jsonFields.a': {
    en: 'These address JSON rather than a table column. Which ones a page offers depends on the resource: Issues carry',
    ar: 'تخاطب هذه بنية JSON لا عمودًا في جدول. وأيّها تتيحه الصفحة يعتمد على المورد: فالاستثناءات تحمل',
  },
  'docs.search.jsonFields.b': { en: 'but no', ar: 'لكن دون عمود' },
  'docs.search.jsonFields.c': {
    en: 'column, Sessions carry the reverse, and the box only ever suggests what its own resource declares.',
    ar: '، والجلسات تحمل العكس، ولا يقترح الصندوق إلا ما يعلنه مورده الخاص.',
  },
  'docs.search.exampleQuery.a': {
    en: 'Unresolved issues on the checkout path, plus a free-text term. Written as chips it produces the first URL; written in the query box, the second. Both are accepted, and old',
    ar: 'الاستثناءات غير المحلولة في مسار الدفع، مع مصطلح نصي حر. فإذا كُتبت كرقائق أنتجت الرابط الأول، وإذا كُتبت في صندوق الاستعلام أنتجت الثاني. وكلاهما مقبول، كما أن الإشارات المرجعية القديمة من نوع',
  },
  'docs.search.exampleQuery.b': {
    en: 'bookmarks keep working — they are bridged onto the same query internally, so they return the same rows.',
    ar: 'تظل تعمل — إذ تُجسَّر داخليًا على الاستعلام نفسه، فتعيد الصفوف ذاتها.',
  },
  'docs.nav.item.dsn': { en: 'Your DSN', ar: 'عنوان DSN الخاص بك' },
  'docs.nav.item.concepts': { en: 'How it works', ar: 'كيف يعمل' },
  // --- Retention guide -------------------------------------------------------
  'docs.nav.item.retention': { en: 'Retention', ar: 'الاحتفاظ' },
  'docs.ret.title': { en: 'Retention & cohorts', ar: 'الاحتفاظ والأفواج' },
  'docs.ret.lead': {
    en: 'Retention answers the one question a rising active-user count cannot: did the people who arrived come back? Acquisition can refill a bucket that is draining just as fast, and only a cohort view shows the difference.',
    ar: 'يجيب الاحتفاظ عن السؤال الذي لا يجيب عنه ارتفاع عدد المستخدمين النشطين: هل عاد من وصلوا؟ فالاستقطاب قد يملأ دلوًا يفرغ بالسرعة نفسها، ولا يُظهر الفرق إلا عرض الأفواج.',
  },
  'docs.ret.readingTheGrid': { en: 'Reading the grid', ar: 'قراءة الشبكة' },
  'docs.ret.rowIs': {
    en: 'Each row is one cohort: everyone whose first-ever activity in this app fell on that day (or ISO week). Each column then follows that same group forward through THEIR own calendar, not yours.',
    ar: 'كل صف فوج واحد: كل من كان أول نشاط له في هذا التطبيق في ذلك اليوم (أو الأسبوع). ثم تتابع كل خانة المجموعة نفسها عبر تقويمها هي، لا تقويمك.',
  },
  'docs.ret.dayNIsRelative': {
    en: '"Day 5" is a different date on every row — the 6th for a cohort that started on the 1st, the 15th for one that started on the 10th. That alignment is the whole point.',
    ar: '"اليوم 5" تاريخ مختلف في كل صف — السادس لفوج بدأ في الأول، والخامس عشر لفوج بدأ في العاشر. وهذه المحاذاة هي جوهر الأمر.',
  },
  'docs.ret.day0': {
    en: 'Day 0 is always 100%: they were active in the period they arrived, by definition.',
    ar: 'اليوم 0 دائمًا 100%: فقد كانوا نشطين في الفترة التي وصلوا فيها، بحكم التعريف.',
  },
  'docs.ret.independent.h': { en: 'Each cell is independent — not a streak', ar: 'كل خانة مستقلة — وليست سلسلة متصلة' },
  'docs.ret.independent.b': {
    en: 'Day 10 = 5% means 5% were active on day 10. It says nothing about days 1-9. Someone who came on day 1, vanished, and reappeared on day 10 counts in both, and the 5% on day 9 need not be the same people as the 5% on day 10. "Used it every day for ten days" is a different, much smaller number that this grid does not show.',
    ar: 'اليوم 10 = 5% يعني أن 5% كانوا نشطين في اليوم العاشر. ولا يقول شيئًا عن الأيام 1-9. فمن جاء في اليوم الأول ثم اختفى وعاد في اليوم العاشر يُحتسب في الاثنين، وليس بالضرورة أن يكون 5% اليوم التاسع هم أنفسهم 5% اليوم العاشر. أما "استخدمه كل يوم لعشرة أيام" فرقم مختلف وأصغر بكثير لا تعرضه هذه الشبكة.',
  },
  'docs.ret.hatched.h': { en: 'Hatched is never zero', ar: 'الخانة المخطّطة ليست صفرًا أبدًا' },
  'docs.ret.hatched.b': {
    en: 'Two different things are unknowable and both render hatched: the period has not elapsed yet (which is why the hatching forms a staircase — newer cohorts have had less time), or it predates the recorded data. An elapsed period with data behind it and nobody returning is a true 0%, and is shown as one.',
    ar: 'أمران مختلفان لا يمكن معرفتهما وكلاهما يظهر مخطّطًا: فترة لم تنقضِ بعد (ولهذا يتخذ التخطيط شكل السلّم — فالأفواج الأحدث أمامها وقت أقل)، أو فترة تسبق البيانات المسجّلة. أما فترة انقضت ولها بيانات ولم يعد فيها أحد فهي 0% حقيقية وتُعرض كذلك.',
  },
  'docs.ret.readDirections': {
    en: 'Read across a row to see whether that group sticks. Read DOWN a column to compare cohorts at the same age — that is where a product change shows up first.',
    ar: 'اقرأ الصف أفقيًا لترى هل تستمر تلك المجموعة. واقرأ العمود رأسيًا لمقارنة الأفواج في العمر نفسه — وهناك يظهر أثر أي تغيير في المنتج أولًا.',
  },
  'docs.ret.units': {
    en: 'Click any cell to switch the whole grid between percentages and user counts, or export CSV for raw counts (unelapsed periods export as empty fields, never 0, so a spreadsheet average is not poisoned).',
    ar: 'انقر أي خلية لتبديل الشبكة كلها بين النسب وأعداد المستخدمين، أو صدّر CSV للأعداد الخام (تُصدَّر الفترات غير المنقضية كحقول فارغة لا كصفر، حتى لا يفسد متوسط جدول البيانات).',
  },
  'docs.ret.lifecycle.h': { en: 'Lifecycle', ar: 'دورة الحياة' },
  'docs.ret.lifecycle.b': {
    en: 'New, returning and resurrected partition each period\u2019s active people exactly; dormant (active last period, silent this one) is drawn below the axis. This is the chart that catches churn-and-replace: a flat active-user line made entirely of "new" is a treadmill, not growth.',
    ar: 'ينقسم النشطون في كل فترة انقسامًا تامًا إلى جدد وعائدين ومستعادين؛ أما الخاملون (نشطون في الفترة السابقة وصامتون في هذه) فيُرسمون تحت المحور. وهذا هو الرسم الذي يكشف الفقدان والاستبدال: فخط نشاط ثابت مكوّن كله من "جدد" هو دوران في المكان لا نمو.',
  },
  'docs.ret.errorSplit.h': { en: 'Compare users who hit an error', ar: 'مقارنة من واجهوا خطأ' },
  'docs.ret.errorSplit.b': {
    en: 'Redraws the grid twice: once for people who hit an error in their FIRST period, once for everyone else. Exposure is measured in the first period only, and that is what keeps it honest rather than circular \u2014 a user who churns immediately cannot accumulate later errors, so splitting over the whole window would sort short-lived users into the clean half by construction. It remains an association, not a cause.',
    ar: 'يعيد رسم الشبكة مرتين: مرة لمن واجهوا خطأً في فترتهم الأولى، ومرة لبقية المستخدمين. ويُقاس التعرّض في الفترة الأولى فقط، وهذا ما يبقي المقارنة نزيهة لا دائرية — فمن ينقطع فورًا لا يمكن أن يراكم أخطاءً لاحقة، ولذا فإن التقسيم على النافذة كلها يضع قصيري البقاء في النصف "السليم" بحكم البناء. وتبقى العلاقة اقترانًا لا سببية.',
  },
  'docs.ret.identified.h': { en: 'Identified users only', ar: 'المستخدمون المعرّفون فقط' },
  'docs.ret.identified.b': {
    en: 'Restricts every card to people your app named via identify(), an event whose context.user.id equals its distinct_id, or the backfill \u2014 the same column Active Users splits on. It defaults to OFF on purpose: guests are most of the arrivals, and the filter selects for people who already converted, so retention reads higher. It filters people, not periods: someone who browsed anonymously and signed up later is identified for their whole history, so their cohort is still their first anonymous sighting.',
    ar: 'يقصر كل البطاقات على من عرّفهم تطبيقك عبر identify()، أو حدث يتطابق فيه context.user.id مع distinct_id، أو التعبئة الأولية — وهو العمود نفسه الذي تنقسم عليه صفحة المستخدمين النشطين. وهو معطّل افتراضيًا عن قصد: فالضيوف هم غالبية الوافدين، والمرشّح يختار من تحوّلوا بالفعل، فيبدو الاحتفاظ أعلى. وهو يرشّح الأشخاص لا الفترات: فمن تصفّح كضيف ثم سجّل يُعدّ معرّفًا طوال تاريخه، ويبقى فوجه أول ظهور مجهول له.',
  },
  'docs.ret.insights.h': { en: 'Insights', ar: 'الرؤى' },
  'docs.ret.insights.b': {
    en: 'A computed reading of what is on screen \u2014 day-1 level and direction, the first-timer share, the ratio of people gained to people going dormant, any period where everyone went silent, and the best cohort. Each finding carries a recommended next step, and several link to the page that answers it. Every statement is derived from the data on the page; nothing is estimated.',
    ar: 'قراءة محسوبة لما يظهر على الشاشة — مستوى اليوم الأول واتجاهه، ونسبة الوافدين لأول مرة، ونسبة المكتسبين إلى الخاملين، وأي فترة صمت فيها الجميع، وأفضل فوج. وتحمل كل نتيجة خطوة تالية مقترحة، ويربط بعضها بالصفحة التي تجيب عنها. وكل عبارة مشتقة من بيانات الصفحة؛ ولا شيء مُقدَّر تقديرًا.',
  },
  'docs.ret.atRisk.h': { en: 'At risk', ar: 'المعرّضون للخطر' },
  'docs.ret.atRisk.b': {
    en: 'People active before and silent since, with their lifetime events, errors and sessions. Every column sorts server-side, each row expands for tenure and silence detail, and the person id opens their profile.',
    ar: 'أشخاص كانوا نشطين ثم صمتوا، مع أحداثهم وأخطائهم وجلساتهم طوال المدة. كل عمود يُرتَّب من الخادم، وكل صف يتوسّع لعرض تفاصيل المدة والصمت، ومعرّف الشخص يفتح ملفه.',
  },
  'docs.ret.backfill.h': { en: 'One-time backfill', ar: 'تعبئة أولية لمرة واحدة' },
  'docs.ret.backfill.b': {
    en: 'Retention reads a rollup that starts recording the day the feature is deployed, so apps older than that need one run of the command below \u2014 it covers every predating app at once. Until then those apps show a card naming it rather than a grid: an empty grid is indistinguishable from "nobody came back", and answering 0% confidently is worse than declining to answer. Run ANALYZE afterwards; a backfill ships no statistics.',
    ar: 'يقرأ الاحتفاظ تجميعة تبدأ التسجيل يوم تثبيت الميزة، لذا تحتاج التطبيقات الأقدم إلى تشغيل الأمر أدناه مرة واحدة — وهو يغطي كل التطبيقات السابقة دفعة واحدة. وحتى ذلك الحين تعرض تلك التطبيقات بطاقة تسمّي الأمر بدل الشبكة: فالشبكة الفارغة لا تُميَّز عن "لم يعد أحد"، والإجابة بثقة بـ 0% أسوأ من الامتناع عن الإجابة. وشغّل ANALYZE بعدها؛ فالتعبئة لا تأتي بإحصاءات.',
  },
  'docs.ret.identity.h': { en: 'If the numbers look impossibly low', ar: 'إذا بدت الأرقام منخفضة على نحو غير معقول' },
  'docs.ret.identity.b': {
    en: 'Retention is only as meaningful as the identity behind it. If a returning visitor arrives carrying a NEW distinct_id, they are counted as a brand-new person in a brand-new cohort and can never appear as retained \u2014 daily cohorts balloon toward your page-load count and retention collapses toward zero. Check that your anonymous id persists across page loads, and that identify() is called on login with your canonical user id, the same string in every app: matching is exact string equality, and there is no server-side repair.',
    ar: 'لا يكون الاحتفاظ ذا معنى إلا بقدر الهوية التي خلفه. فإذا عاد زائر حاملًا معرّفًا جديدًا، احتُسب شخصًا جديدًا تمامًا في فوج جديد ولا يمكن أن يظهر أبدًا كمُحتفَظ به — فتتضخم الأفواج اليومية نحو عدد مرات تحميل الصفحة وينهار الاحتفاظ نحو الصفر. تأكّد أن المعرّف المجهول يبقى عبر تحميلات الصفحة، وأن identify() يُستدعى عند تسجيل الدخول بمعرّف المستخدم المعتمد لديك، وبالسلسلة نفسها في كل التطبيقات: فالمطابقة تساوٍ نصّي تام، ولا إصلاح من جانب الخادم.',
  },
  'docs.ret.freshness': {
    en: 'The grid and lifecycle are served stale-while-revalidate \u2014 under an hour old as-is, between one and three hours as-is while a single background refresh recomputes. The "as of" chip states the age.',
    ar: 'تُقدَّم الشبكة ودورة الحياة وفق مبدأ "قديم أثناء التحديث" — أقل من ساعة كما هي، وبين ساعة وثلاث ساعات كما هي مع إعادة حساب واحدة في الخلفية. وتوضّح شارة "حتى" العمر.',
  },
  'docs.nav.item.funnels': { en: 'Funnels', ar: 'مسارات التحويل' },
  'docs.nav.item.verify': { en: 'Verify setup', ar: 'التحقق من الإعداد' },
  'docs.nav.item.search': { en: 'Search & filtering', ar: 'البحث والتصفية' },
  'docs.nav.item.privacy': { en: 'Privacy inspector', ar: 'مفتّش الخصوصية' },
  'docs.nav.item.troubleshooting': { en: 'Troubleshooting', ar: 'استكشاف الأخطاء' },
  'docs.nav.item.architecture': { en: 'Architecture', ar: 'البنية' },
  'docs.nav.item.grouping': { en: 'Error grouping', ar: 'تجميع الأخطاء' },
  'docs.nav.item.analytics': { en: 'Analytics & people', ar: 'التحليلات والأشخاص' },
  'docs.nav.item.queries': { en: 'Queries behind it', ar: 'الاستعلامات وراءه' },
  'docs.nav.item.tiering': { en: 'Data lifecycle', ar: 'دورة حياة البيانات' },
  'docs.nav.item.uptime': { en: 'Uptime monitoring', ar: 'مراقبة التوافر' },
  'docs.nav.item.rbac': { en: 'Access control', ar: 'التحكم في الوصول' },
  'docs.nav.item.sdkInternals': { en: 'SDK internals', ar: 'داخل حزم التطوير' },
} as const satisfies Record<string, Message>;
