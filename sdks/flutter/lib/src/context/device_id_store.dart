import 'dart:io';

import '../util/prefs_store.dart';
import '../util/uuid.dart';

/// Persists a stable, per-install device identity in the SDK prefs file.
///
/// The value lives under [kDeviceIdKey] in the shared [PrefsStore] — a
/// read-modify-write map, so adding a key here does not disturb the anonymous
/// id stored beside it. The device id is generated once, on first run, and
/// reused for the lifetime of the install. The backend treats
/// `context.device.device_id` as the stable device identity.
///
/// Every operation is defensively guarded: a read/write failure must never
/// prevent an error from being reported — a fresh id is minted for the current
/// run instead.
class DeviceIdStore {
  const DeviceIdStore({this.prefs = const PrefsStore()});

  /// Prefs key under which the device id lives.
  static const String kDeviceIdKey = 'sauron.device_id';

  /// The prefs file this store reads and writes.
  final PrefsStore prefs;

  /// Returns the persisted device id from [directory], generating and
  /// persisting a fresh UUID on first run. Never throws.
  Future<String> resolve(Directory directory) async {
    final Object? existing = (await prefs.read(directory))[kDeviceIdKey];
    if (existing is String && existing.isNotEmpty) {
      return existing;
    }
    final String id = generateUuidV4();
    await prefs.merge(directory, <String, Object?>{kDeviceIdKey: id});
    return id;
  }
}
