import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// `Sauron.init` takes a [SauronOptions] object. These tests drive the facade
/// end-to-end — construct options, init, capture, flush — and assert the
/// configuration actually reached the transport.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const MethodChannel pathProvider =
      MethodChannel('plugins.flutter.io/path_provider');

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, dynamic>> envelopes = <Map<String, dynamic>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_init_test');
    // `init` without an explicit queue directory resolves one via path_provider.
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(pathProvider, (MethodCall _) async => dir.path);

    httpClient = _MockClient();
    envelopes.clear();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((Invocation invocation) async {
      final Object? body = invocation.namedArguments[const Symbol('body')];
      final List<int> bytes =
          body is String ? utf8.encode(body) : body as List<int>;
      envelopes.add(jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>);
      return http.Response('', 202);
    });
  });

  tearDown(() async {
    await Sauron.close();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(pathProvider, null);
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  test('init(SauronOptions(...)) enables the SDK and delivers an event',
      () async {
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      environment: 'staging',
      release: 'app@1.4.2+1402',
      tags: <String, String>{'tier': 'free'},
      httpClient: httpClient,
      gzipThresholdBytes: 1 << 30,
    ));

    expect(Sauron.isEnabled, isTrue);
    expect(Sauron.client!.options.environment, 'staging');

    Sauron.track('checkout_completed');
    await Sauron.flush();

    final Map<String, dynamic> envelope = envelopes.single;
    final Map<String, dynamic> header =
        envelope['header'] as Map<String, dynamic>;
    expect(header['environment'], 'staging');
    expect(header['release'], 'app@1.4.2+1402');
    final Map<String, dynamic> item =
        (envelope['items'] as List<dynamic>).single as Map<String, dynamic>;
    expect(item['name'], 'checkout_completed');
    expect((item['tags'] as Map<String, dynamic>)['tier'], 'free');
  });

  test('defaults apply when only a dsn is given', () async {
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      httpClient: httpClient,
    ));

    final SauronOptions options = Sauron.client!.options;
    expect(options.environment, 'production');
    expect(options.sampleRate, 1.0);
    expect(options.maxBreadcrumbs, 100);
    expect(options.flushInterval, const Duration(seconds: 5));
    expect(options.attachStacktrace, isTrue);
    expect(options.debug, isFalse);
    expect(options.tags, isEmpty);
  });

  test('fields stay mutable after construction', () async {
    final SauronOptions options =
        SauronOptions(dsn: 'https://pk_test@localhost:9/1')
          ..debug = true
          ..httpClient = httpClient;
    await Sauron.init(options);

    expect(Sauron.client!.options.debug, isTrue);
  });

  test('map options are copied, not aliased', () {
    final Map<String, String> tags = <String, String>{'tier': 'free'};
    final SauronOptions options = SauronOptions(tags: tags);

    tags['tier'] = 'pro';

    expect(options.tags, <String, String>{'tier': 'free'});
  });

  test('an empty dsn leaves the SDK disabled', () async {
    await Sauron.init(SauronOptions(httpClient: httpClient));

    expect(Sauron.isEnabled, isFalse);
    Sauron.track('ignored');
    await Sauron.flush();
    expect(envelopes, isEmpty);
  });
}
