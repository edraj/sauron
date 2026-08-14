//! Per-item processing: turn an [`IngestJob`] into durable rows.

use serde_json::{json, Value};
use uuid::Uuid;

use sauron_core::envelope::{
    AnalyticsItem, BreadcrumbBatch, ErrorItem, EventUser, ExceptionInfo, IdentifyItem, IngestJob,
    TransactionItem,
};
use sauron_core::{fingerprint, ids};
use sauron_db::models::{NewAnalyticsEvent, NewErrorEvent, NewIssue, NewTransaction};
use sauron_db::{repo, AsyncPgConnection, PgPool};
use sauron_redis::{keys, RedisStore};

use crate::enrich::enrich_context;

/// Whether `event_users.identified_at` exists, probed once per process.
///
/// An RPM upgrade installs new binaries against an old schema (SETUP.md §11),
/// so this worker must degrade rather than write to a column that is not
/// there. One ERROR at first contact, then silence: a log line per ingested
/// event would drown the deployment it is trying to warn.
#[cfg(not(test))]
static IDENTIFIED_COLUMN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(not(test))]
pub(crate) async fn identified_column_present(conn: &mut AsyncPgConnection) -> bool {
    if let Some(v) = IDENTIFIED_COLUMN.get() {
        return *v;
    }
    let present = repo::probe_event_users_identified(conn).await.is_ok();
    if !present {
        tracing::error!(
            "event_users.identified_at is missing — run sauron-migrate (see \
             packaging/rpm/SETUP.md §11). Active-user identification will not be \
             recorded for the lifetime of this process, and it cannot be \
             reconstructed afterwards."
        );
    }
    *IDENTIFIED_COLUMN.get_or_init(|| present)
}

/// Tests get an un-latched probe on purpose. Each test owns its own ephemeral
/// database and one of them deliberately drops the column, so a process-global
/// latch would leak one test's answer into another's depending on the order
/// the runner happened to pick.
#[cfg(test)]
pub(crate) async fn identified_column_present(conn: &mut AsyncPgConnection) -> bool {
    repo::probe_event_users_identified(conn).await.is_ok()
}

/// Process one job end to end: dispatch by item type.
pub async fn process_job(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &crate::symbolize::SymbolizeCtx,
    masks: &crate::mask::MaskSet,
    job: IngestJob,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;

    // Resolved at the ingest edge from the presented key. The client no longer
    // has any say in which environment a signal lands in.
    let environment_id = Some(job.environment_id);

    let mut context = enrich_context(&job);
    // The enriched-only surface. `error_events.context` and `sessions.context`
    // are both written from this value, and the ingest edge physically cannot
    // see it — the `woothee` ua block and `device_key` are derived right here.
    crate::mask::apply_context(masks, &mut context);

    match job.item.clone() {
        sauron_core::EnvelopeItem::Error(e) => {
            // Hand the connection over rather than dropping and re-acquiring:
            // `process_error` still needs one for the issue upsert, and it
            // releases it before symbolication — which checks out its OWN
            // connections, so holding one across it would let a handful of
            // concurrent errors exhaust the (small) ingest pool.
            process_error(redis, pool, sym, conn, &job, environment_id, context, *e).await
        }
        sauron_core::EnvelopeItem::Event(ev) => {
            process_event(&mut conn, &job, environment_id, context, ev).await
        }
        sauron_core::EnvelopeItem::Identify(id) => process_identify(&mut conn, &job, id).await,
        sauron_core::EnvelopeItem::BreadcrumbBatch(b) => process_breadcrumbs(redis, &job, b).await,
        sauron_core::EnvelopeItem::Transaction(t) => {
            process_transaction(&mut conn, &job, environment_id, context, t).await
        }
    }
}

/// Fold one signal into its `sessions` / `devices` roll-ups. `events_delta` /
/// `errors_delta` decide which counter to bump. No-ops when there is no session
/// id / device key to key on.
#[allow(clippy::too_many_arguments)]
async fn rollup(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: &Value,
    session_id: Option<&str>,
    distinct_id: Option<&str>,
    at: chrono::DateTime<chrono::Utc>,
    events_delta: i64,
    errors_delta: i64,
) {
    let info = crate::enrich::device_info(context);
    let session_id = session_id.filter(|s| !s.is_empty());
    let distinct_id = distinct_id.filter(|s| !s.is_empty());

    // `bump_session` reports whether it INSERTED. A session is bumped again by
    // every signal it carries, so crediting the person rollup per bump would
    // count one session many times — the error grows with session length.
    let mut sessions_delta = 0i64;
    if let Some(sid) = session_id {
        if let Ok(true) = repo::bump_session(
            conn,
            job.app_id,
            sid,
            distinct_id,
            info.device_key.as_deref(),
            at,
            context,
            job.release.as_deref(),
            environment_id,
            job.ip.as_deref(),
            events_delta,
            errors_delta,
        )
        .await
        {
            sessions_delta = 1;
        }
    }

    if let Some(dk) = info.device_key.as_deref() {
        let _ = repo::bump_device(
            conn,
            job.app_id,
            dk,
            info.family.as_deref(),
            info.model.as_deref(),
            info.os_name.as_deref(),
            info.os_version.as_deref(),
            info.arch.as_deref(),
            info.browser.as_deref(),
            distinct_id,
            at,
            events_delta,
            errors_delta,
        )
        .await;
    }

    // The person/environment rollup. Fed from every signal that names an
    // identity, not just from the session leg — membership here must admit
    // exactly the people the live query's three membership legs admit, and a
    // person whose only signal is a plain event has no session id.
    //
    // Deliberately `let _ =`, matching the two bumps above: this path's
    // roll-up writes are best-effort, and a rollup miss must not fail an event
    // that is already durable.
    if let Some(did) = distinct_id {
        let _ = repo::bump_person_env(
            conn,
            job.app_id,
            did,
            environment_id,
            at,
            events_delta,
            errors_delta,
            sessions_delta,
        )
        .await;
    }

    // The device/environment rollup. Twin of the person/environment rollup
    // immediately above, keyed on device instead of identity — this is the
    // sequential/per-item replay path, and `device_environments` must land the
    // same rows here as it does from the batched path or a replayed batch
    // contributes nothing to it.
    //
    // Deliberately `let _ =`, matching the bumps above: this path's roll-up
    // writes are best-effort, and a rollup miss must not fail an event that
    // is already durable.
    if let Some(dk) = info.device_key.as_deref().filter(|s| !s.is_empty()) {
        let _ = repo::bump_device_env(
            conn,
            job.app_id,
            dk,
            environment_id,
            at,
            events_delta,
            errors_delta,
            sessions_delta,
        )
        .await;
    }
}

