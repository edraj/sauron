# Python SDK — `sauron-sdk`

Server-side Python SDK (**v0.3.0**). Dispatches product-analytics events and exceptions
over a buffered background HTTP transport (a daemon thread draining an in-memory queue
via `urllib`). **Stdlib only — no runtime dependencies.** Source:
[`sdks/python`](../sdks/python). SDK header name: `sauron-python`.

Server-side = no browser/DOM/auto-instrumentation. The surface is init/config,
`track`, `capture_exception`/`capture_message`, `identify`, `track_transaction`, scope +
breadcrumbs, and `flush`/`close`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/python-server`](../examples/python-server).

## Install

```bash
pip install sauron-sdk
```

Then `import sauron`.

## Init

```python
import sauron

sauron.init(dsn="https://<public_key>@<host>/<project_id>")
```

A **missing/empty** `dsn` puts the SDK into disabled no-op mode (it logs, does not
raise) so code can ship without a DSN. A **non-empty but malformed** DSN raises
`sauron.DsnError`.

### `init(dsn=None, *, ...)` keyword options

| Option | Default | Notes |
| --- | --- | --- |
| `dsn` | `None` | `https://<public_key>@<host>/<project_id>`; empty ⇒ disabled |
| `environment` | `"production"` | |
| `release` | `None` | |
| `sample_rate` | `1.0` | error sample rate |
| `flush_interval` | `5.0` | background flush interval, seconds |
| `max_batch` | `30` | flush eagerly at this many buffered items |
| `max_breadcrumbs` | `100` | breadcrumb ring size on the global scope |
| `gzip_threshold_bytes` | `1024` | gzip the body (sets `Content-Encoding: gzip`) once it exceeds this size |
| `max_queue_bytes` | `1_048_576` | drop-oldest byte cap for the in-memory pending queue |
| `offline_path` | `None` | opt-in directory for FIFO disk persistence of pending items |
| `before_send` | `None` | `(item: dict, hint=None) -> dict \| None` on every item |
| `before_breadcrumb` | `None` | `(crumb: dict) -> dict \| None` on every breadcrumb |
| `auto_capture_unhandled` | `False` | opt-in `sys.excepthook`/`threading.excepthook` capture |
| `debug` | `False` | log to stderr |
| `sender` | `None` | optional HTTP sender `(url, headers, body) -> status` (mainly for tests) |

`init` returns the created `Client` (or `None` when disabled); `sauron.get_client()`
returns the active client. `init` also registers an `atexit` flush once per process, so
a short-lived script drains its buffer on exit even without an explicit `close()`.

## API

| Function | Signature |
| --- | --- |
| `track` | `track(event: str, distinct_id: str, properties: Mapping \| None = None) -> None` |
| `capture_exception` | `capture_exception(error=None, *, user=None, level="error", tags=None, fingerprint=None) -> str \| None` |
| `capture_message` | `capture_message(message: str, level: str = "info") -> str \| None` |
| `identify` | `identify(distinct_id: str, traits: Mapping \| None = None) -> None` |
| `track_transaction` | `track_transaction(name, *, op="custom", duration_ms, status=None, http_method=None, http_status=None, url=None, distinct_id=None) -> None` |
| `add_breadcrumb` | `add_breadcrumb(*, type=None, category=None, message=None, level=None, data=None) -> None` |
| `set_user` | `set_user(user: Mapping \| None) -> None` |
| `set_tag` | `set_tag(key: str, value) -> None` |
| `set_tags` | `set_tags(tags: Mapping) -> None` |
| `set_context` | `set_context(key: str, value) -> None` |
| `set_extra` | `set_extra(key: str, value) -> None` |
| `scope` | `with sauron.scope() as s: ...` (context manager) |
| `push_scope` / `pop_scope` | `push_scope() -> Scope` / `pop_scope() -> None` |
| `configure_scope` | `configure_scope(callback: (Scope) -> None) -> None` |
| `flush` | `flush(timeout: float \| None = None) -> bool` |
| `close` | `close(timeout: float \| None = None) -> None` |

`distinct_id` is **required** on `track` (per the wire contract). Every dispatch call is
a no-op before `init` / when disabled.

### Track an event

```python
sauron.track("checkout_completed", distinct_id="u_123",
             properties={"cart_value": 42.5, "currency": "USD"})
```

### Capture an exception

Called bare inside an `except` block, `capture_exception()` reads the active exception:

```python
try:
    do_work()
except Exception:
    event_id = sauron.capture_exception(tags={"area": "checkout"})
```

You can also pass an explicit exception, plus `user=`, `level=`, and a `fingerprint=`
(a sequence of strings) honored verbatim by the backend for grouping. It returns the
event id (or `None` when disabled). `capture_message("worker started", level="info")`
sends a bare message.

