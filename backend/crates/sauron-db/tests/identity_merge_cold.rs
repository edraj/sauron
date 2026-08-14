//! The bounded map that feeds the DuckDB cold overlay.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::identity_merge::cold_alias_map;

async fn seed(
    db: &TestDb,
    app_id: uuid::Uuid,
    alias: &str,
    state: &str,
    first: Option<chrono::DateTime<Utc>>,
    last: Option<chrono::DateTime<Utc>>,
    cold_stale: bool,
) {
    let mut conn = db.conn().await;
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, alias_first_seen, alias_last_seen, cold_stale) \
         VALUES ($1, $2, 'u-42', $3, $4, $5, $6)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(state)
    .bind::<diesel::sql_types::Nullable<Timestamptz>, _>(first)
    .bind::<diesel::sql_types::Nullable<Timestamptz>, _>(last)
    .bind::<diesel::sql_types::Bool, _>(cold_stale)
    .execute(&mut conn)
    .await
    .expect("seed merge row");
}

/// A merge whose rows were all still hot when it ran was rewritten BEFORE
/// export, so Parquet already holds the person's id. Carrying it in the overlay
/// forever would be pure cost.
#[tokio::test]
async fn a_not_cold_stale_alias_is_excluded() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(
        &db,
        ids.app_id,
        "anon_hot",
        "done",
        Some(now - Duration::hours(1)),
        Some(now),
        false,
    )
    .await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(30), now)
        .await
        .unwrap();
    assert!(
        map.is_empty(),
        "cold_stale = false must be pruned, got {map:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// Window pruning: an alias whose activity does not overlap the query window
/// cannot affect its answer.
#[tokio::test]
async fn an_alias_outside_the_window_is_excluded() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(
        &db,
        ids.app_id,
        "anon_old",
        "done",
        Some(now - Duration::days(90)),
        Some(now - Duration::days(80)),
        true,
    )
    .await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert!(
        map.is_empty(),
        "a non-overlapping span must be pruned, got {map:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// THE HOLE THE SPEC SELF-REVIEW CAUGHT.
///
/// Until the fold runs, the span is NULL and cold_stale is its conservative
/// default — neither prune is safe. Dropping an in-flight alias from the
/// overlay would leave the row stale in BOTH tiers at once: the hot rewrite has
/// not landed either.
#[tokio::test]
async fn a_pending_alias_is_always_included() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(
        &db,
        ids.app_id,
        "anon_inflight",
        "pending",
        None,
        None,
        true,
    )
    .await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert_eq!(
        map.len(),
        1,
        "an unmerged alias must never be pruned, got {map:?}"
    );
    assert_eq!(map[0].alias, "anon_inflight");
    assert_eq!(map[0].person, "u-42");

    drop(conn);
    db.cleanup().await;
}

/// Review finding: a `done` merge can ALSO have a NULL span.
/// `fold_rollups`'s span capture is guarded by `s.f IS NOT NULL`, so an alias
/// whose `moved` CTE came back empty (its activity was entirely cold already,
/// or predates `event_user_environments`) reaches `done` with the span still
/// NULL. `NULL < x` / `NULL >= y` both evaluate to SQL NULL, so a plain
/// inequality predicate silently drops this row — pruned on a span it does
/// not have, while its guest id is still sitting in Parquet, unresolved,
/// forever. This is the exact test that fails against the pre-fix predicate
/// and passes only once NULL is treated as "cannot prove this is safe to
/// drop", the same conservative reading `state <> 'done'` already gets.
#[tokio::test]
async fn a_done_alias_with_a_null_span_is_never_pruned() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(&db, ids.app_id, "anon_nullspan", "done", None, None, true).await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert_eq!(
        map.len(),
        1,
        "a done merge with a NULL span must never be pruned, got {map:?}"
    );
    assert_eq!(map[0].alias, "anon_nullspan");
    assert_eq!(map[0].person, "u-42");

    drop(conn);
    db.cleanup().await;
}