/// Persist one error event.
///
/// Owns its connection lifetime in two phases with symbolication in between, so
/// no pooled connection is ever held across the symbolication path (which checks
/// out its own).
///
/// `conn` is the caller's connection, already checked out for the environment
/// upsert. It is reused for the issue grouping and released before
/// symbolication rather than being dropped and immediately re-acquired — the
/// pool recycles with a liveness check, so each extra checkout costs a
/// round-trip as well as a pool slot.
#[allow(clippy::too_many_arguments)]
async fn process_error(
    redis: &RedisStore,
    pool: &PgPool,
    sym: &crate::symbolize::SymbolizeCtx,
    mut conn: sauron_db::PgConn,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: Value,
    e: ErrorItem,
) -> anyhow::Result<()> {
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
    let device_key = crate::enrich::device_info(&context).device_key;
    // `device_key` is moved into the `NewErrorEvent` below, but the workflow
    // bump after it needs the value too. Clone it here rather than there, and
    // ONLY when a workflow bump will actually consume it: `and_then`'s closure
    // does not run when the stamp is absent, so an app that never calls
    // `startWorkflow` pays no allocation for this line at all. (Cloning
    // unconditionally at the struct literal instead would tax every error
    // event in the system for a path that fires zero times for such an app.)
    let workflow_device_key = e
        .workflow_id
        .as_ref()
        .zip(e.workflow_name.as_ref())
        .and_then(|_| device_key.clone());

    // --- phase 1: group the error into an issue, then release the connection.
    let issue_id = repo::upsert_issue(
        &mut conn,
        NewIssue {
            app_id: job.app_id,
            fingerprint: &fp,
            type_: &exception_type,
            title: &title,
            culprit: &culprit,
            level,
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await?;
    drop(conn);

    let user = e.user.as_ref().or(job.context.user.as_ref());
    let distinct = distinct_id(user);
    // The SAME equality test `process_event` runs, computed here while `user`
    // is still borrowable. On this path it is trivially true whenever
    // `distinct` is `Some` — `distinct` is derived from `user.id` — and that
    // is the point: an earlier draft passed `identified = true`
    // unconditionally here, which made the two paths disagree for no benefit
    // and let one error envelope carrying any `user.id` flag that id.
    let context_user_matches = user
        .and_then(|u| u.id.as_deref())
        .is_some_and(|id| !id.is_empty() && Some(id) == distinct.as_deref());
    let event_user = user.and_then(|u| serde_json::to_value(u).ok());
    let stacktrace = exc
        .map(|x| serde_json::to_value(&x.stacktrace).unwrap_or_else(|_| json!([])))
        .unwrap_or_else(|| json!([]));

    // Hybrid write path: pre-symbolicate when symbols are already uploaded.
    // Strictly time-boxed and non-fatal — misses/timeouts fall to on-read. Dart
    // AOT traces (raw_stacktrace) go through the ELF/DWARF path; everything else
    // through JS source maps.
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

    // --- phase 2: symbolication is done; take a connection again for the writes.
    let mut conn = sauron_db::conn(pool).await?;
    repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: ids::uuid_v7(),
            app_id: job.app_id,
            environment_id,
            issue_id,
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
            // Same strings just handed to `upsert_issue` above (still in
            // scope — only borrowed there, not moved). Persisting them here
            // is what lets a later environment-scoped read derive title/
            // culprit from this occurrence instead of the app-wide `issues`
            // row, which `upsert_issue` overwrites from whichever
            // environment's occurrence lands last.
            title: Some(title),
            culprit: Some(culprit),
        },
    )
    .await?;

    rollup(
        &mut conn,
        job,
        environment_id,
        &context,
        e.session_id.as_deref(),
        distinct.as_deref(),
        now,
        0,
        1,
    )
    .await;

    if let (Some(wf_id), Some(wf_name)) = (e.workflow_id.as_deref(), e.workflow_name.as_deref()) {
        let _ = repo::bump_workflow(
            &mut conn,
            job.app_id,
            job.environment_id,
            wf_id,
            wf_name,
            e.session_id.as_deref(),
            distinct.as_deref(),
            workflow_device_key.as_deref(),
            job.release.as_deref(),
            now,
            0,
            1,
        )
        .await;
    }

    // Affected-user count via HyperLogLog.
    if let Some(did) = distinct {
        let key = keys::issue_users(&issue_id.to_string());
        // Only when the HyperLogLog actually changed. Recomputing and re-writing
        // an unchanged `users_seen` on every occurrence put an `UPDATE issues`
        // in the hot path of every error event, competing with the issue upsert
        // for the same row.
        // Both conditions matter: `pf_add` false means the estimate did not
        // move, and the gate means an issue's denormalized copy is refreshed at
        // most once per interval. Together they take this `UPDATE issues` off
        // the per-event path, where it deadlocked against the issue upsert.
        if redis.pf_add(&key, &did).await.unwrap_or(false) && crate::batch::users_seen_due(issue_id)
        {
            if let Ok(count) = redis.pf_count(&key).await {
                let _ = repo::set_issue_users_seen(&mut conn, issue_id, count).await;
            }
        }
        // Was `let _ = …`; see `process_event`'s identical comment.
        if let Err(e) = repo::touch_event_user(&mut conn, job.app_id, &did).await {
            tracing::warn!(
                app_id = %job.app_id,
                error = %e,
                "touch_event_user failed; event_users.last_seen did not advance"
            );
        }
        if context_user_matches && identified_column_present(&mut conn).await {
            if let Err(e) = repo::mark_event_user_identified(
                &mut conn,
                job.app_id,
                &did,
                repo::IDENTIFIED_SOURCE_CONTEXT_USER,
            )
            .await
            {
                tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
            }
        }
    }

    Ok(())
}

