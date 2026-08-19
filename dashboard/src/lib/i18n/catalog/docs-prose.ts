import type { Message } from '../types';

/**
 * The body prose of the in-dashboard guide (`/docs`).
 *
 * Separate from `docs.ts`, which holds that page's navigation and headings.
 * These are the paragraphs, and they are split into runs for the reason
 * `prose.ts` explains: each is the text between two inline `<code>`, `<b>` or
 * `<a>` elements, and the identifiers inside those must stay verbatim — a
 * translated `--obfuscate` or `distinct_id` names a flag no build accepts.
 *
 * The Arabic is written to read correctly in the run order the markup fixes,
 * not as a free translation of the whole paragraph.
 */
export const docsProse = {
  // --- DSN & concepts ------------------------------------------------------
  'dp.dsn.perEnv': {
    en: 'environment. Each environment has its own — see',
    ar: 'بيئة. لكل بيئة عنوانها الخاص — انظر',
  },
  'dp.dsn.autofill': {
    en: 'to auto-fill your real key.',
    ar: 'لملء مفتاحك الحقيقي تلقائيًا.',
  },
  'dp.concepts.envHoldsDsn': {
    en: 'holds the DSN — a public key plus the environment id. Each environment under an app gets its own, so switching environments means switching DSNs. Your SDK batches, gzips, and posts envelopes to the ingest gateway, where the dashboard sorts them into two signal types:',
    ar: 'تحمل عنوان DSN — مفتاحًا عامًا ومعرّف البيئة. ولكل بيئة تحت التطبيق عنوانها الخاص، لذا فتبديل البيئة يعني تبديل العنوان. تجمّع حزمة التطوير الحمولات وتضغطها وترسلها إلى بوابة الاستقبال، حيث تصنّفها لوحة التحكم إلى نوعين من الإشارات:',
  },
  'dp.concepts.trackIdentify': {
    en: 'track() / identify() feed Users, Sessions & Funnels.',
    ar: 'تغذّي track() وidentify() صفحات المستخدمين والجلسات ومسارات التحويل.',
  },

  // --- funnels -------------------------------------------------------------
  'dp.funnels.eventNames': { en: 'event names', ar: 'أسماء الأحداث' },
  'dp.funnels.addStages': { en: 'and add your stage events', ar: 'وأضف أحداث مراحلك' },
  'dp.funnels.seeConversion': {
    en: 'to see overall conversion and step-by-step drop-off.',
    ar: 'لعرض التحويل الإجمالي والتسرّب خطوةً بخطوة.',
  },

  // --- search & query language --------------------------------------------
  'dp.search.takeReal': { en: '— take a real', ar: '— خذ' },
  'dp.search.queryLanguage': { en: 'query language', ar: 'لغة استعلام حقيقية' },
  'dp.search.pressArrow': {
    en: 'in the search box to see what the current page accepts, and',
    ar: 'في صندوق البحث لعرض ما تقبله الصفحة الحالية، و',
  },
  'dp.search.runIt': {
    en: 'button) to run it — typing alone never queries. An invalid query is rejected with the reason on the box itself — it never silently returns zero rows.',
    ar: 'لتشغيله — فالكتابة وحدها لا تُجري استعلامًا أبدًا. ويُرفض الاستعلام غير الصالح مع بيان السبب على الصندوق نفسه — ولا يعيد صفرًا من الصفوف بصمت.',
  },
  'dp.search.buildChip': { en: 'to build a chip. Chips combine with', ar: 'لبناء رقاقة. وتُجمع الرقائق بعامل' },
  'dp.search.freeTextOnTop': {
    en: '; the free-text box and date range still apply on top. An issue’s',
    ar: '؛ ويظل صندوق النص الحر والنطاق الزمني ينطبقان فوق ذلك. أما',
  },
  'dp.search.twoInputChip': { en: 'is a two-input chip — a', ar: 'رقاقة بحقلَي إدخال —' },
  'dp.search.caseSensitive': { en: 'case-sensitive', ar: 'حسّاسة لحالة الأحرف' },
  'dp.search.jsonbContainment': {
    en: 'whole-value match (a JSONB containment check, backed by a GIN index so it stays fast on a large app). Only reach for it when you know the value’s exact spelling and casing.',
    ar: 'مطابقة للقيمة كاملة (فحص احتواء JSONB مدعوم بفهرس GIN فيظل سريعًا على تطبيق كبير). لا تلجأ إليها إلا حين تعرف الهجاء الدقيق للقيمة وحالة أحرفها.',
  },
  'dp.search.zeroRows': { en: 'zero rows with no error', ar: 'صفر صفوف دون أي خطأ' },

  // --- privacy inspector ---------------------------------------------------
  'dp.privacy.finds': {
    en: 'finds developer-supplied PII sitting in the telemetry JSON columns, proves what it found',
    ar: 'يعثر على البيانات الشخصية التي أرسلها المطور والقابعة في أعمدة JSON للقياسات، ويثبت ما وجده',
  },
  'dp.privacy.noSecondCopy': {
    en: 'without storing a second copy of it',
    ar: 'دون تخزين نسخة ثانية منها',
  },
  'dp.privacy.twelvePlaces': {
    en: '— cold Parquet, the Redis ingest stream and DLQ, alerts that already sent, backups and replicas all still hold the original bytes, and the dialog names all twelve places before it lets you confirm. And',
    ar: '— إذ لا تزال البايتات الأصلية موجودة في Parquet البارد ودفق Redis وقائمة الرسائل الميتة والتنبيهات المُرسَلة والنسخ الاحتياطية والنسخ المتماثلة، ويسمّي مربع الحوار المواضع الاثني عشر كلها قبل أن يسمح لك بالتأكيد. كما أن',
  },
  'dp.privacy.cannotUndo': { en: 'a mask cannot be undone', ar: 'الإخفاء لا يمكن التراجع عنه' },
  'dp.privacy.noReverse': {
    en: ': there is no reverse operation anywhere in the product. Masking a key an app sends as its identity also stops future',
    ar: ': فلا توجد عملية عكسية في أي مكان من المنتج. كما أن إخفاء مفتاح يرسله التطبيق بوصفه هويته يوقف مستقبلاً',
  },
  'dp.privacy.activeUsers': { en: 'active-users', ar: 'تحديد المستخدمين النشطين' },
  'dp.privacy.permanently': {
    en: 'identification through it, permanently.',
    ar: 'عبره، بصورة دائمة.',
  },

  // --- architecture --------------------------------------------------------
  'dp.arch.oneTimeline': { en: 'one timeline', ar: 'مخطط زمني واحد' },
  'dp.arch.keyedToApp': { en: 'keyed to your app.', ar: 'مرتبط بتطبيقك.' },
  'dp.arch.onePerItem': { en: 'one job per item', ar: 'مهمة واحدة لكل عنصر' },

  // --- error grouping ------------------------------------------------------
  'dp.grouping.sha256': {
    en: '— a SHA-256 computed with the first rule below that applies:',
    ar: '— وهي SHA-256 تُحتسب بأول قاعدة تنطبق مما يلي:',
  },
  'dp.grouping.atIngest': {
    en: '— at ingest when symbols are uploaded, otherwise on read. An obfuscated Flutter build needs a',
    ar: '— عند الاستقبال إذا رُفعت الرموز، وإلا فعند القراءة. وتحتاج نسخة Flutter المشوَّشة إلى',
  },
  'dp.grouping.artifactForType': {
    en: 'artifact for the exception',
    ar: 'ملف إضافي لاسم صنف الاستثناء',
  },

  // --- analytics internals -------------------------------------------------
  'dp.analytics.rollups': {
    en: 'are materialized roll-ups, upserted on every signal.',
    ar: 'تجميعات مادّية تُحدَّث أو تُدرَج مع كل إشارة.',
  },
  'dp.analytics.inSql': {
    en: ', in SQL — no pre-aggregation service.',
    ar: '، داخل SQL — دون خدمة تجميع مسبق.',
  },
  'dp.analytics.perPerson': { en: 'per person', ar: 'لكل شخص' },
  'dp.analytics.atOrAfter': {
    en: 'and only at-or-after the previous step’s time. A step’s count is the distinct people who reached it; conversion and drop-off come from the counts.',
    ar: 'وعند وقت الخطوة السابقة أو بعده فقط. وعدد الخطوة هو الأشخاص المميزون الذين بلغوها؛ ويُشتق التحويل والتسرّب من هذه الأعداد.',
  },
  'dp.analytics.dauWauMau': {
    en: 'are rolling 1/7/30-day distinct actives; stickiness is DAU ÷ MAU.',
    ar: 'هي أعداد النشطين المميزين على نوافذ متحركة مدتها 1 و7 و30 يومًا؛ ومعدل الالتصاق هو DAU ÷ MAU.',
  },

  // --- data lifecycle ------------------------------------------------------
  'dp.tiering.hotThenCold': {
    en: 'in Postgres for ~30 days, then age into columnar',
    ar: 'في Postgres نحو 30 يومًا، ثم تنتقل مع الزمن إلى تخزين عمودي',
  },
  'dp.tiering.spanBothTiers': {
    en: '— and reads span both tiers transparently.',
    ar: '— وتشمل عمليات القراءة الطبقتين معًا بصورة شفافة.',
  },
  'dp.tiering.verifiesCounts': {
    en: 'verifies the row counts match',
    ar: 'يتحقق من تطابق أعداد الصفوف',
  },
  'dp.tiering.watermark': {
    en: ', advances a watermark, and only then drops the Postgres partition — after a grace lag and a re-count guard, so a late-arriving row is never dropped. On read, a query’s time window is split at the watermark: the hot half (live partitions) and the cold half (Parquet, plus any late arrivals) run concurrently and their per-day partials are summed. Holistic metrics like percentiles stay hot-only.',
    ar: '، ثم يقدّم علامة مائية، وعندها فقط يُسقط قسم Postgres — بعد مهلة سماح وفحص إعادة عدّ، فلا يُسقط أبدًا صف وصل متأخرًا. وعند القراءة تُقسم نافذة الاستعلام الزمنية عند العلامة المائية: فيُنفَّذ النصف الساخن (الأقسام الحيّة) والنصف البارد (Parquet وما وصل متأخرًا) بالتوازي، وتُجمع نتائجهما الجزئية اليومية. أما المقاييس الكلية كالمئينات فتبقى على الطبقة الساخنة وحدها.',
  },

  // --- uptime --------------------------------------------------------------
  'dp.uptime.probing': {
    en: 'probing — so multiple probers never double-fire and a slow check can’t stack. Each probe records up/down, status code and response time; consecutive-failure and -success thresholds debounce flapping, and a transition opens or resolves an incident and fires a webhook.',
    ar: 'الفحص — فلا يكرّر عدة فاحصين الإطلاق ولا تتراكم فحوص بطيئة. ويسجّل كل فحص حالة العمل أو التوقف ورمز الحالة وزمن الاستجابة؛ وتُخمد عتبات الإخفاق والنجاح المتتاليين التذبذبَ، ويفتح أي تحوّل حادثًا أو يحلّه ويُطلق Webhook.',
  },

  // --- access control ------------------------------------------------------
  'dp.rbac.thirtyPerms': { en: '30 atomic permissions', ar: '30 صلاحية ذرّية' },
  'dp.rbac.whichAre': { en: ', which are', ar: '، وهي' },
  'dp.rbac.atAScope': {
    en: 'at a scope — org, project, or app. Your effective permissions are the',
    ar: 'عند نطاق ما — مؤسسة أو مشروع أو تطبيق. وصلاحياتك الفعلية هي',
  },
  'dp.rbac.cascade': {
    en: 'of every grant that applies, cascading down Org → Project → App: an org grant covers everything beneath it; a project grant covers its apps but not its siblings.',
    ar: 'اتحاد كل منحة تنطبق، متدرّجةً من المؤسسة ← المشروع ← التطبيق: فمنحة المؤسسة تغطي كل ما تحتها؛ ومنحة المشروع تغطي تطبيقاته دون أقرانه.',
  },
  'dp.rbac.oneRole': { en: 'one role', ar: 'دورًا واحدًا' },
  'dp.rbac.ticksScopes': {
    en: ', then ticks any mix of scopes to hand it at — the whole org, whole projects, individual apps — and the server creates the account and every grant together, all or nothing. Ticking a whole project also covers apps added to it later. The response reveals a',
    ar: '، ثم يحدّد أي مزيج من النطاقات لمنحه عندها — المؤسسة بأكملها أو مشاريع كاملة أو تطبيقات بعينها — وينشئ الخادم الحساب وكل المنح معًا، إما كلها أو لا شيء. كما أن تحديد مشروع كامل يغطي التطبيقات المضافة إليه لاحقًا. وتكشف الاستجابة عن',
  },
  'dp.rbac.tempPasswordOnce': {
    en: '16-character temporary password exactly once',
    ar: 'كلمة مرور مؤقتة من 16 حرفًا مرةً واحدة فقط',
  },
  'dp.rbac.copyButton': {
    en: ', with a copy button — no endpoint can retrieve it again, so a lost password means deactivating the account and creating it again. The admin can’t choose or see a durable password for them.',
    ar: '، مع زرّ نسخ — فلا تستطيع أي نقطة نهاية استرجاعها ثانيةً، ومن ثمّ يعني فقدانها تعطيل الحساب وإنشاءه من جديد. ولا يمكن للمسؤول اختيار كلمة مرور دائمة لهم ولا الاطّلاع عليها.',
  },
  'dp.rbac.grantAccessTool': {
    en: '— the other form on the Members page — is still the right tool for someone who already has an account, including a member of another organization; Create member is only for someone who doesn’t.',
    ar: '— وهو النموذج الآخر في صفحة الأعضاء — يظل الأداة الصحيحة لمن يملك حسابًا بالفعل، بمن في ذلك عضو في مؤسسة أخرى؛ أما «إنشاء عضو» فهو لمن لا يملك حسابًا فحسب.',
  },
  'dp.rbac.editInPlace': {
    en: '. Edit changes their role and scope in place, or adds another grant alongside the ones they already hold — each grant saves independently. Deactivate is a',
    ar: '. ويغيّر «تعديل» دورَهم ونطاقَهم في مكانه، أو يضيف منحة أخرى إلى جانب ما يملكونه — وتُحفظ كل منحة على حدة. أما «تعطيل» فهو',
  },
  'dp.rbac.killSwitch': {
    en: 'login kill switch, not a removal',
    ar: 'مفتاح إيقاف لتسجيل الدخول، لا إزالة',
  },
  'dp.rbac.roleInPlace': {
    en: 'in place — the dialog shows how many members hold the role, since saving changes their access immediately. The built-in Owner, Admin, Developer and Viewer roles open in a',
    ar: 'في مكانه — ويعرض مربع الحوار عدد الأعضاء الذين يحملون الدور، لأن الحفظ يغيّر صلاحياتهم فورًا. أما أدوار المالك والمسؤول والمطور والمشاهد المدمجة فتُفتح في',
  },
  'dp.rbac.viewOnly': { en: 'view-only', ar: 'وضع العرض فقط' },
  'dp.rbac.resynced': {
    en: 'dialog instead: they’re re-synced from the server’s own definitions on every restart, so an edit would silently revert. You can’t grant a role, edit one, or mint a custom one with permissions you don’t already hold at that scope, so access can never escalate itself.',
    ar: 'بدلاً من ذلك: إذ تُزامَن من تعريفات الخادم نفسه عند كل إعادة تشغيل، فأي تعديل سيُلغى بصمت. ولا يمكنك منح دور أو تعديله أو إنشاء دور مخصص بصلاحيات لا تملكها أصلاً عند ذلك النطاق، فلا تستطيع الصلاحيات أن تتصاعد ذاتيًا أبدًا.',
  },
  'dp.rbac.devicesList': {
    en: 'page listing the devices their account is signed in on — device, address, when they signed in and when the session was last used. The session they are currently using is badged',
    ar: 'صفحة تسرد الأجهزة التي سُجِّل دخول حسابهم عليها — الجهاز والعنوان ووقت الدخول وآخر استخدام للجلسة. والجلسة التي يستخدمونها حاليًا موسومة',
  },
  'dp.rbac.cannotSignOutHere': {
    en: 'and cannot be signed out from there;',
    ar: 'ولا يمكن تسجيل الخروج منها هناك؛',
  },
  'dp.rbac.topBarVerb': { en: 'in the top bar is that verb.', ar: 'في الشريط العلوي هو ذلك الإجراء.' },
  'dp.rbac.endsEverySession': {
    en: 'ends every session but the current one. "Show recent sign-outs" reveals the last 30 days of ended sessions with the reason each one ended, which is how a user learns that something other than themselves closed a session.',
    ar: 'ينهي كل جلسة عدا الحالية. ويكشف «عرض عمليات الخروج الأخيرة» جلسات آخر 30 يومًا المنتهية مع سبب انتهاء كل منها، وبها يعرف المستخدم أن جهةً غيره أغلقت جلسة.',
  },

  // --- SDK internals -------------------------------------------------------
  'dp.sdk.envelopeShape': {
    en: '— a header (SDK, release), a context block (device, os, app, runtime, user) and a list of typed items (error, event, identify, transaction, breadcrumb batch).',
    ar: '— ترويسة (حزمة التطوير والإصدار)، وكتلة سياق (الجهاز ونظام التشغيل والتطبيق وبيئة التشغيل والمستخدم)، وقائمة عناصر مُصنَّفة (خطأ، حدث، تعريف، معاملة، دفعة خطوات تتبّع).',
  },

  // --- runs split by an inline <code>/<b> -----------------------------------
  // Keyed to the LITERAL text between two tags, not to the paragraph read as a
  // whole. An earlier pass keyed these to the flattened sentence (code removed)
  // and none of them matched: the markup run ends at the `<code>`, so the
  // catalogue has to end there too.
  'dp.r.funnelsSendWith': { en: 'you already send with', ar: 'التي ترسلها بالفعل عبر' },
  'dp.r.funnelsMeasures': {
    en: '. Sauron measures how many distinct people reach each step — counted in order, per person — plus the drop-off between them.',
    ar: '. يقيس Sauron عدد الأشخاص المميزين الذين يبلغون كل خطوة — محسوبين بالترتيب ولكل شخص — إضافةً إلى نسبة التسرّب بينها.',
  },
  'dp.r.boxAutocompletes': {
    en: '. The box autocompletes the fields and values the resource actually has, and a term with no field is a free-text search over the payload.',
    ar: '. يكمل الصندوق تلقائيًا الحقول والقيم التي يملكها المورد فعلاً، وأي مصطلح بلا حقل يُعدّ بحثًا نصيًا حرًا في الحمولة.',
  },
  'dp.r.chipsBeside': {
    en: ') still sit beside it and still work; they AND with whatever the box holds. Everywhere else there is a plain substring box, or nothing — coverage is uneven by design, so check which a page has before deciding a term "isn\u2019t there."',
    ar: ') تبقى بجانبه وتظل تعمل؛ وتُجمع بعامل «و» مع ما يحتويه الصندوق. أما في المواضع الأخرى فيوجد صندوق مطابقة نصية بسيط أو لا شيء — والتغطية متفاوتة عن قصد، فتحقّق مما تملكه الصفحة قبل أن تستنتج أن المصطلح «غير موجود».',
  },
  'dp.r.freeTextIs': {
    en: 'in front of it is a free-text search, and it is always a case-insensitive substring match (Postgres',
    ar: 'الذي يسبقه بحثٌ نصي حر، وهو دائمًا مطابقة جزئية غير حساسة لحالة الأحرف (Postgres',
  },
  'dp.r.noRanking': {
    en: ') — no ranking, no fuzzy matching, no tokenizer. An empty box returns everything in range. If you hold',
    ar: ') — دون ترتيب بالأهمية ولا مطابقة تقريبية ولا تجزئة للكلمات. ويعيد الصندوق الفارغ كل ما في النطاق. وإذا كنت تملك',
  },
  'dp.r.payloadWithheld': {
    en: ', the payload columns are withheld and the search quietly matches fewer of them; the list says so above the rows rather than leaving you to guess.',
    ar: '، فإن أعمدة الحمولة تُحجب ويطابق البحث عددًا أقل منها بهدوء؛ وتذكر القائمة ذلك فوق الصفوف بدل أن تتركك تخمّن.',
  },
  'dp.r.addFilter': { en: '+ Add filter', ar: '+ إضافة مرشّح' },
  'dp.r.occurrencesList': {
    en: 'list (Issue detail page) offers the same mechanism, but only the',
    ar: 'قائمة التكرارات (في صفحة تفاصيل الاستثناء) تتيح الآلية نفسها، لكن للحقل',
  },
  'dp.r.spelledInBox': {
    en: '. In the query box the same two behaviours are spelled',
    ar: '. وفي صندوق الاستعلام يُكتب السلوكان نفساهما',
  },
  'dp.r.substringAnd': { en: '(substring) and', ar: '(مطابقة جزئية) و' },
  'dp.r.exactTrap': {
    en: '(exact), so the trap below is visible in the query itself rather than hidden behind a dropdown that defaulted for you.',
    ar: '(مطابقة تامة)، فيصبح الفخّ الموصوف أدناه ظاهرًا في الاستعلام ذاته بدل أن يختفي خلف قائمة اختارت لك افتراضًا.',
  },
  'dp.r.composedInto': { en: '— composed into one', ar: '— تُدمج في قيمة' },
  'dp.r.filterValueHood': {
    en: 'filter value under the hood (the backend splits on the',
    ar: 'مرشّح واحدة داخليًا (إذ يفصل الخادم عند',
  },
  'dp.r.valueContains': { en: ', so a value that itself contains', ar: '، فالقيمة التي تحتوي بنفسها على' },
  'dp.r.roundTrips': {
    en: 'still round-trips). Both a key and a value are required, or the chip silently doesn\u2019t get added. Two operators are offered, and the UI picks the',
    ar: 'تظل سليمة ذهابًا وإيابًا). ويلزم مفتاح وقيمة معًا، وإلا لم تُضف الرقاقة بصمت. ويُتاح معاملان، وتختار الواجهة',
  },
  'dp.r.substringMatch': {
    en: '— case-insensitive substring match on that key\u2019s value. Use this when you know the key but only part of the value, or aren\u2019t sure of the casing — key',
    ar: '— مطابقة جزئية غير حساسة لحالة الأحرف على قيمة ذلك المفتاح. استخدمها حين تعرف المفتاح وجزءًا من القيمة فقط، أو حين لا تتأكد من حالة الأحرف — فالمفتاح',
  },
  'dp.r.partialReturns': {
    en: 'and type a partial or wrong-case value, and the filter returns',
    ar: 'وتكتب قيمة جزئية أو بحالة أحرف خاطئة، فيعيد المرشّح',
  },
  'dp.r.indistinguishable': {
    en: '— indistinguishable from "search doesn\u2019t work." If a Tag chip comes back empty, switch it to',
    ar: '— وهو ما لا يمكن تمييزه عن «البحث لا يعمل». فإذا عادت رقاقة الوسم فارغة، فبدّلها إلى',
  },
  'dp.r.beforeConcluding': {
    en: 'before concluding the tag isn\u2019t there.',
    ar: 'قبل أن تستنتج أن الوسم غير موجود.',
  },
  'dp.r.onlyLooksAt': { en: 'only looks at the developer-set', ar: 'لا تنظر إلا في خريطة' },
  'dp.r.mapNever': { en: 'map — never', ar: 'التي يضبطها المطور — لا في' },
  'dp.r.machineOwned': { en: ', or the machine-owned', ar: '، ولا في كتلة' },
  'dp.r.singularBlob': {
    en: '(singular) blob; use the free-text box for those.',
    ar: '(المفردة) المملوكة للنظام؛ استخدم صندوق النص الحر لتلك.',
  },
  'dp.r.addressBar': {
    en: ', every chip and the search term are written into the address bar (',
    ar: '، تُكتب كل رقاقة ومصطلح البحث في شريط العنوان (',
  },
  'dp.r.repeatedPlus': { en: ', repeated, plus', ar: '، مكرَّرةً، مع' },
  'dp.r.copyUrl': {
    en: ') — copy the URL to hand someone the exact same filtered view. The Occurrences list, and every free-text-only or client-side page above, doesn\u2019t do this — reloading or sharing the link loses whatever you typed.',
    ar: ') — فانسخ الرابط لتسلّم شخصًا العرض المصفّى نفسه تمامًا. أما قائمة التكرارات وكل صفحة نصية-حرة أو تعمل في المتصفح مما سبق فلا تفعل ذلك — وإعادة التحميل أو مشاركة الرابط تفقد ما كتبته.',
  },
  'dp.r.masksInHot': {
    en: ', masks it in hot Postgres, and enforces that mask on all future ingest. It needs',
    ar: '، ويخفيها في Postgres الساخن، ويفرض ذلك الإخفاء على كل ما يُستقبل لاحقًا. وهو يتطلب',
  },
  'dp.r.maskingNeeds': { en: '; masking needs', ar: '؛ ويتطلب الإخفاء' },
  'dp.r.ownerAdminHold': {
    en: '. Owner and Admin hold both; Developer and Viewer hold neither.',
    ar: '. ويملك المالك والمسؤول كلتيهما، ولا يملك المطور ولا المشاهد أيًّا منهما.',
  },
  'dp.r.tenancyFromKey': {
    en: '(the URL\u2019s project id is ignored — tenancy comes from the key), applies a per-app rate limit, splits the envelope into',
    ar: '(ويُتجاهل معرّف المشروع في الرابط — إذ تأتي هوية المستأجر من المفتاح)، وتطبّق حدًّا للمعدل لكل تطبيق، وتقسّم الحمولة إلى',
  },
  'dp.r.ontoRedis': { en: 'onto a Redis stream, and answers', ar: 'على دفق Redis، وتردّ بـ' },
  'dp.r.neverBlocks': {
    en: 'immediately — your app never blocks on processing.',
    ar: 'فورًا — فلا يتوقف تطبيقك أبدًا في انتظار المعالجة.',
  },
  'dp.r.workersDrain': {
    en: 'in the same process drain the stream as a consumer group (at-least-once, with acknowledgements and a dead-letter queue for poison messages) and write to Postgres. Signals live in time-partitioned tables, all tagged with your',
    ar: 'في العملية نفسها الدفقَ بوصفهم مجموعة استهلاك (بتسليم مرة واحدة على الأقل، مع إقرارات وقائمة رسائل ميتة للرسائل المعطوبة) ويكتبون إلى Postgres. وتعيش الإشارات في جداول مقسَّمة زمنيًا، جميعها موسومة بـ',
  },
  'dp.r.sideBySide': {
    en: '— which is exactly what lets an error and an event for the same person sit side by side.',
    ar: '— وهذا تحديدًا ما يجعل خطأً وحدثًا للشخص نفسه يقفان جنبًا إلى جنب.',
  },
  'dp.r.dartVia': { en: '), Dart via', ar: ')، وDart عبر' },
  'dp.r.jsonSameDebugId': {
    en: 'JSON, uploaded under the same debug id. Symbols fix the frames; only the map fixes the type, because the SDK sends',
    ar: 'بصيغة JSON، يُرفع تحت معرّف التنقيح نفسه. تصلح الرموزُ الإطارات؛ ولا يصلح النوعَ إلا الخريطة، لأن حزمة التطوير ترسل',
  },
  'dp.r.presentational': {
    en: 'and the build already renamed it. Both are presentational — grouping stays on the raw values, so uploading either one later never re-groups issues you have. Affected-user counts use a HyperLogLog sketch, so they stay cheap at any volume.',
    ar: 'وقد غيّرت عملية البناء اسمه بالفعل. وكلاهما للعرض فقط — إذ يبقى التجميع على القيم الخام، فرفع أيٍّ منهما لاحقًا لا يعيد تجميع استثناءاتك القائمة. أما أعداد المستخدمين المتأثرين فتستخدم مخطط HyperLogLog، فتظل زهيدة الكلفة مهما بلغ الحجم.',
  },
  'dp.r.writesEvents': { en: 'writes events;', ar: 'يكتب الأحداث؛ و' },
  'dp.r.writesPeople': {
    en: 'writes people (aliasing an anonymous id onto a known one when you pass one).',
    ar: 'يكتب الأشخاص (مع ربط معرّف مجهول بآخر معروف حين تمرّره).',
  },
  'dp.r.noNext': { en: 'event (no \u201cnext\u201d, so', ar: 'الأخير (فلا حدث «تالٍ»، ومن ثمّ' },
  'dp.r.isNull': { en: 'is null) — otherwise', ar: 'تكون القيمة فارغة) — وإلا لَأسند' },
  'dp.r.bogusDwell': {
    en: 'would hand it a bogus 30-minute dwell.',
    ar: 'إليه زمن بقاء وهميًا مدته 30 دقيقة.',
  },
  'dp.r.smoothPercentiles': { en: 'gives smooth p50/p95/p99 over', ar: 'يعطي قيم p50 وp95 وp99 سلسة على' },
  'dp.r.errorRateShare': {
    en: '; error rate is the share of transactions that failed.',
    ar: '؛ أما معدل الأخطاء فهو نسبة المعاملات التي أخفقت.',
  },
  'dp.r.numberSteps': { en: 'number each person\u2019s events into steps (', ar: 'ترقّم أحداث كل شخص إلى خطوات (' },
  'dp.r.sankey': {
    en: ') and count step\u2192step transitions into a Sankey.',
    ar: ') وتحصي الانتقالات من خطوة إلى أخرى في مخطط Sankey.',
  },
  'dp.r.advancesNextCheck': {
    en: 'that advances the next check time',
    ar: 'تقدّم موعد الفحص التالي',
  },
  'dp.r.bundleInto': { en: ', …) bundle into', ar: '، …) تتجمّع في' },
  'dp.r.createMemberFor': {
    en: '(Members \u2192 Create member) is for someone who doesn\u2019t have an account yet. An admin with',
    ar: '(الأعضاء ← إنشاء عضو) مخصّص لمن ليس لديه حساب بعد. فالمسؤول الذي يملك',
  },
  'dp.r.suppliesEmail': {
    en: 'supplies their email and name, picks',
    ar: 'يقدّم بريدَه واسمَه، ويختار',
  },
  'dp.r.grantsIntact': {
    en: ': every grant stays intact, the row stays listed with a "Deactivated" badge, and Reactivate restores normal sign-in. Their sessions are revoked immediately, and any access token already issued stops working within a few seconds — every API replica refreshes its revoked-session list on the',
    ar: ': فتبقى كل منحة سليمة، ويظل الصف مدرجًا بشارة «معطَّل»، ويعيد «إعادة التفعيل» تسجيلَ الدخول الطبيعي. وتُلغى جلساتهم فورًا، ويتوقف أي رمز وصول سبق إصداره خلال ثوانٍ — إذ تحدّث كل نسخة من واجهة البرمجة قائمة الجلسات الملغاة على فترة',
  },
  'dp.r.pollInterval': {
    en: 'interval (5 seconds by default). Deactivating yourself, a member who also belongs to another organization, or the last holder of',
    ar: '(5 ثوانٍ افتراضيًا). أما تعطيل نفسك، أو عضو ينتمي أيضًا إلى مؤسسة أخرى، أو آخر من يملك',
  },
  'dp.r.refusedExplanation': {
    en: 'is refused with an explanation instead.',
    ar: 'فيُرفض مع بيان السبب.',
  },
  'dp.r.canSignOut': {
    en: 'can sign a member out of every device from the Members page. That permission is carved out of',
    ar: 'تتيح تسجيل خروج عضو من كل الأجهزة عبر صفحة الأعضاء. وهذه الصلاحية مقتطعة من',
  },
  'dp.r.carvedOut': {
    en: 'rather than added beside it, so a role can administer membership without also holding the verbs that act on someone\u2019s credentials. The admin surface deliberately shows no per-device detail — only the one coarse verb. Signing someone out does not deactivate them and does not force a password change.',
    ar: 'لا مضافة إلى جانبها، فيستطيع دور أن يدير العضوية دون أن يملك أيضًا الإجراءات التي تمسّ بيانات اعتماد أحد. وواجهة الإدارة لا تعرض عمدًا أي تفصيل لكل جهاز — بل إجراءً واحدًا عامًا. وتسجيل خروج أحدهم لا يعطّله ولا يفرض تغيير كلمة المرور.',
  },
  'dp.r.oneDash': { en: 'one —', ar: 'أحدهما —' },
  'dp.r.byDefault': { en: '— by default:', ar: '— افتراضيًا:' },

  // --- reference tables ----------------------------------------------------
  // These render from arrays declared in the page's <script>. They were the
  // last thing still in English, and the reason is worth recording: the leak
  // test strips <script> before scanning, so display strings declared there are
  // invisible to it. Routing them through the catalogue is what makes the gate
  // honest about this page, not just what translates it.
  //
  // `sig` columns stay untouched in the markup — they are query-language
  // syntax, and a translated `field:!value` matches nothing.
  'dp.t.coverage.exceptions': {
    en: 'The query language, with autocomplete, plus filter chips — all server-side over the full dataset in the selected date range.',
    ar: 'لغة الاستعلام مع الإكمال التلقائي، إضافةً إلى رقائق التصفية — وكلها على الخادم عبر مجموعة البيانات الكاملة في النطاق الزمني المحدد.',
  },
  'dp.t.coverage.occurrences': {
    en: "The query language and the same chips, server-side, scoped to just that issue's events — but this view doesn't write its state to the URL.",
    ar: 'لغة الاستعلام والرقائق نفسها، على الخادم، مقصورةً على أحداث ذلك الاستثناء وحده — غير أن هذا العرض لا يكتب حالته في الرابط.',
  },
  // query-language operators
  'dp.t.op.equals': { en: 'Equals. level:error', ar: 'يساوي. level:error' },
  'dp.t.op.notEqual': { en: 'Not equal. level:!info', ar: 'لا يساوي. level:!info' },
  'dp.t.op.greater': {
    en: 'Greater than, or greater-or-equal. timesSeen:>5',
    ar: 'أكبر من، أو أكبر من أو يساوي. timesSeen:>5',
  },
  'dp.t.op.less': {
    en: 'Less than, or less-or-equal. duration:<500ms',
    ar: 'أصغر من، أو أصغر من أو يساوي. duration:<500ms',
  },
  'dp.t.op.relative': {
    en: 'On a timestamp field, a relative offset before now: s/sec, m/min, h/hour, d/day, w/week, mo/month. Months are real calendar months (1 month before 31 Mar is 28 Feb); everything else is a fixed span. m is MINUTES — months are mo or longer. A leading - is optional: 7d and -7d are the same. lastSeen:>=1month',
    ar: 'في حقل زمني، إزاحة نسبية قبل الآن: s/sec وm/min وh/hour وd/day وw/week وmo/month. والأشهر أشهر تقويمية حقيقية (شهر قبل 31 مارس هو 28 فبراير)؛ وما عداها مدد ثابتة. وحرف m يعني الدقائق — أما الأشهر فهي mo أو أطول. وعلامة - في البداية اختيارية: فـ 7d و-7d سواء. lastSeen:>=1month',
  },
  'dp.t.op.iso': {
    en: 'On a timestamp field, a full ISO-8601 instant.',
    ar: 'في حقل زمني، لحظة كاملة بصيغة ISO-8601.',
  },
  // query-language variables
  'dp.t.var.anyTag': {
    en: 'Matches across EVERY tag key — a bare @tag does not mean the key named "tag".',
    ar: 'يطابق عبر كل مفاتيح الوسوم — و@tag المجردة لا تعني المفتاح المسمّى «tag».',
  },
  'dp.t.var.namedKey': { en: 'One named key. @tag.region:eu', ar: 'مفتاح واحد بعينه. @tag.region:eu' },
  'dp.t.var.escapeHatch': {
    en: 'The escape hatch for keys containing characters outside A-Z a-z 0-9 _ . - — e.g. cart@checkout or 100%off.',
    ar: 'مخرج للمفاتيح التي تحتوي محارف خارج A-Z a-z 0-9 _ . - — مثل cart@checkout أو 100%off.',
  },
  'dp.t.var.context': {
    en: 'Device and runtime context. Requires event:read.',
    ar: 'سياق الجهاز وبيئة التشغيل. يتطلب event:read.',
  },
  'dp.t.var.extra': {
    en: 'Developer-attached extra metadata. Requires event:read.',
    ar: 'بيانات وصفية إضافية يرفقها المطور. يتطلب event:read.',
  },
  'dp.t.var.sort': {
    en: 'A bare column sorts DESCENDING; a leading - reverses it to ascending.',
    ar: 'العمود المجرّد يرتّب تنازليًا؛ وعلامة - في البداية تعكسه إلى تصاعدي.',
  },
  // free-text coverage
  'dp.t.free.exceptions': {
    en: "title, type, culprit — plus the underlying event's tags/contexts/extra payload (matched as text).",
    ar: 'العنوان والنوع والموضع المسبِّب — إضافةً إلى حمولة الوسوم والسياقات والبيانات الإضافية للحدث الأساسي (تُطابَق كنص).',
  },
  'dp.t.free.events': {
    en: 'name, distinct_id — plus the tags/contexts/extra/properties payload (as text).',
    ar: 'الاسم والمعرّف المميز — إضافةً إلى حمولة الوسوم والسياقات والبيانات الإضافية والخصائص (كنص).',
  },
  'dp.t.free.occurrences': {
    en: 'message, exception_value, exception_type — plus the tags/contexts/extra payload.',
    ar: 'الرسالة وقيمة الاستثناء ونوعه — إضافةً إلى حمولة الوسوم والسياقات والبيانات الإضافية.',
  },
  'dp.t.free.users': {
    en: 'distinct_id — plus the entire traits object as text, so an email, plan, or company typed here finds the person.',
    ar: 'المعرّف المميز — إضافةً إلى كائن السمات كاملاً كنص، فيكفي كتابة بريد إلكتروني أو خطة أو شركة للعثور على الشخص.',
  },
  'dp.t.free.devices': {
    en: 'family, model, os_name and device_key, glued together — "iphone 15" or "macos" both work.',
    ar: 'العائلة والطراز واسم النظام ومفتاح الجهاز، ملتصقة معًا — فـ «iphone 15» أو «macos» كلاهما يعمل.',
  },
  'dp.t.free.screens': { en: 'the screen name only.', ar: 'اسم الشاشة فقط.' },
  // chip operators by field type
  'dp.t.chip.text': {
    en: '=  ≠  contains — exact / not-exact / case-insensitive substring',
    ar: '=  ≠  يحتوي — مطابقة تامة / غير تامة / جزئية غير حساسة لحالة الأحرف',
  },
  'dp.t.chip.enum': {
    en: '=  ≠ — exact / not-exact against a fixed option list',
    ar: '=  ≠ — مطابقة تامة / غير تامة مقابل قائمة خيارات ثابتة',
  },
  'dp.t.chip.number': { en: '=  >  < — numeric compare', ar: '=  >  < — مقارنة عددية' },
  'dp.t.chip.tag': {
    en: 'contains (default)  = — see "The Tag filter" below',
    ar: 'يحتوي (الافتراضي)  = — انظر «مرشّح الوسم» أدناه',
  },
  // fingerprint rules
  'dp.t.fp.override.q': { en: '1 · Your override', ar: '1 · تجاوزك الخاص' },
  'dp.t.fp.override.a': {
    en: "If the SDK sends a fingerprint[], it's hashed verbatim — you control the grouping.",
    ar: 'إذا أرسلت حزمة التطوير fingerprint[]، فإنها تُجزّأ حرفيًا — فأنت من يتحكم في التجميع.',
  },
  'dp.t.fp.frames.q': { en: '2 · Stack frames', ar: '2 · إطارات المكدس' },
  'dp.t.fp.frames.a': {
    en: 'Otherwise: the exception type plus up to five frames (in-app first, crash last), each reduced to module::function. Line numbers, 0x… addresses, UUIDs, and content-hashed filenames (app.4f3a2b.js → app.js) are masked, so the same bug groups across builds and machines.',
    ar: 'وإلا: نوع الاستثناء مع خمسة إطارات كحد أقصى (إطارات التطبيق أولاً والعطل أخيرًا)، وكلٌّ منها مختزل إلى module::function. وتُخفى أرقام الأسطر والعناوين ‎0x…‎ ومعرّفات UUID وأسماء الملفات المجزّأة بالمحتوى (app.4f3a2b.js ← app.js)، فيتجمّع الخلل نفسه عبر النسخ والأجهزة.',
  },
  'dp.t.fp.message.q': { en: '3 · Message', ar: '3 · الرسالة' },
  'dp.t.fp.message.a': {
    en: 'No usable frames falls back to the type plus a normalized message; no exception at all hashes just the message.',
    ar: 'وعند غياب إطارات صالحة يُرجَع إلى النوع مع رسالة مُنمَّطة؛ وإذا لم يوجد استثناء أصلاً فتُجزّأ الرسالة وحدها.',
  },
  'dp.t.cov.exceptionsQ': { en: 'Exceptions (Issues)', ar: 'الاستثناءات' },
  'dp.t.cov.occurrencesQ': {
    en: "An issue's Occurrences (Issue detail page)",
    ar: 'تكرارات الاستثناء (صفحة تفاصيل الاستثناء)',
  },
  'dp.t.cov.sessionsA': {
    en: 'The query language, with autocomplete, server-side. Sessions carry no developer tags, so @tag is not offered here and a tag term is rejected.',
    ar: 'لغة الاستعلام مع الإكمال التلقائي، على الخادم. ولا تحمل الجلسات وسوم المطوّرين، لذا لا يُتاح @tag هنا ويُرفض أي مصطلح وسم.',
  },
  'dp.t.cov.plainQ': { en: 'Users, Devices, Screens', ar: 'المستخدمون والأجهزة والشاشات' },
  'dp.t.cov.plainA': {
    en: 'A plain free-text box — no query language, no chips. Server-side; Users has no date window (searches all time), Devices and Screens are scoped to the selected range.',
    ar: 'صندوق نص حر بسيط — بلا لغة استعلام ولا رقائق. ويعمل على الخادم؛ فالمستخدمون بلا نافذة زمنية (يبحث في كل الأوقات)، أما الأجهزة والشاشات فمقصورتان على النطاق المحدد.',
  },
  'dp.t.cov.clientA': {
    en: "A search box exists, but it only filters rows already loaded on the current page in the browser. It never queries the server — a match sitting on the next page won't show up.",
    ar: 'يوجد صندوق بحث، لكنه لا يصفّي إلا الصفوف المحمّلة بالفعل في الصفحة الحالية داخل المتصفح. ولا يستعلم الخادم أبدًا — فالنتيجة الواقعة في الصفحة التالية لن تظهر.',
  },
  'dp.t.cov.noneQ': {
    en: 'Performance, Journeys, Overview, Members, Monitors, Storage',
    ar: 'الأداء والرحلات والنظرة العامة والأعضاء والمراقبات والتخزين',
  },
  'dp.t.op.anyOf': { en: 'Any of. level:[error,fatal]', ar: 'أيٌّ من. level:[error,fatal]' },
  'dp.t.op.range': {
    en: 'Inclusive range, on the ordered types only (timestamps, integers, durations) — the same brackets as any-of, told apart by the .. and by the field. Both ends required. firstSeen:[7d..1d], timesSeen:[10..100]',
    ar: 'نطاق شامل، على الأنواع المرتَّبة فقط (الطوابع الزمنية والأعداد الصحيحة والمدد) — بالأقواس نفسها المستخدمة لـ«أيٌّ من»، ويُميَّز بينهما بـ .. وبالحقل. ويلزم طرفا النطاق معًا. firstSeen:[7d..1d], timesSeen:[10..100]',
  },
  'dp.t.op.contains': {
    en: 'Contains this literal substring, case-insensitive. * is NOT a wildcard here — it matches a literal asterisk.',
    ar: 'يحتوي على هذا النص الجزئي حرفيًا، دون حساسية لحالة الأحرف. والرمز * ليس محرف بدل هنا — بل يطابق نجمة حرفية.',
  },
  'dp.t.op.has': {
    en: 'The field is present at all. Carries no value.',
    ar: 'الحقل موجود أصلاً. ولا يحمل قيمة.',
  },
  'dp.t.op.freeText': {
    en: 'Free text against the payload. A term with no field is a payload search.',
    ar: 'نص حر مقابل الحمولة. وأي مصطلح بلا حقل يُعدّ بحثًا في الحمولة.',
  },
  'dp.t.op.or': {
    en: 'Either. Two terms separated by a space are AND by default.',
    ar: 'أيٌّ منهما. والمصطلحان المفصولان بمسافة يُجمعان بـ«و» افتراضيًا.',
  },
  'dp.t.op.not': {
    en: 'Negation, over a single term or a whole parenthesised group.',
    ar: 'نفي، على مصطلح واحد أو على مجموعة كاملة بين قوسين.',
  },
  'dp.t.op.quote': {
    en: 'Quote a value containing spaces or a closing parenthesis.',
    ar: 'ضع بين علامتَي اقتباس أي قيمة تحتوي مسافات أو قوسًا مغلقًا.',
  },
  'dp.t.ts.nothingQ': { en: 'Nothing shows up in the dashboard', ar: 'لا يظهر شيء في لوحة التحكم' },
  'dp.t.ts.nothingA': {
    en: 'Confirm the DSN matches the right app and environment (top-bar app switcher, then Settings → Environments) and that the ingest gateway is reachable from your client. Watch for POST /api/<environment_id>/envelope in the Network tab.',
    ar: 'تأكد من أن عنوان DSN يطابق التطبيق والبيئة الصحيحين (مبدّل التطبيقات في الشريط العلوي، ثم الإعدادات ← البيئات)، ومن أن بوابة الاستقبال يمكن الوصول إليها من عميلك. وراقب طلب POST /api/<environment_id>/envelope في تبويب الشبكة.',
  },
  'dp.t.ts.authA': {
    en: 'The public key is wrong or was rotated. Copy the current DSN from Settings → Environments. (The Flutter SDK disables itself after a 401/403.)',
    ar: 'المفتاح العام خاطئ أو جرى تدويره. انسخ عنوان DSN الحالي من الإعدادات ← البيئات. (توقف حزمة Flutter نفسها بعد استجابة 401 أو 403.)',
  },
  'dp.t.ts.noPersonQ': {
    en: 'Events arrive but there is no person',
    ar: 'تصل الأحداث لكن لا يوجد شخص مرتبط بها',
  },
  'dp.t.ts.noPersonA': {
    en: 'Call identify() before track() so events attach to a user.',
    ar: 'استدعِ identify() قبل track() كي تُرفق الأحداث بمستخدم.',
  },
  'dp.t.ts.fewerQ': { en: 'Fewer errors than expected', ar: 'أخطاء أقل من المتوقع' },
  'dp.t.ts.fewerA': {
    en: 'Errors are sampled by sampleRate (default 1 = all). Lower values drop a fraction on the client.',
    ar: 'تُؤخذ الأخطاء بالعيّنة وفق sampleRate (والافتراضي 1 = الكل). والقيم الأقل تُسقط نسبةً منها على العميل.',
  },

  // --- SDK quickstart step captions ---------------------------------------
  // Passed positionally into the `step` snippet, so they live in the markup as
  // bare arguments rather than as text nodes — another shape the leak test's
  // text-node pattern does not reach.
  'dp.s.install': { en: 'Install the SDK', ar: 'ثبّت حزمة التطوير' },
  'dp.s.installPackage': { en: 'Install the package', ar: 'ثبّت الحزمة' },
  'dp.s.addDependency': { en: 'Add the dependency', ar: 'أضف الاعتمادية' },
  'dp.s.fullExample': { en: 'Full example', ar: 'مثال كامل' },
  'dp.s.initStartup': { en: 'Initialize once at startup', ar: 'هيّئ مرة واحدة عند الإقلاع' },
  'dp.s.initWeb': {
    en: 'Call before your app renders — auto-instrumentation binds immediately.',
    ar: 'استدعِ ذلك قبل عرض تطبيقك — إذ يرتبط التتبّع التلقائي فورًا.',
  },
  'dp.s.initPython': {
    en: 'Call init() during boot — a missing DSN is a no-op, not a crash.',
    ar: 'استدعِ init() أثناء الإقلاع — وغياب عنوان DSN لا يفعل شيئًا ولا يسبّب عطلاً.',
  },
  'dp.s.initCsharp': {
    en: 'Call Init() during boot — a missing DSN is a no-op, not a crash.',
    ar: 'استدعِ Init() أثناء الإقلاع — وغياب عنوان DSN لا يفعل شيئًا ولا يسبّب عطلاً.',
  },
  'dp.s.initFlutter': { en: 'Initialize with appRunner', ar: 'التهيئة عبر appRunner' },
  'dp.s.initFlutterBody': {
    en: 'appRunner launches your app inside runZonedGuarded with all capture layers bound.',
    ar: 'يشغّل appRunner تطبيقك داخل runZonedGuarded مع ربط كل طبقات الالتقاط.',
  },
  'dp.s.captureErrors': { en: 'Capture errors', ar: 'التقاط الأخطاء' },
  'dp.s.captureWeb': {
    en: 'Uncaught errors are automatic; report handled ones explicitly.',
    ar: 'تُلتقط الأخطاء غير المعالَجة تلقائيًا؛ أما المعالَجة فأبلغ عنها صراحةً.',
  },
  'dp.s.captureFlutter': {
    en: 'All four Dart error layers are automatic; report handled ones explicitly.',
    ar: 'طبقات أخطاء Dart الأربع كلها تلقائية؛ أما المعالَجة فأبلغ عنها صراحةً.',
  },
  'dp.s.captureExceptions': { en: 'Capture exceptions', ar: 'التقاط الاستثناءات' },
  'dp.s.captureServerPy': {
    en: 'Server SDKs are explicit — report handled exceptions with their traceback.',
    ar: 'حزم الخوادم صريحة — أبلغ عن الاستثناءات المعالَجة مع أثر التتبّع الخاص بها.',
  },
  'dp.s.captureServerCs': {
    en: 'Server SDKs are explicit — report handled exceptions with their stack.',
    ar: 'حزم الخوادم صريحة — أبلغ عن الاستثناءات المعالَجة مع مكدسها.',
  },
  'dp.s.navBreadcrumbs': {
    en: 'Automatic navigation breadcrumbs',
    ar: 'خطوات تتبّع تلقائية للتنقّل',
  },
  'dp.s.navBreadcrumbsBody': {
    en: 'Add the observer to record route changes.',
    ar: 'أضف المراقب لتسجيل تغيّرات المسارات.',
  },
  'dp.s.trackEvents': { en: 'Track product events', ar: 'تتبّع أحداث المنتج' },
  'dp.s.trackWeb': {
    en: 'Identify the user, then record events.',
    ar: 'عرّف المستخدم، ثم سجّل الأحداث.',
  },
  'dp.s.trackPy': {
    en: 'distinct_id is required — it ties the event to a person.',
    ar: 'الحقل distinct_id مطلوب — فهو يربط الحدث بشخص.',
  },
  'dp.s.trackCs': {
    en: 'distinctId is required — it ties the event to a person.',
    ar: 'الحقل distinctId مطلوب — فهو يربط الحدث بشخص.',
  },
} as const satisfies Record<string, Message>;
