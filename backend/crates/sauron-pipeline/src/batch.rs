//! Batched ingest: one Redis read's worth of jobs written as a handful of
//! statements instead of ~10 per event.
//!
//! [`crate::process::process_job`] is still the definition of what one signal
//! means — this module does exactly the same work, in the same order, but folds
//! the whole batch before touching Postgres. The shapes it produces
//! (`NewErrorEvent`, `NewIssue`, session/device deltas) are built by code
//! transcribed from `process.rs`; where a rule looks surprising, the comment
//! explaining it lives there.
//!
//! ## What is and is not batched
//!
//! Batched, because they dominate: issue grouping, the three event inserts,
//! session and device roll-ups, `event_users` touch and identification, the
//! HyperLogLog cardinality write-back, and workflow bumps.
//!
//! Workflow bumps joined that list late. They are not rare for an app that
//! tags its traffic — one per tagged item — so leaving them as sequential
//! autocommits meant such an app paid `batch_size` single-row upserts
//! immediately after everything else had been amortized, and the batched
//! write path bought it close to nothing.
//!
//! Left per-item, because they are rare enough that batching them would add
//! risk for no measurable gain: `identify()`, workflow lifecycle transitions
//! (an ordered state machine, unlike the commutative counter bumps), and
//! breadcrumb pushes (Redis-only anyway).
//!
//! ## Failure model
//!
//! A batch write is all-or-nothing per statement, so one malformed row would
//! take its neighbours down with it. [`crate::worker`] handles that by falling
//! back to the per-item path for the whole batch on any error — the poison
//! entry then dead-letters alone, exactly as before. That fallback is why this
//! module may return `Err` freely rather than trying to isolate bad rows
//! itself.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_core::envelope::{AnalyticsItem, ErrorItem, IngestJob, TransactionItem};
use sauron_core::{fingerprint, ids};
use sauron_db::batch as db;
use sauron_db::models::{NewAnalyticsEvent, NewErrorEvent, NewIssue, NewTransaction};
use sauron_db::{repo, PgPool};
use sauron_redis::{keys, RedisStore};

use crate::enrich::{device_info, enrich_context, DeviceInfo};
use crate::mask::MaskSet;
use crate::process::{
    build_culprit, build_title, distinct_id, handled_of, identified_column_present,
    object_or_empty, truncate,
};
use crate::symbolize::SymbolizeCtx;

/// One decoded, masked stream entry ready to be written.
pub struct Decoded {
    pub id: String,
    pub job: IngestJob,
    /// Shared because [`crate::mask::PolicyCache`] hands out an `Arc` — a batch
    /// of 50 entries for one app then holds one mask set, not 50 copies.
    pub masks: std::sync::Arc<MaskSet>,
    /// Whether this is the LAST item decoded from stream entry `id`.
    ///
    /// One entry now carries a whole envelope, so `id` is no longer unique
    /// within a batch. The ack is owed once per entry, after every item it
    /// carried has been written or dead-lettered — acking on the first would
    /// retire the entry while its siblings were still in flight, turning a
    /// crash from a redelivery into a silent partial loss.
    pub entry_tail: bool,
}

/// An error awaiting its `issue_id`. The row already carries `(app_id,
/// fingerprint)`, which is the key the issue upsert returns against, so no
/// separate correlation field is needed.
struct PendingError {
    row: NewErrorEvent,
    /// Kept out of the row because the HyperLogLog write-back needs it after
    /// `row` has been moved into the bulk insert.
    distinct: Option<String>,
}

/// A candidate `issues` row, owned so `NewIssue`'s borrows have somewhere to
/// point once the batch is folded.
struct IssueDraft {
    app_id: Uuid,
    fingerprint: String,
    type_: String,
    title: String,
    culprit: String,
    level: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    times_seen: i64,
}

struct Lifecycle {
    app_id: Uuid,
    environment_id: Uuid,
    workflow_id: String,
    workflow_name: String,
    action: repo::WorkflowAction,
    cancel_reason: Option<String>,
    session_id: Option<String>,
    distinct_id: Option<String>,
    at: DateTime<Utc>,
}

/// Everything one batch wants to write, folded.
#[derive(Default)]
struct Acc {
    issues: Vec<IssueDraft>,
    /// `(app_id, fingerprint)` → index into `issues`, so a repeat fingerprint
    /// folds into the existing draft rather than becoming a second row that
    /// `ON CONFLICT DO UPDATE` would reject.
    issue_at: HashMap<(Uuid, String), usize>,
    errors: Vec<PendingError>,
    analytics: Vec<NewAnalyticsEvent>,
    transactions: Vec<NewTransaction>,
    sessions: Vec<db::SessionBump>,
    session_at: HashMap<(Uuid, String), usize>,
    devices: Vec<db::DeviceBump>,
    device_at: HashMap<(Uuid, String), usize>,
    touch_users: Vec<(Uuid, String)>,
    touch_seen: HashSet<(Uuid, String)>,
    identified: Vec<(Uuid, String, &'static str)>,
    identified_seen: HashSet<(Uuid, String)>,
    workflows: Vec<db::WorkflowBump>,
    /// `(app_id, workflow_id)` → index into `workflows`, for the same
    /// `ON CONFLICT DO UPDATE` dedupe reason as `issue_at`.
    workflow_at: HashMap<(Uuid, String), usize>,
    lifecycles: Vec<Lifecycle>,
}

impl Acc {
    /// Fold one workflow signal, mirroring `bump_workflow`'s conflict arm.
    ///
    /// The `COALESCE`s there put the EXISTING row first
    /// (`COALESCE(workflows.session_id, EXCLUDED.session_id)`), so under the
    /// sequential upserts this replaces the *first* non-null value in a run of
    /// signals is the one that stuck and later ones never overwrote it. Hence
    /// `get_or_insert_with` rather than the unconditional assignment the
    /// session fold uses — the two folds look alike and disagree on purpose.
    #[allow(clippy::too_many_arguments)]
    fn workflow(
        &mut self,
        job: &IngestJob,
        workflow_id: &str,
        workflow_name: &str,
        session_id: Option<String>,
        distinct_id: Option<String>,
        device_key: Option<String>,
        at: DateTime<Utc>,
        events_delta: i32,
        errors_delta: i32,
    ) {
        let key = (job.app_id, workflow_id.to_string());
        match self.workflow_at.get(&key) {
            Some(&i) => {
                let b = &mut self.workflows[i];
                b.first_at = b.first_at.min(at);
                b.last_at = b.last_at.max(at);
                b.events_delta += events_delta;
                b.errors_delta += errors_delta;
                // First non-null wins; a later value must not displace one
                // already folded in. `NULLIF(name, '')` in the conflict arm
                // means an empty name counts as absent, so treat it that way.
                if b.workflow_name.is_empty() {
                    b.workflow_name = workflow_name.to_string();
                }
                // Assign only when still absent — and assign the incoming
                // `Option` as-is, so a `None` stays a NULL rather than
                // becoming an empty string, which `COALESCE` would treat as
                // a present value.
                if b.session_id.is_none() {
                    b.session_id = session_id;
                }
                if b.distinct_id.is_none() {
                    b.distinct_id = distinct_id;
                }
                if b.device_key.is_none() {
                    b.device_key = device_key;
                }
            }
            None => {
                self.workflow_at.insert(key, self.workflows.len());
                self.workflows.push(db::WorkflowBump {
                    app_id: job.app_id,
                    environment_id: job.environment_id,
                    workflow_id: workflow_id.to_string(),
                    workflow_name: workflow_name.to_string(),
                    session_id,
                    distinct_id,
                    device_key,
                    release: job.release.clone(),
                    first_at: at,
                    last_at: at,
                    events_delta,
                    errors_delta,
                });
            }
        }
    }