async fn process_event(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: Value,
    ev: AnalyticsItem,
) -> anyhow::Result<()> {
    let info = crate::enrich::device_info(&context);
    let at = ev.timestamp;
    let session_id = ev.session_id.clone();
    let distinct_id = ev.distinct_id.clone();
    // Captured before `ev.workflow_*` are moved into the insert below — needed
    // again afterward for the workflow bump and the lifecycle call.
    let workflow_id = ev.workflow_id.clone();
    let workflow_name = ev.workflow_name.clone();

    // Decided from a borrow of `ev.name`, BEFORE the insert moves it — so the
    // `properties` snapshot below can be made conditional on it.
    let action = match ev.name.as_str() {
        "$workflow_start" => Some(repo::WorkflowAction::Start),
        "$workflow_end" => Some(repo::WorkflowAction::End),
        "$workflow_cancel" => Some(repo::WorkflowAction::Cancel),
        _ => None,
    };
    // `ev.properties` is arbitrary caller JSON up to the ingest size cap, and
    // cloning a `serde_json::Value` is a full recursive allocation walk. Only
    // the three `$workflow_*` events ever read it back (for the hand-rolled-
    // client property fallback), so only they pay for the clone — an app that
    // never uses workflows sees `None` here and allocates nothing, which is
    // what "byte-identical when workflows are absent" has to mean in
    // allocator terms, not just in stored-bytes terms.
    let properties_snapshot = action.is_some().then(|| ev.properties.clone());

    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
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
        },
    )
    .await?;

    rollup(
        conn,
        job,
        environment_id,
        &context,
        session_id.as_deref(),
        Some(distinct_id.as_str()),
        at,
        1,
        0,
    )
    .await;

    if let (Some(wf_id), Some(wf_name)) = (workflow_id.as_deref(), workflow_name.as_deref()) {
        let _ = repo::bump_workflow(
            conn,
            job.app_id,
            job.environment_id,
            wf_id,
            wf_name,
            session_id.as_deref(),
            Some(distinct_id.as_str()).filter(|s| !s.is_empty()),
            info.device_key.as_deref(),
            job.release.as_deref(),
            at,
            1,
            0,
        )
        .await;
    }

    if !distinct_id.is_empty() {
        // Was `let _ = …`. Swallowing this is how a deployment-wide stall of
        // `event_users.first_seen`/`last_seen` became invisible: no dead
        // letter, no metric, no log.
        if let Err(e) = repo::touch_event_user(conn, job.app_id, &distinct_id).await {
            tracing::warn!(
                app_id = %job.app_id,
                error = %e,
                "touch_event_user failed; event_users.last_seen did not advance"
            );
        }
        // An envelope-scoped `context.user.id` identifies this person only
        // when it IS the id the signal was filed under. Server SDKs take an
        // explicit distinctId that may differ from any scope user, and
        // marking that one identified would flag an id nobody claimed.
        let context_user_matches = job
            .context
            .user
            .as_ref()
            .and_then(|u| u.id.as_deref())
            .is_some_and(|id| !id.is_empty() && id == distinct_id);
        if context_user_matches && identified_column_present(conn).await {
            if let Err(e) = repo::mark_event_user_identified(
                conn,
                job.app_id,
                &distinct_id,
                repo::IDENTIFIED_SOURCE_CONTEXT_USER,
            )
            .await
            {
                tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
            }
        }
    }

    // The three reserved lifecycle events drive a `workflows` status
    // transition in addition to the ordinary stamped-event bump above — the
    // event row itself is still inserted normally (already done above), so a
    // `$workflow_start`/`_end`/`_cancel` stays visible in the events feed
    // like any other event. `action` was decided before the insert (see its
    // declaration); `properties_snapshot` is `Some` exactly when it is.
    if let Some(action) = action {
        // A string property from the lifecycle event's own `properties`, for
        // the hand-rolled clients the fallbacks below exist for.
        let prop = |key: &str| {
            properties_snapshot
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
        };
        // A hand-rolled client may not have stamped `workflow_id`/`name` at
        // the envelope level and instead sent them as ordinary properties —
        // fall back to those before giving up. Never errors the job: a
        // lifecycle event with no resolvable workflow id is just skipped.
        let resolved_id = workflow_id
            .clone()
            .or_else(|| prop("workflow_id").map(str::to_string));
        if let Some(wf_id) = resolved_id {
            let resolved_name = workflow_name
                .clone()
                .or_else(|| prop("workflow_name").map(str::to_string))
                .unwrap_or_default();
            // Only a cancellation carries a reason. Reading this for
            // `$workflow_end` too would let `{"reason": "user completed"}` on
            // a successful workflow land in `cancel_reason` beside
            // `status = 'completed'` — and a dashboard that renders
            // "Cancelled: {reason}" off a non-null `cancel_reason` would then
            // report a spurious cancellation on a workflow that finished.
            let cancel_reason = (action == repo::WorkflowAction::Cancel)
                .then(|| prop("reason").map(|r| truncate(r, 120).to_string()))
                .flatten();
            let _ = repo::apply_workflow_lifecycle(
                conn,
                job.app_id,
                job.environment_id,
                &wf_id,
                &resolved_name,
                action,
                cancel_reason.as_deref(),
                session_id.as_deref(),
                Some(distinct_id.as_str()).filter(|s| !s.is_empty()),
                at,
            )
            .await;
        }
    }
    Ok(())
}

