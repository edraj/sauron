//! `sauron-storesync` — pulls daily install/uninstall counts from Google Play
//! and the Apple App Store.
//!
//! Same shape as `sauron-monitor`: claim due rows `FOR UPDATE SKIP LOCKED`,
//! fetch concurrently, persist, reschedule. One connection's failure is written
//! to that connection's `last_error` and touches nothing else — a store outage
//! for one tenant must not stall every other tenant's sync.
//!
//! Deliberately *not* on the API's critical path. Apple's report walk can take
//! minutes and Google's backfill downloads multiple megabytes; neither belongs
//! inside an HTTP request. The dashboard's "Queue sync" button only moves
//! `next_sync_at`, and this process does the work.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_alerts::SecretCipher;
use sauron_core::Config;
use sauron_db::models::AppStoreConnection;
use sauron_db::{repo, PgPool};
use sauron_store::{apple, google, AppleProgress, StoreKind};

/// How long the loop sleeps between claim passes. Not the sync interval —
/// `store_sync_interval_secs` is how far ahead a *claimed* row is pushed. This
/// is just how promptly a queued sync gets noticed.
const TICK: Duration = Duration::from_secs(60);

/// Rows claimed per pass.
const BATCH: i64 = 50;

/// Lookback for a connection that has synced before. Stores restate the last
/// few days, so a short window keeps those restatements current without
/// re-reading the whole backfill every tick.
const INCREMENTAL_LOOKBACK_DAYS: i64 = 7;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-storesync");
    let cfg = Arc::new(Config::from_env()?);
    let pool = sauron_db::build_pool(&cfg.database_url, cfg.store_sync_max_concurrency + 4)?;

    // Fail-closed, no JWT_SECRET derivation. A key mismatch here would surface
    // only as a stream of decrypt errors hours later, so it is a boot failure.
    let cipher = Arc::new(SecretCipher::new(cfg.require_notify_secret_key()?));

    // Prove the configured key can actually open what is stored, in the style
    // of the API's channel-secret self-test. A silently wrong key otherwise
    // looks exactly like "every store credential is invalid", and the operator
    // goes looking at Google and Apple rather than at their own env file.
    {
        let mut conn = sauron_db::conn(&pool).await?;
        if let Some(blob) = repo::any_store_secret_enc(&mut conn).await? {
            cipher.decrypt(&blob).map_err(|_| {
                anyhow::anyhow!(
                    "NOTIFY_SECRET_KEY cannot decrypt stored store credentials — \
                     refusing to start rather than reporting every connection as broken"
                )
            })?;
            info!("store credential key self-test passed");
        }
    }

    let http = reqwest::Client::builder()
        .user_agent("Sauron-StoreSync/1.0")
        .timeout(Duration::from_secs(120))
        .build()?;

    let sem = Arc::new(Semaphore::new(cfg.store_sync_max_concurrency));
    info!(
        interval_secs = cfg.store_sync_interval_secs,
        concurrency = cfg.store_sync_max_concurrency,
        backfill_days = cfg.store_backfill_days,
        "store sync started"
    );

    loop {
        match claim_and_sync(&pool, &http, &cipher, &cfg, &sem).await {
            Ok(0) => {}
            Ok(n) => info!(count = n, "store sync pass complete"),
            // A failure to CLAIM is a database problem, not a store problem;
            // log and keep ticking rather than exiting the daemon.
            Err(e) => warn!(error = %e, "store sync pass failed"),
        }
        tokio::time::sleep(TICK).await;
    }
}

