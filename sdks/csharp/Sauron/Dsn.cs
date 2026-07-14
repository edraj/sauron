using System;

namespace Sauron;

/// <summary>
/// A parsed Sauron DSN of the form <c>https://&lt;public_key&gt;@&lt;host&gt;/&lt;project_id&gt;</c>.
/// The public key is a non-secret write key; there is no password component.
/// </summary>
public sealed class Dsn
{
    /// <summary>URL scheme, e.g. <c>https</c> or <c>http</c>.</summary>
    public string Protocol { get; }

    /// <summary>Non-secret public write key (the DSN user component).</summary>
    public string PublicKey { get; }

    /// <summary>Host, including a non-default port (e.g. <c>example.com</c> or <c>localhost:8080</c>).</summary>
    public string Host { get; }

    /// <summary>Project identifier (the DSN path segment).</summary>
    public string ProjectId { get; }

    /// <summary>The original DSN string.</summary>
    public string Raw { get; }

    private Dsn(string protocol, string publicKey, string host, string projectId, string raw)
    {
        Protocol = protocol;
        PublicKey = publicKey;
        Host = host;
        ProjectId = projectId;
        Raw = raw;
    }

    /// <summary>
    /// The ingest endpoint: <c>{protocol}://{host}/api/{project_id}/envelope</c>.
    /// </summary>
    public string EnvelopeUrl => $"{Protocol}://{Host}/api/{ProjectId}/envelope";

    /// <summary>
    /// Parse a DSN string. Throws <see cref="ArgumentException"/> for a clearly-invalid DSN.
    /// </summary>
    public static Dsn Parse(string dsn)
    {
        if (string.IsNullOrWhiteSpace(dsn))
            throw new ArgumentException("DSN must not be empty.", nameof(dsn));

        Uri uri;
        try
        {
            uri = new Uri(dsn, UriKind.Absolute);
        }
        catch (UriFormatException ex)
        {
            throw new ArgumentException($"Invalid DSN: '{dsn}'.", nameof(dsn), ex);
        }

        if (uri.Scheme != "http" && uri.Scheme != "https")
            throw new ArgumentException($"Invalid DSN scheme '{uri.Scheme}'; expected http or https.", nameof(dsn));

        var userInfo = uri.UserInfo;
        if (string.IsNullOrEmpty(userInfo))
            throw new ArgumentException("Invalid DSN: missing public key (user component).", nameof(dsn));

        // Public key is the user component; ignore any password part.
        var publicKey = userInfo.Split(':')[0];
        if (string.IsNullOrEmpty(publicKey))
            throw new ArgumentException("Invalid DSN: empty public key.", nameof(dsn));

        if (string.IsNullOrEmpty(uri.Host))
            throw new ArgumentException("Invalid DSN: missing host.", nameof(dsn));

        var projectId = uri.AbsolutePath.Trim('/');
        if (string.IsNullOrEmpty(projectId))
            throw new ArgumentException("Invalid DSN: missing project id.", nameof(dsn));

        var host = uri.IsDefaultPort ? uri.Host : $"{uri.Host}:{uri.Port}";

        return new Dsn(uri.Scheme, publicKey, host, projectId, dsn);
    }
}
