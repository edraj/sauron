import 'package:http/http.dart' as http;

import 'envelope.dart';

/// Hook invoked before an [EnvelopeItem] is queued for delivery.
///
/// Receives the outgoing item — an [ErrorItem], [EventItem], [IdentifyItem],
/// [TransactionItem], or [BreadcrumbBatchItem]. Return the (possibly mutated or
/// replaced) item to send it, or `null` to drop it.
///
/// > Behavioral change in 0.3.0: this previously ran on errors only. It now
/// > runs on EVERY outgoing item. Guard on the runtime type if you only want to
/// > act on a subset, e.g. `if (item is! ErrorItem) return item;`.
typedef BeforeSendCallback = Object? Function(Object item);

/// Configuration for the Sauron SDK. Constructed and handed to [Sauron.init]:
///
/// ```dart
/// await Sauron.init(
///   SauronOptions(
///     dsn: 'https://pk_test@localhost:8081/1',
///     release: 'app@1.4.2+1402',
///   ),
///   appRunner: () => runApp(const MyApp()),
/// );
/// ```
///
/// Every field is also mutable, so post-construction tweaks still work:
/// `final o = SauronOptions(dsn: dsn)..debug = true;`.
class SauronOptions {
  /// Creates the options. Every parameter is optional and falls back to the
  /// documented default; only [dsn] is needed to enable the SDK.
  ///
  /// [tags], [contexts] and [extra] are defensively copied, so mutating the
  /// maps you pass in afterwards does not affect the SDK.
  SauronOptions({
    this.dsn,
    this.release,
    this.appVersion,
    this.appBuild,
    this.screen,
    this.sampleRate = 1.0,
    this.maxBreadcrumbs = 100,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
    this.beforeSend,
    this.flushInterval = const Duration(seconds: 5),
    this.maxBatchItems = 30,
    this.maxItemsPerEnvelope = 1000,
    this.maxQueueBytes = 5 * 1024 * 1024,
    this.gzipThresholdBytes = 1024,
    this.debug = false,
    this.attachStacktrace = true,
    this.httpClient,
  })  : tags = Map<String, String>.of(tags ?? const <String, String>{}),
        contexts = Map<String, Map<String, Object?>>.of(
            contexts ?? const <String, Map<String, Object?>>{}),
        extra = Map<String, Object?>.of(extra ?? const <String, Object?>{});

  /// The project DSN. When null/empty the SDK stays disabled (all calls no-op).
  String? dsn;

  /// Release identifier, e.g. `app@1.4.2+1402`.
  String? release;

  /// App version reported in the envelope's `context.app`, e.g. `1.4.2`.
  ///
  /// Developer-supplied: the SDK does not read this off the platform, so it
  /// carries no plugin dependency. Wire it from your own build config — e.g.
  /// a `--dart-define`, a generated constants file, or `package_info_plus` if
  /// you already depend on it. When both this and [appBuild] are null the
  /// `app` block is omitted.
  String? appVersion;

  /// App build number reported in the envelope's `context.app`, e.g. `1402`.
  /// See [appVersion].
  String? appBuild;

  /// Seed the initial screen/route name. Stamped on events/errors until
  /// [SauronClient.setScreen] (or the [SauronNavigatorObserver]) changes it.
  String? screen;

  /// Error sample rate in `[0.0, 1.0]`. Applies to error events only;
  /// analytics events and identifies are always sent. Defaults to `1.0`.
  double sampleRate;

  /// Maximum breadcrumbs retained per scope. Defaults to `100`.
  int maxBreadcrumbs;

  /// Default tags (string->string) seeded into the client's global scope at
  /// init. Per-call tags override these by key on each capture.
  Map<String, String> tags;

  /// Default contexts (name -> structured block) seeded into the global scope.
  /// Distinct from the machine-owned device/os/app/runtime `context`.
  Map<String, Map<String, Object?>> contexts;

  /// Default extra (freeform JSON) seeded into the global scope.
  Map<String, Object?> extra;

  /// Called before every item (error, event, identify, transaction) is
  /// enqueued; return the item to send it or `null` to drop it.
  BeforeSendCallback? beforeSend;

  /// How often the transport auto-flushes batched items. Defaults to 5s.
  Duration flushInterval;

  /// Flush eagerly once this many items have been buffered. Defaults to `30`.
  int maxBatchItems;

  /// Hard ceiling on items in a single envelope, matching the server's limit.
  ///
  /// [maxBatchItems] only *triggers* a flush; it does not bound the request. If
  /// events are produced faster than flushes complete — offline, or mid-retry —
  /// the buffer keeps growing and would otherwise go out as one oversized
  /// envelope, which the server rejects with a non-retryable 400. Defaults to
  /// `1000`.
  int maxItemsPerEnvelope;

  /// Hard cap on the on-disk offline queue (bytes). Oldest envelopes are
  /// evicted FIFO once exceeded. Defaults to 5 MiB.
  int maxQueueBytes;

  /// Payloads at or above this size are gzipped (when gzip is available).
  /// Defaults to `1024`.
  int gzipThresholdBytes;

  /// Emit verbose diagnostics via `debugPrint`. Defaults to `false`.
  bool debug;

  /// Automatically attach the current stack trace to captured errors that
  /// arrive without one. Defaults to `true`.
  bool attachStacktrace;

  /// Optional injected HTTP client (used by tests). Defaults to a fresh
  /// [http.Client].
  http.Client? httpClient;

  /// Validates the sample rate, clamping to `[0.0, 1.0]`.
  double get normalizedSampleRate => sampleRate.clamp(0.0, 1.0).toDouble();

  /// Whether the SDK has enough configuration to send data.
  bool get isConfigured => (dsn ?? '').isNotEmpty;
}
