-- Which environment represents the build that ships to the app stores.
--
-- The stores key their data to a package name / bundle id and have no idea
-- environments exist, so this is a VISIBILITY choice, not a data partition:
-- store_daily_metrics below is deliberately NOT environment-scoped.
--
-- References the per-app ENROLLMENT (app_environments), not the project
-- catalogue (environments), because the enrollment id is what the dashboard's
-- switcher and `?environment_id=` already carry — comparing the designation
-- against the selected environment is a plain `=` only if both are the same
-- kind of id.
--
-- SET NULL, not CASCADE: retiring an environment should hide the Overview
-- section, not delete the app.
ALTER TABLE apps
  ADD COLUMN store_environment_id UUID REFERENCES app_environments(id) ON DELETE SET NULL;

CREATE TABLE app_store_connections (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  store          TEXT NOT NULL CHECK (store IN ('google_play', 'app_store')),
  enabled        BOOLEAN NOT NULL DEFAULT true,
  -- Non-secret, displayable identifiers. JSONB rather than seven columns that
  -- would be half NULL on every row, because the two stores need disjoint
  -- field sets:
  --   google_play: {package_name, gcs_bucket}
  --   app_store:   {bundle_id, apple_app_id, issuer_id, key_id, vendor_number}
  identifiers    JSONB NOT NULL DEFAULT '{}'::jsonb,
  -- AES-256-GCM (sauron_alerts::SecretCipher, NOTIFY_SECRET_KEY).
  -- Play: the service-account JSON. Apple: the .p8 private key.
  secret_enc     BYTEA,
  -- Apple's analyticsReportRequests id lives here: created once, reused for
  -- the life of the connection.
  sync_state     JSONB NOT NULL DEFAULT '{}'::jsonb,
  next_sync_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_synced_at TIMESTAMPTZ,
  last_error     TEXT,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (app_id, store)
);

-- The daemon's claim query orders by next_sync_at over enabled rows only.
CREATE INDEX app_store_connections_due_idx
  ON app_store_connections (next_sync_at) WHERE enabled;

CREATE TABLE store_daily_metrics (
  app_id     UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  store      TEXT NOT NULL CHECK (store IN ('google_play', 'app_store')),
  day        DATE NOT NULL,
  installs   BIGINT NOT NULL DEFAULT 0,
  uninstalls BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  -- Writers MUST use ON CONFLICT DO UPDATE SET (not +=). Both stores restate
  -- recent days as their pipelines settle; an additive upsert inflates every
  -- number on every sync and produces a chart that still looks plausible.
  PRIMARY KEY (app_id, store, day)
);

-- The chart feed reads one app's range across both stores.
CREATE INDEX store_daily_metrics_app_day_idx ON store_daily_metrics (app_id, day);