/// Defence in depth: chains are refused at claim time, so this shape should
/// never occur through normal code paths — but if the invariant were ever
/// broken, a plain `COALESCE` in the DuckDB join only resolves one hop, so
/// `x → y` and `y → z` would silently be returned as two independent, correct
/// edges instead of the broken chain they are. Seeded directly via SQL
/// (bypassing `claim_identity`, which would refuse this) to exercise the
/// `NOT EXISTS` guard in isolation.
#[tokio::test]
async fn a_row_whose_person_is_itself_an_alias_is_excluded() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    let mut conn = db.conn().await;

    // x → y: state 'pending' so this row alone would otherwise always be
    // included (proving the exclusion below is from the chain guard, not from
    // an unrelated span/cold_stale prune).
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'chain_x', 'chain_y', 'pending')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed x -> y");

    // y → z: makes 'chain_y' itself an alias, forming a chain x -> y -> z.
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'chain_y', 'chain_z', 'pending')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed y -> z");

    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert!(
        !map.iter().any(|e| e.alias == "chain_x"),
        "x -> y must be excluded when y is itself claimed as an alias, got {map:?}"
    );
    assert!(
        map.iter()
            .any(|e| e.alias == "chain_y" && e.person == "chain_z"),
        "y -> z itself is a normal, unchained edge and must still be included, got {map:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// **The chain guard is per-APP, and the fixture cannot show that on its own.**
///
/// `seed_two_envs` builds ONE app with two environments, so
/// `a_row_whose_person_is_itself_an_alias_is_excluded` above passes
/// identically whether or not the guard correlates `c.app_id = m.app_id` — a
/// dropped correlation is invisible to it. This seeds a second app under the
/// same project and puts the chain's second edge THERE.
///
/// The failure a dropped correlation causes is not cosmetic and not
/// self-announcing. `distinct_id` is an application-chosen string
/// (`u-42`, `user_1`, an email), so collisions ACROSS tenants are ordinary,
/// not pathological. Without the correlation, one app's alias row silently
/// evicts another app's perfectly valid edge from the cold overlay — and an
/// evicted edge means that guest double-counts in every cold query for that
/// app, with no error anywhere. The more apps a deployment has, the more of
/// its overlay quietly disappears.
///
/// This now guards more than one call site: the same anti-join, over the same
/// table with the same correlation, is carried by `cold_alias_map` (here),
/// `repo::repair_restored_rows`, and `identity_merge::chain_conflict`.
#[tokio::test]
async fn a_chain_edge_in_another_app_does_not_exclude_this_apps_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    let mut conn = db.conn().await;

    let app_b = sauron_db::repo::create_app(
        &mut conn,
        ids.project_id,
        "second app",
        &format!("second-app-{}", uuid::Uuid::new_v4().simple()),
        "web",
    )
    .await
    .expect("create a second app under the same project");

    // App A: an ordinary, unchained edge. Nothing in app A claims
    // `xapp_person` as an alias.
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'xapp_alias', 'xapp_person', 'pending')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed app A's edge");

    // App B: a DIFFERENT tenant happens to use the same id string as an
    // alias. Nothing about app A's edge changes.
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'xapp_person', 'xapp_final', 'pending')",
    )
    .bind::<SqlUuid, _>(app_b.id)
    .execute(&mut conn)
    .await
    .expect("seed app B's unrelated edge");

    let map_a = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .expect("cold_alias_map for app A");
    assert!(
        map_a
            .iter()
            .any(|e| e.alias == "xapp_alias" && e.person == "xapp_person"),
        "app A's edge is not a chain — the row that makes `xapp_person` an alias belongs to \
         ANOTHER app. Dropping `c.app_id = m.app_id` from the guard's anti-join evicts it \
         anyway, and this guest then double-counts in every cold query for app A with no \
         error anywhere. Map was {map_a:?}"
    );

    // The mirror: app B's own edge is likewise unaffected by app A's rows.
    // Without the correlation, `xapp_final` being nobody's alias anywhere is
    // what saves it — so this half alone would NOT catch the bug, and is
    // asserted only to pin that the second app is really populated and
    // readable rather than silently empty.
    let map_b = cold_alias_map(&mut conn, app_b.id, now - Duration::days(7), now)
        .await
        .expect("cold_alias_map for app B");
    assert!(
        map_b
            .iter()
            .any(|e| e.alias == "xapp_person" && e.person == "xapp_final"),
        "fixture precondition: app B's own edge must be readable under app B, got {map_b:?}"
    );
    assert!(
        !map_b.iter().any(|e| e.alias == "xapp_alias"),
        "and app A's edge must never appear under app B — the arms' own `app_id = $1` \
         filter, checked here so an over-broad fix to the anti-join cannot pass by \
         returning everything. Map was {map_b:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// **The rewrite's safety net: the four `UNION ALL` arms must select exactly
/// the row set the single `OR`-ed `WHERE` used to.**
///
/// `cold_alias_map` was rewritten from one predicate into four index-backed
/// arms purely for cost (the `OR` defeated every partial index; measured
/// 7,438 buffers / 22.0 ms at 200k rows on a per-request dashboard path).
/// The selection is the single source of truth for BOTH the cold overlay and
/// its prune, so a rewrite that changes it does not fail loudly — it silently
/// double-counts a guest (a row wrongly dropped) or resolves rows that should
/// have been left alone (a row wrongly added), in a tier nothing else checks.
///
/// Seeds EVERY combination of the three axes the predicate reads — all five
/// `state` values × `cold_stale` × {NULL span, span inside the window, span
/// entirely before it, span entirely after it} — and compares the shipping
/// function's output against the ORIGINAL predicate, executed as raw SQL, on
/// the same rows. The original is written out verbatim rather than described,
/// so this is a differential test and not a restatement of what the new code
/// happens to do.
#[tokio::test]
async fn the_union_arms_select_exactly_what_the_original_predicate_did() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    let win_from = now - Duration::days(10);
    let win_to = now - Duration::days(2);

    // Four span shapes, named so a failure says which one moved.
    type SpanShape = (
        &'static str,
        Option<chrono::DateTime<Utc>>,
        Option<chrono::DateTime<Utc>>,
    );
    let spans: [SpanShape; 4] = [
        ("nullspan", None, None),
        (
            "inside",
            Some(now - Duration::days(8)),
            Some(now - Duration::days(6)),
        ),
        (
            "before",
            Some(now - Duration::days(40)),
            Some(now - Duration::days(30)),
        ),
        ("after", Some(now - Duration::days(1)), Some(now)),
    ];

    for state in ["pending", "running", "done", "failed", "dead"] {
        for cold_stale in [true, false] {
            for (shape, first, last) in spans {
                seed(
                    &db,
                    ids.app_id,
                    &format!("{state}_{cold_stale}_{shape}"),
                    state,
                    first,
                    last,
                    cold_stale,
                )
                .await;
            }
        }
    }

    let mut conn = db.conn().await;

    // The ORIGINAL predicate, verbatim, as the reference implementation.
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        alias: String,
    }
    let mut reference: Vec<String> = diesel::sql_query(
        "SELECT alias_id AS alias FROM identity_merges m \
          WHERE m.app_id = $1 \
            AND ( m.state <> 'done' \
                  OR (m.cold_stale \
                      AND (m.alias_first_seen IS NULL \
                           OR (m.alias_first_seen < $3 AND m.alias_last_seen >= $2))) ) \
            AND NOT EXISTS (SELECT 1 FROM identity_merges c \
                             WHERE c.app_id = m.app_id AND c.alias_id = m.distinct_id)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(win_from)
    .bind::<Timestamptz, _>(win_to)
    .load::<Row>(&mut conn)
    .await
    .expect("reference selection")
    .into_iter()
    .map(|r| r.alias)
    .collect();

    let mut actual: Vec<String> = cold_alias_map(&mut conn, ids.app_id, win_from, win_to)
        .await
        .expect("cold_alias_map")
        .into_iter()
        .map(|e| e.alias)
        .collect();

    // The seed has every row pointing at the same person `u-42`, and no row
    // has `alias_id = 'u-42'`, so the chain guard excludes nothing here —
    // which is what makes the count assertion below meaningful. Guard that
    // premise rather than assuming it.
    assert!(
        !reference.is_empty(),
        "precondition: the reference predicate must select something, or this test \
         compares two empty sets and proves nothing"
    );

    reference.sort();
    actual.sort();
    assert_eq!(
        actual, reference,
        "the four UNION ALL arms must select exactly the original predicate's row set — \
         a difference here is a silent double-count (row dropped) or a wrong resolution \
         (row added) in the cold tier"
    );

    // `UNION ALL` does not de-duplicate, so overlapping arms would emit an
    // alias twice — and this map is joined inside DuckDB, where a doubled
    // edge doubles the rows it resolves. Equality against the reference above
    // would NOT catch that on its own if the reference also happened to be
    // sorted the same way, so the arms' disjointness is asserted directly.
    let mut deduped = actual.clone();
    deduped.dedup();
    assert_eq!(
        deduped, actual,
        "the arms must be mutually disjoint — UNION ALL emits duplicates, and a duplicated \
         alias edge doubles the rows the DuckDB overlay resolves"
    );

    drop(conn);
    db.cleanup().await;
}