async fn process_identify(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    id: IdentifyItem,
) -> anyhow::Result<()> {
    let traits = object_or_empty(id.traits);
    repo::upsert_event_user(conn, job.app_id, &id.distinct_id, &traits).await?;
    // Identified by construction: identify() IS the caller naming a person.
    if !id.distinct_id.is_empty() && identified_column_present(conn).await {
        if let Err(e) = repo::mark_event_user_identified(
            conn,
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
        if !anon.is_empty() && !id.distinct_id.is_empty() {
            claim_and_enqueue(conn, job.app_id, &anon, &id.distinct_id).await;
        }
    }
    Ok(())
}

/// Claim an alias and, on a fresh claim, schedule the merge.
///
/// Shared by `process_identify` and `batch`'s identify loop. Both call it; a
/// change to one is a change to both, which is the point — an earlier draft had
/// the enqueue inlined in one path only, and the other silently never merged.
///
/// A thin wrapper, not the transaction owner: `sauron_db::identity_merge::
/// claim_and_schedule` does the claim AND the (fresh-only) enqueue in one
/// transaction — see its doc comment for why those two cannot be split into a
/// separate `claim_identity` call followed by a separate `enqueue_merge` call
/// here. `conn` must NOT already be inside an open transaction when this is
/// called (both call sites check out a plain, autocommit connection).
pub(crate) async fn claim_and_enqueue(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
) {
    match sauron_db::identity_merge::claim_and_schedule(conn, app_id, alias, person).await {
        // The merge was already scheduled atomically with the claim above —
        // nothing left to do or log.
        Ok(sauron_db::identity_merge::Claim::Fresh) => {}
        Ok(sauron_db::identity_merge::Claim::Repeat) => {}
        Ok(sauron_db::identity_merge::Claim::Conflict { existing }) => {
            // The burn rule drops this silently by design. This warning is the
            // ONLY signal an operator ever gets that an app is not calling
            // reset() on logout.
            tracing::warn!(
                app_id = %app_id, alias, claimed_by = %existing, attempted_by = person,
                "identity alias conflict; alias stays bound to its first claimant"
            );
        }
        Ok(sauron_db::identity_merge::Claim::Chain) => {
            tracing::warn!(
                app_id = %app_id, alias, person,
                "identity alias would form a chain; refused"
            );
        }
        Err(e) => tracing::warn!(app_id = %app_id, error = %e, "claiming an alias failed"),
    }
}

async fn process_breadcrumbs(
    redis: &RedisStore,
    job: &IngestJob,
    b: BreadcrumbBatch,
) -> anyhow::Result<()> {
    let Some(distinct) = b.distinct_id.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let key = keys::breadcrumbs(&job.app_id.to_string(), &distinct);
    let json = serde_json::to_string(&b.breadcrumbs).unwrap_or_else(|_| "[]".into());
    redis.push_breadcrumbs(&key, &json, 100, 1800).await
}

async fn process_transaction(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    environment_id: Option<Uuid>,
    context: Value,
    t: TransactionItem,
) -> anyhow::Result<()> {
    let at = t.timestamp;
    let distinct = t.distinct_id.clone();
    let session_id = t.session_id.clone();
    let workflow_id = t.workflow_id.clone();
    let workflow_name = t.workflow_name.clone();
    let info = crate::enrich::device_info(&context);

    repo::insert_transaction(
        conn,
        NewTransaction {
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
            finished_at: t.finished_at,
        },
    )
    .await?;

    // Keep the owning session's window and device fresh (no event/error bump).
    rollup(
        conn,
        job,
        environment_id,
        &context,
        session_id.as_deref(),
        distinct.as_deref(),
        at,
        0,
        0,
    )
    .await;

    if let (Some(wf_id), Some(wf_name)) = (workflow_id.as_deref(), workflow_name.as_deref()) {
        let _ = repo::bump_workflow(
            conn,
            job.app_id,
            job.environment_id,
            wf_id,
            wf_name,
            session_id.as_deref(),
            distinct.as_deref(),
            info.device_key.as_deref(),
            job.release.as_deref(),
            at,
            0,
            0,
        )
        .await;
    }

    Ok(())
}

// --- helpers --------------------------------------------------------------

pub(crate) fn distinct_id(user: Option<&EventUser>) -> Option<String> {
    user.and_then(|u| u.id.clone()).filter(|s| !s.is_empty())
}

/// Normalize a dev-supplied scope map for JSONB storage: `null` (the serde
/// default for an omitted key) becomes an empty object so the column is never
/// NULL; any other value passes through verbatim. The backend does not merge —
/// the SDK ships the already-merged effective scope.
pub(crate) fn object_or_empty(v: Value) -> Value {
    if v.is_null() {
        json!({})
    } else {
        v
    }
}

/// Whether the SDK reported this exception as caught by application code.
///
/// `None` means the SDK did not tell us, and it must stay `None` all the way
/// to the column: NULL is the design's "unknown". Never substitute a fallback
/// here — `unwrap_or(true)` would file every pre-upgrade crash as handled, and
/// `unwrap_or(false)` would report every unknown as a crash. Both `handled =
/// true` and `handled = false` filters must exclude unknown rows.
pub(crate) fn handled_of(exc: Option<&ExceptionInfo>) -> Option<bool> {
    exc.and_then(|x| x.mechanism.as_ref())
        .and_then(|m| m.handled)
}

pub(crate) fn build_title(exc: Option<&ExceptionInfo>, message: Option<&str>) -> String {
    match exc {
        Some(x) => {
            let value = x.value.as_deref().unwrap_or("").trim();
            if value.is_empty() {
                x.ty.clone()
            } else {
                format!("{}: {}", x.ty, truncate(value, 200))
            }
        }
        None => truncate(message.unwrap_or("Error").trim(), 200).to_string(),
    }
}

pub(crate) fn build_culprit(exc: Option<&ExceptionInfo>) -> String {
    let Some(x) = exc else {
        return String::new();
    };
    // Prefer the top in-app frame (crashing frame is last).
    let frame = x
        .stacktrace
        .iter()
        .rev()
        .find(|f| f.in_app == Some(true))
        .or_else(|| x.stacktrace.last());
    match frame {
        Some(f) => {
            let func = f.function.as_deref().unwrap_or("?");
            match f.filename.as_deref().or(f.module.as_deref()) {
                Some(loc) => format!("{func} ({loc})"),
                None => func.to_string(),
            }
        }
        None => String::new(),
    }
}

/// Truncate `s` to at most `max` chars (char-boundary safe). Shared by
/// `build_title` to cap stored issue titles/messages at a fixed length.
pub(crate) fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::{handled_of, object_or_empty};
    use sauron_core::envelope::{ExceptionInfo, Mechanism};
    use serde_json::json;

    fn exception(mechanism: Option<Mechanism>) -> ExceptionInfo {
        ExceptionInfo {
            ty: "TypeError".to_string(),
            value: Some("x is not a function".to_string()),
            mechanism,
            stacktrace: Vec::new(),
        }
    }

    fn mechanism(handled: Option<bool>) -> Mechanism {
        Mechanism {
            ty: "onerror".to_string(),
            handled,
        }
    }

    #[test]
    fn handled_is_none_when_the_sdk_did_not_say() {
        // Three ways to arrive at unknown: no exception, an exception with no
        // mechanism, and a mechanism that omitted the flag. None may become
        // `Some(true)` — that would classify a real crash as handled.
        assert_eq!(handled_of(None), None);
        assert_eq!(handled_of(Some(&exception(None))), None);
        assert_eq!(handled_of(Some(&exception(Some(mechanism(None))))), None);
    }

    #[test]
    fn handled_round_trips_both_known_values() {
        assert_eq!(
            handled_of(Some(&exception(Some(mechanism(Some(true)))))),
            Some(true)
        );
        assert_eq!(
            handled_of(Some(&exception(Some(mechanism(Some(false)))))),
            Some(false)
        );
    }

    #[test]
    fn object_or_empty_maps_null_to_empty_object() {
        assert_eq!(object_or_empty(serde_json::Value::Null), json!({}));
    }

    #[test]
    fn object_or_empty_preserves_non_empty_maps() {
        assert_eq!(
            object_or_empty(json!({ "region": "eu" })),
            json!({ "region": "eu" })
        );
        assert_eq!(
            object_or_empty(json!({ "order": { "id": 7 } })),
            json!({ "order": { "id": 7 } })
        );
    }
}

