using System;
using System.Collections.Generic;
using System.IO;
using System.Text.RegularExpressions;

namespace Sauron;

/// <summary>A single stack frame in the ingest wire format.</summary>
internal sealed class StackFrame
{
    public string? Function { get; set; }
    public string? Module { get; set; }
    public string? Filename { get; set; }
    public string? AbsPath { get; set; }
    public int? Lineno { get; set; }
    public int? Colno { get; set; }
    public bool? InApp { get; set; }
}

/// <summary>
/// Parses a .NET <see cref="Exception.StackTrace"/> string into wire stack frames.
/// Frames are returned ordered call-site → crash (crash frame LAST), per the contract.
/// </summary>
internal static class StackTraceExtractor
{
    // Matches lines like:
    //   "   at Ns.Cls.Method(Type arg) in /path/File.cs:line 42"
    //   "   at Ns.Cls.Method(Type arg)"
    private static readonly Regex FrameRegex = new(
        @"^\s*at\s+(?<frame>.+?)(?:\s+in\s+(?<file>.+):line\s+(?<line>\d+))?\s*$",
        RegexOptions.Compiled);

    private static readonly string[] SystemPrefixes =
    {
        "System.", "Microsoft.",
    };

    public static List<StackFrame> Extract(Exception exception, IReadOnlyList<string>? inAppInclude = null)
        => Parse(exception?.StackTrace, inAppInclude);

    public static List<StackFrame> Parse(string? stackTrace, IReadOnlyList<string>? inAppInclude = null)
    {
        var frames = new List<StackFrame>();
        if (string.IsNullOrEmpty(stackTrace))
            return frames;

        var lines = stackTrace.Split('\n');
        foreach (var rawLine in lines)
        {
            var line = rawLine.TrimEnd('\r');
            if (string.IsNullOrWhiteSpace(line))
                continue;

            var m = FrameRegex.Match(line);
            if (!m.Success)
                continue;

            var frameText = m.Groups["frame"].Value.Trim();
            var (module, function) = SplitModuleFunction(frameText);

            var frame = new StackFrame
            {
                Function = function,
                Module = module,
                InApp = IsInApp(module, inAppInclude),
            };

            if (m.Groups["file"].Success)
            {
                var absPath = m.Groups["file"].Value.Trim();
                frame.AbsPath = absPath;
                frame.Filename = SafeFileName(absPath);
            }

            if (m.Groups["line"].Success && int.TryParse(m.Groups["line"].Value, out var lineno))
                frame.Lineno = lineno;

            frames.Add(frame);
        }

        // .NET stack traces are ordered crash-first (most-recent call first).
        // The wire contract wants call-site → crash, with the crash frame LAST.
        frames.Reverse();
        return frames;
    }

    private static (string? module, string? function) SplitModuleFunction(string frameText)
    {
        var parenIdx = frameText.IndexOf('(');
        var beforeParen = parenIdx >= 0 ? frameText.Substring(0, parenIdx) : frameText;
        var paramsPart = parenIdx >= 0 ? frameText.Substring(parenIdx) : string.Empty;

        var lastDot = beforeParen.LastIndexOf('.');
        if (lastDot <= 0)
            return (null, frameText);

        var module = beforeParen.Substring(0, lastDot);
        var function = beforeParen.Substring(lastDot + 1) + paramsPart;
        return (module, function);
    }

    private static bool IsInApp(string? module, IReadOnlyList<string>? inAppInclude)
    {
        if (string.IsNullOrEmpty(module))
            return false;

        if (inAppInclude is { Count: > 0 })
        {
            foreach (var prefix in inAppInclude)
            {
                if (module!.StartsWith(prefix, StringComparison.Ordinal))
                    return true;
            }
            return false;
        }

        foreach (var prefix in SystemPrefixes)
        {
            if (module!.StartsWith(prefix, StringComparison.Ordinal))
                return false;
        }
        return true;
    }

    private static string SafeFileName(string path)
    {
        try
        {
            return Path.GetFileName(path);
        }
        catch (ArgumentException)
        {
            return path;
        }
    }
}
