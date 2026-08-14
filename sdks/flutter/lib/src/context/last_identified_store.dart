import 'dart:io';

import '../util/prefs_store.dart';

/// Persists a short one-way DIGEST — never the raw id — of the last user who
/// called `identify()` on this device.
///
/// Exists so `SauronClient.prepareIdentify` can detect a login by a
/// DIFFERENT user than last time even when the app never wired
/// `SauronClient.reset()` on logout. Without it, the next person's anonymous
/// activity keeps flowing under the previous person's already-burned alias,
/// and the server resolves it to them — permanently, and with no
/// client-side symptom (an anonymous id binds to a person exactly once,
/// server-side, and can never be re-pointed).
///
/// The raw id is deliberately never stored: it is very often an email or a
/// username, and persisting it verbatim on-device would create a second,
/// durable, plaintext copy of it — a retention and consent consequence, not
/// just an implementation detail. `hashIdentity` only needs to answer one
/// question ("is this the same person as last time"), which an equality
/// check over a digest answers exactly as well as the raw value would.
///
/// Format and key name deliberately match the browser SDK
/// (`sdks/js/src/identity.ts` stores the same digest under
/// `sauron.last_identified` in `localStorage`) — not because the two SDKs
/// ever compare a digest written by one platform against the other (they
/// don't; this is a same-device, same-install check), but so the two
/// implementations read as one system to anyone auditing on-device storage.
///
/// Every operation is defensively guarded: a storage failure yields `null`/a
/// no-op for the current call rather than throwing into the host app. Note
/// that this alone is not sufficient to make switch detection reliable — see
/// `SauronClient`'s own in-memory `_lastIdentifiedDigest`, which is what
/// actually closes the "a failed write leaves the read stuck returning
/// nothing forever" hole (a storage failure here reads identically to "no
/// one has ever identified", and only the caller holding its own copy of
/// what it just wrote can tell those apart).
class LastIdentifiedStore {
  const LastIdentifiedStore({this.prefs = const PrefsStore()});

  /// Prefs key under which the digest lives — the same name the browser SDK
  /// uses for its `localStorage` entry.
  static const String kLastIdentifiedKey = 'sauron.last_identified';

  /// Format tag prefixed to the STORED value, as `<tag>:<digest>`.
  ///
  /// [hashIdentity]'s output has already changed shape once (8 hex digits →
  /// 16), and an untagged store cannot tell "a digest in a format I no longer
  /// produce" from "a digest of a different person". Both compare unequal, so
  /// both read as a SWITCH — and a widening would therefore mint a fresh
  /// anonymous id and rotate the session for every returning user on their
  /// next `identify()`, once, silently, with nothing in the data saying why
  /// guest counts moved.
  ///
  /// An unrecognised (or absent) prefix reads as **no previous identity**,
  /// which is the safe direction: a first identify is never a switch, so
  /// nothing rotates and the next write re-tags the entry. The cost of
  /// guessing wrong that way is one missed switch on one device — the same
  /// exposure as before this was persisted at all.
  ///
  /// Byte-compatible with the browser SDK, which writes the identical
  /// `<tag>:<digest>` string under the same key (`LAST_IDENTIFIED_FORMAT` in
  /// `sdks/js/src/identity.ts`). The tag is on the STORED VALUE, never on
  /// [hashIdentity] itself, so the cross-SDK digest golden is untouched by it.
  static const String kFormatTag = 'v1';

  /// The prefs file this store reads and writes.
  final PrefsStore prefs;

  /// The digest of the last identified user, or null when none has ever been
  /// recorded, the value is unreadable (including "written in a format tag
  /// this build does not know"), or the read fails outright. Never throws.
  ///
  /// Returns the bare DIGEST — the tag is a storage concern and never reaches
  /// `SauronClient.prepareIdentify`'s comparison.
  Future<String?> read(Directory directory) async {
    try {
      final Object? existing =
          (await prefs.read(directory))[kLastIdentifiedKey];
      if (existing is String) {
        return _decode(existing);
      }
    } on Object {
      // A storage failure must read as "nothing persisted yet", not crash
      // the host app — see the class doc for why the caller still needs its
      // own in-memory fallback on top of this.
    }
    return null;
  }

