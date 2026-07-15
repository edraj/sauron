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

/// Process one job end to end: resolve the environment, then dispatch by type.
pub async fn process_job(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &crate::symbolize::SymbolizeCtx,
    job: IngestJob,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;

    let environment_id = match &job.environment {
        Some(name) if !name.is_empty() => Some(
            repo::upsert_environment(&mut conn, job.app_id, name)
                .await?
                .id,
        ),
        _ => None,
    };

    let context = enrich_context(&job);

    match job.item.clone() {
        sauron_core::EnvelopeItem::Error(e) => {
            process_error(&mut conn, redis, pool, sym, &job, environment_id, context, *e).await
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

    if let Some(sid) = session_id {
        let _ = repo::bump_session(
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
        .await;
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
}

#[allow(clippy::too_many_arguments)]
async fn process_error(
    conn: &mut AsyncPgConnection,
    redis: &RedisStore,
    pool: &PgPool,
    sym: &crate::symbolize::SymbolizeCtx,
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

    let issue_id = repo::upsert_issue(
        conn,
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

    let user = e.user.as_ref().or(job.context.user.as_ref());
    let distinct = distinct_id(user);
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

    repo::insert_error_event(
        conn,
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
            tags: if e.tags.is_null() {
                json!({})
            } else {
                e.tags.clone()
            },
            release: job.release.clone(),
            distinct_id: distinct.clone(),
            event_user,
            sdk: None,
            ip_address: job.ip.clone(),
            occurred_at: now,
            session_id: e.session_id.clone(),
            device_key,
            screen: e.screen.clone(),
            stacktrace_symbolicated,
            symbolication_status,
            debug_meta,
        },
    )
    .await?;

    rollup(
        conn,
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

    // Affected-user count via HyperLogLog.
    if let Some(did) = distinct {
        let key = keys::issue_users(&issue_id.to_string());
        if redis.pf_add(&key, &did).await.is_ok() {
            if let Ok(count) = redis.pf_count(&key).await {
                let _ = repo::set_issue_users_seen(conn, issue_id, count).await;
            }
        }
        let _ = repo::touch_event_user(conn, job.app_id, &did).await;
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

    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: ids::uuid_v7(),
            app_id: job.app_id,
            environment_id,
            name: ev.name,
            distinct_id: ev.distinct_id.clone(),
            properties: if ev.properties.is_null() {
                json!({})
            } else {
                ev.properties
            },
            context: context.clone(),
            session_id: ev.session_id,
            release: job.release.clone(),
            ip_address: job.ip.clone(),
            occurred_at: ev.timestamp,
            device_key: info.device_key.clone(),
            screen: ev.screen.clone(),
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

    if !distinct_id.is_empty() {
        let _ = repo::touch_event_user(conn, job.app_id, &distinct_id).await;
    }
    Ok(())
}

async fn process_identify(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    id: IdentifyItem,
) -> anyhow::Result<()> {
    let traits = if id.traits.is_null() {
        json!({})
    } else {
        id.traits
    };
    repo::upsert_event_user(conn, job.app_id, &id.distinct_id, &traits).await?;
    if let Some(anon) = id.anonymous_id {
        if !anon.is_empty() {
            let _ = repo::insert_identity(conn, job.app_id, &anon, &id.distinct_id).await;
        }
    }
    Ok(())
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
            release: job.release.clone(),
            ip_address: job.ip.clone(),
            occurred_at: t.timestamp,
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

    Ok(())
}

// --- helpers --------------------------------------------------------------

fn distinct_id(user: Option<&EventUser>) -> Option<String> {
    user.and_then(|u| u.id.clone()).filter(|s| !s.is_empty())
}

fn build_title(exc: Option<&ExceptionInfo>, message: Option<&str>) -> String {
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

fn build_culprit(exc: Option<&ExceptionInfo>) -> String {
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

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
