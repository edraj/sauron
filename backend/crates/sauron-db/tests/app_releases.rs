//! `app_releases` — the observed (app, environment, release) table behind the
//! dashboard's release switcher. Mirrors `device_env_rollup.rs`: the identity
//! lives in a UNIQUE EXPRESSION index because NULL never equals NULL.

mod common;

use common::TestDb;
use diesel_async::RunQueryDsl;

#[tokio::test]
async fn unattributed_release_rows_are_unique_per_app() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let insert = || {
        diesel::sql_query(
            "INSERT INTO app_releases \
               (app_id, environment_id, release, first_seen_at, last_seen_at) \
             VALUES ($1, NULL, '1.0.0', now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
    };

    insert().execute(&mut conn).await.expect("first insert");
    assert!(
        insert().execute(&mut conn).await.is_err(),
        "a second NULL-environment row for the same (app, release) must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}

/// The other half of the identity index: the NON-NULL half. `(app, env,
/// release)` with a real `environment_id` must be just as unique as the
/// unattributed one — the expression index covers both, and a test that only
/// exercised the `COALESCE` branch would still pass if the index had been
/// built on `COALESCE(environment_id, nil)` alone with no `release` column.
#[tokio::test]
async fn attributed_release_rows_are_unique_per_app_and_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let insert = |env: uuid::Uuid| {
        diesel::sql_query(
            "INSERT INTO app_releases \
               (app_id, environment_id, release, first_seen_at, last_seen_at) \
             VALUES ($1, $2, '1.0.0', now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Uuid, _>(env)
    };

    insert(ids.env_a).execute(&mut conn).await.expect("first");
    assert!(
        insert(ids.env_a).execute(&mut conn).await.is_err(),
        "a second row for the same (app, environment, release) must be rejected"
    );
    // A DIFFERENT environment is a different identity, not a duplicate — the
    // same release reporting from staging and prod is the normal case.
    insert(ids.env_b)
        .execute(&mut conn)
        .await
        .expect("a different environment is a different row");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn environment_id_is_foreign_keyed_to_app_environments() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let result = diesel::sql_query(
        "INSERT INTO app_releases \
           (app_id, environment_id, release, first_seen_at, last_seen_at) \
         VALUES ($1, gen_random_uuid(), '1.0.0', now(), now())",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .execute(&mut conn)
    .await;

    assert!(
        result.is_err(),
        "environment_id must be foreign-keyed to app_environments(id); \
         a random uuid unrelated to any enrollment must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}

use chrono::{Duration, Utc};
use sauron_db::releases::{backfill_all, list_for_app, upsert_seen};
use sauron_db::scope::{EnvFilter, ReadScope};

#[tokio::test]
async fn upsert_widens_one_row_per_identity() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let t0 = Utc::now() - Duration::minutes(10);
    let t1 = Utc::now();

    upsert_seen(&mut conn, ids.app_id, Some(ids.env_a), "1.0.0", t1)
        .await
        .unwrap();
    upsert_seen(&mut conn, ids.app_id, Some(ids.env_a), "1.0.0", t0)
        .await
        .unwrap();
    upsert_seen(&mut conn, ids.app_id, None, "1.0.0", t1)
        .await
        .unwrap();
    upsert_seen(&mut conn, ids.app_id, None, "1.0.0", t1)
        .await
        .unwrap();

    let rows = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::All))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let attributed = rows
        .iter()
        .find(|r| r.environment_id == Some(ids.env_a))
        .unwrap();
    assert!(
        attributed.first_seen_at <= t0 + Duration::seconds(1),
        "first_seen widened backwards"
    );
    assert!(attributed.last_seen_at >= t1 - Duration::seconds(1));

    let only_a = list_for_app(
        &mut conn,
        &ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
    )
    .await
    .unwrap();
    assert_eq!(only_a.len(), 1);
    let unattributed = list_for_app(
        &mut conn,
        &ReadScope::new(ids.app_id, EnvFilter::Unattributed),
    )
    .await
    .unwrap();
    assert_eq!(unattributed.len(), 1);
    assert_eq!(unattributed[0].environment_id, None);

    // `Subset` binds an `Array<Uuid>` against `= ANY($2)` while `One` binds a
    // scalar against `= $2` (see `bind_env!`'s note in `releases.rs`): the two
    // are NOT interchangeable, so a one-element `Subset` is its own case and
    // not covered by the `One` assertion above. Both sizes are exercised
    // because a fragment that forgot the array cast would still bind a
    // single-element list on some drivers.
    upsert_seen(&mut conn, ids.app_id, Some(ids.env_b), "9.9.9", t1)
        .await
        .unwrap();

    let subset_a = list_for_app(
        &mut conn,
        &ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a])),
    )
    .await
    .unwrap();
    assert_eq!(subset_a.len(), 1, "{subset_a:?}");
    assert_eq!(subset_a[0].environment_id, Some(ids.env_a));
    assert_eq!(subset_a[0].release, "1.0.0");

    let subset_both = list_for_app(
        &mut conn,
        &ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b])),
    )
    .await
    .unwrap();
    // Both attributed rows, and NOT the unattributed one: a `Subset` names
    // environments, and the NULL row belongs to none of them.
    let mut got: Vec<(&str, Option<uuid::Uuid>)> = subset_both
        .iter()
        .map(|r| (r.release.as_str(), r.environment_id))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![("1.0.0", Some(ids.env_a)), ("9.9.9", Some(ids.env_b))],
        "{subset_both:?}"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_seeds_from_both_event_tables_and_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Two rows on one release, with `received_at` DELIBERATELY far from
    // `occurred_at` (minutes ago vs days ago). `first_seen_at`/`last_seen_at`
    // are asserted against the `received_at` pair below: that is the clock the
    // pipeline's `note_release` stamps, and a backfill that used `occurred_at`
    // instead would make a release's `first_seen_at` jump backwards the first
    // time it was re-sighted live. With the two clocks days apart, reading the
    // wrong one is a two-day error, not a rounding one.
    let recv_a = Utc::now() - Duration::minutes(30);
    let recv_b = Utc::now() - Duration::minutes(10);
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, received_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'seen', 'd1', '{}', '{}', '2.0.0', now() - interval '2 days', $3, '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'seen', 'd4', '{}', '{}', '2.0.0', now() - interval '1 day',  $4, '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Timestamptz, _>(recv_a)
    .bind::<diesel::sql_types::Timestamptz, _>(recv_b)
    .execute(&mut conn)
    .await
    .unwrap();

    // A whitespace-only release: NOT NULL, so `release IS NOT NULL` alone
    // seeds it — as a switcher entry with a blank label that matches nothing
    // a user can name. Old SDKs and hand-rolled senders are the source (every
    // current SDK rejects a blank release at init, and `sauron-ingest`
    // normalises one to NULL at the edge), and history is exactly what this
    // backfill reads.
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'blank', 'd2', '{}', '{}', '   ', now() - interval '2 days', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'empty', 'd3', '{}', '{}', '',    now() - interval '2 days', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();

    // `seed_two_envs` already inserted 6 `error_events` rows for `issue_id`
    // (and one more for `issue_env_b_only`) under this app, all with
    // `release: None`. Widening one of them to a real release is simpler
    // than a full manual INSERT and exercises the same `error_events` leg.
    diesel::sql_query("UPDATE error_events SET release = '3.0.0' WHERE app_id = $1")
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .execute(&mut conn)
        .await
        .unwrap();

    // The `error_events` leg needs its own blank too — the two SELECTs are
    // separate statements and one could be filtered while the other is not.
    // Run AFTER the blanket UPDATE above, which would otherwise overwrite it.
    diesel::sql_query(
        "UPDATE error_events SET release = '  ' \
         WHERE id = (SELECT id FROM error_events WHERE app_id = $1 LIMIT 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    drop(conn);

    let first = backfill_all(db.pool()).await.unwrap();
    let second = backfill_all(db.pool()).await.unwrap();
    assert!(
        first.upserted >= 2,
        "expected at least two seeded rows, got {first:?}"
    );
    assert_eq!(
        second.upserted, first.upserted,
        "the seed must be idempotent"
    );
    assert!(
        first.normalised_event_rows >= 3,
        "the three blank-ish event rows seeded above must have been repaired: {first:?}"
    );
    assert_eq!(
        second.normalised_event_rows, 0,
        "the repair must be a no-op on a second run: {second:?}"
    );

    let mut conn = db.conn().await;
    let rows = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::All))
        .await
        .unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.release.as_str()).collect();
    assert!(
        names.contains(&"2.0.0") && names.contains(&"3.0.0"),
        "{names:?}"
    );
    assert!(
        names.iter().all(|n| !n.trim().is_empty()),
        "a blank release was seeded into the switcher's catalogue: {names:?}"
    );

    // One clock: `received_at`, not `occurred_at`. The two rows behind
    // `2.0.0` were inserted 30 and 10 minutes ago in `received_at` but 2 and 1
    // DAYS ago in `occurred_at`, so reading the wrong column misses by days.
    let two = rows.iter().find(|r| r.release == "2.0.0").unwrap();
    let skew = |a: chrono::DateTime<Utc>, b: chrono::DateTime<Utc>| (a - b).num_seconds().abs();
    assert!(
        skew(two.first_seen_at, recv_a) <= 1,
        "first_seen_at must be MIN(received_at) ({recv_a}), got {}",
        two.first_seen_at
    );
    assert!(
        skew(two.last_seen_at, recv_b) <= 1,
        "last_seen_at must be MAX(received_at) ({recv_b}), got {}",
        two.last_seen_at
    );

    drop(conn);
    db.cleanup().await;
}

