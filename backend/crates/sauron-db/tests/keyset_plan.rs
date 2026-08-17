//! Does the keyset pager actually *use* the keyset index?
//!
//! Every other paging test asserts on rows: that a walk reaches each row once,
//! that ties on `occurred_at` are broken by `id`, that a cursor round-trips.
//! **All of them pass against a sequential scan.** Correct output is not
//! evidence of the plan that produced it, so `analytics_events_app_time_id_idx`
//! — the whole point of S2c Task 1, and an index that locks the parent and
//! every child partition to build — has been load-bearing and unverified.
//!
//! The near miss is what makes this worth a test of its own. `analytics_events`
//! already carried `analytics_project_idx (app_id, occurred_at DESC)`, which
//! serves the same `WHERE` perfectly and differs only in the `id` tiebreaker.
//! A build that lost the new index would still answer every row assertion, just
//! with a sort over the matches — invisible until the table is large.
//!
//! So this measures the plan instead of the rows, and it does so by running the
//! **real** `repo::search_events` and reading Postgres' own per-index counters
//! afterwards, rather than by re-deriving the SQL in the test. A test that
//! builds its own query proves its own copy is indexable, which is not the
//! claim.

mod common;

use chrono::{DateTime, Duration, Utc};
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use diesel_async::RunQueryDsl;
use sauron_db::models::{AnalyticsEvent, ErrorEvent, NewAnalyticsEvent, NewErrorEvent, NewIssue};
use sauron_db::query_plan::cursor::Cursor;
use sauron_db::query_plan::PrepCtx;
use sauron_db::repo::{self, EventSearch};
use sauron_db::scope::ReadScope;
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;

use common::TestDb;

/// Rows to seed before asking the planner anything.
///
/// A planner handed twelve rows picks a sequential scan and is *right* to — the
/// index would cost more than the table. Asserting on the plan below that
/// threshold would be asserting that Postgres does the wrong thing. Five
/// thousand rows in one app is past it with room to spare, and still inserts in
/// a single statement.
const SEEDED_ROWS: i64 = 5_000;

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Scans recorded against one index, summed over the partitioned parent and
/// every child index beneath it.
///
/// `pg_stat_all_indexes` records against the CHILD index on a partitioned
/// table, so reading the parent alone reports zero forever and the test would
/// pass by never observing anything. Resolved through `pg_inherits` rather than
/// by name matching, so a renamed child partition cannot silently drop out of
/// the sum.
///
/// `to_regclass` rather than a `::regclass` cast, which ERRORS on a name that
/// does not resolve. A dropped index is the single most likely regression this
/// file exists to catch, and it should arrive as the assertion below — naming
/// the index and what was expected of it — not as a Postgres error about an
/// unknown relation raised from a stats helper.
async fn index_scans(conn: &mut sauron_db::PgConn, index: &str) -> i64 {
    let row: Count = diesel::sql_query(
        "SELECT COALESCE(SUM(s.idx_scan), 0)::bigint AS n \
         FROM pg_stat_all_indexes s \
         WHERE s.indexrelid = to_regclass($1) \
            OR s.indexrelid IN (SELECT inhrelid FROM pg_inherits \
                                WHERE inhparent = to_regclass($1))",
    )
    .bind::<Text, _>(index)
    .get_result(conn)
    .await
    .unwrap_or_else(|e| panic!("read index stats for {index}: {e}"));
    row.n
}

/// The index S2c Task 1 added: `(app_id, occurred_at DESC, id DESC)`.
const KEYSET_INDEX: &str = "analytics_events_app_time_id_idx";

/// The pre-existing `(app_id, occurred_at DESC)` index that differs only by the
/// tiebreaker — the plan a regression would silently fall back to.
const NEAR_MISS_INDEX: &str = "analytics_project_idx";

/// Sequential scans over the whole partition tree.
async fn seq_scans(conn: &mut sauron_db::PgConn) -> i64 {
    let row: Count = diesel::sql_query(
        "SELECT COALESCE(SUM(t.seq_scan), 0)::bigint AS n \
         FROM pg_stat_all_tables t \
         WHERE t.relid IN (SELECT relid FROM pg_partition_tree('analytics_events'))",
    )
    .get_result(conn)
    .await
    .expect("read table stats");
    row.n
}

async fn force_flush(conn: &mut sauron_db::PgConn) {
    diesel::sql_query("SELECT pg_stat_force_next_flush()")
        .execute(conn)
        .await
        .expect("force stats flush");
}