/// Workflow grouping, Task 3: exercises `process_event` — not `process_job` —
/// directly against a real, ephemeral Postgres database.
///
/// `process_job` is skipped deliberately: its dispatch also requires a live
/// `RedisStore` connection and a `SymbolizeCtx`, neither of which the `Event`
/// branch (`process_event`) ever touches, and standing up a real Redis
/// connection just to satisfy an otherwise-unused parameter would make this
/// test depend on infrastructure it does not need. `process_event` is private
/// to this module, so this test lives here (same module) rather than as a
/// separate `tests/` integration file, which could not see it.
#[cfg(test)]
pub(crate) mod workflow_pipeline_tests {
    use super::process_event;
    use chrono::{DateTime, Duration, Utc};
    use diesel::sql_types::{Integer, Text, Uuid as SqlUuid};
    use diesel_async::RunQueryDsl;
    use sauron_core::envelope::{AnalyticsItem, EnvelopeContext, EnvelopeItem, IngestJob};
    use sauron_db::models::NewAppEnvironment;
    use sauron_db::repo;
    use serde_json::json;
    use uuid::Uuid;

    /// One throwaway database for this test, created/migrated/dropped here
    /// rather than reusing `sauron-db`'s own `tests/common::TestDb`: that
    /// harness lives under `sauron-db`'s `tests/` directory, which is private
    /// to that crate's own integration-test binaries and cannot be named from
    /// a different crate (`sauron-pipeline`). Deliberately minimal — no rich
    /// seed — since this test needs exactly one app and one environment
    /// enrollment, nothing else.
    ///
    /// It does NOT run its own stale-database reaper, but its database names
    /// are deliberately shaped so `sauron-db`'s reaper collects them; see
    /// [`PipelineTestDb::setup`].
    ///
    /// `pub(super)` so the identification tests in `identity_pipeline_tests`
    /// can reuse it. A second hand-rolled ephemeral-database harness in the
    /// same file would drift from this one's load-bearing database-name shape
    /// (see `setup`).
    pub(crate) struct PipelineTestDb {
        pool: sauron_db::PgPool,
        admin_url: String,
        db_name: String,
        /// Tracked so `Drop` can tell whether `cleanup()` ever ran — same
        /// role, and same `Cell`-not-`AtomicBool` reasoning, as
        /// `tests/common::TestDb::cleaned_up`.
        cleaned_up: std::cell::Cell<bool>,
    }

