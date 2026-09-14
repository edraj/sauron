//! `process_job` must never hold two pool connections at once.
//!
//! The per-item path checks out a connection for the dispatch and, when the
//! job carries a release, a second one afterwards for the `app_releases`
//! upsert. Those two checkouts have to be strictly sequential: the ingest pool
//! is small (`INGEST_DB_POOL`, 8 by default) and every worker task
//! (`WORKER_CONCURRENCY`, also 8) runs this function, so if each task held its
//! first connection while asking for a second, all eight slots would be held
//! by tasks waiting for a ninth and the whole worker would livelock until
//! `POOL_WAIT_TIMEOUT` (5s) fired on every one of them — and a timeout there
//! used to be propagated with `?`, re-queuing (and duplicating) a job whose
//! event had already committed.
//!
//! A pool of **max_size 1** is the smallest expression of that invariant: the
//! second checkout can only succeed if the first has already been dropped. Any
//! code shape that overlaps them fails this test by timing out, which is
//! exactly how it failed before the fix.
//!
//! Same ephemeral-database harness as `identity_merge_batch.rs` (which
//! documents why `sauron-db`'s `tests/common::TestDb` and the library's
//! `cfg(test)` `PipelineTestDb` are both invisible from an integration test in
//! this crate), with its own two-letter discriminator "rp" so the two
//! harnesses' database names cannot collide.

use std::cell::Cell;
use std::sync::Arc;

use chrono::Utc;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_core::envelope::{AnalyticsItem, EnvelopeContext, EnvelopeItem, IngestJob};
use sauron_db::models::NewAppEnvironment;
use sauron_db::repo;
use sauron_pipeline::mask::MaskSet;
use sauron_pipeline::{process_job, SymbolizeCtx};
use sauron_redis::RedisStore;
use uuid::Uuid;

struct TestDb {
    /// Deliberately `max_size = 1`. See the module docs.
    pool: sauron_db::PgPool,
    admin_url: String,
    db_name: String,
    cleaned_up: Cell<bool>,
}