/// Drain everything this backend has already done into the shared counters, and
/// wait for them to stop moving, so the snapshot taken next attributes only
/// what happens *after* it.
///
/// Postgres accumulates statistics in per-backend memory and flushes them on a
/// timer. `pg_stat_force_next_flush()` waives the timer for THIS backend, which
/// is the one running the searches — the whole test therefore has to stay on a
/// single connection, or the counters it reads belong to a backend that did no
/// work.
///
/// Skipping this step is not a theoretical concern: without it the first page's
/// index scan was still unflushed when the baseline was read, and surfaced
/// during the post-measurement poll instead. The "did the second page use the
/// index" assertion then passed on the FIRST page's evidence, and survived a
/// sabotage run with index scans disabled entirely.
async fn quiesce_stats(conn: &mut sauron_db::PgConn) {
    let mut last = -1_i64;
    for _ in 0..40 {
        force_flush(conn).await;
        let now = index_scans(conn, KEYSET_INDEX).await;
        if now == last {
            return;
        }
        last = now;
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Read the keyset counter back after the measured query, allowing for a flush
/// still in flight.
///
/// Only ever waits for the counter to RISE, so a run where it never rises times
/// out into a failed assertion rather than into a fabricated pass.
async fn settled_stats(conn: &mut sauron_db::PgConn, before: i64) -> i64 {
    for _ in 0..40 {
        force_flush(conn).await;
        let now = index_scans(conn, KEYSET_INDEX).await;
        if now > before {
            return now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    index_scans(conn, KEYSET_INDEX).await
}

#[tokio::test]
async fn the_event_pager_reads_through_the_keyset_index() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping keyset_plan");
        return;
    };
    let ids = db.seed_two_envs().await;

    // ONE connection for the whole test: see `settled_stats`.
    let mut conn = db.conn().await;

    // Spread across 500 distinct timestamps so ~10 rows share each one. Ties are
    // exactly where the tiebreaker earns its place, and a fixture with none
    // would let `analytics_project_idx` serve the ordering unaided.
    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, name, distinct_id, properties, context, occurred_at, received_at, \
            tags, contexts, extra) \
         SELECT gen_random_uuid(), $1, 'keyset_probe', 'u-' || (g % 97), \
                '{}'::jsonb, '{}'::jsonb, \
                $2 - ((g % 500) || ' minutes')::interval, now(), \
                '{}'::jsonb, '{}'::jsonb, '{}'::jsonb \
         FROM generate_series(1, $3::bigint) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(Utc::now())
    .bind::<BigInt, _>(SEEDED_ROWS)
    .execute(&mut conn)
    .await
    .expect("seed events");

    // Without this the planner is working from the empty-table statistics it
    // captured at CREATE TABLE and will pick a sequential scan whatever indexes
    // exist — the test would fail against a perfectly good index.
    diesel::sql_query("ANALYZE analytics_events")
        .execute(&mut conn)
        .await
        .expect("analyze");

    let ast = sauron_query::from_legacy(&[], None).expect("empty legacy filter");
    let node = sauron_query::resolve(&ast, sauron_query::Resource::Events).expect("resolve");
    let ctx = PrepCtx {
        environments: HashMap::new(),
        now: Utc::now(),
    };
    let scope = ReadScope::all(ids.app_id);

    // Page one, to obtain a cursor that lands in the middle of the data rather
    // than at its edge.
    let first = repo::search_events(
        &mut conn,
        &scope,
        &EventSearch {
            node: &node,
            ctx: &ctx,
            since: Utc::now() - Duration::days(365),
            until: None,
            sort: repo::EventSort::OccurredAt,
            descending: true,
            after: None,
            limit: 50,
            offset: 0,
        },
    )
    .await
    .expect("first page");
    assert_eq!(first.len(), 51, "50 rows plus the has-more probe");
    let boundary = &first[49];

    quiesce_stats(&mut conn).await;
    let idx_before = index_scans(&mut conn, KEYSET_INDEX).await;
    let near_before = index_scans(&mut conn, NEAR_MISS_INDEX).await;
    let seq_before = seq_scans(&mut conn).await;

    // THE measured query: page two, through the cursor, exactly as the route
    // issues it.
    let second = repo::search_events(
        &mut conn,
        &scope,
        &EventSearch {
            node: &node,
            ctx: &ctx,
            since: Utc::now() - Duration::days(365),
            until: None,
            sort: repo::EventSort::OccurredAt,
            descending: true,
            after: Some(sauron_db::query_plan::cursor::Cursor {
                key: "occurred_at".to_string(),
                value: sauron_db::query_plan::cursor::CursorValue::Ts(boundary.occurred_at),
                id: boundary.id,
            }),
            limit: 50,
            offset: 0,
        },
    )
    .await
    .expect("second page");
    assert_eq!(second.len(), 51, "the fixture is far larger than two pages");

    let idx_after = settled_stats(&mut conn, idx_before).await;
    let near_after = index_scans(&mut conn, NEAR_MISS_INDEX).await;
    let seq_after = seq_scans(&mut conn).await;

    assert!(
        idx_after > idx_before,
        "the keyset page did not read through `analytics_events_app_time_id_idx` \
         ({idx_before} -> {idx_after}). Every row-level paging assertion passes \
         against a sequential scan, so this counter is the only thing that \
         notices the index is gone or unusable for this query's shape."
    );
    assert_eq!(
        seq_after, seq_before,
        "the keyset page fell back to a sequential scan over {SEEDED_ROWS} rows \
         ({seq_before} -> {seq_after})"
    );
    assert_eq!(
        near_after, near_before,
        "the page was served by `analytics_project_idx`, which has no `id` \
         column ({near_before} -> {near_after}). It satisfies the WHERE clause \
         but not the `(occurred_at DESC, id DESC)` ordering, so this is the \
         plan that sorts — the exact regression the new index exists to prevent."
    );

    db.cleanup().await;
}

/// The index must also serve the ASCENDING walk, which `sort=-occurred_at`
/// reaches and which reads the same btree backwards.
///
/// Split from the descending case rather than folded into it: a single test
/// asserting "some index scan happened" would pass with one direction indexed
/// and the other sorting.
#[tokio::test]
async fn the_ascending_pager_reads_through_the_same_index() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping keyset_plan");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, name, distinct_id, properties, context, occurred_at, received_at, \
            tags, contexts, extra) \
         SELECT gen_random_uuid(), $1, $2, 'u-' || (g % 97), \
                '{}'::jsonb, '{}'::jsonb, \
                $3 - ((g % 500) || ' minutes')::interval, now(), \
                '{}'::jsonb, '{}'::jsonb, '{}'::jsonb \
         FROM generate_series(1, $4::bigint) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("keyset_probe_asc")
    .bind::<Timestamptz, _>(Utc::now())
    .bind::<BigInt, _>(SEEDED_ROWS)
    .execute(&mut conn)
    .await
    .expect("seed events");
    diesel::sql_query("ANALYZE analytics_events")
        .execute(&mut conn)
        .await
        .expect("analyze");

    let ast = sauron_query::from_legacy(&[], None).expect("empty legacy filter");
    let node = sauron_query::resolve(&ast, sauron_query::Resource::Events).expect("resolve");
    let ctx = PrepCtx {
        environments: HashMap::new(),
        now: Utc::now(),
    };
    let scope = ReadScope::all(ids.app_id);
    let search = |after: Option<sauron_db::query_plan::cursor::Cursor>| EventSearch {
        node: &node,
        ctx: &ctx,
        since: Utc::now() - Duration::days(365),
        until: None,
        sort: repo::EventSort::OccurredAt,
        descending: false,
        after,
        limit: 50,
        offset: 0,
    };

    let first = repo::search_events(&mut conn, &scope, &search(None))
        .await
        .expect("first page");
    let boundary = &first[49];
    let boundary = sauron_db::query_plan::cursor::Cursor {
        key: "occurred_at".to_string(),
        value: sauron_db::query_plan::cursor::CursorValue::Ts(boundary.occurred_at),
        id: boundary.id,
    };

    quiesce_stats(&mut conn).await;
    let idx_before = index_scans(&mut conn, KEYSET_INDEX).await;
    let seq_before = seq_scans(&mut conn).await;

    repo::search_events(&mut conn, &scope, &search(Some(boundary)))
        .await
        .expect("second page");

    let idx_after = settled_stats(&mut conn, idx_before).await;
    let seq_after = seq_scans(&mut conn).await;

    assert!(
        idx_after > idx_before,
        "the ascending walk did not use the keyset index ({idx_before} -> {idx_after})"
    );
    assert_eq!(
        seq_after, seq_before,
        "the ascending walk fell back to a sequential scan ({seq_before} -> {seq_after})"
    );

    db.cleanup().await;
}