    impl PipelineTestDb {
        pub(crate) async fn setup() -> Option<Self> {
            let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
            // Name shape is load-bearing, in two independent ways:
            //
            // 1. LENGTH. `sauron_db::validate_db_ident` caps identifiers at 63
            //    bytes: "sauron_test_" (12) + a 10-digit unix timestamp + "_"
            //    + "pl" + a 32-hex-digit UUID = 57 bytes. An earlier
            //    "sauron_test_pipeline_<ts>_<uuid>" spelling was 65 and failed
            //    outright with `unsafe database identifier`.
            //
            // 2. REAPER PARSE. `sauron-db`'s `tests/common::
            //    reap_stale_test_databases` collects abandoned `sauron_test_%`
            //    databases by doing `strip_prefix("sauron_test_")` ->
            //    `split('_').next()` -> `parse::<i64>()`, and silently SKIPS
            //    (`else { continue }`) any name whose first underscore-
            //    delimited segment is not a timestamp. So the timestamp must
            //    come FIRST and the "pl" discriminator must be glued to the
            //    uuid rather than separated by an underscore: a
            //    "sauron_test_pl_<ts>_<uuid>" spelling yields "pl", fails the
            //    parse, and leaks the database permanently — invisible to the
            //    only process that would ever collect it. Do not reorder these
            //    segments.
            let db_name = format!(
                "sauron_test_{}_pl{}",
                Utc::now().timestamp(),
                Uuid::new_v4().simple()
            );
            sauron_db::create_database(&admin_url, &db_name)
                .await
                .expect("create ephemeral pipeline test database");
            let db_url = swap_database(&admin_url, &db_name);
            sauron_db::run_pending_migrations(&db_url)
                .await
                .expect("run migrations on ephemeral pipeline test database");
            let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");
            Some(Self {
                pool,
                admin_url,
                db_name,
                cleaned_up: std::cell::Cell::new(false),
            })
        }

        pub(crate) async fn conn(&self) -> sauron_db::PgConn {
            sauron_db::conn(&self.pool).await.expect("checkout")
        }

        /// `process_error` re-acquires its own connection after symbolication,
        /// so it needs the pool rather than a checked-out connection.
        pub(crate) fn pool(&self) -> &sauron_db::PgPool {
            &self.pool
        }

        /// Takes `&self`, not `self`, so `Drop` still runs afterwards and can
        /// see the `cleaned_up` flag this sets.
        pub(crate) async fn cleanup(&self) {
            sauron_db::drop_database(&self.admin_url, &self.db_name)
                .await
                .expect("drop ephemeral pipeline test database");
            self.cleaned_up.set(true);
        }
    }

    impl Drop for PipelineTestDb {
        fn drop(&mut self) {
            // Async work cannot run in `Drop`. If the test panicked before
            // reaching `cleanup()`, make the leak loud rather than attempt a
            // runtime-in-Drop workaround — the same tradeoff
            // `tests/common::TestDb` makes for the identical reason. The
            // database is still reaper-collectable (see `setup`), but that
            // only happens on some later `sauron-db` test run, so say so now.
            if !self.cleaned_up.get() {
                eprintln!(
                    "WARNING: ephemeral test database {} may remain (PipelineTestDb::cleanup() \
                     was never reached — the test likely panicked). It is named so sauron-db's \
                     stale-db reaper will collect it after 3h, or drop it manually:\n  \
                     DROP DATABASE \"{}\" WITH (FORCE);",
                    self.db_name, self.db_name
                );
            }
        }
    }

    /// Same string-rewrite `sauron-db`'s own `tests/common::swap_database`
    /// does (preserve scheme/authority/query, replace the database segment) —
    /// duplicated rather than imported for the same private-`tests/`-module
    /// reason as [`PipelineTestDb`] itself.
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

    fn analytics_item(
        name: &str,
        workflow_id: &str,
        workflow_name: &str,
        at: DateTime<Utc>,
    ) -> AnalyticsItem {
        AnalyticsItem {
            name: name.to_string(),
            distinct_id: "pipeline-test-user".to_string(),
            properties: json!({}),
            timestamp: at,
            session_id: None,
            workflow_id: Some(workflow_id.to_string()),
            workflow_name: Some(workflow_name.to_string()),
            screen: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        }
    }

    #[derive(diesel::QueryableByName)]
    struct WorkflowRow {
        #[diesel(sql_type = Text)]
        status: String,
        #[diesel(sql_type = Integer)]
        events_count: i32,
    }

    /// `$workflow_start` (bumps events_count to 1, sets status active via the
    /// lifecycle path), an ordinary stamped event in between (bumps
    /// events_count to 2), then `$workflow_end` (bumps events_count to 3 AND
    /// transitions status) — all three land in exactly one `workflows` row,
    /// because all three are stamped with the same `workflow_id`. The
    /// lifecycle events are ordinary analytics events too, so they count
    /// toward `events_count` just like the plain one in the middle.
    #[tokio::test]
    async fn lifecycle_events_and_a_stamped_event_produce_one_completed_workflow_row() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let mut conn = db.conn().await;

