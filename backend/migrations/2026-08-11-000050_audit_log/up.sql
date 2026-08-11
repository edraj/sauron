-- The Wall of Shame: a general trail of administrative actions.
--
-- Until now this schema audited exactly two things — who revealed PII
-- (inspector_reveal_audit) and who masked it (inspector_mask_actions). Every
-- other administrative mutation left no trace, so "who deleted that app"
-- had no answer. This table is the general case.
--
-- Deliberately NOT a product-activity feed. Issue triage and saved views are
-- excluded: they are high-volume and would bury the security-relevant events
-- (member creation, password resets, role widening, key rotation) that are
-- the reason to keep a trail at all.
--
-- Rows are written fail-open by the API: a failure to audit logs at error
-- level and lets the action proceed, because an audit-table problem must not
-- take down member management. That is the opposite of the choice
-- inspector_reveal_audit makes, and deliberately so — there, the audit row IS
-- the authorization to emit PII; here, refusing to create a project because a
-- log insert failed would be a self-inflicted outage.
--
-- On upgrade this migration runs by itself, and the mechanism is worth knowing
-- because older comments in this tree (and an earlier draft of this one) claim
-- the opposite. `sauron-migrate.service` indeed has no [Install] section and is
-- deliberately absent from %postun's restart list — but all six daemon units
-- carry `Requires=sauron-migrate.service` with `After=`, and the migrator has
-- no `RemainAfterExit`, so it re-runs ahead of every daemon start. %postun's
-- `%systemd_postun_with_restart` restarts the daemons, which pulls it in.
-- Verified end to end in packaging/rpm/. Do not add a manual
-- "run sauron-migrate after upgrading" step on the strength of the old note.

CREATE TABLE audit_log (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The partition: each org has its own history, and the read endpoint is
    -- gated on org-scoped org:manage. CASCADE because once the tenant is gone
    -- there is nobody left who could hold that grant to read the trail.
    org_id           UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,

    -- NO foreign key, deliberately. See the note on the filter axes below:
    -- every id on this table is an inert snapshot, not a live reference.
    actor_id         UUID,
    -- Without this, deleting a user would leave their entries anonymous —
    -- precisely the moment the trail matters most.
    actor_email      TEXT NOT NULL DEFAULT '',

    -- 'entity.verb', e.g. 'environment.create'. The authoritative list lives
    -- in sauron-api/src/audit.rs; kept as TEXT rather than an enum so adding
    -- an action is a code change, not a migration.
    action           TEXT NOT NULL,
    entity_type      TEXT NOT NULL,
    -- Nullable: the entity may since have been deleted, and on a delete action
    -- it is gone by the time anyone reads this.
    entity_id        UUID,
    entity_name      TEXT NOT NULL DEFAULT '',

    -- Filter axes. Names denormalized alongside the ids so filtering never
    -- joins and so a row stays readable after its project or app is deleted.
    --
    -- NONE of these carry a foreign key, and that is the whole point.
    --
    -- `REFERENCES projects(id) ON DELETE SET NULL` would be the obvious
    -- choice and is wrong twice over. First, deleting a project would blank
    -- project_id on every historical entry, so "show me everything that
    -- happened in that project" — the question you ask precisely BECAUSE it
    -- was deleted — silently returns nothing. Second, the delete handler
    -- could not record its own event: the referenced row is already gone by
    -- the time the entry is written, so the insert would fail the FK and the
    -- deletion would go unrecorded through the fail-open path.
    --
    -- An audit row is an immutable statement about the past. Referential
    -- integrity against mutable business tables is the wrong model for it.
    project_id       UUID,
    project_name     TEXT NOT NULL DEFAULT '',
    app_id           UUID,
    app_name         TEXT NOT NULL DEFAULT '',
    -- Additionally ambiguous by nature: an entry may name either a catalogue
    -- environment (environments) or one app's enrollment (app_environments),
    -- so no single FK could describe it even if one were wanted.
    environment_id   UUID,
    environment_name TEXT NOT NULL DEFAULT '',

    -- {field: {from, to}} for the fields that actually changed. Built from a
    -- per-entity ALLOWLIST in audit.rs, never by serializing the entity — the
    -- allowlist is what guarantees an ingest key, a channel secret or a
    -- password hash can never reach a table that org admins read and that is
    -- kept forever. A test pins the forbidden field names.
    changes          JSONB NOT NULL DEFAULT '{}'::jsonb,

    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The default view and every filtered view page through this. `id` is the
-- keyset tiebreaker, not decoration: entries written in one transaction share
-- a created_at to microsecond precision, and an untiebroken cursor would skip
-- or repeat one of them at the page boundary.
CREATE INDEX audit_log_org_time_idx ON audit_log (org_id, created_at DESC, id DESC);

-- One partial index per filter axis. Partial on the nullable axes so entries
-- with no project/app (org-level actions such as role edits) stay out of them.
CREATE INDEX audit_log_org_project_idx ON audit_log (org_id, project_id, created_at DESC)
    WHERE project_id IS NOT NULL;
CREATE INDEX audit_log_org_app_idx ON audit_log (org_id, app_id, created_at DESC)
    WHERE app_id IS NOT NULL;
CREATE INDEX audit_log_org_env_idx ON audit_log (org_id, environment_id, created_at DESC)
    WHERE environment_id IS NOT NULL;
CREATE INDEX audit_log_org_actor_idx ON audit_log (org_id, actor_id, created_at DESC);
CREATE INDEX audit_log_org_action_idx ON audit_log (org_id, action, created_at DESC);
CREATE INDEX audit_log_org_entity_idx ON audit_log (org_id, entity_type, created_at DESC);
