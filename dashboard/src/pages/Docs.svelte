<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import CodeBlock from '../lib/components/ui/CodeBlock.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { buildDsn, appTypeIcon, appTypeLabel } from '../lib/utils/format';

  type Platform = 'web' | 'flutter' | 'python' | 'node' | 'csharp';

  const app = $derived(sessionStore.currentApp);
  const hasApp = $derived(!!app);

  // Every snippet is filled in with the selected app's DSN so it's copy-paste
  // ready. Falls back to an obvious placeholder when no app is selected yet.
  const dsn = $derived(
    app ? buildDsn(app.public_key, app.id) : buildDsn('pk_your_public_key', '<APP_ID>'),
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

  const webInstall = 'npm install @sauron/browser';

  const webInit = $derived(`import { Sauron } from '@sauron/browser';

Sauron.init({
  dsn: '${dsn}',
  environment: 'production', // e.g. import.meta.env.MODE
  release: 'web@1.0.0',      // ties errors to a version
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

  const webFull = $derived(`import { Sauron } from '@sauron/browser';

Sauron.init({
  dsn: '${dsn}',
  environment: import.meta.env.MODE,
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
  await Sauron.init((o) {
    o.dsn = '${dsn}';
    o.environment = 'production';
    o.release = 'app@1.0.0+1';
  }, appRunner: () => runApp(const MyApp()));
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
  await Sauron.init((o) {
    o.dsn = '${dsn}';
    o.environment = 'production';
    o.release = 'app@1.0.0+1';
    o.sampleRate = 1.0;
  }, appRunner: () => runApp(const MyApp()));
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
    environment="production",  # e.g. os.environ["ENV"]
    release="api@1.0.0",       # ties errors to a version
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

  // --- Node (server) — @sauron/node ----------------------------------------

  const nodeInstall = 'npm install @sauron/node';

  const nodeInit = $derived(`import { Sauron } from '@sauron/node';

Sauron.init({
  dsn: '${dsn}',
  environment: process.env.NODE_ENV, // e.g. 'production'
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
    Environment = "production",
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
    { sig: 'Sauron.init(configure, appRunner:)', desc: 'Initialize inside runZonedGuarded.' },
    { sig: 'captureException(error, stackTrace:)', desc: 'Report an error with its stack.' },
    { sig: 'track(name, properties:)', desc: 'Record a product-analytics event.' },
    { sig: 'identify(id, traits:)', desc: 'Associate the session with a user.' },
    { sig: 'addBreadcrumb(Breadcrumb…)', desc: 'Manually add a breadcrumb.' },
    { sig: 'setUser(SauronUser?)', desc: 'Set or clear the current user.' },
    { sig: 'flush() / close()', desc: 'Send pending envelopes / shut down.' },
    { sig: 'SauronNavigatorObserver(client)', desc: 'Automatic navigation breadcrumbs.' },
  ];

  const pythonApi: { sig: string; desc: string }[] = [
    { sig: 'init(dsn, environment?, release?, …)', desc: 'Initialize the SDK (no-op when the DSN is missing).' },
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

  const troubleshooting: { q: string; a: string }[] = [
    {
      q: 'Nothing shows up in the dashboard',
      a: "Confirm the DSN matches this app (top-bar app switcher or App settings) and that the ingest gateway is reachable from your client. Watch for POST /api/<app_id>/envelope in the Network tab.",
    },
    {
      q: '401 or 403 responses',
      a: 'The public key is wrong or was rotated. Copy the current DSN from App settings. (The Flutter SDK disables itself after a 401/403.)',
    },
    {
      q: 'Events arrive but there is no person',
      a: 'Call identify() before track() so events attach to a user.',
    },
    {
      q: 'Fewer errors than expected',
      a: 'Errors are sampled by sampleRate (default 1 = all). Lower values drop a fraction on the client.',
    },
  ];
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

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">Docs</h1>
      <p class="muted sub">
        Integrate Sauron into your web, mobile, and server apps — install, initialize, capture
        errors, and track product events.
      </p>
    </div>
  </div>

  <div class="doc">
    <!-- DSN context -->
    <Card class="dsn-card">
      {#if hasApp && app}
        <div class="dsn-top">
          <span class="app-chip">
            <Icon name={appTypeIcon(app.app_type)} size={15} />
            {app.name}
          </span>
          <Badge tone="neutral" size="sm">{appTypeLabel(app.app_type)}</Badge>
          <span class="dsn-note muted">Snippets below use this app's DSN.</span>
        </div>
        <div class="dsn-row">
          <code class="dsn mono">{dsn}</code>
          <CopyButton value={dsn} />
        </div>
      {:else}
        <div class="dsn-empty">
          <span class="ic"><Icon name="key-round" size={18} /></span>
          <p class="muted">
            Snippets use a placeholder DSN.
            <a href="#/projects">Create or select an app</a> to auto-fill your real key.
          </p>
        </div>
      {/if}
    </Card>

    <!-- Concepts -->
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="compass" size={16} /><h3>How Sauron works</h3></div>
      {/snippet}
      <div class="hierarchy">
        <span class="node">Org</span>
        <Icon name="chevron-right" size={14} />
        <span class="node">Project</span>
        <Icon name="chevron-right" size={14} />
        <span class="node">App</span>
        <Icon name="chevron-right" size={14} />
        <span class="node key">DSN</span>
      </div>
      <p class="muted concept-lead">
        An <b>app</b> holds a DSN — a public key plus the app id. Your SDK batches, gzips, and posts
        envelopes to the ingest gateway, where the dashboard sorts them into two signal types:
      </p>
      <div class="signals">
        <div class="signal">
          <span class="s-ic err"><Icon name="triangle-alert" size={16} /></span>
          <div>
            <b>Errors → Exceptions</b>
            <span class="muted">Stack-traced and grouped into issues.</span>
          </div>
        </div>
        <div class="signal">
          <span class="s-ic ana"><Icon name="chart-column" size={16} /></span>
          <div>
            <b>Events → Analytics</b>
            <span class="muted">track() / identify() feed Users, Sessions & Funnels.</span>
          </div>
        </div>
      </div>
    </Card>

    <!-- Platform switcher -->
    <div class="seg" role="tablist" aria-label="SDK platform">
      <button
        class="seg-opt"
        class:active={platform === 'web'}
        role="tab"
        aria-selected={platform === 'web'}
        onclick={() => (picked = 'web')}
      >
        <Icon name="globe" size={16} />
        Web
        <span class="seg-sub">@sauron/browser</span>
      </button>
      <button
        class="seg-opt"
        class:active={platform === 'flutter'}
        role="tab"
        aria-selected={platform === 'flutter'}
        onclick={() => (picked = 'flutter')}
      >
        <Icon name="smartphone" size={16} />
        Flutter
        <span class="seg-sub">sauron_flutter</span>
      </button>
      <button
        class="seg-opt"
        class:active={platform === 'python'}
        role="tab"
        aria-selected={platform === 'python'}
        onclick={() => (picked = 'python')}
      >
        <Icon name="braces" size={16} />
        Python
        <span class="seg-sub">sauron-sdk</span>
      </button>
      <button
        class="seg-opt"
        class:active={platform === 'node'}
        role="tab"
        aria-selected={platform === 'node'}
        onclick={() => (picked = 'node')}
      >
        <Icon name="server" size={16} />
        Node.js
        <span class="seg-sub">@sauron/node</span>
      </button>
      <button
        class="seg-opt"
        class:active={platform === 'csharp'}
        role="tab"
        aria-selected={platform === 'csharp'}
        onclick={() => (picked = 'csharp')}
      >
        <Icon name="hash" size={16} />
        C#
        <span class="seg-sub">Sauron</span>
      </button>
    </div>

    <!-- Platform steps -->
    {#if platform === 'web'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="globe" size={16} /><h3>Web quickstart</h3></div>
        {/snippet}
        <div class="steps">
          {@render step(1, 'Install the SDK', '', webInstall, 'bash')}
          {@render step(
            2,
            'Initialize once at startup',
            'Call before your app renders — auto-instrumentation binds immediately.',
            webInit,
            'ts',
          )}
          {@render step(
            3,
            'Capture errors',
            'Uncaught errors are automatic; report handled ones explicitly.',
            webCapture,
            'ts',
          )}
          {@render step(
            4,
            'Track product events',
            'Identify the user, then record events.',
            webAnalytics,
            'ts',
          )}
          {@render step(5, 'Full example', '', webFull, 'ts')}
        </div>
      </Card>

      <Card title="Web API reference">
        {@render apiTable(webApi)}
      </Card>
    {:else if platform === 'flutter'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="smartphone" size={16} /><h3>Flutter quickstart</h3></div>
        {/snippet}
        <div class="steps">
          {@render step(1, 'Add the dependency', '', flutterInstall, 'yaml')}
          {@render step(
            2,
            'Initialize with appRunner',
            'appRunner launches your app inside runZonedGuarded with all capture layers bound.',
            flutterInit,
            'dart',
          )}
          {@render step(
            3,
            'Capture errors',
            'All four Dart error layers are automatic; report handled ones explicitly.',
            flutterCapture,
            'dart',
          )}
          {@render step(
            4,
            'Automatic navigation breadcrumbs',
            'Add the observer to record route changes.',
            flutterNav,
            'dart',
          )}
          {@render step(
            5,
            'Track product events',
            'Identify the user, then record events.',
            flutterAnalytics,
            'dart',
          )}
          {@render step(6, 'Full example', '', flutterFull, 'dart')}
        </div>
      </Card>

      <Card title="Flutter API reference">
        {@render apiTable(flutterApi)}
      </Card>
    {:else if platform === 'python'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="braces" size={16} /><h3>Python quickstart</h3></div>
        {/snippet}
        <div class="steps">
          {@render step(1, 'Install the SDK', '', pyInstall, 'bash')}
          {@render step(
            2,
            'Initialize once at startup',
            'Call init() during boot — a missing DSN is a no-op, not a crash.',
            pyInit,
            'python',
          )}
          {@render step(
            3,
            'Capture exceptions',
            'Server SDKs are explicit — report handled exceptions with their traceback.',
            pyCapture,
            'python',
          )}
          {@render step(
            4,
            'Track product events',
            'distinct_id is required — it ties the event to a person.',
            pyAnalytics,
            'python',
          )}
        </div>
      </Card>

      <Card title="Python API reference">
        {@render apiTable(pythonApi)}
      </Card>
    {:else if platform === 'node'}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="server" size={16} /><h3>Node.js quickstart</h3></div>
        {/snippet}
        <div class="steps">
          {@render step(1, 'Install the SDK', '', nodeInstall, 'bash')}
          {@render step(
            2,
            'Initialize once at startup',
            'Call init() during boot — a missing DSN is a no-op, not a crash.',
            nodeInit,
            'ts',
          )}
          {@render step(
            3,
            'Capture exceptions',
            'Server SDKs are explicit — report handled exceptions with their stack.',
            nodeCapture,
            'ts',
          )}
          {@render step(
            4,
            'Track product events',
            'distinctId is required — it ties the event to a person.',
            nodeAnalytics,
            'ts',
          )}
        </div>
      </Card>

      <Card title="Node.js API reference">
        {@render apiTable(nodeApi)}
      </Card>
    {:else}
      <Card class="steps-card">
        {#snippet header()}
          <div class="card-h"><Icon name="hash" size={16} /><h3>C# quickstart</h3></div>
        {/snippet}
        <div class="steps">
          {@render step(1, 'Install the package', '', csharpInstall, 'bash')}
          {@render step(
            2,
            'Initialize once at startup',
            'Call Init() during boot — a missing DSN is a no-op, not a crash.',
            csharpInit,
            'csharp',
          )}
          {@render step(
            3,
            'Capture exceptions',
            'Server SDKs are explicit — report handled exceptions with their stack.',
            csharpCapture,
            'csharp',
          )}
          {@render step(
            4,
            'Track product events',
            'distinctId is required — it ties the event to a person.',
            csharpAnalytics,
            'csharp',
          )}
        </div>
      </Card>

      <Card title="C# API reference">
        {@render apiTable(csharpApi)}
      </Card>
    {/if}

    <!-- Funnels -->
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="funnel" size={16} /><h3>Build a funnel</h3></div>
      {/snippet}
      <p class="muted verify-lead">
        A funnel is an ordered list of <b>event names</b> you already send with
        <code class="ic">track()</code>. Sauron measures how many distinct people reach each step —
        counted in order, per person — plus the drop-off between them.
      </p>
      <CodeBlock code={funnelSnippet} language={lang} />
      <ol class="mini-steps">
        <li>Open <a href="#/funnels">Funnels</a> and add your stage events <b>in order</b> (2–10 steps).</li>
        <li>Pick a date range — it defaults to the last 30 days.</li>
        <li><b>Compute</b> to see overall conversion and step-by-step drop-off.</li>
      </ol>
      <p class="faint fine">
        Each step is matched per person and only at-or-after the previous step's time, so order
        matters — call <code class="ic">identify()</code> so events attribute to the same person.
        Only event names seen in the selected window appear in the picker.
      </p>
    </Card>

    <!-- Verify -->
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="circle-check" size={16} /><h3>Verify it works</h3></div>
      {/snippet}
      <p class="muted verify-lead">
        Fire a test event from your app, then watch it land here. The first event can take a few
        seconds.
      </p>
      <CodeBlock code={verifySnippet} language={lang} />
      <div class="verify-links">
        <a class="vl" href="#/issues"><Icon name="triangle-alert" size={15} /> Exceptions</a>
        <a class="vl" href="#/events"><Icon name="diamond" size={15} /> Events</a>
      </div>
    </Card>

    <!-- Troubleshooting -->
    <Card>
      {#snippet header()}
        <div class="card-h"><Icon name="life-buoy" size={16} /><h3>Troubleshooting</h3></div>
      {/snippet}
      <div class="tshoot">
        {#each troubleshooting as t (t.q)}
          <div class="ts-row">
            <div class="ts-q">{t.q}</div>
            <div class="ts-a muted">{t.a}</div>
          </div>
        {/each}
      </div>
    </Card>

    <!-- Footer links -->
    <div class="foot-links">
      <a class="fl" href="#/settings">
        <span class="fl-ic"><Icon name="settings" size={16} /></span>
        <span class="fl-tx"><b>App settings</b><span class="muted">Copy or rotate your DSN</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
      <a class="fl" href="#/projects">
        <span class="fl-ic"><Icon name="folders" size={16} /></span>
        <span class="fl-tx"><b>Projects & apps</b><span class="muted">Add another platform</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
      <a class="fl" href="#/overview">
        <span class="fl-ic"><Icon name="layout-dashboard" size={16} /></span>
        <span class="fl-tx"><b>Overview</b><span class="muted">See signals roll in</span></span>
        <Icon name="arrow-right" size={15} />
      </a>
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
  .doc {
    display: flex;
    flex-direction: column;
    gap: 18px;
    max-width: 860px;
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
    margin-left: auto;
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

  /* platform segmented control */
  .seg {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .seg-opt {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-weight: 600;
    font-size: 14px;
    transition: all 0.13s ease;
  }
  .seg-opt:hover {
    border-color: var(--text-faint);
    color: var(--text);
  }
  .seg-opt.active {
    border-color: var(--primary);
    background: var(--primary-soft);
    color: var(--primary);
  }
  .seg-sub {
    font-family: var(--font-mono);
    font-size: 11.5px;
    font-weight: 500;
    opacity: 0.7;
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
    grid-template-columns: minmax(0, 320px) 1fr;
    gap: 16px;
    padding: 10px 2px;
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
    padding-left: 20px;
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