        let suffix = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(
            &mut conn,
            "pipeline wf org",
            &format!("pipeline-wf-org-{suffix}"),
        )
        .await
        .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "pipeline wf project",
            &format!("pipeline-wf-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "pipeline wf app",
            &format!("pipeline-wf-app-{suffix}"),
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
                public_key: &format!("pk_pipeline_wf_{suffix}"),
                is_default: true,
            }],
        )
        .await
        .expect("enroll app in env")
        .remove(0)
        .id;

        let t0 = Utc::now();
        let job = IngestJob {
            app_id: app.id,
            project_id: project.id,
            org_id: org.id,
            environment_id,
            release: Some("1.0.0".to_string()),
            received_at: t0,
            ip: None,
            user_agent: None,
            context: EnvelopeContext::default(),
            sdk: None,
            // Never read by `process_event` (which receives its item as a
            // separate, already-extracted argument below) — present only
            // because `IngestJob` has no `Default` and this field is
            // required to construct one.
            item: EnvelopeItem::Event(analytics_item(
                "$workflow_start",
                "pipeline-wf-1",
                "checkout",
                t0,
            )),
        };
        let context = json!({});

        process_event(
            &mut conn,
            &job,
            Some(environment_id),
            context.clone(),
            analytics_item("$workflow_start", "pipeline-wf-1", "checkout", t0),
        )
        .await
        .expect("process $workflow_start");

        process_event(
            &mut conn,
            &job,
            Some(environment_id),
            context.clone(),
            analytics_item(
                "app.custom_event",
                "pipeline-wf-1",
                "checkout",
                t0 + Duration::minutes(1),
            ),
        )
        .await
        .expect("process stamped event");

        process_event(
            &mut conn,
            &job,
            Some(environment_id),
            context.clone(),
            analytics_item(
                "$workflow_end",
                "pipeline-wf-1",
                "checkout",
                t0 + Duration::minutes(2),
            ),
        )
        .await
        .expect("process $workflow_end");

        let row: WorkflowRow = diesel::sql_query(
            "SELECT status, events_count FROM workflows WHERE app_id = $1 AND workflow_id = $2",
        )
        .bind::<SqlUuid, _>(app.id)
        .bind::<Text, _>("pipeline-wf-1")
        .get_result(&mut conn)
        .await
        .expect("select workflows row");

        assert_eq!(row.status, "completed", "status");
        assert_eq!(row.events_count, 3, "events_count");

        drop(conn);
        db.cleanup().await;
    }
}

#[cfg(test)]
mod identity_pipeline_tests {
    use super::workflow_pipeline_tests::PipelineTestDb;
    use super::{process_error, process_event, process_identify};
    use chrono::Utc;
    use diesel::sql_types::{Nullable, Text, Uuid as SqlUuid};
    use diesel_async::RunQueryDsl;
    use sauron_core::envelope::{
        AnalyticsItem, EnvelopeContext, EnvelopeItem, ErrorItem, EventUser, IdentifyItem,
        IngestJob, Level,
    };
    use sauron_db::models::NewAppEnvironment;
    use sauron_db::repo;
    use sauron_redis::RedisStore;
    use serde_json::json;
    use uuid::Uuid;

    struct Fixture {
        app_id: Uuid,
        job: IngestJob,
    }

    async fn seed(db: &PipelineTestDb, scope_user_id: Option<&str>) -> Fixture {
        let mut conn = db.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut conn, "id org", &format!("id-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "id project",
            &format!("id-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "id app",
            &format!("id-app-{suffix}"),
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
                public_key: &format!("pk_id_{suffix}"),
                is_default: true,
            }],
        )
        .await
        .expect("enroll")
        .remove(0)
        .id;
        drop(conn);

        // One literal, never `default()` + field assignment: clippy's
        // `field_reassign_with_default` is warn-by-default and every task here
        // gates on `-D warnings`.
        let context = EnvelopeContext {
            user: scope_user_id.map(|id| EventUser {
                id: Some(id.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        Fixture {
            app_id: app.id,
            job: IngestJob {
                app_id: app.id,
                project_id: project.id,
                org_id: org.id,
                environment_id,
                release: None,
                received_at: Utc::now(),
                ip: None,
                user_agent: None,
                context,
                sdk: None,
                // Never read: every call below passes its item explicitly.
                item: EnvelopeItem::Identify(IdentifyItem {
                    distinct_id: "unused".to_string(),
                    anonymous_id: None,
                    traits: json!({}),
                    timestamp: Utc::now(),
                }),
            },
        }
    }

    fn event(distinct_id: &str) -> AnalyticsItem {
        AnalyticsItem {
            name: "app.opened".to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            timestamp: Utc::now(),
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        }
    }

    #[derive(diesel::QueryableByName)]
    struct SourceRow {
        #[diesel(sql_type = Nullable<Text>)]
        identified_source: Option<String>,
    }

    async fn source_of(
        conn: &mut sauron_db::PgConn,
        app_id: Uuid,
        distinct_id: &str,
    ) -> Option<String> {
        let row: SourceRow = diesel::sql_query(
            "SELECT identified_source FROM event_users WHERE app_id = $1 AND distinct_id = $2",
        )
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(distinct_id)
        .get_result(conn)
        .await
        .expect("event_users row must exist");
        row.identified_source
    }

    #[derive(diesel::QueryableByName)]
    struct QueuedMerge {
        #[diesel(sql_type = Text)]
        alias_id: String,
        #[diesel(sql_type = Text)]
        distinct_id: String,
    }

    async fn queued(conn: &mut sauron_db::PgConn, app_id: Uuid) -> Vec<QueuedMerge> {
        diesel::sql_query(
            "SELECT alias_id, distinct_id FROM identity_merges \
              WHERE app_id = $1 ORDER BY alias_id",
        )
        .bind::<SqlUuid, _>(app_id)
        .load(conn)
        .await
        .expect("queued merges")
    }

    /// The single-item path must enqueue a merge on a fresh claim.
    #[tokio::test]
    async fn process_identify_enqueues_a_merge() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, None).await;
        let mut conn = db.conn().await;

        process_identify(
            &mut conn,
            &f.job,
            IdentifyItem {
                distinct_id: "u-42".to_string(),
                anonymous_id: Some("anon_single".to_string()),
                traits: json!({}),
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("process identify");

        let q = queued(&mut conn, f.app_id).await;
        assert_eq!(
            q.len(),
            1,
            "the single-item path must enqueue exactly one merge"
        );
        assert_eq!(q[0].alias_id, "anon_single");
        assert_eq!(q[0].distinct_id, "u-42");

        drop(conn);
        db.cleanup().await;
    }

    /// A repeat identify() must NOT enqueue a second merge.
    #[tokio::test]
    async fn a_repeat_identify_does_not_enqueue_twice() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, None).await;
        let mut conn = db.conn().await;

        for _ in 0..2 {
            process_identify(
                &mut conn,
                &f.job,
                IdentifyItem {
                    distinct_id: "u-42".to_string(),
                    anonymous_id: Some("anon_twice".to_string()),
                    traits: json!({}),
                    timestamp: Utc::now(),
                },
            )
            .await
            .expect("process identify");
        }

        assert_eq!(queued(&mut conn, f.app_id).await.len(), 1);

        drop(conn);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn process_identify_marks_the_user_identified() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, None).await;
        let mut conn = db.conn().await;
        process_identify(
            &mut conn,
            &f.job,
            IdentifyItem {
                distinct_id: "u-42".to_string(),
                anonymous_id: None,
                traits: json!({ "plan": "pro" }),
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("process identify");
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-42").await.as_deref(),
            Some("identify"),
        );
        drop(conn);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn process_event_marks_identified_only_when_the_envelope_user_id_matches() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };

        // Case 1: scope user id == the event's distinct_id -> flagged.
        let f = seed(&db, Some("u-match")).await;
        let mut conn = db.conn().await;
        process_event(&mut conn, &f.job, None, json!({}), event("u-match"))
            .await
            .expect("matching event");
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-match").await.as_deref(),
            Some("context_user"),
        );

