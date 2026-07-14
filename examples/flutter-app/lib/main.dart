import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:sauron_flutter/sauron_flutter.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'showcase.dart';

/// Default DSN — a real "Flutter Demo" app on the running dev ingest gateway
/// (`:8091`). Editable and persisted at runtime; see [DemoConfig].
///
/// Notes:
///  * Under Docker Compose the ingest listens on `:8081` instead of `:8091`.
///  * On an Android emulator, `localhost` is the emulator itself — reach the
///    host machine via `10.0.2.2` (swap it into the DSN host).
const String kDefaultDsn =
    'http://pk_8c94e744ebc5376ffd9a16049ac4147b@localhost:8081/'
    '32a7799a-2027-45bd-9d67-2c0df85656ac';
const String kDefaultEnvironment = 'development';
const String kDefaultRelease = 'sauron_flutter_demo@1.0.0+1';
const String kDefaultDistinctId = 'demo_user_1';

Future<void> main() async {
  // Needed so we can read persisted config before configuring the SDK.
  WidgetsFlutterBinding.ensureInitialized();
  final DemoConfig config = await DemoConfig.load();

  // `appRunner` launches the app inside `runZonedGuarded`, binding all four
  // uncaught-error capture layers inside the zone. Any uncaught Flutter/Dart
  // error is auto-captured from here on.
  await Sauron.init(
    (SauronOptions o) {
      o.dsn = config.dsn;
      o.environment = config.environment;
      o.release = config.release;
      o.debug = true;
      o.flushInterval = const Duration(seconds: 5);
    },
    appRunner: () => runApp(SauronDemoApp(config: config)),
  );
}

/// The editable, persisted demo configuration.
class DemoConfig {
  const DemoConfig({
    required this.dsn,
    required this.environment,
    required this.release,
    required this.distinctId,
  });

  final String dsn;
  final String environment;
  final String release;
  final String distinctId;

  static const String _kDsn = 'demo.dsn';
  static const String _kEnvironment = 'demo.environment';
  static const String _kRelease = 'demo.release';
  static const String _kDistinctId = 'demo.distinctId';

  static Future<DemoConfig> load() async {
    final SharedPreferences prefs = await SharedPreferences.getInstance();
    return DemoConfig(
      dsn: prefs.getString(_kDsn) ?? kDefaultDsn,
      environment: prefs.getString(_kEnvironment) ?? kDefaultEnvironment,
      release: prefs.getString(_kRelease) ?? kDefaultRelease,
      distinctId: prefs.getString(_kDistinctId) ?? kDefaultDistinctId,
    );
  }

  Future<void> save() async {
    final SharedPreferences prefs = await SharedPreferences.getInstance();
    await prefs.setString(_kDsn, dsn);
    await prefs.setString(_kEnvironment, environment);
    await prefs.setString(_kRelease, release);
    await prefs.setString(_kDistinctId, distinctId);
  }

  /// True when a field that only binds at startup (DSN / environment / release)
  /// differs, meaning a restart is required to fully re-point the SDK.
  bool requiresRestartVersus(DemoConfig other) =>
      dsn != other.dsn ||
      environment != other.environment ||
      release != other.release;
}

class SauronDemoApp extends StatelessWidget {
  const SauronDemoApp({required this.config, super.key});

  final DemoConfig config;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Sauron — Flutter SDK Demo',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorSchemeSeed: const Color(0xFF6C4CF1),
        brightness: Brightness.light,
        useMaterial3: true,
      ),
      darkTheme: ThemeData(
        colorSchemeSeed: const Color(0xFF6C4CF1),
        brightness: Brightness.dark,
        useMaterial3: true,
      ),
      navigatorObservers: <NavigatorObserver>[
        // Records a navigation breadcrumb on every route change.
        if (Sauron.client != null) SauronNavigatorObserver(Sauron.client!),
      ],
      home: DemoHomePage(config: config),
    );
  }
}

