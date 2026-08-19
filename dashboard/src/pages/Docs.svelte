<script lang="ts">
  import { t } from '../lib/i18n';
  import { onMount } from 'svelte';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon, { type IconName } from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { buildDsn, appTypeIcon, appTypeLabel } from '../lib/utils/format';
  import { fetchSchema, type SchemaDefinition } from '../lib/api/schema';

  type Platform = 'web' | 'flutter' | 'python' | 'node' | 'csharp';

  const app = $derived(sessionStore.currentApp);
  const hasApp = $derived(!!app);

  // The DSN lives on the app's default environment, not the app itself.
  //
  // Read straight off the store rather than re-fetching: `sessionStore.environments`
  // IS `listEnvironments(currentAppId)` — the store owns that call and Topbar's
  // self-heal effect (Topbar.svelte:58-61) guarantees it runs for the selected
  // app, which this page always mounts alongside via AppShell. The private copy
  // this page used to keep meant a second identical request on every app switch.
  //
  // Deriving also retires the whole out-of-order-response guard the fetch needed:
  // `setApp` clears `environments` to `[]` synchronously before reloading it
  // (session.svelte.ts:345-347), so on a switch this collapses to `null` in the
  // same tick as the app name and type badge. The hazard the old comment
  // described — the NEW app's name beside the OLD app's DSN, a well-formed and
  // copyable credential pointing at the wrong app — is therefore structurally
  // unreachable now rather than defended against after the fact.
  const defaultEnv = $derived(
    sessionStore.environments.find((e) => e.is_default) ?? sessionStore.environments[0] ?? null,
  );

  // Every snippet is filled in with the selected environment's DSN so it's
  // copy-paste ready. Falls back to an obvious placeholder when no app is
  // selected yet, or while the environment fetch is still in flight.
  const dsn = $derived(
    defaultEnv
      ? buildDsn(defaultEnv.public_key, defaultEnv.id)
      : buildDsn('pk_your_public_key', '<ENVIRONMENT_ID>'),
  );

  // Default the platform tab to the current app's SDK; a manual pick wins after.
  let picked = $state<Platform | null>(null);
  // app_type → docs tab. Anything without a dedicated guide falls back to Web.
  const DEFAULT_BY_APP_TYPE: Record<string, Platform> = {
    flutter: 'flutter',
    python: 'python',
    node: 'node',
    csharp: 'csharp',
  };
  const platform = $derived<Platform>(
    picked ?? (app ? (DEFAULT_BY_APP_TYPE[app.app_type] ?? 'web') : 'web'),
  );

  // Language label passed to <CodeBlock> per platform.
  const LANG_BY_PLATFORM: Record<Platform, string> = {
    web: 'ts',
    flutter: 'dart',
    python: 'python',
    node: 'ts',
    csharp: 'csharp',
  };
  const lang = $derived(LANG_BY_PLATFORM[platform]);

  // --- snippets (derived so the DSN stays live) ----------------------------

  const webInstall = 'npm install @edraj/sauron-browser';

  const webInit = $derived(`import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: '${dsn}',
  release: 'web@1.0.0', // ties errors to a version
});`);

  const webCapture = `// Uncaught errors + unhandled promise rejections are captured automatically.

// Report a handled error yourself:
try {
  await checkout();
} catch (err) {
  Sauron.captureException(err);
}

// …or a plain message with a level:
Sauron.captureMessage('Payment retried', 'warning');`;

  const webAnalytics = `// Associate the session with a user…
Sauron.identify('u_123', { plan: 'pro', email: 'ada@example.com' });

// …then record product events:
Sauron.track('checkout_completed', { cart_value: 42.5, currency: 'USD' });`;

  const webFull = $derived(`import { Sauron } from '@edraj/sauron-browser';

Sauron.init({
  dsn: '${dsn}',
  release: 'web@1.0.0',
  sampleRate: 1,
  beforeSend(item) {
    // PII escape hatch — return null to drop the event.
    return item;
  },
});

Sauron.identify(user.id, { plan: user.plan });

document.querySelector('#buy')?.addEventListener('click', () => {
  Sauron.track('cta_clicked', { id: 'buy' });
});`);

  const flutterInstall = `# pubspec.yaml
dependencies:
  sauron_flutter:
    path: ../sdks/flutter # or a git / hosted ref

# then
flutter pub get`;

  const flutterInit = $derived(`import 'package:flutter/widgets.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

Future<void> main() async {
  await Sauron.init(
    SauronOptions(
      dsn: '${dsn}',
      release: 'app@1.0.0+1',
    ),
    appRunner: () => runApp(const MyApp()),
  );
}`);

  const flutterCapture = `// All four Flutter/Dart layers are captured automatically (FlutterError,
// PlatformDispatcher, isolates, and the outer runZonedGuarded zone).

// Report a handled error yourself:
try {
  await checkout();
} catch (err, stack) {
  Sauron.captureException(err, stackTrace: stack);
}`;

  const flutterNav = `MaterialApp(
  navigatorObservers: [SauronNavigatorObserver(Sauron.client!)],
  home: const HomePage(),
);`;

  const flutterAnalytics = `Sauron.identify('u_123', traits: {'plan': 'pro'});
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});`;

  const flutterFull = $derived(`import 'package:flutter/material.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

Future<void> main() async {
  await Sauron.init(
    SauronOptions(
      dsn: '${dsn}',
      release: 'app@1.0.0+1',
      sampleRate: 1.0,
    ),
    appRunner: () => runApp(const MyApp()),
  );
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      navigatorObservers: [SauronNavigatorObserver(Sauron.client!)],
      home: const HomePage(),
    );
  }
}`);

  // --- Python (server) — sauron-sdk ----------------------------------------

  const pyInstall = 'pip install sauron-sdk';

  const pyInit = $derived(`import sauron

sauron.init(
    dsn="${dsn}",
    release="api@1.0.0",  # ties errors to a version
)`);

  const pyCapture = `# Report a handled exception (reads the active traceback):
try:
    charge(order)
except Exception as exc:
    sauron.capture_exception(exc)

# …or a plain message with a level:
sauron.capture_message("Payment retried", level="warning")

# Flush the background worker before the process exits:
sauron.close()`;

  const pyAnalytics = `# distinct_id is required — it attributes the event to a person.
sauron.identify("u_123", traits={"plan": "pro"})

sauron.track(
    "checkout_completed",
    distinct_id="u_123",
    properties={"cart_value": 42.5, "currency": "USD"},
)`;

  // --- Node (server) — @edraj/sauron-node ----------------------------------------

  const nodeInstall = 'npm install @edraj/sauron-node';

  const nodeInit = $derived(`import { Sauron } from '@edraj/sauron-node';

Sauron.init({
  dsn: '${dsn}',
  release: 'api@1.0.0',
});`);

  const nodeCapture = `// Report a handled exception:
try {
  await charge(order);
} catch (err) {
  Sauron.captureException(err);
}

// …or a plain message with a level:
Sauron.captureMessage('Payment retried', 'warning');

// Flush the background queue before the process exits:
await Sauron.close();`;

  const nodeAnalytics = `// distinctId is required — it attributes the event to a person.
Sauron.identify('u_123', { plan: 'pro' });

Sauron.track('checkout_completed', 'u_123', { cart_value: 42.5, currency: 'USD' });`;

  // --- C# (server) — Sauron ------------------------------------------------

  const csharpInstall = 'dotnet add package Sauron';

  const csharpInit = $derived(`using Sauron;

SauronSdk.Init(new SauronOptions
{
    Dsn = "${dsn}",
    Release = "api@1.0.0",
});`);

  const csharpCapture = `// Report a handled exception:
try
{
    Charge(order);
}
catch (Exception ex)
{
    SauronSdk.CaptureException(ex);
}

// …or a plain message with a level:
SauronSdk.CaptureMessage("Payment retried", "warning");

// Flush the background queue before the process exits:
SauronSdk.Close();`;

  const csharpAnalytics = `// distinctId is required — it attributes the event to a person.
SauronSdk.Identify("u_123", new Dictionary<string, object> { ["plan"] = "pro" });

SauronSdk.Track("checkout_completed", "u_123",
    new Dictionary<string, object> { ["cart_value"] = 42.5, ["currency"] = "USD" });`;

  const verifyByPlatform: Record<Platform, string> = {
    web: "Sauron.captureMessage('Sauron test event');",
    flutter: "Sauron.captureMessage('Sauron test event');",
    python: 'sauron.capture_message("Sauron test event")',
    node: "Sauron.captureMessage('Sauron test event');",
    csharp: 'SauronSdk.CaptureMessage("Sauron test event");',
  };
  const verifySnippet = $derived(verifyByPlatform[platform]);

  const webFunnel = `// Emit one event per funnel stage, using stable names.
Sauron.identify(user.id); // stitch the steps to one person

Sauron.track('signup_started');
Sauron.track('signup_email_verified');
Sauron.track('signup_completed', { plan: 'pro' });`;

  const flutterFunnel = `// Emit one event per funnel stage, using stable names.
Sauron.identify(user.id); // stitch the steps to one person

Sauron.track('signup_started');
Sauron.track('signup_email_verified');
Sauron.track('signup_completed', properties: {'plan': 'pro'});`;

  const pyFunnel = `# Emit one event per funnel stage, using stable names.
sauron.identify("u_123")  # stitch the steps to one person

sauron.track("signup_started", distinct_id="u_123")
sauron.track("signup_email_verified", distinct_id="u_123")
sauron.track("signup_completed", distinct_id="u_123", properties={"plan": "pro"})`;

  const nodeFunnel = `// Emit one event per funnel stage, using stable names.
Sauron.identify('u_123'); // stitch the steps to one person

Sauron.track('signup_started', 'u_123');
Sauron.track('signup_email_verified', 'u_123');
Sauron.track('signup_completed', 'u_123', { plan: 'pro' });`;

  const csharpFunnel = `// Emit one event per funnel stage, using stable names.
SauronSdk.Identify("u_123"); // stitch the steps to one person

SauronSdk.Track("signup_started", "u_123");
SauronSdk.Track("signup_email_verified", "u_123");
SauronSdk.Track("signup_completed", "u_123");`;

  const funnelByPlatform: Record<Platform, string> = {
    web: webFunnel,
    flutter: flutterFunnel,
    python: pyFunnel,
    node: nodeFunnel,
    csharp: csharpFunnel,
  };
  const funnelSnippet = $derived(funnelByPlatform[platform]);

  // --- API reference tables ------------------------------------------------

  const webApi: { sig: string; desc: string }[] = [
    { sig: 'init(options)', desc: 'Initialize the SDK (idempotent).' },
    { sig: 'captureException(err, hint?)', desc: 'Report an exception or any thrown value.' },
    { sig: 'captureMessage(msg, level?)', desc: 'Report a plain message.' },
    { sig: 'track(name, props?)', desc: 'Record a product-analytics event.' },
    { sig: 'identify(id, traits?)', desc: 'Associate the session with a user.' },
    { sig: 'addBreadcrumb(crumb)', desc: 'Manually add a breadcrumb.' },
    { sig: 'setUser(user | null)', desc: 'Set or clear the current user.' },
    { sig: 'flush(timeoutMs?)', desc: 'Send everything pending.' },
    { sig: 'close(timeoutMs?)', desc: 'Flush, then restore patched globals.' },
  ];

  const flutterApi: { sig: string; desc: string }[] = [
    { sig: 'Sauron.init(options, appRunner:)', desc: 'Initialize inside runZonedGuarded.' },
    { sig: 'captureException(error, stackTrace:)', desc: 'Report an error with its stack.' },
    { sig: 'track(name, properties:)', desc: 'Record a product-analytics event.' },
    { sig: 'identify(id, traits:)', desc: 'Associate the session with a user.' },
    { sig: 'addBreadcrumb(Breadcrumb…)', desc: 'Manually add a breadcrumb.' },
    { sig: 'setUser(SauronUser?)', desc: 'Set or clear the current user.' },
    { sig: 'flush() / close()', desc: 'Send pending envelopes / shut down.' },
    { sig: 'SauronNavigatorObserver(client)', desc: 'Automatic navigation breadcrumbs.' },
  ];

  const pythonApi: { sig: string; desc: string }[] = [
    { sig: 'init(dsn, release?, …)', desc: 'Initialize the SDK (no-op when the DSN is missing).' },
    { sig: 'capture_exception(exc, *, level?)', desc: 'Report an exception with its traceback.' },
    { sig: 'capture_message(msg, level?)', desc: 'Report a plain message.' },
    { sig: 'track(event, distinct_id, properties?)', desc: 'Record a product-analytics event.' },
    { sig: 'identify(distinct_id, traits?)', desc: 'Attach traits to a person.' },
    { sig: 'flush(timeout?)', desc: 'Send everything pending.' },
    { sig: 'close(timeout?)', desc: 'Flush, then stop the worker thread.' },
  ];

  const nodeApi: { sig: string; desc: string }[] = [
    { sig: 'init(options)', desc: 'Initialize the SDK (no-op when the DSN is missing).' },
    { sig: 'captureException(err)', desc: 'Report an exception with its stack.' },
    { sig: 'captureMessage(msg, level?)', desc: 'Report a plain message.' },
    { sig: 'track(event, distinctId, properties?)', desc: 'Record a product-analytics event.' },
    { sig: 'identify(distinctId, traits?)', desc: 'Attach traits to a person.' },
    { sig: 'flush(timeoutMs?)', desc: 'Send everything pending.' },
    { sig: 'close(timeoutMs?)', desc: 'Flush, then stop the flush timer.' },
  ];

  const csharpApi: { sig: string; desc: string }[] = [
    { sig: 'SauronSdk.Init(options)', desc: 'Initialize the SDK (no-op when the DSN is missing).' },
    { sig: 'CaptureException(ex)', desc: 'Report an exception with its stack.' },
    { sig: 'CaptureMessage(msg, level?)', desc: 'Report a plain message.' },
    { sig: 'Track(evt, distinctId, properties?)', desc: 'Record a product-analytics event.' },
    { sig: 'Identify(distinctId, traits?)', desc: 'Attach traits to a person.' },
    { sig: 'Flush(timeout?)', desc: 'Send everything pending.' },
    { sig: 'Close(timeout?)', desc: 'Flush, then stop the flush timer.' },
  ];

  // --- Search & filtering ---------------------------------------------------

  const searchCoverageRows: { q: string; a: string }[] = $derived([
    {
      q: t('dp.t.cov.exceptionsQ'),
      a: t('dp.t.coverage.exceptions'),
    },
    {
      q: 'Events',
      a: t('dp.t.coverage.exceptions'),
    },
    {
      q: t('dp.t.cov.occurrencesQ'),
      a: t('dp.t.coverage.occurrences'),
    },
    {
      q: 'Sessions',
      a: t('dp.t.cov.sessionsA'),
    },
    {
      q: t('dp.t.cov.plainQ'),
      a: t('dp.t.cov.plainA'),
    },
    {
      q: 'Funnels',
      a: t('dp.t.cov.clientA'),
    },
    {
      q: t('dp.t.cov.noneQ'),
      a: 'No search.',
    },
  ]);

  /**
   * The operators the grammar actually resolves.
   *
   * Hand-maintained because the grammar is frozen and small; the FIELD lists
   * below are fetched instead, because those change per resource and per
   * release and this page has already carried a rotted copy of them once.
   */
  const queryOperatorRows: { sig: string; desc: string }[] = $derived([
    { sig: 'field:value', desc: t('dp.t.op.equals') },
    { sig: 'field:!value', desc: t('dp.t.op.notEqual') },
    { sig: 'field:>n   field:>=n', desc: t('dp.t.op.greater') },
    { sig: 'field:<n   field:<=n', desc: t('dp.t.op.less') },
    {
      sig: 'field:>2day   field:<1month',
      desc: t('dp.t.op.relative'),
    },
    {
      sig: 'field:>2026-07-01T00:00:00Z',
      desc: t('dp.t.op.iso'),
    },
    { sig: 'field:[a,b]', desc: t('dp.t.op.anyOf') },
    {
      sig: 'field:[lo..hi]',
      desc: t('dp.t.op.range'),
    },
    {
      sig: 'field:~text',
      desc: t('dp.t.op.contains'),
    },
    { sig: 'has:field', desc: t('dp.t.op.has') },
    { sig: 'bare words', desc: t('dp.t.op.freeText') },
    { sig: 'A OR B', desc: t('dp.t.op.or') },
    { sig: '!term   !(a b)', desc: t('dp.t.op.not') },
    { sig: '"two words"', desc: t('dp.t.op.quote') },
  ]);

  /** The variable prefixes, which address JSON rather than a column. */
  const queryVariableRows: { sig: string; desc: string }[] = $derived([
    { sig: '@tag:value', desc: t('dp.t.var.anyTag') },
    { sig: '@tag.key:value', desc: t('dp.t.var.namedKey') },
    {
      sig: 'tag:key=value',
      desc: t('dp.t.var.escapeHatch'),
    },
    { sig: '@context.os.name:Linux', desc: t('dp.t.var.context') },
    { sig: '@extra.key:value', desc: t('dp.t.var.extra') },
    { sig: 'sort=col   sort=-col', desc: t('dp.t.var.sort') },
  ]);

  const queryExample = `level:[error,fatal] @tag.region:eu !status:resolved timeout

  level:[error,fatal]   either level
  @tag.region:eu        the region tag is exactly "eu"
  !status:resolved      anything not resolved
  timeout               ...and "timeout" somewhere in the payload

Terms are ANDed. Wrap alternatives in parentheses to mix in an OR:

  (level:error OR level:fatal) @tag.region:~eu`;

  const freeTextRows: { q: string; a: string }[] = $derived([
    {
      q: 'Exceptions',
      a: t('dp.t.free.exceptions'),
    },
    {
      q: 'Events',
      a: t('dp.t.free.events'),
    },
    {
      q: 'Occurrences',
      a: t('dp.t.free.occurrences'),
    },
    {
      q: 'Users',
      a: t('dp.t.free.users'),
    },
    {
      q: 'Devices',
      a: t('dp.t.free.devices'),
    },
    { q: 'Screens', a: t('dp.t.free.screens') },
  ]);

  const filterOpRows: { sig: string; desc: string }[] = $derived([
    { sig: 'text', desc: t('dp.t.chip.text') },
    { sig: 'enum', desc: t('dp.t.chip.enum') },
    { sig: 'number', desc: t('dp.t.chip.number') },
    { sig: 'tag', desc: t('dp.t.chip.tag') },
  ]);

  /**
   * The searchable resources, each documented from the schema the server
   * actually serves.
   *
   * These lists used to be hardcoded here — and they rotted: they still named
   * `times_seen` and `users_seen` long after the resolver had moved to
   * `timesSeen`/`usersSeen`, and they listed a `tag` field with chip operators
   * that the query language spells differently. A page that documents a
   * catalog must read that catalog.
   */
  const SEARCHABLE = ['issues', 'events', 'occurrences', 'sessions'] as const;

  let searchSchemas = $state<Record<string, SchemaDefinition>>({});
  let searchSchemaError = $state<string | null>(null);

  $effect(() => {
    const id = sessionStore.currentAppId;
    if (!id) return;
    let cancelled = false;
    Promise.all(
      SEARCHABLE.map((ctx) =>
        fetchSchema(id, ctx)
          .then((s) => [ctx, s] as const)
          .catch(() => null),
      ),
    ).then((pairs) => {
      if (cancelled) return;
      const next: Record<string, SchemaDefinition> = {};
      for (const p of pairs) if (p) next[p[0]] = p[1];
      searchSchemas = next;
      searchSchemaError = Object.keys(next).length
        ? null
        : 'Could not load the field list for this app.';
    });
    return () => {
      cancelled = true;
    };
  });

  /** A schema's dimensions as the `apiTable` snippet wants them. */
  function dimensionRows(schema: SchemaDefinition): { sig: string; desc: string }[] {
    return schema.dimensions.map((d) => {
      // Annotated: inferred from `d.type` alone this narrows to the literal
      // union, and the pushes below stop compiling.
      const parts: string[] = [d.type];
      if (d.options?.length) parts.push(d.options.join(', '));
      if (d.aliases?.length) parts.push(`also: ${d.aliases.join(', ')}`);
      return { sig: d.name, desc: `${parts.join(' — ')}   ·   ops ${d.ops.join(' ')}` };
    });
  }

  const searchUrlExample = `#/issues?filter=status:eq:unresolved&filter=culprit:contains:checkout&q=timeout&since_days=30

#/issues?query=status:unresolved culprit:~checkout timeout&since_days=30`;

  const tagFilterExample = `Chip: Tag   contains   key=region   value=eu
→ matches tags.region containing "eu": "eu-central-1", "EU-WEST-2", …

Chip: Tag   =   key=region   value=eu
→ matches ONLY an exact, case-sensitive tags.region of "eu" — zero rows
  if every event actually has "eu-central-1" or "EU-CENTRAL-1"`;

  const troubleshooting: { q: string; a: string }[] = $derived([
    {
      q: t('dp.t.ts.nothingQ'),
      a: t('dp.t.ts.nothingA'),
    },
    {
      q: '401 or 403 responses',
      a: t('dp.t.ts.authA'),
    },
    {
      q: t('dp.t.ts.noPersonQ'),
      a: t('dp.t.ts.noPersonA'),
    },
    {
      q: t('dp.t.ts.fewerQ'),
      a: t('dp.t.ts.fewerA'),
    },
  ]);

  /**
   * Where each SDK is published, so "install the SDK" has somewhere to point.
   *
   * `pkg` is the REGISTRY name and is deliberately not the wire name the
   * envelope header reports (`sauron.javascript`, `sauron.flutter`, …). They
   * differ on every SDK and conflating them has bitten this repo before —
   * never "fix" one to match the other.
   *
   * C# is `null` on purpose rather than a NuGet URL: `Sauron` has never been
   * published there, and a link that 404s is worse than no link. It points at
   * the source instead, which is genuinely where you get it today.
   */
  const sdkRegistry: Record<Platform, { pkg: string; registry: string; url: string }> = {
    web: {
      pkg: '@edraj/sauron-browser',
      registry: 'npm',
      url: 'https://www.npmjs.com/package/@edraj/sauron-browser',
    },
    node: {
      pkg: '@edraj/sauron-node',
      registry: 'npm',
      url: 'https://www.npmjs.com/package/@edraj/sauron-node',
    },
    flutter: {
      pkg: 'sauron_flutter',
      registry: 'pub.dev',
      url: 'https://pub.dev/packages/sauron_flutter',
    },
    python: {
      pkg: 'sauron-sdk',
      registry: 'PyPI',
      url: 'https://pypi.org/project/sauron-sdk/',
    },
    csharp: {
      pkg: 'Sauron',
      registry: 'source',
      url: 'https://github.com/edraj/sauron/tree/main/sdks/csharp',
    },
  };

  // --- worked examples for the "under the hood" sections --------------------
  /**
   * Every one of these is checked against the code it documents, not written
   * from memory: the fingerprint join and precedence are `sauron-core`'s
   * `fingerprint()`, the mask sentinel is `sauron-inspector`'s `MASK_SENTINEL`,
   * and the tier knobs are the `TIER_*` keys `Config::from_env` reads with
   * their real defaults.
   */
  const groupingExample = `// Default: type + the top in-app frames decide the issue.
// These two land in the SAME issue -- the message differs, the frames don't.
throw new TypeError("Cannot read 'id' of undefined")   // user 1
throw new TypeError("Cannot read 'sku' of undefined")  // user 2

// Override when the default splits (or merges) wrongly. The array is
// joined with newlines and hashed, so it is the WHOLE grouping key:
Sauron.captureException(err, {
  fingerprint: ['checkout', 'payment-gateway-timeout'],
});

// One constant string = one issue for every occurrence, forever.
// A value that varies per user (an id, a URL with a query string)
// makes one issue PER USER -- the usual way this goes wrong.`;

  const maskExample = `// Before -- what the SDK sent, sitting in error_events.extra
{ "customer": { "email": "ana@example.com", "plan": "pro" } }

// After masking extra.customer.email
{ "customer": { "email": "****", "plan": "pro" } }

// The KEY survives; only the value is replaced. That is deliberate:
// "this app sends an email here" stays visible to the next audit,
// and a JSON shape your dashboards read does not change underneath
// them. Note the type collapses to a string -- a masked number is
// "****", so anything doing arithmetic on it will stop working.`;

  const tieringExample = `# Hot window: how long signals stay in Postgres. Default 30.
TIER_HOT_DAYS=30

# Where the Parquet lands, laid out app/year/month.
TIER_COLD_PATH=/var/lib/sauron/cold

# Export runs on this interval (seconds). Default 3600.
TIER_TICK_SECS=3600

# Grace between "exported and row counts matched" and "drop the
# Postgres partition". Late-arriving rows land inside this lag.
TIER_DROP_LAG_HOURS=24`;

  // --- in-page navigation --------------------------------------------------
  const sdkNav: { key: Platform; label: string; icon: IconName }[] = [
    { key: 'web', label: 'Web', icon: 'globe' },
    { key: 'flutter', label: 'Flutter', icon: 'smartphone' },
    { key: 'python', label: 'Python', icon: 'braces' },
    { key: 'node', label: 'Node.js', icon: 'server' },
    { key: 'csharp', label: 'C#', icon: 'hash' },
  ];
  const startNav: { id: string; label: string; icon: IconName }[] = $derived([
    { id: 'dsn', label: t('docs.nav.item.dsn'), icon: 'key-round' },
    { id: 'concepts', label: t('docs.nav.item.concepts'), icon: 'compass' },
  ]);
  const guideNav: { id: string; label: string; icon: IconName }[] = $derived([
    { id: 'funnels', label: t('docs.nav.item.funnels'), icon: 'funnel' },
    { id: 'verify', label: t('docs.nav.item.verify'), icon: 'circle-check' },
    { id: 'search', label: t('docs.nav.item.search'), icon: 'search' },
    { id: 'privacy-inspector', label: t('docs.nav.item.privacy'), icon: 'shield-alert' },
    { id: 'troubleshooting', label: t('docs.nav.item.troubleshooting'), icon: 'life-buoy' },
  ]);
  // "How it works under the hood" — every feature + its internals.
  const archNav: { id: string; label: string; icon: IconName }[] = $derived([
    { id: 'architecture', label: t('docs.nav.item.architecture'), icon: 'waypoints' },
    { id: 'grouping', label: t('docs.nav.item.grouping'), icon: 'triangle-alert' },
    { id: 'analytics-internals', label: t('docs.nav.item.analytics'), icon: 'users' },
    { id: 'queries', label: t('docs.nav.item.queries'), icon: 'chart-column' },
    { id: 'tiering', label: t('docs.nav.item.tiering'), icon: 'package' },
    { id: 'uptime', label: t('docs.nav.item.uptime'), icon: 'monitor' },
    { id: 'rbac', label: t('docs.nav.item.rbac'), icon: 'lock' },
    { id: 'sdk-internals', label: t('docs.nav.item.sdkInternals'), icon: 'terminal' },
  ]);
  // Section anchors in document order — drives scroll-spy highlighting.
  const sectionIds = [
    'dsn', 'concepts', 'quickstart', 'funnels', 'verify', 'search', 'privacy-inspector',
    'troubleshooting', 'architecture', 'grouping', 'analytics-internals', 'queries', 'tiering',
    'uptime', 'rbac', 'sdk-internals',
  ];

  // --- "under the hood" content (accurate to the shipped backend) ----------
  const funnelSql = `-- one CTE per step; each must happen at-or-after the previous, per person
s0 AS (SELECT distinct_id, min(occurred_at) AS t
       FROM analytics_events
       WHERE app_id = $1 AND name = 'signup_started'
       GROUP BY distinct_id),
s1 AS (SELECT a.distinct_id, min(a.occurred_at) AS t
       FROM analytics_events a
       JOIN s0 ON s0.distinct_id = a.distinct_id
       WHERE a.name = 'signup_completed' AND a.occurred_at >= s0.t
       GROUP BY a.distinct_id)
-- a step's count = the number of distinct people in its CTE`;

  const dwellSql = `-- time on a screen = gap to the next event in the same session, capped at 30 min
SELECT screen, sum(LEAST(raw_ms, 1800000)) AS total_dwell_ms
FROM (
  SELECT screen, 1000 * EXTRACT(EPOCH FROM (
    LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at)
      - occurred_at
  )) AS raw_ms
  FROM analytics_events
  WHERE session_id IS NOT NULL AND screen IS NOT NULL
) g
WHERE raw_ms IS NOT NULL AND raw_ms > 0   -- a session's last event has no "next"
GROUP BY screen`;

  const percentileSql = `SELECT name, op,
  percentile_cont(0.50) WITHIN GROUP (ORDER BY duration_ms) AS p50,
  percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95,
  count(*) FILTER (WHERE status = 'error' OR http_status >= 500)::float8
    / count(*) AS error_rate
FROM transactions
WHERE app_id = $1 AND op = 'http'
GROUP BY name, op`;

  const fingerprintRows = $derived([
    { q: t('dp.t.fp.override.q'), a: t('dp.t.fp.override.a') },
    { q: t('dp.t.fp.frames.q'), a: t('dp.t.fp.frames.a') },
    { q: t('dp.t.fp.message.q'), a: t('dp.t.fp.message.a') },
  ]);

  // The five-step flow of the Privacy page (Manage → Privacy). Steps 4 and 5
  // are one dialog in the UI but two decisions for the reader, and conflating
  // them is how someone confirms a mask they never read the blast radius of.
  const inspectorRows = [
    {
      q: '1 · Create a policy',
      a: 'Privacy → Policy. Track literal key names (email, phone) — case-insensitive, exact, at any depth; not patterns. A policy sits on a project, an app, or one environment: the most specific one wins whole, and a narrower one subtracts its scope from the parent, which is how you exclude one noisy environment.',
    },
    {
      q: '2 · Run a scan',
      a: 'From the Scans tab, or on the policy schedule. It reads a bounded recent window of the telemetry columns and records where tracked keys appear. Nothing runs until an operator sets INSPECTOR_ENABLED — a scan that stays queued is usually that, not a bug.',
    },
    {
      q: '3 · Review findings',
      a: 'A finding is a location — table, column, JSON path — plus the value type and a shape-only preview. The value itself is never stored. Reveal returns one raw value and writes an audit row before it answers. Detection is best-effort: it greps the JSON text for the quoted key name, so unicode-escaped, base64 or URL-encoded payloads are not found.',
    },
    {
      q: '4 · Preview a mask',
      a: 'The dialog counts the affected rows against a frozen target list, adds the companion columns a mask must also rewrite, and shows every place a mask does not reach. The count expires, so what you confirm is what was counted — read the panel here, not after.',
    },
    {
      q: '5 · Confirm with the app slug',
      a: 'Typing the app slug is what enables the button: the realistic mistake is masking the wrong app, not a mis-click. The pass then rewrites hot rows in batches and the key is enforced on every future event for that app. Cancelling stops it where it is; it does not put anything back.',
    },
  ];

  const presetRows = [
    { q: 'Owner', a: 'All 30 permissions.' },
    { q: 'Admin', a: 'Everything except org:manage.' },
    { q: 'Developer', a: 'Read/write issues, events, funnels, artifacts, source maps and monitors; create and update apps.' },
    { q: 'Viewer', a: 'Read-only across the board.' },
  ];

  const transportRows = [
    { q: 'Batching', a: 'Signals buffer and flush every 5 seconds, or as soon as 30 accumulate — whichever comes first.' },
    { q: 'Compression', a: 'Payloads over 1 KiB are gzipped; the ingest edge transparently decompresses them.' },
    { q: 'Delivery', a: 'Transient failures (429, 5xx, network) retry with exponential backoff and honor Retry-After; 4xx are dropped. A byte-bounded queue rides out short outages, with opt-in disk persistence across restarts.' },
    { q: 'Scope', a: "A process-wide scope plus an isolated per-request scope (AsyncLocalStorage / contextvars / AsyncLocal) so one request's user, tags and breadcrumbs never leak into another." },
  ];
  let activeSection = $state('dsn');

  const prefersReducedMotion = () =>
    typeof window !== 'undefined' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  function scrollToId(id: string) {
    document
      .getElementById(id)
      ?.scrollIntoView({ behavior: prefersReducedMotion() ? 'auto' : 'smooth', block: 'start' });
  }

  function selectSdk(key: Platform) {
    picked = key;
    scrollToId('quickstart');
  }

  onMount(() => {
    const els = sectionIds
      .map((id) => document.getElementById(id))
      .filter((el): el is HTMLElement => el !== null);
    if (els.length === 0) return;
    const io = new IntersectionObserver(
      (entries) => {
        const topMost = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (topMost) activeSection = topMost.target.id;
      },
      { rootMargin: '-72px 0px -65% 0px', threshold: 0 },
    );
    els.forEach((el) => io.observe(el));
    return () => io.disconnect();
  });
