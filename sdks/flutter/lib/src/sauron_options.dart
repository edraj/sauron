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

/// Configuration for the Sauron SDK. Populated via the builder passed to
/// [Sauron.init].
class SauronOptions {
  /// The project DSN. When null/empty the SDK stays disabled (all calls no-op).
  String? dsn;

  /// Deployment environment, e.g. `production`, `staging`.
  String environment = 'production';

  /// Release identifier, e.g. `app@1.4.2+1402`.
  String? release;

  /// Seed the initial screen/route name. Stamped on events/errors until
  /// [SauronClient.setScreen] (or the [SauronNavigatorObserver]) changes it.
  String? screen;

  /// Error sample rate in `[0.0, 1.0]`. Applies to error events only;
  /// analytics events and identifies are always sent.
  double sampleRate = 1.0;

  /// Maximum breadcrumbs retained per scope.
  int maxBreadcrumbs = 100;

  /// Default tags (string->string) seeded into the client's global scope at
  /// init. Per-call tags override these by key on each capture.
  Map<String, String> tags = <String, String>{};

  /// Default contexts (name -> structured block) seeded into the global scope.
  /// Distinct from the machine-owned device/os/app/runtime `context`.
  Map<String, Map<String, Object?>> contexts = <String, Map<String, Object?>>{};

  /// Default extra (freeform JSON) seeded into the global scope.
  Map<String, Object?> extra = <String, Object?>{};

  /// Called before every item (error, event, identify, transaction) is
  /// enqueued; return the item to send it or `null` to drop it.
  BeforeSendCallback? beforeSend;

  /// How often the transport auto-flushes batched items.
  Duration flushInterval = const Duration(seconds: 5);

  /// Flush eagerly once this many items have been buffered.
  int maxBatchItems = 30;

  /// Hard cap on the on-disk offline queue (bytes). Oldest envelopes are
  /// evicted FIFO once exceeded.
  int maxQueueBytes = 5 * 1024 * 1024;

  /// Payloads at or above this size are gzipped (when gzip is available).
  int gzipThresholdBytes = 1024;

  /// Emit verbose diagnostics via `debugPrint`.
  bool debug = false;

  /// Automatically attach the current stack trace to captured errors that
  /// arrive without one.
  bool attachStacktrace = true;

  /// Optional injected HTTP client (used by tests). Defaults to a fresh
  /// [http.Client].
  http.Client? httpClient;

  /// Validates the sample rate, clamping to `[0.0, 1.0]`.
  double get normalizedSampleRate => sampleRate.clamp(0.0, 1.0).toDouble();

  /// Whether the SDK has enough configuration to send data.
  bool get isConfigured => (dsn ?? '').isNotEmpty;
}
