import 'dart:io';

/// Native gzip using `dart:io`'s [GZipCodec] (mobile/desktop).
class SauronGzip {
  const SauronGzip._();

  /// gzip is available on this platform.
  static bool get isSupported => true;

  /// Compresses [data] with gzip.
  static List<int> encode(List<int> data) => gzip.encode(data);
}