async fn claim_and_sync(
    pool: &PgPool,
    http: &reqwest::Client,
    cipher: &Arc<SecretCipher>,
    cfg: &Arc<Config>,
    sem: &Arc<Semaphore>,
) -> anyhow::Result<usize> {
    let claimed = {
        let mut conn = sauron_db::conn(pool).await?;
        repo::claim_due_store_connections(&mut conn, BATCH, cfg.store_sync_interval_secs).await?
    };
    if claimed.is_empty() {
        return Ok(0);
    }

    let n = claimed.len();
    let mut tasks = Vec::with_capacity(n);
    for c in claimed {
        let permit = sem.clone().acquire_owned().await?;
        let (pool, http, cipher, cfg) = (pool.clone(), http.clone(), cipher.clone(), cfg.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            let id = c.id;
            let store = c.store.clone();
            if let Err(e) = sync_one(&pool, &http, &cipher, &cfg, c).await {
                warn!(connection_id = %id, store = %store, error = %e, "store sync failed");
                // Best-effort: if the database is also down there is nothing
                // useful left to record, and the next tick will retry.
                if let Ok(mut conn) = sauron_db::conn(&pool).await {
                    let _ = repo::record_store_sync_result(&mut conn, id, Some(&e.to_string()))
                        .await;
                }
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    Ok(n)
}

async fn sync_one(
    pool: &PgPool,
    http: &reqwest::Client,
    cipher: &SecretCipher,
    cfg: &Config,
    c: AppStoreConnection,
) -> anyhow::Result<()> {
    let kind = StoreKind::parse(&c.store)
        .ok_or_else(|| anyhow::anyhow!("unknown store {:?}", c.store))?;
    let blob = c
        .secret_enc
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no credential saved for this store"))?;
    let secret = cipher
        .decrypt_str(blob)
        .map_err(|_| anyhow::anyhow!("stored credential could not be decrypted"))?;

    let today = chrono::Utc::now().date_naive();
    let lookback = if c.last_synced_at.is_none() {
        cfg.store_backfill_days
    } else {
        INCREMENTAL_LOOKBACK_DAYS
    };
    let since = today - chrono::Duration::days(lookback);

    // Apple only: carried out of the match so the post-upsert bookkeeping below
    // can mark the connection as having produced data at least once. That flag
    // is what lets the API distinguish "still in Apple's 24-48h startup window"
    // from "syncing fine, this range happens to be empty".
    let mut apple_report_request_id: Option<String> = None;

    let metrics = match kind {
        StoreKind::GooglePlay => {
            let ids: google::GoogleIdentifiers = serde_json::from_value(c.identifiers.clone())
                .map_err(|e| anyhow::anyhow!("stored Google Play identifiers are invalid: {e}"))?;
            google::fetch(http, &ids, &secret, since, today).await?
        }
        StoreKind::AppStore => {
            let ids: apple::AppleIdentifiers = serde_json::from_value(c.identifiers.clone())
                .map_err(|e| anyhow::anyhow!("stored App Store identifiers are invalid: {e}"))?;
            let request_id = c
                .sync_state
                .get("report_request_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let (new_id, progress) =
                apple::fetch(http, &ids, &secret, request_id.as_deref(), since, today).await?;

            // Persist the report-request id the moment it exists. Creating a
            // second ONGOING request for the same app is wasteful and Apple
            // may reject it, so this must survive even if the walk below fails.
            if request_id.as_deref() != Some(new_id.as_str()) {
                let mut conn = sauron_db::conn(pool).await?;
                repo::set_store_sync_state(
                    &mut conn,
                    c.id,
                    &serde_json::json!({ "report_request_id": new_id }),
                )
                .await?;
            }

            match progress {
                // Apple's normal 24-48h startup window. Recorded as a CLEAN
                // sync with no rows, not as an error: a red badge here would
                // be wrong every time, and admins would learn to ignore it.
                AppleProgress::Pending => {
                    info!(connection_id = %c.id, "Apple report still pending");
                    let mut conn = sauron_db::conn(pool).await?;
                    repo::record_store_sync_result(&mut conn, c.id, None).await?;
                    return Ok(());
                }
                AppleProgress::Ready(m) => {
                    apple_report_request_id = Some(new_id);
                    m
                }
            }
        }
    };

    let rows: Vec<_> = metrics
        .iter()
        .map(|m| (m.day, m.installs, m.uninstalls))
        .collect();

    let mut conn = sauron_db::conn(pool).await?;
    repo::upsert_store_daily_metrics(&mut conn, c.app_id, kind.as_str(), &rows).await?;
    if let Some(request_id) = apple_report_request_id {
        // Apple has now produced real rows at least once, so the connection is
        // out of `pending` for good. Written together with the request id
        // because `set_store_sync_state` replaces the whole object.
        repo::set_store_sync_state(
            &mut conn,
            c.id,
            &serde_json::json!({ "report_request_id": request_id, "installs_seen": true }),
        )
        .await?;
    }
    repo::record_store_sync_result(&mut conn, c.id, None).await?;
    info!(connection_id = %c.id, store = %c.store, days = rows.len(), "store sync ok");
    Ok(())
}
