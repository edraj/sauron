-- 0015: gate viewing de-obfuscated SOURCE CODE (symbolication context lines)
-- behind a dedicated `source:read` permission. Symbol names / file / line stay
-- visible with `issue:read`; this only controls the embedded source lines.
--
-- Seeds the permission onto existing preset roles so a freshly-migrated DB is
-- correct immediately (ensure_preset_roles also re-syncs at api startup).
UPDATE roles SET permissions = permissions || '["source:read"]'::jsonb
WHERE name IN ('Owner', 'Admin', 'Developer')
  AND NOT (permissions @> '["source:read"]'::jsonb);
