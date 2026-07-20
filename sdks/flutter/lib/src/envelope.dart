import 'dart:convert';

import 'types.dart';

/// SDK identity broadcast in every envelope header.
const String kSauronSdkName = 'sauron.flutter';

/// SDK version — keep in sync with `pubspec.yaml`.
const String kSauronSdkVersion = '0.3.0';

/// The envelope header: routing + provenance metadata.
class EnvelopeHeader {
  const EnvelopeHeader({
    required this.dsn,
    required this.sentAt,
    required this.environment,
    this.release,
    this.sdkName = kSauronSdkName,
    this.sdkVersion = kSauronSdkVersion,
  });

  /// The (public) DSN string, e.g. `https://pk_test@localhost:8081/1`.
  final String dsn;

  /// When the envelope left the device (UTC).
  final DateTime sentAt;

  /// Deployment environment, e.g. `production`.
  final String environment;

  /// Release identifier, e.g. `app@1.4.2+1402`.
  final String? release;

  final String sdkName;
  final String sdkVersion;

  Map<String, Object?> toJson() => <String, Object?>{
        'dsn': dsn,
        'sdk': <String, Object?>{
          'name': sdkName,
          'version': sdkVersion,
        },
        'sent_at': sauronIso(sentAt),
        'environment': environment,
        'release': release,
      };
}

/// Ambient context shared by every item in an envelope.
class SauronContext {
  const SauronContext({
    this.device,
    this.os,
    this.app,
    this.runtime,
    this.user,
  });

  final DeviceDescriptor? device;
  final OsDescriptor? os;
  final AppDescriptor? app;
  final RuntimeDescriptor? runtime;
  final SauronUser? user;

  SauronContext copyWith({
    DeviceDescriptor? device,
    OsDescriptor? os,
    AppDescriptor? app,
    RuntimeDescriptor? runtime,
    SauronUser? user,
  }) =>
      SauronContext(
        device: device ?? this.device,
        os: os ?? this.os,
        app: app ?? this.app,
        runtime: runtime ?? this.runtime,
        user: user ?? this.user,
      );

  Map<String, Object?> toJson() => <String, Object?>{
        'device': device?.toJson(),
        'os': os?.toJson(),
        'app': app?.toJson(),
        'runtime': runtime?.toJson(),
        'user': (user ?? const SauronUser()).toJson(),
      };
}

/// Base class for every item carried in an envelope's `items` array.
abstract class EnvelopeItem {
  const EnvelopeItem();

  /// The discriminator:
  /// `error | event | identify | breadcrumb_batch | transaction`.
  String get type;

  Map<String, Object?> toJson();

  /// Rough serialized byte cost, used for batch splitting.
  int get approximateBytes => utf8.encode(jsonEncode(toJson())).length;
}

/// An error/crash item.
class ErrorItem extends EnvelopeItem {
  ErrorItem({
    required this.exception,
    required this.timestamp,
    this.level = SauronLevel.error,
    this.breadcrumbs = const <Breadcrumb>[],
    this.fingerprint,
    this.sessionId,
    this.screen,
    this.rawStacktrace,
    this.debugMeta,
    this.tags = const <String, String>{},
    this.contexts = const <String, Map<String, Object?>>{},
    this.extra = const <String, Object?>{},
  });

  final SauronException exception;
  final DateTime timestamp;
  final SauronLevel level;
  final List<Breadcrumb> breadcrumbs;

  /// Optional custom grouping key. `null` lets the server fingerprint.
  final List<String>? fingerprint;

  /// The session this error occurred in, tying it onto the session timeline.
  final String? sessionId;

  /// The screen/route this error occurred on, if known.
  final String? screen;

  /// Verbatim obfuscated Dart (AOT) trace, for server-side symbolication.
  final String? rawStacktrace;

  /// Symbol-matching metadata for [rawStacktrace] (build-id, load base, os).
  final DebugMeta? debugMeta;

  /// Developer-attached flat tags (string->string). Omitted from the wire when
  /// empty. Distinct from breadcrumbs and the machine-owned `context`.
  final Map<String, String> tags;

  /// Developer-attached structured contexts (name -> block). Omitted when empty.
  final Map<String, Map<String, Object?>> contexts;

  /// Developer-attached freeform extra (JSON). Omitted when empty.
  final Map<String, Object?> extra;

  @override
  String get type => 'error';

  @override
  Map<String, Object?> toJson() {
    final Map<String, Object?> json = <String, Object?>{
      'type': type,
      'timestamp': sauronIso(timestamp),
      'level': level.name,
      'exception': exception.toJson(),
      'breadcrumbs':
          breadcrumbs.map((Breadcrumb crumb) => crumb.toJson()).toList(),
      'fingerprint': fingerprint,
      'session_id': sessionId,
      'screen': screen,
    };
    // Only present for obfuscated Dart errors — keeps the common shape identical
    // across SDKs (the server defaults both fields).
    if (rawStacktrace != null) {
      json['raw_stacktrace'] = rawStacktrace;
      json['debug_meta'] = debugMeta?.toJson();
    }
    if (tags.isNotEmpty) {
      json['tags'] = tags;
    }
    if (contexts.isNotEmpty) {
      json['contexts'] = contexts;
    }
    if (extra.isNotEmpty) {
      json['extra'] = extra;
    }
    return json;
  }
}

