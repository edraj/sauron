import 'dart:io' show Directory, Platform;

import 'package:device_info_plus/device_info_plus.dart';
import 'package:flutter/foundation.dart';
import 'package:package_info_plus/package_info_plus.dart';

import '../envelope.dart';
import '../types.dart';
import 'device_id_store.dart';

/// Collects device / OS / app / runtime context once, then caches it.
///
/// Every plugin call is defensively guarded: a failure to read one field must
/// never prevent an error from being reported.
class DeviceContextProvider {
  DeviceContextProvider({
    DeviceInfoPlugin? deviceInfo,
    DeviceIdStore? deviceIdStore,
  })  : _deviceInfo = deviceInfo ?? DeviceInfoPlugin(),
        _deviceIdStore = deviceIdStore ?? DeviceIdStore();

  final DeviceInfoPlugin _deviceInfo;
  final DeviceIdStore _deviceIdStore;

  SauronContext? _cached;

  /// The most recently loaded context, or an empty one if not yet loaded.
  SauronContext get current => _cached ?? const SauronContext();

  /// Loads (and caches) device/app/runtime context. Idempotent.
  ///
  /// When [storageDirectory] is supplied, a stable per-install `device_id` is
  /// resolved (and persisted on first run) and attached to the device
  /// descriptor.
  Future<SauronContext> load({Directory? storageDirectory}) async {
    final SauronContext? cached = _cached;
    if (cached != null) {
      return cached;
    }
    final String? deviceId = await _resolveDeviceId(storageDirectory);
    DeviceDescriptor? device = await _readDevice();
    if (deviceId != null) {
      device = (device ?? const DeviceDescriptor()).copyWith(deviceId: deviceId);
    }
    final OsDescriptor? os = await _readOs();
    final AppDescriptor? app = await _readApp();
    final RuntimeDescriptor runtime = _readRuntime();
    final SauronContext context = SauronContext(
      device: device,
      os: os,
      app: app,
      runtime: runtime,
    );
    _cached = context;
    return context;
  }

  Future<String?> _resolveDeviceId(Directory? storageDirectory) async {
    if (storageDirectory == null) {
      return null;
    }
    try {
      return await _deviceIdStore.resolve(storageDirectory);
    } on Object {
      return null;
    }
  }

  Future<DeviceDescriptor?> _readDevice() async {
    try {
      if (kIsWeb) {
        final WebBrowserInfo web = await _deviceInfo.webBrowserInfo;
        return DeviceDescriptor(
          family: web.browserName.name,
          model: web.userAgent,
          arch: null,
        );
      }
      switch (defaultTargetPlatform) {
        case TargetPlatform.android:
          final AndroidDeviceInfo info = await _deviceInfo.androidInfo;
          return DeviceDescriptor(
            family: info.manufacturer,
            model: info.model,
            arch: info.supportedAbis.isNotEmpty
                ? info.supportedAbis.first
                : null,
          );
        case TargetPlatform.iOS:
          final IosDeviceInfo info = await _deviceInfo.iosInfo;
          return DeviceDescriptor(
            family: 'Apple',
            model: info.utsname.machine,
            arch: null,
          );
        case TargetPlatform.macOS:
          final MacOsDeviceInfo info = await _deviceInfo.macOsInfo;
          return DeviceDescriptor(
            family: 'Apple',
            model: info.model,
            arch: info.arch,
          );
        case TargetPlatform.windows:
          final WindowsDeviceInfo info = await _deviceInfo.windowsInfo;
          return DeviceDescriptor(
            family: 'PC',
            model: info.productName,
            arch: null,
          );
        case TargetPlatform.linux:
          final LinuxDeviceInfo info = await _deviceInfo.linuxInfo;
          return DeviceDescriptor(
            family: info.name,
            model: info.prettyName,
            arch: null,
          );
        case TargetPlatform.fuchsia:
          return const DeviceDescriptor(family: 'Fuchsia');
      }
    } on Object {
      return null;
    }
  }

  Future<OsDescriptor?> _readOs() async {
    try {
      if (kIsWeb) {
        final WebBrowserInfo web = await _deviceInfo.webBrowserInfo;
        return OsDescriptor(name: web.platform, version: web.appVersion);
      }
      switch (defaultTargetPlatform) {
        case TargetPlatform.android:
          final AndroidDeviceInfo info = await _deviceInfo.androidInfo;
          return OsDescriptor(name: 'Android', version: info.version.release);
        case TargetPlatform.iOS:
          final IosDeviceInfo info = await _deviceInfo.iosInfo;
          return OsDescriptor(
            name: info.systemName,
            version: info.systemVersion,
          );
        case TargetPlatform.macOS:
          final MacOsDeviceInfo info = await _deviceInfo.macOsInfo;
          return OsDescriptor(
            name: 'macOS',
            version:
                '${info.majorVersion}.${info.minorVersion}.${info.patchVersion}',
          );
        case TargetPlatform.windows:
          final WindowsDeviceInfo info = await _deviceInfo.windowsInfo;
          return OsDescriptor(name: 'Windows', version: info.displayVersion);
        case TargetPlatform.linux:
          final LinuxDeviceInfo info = await _deviceInfo.linuxInfo;
          return OsDescriptor(
            name: 'Linux',
            version: info.versionId ?? info.version,
          );
        case TargetPlatform.fuchsia:
          return const OsDescriptor(name: 'Fuchsia');
      }
    } on Object {
      return null;
    }
  }

  Future<AppDescriptor?> _readApp() async {
    try {
      final PackageInfo info = await PackageInfo.fromPlatform();
      return AppDescriptor(version: info.version, build: info.buildNumber);
    } on Object {
      return null;
    }
  }

  RuntimeDescriptor _readRuntime() {
    return RuntimeDescriptor(name: 'Dart', version: _dartVersion());
  }

  String? _dartVersion() {
    if (kIsWeb) {
      return null;
    }
    try {
      // Platform.version looks like "3.12.2 (stable) (...) on ...".
      final String raw = Platform.version.split(' ').first;
      final List<String> parts = raw.split('.');
      if (parts.length >= 2) {
        return '${parts[0]}.${parts[1]}';
      }
      return raw;
    } on Object {
      return null;
    }
  }
}
