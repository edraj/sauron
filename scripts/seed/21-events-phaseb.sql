-- PHASE B: production DENSITY. 140,000,000 rows over 14 days (2026-08-11 →
-- 2026-08-24) at exactly 10M rows/day: 4.3M analytics + 1.075M errors + 4.3M
-- transactions + 325k sessions per weekday-weighted day. Identity pools
-- (users/devices/issues/envs) are REUSED from 10-dimensions (run it with
-- -v light=1 first so payloads are light); only the session namespace is new
-- (`seed_b_…`) — reusing `seed_s_<day_idx>` would collide with the original
-- 90-day seed's day indexes and silently attach these events to May's
-- sessions through the ON CONFLICT DO NOTHING.
--
-- Shape rules, and why each one is not cosmetic:
--
--  * `occurred_at` is drawn from a diurnal curve (mean of three uniforms, so a
--    bell centred at midday) and never leaves its own day, which is what keeps
--    every row inside a real partition instead of the DEFAULT.
--  * `distinct_id` is drawn ZIPFIAN over the 50k users, so a minority carry a
--    majority of the events. A uniform draw would make every per-user query
--    equally cheap and hide exactly the tail this dataset exists to expose.
--  * Events reference a session, and inherit the user / device / environment
--    FROM that session. Picking them independently would produce sessions whose
--    rows disagree about who they belong to, and the rollups would then be
--    aggregating nonsense.
--  * Payloads are cloned from real ingest-worker output (`seed_tpl_*`).
--
-- Commits per day rather than one 18.5M-row transaction: keeps WAL bounded,
-- makes progress durable, and means an interruption costs one day, not the run.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';
SET work_mem = '512MB';
SELECT setseed(0.4242);

\set app_id '\'ee1fb653-cadd-4f27-9321-ff10f382a18c\''

-- ---------------------------------------------------------------------------
-- Per-day allocation. Weekends run at 60% of a weekday, and the remainder is
-- credited to the last day so the totals land EXACTLY on target rather than
-- "about 8 million".
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_plan;
CREATE TABLE seed_plan AS
WITH days AS (
  SELECT row_number() OVER (ORDER BY d) AS day_idx,
         d::date AS day,
         CASE WHEN extract(dow FROM d) IN (0, 6) THEN 0.6 ELSE 1.0 END AS w
  FROM generate_series(DATE '2026-08-11', DATE '2026-08-24', INTERVAL '1 day') d
), tot AS (SELECT sum(w) AS sw FROM days)
SELECT day_idx, day,
       floor(60200000 * w / sw)::bigint AS n_analytics,
       floor(15050000 * w / sw)::bigint AS n_errors,
       floor( 4550000 * w / sw)::bigint AS n_sessions
FROM days, tot;

UPDATE seed_plan SET
  n_analytics = n_analytics + (60200000 - (SELECT sum(n_analytics) FROM seed_plan)),
  n_errors    = n_errors    + (15050000 - (SELECT sum(n_errors)    FROM seed_plan)),
  n_sessions  = n_sessions  + ( 4550000 - (SELECT sum(n_sessions)  FROM seed_plan))
WHERE day_idx = (SELECT max(day_idx) FROM seed_plan);

CREATE UNIQUE INDEX ON seed_plan (day_idx);

SELECT count(*) AS days, sum(n_analytics) AS analytics, sum(n_errors) AS errors,
       sum(n_sessions) AS sessions
FROM seed_plan;

-- Per-day session scratch, reused across days (TRUNCATE, not re-CREATE, because
-- the loop commits and a temp table declared ON COMMIT DROP would not survive).
DROP TABLE IF EXISTS seed_day_sessions;
CREATE TABLE seed_day_sessions (
  sn             int primary key,
  session_id     text not null,
  distinct_id    text not null,
  device_key     text not null,
  environment_id uuid,
  started_at     timestamptz not null,
  release        text
);

DO $seed$
DECLARE
  app        uuid := 'ee1fb653-cadd-4f27-9321-ff10f382a18c';
  -- Weby's three ENROLLMENT ids (`app_environments.id`). Not the catalogue ids
  -- from `environments` — the API filters on the enrollment, and the catalogue
  -- id produces rows no query will ever return.
  env_prod   uuid := '988d70d7-d24c-499d-af1e-8dd9d7be06ac';
  env_bench  uuid := 'f5ca97e8-3d9d-4b62-8a40-a5ae091db31f';
  env_demo   uuid := 'de670846-4260-403f-8848-d455ec86aad6';
  p          record;
  day0       timestamptz;
  t_start    timestamptz := clock_timestamp();