    /// Fold one signal into its session and device roll-ups. The batch twin of
    /// `process::rollup`, with the same "no session id / no device key → no
    /// row" rule.
    #[allow(clippy::too_many_arguments)]
    fn rollup(
        &mut self,
        job: &IngestJob,
        environment_id: Option<Uuid>,
        context: &Value,
        info: &DeviceInfo,
        session_id: Option<&str>,
        distinct_id: Option<&str>,
        at: DateTime<Utc>,
        events_delta: i64,
        errors_delta: i64,
    ) {
        let session_id = session_id.filter(|s| !s.is_empty());
        let distinct_id = distinct_id.filter(|s| !s.is_empty());

        if let Some(sid) = session_id {
            let key = (job.app_id, sid.to_string());
            match self.session_at.get(&key) {
                Some(&i) => {
                    let b = &mut self.sessions[i];
                    b.first_at = b.first_at.min(at);
                    b.last_at = b.last_at.max(at);
                    b.events_delta += events_delta;
                    b.errors_delta += errors_delta;
                    // `COALESCE(EXCLUDED.x, existing.x)` in the conflict arm
                    // means the last non-null of a sequence wins. Folding must
                    // reproduce that, so a later `None` never clears a value.
                    if distinct_id.is_some() {
                        b.distinct_id = distinct_id.map(str::to_string);
                    }
                    if info.device_key.is_some() {
                        b.device_key = info.device_key.clone();
                    }
                    // Matches the `CASE WHEN EXCLUDED.context <> '{}'` arm:
                    // an empty context never overwrites a populated one.
                    if !is_empty_object(context) {
                        b.context = context.clone();
                    }
                    if job.release.is_some() {
                        b.release = job.release.clone();
                    }
                    if environment_id.is_some() {
                        b.environment_id = environment_id;
                    }
                    if job.ip.is_some() {
                        b.ip = job.ip.clone();
                    }
                }
                None => {
                    self.session_at.insert(key, self.sessions.len());
                    self.sessions.push(db::SessionBump {
                        app_id: job.app_id,
                        session_id: sid.to_string(),
                        distinct_id: distinct_id.map(str::to_string),
                        device_key: info.device_key.clone(),
                        first_at: at,
                        last_at: at,
                        context: context.clone(),
                        release: job.release.clone(),
                        environment_id,
                        ip: job.ip.clone(),
                        events_delta,
                        errors_delta,
                    });
                }
            }
        }

        if let Some(dk) = info.device_key.as_deref() {
            let key = (job.app_id, dk.to_string());
            match self.device_at.get(&key) {
                Some(&i) => {
                    let b = &mut self.devices[i];
                    b.first_at = b.first_at.min(at);
                    b.last_at = b.last_at.max(at);
                    b.events_delta += events_delta;
                    b.errors_delta += errors_delta;
                    if distinct_id.is_some() {
                        b.distinct_id = distinct_id.map(str::to_string);
                    }
                    if info.family.is_some() {
                        b.family = info.family.clone();
                    }
                    if info.model.is_some() {
                        b.model = info.model.clone();
                    }
                    if info.os_name.is_some() {
                        b.os_name = info.os_name.clone();
                    }
                    if info.os_version.is_some() {
                        b.os_version = info.os_version.clone();
                    }
                    if info.arch.is_some() {
                        b.arch = info.arch.clone();
                    }
                    if info.browser.is_some() {
                        b.browser = info.browser.clone();
                    }
                }
                None => {
                    self.device_at.insert(key, self.devices.len());
                    self.devices.push(db::DeviceBump {
                        app_id: job.app_id,
                        device_key: dk.to_string(),
                        family: info.family.clone(),
                        model: info.model.clone(),
                        os_name: info.os_name.clone(),
                        os_version: info.os_version.clone(),
                        arch: info.arch.clone(),
                        browser: info.browser.clone(),
                        distinct_id: distinct_id.map(str::to_string),
                        first_at: at,
                        last_at: at,
                        events_delta,
                        errors_delta,
                    });
                }
            }
        }
    }

    fn touch_user(&mut self, app_id: Uuid, did: &str) {
        if did.is_empty() {
            return;
        }
        let key = (app_id, did.to_string());
        if self.touch_seen.insert(key.clone()) {
            self.touch_users.push(key);
        }
    }

    fn identify_user(&mut self, app_id: Uuid, did: &str, source: &'static str) {
        if did.is_empty() {
            return;
        }
        let key = (app_id, did.to_string());
        if self.identified_seen.insert(key) {
            self.identified.push((app_id, did.to_string(), source));
        }
    }