/// A single entry in the in-app activity log.
class _LogEntry {
  _LogEntry(this.message, this.icon) : time = DateTime.now();

  final String message;
  final IconData icon;
  final DateTime time;

  String get formattedTime {
    final DateTime t = time;
    String two(int n) => n.toString().padLeft(2, '0');
    return '${two(t.hour)}:${two(t.minute)}:${two(t.second)}';
  }
}

class DemoHomePage extends StatefulWidget {
  const DemoHomePage({required this.config, super.key});

  final DemoConfig config;

  @override
  State<DemoHomePage> createState() => _DemoHomePageState();
}

class _DemoHomePageState extends State<DemoHomePage> {
  late final DemoConfig _launchConfig = widget.config;
  late final TextEditingController _dsnController =
      TextEditingController(text: widget.config.dsn);
  late final TextEditingController _environmentController =
      TextEditingController(text: widget.config.environment);
  late final TextEditingController _releaseController =
      TextEditingController(text: widget.config.release);
  late final TextEditingController _distinctIdController =
      TextEditingController(text: widget.config.distinctId);

  final Random _random = Random();
  final List<_LogEntry> _log = <_LogEntry>[];
  bool _pendingRestart = false;

  /// The current screen name for the v0.2.0 screen API. The wired-in
  /// [SauronNavigatorObserver] sets this automatically for *named* routes; the
  /// home route here is unnamed, so we drive it explicitly (see [initState] and
  /// [_toggleScreen]).
  String _demoScreen = 'Home';

  @override
  void initState() {
    super.initState();
    // Declare the initial screen up front. Emits a `$screen` view and tags
    // every later track()/captureException call with `Home`.
    Sauron.setScreen('Home');
    _log.insert(
      0,
      _LogEntry('setScreen("Home") — screen tracking active', Icons.web_outlined),
    );
  }

  // ---- showcase (cohort simulator) state -------------------------------------
  late final TextEditingController _showcaseCountController =
      TextEditingController(text: '$defaultUsers');
  /// The demo's current "real" identity, restored after a showcase run.
  SauronUser? _identifiedUser;
  bool _showcaseRunning = false;
  ShowcaseProgress? _showcaseProgress;
  ShowcaseSummary? _showcaseSummary;

  @override
  void dispose() {
    _dsnController.dispose();
    _environmentController.dispose();
    _releaseController.dispose();
    _distinctIdController.dispose();
    _showcaseCountController.dispose();
    super.dispose();
  }

  // ---- logging ---------------------------------------------------------------

  void _record(String message, IconData icon) {
    setState(() => _log.insert(0, _LogEntry(message, icon)));
  }

  // ---- config ----------------------------------------------------------------

