# Admin — data purge & ingest failures

Two operator surfaces that were previously undocumented, and the two with the
largest consequences: one deletes data permanently, the other is where events
go when they could not be accepted.

Both live under **Admin** and require **deployment admin** — a stricter gate
than the org/project/app roles the rest of the product uses. `require_deployment_admin`
is checked on every handler, not just on the page.

## Data purge

Permanently deletes event data for a chosen scope. This is the GDPR-erasure and
tenant-offboarding path, and there is no undo.

### The flow is preview → confirm, and it cannot widen

1. **Preview** — `POST /v1/admin/purge` describes what *would* be deleted and
   creates an unconfirmed job. Nothing is removed at this point.
2. **Confirm** — `POST /v1/admin/purge/{id}/confirm` performs the deletion. It
   requires echoing back a token from the preview response.
3. **Cancel** — `POST /v1/admin/purge/{id}/cancel` discards the job. A preview
   left alone also expires on its own.

The important property: **`confirm` takes no scope fields at all** — only the
typed slug. It cannot broaden what the preview described, so the thing you
reviewed is exactly the thing that gets deleted. Typing the slug is deliberate
friction: it is the one confirmation that forces attention onto *which* scope is
about to be erased, rather than onto a yes/no button.

### Before you confirm

- **Read the preview counts.** They are the only chance to notice the scope is
  wrong.
- **A purge is not a retention policy.** For routine ageing-out use the tiering
  and retention settings; purge is for erasure requests and offboarding.
- **Aggregates are not raw rows.** Rollups and person-day counts derived from
  purged events are recomputed or dropped alongside them — but a number you
  exported earlier will not retroactively change.

## Ingest failures

The dead-letter queue: envelopes the gateway accepted from the network but could
not process. This is where to look when an SDK reports success and the data
never appears.

| Endpoint | Purpose |
|---|---|
| `GET /v1/admin/ingest-failures` | List failures, most recent first |
| `GET /v1/admin/ingest-failures/{id}` | One failure with its reason |
| `GET /v1/admin/ingest-failures/{id}/payloads` | The rejected payload itself |
| `POST /v1/admin/ingest-failures/{id}/retry` | Re-submit after fixing the cause |

### Reading one

The payload view is the point: it shows the envelope as received, so you can see
which field the validator rejected. Common causes are wire-contract violations
from an outdated SDK — a non-optional field sent as `null` rejects the **entire
envelope**, so a single bad item takes every item batched with it. See
[Ingest Wire Contract](Ingest-Wire-Contract.md).

**Retry only after the cause is fixed.** A retry re-runs the same validation; if
the payload was malformed it will simply fail again. When the cause was an
outdated SDK, upgrading fixes new traffic but the parked payloads stay invalid.

### Privacy note

Payloads are raw captured data and may contain personal information — that is
precisely why this page sits behind deployment admin rather than an app-scoped
role. See [Privacy Inspector](Privacy-Inspector.md) for masking before capture.
