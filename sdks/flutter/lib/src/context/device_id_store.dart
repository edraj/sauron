import 'dart:convert';
import 'dart:io';

import '../util/uuid.dart';

/// Persists a stable, per-install device identity under the SDK storage dir.
///
/// The value is stored in a tiny JSON prefs file (so more keys can be added
/// later without a schema change) keyed by [kDeviceIdKey]. The id is generated
/// once, on first run, and reused for the lifetime of the install. The backend
/// treats `context.device.device_id` as the stable device identity.
///
/// Every operation is defensively guarded: a read/write failure must never
/// prevent an error from being reported — a fresh id is minted for the current
/// run instead.
class DeviceIdStore {
  DeviceIdStore({this.fileName = 'sauron_prefs.json'});

  /// Prefs key under which the device id lives.
  static const String kDeviceIdKey = 'sauron.device_id';

  /// Prefs file name within the SDK storage directory.
  final String fileName;

  /// Returns the persisted device id from [directory], generating and
  /// persisting a fresh UUID on first run. Never throws.
  Future<String> resolve(Directory directory) async {
    final File file = File('${directory.path}/$fileName');
    try {
      if (await file.exists()) {
        final Object? decoded = jsonDecode(await file.readAsString());
        if (decoded is Map<String, dynamic>) {
          final Object? existing = decoded[kDeviceIdKey];
          if (existing is String && existing.isNotEmpty) {
            return existing;
          }
        }
      }
    } on Object {
      // A corrupt/unreadable prefs file must never crash the host app; fall
      // through and mint a fresh id below.
    }

    final String id = generateUuidV4();
    try {
      await directory.create(recursive: true);
      await file.writeAsString(
        jsonEncode(<String, Object?>{kDeviceIdKey: id}),
        flush: true,
      );
    } on Object {
      // Persistence failure is non-fatal; the id is still valid for this run.
    }
    return id;
  }
}