// ===========================================================================
// S2c "table sorting" Slice 2, Task 2: paging by a NULLABLE sort column must
// still reach the rows whose value is NULL.
//
// A deliberately minimal harness — its own org/project/app, not
// `TestDb::seed_two_envs()`'s richer fixture — because the assertion below
// (`seen.len() == 40`) needs to know the EXACT row count for this app, and
// `seed_two_envs` already leaves 11 `analytics_events` rows of its own
// behind that would silently inflate it.
// ===========================================================================

/// Bundles the connection and query-plan scaffolding [`page_events_by`]
/// needs, so the test body reads as "seed, page repeatedly, assert" rather
/// than re-deriving `node`/`ctx`/`scope` at every call site.
struct Harness {
    // Never read directly again after `harness()` builds it — held only so
    // its `Drop` doesn't fire (and warn about a leaked database) until the
    // test calls `h.db.cleanup().await` itself.
    db: TestDb,
    conn: sauron_db::PgConn,
    scope: ReadScope,
    ctx: PrepCtx,
    node: sauron_query::ResolvedNode,
    app_id: Uuid,
}

/// An empty query-language predicate, resolved against `resource` — every
/// harness in this file needs one, and each needs its OWN (an Events-
/// resolved node cannot lower a `search_occurrences` call; the catalogs
/// differ per resource), so this is shared rather than re-typed at each call
/// site.
fn resolved_node(resource: sauron_query::Resource) -> sauron_query::ResolvedNode {
    let ast = sauron_query::from_legacy(&[], None).expect("empty legacy filter");
    sauron_query::resolve(&ast, resource).expect("resolve")
}

