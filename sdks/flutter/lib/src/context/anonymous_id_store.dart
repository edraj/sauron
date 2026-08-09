import 'dart:io';

import '../util/prefs_store.dart';
import '../util/uuid.dart';

/// Persists the anonymous id: the `distinct_id` analytics are attributed to
/// until the app calls `identify()`.
///
/// Format and key deliberately match the browser SDK (`sdks/js/src/identity.ts`
/// stores `anon_<uuidv4>` under `sauron.anon_id`): the two client SDKs feed the
/// same `event_users` table and the same Active Users series, which counts
/// distinct `distinct_id`s per UTC day. An id shaped differently per platform
/// would split one population into two visibly different species of row, and
/// the `anon_` prefix is what makes an unidentified person recognisable as one
/// rather than as a mysterious opaque account.
///
/// **An id that already exists is adopted verbatim — never re-minted, never
/// reformatted.** Rewriting a stored id, even to "repair" its shape, is
/// indistinguishable downstream from that person uninstalling and a different
/// one appearing: their history splits and the Active Users chart shows a spike
/// of new users that never happened. That rule is what makes this store safe to
/// change again later.
///
/// Every operation is defensively guarded: a storage failure yields an id that
/// is valid for the current run instead of throwing into the host app.
class AnonymousIdStore {
  const AnonymousIdStore({this.prefs = const PrefsStore()});

  /// Prefs key under which the anonymous id lives — the same name the browser
  /// SDK uses for its `localStorage` entry.
  static const String kAnonymousIdKey = 'sauron.anon_id';

  /// The prefs file this store reads and writes.
  final PrefsStore prefs;

  /// In-flight [resolve] calls, keyed by directory.
  ///
  /// Without this, two overlapping resolves on a fresh install both read "no
  /// id", both mint a DIFFERENT one, and both persist — so one caller returns
  /// an id that is not the one on disk and will not survive the next launch.
  /// That is precisely the churn this store exists to prevent, arriving through
  /// the back door. A reviewer reproduced it with two parallel resolves.
  ///
  /// Reachable in practice: `Sauron.init` running twice (hot restart, or a
  /// re-init once a remote DSN arrives) resolves the device id and the
  /// anonymous id concurrently.
  ///
  /// Static so it spans instances — the racing callers construct their own
  /// `AnonymousIdStore`, so an instance field would guard nothing. Cleared on
  /// completion so a later `reset()` + `resolve()` re-reads rather than being
  /// handed the pre-reset id forever.
  static final Map<String, Future<String>> _inFlight = <String, Future<String>>{};

  /// The persisted anonymous id, minting and persisting one on first run.
  Future<String> resolve(Directory directory) {
    final String key = directory.path;
    final Future<String>? running = _inFlight[key];
    if (running != null) return running;
    // Installed synchronously, before the first await inside `_resolve`, so a
    // second caller in the same microtask turn sees it.
    final Future<String> pending = _resolve(directory);
    _inFlight[key] = pending;
    return pending.whenComplete(() {
      // Only clear our own entry: a `reset()` between start and finish may have
      // already installed a newer one.
      if (identical(_inFlight[key], pending)) _inFlight.remove(key);
    });
  }

  Future<String> _resolve(Directory directory) async {
    final Object? existing = (await prefs.read(directory))[kAnonymousIdKey];
    if (existing is String && existing.isNotEmpty) {
      // Verbatim, whatever its shape — see the note on this class.
      return existing;
    }
    return mintFresh(directory);
  }

  /// Mints, persists and returns a NEW anonymous id, discarding any existing
  /// one. Backs `SauronClient.reset()`, which documents when discarding one is
  /// the right thing to do.
  Future<String> mintFresh(Directory directory) async {
    final String id = 'anon_${generateUuidV4()}';
    await prefs.merge(directory, <String, Object?>{kAnonymousIdKey: id});
    return id;
  }
}
