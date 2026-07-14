import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;

import '../dsn.dart';
import '../envelope.dart';
import '../sauron_options.dart';
import 'connectivity.dart';
import 'gzip.dart';
import 'queue.dart';

/// Builds an [EnvelopeHeader] stamped with the given send time.
typedef HeaderBuilder = EnvelopeHeader Function(DateTime sentAt);

/// Provides the current ambient context (device + live user) at send time.
typedef ContextBuilder = SauronContext Function();

enum _OutcomeKind { success, dropNoRetry, disable, split, retry }

class _Outcome {
  const _Outcome(this.kind, {this.retryAfter});
  final _OutcomeKind kind;
  final Duration? retryAfter;
}

/// Batches items into envelopes, compresses, delivers them to the ingest
/// gateway, and durably retries failures with exponential backoff + jitter.
///
/// The full HTTP response policy:
///  * 200/202 → success, drop
///  * 400 → drop, no retry
///  * 401/403 → drop **and disable** the transport
///  * 413 → split the envelope and retry the halves
///  * 429 → honor `Retry-After`
///  * 408 / 5xx / network error → backoff + jitter (cap 30s) retry
class SauronTransport {
  SauronTransport({
    required SauronOptions options,
    required Dsn dsn,
    required HeaderBuilder headerBuilder,
    required ContextBuilder contextBuilder,
    required EnvelopeQueue queue,
    http.Client? httpClient,
    ConnectivityMonitor? connectivity,
    Random? random,
  })  : _options = options,
        _dsn = dsn,
        _headerBuilder = headerBuilder,
        _contextBuilder = contextBuilder,
        _queue = queue,
        _client = httpClient ?? options.httpClient ?? http.Client(),
        _connectivity = connectivity,
        _random = random ?? Random();

  final SauronOptions _options;
  final Dsn _dsn;
  final HeaderBuilder _headerBuilder;
  final ContextBuilder _contextBuilder;
  final EnvelopeQueue _queue;
  final http.Client _client;
  final ConnectivityMonitor? _connectivity;
  final Random _random;

  final List<EnvelopeItem> _buffer = <EnvelopeItem>[];

  Timer? _flushTimer;
  Timer? _retryTimer;
  int _retryAttempt = 0;
  bool _enabled = true;
  bool _closed = false;
  bool _started = false;
  bool _draining = false;

  /// Whether the transport is still accepting/sending data.
  bool get isEnabled => _enabled && !_closed;

  /// Starts the flush timer, connectivity listener, and an initial drain of any
  /// envelopes persisted by a previous app session.
  void start() {
    if (_started) {
      return;
    }
    _started = true;
    _flushTimer = Timer.periodic(_options.flushInterval, (_) {
      unawaited(flush());
    });
    _connectivity?.start(() {
      unawaited(_drainQueue());
    });
    unawaited(_drainQueue());
  }

  /// Buffers [item] for the next batch; flushes eagerly when the batch is full.
  void capture(EnvelopeItem item) {
    if (!isEnabled) {
      return;
    }
    _buffer.add(item);
    if (_buffer.length >= _options.maxBatchItems) {
      unawaited(flush());
    }
  }

  /// Packs the current buffer into an envelope, persists it, and drains.
  Future<void> flush() async {
    if (_closed) {
      return;
    }
    await _packBufferIntoQueue();
    await _drainQueue();
  }

  /// Flushes and tears down all resources.
  Future<void> close() async {
    _flushTimer?.cancel();
    _retryTimer?.cancel();
    _flushTimer = null;
    _retryTimer = null;
    try {
      await _packBufferIntoQueue();
      await _drainQueue();
    } finally {
      _closed = true;
      await _connectivity?.dispose();
      _client.close();
    }
  }

  Future<void> _packBufferIntoQueue() async {
    if (_buffer.isEmpty || !_enabled) {
      return;
    }
    final List<EnvelopeItem> items = List<EnvelopeItem>.of(_buffer);
    _buffer.clear();
    final Envelope envelope = _buildEnvelope(items);
    await _queue.enqueue(envelope.encode());
  }

  Envelope _buildEnvelope(List<EnvelopeItem> items) {
    final DateTime sentAt = DateTime.now().toUtc();
    return Envelope(
      header: _headerBuilder(sentAt),
      context: _contextBuilder(),
      items: items,
    );
  }

  Future<void> _drainQueue() async {
    if (!_enabled || _closed || _draining) {
      return;
    }
    _draining = true;
    try {
      while (true) {
        final String? json = await _queue.peek();
        if (json == null) {
          break;
        }
        final _Outcome outcome = await _send(json);
        switch (outcome.kind) {
          case _OutcomeKind.success:
            await _queue.acknowledgeFirst();
            _retryAttempt = 0;
          case _OutcomeKind.dropNoRetry:
            _log('dropping envelope (non-retryable).');
            await _queue.acknowledgeFirst();
          case _OutcomeKind.disable:
            _log('disabling transport (auth rejected).');
            _enabled = false;
            await _queue.acknowledgeFirst();
            return;
          case _OutcomeKind.split:
            await _splitHead(json);
          case _OutcomeKind.retry:
            _scheduleRetry(outcome.retryAfter);
            return;
        }
      }
    } finally {
      _draining = false;
    }
  }

