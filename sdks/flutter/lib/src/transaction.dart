import 'client.dart';

/// A bounded span of activity that computes its own duration.
class ActiveTransaction {
  ActiveTransaction(
    this._client, {
    required this.name,
    this.op = 'custom',
    this.status,
    this.httpMethod,
    this.httpStatus,
    this.url,
    this.tags,
    this.extra,
  }) : startedAt = DateTime.now().toUtc();

  final SauronClient? _client;
  
  /// Route / screen / operation label.
  final String name;
  
  /// Operation class: `navigation | http | resource | screen_load | custom`.
  final String op;
  
  /// When the transaction was started (UTC).
  final DateTime startedAt;

  /// Free-form outcome, e.g. `ok`, `error`, or an HTTP status class.
  String? status;
  
  /// HTTP verb for `http` transactions, e.g. `GET`.
  String? httpMethod;
  
  /// HTTP response status code for `http` transactions.
  int? httpStatus;
  
  /// Request URL for `http` / `resource` transactions.
  String? url;

  /// Flat string tags for this span. Mutable, like [status] — the interesting
  /// facts about an HTTP call are usually known after it returns, not before.
  ///
  /// Per-call only: never merged with the scope. See
  /// `SauronClient.trackTransaction`.
  Map<String, String>? tags;

  /// Freeform JSON for this span — the request body, the response body, a
  /// retry count. Mutable for the reason [tags] is: you typically assign it in
  /// the middle of the operation you are timing.
  ///
  /// Capped when the span is recorded, not when this is assigned.
  Map<String, Object?>? extra;

  bool _isFinished = false;

  /// Ends the transaction and records it.
  ///
  /// [tags] and [extra] override the fields of the same name **wholesale**, not
  /// per key — matching how [status]/[httpStatus]/[url] behave here. To add to
  /// what is already set, mutate the field (`tx.extra!['response'] = …`) rather
  /// than passing a partial map, which would silently discard the rest.
  void end({
    String? status,
    int? httpStatus,
    String? url,
    Map<String, String>? tags,
    Map<String, Object?>? extra,
  }) {
    if (_isFinished) return;
    _isFinished = true;

    final Duration duration = DateTime.now().toUtc().difference(startedAt);

    _client?.trackTransaction(
      name: name,
      duration: duration,
      op: op,
      status: status ?? this.status,
      httpMethod: httpMethod ?? this.httpMethod,
      httpStatus: httpStatus ?? this.httpStatus,
      url: url ?? this.url,
      tags: tags ?? this.tags,
      extra: extra ?? this.extra,
    );
  }

  /// Cancels the transaction and records it with an optional reason.
  void cancel([String? reason]) {
    final String finalStatus =
        reason != null ? 'cancelled: $reason' : 'cancelled';
    end(status: finalStatus);
  }
}
