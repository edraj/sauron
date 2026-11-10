//! `issues.users_seen` converges after a burst inside one throttle window.
//!
//! The denormalised count is written back at most once per issue per
//! `users_seen::WRITE_BACK_INTERVAL`. Before the pending set existed, an
//! occurrence whose HyperLogLog moved while that gate was closed was simply
//! skipped, and nothing wrote the count back later: eight users arriving
//! across several batches inside one window left `users_seen = 1` for good
//! (found live in the MCP acceptance run, 2026-11-10).
//!
//! These tests drive both write paths (`process_batch` and the per-item
//! `process_job`) against a real Postgres and Redis, check that the gated
//! issues went into the pending set instead of into a write-back, then run
//! the flusher's tick function with an injected clock past the interval and
//! require the stored count to equal Redis' `PFCOUNT`.
//!
//! ## Why its own test binary, and why the lock
//!
//! The throttle is process-global by design (N workers share one budget), so
//! a drain with a future clock takes EVERY due pending issue in the process.
//! Run beside other DB tests it would drain their issues against this test's
//! database. A separate binary is a separate process, and `SERIAL` keeps the
//! two tests in this file from draining each other's issues.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use diesel::sql_types::{BigInt, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_core::envelope::{
    EnvelopeContext, EnvelopeItem, ErrorItem, EventUser, ExceptionInfo, IngestJob, Level,
};
use sauron_db::models::NewAppEnvironment;
use sauron_db::repo;
use sauron_pipeline::batch::{process_batch, Decoded};
use sauron_pipeline::mask::MaskSet;
use sauron_pipeline::users_seen;
use sauron_pipeline::SymbolizeCtx;
use sauron_redis::{keys, RedisStore};
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Throwaway database, same shape as `tests/identity_merge_batch.rs`'s (see
/// its doc comment for why it cannot be shared), with its own discriminator
/// ("us") so the ephemeral database names cannot collide.
struct TestDb {
    pool: sauron_db::PgPool,
    admin_url: String,
    db_name: String,
    cleaned_up: Cell<bool>,
}

