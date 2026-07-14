# Feature Audit — Sauron vs. Sentry + UXCam/Smartlook checklist

**Date:** 2026-07-13
**Scope of audit:** verify whether the 13 features in the requested "Core Monitoring / Qualitative / Quantitative" checklist actually exist in this codebase.
**Method:** each feature was deep-read across backend (Rust), dashboard (Svelte), SDKs (JS + Flutter), migrations, and the wire contract by an independent agent, then **adversarially re-checked** by a second agent trying to disprove the first verdict. All 13 verdicts held on re-check. Findings were cross-checked with word-boundary `grep` sweeps. Evidence is cited as `file:line`.

> **Framing.** The checklist is the combined marketing surface of **Sentry** (error/trace/profiling/metrics) **+ UXCam/Smartlook** (mobile replay/heatmaps/touch/AI). This repo's README scopes this build as an MVP wedge — *"error → replay in one click"* analytics — and **explicitly declares session replay/video, ClickHouse/Kafka/object storage, SSO, and billing out of scope for this cut.** So most of the checklist was never in scope for this cut; the audit's job is to say precisely what *is* built, what's *partial*, and how each gap maps to the declared scope and the `plan.md` roadmap.

## Verdict summary

| # | Feature | Verdict | Coverage | Scope status |
|---|---------|:-------:|:--------:|--------------|
| **Core Monitoring & Debugging** |
| 1 | Error Tracking (group/aggregate exceptions & crashes) | ✅ **Present** | ~95% | In scope — the wedge |
| 2 | Session Replay (video-like recordings) | ❌ Absent | 0% | **Declared out-of-scope** (README) |
| 3 | Distributed Tracing (cross-service paths, frozen frames, app-start) | ❌ Absent | ~10% | Not built; not on roadmap as a product feature |
| 4 | Continuous Profiling (production CPU profiles) | ❌ Absent | 0% | Not built; not on roadmap |
| 5 | Application Metrics (custom counters/gauges/distributions) | ❌ Absent | 0% | Not built; not on roadmap |
| **Qualitative Analytics** |
| 6 | Heatmaps (taps/scrolls/abandoned interactions) | ❌ Absent | 0% | Not built; not on roadmap |
| 7 | Touch Analysis (double-taps, rage taps) | ❌ Absent | 0% | On `plan.md` roadmap, not built |
| 8 | Tara AI Analyst (AI-driven insights) | ❌ Absent | 0% | Not built; not on roadmap |
| **Quantitative Analytics & Diagnostics** |
| 9 | User Journey Analytics (paths, drop-offs) | ✅ **Present** | ~85% | In scope — built |
| 10 | Funnel Analytics (step-by-step, abandonment) | ✅ **Present** | 100% | In scope — built |
| 11 | Issue Analytics (autocapture logs/crashes/freezes + Slack/Teams alerts) | ⚠️ **Partial** | ~35% | Autocapture built; freezes + alerting on roadmap |
| 12 | Retention Analysis (churn, stickiness, session frequency) | ❌ Absent | ~5% | On `plan.md` roadmap (P4), not built |
| 13 | Dashboards & Segments (custom KPI reports + segments) | ❌ Absent | ~5% | Segments on roadmap (P4); custom builders not built |

**Bottom line: 3 of 13 present, 1 partial, 9 absent.** Of the 9 absent, one is explicitly declared out-of-scope (session replay), four are roadmap items in `plan.md` not yet built (touch analysis, retention, segments, and the alerting half of issue analytics), and four are neither built nor planned (distributed tracing, continuous profiling, application metrics, heatmaps, Tara AI).

---

## Detail

### 1. Error Tracking — ✅ Present (~95%)
Real grouping **algorithm** and real aggregation, end-to-end.
- **Grouping:** `fingerprint.rs:23` — honors an explicit client fingerprint, else SHA-256 over the top-5 in-app frames nearest the crash, reducing each to a `module::function/filename` signature (drops line/col, collapses content-hash chunk names like `app.4f3a2b91.js → app.js`, masks digits/hex-addresses/uuids); with no frames, falls back to `type + normalized message`. Eight unit tests assert these actually group (`fingerprint.rs:224-338`).
- **Aggregation:** `process.rs:113` → `repo::upsert_issue` does `INSERT … ON CONFLICT (app_id, fingerprint) DO UPDATE SET times_seen = times_seen + 1, last_seen = excluded` (`repo.rs:522`) — a genuine per-occurrence counter. Affected-user count via Redis HyperLogLog (`process.rs:193`). Each occurrence also persisted as an `error_events` row with full stacktrace/breadcrumbs/context.
- **Crash capture:** JS `window.onerror` + `onunhandledrejection`, `handled:false` (`globalHandlers.ts:19`); Flutter wires all four uncaught layers — `FlutterError.onError`, `PlatformDispatcher.onError`, isolate listener, `runZonedGuarded` (`client.dart:62-72`).
- **Surfaced:** `/v1/apps/{id}/issues` list + detail (`routes/issues.rs`), `Issues.svelte` / `IssueDetail.svelte`. (List sorts by `last_seen DESC`, `repo.rs:562`.)
- **Caveats (beyond the claim):** no server-side symbolication/deminification; no issue merge/split UI.