/// `None` when no database is reachable — matches every other test in this
/// file's `let Some(db) = TestDb::setup().await else { ...; return; }`
/// convention, folded into one early return here.
///
/// Seeds a bare org/project/app of its own, deliberately NOT
/// `TestDb::seed_two_envs()` — see the section comment above for why.
async fn harness() -> Option<Harness> {
    let db = TestDb::setup().await?;
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();

    let org = repo::create_org(
        &mut conn,
        "keyset harness org",
        &format!("keyset-org-{suffix}"),
    )
    .await
    .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "keyset harness project",
        &format!("keyset-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "keyset harness app",
        &format!("keyset-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");

    let node = resolved_node(sauron_query::Resource::Events);
    let ctx = PrepCtx {
        environments: HashMap::new(),
        now: Utc::now(),
    };
    let scope = ReadScope::all(app.id);

    Some(Harness {
        db,
        conn,
        scope,
        ctx,
        node,
        app_id: app.id,
    })
}

/// One minimal `analytics_events` row, with the caller in control of `id` —
/// `repo::insert_analytics_event` already takes a caller-supplied
/// `NewAnalyticsEvent`, but every seed in this file used to construct one
/// inline, which is how [`seed_cross_tenant_session_probe`] would have ended
/// up as a second, drifting copy of this shape. Shared instead: one spelling
/// of "a minimal analytics event" for every seed below.
async fn seed_one_analytics_event(
    conn: &mut sauron_db::PgConn,
    id: Uuid,
    app_id: Uuid,
    distinct_id: &str,
    session_id: Option<String>,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id,
            app_id,
            environment_id: None,
            name: "keyset.probe".to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            context: json!({}),
            session_id,
            release: None,
            ip_address: None,
            occurred_at,
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("seed analytics event");
}

/// `n` `analytics_events` rows, alternating a real `session_id` with `None` —
/// interleaved, not clustered, so a paging bug that only drops a *contiguous*
/// run of nulls would still be caught by a run that scatters them across the
/// whole keyset walk. Half real, half `None` for `n` even (the only value
/// this test passes).
async fn seed_events_with_some_null_sessions(h: &mut Harness, n: i64) {
    for i in 0..n {
        let session_id = if i % 2 == 0 {
            Some(format!("keyset-session-{i}"))
        } else {
            None
        };
        seed_one_analytics_event(
            &mut h.conn,
            Uuid::new_v4(),
            h.app_id,
            &format!("keyset-distinct-{i}"),
            session_id,
            Utc::now() - Duration::seconds(i),
        )
        .await;
    }
}

/// One page of one app, ordered by `sort`, mirroring exactly what a route
/// does with the `limit + 1` rows [`repo::search_events`] returns: the
/// surplus has-more probe is truncated away before the caller ever sees it,
/// so the cursor built from this page's last row is the same boundary a real
/// route would hand back as `next_cursor`.
///
/// Takes `conn`/`scope`/`ctx`/`node` directly rather than a `&mut Harness` so
/// [`paging_by_session_never_returns_another_apps_rows`] can page ONE app out
/// of a [`TwoAppHarness`] holding two — a second, hand-rolled copy of this
/// query-building logic is exactly the kind of drift this whole round is
/// about.
#[allow(clippy::too_many_arguments)]
async fn page_events_by(
    conn: &mut sauron_db::PgConn,
    scope: &ReadScope,
    ctx: &PrepCtx,
    node: &sauron_query::ResolvedNode,
    sort: repo::EventSort,
    descending: bool,
    after: Option<Cursor>,
    limit: i64,
) -> Vec<AnalyticsEvent> {
    let search = EventSearch {
        node,
        ctx,
        since: Utc::now() - Duration::days(3650),
        until: None,
        sort,
        descending,
        after,
        limit,
        offset: 0,
    };
    let mut rows = repo::search_events(conn, scope, &search)
        .await
        .expect("page events by sort column");
    rows.truncate(limit as usize);
    rows
}

/// The cursor a route would mint from this page's last row under `sort` —
/// the coalesced value, exactly as `text_of` in `repo.rs` would read it back
/// out of the predicate. Building this from the TRUNCATED page (not the
/// has-more probe row `page_events_by` already discarded) is what makes this
/// a faithful stand-in for the real route's `next_cursor`.
/// Delegates the actual value extraction to [`repo::EventSort::cursor_value`]
/// rather than re-deriving the nullable-coalescing rule here: that method
/// exists specifically so a caller minting the next page's cursor (a route,
/// or this harness standing in for one) has exactly one spelling of it to
/// call, not a second copy that can drift from `event_query_for`'s predicate.
fn cursor_from_last(page: &[AnalyticsEvent], sort: repo::EventSort) -> Cursor {
    let last = page
        .last()
        .expect("cursor_from_last called on an empty page");
    Cursor {
        key: sort.column().to_string(),
        value: sort.cursor_value(last),
        id: last.id,
    }
}

/// Paging a nullable sort column must reach rows whose value is NULL.
///
/// The defect: `WHERE (session_id, id) < ($1, $2)` is NULL — not true — for a
/// row with no session, so every such row vanishes from page two onward. It
/// looks like a short result set, not like a bug.
#[tokio::test]
async fn paging_by_session_reaches_rows_with_no_session() {
    let Some(mut h) = harness().await else {
        return;
    };
    seed_events_with_some_null_sessions(&mut h, 40).await;

    let mut seen: Vec<Uuid> = Vec::new();
    let mut after: Option<sauron_db::query_plan::cursor::Cursor> = None;
    for _ in 0..20 {
        let page = page_events_by(
            &mut h.conn,
            &h.scope,
            &h.ctx,
            &h.node,
            sauron_db::repo::EventSort::SessionId,
            true,
            after.clone(),
            7,
        )
        .await;
        if page.is_empty() {
            break;
        }
        after = Some(cursor_from_last(
            &page,
            sauron_db::repo::EventSort::SessionId,
        ));
        seen.extend(page.iter().map(|r| r.id));
    }

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a row was returned on more than one page"
    );
    assert_eq!(
        seen.len(),
        40,
        "paging by a nullable column lost rows with a NULL value"
    );

    h.db.cleanup().await;
}

// ===========================================================================
// Fix round 1: the raw keyset SQL fragments leak across tenants unless each
// one is self-parenthesised. See the module doc comment on
// `paging_by_session_never_returns_another_apps_rows` for the mechanism.
// ===========================================================================

