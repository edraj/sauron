// gzip compression that degrades gracefully on platforms without `dart:io`
// (i.e. web). The concrete implementation is selected at compile time.
export 'gzip_stub.dart' if (dart.library.io) 'gzip_io.dart';
