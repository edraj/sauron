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

  bool _isFinished = false;

  /// Ends the transaction and records it.
  void end({String? status, int? httpStatus, String? url}) {
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
    );
  }

  /// Cancels the transaction and records it with an optional reason.
  void cancel([String? reason]) {
    final String finalStatus =
        reason != null ? 'cancelled: $reason' : 'cancelled';
    end(status: finalStatus);
  }
}
