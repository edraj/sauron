import 'package:http/http.dart' as http;

import 'envelope.dart';

/// Hook invoked before an [ErrorItem] is queued for delivery.
///
/// Return the (possibly mutated) item to send it, or `null` to drop it.
typedef BeforeSendCallback = ErrorItem? Function(ErrorItem event);

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

  /// Called before each error is enqueued; return `null` to drop.
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
