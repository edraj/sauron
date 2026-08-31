# sauron-sdk (Python)

Server-side Python SDK for the [Sauron](https://github.com/edraj/sauron)
observability + analytics gateway. It dispatches product-analytics events,
identifies, performance transactions and exceptions to the Sauron ingest
endpoint over a buffered background HTTP transport.

This is a **server-side** SDK — it is meant for web backends, workers, CLIs and
daemons. If you are instrumenting a browser page use `@edraj/sauron-browser`
(`sdks/js`); for a Node.js service use `@edraj/sauron-node` (`sdks/node`).

- Captures errors (with stack traces and in-app frame detection), analytics
  events, identifies, breadcrumbs and manual performance transactions.
- **Zero runtime dependencies** — the transport is stdlib `urllib` +
  `threading`; gzip is stdlib `gzip`.
- Per-request isolation built on `contextvars`, so concurrent requests, threads
  and `asyncio` tasks never leak each other's user/tags/breadcrumbs.
- Byte-bounded outbound queue with optional on-disk persistence, gzip
  compression, and a retry/backoff policy that honors `Retry-After`.
- Ships `py.typed`, so `mypy`/`pyright` resolve the SDK's inline annotations
  with no stub package.
- No auto-instrumentation. Nothing is installed into your process unless you
  opt in (`auto_capture_unhandled`).

## Install

```bash
pip install sauron-sdk
```

Requires Python **3.9** or newer. The import name is `sauron`; the PyPI
distribution name is `sauron-sdk`.

## Quick start

```python
import sauron

sauron.init(dsn="https://pk_live_xxx@ingest.sauron.example/1")

# Product analytics — distinct_id is required by the wire contract.
sauron.track("checkout_completed", distinct_id="u_123",
             properties={"cart_value": 42.5})

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

A DSN looks like `https://<public_key>@<host>/<environment_id>`. The SDK derives the
ingest endpoint from it and POSTs to
`{protocol}://{host}/api/{environment_id}/envelope` with the header
`X-Sauron-Key: <public_key>`.

## Configuration

`sauron.init()` takes `dsn` positionally (or by keyword); **every other option
is keyword-only**.

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `Optional[str]` | `None` | `https://<public_key>@<host>/<environment_id>`. Empty/`None` puts the SDK in disabled no-op mode (never raises); a non-empty malformed value raises `DsnError`. |
| `release` | `Optional[str]` | `None` | Release/version string, stamped onto the envelope header. |
| `sample_rate` | `float` | `1.0` | Fraction of **errors** kept (`capture_exception` only). Events, identifies, messages and transactions are never sampled. |
| `flush_interval` | `float` | `5.0` | Seconds the background worker waits between drains. |
| `max_batch` | `int` | `30` | Queued-item count that triggers an immediate drain instead of waiting for `flush_interval`. |
| `max_breadcrumbs` | `int` | `100` | Breadcrumb ring size on the global scope; scopes cloned afterwards inherit it. |
| `tags` | `Optional[Mapping[str, Any]]` | `None` | Process-wide default tags; seeded onto the global scope. |
| `contexts` | `Optional[Mapping[str, Any]]` | `None` | Process-wide default context blocks; seeded onto the global scope. |
| `extra` | `Optional[Mapping[str, Any]]` | `None` | Process-wide default extra keys; seeded onto the global scope. |
| `gzip_threshold_bytes` | `int` | `1024` | Bodies **larger than** this are gzipped and sent with `Content-Encoding: gzip`. |
| `max_queue_bytes` | `int` | `1_048_576` (1 MiB) | Byte budget for the pending queue; oldest entries are evicted past it (the newest entry is always kept). |
| `offline_path` | `Optional[str]` | `None` | Opt-in directory for FIFO disk persistence of pending items (reloaded on init, deleted on delivery or permanent rejection). Off by default. |
| `before_send` | `Optional[Callable[..., Optional[Dict[str, Any]]]]` | `None` | `fn(item, hint)` run on **every** outgoing item. Return `None` to drop, or a dict to replace. A hook that raises drops that item and never propagates. |
| `before_breadcrumb` | `Optional[Callable[[Dict[str, Any]], Optional[Dict[str, Any]]]]` | `None` | `fn(crumb)` run before a breadcrumb is recorded. Return `None` to drop, or a dict to replace. A hook that raises drops the crumb. |
| `auto_capture_unhandled` | `bool` | `False` | Opt in to `sys.excepthook` / `threading.excepthook` capture of uncaught exceptions. |
| `debug` | `bool` | `False` | Print `[sauron] ...` diagnostics to `stderr`. |
| `sender` | `Optional[Any]` | `None` | Replacement HTTP sender `(url, headers, body) -> status` (or `(status, headers)`). Mainly for tests. |

Fully populated:

```python
import sauron

def scrub(item, hint=None):
    # Runs on error / event / identify / transaction items alike.
    if item["type"] == "event":
        item.get("properties", {}).pop("email", None)
    return item

def quiet(crumb):
    return None if crumb.get("category") == "noise" else crumb

sauron.init(
    "https://pk_live_xxx@ingest.sauron.example/1",
    release="api@2.4.1",
    sample_rate=0.25,
    flush_interval=2.0,
    max_batch=50,
    max_breadcrumbs=50,
    tags={"service": "checkout"},
    contexts={"deploy": {"region": "eu-west-1"}},
    extra={"build": "abc123"},
    gzip_threshold_bytes=2048,
    max_queue_bytes=4 * 1024 * 1024,
    offline_path="/var/lib/myapp/sauron-queue",
    before_send=scrub,
    before_breadcrumb=quiet,
    auto_capture_unhandled=True,
    debug=False,
)
```

Every capture/track function is a **silent no-op before `init`**, after
`close()`, **and** after the transport auto-disables itself on a `401`/`403`
(see [`Client`](#client)'s `enabled` attribute below) — the last one can
happen mid-process with no `close()` call from you at all. So instrumented
library code is safe to import in a process that never configures a DSN.
`start_workflow`/`end_workflow`/`cancel_workflow` follow the same rule but
aren't silent about it the way the `None`-returning functions are — each
returns `WorkflowResult(status="disabled")` so callers that branch on the
status still get a well-formed value.

## API reference

### `init`

```python
sauron.init(dsn: Optional[str] = None, *, ...) -> Optional[Client]
```

Creates the process-wide client, starts the background transport thread, seeds
the global scope with `tags`/`contexts`/`extra`, and registers an `atexit`
handler that closes (flush + stop) the client at interpreter shutdown. The
`atexit` handler is registered exactly once per process, no matter how many
times `init` is called.

Arguments are documented in [Configuration](#configuration).

**Returns** the created `Client`, or `None` when no DSN was supplied. **Raises**
`DsnError` when a non-empty DSN is malformed.

```python
import os
import sauron

client = sauron.init(os.environ.get("SAURON_DSN"))
if client is None:
    print("sauron disabled: no DSN")
```

### `get_client`

```python
sauron.get_client() -> Optional[Client]
```

Returns the active global client, or `None` when the SDK is disabled or has been
closed. Useful for feature-gating instrumentation without duplicating the DSN
check.

```python
if sauron.get_client() is not None:
    sauron.track("expensive_metric", distinct_id=user_id, properties=compute())
```

### `capture_exception`

```python
sauron.capture_exception(
    error: Optional[BaseException] = None,
    *,
    user: Optional[Mapping[str, Any]] = None,
    level: str = "error",
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
    fingerprint: Optional[Sequence[str]] = None,
) -> Optional[str]
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `error` | `Optional[BaseException]` | `None` | The exception to report. When omitted the SDK reads the active exception via `sys.exc_info()[1]`. |
| `user` | `Optional[Mapping[str, Any]]` | `None` | Per-call user. Reduced to `{"id", "email", "username"}` — other keys are discarded. Wins over the scope user. |
| `level` | `str` | `"error"` | One of `debug`, `info`, `warning`, `error`, `fatal`. Any other value falls back to `"error"`. |
| `tags` | `Optional[Mapping[str, Any]]` | `None` | Per-call tags, merged over scope tags key-by-key. |
| `contexts` | `Optional[Mapping[str, Any]]` | `None` | Per-call context blocks, merged over scope contexts by block name. |
| `extra` | `Optional[Mapping[str, Any]]` | `None` | Per-call extra keys, merged over scope extra key-by-key. |
| `fingerprint` | `Optional[Sequence[str]]` | `None` | Client-supplied grouping override, honored verbatim by the backend. |

**Returns** the generated 32-char hex `event_id`, or `None` when the SDK is
disabled, when there was no exception to capture, or when `sample_rate` dropped
it. The error carries the active scope's user, tags, contexts, extra and
breadcrumb trail, plus an extracted stack trace (call site first, crashing frame
last) with a per-frame `in_app` flag.

```python
try:
    charge(order)
except PaymentError as exc:
    event_id = sauron.capture_exception(
        exc,
        level="fatal",
        user={"id": "u_123", "email": "a@b.co"},
        tags={"area": "billing"},
        contexts={"order": {"id": order.id}},
        extra={"attempt": 3},
        fingerprint=["billing", "PaymentError"],
    )
```

Calling it with no argument inside an `except` block is the common case:

```python
try:
    do_work()
except Exception:
    sauron.capture_exception()
```

### `capture_message`

```python
sauron.capture_message(
    message: str,
    level: str = "info",
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> Optional[str]
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `message` | `str` | — (required) | The message body. |
| `level` | `str` | `"info"` | One of `debug`, `info`, `warning`, `error`, `fatal`. Any other value falls back to `"info"`. |
| `tags` | `Optional[Mapping[str, Any]]` | `None` | Per-call tags, merged over scope tags. |
| `contexts` | `Optional[Mapping[str, Any]]` | `None` | Per-call context blocks, merged over scope contexts. |
| `extra` | `Optional[Mapping[str, Any]]` | `None` | Per-call extra keys, merged over scope extra. |

**Returns** the generated `event_id`, or `None` when disabled. Messages are sent
as error items with no `exception` payload and are **not** affected by
`sample_rate`. Unlike `capture_exception`, `capture_message` takes no per-call
`user` — the scope user is attached instead.

```python
sauron.capture_message(
    "queue depth above threshold",
    "warning",
    tags={"queue": "emails"},
    extra={"depth": 12_000},
)
```

### `track`

```python
sauron.track(
    event: str,
    distinct_id: str,
    properties: Optional[Mapping[str, Any]] = None,
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `event` | `str` | — (required) | Event name. |
| `distinct_id` | `str` | — (required) | Person identifier. An empty value drops the event (logged when `debug=True`) — except for the three reserved `$workflow_start`/`$workflow_end`/`$workflow_cancel` names, which are [deliberately exempt](#start_workflow). |
| `properties` | `Optional[Mapping[str, Any]]` | `None` | Event properties; serialized as `{}` when omitted. |
| `tags` | `Optional[Mapping[str, Any]]` | `None` | Per-call tags, merged over scope tags. |
| `contexts` | `Optional[Mapping[str, Any]]` | `None` | Per-call context blocks, merged over scope contexts. |
| `extra` | `Optional[Mapping[str, Any]]` | `None` | Per-call extra keys, merged over scope extra. |

**Returns** `None`. Analytics events pick up the scope's tags/contexts/extra but
not its user or breadcrumbs.

```python
sauron.track(
    "checkout_completed",
    "u_123",
    {"cart_value": 42.5, "currency": "EUR"},
    tags={"experiment": "new_checkout"},
    extra={"coupon": "SUMMER"},
)
```

### `identify`

```python
sauron.identify(
    distinct_id: str,
    traits: Optional[Mapping[str, Any]] = None,
) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `distinct_id` | `str` | — (required) | Person identifier. An empty value drops the item. |
| `traits` | `Optional[Mapping[str, Any]]` | `None` | Person traits; serialized as `{}` when omitted. |

**Returns** `None`. Identify items are not scope-merged; they carry only the
distinct id and traits.

```python
sauron.identify("u_123", {"plan": "pro", "seats": 12, "country": "DZ"})
```

### `track_transaction`

```python
sauron.track_transaction(
    name: str,
    *,
    op: str = "custom",
    duration_ms: float,
    status: Optional[str] = None,
    http_method: Optional[str] = None,
    http_status: Optional[int] = None,
    url: Optional[str] = None,
    distinct_id: Optional[str] = None,
    tags: Optional[Dict[str, str]] = None,
    extra: Optional[Dict[str, Any]] = None,
) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `str` | — (required) | Transaction name, e.g. `"GET /api/users"`. |
| `op` | `str` | `"custom"` | Operation class, e.g. `"http"`, `"db"`. An empty string falls back to `"custom"`. |
| `duration_ms` | `float` | — (**required, keyword-only**) | Measured duration in milliseconds; coerced with `float()`. |
| `status` | `Optional[str]` | `None` | Outcome string, e.g. `"ok"`, `"internal_error"`. |
| `http_method` | `Optional[str]` | `None` | HTTP method when `op="http"`. |
| `http_status` | `Optional[int]` | `None` | HTTP response status. |
| `url` | `Optional[str]` | `None` | Request path or URL. |
| `distinct_id` | `Optional[str]` | `None` | Person identifier. When omitted it falls back to the active scope user's `id` (and stays `None` if there is no scope user). |
| `tags` | `Optional[Dict[str, str]]` | `None` | Indexed string→string labels. Filter with `@tag.key:value` on the Transactions page. |
| `extra` | `Optional[Dict[str, Any]]` | `None` | Freeform JSON — request body, response body, SQL text, row counts. Searchable with `extra.key:value`. |

**Returns** `None`. Timing is entirely manual — the SDK does not instrument
anything for you.

**`tags` and `extra` are per-call only.** Unlike `track()` and
`capture_exception()`, a transaction does **not** inherit the scope:
`set_tag()` / `set_extra()` defaults are not merged in. Transactions are the
highest-volume signal a service emits — one per request and per query — so
inheriting a global blob would write it onto every row.

`extra` is serialized and capped at **16 KB**
(`sauron._transaction_extra.MAX_TRANSACTION_EXTRA_BYTES`). Past that the whole
map is replaced with `{"_truncated": True, "_bytes": N}` and the dashboard says
so on the row. The cap is not cosmetic: envelopes are batched, and one
oversized body would push the whole envelope past the ingest limit and drop
every unrelated span sent with it. A value that cannot be JSON-encoded at all
becomes the same marker with `"_bytes": -1` rather than raising.

Nothing in `extra` is scrubbed. Use `before_send` for redaction, and think twice
before attaching a body that can carry tokens, passwords or personal data.

```python
import time

started = time.perf_counter()
resp = handler(request)
sauron.track_transaction(
    "GET /api/users",
    op="http",
    duration_ms=(time.perf_counter() - started) * 1000,
    status="ok",
    http_method="GET",
    http_status=resp.status_code,
    url="/api/users",
    distinct_id="u_123",
)
```

#### Example: an HTTP call, with request and response bodies

```python
import json
import time

import requests

import sauron


def create_order(payload: dict) -> dict:
    url = "https://api.example.com/orders"
    body = json.dumps(payload)
    started = time.perf_counter()

    try:
        resp = requests.post(
            url, data=body, headers={"content-type": "application/json"}, timeout=10
        )
        sauron.track_transaction(
            "POST /orders",  # the grouping key — keep it low cardinality
            op="http",
            duration_ms=(time.perf_counter() - started) * 1000,
            http_method="POST",
            http_status=resp.status_code,
            url=url,
            status="ok" if resp.ok else "error",
            tags={"api": "orders", "tier": current_plan},
            extra={
                "request": body,
                "response": resp.text,
                "response_bytes": len(resp.content),
                # Header VALUES are omitted on purpose — `authorization`
                # lives there.
                "request_headers": sorted(resp.request.headers),
            },
        )
        resp.raise_for_status()
        return resp.json()
    except requests.RequestException as exc:
        sauron.track_transaction(
            "POST /orders",
            op="http",
            duration_ms=(time.perf_counter() - started) * 1000,
            http_method="POST",
            url=url,
            status="error",
            extra={"request": body, "error": str(exc)},
        )
        raise
```

On the dashboard: **Transactions → the row → expand**. Both bodies render as a
JSON tree, and every one of these finds it:

```text
extra.response:~9001        # substring, inside the stored response body
@tag.api:orders             # indexed tag
op:http http.status:>=500   # the failures
duration:>2s                # the slow ones
```

#### Example: a SQL query (`psycopg`)

Put the **statement** in `extra` and keep `name` a stable label — a query with
literals baked in would mint a new dashboard row per execution. A context
manager keeps the timing and the error path in one place.

```python
import time
from contextlib import contextmanager

import psycopg

import sauron


@contextmanager
def traced_query(label: str, statement: str, params: tuple = ()):
    # Times a query and records it as a span. Yields the cursor.
    started = time.perf_counter()
    extra = {"statement": statement, "params": params}
    status = "ok"
    try:
        with conn.cursor() as cur:
            cur.execute(statement, params)
            yield cur
            extra["row_count"] = cur.rowcount
    except psycopg.Error as exc:
        extra["error"] = str(exc)
        status = "error"
        raise
    finally:
        sauron.track_transaction(
            label,  # the LABEL, not the statement
            op="db",
            duration_ms=(time.perf_counter() - started) * 1000,
            status=status,
            tags={"db": "postgres", "table": "orders"},
            # Bind PARAMETERS are user data. Log them only if you have decided
            # that is acceptable, or log their shape instead.
            extra=extra,
        )


with traced_query(
    "SELECT orders",
    "SELECT id, total FROM orders WHERE user_id = %s ORDER BY created_at DESC LIMIT 20",
    (user_id,),
) as cur:
    rows = cur.fetchall()
```

Then `@tag.table:orders duration:>500ms` is your slow-query list.

### `start_workflow`

```python
sauron.start_workflow(
    name: str,
    *,
    force: bool = False,
) -> WorkflowResult
```

A workflow is a named, explicitly-bounded span of activity — start it, let
events/errors/transactions happen inside it, then end (or cancel) it.
**Workflows are entirely optional**: an app that never calls `start_workflow`
never sees `workflow_id`/`workflow_name` on any item — the keys are omitted
entirely, never sent as `null`.

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `str` | — (required) | Workflow name. Trimmed; rejected (`invalid_name`) if empty after trimming or over 120 characters — never truncated. |
| `force` | `bool` | `False` | When `True`, supersede an already-active workflow instead of returning `already_active`: emits `$workflow_cancel` (`reason="superseded"`) for the old one, then starts the new. |

**Returns** a `WorkflowResult(status, workflow_id)`. `workflow_id` is a fresh,
**client-generated UUID v4** minted by `start_workflow` itself — never a
session id, a device id, or anything derived from the name. (The server rolls
counters up on `(app_id, workflow_id)` across the whole app, so a
deterministic or reused id would merge unrelated environments'/sessions'
counts into one row.)

| Status | Meaning |
| --- | --- |
| `ok` | Started (or, with `force=True`, superseded the previous one). `workflow_id` is set. |
| `invalid_name` | `name` is empty after trimming, or over 120 characters. Only reachable from `start_workflow` — a malformed name passed to `end_workflow`/`cancel_workflow` is `name_mismatch` instead (see below). |
| `already_active` | Another workflow is already active on this scope and `force` was not passed. |
| `disabled` | No client, the client is closed/disabled, or an unexpected internal error. Never raises. |

The scope's workflow is set **before** `$workflow_start` is emitted, so the
start event itself carries the new `workflow_id`/`workflow_name`. From then on,
every `track` / `capture_exception` / `capture_message` / `track_transaction`
call is stamped with `workflow_id` and `workflow_name` — **never** `identify`
(the server has no workflow columns for it). See
[Workflows are request-scoped](#workflows-are-request-scoped-not-global) below
for what "the active scope" means under concurrency.

```python
result = sauron.start_workflow("checkout")
if result.status == "ok":
    ...  # result.workflow_id
```

**Who the lifecycle events are attributed to.** The three `$workflow_*` events
carry the active scope user's `id` as their `distinct_id`
(`sauron.set_user({"id": ...})`), or an **empty string** when nobody has been
identified. Empty is intentional, not a fallback gap: the server stores an
empty `distinct_id` as `NULL`, and its `unique_users` metric counts distinct
non-`NULL` values — so an anonymous workflow run correctly contributes nothing
to that count instead of collapsing every anonymous run in the process onto one
synthetic user. These three reserved events are the only ones exempt from
`track`'s "an empty `distinct_id` drops the event" rule; ordinary `track` calls
are unaffected.

### `end_workflow`

```python
sauron.end_workflow(name: Optional[str] = None) -> WorkflowResult
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `Optional[str]` | `None` | When given, must match the active workflow's (trimmed) name or the call is a no-op. `None` ends whichever workflow is active. |

**Returns** a `WorkflowResult`.

| Status | Meaning |
| --- | --- |
| `ok` | Ended. The emitted `$workflow_end` carries `duration_ms` — wall-clock time since `start_workflow`. |
| `not_active` | No workflow is active on this scope. |
| `name_mismatch` | `name` was given and doesn't match the active workflow — **including** when `name` is itself blank or over 120 characters. (`invalid_name` is reachable only from `start_workflow`; a malformed name here is just another kind of mismatch.) |
| `disabled` | No client, the client is closed/disabled, or an unexpected internal error. |

Emits `$workflow_end` **while the workflow is still active** (so the event is
itself stamped with it), then clears it. Never auto-cancels or otherwise
fabricates a lifecycle event on its own — this is the only way a workflow's
state changes.

### `cancel_workflow`

```python
sauron.cancel_workflow(
    name: Optional[str] = None,
    *,
    reason: Optional[str] = None,
) -> WorkflowResult
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `Optional[str]` | `None` | Same matching rule as `end_workflow`. |
| `reason` | `Optional[str]` | `None` → `"user"` | Trimmed and capped at 120 characters (never truncated *before* the cap — the trim happens first). `start_workflow(..., force=True)`'s internal supersede call passes the literal reason `"superseded"` through this same normalization. |

**Returns** a `WorkflowResult` with the same status table as `end_workflow`.
Emits `$workflow_cancel` (carrying `duration_ms` **and** `reason`) instead of
`$workflow_end` — `$workflow_end` never carries a `reason`.

### `get_workflow`

```python
sauron.get_workflow() -> Optional[ActiveWorkflow]
```

**Returns** the active `ActiveWorkflow` (`workflow_id: str`, `name: str`,
`started_at: datetime`) on the current scope, or `None`. A plain state read —
it works before `init` and is unaffected by the client being closed/disabled
(unlike the three mutators above, it never returns a `disabled` status; there
is nothing to be disabled about a getter).

The server derives a workflow as **abandoned** after 30 minutes without a new
stamped event — computed on read, never stored, and requiring no action from
the SDK. A workflow that later receives another stamped event (even one sent
well past the 30-minute mark) simply reads as active again.

### Workflows are request-scoped, not global

The active workflow is a plain field, `Scope.workflow` — there is **no
separate module-level workflow state**. It lives on the exact same per-scope
`contextvars.ContextVar` as `tags`/`user`/`breadcrumbs` (see
[Isolation semantics](#isolation-semantics--contextvars-not-thread-locals)
below), so the same rules apply, concretely:

- Push a scope per request/task (`with sauron.scope():`) so
  `start_workflow`'s `already_active` check — and every stamp it produces —
  is evaluated **per request**, not against whatever the previous request left
  on the shared global scope. Skipping this leaks exactly like an un-scoped
  `set_tag` would: concurrent requests share one workflow, and one request's
  `start_workflow` can stamp another's errors.
- A `threading.Thread` or a `ThreadPoolExecutor` submission starts with a
  **fresh, empty context** — it does not inherit the spawning thread's pushed
  scope — so a workflow started on the request thread is invisible to code
  that runs on a worker thread/executor. Propagate it explicitly (pass the
  `ActiveWorkflow`/`workflow_id` in) or run the work with
  `contextvars.copy_context().run(...)`.
- `asyncio.create_task` / `TaskGroup` **does** copy the current context at
  creation time: a workflow started before spawning a child task is visible
  inside it, while a workflow started **inside** a child task stays local to
  that task and is invisible to its parent or to sibling tasks.

### `add_breadcrumb`

```python
sauron.add_breadcrumb(
    *,
    type: Optional[str] = None,
    category: Optional[str] = None,
    message: Optional[str] = None,
    level: Optional[str] = None,
    data: Optional[Mapping[str, Any]] = None,
) -> None
```

All parameters are **keyword-only**.

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `type` | `Optional[str]` | `None` → `"default"` | Crumb type, e.g. `"http"`, `"navigation"`. |
| `category` | `Optional[str]` | `None` | Free-form category. |
| `message` | `Optional[str]` | `None` | Human-readable message. |
| `level` | `Optional[str]` | `None` | Severity string; not validated. |
| `data` | `Optional[Mapping[str, Any]]` | `None` → `{}` | Structured payload. |

**Returns** `None`. The crumb is stamped with an ISO-8601 `timestamp` and
appended to the **active scope's** bounded ring (oldest dropped past
`max_breadcrumbs`). With a client initialized the `before_breadcrumb` hook runs
first; before `init` the crumb is written straight to the global scope with no
hook and never raises.

```python
sauron.add_breadcrumb(
    type="http",
    category="request",
    message="GET /cart",
    level="info",
    data={"status": 200, "duration_ms": 31},
)
```

`sauron.build_breadcrumb(...)` — the function that shapes the dict — is
importable from the package but is deliberately **not** part of `__all__`; use
`add_breadcrumb` instead.

### `set_user`

```python
sauron.set_user(user: Optional[Mapping[str, Any]]) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `user` | `Optional[Mapping[str, Any]]` | — (required) | The fallback user for the active scope. Stored verbatim (all keys kept). Pass `None` to clear it. |

**Returns** `None`. Applies to the active scope, so inside a `with
sauron.scope():` block it is request-local.

```python
sauron.set_user({"id": "u_123", "email": "a@b.co", "username": "ada"})
sauron.set_user(None)  # log out
```

### `set_tag`

```python
sauron.set_tag(key: str, value: Any) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `str` | — (required) | Tag name. |
| `value` | `Any` | — (required) | Tag value. |

**Returns** `None`.

```python
sauron.set_tag("request_id", "req_42")
```

### `set_tags`

```python
sauron.set_tags(tags: Mapping[str, Any]) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `tags` | `Mapping[str, Any]` | — (required) | Tags merged into the active scope's tags (`dict.update` semantics). |

**Returns** `None`.

```python
sauron.set_tags({"tier": "pro", "region": "eu-west-1"})
```

### `set_context`

```python
sauron.set_context(key: str, value: Any) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `str` | — (required) | Context **block** name. |
| `value` | `Any` | — (required) | The whole block; replaces any block already stored under `key`. |

**Returns** `None`. Contexts merge by block name, not by inner key.

```python
sauron.set_context("order", {"id": 7, "items": 3})
```

### `set_extra`

```python
sauron.set_extra(key: str, value: Any) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `str` | — (required) | Extra key. |
| `value` | `Any` | — (required) | Any JSON-serializable value (non-serializable values fall back to `str()` at send time). |

**Returns** `None`.

```python
sauron.set_extra("build", "abc123")
```

### `scope`

```python
with sauron.scope() as s:  # -> Iterator[Scope]
    ...
```

The context manager form of `push_scope` / `pop_scope`. Clones the active scope,
makes the clone active for the block, and restores the parent on exit — including
on an exception. This is the recommended way to get per-request isolation.

**Yields** the child `Scope` (so you can call its methods directly instead of the
module-level setters).

```python
with sauron.scope() as s:
    s.set_user({"id": "u_123"})
    s.set_tag("request_id", "req_42")
    sauron.add_breadcrumb(category="db", message="SELECT orders")
    handle(request)
# tags/user/breadcrumbs added inside the block are gone here
```

### `push_scope`

```python
sauron.push_scope() -> Scope
```

Clones the active scope and makes the clone active. **Returns** the clone. Pair
every `push_scope()` with a `pop_scope()` — prefer `with sauron.scope():` unless
your framework's enter/exit hooks are separate callbacks (see the Flask recipe).

```python
child = sauron.push_scope()
child.set_tag("job", "nightly-rollup")
try:
    run_job()
finally:
    sauron.pop_scope()
```

### `pop_scope`

```python
sauron.pop_scope() -> None
```

Restores the parent of the active scope. **Returns** `None`. A no-op when the
global scope is already active — it never raises on an unbalanced call.

### `get_current_scope`

```python
sauron.get_current_scope() -> Scope
```

**Returns** the active `Scope`: the innermost pushed scope for the current
context, or the global scope when none is pushed.

```python
sauron.get_current_scope().set_tag("shard", "3")
```

### `get_global_scope`

```python
sauron.get_global_scope() -> Scope
```

**Returns** the single process-wide `Scope` that every pushed scope is
ultimately cloned from. `init`'s `tags`/`contexts`/`extra` are seeded here.

```python
sauron.get_global_scope().set_tags({"service": "checkout"})
```

### `configure_scope`

```python
sauron.configure_scope(callback: Callable[[Scope], Any]) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `callback` | `Callable[[Scope], Any]` | — (required) | Invoked synchronously with the **active** scope. Its return value is ignored. Exceptions are not caught. |

**Returns** `None`.

```python
def with_request(request):
    def apply(s):
        s.set_user({"id": request.user_id})
        s.set_tag("route", request.route)
        s.set_context("http", {"method": request.method, "path": request.path})
    sauron.configure_scope(apply)
```

### `flush`

```python
sauron.flush(timeout: Optional[float] = None) -> bool
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `timeout` | `Optional[float]` | `None` | Accepted for symmetry with `close()`. The drain runs synchronously on the calling thread and is not bounded by it. |

**Returns** `True` — including when the SDK is disabled (nothing to send).
Drains the queue in envelopes of at most 1000 items until it is empty or a send
fails transiently.

```python
sauron.capture_message("job finished")
sauron.flush()  # block until the buffer has been POSTed
```

### `close`

```python
sauron.close(timeout: Optional[float] = None) -> None
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `timeout` | `Optional[float]` | `None` | Seconds to wait for the background worker thread to join. `None` waits indefinitely. |

**Returns** `None`. Uninstalls the uncaught-exception hooks (if
`auto_capture_unhandled` was on), stops the worker, flushes anything still
buffered on the calling thread, marks the client disabled, and clears the global
client — so every subsequent capture/track call is a no-op until you `init`
again.

```python
import atexit
atexit.register(lambda: sauron.close(timeout=2))
```

### `Client`

```python
sauron.Client(dsn: str, *, release: Optional[str] = None, ...) -> Client
```

The class behind `init`. Constructing one directly gives you an isolated client
that the module-level functions do **not** route through — useful in tests and
for multi-tenant dispatch. It takes the same keyword options as `init` (minus
`dsn`, which is a required positional here), starts its own transport thread,
and is not registered with `atexit`, so you must `close()` it yourself.

Attributes: `dsn` (a parsed `Dsn`), `release`, `sample_rate`, `enabled` (a
read-only property — assigning to it raises `AttributeError`). `enabled` is
`True` only when the client has not been `close()`d **and** its transport has
not auto-disabled itself; a `401`/`403` disables the transport permanently
for the process (see [Retry vs drop](#transport--delivery)), so `enabled` can
flip to `False` mid-session with no `close()` call from you — check it (or
`get_client()`) after `flush()`/`track()` calls if you want to detect a
revoked key without waiting for a log line.

Methods mirror the module-level functions — `track`, `capture_exception`,
`capture_message`, `identify`, `track_transaction`, `start_workflow`,
`end_workflow`, `cancel_workflow`, `add_breadcrumb`, `flush`, `close` — with
one addition: `Client.capture_exception` accepts an extra keyword-only
`mechanism: Optional[Mapping[str, Any]]` that overrides the default
`{"type": "generic", "handled": True}`. The auto-capture hooks use it to mark
uncaught crashes `handled=False`. (`get_workflow`, like `get_current_scope`,
has no `Client` counterpart — it reads the active scope directly and needs no
client at all.)

```python
from sauron import Client

client = Client("https://pk@ingest.example/1",
                flush_interval=3600, max_batch=1000)
try:
    client.capture_message("hello from an isolated client")
    client.flush()
finally:
    client.close(timeout=2)
```

### `Scope`

```python
sauron.Scope(max_breadcrumbs: int = 100) -> Scope
```

One layer of ambient signal context. Attributes: `user`, `tags`, `contexts`,
`extra`, `breadcrumbs`, `max_breadcrumbs`, `workflow` (the active
`ActiveWorkflow`, or `None` — see [`start_workflow`](#start_workflow)).

| Method | Signature | Description |
| --- | --- | --- |
| `set_user` | `(user: Optional[Mapping[str, Any]]) -> Scope` | Set or clear the fallback user. |
| `set_tag` | `(key: str, value: Any) -> Scope` | Set one tag. |
| `set_tags` | `(tags: Mapping[str, Any]) -> Scope` | Merge several tags. |
| `set_context` | `(key: str, value: Any) -> Scope` | Set one context block. |
| `set_extra` | `(key: str, value: Any) -> Scope` | Set one extra key. |
| `add_breadcrumb` | `(crumb: Mapping[str, Any]) -> Scope` | Append a **pre-built** crumb dict, trimming to `max_breadcrumbs`. |
| `clear` | `() -> None` | Reset user/tags/contexts/extra/breadcrumbs/workflow. |
| `clone` | `() -> Scope` | An independent copy (what `push_scope` uses). |
| `apply_to_error` | `(item: Dict[str, Any]) -> None` | Stamp scope state onto an error item in place. |
| `apply_to_event` | `(item: Dict[str, Any]) -> None` | Stamp scope tags/contexts/extra onto an analytics item in place. |
| `apply_workflow` | `(item: Dict[str, Any]) -> None` | Stamp `workflow_id`/`workflow_name` onto an item in place when a workflow is active; omits both keys otherwise. Called from the client's single dispatch chokepoint for error/event/transaction items, not from `apply_to_error`/`apply_to_event` directly. |

The mutators return `self`, so they chain:

```python
with sauron.scope() as s:
    s.set_user({"id": "u_1"}).set_tag("area", "billing").set_extra("try", 2)
```

### `Dsn`, `parse_dsn`, `DsnError`

```python
sauron.parse_dsn(dsn: str) -> Dsn
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `str` | — (required) | The DSN string. |

**Returns** a `Dsn` with attributes `raw`, `public_key`, `host` (`host:port` when
a port is present), `hostname`, `protocol` (`http` or `https`, no colon),
`project_id` (the DSN's path segment — despite the name, this is the
**environment** id since the ingest key now lives on the environment, not the
app), and `envelope_url` (`{protocol}://{host}/api/{environment_id}/envelope`).

**Raises** `DsnError` (a subclass of `ValueError`, message prefixed
`[sauron] invalid DSN: `) when the DSN is empty or not a string, uses a protocol
other than `http`/`https`, is missing the public key, carries a password/secret
component, or is missing the host or the environment-id path segment.

```python
from sauron import DsnError, parse_dsn

try:
    dsn = parse_dsn("https://pk_live_xxx@ingest.sauron.example:8443/7")
except DsnError as exc:
    raise SystemExit(str(exc))

print(dsn.project_id)    # 7
print(dsn.host)          # ingest.sauron.example:8443
print(dsn.envelope_url)  # https://ingest.sauron.example:8443/api/7/envelope
```

### `SDK_NAME`, `SDK_VERSION`

Module constants (`str`) reported in the envelope header's `sdk` block:
`"sauron-python"` and `"1.5.0"`.

```python
import sauron
print(sauron.SDK_NAME, sauron.SDK_VERSION)  # sauron-python 1.5.0
```

## Scope & metadata

There are three places metadata can come from, and they merge in this
**precedence order (last wins)**:

1. **init defaults** — `tags` / `contexts` / `extra` passed to `init`, seeded
   onto the global scope.
2. **Scope values** — `set_tag` / `set_tags` / `set_context` / `set_extra` /
   `set_user` / `add_breadcrumb` on the active scope. A pushed scope starts as a
   clone of its parent, so it already contains the levels above it.
3. **Per-call arguments** — the `tags` / `contexts` / `extra` / `user` /
   `fingerprint` keywords on `capture_exception`, `capture_message` and `track`.

Merge granularity differs per block:

| Block | Merge granularity | Notes |
| --- | --- | --- |
| `tags` | per key | Per-call key overrides the scope's same key. |
| `contexts` | per **block name** | A per-call `"order"` block replaces the scope's `"order"` block wholesale — it is not deep-merged. |
| `extra` | per key | Shallow. |
| `user` | whole object | A per-call `user=` wins over the scope user, and is reduced to `id`/`email`/`username`. A scope user set with `set_user` keeps all its keys. |
| `breadcrumbs` | scope only | The active scope's trail (capped at `max_breadcrumbs`) is attached to errors and messages. |

Empty blocks are omitted rather than emitted as `{}`. `track`/`capture_*` never
send an empty `contexts` or `extra` key.

Which signals get what:

| Item | tags | contexts | extra | user | breadcrumbs |
| --- | --- | --- | --- | --- | --- |
| `capture_exception` | yes | yes | yes | yes | yes |
| `capture_message` | yes | yes | yes | scope only | yes |
| `track` | yes | yes | yes | no | no |
| `identify` | no | no | no | n/a | no |
| `track_transaction` | no | no | no | `distinct_id` falls back to the scope user's `id` | no |

### Isolation semantics — contextvars, not thread-locals

The active scope lives in a `contextvars.ContextVar`. Concretely:

- **asyncio**: a task created with `asyncio.create_task` / `TaskGroup` copies the
  *current* context at creation. A scope pushed **before** the task is created is
  therefore visible inside it; a scope pushed **inside** a task rebinds the var
  only in that task's own context, so it never leaks to the parent or to sibling
  tasks. That makes `with sauron.scope():` around a request handler correct even
  with high concurrency on one event loop.
- **Mutation vs rebinding**: the ContextVar holds a reference to a *mutable*
  `Scope`. `set_tag`/`set_user`/`add_breadcrumb` mutate that shared object, so a
  child task's mutations *are* visible to whoever pushed the scope. Only
  `push_scope`/`pop_scope` (rebinding) are context-local. Push a fresh scope when
  you need a child task's metadata to stay private.
- **Threads**: a new `threading.Thread` starts with a *fresh, empty* context, so
  it sees the **global** scope — it does **not** inherit the spawning thread's
  pushed scope. Pass what you need explicitly, or run the work with
  `contextvars.copy_context().run(...)`. The same applies to
  `ThreadPoolExecutor`, which does not copy the submitter's context.
- **Workflows**: `Scope.workflow` (see [`start_workflow`](#start_workflow)) is
  governed by the exact same two rules above, not a separate mechanism —
  `start_workflow`/`end_workflow`/`cancel_workflow` **assign** `scope.workflow`
  on whichever `Scope` is currently active, which is a *mutation* like
  `set_tag`, not a rebind. Push a scope per request so `already_active` and
  every stamp it produces are evaluated against that request only; skip the
  push and every concurrent caller shares (and can clobber) the one workflow
  living on the global scope.

### Thread safety and fork safety

- The transport and its queue are internally locked and safe to call from any
  thread; `track` / `capture_*` are non-blocking enqueues.
- `Scope` objects are **not** internally locked. Per-request scopes are isolated
  by construction, but the global scope is shared — set process-wide defaults at
  startup (or via `init`'s `tags`/`contexts`/`extra`), not from request threads.
- The transport worker is an ordinary daemon thread started in the `Client`
  constructor. Threads do not survive `os.fork()`, so a client created in a
  pre-fork parent has **no** worker in the child: items pile up in the child's
  queue and only leave on an explicit `flush()`/`close()`/`atexit`. Call `init`
  **inside** each worker process (gunicorn `post_fork`, uWSGI `@postfork`,
  `multiprocessing` child entry point).

## Framework integration

There is no auto-instrumentation; these are the recipes to wire it up by hand.

### Flask (WSGI)

```python
import time

import sauron
from flask import Flask, got_request_exception, request

app = Flask(__name__)


def _report(sender, exception, **extra):
    sauron.capture_exception(exception)


got_request_exception.connect(_report, app)


@app.before_request
def _sauron_push():
    sauron.push_scope()
    sauron.set_tag("route", request.endpoint or "unknown")
    sauron.set_context(
        "http", {"method": request.method, "path": request.path}
    )
    sauron.add_breadcrumb(
        type="http",
        category="request",
        message=f"{request.method} {request.path}",
    )
    request.environ["sauron.started"] = time.perf_counter()


@app.after_request
def _sauron_transaction(response):
    started = request.environ.get("sauron.started")
    if started is not None:
        sauron.track_transaction(
            f"{request.method} {request.url_rule or request.path}",
            op="http",
            duration_ms=(time.perf_counter() - started) * 1000,
            http_method=request.method,
            http_status=response.status_code,
            url=request.path,
        )
    return response


@app.teardown_request
def _sauron_pop(exc):
    # Always pop: a WSGI worker thread is reused, and an un-popped scope would
    # leak this request's user/tags into the next one served by that thread.
    sauron.pop_scope()
```

`got_request_exception` needs `blinker` (a Flask dependency since 2.3). Set the
user once you have authenticated the request:

```python
sauron.set_user({"id": current_user.id, "email": current_user.email})
```

Under gunicorn, initialize per worker and close on worker exit —
`gunicorn.conf.py`:

```python
import os

import sauron


def post_fork(server, worker):
    # After the fork, so the transport thread exists in this process.
    sauron.init(
        os.environ["SAURON_DSN"],
        release=os.environ.get("APP_RELEASE"),
        auto_capture_unhandled=True,
    )


def worker_exit(server, worker):
    sauron.close(timeout=5)
```

### Django

`myapp/apps.py` — initialize once per process:

```python
import os

import sauron
from django.apps import AppConfig


class MyAppConfig(AppConfig):
    name = "myapp"

    def ready(self):
        sauron.init(
            os.environ.get("SAURON_DSN"),
            release=os.environ.get("APP_RELEASE"),
            tags={"service": "django"},
        )
```

`myapp/middleware.py`:

```python
import time

import sauron


class SauronMiddleware:
    def __init__(self, get_response):
        self.get_response = get_response

    def __call__(self, request):
        with sauron.scope() as scope:
            scope.set_context(
                "http", {"method": request.method, "path": request.path}
            )
            if getattr(request, "user", None) and request.user.is_authenticated:
                scope.set_user(
                    {"id": str(request.user.pk), "email": request.user.email}
                )
            sauron.add_breadcrumb(
                type="http",
                category="request",
                message=f"{request.method} {request.path}",
            )
            started = time.perf_counter()
            response = self.get_response(request)
            sauron.track_transaction(
                f"{request.method} {request.resolver_match.route}"
                if request.resolver_match
                else f"{request.method} {request.path}",
                op="http",
                duration_ms=(time.perf_counter() - started) * 1000,
                http_method=request.method,
                http_status=response.status_code,
                url=request.path,
            )
            return response

    def process_exception(self, request, exception):
        # Django calls this inside get_response, so the scope above is active.
        sauron.capture_exception(exception)
        return None  # let Django's own handling continue
```

Register it in `settings.py`:

```python
MIDDLEWARE = [
    "myapp.middleware.SauronMiddleware",
    # ... the rest
]
```

Graceful shutdown is handled by the `atexit` hook `init` installs; under
gunicorn add the `worker_exit` hook from the Flask recipe for a bounded flush.
If you run gunicorn with `--preload`, move `init` into `post_fork` so the
transport thread exists in each worker.

### FastAPI / ASGI

```python
import os
import time
from contextlib import asynccontextmanager

import sauron
from fastapi import FastAPI, Request


@asynccontextmanager
async def lifespan(app: FastAPI):
    sauron.init(
        os.environ.get("SAURON_DSN"),
        release=os.environ.get("APP_RELEASE"),
        auto_capture_unhandled=True,
    )
    yield
    # Runs on graceful shutdown, before the process exits.
    sauron.close(timeout=5)


app = FastAPI(lifespan=lifespan)


@app.middleware("http")
async def sauron_scope(request: Request, call_next):
    # The scope is pushed in this task's context; the endpoint coroutine
    # inherits it, and nothing leaks between concurrent requests.
    with sauron.scope() as scope:
        scope.set_tag("route", request.url.path)
        scope.set_context(
            "http", {"method": request.method, "path": request.url.path}
        )
        sauron.add_breadcrumb(
            type="http",
            category="request",
            message=f"{request.method} {request.url.path}",
        )
        started = time.perf_counter()
        try:
            response = await call_next(request)
        except Exception as exc:
            sauron.capture_exception(exc)
            raise
        sauron.track_transaction(
            f"{request.method} {request.url.path}",
            op="http",
            duration_ms=(time.perf_counter() - started) * 1000,
            http_method=request.method,
            http_status=response.status_code,
            url=request.url.path,
        )
        return response
```

Set the user from a dependency once the request is authenticated — it lands on
the scope the middleware pushed:

```python
from fastapi import Depends


async def current_user(request: Request):
    user = await authenticate(request)
    sauron.set_user({"id": user.id, "email": user.email})
    return user


@app.get("/me")
async def me(user=Depends(current_user)):
    return {"id": user.id}
```

Note that `sauron.flush()` and `sauron.close()` are **synchronous** — they block
the calling thread. Do not call them from inside a hot request handler on the
event loop; use them at shutdown, or offload with
`asyncio.to_thread(sauron.flush)`.

Under a multi-worker uvicorn/gunicorn deployment the same fork rule applies:
`init` must run in the worker process (the `lifespan` handler above does, since
lifespan runs per worker).

## Transport & delivery

- **Batching**: items are enqueued non-blockingly and drained by a daemon thread
  named `sauron-transport` every `flush_interval` seconds (default `5.0`), or
  immediately once `max_batch` (default `30`) items are pending.
- **Envelope**: each flush builds exactly one envelope
  (`header` + `context` + `items[]`) and POSTs it to
  `{protocol}://{host}/api/{environment_id}/envelope` with
  `X-Sauron-Key: <public_key>` and `Content-Type: application/json`. The header
  carries the raw DSN, `sdk: {name, version}`, `sent_at` and `release`; the
  context carries a per-process `device.device_id` (a uuid4), the
  OS name from `platform.system()`, and `runtime: {name: "python", version}`.
- **Envelope size cap**: at most **1000 items** per envelope
  (`MAX_ITEMS_PER_ENVELOPE`), matching the server limit. A larger backlog drains
  as consecutive envelopes in one flush.
- **Gzip**: bodies **strictly larger** than `gzip_threshold_bytes` (default
  `1024`) are gzipped and sent with `Content-Encoding: gzip`; smaller bodies go
  out verbatim with no extra header.
- **Queue cap**: pending items are bounded by `max_queue_bytes` (default 1 MiB
  of serialized JSON). Past the budget the **oldest** entries are evicted, but a
  single oversized item is always kept so it still ships.
- **Offline persistence** (opt-in, `offline_path`): every pending item is also
  written to its own FIFO-named file under that directory. A fresh process
  reloads them on `init`, so delivery is at-least-once across restarts. Files are
  deleted on successful delivery *and* on permanent rejection (otherwise a poison
  payload would be replayed on every boot). A transient failure keeps them.
- **Retry policy**: up to **3 retries** (4 attempts total) per envelope, with
  exponential backoff and full jitter over a `0.1s` base, capped at **30s**.
  These two constants are transport-internal and not exposed through `init`.
- **Retry vs drop**:

  | Status | Behavior |
  | --- | --- |
  | `2xx` | Delivered; persisted copies deleted. |
  | `408`, `429`, any `5xx` | Retried with backoff. A `429` `Retry-After` header (delta-seconds or HTTP-date) is honored verbatim, capped at 30s. |
  | network error / raise / sender returns `None` | Retried with backoff. |
  | `413` | **Not** retried unchanged — the envelope is split in half and each half sent separately. A lone item that still does not fit is dropped. |
  | `401`, `403` | Hard auth failure: the client is **disabled for good**, the queue is cleared, and nothing further is sent until you `init` again. |
  | `400`, `404`, other `4xx` | Dropped without retry (retrying cannot help). |

- After the retry budget is exhausted the in-memory copies are gone but any
  persisted files remain for the next process.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives, no error, `init` returned `None` | No DSN was supplied — the SDK is in disabled no-op mode | Pass a DSN. Run with `debug=True` to see `[sauron] no DSN configured; SDK disabled`. |
| Nothing arrives, requests reach a proxy but never the ingest | The DSN cannot express a path prefix — ingest **must** be exposed at `/api/{environment_id}/envelope` on the host root | Fix the proxy/route so `POST /api/{environment_id}/envelope` on the DSN host reaches the ingest. Events dropped this way look delivered client-side. |
| `DsnError` at startup | Malformed DSN (bad protocol, missing key/host/environment id, or a password component) | Use `https://<public_key>@<host>/<environment_id>`, key only, no secret. |
| Events stop arriving after a while | A `401`/`403` disabled the client permanently and cleared the queue (`Client.enabled`/`get_client().enabled` flips to `False`, with no `close()` call from you) | Fix the public key, then re-`init`. `debug=True` logs `auth rejected (status=...), disabling`. |
| A short-lived script sends nothing | The process exited before the 5s flush tick | Call `sauron.flush()` or `sauron.close()` before exit (the `atexit` hook also closes, but only on a clean interpreter shutdown). |
| Nothing sends under gunicorn/uWSGI | `init` ran in the pre-fork parent, so the worker has no transport thread | `init` in `post_fork` / `@postfork` / the ASGI `lifespan`. |
| `capture_exception()` returned `None` | No active exception, `sample_rate` dropped it, or the client is closed/disabled | Pass the exception explicitly; raise `sample_rate`; check `sauron.get_client()`. |
| `track()`/`identify()` silently dropped | Empty `distinct_id` | Supply a non-empty id. `debug=True` logs `track() requires a distinct_id`. |
| Breadcrumbs/user missing from an error | They were set on a different scope — e.g. in a `threading.Thread`, which starts at the global scope | Set them on the scope that is active where you capture, or push a scope inside the thread. |
| Items disappear under burst load | The pending queue exceeded `max_queue_bytes` and evicted the oldest entries | Raise `max_queue_bytes`, lower `flush_interval`, or set `offline_path`. |
| `before_send` changes have no effect | The hook raised, so the item was dropped | Run with `debug=True` to see `before_send raised, dropping item`. |

Set `debug=True` in `init` to print all of the above diagnostics to `stderr`
with a `[sauron]` prefix.

## Development

```bash
cd sdks/python

python -m venv .venv && . .venv/bin/activate
pip install -e ".[test]"          # the only extra is `test` (pytest>=7)

python -m pytest -q               # test suite (testpaths = tests)
python -m unittest                # the same suite via stdlib unittest

pip install build twine           # not declared as deps
python -m build                   # -> dist/*.whl and dist/*.tar.gz
python -m twine check dist/*
```

There is no bundled linter or type-checker config; the package ships `py.typed`
so consumers' `mypy`/`pyright` pick up the inline annotations directly.

## License

LGPL-3.0-only — GNU Lesser General Public License v3.0. LGPLv3 applies on top of
the GNU GPL v3, whose text ships alongside it in `COPYING`.
