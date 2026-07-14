/// Web fallback: gzip is unavailable, so payloads are sent uncompressed.
class SauronGzip {
  const SauronGzip._();

  /// gzip is not available on this platform (web).
  static bool get isSupported => false;

  /// Returns [data] unchanged.
  static List<int> encode(List<int> data) => data;
}
