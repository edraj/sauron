import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

import 'probes.dart';

/// DSN points at the host's ingest through `adb reverse tcp:8081 tcp:8081`.
/// The trailing path segment is parsed-and-discarded by the ingest route;
/// identity comes from the public key.
const String kDsn = String.fromEnvironment(
  'SAURON_DSN',
  defaultValue:
      'http://pk_8620df9da9730735169d7224654bfdc7@127.0.0.1:8081/00000000-0000-0000-0000-000000000000',
);

Future<void> main() async {
  // Cleartext pre-flight runs FIRST and prints an explicit verdict, so a
  // delivery failure can never be mistaken for a symbolication failure.
  await cleartextPreflight();

  final options = SauronOptions()
    ..dsn = kDsn
    ..debug = true
    ..release = 'flutter_symbol_probe@1.0.0'
    ..attachStacktrace = true;

  await Sauron.init(options, appRunner: () async {
    runApp(const ProbeApp());
    Timer(const Duration(seconds: 3), runAllProbes);
  });
}

Future<void> runAllProbes() async {
  for (final entry in kProbes.entries) {
    try {
      entry.value();
    } catch (e, st) {
      print('SAURON_TRACE_BEGIN id=${entry.key}');
      print('SAURON_MSG ${e.toString()}');
      // Print frame-per-line so logcat cannot truncate the tail of the trace.
      for (final line in st.toString().split('\n')) {
        print('SAURON_ST ${entry.key} $line');
      }
      print('SAURON_TRACE_END id=${entry.key}');
      // Ship the SAME error+trace through Sauron.
      Sauron.captureException(e, stackTrace: st, tags: {'probe_id': entry.key});
    }
    await Future<void>.delayed(const Duration(milliseconds: 400));
  }
  await Sauron.flush();
  print('SAURON_ALL_PROBES_DONE flushed');
}

/// Proves whether dart:io sockets may talk plain HTTP under this target SDK.
Future<void> cleartextPreflight() async {
  try {
    final client = HttpClient();
    final req = await client.getUrl(Uri.parse('http://127.0.0.1:8081/health'));
    final res = await req.close();
    final body = await res.transform(const SystemEncoding().decoder).join();
    print('SAURON_CLEARTEXT_OK status=${res.statusCode} body=${body.trim()}');
    client.close();
  } catch (e) {
    print('SAURON_CLEARTEXT_FAIL ${e.runtimeType}: $e');
  }
}

class ProbeApp extends StatelessWidget {
  const ProbeApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        appBar: AppBar(title: const Text('Sauron symbol probe')),
        body: Center(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              ElevatedButton(
                onPressed: runAllProbes,
                child: const Text('Run all probes'),
              ),
              ElevatedButton(
                onPressed: cleartextPreflight,
                child: const Text('Cleartext preflight'),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
