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
