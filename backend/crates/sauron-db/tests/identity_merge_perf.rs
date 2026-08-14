//! Guards the property that justified approach B over a pure read-time
//! overlay: migrations `2026-07-28-000028`, `2026-07-29-000031`,
//! `2026-08-01-000039` and `2026-08-01-000040` widened
//! `analytics_events`/`error_events`' covering indexes specifically so
//! `count(DISTINCT distinct_id)` (and the sibling per-issue aggregate) run as
//! an INDEX ONLY SCAN. Adding `guest_alias` to both tables (migration
//! `2026-08-12-000058`) must not have cost either covering index its ability
//! to serve that scan — if it did, active-users/issue-list reads go from
//! index-only back to the heap-scan-every-partition shape that already
//! produced a real 152 ms → 28.96 s / 30 s-`TimeoutLayer` 503 on this
//! codebase. See "Why this task exists" in
//! `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`.
//!
//! ## Correcting the originating brief's sample query
//!
//! The brief's own Step 1 sketch filtered on `environment_id IS NOT NULL`.
//! No `EnvFilter` variant this codebase ever emits produces that predicate —
//! `sauron_db::scope::EnvFilter::sql_fragment` shows `All` emits nothing,
//! `One` emits `= $n`, `Subset` emits `= ANY($n)`, and only `Unattributed`
//! emits an `IS NULL`/`IS NOT NULL`-shaped test, and it emits `IS NULL`, the
//! opposite predicate. `IS NOT NULL` is not a shape `sauron-api` ever issues,
//! so a guard built on it would not correspond to anything in production —
//! passing or failing would prove nothing. Every test below instead uses the
//! REAL predicate shape the migrations were built to serve — `EnvFilter::One`,
//! `app_id = $1 AND environment_id = $2 AND occurred_at >= $3` — taken
//! verbatim from the call sites that motivated them: `repo::user_stats` /
//! `repo::active_user_series` (both legs of their `UNION ALL`: analytics via
//! migration 0039, error_events via migration 0040) and `repo::list_issues`'s
//! `agg` LATERAL (migrations 0028 → 0031).
//!
//! ## Why the competing indexes are dropped before `EXPLAIN`, not just seeded
//! ## and `enable_seqscan = off`
//!
//! This was measured, and the first measurement was wrong in an instructive
//! way. With `SEEDED_ROWS` real rows, `ANALYZE`, and `enable_seqscan = off` —
//! exactly the brief's own recipe — the query was NOT stable: one run planned
//! `analytics_events_default_app_id_distinct_id_occurred_at_idx1` (migration
//! 0020's unrelated `(app_id, distinct_id, occurred_at)` index) via a plain,
//! heap-touching `Index Scan`, and a second run against the identical rows —
//! after nothing but a fresh `ANALYZE` reshuffling its sample — planned
//! `analytics_events_app_distinct_env_idx` (migration 0055's `(app_id,
//! distinct_id, environment_id, occurred_at)`) via `Index Only Scan`. Both are
//! real, legitimate indexes that happen to also satisfy this exact query
//! shape; migration 0039's own comment documents the identical phenomenon in
//! the other direction ("the planner never chooses
//! `analytics_events_app_env_time_idx`... reaches for `..._app_id_device_key_idx`
//! instead"). Asserting on the plan while several valid covering indexes
//! compete is asserting on `ANALYZE`'s sampling noise, not on `guest_alias`.
//!
//! What this test actually needs to know is narrower than "which index wins
//! the race": does the ONE covering index each migration built still cover
//! this query as INDEX ONLY, on its own. So each test drops every other
//! secondary index on the table (via `pg_indexes`, not a hardcoded list — a
//! future migration's index should be excluded automatically, not silently
//! left in the race) before seeding, `VACUUM ANALYZE`, and `EXPLAIN`. This is
//! narrower than "what plan does production choose today" — with every index
//! present, Postgres may legitimately reach for a different, ALSO
//! index-only-capable index instead (as measured above, that would not be a
//! regression). It is exactly as wide as the property migrations 0028/0031/
//! 0039/0040 exist to guarantee: that THEIR covering index's `INCLUDE`/key
//! list still carries everything this query needs.
//!
//! Isolating the index was not sufficient by itself, either — same
//! measure-first discovery, second round. With only the target index and the
//! primary key left, `enable_seqscan = off` alone STILL flip-flopped between
//! runs, sometimes choosing a `Bitmap Heap Scan` on the bare PK over an
//! `Index Only Scan` on the covering index, purely on which way `ANALYZE`'s
//! sample happened to land. The mechanism: a `Bitmap Heap Scan` is never
//! index-only regardless of any index's `INCLUDE` payload — the bitmap
//! machinery hands back TIDs, not index tuple data, so it always visits the
//! heap — and Postgres's cost model does not treat "never has to visit the
//! heap" as an automatic win over "narrower index, cheap heap fetches" at
//! these row counts. `enable_bitmapscan = off` alongside `enable_seqscan =
//! off` removes that whole race: with both off the only remaining candidates
//! are plain `Index (Only) Scan`s, where the covering index's zero heap
//! fetches beats the PK's one-per-row deterministically, confirmed stable
//! across repeated `ANALYZE`/re-run cycles.
//!
//! ## `Index Only Scan` in the plan is necessary, not sufficient — this was a
//! ## real gap in an earlier version of this file, caught in review
//!
//! An `Index Only Scan` NODE does not mean zero heap fetches. Postgres falls
//! back to a per-row heap visit, silently, whenever a page's all-visible bit
//! is unset — which `rewrite_hot_rows`' own `UPDATE ... SET distinct_id =
//! $3, guest_alias = $2` does on every page it touches, and a merged guest's
//! rows are scattered across the partition (they accumulated over real time,
//! interleaved with everyone else's), so a handful of merged rows can clear
//! the bit on a large fraction of the partition's pages. Plain `EXPLAIN` has
//! no `Heap Fetches` line at all — it describes the chosen PLAN, not what
//! running it actually touched — so a version of this file that only checked
//! for the `Index Only Scan` string would have stayed green through exactly
//! the regression this whole feature is about. Every test below uses
//! `EXPLAIN (ANALYZE, BUFFERS)` and asserts `Heap Fetches: 0` specifically,
//! after an explicit `VACUUM` to give the assertion a real baseline (without
//! it, freshly-inserted pages are not yet marked all-visible either, and the
//! pre-merge case would show nonzero heap fetches for a reason that has
//! nothing to do with `guest_alias`).
//!
//! ## A real merge DID reintroduce heap fetches — mitigated by migration
//! ## `2026-08-13-000060`, not by this file
//!
//! A prior version of [`merging_a_guest_reintroduces_heap_fetches`] ran the
//! real `rewrite_hot_rows` and asserted `Heap Fetches: 0` afterward, and that
//! assertion FAILED: `0` before the merge, `1974` after (37 of 5,000 rows
//! touched, ≈0.74%), still `1937` on a re-run — an identical, still-green
//! `Index Only Scan` node, 30–48× the buffer traffic, invisible to plain
//! `EXPLAIN` and to a plan-shape assertion. It was left failing on purpose —
//! a documented, tracked regression rather than a suite that quietly did not
//! cover its own headline scenario — until a mitigation shipped.
//!
//! Migration `2026-08-13-000060` is that mitigation: it tunes
//! `(autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold =
//! 20)` on every leaf partition of both tables (existing ones via the
//! migration itself, future ones via `repo::create_range_partition` carrying
//! the setting forward — see both files' own doc comments for the full
//! reasoning and the measurement that the DEFAULT threshold, `50 + 0.2 ×
//! reltuples`, never reaches the "tens of dead tuples" one guest's merge
//! produces, at any partition size). [`merging_a_guest_is_repaired_by_a_vacuum`]
//! now proves the REPAIR side of that fix — see its own doc comment for why
//! it does not simply sleep and wait for autovacuum to fire on its own, and
//! [`event_table_partitions_are_tuned_for_frequent_autovacuum`] proves the
//! CONFIGURATION side (the setting is actually in place, on both existing
//! and newly-created partitions). Together they are the honest version of
//! "does this migration actually fix the bug" that a single sleep-based test
//! would not reliably be.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;

