import 'dart:convert';
import 'dart:io';

/// The SDK's tiny JSON preferences file (`sauron_prefs.json` inside the SDK
/// storage directory), shared by every durable per-install identifier.
///
/// Writes go through a read-modify-write [merge] rather than a whole-file
/// replace, and that is the entire reason this type exists. Two identifiers now
/// live in this file — `sauron.device_id` and `sauron.anon_id` — resolved by
/// two separate stores; a writer that serialized only its own key would delete
/// the other's on the very next launch. A persisted id that silently re-mints
/// itself is indistinguishable, downstream, from an uninstall followed by a new
/// install: the person's history splits and every report that counts distinct
/// ids shows new users who never arrived.
///
/// [merge] also preserves keys this build knows nothing about, so a downgrade
/// does not strip a newer SDK's entries.
///
/// Every operation is defensively guarded: a read or write failure must never
/// prevent an error from being reported. A failed read reads as "no prefs yet",
/// and a failed write leaves the caller's value valid for the current run only.
///
/// ## Concurrency
///
/// Writes are serialized in-process and land atomically on disk. Both halves
/// are necessary and neither is theoretical:
///
///  * **In-process serialization.** [merge] is read-modify-write, so two
///    overlapping calls both read the pre-write map and the second overwrites
///    the first's key. That is not a hypothetical ordering — `Sauron.init`
///    running twice (hot restart, or a re-init after the DSN is fetched
///    remotely) resolves both ids concurrently, and a reviewer reproduced two
///    parallel resolves leaving a file containing only one of them.
///  * **Atomic replace.** `writeAsString` truncates and then writes. A process
///    killed between the two — which on Android is an ordinary event, not a
///    crash — leaves a zero-length or half-written file that [read] then
///    discards as corrupt, silently re-minting BOTH identifiers. Writing to a
///    sibling temp file and renaming makes the swap a single atomic operation:
///    a reader sees either the whole old file or the whole new one.
///
/// A re-minted id is indistinguishable downstream from an uninstall and
/// reinstall — the person's history splits and every distinct-id count shows
/// arrivals that never happened. That is the failure this class exists to
/// prevent, so losing it to a torn write would defeat the entire point.
class PrefsStore {
  const PrefsStore({this.fileName = 'sauron_prefs.json'});

  /// Prefs file name within the SDK storage directory.
  final String fileName;

  /// Serializes [merge] across all instances.
  ///
  /// Static because the races that matter are between two *different* store
  /// objects — `DeviceIdStore` and `AnonymousIdStore` each construct their own
  /// `PrefsStore` and write the same file. An instance field would leave
  /// exactly the collision this guards against.
  ///
  /// Keyed by file path so two different prefs files never block each other,
  /// and a test using a temp directory is not serialized behind an unrelated one.
  static final Map<String, Future<void>> _writeQueues = <String, Future<void>>{};

  File _file(Directory directory) => File('${directory.path}/$fileName');

  /// The stored map, or an empty map when the file is absent, unreadable or
  /// not a JSON object. Never throws.
  Future<Map<String, Object?>> read(Directory directory) async {
    try {
      final File file = _file(directory);
      if (await file.exists()) {
        final Object? decoded = jsonDecode(await file.readAsString());
        if (decoded is Map<String, dynamic>) {
          return Map<String, Object?>.from(decoded);
        }
      }
    } on Object {
      // A corrupt/unreadable prefs file must never crash the host app; the
      // caller mints a fresh value for this run instead.
    }
    return <String, Object?>{};
  }

  /// Writes [values] over the stored map, keeping every key not named in it.
  /// Never throws.
  ///
  /// Serialized per file and atomic on disk — see the class doc for why both
  /// are load-bearing.
  Future<void> merge(Directory directory, Map<String, Object?> values) async {
    final String path = _file(directory).path;
    // Chain onto whatever write is already queued for this file. `.then` on the
    // stored future rather than `await`ing it: the new tail must be installed
    // synchronously, before any other caller can observe the map, or two
    // callers race to read the same tail and chain in parallel.
    final Future<void> queued = (_writeQueues[path] ?? Future<void>.value())
        .then((_) => _merge(directory, values));
    // Swallow here so one failed write cannot poison every later write chained
    // behind it; `_merge` already reports nothing upward.
    _writeQueues[path] = queued.catchError((Object _) {});
    return queued;
  }

  Future<void> _merge(Directory directory, Map<String, Object?> values) async {
    final String path = _file(directory).path;
    try {
      final Map<String, Object?> merged = await read(directory);
      merged.addAll(values);
      await directory.create(recursive: true);
      // Temp file + rename. `rename` is atomic within a filesystem, and the temp
      // sits in the SAME directory so it never crosses a mount boundary (where
      // rename degrades to copy-and-delete and stops being atomic).
      //
      // The pid/timestamp suffix keeps two processes — a real possibility on
      // Android, where a background isolate or a second app process can be
      // running — from colliding on the temp name itself.
      final File tmp = File('$path.${pid}_${DateTime.now().microsecondsSinceEpoch}.tmp');
      try {
        await tmp.writeAsString(jsonEncode(merged), flush: true);
        await tmp.rename(path);
      } on Object {
        // Leave nothing behind on a failed write; a stray .tmp would otherwise
        // accumulate once per failed launch forever.
        try {
          if (await tmp.exists()) await tmp.delete();
        } on Object {
          // Best effort.
        }
        rethrow;
      }
    } on Object {
      // Persistence failure is non-fatal.
    }
  }
}
