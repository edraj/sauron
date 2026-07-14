using System;
using Xunit;

namespace Sauron.Tests;

public class DsnTests
{
    [Fact]
    public void Parse_ValidDsn_ExtractsComponents()
    {
        var dsn = Dsn.Parse("https://pub123@example.com/42");

        Assert.Equal("https", dsn.Protocol);
        Assert.Equal("pub123", dsn.PublicKey);
        Assert.Equal("example.com", dsn.Host);
        Assert.Equal("42", dsn.ProjectId);
        Assert.Equal("https://example.com/api/42/envelope", dsn.EnvelopeUrl);
    }

    [Fact]
    public void Parse_WithNonDefaultPort_KeepsPortInHost()
    {
        var dsn = Dsn.Parse("http://key@localhost:8080/proj-1");

        Assert.Equal("http", dsn.Protocol);
        Assert.Equal("localhost:8080", dsn.Host);
        Assert.Equal("proj-1", dsn.ProjectId);
        Assert.Equal("http://localhost:8080/api/proj-1/envelope", dsn.EnvelopeUrl);
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("not a url")]
    [InlineData("ftp://key@host/1")]          // bad scheme
    [InlineData("https://example.com/42")]     // missing public key
    [InlineData("https://pub@example.com")]    // missing project id
    [InlineData("https://pub@example.com/")]   // empty project id
    public void Parse_InvalidDsn_Throws(string dsn)
    {
        Assert.Throws<ArgumentException>(() => Dsn.Parse(dsn));
    }
}