/// Like [`Harness`], but with a SECOND app (`app_b`) seeded in the same
/// database — for the cross-tenant leak test, which needs two apps whose
/// rows can collide on a shared `session_id` value if a keyset predicate
/// loses its tenant scoping. A separate, deliberately pathological fixture
/// rather than a second field bolted onto `Harness`, matching
/// `common::TestDb::seed_cross_env_session` being its own fixture rather
/// than a variant of `seed_two_envs`.
struct TwoAppHarness {
    db: TestDb,
    conn: sauron_db::PgConn,
    /// Scoped to `app_a` — the tenant actually being paged. Rows never
    /// belonging to `app_a` (i.e. `app_b`'s) must never appear through it.
    scope: ReadScope,
    ctx: PrepCtx,
    node: sauron_query::ResolvedNode,
    app_a: Uuid,
    app_b: Uuid,
}

/// `None` when no database is reachable, matching every other harness in
/// this file.
async fn two_app_harness() -> Option<TwoAppHarness> {
    let db = TestDb::setup().await?;
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();

    let org = repo::create_org(
        &mut conn,
        "keyset cross-tenant org",
        &format!("keyset-ct-org-{suffix}"),
    )
    .await
    .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "keyset cross-tenant project",
        &format!("keyset-ct-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app_a = repo::create_app(
        &mut conn,
        project.id,
        "keyset cross-tenant app a",
        &format!("keyset-ct-app-a-{suffix}"),
        "web",
    )
    .await
    .expect("create app a");
    let app_b = repo::create_app(
        &mut conn,
        project.id,
        "keyset cross-tenant app b",
        &format!("keyset-ct-app-b-{suffix}"),
        "web",
    )
    .await
    .expect("create app b");

    let node = resolved_node(sauron_query::Resource::Events);
    let ctx = PrepCtx {
        environments: HashMap::new(),
        now: Utc::now(),
    };
    let scope = ReadScope::all(app_a.id);

    Some(TwoAppHarness {
        db,
        conn,
        scope,
        ctx,
        node,
        app_a: app_a.id,
        app_b: app_b.id,
    })
}

/// `n` rows on `app_a` with distinct `session_id` values
/// `"cross-tenant-shared-0".."cross-tenant-shared-{n-1}"`, and — for EVERY
/// one of those same values — a matching row on `app_b`. Whichever value(s)
/// end up as a page boundary while paging `app_a` by session, `app_b` has a
/// same-valued row ready to leak through an unparenthesised `OR`, so every
/// boundary is covered rather than just one guessed at.
///
/// `app_b`'s rows carry deliberately extreme, explicit ids
/// (`Uuid::from_u128`), not `Uuid::new_v4()`, and how extreme depends on
/// `descending`: the vulnerable disjunct's tie branch is
/// `COALESCE(session_id,'') = $b AND id {cmp} $c`, where `cmp` is `<` when
/// paging descending and `>` when paging ascending. A TINY id (near
/// `Uuid::nil()`) satisfies `id < $c` against any real page boundary
/// unconditionally; a HUGE id (near `Uuid::max()`) does the same for
/// `id > $c`. Either way this is deterministic rather than the ~50% chance a
/// second random v4 id would give — an occasional false negative on the one
/// check this whole round exists to make reliable would be its own defect.
async fn seed_cross_tenant_session_probe(h: &mut TwoAppHarness, n: i64, descending: bool) {
    for i in 0..n {
        let session_id = format!("cross-tenant-shared-{i}");
        seed_one_analytics_event(
            &mut h.conn,
            Uuid::new_v4(),
            h.app_a,
            &format!("app-a-distinct-{i}"),
            Some(session_id.clone()),
            Utc::now() - Duration::seconds(i),
        )
        .await;
        let leak_id = if descending {
            Uuid::from_u128(1000 + i as u128)
        } else {
            Uuid::from_u128(u128::MAX - i as u128)
        };
        seed_one_analytics_event(
            &mut h.conn,
            leak_id,
            h.app_b,
            &format!("app-b-distinct-{i}"),
            Some(session_id),
            Utc::now() - Duration::seconds(i),
        )
        .await;
    }
}

/// Paging one app by a nullable sort column must never surface another
/// app's rows.
///
/// The defect: a raw `sql::<Bool>` fragment containing a top-level `OR`,
/// applied via `.filter()`, is not grouped by diesel — `SqlLiteral::walk_ast`
/// emits it verbatim, and `WhereAnd::and` wraps `existing AND predicate` as a
/// whole, not `predicate` on its own (diesel 2.3.11,
/// `query_builder/where_clause.rs`). Because `AND` binds tighter than `OR`,
/// an unparenthesised fragment splits the WHERE clause in two, and the
/// second half — `COALESCE(session_id,'') = $b AND id < $c` — carries no
/// `app_id`, no `since` window, no environment filter: ANY row in ANY app
/// with a matching `session_id` and a smaller id satisfies it and is
/// returned as though it were `app_a`'s.
///
/// A single-app fixture cannot see this — every row that matches also
/// belongs to the one app being paged, so the unguarded disjunct returns
/// exactly the same set the guarded one would. Hence [`TwoAppHarness`].
#[tokio::test]
async fn paging_by_session_never_returns_another_apps_rows() {
    let Some(mut h) = two_app_harness().await else {
        return;
    };
    seed_cross_tenant_session_probe(&mut h, 20, true).await;

    let mut leaked: Vec<(Uuid, Uuid)> = Vec::new();
    let mut after: Option<Cursor> = None;
    for _ in 0..20 {
        let page = page_events_by(
            &mut h.conn,
            &h.scope,
            &h.ctx,
            &h.node,
            repo::EventSort::SessionId,
            true,
            after.clone(),
            5,
        )
        .await;
        if page.is_empty() {
            break;
        }
        for row in &page {
            if row.app_id != h.app_a {
                leaked.push((row.id, row.app_id));
            }
        }
        after = Some(cursor_from_last(&page, repo::EventSort::SessionId));
    }

    assert!(
        leaked.is_empty(),
        "paging app_a ({}) leaked {} row(s) belonging to another app (app_b = {}): {leaked:?}",
        h.app_a,
        leaked.len(),
        h.app_b
    );

    h.db.cleanup().await;
}

