# Firehose index audit — 2026-08-25

**Verdict: drop nothing.** All 40 indexes across `analytics_events` /
`error_events` / `transactions` (~85 GB at the 158M-row dataset) have live
consumers. The earlier hypothesis — "the migration-71 rollups orphaned some
btrees" — is **falsified**, for one structural reason: the pro-search
programme made nearly every column a filterable, time-windowed search
dimension, so each `(app_id, <column>, occurred_at)` index serves the search
planner and the list pages, not only the aggregate endpoints the rollups
replaced. The rollup gates also deliberately keep their legacy queries as
fallbacks for not-yet-backfilled apps, which pins the remainder.

Method: per-parent aggregated `pg_stat` scan counts + sizes (stats window
covered heavy exercising of BOTH the legacy and rollup regimes on the same
day), then a code-consumer citation for every index — scan counts advise,
code decides.

| Index (family) | Size @158M | Verdict — consumer |
|---|---|---|
| `*_pkey1 (id, occurred_at)` | 3.5+0.9+3.5 GB | ACTIVE — row identity, detail fetches |
| `*_app_time_id / project_idx / app_occurred (app, time[, id])` | 5.0+0.9+3.5 GB | ACTIVE — keyset list pagination |
| `analytics_name_idx (app, name, time)` | 5.2 GB | ACTIVE — funnels (raw by design, `funnel_sql` binds step names), `name` search |
| `*_app_screen_time (app, screen, time) WHERE screen NOT NULL` | 4.9+1.2 GB | ACTIVE — screen-detail drill-down samples; screens legacy fallback |
| `*_app_env_time_users (app, env, time) INCLUDE (distinct_id)` | 6.6+1.7 GB | ACTIVE — env-scoped list/search keyset is `(app, env, time DESC)`-shaped; also active-users/series legacy fallback |
| `*_app_distinct_time / distinct_idx (app, distinct, time)` | 5.0+1.3 GB | ACTIVE — person timeline (time-ordered per user) |
| `*_app_distinct_env (app, distinct, env, time)` | 6.6+1.6 GB | ACTIVE — `event_user_membership_exists` legs; env-scoped person reads |
| `*_app_device_env (app, device, env, time)` | 6.6+1.6+6.6 GB | ACTIVE — device drill-down, `list_devices` scoped LATERALs, membership |
| `*_app_rel_time / app_release_time (app, release, time)` | 5.0+1.3 GB | ACTIVE — `release` is a searchable column (`Store::Column("release")`) |
| `error_events_app_level_time` | 1.1 GB | ACTIVE — `level` searchable on occurrences |
| `error_events_issue_time_id / issue_env_time` | 1.3+1.7 GB | ACTIVE — issue detail pages |
| `transactions_app_name_time / app_op_time / app_op_name` | 5.4+5.0+0.4 GB | ACTIVE — `name`/`op` searchable (`query_plan/transactions.rs:639-640`); perf legacy fallback |
| `*_app_session (app, session_id)` | 0.5+0.9 GB | ACTIVE — session timeline signal streams |
| `*_tags_gin` | 0.7 GB total | ACTIVE — `@tag` search (0 scans here only because the bench account never used tag search — **scans=0 ≠ dead**) |
| `*_app_workflow (partial)` | 736 kB ×3 | ACTIVE — workflows pages; size-irrelevant anyway |
| `error_events_stack_sha (partial)` | 728 kB | ACTIVE — ingest-time stack-pool dedup (migration 68) |
| `*_received_brin` | ~2.5 MB ×3 | ACTIVE — the rollup fold's watermark pull |

Two forward-looking notes:

1. **If a future feature removes a search dimension**, its index becomes the
   drop candidate — this table is the checklist to re-run.
2. **The fallback tax is real but small in count**: once a deployment confirms
   fleet-wide `backfill-rollups`, the *purely* legacy-fallback shapes could in
   principle be revisited — but as of today every such index is co-owned by an
   active search/list shape, so there is no post-backfill drop list either.
