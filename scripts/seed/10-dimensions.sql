-- Fixed cardinality for the seed: issues, users, devices, and the payload
-- template pools the event generator clones from.
--
-- Everything created here is prefixed `seed_` (helper tables) or carries a
-- `seed_` identity key (`seed_u_000001`, `seed_d_000001`, `seed_fp_001`), which
-- is what makes `90-cleanup.sql` able to remove exactly what this added instead
-- of pattern-matching at what it hopes is the right data.
--
-- `first_seen` / `last_seen` / counters are placeholders here and are REPLACED
-- in `30-rollups.sql` from the events actually generated. Deriving them rather
-- than inventing them is the only way the dimension rows and the event rows
-- cannot disagree.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';

-- Reproducible: same seed, same dataset.
SELECT setseed(0.42);

\set app_id '\'ee1fb653-cadd-4f27-9321-ff10f382a18c\''

-- ---------------------------------------------------------------------------
-- Device model pool: exactly 100 distinct models.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_models;
CREATE TABLE seed_models AS
SELECT
  row_number() OVER (ORDER BY f.family, v.n) AS k,
  f.family,
  f.family || ' ' || v.n::text AS model,
  f.os_name,
  (9 + v.n)::text || '.' || (v.n % 4)::text AS os_version,
  f.browser,
  f.arch
FROM (VALUES
  ('Pixel',      'Android', 'Chrome',  'arm64'),
  ('Galaxy S',   'Android', 'Chrome',  'arm64'),
  ('Redmi Note', 'Android', 'Chrome',  'arm64'),
  ('Moto G',     'Android', 'Firefox', 'arm64'),
  ('OnePlus',    'Android', 'Chrome',  'arm64'),
  ('iPhone',     'iOS',     'Safari',  'arm64'),
  ('iPad',       'iPadOS',  'Safari',  'arm64'),
  ('Surface',    'Windows', 'Edge',    'x86_64'),
  ('ThinkPad',   'Windows', 'Chrome',  'x86_64'),
  ('MacBook',    'macOS',   'Safari',  'arm64')
) f(family, os_name, browser, arch)
CROSS JOIN generate_series(1, 10) v(n);

-- ---------------------------------------------------------------------------
-- Issue pool: 300 distinct fingerprints.
--
-- The error generator takes type/title/culprit/level from HERE rather than from
-- the payload template, so every occurrence sharing a fingerprint also shares
-- its identity — which is what a fingerprint means. Cloning identity from a
-- random template row instead would produce one "issue" wearing 300 different
-- exception types.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_issue_pool;
CREATE TABLE seed_issue_pool AS
SELECT
  i AS k,
  'seed_fp_' || lpad(i::text, 3, '0') AS fingerprint,
  (ARRAY['TypeError','RangeError','NetworkError','StateError','TimeoutException',
         'FormatException','NullPointerException','ArgumentError'])[1 + (i % 8)] AS type,
  (ARRAY['TypeError','RangeError','NetworkError','StateError','TimeoutException',
         'FormatException','NullPointerException','ArgumentError'])[1 + (i % 8)]
    || ': ' ||
  (ARRAY['cannot read property of undefined','index out of range','request failed',
         'setState after dispose','operation timed out','unexpected token',
         'null receiver','invalid argument'])[1 + (i % 8)] AS title,
  (ARRAY['app/checkout','app/cart','app/search','app/profile','app/feed',
         'app/settings','app/auth','app/media'])[1 + (i % 8)]
    || '.' ||
  (ARRAY['submit','load','render','sync','parse','init'])[1 + (i % 6)] AS culprit,
  -- Mostly `error`; a minority of noisier levels so level filters have
  -- something to separate.
  CASE WHEN i % 20 = 0 THEN 'fatal' WHEN i % 7 = 0 THEN 'warning' ELSE 'error' END AS level
FROM generate_series(1, 300) i;

INSERT INTO issues (app_id, fingerprint, type, title, culprit, level, status,
                    first_seen, last_seen, last_event_at, times_seen, users_seen)
SELECT
  :app_id::uuid, p.fingerprint, p.type, p.title, p.culprit, p.level,
  -- A realistic triage mix rather than 300 unresolved rows.
  CASE WHEN p.k % 11 = 0 THEN 'resolved' WHEN p.k % 17 = 0 THEN 'ignored' ELSE 'unresolved' END,
  now(), now(), now(), 0, 0
FROM seed_issue_pool p
ON CONFLICT (app_id, fingerprint) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 50,000 users and 50,000 devices, paired 1:1.
--
-- One device per person is the ordinary shape for this data; the 100-model pool
-- is what `devices.model` draws from, so the Devices page shows 100 distinct
-- models across 50k rows rather than 50k one-off strings.
-- ---------------------------------------------------------------------------
INSERT INTO event_users (app_id, distinct_id, properties, first_seen, last_seen,
                         identified_at, identified_source)
