/// Parses and represents a Sauron DSN.
///
/// Format: `https://<public_key>@<host>[:port][/path...]/<project_id>`.
///
/// The public key is a non-secret, write-only ingest key. The project id is
/// the final path segment. Any preceding path segments form an optional path
/// prefix (mirroring Sentry-style self-hosted deployments).
class Dsn {
  Dsn({
    required this.scheme,
    required this.publicKey,
    required this.host,
    required this.port,
    required this.projectId,
    this.pathPrefix = const <String>[],
  });

  final String scheme;
  final String publicKey;
  final String host;
  final int port;
  final String projectId;
  final List<String> pathPrefix;

  /// Parses [input], throwing [FormatException] on malformed DSNs.
  factory Dsn.parse(String input) {
    final Uri uri = Uri.parse(input.trim());
    if (uri.scheme.isEmpty) {
      throw const FormatException('DSN is missing a scheme (http/https).');
    }
    if (uri.userInfo.isEmpty) {
      throw const FormatException('DSN is missing the public key.');
    }
    if (uri.host.isEmpty) {
      throw const FormatException('DSN is missing a host.');
    }
    final String publicKey = uri.userInfo.split(':').first;
    if (publicKey.isEmpty) {
      throw const FormatException('DSN public key is empty.');
    }
    final List<String> segments =
        uri.pathSegments.where((String s) => s.isNotEmpty).toList();
    if (segments.isEmpty) {
      throw const FormatException('DSN is missing the project id.');
    }
    final String projectId = segments.last;
    final List<String> prefix = segments.sublist(0, segments.length - 1);
    return Dsn(
      scheme: uri.scheme,
      publicKey: publicKey,
      host: uri.host,
      port: uri.hasPort ? uri.port : _defaultPort(uri.scheme),
      projectId: projectId,
      pathPrefix: prefix,
    );
  }

  static int _defaultPort(String scheme) =>
      scheme == 'http' ? 80 : 443;

  bool get _isDefaultPort => port == _defaultPort(scheme);

  String get _authority => _isDefaultPort ? host : '$host:$port';

  /// The endpoint that accepts envelopes: `.../api/{project_id}/envelope`.
  Uri get envelopeEndpoint => Uri(
        scheme: scheme,
        host: host,
        port: _isDefaultPort ? null : port,
        pathSegments: <String>[...pathPrefix, 'api', projectId, 'envelope'],
      );

  /// The canonical DSN string echoed back in the envelope header (includes the
  /// public key, which is non-secret).
  @override
  String toString() {
    final String prefix =
        pathPrefix.isEmpty ? '' : '/${pathPrefix.join('/')}';
    return '$scheme://$publicKey@$_authority$prefix/$projectId';
  }
}
