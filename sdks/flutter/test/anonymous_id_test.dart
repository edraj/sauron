import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';
import 'package:sauron_flutter/src/context/anonymous_id_store.dart';
import 'package:sauron_flutter/src/context/device_id_store.dart';
import 'package:sauron_flutter/src/util/prefs_store.dart';

class _MockClient extends Mock implements http.Client {}

/// The anonymous id is the `distinct_id` an unidentified person is counted
/// under, and Active Users is `count(DISTINCT distinct_id)` per UTC day. So the
/// property these tests defend is not "an id exists" but "the SAME id comes
/// back" — an id that re-mints on upgrade or on every launch reads as a crowd
/// of new users who never arrived, and nothing else in the system can tell the
/// difference.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const AnonymousIdStore store = AnonymousIdStore();
  const DeviceIdStore deviceStore = DeviceIdStore();

  late Directory dir;

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_anon_id_test');
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  File prefsFile() => File('${dir.path}/sauron_prefs.json');

  Future<Map<String, Object?>> readPrefs() async =>
      (jsonDecode(await prefsFile().readAsString()) as Map<String, dynamic>)
          .cast<String, Object?>();

  Future<void> writePrefs(Map<String, Object?> values) async {
    await prefsFile().writeAsString(jsonEncode(values), flush: true);
  }

  group('generation', () {
    test('a fresh install mints anon_<uuidv4> and persists it under '
        'sauron.anon_id', () async {
      final String id = await store.resolve(dir);

      // Shape matches the browser SDK's `anon_${uuidv4()}` exactly — the two
      // client SDKs land in the same `distinct_id` column.
      expect(
        id,
        matches(RegExp(
            r'^anon_[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$')),
      );
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey], id);
    });

    test('the id is stable across launches', () async {
      final String first = await store.resolve(dir);
      // A second launch: nothing in memory, everything on disk.
      expect(await const AnonymousIdStore().resolve(dir), first);
    });

    test('two installs get different ids', () async {
      final Directory other =
          await Directory.systemTemp.createTemp('sauron_anon_id_other');
      addTearDown(() => other.delete(recursive: true));

      expect(await store.resolve(dir), isNot(await store.resolve(other)));
    });
  });

  group('migration', () {
    test('an existing id in an older format is adopted verbatim, not '
        're-minted or re-prefixed', () async {
      // A bare UUID — no `anon_` prefix. Repairing the shape would be
      // indistinguishable, downstream, from this person being a new one.
      const String legacy = '3f2504e0-4f89-41d3-9a0c-0305e82c3301';
      await writePrefs(<String, Object?>{
        AnonymousIdStore.kAnonymousIdKey: legacy,
      });

      expect(await store.resolve(dir), legacy);
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey], legacy);
    });

    test('an install that predates the anonymous id keeps its device id',
        () async {
      // What every upgrading install actually looks like: a prefs file holding
      // only the device id. Adding a key must not rewrite the file wholesale —
      // a churning device_id splits the device dimension the same way a
      // churning anon id splits the user one.
      final String deviceId = await deviceStore.resolve(dir);
      expect((await readPrefs()).keys, <String>[DeviceIdStore.kDeviceIdKey]);

      final String anonId = await store.resolve(dir);

      final Map<String, Object?> prefs = await readPrefs();
      expect(prefs[DeviceIdStore.kDeviceIdKey], deviceId);
      expect(prefs[AnonymousIdStore.kAnonymousIdKey], anonId);
      // And the reverse order, on the next launch: neither store may clobber
      // the other's key.
      expect(await deviceStore.resolve(dir), deviceId);
      expect(await store.resolve(dir), anonId);
    });

    test('keys written by a newer SDK version survive a write', () async {
      await writePrefs(<String, Object?>{'sauron.from_the_future': 'keep me'});

      await store.resolve(dir);

      expect((await readPrefs())['sauron.from_the_future'], 'keep me');
    });

    test('a corrupt prefs file yields a usable id and is repaired', () async {
      await prefsFile().writeAsString('{not json', flush: true);

      final String id = await store.resolve(dir);

      expect(id, startsWith('anon_'));
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey], id);
    });

    test('an empty stored value is treated as absent', () async {
      await writePrefs(<String, Object?>{AnonymousIdStore.kAnonymousIdKey: ''});

      expect(await store.resolve(dir), startsWith('anon_'));
    });
  });

  // Concurrency. Both of these were REPRODUCED as bugs by a reviewer before
  // being fixed; neither is a hypothetical. The failure mode they cause is the
  // same one the whole file exists to prevent — a person's id changing — so a
  // regression here would look like organic growth in Active Users rather than
  // like a bug.
  group('concurrency', () {
    test('two overlapping resolves agree, and agree with what is on disk', () async {
      // Reachable via a double Sauron.init: hot restart, or a re-init once a
      // remote DSN arrives. Before the single-flight guard these returned two
      // different ids and only one reached the file.
      final List<String> ids = await Future.wait<String>(<Future<String>>[
        store.resolve(dir),
        store.resolve(dir),
        store.resolve(dir),
      ]);
      expect(ids.toSet(), hasLength(1), reason: 'every caller must see one id');
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey], ids.first,
          reason: 'the id handed out must be the id that persisted');
    });

    test('a concurrent device-id write does not clobber the anonymous id', () async {
      // Two DIFFERENT stores writing the same prefs file. Read-modify-write
      // without serialization loses whichever key was written first.
      final List<Object> results = await Future.wait<Object>(<Future<Object>>[
        store.resolve(dir),
        deviceStore.resolve(dir),
      ]);
      final Map<String, Object?> prefs = await readPrefs();
      expect(prefs[AnonymousIdStore.kAnonymousIdKey], results[0]);
      expect(prefs[DeviceIdStore.kDeviceIdKey], results[1]);
    });

    test('a resolve after a completed one still re-reads, so reset is visible', () async {
      // The single-flight entry must be cleared on completion. If it were
      // cached for the process lifetime, reset() would be invisible.
      final String first = await store.resolve(dir);
      final String minted = await store.mintFresh(dir);
      expect(minted, isNot(first));
      expect(await store.resolve(dir), minted);
    });

    test('many interleaved writes keep every key and leave no temp files', () async {
      // NOTE ON WHAT THIS DOES AND DOES NOT PROVE. A mutation run reverting
      // `merge` to the old truncate-then-write did NOT fail this test — a
      // single Dart isolate does not interleave finely enough to observe a torn
      // file. So this pins the serialization (no key lost across 25 writers)
      // and the temp-file hygiene, NOT atomicity. Atomicity guards
      // process death mid-write, which no in-process test can stage; the
      // argument for it is in `PrefsStore`'s class doc, and the sibling
      // 'concurrent device-id write' test is what actually discriminates the
      // locking.
      await store.resolve(dir);
      final List<Future<void>> work = <Future<void>>[];
      for (int i = 0; i < 25; i++) {
        work.add(const PrefsStore().merge(dir, <String, Object?>{'k$i': i}));
        work.add(() async {
          final Map<String, Object?> seen = await const PrefsStore().read(dir);
          // `read` swallows corruption and returns {}, so an empty map IS the
          // symptom of a torn file here — the anon id can never legitimately
          // vanish once written.
          expect(seen[AnonymousIdStore.kAnonymousIdKey], isNotNull,
              reason: 'a torn or truncated write would read as no prefs at all');
        }());
      }
      await Future.wait(work);
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey], isNotNull);
      // No temp files left behind.
      final List<FileSystemEntity> stray =
          dir.listSync().where((FileSystemEntity e) => e.path.endsWith('.tmp')).toList();
      expect(stray, isEmpty, reason: 'a failed or completed write must leave no .tmp');
    });
  });

  group('reset', () {
    test('mintFresh replaces the persisted id', () async {
      final String first = await store.resolve(dir);
      final String second = await store.mintFresh(dir);

      expect(second, isNot(first));
      expect(await store.resolve(dir), second);
    });
  });

  group('client', () {
    late _MockClient httpClient;
    final List<Map<String, Object?>> items = <Map<String, Object?>>[];

    setUp(() {
      items.clear();
      httpClient = _MockClient();
      when(() => httpClient.post(
            any(),
            headers: any(named: 'headers'),
            body: any(named: 'body'),
          )).thenAnswer((Invocation invocation) async {
        final Object? body = invocation.namedArguments[const Symbol('body')];
        final List<int> bytes =
            body is String ? utf8.encode(body) : body as List<int>;
        final Map<String, dynamic> envelope =
            jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>;
        for (final dynamic item in envelope['items'] as List<dynamic>) {
          items.add((item as Map<String, dynamic>).cast<String, Object?>());
        }
        return http.Response('', 202);
      });
    });

    Future<SauronClient> buildClient() async {
      final SauronOptions options = SauronOptions()
        ..dsn = 'https://pk_test@localhost:9/1'
        ..httpClient = httpClient
        ..gzipThresholdBytes = 1 << 30;
      final SauronClient client = SauronClient(options);
      await client.bootstrap(queueDirectory: dir);
      return client;
    }

    Future<void> deliver(SauronClient client) async {
      await Future<void>.delayed(const Duration(milliseconds: 50));
      await client.flush();
      await client.close();
    }

    List<Map<String, Object?>> ofType(String type) =>
        items.where((Map<String, Object?> i) => i['type'] == type).toList();

    test('track() before identify() is attributed to the anonymous id',
        () async {
      final SauronClient client = await buildClient();
      client.track('viewed_pricing');
      await deliver(client);

      expect(ofType('event'), hasLength(1));
      expect(ofType('event').single['distinct_id'], client.anonymousId);
      expect(client.anonymousId, startsWith('anon_'));
    });

    test('a relaunch reports the same anonymous id', () async {
      final SauronClient first = await buildClient();
      final String? id = first.anonymousId;
      await first.close();

      final SauronClient second = await buildClient();
      expect(second.anonymousId, id);
      await second.close();
    });

    test('identify() aliases the anonymous id only after it was used',
        () async {
      final SauronClient client = await buildClient();
      client.track('viewed_pricing');
      client.identify('u_123');
      await deliver(client);

      expect(ofType('identify').single['anonymous_id'], client.anonymousId);
      expect(ofType('identify').single['distinct_id'], 'u_123');
      // The event that came first still carries the anonymous id: that is the
      // activity the alias row exists to stitch on.
      expect(ofType('event').single['distinct_id'], client.anonymousId);
    });

    test('identify() on a first-ever launch sends no alias', () async {
      final SauronClient client = await buildClient();
      client.identify('u_123');
      await deliver(client);

      // Nothing was ever observed anonymously, so there is nothing to link —
      // and `process_identify` would have written a permanent alias row.
      expect(ofType('identify').single['anonymous_id'], isNull);
    });

    test('an identified user is never attributed to the anonymous id',
        () async {
      final SauronClient client = await buildClient();
      client.identify('u_123');
      client.track('checkout_completed');
      await deliver(client);

      expect(ofType('event').single['distinct_id'], 'u_123');
    });

    test('reset() mints a new id, persists it, and drops the pending alias',
        () async {
      final SauronClient client = await buildClient();
      client.track('viewed_pricing');
      final String? before = client.anonymousId;

      await client.reset();

      expect(client.anonymousId, isNot(before));
      expect((await readPrefs())[AnonymousIdStore.kAnonymousIdKey],
          client.anonymousId);
      // The next person's identify must not inherit the previous person's id.
      client.identify('u_456');
      await deliver(client);
      expect(ofType('identify').single['anonymous_id'], isNull);
    });

    test('an item tracked before bootstrap is dropped, not sent with a null '
        'distinct_id', () async {
      final SauronOptions options = SauronOptions()
        ..dsn = 'https://pk_test@localhost:9/1'
        ..httpClient = httpClient
        ..gzipThresholdBytes = 1 << 30;
      final SauronClient client = SauronClient(options);

      // The only remaining window with no identity of either kind.
      client.track('too_early');
      expect(client.anonymousId, isNull);

      await client.bootstrap(queueDirectory: dir);
      await deliver(client);

      expect(ofType('event'), isEmpty);
    });
  });
}