/// A1 — the seed trims exactly like the pipeline does.
///
/// `btrim(release)` (ASCII space only) and the regex `\s` (`[[:space:]]`,
/// which under `en_US.utf8` does not contain U+00A0) both let a release
/// through that Rust's `str::trim` would have rejected or folded. This pins
/// the three shapes that separate the three rules: a TAB (every rule catches
/// it), a NO-BREAK SPACE (only the Unicode rule catches it), and a padded
/// real release (which must fold into its unpadded twin rather than sit
/// beside it as a second, visually identical switcher entry).
#[tokio::test]
async fn backfill_folds_padded_releases_and_rejects_unicode_blanks() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // `U&'\00a0'` is a NO-BREAK SPACE written as an escape so this file stays
    // ASCII; `E'\t'` is a tab.
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'tab',    'd1', '{}', '{}', E'\\t',      now() - interval '2 days', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'nbsp',   'd2', '{}', '{}', U&'\\00a0',  now() - interval '2 days', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'padded', 'd3', '{}', '{}', ' 1.4.0 ',   now() - interval '2 days', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'clean',  'd4', '{}', '{}', '1.4.0',     now() - interval '2 days', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);

    backfill_all(db.pool()).await.unwrap();

    let mut conn = db.conn().await;
    let rows = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::All))
        .await
        .unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.release.as_str()).collect();
    assert_eq!(
        names,
        vec!["1.4.0"],
        "exactly one catalogue row, named `1.4.0`, must survive: {rows:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// A2 — the backfill also repairs the event rows themselves.
///
/// The edge's trim is forward-only: it fixes what arrives from now on and
/// nothing that is already stored. That matters because `?release=1.4.0`
/// lowers to a plain column equality, so a stored `' 1.4.0 '` answers NO to
/// the filter for its own release — the row is not merely mislabelled in the
/// switcher, it is missing from every filtered list. So the operator-run
/// backfill closes the gap on both event tables before it seeds.
#[tokio::test]
async fn backfill_normalises_historical_event_rows_in_place() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'blank',  'd1', '{}', '{}', '  ',      now() - interval '2 days', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'padded', 'd2', '{}', '{}', ' 2.0.0 ', now() - interval '2 days', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();

    // The `error_events` leg is a separate pair of statements, so it gets its
    // own pair of rows. `seed_two_envs` already wrote `error_events` rows with
    // `release: None`; widen two of them.
    diesel::sql_query(
        "UPDATE error_events SET release = ' 2.0.0 ' \
         WHERE id = (SELECT id FROM error_events WHERE app_id = $1 ORDER BY id LIMIT 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "UPDATE error_events SET release = E'\\t' \
         WHERE id = (SELECT id FROM error_events WHERE app_id = $1 ORDER BY id DESC LIMIT 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);

    let out = backfill_all(db.pool()).await.unwrap();
    assert_eq!(
        out.normalised_event_rows, 4,
        "two rows per event table were blank-ish or padded: {out:?}"
    );

    let mut conn = db.conn().await;
    for table in ["analytics_events", "error_events"] {
        #[derive(diesel::QueryableByName)]
        struct Count {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            n: i64,
        }
        let stored: Vec<Count> = diesel::sql_query(format!(
            "SELECT count(*) AS n FROM {table} WHERE app_id = $1 AND release = '2.0.0'"
        ))
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .load(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            stored[0].n, 1,
            "{table}: the padded row must now read `2.0.0`"
        );

        // Every other row this app has on this table was seeded with
        // `release: None`, so `2.0.0` is the ONLY non-NULL value that may be
        // left: the blank-ish one (a TAB on `error_events`, two spaces on
        // `analytics_events`) must have become NULL rather than surviving as a
        // stored non-NULL blank. Asserted as a total rather than as "no
        // blank-ish rows" because `btrim(release) = ''` is itself the
        // ASCII-only rule this change exists to replace — using it here would
        // make the test blind to exactly the character it is about.
        let non_null: Vec<Count> = diesel::sql_query(format!(
            "SELECT count(*) AS n FROM {table} WHERE app_id = $1 AND release IS NOT NULL"
        ))
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .load(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            non_null[0].n, 1,
            "{table}: a blank-ish release must have been set to NULL, not left stored"
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// The repair runs per CHILD PARTITION, so it has to visit all of them.
///
/// A parent-level `UPDATE` locks every partition of both event tables and
/// holds them for the whole run — hours at 158M rows — which blocks
/// `sauron-tier`'s `DETACH PARTITION` for exactly that long and makes an
/// interrupted run lose all of its work. Per partition the locks are per
/// partition and each statement commits on its own. The lock behaviour itself
/// is not observable from a test; what IS observable, and what the loop can
/// get wrong, is COVERAGE — a loop that only reached the default partition
/// (or only the explicit ones) would leave half the history unrepaired and
/// still report a plausible-looking count. So: one padded row in an explicit
/// month partition, one in the default, and both must come out repaired.
#[tokio::test]
async fn the_repair_reaches_every_partition_not_just_the_default() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // An explicit range partition, sited in the past so nothing else routes
    // into it. `occurred_at` is the partition key.
    diesel::sql_query(
        "CREATE TABLE analytics_events_rel_probe PARTITION OF analytics_events \
         FOR VALUES FROM ('2019-01-01') TO ('2019-02-01')",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    #[derive(diesel::QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let children: Vec<Count> = diesel::sql_query(
        "SELECT count(*) AS n FROM pg_inherits \
         WHERE inhparent = 'analytics_events'::regclass",
    )
    .load(&mut conn)
    .await
    .unwrap();
    assert!(
        children[0].n >= 2,
        "fixture must span more than one partition, got {}",
        children[0].n
    );

    // One padded row into the 2019 partition, one into the default.
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'old', 'd1', '{}', '{}', ' 9.9.9 ', '2019-01-15T00:00:00Z', '{}', '{}', '{}'), \
                (gen_random_uuid(), $1, $2, 'new', 'd2', '{}', '{}', ' 9.9.9 ', now() - interval '2 days', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);

    backfill_all(db.pool()).await.unwrap();

    let mut conn = db.conn().await;
    for (label, sql) in [
        (
            "the 2019 partition",
            "SELECT count(*) AS n FROM analytics_events_rel_probe WHERE release = '9.9.9'",
        ),
        (
            "every partition",
            "SELECT count(*) AS n FROM analytics_events WHERE app_id = $1 AND release = ' 9.9.9 '",
        ),
    ] {
        let rows: Vec<Count> = diesel::sql_query(sql)
            .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
            .load(&mut conn)
            .await
            .unwrap();
        let n = rows[0].n;
        if label == "the 2019 partition" {
            assert_eq!(n, 1, "the row in {label} was not repaired");
        } else {
            assert_eq!(n, 0, "a padded value survived in {label}");
        }
    }

    // And the catalogue got ONE entry for the two rows, from both partitions.
    let rows: Vec<Count> = diesel::sql_query(
        "SELECT count(*) AS n FROM app_releases WHERE app_id = $1 AND release = '9.9.9'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .load(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows[0].n, 1);

    drop(conn);
    db.cleanup().await;
}

/// Fix round 2, item 4 — the repair covers every table that stores `release`,
/// not only the two event tables.
///
/// `sessions`, `transactions` and `workflows` all carry a `release` column
/// (schema.rs), and Sessions and Transactions are both release-FILTERED in the
/// query layer — so a stored `' 3.0.0 '` on one of those rows is the same
/// silent wrong answer the event-table repair exists to fix: `?release=3.0.0`
/// lowers to a plain column equality and the row simply vanishes from its own
/// release's list. The catalogue SEED still reads only the two event tables
/// (they are the only ones with a `received_at` to date a release by), so this
/// test asserts the stored values, not the switcher.
///
/// The three between them also cover both shapes of the repair loop:
/// `sessions` (partitioned by `started_at`) and `transactions` (partitioned by
/// `occurred_at`) go per child partition; `workflows` is not partitioned at
/// all and takes the fall-back-to-the-table branch.
#[tokio::test]
async fn the_repair_also_normalises_sessions_transactions_and_workflows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Minimal rows: every column omitted below is either nullable or has a
    // DEFAULT (see migrations 0073 sessions, 0013 transactions, 0032
    // workflows). `environment_id` on all three is the app-environment
    // ENROLLMENT id, which is what `seed_two_envs` hands back as `env_a`.
    diesel::sql_query(
        "INSERT INTO sessions (app_id, environment_id, session_id, started_at, last_event_at, release) \
         VALUES ($1, $2, 'rel-fix-session', now(), now(), ' 3.0.0 ')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO transactions (app_id, environment_id, name, op, duration_ms, occurred_at, release) \
         VALUES ($1, $2, 'rel-fix-tx', 'http.client', 12.5, now() - interval '2 days', ' 3.0.0 ')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO workflows (app_id, environment_id, workflow_id, name, started_at, last_event_at, release) \
         VALUES ($1, $2, 'rel-fix-wf', 'Checkout', now(), now(), ' 3.0.0 ')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);

    backfill_all(db.pool()).await.unwrap();

    let mut conn = db.conn().await;
    #[derive(diesel::QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    for table in ["sessions", "transactions", "workflows"] {
        let trimmed: Vec<Count> = diesel::sql_query(format!(
            "SELECT count(*) AS n FROM {table} WHERE app_id = $1 AND release = '3.0.0'"
        ))
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .load(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            trimmed[0].n, 1,
            "{table}: the padded release was left stored, so `?release=3.0.0` \
             will not find this row"
        );

        let padded: Vec<Count> = diesel::sql_query(format!(
            "SELECT count(*) AS n FROM {table} WHERE app_id = $1 AND release = ' 3.0.0 '"
        ))
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .load(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            padded[0].n, 0,
            "{table}: a padded value survived the repair"
        );
    }

    drop(conn);
    db.cleanup().await;
}