SELECT
  :app_id::uuid,
  'seed_u_' || lpad(i::text, 6, '0'),
  jsonb_build_object(
    'plan',   (ARRAY['free','pro','team','enterprise'])[1 + (i % 4)],
    'region', (ARRAY['eu-west','us-east','us-west','ap-south','sa-east'])[1 + (i % 5)]
  ),
  now(), now(),
  -- Two thirds identified, the rest still anonymous.
  CASE WHEN i % 3 <> 0 THEN now() END,
  CASE WHEN i % 3 <> 0 THEN 'identify' END
FROM generate_series(1, 50000) i
ON CONFLICT (app_id, distinct_id) DO NOTHING;

INSERT INTO devices (app_id, device_key, family, model, os_name, os_version, arch,
                     browser, last_distinct_id, first_seen, last_seen)
SELECT
  :app_id::uuid,
  'seed_d_' || lpad(i::text, 6, '0'),
  m.family, m.model, m.os_name, m.os_version, m.arch, m.browser,
  'seed_u_' || lpad(i::text, 6, '0'),
  now(), now()
FROM generate_series(1, 50000) i
JOIN seed_models m ON m.k = 1 + (i % 100)
ON CONFLICT (app_id, device_key) DO NOTHING;

-- ---------------------------------------------------------------------------
-- Payload template pools.
--
-- These are REAL rows written by the ingest worker (partition 2026_07_27 holds
-- ~210k of them). Cloning their jsonb is what makes the seeded rows carry
-- production-shaped `properties`/`context`/`contexts`/`extra` — roughly 1,475
-- bytes per analytics event — instead of a synthetic payload whose TOAST and
-- buffer behaviour would not resemble the real thing.
--
-- A dense `k` lets the generator join on `(i % n) + 1`, which is a hash join.
-- The obvious alternative (`ORDER BY random() LIMIT 1` per row) is a sort per
-- generated row and does not finish at this scale.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_tpl_analytics;
CREATE TABLE seed_tpl_analytics AS
SELECT row_number() OVER () AS k,
       name, properties, context, release, ip_address, screen, tags, contexts, extra
FROM (SELECT * FROM analytics_events_2026_07_27 ORDER BY random() LIMIT 20000) s;

DROP TABLE IF EXISTS seed_tpl_error;
CREATE TABLE seed_tpl_error AS
SELECT row_number() OVER () AS k,
       message, exception_value, stacktrace, breadcrumbs, context, tags, sdk,
       contexts, extra, release, ip_address, screen
FROM (SELECT * FROM error_events_2026_07_27 ORDER BY random() LIMIT 20000) s;

-- Light-payload mode (`-v light=1`): overwrite the sampled pools with
-- minimal payloads (~0.2 KB/row vs ~1.9 KB), so a production-DENSITY bench
-- (14 days x 10M/day = 140M rows) fits this disk. Row COUNTS and identity
-- shapes are untouched — only payload weight changes, so every counter-level
-- assertion in 50-verify.sql still holds.
\if :{?light}
UPDATE seed_tpl_analytics SET
  properties = '{"k":1}'::jsonb, context = '{}'::jsonb, contexts = '{}'::jsonb,
  extra = '{}'::jsonb, tags = '{"t":"light"}'::jsonb;
UPDATE seed_tpl_error SET
  message = 'light error', exception_value = 'light',
  stacktrace = '[{"function":"f","filename":"a.rs","lineno":1}]'::jsonb,
  breadcrumbs = '[]'::jsonb, context = '{}'::jsonb, tags = '{}'::jsonb,
  contexts = '{}'::jsonb, extra = '{}'::jsonb;
\endif

CREATE UNIQUE INDEX ON seed_tpl_analytics (k);
CREATE UNIQUE INDEX ON seed_tpl_error (k);
CREATE UNIQUE INDEX ON seed_models (k);
CREATE UNIQUE INDEX ON seed_issue_pool (k);

ANALYZE seed_tpl_analytics;
ANALYZE seed_tpl_error;
ANALYZE seed_models;
ANALYZE seed_issue_pool;

SELECT
  (SELECT count(*) FROM seed_models)                                        AS models,
  (SELECT count(DISTINCT model) FROM seed_models)                           AS distinct_models,
  (SELECT count(*) FROM issues WHERE fingerprint LIKE 'seed\_%')            AS issues,
  (SELECT count(*) FROM event_users WHERE distinct_id LIKE 'seed\_%')       AS users,
  (SELECT count(*) FROM devices WHERE device_key LIKE 'seed\_%')            AS devices,
  (SELECT count(*) FROM seed_tpl_analytics)                                 AS tpl_analytics,
  (SELECT count(*) FROM seed_tpl_error)                                     AS tpl_error;