</script>

{#snippet step(n: number, title: string, desc: string, code: string, lang: string)}
  <div class="step">
    <div class="step-num">{n}</div>
    <div class="step-body">
      <h3 class="step-title">{title}</h3>
      {#if desc}<p class="muted step-desc">{desc}</p>{/if}
      <CodeBlock {code} language={lang} />
    </div>
  </div>
{/snippet}

{#snippet registryLink(key: Platform)}
  {@const reg = sdkRegistry[key]}
  <a class="reg-link" href={reg.url} target="_blank" rel="noopener noreferrer">
    <Icon name="package" size={13} />
    <code class="mono">{reg.pkg}</code>
    <span class="reg-on">on {reg.registry}</span>
    <Icon name="arrow-up-right" size={12} />
  </a>
{/snippet}

{#snippet apiTable(rows: { sig: string; desc: string }[])}
  <div class="api-list">
    {#each rows as row (row.sig)}
      <div class="api-row">
        <code class="api-sig mono">{row.sig}</code>
        <span class="api-desc muted">{row.desc}</span>
      </div>
    {/each}
  </div>
{/snippet}

{#snippet defRows(rows: { q: string; a: string }[])}
  <div class="tshoot">
    {#each rows as r (r.q)}
      <div class="ts-row">
        <div class="ts-q">{r.q}</div>
        <div class="ts-a muted">{r.a}</div>
      </div>
    {/each}
  </div>
{/snippet}

<AppShell requireProject={false}>
  <div class="docs-page">
    <div class="head">
      <div>
        <h1 class="page-title">{t('docs.title')}</h1>
        <p class="muted sub">
          {t('docs.subtitle')}
        </p>
      </div>
    </div>

    <div class="docs-layout">
      <nav class="docs-nav" aria-label={t('docs.sections')}>
        <div class="nav-group">
          <div class="nav-label">{t('docs.nav.getStarted')}</div>
          <div class="nav-items">
            {#each startNav as n (n.id)}
              <button
                class="nav-item"
                class:active={activeSection === n.id}
                aria-current={activeSection === n.id ? 'true' : undefined}
                onclick={() => scrollToId(n.id)}
              >
                <Icon name={n.icon} size={15} />
                {n.label}
              </button>
            {/each}
          </div>
        </div>
        <div class="nav-group nav-sdks">
          <div class="nav-label">{t('docs.nav.sdks')}</div>
          <div class="nav-items">
            {#each sdkNav as s (s.key)}
              <button
                class="nav-item"
                class:active={platform === s.key}
                aria-current={platform === s.key ? 'true' : undefined}
                onclick={() => selectSdk(s.key)}
              >
                <Icon name={s.icon} size={15} />
                {s.label}
              </button>
            {/each}
          </div>
        </div>
        <div class="nav-group">
          <div class="nav-label">{t('docs.nav.guides')}</div>
          <div class="nav-items">
            {#each guideNav as g (g.id)}
              <button
                class="nav-item"
                class:active={activeSection === g.id}
                aria-current={activeSection === g.id ? 'true' : undefined}
                onclick={() => scrollToId(g.id)}
              >
                <Icon name={g.icon} size={15} />
                {g.label}
              </button>
            {/each}
          </div>
        </div>
        <div class="nav-group">
          <div class="nav-label">{t('docs.nav.underTheHood')}</div>
          <div class="nav-items">
            {#each archNav as a (a.id)}
              <button
                class="nav-item"
                class:active={activeSection === a.id}
                aria-current={activeSection === a.id ? 'true' : undefined}
                onclick={() => scrollToId(a.id)}
              >
                <Icon name={a.icon} size={15} />
                {a.label}
              </button>
            {/each}
          </div>
        </div>
      </nav>

      <div class="doc">
        <!-- DSN context -->
        <section id="dsn" class="doc-sec">
        <Card class="dsn-card">
      {#if hasApp && app}
        <div class="dsn-top">
          <span class="app-chip">
            <Icon name={appTypeIcon(app.app_type)} size={15} />
            {app.name}
          </span>
          <Badge tone="neutral" size="sm">{appTypeLabel(app.app_type)}</Badge>
          <span class="dsn-note muted">{t('docs.snippetsUseDsn')}</span>
        </div>
        <div class="dsn-row">
          <code class="dsn mono">{dsn}</code>
          <CopyButton value={dsn} />
        </div>
        <p class="muted dsn-env-hint">
          {t('docs.dsnForApp')} <strong>{defaultEnv?.name ?? 'default'}</strong> {t('dp.dsn.perEnv')} <a href="#/admin/environments">{t('notif.column.environments')}</a>.
        </p>
      {:else}
        <div class="dsn-empty">
          <span class="ic"><Icon name="key-round" size={18} /></span>
          <p class="muted">
            {t('docs.snippetsPlaceholder')}
            <a href="#/admin/projects">{t('docs.step.createApp')}</a> {t('dp.dsn.autofill')}
          </p>
        </div>
      {/if}
    </Card>

        </section>

        <!-- How it works -->
        <section id="concepts" class="doc-sec">
        <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="compass" size={16} /><h3>{t('docs.nav.howItWorks')}</h3></div>
      {/snippet}
      <div class="hierarchy">
        <span class="node">{t('storage.column.org')}</span>
        <Icon name="chevron-right" size={14} />
        <span class="node">{t('storage.column.project')}</span>
        <Icon name="chevron-right" size={14} />
        <span class="node">{t('nav.selectApp')}</span>
        <Icon name="chevron-right" size={14} />
        <span class="node">{t('nav.env')}</span>
        <Icon name="chevron-right" size={14} />
        <span class="node key">DSN</span>
      </div>
      <p class="muted concept-lead">
        An <b>environment</b> {t('dp.concepts.envHoldsDsn')}
      </p>
      <div class="signals">
        <div class="signal">
          <span class="s-ic err"><Icon name="triangle-alert" size={16} /></span>
          <div>
            <b>{t('docs.errorsToExceptions')}</b>
            <span class="muted">{t('docs.stackTracedGrouped')}</span>
          </div>
        </div>
        <div class="signal">
          <span class="s-ic ana"><Icon name="chart-column" size={16} /></span>
          <div>
            <b>{t('docs.eventsToAnalytics')}</b>
            <span class="muted">{t('dp.concepts.trackIdentify')}</span>
          </div>
        </div>
      </div>
    </Card>

        </section>

        <!-- SDK quickstart -->
        <section id="quickstart" class="doc-sec">
    {#if platform === 'web'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="globe" size={16} /><h3>{t('docs.quickstart.web')}</h3></div>
        {/snippet}
        {#snippet actions()}{@render registryLink('web')}{/snippet}
        <div class="steps">
          {@render step(1, t('dp.s.install'), '', webInstall, 'bash')}
          {@render step(
            2,
            t('dp.s.initStartup'),
            t('dp.s.initWeb'),
            webInit,
            'ts',
          )}
          {@render step(
            3,
            t('dp.s.captureErrors'),
            t('dp.s.captureWeb'),
            webCapture,
            'ts',
          )}
          {@render step(
            4,
            t('dp.s.trackEvents'),
            t('dp.s.trackWeb'),
            webAnalytics,
            'ts',
          )}
          {@render step(5, t('dp.s.fullExample'), '', webFull, 'ts')}
        </div>
      </Card>

      <Card title={t('docs.apiReference', { language: 'Web' })}>
        {@render apiTable(webApi)}
      </Card>
    {:else if platform === 'flutter'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="smartphone" size={16} /><h3>{t('docs.quickstart.flutter')}</h3></div>
        {/snippet}
        {#snippet actions()}{@render registryLink('flutter')}{/snippet}
        <div class="steps">
          {@render step(1, t('dp.s.addDependency'), '', flutterInstall, 'yaml')}
          {@render step(
            2,
            t('dp.s.initFlutter'),
            t('dp.s.initFlutterBody'),
            flutterInit,
            'dart',
          )}
          {@render step(
            3,
            t('dp.s.captureErrors'),
            t('dp.s.captureFlutter'),
            flutterCapture,
            'dart',
          )}
          {@render step(
            4,
            t('dp.s.navBreadcrumbs'),
            t('dp.s.navBreadcrumbsBody'),
            flutterNav,
            'dart',
          )}
          {@render step(
            5,
            t('dp.s.trackEvents'),
            t('dp.s.trackWeb'),
            flutterAnalytics,
            'dart',
          )}
          {@render step(6, t('dp.s.fullExample'), '', flutterFull, 'dart')}
        </div>
      </Card>

      <Card title={t('docs.apiReference', { language: 'Flutter' })}>
        {@render apiTable(flutterApi)}
      </Card>
    {:else if platform === 'python'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="braces" size={16} /><h3>{t('docs.quickstart.python')}</h3></div>
        {/snippet}
        {#snippet actions()}{@render registryLink('python')}{/snippet}
        <div class="steps">
          {@render step(1, t('dp.s.install'), '', pyInstall, 'bash')}
          {@render step(
            2,
            t('dp.s.initStartup'),
            t('dp.s.initPython'),
            pyInit,
            'python',
          )}
          {@render step(
            3,
            t('dp.s.captureExceptions'),
            t('dp.s.captureServerPy'),
            pyCapture,
            'python',
          )}
          {@render step(
            4,
            t('dp.s.trackEvents'),
            t('dp.s.trackPy'),
            pyAnalytics,
            'python',
          )}
        </div>
      </Card>

      <Card title={t('docs.apiReference', { language: 'Python' })}>
        {@render apiTable(pythonApi)}
      </Card>
    {:else if platform === 'node'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="server" size={16} /><h3>{t('docs.quickstart.node')}</h3></div>
        {/snippet}
        {#snippet actions()}{@render registryLink('node')}{/snippet}
        <div class="steps">
          {@render step(1, t('dp.s.install'), '', nodeInstall, 'bash')}
          {@render step(
            2,
            t('dp.s.initStartup'),
            t('dp.s.initPython'),
            nodeInit,
            'ts',
          )}
          {@render step(
            3,
            t('dp.s.captureExceptions'),
            t('dp.s.captureServerCs'),
            nodeCapture,
            'ts',
          )}
          {@render step(
            4,
            t('dp.s.trackEvents'),
            t('dp.s.trackCs'),
            nodeAnalytics,
            'ts',
          )}
        </div>
      </Card>

      <Card title={t('docs.apiReference', { language: 'Node.js' })}>
        {@render apiTable(nodeApi)}
      </Card>
    {:else}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="hash" size={16} /><h3>{t('docs.quickstart.csharp')}</h3></div>
        {/snippet}
        {#snippet actions()}{@render registryLink('csharp')}{/snippet}
        <div class="steps">
          {@render step(1, t('dp.s.installPackage'), '', csharpInstall, 'bash')}
          {@render step(
            2,
            t('dp.s.initStartup'),
            t('dp.s.initCsharp'),
            csharpInit,
            'csharp',
          )}
          {@render step(
            3,
            t('dp.s.captureExceptions'),
            t('dp.s.captureServerCs'),
            csharpCapture,
            'csharp',
          )}
          {@render step(
            4,
            t('dp.s.trackEvents'),
            t('dp.s.trackCs'),
            csharpAnalytics,
            'csharp',
          )}
        </div>
      </Card>

      <Card title={t('docs.apiReference', { language: 'C#' })}>
        {@render apiTable(csharpApi)}
      </Card>
    {/if}

        </section>

        <!-- Funnels -->
        <section id="funnels" class="doc-sec">
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="funnel" size={16} /><h3>{t('docs.analytics.buildFunnel')}</h3></div>
      {/snippet}
      <p class="muted verify-lead">
        {t('docs.analytics.funnelIs')} <b>{t('dp.funnels.eventNames')}</b> {t('dp.r.funnelsSendWith')}
        <code class="ic">track()</code>{t('dp.r.funnelsMeasures')}
      </p>
      <CodeBlock code={funnelSnippet} language={lang} />
      <ol class="mini-steps">
        <li>{t('projects.open')} <a href="#/funnels">{t('funnels.title')}</a> {t('dp.funnels.addStages')} <b>in order</b> (2–10 steps).</li>
        <li>{t('docs.analytics.pickRange')}</li>
        <li><b>{t('docs.analytics.compute')}</b> {t('dp.funnels.seeConversion')}</li>
      </ol>
      <p class="faint fine">
        {t('docs.analytics.stepOrder.a')} <code class="ic">identify()</code>
        {t('docs.analytics.stepOrder.b')}
      </p>
    </Card>

        </section>

        <!-- Verify -->
        <section id="verify" class="doc-sec">
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="circle-check" size={16} /><h3>{t('docs.step.verify')}</h3></div>
      {/snippet}
      <p class="muted verify-lead">
        {t('docs.fireTestEvent')}
      </p>
      <CodeBlock code={verifySnippet} language={lang} />
      <div class="verify-links">
        <a class="vl" href="#/issues"><Icon name="triangle-alert" size={15} /> {t('issues.title')}</a>
        <a class="vl" href="#/events"><Icon name="diamond" size={15} /> {t('overview.stat.events')}</a>
      </div>
    </Card>

        </section>

        <!-- Search & filtering -->
        <section id="search" class="doc-sec">
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="search" size={16} /><h3>{t('docs.nav.searchFiltering')}</h3></div>
      {/snippet}
      <p class="muted concept-lead">
        {t('docs.search.fourBiggestLists')} <b>{t('issues.title')}</b>, <b>{t('overview.stat.events')}</b>, an issue's <b>{t('issues.occurrences')}</b>
        and <b>{t('overview.stat.sessions')}</b> {t('dp.search.takeReal')} <b>{t('dp.search.queryLanguage')}</b>:
        <code class="ic">level:error @tag.region:eu timeout</code>{t('dp.r.boxAutocompletes')} <b>{t('docs.search.filterChips')}</b> (<code class="ic">field · operator ·
        value</code>{t('dp.r.chipsBeside')}
      </p>

      <h4 class="q-h">{t('docs.search.whereEachPage')}</h4>
      {@render defRows(searchCoverageRows)}

      <h4 class="q-h">{t('docs.search.operators')}</h4>
      <p class="muted q-note">
        {t('docs.frag.pressArrow')} <b>↓</b> {t('dp.search.pressArrow')} <b>Enter</b> (or
        the <b>{t('common.search')}</b> {t('dp.search.runIt')}
      </p>
      {@render apiTable(queryOperatorRows)}

      <h4 class="q-h">{t('docs.search.variables')}</h4>
      <p class="muted q-note">
        {t('docs.search.jsonFields.a')} <code class="ic">tags</code>
        {t('docs.search.jsonFields.b')} <code class="ic">context</code>{t('docs.search.jsonFields.c')}
      </p>
      {@render apiTable(queryVariableRows)}

      <h4 class="q-h">{t('docs.example')}</h4>
      <CodeBlock code={queryExample} language="text" />

      <h4 class="q-h">{t('docs.search.freeText')}</h4>
      <p class="muted q-note">
        {t('docs.search.bareTerm')} <code class="ic">field:</code> {t('dp.r.freeTextIs')} <code class="ic">ILIKE</code>{t('dp.r.noRanking')} <code class="ic">issue:read</code> without <code class="ic">event:read</code>{t('dp.r.payloadWithheld')}
      </p>
      {@render defRows(freeTextRows)}

      <h4 class="q-h">{t('docs.search.structuredFilters')}</h4>
      <p class="muted q-note">
        {t('docs.frag.clickAddFilter')} <b>{t('dp.r.addFilter')}</b> {t('dp.search.buildChip')} <b>AND</b>{t('dp.search.freeTextOnTop')} <b>{t('issues.occurrences')}</b> {t('dp.r.occurrencesList')} <code class="ic">{t('events.tag')}</code> field.
      </p>
      {@render apiTable(filterOpRows)}

      <h4 class="q-h">{t('docs.search.fieldsPerList')}</h4>
      <p class="muted q-note">
        {t('docs.search.readLive')}
      </p>
      {#each SEARCHABLE as ctx (ctx)}
        {#if searchSchemas[ctx]}
          <p class="muted q-note"><b>{ctx}</b>:</p>
          {@render apiTable(dimensionRows(searchSchemas[ctx]))}
        {/if}
      {/each}
      {#if searchSchemaError}
        <p class="faint fine">{searchSchemaError}</p>
      {/if}
      <p class="faint fine">
        {t('docs.search.tagSuggestions')}
      </p>

      <h4 class="q-h">{t('docs.search.example')}</h4>
      <p class="muted q-note">
        {t('docs.search.exampleQuery.a')} <code class="ic">filter=</code>
        {t('docs.search.exampleQuery.b')}
      </p>
      <CodeBlock code={searchUrlExample} language="url" />

      <h4 class="q-h">{t('docs.search.tagChip')}</h4>
      <p class="muted q-note">
        {t('docs.search.thisIsAbout')} <b>chip</b>{t('dp.r.spelledInBox')}
        <code class="ic">@tag.region:~eu</code> {t('dp.r.substringAnd')}
        <code class="ic">@tag.region:eu</code> {t('dp.r.exactTrap')}
      </p>
      <p class="muted concept-lead">
        <code class="ic">{t('events.tag')}</code> {t('dp.search.twoInputChip')} <b>{t('filter.placeholder.key')}</b> and a <b>{t('filter.placeholder.value')}</b> {t('dp.r.composedInto')} <code class="ic">key=value</code> {t('dp.r.filterValueHood')} <b>first</b> <code class="ic">=</code>{t('dp.r.valueContains')}
        <code class="ic">=</code> {t('dp.r.roundTrips')}
        <b>first</b> {t('dp.r.oneDash')} <code class="ic">contains</code> {t('dp.r.byDefault')}
      </p>
      <ul class="mini-steps">
        <li>
          <b>contains</b> <i>(default)</i> {t('dp.r.substringMatch')} <code class="ic">region</code>, value <code class="ic">eu</code> matches
          <code class="ic">eu-central-1</code>, <code class="ic">EU-WEST-2</code>, etc.
        </li>
        <li>
          <b>=</b> — exact, <b>{t('dp.search.caseSensitive')}</b> {t('dp.search.jsonbContainment')}
        </li>
      </ul>
      <p class="faint fine">
        {t('docs.search.realTickets')} <code class="ic">=</code>
        {t('dp.r.partialReturns')} <b>{t('dp.search.zeroRows')}</b> — indistinguishable from "search doesn't work." If a Tag chip comes back empty,
        switch it to <code class="ic">contains</code> {t('dp.r.beforeConcluding')}
        <code class="ic">{t('events.tag')}</code> {t('dp.r.onlyLooksAt')} <code class="ic">tags</code>
        {t('dp.r.mapNever')} <code class="ic">contexts</code>, <code class="ic">extra</code>{t('dp.r.machineOwned')} <code class="ic">context</code> {t('dp.r.singularBlob')}
      </p>
      <CodeBlock code={tagFilterExample} language="text" />

      <h4 class="q-h">{t('docs.search.filtersInUrl')}</h4>
      <p class="muted q-note">
        On <b>{t('issues.title')}</b> and <b>{t('overview.stat.events')}</b>{t('dp.r.addressBar')}<code class="ic">filter=field:op:value</code>{t('dp.r.repeatedPlus')}
        <code class="ic">q=</code> and <code class="ic">since_days=</code>{t('dp.r.copyUrl')}
      </p>
    </Card>

        </section>

        <!-- Privacy inspector -->
        <section id="privacy-inspector" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h">
                <Icon name="shield-alert" size={16} /><h3>{t('inspector.title')}</h3>
              </div>
            {/snippet}
            <p class="muted concept-lead">
              <b>{t('docs.rbac.managePrivacy')}</b> {t('dp.privacy.finds')} <b>{t('dp.privacy.noSecondCopy')}</b>{t('dp.r.masksInHot')}
              <code class="ic">pii:read</code>{t('dp.r.maskingNeeds')} <code class="ic">pii:manage</code>{t('dp.r.ownerAdminHold')}
            </p>
            {@render defRows(inspectorRows)}
            <h4 class="q-h">{t('docs.lifecycle.whatMaskDoes')}</h4>
            <CodeBlock code={maskExample} language="json" />
            <p class="faint fine">
              {t('docs.lifecycle.notRecoverable')}
              <b>{t('docs.lifecycle.maskingHotOnly')}</b> {t('dp.privacy.twelvePlaces')} <b>{t('dp.privacy.cannotUndo')}</b>{t('dp.privacy.noReverse')}
              <a href="#/active-users">{t('dp.privacy.activeUsers')}</a> {t('dp.privacy.permanently')}
            </p>
          </Card>
        </section>

        <!-- Troubleshooting -->
        <section id="troubleshooting" class="doc-sec">
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="life-buoy" size={16} /><h3>{t('docs.nav.troubleshooting')}</h3></div>
      {/snippet}
      <div class="tshoot">
        {#each troubleshooting as item (item.q)}
          <div class="ts-row">
            <div class="ts-q">{item.q}</div>
            <div class="ts-a muted">{item.a}</div>
          </div>
        {/each}
      </div>
    </Card>

        </section>

        <!-- ===================== Under the hood ===================== -->
        <div class="uth-divider"><span>{t('docs.nav.underTheHood')}</span></div>

        <!-- Architecture -->
        <section id="architecture" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="waypoints" size={16} /><h3>{t('docs.nav.architecture')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.arch.everythingLandsOn')} <b>{t('dp.arch.oneTimeline')}</b> {t('dp.arch.keyedToApp')}
            </p>
            <div class="hierarchy">
              <span class="node">{t('docs.arch.sdkBatch')}</span>
              <Icon name="chevron-right" size={14} />
              <span class="node">{t('docs.arch.ingestEdge')}</span>
              <Icon name="chevron-right" size={14} />
              <span class="node">{t('docs.arch.redisStream')}</span>
              <Icon name="chevron-right" size={14} />
              <span class="node">{t('docs.arch.workers')}</span>
              <Icon name="chevron-right" size={14} />
              <span class="node key">Postgres</span>
            </div>
            <p class="muted concept-lead">
              {t('docs.frag.theEdge')} <b>edge</b> the <code class="ic">X-Sauron-Key</code> {t('dp.r.tenancyFromKey')} <b>{t('dp.arch.onePerItem')}</b> {t('dp.r.ontoRedis')}
              <code class="ic">202</code> {t('dp.r.neverBlocks')}
              <b>{t('docs.arch.workers')}</b> {t('dp.r.workersDrain')}
              <code class="ic">app_id</code> {t('dp.r.sideBySide')}
            </p>
          </Card>
        </section>

        <!-- Error grouping -->
        <section id="grouping" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="triangle-alert" size={16} /><h3>{t('docs.nav.errorGrouping')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.grouping.rawCollapse')} <b>{t('docs.grouping.issues')}</b> by a stable <b>fingerprint</b> {t('dp.grouping.sha256')}
            </p>
            {@render defRows(fingerprintRows)}
            <h4 class="q-h">{t('docs.analytics.inPractice')}</h4>
            <CodeBlock code={groupingExample} language="ts" />
            <p class="faint fine">
              {t('docs.arch.symbolication')}
              <b>Source Map v3</b> (needs a <code class="ic">release</code>{t('dp.r.dartVia')}
              <b>DWARF / addr2line</b> {t('dp.grouping.atIngest')} <b>second</b> {t('dp.grouping.artifactForType')}
              <i>class</i>: the <code class="ic">--save-obfuscation-map</code> {t('dp.r.jsonSameDebugId')} <code class="ic">runtimeType.toString()</code> {t('dp.r.presentational')}
            </p>
          </Card>
        </section>

        <!-- Analytics & people -->
        <section id="analytics-internals" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="users" size={16} /><h3>{t('docs.nav.analyticsPeople')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              <code class="ic">track()</code> {t('dp.r.writesEvents')} <code class="ic">identify()</code>
              {t('dp.r.writesPeople')}
              <b>{t('overview.stat.sessions')}</b> and <b>devices</b> {t('dp.analytics.rollups')}
            </p>
            <div class="signals">
              <div class="signal">
                <span class="s-ic ana"><Icon name="clock" size={16} /></span>
                <div>
                  <b>{t('ui.opModal.session')}</b>
                  <span class="muted"
                    >{t('docs.analytics.sessionKeyed')}</span
                  >
                </div>
              </div>
              <div class="signal">
                <span class="s-ic ana"><Icon name="monitor-smartphone" size={16} /></span>
                <div>
                  <b>{t('ui.section.device')}</b>
                  <span class="muted"
                    >{t('docs.analytics.deviceKeyed')}</span
                  >
                </div>
              </div>
            </div>
            <p class="faint fine">
              {t('docs.arch.breadcrumbs')}
            </p>
          </Card>
        </section>

        <!-- Queries behind it -->
        <section id="queries" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="chart-column" size={16} /><h3>{t('docs.nav.queriesBehind')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.analytics.harderNumbers')} <b>on read</b>{t('dp.analytics.inSql')}
            </p>
            <h4 class="q-h">{t('docs.analytics.funnelsDistinct')}</h4>
            <CodeBlock code={funnelSql} language="sql" />
            <p class="muted q-note">
              {t('docs.analytics.oneCte')} <b>{t('dp.analytics.perPerson')}</b> {t('dp.analytics.atOrAfter')}
            </p>
            <h4 class="q-h">{t('docs.analytics.screenDwell')}</h4>
            <CodeBlock code={dwellSql} language="sql" />
            <p class="muted q-note">
              {t('docs.analytics.dwellFull')} <i>last</i> event (no “next”, so
              <code class="ic">raw_ms</code> {t('dp.r.isNull')} <code class="ic">LEAST(NULL, …)</code>
              {t('dp.r.bogusDwell')}
            </p>
            <h4 class="q-h">{t('docs.analytics.perfPercentiles')}</h4>
            <CodeBlock code={percentileSql} language="sql" />
            <p class="muted q-note">
              <code class="ic">percentile_cont</code> {t('dp.r.smoothPercentiles')}
              <code class="ic">duration_ms</code>{t('dp.r.errorRateShare')} <b>{t('journeys.title')}</b> {t('dp.r.numberSteps')}<code class="ic">row_number</code>{t('dp.r.sankey')}
              <b>DAU/WAU/MAU</b> {t('dp.analytics.dauWauMau')}
            </p>
          </Card>
        </section>

        <!-- Data lifecycle -->
        <section id="tiering" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="package" size={16} /><h3>{t('docs.nav.dataLifecycle')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.lifecycle.signalsStay')} <b>hot</b> {t('dp.tiering.hotThenCold')}
              <b>Parquet</b> {t('dp.tiering.spanBothTiers')}
            </p>
            <p class="muted concept-lead">
              {t('docs.lifecycle.hourlyExportFull')} <b>{t('dp.tiering.verifiesCounts')}</b>{t('dp.tiering.watermark')}
            </p>
            <h4 class="q-h">{t('docs.uptime.knobs')}</h4>
            <CodeBlock code={tieringExample} language="bash" />
          </Card>
        </section>

        <!-- Uptime monitoring -->
        <section id="uptime" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="monitor" size={16} /><h3>{t('docs.nav.uptime')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.uptime.probes')}
            </p>
            <p class="muted concept-lead">
              {t('docs.uptime.claim')}
              <code class="ic">UPDATE … FOR UPDATE SKIP LOCKED</code> {t('dp.r.advancesNextCheck')} <i>before</i> {t('dp.uptime.probing')}
            </p>
            <p class="faint fine">
              {t('docs.uptime.ssrf')}
            </p>
          </Card>
        </section>

        <!-- Access control -->
        <section id="rbac" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="lock" size={16} /><h3>{t('docs.nav.accessControl')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.rbac.fineGrained')} <b>{t('dp.rbac.thirtyPerms')}</b> (<code class="ic">issue:read</code>,
              <code class="ic">funnel:write</code>, <code class="ic">source:read</code>{t('dp.r.bundleInto')} <b>roles</b>{t('dp.rbac.whichAre')} <b>granted</b> {t('dp.rbac.atAScope')} <b>union</b> {t('dp.rbac.cascade')}
            </p>
            {@render defRows(presetRows)}
            <p class="muted concept-lead">
              <b>{t('members.create')}</b> {t('dp.r.createMemberFor')} <code class="ic">member:manage</code> {t('dp.r.suppliesEmail')} <b>{t('dp.rbac.oneRole')}</b>{t('dp.rbac.ticksScopes')} <b>{t('dp.rbac.tempPasswordOnce')}</b>{t('dp.rbac.copyButton')}
            </p>
            <p class="faint fine">
              {t('docs.rbac.tempPasswordFull')} <b>{t('members.grantAccess')}</b> {t('dp.rbac.grantAccessTool')}
            </p>
            <p class="muted concept-lead">
              {t('docs.rbac.memberRowHas')} <b>{t('common.edit')}</b> and <b>{t('members.deactivate')}</b>{t('dp.rbac.editInPlace')} <b>{t('dp.rbac.killSwitch')}</b>:
              every grant stays intact, the row stays listed with a "Deactivated" badge, and
              Reactivate restores normal sign-in. Their sessions are revoked immediately, and any
              access token already issued stops working within a few seconds — every API replica
              refreshes its revoked-session list on the
              <code class="ic">AUTH_REVOCATION_POLL_SECS</code> {t('dp.r.pollInterval')} <code class="ic">org:manage</code> {t('dp.r.refusedExplanation')}
            </p>
            <p class="faint fine">
              {t('docs.rbac.customRoles')} <b>edited</b> {t('dp.rbac.roleInPlace')} <b>{t('dp.rbac.viewOnly')}</b> {t('dp.rbac.resynced')}
            </p>
            <p class="muted concept-lead">
              {t('docs.rbac.everyUserHas')} <b>{t('docs.rbac.account')}</b> {t('dp.rbac.devicesList')}
              <b>{t('docs.rbac.thisDevice')}</b> {t('dp.rbac.cannotSignOutHere')} <b>{t('docs.rbac.logOut')}</b> {t('dp.rbac.topBarVerb')} <b>{t('docs.rbac.signOutOthers')}</b> {t('dp.rbac.endsEverySession')}
            </p>
            <p class="faint fine">
              {t('docs.rbac.adminHolding')} <code class="ic">member:credential</code> {t('dp.r.canSignOut')}
              <code class="ic">member:manage</code> {t('dp.r.carvedOut')}
            </p>
          </Card>
        </section>

        <!-- SDK internals -->
        <section id="sdk-internals" class="doc-sec">
          <Card>
            {#snippet header()}
              <div class="card-h"><Icon name="terminal" size={16} /><h3>{t('docs.nav.sdkInternals')}</h3></div>
            {/snippet}
            <p class="muted concept-lead">
              {t('docs.arch.sdkBatchNote')}
              <b>envelope</b> {t('dp.sdk.envelopeShape')}
            </p>
            {@render defRows(transportRows)}
            <p class="faint fine">
              {t('docs.fullSurface')}
              <button class="linkish" onclick={() => scrollToId('quickstart')}>{t('docs.quickstarts')}</button>
              above.
            </p>
          </Card>
        </section>

        <!-- Footer links -->
    <div class="foot-links">
      <a class="fl" href="#/admin/settings">
        <span class="fl-ic"><Icon name="settings" size={16} /></span>
        <span class="fl-tx"><b>{t('settings.title')}</b><span class="muted">{t('docs.step.copyDsn')}</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
      <a class="fl" href="#/admin/projects">
        <span class="fl-ic"><Icon name="folders" size={16} /></span>
        <span class="fl-tx"><b>{t('docs.nav.projectsApps')}</b><span class="muted">{t('docs.addPlatform')}</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
      <a class="fl" href="#/overview">
        <span class="fl-ic"><Icon name="layout-dashboard" size={16} /></span>
        <span class="fl-tx"><b>{t('overview.title')}</b><span class="muted">{t('docs.step.seeSignals')}</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
        </div>
      </div>
    </div>
  </div>
</AppShell>

<style>
  .head {
    margin-bottom: 18px;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
    max-width: 640px;
  }
  .docs-page {
    /* Was 1120px, which left a third of a wide screen blank while the
       reference tables and code blocks inside were the things being scrolled.
       Not uncapped to `--content-max` (2200px): a 2200px measure is unreadable
       for the prose paragraphs that sit in the same column. 1560 is the width
       at which the widest reference table stops needing its own scrollbar. */
    max-width: 1560px;
    margin: 0 auto;
  }
  .docs-layout {
    display: grid;
    grid-template-columns: 232px minmax(0, 1fr);
    gap: 44px;
    align-items: start;
  }
  .doc {
    display: flex;
    flex-direction: column;
    /* 22, not 18: each of these children is a full Card with its own border,
       and at 18 a quickstart and the API reference under it read as one
       run-on block. */
    gap: 22px;
    min-width: 0;
  }
  .doc-sec {
    scroll-margin-top: calc(var(--topbar-h) + 16px);
  }

  /* in-page docs nav (sticky table of contents) */
  .docs-nav {
    position: sticky;
    top: calc(var(--topbar-h) + 16px);
    align-self: start;
    max-height: calc(100vh - var(--topbar-h) - 32px);
    overflow-y: auto;
  }
  .nav-group + .nav-group {
    margin-top: 18px;
  }
  .nav-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-faint);
    padding: 0 10px;
    margin-bottom: 6px;
  }
  .nav-items {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    text-align: start;
    padding: 6px 10px;
    border-radius: var(--radius);
    background: transparent;
    border: 1px solid transparent;
    color: var(--text-muted);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.12s ease;
  }
  .nav-item:hover {
    color: var(--text);
    background: var(--surface-2);
  }
  .nav-item.active {
    color: var(--primary);
    background: var(--primary-soft);
    border-color: var(--primary-border);
    font-weight: 600;
  }

  /* card header with a leading icon */
  .card-h {
    display: flex;
    align-items: center;
    gap: 9px;
    color: var(--text-muted);
  }
  .card-h h3 {
    font-size: 14.5px;
    font-weight: 620;
    color: var(--text);
  }
  /* Where this SDK is published. Rendered through Card's `actions` slot, which
     is the header's right-hand cell — putting it in the `header` snippet
     instead lands it in `head-left`, a content-sized flex item where no amount
     of `margin-inline-start: auto` reaches the card's right edge. */
  .reg-link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 12px;
    color: var(--text-muted);
    text-decoration: none;
    white-space: nowrap;
    transition: border-color 0.12s ease, color 0.12s ease;
  }
  .reg-link:hover {
    border-color: var(--primary);
    color: var(--text);
  }
  .reg-link code {
    font-size: 11.5px;
    color: var(--text);
  }
  .reg-on {
    color: var(--text-faint);
  }
  @media (max-width: 640px) {
    /* The package name is the long part of a header that already carries a
       title; below this width it wraps the row instead of sitting beside it. */
    .reg-on {
      display: none;
    }
  }

  /* DSN context card */
  .dsn-top {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-bottom: 12px;
  }
  .app-chip {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-weight: 600;
    font-size: 13.5px;
  }
  .dsn-note {
    font-size: 12.5px;
    margin-inline-start: auto;
  }
  .dsn-env-hint {
    font-size: 12.5px;
    margin-top: 10px;
  }
  .dsn-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .dsn {
    flex: 1;
    min-width: 0;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12.5px;
    overflow-x: auto;
    white-space: nowrap;
    color: var(--text);
  }
  .dsn-empty {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .dsn-empty .ic {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--text-muted);
    flex-shrink: 0;
  }
  .dsn-empty p {
    font-size: 13px;
  }
  .dsn-empty a {
    color: var(--primary);
  }
  .dsn-empty a:hover {
    text-decoration: underline;
  }

  /* concepts */
  .hierarchy {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-faint);
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  .node {
    padding: 5px 11px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-muted);
  }
  .node.key {
    background: var(--primary-soft);
    border-color: var(--primary-border);
    color: var(--primary);
    font-family: var(--font-mono);
  }
  .concept-lead {
    font-size: 13.5px;
    line-height: 1.55;
  }
  .signals {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    margin-top: 14px;
  }
  .signal {
    display: flex;
    align-items: flex-start;
    gap: 11px;
    padding: 13px 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .signal > div {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
  }
  .signal .muted {
    font-size: 12.5px;
    line-height: 1.45;
  }
  .s-ic {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 8px;
    flex-shrink: 0;
  }
  .s-ic.err {
    background: var(--error-soft);
    color: var(--error);
  }
  .s-ic.ana {
    background: var(--info-soft);
    color: var(--info);
  }

  /* steps */
  .steps {
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .step {
    display: flex;
    gap: 14px;
  }
  .step-num {
    width: 26px;
    height: 26px;
    flex-shrink: 0;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 12.5px;
    font-weight: 680;
  }
  .step-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .step-title {
    font-size: 14px;
    font-weight: 600;
    margin-top: 3px;
  }
  .step-desc {
    font-size: 13px;
    margin-top: -4px;
    line-height: 1.5;
  }

  /* API reference */
  .api-list {
    display: flex;
    flex-direction: column;
  }
  .api-row {
    display: grid;
    /* The signature column tracks the container rather than sitting at a fixed
       320px: on the wider page these rows had a short code column against a
       very long description, which is the ragged look the extra width was
       supposed to fix. */
    grid-template-columns: minmax(240px, 26%) 1fr;
    gap: 20px;
    padding: 12px 2px;
    border-top: 1px solid var(--border);
  }
  .api-row:first-child {
    border-top: none;
  }
  .api-sig {
    font-size: 12.5px;
    color: var(--text);
    word-break: break-word;
  }
  .api-desc {
    font-size: 13px;
  }

  /* inline code (within paragraphs) */
  .ic {
    font-family: var(--font-mono);
    font-size: 0.86em;
    padding: 1px 5px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text);
  }

  /* funnel mini-guide */
  .mini-steps {
    margin: 14px 0 0;
    padding-inline-start: 20px;
    display: flex;
    flex-direction: column;
    gap: 7px;
    font-size: 13.5px;
    color: var(--text-muted);
    line-height: 1.5;
  }
  .mini-steps b {
    color: var(--text);
    font-weight: 600;
  }
  .mini-steps a {
    color: var(--primary);
  }
  .mini-steps a:hover {
    text-decoration: underline;
  }
  .fine {
    font-size: 12.5px;
    line-height: 1.55;
    margin-top: 14px;
  }

  /* verify */
  .verify-lead {
    font-size: 13.5px;
    margin-bottom: 12px;
    line-height: 1.5;
  }
  .verify-links {
    display: flex;
    gap: 10px;
    margin-top: 14px;
  }
  .vl {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 12px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 13px;
    font-weight: 540;
    transition: all 0.13s ease;
  }
  .vl:hover {
    color: var(--text);
    border-color: var(--text-faint);
    background: var(--surface-2);
  }

  /* troubleshooting */
  .tshoot {
    display: flex;
    flex-direction: column;
  }
  .ts-row {
    padding: 12px 0;
    border-top: 1px solid var(--border);
  }
  .ts-row:first-child {
    border-top: none;
    padding-top: 0;
  }
  .ts-q {
    font-size: 13.5px;
    font-weight: 600;
    margin-bottom: 4px;
  }
  .ts-a {
    font-size: 13px;
    line-height: 1.55;
  }

  /* under-the-hood: section divider + query sub-headings + spacing */
  .uth-divider {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 8px;
    color: var(--text-faint);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .uth-divider::before,
  .uth-divider::after {
    content: '';
    height: 1px;
    flex: 1;
    background: var(--border);
  }
  .q-h {
    font-size: 13px;
    font-weight: 620;
    color: var(--text);
    margin: 20px 0 9px;
  }
  .q-h:first-of-type {
    margin-top: 4px;
  }
  .q-note {
    font-size: 12.5px;
    line-height: 1.55;
    margin-top: 10px;
  }
  .doc-sec .concept-lead + .concept-lead,
  .doc-sec .concept-lead + .hierarchy {
    margin-top: 12px;
  }
  .doc-sec .tshoot {
    margin-top: 4px;
  }
  .linkish {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--primary);
    cursor: pointer;
  }
  .linkish:hover {
    text-decoration: underline;
  }

  /* footer links */
  .foot-links {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }
  .fl {
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 14px 15px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    color: var(--text-muted);
    transition: all 0.13s ease;
  }
  .fl:hover {
    border-color: var(--text-faint);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .fl-ic {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border-radius: 8px;
    background: var(--surface-2);
    flex-shrink: 0;
  }
  .fl-tx {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
    flex: 1;
    min-width: 0;
  }
  .fl-tx b {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--text);
  }
  .fl-tx .muted {
    font-size: 12px;
  }

  @media (max-width: 900px) {
    .docs-layout {
      grid-template-columns: 1fr;
      gap: 20px;
    }
    .docs-nav {
      position: static;
      top: auto;
      max-height: none;
      overflow: visible;
    }
    /* On narrow screens the section links fall away; the SDK switcher stays,
       laid out as a horizontal, scrollable chip row above the content. */
    .nav-group:not(.nav-sdks) {
      display: none;
    }
    .nav-sdks .nav-label {
      display: none;
    }
    .nav-sdks .nav-items {
      flex-direction: row;
      gap: 8px;
      overflow-x: auto;
      padding-bottom: 4px;
    }
    .nav-sdks .nav-item {
      flex: 0 0 auto;
      border-color: var(--border-strong);
    }
    .nav-sdks .nav-item.active {
      border-color: var(--primary);
    }
  }

  @media (max-width: 640px) {
    .signals,
    .foot-links {
      grid-template-columns: 1fr;
    }
    .api-row {
      grid-template-columns: 1fr;
      gap: 4px;
    }
    .dsn-note {
      display: none;
    }
  }
</style>