impl TestDb {
    async fn setup() -> Option<Self> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let db_name = format!(
            "sauron_test_{}_rp{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::create_test_database(&admin_url, &db_name)
            .await
            .expect("create migrated ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 1).expect("build size-1 test pool");
        Some(Self {
            pool,
            admin_url,
            db_name,
            cleaned_up: Cell::new(false),
        })
    }

    async fn cleanup(&self) {
        sauron_db::drop_database(&self.admin_url, &self.db_name)
            .await
            .expect("drop ephemeral test database");
        self.cleaned_up.set(true);
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if !self.cleaned_up.get() {
            eprintln!(
                "WARNING: ephemeral test database {} may remain (TestDb::cleanup() was never \
                 reached — the test likely panicked). It is named so sauron-db's stale-db \
                 reaper will collect it after 3h, or drop it manually:\n  \
                 DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

fn swap_database(url: &str, new_db: &str) -> String {
    let (scheme, rest) = url
        .split_once("://")
        .expect("TEST_DATABASE_URL must be scheme://...");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
    format!("{scheme}://{authority}/{new_db}{query}")
}

struct SeedIds {
    app_id: Uuid,
    project_id: Uuid,
    org_id: Uuid,
    environment_id: Uuid,
}

async fn seed_app(pool: &sauron_db::PgPool) -> SeedIds {
    // Scoped so the single pool slot is FREE again before the test body runs —
    // otherwise the seeding helper, not the code under test, would be what
    // exhausted the pool.
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "rp org", &format!("rp-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "rp project",
        &format!("rp-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "rp app",
        &format!("rp-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");
    let env = repo::create_project_environment(&mut conn, project.id, "production")
        .await
        .expect("create catalogue env");
    let environment_id = repo::create_app_environments(
        &mut conn,
        &[NewAppEnvironment {
            app_id: app.id,
            environment_id: env.id,
            public_key: &format!("pk_rp_{suffix}"),
            is_default: true,
        }],
    )
    .await
    .expect("enroll app in env")
    .remove(0)
    .id;

    SeedIds {
        app_id: app.id,
        project_id: project.id,
        org_id: org.id,
        environment_id,
    }
}

fn event_job(ids: &SeedIds, release: Option<&str>) -> IngestJob {
    IngestJob {
        app_id: ids.app_id,
        project_id: ids.project_id,
        org_id: ids.org_id,
        environment_id: ids.environment_id,
        release: release.map(str::to_string),
        received_at: Utc::now(),
        ip: None,
        user_agent: None,
        context: EnvelopeContext::default(),
        sdk: None,
        item: EnvelopeItem::Event(AnalyticsItem {
            name: "rp_event".to_string(),
            distinct_id: "person-rp".to_string(),
            properties: serde_json::json!({}),
            timestamp: Utc::now(),
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: serde_json::json!({}),
            contexts: serde_json::json!({}),
            extra: serde_json::json!({}),
        }),
    }
}

async fn test_redis() -> Option<RedisStore> {
    let url = std::env::var("TEST_REDIS_URL").ok()?;
    RedisStore::connect(&url).await.ok()
}

async fn sym_ctx() -> SymbolizeCtx {
    SymbolizeCtx::new(
        Arc::new(sauron_symbols::Symbolicator::new(1 << 20)),
        sauron_redis::SymbolBlobCache::connect(None, 1 << 20).await,
        100,
        1 << 20,
    )
}

#[derive(diesel::QueryableByName)]
struct ReleaseRow {
    #[diesel(sql_type = Text)]
    release: String,
}

/// The Event arm: `process_job` takes the dispatch connection by `&mut`, so
/// before the fix it was still checked out when the `app_releases` upsert
/// asked for its own. On a size-1 pool that is a 5-second wait followed by a
/// propagated error; with the fix the first connection is dropped
/// unconditionally after the match and the upsert gets the (now free) slot.
#[tokio::test]
async fn the_event_arm_holds_one_pool_slot_at_a_time() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let Some(redis) = test_redis().await else {
        eprintln!("TEST_REDIS_URL unset — skipping");
        db.cleanup().await;
        return;
    };
    let ids = seed_app(&db.pool).await;
    let sym = sym_ctx().await;
    let masks = MaskSet::from_rows(vec![]);

    process_job(
        &db.pool,
        &redis,
        &sym,
        &masks,
        event_job(&ids, Some("1.4.0")),
    )
    .await
    .expect(
        "process_job must not need two pool slots at once — a failure here is the \
         5s POOL_WAIT_TIMEOUT on the second checkout",
    );

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let rows: Vec<ReleaseRow> =
        diesel::sql_query("SELECT release FROM app_releases WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .load(&mut conn)
            .await
            .expect("app_releases rows");

    assert_eq!(rows.len(), 1, "the release must have been recorded");
    assert_eq!(rows[0].release, "1.4.0");

    drop(conn);
    db.cleanup().await;
}

/// The same invariant with the upsert leg switched off: a job with no release
/// takes the early exit and must still complete on one slot. Guards against a
/// "fix" that simply moved the overlap behind the `clean(...)` gate.
#[tokio::test]
async fn a_releaseless_event_also_holds_one_pool_slot_at_a_time() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let Some(redis) = test_redis().await else {
        eprintln!("TEST_REDIS_URL unset — skipping");
        db.cleanup().await;
        return;
    };
    let ids = seed_app(&db.pool).await;
    let sym = sym_ctx().await;
    let masks = MaskSet::from_rows(vec![]);

    process_job(&db.pool, &redis, &sym, &masks, event_job(&ids, None))
        .await
        .expect("process_job with no release");

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let rows: Vec<ReleaseRow> =
        diesel::sql_query("SELECT release FROM app_releases WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .load(&mut conn)
            .await
            .expect("app_releases rows");
    assert!(rows.is_empty(), "no release on the job, no catalogue row");

    drop(conn);
    db.cleanup().await;
}
