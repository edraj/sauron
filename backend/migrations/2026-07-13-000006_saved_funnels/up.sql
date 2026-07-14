-- 0006: saved_funnels — persisted, app-scoped funnel definitions (a shared team
-- library). A definition is an ordered array of event-name strings; conversion is
-- still computed on read via POST /funnel. `created_by` is display-only.
CREATE TABLE saved_funnels (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id      UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT,
    steps       JSONB NOT NULL,
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX saved_funnels_app_updated_idx ON saved_funnels (app_id, updated_at DESC);