/// The ASCENDING counterpart to
/// [`paging_by_session_never_returns_another_apps_rows`] — a re-review of
/// this round's fix found the descending case above was the ONLY runtime
/// cross-tenant coverage any of the 8 raw keyset fragments had, since
/// [`page_events_by`] used to hardcode `descending: true` unconditionally.
/// The ascending arm of this exact fragment —
/// `COALESCE(session_id,'') > $a OR (COALESCE(session_id,'') = $b AND id >
/// $c)` — had never run against a real cross-tenant fixture at all; it
/// rested on the static `debug_query` pin alone, which this whole round
/// exists because that pin was twice found to accept a leaking shape.
/// `seed_cross_tenant_session_probe`'s `descending` parameter switches
/// `app_b`'s ids from tiny to huge for exactly this direction — see its doc
/// comment.
#[tokio::test]
async fn ascending_paging_by_session_never_returns_another_apps_rows() {
    let Some(mut h) = two_app_harness().await else {
        return;
    };
    seed_cross_tenant_session_probe(&mut h, 20, false).await;

    let mut leaked: Vec<(Uuid, Uuid)> = Vec::new();
    let mut after: Option<Cursor> = None;
    for _ in 0..20 {
        let page = page_events_by(
            &mut h.conn,
            &h.scope,
            &h.ctx,
            &h.node,
            repo::EventSort::SessionId,
            false,
            after.clone(),
            5,
        )
        .await;
        if page.is_empty() {
            break;
        }
        for row in &page {
            if row.app_id != h.app_a {
                leaked.push((row.id, row.app_id));
            }
        }
        after = Some(cursor_from_last(&page, repo::EventSort::SessionId));
    }

    assert!(
        leaked.is_empty(),
        "ASCENDING paging of app_a ({}) leaked {} row(s) belonging to another app \
         (app_b = {}): {leaked:?}",
        h.app_a,
        leaked.len(),
        h.app_b
    );

    h.db.cleanup().await;
}

// ===========================================================================
// Fix round 1, Important #2: `OccurrenceSort`'s six raw arms had never
// executed — `OccurrenceSearch` is constructed nowhere else in `sauron-db`.
// Runtime coverage below, in the same two shapes as the `EventSort` tests
// above: nullable-reaches-every-row, and never-leaks-across-tenants.
// ===========================================================================

/// One minimal `error_events` row, with the caller in control of `id` and
/// all three nullable columns — mirrors [`seed_one_analytics_event`] for the
/// same reason: one spelling of "a minimal occurrence row", shared by both
/// tests below rather than reimplemented per call site.
#[allow(clippy::too_many_arguments)]
async fn seed_one_error_event(
    conn: &mut sauron_db::PgConn,
    id: Uuid,
    app_id: Uuid,
    issue_id: Uuid,
    distinct_id: Option<String>,
    session_id: Option<String>,
    device_key: Option<String>,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id,
            app_id,
            environment_id: None,
            issue_id,
            fingerprint: "keyset-occurrence-probe".to_string(),
            level: "error".into(),
            message: "keyset probe".into(),
            exception_type: "KeysetProbe".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id,
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at,
            session_id,
            device_key,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: None,
            title: None,
            culprit: None,
        },
    )
    .await
    .expect("seed error event");
}

/// One page of one issue's occurrences, ordered by `sort` — the
/// `OccurrenceSort` analogue of [`page_events_by`], for the same reasons:
/// truncates the has-more probe row itself, so the caller's cursor matches
/// what a real route would hand back.
#[allow(clippy::too_many_arguments)]
async fn page_occurrences_by(
    conn: &mut sauron_db::PgConn,
    scope: &ReadScope,
    ctx: &PrepCtx,
    node: &sauron_query::ResolvedNode,
    issue_id: Uuid,
    sort: repo::OccurrenceSort,
    descending: bool,
    after: Option<Cursor>,
    limit: i64,
) -> Vec<ErrorEvent> {
    let search = repo::OccurrenceSearch {
        node,
        ctx,
        since: Utc::now() - Duration::days(3650),
        sort,
        descending,
        after,
        limit,
        offset: 0,
        text_reach: repo::TextSearchReach::ShellOnly,
    };
    let mut rows = repo::search_occurrences(conn, scope, issue_id, &search)
        .await
        .expect("page occurrences by sort column");
    rows.truncate(limit as usize);
    rows
}

/// The `OccurrenceSort` analogue of [`cursor_from_last`] — same reason for
/// delegating to `cursor_value` rather than re-deriving the coalescing rule.
fn occurrence_cursor_from_last(page: &[ErrorEvent], sort: repo::OccurrenceSort) -> Cursor {
    let last = page
        .last()
        .expect("occurrence_cursor_from_last called on an empty page");
    Cursor {
        key: sort.column().to_string(),
        value: sort.cursor_value(last),
        id: last.id,
    }
}

