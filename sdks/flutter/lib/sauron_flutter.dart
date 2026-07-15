/// Sauron — error reporting + product analytics for Flutter.
///
/// A single import exposes the public surface:
///
/// ```dart
/// import 'package:sauron_flutter/sauron_flutter.dart';
/// ```
library;

export 'src/client.dart' show SauronClient;
export 'src/dsn.dart' show Dsn;
export 'src/envelope.dart'
    show
        Envelope,
        EnvelopeHeader,
        EnvelopeItem,
        ErrorItem,
        EventItem,
        IdentifyItem,
        BreadcrumbBatchItem,
        TransactionItem,
        SauronContext,
        kSauronSdkName,
        kSauronSdkVersion;
export 'src/integrations/widgets_binding_observer.dart'
    show SauronNavigatorObserver, SauronWidgetsBindingObserver;
export 'src/sauron.dart' show Sauron;
export 'src/sauron_options.dart' show SauronOptions, BeforeSendCallback;
export 'src/stacktrace/dart_stacktrace_parser.dart' show DartStackTraceParser;
export 'src/types.dart'
    show
        AppDescriptor,
        Breadcrumb,
        DebugMeta,
        DeviceDescriptor,
        Mechanism,
        OsDescriptor,
        RuntimeDescriptor,
        SauronException,
        SauronLevel,
        SauronUser,
        StackFrame,
        isObfuscatedDartTrace,
        sauronIso;