#[derive(QueryableByName)]
struct Plan {
    #[diesel(sql_type = Text)]
    #[diesel(column_name = "QUERY PLAN")]
    line: String,
}

/// Rows to seed before asking the planner anything for real. Matches
/// `keyset_plan.rs`'s `SEEDED_ROWS` — past the point a planner should ever
/// prefer a sequential scan, with room to spare.
const SEEDED_ROWS: i64 = 5_000;

async fn explain_analyze_buffers(conn: &mut sauron_db::PgConn, sql: &str) -> String {
    let plan: Vec<Plan> = diesel::sql_query(format!("EXPLAIN (ANALYZE, BUFFERS) {sql}"))
        .load(conn)
        .await
        .expect("explain analyze");
    plan.iter()
        .map(|p| p.line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pulls the `Heap Fetches: N` line `EXPLAIN (ANALYZE, BUFFERS)` prints under
/// an `Index Only Scan` node. `None` if the plan has no such node at all
/// (e.g. it degraded to a plain `Index Scan`) — callers that need to
/// distinguish "not index-only" from "index-only with fetches" check both
/// this and the `"Index Only Scan"` substring, not this alone.
fn heap_fetches(text: &str) -> Option<i64> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Heap Fetches: ")
            .and_then(|rest| rest.trim().parse().ok())
    })
}

