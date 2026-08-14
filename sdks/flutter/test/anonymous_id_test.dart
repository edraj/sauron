import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';
import 'package:sauron_flutter/src/context/anonymous_id_store.dart';
import 'package:sauron_flutter/src/context/device_id_store.dart';
import 'package:sauron_flutter/src/context/last_identified_store.dart';
import 'package:sauron_flutter/src/util/prefs_store.dart';

class _MockClient extends Mock implements http.Client {}

/// A [PrefsStore] whose [read] and [merge] always fail, used to prove
/// [LastIdentifiedStore] degrades to a no-op/`null` instead of propagating a
/// storage failure (a full disk, a permission error) into the host app.
class _ThrowingPrefsStore extends PrefsStore {
  const _ThrowingPrefsStore();

  @override
  Future<Map<String, Object?>> read(Directory directory) async {
    throw const FileSystemException('simulated read failure');
  }

  @override
  Future<void> merge(Directory directory, Map<String, Object?> values) async {
    throw const FileSystemException(
        'simulated write failure (e.g. quota/disk full)');
  }
}

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
    // The envelope-level `context` block, captured alongside the items. The
    // scope user lives HERE, not on the items, so a test about what identify()
    // does to the scope user has to read this — reading items alone cannot see
    // the bug at all.
    final List<Map<String, Object?>> contexts = <Map<String, Object?>>[];

    setUp(() {
      items.clear();
      contexts.clear();
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
        final Object? ctx = envelope['context'];
        if (ctx is Map<String, dynamic>) {
          contexts.add(ctx.cast<String, Object?>());
        }
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
      await client.identify('u_123');
      await deliver(client);

      expect(ofType('identify').single['anonymous_id'], client.anonymousId);
      expect(ofType('identify').single['distinct_id'], 'u_123');
      // The event that came first still carries the anonymous id: that is the
      // activity the alias row exists to stitch on.
      expect(ofType('event').single['distinct_id'], client.anonymousId);
    });

    test('identify() on a first-ever launch sends no alias', () async {
      final SauronClient client = await buildClient();
      await client.identify('u_123');
      await deliver(client);

      // Nothing was ever observed anonymously, so there is nothing to link —
      // and `process_identify` would have written a permanent alias row.
      expect(ofType('identify').single['anonymous_id'], isNull);
    });

    test('an identified user is never attributed to the anonymous id',
        () async {
      final SauronClient client = await buildClient();
      await client.identify('u_123');
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
      await client.identify('u_456');
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

    // A forgotten reset() on logout is silently expensive: server-side, an
    // anonymous id binds to a person exactly once and can never be
    // re-pointed, so without these two behaviours person B's pre-login
    // activity is attributed to person A forever, with no client-side
    // symptom.
    group('identity switch', () {
      // The privacy property this whole file relies on: `hashIdentity` is
      // pinned to a known, byte-exact value, and `identify()`'s ACTUAL
      // on-disk write is checked against it directly — not inferred from
      // switch-detection behavior, which a consistent break (drop
      // `hashIdentity`, compare/store the raw id on both sides) would not
      // disturb: every other test in this file would stay green while
      // `sauron.last_identified` silently filled with plaintext ids.
      test('hashIdentity matches the browser SDK byte-for-byte (cross-SDK '
          'parity golden)', () {
        // Independently computed from the browser SDK's `fnv1a32`/
        // `hashIdentity` in sdks/js/src/identity.ts and cross-checked with a
        // standalone Node run — not derived from this Dart implementation.
        expect(hashIdentity('ahmed'), '33962cf2ad3cb681');
        expect(hashIdentity('sara'), 'b8f66470861ed579');
        expect(hashIdentity(''), '811c9dc5040c5b8c');
        expect(hashIdentity('42'), '87e385838637386e');
      });

      test('identify() persists a DIGEST of the identity under '
          'sauron.last_identified, never the raw value', () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing');
        await client.identify('ahmed@example.com');
        await client.close();

        final Object? stored =
            (await readPrefs())[LastIdentifiedStore.kLastIdentifiedKey];
        // The stored bytes are `<tag>:<digest>`. Pinned as a literal rather
        // than built from `kFormatTag`, because the browser SDK writes this
        // exact string under this exact key and the two must stay
        // byte-compatible — recomputing the prefix from the constant would
        // follow a one-sided change instead of catching it.
        expect(stored, 'v1:${hashIdentity('ahmed@example.com')}');
        expect(stored, isNot('ahmed@example.com'),
            reason: 'the raw identity must never reach disk — it is often '
                'an email or username, and a plaintext copy here is a '
                'retention/consent regression, not just a bug');
      });

      // Without a format tag, a stored digest in a shape this build no longer
      // produces is indistinguishable from a DIFFERENT person's digest: both
      // compare unequal, both read as a switch. `hashIdentity` already
      // changed width once (8 hex digits -> 16), so this is the
      // shipped-and-widened case, not a hypothetical — and it would mint a
      // fresh anonymous id and rotate the session for every returning user on
      // their next identify(), once, silently.
      // Each fixture is a value that would compare UNEQUAL to `hashIdentity`'s
      // current output for the person identifying below — the untagged one is
      // deliberately sara's digest, not ahmed's, so removing the tag check
      // makes these fail on BEHAVIOUR (a spurious switch), not only on the
      // store-level read.
      for (final (String label, String raw) in <(String, String)>[
        ('an untagged (pre-v1) value', 'b8f66470861ed579'),
        ('a newer format tag', 'v2:whatever-that-turns-out-to-be'),
        ('an empty payload behind a known tag', 'v1:'),
      ]) {
        test('reads $label as "no previous identity", not as a switch',
            () async {
          await writePrefs(<String, Object?>{
            LastIdentifiedStore.kLastIdentifiedKey: raw,
          });
          expect(await const LastIdentifiedStore().read(dir), isNull,
              reason: 'the store itself must refuse to hand back a value it '
                  'cannot read, or every caller has to re-derive the check');

          final SauronClient client = await buildClient();
          client.track('viewed_pricing'); // marks the anon id used
          final String? anonBefore = client.anonymousId;

          final IdentifyPreparation prep =
              await client.prepareIdentify('ahmed');

          expect(prep.switched, isFalse,
              reason: 'an unreadable record means "nobody has identified on '
                  'this device yet", which is never a switch');
          expect(prep.aliasOf, anonBefore,
              reason: 'so the anon id is still offered as the alias to merge');
          expect(client.anonymousId, anonBefore);
          // ...and the unreadable entry is replaced with a readable one, so
          // this degrades for exactly one identify rather than permanently.
          expect((await readPrefs())[LastIdentifiedStore.kLastIdentifiedKey],
              'v1:${hashIdentity('ahmed')}');
          await client.close();
        });
      }

      test('a different user identifying mints a fresh anonymous id',
          () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing'); // marks the anon id used
        final String? ahmedAlias =
            (await client.prepareIdentify('ahmed')).aliasOf;
        expect(ahmedAlias, isNotNull);

        client.track('viewed_pricing'); // Sara browses; reset() never called
        final String? saraAlias =
            (await client.prepareIdentify('sara')).aliasOf;

        expect(saraAlias, isNull,
            reason: 'a burned alias must never be offered to a second user');
        expect(client.anonymousId, isNot(equals(ahmedAlias)));
        await client.close();
      });

      test('a different user identifying also rotates the session id',
          () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing');
        await client.prepareIdentify('ahmed');
        final String sessionBeforeSwitch = client.sessionId;

        client.track('viewed_pricing'); // Sara browses; reset() never called
        await client.prepareIdentify('sara');

        expect(client.sessionId, isNot(equals(sessionBeforeSwitch)),
            reason: 'otherwise one sessions row could end up representing '
                'both people, recording only whichever wrote last '
                '(bump_session is last-write-wins on distinct_id)');
        await client.close();
      });

      test('a switch clears the previous person\'s email and traits from the '
          'scope user', () async {
        final SauronClient client = await buildClient();

        await client.identify('ahmed');
        // The app supplies contact details for Ahmed, as apps do.
        client.setUser(const SauronUser(
          id: 'ahmed',
          email: 'ahmed@example.com',
          traits: <String, Object?>{'plan': 'gold'},
        ));

        // Sara logs in on the same device; reset() was never called. This is
        // exactly the boundary the switch detection exists to police.
        await client.identify('sara');
        client.track('viewed_pricing');
        await deliver(client);

        final Map<String, Object?> user =
            contexts.last['user']! as Map<String, Object?>;
        expect(user['id'], 'sara');
        expect(user['email'], isNull,
            reason: 'the scope user is attached to EVERY envelope, so carrying '
                'Ahmed\'s email forward stamps it onto every event, error and '
                'session recorded under Sara\'s distinct_id — a cross-user PII '
                'leak that lasts the whole process, not one guest window');
        expect(user['traits'], isEmpty,
            reason: 'traits belong to the previous person for the same reason '
                'the email does');
      });

      test('a same-user re-identify still carries email and traits forward',
          () async {
        final SauronClient client = await buildClient();

        await client.identify('ahmed');
        client.setUser(const SauronUser(
          id: 'ahmed',
          email: 'ahmed@example.com',
          traits: <String, Object?>{'plan': 'gold'},
        ));

        // The same person re-identifying — a token refresh, a trait update.
        // No switch, so nothing may be discarded: this is the control that
        // stops the fix above from being implemented as "always clear".
        await client.identify('ahmed');
        client.track('viewed_pricing');
        await deliver(client);

        final Map<String, Object?> user =
            contexts.last['user']! as Map<String, Object?>;
        expect(user['id'], 'ahmed');
        expect(user['email'], 'ahmed@example.com');
        expect((user['traits']! as Map<String, Object?>)['plan'], 'gold');
      });

      test('the same user identifying twice in a row is never treated as a '
          'switch', () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing');
        await client.identify('ahmed');
        final String? anonAfterFirst = client.anonymousId;
        final String sessionAfterFirst = client.sessionId;

        client.track('still_ahmed');
        await client.identify('ahmed'); // re-identifies as the SAME person

        expect(client.anonymousId, anonAfterFirst,
            reason: 're-identifying as the same person must not churn the '
                'anonymous id');
        expect(client.sessionId, sessionAfterFirst,
            reason: 're-identifying as the same person must not rotate the '
                'session either');
        await client.close();
      });

      test('reset() rotates the session id', () async {
        final SauronClient client = await buildClient();
        final String before = client.sessionId;

        await client.reset();

        expect(client.sessionId, isNot(equals(before)));
        await client.close();
      });

      test('reset() clears the last identified record, so the very next '
          'identify() is never falsely treated as a switch', () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing');
        await client.identify('ahmed');

        await client.reset();

        // A person's first-ever identify() right after a clean reset() must
        // not be treated as a switch: reset() already handed them a fresh,
        // unused anonymous id. If the stale 'ahmed' digest had survived the
        // reset, this identify would be misread as a switch, discarding
        // sara's own legitimate pre-identify activity for nothing.
        client.track('viewed_pricing'); // marks the reset-minted anon id used
        final String? freshAnon = client.anonymousId;
        await client.identify('sara');
        await deliver(client);

        expect(ofType('identify').last['anonymous_id'], freshAnon);
        expect(client.anonymousId, freshAnon,
            reason: 'no further anon id churn should have happened');
      });

      test('the public identify() sends the real alias for the first user '
          'and null for a switch, on the wire', () async {
        final SauronClient client = await buildClient();

        client.track('page_view'); // marks the anon id used
        await client.identify('ahmed');
        await Future<void>.delayed(const Duration(milliseconds: 50));
        await client.flush();

        // Logout was never wired. Sara browses under ahmed's device, then
        // logs in.
        client.track('page_view');
        await client.identify('sara');
        await deliver(client); // flush + close

        final List<Map<String, Object?>> identifies = ofType('identify');
        expect(identifies, hasLength(2));
        expect(identifies.first['anonymous_id'], matches(RegExp(r'^anon_')));
        expect(identifies.last['anonymous_id'], isNull);
      });

      test('prepareIdentify degrades gracefully before bootstrap (no '
          'storage resolved yet)', () async {
        final SauronOptions options = SauronOptions()
          ..dsn = 'https://pk_test@localhost:9/1'
          ..httpClient = httpClient
          ..gzipThresholdBytes = 1 << 30;
        final SauronClient client = SauronClient(options);

        // Never bootstrapped: no anonymous id has been resolved yet.
        final String? alias = (await client.prepareIdentify('ahmed')).aliasOf;

        expect(alias, isNull);
        expect(client.anonymousId, isNull);
        await client.close();
      });

      test('LastIdentifiedStore.write does not propagate a storage failure',
          () async {
        const LastIdentifiedStore throwing =
            LastIdentifiedStore(prefs: _ThrowingPrefsStore());
        // Must not throw.
        await throwing.write(dir, 'deadbeefdeadbeef');
      });

      test('LastIdentifiedStore.read returns null instead of propagating a '
          'storage failure', () async {
        const LastIdentifiedStore throwing =
            LastIdentifiedStore(prefs: _ThrowingPrefsStore());
        expect(await throwing.read(dir), isNull);
      });

      test('LastIdentifiedStore.read treats an empty string as absent — a '
          'Dart-side divergence from the browser SDK, where "" is a valid '
          '(falsy but real) digest', () async {
        // Documented, not fixed: `hashIdentity` never actually produces an
        // empty digest (it is always 16 hex chars), so this path is
        // unreachable through `identify()`/`prepareIdentify` — this pins the
        // store-level behavior directly, mirroring the identical
        // `existing.isNotEmpty` guard already used by `AnonymousIdStore`.
        const LastIdentifiedStore store = LastIdentifiedStore();
        await store.write(dir, '');
        expect(await store.read(dir), isNull);
      });

      test('a lost persisted last-identified value still falls back to the '
          'in-memory one, so a real switch is still caught', () async {
        final SauronClient client = await buildClient();

        client.track('viewed_pricing');
        await client.identify('ahmed'); // persists a digest for 'ahmed'

        // Simulate the persisted copy being lost after the fact (disk-level
        // corruption/failure): `read()` must fall back to the client's own
        // in-memory record of what it just wrote, not read this as "nobody
        // has ever identified" — that hole is exactly what would silently
        // disable switch detection for the rest of the process's life.
        await prefsFile().writeAsString('{not json', flush: true);

        client.track('viewed_pricing'); // marks the still-current anon id used
        final String? anonBeforeSwitch = client.anonymousId;
        await client.identify('sara');
        await deliver(client);

        expect(ofType('identify').last['anonymous_id'], isNull,
            reason: 'the in-memory last-identified digest must still catch '
                'the switch even though the persisted copy was lost');
        expect(client.anonymousId, isNot(anonBeforeSwitch));
      });
    });
  });
}