        // Case 2: scope user id present but different -> NOT flagged. Server
        // SDKs take an explicit distinctId that may differ from any scope
        // user; marking THAT one identified would be wrong.
        process_event(&mut conn, &f.job, None, json!({}), event("u-other"))
            .await
            .expect("mismatched event");
        assert_eq!(source_of(&mut conn, f.app_id, "u-other").await, None);
        drop(conn);

        // Case 3: no scope user at all -> NOT flagged.
        let g = seed(&db, None).await;
        let mut conn = db.conn().await;
        process_event(&mut conn, &g.job, None, json!({}), event("u-anon"))
            .await
            .expect("userless event");
        assert_eq!(source_of(&mut conn, g.app_id, "u-anon").await, None);

        drop(conn);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn process_error_runs_the_same_identification_test_as_process_event() {
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
        // `SymbolizeCtx` derives only `Clone` and its `presence` field is
        // private, so `new` is the only constructor. `SymbolBlobCache::connect`
        // with a `None` url returns a disabled cache, which is what this test
        // wants: no error here carries a stack trace, so symbolication has
        // nothing to do and must not reach out to anything.
        let sym = crate::symbolize::SymbolizeCtx::new(
            std::sync::Arc::new(sauron_symbols::Symbolicator::new(1 << 20)),
            sauron_redis::SymbolBlobCache::connect(None, 1 << 20).await,
            100,
            1 << 20,
        );

        let f = seed(&db, Some("u-err")).await;
        let conn = db.conn().await;
        // Field-by-field: `ErrorItem` derives `Debug, Clone, Serialize,
        // Deserialize` and no `Default`, and `event_id`/`timestamp`/
        // `breadcrumbs` are not `Option` — their serde defaults do nothing for
        // a Rust literal.
        let item = ErrorItem {
            event_id: Uuid::new_v4(),
            level: Level::Error,
            timestamp: Utc::now(),
            exception: None,
            message: Some("boom".to_string()),
            breadcrumbs: vec![],
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
            fingerprint: None,
            user: None,
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            raw_stacktrace: None,
            debug_meta: None,
        };
        process_error(&redis, db.pool(), &sym, conn, &f.job, None, json!({}), item)
            .await
            .expect("process error");

        let mut conn = db.conn().await;
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-err").await.as_deref(),
            Some("context_user"),
            "the error path runs the same equality test, so the two can never disagree"
        );
        drop(conn);
        db.cleanup().await;
    }

    /// The §2.4 contract, and the only thing that pins it: with the column
    /// gone, an ordinary event must still advance `last_seen` and must not
    /// return an error (which `worker.rs` would turn into a dead letter).
    #[tokio::test]
    async fn event_users_maintenance_survives_a_missing_identified_at_column() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, Some("u-nocol")).await;
        let mut conn = db.conn().await;
        diesel::sql_query("ALTER TABLE event_users DROP COLUMN identified_at")
            .execute(&mut conn)
            .await
            .expect("drop the column to simulate an un-migrated deployment");

        process_event(&mut conn, &f.job, None, json!({}), event("u-nocol"))
            .await
            .expect("an un-migrated schema must not fail the job");

        #[derive(diesel::QueryableByName)]
        struct Seen {
            #[diesel(sql_type = diesel::sql_types::Timestamptz)]
            last_seen: chrono::DateTime<Utc>,
        }
        let row: Seen = diesel::sql_query(
            "SELECT last_seen FROM event_users WHERE app_id = $1 AND distinct_id = $2",
        )
        .bind::<SqlUuid, _>(f.app_id)
        .bind::<Text, _>("u-nocol")
        .get_result(&mut conn)
        .await
        .expect("touch_event_user must still have created the row");
        assert!(row.last_seen <= Utc::now());

        drop(conn);
        db.cleanup().await;
    }
}
