-- Terminal storage for ingest failures, and the recovery path for them.
--
-- Until now a job that failed in the worker was dead-lettered on its FIRST
-- attempt into `sauron:ingest:dlq`, a Redis stream that nothing reads. There is
-- no replay path, no admin UI and no CLI; the only consumer is the Prometheus
-- gauge `sauron_ingest_dlq_length`. Failures were countable but not
-- recoverable, and a two-second Postgres hiccup permanently lost every event in
-- flight.
--
-- These two tables are where a failure lands once retrying is finished with it.
-- The Redis DLQ survives with a narrower job: the backstop for failures we
-- could not even record here, which is precisely what a Postgres outage looks
-- like from the worker. Without that fallback, "Postgres is down" would become
-- silent event loss.
--
-- Parent/child rather than one flat table because 242,700 identical failures
-- must read as one row a human can act on, while still retaining the individual
-- payloads — grouping alone would reduce "retry" to replaying a single sample,
-- which verifies a fix but recovers nothing.
--
-- Upgrade hazard, same as every other migration in this tree:
-- sauron-migrate.service has no [Install] section and is not in %postun's
-- restart list, so `dnf upgrade` leaves new binaries running against the old
-- schema. Until `systemctl start sauron-migrate` runs, the worker's
-- record_failure falls through to the Redis DLQ backstop (harmlessly, by
-- design) and the admin page 500s on read.

CREATE TABLE ingest_failures (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- sha256(error_kind ‖ normalize(error_message) ‖ app_id). `normalize`
    -- strips UUIDs, standalone integers, quoted literals and byte offsets
    -- before hashing, so `row 4821` and `row 9` collapse into one group.
    -- Without that normalization the grouping this table exists for buys
    -- nothing and you get one row per occurrence anyway.
    fingerprint       TEXT NOT NULL UNIQUE,

    -- A short stable slug (`decode`, `db_fk_violation`, `db_deadlock`,
    -- `symbolication`, `unknown`), NOT the raw message. It is also the metrics
    -- label, so it must stay low-cardinality.
    error_kind        TEXT NOT NULL,
    -- The most recent raw message, kept for the human reading the page. High
    -- cardinality by nature, which is exactly why it is not the group key.
    error_message     TEXT NOT NULL,

    -- NO foreign keys, deliberately, for the same reason audit_log has none:
    -- these are inert snapshots, not live references. A failure row must
    -- survive the deletion of the app it came from — that is often the moment
    -- someone finally looks at it. They are additionally NULLABLE because the
    -- dominant failure mode is a payload that never decoded, so there is no
    -- app_id to record.
    org_id            UUID,
    project_id        UUID,
    app_id            UUID,

    -- Everything ever seen for this fingerprint, including occurrences the
    -- per-group payload cap refused to store.
    --
    -- This is the ONLY counter. "Retained" is COUNT(children) and "dropped" is
    -- `occurrences - retained`, both computed on read, and that is deliberate:
    -- denormalized retained/dropped columns would have to be bumped in the same
    -- statement as this upsert, and Postgres forbids a single statement from
    -- updating one row twice — the second CTE's UPDATE would silently not
    -- apply, leaving counters that drift from reality while every test passes.
    -- Deriving them cannot drift at all.
    occurrences       BIGINT NOT NULL DEFAULT 1,

    -- 'failed' | 'requeued' | 'resolved'. TEXT rather than an enum so adding a
    -- state is a code change, not a migration.
    status            TEXT NOT NULL DEFAULT 'failed',

    first_seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The default view and every filtered view page through this. `id` is the
-- keyset tiebreaker, not decoration: a burst of failures recorded in one
-- transaction shares a last_seen_at to microsecond precision, and an
-- untiebroken cursor silently skips or repeats one of them at the page
-- boundary.
CREATE INDEX ingest_failures_status_time_idx
    ON ingest_failures (status, last_seen_at DESC, id DESC);

-- The unfiltered listing, same tiebreaker rule.
CREATE INDEX ingest_failures_time_idx
    ON ingest_failures (last_seen_at DESC, id DESC);

-- Partial: rows whose payload never decoded have no app_id and would otherwise
-- bloat this index with NULLs it can never be used to find.
CREATE INDEX ingest_failures_app_idx
    ON ingest_failures (app_id, last_seen_at DESC)
    WHERE app_id IS NOT NULL;

-- The retention reaper's access path (see INGEST_FAILURE_RETENTION_DAYS).
CREATE INDEX ingest_failures_kind_idx
    ON ingest_failures (error_kind, last_seen_at DESC);


CREATE TABLE ingest_failure_payloads (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- CASCADE is the whole retention story for children: the reaper and the
    -- admin's Drop both delete the parent only, and the payloads follow. A
    -- child outliving its group would be an orphaned copy of a real user event
    -- that no page can show and no reaper can find.
    failure_id   UUID NOT NULL REFERENCES ingest_failures(id) ON DELETE CASCADE,

    -- ALREADY PII-MASKED. `mask::apply_wire` runs in the worker before anything
    -- is persisted or re-queued, so this column holds the masked wire payload,
    -- never the raw one. It is still a copy of a real user event, which is why
    -- the retention reaper exists at all.
    payload      JSONB NOT NULL,

    -- How many retries were burned before this landed here. 0 for a permanent
    -- failure, which is never retried by design: retrying malformed JSON three
    -- times a minute apart costs three minutes to reach a guaranteed-identical
    -- result.
    attempts     INTEGER NOT NULL DEFAULT 0,

    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Set when re-injected onto the ingest stream, cleared if that attempt
    -- fails. This is what closes the manual-retry loop: without it the admin
    -- presses Retry, the row sits there, and they never learn the outcome.
    requeued_at  TIMESTAMPTZ
);

CREATE INDEX ingest_failure_payloads_failure_idx
    ON ingest_failure_payloads (failure_id, created_at);
