//! The BATCHED identify path enqueues merges too.
//!
//! `insert_identity` had two independent callers: `process::process_identify`
//! and the per-item identify loop at the tail of `batch::process_batch`. This
//! file exists because a test that only drove `process_identify` would pass
//! while the path an actual deployment uses — the batched worker — silently
//! never merged anything. Do NOT call `process_identify` from this file;
//! exercising the other code path is the entire point.
//!
//! ## Why a plain `TEST_REDIS_URL`, not `TEST_ISOLATED_REDIS_URL`
//!
//! `tests/retry_drain.rs` needs an isolated Redis because it drains parked
//! jobs onto the REAL `keys::INGEST_STREAM` — a shared instance would inject
//! synthetic payloads into a live ingest stream. `process_batch` never writes
//! to that stream, and the one-item `Identify`-only batch this file feeds it
//! never touches Redis at all (no breadcrumbs to push, no error occurrences to
//! feed the affected-user HyperLogLog). The connection below exists only to
//! satisfy `process_batch`'s signature, matching the same convention already
//! used by `batch.rs`'s own `equivalence_tests` module and by
//! `process.rs`'s `process_error_runs_the_same_identification_test_as_process_event`.

use std::cell::Cell;
use std::sync::Arc;

use chrono::Utc;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_core::envelope::{EnvelopeContext, EnvelopeItem, IdentifyItem, IngestJob};
use sauron_db::models::NewAppEnvironment;
use sauron_db::repo;
use sauron_pipeline::batch::{process_batch, Decoded};
use sauron_pipeline::mask::MaskSet;
use sauron_pipeline::SymbolizeCtx;
use sauron_redis::RedisStore;
use uuid::Uuid;

/// One throwaway database for this test, created/migrated/dropped here rather
/// than reusing `sauron-db`'s own `tests/common::TestDb` (private to that
/// crate's own integration-test binaries, so it cannot be named from
/// `sauron-pipeline`) or `sauron-pipeline`'s `cfg(test)`-only
/// `process::workflow_pipeline_tests::PipelineTestDb` (gated behind
/// `#[cfg(test)]` on the LIBRARY crate, which is not set when the library is
/// compiled as a dependency of an integration-test binary like this one — so
/// it is equally invisible here). Same database-name shape rules as
/// `PipelineTestDb` — see its own doc comment for why the timestamp must come
/// first and the discriminator must be glued to the uuid — with a distinct
/// two-letter discriminator ("ib") so the two harnesses' ephemeral database
/// names cannot collide.
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
            "sauron_test_{}_ib{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        // One migrated template, copied per test — see
        // `sauron_db::create_test_database`. Falls back to replaying the
        // migrations, so the resulting schema is identical either way.
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
        // Async work cannot run in `Drop`. Same tradeoff as
        // `PipelineTestDb`/`sauron-db`'s `common::TestDb` for the identical
        // reason: make a leak from a panicked test loud rather than attempt a
        // runtime-in-Drop workaround. Still reaper-collectable (see `setup`).
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

/// Same string-rewrite `sauron-db`'s own `tests/common::swap_database` and
/// `process.rs`'s `PipelineTestDb`-local copy do — duplicated rather than
/// imported for the same private-module-boundary reason as `TestDb` above.
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
    let org = repo::create_org(&mut conn, "ib org", &format!("ib-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "ib project",
        &format!("ib-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "ib app",
        &format!("ib-app-{suffix}"),
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
            public_key: &format!("pk_ib_{suffix}"),
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

fn identify_job(ids: &SeedIds, item: IdentifyItem) -> IngestJob {
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
        item: EnvelopeItem::Identify(item),
    }
}

async fn test_redis() -> Option<RedisStore> {
    let url = std::env::var("TEST_REDIS_URL").ok()?;
    RedisStore::connect(&url).await.ok()
}

async fn sym_ctx() -> SymbolizeCtx {
    // Same construction `process.rs`'s and `batch.rs`'s own DB-backed tests
    // use: a real (tiny) in-process symbolicator plus a disabled blob cache
    // (`None` url) — nothing in this file's fixture carries a stack trace, so
    // symbolication has no work to do and must not reach out to anything.
    SymbolizeCtx::new(
        Arc::new(sauron_symbols::Symbolicator::new(1 << 20)),
        sauron_redis::SymbolBlobCache::connect(None, 1 << 20).await,
        100,
        1 << 20,
    )
}

#[derive(diesel::QueryableByName)]
struct QueuedMerge {
    #[diesel(sql_type = Text)]
    alias_id: String,
    #[diesel(sql_type = Text)]
    distinct_id: String,
}

#[tokio::test]
async fn the_batched_identify_path_enqueues_a_merge() {
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

    let decoded = vec![Decoded {
        id: "0-1".into(),
        job: identify_job(
            &ids,
            IdentifyItem {
                distinct_id: "u-42".into(),
                anonymous_id: Some("anon_batched".into()),
                traits: serde_json::json!({}),
                timestamp: Utc::now(),
            },
        ),
        masks: Arc::new(MaskSet::from_rows(vec![])),
        entry_tail: true,
    }];

    process_batch(&db.pool, &redis, &sym, &decoded)
        .await
        .expect("batch");

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let rows: Vec<QueuedMerge> =
        diesel::sql_query("SELECT alias_id, distinct_id FROM identity_merges WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .load(&mut conn)
            .await
            .expect("queued merges");

    assert_eq!(
        rows.len(),
        1,
        "the batched path must enqueue exactly one merge"
    );
    assert_eq!(rows[0].alias_id, "anon_batched");
    assert_eq!(rows[0].distinct_id, "u-42");

    drop(conn);
    db.cleanup().await;
}

/// A repeat identify() through the batched path must NOT enqueue a second
/// merge — the batch twin of `process.rs`'s
/// `a_repeat_identify_does_not_enqueue_twice`, run as two separate batches so
/// it also proves the claim survives across process_batch calls, not just
/// across items folded within one.
#[tokio::test]
async fn a_repeat_batched_identify_does_not_enqueue_twice() {
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

    for i in 0..2 {
        let decoded = vec![Decoded {
            id: format!("0-{i}"),
            job: identify_job(
                &ids,
                IdentifyItem {
                    distinct_id: "u-42".into(),
                    anonymous_id: Some("anon_twice".into()),
                    traits: serde_json::json!({}),
                    timestamp: Utc::now(),
                },
            ),
            masks: Arc::new(MaskSet::from_rows(vec![])),
            entry_tail: true,
        }];
        process_batch(&db.pool, &redis, &sym, &decoded)
            .await
            .expect("batch");
    }

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let rows: Vec<QueuedMerge> =
        diesel::sql_query("SELECT alias_id, distinct_id FROM identity_merges WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .load(&mut conn)
            .await
            .expect("queued merges");

    assert_eq!(rows.len(), 1, "a repeat identify() must not enqueue twice");

    drop(conn);
    db.cleanup().await;
}
