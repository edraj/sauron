# sauron-sdk (Python)

Server-side Python SDK for the [Sauron](../../) observability + analytics gateway.
Dispatches product-analytics events and exceptions to the Sauron ingest endpoint
over a buffered background HTTP transport. No runtime dependencies (stdlib only).

## Install

```bash
pip install sauron-sdk
```

## Usage

```python
import sauron

sauron.init(dsn="https://pk_live_xxx@ingest.sauron.example/1")

# Product analytics — distinct_id is required by the wire contract.
sauron.track("checkout_completed", distinct_id="u_123", properties={"cart_value": 42.5})

# Identify a person with traits.
sauron.identify("u_123", traits={"plan": "pro"})

# Exceptions.
try:
    do_work()
except Exception:
    sauron.capture_exception()  # reads the active exception

# A bare message.
sauron.capture_message("worker started", level="info")

# On shutdown — flush the buffer and stop the background thread.
sauron.close()
```

## Scope, breadcrumbs & transactions

Per-request isolation is built on `contextvars`, so concurrent requests never
leak each other's user/tags/breadcrumbs:

```python
import sauron

# Global defaults (process-wide).
sauron.set_tag("service", "checkout")

# Per-request scope — auto-popped on exit.
with sauron.scope():
    sauron.set_user({"id": "u_123", "email": "a@b.co"})
    sauron.set_tag("request_id", "req_42")
    sauron.add_breadcrumb(category="http", message="GET /cart", level="info")

    try:
        do_work()
    except Exception as exc:
        # Captured errors carry the scope's user, tags and breadcrumbs, plus an
        # optional fingerprint grouping override.
        sauron.capture_exception(exc, fingerprint=["checkout", "timeout"])

    # Manual performance transaction.
    sauron.track_transaction(
        "GET /cart", op="http", duration_ms=128.4, http_status=200
    )
```

Other hooks: `before_send=fn(item, hint)` (runs on **every** item — the PII
scrubbing seam), `before_breadcrumb=fn(crumb)`, `max_breadcrumbs` (default 100),
`gzip_threshold_bytes`, `max_queue_bytes`, and opt-in `offline_path` disk
persistence.

## Auto uncaught-capture & graceful shutdown

Opt in to reporting uncaught exceptions. Off by default because top-level hooks
that alter exit behavior are risky on a server; when enabled the SDK captures
with `mechanism.handled=false`, flushes, then **delegates to the previous hook**
so the interpreter's default crash/exit behavior is preserved:

```python
sauron.init(dsn=..., auto_capture_unhandled=True)
```

`init` also registers an `atexit` flush so buffered signals are sent at
interpreter shutdown; `flush()` / `close()` remain available for explicit
control in short-lived processes.

## Design

- **Transport:** an in-memory buffer drained by a daemon thread every
  `flush_interval` (default 5s) or immediately when `max_batch` (default 30)
  items accumulate. `flush()` sends synchronously; `close()` flushes and stops.
- **HTTP:** `urllib.request` on the worker thread. `POST {proto}://{host}/api/{project_id}/envelope`
  with header `X-Sauron-Key: <public_key>`.
- **No auto-instrumentation** — this is a plain server-side dispatch API.

## Tests

```bash
cd sdks/python && python -m pytest -q
# or
cd sdks/python && python -m unittest
```