/// Drop every secondary index on `table` except `keep` (and the primary
/// key), so the plan below can only answer "does `keep` itself still serve
/// this query index-only", not "which of several valid indexes wins today".
/// Scoped to the parent (partitioned) table name — `DROP INDEX` on a
/// partitioned parent applies synchronously to every child, the same
/// mechanism migrations 0028/0031/0039/0040/0055 all rely on to build theirs.
/// Enumerated from `pg_indexes` rather than hardcoded: a future migration's
/// index must join this exclusion automatically, not be silently left
/// competing because this list forgot it.
async fn isolate_index(conn: &mut sauron_db::PgConn, table: &str, keep: &str) {
    diesel::sql_query(format!(
        "DO $$ \
         DECLARE r RECORD; \
         BEGIN \
           FOR r IN SELECT indexname FROM pg_indexes \
                     WHERE tablename = '{table}' \
                       AND indexname <> '{keep}' \
                       AND indexname NOT LIKE '%pkey%' \
           LOOP \
             EXECUTE format('DROP INDEX %I', r.indexname); \
           END LOOP; \
         END $$;"
    ))
    .execute(conn)
    .await
    .expect("isolate target index");
}

/// `VACUUM` until the partition's visibility map is actually set, or fail
/// saying so.
///
/// A single `VACUUM` is not enough here, and the reason is a property of the
/// harness rather than of anything under test. `VACUUM` can only mark a page
/// all-visible if every tuple on it is older than the cluster's
/// oldest-non-removable xid, and `TestDb::setup()`'s migration connection
/// (`run_pending_migrations` → `AsyncConnectionWrapper` inside
/// `spawn_blocking`) does not disappear the instant that call returns: for a
/// short window afterwards its backend is still there, `idle in transaction`,
/// pinning the horizon. These tests seed and vacuum within a few milliseconds
/// of setup, i.e. inside that window, so the `VACUUM` succeeds and marks
/// nothing.
///
/// Measured on an idle server, this test as the only client: consecutive runs
/// left `relallvisible` at 0 of 97 pages, then 53 of 97, then 97 of 97, so
/// `Heap Fetches` read 5005, then 2273, then 0 — a coin flip that has nothing
/// to do with `guest_alias`. Four `VACUUM`s back to back all read 0 of 97
/// (they all land inside the window); one `VACUUM` a second later reads 97 of
/// 97, as does a `psql` `VACUUM` against the same database once the test
/// process has exited. That is what proves the covering index was never the
/// problem.
///
/// Retrying rather than sleeping a fixed amount keeps the wait proportional to
/// the actual cause and also covers the other horizon holders CI can produce —
/// notably an autovacuum worker on the same partition, which migration
/// `2026-08-13-000060` deliberately makes likely by tuning these partitions to
/// `(scale_factor = 0.0, threshold = 20)`, well below `SEEDED_ROWS`. If the map
/// never converges this panics with the page counts, so a future horizon holder
/// shows up as itself instead of as an inexplicable `Heap Fetches` number.
/// `vacuum_sql` is the exact command to repeat — callers pass `VACUUM ANALYZE
/// <table>` when they also need fresh planner statistics (after a bulk seed),
/// and a plain `VACUUM <partition>` when they must not disturb the statistics
/// the assertion under test depends on.
async fn vacuum_until_all_visible(conn: &mut sauron_db::PgConn, vacuum_sql: &str, partition: &str) {
    #[derive(QueryableByName)]
    struct VisMap {
        #[diesel(sql_type = BigInt)]
        all_visible: i64,
        #[diesel(sql_type = BigInt)]
        pages: i64,
    }

    let mut last = (0i64, 0i64);
    for _ in 0..100 {
        diesel::sql_query(vacuum_sql)
            .execute(conn)
            .await
            .expect("vacuum");

        let rows: Vec<VisMap> = diesel::sql_query(format!(
            "SELECT relallvisible::bigint AS all_visible, relpages::bigint AS pages \
             FROM pg_class WHERE relname = '{partition}'"
        ))
        .load(conn)
        .await
        .expect("read visibility map counters");
        // `rows.first()` would resolve to diesel's `FirstDsl`, not the slice
        // method, and fail to compile against `Vec<VisMap>`.
        let row = rows
            .into_iter()
            .next()
            .expect("partition exists in pg_class");
        last = (row.all_visible, row.pages);
        // `relpages = 0` means ANALYZE has not sized it yet, which is not the
        // same as "fully visible" — keep going rather than pass vacuously.
        if row.pages > 0 && row.all_visible >= row.pages {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!(
        "{partition}: visibility map never converged — {} of {} pages all-visible after 100 \
         VACUUMs. Something is holding the oldest-non-removable xid back for the whole run; \
         every `Heap Fetches: 0` assertion below is unmeasurable until that is fixed.",
        last.0, last.1
    );
}

/// Bulk-seed `analytics_events` for one app+environment, spread across 137
/// distinct_ids and the last ~8 hours (well inside any `since` window a test
/// below uses, and inside whatever partition already covers "now").
async fn seed_analytics_events(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    env_id: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    rows: i64,
) {
    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, environment_id, name, distinct_id, occurred_at, received_at) \
         SELECT gen_random_uuid(), $1, $2, 'perf_guard_probe', 'guest-' || (g % 137), \
                $3 - ((g % 500) || ' minutes')::interval, now() \
         FROM generate_series(1, $4::bigint) g",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(env_id)
    .bind::<Timestamptz, _>(now)
    .bind::<BigInt, _>(rows)
    .execute(conn)
    .await
    .expect("seed analytics_events");
}

/// Bulk-seed `error_events` for one app+environment+issue, same spread as
/// [`seed_analytics_events`]. `issue_id` is required by the schema
/// (`error_events.issue_id` is `NOT NULL` with an FK to `issues`) even for
/// tests that never read it — reuses whatever real issue `seed_two_envs`
/// already created.
async fn seed_error_events(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    env_id: uuid::Uuid,
    issue_id: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    rows: i64,
) {
    diesel::sql_query(
        "INSERT INTO error_events \
           (id, app_id, environment_id, issue_id, fingerprint, distinct_id, occurred_at, received_at) \
         SELECT gen_random_uuid(), $1, $2, $3, 'perf-guard-fp', 'guest-' || (g % 137), \
                $4 - ((g % 500) || ' minutes')::interval, now() \
         FROM generate_series(1, $5::bigint) g",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(env_id)
    .bind::<SqlUuid, _>(issue_id)
    .bind::<Timestamptz, _>(now)
    .bind::<BigInt, _>(rows)
    .execute(conn)
    .await
    .expect("seed error_events");
}

/// Migration 0039 exists to answer exactly this aggregate without touching
/// the heap. If `guest_alias` ever pushed
/// `analytics_events_app_env_time_users_idx` off an index-only scan for this
/// shape, active-users goes from an index-only scan to a heap scan across
/// every retained partition — the shape that already produced a real 30 s
/// `TimeoutLayer` 503 on this codebase.
#[tokio::test]
async fn active_users_still_uses_an_index_only_scan() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    isolate_index(
        &mut conn,
        "analytics_events",
        "analytics_events_app_env_time_users_idx",
    )
    .await;

    let now = Utc::now();
    seed_analytics_events(&mut conn, ids.app_id, ids.env_a, now, SEEDED_ROWS).await;

    // `VACUUM`, not just `ANALYZE` — see the module doc comment's "necessary,
    // not sufficient" section. Without it, freshly-inserted heap pages are
    // not yet marked all-visible, `Heap Fetches` would read nonzero for a
    // reason that has nothing to do with `guest_alias`, and this assertion
    // would be meaningless either way it came out.
    vacuum_until_all_visible(
        &mut conn,
        "VACUUM ANALYZE analytics_events",
        "analytics_events_default",
    )
    .await;
    diesel::sql_query("SET enable_seqscan = off")
        .execute(&mut conn)
        .await
        .expect("disable seqscan");
    diesel::sql_query("SET enable_bitmapscan = off")
        .execute(&mut conn)
        .await
        .expect("disable bitmapscan");

    // The real `EnvFilter::One` shape from `repo::user_stats`/
    // `repo::active_user_series`'s dau/wau/mau subqueries: app + env
    // equality, a time floor, `distinct_id` non-empty.
    let text = explain_analyze_buffers(
        &mut conn,
        &format!(
            "SELECT count(DISTINCT distinct_id) FROM analytics_events \
              WHERE app_id = '{}' AND environment_id = '{}' AND occurred_at >= '{}' \
                AND distinct_id IS NOT NULL AND distinct_id <> ''",
            ids.app_id,
            ids.env_a,
            (now - Duration::days(7)).to_rfc3339(),
        ),
    )
    .await;

    assert!(
        text.contains("Index Only Scan"),
        "active-users must stay index-only after guest_alias; plan was:\n{text}"
    );
    // The plan names the CHILD partition's index, whose name Postgres derives
    // from its own columns, not the parent migration 0039 gave
    // (`analytics_events_app_env_time_users_idx`) — so this checks the column
    // signature rather than the parent's name.
    assert!(
        text.contains("app_id_environment_id_occurred_at"),
        "expected migration 0039's covering index specifically; plan was:\n{text}"
    );
    assert_eq!(
        heap_fetches(&text),
        Some(0),
        "the plan node is index-only but still visited the heap — the property \
         migration 0039 exists to guarantee is broken; plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}

/// The `error_events` sibling of the test above: migration 0040 widened
/// `error_events_app_env_time_users_idx` the same way and for the same
/// reason — `repo::user_stats`/`repo::active_user_series` both run this
/// exact shape as the SECOND leg of a `UNION ALL` over analytics_events AND
/// error_events. Testing only the analytics leg (as an earlier version of
/// this file did) leaves half the production query path — the half that
/// migration 0040 exists for — completely unguarded.
#[tokio::test]
async fn active_users_error_leg_still_uses_an_index_only_scan() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    isolate_index(
        &mut conn,
        "error_events",
        "error_events_app_env_time_users_idx",
    )
    .await;

    let now = Utc::now();
    seed_error_events(
        &mut conn,
        ids.app_id,
        ids.env_a,
        ids.issue_id,
        now,
        SEEDED_ROWS,
    )
    .await;

    vacuum_until_all_visible(
        &mut conn,
        "VACUUM ANALYZE error_events",
        "error_events_default",
    )
    .await;
    diesel::sql_query("SET enable_seqscan = off")
        .execute(&mut conn)
        .await
        .expect("disable seqscan");
    diesel::sql_query("SET enable_bitmapscan = off")
        .execute(&mut conn)
        .await
        .expect("disable bitmapscan");

    let text = explain_analyze_buffers(
        &mut conn,
        &format!(
            "SELECT count(DISTINCT distinct_id) FROM error_events \
              WHERE app_id = '{}' AND environment_id = '{}' AND occurred_at >= '{}' \
                AND distinct_id IS NOT NULL AND distinct_id <> ''",
            ids.app_id,
            ids.env_a,
            (now - Duration::days(7)).to_rfc3339(),
        ),
    )
    .await;

    assert!(
        text.contains("Index Only Scan"),
        "active-users' error_events leg must stay index-only after guest_alias; plan was:\n{text}"
    );
    assert!(
        text.contains("app_id_environment_id_occurred_at"),
        "expected migration 0040's covering index specifically; plan was:\n{text}"
    );
    assert_eq!(
        heap_fetches(&text),
        Some(0),
        "the plan node is index-only but still visited the heap — the property \
         migration 0040 exists to guarantee is broken; plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}

/// Migrations 0028 → 0031 exist to answer `list_issues`/`top_issues`/
/// `get_issue`'s per-issue aggregate (`times_seen`, `users_seen`,
/// `first_seen`, `last_seen`) without touching the heap. If `guest_alias`
/// ever pushed `error_events_issue_env_time_idx` off an index-only scan for
/// this shape, every issue list/detail read in a single environment goes
/// from index-only back to a full scan of that issue's error history — the
/// exact regression migration 0028 closed and 0031 preserved while adding
/// the newest-row derivation.
#[tokio::test]
async fn issue_aggregate_still_uses_an_index_only_scan() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    isolate_index(&mut conn, "error_events", "error_events_issue_env_time_idx").await;

    let now = Utc::now();
    seed_error_events(
        &mut conn,
        ids.app_id,
        ids.env_a,
        ids.issue_id,
        now,
        SEEDED_ROWS,
    )
    .await;

    vacuum_until_all_visible(
        &mut conn,
        "VACUUM ANALYZE error_events",
        "error_events_default",
    )
    .await;
    diesel::sql_query("SET enable_seqscan = off")
        .execute(&mut conn)
        .await
        .expect("disable seqscan");
    diesel::sql_query("SET enable_bitmapscan = off")
        .execute(&mut conn)
        .await
        .expect("disable bitmapscan");

    // The real `EnvFilter::One` shape from `list_issues`'s `agg` LATERAL.
    let text = explain_analyze_buffers(
        &mut conn,
        &format!(
            "SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events \
             WHERE issue_id = '{}' AND environment_id = '{}' AND occurred_at >= '{}' \
             HAVING count(*) > 0",
            ids.issue_id,
            ids.env_a,
            (now - Duration::days(365)).to_rfc3339(),
        ),
    )
    .await;

    assert!(
        text.contains("Index Only Scan"),
        "the issue aggregate must stay index-only after guest_alias; plan was:\n{text}"
    );
    // Same child-vs-parent naming note as the analytics tests above: the plan
    // names `error_events_default_issue_id_environment_id_occurred_at_di_idx`
    // (derived from the child partition's own columns), not the parent
    // `error_events_issue_env_time_idx` migration 0031 named.
    assert!(
        text.contains("issue_id_environment_id_occurred_at"),
        "expected migration 0031's covering index specifically; plan was:\n{text}"
    );
    assert_eq!(
        heap_fetches(&text),
        Some(0),
        "the plan node is index-only but still visited the heap — the property \
         migrations 0028/0031 exist to guarantee is broken; plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}

/// Proves the CONFIGURATION half of migration `2026-08-13-000060`'s fix: the
/// tuned `(autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold
/// = 20)` is actually in place, both on partitions that existed when the
/// migration ran and on ones created afterward.
///
/// Two checks, not one, because they cover two different code paths that
/// could each independently fail to carry the setting:
/// 1. `analytics_events_default`/`error_events_default` — these exist from
///    migrations 0011/0012, long before 0060 runs in this same `run_pending_
///    migrations` call, so this checks the migration's own `pg_partition_tree`
///    sweep actually reached them (and did not, say, silently match zero
///    rows from a typo'd table name).
/// 2. A partition created fresh via `repo::create_range_partition` — this
///    checks the CODE path (not the migration) carries the setting forward,
///    which is the whole reason `create_range_partition` was changed at all:
///    a migration alone only fixes partitions that already existed on the
///    day it ran.
#[tokio::test]
async fn event_table_partitions_are_tuned_for_frequent_autovacuum() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    for table in ["analytics_events_default", "error_events_default"] {
        let opts = reloptions(&mut conn, table).await;
        assert!(
            opts.contains(&"autovacuum_vacuum_scale_factor=0.0".to_string()),
            "migration 2026-08-13-000060 did not tune {table}'s scale factor; \
             reloptions were: {opts:?}"
        );
        assert!(
            opts.contains(&"autovacuum_vacuum_threshold=20".to_string()),
            "migration 2026-08-13-000060 did not tune {table}'s threshold; \
             reloptions were: {opts:?}"
        );
    }

    // A NEW partition, created the way `sauron-tier` creates one — proves
    // `create_range_partition` (not just migration 0060) carries the setting.
    let start = Utc::now() + Duration::days(400);
    let end = start + Duration::days(1);
    sauron_db::repo::create_range_partition(
        &mut conn,
        "analytics_events",
        "perf_guard_future_partition",
        start,
        end,
    )
    .await
    .expect("create a future partition");
    let opts = reloptions(&mut conn, "analytics_events_perf_guard_future_partition").await;
    assert!(
        opts.contains(&"autovacuum_vacuum_scale_factor=0.0".to_string())
            && opts.contains(&"autovacuum_vacuum_threshold=20".to_string()),
        "create_range_partition did not carry the tuned setting onto a newly \
         created partition; reloptions were: {opts:?}"
    );

    drop(conn);
    db.cleanup().await;
}

#[derive(QueryableByName)]
struct RelOptions {
    #[diesel(sql_type = diesel::sql_types::Array<Text>)]
    reloptions: Vec<String>,
}

async fn reloptions(conn: &mut sauron_db::PgConn, relation: &str) -> Vec<String> {
    diesel::sql_query(format!(
        "SELECT COALESCE(reloptions, '{{}}') AS reloptions FROM pg_class WHERE oid = '{relation}'::regclass"
    ))
    .get_result::<RelOptions>(conn)
    .await
    .expect("read reloptions")
    .reloptions
}

/// Proves the REPAIR half of migration `2026-08-13-000060`'s fix: once the
/// tuned partition is `VACUUM`ed, `Heap Fetches` genuinely returns to `0` —
/// this is not a plan-shape assertion, it re-runs `EXPLAIN (ANALYZE,
/// BUFFERS)` for real.
///
/// This does NOT sleep and wait for autovacuum to fire on its own. Measured
/// separately (see the task 10 report): with this exact setting, autovacuum
/// self-healed in ~25s in a quiet test database — but `autovacuum_naptime`
/// (default 1 minute) and worker contention (`autovacuum_max_workers`,
/// default 3, shared across every table in the cluster) make that number
/// environment-dependent, and CI is typically slower and more contended than
/// a quiet local database, not less. A wall-clock-bounded wait here would
/// either be flaky (too short) or slow down every CI run for a fixed
/// worst-case margin (too long) — asserting a state autovacuum reaches on
/// its own time is the wrong shape for a fast, deterministic unit test.
///
/// So this splits the claim into two DETERMINISTIC pieces, matching how
/// [`event_table_partitions_are_tuned_for_frequent_autovacuum`] and this
/// test divide the work:
/// - that test proves the trigger is armed (the exact reloptions are set,
///   on real partitions, via both the migration and the code path);
/// - this test proves that when a vacuum DOES run against a page whose
///   all-visible bit a merge cleared, `Heap Fetches` genuinely returns to
///   `0` — i.e. the mechanism the tuned trigger relies on actually works,
///   not merely that a config value is present in `pg_class`.
/// An explicit `VACUUM` stands in for "the tuned autovacuum eventually woke
/// up and ran" — same effect on the visibility map, zero wall-clock
/// dependency. Together, "the trigger is armed at a threshold (20) below
/// what a real merge produces (37, measured below) AND a vacuum reaching the
/// partition fully repairs it" is the honest version of "autovacuum will fix
/// this in production" that a sleep-based test would only approximate, more
/// slowly and less reliably.
///
/// Also keeps the ORIGINAL failing scenario inline (seed → baseline `Heap
/// Fetches: 0` → merge → `Heap Fetches` goes nonzero) as a live assertion,
/// not just a comment: if a future change ever made merges stop damaging the
/// visibility map at all, this test's own "the bug is real" assertion would
/// start failing loudly, rather than this file quietly losing the scenario
/// it exists to guard.
#[tokio::test]
async fn merging_a_guest_is_repaired_by_a_vacuum() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    isolate_index(
        &mut conn,
        "analytics_events",
        "analytics_events_app_env_time_users_idx",
    )
    .await;

    let now = Utc::now();
    seed_analytics_events(&mut conn, ids.app_id, ids.env_a, now, SEEDED_ROWS).await;

    vacuum_until_all_visible(
        &mut conn,
        "VACUUM ANALYZE analytics_events",
        "analytics_events_default",
    )
    .await;
    diesel::sql_query("SET enable_seqscan = off")
        .execute(&mut conn)
        .await
        .expect("disable seqscan");
    diesel::sql_query("SET enable_bitmapscan = off")
        .execute(&mut conn)
        .await
        .expect("disable bitmapscan");

    let query = format!(
        "SELECT count(DISTINCT distinct_id) FROM analytics_events \
          WHERE app_id = '{}' AND environment_id = '{}' AND occurred_at >= '{}' \
            AND distinct_id IS NOT NULL AND distinct_id <> ''",
        ids.app_id,
        ids.env_a,
        (now - Duration::days(7)).to_rfc3339(),
    );

    let before = explain_analyze_buffers(&mut conn, &query).await;
    assert_eq!(
        heap_fetches(&before),
        Some(0),
        "precondition: the baseline must be genuinely index-only before the \
         merge, or this test proves nothing about the merge's effect; plan was:\n{before}"
    );

    // The real merge primitive — one guest bucket (`guest-1`, ~37 of the
    // 5,000 rows) folded into a target person, exactly as the drain runs it.
    sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "guest-1", "merged-target")
        .await
        .expect("rewrite_hot_rows");

    // Confirm the bug is genuinely reproduced, not assumed. `pg_stat_force_
    // next_flush` because the stats collector is otherwise asynchronous —
    // reading `n_dead_tup` immediately after the UPDATE without this can
    // read a stale, pre-merge value.
    diesel::sql_query("SELECT pg_stat_force_next_flush()")
        .execute(&mut conn)
        .await
        .expect("flush stats");
    #[derive(QueryableByName)]
    struct DeadTup {
        #[diesel(sql_type = BigInt)]
        n_dead_tup: i64,
    }
    let dead: DeadTup = diesel::sql_query(
        "SELECT n_dead_tup FROM pg_stat_user_tables WHERE relname = 'analytics_events_default'",
    )
    .get_result(&mut conn)
    .await
    .expect("read dead tuple count");
    assert!(
        dead.n_dead_tup >= 20,
        "precondition: this merge must produce at least as many dead tuples as \
         the tuned threshold (20) for this test to demonstrate the trigger would \
         fire; only produced {}",
        dead.n_dead_tup
    );

    let after_merge = explain_analyze_buffers(&mut conn, &query).await;
    let fetches_after_merge = heap_fetches(&after_merge);
    assert!(
        fetches_after_merge.is_some_and(|n| n > 0),
        "the merge did not damage the visibility map the way it did when this \
         regression was found — this test's own precondition for proving the \
         repair no longer holds; plan was:\n{after_merge}"
    );

    // The repair: an explicit VACUUM, standing in for "the tuned autovacuum
    // woke up and ran" — see this test's doc comment for why this is not a
    // wall-clock wait.
    //
    // Repeated until the map converges, not once: measured, a single VACUUM
    // here left `Heap Fetches` above zero whenever the other tests in this
    // binary had run first, while the same test alone passed every time. That
    // is the harness's horizon window (see `vacuum_until_all_visible`), not a
    // property of the merge. Converging also states the production mechanism
    // more honestly than a one-shot would: autovacuum is a repeating process,
    // so what migration 2026-08-13-000060 actually relies on is that
    // *vacuuming* repairs this, not that exactly one pass does.
    vacuum_until_all_visible(
        &mut conn,
        "VACUUM analytics_events_default",
        "analytics_events_default",
    )
    .await;

    let after_vacuum = explain_analyze_buffers(&mut conn, &query).await;
    assert_eq!(
        heap_fetches(&after_vacuum),
        Some(0),
        "a VACUUM of the touched partition should fully repair Heap Fetches back \
         to 0 — the mechanism migration 2026-08-13-000060's tuned autovacuum \
         threshold relies on; plan was:\n{after_vacuum}"
    );

    drop(conn);
    db.cleanup().await;
}
