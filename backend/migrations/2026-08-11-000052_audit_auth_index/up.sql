-- Keeps the Wall of Shame's DEFAULT view fast once sign-in activity starts
-- landing in the same table.
--
-- Auth events (`entity_type = 'auth'`) are a separate opt-in stream: the feed
-- hides them unless a caller explicitly asks. That exclusion is a NEGATION —
-- `entity_type <> 'auth'` — which `audit_log_org_time_idx` cannot serve
-- selectively. On a deployment where sign-ins outnumber administrative actions
-- by orders of magnitude (the normal case), the default query would walk
-- through mostly-auth rows to find the handful of admin ones, and the page
-- would get slower the longer the deployment ran.
--
-- A partial index whose predicate is exactly the default filter fixes that: it
-- physically contains only the rows the default view can return, so its size
-- tracks administrative volume rather than login volume.
--
-- The predicate must stay character-identical to the SQL in
-- `repo::list_audit_feed` and to `audit::entity::AUTH`. Postgres only uses a
-- partial index when it can prove the query predicate implies the index
-- predicate; a cosmetic difference silently drops it back to a full scan, with
-- no error and no signal other than latency.
CREATE INDEX audit_log_org_time_admin_idx
    ON audit_log (org_id, created_at DESC, id DESC)
    WHERE entity_type <> 'auth';

-- The same exclusion applies when filtering by actor, which is the one axis a
-- reader is most likely to combine with "and hide the logins" — "what did this
-- person DO", as distinct from "when were they here".
CREATE INDEX audit_log_org_actor_admin_idx
    ON audit_log (org_id, actor_id, created_at DESC)
    WHERE entity_type <> 'auth';

-- And the mirror image, for when auth events ARE asked for: the stream is
-- read on its own, so it gets its own index rather than sharing the general
-- entity_type one with fifteen other families.
CREATE INDEX audit_log_org_auth_idx
    ON audit_log (org_id, created_at DESC, id DESC)
    WHERE entity_type = 'auth';