### 2. Session Replay — ❌ Absent (0%, declared out-of-scope)
No recording or playback anywhere. No DOM-snapshot/rrweb/mutation recorder, no screen-frame capture, no replay-chunk envelope type, no player/`<video>`/canvas reconstruction in the dashboard. The `sessions` roll-up + `SessionDetail` **event timeline** is an event list, *not* video/DOM playback. The Flutter *"Replay anything captured before the transport was ready"* comment (`client.dart:100`) is about flushing a queued buffer. **README explicitly excludes this** (and the object storage it would need). Note: no iOS / Android / React Native SDK exists at all — only web + Flutter.

### 3. Distributed Tracing — ❌ Absent (~10%)
Only **single-span** performance monitoring. `transactions` (migration 0005) / `TransactionItem` (`envelope.rs:198`) store isolated rows (`op ∈ navigation|http|resource|screen_load|custom`, `duration_ms`, `http_status`, `url`, `session_id`), aggregated to p50/p95/throughput/error-rate on read (`routes/performance.rs`). None of the three claimed capabilities exist: (1) **no distributed tracing** — zero hits for `trace_id`/`span_id`/`parent_span`/`traceparent`; transactions carry no trace/span/parent fields, so spans are unlinked and there is no cross-service path map; (2) **no frozen/slow UI-frame** capture; (3) **no app-start (cold/warm) timing** — `screen_load` is only an allowed label, no SDK instruments app start (Flutter transactions are manual-only).

### 4. Continuous Profiling — ❌ Absent (0%)
No sampling profiler, no stack-sample capture, no flamegraph data, no profile envelope item or storage/UI. `EnvelopeItem` has exactly 5 variants, none a profile (`envelope.rs:90-95`). Every `profil*` grep hit is a user/person/device **profile**; every `cpu`/`sampling` hit is unrelated (`plan.md` quota-sampling / SDK guardrails). A single-span Transaction is timing, not a CPU profile.

### 5. Application Metrics — ❌ Absent (0%)
No user-facing metrics API. SDK surface is `init/captureException/captureMessage/track/identify/trackTransaction/addBreadcrumb/setUser/flush/close` (`index.ts:26-95`) — no counter/gauge/distribution/histogram method; no `Metric` envelope variant; no metrics table (the 5 migrations are init, rbac, rbac_index, sessions_devices, transactions); no metrics route or screen. All `counter`/`gauge` hits are **internal plumbing** (per-session roll-up counters, rate-limit window, HLL) — the exact look-alike this feature is not.

### 6. Heatmaps — ❌ Absent (0%)
The string `heatmap` appears **nowhere** in the repo. The only click capture is the JS DOM integration recording a `ui.click` breadcrumb whose message is a CSS selector — deliberately **no coordinates, no text/PII** (`dom.ts:27`). No scroll capture, no abandonment tracking, no spatial aggregation, no overlay.

### 7. Touch Analysis (rage/dead/double taps) — ❌ Absent (0%, on roadmap)
No gesture/coordinate capture, no repeat-tap timing, no dead-tap or rage-tap computation in SDKs, pipeline, schema, routes, or dashboard. The lone `ui.click` breadcrumb is an isolated event, not frustration analysis. Flutter has **no** gesture capture (its integrations are error-handlers only). Listed on the roadmap: `plan.md:67` *"Implement rage click, dead click, and frustration signal detection."*

### 8. Tara AI Analyst — ❌ Absent (0%)
No LLM/AI client, no insight engine, no NL-query interface, no `Tara` identifier. Zero functional hits for `anthropic|openai|langchain|mistral|cohere|ollama|gpt|llm|tara`. The analytics surface is entirely deterministic SQL. Not in `plan.md`.

### 9. User Journey Analytics — ✅ Present (~85%)
Real, data-driven path mapping. `repo::journey_nodes`/`journey_links` build a step-indexed transition graph from raw `analytics_events` using `row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at) - 1` as the step, aggregating per-step counts and adjacent-step transitions via a self-join (`repo.rs:1470-1514`). Route `GET /v1/apps/{id}/journeys` (`journeys.rs:40`, `EVENT_READ`, depth 2–10). `JourneyExplorer.svelte` renders a Sankey (`SankeyChart.svelte:31-95`) + top entry points + top transitions.
- **Gap:** drop-offs are only **implicit** (Sankey node heights narrow across steps); there is **no computed/quantified drop-off** — no terminal/exit node, no per-step drop count or %. The "uncover drop-offs" half is met visually, not as a metric.