impl TestDb {
    async fn setup() -> Option<Self> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let db_name = format!(
            "sauron_test_{}_us{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::create_test_database(&admin_url, &db_name)
            .await
            .expect("create migrated ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");
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
                "WARNING: ephemeral test database {} may remain (the test likely panicked). \
                 sauron-db's stale-db reaper collects it after 3h, or drop it manually:\n  \
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
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "us org", &format!("us-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "us project",
        &format!("us-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "us app",
        &format!("us-app-{suffix}"),
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
            public_key: &format!("pk_us_{suffix}"),
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

/// One error occurrence from `user`. The explicit fingerprint pins every
/// occurrence to one issue whatever the grouping rules do with the rest.
fn error_job(ids: &SeedIds, fingerprint: &str, user: &str) -> IngestJob {
    IngestJob {
        app_id: ids.app_id,
        project_id: ids.project_id,
        org_id: ids.org_id,
        environment_id: ids.environment_id,
        release: None,
        received_at: Utc::now(),
        ip: None,
        user_agent: None,
        context: EnvelopeContext::default(),
        sdk: None,
        item: EnvelopeItem::Error(Box::new(ErrorItem {
            event_id: Uuid::new_v4(),
            level: Level::Error,
            timestamp: Utc::now(),
            exception: Some(ExceptionInfo {
                ty: "TypeError".to_string(),
                value: Some("users_seen burst".to_string()),
                mechanism: None,
                stacktrace: vec![],
            }),
            message: Some("users_seen burst".to_string()),
            breadcrumbs: vec![],
            tags: serde_json::json!({}),
            contexts: serde_json::json!({}),
            extra: serde_json::json!({}),
            user: Some(EventUser {
                id: Some(user.to_string()),
                ..Default::default()
            }),
            session_id: None,
            screen: None,
            fingerprint: Some(vec![fingerprint.to_string()]),
            workflow_id: None,
            workflow_name: None,
            raw_stacktrace: None,
            debug_meta: None,
        })),
    }
}

fn decoded(job: IngestJob, n: usize) -> Decoded {
    Decoded {
        id: format!("0-{n}"),
        job,
        masks: Arc::new(MaskSet::from_rows(vec![])),
        entry_tail: true,
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
struct IssueRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = BigInt)]
    users_seen: i64,
}

async fn the_issue(pool: &sauron_db::PgPool, app_id: Uuid) -> IssueRow {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    let mut rows: Vec<IssueRow> =
        diesel::sql_query("SELECT id, users_seen FROM issues WHERE app_id = $1")
            .bind::<SqlUuid, _>(app_id)
            .load(&mut conn)
            .await
            .expect("read issues");
    assert_eq!(rows.len(), 1, "every occurrence must group into one issue");
    rows.remove(0)
}

/// The users of each burst, split the way the live repro arrived: one, then
/// the rest across several deliveries well inside one throttle window.
const BURST: &[&[&str]] = &[&["u1"], &["u2", "u3"], &["u4", "u5", "u6"], &["u7", "u8"]];

/// The batched write path: the one a deployment runs.
#[tokio::test]
async fn a_batched_burst_inside_one_window_converges_once_the_drain_runs() {
    let _serial = SERIAL.lock().await;
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
    let fp = format!("users-seen-batched-{}", Uuid::new_v4().simple());

    let started = Instant::now();
    let mut n = 0;
    for users in BURST {
        let batch: Vec<Decoded> = users
            .iter()
            .map(|u| {
                n += 1;
                decoded(error_job(&ids, &fp, u), n)
            })
            .collect();
        process_batch(&db.pool, &redis, &sym, &batch)
            .await
            .expect("batch");
    }
    let burst_took = started.elapsed();

    let issue = the_issue(&db.pool, ids.app_id).await;
    let key = keys::issue_users(&issue.id.to_string());
    let hll = redis.pf_count(&key).await.expect("PFCOUNT");
    assert_eq!(hll, 8, "Redis is authoritative and exact at this size");

    // The throttle, documented: only the first batch's write-back landed. The
    // other three changed the HLL while the gate was closed, so they must have
    // gone into the pending set — NOT into a write-back of their own. That is
    // the structural guard that the fix added no extra `issues` UPDATE to the
    // batch path for a gated issue.
    if burst_took < users_seen::WRITE_BACK_INTERVAL {
        assert_eq!(
            issue.users_seen, 1,
            "a gated batch must not write users_seen back itself"
        );
        assert!(
            users_seen::is_pending(issue.id),
            "a gated issue whose HLL moved must be recorded as pending"
        );
    } else {
        eprintln!("burst took {burst_took:?} (> one interval); throttle assertions skipped");
    }

    // The flusher's tick, with the clock injected one interval ahead.
    let flushed = users_seen::flush_due(
        &db.pool,
        &redis,
        Instant::now() + users_seen::WRITE_BACK_INTERVAL,
        usize::MAX,
    )
    .await
    .expect("flush");
    assert!(flushed >= 1, "the pending issue must be flushed");

    let after = the_issue(&db.pool, ids.app_id).await;
    assert_eq!(
        after.users_seen, hll,
        "users_seen must converge to the Redis PFCOUNT once the gate reopens"
    );
    assert!(
        !users_seen::is_pending(issue.id),
        "a flushed issue leaves the pending set"
    );

    let _ = redis.del(&key).await;
    db.cleanup().await;
}

/// The per-item write path (`INGEST_BATCH_WRITES=0`, and the fallback a
/// failed batch replays through) gets the same pending treatment.
#[tokio::test]
async fn a_per_item_burst_inside_one_window_converges_once_the_drain_runs() {
    let _serial = SERIAL.lock().await;
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
    let fp = format!("users-seen-per-item-{}", Uuid::new_v4().simple());

    let started = Instant::now();
    for user in BURST.iter().flat_map(|b| b.iter()) {
        sauron_pipeline::process_job(&db.pool, &redis, &sym, &masks, error_job(&ids, &fp, user))
            .await
            .expect("process_job");
    }
    let burst_took = started.elapsed();

    let issue = the_issue(&db.pool, ids.app_id).await;
    let key = keys::issue_users(&issue.id.to_string());
    let hll = redis.pf_count(&key).await.expect("PFCOUNT");
    assert_eq!(hll, 8);

    if burst_took < users_seen::WRITE_BACK_INTERVAL {
        assert_eq!(issue.users_seen, 1, "only the first occurrence writes back");
        assert!(
            users_seen::is_pending(issue.id),
            "the per-item path must record a gated issue as pending too"
        );
    } else {
        eprintln!("burst took {burst_took:?} (> one interval); throttle assertions skipped");
    }

    users_seen::flush_due(
        &db.pool,
        &redis,
        Instant::now() + users_seen::WRITE_BACK_INTERVAL,
        usize::MAX,
    )
    .await
    .expect("flush");

    let after = the_issue(&db.pool, ids.app_id).await;
    assert_eq!(after.users_seen, hll, "users_seen must converge to PFCOUNT");

    let _ = redis.del(&key).await;
    db.cleanup().await;
}

/// A restart drops the in-memory pending set (`sauron-ingest` has no graceful
/// shutdown). The flusher's first pass re-checks every issue an event landed
/// on shortly before startup, so an issue whose catch-up died with the old
/// process still converges without waiting for a new user.
#[tokio::test]
async fn the_startup_recheck_repairs_an_issue_whose_pending_entry_died_in_a_restart() {
    let _serial = SERIAL.lock().await;
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
    let fp = format!("users-seen-restart-{}", Uuid::new_v4().simple());

    // One batch with the gate open: users_seen = 1 is written.
    process_batch(
        &db.pool,
        &redis,
        &sym,
        &[decoded(error_job(&ids, &fp, "u1"), 1)],
    )
    .await
    .expect("batch");
    let issue = the_issue(&db.pool, ids.app_id).await;
    assert_eq!(issue.users_seen, 1);

    // The old process saw three more users and parked the issue, then died:
    // the HyperLogLog moved, and the pending entry that would have written it
    // back is gone.
    let key = keys::issue_users(&issue.id.to_string());
    redis
        .pf_add_many(&key, &["u2", "u3", "u4"])
        .await
        .expect("PFADD");
    assert!(!users_seen::is_pending(issue.id), "nothing remembers it");

    let seeded = users_seen::seed_recent(
        &db.pool,
        Utc::now() - chrono::Duration::from_std(users_seen::RESTART_RECHECK).unwrap(),
    )
    .await
    .expect("startup re-check");
    assert!(seeded >= 1, "the recently active issue is re-checked");
    assert!(users_seen::is_pending(issue.id));

    users_seen::flush_due(&db.pool, &redis, Instant::now(), usize::MAX)
        .await
        .expect("flush");
    let after = the_issue(&db.pool, ids.app_id).await;
    assert_eq!(
        after.users_seen, 4,
        "converged to PFCOUNT after the restart"
    );

    let _ = redis.del(&key).await;
    db.cleanup().await;
}
