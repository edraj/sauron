-- 0014: source-map / symbol artifact storage (content-addressed) + symbolication
-- columns on error_events. Symbolication is presentational; grouping is unaffected.

CREATE TABLE symbol_blobs (
    sha256            BYTEA PRIMARY KEY,
    content           BYTEA NOT NULL,          -- zstd-compressed artifact bytes
    uncompressed_size BIGINT NOT NULL,
    compressed_size   BIGINT NOT NULL,
    refcount          INTEGER NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- content is pre-compressed with zstd; keep it out-of-line and don't let TOAST
-- waste CPU trying to re-compress it.
ALTER TABLE symbol_blobs ALTER COLUMN content SET STORAGE EXTERNAL;

CREATE TABLE symbol_artifacts (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id                UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    kind                  TEXT NOT NULL,        -- 'js_sourcemap' | 'dart_symbols'
    platform              TEXT NOT NULL,        -- 'web' | 'android' | 'ios'
    arch                  TEXT,                 -- dart: 'arm64' | 'armeabi-v7a' | 'x86_64'
    release               TEXT,                 -- js release+path matching
    dist                  TEXT,
    name                  TEXT,                 -- js: minified file path/URL the map applies to
    debug_id              TEXT,                 -- dart build-id / uuid
    blob_sha256           BYTEA NOT NULL REFERENCES symbol_blobs(sha256),
    prebuilt_index_sha256 BYTEA REFERENCES symbol_blobs(sha256),  -- js parse-on-upload index
    uploaded_by           UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX symbol_artifacts_debugid_idx
    ON symbol_artifacts (app_id, debug_id) WHERE debug_id IS NOT NULL;
CREATE INDEX symbol_artifacts_release_name_idx
    ON symbol_artifacts (app_id, release, name);

-- Symbolication columns on the partitioned error_events parent (propagate to
-- every partition). Raw `stacktrace` stays the source of truth.
ALTER TABLE error_events ADD COLUMN stacktrace_symbolicated JSONB;
ALTER TABLE error_events ADD COLUMN symbolication_status TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE error_events ADD COLUMN debug_meta JSONB;

-- Seed the new permission onto existing preset roles so a freshly-migrated DB is
-- correct immediately (ensure_preset_roles also re-syncs at api startup).
UPDATE roles SET permissions = permissions || '["artifact:write"]'::jsonb
WHERE name IN ('Owner', 'Admin', 'Developer')
  AND NOT (permissions @> '["artifact:write"]'::jsonb);