/// `n` `error_events` rows for one issue, with `distinct_id`/`session_id`/
/// `device_key` independently null on different rows — different moduli (2,
/// 3, 5) rather than one shared pattern, so a bug specific to ONE column's
/// keyset arm cannot hide behind another column's null/non-null split
/// happening to line up the same way.
async fn seed_occurrences_with_some_nulls(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    issue_id: Uuid,
    n: i64,
) {
    for i in 0..n {
        let distinct_id = (i % 2 == 0).then(|| format!("occ-distinct-{i}"));
        let session_id = (i % 3 == 0).then(|| format!("occ-session-{i}"));
        let device_key = (i % 5 == 0).then(|| format!("occ-device-{i}"));
        seed_one_error_event(
            conn,
            Uuid::new_v4(),
            app_id,
            issue_id,
            distinct_id,
            session_id,
            device_key,
            Utc::now() - Duration::seconds(i),
        )
        .await;
    }
}

/// Paging occurrences by any of the three nullable `OccurrenceSort` columns
/// must reach rows whose value is NULL — the `OccurrenceSort` analogue of
/// `paging_by_session_reaches_rows_with_no_session`, covering all three
/// (`distinct_id`, `session_id`, `device_key`) in one seeded fixture rather
/// than three, since a single independently-patterned seed (see
/// [`seed_occurrences_with_some_nulls`]) already exercises all three.
#[tokio::test]
async fn paging_occurrences_by_nullable_columns_reaches_rows_with_no_value() {
    let Some(mut h) = harness().await else {
        return;
    };
    let issue_id = repo::upsert_issue(
        &mut h.conn,
        NewIssue {
            app_id: h.app_id,
            fingerprint: "keyset-occurrence-probe",
            type_: "Error",
            title: "keyset occurrence probe",
            culprit: "keyset::probe",
            level: "error",
            first_seen: Utc::now() - Duration::days(1),
            last_seen: Utc::now(),
            times_seen: 30,
        },
    )
    .await
    .expect("seed issue");
    seed_occurrences_with_some_nulls(&mut h.conn, h.app_id, issue_id, 30).await;

    let node = resolved_node(sauron_query::Resource::Occurrences);

    for sort in [
        repo::OccurrenceSort::DistinctId,
        repo::OccurrenceSort::SessionId,
        repo::OccurrenceSort::DeviceKey,
    ] {
        let mut seen: Vec<Uuid> = Vec::new();
        let mut after: Option<Cursor> = None;
        for _ in 0..20 {
            let page = page_occurrences_by(
                &mut h.conn,
                &h.scope,
                &h.ctx,
                &node,
                issue_id,
                sort,
                true,
                after.clone(),
                7,
            )
            .await;
            if page.is_empty() {
                break;
            }
            after = Some(occurrence_cursor_from_last(&page, sort));
            seen.extend(page.iter().map(|r| r.id));
        }

        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            seen.len(),
            "{sort:?}: a row was returned on more than one page"
        );
        assert_eq!(
            seen.len(),
            30,
            "{sort:?}: paging by a nullable column lost rows with a NULL value"
        );
    }

    h.db.cleanup().await;
}

/// `n` rows on `issue_a` (owned by `app_a`) with distinct values in
/// whichever nullable column `sort` names, and — for every one of those same
/// values — a matching row on `issue_b` (owned by `app_b`). Mirrors
/// [`seed_cross_tenant_session_probe`]; see its doc comment for why `app_b`'s
/// ids are deliberately tiny (descending) or huge (ascending) rather than
/// random. Generalised over `sort` — originally this only ever seeded
/// `session_id` — because a re-review of this round's fix found runtime
/// cross-tenant coverage existed for exactly one of `OccurrenceSort`'s three
/// nullable columns; the other two rested entirely on the (twice-fixed)
/// static pin.
async fn seed_cross_tenant_occurrence_probe(
    h: &mut TwoAppHarness,
    issue_a: Uuid,
    issue_b: Uuid,
    sort: repo::OccurrenceSort,
    descending: bool,
    n: i64,
) {
    for i in 0..n {
        let shared = format!("occ-cross-tenant-shared-{i}");
        let (distinct_id, session_id, device_key) = match sort {
            repo::OccurrenceSort::DistinctId => (Some(shared), None, None),
            repo::OccurrenceSort::SessionId => (None, Some(shared), None),
            repo::OccurrenceSort::DeviceKey => (None, None, Some(shared)),
            repo::OccurrenceSort::OccurredAt => panic!(
                "seed_cross_tenant_occurrence_probe is for the three NULLABLE \
                 OccurrenceSort columns only — OccurredAt isn't a raw-SQL \
                 fragment and has no leak of this shape to probe for"
            ),
        };
        seed_one_error_event(
            &mut h.conn,
            Uuid::new_v4(),
            h.app_a,
            issue_a,
            distinct_id.clone(),
            session_id.clone(),
            device_key.clone(),
            Utc::now() - Duration::seconds(i),
        )
        .await;
        let leak_id = if descending {
            Uuid::from_u128(2000 + i as u128)
        } else {
            Uuid::from_u128(u128::MAX - i as u128)
        };
        seed_one_error_event(
            &mut h.conn,
            leak_id,
            h.app_b,
            issue_b,
            distinct_id,
            session_id,
            device_key,
            Utc::now() - Duration::seconds(i),
        )
        .await;
    }
}

