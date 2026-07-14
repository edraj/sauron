/// Core value types shared across the Sauron SDK and the LOCKED wire contract.
///
/// Every [Object] here knows how to serialize itself into the exact JSON shape
/// consumed by the Sauron ingest gateway (and mirrored by the JS SDK).
library;

/// Severity levels understood by the ingest pipeline.
///
/// The wire value is the enum [Enum.name] (`debug|info|warning|error|fatal`).
enum SauronLevel {
  debug,
  info,
  warning,
  error,
  fatal,
}

/// Formats a [DateTime] as an ISO-8601 UTC string, e.g.
/// `2026-07-12T10:30:00.123Z`.
///
/// Always normalizes to UTC first so the trailing `Z` is emitted.
String sauronIso(DateTime dateTime) => dateTime.toUtc().toIso8601String();

/// Describes *how* an exception reached the SDK (which capture layer, and
/// whether the app handled it explicitly).
class Mechanism {
  const Mechanism({required this.type, this.handled = false});

  /// The capture layer, e.g. `PlatformDispatcher.onError`, `FlutterError.onError`,
  /// `runZonedGuarded`, `Isolate.addErrorListener`, or `manual`.
  final String type;

  /// Whether the error was handled by the application (vs. an uncaught crash).
  final bool handled;

  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'handled': handled,
      };
}

/// A single stack frame in a normalized, symbolication-agnostic form.
class StackFrame {
  const StackFrame({
    this.function,
    this.filename,
    this.lineno,
    this.colno,
    this.inApp = false,
  });

  /// The function/method name (or a raw program-counter symbol for AOT frames).
  final String? function;

  /// The source file, e.g. `package:app/main.dart` or `dart:async/zone.dart`.
  final String? filename;

  /// 1-based line number, when known.
  final int? lineno;

  /// 1-based column number, when known.
  final int? colno;

  /// Whether the frame belongs to the user's application (vs. framework/SDK).
  final bool inApp;

  Map<String, Object?> toJson() => <String, Object?>{
        'function': function,
        'filename': filename,
        'lineno': lineno,
        'colno': colno,
        'in_app': inApp,
      };
}

/// A normalized exception: type, message, mechanism and raw stack frames.
class SauronException {
  const SauronException({
    required this.type,
    required this.value,
    required this.mechanism,
    this.stacktrace = const <StackFrame>[],
  });

  /// The exception class name, e.g. `StateError`.
  final String type;

  /// The exception message.
  final String value;

  /// How the exception was captured.
  final Mechanism mechanism;

  /// Raw stack frames (no symbolication is performed on-device).
  final List<StackFrame> stacktrace;

  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'value': value,
        'mechanism': mechanism.toJson(),
        'stacktrace':
            stacktrace.map((StackFrame frame) => frame.toJson()).toList(),
      };
}

/// A breadcrumb: a lightweight event leading up to an error.
class Breadcrumb {
  Breadcrumb({
    required this.type,
    required this.category,
    this.message,
    this.level = SauronLevel.info,
    DateTime? timestamp,
    Map<String, Object?>? data,
  })  : timestamp = timestamp ?? DateTime.now().toUtc(),
        data = data ?? const <String, Object?>{};

  /// Coarse breadcrumb type, e.g. `navigation`, `http`, `ui`, `log`.
  final String type;

  /// Finer-grained category, e.g. `route`, `xhr`, `click`.
  final String category;

  /// Human-readable message.
  final String? message;

  /// Severity of the breadcrumb.
  final SauronLevel level;

  /// When the breadcrumb occurred (UTC).
  final DateTime timestamp;

  /// Arbitrary structured payload.
  final Map<String, Object?> data;

  /// Convenience constructor for navigation breadcrumbs.
  factory Breadcrumb.navigation(String route, {Map<String, Object?>? data}) =>
      Breadcrumb(
        type: 'navigation',
        category: 'route',
        message: route,
        data: data,
      );

  /// Convenience constructor for UI breadcrumbs.
  factory Breadcrumb.ui(String message, {Map<String, Object?>? data}) =>
      Breadcrumb(
        type: 'ui',
        category: 'click',
        message: message,
        data: data,
      );

  /// Convenience constructor for log breadcrumbs.
  factory Breadcrumb.log(
    String message, {
    SauronLevel level = SauronLevel.info,
    Map<String, Object?>? data,
  }) =>
      Breadcrumb(
        type: 'log',
        category: 'console',
        message: message,
        level: level,
        data: data,
      );

  Map<String, Object?> toJson() => <String, Object?>{
        'type': type,
        'category': category,
        'message': message,
        'level': level.name,
        'timestamp': sauronIso(timestamp),
        'data': data,
      };
}

/// The identified (or anonymous) user attached to captured events.
class SauronUser {
  const SauronUser({
    this.id,
    this.email,
    this.traits = const <String, Object?>{},
  });

  /// Stable user id / distinct id.
  final String? id;

  /// Optional email.
  final String? email;

  /// Arbitrary user traits (plan, role, etc.).
  final Map<String, Object?> traits;

  SauronUser copyWith({
    String? id,
    String? email,
    Map<String, Object?>? traits,
  }) =>
      SauronUser(
        id: id ?? this.id,
        email: email ?? this.email,
        traits: traits ?? this.traits,
      );

  Map<String, Object?> toJson() => <String, Object?>{
        'id': id,
        'email': email,
        'traits': traits,
      };
}

/// Device descriptor:
/// `{ "family": "Apple", "model": "iPhone15,2", "arch": "arm64",
///    "device_id": "3f2504e0-4f89-41d3-9a0c-0305e82c3301" }`.
///
/// [deviceId] is a stable, per-install UUID that the backend uses as the device
/// identity. It is generated once and persisted (see `DeviceIdStore`).
class DeviceDescriptor {
  const DeviceDescriptor({this.family, this.model, this.arch, this.deviceId});

  final String? family;
  final String? model;
  final String? arch;
  final String? deviceId;

  DeviceDescriptor copyWith({
    String? family,
    String? model,
    String? arch,
    String? deviceId,
  }) =>
      DeviceDescriptor(
        family: family ?? this.family,
        model: model ?? this.model,
        arch: arch ?? this.arch,
        deviceId: deviceId ?? this.deviceId,
      );

  Map<String, Object?> toJson() => <String, Object?>{
        'family': family,
        'model': model,
        'arch': arch,
        'device_id': deviceId,
      };
}

/// OS descriptor: `{ "name": "iOS", "version": "17.5" }`.
class OsDescriptor {
  const OsDescriptor({this.name, this.version});

  final String? name;
  final String? version;

  Map<String, Object?> toJson() => <String, Object?>{
        'name': name,
        'version': version,
      };
}

/// App descriptor: `{ "version": "1.4.2", "build": "1402" }`.
class AppDescriptor {
  const AppDescriptor({this.version, this.build});

  final String? version;
  final String? build;

  Map<String, Object?> toJson() => <String, Object?>{
        'version': version,
        'build': build,
      };
}

/// Runtime descriptor: `{ "name": "Dart", "version": "3.12" }`.
class RuntimeDescriptor {
  const RuntimeDescriptor({this.name, this.version});

  final String? name;
  final String? version;

  Map<String, Object?> toJson() => <String, Object?>{
        'name': name,
        'version': version,
      };
}