### Identify a user

```python
sauron.identify("u_123", traits={"plan": "pro"})
```

## Scope, tags & context

A process-wide **global scope** holds default user/tags/context/breadcrumbs; the
module-level setters mutate the *active* scope:

```python
sauron.set_user({"id": "u_123", "email": "ada@example.com"})  # None to clear
sauron.set_tag("region", "eu-west-1")
sauron.set_tags({"tier": "pro", "shard": "7"})
sauron.set_context("order", {"id": "ord_1001", "items": 3})
sauron.set_extra("cache_hit", False)
```

Scope tags/user/breadcrumbs (and non-empty context/extra) are merged onto every captured
error; per-call values already on the item win.

### Per-request isolation with `scope()`

The active scope lives in a `contextvars.ContextVar`, so each `asyncio` task / thread /
copied context gets its own layer over the global scope. The `scope()` context manager
clones the current scope on entry and restores the parent on exit, so mutations never
leak into concurrent work:

```python
with sauron.scope() as s:
    s.set_user({"id": request.user_id})
    s.set_tag("route", "POST /checkout")
    sauron.add_breadcrumb(category="auth", message="token verified")
    # any capture_exception in here inherits this scope
    handle(request)
```

`push_scope()`/`pop_scope()` are the manual form; `configure_scope(cb)` mutates the
active scope in place (handy for seeding global defaults right after `init`).

## Breadcrumbs

```python
sauron.add_breadcrumb(category="db", message="SELECT users", level="info",
                      data={"ms": 4})
```

All args are keyword-only; missing fields are defaulted and an ISO `timestamp` is
stamped. The crumb lands on the active scope (ring-buffered at `max_breadcrumbs`,
default 100) and attaches to errors captured afterwards. A `before_breadcrumb` hook runs
first — return `None` to drop the crumb:

```python
sauron.init(dsn=DSN, before_breadcrumb=lambda c: None if c["category"] == "noisy" else c)
```

## `before_send` (any item)

`before_send` runs on **every** outgoing item dict (`error | event | identify |
transaction`) at the single enqueue chokepoint — return the (possibly mutated) dict to
send it, or `None` to drop it:

```python
def scrub(item, hint=None):
    if item.get("type") == "event":
        item.get("properties", {}).pop("email", None)
    return item  # return None to drop

sauron.init(dsn=DSN, before_send=scrub)
```

## Performance transactions

```python
import time
start = time.perf_counter()
# ... handle request ...
sauron.track_transaction(
    "GET /api/users", op="http",
    duration_ms=(time.perf_counter() - start) * 1000,
    http_method="GET", http_status=200, url="/api/users",
)
```

`op` defaults to `"custom"`; wire fields are snake_case (`duration_ms`, `http_method`,
`http_status`, `distinct_id`). `distinct_id` falls back to the scoped user's id.

## Gzip, retry & the offline queue

- **Gzip** — the request body is gzipped once it exceeds `gzip_threshold_bytes` (default
  1024), with `Content-Encoding: gzip`; smaller bodies go out uncompressed (stdlib
  `gzip`).
- **Retry** — the transport retries transient failures (408/413/429/5xx and network
  errors) with exponential backoff, honoring `Retry-After` on 429, then re-buffers the
  batch; non-retryable 4xx are dropped and 401/403 disable the SDK.
- **Queue** — items buffer in a byte-bounded queue (`max_queue_bytes`, default 1 MiB,
  drop-oldest). Set `offline_path` to persist pending items FIFO to disk (reloaded on
  `init`, deleted on delivery) for at-least-once delivery across restarts.

## Auto-capture & graceful shutdown

`auto_capture_unhandled=True` (opt-in, default `False`) installs
`sys.excepthook`/`threading.excepthook` hooks that capture uncaught exceptions with
`mechanism.handled=False`, then delegate to the previous hook so the default crash/exit
behavior is preserved:

```python
sauron.init(dsn=DSN, auto_capture_unhandled=True)
```

Shutdown is handled by the `atexit` flush registered in `init`. You can still flush/close
explicitly for short-lived processes:

```python
sauron.flush()   # sends the buffer synchronously
sauron.close()   # flushes then stops the background thread
```

## Example

See [`examples/python-server`](../examples/python-server). Run it with:

```bash
cd examples/python-server
pip install -e ../../sdks/python
SAURON_DSN="https://<public_key>@<host>/<project_id>" python main.py
```

Run the SDK tests with `cd sdks/python && python -m pytest -q` (or `python -m
unittest`). More in **[Examples](Examples.md)**.