  /// Persists [digest] as the last identified user's digest, tagged with
  /// [kFormatTag]. Never throws.
  Future<void> write(Directory directory, String digest) async {
    try {
      await prefs.merge(directory,
          <String, Object?>{kLastIdentifiedKey: '$kFormatTag:$digest'});
    } on Object {
      // Best effort — the caller's own in-memory value still applies for the
      // current run; see the class doc.
    }
  }

  /// Unwrap a stored value, or null when it is not in a format this build
  /// understands. No separator at all is the pre-tag format; a different tag
  /// is another build's. Both fail to "no previous identity" rather than to a
  /// false switch.
  static String? _decode(String raw) {
    final int sep = raw.indexOf(':');
    if (sep < 0 || raw.substring(0, sep) != kFormatTag) {
      return null;
    }
    final String digest = raw.substring(sep + 1);
    return digest.isEmpty ? null : digest;
  }

  /// Forgets the last identified user. Backs `SauronClient.reset()`, so the
  /// same person logging back in afterwards is never mistaken for a switch.
  /// Never throws.
  Future<void> clear(Directory directory) async {
    try {
      await prefs
          .merge(directory, <String, Object?>{kLastIdentifiedKey: null});
    } on Object {
      // Best effort.
    }
  }
}

/// One 32-bit FNV-1a pass over [s]'s UTF-16 code units, returned as an
/// 8-hex-digit string.
///
/// Iterates `codeUnits` — Dart's UTF-16 view of a [String] — to mirror the
/// browser SDK's `fnv1a32`, which iterates `s.charCodeAt(i)` (also UTF-16):
/// the two implementations hash a given string over the identical sequence
/// of code units. `h * prime` is masked to 32 bits after every multiply,
/// which is what makes this Dart's equivalent of JavaScript's `Math.imul` —
/// JS needs that intrinsic because its numbers are doubles and would lose
/// precision on a plain `*`; Dart's native ints already hold a 32×32-bit
/// product exactly, so masking afterwards is all that's needed.
String _fnv1a32(String s) {
  int h = 0x811c9dc5; // FNV-1a 32-bit offset basis
  for (final int unit in s.codeUnits) {
    h = (h ^ unit) & 0xffffffff;
    h = (h * 0x01000193) & 0xffffffff; // FNV-1a 32-bit prime
  }
  return h.toRadixString(16).padLeft(8, '0');
}

/// Two decorrelated 32-bit FNV-1a passes concatenated into a 16-hex-digit
/// (64-bit) digest. Byte-for-byte identical to the browser SDK's
/// `hashIdentity` for the same input (verified against it directly).
///
/// `last_identified` only ever needs an EQUALITY check ("is this the same
/// person as last time"), comparing exactly one previous digest against one
/// current digest — never a set lookup — so the birthday bound doesn't apply
/// here: a collision is ~2^-64 for one consecutive-login pair, and it fails
/// OPEN to the pre-fix behaviour for that one pair (a missed switch), never
/// worse.
///
/// The second pass is decorrelated by INPUT — a `'\x01'`-prefixed copy — not
/// by a different offset basis/prime: two FNV-1a passes over identical bytes
/// with the same constants are structurally correlated, so changing only the
/// basis buys far less independence than it looks like it does.
///
/// NOT a security boundary, and widening this does not change that: this is
/// an UNKEYED hash over what can be a low-entropy space (an email address),
/// so it is a confirmation oracle, not a secret — anyone with local read
/// access and a guess can verify it instantly by hashing the guess and
/// comparing. It exists only so `sauron.last_identified` isn't a second
/// plaintext copy of the app's user id, not to keep that id confidential.
String hashIdentity(String id) => _fnv1a32(id) + _fnv1a32('\x01$id');
