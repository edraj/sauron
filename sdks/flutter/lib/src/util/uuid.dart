import 'dart:math';

/// Generates an RFC-4122 version-4 UUID string, e.g.
/// `3f2504e0-4f89-41d3-9a0c-0305e82c3301`.
///
/// Uses a cryptographic RNG when the platform provides one, transparently
/// falling back to a non-secure [Random] otherwise. Never throws — identity
/// generation must never take down the host app.
String generateUuidV4() {
  Random random;
  try {
    random = Random.secure();
  } on Object {
    random = Random();
  }
  final List<int> bytes = List<int>.generate(16, (_) => random.nextInt(256));
  // Per RFC 4122: set the version (4) and variant (10xx) bits.
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  final String hex =
      bytes.map((int b) => b.toRadixString(16).padLeft(2, '0')).join();
  return '${hex.substring(0, 8)}-${hex.substring(8, 12)}-'
      '${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20)}';
}
