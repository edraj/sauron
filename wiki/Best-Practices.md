# Best Practices

Field-tested conventions for getting clean, groupable, privacy-safe data out of the
Sauron SDKs. Everything here matches the **shipped v0.3.0** APIs. For the exact JSON
each SDK emits see **[Ingest Wire Contract](Ingest-Wire-Contract.md)**; for per-SDK
signatures see the **[Browser](Browser-SDK.md)**, **[Flutter](Flutter-SDK.md)**,
**[Node](Node-SDK.md)**, **[Python](Python-SDK.md)**, and **[C#](CSharp-SDK.md)**
pages.

See also: **[Home](Home.md)** · **[Getting Started](Getting-Started.md)**

---

## 1. Event naming

An event `name` is a grouping key, not a sentence. Consistent names keep Funnels,
Journeys, and the Events table legible.

- **`object_action`, lowercase `snake_case`, past tense.** `order_completed`,
  `checkout_started`, `signup_completed`, `invite_sent`. Not `OrderCompleted`,
  `"User completed the order"`, or `click`.
- **Keep names low-cardinality.** The name identifies *what happened*; the specifics
  go in `properties`. Put the id/amount/plan in properties, never in the name:

  ```ts
  // good — one stable name, detail in properties
  track('order_completed', 'user-123', { order_id: 'o_88', total: 42.5, currency: 'USD' });

  // bad — unbounded cardinality, un-aggregatable
  track('order_completed_o_88_42.5', 'user-123');
  ```

- **Property keys are `snake_case` too**, and stable over time — renaming a property
  splits your history.
- **`$`-prefixed names are reserved** for SDK-internal events (the Browser/Flutter
  SDKs emit `$screen` on screen changes). Don't mint your own `$…` events.
- Pick the vocabulary once and write it down. A tiny shared list of event names beats
  five engineers inventing `purchase` / `bought` / `order_complete` separately.

The same discipline applies to `identify` **traits** (`plan`, `company_size`) and to
transaction **names**, which are the grouping key on Performance — use the route
template, not the concrete URL: `GET /users/:id`, not `GET /users/8412`.

---

## 2. Scrubbing PII with `beforeSend`

`beforeSend` is the single chokepoint every outgoing item passes through **just
before it is enqueued** for transport. It runs on **every** item type
(`error | event | identify | transaction`). Return the (mutated or replaced) item to
send it, or `null` to drop it entirely. It is the right place to strip emails, tokens,
auth headers, and anything else that must never leave the process.

> A hook that throws is caught and the item is dropped (Node/Python/C#) or logged
> (Browser); it never crashes your app. Keep it fast and total — it runs on the hot
> path.

**Node** — the item is a discriminated union; branch on `item.type`:

```ts
init({
  dsn,
  beforeSend(item) {
    if (item.type === 'error' && item.user) item.user.email = null;      // redact
    if (item.type === 'event') delete item.properties.password;          // strip
    if (item.type === 'identify' && item.traits) delete item.traits.ssn; // strip
    return item; // return null to drop the whole item
  },
});
```

**Python** — the item is a plain `dict`, `hint` is optional:

```python
def scrub(item, hint=None):
    if item["type"] == "event":
        item.get("properties", {}).pop("password", None)
    if item["type"] == "error" and item.get("user"):
        item["user"]["email"] = None
    return item  # return None to drop

sauron.init(dsn, before_send=scrub)
```

**C#** — `BeforeSend` is `Func<object, object?>`. Note the item record types
(`ErrorItem`, `EventItem`, …) are **`internal`** in 0.3.0, so type-based branching is
only available inside the SDK assembly. From application code the reliable strategy is
to **not collect the PII in the first place** (scrub before you call `Track` /
`CaptureException`) and use `BeforeSend` to drop whole items:

```csharp
SauronSdk.Init(new SauronOptions
{
    Dsn = dsn,
    // Example that works in-assembly; from app code prefer scrubbing at the call site.
    BeforeSend = item => item is IdentifyItem ? null : item, // drop all identify items
});
```

**Browser** — same discriminated-union shape as Node; branch on `item.type` and
return `null` to drop.

**Flutter** — the callback widened in 0.3.0 to run on *every* item (it was errors-only
before). Item fields are **immutable (`final`)**, so use it to **drop or replace whole
items**, and scrub sensitive values at the call site rather than mutating in place:

```dart
await Sauron.init((o) {
  o.dsn = dsn;
  o.beforeSend = (item) {
    if (item is EventItem && item.name == r'$secret') return null; // drop
    return item; // guard the type if you only care about a subset
  };
});
```

**Also scrub breadcrumbs.** Breadcrumb `data` can leak the same way. Node/Python/C#/
Browser expose a separate `beforeBreadcrumb` hook (`beforeBreadcrumb(crumb) => crumb |
null`) that runs before each crumb is stored — use it to redact `data` or drop noisy
crumbs. The cheapest PII policy of all is to never put secrets into event properties,
traits, tags, or breadcrumbs to begin with.

---

## 3. Sampling

`sampleRate` (a float in `[0, 1]`, default `1.0`) governs **error capture only**. A
rate of `0.25` keeps ~25% of captured errors and drops the rest before they are
buffered. **Analytics events, `identify`, and transactions are always sent** — they
are not subject to `sampleRate`.

| SDK | Option | Notes |
| --- | --- | --- |
| Node | `init({ sampleRate })` | clamped to `[0,1]` |
| Python | `init(dsn, sample_rate=…)` | |
| C# | `new SauronOptions { SampleRate = … }` | **uncaught crashes bypass sampling** (an unhandled exception is always kept) |
| Browser | `init({ sampleRate })` | |
| Flutter | `o.sampleRate = …` (clamped) | applies to error events only |

Guidance:

- **Leave it at `1.0` until volume forces your hand.** Sampling trades completeness
  for cost; only turn it down when a high-frequency error is drowning the quota.
- **Don't sample analytics through this knob** — it doesn't touch events. If you need
  to thin out a chatty event or transaction, gate the `track` / `trackTransaction`
  call yourself (or drop it in `beforeSend`), so your own sampling is explicit and
  under your control.
- **Keep the rate stable per release** so error-count trends stay comparable; a moving
  sample rate makes "is this getting worse?" unanswerable.

---

## 4. Tags vs. context (vs. extra vs. fingerprint)

The scope API (`setTag`/`setTags`, `setContext`, `setExtra`) attaches attribution to
whatever you capture next. Choosing the right bucket keeps the dashboard fast and the
detail rich.

- **Tags — `setTag(key, value)` / `setTags({…})`, or `captureException(err, { tags })`.**
  Flat **string → string** pairs, **low-cardinality**, meant to be **filtered and
  grouped on**: `area=checkout`, `region=eu`, `feature_flag=new_cart`. They land in the
  error item's `tags`. Treat them like database indexes — a handful of stable
  dimensions, not a dumping ground. **Never** put ids, emails, or timestamps in tag
  values (that explodes cardinality and leaks PII).
- **Context — `setContext(key, object)`.** Named **free-form structured blocks** for
  the rich diagnostic detail you read *after* you've found an issue:
  `setContext('request', { route, method, query_id })`. Not indexed for filtering —
  optimized for depth, not slicing.
- **Extra — `setExtra(key, value)`.** Loose one-off values that don't warrant a named
  context block.
- **Fingerprint — `captureException(err, { fingerprint: [...] })` (Node/Python/C#).**
  Not attribution at all: a **grouping override**. Supply it only when the default
  grouping splits or merges issues wrongly; the backend honors it verbatim.

```ts
setTag('area', 'checkout');                       // filterable dimension
setContext('cart', { item_count: 3, total: 42.5 }); // rich detail, not filtered
setExtra('experiment_bucket', 'B');               // loose value
captureException(err, { tags: { severity: 'high' }, fingerprint: ['cart', 'timeout'] });
```

Rule of thumb: **if you'll want to filter or break-down by it on a dashboard, it's a
tag; if you'll only read it while debugging one specific event, it's context/extra.**

Scope setters (`setTag`/`setContext`/`setExtra`/`setUser`, plus `addBreadcrumb`) are
available on the **server SDKs** (Node/Python/C#). Use a per-request scope so
attribution can't leak between concurrent requests — see
**[Getting Started](Getting-Started.md)** and each SDK page for `withScope` (Node),
`with sauron.scope():` (Python), and `using SauronSdk.PushScope()` (C#).

---

## 5. `distinct_id` strategy (anonymous → identify alias)

Every signal is attributed to a person via a stable **`distinct_id`**. `identify` can
additionally carry an **`anonymous_id`** to *alias* a pre-login anonymous person onto a
known one, so the backend stitches the earlier activity onto the same timeline.

**Core rules (all SDKs):**

- **One stable id per person, used everywhere.** Don't switch ids mid-session and don't
  mix schemes (email in one place, uuid in another).
- **Call `identify` exactly once, right after login.** `identify(distinct_id, traits)`
  attaches/updates traits; calling it on every request is wasteful.
- **`track` needs a non-empty `distinct_id`.** On the server SDKs an empty id is
  dropped silently.

**Client SDKs manage the anonymous id for you.**

- **Browser** mints a stable anonymous id (`anon_<uuid>`) and uses it as the
  `distinct_id` for pre-login events. When you call `identify(userId, traits)` it emits
  an identify item whose `anonymous_id` is that prior anonymous id — the backend then
  attributes the pre-login events to the now-known person. You just call `identify`
  once after login; the alias is automatic. On the wire:

  ```json
  { "type": "identify", "distinct_id": "user-123", "anonymous_id": "anon_9f3c…", "traits": { "plan": "pro" } }
  ```

- **Flutter** attributes events to the current user's id (`null` until you identify or
  `setUser`). `identify(distinctId, traits)` sets the user and emits an identify item;
  set the user as early as you hold an id. (The shipped Flutter path does not auto-fill
  `anonymous_id`.)

**Server SDKs — you own the id.** `track(event, distinctId, …)` takes it explicitly;
Node's `trackTransaction` (and the transaction path generally) falls back to the scoped
user's id when you omit it. `identify(distinctId, traits)` sends `anonymous_id: null`
(there is no alias parameter on the server). Two consequences:

- Use the **authenticated user id you already hold** for server-side signals.
- The **anonymous → known stitching is owned by the client** (browser/mobile) that held
  the anonymous session. If you also emit pre-login events server-side, propagate the
  same anonymous id your client minted (e.g. via a cookie/header) and use it as the
  `distinct_id` so both timelines line up under the alias the client sends.

```python
# server: identify at login using the id you already trust
sauron.identify(user.id, traits={"plan": user.plan, "company_size": user.company_size})
sauron.track("order_completed", user.id, properties={"total": 42.5})
```

---

## 6. Flush / shutdown discipline for short-lived processes

Every SDK **buffers** items and flushes on a timer (default ~5s) or once a batch fills
(30 items). A **short-lived process — a CLI, a cron job, a one-off script, a
serverless/Lambda handler — can exit before that timer fires, silently dropping the
buffer.** Flush explicitly.

- **Node.** `await flush()` sends the buffer now; `await close()` on shutdown flushes,
  stops the timer, and clears the client. The flush timer is `unref`'d, so it will
  **never keep your process alive for you** — you must flush/close yourself. Opt in to
  `init({ autoShutdown: true })` to wire `beforeExit`/`SIGTERM`/`SIGINT` → `close()`.
  In a request/Lambda handler, `await flush()` **before returning**.

  ```ts
  try {
    doWork();
    track('job_finished', 'system');
  } finally {
    await close(); // drain before exit
  }
  ```

- **Python.** `init` auto-registers an `atexit` flush (best-effort), so normal exits
  drain themselves. For explicit control use `sauron.flush(timeout)` /
  `sauron.close(timeout)`. `atexit` does **not** run on `SIGKILL` or `os._exit`, and may
  not run cleanly under container `SIGTERM` — call `close()` from your shutdown handler
  in those paths.

- **C#.** `SauronSdk.Flush()` (blocking) / `FlushAsync()` / `Close()` (flush + stop).
  Call `Close()` from your host's shutdown hook
  (`IHostApplicationLifetime.ApplicationStopping`) or at the end of a console app.

- **Browser / Flutter.** The client flushes on page-hide / app-lifecycle transitions
  automatically; `flush()` / `close()` exist but are rarely needed in day-to-day use.

**Opt-in auto-capture doesn't excuse flushing.** Node/Python/C#'s
`autoCaptureUnhandled` records an uncaught crash with `mechanism.handled = false` but
**does not swallow it** — the process still exits with its normal semantics. Put your
`flush`/`close` in a `finally` so the captured crash actually leaves the buffer.

**Cron jobs that may run while ingest is down:** turn on the **opt-in disk queue**
(`offlineDir` in Node, `offline_path` in Python, `OfflineDir` in C#). Pending envelopes
persist FIFO to disk and replay on the next run (at-least-once), so a run during an
ingest outage isn't lost. It stays **off by default** (memory-only).

---

*Next: copy-paste framework recipes, capability matrix, and troubleshooting live on
their own wiki pages. Back to **[Home](Home.md)**.*
