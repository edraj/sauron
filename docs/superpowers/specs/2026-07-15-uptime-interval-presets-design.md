# Uptime Monitor Intervals: Honored & Updateable — Design

**Date:** 2026-07-15
**Status:** Approved (pending implementation plan)

## Problem

The uptime monitoring feature stores a per-monitor `interval_seconds`, but three
gaps prevent intervals from being honored and updateable as intended:

1. **Short intervals are silently clamped.** Create and update both apply
   `interval.max(monitor_min_interval_secs)` with a floor of 30s, so intended
   values of 1s / 5s / 15s get bumped to 30s.
2. **Updating the interval is not honored immediately.** `update_monitor`
   changes `interval_seconds` but never recomputes `next_check_at`. Shortening a
   24h monitor to 1min leaves the already-scheduled check up to 24h in the
   future; the new interval does not take effect until then.
3. **No UI to choose or change the interval from the presets.** The create form
   is a raw number input (`min="30"`); the detail page shows the interval
   read-only.

## Goal

The following 14 intervals are the **only** allowed values, they are honored by
the prober immediately (including when changed), and users can pick and change
them from the dashboard:

`1s, 5s, 15s, 30s, 1min, 3min, 5min, 15min, 30min, 1h, 3h, 6h, 12h, 24h`

In seconds: `1, 5, 15, 30, 60, 180, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400`.

## Decisions (confirmed with user)

- **Enforce the 14 presets server-side** (reject anything else), rather than
  accepting free-form values with presets as a UI convenience.
- **Enable all intervals down to 1s** exactly as listed (lower today's 30s
  floor).
- **Keep the prober tick default at 1000ms** (`MONITOR_TICK_MS`). 1s intervals
  are honored best-effort with up to ~1s jitter; operators can lower the tick if
  they want tighter fidelity. No infra/default change.

## Design

### 1. Single source of truth for the preset set

- **Backend (`sauron-core`):** add a constant and validator, e.g. in a small
  `monitor.rs` module (or alongside config):
  ```rust
  pub const MONITOR_INTERVAL_PRESETS: [i32; 14] =
      [1, 5, 15, 30, 60, 180, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400];

  pub fn is_valid_monitor_interval(secs: i32) -> bool {
      MONITOR_INTERVAL_PRESETS.contains(&secs)
  }
  ```
- **Frontend:** `dashboard/src/lib/constants/monitorIntervals.ts` mirroring the
  same 14 values with human labels:
  ```ts
  export const MONITOR_INTERVALS: { seconds: number; label: string }[] = [
    { seconds: 1, label: '1 second' },
    { seconds: 5, label: '5 seconds' },
    { seconds: 15, label: '15 seconds' },
    { seconds: 30, label: '30 seconds' },
    { seconds: 60, label: '1 minute' },
    { seconds: 180, label: '3 minutes' },
    { seconds: 300, label: '5 minutes' },
    { seconds: 900, label: '15 minutes' },
    { seconds: 1800, label: '30 minutes' },
    { seconds: 3600, label: '1 hour' },
    { seconds: 10800, label: '3 hours' },
    { seconds: 21600, label: '6 hours' },
    { seconds: 43200, label: '12 hours' },
    { seconds: 86400, label: '24 hours' },
  ];
  ```
  A helper `formatInterval(seconds)` returns the matching label (falling back to
  `${seconds}s` for legacy/unknown values) for read-only display.

### 2. Enforce the presets in the API (replaces the silent clamp)

- **Create** (`backend/bins/sauron-api/src/routes/monitors.rs`, `create`):
  remove `interval.max(min)`. When `interval_seconds` is absent, default to 60.
  When present but not a valid preset, return `400 BadRequest` naming the
  allowed values.
- **Update** (`update`): apply the same whitelist validation — reject invalid
  values instead of clamping.
- Remove the now-obsolete `monitor_min_interval_secs` config field and its
  references (`Config`, `.env.example`, `docker-compose.yml`, any docs). It is
  superseded by the whitelist and is misleading (it implied a clamp that no
  longer happens).

### 3. Honor an interval change immediately (core "updateable" fix)

- `update_monitor` (`backend/crates/sauron-db/src/repo.rs`): when
  `interval_seconds` is provided, also recompute
  `next_check_at = now() + make_interval(secs => new_interval)`, so the change
  takes effect right away. This restarts the cadence from "now" — both
  shortening and lengthening apply immediately. The existing re-enable →
  `now()` behavior is preserved (re-enabling still forces an immediate check).

  SQL sketch for the `next_check_at` column in the UPDATE:
  ```sql
  next_check_at = CASE
      WHEN $status = 'unknown' THEN now()
      WHEN $interval_seconds IS NOT NULL THEN now() + make_interval(secs => $interval_seconds)
      ELSE next_check_at
  END
  ```

### 4. Prevent probe pile-up on short intervals

- In `spec_of` (`backend/bins/sauron-monitor/src/main.rs`), cap the effective
  probe timeout at the interval:
  ```rust
  let timeout_ms = m.timeout_ms.min(m.interval_seconds.saturating_mul(1000)).max(1);
  timeout: Duration::from_millis(timeout_ms as u64),
  ```
  Rationale: without this, a 1s monitor with the default 10s timeout could have
  ~10 overlapping in-flight probes (the claim advances `next_check_at` by the
  interval, so the row becomes claimable again before a long probe finishes).
  Deliberate consequence: a 1s monitor times out at ≤1s — a response slower than
  the monitor's own cadence is effectively down/slow anyway.

### 5. Frontend

- **Create form** (`dashboard/src/pages/Monitors.svelte`): replace the
  `<input id="mon-interval" type="number" min="30">` with a `<select>` of the 14
  presets, styled to match the existing `kind`/`method` selects in the same
  file. Default selection: "1 minute" (60). `interval` state stays a number.
- **Detail page** (`dashboard/src/pages/MonitorDetail.svelte`): make the
  "Interval" tile editable for users with `monitor:write`:
  - Render a `<select>` bound to `detail.monitor.interval_seconds`.
  - On change, call `updateMonitor(id, { interval_seconds })`, show a saving
    state, then reload.
  - For users without `monitor:write`, keep the read-only display (use
    `formatInterval`).

### 6. Tests / verification

- **Backend unit tests:** `is_valid_monitor_interval` accepts all 14 presets and
  rejects off-list values (e.g., 45, 0, 100000).
- **Backend behavior:** an update that changes the interval pulls `next_check_at`
  forward (integration or repo-level check); an invalid interval on
  create/update returns 400.
- **E2E (per the project verify pattern):**
  - Create a monitor at 1s → confirm it is probed at ~1s cadence and not clamped.
  - Update a monitor from 24h → 1min → `next_check_at` moves to within ~1min and
    the next check fires promptly.
  - POST/PATCH an off-list interval → 400.
- **Frontend:** both dropdowns render the 14 options; changing the interval on
  the detail page persists and re-renders.

## Out of scope

- Exposing `timeout_ms` / failure & recovery thresholds in the UI.
- Per-preset timeout defaults (beyond the `min(timeout, interval)` cap).
- Changing the prober tick default or the scheduler's claim/skip-locked design.
- A DB `CHECK` constraint on `interval_seconds` (app-level validation is the
  single source of truth; keeping the set out of the schema avoids a migration
  each time the set evolves and avoids breaking direct-insert paths like the
  crebain benchmark).
