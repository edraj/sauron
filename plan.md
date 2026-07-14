# Project Plan — Unified Observability & Product Analytics Platform

A platform that merges **session replay + product analytics (à la UXCam)** with **error, crash & event reporting (à la Sentry)** into one product for **web and mobile apps**. The core value proposition: when an error fires, you can jump straight into the exact user session that produced it — one timeline that ties *what the user did* to *what broke*.

> Codename used throughout: **"Lens"** (rename as you like).

---

## 0. Guiding product decisions (do these first)

- [ ] Write a one-page product thesis: the wedge is "error → replay in one click" (the thing neither Sentry nor UXCam does cleanly).
- [ ] Define the 3 target personas: mobile engineer, frontend/web engineer, product manager.
- [ ] Pick the initial platform scope for the MVP (recommend: **Web JS + one mobile SDK, iOS or Android** — not all at once).
- [ ] Decide the core data model boundary: a single "Session" object that owns both events (product analytics) and issues (errors) on one timeline.
- [ ] Set non-negotiables up front: privacy/PII masking, data residency, and pricing/quota model (these shape architecture, so decide early).
- [ ] Define success metrics for the MVP (e.g., time-to-root-cause, activation = first replay viewed within X min of signup).

---

## 1. Discovery & Foundations

### Research & positioning
- [ ] Competitive teardown of Sentry, UXCam, LogRocket, FullStory, Datadog RUM, PostHog — feature matrix + pricing.
- [ ] Identify the differentiator features (unified session, cross-platform correlation, one SDK for both signals).
- [ ] Interview 8–12 target engineers/PMs to validate the "one timeline" pain point.

