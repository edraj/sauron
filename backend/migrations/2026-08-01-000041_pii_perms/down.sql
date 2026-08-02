-- Leaving pii:* on a custom role after the code is reverted makes that role
-- permanently ungrantable: the grant path requires the caller to hold every
-- permission in the role, and nobody can hold one that is no longer in
-- perm::ALL. Presets re-sync from code at boot; custom roles do not.
UPDATE roles
SET permissions = permissions - 'pii:read' - 'pii:manage'
WHERE jsonb_typeof(permissions) = 'array'
  AND permissions ?| array['pii:read','pii:manage'];