BEGIN
  FOR p IN SELECT * FROM seed_plan ORDER BY day_idx LOOP
    day0 := (to_char(p.day, 'YYYY-MM-DD') || ' 00:00:00+00')::timestamptz;

    TRUNCATE seed_day_sessions;

    -- Sessions own the identity. Everything else inherits from here.
    INSERT INTO seed_day_sessions (sn, session_id, distinct_id, device_key,
                                   environment_id, started_at, release)
    SELECT
      s,
      'seed_b_' || p.day_idx || '_' || s,
      'seed_u_' || lpad(u::text, 6, '0'),
      'seed_d_' || lpad(u::text, 6, '0'),
      CASE WHEN r < 0.70 THEN env_prod WHEN r < 0.90 THEN env_bench ELSE env_demo END,
      day0 + make_interval(secs => sec),
      'web@1.' || (1 + (s % 9))::text || '.0'
    FROM (
      SELECT s,
             -- Zipfian: power() < 1 would flatten it, > 1 concentrates on the
             -- low indices. 2.5 gives a heavy head without starving the tail.
             least(50000, 1 + floor(50000 * power(random(), 2.5))::int) AS u,
             random() AS r,
             (86400.0 * (random() + random() + random()) / 3.0) AS sec
      FROM generate_series(1, p.n_sessions) s
      OFFSET 0
    ) g;

    INSERT INTO sessions (app_id, session_id, distinct_id, device_key, started_at,
                          last_event_at, environment_id, release, context)
    SELECT app, session_id, distinct_id, device_key, started_at, started_at,
           environment_id, release, '{}'::jsonb
    FROM seed_day_sessions
    ON CONFLICT DO NOTHING;

    -- Analytics events.
    INSERT INTO analytics_events (
      app_id, environment_id, name, distinct_id, properties, context, session_id,
      release, ip_address, occurred_at, received_at, device_key, screen, tags,
      contexts, extra)
    SELECT
      app, ds.environment_id, t.name, ds.distinct_id, t.properties, t.context,
      ds.session_id, ds.release, t.ip_address,
      day0 + make_interval(secs => g.sec), day0 + make_interval(secs => g.sec),
      ds.device_key, t.screen, t.tags, t.contexts, t.extra
    FROM (
      SELECT 1 + floor(random() * p.n_sessions)::int AS sn,
             1 + floor(random() * 20000)::int        AS tk,
             (86400.0 * (random() + random() + random()) / 3.0) AS sec
      FROM generate_series(1, p.n_analytics)
      OFFSET 0
    ) g
    JOIN seed_day_sessions ds ON ds.sn = g.sn
    JOIN seed_tpl_analytics t ON t.k = g.tk;

    -- Error events. Identity (type/title/culprit/level/fingerprint) comes from
    -- the issue pool so a fingerprint means one thing; only the bulky payload
    -- is cloned.
    INSERT INTO error_events (
      app_id, environment_id, issue_id, fingerprint, level, message,
      exception_type, exception_value, stacktrace, breadcrumbs, context, tags,
      release, distinct_id, sdk, ip_address, occurred_at, received_at,
      session_id, device_key, screen, contexts, extra, handled, title, culprit)
    SELECT
      app, ds.environment_id, iss.id, ip.fingerprint, ip.level, t.message,
      ip.type, t.exception_value, t.stacktrace, t.breadcrumbs, t.context, t.tags,
      ds.release, ds.distinct_id, t.sdk, t.ip_address,
      day0 + make_interval(secs => g.sec), day0 + make_interval(secs => g.sec),
      ds.session_id, ds.device_key, t.screen, t.contexts, t.extra,
      -- Crash-free rate counts UNHANDLED only, so this ratio is what makes that
      -- metric a real number instead of 0% or 100%.
      (g.hnd >= 0.25),
      ip.title, ip.culprit
    FROM (
      SELECT 1 + floor(random() * p.n_sessions)::int AS sn,
             1 + floor(random() * 20000)::int        AS tk,
             1 + floor(random() * 300)::int          AS ik,
             random()                                AS hnd,
             (86400.0 * (random() + random() + random()) / 3.0) AS sec
      FROM generate_series(1, p.n_errors)
      OFFSET 0
    ) g
    JOIN seed_day_sessions ds ON ds.sn = g.sn
    JOIN seed_tpl_error    t  ON t.k  = g.tk
    JOIN seed_issue_pool   ip ON ip.k = g.ik
    JOIN issues            iss ON iss.app_id = app AND iss.fingerprint = ip.fingerprint;

    -- Transactions, one per analytics event.
    INSERT INTO transactions (
      app_id, environment_id, name, op, duration_ms, status, http_method,
      http_status, url, distinct_id, session_id, device_key, release,
      ip_address, occurred_at, received_at, finished_at, tags, extra)
    SELECT
      app, ds.environment_id,
      (ARRAY['GET /api/items','POST /api/checkout','GET /api/search',
             'GET /api/profile','POST /api/auth/login','GET /api/feed'])[1 + (g.nk % 6)],
      (ARRAY['http.server','db.query','navigation','resource.fetch'])[1 + (g.nk % 4)],
      -- Log-ish spread so p95 sits well above the median, which is what makes
      -- the Performance page's percentiles distinguishable.
      round((5.0 * exp(4.0 * g.dur))::numeric, 2)::float8,
      CASE WHEN g.dur > 0.97 THEN 'internal_error' ELSE 'ok' END,
      (ARRAY['GET','POST','PUT','DELETE'])[1 + (g.nk % 4)],
      CASE WHEN g.dur > 0.97 THEN 500 ELSE 200 END,
      'https://app.example.com/' || (ARRAY['items','checkout','search','profile','feed'])[1 + (g.nk % 5)],
      ds.distinct_id, ds.session_id, ds.device_key, ds.release, NULL,
      day0 + make_interval(secs => g.sec), day0 + make_interval(secs => g.sec),
      day0 + make_interval(secs => g.sec), '{}'::jsonb, '{}'::jsonb
    FROM (
      SELECT 1 + floor(random() * p.n_sessions)::int AS sn,
             floor(random() * 1000)::int             AS nk,
             random()                                AS dur,
             (86400.0 * (random() + random() + random()) / 3.0) AS sec
      FROM generate_series(1, p.n_analytics)
      OFFSET 0
    ) g
    JOIN seed_day_sessions ds ON ds.sn = g.sn;

    COMMIT;

    RAISE NOTICE 'day % / 14 (%) done, elapsed %',
      p.day_idx, p.day, clock_timestamp() - t_start;
  END LOOP;

  RAISE NOTICE 'event generation complete in %', clock_timestamp() - t_start;
END
$seed$;