### Legal, privacy & compliance
- [ ] Draft data classification policy (what's captured, what's masked, what's never captured).
- [ ] GDPR/CCPA plan: data subject deletion, consent, retention windows.
- [ ] Define PII masking defaults (mask all text/inputs by default; opt-in to unmask).
- [ ] SOC 2 Type II roadmap (needed to sell to any serious customer) + DPA template.
- [ ] Decide data residency options (US/EU regions) — affects infra topology.

### Architecture & tech choices
- [ ] High-level architecture diagram: SDKs → ingest gateway → stream → processing → storage → API → dashboard.
- [ ] Choose ingestion transport (HTTPS batching + optional gRPC; consider OpenTelemetry compatibility).
- [ ] Choose event streaming backbone (Kafka / Redpanda / Kinesis).
- [ ] Choose storage tiers: time-series/columnar for events (ClickHouse), object storage for replay blobs (S3), relational for metadata (Postgres).
- [ ] Decide multi-tenancy model (org → project → environment isolation).
- [ ] Define the event schema / envelope (shared by errors and product events; align with OpenTelemetry where possible).

---

## 2. Data Ingestion Pipeline (backend core)

- [ ] Build the ingest gateway/edge endpoint (auth via project DSN/key, rate limiting, payload validation).
- [ ] Implement envelope format supporting: errors, breadcrumbs, product events, replay chunks, performance/spans.
- [ ] Add batching, compression (gzip/brotli), and offline queue acknowledgment.
- [ ] Stream ingested data into the processing pipeline (dedup, enrich, normalize).
- [ ] Build error grouping/fingerprinting engine (stack-trace normalization → issue grouping).
- [ ] Build event enrichment (geo/IP, device, OS, app version, release, user id).
- [ ] Implement sampling & quota enforcement per project/plan.
- [ ] Build source map / symbolication service (JS source maps, iOS dSYM, Android ProGuard/R8 mapping).
- [ ] Set up dead-letter queue + replay-of-failed-ingestion tooling.
- [ ] Load test the pipeline (target: sustained X k events/sec with p99 < N ms).

---

## 3. Session Replay Engine

- [ ] Choose web capture strategy (DOM mutation recording, e.g. rrweb-style vs. canvas/screenshot).
- [ ] Choose mobile capture strategy (view-hierarchy snapshots + touch/gesture events; optional screenshot fallback).
- [ ] Implement privacy masking at capture time (block/mask selectors, sensitive fields never leave device).
- [ ] Build replay chunk upload (incremental, resumable, compressed).
- [ ] Build the replay reconstruction/player (web + mobile session playback in the dashboard).
- [ ] Sync the replay timeline with events, errors, network requests, and console logs.
- [ ] Implement "rage click", dead click, and frustration signal detection.
- [ ] Handle replay storage lifecycle (hot vs. cold, retention, deletion on request).
- [ ] Build performance guardrails so the SDK never degrades the host app (CPU, battery, payload size caps).

---

## 4. SDKs / Client Libraries

> The SDK is the product's front door — invest heavily in reliability and small footprint.

### Web (JavaScript/TypeScript) — MVP
- [ ] Core SDK: init, DSN, automatic error/unhandled-rejection capture.
- [ ] Breadcrumbs (console, clicks, navigation, XHR/fetch).
- [ ] Session replay recorder integration.
- [ ] Product event API (`track()`, `identify()`, screen/route tracking).
- [ ] Performance/Web Vitals + network capture.
- [ ] Source map upload CLI + release/version tagging.
- [ ] Framework wrappers: React, Vue, Angular, Next.js.

### Mobile — MVP (pick iOS **or** Android first)
- [ ] iOS SDK (Swift): crash capture, signal/exception handling, replay, events.
- [ ] Android SDK (Kotlin): crash/ANR capture, replay, events.
- [ ] Offline buffering + retry on reconnect.
- [ ] Symbolication mapping upload (dSYM / ProGuard).

### Cross-platform (post-MVP)
- [ ] React Native SDK.
- [ ] Flutter SDK.
- [ ] Backend SDKs for server errors (Node, Python, Go) to close the full-stack loop.
- [ ] SDK versioning, deprecation policy, and auto-update strategy.

---

## 5. Backend Services & APIs

- [ ] AuthN/AuthZ service (org/team/project roles, SSO/SAML for enterprise, API tokens).
- [ ] Project & environment management API.
- [ ] Issues API (list, group, assign, resolve, ignore, regressions).
- [ ] Sessions/Replay API (search, fetch, stream).
- [ ] Product analytics query API (funnels, retention, segments, event counts).
- [ ] Alerting & notification service (thresholds, spikes, new-issue, regression alerts).
- [ ] Integrations service (Slack, Jira, Linear, GitHub, PagerDuty, webhooks).
- [ ] Billing & metering service (usage-based quotas, plan limits, overage).
- [ ] Data deletion / GDPR request processor.

---

## 6. Dashboard / Frontend App

- [ ] Design system + component library.
- [ ] Onboarding flow: create project → copy SDK snippet → "waiting for first event" live state.
- [ ] **Issues view**: grouped errors, frequency, affected users, stack trace, breadcrumbs.
- [ ] **The killer feature — "Watch the session"**: one click from an error to the replay at the exact moment it happened.
- [ ] **Session replay view**: player + synced event/error/network timeline.
- [ ] **Product analytics views**: funnels, retention, event explorer, user segments, screen flow.
- [ ] Unified user profile: a person's errors + sessions + events in one place.
- [ ] Search & filtering across errors, sessions, and events (release, device, version, user).
- [ ] Alerts & integrations configuration UI.
- [ ] Team/settings/billing/quota management UI.
- [ ] Dark mode + accessibility pass.

---

## 7. Infrastructure, DevOps & Reliability

- [ ] Provision cloud infra as code (Terraform): regions for US + EU.
- [ ] Kubernetes (or equivalent) for services; autoscaling for ingest spikes.
- [ ] CI/CD pipelines for backend, SDKs, and dashboard.
- [ ] Observability of the platform itself (metrics, logs, traces, uptime).
- [ ] Secrets management + key rotation.
- [ ] Backup & disaster recovery plan + tested restore.
- [ ] Cost monitoring (storage of replays is the big cost driver — budget for it).
- [ ] Security hardening: pen test, dependency scanning, WAF, DDoS protection on ingest edge.
- [ ] Data retention automation (auto-purge per plan/policy).

---

## 8. Quality, Testing & Verification

- [ ] Unit + integration test suites per SDK and service.
- [ ] End-to-end test: fire an error in a sample app → see grouped issue → open its replay.
- [ ] Cross-browser + cross-device SDK test matrix.
- [ ] Load/soak testing of ingestion at target scale.
- [ ] Privacy verification: confirm masked fields never leave the device (audit payloads on the wire).
- [ ] Symbolication accuracy tests (obfuscated → readable stack traces).
- [ ] Dogfood: run Lens's SDK inside Lens's own dashboard.
- [ ] Beta program with 5–10 design-partner customers; collect structured feedback.

---

## 9. Go-to-Market & Launch

- [ ] Pricing & packaging (free tier for adoption; usage-based for events/replays/retention).
- [ ] Public docs site (quickstarts per platform, API reference, migration guides).
- [ ] "Migrate from Sentry / from UXCam" guides (import + SDK swap paths).
- [ ] Marketing site + interactive demo/sandbox.
- [ ] Sample/demo apps (web + mobile) that show the full flow.
- [ ] Developer relations: launch content, integrations directory, open-source the SDKs.
- [ ] Support & status page + on-call rotation.
- [ ] Launch checklist (Product Hunt / HN / dev communities) and beta → GA gating criteria.

---

## Suggested phasing

| Phase | Goal | Key deliverables |
|-------|------|------------------|
| **P0 — Foundations** | Decisions locked, architecture drawn | Product thesis, data model, privacy policy, infra design |
| **P1 — Ingest + Errors (Web)** | Errors work end-to-end on web | Ingest pipeline, grouping, Web SDK, Issues view |
| **P2 — Replay + the "watch session" link** | The differentiator is live | Web replay engine, player, error→replay jump |
| **P3 — Mobile** | One mobile SDK shipped | iOS *or* Android SDK, crash + replay, symbolication |
| **P4 — Product analytics** | PM persona served | Funnels, retention, segments, unified user profile |
| **P5 — Enterprise & GA** | Sellable & scalable | SSO, SOC 2, alerting/integrations, billing, EU region, launch |

---

## Biggest risks to watch

- **Replay storage cost** — the single largest cost/scaling driver; design retention and sampling early.
- **SDK performance footprint** — if the SDK slows down customer apps, you're dead on arrival.
- **Privacy/PII** — mask by default; a single leak of sensitive replay data is an existential event.
- **Scope creep** — "merge two big products" invites doing everything. The wedge is *error → replay*. Ship that first.
