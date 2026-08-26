# Active-users report: serve-stale + background recompute (~1h freshness)

Date: 2026-08-26. Request: `/v1/projects/{id}/active-users` takes ~25 s on the
reporting deployment; cache it "the same as the overview — okay to show ~1h
stale data".

## Current state

The route already has a Redis cache — but 60 s TTL and READ-THROUGH: a miss
runs the 25 s aggregate on the request path while holding one of the three
semaphore permits, and the permit is taken even for cache hits. Effectively
every visit past a minute pays full price.

## Design (the overview's essence, not its machinery)

The overview's full apparatus (per-section envelopes, SSE, nullable `data`)
exists because its cold answer crossed the request timeout entirely. This
report's cold answer fits inside the 60 s budget, and its CSV twin cannot
carry a "computing" envelope — so we take the overview's serving DISCIPLINE
and skip its wire-format change:

- `ACTIVE_USERS_FRESH_FOR_SECS = 3600`: a cached report younger than this is
  served as-is, nothing recomputed.
- `ACTIVE_USERS_CACHE_TTL_SECS = 3 h` (was 60 s): how long a report stays
  servable at all. Between 1 h and 3 h it is served STALE and a background
  recompute is kicked — the next visitor gets fresh numbers. Past 3 h it is a
  cold miss.
- Cold miss: computed on-path exactly as today (25 s, once per key per cold
  window), still behind the semaphore. This is the one path that stays slow,
  and it is the rare one for any regularly-visited report.
- Background refresh: `tokio::spawn` from the request, guarded by
  `set_nx_ex` single-flight on the cache key's refresh lock (120 s — outlives
  a 60 s-budget compute; expiry un-wedges a crashed refresher) AND by
  `try_acquire` on the SAME active-users semaphore (a refresh is the same
  heavy query; unbounded refreshes would be the DoS the semaphore exists to
  stop). Both unavailable → skip; a later request retries. The refresh reuses
  the request's RESOLVED scopes and effective window — exactly what the cache
  key hashes — so no re-authorization happens off-path for a key nobody could
  have read unauthorized.
- `ActiveUsersReport.computed_at: Option<DateTime<Utc>>`, `#[serde(default)]`
  (the struct's own doc mandates that for post-v1 fields, so entries cached
  by the previous build keep deserializing — they read as `None`, which is
  treated as stale: served, refreshed in background).
- Semaphore placement fix: permits are now taken only around actual computes
  (on-path miss, background refresh) — a cache hit no longer consumes one and
  can no longer be shed 503-busy while three computes run.
- Dashboard: the report page shows the overview-style "Updated HH:MM (Xm
  ago)" stamp from `computed_at`. No other contract change; CSV unchanged.

## Testing

- HTTP: cold GET carries `computed_at`; an immediate second GET returns the
  SAME `computed_at` (served from cache, no recompute); a cache entry
  rewritten to 2 h old is served instantly with the old stamp and the
  background refresh lands a newer stamp within a poll deadline (found via
  Redis SCAN on the key prefix — the real end-to-end of the SWR path).
- Live drive against the 158M dev dataset: cold vs warm vs stale timings.

## Also in this change (separate ask)

`/admin/storage` renders a skeleton while its data loads, instead of its
current loading state.