    fn fold_issue(&mut self, d: IssueDraft) {
        let key = (d.app_id, d.fingerprint.clone());
        match self.issue_at.get(&key) {
            Some(&i) => {
                let e = &mut self.issues[i];
                e.first_seen = e.first_seen.min(d.first_seen);
                e.last_seen = e.last_seen.max(d.last_seen);
                e.times_seen += d.times_seen;
                // Last occurrence wins, as it does when N single-row upserts
                // run in sequence and each overwrites with `excluded.*`.
                e.type_ = d.type_;
                e.title = d.title;
                e.culprit = d.culprit;
                e.level = d.level;
            }
            None => {
                self.issue_at.insert(key, self.issues.len());
                self.issues.push(d);
            }
        }
    }
}

/// How long an issue's `users_seen` may go without a Postgres write-back.
///
/// `users_seen` is a HyperLogLog *estimate* whose authoritative copy lives in
/// Redis; Postgres holds a denormalized copy so the Issues page can sort by it.
/// Writing that copy on every occurrence put an `UPDATE issues` in the hot path
/// of every error event, and it deadlocked against the issue upsert — two
/// statements touching the same rows, one ordered by `(app_id, fingerprint)`
/// and one by `id`, which no amount of sorting can reconcile. Measured on an
/// 8-worker ingest against 5 fingerprints, nearly every batch lost that race,
/// rolled back, and replayed item-by-item: the write path ran ~14x slower than
/// with the write-back removed entirely.
///
/// Throttling is what makes the statement rare instead of constant. The
/// estimate in Redis stays exact and immediate; only the denormalized copy
/// lags, by at most this long.
const USERS_SEEN_WRITE_BACK: Duration = Duration::from_secs(5);

/// Last Postgres write-back per issue, shared by every worker in the process.
///
/// Process-global rather than per-worker so N workers throttle to N-per-interval
/// combined rather than N-per-interval each.
fn users_seen_gate() -> &'static Mutex<HashMap<Uuid, Instant>> {
    static GATE: OnceLock<Mutex<HashMap<Uuid, Instant>>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `issue_id` is due a `users_seen` write-back, stamping it if so.
pub(crate) fn users_seen_due(issue_id: Uuid) -> bool {
    let mut gate = match users_seen_gate().lock() {
        Ok(g) => g,
        // A panic elsewhere poisoned it. Writing the estimate is best-effort,
        // so recover rather than propagate a panic into the write path.
        Err(p) => p.into_inner(),
    };
    let now = Instant::now();
    match gate.get(&issue_id) {
        Some(at) if now.duration_since(*at) < USERS_SEEN_WRITE_BACK => false,
        _ => {
            // Bounded so a long-lived process tracking many fingerprints cannot
            // grow this without limit. Clearing wholesale (rather than evicting
            // an LRU) costs at most one extra write-back per issue.
            if gate.len() > 50_000 {
                gate.clear();
            }
            gate.insert(issue_id, now);
            true
        }
    }
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(|m| m.is_empty())
}

/// Write a whole batch. Returns `Err` on the first statement that fails; the
/// caller is expected to retry the batch item-by-item (see the module docs).
pub async fn process_batch(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &SymbolizeCtx,
    decoded: &[Decoded],
) -> anyhow::Result<()> {
    let mut acc = Acc::default();
    // Items whose whole implementation is already a single cheap call and which
    // do not fold: run them on the per-item path after the bulk writes.
    let mut identifies: Vec<&IngestJob> = Vec::new();

    // --- stage 1: prepare. No pooled connection is held here, because
    // symbolication checks out its own and a batch of 50 would otherwise pin
    // 50 slots of a pool sized for the worker count.
    for d in decoded {
        let job = &d.job;
        let environment_id = Some(job.environment_id);
        let mut context = enrich_context(job);
        crate::mask::apply_context(&d.masks, &mut context);
        let info = device_info(&context);

        // `item` is cloned rather than borrowed because the `prepare_*` calls
        // consume it — exactly what `process::process_job` already does, so
        // this costs the batch path nothing the per-item path did not pay.
        match job.item.clone() {
            sauron_core::EnvelopeItem::Error(e) => {
                prepare_error(
                    pool,
                    sym,
                    &mut acc,
                    job,
                    environment_id,
                    &context,
                    &info,
                    *e,
                )
                .await;
            }
            sauron_core::EnvelopeItem::Event(ev) => {
                prepare_event(&mut acc, job, environment_id, &context, &info, ev);
            }
            sauron_core::EnvelopeItem::Transaction(t) => {
                prepare_transaction(&mut acc, job, environment_id, &context, &info, t);
            }
            sauron_core::EnvelopeItem::Identify(_) => identifies.push(job),
            sauron_core::EnvelopeItem::BreadcrumbBatch(b) => {
                // Redis-only and already one round trip; nothing to fold.
                if let Some(distinct) = b.distinct_id.filter(|s| !s.is_empty()) {
                    let key = keys::breadcrumbs(&job.app_id.to_string(), &distinct);
                    let json =
                        serde_json::to_string(&b.breadcrumbs).unwrap_or_else(|_| "[]".into());
                    let _ = redis.push_breadcrumbs(&key, &json, 100, 1800).await;
                }
            }
        }
    }

    let mut conn = sauron_db::conn(pool).await?;

    // --- stage 2: group into issues, then stamp the ids onto the error rows.
    if !acc.issues.is_empty() {
        let rows: Vec<NewIssue<'_>> = acc
            .issues
            .iter()
            .map(|d| NewIssue {
                app_id: d.app_id,
                fingerprint: &d.fingerprint,
                type_: &d.type_,
                title: &d.title,
                culprit: &d.culprit,
                level: &d.level,
                first_seen: d.first_seen,
                last_seen: d.last_seen,
                times_seen: d.times_seen,
            })
            .collect();
        let ids = db::upsert_issues(&mut conn, &rows).await?;
        let map: HashMap<(Uuid, String), Uuid> = ids
            .into_iter()
            .map(|(app, fp, id)| ((app, fp), id))
            .collect();
        for p in &mut acc.errors {
            let key = (p.row.app_id, p.row.fingerprint.clone());
            // An error row whose fingerprint did not come back means the upsert
            // silently skipped it — treat that as a batch failure rather than
            // writing an event pointing at a nil issue.
            let id = map.get(&key).copied().ok_or_else(|| {
                anyhow::anyhow!("issue id missing for fingerprint {}", p.row.fingerprint)
            })?;
            p.row.issue_id = id;
        }
    }

    // --- stage 3: every row this batch writes, in one transaction. The
    // reasoning (one WAL flush instead of seven; and a clean slate for the
    // worker's item-by-item replay) is on `sauron_db::batch::write_rows`.
    //
    // The issue upsert above is deliberately outside it: holding those row
    // locks across five more statements would serialize every worker touching
    // the same fingerprint, which is the opposite of the goal. The cost is that
    // a rolled-back batch has already bumped `issues.times_seen`, so the replay
    // counts those occurrences twice — an inflated counter on an error path,
    // far cheaper than duplicated event rows.
    let hll: Vec<(Uuid, String)> = acc
        .errors
        .iter()
        .filter_map(|p| p.distinct.clone().map(|d| (p.row.issue_id, d)))
        .collect();
    let error_rows: Vec<NewErrorEvent> = acc.errors.into_iter().map(|p| p.row).collect();
    // Probed before the transaction opens, and resolved to an empty slice when
    // the column is absent: an RPM upgrade can run this binary against a schema
    // that predates `identified_at`, and letting that statement fail would roll
    // back every event in the batch rather than just skipping one optional
    // feature.
    let identified: &[(Uuid, String, &'static str)] =
        if !acc.identified.is_empty() && identified_column_present(&mut conn).await {
            &acc.identified
        } else {
            &[]
        };
    db::write_rows(
        &mut conn,
        db::WriteSet {
            errors: &error_rows,
            analytics: &acc.analytics,
            transactions: &acc.transactions,
            sessions: &acc.sessions,
            devices: &acc.devices,
            touch_users: &acc.touch_users,
            identified,
        },
    )
    .await?;

    // --- stage 6: affected-user cardinality. One PFADD per distinct ISSUE,
    // PFCOUNT likewise, and the write-back is one statement.
    if !hll.is_empty() {
        // Grouped rather than iterated: `PFADD` is variadic, and its reply — did
        // any register move — is already per-key, so N members for one issue
        // answer the same question in one round trip that N round trips did.
        // With the fingerprint concentration a real app shows, this is the
        // difference between one Redis wait per error event and one per issue.
        let mut order: Vec<Uuid> = Vec::new();
        let mut members: HashMap<Uuid, Vec<&str>> = HashMap::new();
        for (issue_id, did) in &hll {
            members
                .entry(*issue_id)
                .or_insert_with(|| {
                    // First sighting drives `order`, so the issues are visited
                    // in batch order and not in HashMap order — the write-back
                    // below touches the same rows as the issue upsert, and
                    // stable ordering is what keeps the two from interleaving.
                    order.push(*issue_id);
                    Vec::new()
                })
                .push(did.as_str());
        }
        let mut issues_touched: Vec<Uuid> = Vec::new();
        for issue_id in order {
            let key = keys::issue_users(&issue_id.to_string());
            // Answers true only when the estimate moved, so a batch in which
            // every person has been seen before does no `issues` write at all —
            // which is the steady state, and the difference between this
            // statement being rare and it being on every batch.
            if redis
                .pf_add_many(&key, &members[&issue_id])
                .await
                .unwrap_or(false)
            {
                issues_touched.push(issue_id);
            }
        }
        let mut counts: Vec<(Uuid, i64)> = Vec::with_capacity(issues_touched.len());
        for issue_id in issues_touched {
            if !users_seen_due(issue_id) {
                continue;
            }
            if let Ok(n) = redis
                .pf_count(&keys::issue_users(&issue_id.to_string()))
                .await
            {
                counts.push((issue_id, n));
            }
        }
        if let Err(e) = db::set_issue_users_seen(&mut conn, &counts).await {
            tracing::warn!(error = %e, "batched users_seen write-back failed");
        }
    }

    // --- stage 7: the per-item tail. Rare paths, left unbatched on purpose.
    // Folded, not replayed: this was one autocommit round trip per item
    // carrying a workflow tag, so for an app that tags every event the batched
    // write path above bought nothing — it paid `batch_size` individual
    // upserts right after amortizing everything else.
    if let Err(e) = db::bump_workflows(&mut conn, &acc.workflows).await {
        tracing::warn!(error = %e, "batched workflow bump failed");
    }
    for l in &acc.lifecycles {
        let _ = repo::apply_workflow_lifecycle(
            &mut conn,
            l.app_id,
            l.environment_id,
            &l.workflow_id,
            &l.workflow_name,
            l.action,
            l.cancel_reason.as_deref(),
            l.session_id.as_deref(),
            l.distinct_id.as_deref(),
            l.at,
        )
        .await;
    }
    for job in identifies {
        if let sauron_core::EnvelopeItem::Identify(id) = job.item.clone() {
            let traits = object_or_empty(id.traits);
            // Deliberately NOT `?`. Every row this batch writes was committed by
            // `write_rows` above, and the worker's reaction to an `Err` from
            // here is to replay the WHOLE batch item-by-item — which would
            // insert every one of those already-durable events a second time
            // (each carries a fresh uuid_v7, so nothing dedupes them). Losing
            // one identify()'s traits is the far smaller failure, and it is the
            // same call every one of its neighbours in this loop already makes.
            if let Err(e) =
                repo::upsert_event_user(&mut conn, job.app_id, &id.distinct_id, &traits).await
            {
                tracing::warn!(app_id = %job.app_id, error = %e, "identify() traits were not stored");
            }
            if !id.distinct_id.is_empty() && identified_column_present(&mut conn).await {
                if let Err(e) = repo::mark_event_user_identified(
                    &mut conn,
                    job.app_id,
                    &id.distinct_id,
                    repo::IDENTIFIED_SOURCE_IDENTIFY,
                )
                .await
                {
                    tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
                }
            }
            if let Some(anon) = id.anonymous_id {
                if !anon.is_empty() {
                    let _ =
                        repo::insert_identity(&mut conn, job.app_id, &anon, &id.distinct_id).await;
                }
            }
        }
    }

    Ok(())
}

/// Transcribed from `process::process_error`, minus the two connection
/// checkouts and with every write turned into a fold. Symbolication still runs
/// here, unchanged and still time-boxed.
#[allow(clippy::too_many_arguments)]
async fn prepare_error(
    pool: &PgPool,
    sym: &SymbolizeCtx,
    acc: &mut Acc,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: &Value,
    info: &DeviceInfo,
    e: ErrorItem,
) {
    let exc = e.exception.as_ref();
    let fp = fingerprint(exc, e.message.as_deref(), e.fingerprint.as_deref());
    let (exception_type, exception_value) = match exc {
        Some(x) => (x.ty.clone(), x.value.clone().unwrap_or_default()),
        None => (String::new(), String::new()),
    };
    let title = build_title(exc, e.message.as_deref());
    let culprit = build_culprit(exc);
    let level = e.level.as_str();
    let now = e.timestamp;
    let device_key = info.device_key.clone();

    acc.fold_issue(IssueDraft {
        app_id: job.app_id,
        fingerprint: fp.clone(),
        type_: exception_type.clone(),
        title: title.clone(),
        culprit: culprit.clone(),
        level: level.to_string(),
        first_seen: now,
        last_seen: now,
        times_seen: 1,
    });

    let user = e.user.as_ref().or(job.context.user.as_ref());
    let distinct = distinct_id(user);
    let context_user_matches = user
        .and_then(|u| u.id.as_deref())
        .is_some_and(|id| !id.is_empty() && Some(id) == distinct.as_deref());
    let event_user = user.and_then(|u| serde_json::to_value(u).ok());
    let stacktrace = exc
        .map(|x| serde_json::to_value(&x.stacktrace).unwrap_or_else(|_| json!([])))
        .unwrap_or_else(|| json!([]));

    let (stacktrace_symbolicated, symbolication_status, debug_meta) =
        if let Some(raw_trace) = e.raw_stacktrace.as_deref() {
            let dm = crate::symbolize::build_debug_meta(e.debug_meta.as_ref(), raw_trace);
            let (frames, status) = crate::symbolize::symbolicate_ingest_dart(
                pool,
                sym,
                job.app_id,
                raw_trace,
                e.debug_meta.as_ref(),
            )
            .await;
            (frames, status, Some(dm))
        } else {
            let raw_frames = exc.map(|x| x.stacktrace.as_slice()).unwrap_or(&[]);
            let (frames, status) = crate::symbolize::symbolicate_ingest(
                pool,
                sym,
                job.app_id,
                job.release.as_deref(),
                raw_frames,
            )
            .await;
            (frames, status, None)
        };

    acc.errors.push(PendingError {
        row: NewErrorEvent {
            id: ids::uuid_v7(),
            app_id: job.app_id,
            environment_id,
            // Filled in stage 2 from the issue upsert's RETURNING.
            issue_id: Uuid::nil(),
            fingerprint: fp,
            level: level.to_string(),
            message: e.message.clone().unwrap_or_else(|| exception_value.clone()),
            exception_type,
            exception_value,
            stacktrace,
            breadcrumbs: serde_json::to_value(&e.breadcrumbs).unwrap_or_else(|_| json!([])),
            context: context.clone(),
            tags: object_or_empty(e.tags.clone()),
            contexts: object_or_empty(e.contexts.clone()),
            extra: object_or_empty(e.extra.clone()),
            release: job.release.clone(),
            distinct_id: distinct.clone(),
            event_user,
            sdk: job.sdk.as_ref().and_then(|s| serde_json::to_value(s).ok()),
            ip_address: job.ip.clone(),
            occurred_at: now,
            session_id: e.session_id.clone(),
            device_key,
            screen: e.screen.clone(),
            workflow_id: e.workflow_id.clone(),
            workflow_name: e.workflow_name.clone(),
            stacktrace_symbolicated,
            symbolication_status,
            debug_meta,
            handled: handled_of(exc),
            title: Some(title),
            culprit: Some(culprit),
        },
        distinct: distinct.clone(),
    });

    acc.rollup(
        job,
        environment_id,
        context,
        info,
        e.session_id.as_deref(),
        distinct.as_deref(),
        now,
        0,
        1,
    );

    if let (Some(wf_id), Some(wf_name)) = (e.workflow_id.as_deref(), e.workflow_name.as_deref()) {
        acc.workflow(
            job,
            wf_id,
            wf_name,
            e.session_id.clone(),
            distinct.clone(),
            info.device_key.clone(),
            now,
            0,
            1,
        );
    }

    if let Some(did) = distinct {
        acc.touch_user(job.app_id, &did);
        if context_user_matches {
            acc.identify_user(job.app_id, &did, repo::IDENTIFIED_SOURCE_CONTEXT_USER);
        }
    }
}

/// Transcribed from `process::process_event`.
fn prepare_event(
    acc: &mut Acc,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: &Value,
    info: &DeviceInfo,
    ev: AnalyticsItem,
) {
    let at = ev.timestamp;
    let session_id = ev.session_id.clone();
    let distinct_id = ev.distinct_id.clone();
    let workflow_id = ev.workflow_id.clone();
    let workflow_name = ev.workflow_name.clone();

    let action = match ev.name.as_str() {
        "$workflow_start" => Some(repo::WorkflowAction::Start),
        "$workflow_end" => Some(repo::WorkflowAction::End),
        "$workflow_cancel" => Some(repo::WorkflowAction::Cancel),
        _ => None,
    };
    let properties_snapshot = action.is_some().then(|| ev.properties.clone());

    acc.analytics.push(NewAnalyticsEvent {
        id: ids::uuid_v7(),
        app_id: job.app_id,
        environment_id,
        name: ev.name,
        distinct_id: ev.distinct_id.clone(),
        properties: object_or_empty(ev.properties),
        context: context.clone(),
        session_id: ev.session_id,
        release: job.release.clone(),
        ip_address: job.ip.clone(),
        occurred_at: ev.timestamp,
        device_key: info.device_key.clone(),
        screen: ev.screen.clone(),
        workflow_id: workflow_id.clone(),
        workflow_name: workflow_name.clone(),
        tags: object_or_empty(ev.tags),
        contexts: object_or_empty(ev.contexts),
        extra: object_or_empty(ev.extra),
    });

    acc.rollup(
        job,
        environment_id,
        context,
        info,
        session_id.as_deref(),
        Some(distinct_id.as_str()),
        at,
        1,
        0,
    );

    if let (Some(wf_id), Some(wf_name)) = (workflow_id.as_deref(), workflow_name.as_deref()) {
        acc.workflow(
            job,
            wf_id,
            wf_name,
            session_id.clone(),
            Some(distinct_id.clone()).filter(|s| !s.is_empty()),
            info.device_key.clone(),
            at,
            1,
            0,
        );
    }

    if !distinct_id.is_empty() {
        acc.touch_user(job.app_id, &distinct_id);
        let context_user_matches = job
            .context
            .user
            .as_ref()
            .and_then(|u| u.id.as_deref())
            .is_some_and(|id| !id.is_empty() && id == distinct_id);
        if context_user_matches {
            acc.identify_user(
                job.app_id,
                &distinct_id,
                repo::IDENTIFIED_SOURCE_CONTEXT_USER,
            );
        }
    }

    if let Some(action) = action {
        let prop = |key: &str| {
            properties_snapshot
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
        };
        let resolved_id = workflow_id
            .clone()
            .or_else(|| prop("workflow_id").map(str::to_string));
        if let Some(wf_id) = resolved_id {
            let resolved_name = workflow_name
                .clone()
                .or_else(|| prop("workflow_name").map(str::to_string))
                .unwrap_or_default();
            let cancel_reason = (action == repo::WorkflowAction::Cancel)
                .then(|| prop("reason").map(|r| truncate(r, 120).to_string()))
                .flatten();
            acc.lifecycles.push(Lifecycle {
                app_id: job.app_id,
                environment_id: job.environment_id,
                workflow_id: wf_id,
                workflow_name: resolved_name,
                action,
                cancel_reason,
                session_id: session_id.clone(),
                distinct_id: Some(distinct_id.clone()).filter(|s| !s.is_empty()),
                at,
            });
        }
    }
}

/// Transcribed from `process::process_transaction`.
fn prepare_transaction(
    acc: &mut Acc,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: &Value,
    info: &DeviceInfo,
    t: TransactionItem,
) {
    let at = t.timestamp;
    let distinct = t.distinct_id.clone();
    let session_id = t.session_id.clone();
    let workflow_id = t.workflow_id.clone();
    let workflow_name = t.workflow_name.clone();

    acc.transactions.push(NewTransaction {
        id: ids::uuid_v7(),
        app_id: job.app_id,
        environment_id,
        name: t.name,
        op: t.op,
        duration_ms: t.duration_ms,
        status: t.status,
        http_method: t.http_method,
        http_status: t.http_status,
        url: t.url,
        distinct_id: t.distinct_id,
        session_id: t.session_id,
        device_key: info.device_key.clone(),
        workflow_id: workflow_id.clone(),
        workflow_name: workflow_name.clone(),
        release: job.release.clone(),
        ip_address: job.ip.clone(),
        occurred_at: t.timestamp,
    });

    acc.rollup(
        job,
        environment_id,
        context,
        info,
        session_id.as_deref(),
        distinct.as_deref(),
        at,
        0,
        0,
    );

    if let (Some(wf_id), Some(wf_name)) = (workflow_id.as_deref(), workflow_name.as_deref()) {
        acc.workflow(
            job,
            wf_id,
            wf_name,
            session_id.clone(),
            distinct.clone(),
            info.device_key.clone(),
            at,
            0,
            0,
        );
    }
}

#[cfg(test)]
mod equivalence_tests {
    //! The batch path must produce byte-for-byte the same durable state as the
    //! per-item path. Reviewing the transcription cannot establish that — the
    //! interesting failures are in the *folding*, where a batch collapses rows
    //! the sequential path would have written one at a time (summed counters,
    //! `LEAST`/`GREATEST` timestamps, last-non-null-wins descriptors).
    //!
    //! So: seed two identical apps, feed both the same signals, write one
    //! through `process_job` and the other through `process_batch`, and diff
    //! the resulting rows.

    use super::{process_batch, Decoded};
    use crate::mask::MaskSet;
    use crate::process::workflow_pipeline_tests::PipelineTestDb;
    use chrono::round::SubsecRound;
    use chrono::{DateTime, Duration, Utc};
    use diesel::sql_types::{BigInt, Integer, Nullable, Text, Timestamptz};
    use diesel_async::RunQueryDsl;
    use sauron_core::envelope::{
        AnalyticsItem, EnvelopeContext, EnvelopeItem, ErrorItem, EventUser, ExceptionInfo, Frame,
        IngestJob, Level, TransactionItem,
    };
    use sauron_db::models::NewAppEnvironment;
    use sauron_db::repo;
    use sauron_redis::RedisStore;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    struct App {
        app_id: Uuid,
        project_id: Uuid,
        org_id: Uuid,
        environment_id: Uuid,
    }

    async fn seed_app(db: &PipelineTestDb, tag: &str) -> App {
        let mut conn = db.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut conn, "eq org", &format!("eq-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "eq project",
            &format!("eq-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            tag,
            &format!("eq-app-{tag}-{suffix}"),
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
                public_key: &format!("pk_eq_{tag}_{suffix}"),
                is_default: true,
            }],
        )
        .await
        .expect("enroll")
        .remove(0)
        .id;
        App {
            app_id: app.id,
            project_id: project.id,
            org_id: org.id,
            environment_id,
        }
    }

    fn job(app: &App, at: DateTime<Utc>, item: EnvelopeItem) -> IngestJob {
        IngestJob {
            app_id: app.app_id,
            project_id: app.project_id,
            org_id: app.org_id,
            environment_id: app.environment_id,
            release: Some("1.0.0".to_string()),
            received_at: at,
            ip: Some("203.0.113.7".to_string()),
            user_agent: Some(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"
                    .to_string(),
            ),
            sdk: None,
            context: EnvelopeContext {
                user: Some(EventUser {
                    id: Some("person-1".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            item,
        }
    }

    fn error_item(msg: &str, ty: &str, session: &str, at: DateTime<Utc>) -> EnvelopeItem {
        EnvelopeItem::Error(Box::new(ErrorItem {
            event_id: Uuid::new_v4(),
            level: Level::Error,
            timestamp: at,
            exception: Some(ExceptionInfo {
                ty: ty.to_string(),
                value: Some(msg.to_string()),
                mechanism: None,
                stacktrace: vec![Frame {
                    function: Some("boom".to_string()),
                    module: None,
                    filename: Some("app.js".to_string()),
                    abs_path: None,
                    lineno: Some(12),
                    colno: Some(3),
                    in_app: Some(true),
                }],
            }),
            message: Some(msg.to_string()),
            breadcrumbs: vec![],
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
            user: Some(EventUser {
                id: Some("person-1".to_string()),
                ..Default::default()
            }),
            session_id: Some(session.to_string()),
            screen: None,
            fingerprint: None,
            workflow_id: None,
            workflow_name: None,
            raw_stacktrace: None,
            debug_meta: None,
        }))
    }

    fn event_item(name: &str, session: &str, at: DateTime<Utc>) -> EnvelopeItem {
        EnvelopeItem::Event(AnalyticsItem {
            name: name.to_string(),
            distinct_id: "person-1".to_string(),
            properties: json!({"a": 1}),
            timestamp: at,
            session_id: Some(session.to_string()),
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        })
    }

    fn tx_item(name: &str, session: &str, at: DateTime<Utc>) -> EnvelopeItem {
        EnvelopeItem::Transaction(TransactionItem {
            name: name.to_string(),
            op: "http.server".to_string(),
            duration_ms: 12.5,
            status: Some("ok".to_string()),
            http_method: Some("GET".to_string()),
            http_status: Some(200),
            url: Some("https://example.test/x".to_string()),
            distinct_id: Some("person-1".to_string()),
            session_id: Some(session.to_string()),
            timestamp: at,
            workflow_id: None,
            workflow_name: None,
            finished_at: None,
        })
    }

    /// Stamp a workflow tag onto an item, whatever kind it is.
    ///
    /// A setter rather than three more constructor parameters so the existing
    /// untagged fixtures stay untouched — the point of the workflow rows is
    /// that they sit ALONGSIDE untagged traffic, since a fold that leaked
    /// across the two would not show up if everything were tagged.
    fn tagged(item: EnvelopeItem, id: &str, name: &str) -> EnvelopeItem {
        match item {
            EnvelopeItem::Error(mut e) => {
                e.workflow_id = Some(id.to_string());
                e.workflow_name = Some(name.to_string());
                EnvelopeItem::Error(e)
            }
            EnvelopeItem::Event(mut a) => {
                a.workflow_id = Some(id.to_string());
                a.workflow_name = Some(name.to_string());
                EnvelopeItem::Event(a)
            }
            EnvelopeItem::Transaction(mut t) => {
                t.workflow_id = Some(id.to_string());
                t.workflow_name = Some(name.to_string());
                EnvelopeItem::Transaction(t)
            }
            other => other,
        }
    }

    /// The signals both paths receive. Deliberately exercises every fold:
    /// two occurrences of ONE fingerprint plus a third of another (issue
    /// `times_seen` summing), three signals on ONE session (counter summing
    /// and the `LEAST`/`GREATEST` timestamp pair, fed out of chronological
    /// order so a fold that just takes the last value is caught), and one
    /// person seen repeatedly (`event_users` dedupe).
    fn signals(app: &App, t0: DateTime<Utc>) -> Vec<IngestJob> {
        vec![
            job(app, t0, error_item("boom", "TypeError", "sess-a", t0)),
            job(
                app,
                t0,
                event_item("page_view", "sess-a", t0 + Duration::seconds(30)),
            ),
            // Older than the first signal on purpose: `started_at` must move
            // BACKWARD to here, which a "last write wins" fold would miss.
            job(
                app,
                t0,
                error_item("boom", "TypeError", "sess-a", t0 - Duration::seconds(90)),
            ),
            job(
                app,
                t0,
                error_item("other", "RangeError", "sess-b", t0 + Duration::seconds(5)),
            ),
            job(
                app,
                t0,
                tx_item("GET /x", "sess-a", t0 + Duration::seconds(60)),
            ),
            // Three signals on ONE workflow, in this order on purpose:
            //
            //   1. an error on sess-w1, 45s AFTER t0
            //   2. an event on sess-w2, 120s BEFORE t0
            //   3. a transaction on sess-w2, 200s AFTER t0
            //
            // Their own sessions, not sess-a/sess-b, so the session
            // assertions above keep measuring the fixture they were written
            // for rather than quietly absorbing these.
            //
            // `bump_workflow`'s conflict arm reads
            // `COALESCE(workflows.session_id, EXCLUDED.session_id)` — the row's
            // own value first — so under sequential upserts sess-a wins and
            // sess-b never displaces it. A fold copied from the session one
            // (where `EXCLUDED` comes first, so the LAST value wins) would
            // land sess-b here and nothing else in this test would notice.
            //
            // Signal 2 also sits before signal 1 in time, so `started_at` has
            // to move backward, and signal 3 is the only one that can supply
            // `last_event_at`.
            job(
                app,
                t0,
                tagged(
                    error_item(
                        "wf boom",
                        "StateError",
                        "sess-w1",
                        t0 + Duration::seconds(45),
                    ),
                    "wf-1",
                    "checkout",
                ),
            ),
            job(
                app,
                t0,
                tagged(
                    event_item("wf_step", "sess-w2", t0 - Duration::seconds(120)),
                    "wf-1",
                    "checkout",
                ),
            ),
            job(
                app,
                t0,
                tagged(
                    tx_item("POST /pay", "sess-w2", t0 + Duration::seconds(200)),
                    "wf-1",
                    "checkout",
                ),
            ),
            // A second workflow with a single signal, so the batch upsert has
            // to carry more than one conflict key — a fold keyed only by
            // app_id would collapse these two into one row.
            job(
                app,
                t0,
                tagged(
                    event_item("wf_other", "sess-w2", t0 + Duration::seconds(10)),
                    "wf-2",
                    "onboarding",
                ),
            ),
        ]
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct Counts {
        #[diesel(sql_type = BigInt)]
        issues: i64,
        #[diesel(sql_type = BigInt)]
        errors: i64,
        #[diesel(sql_type = BigInt)]
        events: i64,
        #[diesel(sql_type = BigInt)]
        txs: i64,
        #[diesel(sql_type = BigInt)]
        sessions: i64,
        #[diesel(sql_type = BigInt)]
        devices: i64,
        #[diesel(sql_type = BigInt)]
        users: i64,
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct IssueAgg {
        #[diesel(sql_type = Text)]
        fingerprint: String,
        #[diesel(sql_type = BigInt)]
        times_seen: i64,
        #[diesel(sql_type = Text)]
        title: String,
        // Compared because the two paths reach this window differently — the
        // per-item one through N sequential upserts, the batch one through a
        // fold — and an earlier draft diverged here on out-of-order
        // occurrences while every other assertion still passed.
        #[diesel(sql_type = Timestamptz)]
        first_seen: DateTime<Utc>,
        #[diesel(sql_type = Timestamptz)]
        last_seen: DateTime<Utc>,
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct SessionAgg {
        #[diesel(sql_type = Text)]
        session_id: String,
        #[diesel(sql_type = BigInt)]
        events_count: i64,
        #[diesel(sql_type = BigInt)]
        errors_count: i64,
        #[diesel(sql_type = Timestamptz)]
        started_at: DateTime<Utc>,
        #[diesel(sql_type = Timestamptz)]
        last_event_at: DateTime<Utc>,
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct WorkflowAgg {
        #[diesel(sql_type = Text)]
        workflow_id: String,
        #[diesel(sql_type = Text)]
        name: String,
        /// The whole reason this struct exists. `session_id` is the field whose
        /// `COALESCE` runs the opposite way round from the session fold's, so
        /// it is the one that catches a fold copied from the wrong neighbour.
        #[diesel(sql_type = Nullable<Text>)]
        session_id: Option<String>,
        #[diesel(sql_type = Integer)]
        events_count: i32,
        #[diesel(sql_type = Integer)]
        errors_count: i32,
        #[diesel(sql_type = Timestamptz)]
        started_at: DateTime<Utc>,
        #[diesel(sql_type = Timestamptz)]
        last_event_at: DateTime<Utc>,
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct DeviceAgg {
        #[diesel(sql_type = BigInt)]
        events_count: i64,
        #[diesel(sql_type = BigInt)]
        errors_count: i64,
        #[diesel(sql_type = Timestamptz)]
        first_seen: DateTime<Utc>,
        #[diesel(sql_type = Timestamptz)]
        last_seen: DateTime<Utc>,
    }

    async fn counts(conn: &mut sauron_db::AsyncPgConnection, app_id: Uuid) -> Counts {
        diesel::sql_query(
            "SELECT (SELECT count(*) FROM issues WHERE app_id = $1) AS issues, \
                    (SELECT count(*) FROM error_events WHERE app_id = $1) AS errors, \
                    (SELECT count(*) FROM analytics_events WHERE app_id = $1) AS events, \
                    (SELECT count(*) FROM transactions WHERE app_id = $1) AS txs, \
                    (SELECT count(*) FROM sessions WHERE app_id = $1) AS sessions, \
                    (SELECT count(*) FROM devices WHERE app_id = $1) AS devices, \
                    (SELECT count(*) FROM event_users WHERE app_id = $1) AS users",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_result(conn)
        .await
        .expect("counts")
    }

    #[tokio::test]
    async fn batched_writes_land_the_same_rows_as_per_item_writes() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let Ok(redis_url) = std::env::var("TEST_REDIS_URL") else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            db.cleanup().await;
            return;
        };
        let redis = RedisStore::connect(&redis_url)
            .await
            .expect("connect redis");
        let sym = crate::symbolize::SymbolizeCtx::new(
            Arc::new(sauron_symbols::Symbolicator::new(1 << 20)),
            sauron_redis::SymbolBlobCache::connect(None, 1 << 20).await,
            100,
            1 << 20,
        );
        let masks = MaskSet::from_rows(vec![]);
        // Truncated to microseconds because `timestamptz` is: comparing a
        // nanosecond-precision Rust instant against what Postgres stored would
        // fail on the rounding, not on anything this test is about.
        let t0 = Utc::now().trunc_subsecs(6);

        let a = seed_app(&db, "seq").await;
        let b = seed_app(&db, "bat").await;

        // Per-item, one at a time — the reference.
        for j in signals(&a, t0) {
            crate::process::process_job(db.pool(), &redis, &sym, &masks, j)
                .await
                .expect("per-item write");
        }

        // The same signals, as one batch.
        let decoded: Vec<Decoded> = signals(&b, t0)
            .into_iter()
            .enumerate()
            .map(|(i, job)| Decoded {
                id: format!("0-{i}"),
                job,
                masks: Arc::new(MaskSet::from_rows(vec![])),
                // One item per entry here, so every item is its own tail.
                // `process_batch` does not read this field — the ack it feeds
                // is the worker's job — but constructing it honestly keeps the
                // fixture describing something the worker could actually
                // produce.
                entry_tail: true,
            })
            .collect();
        process_batch(db.pool(), &redis, &sym, &decoded)
            .await
            .expect("batched write");

        let mut conn = db.conn().await;

        assert_eq!(
            counts(&mut conn, a.app_id).await,
            counts(&mut conn, b.app_id).await,
            "row counts diverge between the per-item and batched paths"
        );

        for (label, app_id) in [("seq", a.app_id), ("bat", b.app_id)] {
            let issues = issue_aggs(&mut conn, app_id).await;
            // Two occurrences of one fingerprint, one of the other. If the
            // batch had passed `times_seen: 1` per row instead of the folded
            // count, this is where it would read 1 instead of 2.
            // Two occurrences of one fingerprint, one of another, plus the
            // workflow-tagged error's own — four occurrences over three
            // fingerprints.
            assert_eq!(issues.len(), 3, "{label}: expected three fingerprints");
            let total: i64 = issues.iter().map(|i| i.times_seen).sum();
            assert_eq!(total, 4, "{label}: times_seen must total the occurrences");
        }

        let seq_issues = issue_aggs(&mut conn, a.app_id).await;
        let bat_issues = issue_aggs(&mut conn, b.app_id).await;
        assert_eq!(seq_issues, bat_issues, "issue aggregates diverge");
        // Cross-path equality alone would still pass if BOTH paths recorded an
        // order-dependent window. The repeated fingerprint's two occurrences
        // straddle t0, so pin the actual values.
        let repeated = seq_issues
            .iter()
            .max_by_key(|i| i.times_seen)
            .expect("the twice-seen fingerprint");
        assert_eq!(repeated.times_seen, 2);
        assert_eq!(
            repeated.first_seen,
            t0 - Duration::seconds(90),
            "first_seen must be the earliest occurrence regardless of arrival order"
        );
        assert_eq!(
            repeated.last_seen, t0,
            "last_seen must be the latest occurrence, not the last one processed"
        );

        let seq_sessions = session_aggs(&mut conn, a.app_id).await;
        let bat_sessions = session_aggs(&mut conn, b.app_id).await;
        assert_eq!(seq_sessions, bat_sessions, "session roll-ups diverge");
        // Guard the guard: if the fixture ever stopped putting several signals
        // on one session, the comparison above would still pass while testing
        // nothing about folding.
        let sess_a = seq_sessions
            .iter()
            .find(|s| s.session_id == "sess-a")
            .expect("sess-a present");
        assert_eq!(sess_a.events_count, 1, "one analytics event on sess-a");
        assert_eq!(sess_a.errors_count, 2, "two errors on sess-a");
        assert_eq!(
            sess_a.started_at,
            t0 - Duration::seconds(90),
            "started_at must be the EARLIEST signal, not the last one folded"
        );
        assert_eq!(
            sess_a.last_event_at,
            t0 + Duration::seconds(60),
            "last_event_at must be the LATEST signal"
        );

        let seq_devices = device_aggs(&mut conn, a.app_id).await;
        let bat_devices = device_aggs(&mut conn, b.app_id).await;
        assert_eq!(seq_devices, bat_devices, "device roll-ups diverge");

        let seq_workflows = workflow_aggs(&mut conn, a.app_id).await;
        let bat_workflows = workflow_aggs(&mut conn, b.app_id).await;
        assert_eq!(seq_workflows, bat_workflows, "workflow roll-ups diverge");
        // As with the sessions above: cross-path equality would still hold if
        // both paths folded wrongly in the same direction, so pin the values.
        assert_eq!(seq_workflows.len(), 2, "two distinct workflows");
        let wf1 = seq_workflows
            .iter()
            .find(|w| w.workflow_id == "wf-1")
            .expect("wf-1 present");
        assert_eq!(wf1.name, "checkout");
        // The transaction contributes 0 to BOTH counters — it bumps the
        // workflow only to widen the timestamp window, which is why it is the
        // sole supplier of `last_event_at` below.
        assert_eq!(wf1.events_count, 1, "the analytics event only");
        assert_eq!(wf1.errors_count, 1);
        assert_eq!(
            wf1.session_id.as_deref(),
            Some("sess-w1"),
            "session_id must be the FIRST one folded — the conflict arm puts \
             the existing row ahead of EXCLUDED, unlike the session fold"
        );
        assert_eq!(
            wf1.started_at,
            t0 - Duration::seconds(120),
            "started_at must be the earliest signal, which arrived second"
        );
        assert_eq!(
            wf1.last_event_at,
            t0 + Duration::seconds(200),
            "last_event_at must be the latest signal"
        );

        db.cleanup().await;
    }

    async fn issue_aggs(conn: &mut sauron_db::AsyncPgConnection, app_id: Uuid) -> Vec<IssueAgg> {
        diesel::sql_query(
            "SELECT fingerprint, times_seen, title, first_seen, last_seen FROM issues \
             WHERE app_id = $1 ORDER BY fingerprint",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_results(conn)
        .await
        .expect("issue aggs")
    }

    async fn session_aggs(
        conn: &mut sauron_db::AsyncPgConnection,
        app_id: Uuid,
    ) -> Vec<SessionAgg> {
        diesel::sql_query(
            "SELECT session_id, events_count, errors_count, started_at, last_event_at \
             FROM sessions WHERE app_id = $1 ORDER BY session_id",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_results(conn)
        .await
        .expect("session aggs")
    }

    async fn workflow_aggs(
        conn: &mut sauron_db::AsyncPgConnection,
        app_id: Uuid,
    ) -> Vec<WorkflowAgg> {
        diesel::sql_query(
            "SELECT workflow_id, name, session_id, events_count, errors_count, \
                    started_at, last_event_at \
             FROM workflows WHERE app_id = $1 ORDER BY workflow_id",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_results(conn)
        .await
        .expect("workflow aggs")
    }

    async fn device_aggs(conn: &mut sauron_db::AsyncPgConnection, app_id: Uuid) -> Vec<DeviceAgg> {
        diesel::sql_query(
            "SELECT events_count, errors_count, first_seen, last_seen \
             FROM devices WHERE app_id = $1 ORDER BY device_key",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_results(conn)
        .await
        .expect("device aggs")
    }
}