/// Paging one app's issue by a nullable sort column must never surface
/// another app's rows — the `OccurrenceSort` analogue of
/// `paging_by_session_never_returns_another_apps_rows`. `occurrence_search_
/// base` filters on BOTH `app_id` and `issue_id`, and the leaked disjunct
/// (before the fix) bypassed both, along with `since` and the environment
/// filter — so this also stands in for a same-app-different-issue leak,
/// which a real deployment reaches far more often than a cross-app one.
#[tokio::test]
async fn paging_occurrences_never_returns_another_apps_rows() {
    let Some(mut h) = two_app_harness().await else {
        return;
    };
    let issue_a = repo::upsert_issue(
        &mut h.conn,
        NewIssue {
            app_id: h.app_a,
            fingerprint: "keyset-ct-occurrence-a",
            type_: "Error",
            title: "keyset cross-tenant occurrence probe a",
            culprit: "keyset::probe",
            level: "error",
            first_seen: Utc::now() - Duration::days(1),
            last_seen: Utc::now(),
            times_seen: 20,
        },
    )
    .await
    .expect("seed issue a");
    let issue_b = repo::upsert_issue(
        &mut h.conn,
        NewIssue {
            app_id: h.app_b,
            fingerprint: "keyset-ct-occurrence-b",
            type_: "Error",
            title: "keyset cross-tenant occurrence probe b",
            culprit: "keyset::probe",
            level: "error",
            first_seen: Utc::now() - Duration::days(1),
            last_seen: Utc::now(),
            times_seen: 20,
        },
    )
    .await
    .expect("seed issue b");
    seed_cross_tenant_occurrence_probe(
        &mut h,
        issue_a,
        issue_b,
        repo::OccurrenceSort::SessionId,
        true,
        20,
    )
    .await;

    let node = resolved_node(sauron_query::Resource::Occurrences);
    let mut leaked: Vec<(Uuid, Uuid)> = Vec::new();
    let mut after: Option<Cursor> = None;
    for _ in 0..20 {
        let page = page_occurrences_by(
            &mut h.conn,
            &h.scope,
            &h.ctx,
            &node,
            issue_a,
            repo::OccurrenceSort::SessionId,
            true,
            after.clone(),
            5,
        )
        .await;
        if page.is_empty() {
            break;
        }
        for row in &page {
            if row.app_id != h.app_a {
                leaked.push((row.id, row.app_id));
            }
        }
        after = Some(occurrence_cursor_from_last(
            &page,
            repo::OccurrenceSort::SessionId,
        ));
    }

    assert!(
        leaked.is_empty(),
        "paging issue_a ({issue_a}) leaked {} row(s) belonging to another app: {leaked:?}",
        leaked.len()
    );

    h.db.cleanup().await;
}

/// The "other column" counterpart to
/// `paging_occurrences_never_returns_another_apps_rows`, which only ever
/// exercised `SessionId`. A re-review of this round's fix found `DistinctId`
/// and `DeviceKey` had NO runtime cross-tenant coverage at all — both
/// fragments rested entirely on the static `debug_query` pin, the same
/// guard twice found to accept a leaking shape. Picks `DeviceKey`
/// arbitrarily as the one other column exercised here; `DistinctId` remains
/// pin-only, which is an accepted gap per this round's brief ("you do not
/// need all eight").
#[tokio::test]
async fn paging_occurrences_by_device_key_never_returns_another_apps_rows() {
    let Some(mut h) = two_app_harness().await else {
        return;
    };
    let issue_a = repo::upsert_issue(
        &mut h.conn,
        NewIssue {
            app_id: h.app_a,
            fingerprint: "keyset-ct-occurrence-device-a",
            type_: "Error",
            title: "keyset cross-tenant occurrence probe a (device_key)",
            culprit: "keyset::probe",
            level: "error",
            first_seen: Utc::now() - Duration::days(1),
            last_seen: Utc::now(),
            times_seen: 20,
        },
    )
    .await
    .expect("seed issue a");
    let issue_b = repo::upsert_issue(
        &mut h.conn,
        NewIssue {
            app_id: h.app_b,
            fingerprint: "keyset-ct-occurrence-device-b",
            type_: "Error",
            title: "keyset cross-tenant occurrence probe b (device_key)",
            culprit: "keyset::probe",
            level: "error",
            first_seen: Utc::now() - Duration::days(1),
            last_seen: Utc::now(),
            times_seen: 20,
        },
    )
    .await
    .expect("seed issue b");
    seed_cross_tenant_occurrence_probe(
        &mut h,
        issue_a,
        issue_b,
        repo::OccurrenceSort::DeviceKey,
        true,
        20,
    )
    .await;

    let node = resolved_node(sauron_query::Resource::Occurrences);
    let mut leaked: Vec<(Uuid, Uuid)> = Vec::new();
    let mut after: Option<Cursor> = None;
    for _ in 0..20 {
        let page = page_occurrences_by(
            &mut h.conn,
            &h.scope,
            &h.ctx,
            &node,
            issue_a,
            repo::OccurrenceSort::DeviceKey,
            true,
            after.clone(),
            5,
        )
        .await;
        if page.is_empty() {
            break;
        }
        for row in &page {
            if row.app_id != h.app_a {
                leaked.push((row.id, row.app_id));
            }
        }
        after = Some(occurrence_cursor_from_last(
            &page,
            repo::OccurrenceSort::DeviceKey,
        ));
    }

    assert!(
        leaked.is_empty(),
        "paging issue_a ({issue_a}) by device_key leaked {} row(s) belonging to \
         another app: {leaked:?}",
        leaked.len()
    );

    h.db.cleanup().await;
}
