# Python SDK — `sauron-sdk`

Server-side Python SDK. Dispatches product-analytics events and exceptions over a
buffered background HTTP transport (a daemon thread draining an in-memory queue via
`urllib`). **Stdlib only — no runtime dependencies.** Source:
[`sdks/python`](../sdks/python). SDK header name: `sauron-python`.

Server-side = no browser/DOM/auto-instrumentation. The surface is init/config,
`track`, `capture_exception`/`capture_message`, `identify`, `flush`/`close`.

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
| `debug` | `False` | log to stderr |
| `sender` | `None` | optional HTTP sender `(url, headers, body) -> status` (mainly for tests) |

`init` returns the created `Client` (or `None` when disabled); `sauron.get_client()`
returns the active client.

## API

| Function | Signature |
| --- | --- |
| `track` | `track(event: str, distinct_id: str, properties: Mapping \| None = None) -> None` |
| `capture_exception` | `capture_exception(error: BaseException \| None = None, *, user=None, level="error", tags=None) -> str \| None` |
| `capture_message` | `capture_message(message: str, level: str = "info") -> str \| None` |
| `identify` | `identify(distinct_id: str, traits: Mapping \| None = None) -> None` |
| `flush` | `flush(timeout: float \| None = None) -> bool` |
| `close` | `close(timeout: float \| None = None) -> None` |

`distinct_id` is **required** on `track` (per the wire contract).

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

You can also pass an explicit exception, plus `user=` and `level=`. It returns the
event id (or `None` when disabled). `capture_message("worker started", level="info")`
sends a bare message.

### Identify a user

```python
sauron.identify("u_123", traits={"plan": "pro"})
```

### Flush / close

`flush()` sends the buffer synchronously; `close()` flushes then stops the background
thread. Call these before a short-lived process exits:

```python
sauron.flush()
sauron.close()
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
