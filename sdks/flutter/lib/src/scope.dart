import 'dart:collection';

import 'types.dart';

/// Mutable, per-client state that rides along with captured events: the current
/// user and a bounded ring buffer of breadcrumbs.
class Scope {
  Scope({this.maxBreadcrumbs = 100});

  /// Maximum breadcrumbs retained; oldest are evicted first (FIFO).
  final int maxBreadcrumbs;

  final Queue<Breadcrumb> _breadcrumbs = Queue<Breadcrumb>();

  /// The currently identified user (may be `null`).
  SauronUser? user;

  /// The distinct id to attach to analytics events, if known.
  String? get distinctId => user?.id;

  /// Adds a breadcrumb, evicting the oldest if the buffer is full.
  void addBreadcrumb(Breadcrumb crumb) {
    if (maxBreadcrumbs <= 0) {
      return;
    }
    _breadcrumbs.addLast(crumb);
    while (_breadcrumbs.length > maxBreadcrumbs) {
      _breadcrumbs.removeFirst();
    }
  }

  /// A snapshot of current breadcrumbs, oldest first.
  List<Breadcrumb> get breadcrumbs =>
      List<Breadcrumb>.unmodifiable(_breadcrumbs);

  /// Clears all breadcrumbs.
  void clearBreadcrumbs() => _breadcrumbs.clear();
}
