import type { Message } from '../types';

/**
 * Explanatory paragraphs that wrap inline `<code>`, `<b>` or `<a>` elements.
 *
 * These are split into runs rather than held as one string because the markup
 * has to survive: a code identifier must stay inside `<code>` to keep its
 * font, its `direction: ltr` island, and its meaning. Each `.a`/`.b`/`.c` key
 * is one prose run between two inline elements, and the Arabic is written to
 * read correctly in that fixed order rather than assuming the translator can
 * move the code around.
 */
export const prose = {
  // --- transaction detail panel -------------------------------------------
  'prose.tx.noMeta.a': {
    en: 'No tags or additional data on this span. Attach some by passing',
    ar: 'لا توجد وسوم أو بيانات إضافية على هذا المقطع. أرفق بعضها بتمرير',
  },
  'prose.tx.noMeta.b': { en: '/', ar: 'أو' },
  'prose.tx.noMeta.c': { en: 'to', ar: 'إلى' },
  'prose.tx.withheld.a': {
    en: 'Tags and additional data are withheld — they need the',
    ar: 'الوسوم والبيانات الإضافية محجوبة — فهي تتطلب صلاحية',
  },

  // --- store connections ---------------------------------------------------
  'prose.stores.remove': {
    en: 'Removes the stored credentials and stops syncing. The install history already collected is kept, and reconnecting resumes against it.',
    ar: 'يزيل بيانات الاعتماد المخزَّنة ويوقف المزامنة. يُحتفظ بسجل التثبيت المجمَّع بالفعل، وتستأنف إعادة الربط العمل عليه.',
  },
  'prose.stores.environment': {
    en: 'Which environment represents the build that ships to the stores. The Overview section appears only when this environment is selected. Store numbers themselves are app-wide — the stores report per package, not per environment.',
    ar: 'أي بيئة تمثّل النسخة التي تُنشر في المتاجر. يظهر قسم النظرة العامة فقط عند اختيار هذه البيئة. أما أرقام المتاجر فهي على مستوى التطبيق كله — إذ تبلّغ المتاجر لكل حزمة لا لكل بيئة.',
  },

  // --- alerts --------------------------------------------------------------
  'prose.alerts.channels': {
    en: 'A channel is where an alert is delivered. Secrets are encrypted at rest and never returned by the API.',
    ar: 'القناة هي وجهة تسليم التنبيه. تُشفَّر الأسرار عند التخزين ولا تعيدها واجهة البرمجة أبدًا.',
  },
  'prose.alerts.rules': {
    en: 'A rule decides when to notify and which channels to fan out to. Repeat alerts for the same cause are suppressed for the throttle window.',
    ar: 'تحدّد القاعدة متى يجري التنبيه وإلى أي القنوات يُوزَّع. وتُكتم التنبيهات المتكررة للسبب نفسه طوال نافذة التقييد.',
  },
  'prose.alerts.subtitle': {
    en: 'Deliver notifications to email, Slack, Discord, Element/Matrix, Telegram or any webhook — on triggers you define.',
    ar: 'أرسل الإشعارات إلى البريد الإلكتروني أو Slack أو Discord أو Element/Matrix أو Telegram أو أي Webhook — وفق مُطلِقات تحدّدها بنفسك.',
  },

  // --- source maps ---------------------------------------------------------
  'prose.sourcemaps.subtitle': {
    en: 'Upload JavaScript source maps and Flutter symbol files so minified and obfuscated stack traces resolve to your original code.',
    ar: 'ارفع خرائط مصدر JavaScript وملفات رموز Flutter لتُحَلّ آثار المكدس المصغَّرة والمشوَّشة إلى شيفرتك الأصلية.',
  },
  'prose.sourcemaps.coverage.a': {
    en: 'An obfuscated Flutter build needs',
    ar: 'تحتاج نسخة Flutter المشوَّشة إلى',
  },
  'prose.sourcemaps.coverage.b': {
    en: 'artifacts, and they fix different halves of what you read. The symbols file resolves stack',
    ar: 'من الملفات، وكلٌّ منها يصلح نصفًا مختلفًا مما تقرأه. يحلّ ملف الرموز',
  },
  'prose.sourcemaps.coverage.c': {
    en: '; only the obfuscation map resolves the exception',
    ar: ' المكدس؛ أما خريطة التشويش فهي وحدها التي تحلّ',
  },
  'prose.sourcemaps.coverage.d': {
    en: '— the SDK sends',
    ar: ' للاستثناء — إذ ترسل حزمة التطوير',
  },
  'prose.sourcemaps.coverage.e': {
    en: ', which the build already renamed, and no amount of debug info reverses that.',
    ar: '، وقد أعادت عملية البناء تسميته بالفعل، ولا يعكس ذلك أي قدر من معلومات التنقيح.',
  },
  'prose.sourcemaps.noMap.a': {
    en: 'Not every build needs a map — one compiled without',
    ar: 'لا تحتاج كل نسخة إلى خريطة — فالنسخة المبنية بدون',
  },
  'prose.sourcemaps.noMap.b': {
    en: 'has no renamed names to reverse. This lists what is uploaded, not a guess about how you built.',
    ar: 'ليس فيها أسماء معاد تسميتها لعكسها. تسرد هذه القائمة ما جرى رفعه، لا تخمينًا لكيفية بنائك.',
  },
  'prose.sourcemaps.debugId.a': {
    en: "The debug id is read out of the file's own build-id note — nothing to paste. Flutter emits these with",
    ar: 'يُقرأ معرّف التنقيح من ملاحظة معرّف البناء داخل الملف نفسه — فلا شيء تلصقه. ويُصدِر Flutter هذه الملفات عبر',
  },
  'prose.sourcemaps.sameId.a': { en: 'Use the', ar: 'استخدم' },
  'prose.sourcemaps.sameId.b': {
    en: 'same debug id as this build’s symbols',
    ar: 'معرّف التنقيح نفسه المستخدم لرموز هذه النسخة',
  },
  'prose.sourcemaps.sameId.c': {
    en: '— it is the only thing tying the two together, and the upload is refused without it. Flutter emits the map with',
    ar: '— فهو الرابط الوحيد بينهما، ويُرفض الرفع بدونه. ويُصدِر Flutter الخريطة عبر',
  },

  // --- purge ---------------------------------------------------------------
  'prose.purge.lede': {
    en: 'Permanently delete signal data for one app, then repair the session, device, person and issue counters the deletion affects.',
    ar: 'احذف بيانات الإشارات نهائيًا لتطبيق واحد، ثم أصلح عدّادات الجلسات والأجهزة والأشخاص والاستثناءات التي يمسّها الحذف.',
  },
  'prose.purge.previewFirst': {
    en: 'Nothing is deleted until you confirm what the preview shows.',
    ar: 'لا يُحذف شيء حتى تؤكّد ما تعرضه المعاينة.',
  },
  'prose.purge.rollups': {
    en: 'Raw kinds are deleted. Rollup kinds are recomputed from what survives and removed only when nothing is left — they are repaired whether or not you tick them.',
    ar: 'تُحذف الأنواع الخام. أما أنواع التجميع فيُعاد احتسابها مما تبقّى ولا تُزال إلا حين لا يتبقّى شيء — وتُصلَح سواء حدّدتها أم لا.',
  },
  'prose.purge.drift': {
    en: 'This app was still receiving events when the job started. Recomputed counters can drift; stop the sender first for an exact result.',
    ar: 'كان هذا التطبيق لا يزال يستقبل الأحداث عند بدء المهمة. قد تنحرف العدّادات المعاد احتسابها؛ أوقف المُرسِل أولاً للحصول على نتيجة دقيقة.',
  },
  'prose.purge.title': { en: 'Purge data', ar: 'تطهير البيانات' },
  'prose.purge.limitEnvs': { en: 'Limit to specific environments', ar: 'القصر على بيئات محددة' },
  'prose.purge.kinds': { en: 'Kinds', ar: 'الأنواع' },
  'prose.purge.matched': { en: 'Matched', ar: 'المطابق' },
  'prose.purge.requestedBy': { en: 'Requested by', ar: 'طلبها' },

  // --- storage -------------------------------------------------------------
  'prose.storage.restore': {
    en: 'Copies a range back out of Parquet into Postgres so the rest of the dashboard can query it again. The Parquet copy is never removed, so a restore adds storage rather than moving it — which is why every restore expires.',
    ar: 'ينسخ نطاقًا من Parquet إلى Postgres ليتمكن باقي لوحة التحكم من الاستعلام عنه مجددًا. لا تُحذف نسخة Parquet أبدًا، لذا فالاستعادة تضيف مساحة تخزين بدل نقلها — ولهذا تنتهي صلاحية كل استعادة.',
  },
  'prose.storage.rotation': {
    en: 'Data older than this moves out of Postgres into Parquet. It stays readable — queries span both tiers — but it no longer occupies database storage.',
    ar: 'تنتقل البيانات الأقدم من ذلك من Postgres إلى Parquet. وتبقى قابلة للقراءة — إذ تشمل الاستعلامات الطبقتين — لكنها لم تعد تشغل مساحة في قاعدة البيانات.',
  },
  'prose.storage.pins': {
    en: 'Each pin keeps a restored range from being re-tiered. Without one, a restore is undone on the next cycle.',
    ar: 'يمنع كل تثبيت إعادةَ ترحيل نطاق مستعاد. وبدونه تُلغى الاستعادة في الدورة التالية.',
  },

  // --- audit trail ---------------------------------------------------------
  'prose.audit.lede': { en: 'Every administrative action taken in', ar: 'كل إجراء إداري جرى في' },

  // --- environments --------------------------------------------------------
  'prose.env.keysFailed': {
    en: "Ingest keys could not be loaded for this project's apps — the request failed, so whether anything is enrolled here is unknown. Reload the page to try again.",
    ar: 'تعذّر تحميل مفاتيح الاستقبال لتطبيقات هذا المشروع — فشل الطلب، لذا لا يُعرف ما إذا كان أي شيء مسجَّلاً هنا. أعد تحميل الصفحة للمحاولة مجددًا.',
  },
  'prose.env.noApps': {
    en: 'No apps in this project yet. Create one and it is enrolled here automatically, with its own ingest key.',
    ar: 'لا توجد تطبيقات في هذا المشروع بعد. أنشئ تطبيقًا وسيُسجَّل هنا تلقائيًا بمفتاح استقبال خاص به.',
  },
  'prose.env.partialList.a': {
    en: 'Only the apps you can see are listed. Others in this project may be enrolled too — showing them needs',
    ar: 'تُدرَج التطبيقات التي يمكنك رؤيتها فقط. وقد تكون تطبيقات أخرى في هذا المشروع مسجَّلة أيضًا — ويتطلب عرضها صلاحية',
  },
  'prose.env.keysHidden.a': {
    en: "Per-app ingest keys aren't shown — listing this project's apps needs the",
    ar: 'لا تُعرض مفاتيح الاستقبال لكل تطبيق — إذ يتطلب سرد تطبيقات هذا المشروع صلاحية',
  },
  'prose.env.someFailed.a': {
    en: 'Some apps could not be loaded — their ingest keys failed to fetch, so apps that',
    ar: 'تعذّر تحميل بعض التطبيقات — أخفق جلب مفاتيح استقبالها، لذا فالتطبيقات التي',
  },
  'prose.env.catalogueNote.a': {
    en: 'The project-wide catalogue needs project-level',
    ar: 'يتطلب كتالوج المشروع صلاحية على مستوى المشروع',
  },

  // --- ingest failures -----------------------------------------------------
  'prose.failures.auditFirst': {
    en: 'An entry naming this group and its counts is written to the audit trail first — that record is the only thing that survives.',
    ar: 'يُكتب في سجل التدقيق أولاً قيدٌ يسمّي هذه المجموعة وأعدادها — وذلك القيد هو الشيء الوحيد الذي يبقى.',
  },
  'prose.failures.lede': {
    en: 'Events that never made it into storage, grouped by cause. Transient failures are retried automatically before they appear here.',
    ar: 'أحداث لم تصل إلى التخزين، مجمَّعة حسب السبب. تُعاد محاولة الإخفاقات العابرة تلقائيًا قبل ظهورها هنا.',
  },
  'prose.failures.deletes.a': { en: 'This permanently deletes', ar: 'يحذف هذا نهائيًا' },

  // --- privacy inspector ---------------------------------------------------
  'prose.inspector.shapeDetectors': {
    en: 'Match by value SHAPE rather than key name — they find PII under a key you did not think to track. They read more rows than a key list does, so a scan takes longer.',
    ar: 'تطابق حسب شكل القيمة لا اسم المفتاح — فتجد بيانات شخصية تحت مفتاح لم يخطر لك تتبّعه. وهي تقرأ صفوفًا أكثر من قائمة المفاتيح، فيستغرق الفحص وقتًا أطول.',
  },
  'prose.inspector.dst': {
    en: 'On the spring-forward day this resolves to a valid instant; on the fall-back day it runs once, not twice. Times from 04:00 avoid the question entirely.',
    ar: 'في يوم التقديم الصيفي يُحلّ هذا إلى لحظة صالحة؛ وفي يوم التأخير يُنفَّذ مرة واحدة لا مرتين. والأوقات من الساعة 04:00 فصاعدًا تتفادى المسألة تمامًا.',
  },
  'prose.inspector.precedence': {
    en: 'The most specific policy covering an app wins whole. A narrower one subtracts its scope from the parent, which is how you exclude one noisy environment.',
    ar: 'تسود السياسة الأكثر تحديدًا التي تغطي التطبيق بالكامل. أما الأضيق فتطرح نطاقها من الأصل، وبذلك تستثني بيئة واحدة كثيرة الضجيج.',
  },
  'prose.inspector.scopeLabel': { en: 'Scope:', ar: 'النطاق:' },
  'prose.inspector.statusLabel': { en: 'Status:', ar: 'الحالة:' },
  'prose.inspector.enabled': { en: 'enabled', ar: 'مُفعَّلة' },
  'prose.inspector.disabled': { en: 'disabled', ar: 'مُعطَّلة' },

  // --- final wrapped paragraphs -------------------------------------------
  'prose.route.error': {
    en: 'Sauron loads each page on demand and this one’s code could not be downloaded. If Sauron was updated while this tab was open, reloading picks up the new version.',
    ar: 'يحمّل Sauron كل صفحة عند الطلب، وتعذّر تنزيل شيفرة هذه الصفحة. وإذا جرى تحديث Sauron أثناء فتح هذه العلامة، فسيجلب إعادةُ التحميل الإصدارَ الجديد.',
  },
  'prose.timeline.truncated': {
    en: 'The SDK capped this payload at 16 KB and sent a marker instead. The span and its timing are accurate; only the attached data was dropped.',
    ar: 'حدّت حزمة التطوير حجم هذه الحمولة عند 16 كيلوبايت وأرسلت علامة بدلاً منها. المقطع وتوقيته دقيقان؛ ولم تُحذف سوى البيانات المرفقة.',
  },
  'prose.mask.countDrift': {
    en: 'The count was taken a moment ago. On an actively ingesting app more rows will match by the time the mask runs, so a larger "rows masked" figure afterwards is normal, not an error.',
    ar: 'أُخذ العدّ قبل لحظات. وفي تطبيق يستقبل البيانات باستمرار ستطابق صفوف أكثر بحلول وقت تنفيذ الإخفاء، لذا فإن رقم «الصفوف المخفاة» الأكبر لاحقًا أمر طبيعي وليس خطأ.',
  },
  'prose.mask.enforcerOrder.a': {
    en: 'The mask enforcer runs before identification, so once',
    ar: 'يُنفَّذ مطبِّق الإخفاء قبل تحديد الهوية، لذا بمجرد أن يصبح',
  },
  'prose.members.oneTimeSecret': {
    en: 'This is the only time it is shown. If you lose it, deactivate the account and create it again.',
    ar: 'تُعرض هذه المرة فقط. وإذا فقدتها، عطّل الحساب وأنشئه من جديد.',
  },
  'prose.members.deactivated': {
    en: 'This account is deactivated. You can remove access, but new grants will be refused until it is reactivated.',
    ar: 'هذا الحساب معطَّل. يمكنك إزالة الصلاحيات، لكن المنح الجديدة سترفض حتى إعادة تفعيله.',
  },
  'prose.members.selfEdit': {
    en: 'You are editing your own access. Removing these grants can end your ability to manage members.',
    ar: 'أنت تعدّل صلاحياتك الخاصة. وقد تفقد بإزالة هذه الصلاحيات قدرتك على إدارة الأعضاء.',
  },
  'prose.members.resetWarning': {
    en: 'Their current password stops working immediately and they are signed out of every device within a few seconds. We email them a link that expires in 24 hours. If it does not arrive, come back here to send another or to cancel.',
    ar: 'تتوقف كلمة مرورهم الحالية فورًا ويُسجَّل خروجهم من كل الأجهزة خلال ثوانٍ. ونرسل إليهم رابطًا تنتهي صلاحيته خلال 24 ساعة. وإن لم يصل، عد إلى هنا لإرسال رابط آخر أو للإلغاء.',
  },
  'prose.members.resetPending': {
    en: 'They will still be asked to choose a new one when they do. Any reset link already sent stops working.',
    ar: 'سيُطلب منهم اختيار كلمة مرور جديدة عند دخولهم. ويتوقف أي رابط إعادة تعيين سبق إرساله.',
  },
  'prose.stores.lede': {
    en: "Pull daily install and uninstall counts from Google Play and the App Store. Reports are daily and arrive one to three days late — this is the stores' own cadence, not a delay Sauron adds.",
    ar: 'اجلب أعداد التثبيت وإلغاء التثبيت اليومية من Google Play وApp Store. التقارير يومية وتصل متأخرة من يوم إلى ثلاثة أيام — وهذه وتيرة المتاجر نفسها، لا تأخيرٌ يضيفه Sauron.',
  },
  'prose.boot.loading': { en: 'Loading Sauron…', ar: 'جارٍ تحميل Sauron…' },
  // Descriptive placeholders — these tell the user what to type, so they are
  // translated. The format examples beside them (hostnames, versions, addresses)
  // deliberately stay Latin: their shape is the instruction.
  'prose.placeholder.opsSlack': { en: 'Ops Slack', ar: 'قناة العمليات' },
  'prose.placeholder.ruleName': { en: 'API down → oncall', ar: 'تعطّل الواجهة ← المناوب' },
  'prose.placeholder.optional': { en: 'Optional', ar: 'اختياري' },
  'prose.placeholder.debugId': {
    en: 'the id the symbols upload reported',
    ar: 'المعرّف الذي أبلغ عنه رفع الرموز',
  },

  // --- trailing sentence runs ---------------------------------------------
  // Each closes a sentence that an inline <strong>/<TimeValue> interrupts.
  'prose.store.directions': { en: '↑ installs · ↓ uninstalls', ar: '↑ تثبيت · ↓ إلغاء تثبيت' },
  'prose.alerts.isMoreThan': { en: 'is more than', ar: 'أكبر من' },
  'prose.alerts.isLessThan': { en: 'is less than', ar: 'أصغر من' },
  'prose.login.resetSent': { en: 'a link to set a new one.', ar: 'رابطًا لتعيين كلمة مرور جديدة.' },
  'prose.perf.perBucket': { en: 'transactions / bucket', ar: 'معاملة لكل فترة' },
  'prose.person.lastSeen': { en: '· Last seen', ar: '· آخر ظهور' },
  'prose.projects.confirmDelete': { en: 'and every app beneath it?', ar: 'وكل تطبيق تحته؟' },
  'prose.settings.confirmDelete': { en: 'and all its data?', ar: 'وكل بياناته؟' },
  'prose.audit.newestFirst': { en: ', newest first.', ar: '، الأحدث أولاً.' },
  'prose.purge.step1': { en: '1 — What to purge', ar: '1 — ما الذي سيُطهَّر' },
  'prose.purge.step3': { en: '3 — Time range', ar: '3 — النطاق الزمني' },
  'prose.purge.allTimeNote': {
    en: "— the app's entire history for the ticked kinds",
    ar: '— كامل تاريخ التطبيق للأنواع المحددة',
  },

  // --- last trailing runs --------------------------------------------------
  'prose.users.combinedNoteTail': {
    en: 'counts people across several apps at once.',
    ar: 'يحتسب الأشخاص عبر عدة تطبيقات دفعةً واحدة.',
  },
  'prose.scope.entireOrg': { en: 'entire org', ar: 'المؤسسة بأكملها' },
  'prose.members.tempPasswordTail': {
    en: 'this temporary password. They must change it the first time they sign in.',
    ar: 'كلمة المرور المؤقتة هذه. يجب عليهم تغييرها عند أول تسجيل دخول.',
  },
  'prose.purge.blockedKind': {
    en: 'no environment column — clear the environment filter to purge this',
    ar: 'لا يوجد عمود بيئة — امسح مرشّح البيئة لتطهير هذا',
  },
  'prose.env.catalogueNote.b': {
    en: '(env:read) — showing only the environments your apps are enrolled in. Renaming and retiring are catalogue-level actions and aren\u2019t available from here.',
    ar: '(env:read) — تُعرض فقط البيئات المسجَّلة فيها تطبيقاتك. أما إعادة التسمية والإيقاف فهي إجراءات على مستوى الكتالوج وغير متاحة من هنا.',
  },
  'prose.env.partialList.b': { en: '(app:read).', ar: '(app:read).' },
  'prose.env.someFailed.b': {
    en: 'enrolled here may be missing from this list. Reload the page to try again.',
    ar: 'المسجَّلة هنا قد تكون غائبة عن هذه القائمة. أعد تحميل الصفحة للمحاولة مجددًا.',
  },
  'prose.env.keysHidden.b': {
    en: "(app:read) permission, which your role doesn't grant here. The environments themselves are shown in full.",
    ar: '(app:read)، وهي غير ممنوحة لدورك هنا. أما البيئات نفسها فتُعرض كاملة.',
  },
  // --- mask dialog identity warning ---------------------------------------
  'prose.mask.identity.b': {
    en: '— or whatever key your app uses as its',
    ar: '— أو أي مفتاح يستخدمه تطبيقك بوصفه',
  },
  'prose.mask.identity.c': {
    en: '; an email address is both a common choice and exactly the kind of value a PII policy flags — is masked, no future person can ever be marked identified through it. Nobody already identified loses the flag, so nothing moves on the day the mask lands: instead',
    ar: '؛ وعنوان البريد الإلكتروني خيار شائع وهو تحديدًا نوع القيم التي تشير إليها سياسة البيانات الشخصية — عند إخفائه لن يُمكن بعد ذلك اعتبار أي شخص جديد معرَّفًا عبره. ولا يفقد أحد ممن عُرِّفوا سابقًا هذه الصفة، فلا يتغير شيء يوم تطبيق الإخفاء: بل',
  },
  'prose.mask.identity.d': {
    en: 'plateaus and then decays as the existing population churns, while',
    ar: 'يستقر ثم يتناقص مع تبدّل الجمهور الحالي، بينما',
  },
  'prose.mask.identity.e': {
    en: 'climbs to meet it. Nothing labels the cause and nothing can reconstruct it afterwards. Decide before you apply the mask, not after.',
    ar: 'يرتفع حتى يلتقي به. ولا شيء يوضّح السبب ولا يمكن استرجاعه لاحقًا. فقرّر قبل تطبيق الإخفاء لا بعده.',
  },
  'prose.mask.identity.f': {
    en: 'report is where this shows up, and it shows up as a trend with no event on it.',
    ar: 'هو التقرير الذي يظهر فيه ذلك، ويظهر بوصفه اتجاهًا دون أي حدث يفسّره.',
  },
} as const satisfies Record<string, Message>;
