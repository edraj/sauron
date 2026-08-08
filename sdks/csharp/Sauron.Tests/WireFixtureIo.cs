using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Sauron.Tests;

/// <summary>
/// Writer for <c>sdks/wire-fixtures/&lt;sdk&gt;.json</c> — the envelopes the backend's
/// <c>cargo test -p sauron-core --test sdk_wire_conformance</c> feeds through the REAL
/// <c>serde</c> deserializer.
/// </summary>
/// <remarks>
/// <para>Two categories are pinned so that regenerating is a NO-OP:</para>
/// <list type="number">
///   <item>the intrinsically dynamic fields (<c>timestamp</c>, <c>event_id</c>, …);</item>
///   <item>everything the <b>toolchain</b> supplies rather than the SDK — stack-frame
///   identity strings (the test assembly's own names) and the host/runtime values in
///   <c>context.os</c> / <c>.runtime</c> / <c>.device</c>. Without this a .NET SDK
///   upgrade rewrote a committed file with no wire change at all, which makes a CI
///   diff gate noisy and leaves a tracked file dirty after a plain test run.</item>
/// </list>
/// <para>
/// What is deliberately NOT normalized is the part that proves something: item shape,
/// key set, nullability, and the frame COUNT.
/// </para>
/// </remarks>
internal static class WireFixtureIo
{
    private const string Timestamp = "2026-07-12T10:30:00.123Z";

    private static readonly Dictionary<string, string> StringSubs = new()
    {
        ["timestamp"] = Timestamp,
        ["sent_at"] = Timestamp,
        ["event_id"] = "0123456789abcdef0123456789abcdef",
        ["session_id"] = "sess_fixture",
        ["device_id"] = "3f2504e0-4f89-41d3-9a0c-0305e82c3301",
        ["workflow_id"] = "wf_fixture",
        ["raw_stacktrace"] = "<normalized>",
        ["build_id"] = "<normalized>",
        ["isolate_dso_base"] = "<normalized>",
    };

    /// <summary>Stack-frame identity: where the test ran, not what the SDK emits.</summary>
    private static readonly Dictionary<string, string> FrameIdentity = new()
    {
        ["function"] = "<fn>",
        ["module"] = "<module>",
        ["filename"] = "<file>",
        ["abs_path"] = "<file>",
    };

    /// <summary>
    /// <c>"&lt;parent&gt;.&lt;key&gt;"</c> paths carrying host- or runtime-derived values.
    /// <c>context.device</c> / <c>.os</c> / <c>.runtime</c> are free-form
    /// <c>serde_json::Value</c> on the wire, so their contents prove nothing — while
    /// <c>runtime.version</c> is the .NET version. <c>runtime.name</c> is left alone
    /// deliberately: it is an SDK constant, not a host value.
    /// </summary>
    private static readonly HashSet<string> HostDerived = new()
    {
        "os.name",
        "os.version",
        "runtime.version",
        "device.family",
        "device.model",
        "device.arch",
    };

    /// <summary>Write one captured envelope body as this SDK's committed wire fixture.</summary>
    public static string Write(string sdk, string envelopeJson, [CallerFilePath] string sourceFile = "")
    {
        var node = JsonNode.Parse(envelopeJson) ?? throw new InvalidOperationException("empty envelope body");
        Normalize(node, string.Empty, string.Empty);

        // Sauron.Tests/ -> csharp/ -> sdks/. Derived from the SOURCE path, not the test
        // assembly's bin/ directory, so it does not depend on the build layout.
        var testsDir = Path.GetDirectoryName(sourceFile)!;
        var sdksDir = Path.GetFullPath(Path.Combine(testsDir, "..", ".."));
        var path = Path.Combine(sdksDir, "wire-fixtures", $"{sdk}.json");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, node.ToJsonString(new JsonSerializerOptions { WriteIndented = true }) + "\n");
        return path;
    }

    private static void Normalize(JsonNode node, string key, string parentKey)
    {
        switch (node)
        {
            case JsonObject obj:
                foreach (var name in new List<string>(obj.Select(p => p.Key)))
                {
                    var child = obj[name];
                    if (child is null)
                        continue;
                    var replacement = Replacement(name, key, child);
                    if (replacement is not null)
                        obj[name] = replacement;
                    else
                        Normalize(child, name, key);
                }
                break;
            case JsonArray arr:
                // Array children keep the container's key AND its parent, so a frame
                // inside `stacktrace: [...]` is still seen as living under it.
                for (var i = 0; i < arr.Count; i++)
                {
                    var child = arr[i];
                    if (child is null)
                        continue;
                    var replacement = Replacement(key, parentKey, child);
                    if (replacement is not null)
                        arr[i] = replacement;
                    else
                        Normalize(child, key, parentKey);
                }
                break;
        }
    }

    /// <summary>The pinned value for a leaf, or null when the leaf is kept verbatim.</summary>
    private static JsonNode? Replacement(string key, string parentKey, JsonNode node)
    {
        if (node is not JsonValue value)
            return null;
        if (value.TryGetValue<string>(out _))
        {
            if (HostDerived.Contains($"{parentKey}.{key}"))
                return JsonValue.Create("<host>");
            if (FrameIdentity.TryGetValue(key, out var frame))
                return JsonValue.Create(frame);
            if (StringSubs.TryGetValue(key, out var sub))
                return JsonValue.Create(sub);
            return null;
        }
        if (value.TryGetValue<int>(out _))
        {
            if (key == "lineno")
                return JsonValue.Create(42);
            if (key == "colno")
                return JsonValue.Create(13);
        }
        // JSON `null` is left alone: nullability is part of what the fixture proves.
        return null;
    }
}