/// The rewrite's whole point: each arm must actually be served by an index —
/// and by the RIGHT one.
///
/// An earlier version of this test asserted only that the three index NAMES
/// appeared somewhere in the whole union's plan. That is a much weaker claim
/// than the test's name, and it was demonstrably too weak: arm 3 supplies
/// `cold_window_idx` on its own, so dropping `identity_merges_app_span_idx`
/// entirely — which forces arm 4 from 436 buffers onto 3,710 — left this
/// test GREEN. Each arm is now EXPLAINed on its own and pinned to the index
/// it is supposed to ride, so the test defends the per-arm property its name
/// claims rather than the union's aggregate.
#[tokio::test]
async fn every_cold_overlay_arm_is_index_backed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    let mut conn = db.conn().await;

    // Spread over 730 days, not a handful: arm 4's index choice depends on the
    // relative selectivity of `alias_first_seen < $to` (near-unbounded for a
    // window ending near now) against `alias_last_seen >= $from`, and a
    // fixture where every row falls inside the window cannot express that
    // difference. ~1/3 cold_stale, 1-in-97 NULL span so arm 3 has real rows.
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, alias_first_seen, alias_last_seen, \
            cold_stale, completed_at) \
         SELECT $1, 'bulk_' || g, 'u-bulk-' || g, \
                CASE WHEN g % 500 = 0 THEN 'pending' \
                     WHEN g % 700 = 0 THEN 'dead' ELSE 'done' END, \
                CASE WHEN g % 97 = 0 THEN NULL \
                     ELSE now() - make_interval(days => (g % 730)) END, \
                CASE WHEN g % 97 = 0 THEN NULL \
                     ELSE now() - make_interval(days => (g % 730)) + interval '2 hours' END, \
                (g % 3 = 0), now() \
           FROM generate_series(1, 40000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("bulk seed");
    diesel::sql_query("VACUUM ANALYZE identity_merges")
        .execute(&mut conn)
        .await
        .expect("analyze");

    #[derive(diesel::QueryableByName)]
    struct Plan {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        line: String,
    }

    let from = (now - Duration::days(30)).to_rfc3339();
    let to = now.to_rfc3339();
    // (arm number, its predicate, the index it must ride). Verbatim from
    // `cold_alias_map` — if an arm's predicate changes there without changing
    // here, this stops testing the shipped query.
    let arms: [(u8, String, &str); 4] = [
        (
            1,
            "state IN ('pending', 'failed', 'running')".to_string(),
            "identity_merges_runnable_idx",
        ),
        (2, "state = 'dead'".to_string(), "identity_merges_dead_idx"),
        (
            3,
            "state = 'done' AND cold_stale AND alias_first_seen IS NULL".to_string(),
            "identity_merges_cold_window_idx",
        ),
        (
            4,
            format!(
                "state = 'done' AND cold_stale AND alias_first_seen < '{to}' \
                 AND alias_last_seen >= '{from}'"
            ),
            "identity_merges_app_span_idx",
        ),
    ];

    for (n, predicate, index) in &arms {
        let plan: Vec<Plan> = diesel::sql_query(format!(
            "EXPLAIN SELECT app_id, alias_id, distinct_id FROM identity_merges \
              WHERE app_id = '{app}' AND {predicate}",
            app = ids.app_id,
        ))
        .load(&mut conn)
        .await
        .unwrap_or_else(|e| panic!("explain arm {n}: {e}"));
        let text = plan
            .iter()
            .map(|p| p.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !text.contains("Seq Scan"),
            "arm {n} must not sequentially scan identity_merges — its cost would be \
             O(every signup this deployment has ever seen), on a per-request dashboard \
             path; plan was:\n{text}"
        );
        assert!(
            text.contains(index),
            "arm {n} must ride {index} specifically. Asserting only that the name appears \
             somewhere in the FULL union's plan is what previously let \
             identity_merges_app_span_idx be dropped with this test still green (arm 3 \
             supplies cold_window_idx on its own, masking arm 4 regressing 8.5x); \
             plan was:\n{text}"
        );
    }

    // The union as a whole may contain exactly ONE sequential scan — the chain
    // guard's anti-join hashes the table once, which is O(this app's merges)
    // done once rather than per candidate row. More than one means an arm
    // regressed.
    let plan: Vec<Plan> = diesel::sql_query(format!(
        "EXPLAIN SELECT m.alias_id, m.distinct_id \
           FROM ( SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = '{app}' AND {p1} \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = '{app}' AND {p2} \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = '{app}' AND {p3} \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = '{app}' AND {p4} ) m \
          WHERE NOT EXISTS (SELECT 1 FROM identity_merges c \
                             WHERE c.app_id = m.app_id AND c.alias_id = m.distinct_id)",
        app = ids.app_id,
        p1 = arms[0].1,
        p2 = arms[1].1,
        p3 = arms[2].1,
        p4 = arms[3].1,
    ))
    .load(&mut conn)
    .await
    .expect("explain union");
    let text = plan
        .iter()
        .map(|p| p.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let seq_scans = text.matches("Seq Scan on identity_merges").count();
    assert!(
        seq_scans <= 1,
        "at most the chain guard's anti-join may scan sequentially; found {seq_scans}; \
         plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}

/// `dead_merge_count` runs every 5 seconds on every replica and used to be an
/// unconditional sequential scan of a table that gains a row per signup and
/// has no purge path (measured 3,636 buffers / 10 ms at 200k rows).
#[tokio::test]
async fn dead_merge_count_is_index_backed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         SELECT $1, 'dm_' || g, 'u-dm-' || g, \
                CASE WHEN g % 5000 = 0 THEN 'dead' ELSE 'done' END \
           FROM generate_series(1, 20000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("bulk seed");
    diesel::sql_query("VACUUM ANALYZE identity_merges")
        .execute(&mut conn)
        .await
        .expect("analyze");

    #[derive(diesel::QueryableByName)]
    struct Plan {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        line: String,
    }
    let plan: Vec<Plan> = diesel::sql_query(
        "EXPLAIN SELECT count(*)::bigint AS n FROM identity_merges WHERE state = 'dead'",
    )
    .load(&mut conn)
    .await
    .expect("explain");
    let text = plan
        .iter()
        .map(|p| p.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("identity_merges_dead_idx"),
        "the 5-second dead gauge must ride its partial index, not scan every merge ever \
         performed; plan was:\n{text}"
    );
    assert!(
        !text.contains("Seq Scan"),
        "no sequential scan may remain in the dead gauge; plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}