  Future<_Outcome> _send(String json) async {
    final List<int> bytes = utf8.encode(json);
    final Map<String, String> headers = <String, String>{
      'Content-Type': 'application/json',
      'X-Sauron-Key': _dsn.publicKey,
    };
    List<int> body = bytes;
    if (SauronGzip.isSupported && bytes.length >= _options.gzipThresholdBytes) {
      body = SauronGzip.encode(bytes);
      headers['Content-Encoding'] = 'gzip';
    }
    try {
      final http.Response response = await _client.post(
        _dsn.envelopeEndpoint,
        headers: headers,
        body: body,
      );
      return _classify(response.statusCode, response.headers);
    } on Object catch (error) {
      // Connectivity is a hint; the real signal is the HTTP response (or lack
      // of one). A network error means: keep the envelope and back off.
      _log('network error: $error');
      return const _Outcome(_OutcomeKind.retry);
    }
  }

  _Outcome _classify(int status, Map<String, String> headers) {
    if (status == 200 || status == 202) {
      return const _Outcome(_OutcomeKind.success);
    }
    if (status == 400) {
      return const _Outcome(_OutcomeKind.dropNoRetry);
    }
    if (status == 401 || status == 403) {
      return const _Outcome(_OutcomeKind.disable);
    }
    if (status == 413) {
      return const _Outcome(_OutcomeKind.split);
    }
    if (status == 429) {
      return _Outcome(
        _OutcomeKind.retry,
        retryAfter: _parseRetryAfter(headers['retry-after']),
      );
    }
    if (status == 408 || status >= 500) {
      return const _Outcome(_OutcomeKind.retry);
    }
    // Any other 4xx: the payload is malformed for this endpoint — drop it.
    return const _Outcome(_OutcomeKind.dropNoRetry);
  }

  Future<void> _splitHead(String json) async {
    await _queue.acknowledgeFirst();
    final List<String> halves = _splitEnvelope(json);
    for (final String half in halves) {
      await _queue.enqueue(half);
    }
  }

  List<String> _splitEnvelope(String json) {
    try {
      final Map<String, dynamic> map =
          jsonDecode(json) as Map<String, dynamic>;
      final List<dynamic> items = (map['items'] as List<dynamic>?) ?? const [];
      if (items.length <= 1) {
        _log('cannot split a single-item envelope; dropping.');
        return const <String>[];
      }
      final int mid = items.length ~/ 2;
      final Map<String, dynamic> first = Map<String, dynamic>.of(map)
        ..['items'] = items.sublist(0, mid);
      final Map<String, dynamic> second = Map<String, dynamic>.of(map)
        ..['items'] = items.sublist(mid);
      return <String>[jsonEncode(first), jsonEncode(second)];
    } on Object {
      return const <String>[];
    }
  }

  void _scheduleRetry(Duration? retryAfter) {
    if (_closed) {
      return;
    }
    _retryAttempt++;
    final Duration delay = retryAfter ?? _backoffDelay(_retryAttempt);
    _log('scheduling retry #$_retryAttempt in ${delay.inMilliseconds}ms.');
    _retryTimer?.cancel();
    _retryTimer = Timer(delay, () {
      unawaited(_drainQueue());
    });
  }

  /// Exponential backoff with full jitter, capped at 30 seconds.
  Duration _backoffDelay(int attempt) {
    final int baseSeconds = min(30, pow(2, attempt).toInt());
    final int jitterMs = _random.nextInt(1000);
    final int totalMs = min(30000, baseSeconds * 1000 + jitterMs);
    return Duration(milliseconds: totalMs);
  }

  Duration? _parseRetryAfter(String? value) {
    if (value == null) {
      return null;
    }
    final int? seconds = int.tryParse(value.trim());
    if (seconds != null) {
      return Duration(seconds: seconds);
    }
    final DateTime? until = DateTime.tryParse(value.trim());
    if (until != null) {
      final Duration diff = until.toUtc().difference(DateTime.now().toUtc());
      return diff.isNegative ? Duration.zero : diff;
    }
    return null;
  }

  void _log(String message) {
    if (_options.debug) {
      debugPrint('[Sauron] $message');
    }
  }

  // ---- test/inspection hooks -------------------------------------------------

  /// Number of items buffered but not yet packed into an envelope.
  @visibleForTesting
  int get bufferedItemCount => _buffer.length;

  /// The queue backing this transport (for inspection/tests).
  @visibleForTesting
  EnvelopeQueue get queue => _queue;

  /// Cancels pending timers without a full close (keeps tests timer-clean).
  @visibleForTesting
  void debugCancelTimers() {
    _flushTimer?.cancel();
    _retryTimer?.cancel();
    _flushTimer = null;
    _retryTimer = null;
  }
}
