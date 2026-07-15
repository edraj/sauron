# Framework Integrations

Copy-paste recipes that wire Sauron into the popular web frameworks. Every recipe
does the same three things per request:

1. **Set a per-request scope** — user, tags and breadcrumbs isolated to that
   request (async-local on the server, single-client on the browser), so a
   captured error carries the right context and concurrent requests never leak
   into each other.
2. **Capture errors** — unhandled exceptions are reported with the request scope
   attached.
3. **Time the request** — one `transaction` item per request via
   `trackTransaction` (op `http`), so it shows up under **Performance**.

All dispatch calls are **no-ops before `init`** (and when the DSN is missing or
disabled), so it is safe to drop these hooks in before you have wired a DSN.

Wire fields are `snake_case` (`distinct_id`, `duration_ms`, `http_method`,
`http_status`); each SDK exposes them in its own idiom (Node/Python use
`duration_ms`, C#/Browser use `durationMs`) and serializes to the same wire shape
— see the **[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

**Jump to:** [Node](#node) · [Python](#python) · [C# / ASP.NET Core](#c--aspnet-core) · [Browser](#browser-react--vue--svelte)

---

## Node

Recipes use `@sauron/node`. See the **[Node SDK](Node-SDK.md)** page for the full
surface. Per-request isolation is backed by `AsyncLocalStorage`: `withScope(cb)`
runs `cb` (and everything it calls, across `await`s) under an isolated child
scope. Initialize once at startup:

```ts
import { init, close } from '@sauron/node';

init({
  dsn: process.env.SAURON_DSN!,
  environment: process.env.NODE_ENV ?? 'production',
  release: process.env.RELEASE,
});

// Drain the buffer on shutdown (or pass `autoShutdown: true` to init).
process.on('SIGTERM', () => void close());
```

### Express

```ts
import express from 'express';
import {
  withScope,
  addBreadcrumb,
  captureException,
  trackTransaction,
} from '@sauron/node';

const app = express();

// 1) Per-request scope: run the rest of the request inside `withScope` so the
//    async-local child scope spans every downstream handler.
app.use((req, res, next) => {
  const start = process.hrtime.bigint();
  withScope((scope) => {
    scope.setTag('http.method', req.method);
    if (req.user) scope.setUser({ id: req.user.id });
    addBreadcrumb({ type: 'http', category: 'request', message: `${req.method} ${req.path}` });

    // 3) Time the request when the response is done.
    res.on('finish', () => {
      trackTransaction({
        name: `${req.method} ${req.route?.path ?? req.path}`,
        op: 'http',
        duration_ms: Number(process.hrtime.bigint() - start) / 1e6,
        http_method: req.method,
        http_status: res.statusCode,
        url: req.originalUrl,
        distinct_id: req.user?.id,
      });
    });

    next();
  });
});

// ... your routes ...

// 2) Error-handling middleware (register LAST). Still inside the request's
//    async-local scope, so the captured error carries its user/tags.
app.use((err, req, res, next) => {
  captureException(err, { tags: { 'http.route': req.path } });
  next(err);
});
```

### Fastify

Fastify's request lifecycle is a chain of separate hooks, so establish the
async-local scope from an `onRequest` hook by calling `done()` **inside**
`withScope` — the rest of the request then runs under that scope. Any
`captureException` you call from a route handler inherits it.

```ts
import Fastify from 'fastify';
import { withScope, addBreadcrumb, captureException, trackTransaction } from '@sauron/node';

const fastify = Fastify();

fastify.addHook('onRequest', (req, reply, done) => {
  // The `done()`-inside-withScope trick binds the async-local scope for the
  // whole request.
  withScope((scope) => {
    scope.setTag('http.method', req.method);
    addBreadcrumb({ type: 'http', category: 'request', message: `${req.method} ${req.url}` });
    done();
  });
});

fastify.addHook('onResponse', (req, reply, done) => {
  trackTransaction({
    name: `${req.method} ${req.routeOptions?.url ?? req.url}`,
    op: 'http',
    duration_ms: reply.elapsedTime, // ms since the request started
    http_method: req.method,
    http_status: reply.statusCode,
    url: req.url,
  });
  done();
});

fastify.addHook('onError', (req, reply, err, done) => {
  captureException(err, { tags: { 'http.method': req.method } });
  done();
});
```

### Koa

`withScope` returns whatever its callback returns, so an `async` callback lets
the scope span the awaited `next()`:

```ts
import Koa from 'koa';
import { withScope, addBreadcrumb, captureException, trackTransaction } from '@sauron/node';

const app = new Koa();

app.use(async (ctx, next) => {
  const start = Date.now();
  await withScope(async (scope) => {
    scope.setTag('http.method', ctx.method);
    addBreadcrumb({ type: 'http', category: 'request', message: `${ctx.method} ${ctx.path}` });
    try {
      await next();
    } catch (err) {
      captureException(err, { tags: { 'http.route': ctx.path } });
      throw err; // re-raise so Koa's own error handling still runs
    } finally {
      trackTransaction({
        name: `${ctx.method} ${ctx.path}`,
        op: 'http',
        duration_ms: Date.now() - start,
        http_method: ctx.method,
        http_status: ctx.status,
        url: ctx.originalUrl,
      });
    }
  });
});
```

---

## Python

Recipes use the `sauron` package. See the **[Python SDK](Python-SDK.md)** page.
Per-request isolation is backed by `contextvars`: `with sauron.scope():` pushes an
isolated child scope for the block, and `push_scope()` / `pop_scope()` do the same
across separate hook functions. Initialize once at startup:

```python
import sauron

sauron.init(
    dsn=os.environ["SAURON_DSN"],
    environment=os.environ.get("ENV", "production"),
)
# `sauron.init` registers an atexit flush; call `sauron.close()` explicitly for a
# hard shutdown.
```

### Flask

Flask runs `before_request` → view → `after_request` → `teardown_request` in one
thread/context, so `push_scope()` in `before_request` stays active through the
error handler and is popped in `teardown_request`.

```python
import time
import sauron
from flask import Flask, g, request

app = Flask(__name__)

@app.before_request
def _sauron_open_scope():
    sauron.push_scope()                       # 1) per-request scope
    g._sauron_start = time.perf_counter()
    sauron.set_tag("http.method", request.method)
    if getattr(g, "user_id", None):
        sauron.set_user({"id": g.user_id})
    sauron.add_breadcrumb(category="http", message=f"{request.method} {request.path}")

@app.errorhandler(Exception)
def _sauron_capture(exc):
    sauron.capture_exception(exc)             # 2) capture (scope still active)
    raise exc                                 # re-raise for Flask's default handling

@app.after_request
def _sauron_time(response):
    start = getattr(g, "_sauron_start", None)
    if start is not None:
        sauron.track_transaction(             # 3) time the request
            f"{request.method} {request.url_rule or request.path}",
            op="http",
            duration_ms=(time.perf_counter() - start) * 1000,
            http_method=request.method,
            http_status=response.status_code,
            url=request.path,
        )
    return response

@app.teardown_request
def _sauron_close_scope(_exc):
    sauron.pop_scope()
```

### FastAPI

Use an HTTP middleware. The `with sauron.scope():` block (and the
`capture_exception` inside its `except`) run in the same coroutine, so the scope
applies to the captured error and the timed transaction.

```python
import time
import sauron
from fastapi import FastAPI, Request

app = FastAPI()

@app.middleware("http")
async def sauron_middleware(request: Request, call_next):
    start = time.perf_counter()
    with sauron.scope():                                  # 1) per-request scope
        sauron.set_tag("http.method", request.method)
        sauron.add_breadcrumb(category="http", message=f"{request.method} {request.url.path}")
        status = 500
        try:
            response = await call_next(request)
            status = response.status_code
            return response
        except Exception as exc:
            sauron.capture_exception(exc)                 # 2) capture
            raise
        finally:
            sauron.track_transaction(                     # 3) time the request
                f"{request.method} {request.url.path}",
                op="http",
                duration_ms=(time.perf_counter() - start) * 1000,
                http_method=request.method,
                http_status=status,
                url=request.url.path,
            )
```

### Django

A class-based middleware wraps `get_response`. Django invokes `process_exception`
synchronously inside that call, so it runs under the same active scope.

```python
# myapp/sauron_middleware.py
import time
import sauron

class SauronMiddleware:
    def __init__(self, get_response):
        self.get_response = get_response

    def __call__(self, request):
        start = time.perf_counter()
        with sauron.scope():                              # 1) per-request scope
            sauron.set_tag("http.method", request.method)
            uid = getattr(getattr(request, "user", None), "pk", None)
            if uid is not None:
                sauron.set_user({"id": str(uid)})
            sauron.add_breadcrumb(category="http", message=f"{request.method} {request.path}")
            response = self.get_response(request)
            sauron.track_transaction(                     # 3) time the request
                f"{request.method} "
                f"{request.resolver_match.route if request.resolver_match else request.path}",
                op="http",
                duration_ms=(time.perf_counter() - start) * 1000,
                http_method=request.method,
                http_status=response.status_code,
                url=request.path,
            )
            return response

    def process_exception(self, request, exception):
        sauron.capture_exception(exception)               # 2) capture
        return None                                       # let Django render its 500
```

```python
# settings.py
MIDDLEWARE = [
    "myapp.sauron_middleware.SauronMiddleware",
    # ... the rest ...
]
```

---

## C# / ASP.NET Core

Uses the `Sauron` package (`SauronSdk` facade). See the **[C# SDK](CSharp-SDK.md)**
page. Per-request isolation is backed by `AsyncLocal`: `using
(SauronSdk.PushScope())` establishes a child scope that flows across the awaited
`_next(context)`. Initialize once at startup:

```csharp
using Sauron;

var builder = WebApplication.CreateBuilder(args);

SauronSdk.Init(new SauronOptions
{
    Dsn = builder.Configuration["Sauron:Dsn"]!,
    Environment = builder.Environment.EnvironmentName,
});

var app = builder.Build();
app.UseMiddleware<SauronMiddleware>();

// Drain the buffer on shutdown.
app.Lifetime.ApplicationStopping.Register(SauronSdk.Close);

app.Run();
```

### Middleware

```csharp
using System.Diagnostics;
using Sauron;

public sealed class SauronMiddleware
{
    private readonly RequestDelegate _next;

    public SauronMiddleware(RequestDelegate next) => _next = next;

    public async Task InvokeAsync(HttpContext context)
    {
        var sw = Stopwatch.StartNew();
        using (SauronSdk.PushScope())                        // 1) per-request scope
        {
            SauronSdk.SetTag("http.method", context.Request.Method);

            var uid = context.User?.FindFirst("sub")?.Value;
            if (uid is not null)
                SauronSdk.SetUser(new SauronUser { Id = uid });

            SauronSdk.AddBreadcrumb(new Breadcrumb
            {
                Type = "http",
                Category = "request",
                Message = $"{context.Request.Method} {context.Request.Path}",
            });

            try
            {
                await _next(context);
            }
            catch (Exception ex)
            {
                SauronSdk.CaptureException(ex, tags: new Dictionary<string, object?>
                {
                    ["http.method"] = context.Request.Method,
                });                                          // 2) capture
                throw;
            }
            finally
            {
                sw.Stop();
                SauronSdk.TrackTransaction(                   // 3) time the request
                    name: $"{context.Request.Method} {context.Request.Path}",
                    durationMs: sw.Elapsed.TotalMilliseconds,
                    op: "http",
                    httpMethod: context.Request.Method,
                    httpStatus: context.Response.StatusCode,
                    url: context.Request.Path);
            }
        }
    }
}
```

---

## Browser (React / Vue / Svelte)

Uses `@sauron/browser` (the `Sauron` facade). See the **[Browser SDK](Browser-SDK.md)**
page. The browser SDK is a single-user client (one visitor per page), so
"per-request scope" becomes *set the user/tags/breadcrumbs on the one client* —
usually on login and on route change. Tags live on the client scope
(`Sauron.getClient()?.getScope().setTag(...)`); `setUser`, `addBreadcrumb`,
`captureException` and `trackTransaction` are top-level. Uncaught `window` errors
and unhandled rejections are auto-captured, so the recipes below add the
framework-specific error hook plus route scope + timing. Initialize once:

```ts
import { Sauron } from '@sauron/browser';

Sauron.init({ dsn: '<public_dsn>', release: 'web@1.4.2' });
// After sign-in:
Sauron.setUser({ id: 'user-123', email: 'a@example.com' });
```

### React error boundary

```tsx
import React from 'react';
import { Sauron } from '@sauron/browser';

export class SauronErrorBoundary extends React.Component<
  { fallback?: React.ReactNode; children: React.ReactNode },
  { hasError: boolean }
> {
  state = { hasError: false };

  static getDerivedStateFromError() {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    Sauron.addBreadcrumb({ type: 'error', category: 'react', message: 'render error' });
    // The 2nd arg is a hint; `mechanism` is honored on the error item.
    Sauron.captureException(error, {
      mechanism: { type: 'react', handled: false },
      componentStack: info.componentStack,
    });
  }

  render() {
    return this.state.hasError ? (this.props.fallback ?? null) : this.props.children;
  }
}
```

Per-route scope + timing (e.g. with React Router's `useLocation`):

```tsx
import { useEffect } from 'react';
import { useLocation } from 'react-router-dom';
import { Sauron } from '@sauron/browser';

export function useSauronRoute() {
  const location = useLocation();
  useEffect(() => {
    const start = performance.now();
    Sauron.getClient()?.getScope().setTag('route', location.pathname);
    Sauron.addBreadcrumb({ type: 'navigation', category: 'router', message: location.pathname });
    return () => {
      Sauron.trackTransaction({
        name: location.pathname,
        op: 'navigation',
        durationMs: performance.now() - start, // camelCase input on the browser SDK
      });
    };
  }, [location.pathname]);
}
```

### Vue `errorHandler`

```ts
import { createApp } from 'vue';
import { createRouter } from 'vue-router';
import { Sauron } from '@sauron/browser';
import App from './App.vue';

const app = createApp(App);

// 2) Capture render/lifecycle errors.
app.config.errorHandler = (err, _instance, info) => {
  Sauron.addBreadcrumb({ type: 'error', category: 'vue', message: info });
  Sauron.captureException(err, { mechanism: { type: 'vue', handled: false }, lifecycleHook: info });
};

// 1) + 3) Per-route scope + timing.
const router = createRouter({ /* ... */ });
router.beforeEach((to) => {
  (to.meta as Record<string, unknown>)._start = performance.now();
});
router.afterEach((to) => {
  Sauron.getClient()?.getScope().setTag('route', to.path);
  Sauron.trackTransaction({
    name: to.path,
    op: 'navigation',
    durationMs: performance.now() - Number((to.meta as Record<string, unknown>)._start ?? performance.now()),
  });
});

app.use(router).mount('#app');
```

### Svelte / SvelteKit

SvelteKit exposes a client-side error hook and navigation lifecycle helpers.
Put `init` + the error hook in `src/hooks.client.ts`:

```ts
// src/hooks.client.ts
import type { HandleClientError } from '@sveltejs/kit';
import { Sauron } from '@sauron/browser';

Sauron.init({ dsn: '<public_dsn>', release: 'web@1.4.2' });

export const handleError: HandleClientError = ({ error, event }) => {
  Sauron.captureException(error, {
    mechanism: { type: 'sveltekit', handled: false },
    route: event.route?.id,
  });
  return { message: 'Something went wrong.' };
};
```

Per-route scope + timing (e.g. in your root `+layout.svelte`):

```svelte
<script lang="ts">
  import { beforeNavigate, afterNavigate } from '$app/navigation';
  import { Sauron } from '@sauron/browser';

  let start = 0;
  beforeNavigate(() => { start = performance.now(); });
  afterNavigate((nav) => {
    const route = nav.to?.route.id ?? location.pathname;
    Sauron.getClient()?.getScope().setTag('route', route);
    Sauron.trackTransaction({ name: route, op: 'navigation', durationMs: performance.now() - start });
  });
</script>
```

> For a plain (non-Kit) Svelte app, `Sauron.init(...)` alone covers uncaught
> errors via the SDK's global `window` handlers; call `Sauron.captureException`
> from your own `try/catch` blocks and `Sauron.trackTransaction` where you time
> work.

### Source maps

Browser stack frames are sent **raw / minified** — the SDK never symbolicates on
the client. Upload your production source maps so the dashboard renders the
original file/line/column. Use `sauron-symcli` (ships in the backend tools):

```bash
sauron-symcli upload-sourcemap \
  --api https://<host> --token <dashboard-jwt> --app <app-uuid> \
  --release web@1.4.2 --name app.min.js \
  dist/app.min.js.map
```

The `--release` must match the `release` you passed to `Sauron.init` (and
`--name` the minified filename referenced by the stack frames). Symbolication
runs server-side on read, so re-uploading a map re-symbolicates existing errors.

---

*See also:* **[Browser SDK](Browser-SDK.md)** · **[Node SDK](Node-SDK.md)** ·
**[Python SDK](Python-SDK.md)** · **[C# SDK](CSharp-SDK.md)** ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.
