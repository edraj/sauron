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

  /// Developer-attached flat tags (string->string), seeded from init options
  /// and mutated by [setTag]/[setTags]. Merged under per-call tags on capture.
  final Map<String, String> tags = <String, String>{};

  /// Developer-attached structured contexts (name -> block). Distinct from the
  /// machine-owned device/os/app/runtime context.
  final Map<String, Map<String, Object?>> contexts =
      <String, Map<String, Object?>>{};

  /// Developer-attached freeform extra (JSON).
  final Map<String, Object?> extra = <String, Object?>{};

  /// Sets a single tag (last-write-wins by key).
  void setTag(String key, String value) => tags[key] = value;

  /// Merges the given tags into the scope (last-write-wins by key).
  void setTags(Map<String, String> values) => tags.addAll(values);

  /// Sets (replaces) a named context block.
  void setContext(String name, Map<String, Object?> block) =>
      contexts[name] = block;

  /// Sets a single extra value (last-write-wins by key).
  void setExtra(String key, Object? value) => extra[key] = value;
}