/// A product-analytics event item (from `track`).
class EventItem extends EnvelopeItem {
  EventItem({
    required this.name,
    required this.timestamp,
    this.distinctId,
    this.sessionId,
    this.screen,
    Map<String, Object?>? properties,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  })  : properties = properties ?? const <String, Object?>{},
        tags = tags ?? const <String, String>{},
        contexts = contexts ?? const <String, Map<String, Object?>>{},
        extra = extra ?? const <String, Object?>{};

  final String name;
  final DateTime timestamp;
  final String? distinctId;

  /// The session this event was recorded in, if the SDK is tracking one.
  final String? sessionId;

  /// The screen/route this event was recorded on, if known.
  final String? screen;
  final Map<String, Object?> properties;

  /// Developer-attached flat tags (string->string). Omitted when empty.
  final Map<String, String> tags;

  /// Developer-attached structured contexts (name -> block). Omitted when empty.
  final Map<String, Map<String, Object?>> contexts;

  /// Developer-attached freeform extra (JSON). Omitted when empty.
  final Map<String, Object?> extra;

  @override
  String get type => 'event';

  @override
  Map<String, Object?> toJson() {
    final Map<String, Object?> json = <String, Object?>{
      'type': type,
      'name': name,
      'distinct_id': distinctId,
      'timestamp': sauronIso(timestamp),
      'properties': properties,
      'session_id': sessionId,
      'screen': screen,
    };
    if (tags.isNotEmpty) {
      json['tags'] = tags;
    }
    if (contexts.isNotEmpty) {
      json['contexts'] = contexts;
    }
    if (extra.isNotEmpty) {
      json['extra'] = extra;
    }
    return json;
  }
}

/// A user-identification item (from `identify`).
class IdentifyItem extends EnvelopeItem {
  IdentifyItem({
    required this.distinctId,
    this.anonymousId,
    Map<String, Object?>? traits,
  }) : traits = traits ?? const <String, Object?>{};

  final String distinctId;
  final String? anonymousId;
  final Map<String, Object?> traits;

  @override
  String get type => 'identify';

  @override
  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'distinct_id': distinctId,
        'anonymous_id': anonymousId,
        'traits': traits,
      };
}

/// A batch of breadcrumbs not attached to any specific error.
class BreadcrumbBatchItem extends EnvelopeItem {
  BreadcrumbBatchItem({
    required this.breadcrumbs,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now().toUtc();

  final List<Breadcrumb> breadcrumbs;
  final DateTime timestamp;

  @override
  String get type => 'breadcrumb_batch';

  @override
  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'timestamp': sauronIso(timestamp),
        'breadcrumbs':
            breadcrumbs.map((Breadcrumb crumb) => crumb.toJson()).toList(),
      };
}

/// A performance transaction: one timed operation (navigation, HTTP call,
/// resource fetch, screen load, or a custom span). Aggregated server-side into
/// latency percentiles.
///
/// Wire shape:
/// `{ "type": "transaction", "name", "op", "duration_ms", "status"?,
///    "http_method"?, "http_status"?, "url"?, "distinct_id"?, "session_id"?,
///    "timestamp" }`.
class TransactionItem extends EnvelopeItem {
  TransactionItem({
    required this.name,
    required this.durationMs,
    this.op = 'custom',
    this.status,
    this.httpMethod,
    this.httpStatus,
    this.url,
    this.distinctId,
    this.sessionId,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now().toUtc();

  /// Route / screen / operation label — the grouping key on the dashboard.
  final String name;

  /// Operation class:
  /// `navigation | http | resource | screen_load | custom`.
  final String op;

  /// Duration of the operation in fractional milliseconds.
  final double durationMs;

  /// Free-form outcome, e.g. `ok`, `error`, or an HTTP status class.
  final String? status;

  /// HTTP verb for `http` transactions, e.g. `GET`.
  final String? httpMethod;

  /// HTTP response status code for `http` transactions.
  final int? httpStatus;

  /// Request URL for `http` / `resource` transactions.
  final String? url;

  /// The distinct id of the current user, if known.
  final String? distinctId;

  /// The session this transaction belongs to, if the SDK is tracking one.
  final String? sessionId;

  /// When the transaction was recorded (UTC).
  final DateTime timestamp;

  @override
  String get type => 'transaction';

  @override
  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'name': name,
        'op': op,
        'duration_ms': durationMs,
        'status': status,
        'http_method': httpMethod,
        'http_status': httpStatus,
        'url': url,
        'distinct_id': distinctId,
        'session_id': sessionId,
        'timestamp': sauronIso(timestamp),
      };
}

/// A complete envelope: header + context + items.
class Envelope {
  const Envelope({
    required this.header,
    required this.context,
    required this.items,
  });

  final EnvelopeHeader header;
  final SauronContext context;
  final List<EnvelopeItem> items;

  Map<String, Object?> toJson() => <String, Object?>{
        'header': header.toJson(),
        'context': context.toJson(),
        'items': items.map((EnvelopeItem item) => item.toJson()).toList(),
      };

  /// Compact JSON encoding used on the wire and in the offline queue.
  String encode() => jsonEncode(toJson());
}
