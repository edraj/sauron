# Capabilities — SDK feature parity

As of the **v0.3.0** release, all five SDKs converge on one capability model — scope,
breadcrumbs, `beforeSend`, transactions, gzip, retry, a bounded/optional-disk queue, and
opt-in (or default-on) uncaught-error capture — implemented in each language's native
idiom but serializing the **identical** [wire shape](Ingest-Wire-Contract.md).

See also: **[Home](Home.md)** · **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Best Practices](Best-Practices.md)** · **[Framework Integrations](Framework-Integrations.md)**
· **[Troubleshooting](Troubleshooting.md)**.

## Parity matrix

| Capability | [Browser](Browser-SDK.md) | [Flutter](Flutter-SDK.md) | [Node](Node-SDK.md) | [Python](Python-SDK.md) | [C#](CSharp-SDK.md) |
| --- | :-: | :-: | :-: | :-: | :-: |
| **init** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **track** (product events) | ✅ | ✅ | ✅ | ✅ | ✅ |
| **captureException** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **captureMessage** | ✅ | —¹ | ✅ | ✅ | ✅ |
| **identify** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **scope** (user/tags/context) | ✅² | ✅³ | ✅ | ✅ | ✅ |
| **breadcrumbs** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **transactions** (`trackTransaction`) | ✅ | ✅ | ✅ | ✅ | ✅ |
| **auto-capture** (uncaught) | ✅⁴ | ✅⁴ | ✅⁵ | ✅⁵ | ✅⁵ |
| **queue** (bounded + optional disk) | ✅⁶ | ✅⁶ | ✅⁷ | ✅⁷ | ✅⁷ |
| **retry / backoff** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **gzip** (`Content-Encoding: gzip`) | ✅ | ✅ | ✅ | ✅ | ✅ |
| **before-send** (any item) | ✅ | ✅ | ✅ | ✅ | ✅ |

**Legend:** ✅ shipped · ◑ platform-appropriate subset · — not applicable.

1. Flutter ships no `captureMessage` helper — capture a string with
   `Sauron.captureException(...)`, and note that uncaught errors already flow through the
   four auto-capture layers.
2. Browser scope: `setUser`, `setTag`/`setTags`, `setContext`, and `setExtra` are all
   top-level functions. The runtime is single-threaded, so no async-isolation primitive is
   needed; scope user + tags are stamped onto captured errors and events.
3. Flutter exposes `setUser`, `setTag`/`setTags`, `setContext`, and `setExtra` on the
   single global client scope (plus the breadcrumb ring). Like the browser it has one
   global scope — no async scope-isolation block.
4. **Default-on.** Uncaught errors and unhandled rejections (Browser) and the four Flutter
   layers (`FlutterError.onError`, `PlatformDispatcher.onError`, isolate errors, guarding
   zone) capture automatically — no flag.
5. **Opt-in**, default off — enable with `autoCaptureUnhandled` (Node),
   `auto_capture_unhandled` (Python), or `AutoCaptureUnhandled` (C#). Captured with
   `mechanism.handled = false`, and the runtime's default crash/exit behavior is preserved.
6. Durable offline queue — `localStorage` (Browser) / JSONL file (Flutter) — drop-oldest
   FIFO, replayed on the next launch.
7. In-memory byte-bounded queue (drop-oldest, `maxQueueBytes` default 1 MiB); opt-in on-disk
   FIFO persistence via `offlineDir` (Node) / `offline_path` (Python) / `OfflineDir` (C#)
   for at-least-once delivery across restarts.

## How each capability lands per platform

- **Scope isolation** uses the native idiom: `AsyncLocalStorage` (Node), `contextvars`
  (Python), `AsyncLocal<Scope>` (C#). Server SDKs expose a per-request isolated child scope
  — `withScope` (Node), `with sauron.scope():` (Python), `using SauronSdk.PushScope()` (C#)
  — so concurrent requests never leak user/tags/breadcrumbs into each other. Browser and
  Flutter run a single client scope (no isolation needed).
- **Breadcrumbs** ring-buffer at `maxBreadcrumbs` — **100** on Flutter and the three server
  SDKs, **50** on the browser — and attach to any error captured afterwards. A
  `beforeBreadcrumb` hook can drop or mutate each crumb (Browser, Node, Python, C#).
  Browser and Flutter can also upload a standalone `breadcrumb_batch`; the server SDKs
  attach breadcrumbs onto the error item instead.
- **Transactions** — every SDK emits the `transaction` item via `trackTransaction`
  (snake_case wire fields: `duration_ms`, `http_method`, `http_status`, `distinct_id`).
  Browser and Flutter can additionally auto-capture navigation/HTTP/route timings; the
  server SDKs record them manually.
- **before-send** runs on **every** outgoing item (`error | event | identify |
  transaction`) at a single enqueue chokepoint — return the item to send it, or `null` to
  drop it. This is the reconciled behavior in 0.3.0 (previously Flutter ran it on errors
  only).
- **gzip** compresses the request body once it exceeds `gzipThresholdBytes` (default 1024)
  and sets `Content-Encoding: gzip`; smaller bodies go out uncompressed. Node uses `zlib`,
  Python `gzip`, C# `GZipStream`, the browser native `CompressionStream` (fflate fallback).
- **retry** follows a shared policy: retry transient failures (408/413/429/5xx and network
  errors) with exponential backoff + jitter (cap 30 s), honor `Retry-After` on 429, drop on
  non-retryable 4xx, and **disable** the SDK on 401/403.
- **No-op discipline** — every dispatch call is a no-op before `init` and when the DSN is
  missing or disabled, on every SDK.

## Error-item attribution (reconciled shape)

Every SDK now populates the same error-item fields from the active scope: `breadcrumbs`,
`tags`, `user`, and an optional client `fingerprint` (honored verbatim by the backend for
grouping). See the [`error` item](Ingest-Wire-Contract.md#error--a-captured-exception-or-message)
in the wire contract.

## Versioning

All five SDKs ship as **v0.3.0** for this release. Confirm the installed version if a
capability appears missing — see [Troubleshooting](Troubleshooting.md).