### 10. Funnel Analytics — ✅ Present (100%)
Full end-to-end over real event data. `repo::funnel` is a genuine **chained-CTE**: step 0 counts distinct users who fired the first event (min timestamp); each next step joins the prior CTE requiring the event at-or-after the prior step's time; per-step distinct counts `UNION ALL`'d. `POST /v1/apps/{id}/funnel` validates 2–10 ordered steps (`funnels.rs`), returns `conv_from_start`/`conv_from_prev`/`total_entered`. `FunnelBuilder.svelte` is a real interactive builder; `FunnelChart.svelte` shows per-step bars with explicit drop-off (*"X% drop-off · N of M continued"*). Genuine time-ordered abandonment analysis.
- **Minor scope limits (not gaps in the claim):** no per-step property filters/breakdowns; step match is by bare event name.

### 11. Issue Analytics — ⚠️ Partial (~35%)
Claim = *autocapture logs + crashes + UI freezes* **with** *real-time Slack + MS Teams alerts.* Two of the three signals autocaptured; freezes and alerting absent.
- ✅ **Logs:** JS monkey-patches `console.log/info/warn/debug/error` into breadcrumbs, preserving output (`console.ts:32`).
- ✅ **Crashes:** JS global handlers (`globalHandlers.ts:15`) + full Flutter crash layer (flutter/isolate/platform-dispatcher/zone). Flow into `issues` (`Issues.svelte`).
- ❌ **UI freezes (ANR/jank):** none — no watchdog, no frame monitor. Zero hits for `freeze|anr|jank|frozen|unresponsive`. (Roadmap: `plan.md:88` Android ANR capture.)
- ❌ **Slack / Microsoft Teams alerts:** completely absent — zero hits for `slack|teams|webhook`; no notification table, no alert model/repo, no alerting route. (Roadmap: `plan.md:107-108` alerting & integrations service, P5.) **Not** in README's explicit out-of-scope list, so relative to a naive reading it's a genuine gap.

### 12. Retention Analysis — ❌ Absent (~5%, on roadmap P4)
Only point-in-time counts and raw event-count series exist. `overview_totals` (`repo.rs:1262`) returns single-window totals (events/errors/sessions/users, `new_users`, `crashed_sessions`); `overview` derives error-rate + crash-free ratio; `event_series` (`repo.rs:789`) buckets `count(*)` by day — **not** distinct active users. Overview shows `newUserShare = new_users/users` (a share-of-total, not retention). **No** retention curve, cohort grid, churn metric, DAU/MAU stickiness, or session-frequency-over-time; zero hits for `retention|cohort|churn|stickiness|dau|mau`. The raw columns (`event_users.first_seen/last_seen`, sessions/devices) exist to build it, but nothing computes it. Roadmap: `plan.md:106,121,178` (P4 product analytics).

### 13. Dashboards & Segments — ❌ Absent (~5%)
Only fixed, hard-coded screens with basic search/date filters — nothing user-composable or savable.
- **KPI tiles are static/code-defined:** `Overview.svelte:98` and `Performance.svelte:148` render fixed StatTile rows; users can't add/remove/define tiles.
- **No persistence:** `FunnelBuilder` computes ad-hoc but there's **no save** — no funnels/segments/reports/dashboards table in any migration, no saved-view route.
- **No segment builder:** `UsersExplorer` offers one free-text search; Event Explorer has a few fixed filters — none savable/reusable as a named audience.
Roadmap: segments in `plan.md:106,121,178` (P4).

---

## How the gaps map to declared scope

- **Explicitly out-of-scope for this cut (README):** Session Replay (#2) — and the object storage / columnar tiers it would require. Legitimately absent by design.
- **On the `plan.md` roadmap, not yet built:** Touch Analysis (#7, roadmap), Retention (#12, P4), Segments half of #13 (P4), and the *UI-freeze + Slack/Teams-alerting* halves of Issue Analytics (#11, P5). Expected-absent for an MVP, but **not** enumerated in README's out-of-scope note.
- **Neither built nor on the roadmap:** Distributed Tracing (#3), Continuous Profiling (#4), Application Metrics (#5), Heatmaps (#6), Tara AI Analyst (#8), and custom-dashboard builders (#13). These are Sentry/UXCam surface that this product never set out to match.

## Recommendations

1. **Reconcile the checklist with the product.** The checklist ≈ Sentry + UXCam; this MVP is a deliberately narrower "error ↔ analytics on one timeline" wedge. If the checklist is a client-facing promise, align it (mark deferred/roadmap items) before it's read as shipped.
2. **Two honest partials worth naming:** Journey drop-offs (#9) are visual-only — add a computed per-step drop-off count/% to fully meet the claim. Issue Analytics (#11) autocaptures logs+crashes but has no freeze detection and **no alerting** — the alerting gap is the most load-bearing missing piece for an "issue analytics" pitch.
3. **README's out-of-scope note is narrower than the real gap set.** Consider extending it to name the roadmap features (tracing/profiling/metrics/heatmaps/touch/retention/segments/alerting/AI) so "MVP" scope is unambiguous.