  Future<void> _saveConfig() async {
    final DemoConfig next = DemoConfig(
      dsn: _dsnController.text.trim(),
      environment: _environmentController.text.trim(),
      release: _releaseController.text.trim(),
      distinctId: _distinctIdController.text.trim(),
    );
    await next.save();
    final bool needsRestart = next.requiresRestartVersus(_launchConfig);
    setState(() => _pendingRestart = needsRestart);
    _record(
      needsRestart
          ? 'Saved config — restart to re-point the SDK'
          : 'Saved config',
      Icons.save_outlined,
    );
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(
          needsRestart
              ? 'Saved. Restart the app to apply the new DSN / environment / '
                  'release to all capture layers.'
              : 'Saved.',
        ),
      ),
    );
  }

  /// The endpoint envelopes are POSTed to, derived from the DSN field.
  String get _resolvedEndpoint {
    try {
      return Dsn.parse(_dsnController.text.trim()).envelopeEndpoint.toString();
    } on FormatException catch (e) {
      return 'Invalid DSN: ${e.message}';
    }
  }

  // ---- SDK actions -----------------------------------------------------------

  /// Layer 1 — thrown synchronously inside a gesture callback, captured by
  /// `FlutterError.onError`.
  void _throwUncaughtSync() {
    _record('Throwing StateError synchronously (FlutterError layer)…',
        Icons.error_outline);
    throw StateError(
      'Sauron demo: uncaught synchronous error from a gesture callback',
    );
  }

  /// Layer 2 — an unawaited future that throws, captured by
  /// `PlatformDispatcher.onError` / the guarding zone.
  void _asyncGapError() {
    _record('Scheduling an unawaited future that throws (async gap)…',
        Icons.bolt_outlined);
    unawaited(
      Future<void>.delayed(const Duration(milliseconds: 120), () {
        throw StateError(
          'Sauron demo: uncaught async-gap error (PlatformDispatcher layer)',
        );
      }),
    );
  }

  /// A handled error captured manually with an explicit stack trace + mechanism.
  void _captureHandled() {
    try {
      throw const FormatException(
        'Sauron demo: a handled, manually captured error',
      );
    } on FormatException catch (error, stackTrace) {
      Sauron.captureException(
        error,
        stackTrace: stackTrace,
        mechanism: const Mechanism(type: 'manual', handled: true),
      );
      _record('captureException() sent a handled FormatException',
          Icons.bug_report_outlined);
    }
  }

  void _trackCheckout() {
    final double cartValue =
        double.parse((_random.nextDouble() * 200 + 5).toStringAsFixed(2));
    Sauron.track(
      'checkout_completed',
      properties: <String, Object?>{
        'cart_value': cartValue,
        'currency': 'USD',
        'items': _random.nextInt(5) + 1,
      },
    );
    _record('track("checkout_completed") — cart \$$cartValue',
        Icons.shopping_cart_checkout);
  }

  /// v0.2.0 screen API — switch the active screen. `setScreen` emits a
  /// `$screen` view on change and attributes subsequent events/errors to it;
  /// `Sauron.screen` reads it back. Toggles Home ⇄ Checkout.
  void _toggleScreen() {
    _demoScreen = _demoScreen == 'Home' ? 'Checkout' : 'Home';
    Sauron.setScreen(_demoScreen);
    _record(
      'setScreen("$_demoScreen") — active screen is now ${Sauron.screen}',
      Icons.web_outlined,
    );
  }

  void _trackScreenViewed() {
    Sauron.track(
      'screen_viewed',
      properties: <String, Object?>{'screen': 'demo_home', 'source': 'button'},
    );
    _record('track("screen_viewed")', Icons.visibility_outlined);
  }

  void _identify() {
    final String id = _distinctIdController.text.trim();
    if (id.isEmpty) {
      _record('identify skipped — distinct_id is empty', Icons.info_outline);
      return;
    }
    Sauron.identify(
      id,
      traits: <String, Object?>{'plan': 'pro', 'demo': true},
    );
    final SauronUser user = SauronUser(
      id: id,
      email: '$id@example.com',
      traits: const <String, Object?>{'plan': 'pro'},
    );
    Sauron.setUser(user);
    _identifiedUser = user;
    _record('identify("$id") with trait plan=pro', Icons.person_outline);
  }

  /// Records a breadcrumb, then crashes — the crash envelope carries the
  /// breadcrumb trail so you can see what led up to it in the dashboard.
  void _breadcrumbThenThrow() {
    Sauron.addBreadcrumb(
      Breadcrumb.ui(
        'User tapped "addBreadcrumb then throw"',
        data: <String, Object?>{'screen': 'demo_home'},
      ),
    );
    _record('Added a UI breadcrumb, now throwing…', Icons.error_outline);
    throw StateError('Sauron demo: crash preceded by a breadcrumb');
  }

  Future<void> _flush() async {
    _record('flush() — draining batched + queued envelopes…', Icons.sync);
    await Sauron.flush();
    _record('flush() complete', Icons.check_circle_outline);
  }

  // ---- showcase (cohort simulator) -------------------------------------------

  /// A [ShowcaseSink] backed by the live SDK. Switches identities via
  /// `setUser` (not `identify`) so each synthetic user keeps its own
  /// `distinct_id` — real funnel drop-off, no person-aliasing.
  ShowcaseSink _showcaseSink() {
    return _SauronShowcaseSink(
      readUser: () => _identifiedUser,
      writeUser: (SauronUser? u) {
        _identifiedUser = u;
        Sauron.setUser(u);
      },
    );
  }

  Future<void> _runShowcase() async {
    if (_showcaseRunning) {
      return;
    }
    final int count =
        (int.tryParse(_showcaseCountController.text) ?? defaultUsers).clamp(1, maxUsers);
    final String runId = DateTime.now().millisecondsSinceEpoch.toRadixString(36);
    setState(() {
      _showcaseRunning = true;
      _showcaseSummary = null;
      _showcaseProgress = null;
    });
    _record('Showcase started — simulating $count users…', Icons.auto_awesome);
    try {
      final ShowcaseSummary summary = await runShowcase(
        _showcaseSink(),
        users: count,
        runId: runId,
        onProgress: (ShowcaseProgress p) => setState(() => _showcaseProgress = p),
      );
      final int completed = summary.funnel.last.count;
      setState(() => _showcaseSummary = summary);
      _record(
        'Showcase complete — ${summary.users} users · ${summary.events} events · '
        '${summary.transactions} txns · $completed completed',
        Icons.check_circle_outline,
      );
    } catch (error) {
      _record('Showcase failed: $error', Icons.error_outline);
    } finally {
      setState(() {
        _showcaseRunning = false;
        _showcaseProgress = null;
      });
    }
  }

  // ---- UI --------------------------------------------------------------------

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        leading: const Padding(
          padding: EdgeInsets.all(10),
          child: Image(image: AssetImage('assets/sauron_eye.png')),
        ),
        title: const Text('Sauron — Flutter SDK Demo'),
        backgroundColor: theme.colorScheme.surfaceContainerHighest,
      ),
      body: ListView(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 32),
        children: <Widget>[
          if (_pendingRestart) const _RestartBanner(),
          _ConnectionCard(
            dsnController: _dsnController,
            environmentController: _environmentController,
            releaseController: _releaseController,
            distinctIdController: _distinctIdController,
            resolvedEndpoint: _resolvedEndpoint,
            sdkEnabled: Sauron.isEnabled,
            onEndpointRefresh: () => setState(() {}),
            onSave: _saveConfig,
          ),
          const SizedBox(height: 24),
          const _SectionHeader('Showcase funnels, journeys & performance'),
          _ShowcaseCard(
            countController: _showcaseCountController,
            running: _showcaseRunning,
            progress: _showcaseProgress,
            summary: _showcaseSummary,
            onRun: () => unawaited(_runShowcase()),
          ),
          const SizedBox(height: 24),
          const _SectionHeader('Crashes & errors'),
          _ActionTile(
            icon: Icons.error_outline,
            title: 'Throw uncaught (sync)',
            caption:
                'Throws a StateError in a gesture callback → captured by the '
                'FlutterError.onError layer.',
            buttonLabel: 'Throw now',
            tone: _Tone.danger,
            onPressed: _throwUncaughtSync,
          ),
          _ActionTile(
            icon: Icons.bolt_outlined,
            title: 'Async gap error',
            caption:
                'An unawaited future that throws → captured by the '
                'PlatformDispatcher / zone layer.',
            buttonLabel: 'Trigger',
            tone: _Tone.danger,
            onPressed: _asyncGapError,
          ),
          _ActionTile(
            icon: Icons.bug_report_outlined,
            title: 'captureException (handled)',
            caption:
                'Manually captures a synthetic error with an explicit stack '
                'trace and a handled mechanism.',
            buttonLabel: 'Capture',
            onPressed: _captureHandled,
          ),
          _ActionTile(
            icon: Icons.error_outline,
            title: 'addBreadcrumb then throw',
            caption:
                'Records a UI breadcrumb, then crashes — the crash carries the '
                'breadcrumb trail.',
            buttonLabel: 'Breadcrumb + throw',
            tone: _Tone.danger,
            onPressed: _breadcrumbThenThrow,
          ),
          const SizedBox(height: 24),
          const _SectionHeader('Product analytics'),
          _ActionTile(
            icon: Icons.shopping_cart_checkout,
            title: 'track: checkout_completed',
            caption:
                'Sends a checkout_completed event with a random cart value.',
            buttonLabel: 'Track',
            onPressed: _trackCheckout,
          ),
          _ActionTile(
            icon: Icons.visibility_outlined,
            title: 'track: screen_viewed',
            caption: 'Sends a screen_viewed event for the demo home screen.',
            buttonLabel: 'Track',
            onPressed: _trackScreenViewed,
          ),
          _ActionTile(
            icon: Icons.web_outlined,
            title: 'setScreen (navigate)',
            caption:
                'v0.2.0 screen API: Sauron.setScreen(...) emits a \$screen view '
                'and tags later events/errors with the active screen. Toggles '
                'Home ⇄ Checkout.',
            buttonLabel: 'Change screen',
            onPressed: _toggleScreen,
          ),
          _ActionTile(
            icon: Icons.person_outline,
            title: 'identify',
            caption:
                'Identifies the user from the distinct_id field above and '
                'attaches a plan=pro trait.',
            buttonLabel: 'Identify',
            onPressed: _identify,
          ),
          const SizedBox(height: 24),
          const _SectionHeader('Transport'),
          _ActionTile(
            icon: Icons.sync,
            title: 'Flush now',
            caption:
                'Forces the transport to drain batched + persisted envelopes '
                'immediately.',
            buttonLabel: 'Flush',
            onPressed: () => unawaited(_flush()),
          ),
          const SizedBox(height: 24),
          const _SectionHeader('Activity log'),
          _ActivityLog(entries: _log),
          const SizedBox(height: 24),
          Text(
            'Open the Sauron dashboard → Flutter Demo app → Issues/Events to '
            'see these.',
            textAlign: TextAlign.center,
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

// ---- widgets ----------------------------------------------------------------

class _RestartBanner extends StatelessWidget {
  const _RestartBanner();

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Container(
      margin: const EdgeInsets.only(bottom: 16),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: theme.colorScheme.tertiaryContainer,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Row(
        children: <Widget>[
          Icon(Icons.restart_alt, color: theme.colorScheme.onTertiaryContainer),
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              'Config saved. Restart the app to apply the new DSN / '
              'environment / release to all capture layers.',
              style: theme.textTheme.bodyMedium?.copyWith(
                color: theme.colorScheme.onTertiaryContainer,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _ConnectionCard extends StatelessWidget {
  const _ConnectionCard({
    required this.dsnController,
    required this.environmentController,
    required this.releaseController,
    required this.distinctIdController,
    required this.resolvedEndpoint,
    required this.sdkEnabled,
    required this.onEndpointRefresh,
    required this.onSave,
  });

  final TextEditingController dsnController;
  final TextEditingController environmentController;
  final TextEditingController releaseController;
  final TextEditingController distinctIdController;
  final String resolvedEndpoint;
  final bool sdkEnabled;
  final VoidCallback onEndpointRefresh;
  final Future<void> Function() onSave;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            Row(
              children: <Widget>[
                Expanded(
                  child:
                      Text('Connection', style: theme.textTheme.titleMedium),
                ),
                _StatusChip(enabled: sdkEnabled),
              ],
            ),
            const SizedBox(height: 12),
            TextField(
              controller: dsnController,
              onChanged: (_) => onEndpointRefresh(),
              minLines: 1,
              maxLines: 2,
              decoration: const InputDecoration(
                labelText: 'DSN',
                border: OutlineInputBorder(),
                helperText: 'pk_…@host:port/project_id',
              ),
            ),
            const SizedBox(height: 8),
            Text(
              'POST → $resolvedEndpoint',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
                fontFeatures: const <FontFeature>[FontFeature.tabularFigures()],
              ),
            ),
            const SizedBox(height: 16),
            Row(
              children: <Widget>[
                Expanded(
                  child: TextField(
                    controller: environmentController,
                    decoration: const InputDecoration(
                      labelText: 'Environment',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: TextField(
                    controller: releaseController,
                    decoration: const InputDecoration(
                      labelText: 'Release',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 16),
            TextField(
              controller: distinctIdController,
              decoration: const InputDecoration(
                labelText: 'distinct_id (used by identify)',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 16),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton.icon(
                onPressed: () => unawaited(onSave()),
                icon: const Icon(Icons.save_outlined),
                label: const Text('Save & re-initialize'),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.enabled});

  final bool enabled;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final Color color = enabled ? Colors.green : theme.colorScheme.outline;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: <Widget>[
          Icon(enabled ? Icons.check_circle : Icons.cancel,
              size: 16, color: color),
          const SizedBox(width: 6),
          Text(
            enabled ? 'SDK active' : 'SDK disabled',
            style: theme.textTheme.labelMedium?.copyWith(color: color),
          ),
        ],
      ),
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader(this.title);

  final String title;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 8, left: 4),
      child: Text(
        title.toUpperCase(),
        style: theme.textTheme.labelLarge?.copyWith(
          color: theme.colorScheme.primary,
          letterSpacing: 0.8,
        ),
      ),
    );
  }
}

enum _Tone { normal, danger }

class _ActionTile extends StatelessWidget {
  const _ActionTile({
    required this.icon,
    required this.title,
    required this.caption,
    required this.buttonLabel,
    required this.onPressed,
    this.tone = _Tone.normal,
  });

  final IconData icon;
  final String title;
  final String caption;
  final String buttonLabel;
  final VoidCallback onPressed;
  final _Tone tone;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final bool danger = tone == _Tone.danger;
    final Color accent =
        danger ? theme.colorScheme.error : theme.colorScheme.primary;
    return Card(
      margin: const EdgeInsets.only(bottom: 12),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            CircleAvatar(
              backgroundColor: accent.withValues(alpha: 0.14),
              foregroundColor: accent,
              child: Icon(icon),
            ),
            const SizedBox(width: 16),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text(title, style: theme.textTheme.titleSmall),
                  const SizedBox(height: 4),
                  Text(
                    caption,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 12),
                  Align(
                    alignment: Alignment.centerLeft,
                    child: danger
                        ? FilledButton.tonal(
                            onPressed: onPressed,
                            style: FilledButton.styleFrom(
                              backgroundColor:
                                  theme.colorScheme.errorContainer,
                              foregroundColor:
                                  theme.colorScheme.onErrorContainer,
                            ),
                            child: Text(buttonLabel),
                          )
                        : FilledButton.tonal(
                            onPressed: onPressed,
                            child: Text(buttonLabel),
                          ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _ActivityLog extends StatelessWidget {
  const _ActivityLog({required this.entries});

  final List<_LogEntry> entries;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    if (entries.isEmpty) {
      return Card(
        margin: EdgeInsets.zero,
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Center(
            child: Text(
              'No actions yet — tap a button above.',
              style: theme.textTheme.bodyMedium?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
        ),
      );
    }
    return Card(
      margin: EdgeInsets.zero,
      child: Column(
        children: <Widget>[
          for (int i = 0; i < entries.length; i++) ...<Widget>[
            if (i > 0) const Divider(height: 1),
            ListTile(
              dense: true,
              leading: Icon(entries[i].icon, size: 20),
              title: Text(entries[i].message),
              trailing: Text(
                entries[i].formattedTime,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                  fontFeatures: const <FontFeature>[
                    FontFeature.tabularFigures(),
                  ],
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// A [ShowcaseSink] backed by the live SDK. Identity is switched via `setUser`
/// so each synthetic user keeps its own `distinct_id`.
class _SauronShowcaseSink implements ShowcaseSink {
  _SauronShowcaseSink({required this.readUser, required this.writeUser});

  final SauronUser? Function() readUser;
  final void Function(SauronUser?) writeUser;

  @override
  SinkUser? getUser() {
    final SauronUser? user = readUser();
    final String? id = user?.id;
    return id == null ? null : SinkUser(id, user!.traits);
  }

  @override
  void setUser(SinkUser? user) {
    writeUser(user == null
        ? null
        : SauronUser(id: user.id, traits: user.traits ?? const <String, Object?>{}));
  }

  @override
  void track(String name, {Map<String, Object?>? properties}) =>
      Sauron.track(name, properties: properties);

  @override
  void trackTransaction(SimTransaction txn) => Sauron.trackTransaction(
        name: txn.name,
        op: txn.op,
        duration: txn.duration,
        status: txn.status,
        httpMethod: txn.httpMethod,
        httpStatus: txn.httpStatus,
        url: txn.url,
      );

  @override
  Future<void> flush() => Sauron.flush();
}

class _ShowcaseCard extends StatelessWidget {
  const _ShowcaseCard({
    required this.countController,
    required this.running,
    required this.progress,
    required this.summary,
    required this.onRun,
  });

  final TextEditingController countController;
  final bool running;
  final ShowcaseProgress? progress;
  final ShowcaseSummary? summary;
  final VoidCallback onRun;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final ShowcaseSummary? result = summary;
    final int funnelMax = (result == null || result.funnel.isEmpty)
        ? 1
        : max(1, result.funnel.first.count);
    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            Text('Run showcase', style: theme.textTheme.titleSmall),
            const SizedBox(height: 4),
            Text(
              "Drives the SDK through a synthetic e-commerce cohort — many users "
              'with realistic drop-off, branching paths and a spread of performance '
              "transactions. Populates the dashboard's Funnels, Journeys and "
              'Performance screens.',
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
            ),
            const SizedBox(height: 14),
            Row(
              children: <Widget>[
                SizedBox(
                  width: 100,
                  child: TextField(
                    controller: countController,
                    enabled: !running,
                    keyboardType: TextInputType.number,
                    decoration: const InputDecoration(
                      labelText: 'Users',
                      border: OutlineInputBorder(),
                      isDense: true,
                    ),
                  ),
                ),
                const SizedBox(width: 12),
                FilledButton.icon(
                  onPressed: running ? null : onRun,
                  icon: running
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.auto_awesome),
                  label: Text(running ? 'Simulating…' : 'Run showcase'),
                ),
              ],
            ),
            if (running && progress != null) ...<Widget>[
              const SizedBox(height: 14),
              LinearProgressIndicator(
                value: progress!.total == 0 ? null : progress!.done / progress!.total,
              ),
              const SizedBox(height: 6),
              Text(
                '${progress!.done} / ${progress!.total} users · '
                '${progress!.events} events · ${progress!.transactions} txns',
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              ),
            ],
            if (result != null && !running) ...<Widget>[
              const SizedBox(height: 16),
              for (final FunnelCount step in result.funnel)
                Padding(
                  padding: const EdgeInsets.only(bottom: 6),
                  child: Row(
                    children: <Widget>[
                      SizedBox(
                        width: 150,
                        child: Text(
                          step.name,
                          style: theme.textTheme.bodySmall,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      Expanded(
                        child: ClipRRect(
                          borderRadius: BorderRadius.circular(4),
                          child: LinearProgressIndicator(
                            value: step.count / funnelMax,
                            minHeight: 14,
                            backgroundColor:
                                theme.colorScheme.surfaceContainerHighest,
                          ),
                        ),
                      ),
                      SizedBox(
                        width: 44,
                        child: Text(
                          '${step.count}',
                          textAlign: TextAlign.right,
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontFeatures: const <FontFeature>[
                              FontFeature.tabularFigures(),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              const SizedBox(height: 8),
              Text(
                'Sent ${result.events} events + ${result.transactions} transactions '
                'across ${result.users} users. Open the dashboard → Flutter Demo → '
                'Funnels / Journeys / Performance.',
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
