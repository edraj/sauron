//! On-read JS symbolication: resolve a stored error event's minified frames
//! against uploaded source maps, attach source context to the response, and
//! (for hot partitions) persist a lean symbolicated copy back.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{Duration, Utc};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use sauron_db::models::{ErrorEvent, Transaction};
use sauron_db::PgPool;
use sauron_redis::SymbolBlobCache;
use sauron_symbols::{ArtifactRef, BlobFetch, RawFrame, Status};

use crate::AppState;

/// A DB + isolated-Redis backed [`BlobFetch`].
///
/// Artifact lookups are memoized for the lifetime of the instance. A single
/// issue's event list is overwhelmingly one release (or one debug id), so
/// without memoization symbolicating N unresolved events issued N identical
/// artifact queries and checked out N pooled connections. Share one instance
/// across a request (see [`symbolicate_events`]) and it becomes one query.
pub struct SqlBlobFetch {
    pool: PgPool,
    app_id: Uuid,
    cache: SymbolBlobCache,
    max_uncompressed: usize,
    js_memo: Mutex<HashMap<String, Vec<ArtifactRef>>>,
    dart_memo: Mutex<HashMap<String, Vec<ArtifactRef>>>,
}

impl SqlBlobFetch {
    fn new(state: &AppState, app_id: Uuid) -> Self {
        Self {
            pool: state.pool.clone(),
            app_id,
            cache: state.symbols.clone(),
            max_uncompressed: state.cfg.symbols_max_uncompressed_mb * 1024 * 1024,
            js_memo: Mutex::new(HashMap::new()),
            dart_memo: Mutex::new(HashMap::new()),
        }
    }
}

impl SqlBlobFetch {
    /// One kind of Dart artifact for one build, memoized for this instance's
    /// lifetime. The memo key carries the KIND as well as the id: the ELF and
    /// the obfuscation map for a build share a `debug_id` and are distinct rows
    /// (`symbol_artifacts` is unique on (app, kind, debug_id)), so keying on
    /// the id alone would serve one where the other was asked for.
    async fn dart_artifact(&self, kind: &str, debug_id: &str) -> Vec<ArtifactRef> {
        let memo_key = format!("{kind}\u{1}{debug_id}");
        if let Some(hit) = self.dart_memo.lock().await.get(&memo_key) {
            return hit.clone();
        }
        let Ok(mut conn) = sauron_db::conn(&self.pool).await else {
            return Vec::new();
        };
        let refs = match sauron_db::repo::find_artifact_by_debug_id(
            &mut conn,
            self.app_id,
            kind,
            debug_id,
        )
        .await
        {
            Ok(Some(a)) => vec![ArtifactRef {
                name: a.name,
                blob_sha256: a.blob_sha256,
            }],
            _ => Vec::new(),
        };
        self.dart_memo.lock().await.insert(memo_key, refs.clone());
        refs
    }
}

impl BlobFetch for SqlBlobFetch {
    async fn js_artifacts(&self, release: &str) -> Vec<ArtifactRef> {
        if let Some(hit) = self.js_memo.lock().await.get(release) {
            return hit.clone();
        }
        let Ok(mut conn) = sauron_db::conn(&self.pool).await else {
            return Vec::new();
        };
        let rows = sauron_db::repo::find_artifacts_for_release(&mut conn, self.app_id, release)
            .await
            .unwrap_or_default();
        let refs: Vec<ArtifactRef> = rows
            .into_iter()
            .filter(|a| a.kind == "js_sourcemap")
            .map(|a| ArtifactRef {
                name: a.name,
                blob_sha256: a.blob_sha256,
            })
            .collect();
        self.js_memo
            .lock()
            .await
            .insert(release.to_string(), refs.clone());
        refs
    }

    async fn dart_symbols(&self, debug_id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
        self.dart_artifact("dart_symbols", debug_id).await
    }

    async fn dart_obfuscation_map(&self, debug_id: &str) -> Vec<ArtifactRef> {
        self.dart_artifact("dart_obfuscation_map", debug_id).await
    }

    async fn blob(&self, sha: &[u8]) -> Option<Vec<u8>> {
        let hex = sauron_symbols::hex(sha);
        let compressed = match self.cache.get(&hex).await {
            Some(c) => c,
            None => {
                let mut conn = sauron_db::conn(&self.pool).await.ok()?;
                let c = sauron_db::repo::get_blob(&mut conn, sha).await.ok()??;
                self.cache.put(&hex, &c).await;
                c
            }
        };
        sauron_symbols::decompress(&compressed, self.max_uncompressed).ok()
    }
}

/// Remove de-obfuscated **source code** (the symbolication context lines) from a
/// response event, leaving symbol names / file / line intact. Applied for callers
/// lacking `source:read`. Does not touch the stored row (persist keeps context).
pub fn strip_source_context(event: &mut ErrorEvent) {
    if let Some(Value::Array(frames)) = event.stacktrace_symbolicated.as_mut() {
        for f in frames.iter_mut() {
            if let Some(obj) = f.as_object_mut() {
                obj.remove("context_line");
                obj.remove("pre_context");
                obj.remove("post_context");
                obj.remove("context_start_line");
            }
        }
    }
}

/// Apply [`strip_source_context`] to every event in `events`, unless `perms`
/// carries `source:read`.
///
/// The gate's one entry point for handlers that return a *set* of events.
/// `strip_source_context` alone is easy to leave uncalled, and it was: four
/// handlers returned whole `ErrorEvent` rows — with the persisted
/// `stacktrace_symbolicated` column and its context lines — off `event:read`
/// alone, while the gate was enforced only on the two issues routes.
/// Measured before the fix, all four handed `context_line`, `pre_context`,
/// `post_context` and `context_start_line` to a caller holding `event:read`
/// and not `source:read` (see `tests/http_source_context.rs`, which fails on
/// each of them without this call).
///
/// Layered *under* [`gate_event_body`], not beside it: the body gate decides
/// whether there are frames at all, this one whether those frames carry source
/// text. Call both — a handler that calls only this one still hands whole
/// stack traces to a caller holding half the body pair.
///
/// Takes the permission set rather than a `bool` so the permission *name* is
/// checked in one place and a call site cannot invert the condition.
pub fn gate_source_context(perms: &std::collections::HashSet<String>, events: &mut [ErrorEvent]) {
    if perms.contains(sauron_auth::perm::SOURCE_READ) {
        return;
    }
    for ev in events.iter_mut() {
        strip_source_context(ev);
    }
}

/// Remove the event **body**, leaving the issue-level shell.
///
/// Withheld — the crash payload proper: `stacktrace` and its symbolicated
/// twin, `breadcrumbs`, `context` (the captured request/runtime blob),
/// `contexts` and `extra` (dev-supplied, arbitrary, and the most likely place
/// for a secret to land), `tags` (same family — dev-assignable free-form
/// key/values), `sdk`, `debug_meta` (whose `raw_stacktrace` IS a stack trace
/// by another name), `event_user`, and `ip_address`.
///
/// Kept — what the occurrences table and the issue header render, all of which
/// `issue:read` already confers at the issue level: identity/ancestry ids,
/// `level`, `message`, `exception_type`/`exception_value`, `title` (which is
/// just those two joined and truncated, so withholding it would withhold
/// nothing), `release`, `distinct_id`, timestamps, `session_id`, `device_key`,
/// `screen`, `symbolication_status`, `handled`. `distinct_id` stays on purpose:
/// it is the "user" column of the occurrences list and is already the *key* of
/// the person routes, whereas `event_user`'s traits are not.
///
/// **`culprit` goes, though `title` stays** — the two arrived together and are
/// not the same kind of value. `culprit` is a function name and a source path
/// lifted out of the frames, which is precisely what `stacktrace` carries and
/// this gate withholds; handing it over would leak one frame of a stack trace
/// to a caller denied the trace. The issue-level `issues.culprit` is served
/// under `issue:read` alone, but that is the issue's own metadata and a caller
/// holding `event:read` WITHOUT `issue:read` — `sessions::detail`'s
/// authorization — never sees it.
///
/// Fields are nulled rather than the row being dropped, so a coarse-gated
/// caller still gets the occurrence — "this happened, at this time, on this
/// release" — instead of an empty list that reads as "no data".
pub fn strip_event_body(event: &mut ErrorEvent) {
    event.stacktrace = Value::Null;
    event.stacktrace_symbolicated = None;
    event.breadcrumbs = Value::Null;
    event.context = Value::Null;
    event.contexts = Value::Null;
    event.extra = Value::Null;
    event.tags = Value::Null;
    event.sdk = None;
    event.debug_meta = None;
    event.event_user = None;
    event.ip_address = None;
    event.culprit = None;
}

/// Remove a transaction's **developer-supplied** payload, leaving the span.
///
/// Withheld — `tags` and `extra`. `extra` is where the request body, the
/// response body and anything else the call site attached land, which makes it
/// the same "most likely place for a secret" as `ErrorEvent::extra`; `tags` is
/// the same family. `ip_address` goes too, matching [`strip_event_body`].
///
/// Kept — the span itself: `name`, `op`, `duration_ms`, `status`, `http_method`,
/// `http_status`, `url`, timestamps, `session_id`, `device_key`, `release`,
/// `distinct_id`, `workflow_*`. A coarse-gated caller still sees that the
/// operation happened and how long it took, rather than an empty list that
/// reads as "no data".
///
/// **`url` deliberately stays.** It is the label of an HTTP span — withholding
/// it would leave `name`, which for HTTP transactions is usually the same
/// string, so removing one and not the other would withhold nothing while
/// making every list unreadable.
pub fn strip_transaction_body(txn: &mut Transaction) {
    txn.tags = Value::Null;
    txn.extra = Value::Null;
    txn.ip_address = None;
}

/// Whether `perms` may see transaction bodies at all.
///
/// `event:read` ALONE, deliberately not [`may_read_event_body`]'s
/// `issue:read AND event:read`: a performance span is not an issue, and
/// requiring issue-reading rights to see an HTTP span's payload would read as a
/// bug to whoever hit it. `sessions::detail` already authorizes on `event:read`,
/// so this composes there without widening that route's requirement.
///
/// Exposed as a named predicate for the same reason [`may_read_event_body`] is:
/// [`transaction_text_search_reach`] is DERIVED from it rather than restating
/// it, so "what you may search" and "what you may read back" cannot drift.
pub fn may_read_transaction_body(perms: &std::collections::HashSet<String>) -> bool {
    perms.contains(sauron_auth::perm::EVENT_READ)
}

/// How far a free-text `?q=` may reach over `transactions` for this permission
/// set.
///
/// **A search predicate is a read**, and `extra` on a transaction is where
/// request and response bodies live. Answering "does this column contain this
/// substring?" for a column the same response NULLS is not withholding it:
/// probe `?q=sk_live_a`, `?q=sk_live_ab`, … and the row counts spell the value
/// out one byte at a time.
///
/// Derived from [`may_read_transaction_body`] — the SAME predicate
/// [`gate_transaction_body`] uses. The invariant is *what you may search is
/// exactly what you may read back*, and it only holds if one function answers
/// both questions; two copies would drift, and the drift that matters
/// (searchable wider than readable) is silent.
pub fn transaction_text_search_reach(
    perms: &std::collections::HashSet<String>,
) -> sauron_db::repo::TextSearchReach {
    if may_read_transaction_body(perms) {
        sauron_db::repo::TextSearchReach::IncludingBody
    } else {
        sauron_db::repo::TextSearchReach::ShellOnly
    }
}

/// Apply [`strip_transaction_body`] to every transaction unless `perms` carries
/// `event:read`.
///
/// Lives here, taking the permission set rather than a `bool`, for the reason
/// [`gate_event_body`] does: every route that reaches a transaction body is one
/// forgotten line away from a leak, so the check is a function they call rather
/// than a condition each of them restates.
pub fn gate_transaction_body(perms: &std::collections::HashSet<String>, txns: &mut [Transaction]) {
    if may_read_transaction_body(perms) {
        return;
    }
    for t in txns.iter_mut() {
        strip_transaction_body(t);
    }
}

/// Whether `perms` may see event bodies at all.
///
/// Exposed so a handler can *skip the work* that produces a body it would then
/// throw away (symbolication is a blob decompress plus a source-map or DWARF
/// walk) without restating the predicate. Restating it is exactly how the two
/// halves drift apart, and the drift is silent in the safe direction and a leak
/// in the other.
pub fn may_read_event_body(perms: &std::collections::HashSet<String>) -> bool {
    perms.contains(sauron_auth::perm::ISSUE_READ) && perms.contains(sauron_auth::perm::EVENT_READ)
}

/// How far a free-text `?q=` may reach for this permission set.
///
/// **A search predicate is a read.** Before this existed, `issues::list`,
/// `issues::events` and `issues::event_stats` — all three authorized on
/// `issue:read` ALONE — ran an ILIKE over `error_events.contexts::text`,
/// `extra::text` and `tags::text`, three of the ten columns
/// [`strip_event_body`] nulls for exactly that caller. Withholding a value from
/// the response while answering "does it contain this substring?" is not
/// withholding it: probe `?q=sk_live_a`, `?q=sk_live_ab`, … and the row counts
/// spell the value out one byte at a time, over the very columns whose docs call
/// them "the most likely place for a secret to land".
///
/// Derived from [`may_read_event_body`] — the SAME predicate [`gate_event_body`]
/// uses, deliberately not a second copy of "issue:read and event:read". The
/// invariant is *what you may search is exactly what you may read back*, and it
/// only holds if one function answers both questions; two copies would drift,
/// and the drift that matters (searchable wider than readable) is silent.
pub fn text_search_reach(
    perms: &std::collections::HashSet<String>,
) -> sauron_db::repo::TextSearchReach {
    if may_read_event_body(perms) {
        sauron_db::repo::TextSearchReach::IncludingBody
    } else {
        sauron_db::repo::TextSearchReach::ShellOnly
    }
}

/// Apply [`strip_event_body`] to every event in `events` unless `perms` carries
/// BOTH `issue:read` and `event:read`.
///
/// `issue:read` is the coarse gate (the issue list and its metadata);
/// `event:read` is additionally required for a body. Neither alone is enough —
/// see `sauron_auth::perm::EVENT_READ`, whose doc records that this reverses the
/// 2026-08-08 ruling. Six handlers reach a body and each authorizes on only one
/// of the pair: `issues::detail` and `issues::events` on `issue:read`,
/// `sessions::detail` / `devices::detail` / `screens::detail` /
/// `analytics::person` on `event:read`. So every one of them needed this call,
/// and every one of them was one forgotten line away from a leak — which is why
/// the check lives here and takes the permission set rather than a `bool`,
/// exactly as [`gate_source_context`] does after the same mistake was made once
/// already.
pub fn gate_event_body(perms: &std::collections::HashSet<String>, events: &mut [ErrorEvent]) {
    if may_read_event_body(perms) {
        return;
    }
    for ev in events.iter_mut() {
        strip_event_body(ev);
    }
}

/// Symbolicate an event in place: sets `stacktrace_symbolicated` (with source
/// context) + `symbolication_status` on the response copy, and persists a copy
/// for hot partitions that hadn't been symbolicated yet.
pub async fn symbolicate_event(state: &AppState, app_id: Uuid, event: &mut ErrorEvent) {
    let fetch = SqlBlobFetch::new(state, app_id);
    symbolicate_with(state, &fetch, event).await;
}

/// Symbolicate a batch of events sharing one [`SqlBlobFetch`], so the artifact
/// lookup runs once per distinct release/debug id rather than once per event.
pub async fn symbolicate_events(state: &AppState, app_id: Uuid, events: &mut [ErrorEvent]) {
    let fetch = SqlBlobFetch::new(state, app_id);
    for ev in events.iter_mut() {
        symbolicate_with(state, &fetch, ev).await;
    }
}

/// Concurrency ceiling for on-read symbolication.
///
/// Resolving an unsymbolicated event decompresses a blob and parses a source map
/// (or walks DWARF via addr2line) — hundreds of milliseconds of CPU on input the
/// uploader controls. Nothing else bounds how many of these run at once, so a
/// handful of concurrent requests over unsymbolicated events could occupy every
/// runtime thread. A small permit pool keeps that work from starving unrelated
/// requests; waiters queue instead of piling onto the executor.
static SYMBOLICATION_SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

fn symbolication_permits() -> &'static tokio::sync::Semaphore {
    SYMBOLICATION_SLOTS.get_or_init(|| {
        // Leave headroom for request handling; symbolication is a background-ish
        // concern even when it happens on a read.
        let n = (std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4)
            / 2)
        .max(1);
        tokio::sync::Semaphore::new(n)
    })
}

/// Repair a missing `culprit` from frames that are ALREADY stored.
///
/// The rows this exists for are the awkward ones: symbolicated at ingest, but
/// *before* ingest learned to derive the culprit from the resolved frames, so
/// they carry good frames beside a culprit built from the raw ones — minified
/// for a JS build, and the empty string for an obfuscated Dart build, whose
/// events have no raw frames at all. They can never reach the resolver below:
/// `symbolicate_with`'s fast path exists precisely to skip re-symbolicating
/// them, and it is right to. So the repair has to happen here, off the frames
/// on the row, with no artifact lookup and no source-map or DWARF parse.
///
/// Costs a deserialize of a column already in memory, and only for rows still
/// missing the value — the write below means each row pays it at most once.
async fn backfill_culprit_from_stored(state: &AppState, event: &mut ErrorEvent) {
    // `Some("")` counts as missing: that is what an obfuscated Dart event got
    // from the raw derivation, and it is the case this whole path is for.
    if event.culprit.as_deref().is_some_and(|c| !c.is_empty()) {
        return;
    }
    let Some(frames) = event.stacktrace_symbolicated.as_ref() else {
        return;
    };
    let Ok(resolved) = serde_json::from_value::<Vec<sauron_symbols::ResolvedFrame>>(frames.clone())
    else {
        return;
    };
    let Some(culprit) = sauron_symbols::culprit_of_resolved(&resolved) else {
        return;
    };

    // Same tiering guard as the resolve path: never write into a cold/exported
    // partition. A cold event still gets the value on its response.
    if event.occurred_at > Utc::now() - Duration::days(state.cfg.tier_hot_days) {
        if let Ok(mut conn) = sauron_db::conn(&state.pool).await {
            let _ = sauron_db::repo::update_event_culprit(
                &mut conn,
                event.id,
                event.occurred_at,
                &culprit,
            )
            .await;
            let _ = sauron_db::repo::update_issue_culprit_if_latest(
                &mut conn,
                event.issue_id,
                &culprit,
                event.occurred_at,
            )
            .await;
        }
    }
    event.culprit = Some(culprit);
}

async fn symbolicate_with(state: &AppState, fetch: &SqlBlobFetch, event: &mut ErrorEvent) {
    // Fast path: already fully symbolicated (at ingest or a prior read) and the
    // frames are stored with context — serve them as-is. This keeps issue/event
    // views cheap: no per-event artifact query, no re-parse (crucial for Dart,
    // whose DWARF context is rebuilt per call). Only pending/partial/no_artifacts
    // events do work (the backfill case).
    if event.symbolication_status == "symbolicated"
        && event
            .stacktrace_symbolicated
            .as_ref()
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        backfill_culprit_from_stored(state, event).await;
        deobfuscate_type_on_read(state, fetch, event).await;
        return;
    }

    // Past the fast path, this event needs real work — take a permit so the
    // number of concurrent decompress/parse/DWARF walks stays bounded.
    let permit = symbolication_permits().acquire().await;

    // Dart AOT trace (in debug_meta.raw_stacktrace) → ELF/DWARF path; otherwise
    // the JS source-map path over the raw frames.
    let (resolved, status) = if let Some(dm) = event.debug_meta.as_ref() {
        match dm.get("raw_stacktrace").and_then(|v| v.as_str()) {
            Some(rt) if !rt.is_empty() => {
                let build_id = dm.get("build_id").and_then(|v| v.as_str());
                let arch = dm.get("arch").and_then(|v| v.as_str());
                state
                    .symbolicator
                    .symbolicate_dart(fetch, rt, build_id, arch)
                    .await
            }
            _ => return,
        }
    } else {
        let frames: Vec<RawFrame> = match serde_json::from_value(event.stacktrace.clone()) {
            Ok(f) => f,
            Err(_) => return,
        };
        if frames.is_empty() {
            return;
        }
        state
            .symbolicator
            .symbolicate_js(fetch, event.release.as_deref(), &frames)
            .await
    };

    // The CPU-bound work is done. Release the permit before the write-back
    // below: the pool exists to bound parallel parsing, and holding it across a
    // pool checkout plus an UPDATE made the effective concurrency limit far
    // lower than the intended cores/2.
    drop(permit);

    // Only override the response when we actually resolved something; otherwise
    // keep whatever was stored (e.g. an ingest-time pre-symbolication).
    if !matches!(status, Status::Symbolicated | Status::Partial) {
        return;
    }

    // Persist for hot partitions that were previously unresolved — never write
    // into cold/exported partitions (respects the tiering guard). NOT a "lean"
    // copy: the write below is `serde_json::to_value(&resolved)`, i.e. the full
    // frames INCLUDING source context. See the comment at the write itself.
    let hot = event.occurred_at > Utc::now() - Duration::days(state.cfg.tier_hot_days);
    let was_unresolved = matches!(
        event.symbolication_status.as_str(),
        "pending" | "no_artifacts"
    );
    // The culprit the newly-resolved frames name — the readable
    // `checkout (cart_bloc.dart)` that the Exceptions list and the session
    // timeline render beside the exception type. Ingest derives this too, but
    // only for events whose symbols were already uploaded when they arrived;
    // every crash that landed BEFORE its symbol upload got the raw derivation,
    // which for an obfuscated Dart build is the empty string. Those rows are
    // the ones this path repairs.
    let culprit = sauron_symbols::culprit_of_resolved(&resolved);

    if hot && was_unresolved {
        // Persist WITH context so later views short-circuit to the stored frames.
        if let (Ok(frames_json), Ok(mut conn)) = (
            serde_json::to_value(&resolved),
            sauron_db::conn(&state.pool).await,
        ) {
            let _ = sauron_db::repo::update_event_symbolication(
                &mut conn,
                event.id,
                event.occurred_at,
                frames_json,
                status.as_str(),
                culprit.as_deref(),
            )
            .await;
            // Best-effort, and deliberately not gated on the write above
            // reporting a row: both are repairs of a value that is only ever
            // displayed, and neither failing may cost the caller the response.
            if let Some(c) = culprit.as_deref() {
                let _ = sauron_db::repo::update_issue_culprit_if_latest(
                    &mut conn,
                    event.issue_id,
                    c,
                    event.occurred_at,
                )
                .await;
            }
        }
    }

    // On the response regardless of `hot` — a cold-partition event may not be
    // written back (the tiering drop-guard), but the caller still asked for
    // this event and should see the resolved name rather than the minified one
    // the row happens to hold.
    if let Some(c) = culprit {
        event.culprit = Some(c);
    }

    event.stacktrace_symbolicated = serde_json::to_value(&resolved)
        .ok()
        .filter(|v| !v.is_null());
    if event.stacktrace_symbolicated.is_none() {
        event.stacktrace_symbolicated = Some(Value::Array(Vec::new()));
    }
    event.symbolication_status = status.as_str().to_string();

    deobfuscate_type_on_read(state, fetch, event).await;
}

/// Replace an obfuscated Dart class name with the real one, if a map has been
/// uploaded for this build.
///
/// The counterpart of `backfill_culprit_from_stored` for the OTHER half of what
/// a reader sees. Symbolication makes the frames readable; only the obfuscation
/// map makes the *type* readable, because the Flutter SDK sends
/// `error.runtimeType.toString()` and under `--obfuscate` that string is
/// already the renamed identifier on the wire. Nothing derived from DWARF can
/// recover it.
///
/// Writes to `title` (and the issue's `type`/`title`), never to
/// `exception_type` or the fingerprint — see `update_issue_display_if_latest`.
/// `exception_type` is the verbatim wire value and stays that way, so grouping
/// cannot move under an app that uploads a map late.
async fn deobfuscate_type_on_read(state: &AppState, fetch: &SqlBlobFetch, event: &mut ErrorEvent) {
    // Dart only, and only when there is something to look the name up by.
    let Some(build_id) = event
        .debug_meta
        .as_ref()
        .and_then(|d| d.get("build_id"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    if event.exception_type.is_empty() {
        return;
    }
    let Some(original) = state
        .symbolicator
        .deobfuscate_type(fetch, Some(build_id), &event.exception_type)
        .await
    else {
        return;
    };

    // `build_title`'s shape, rebuilt here rather than imported: the pipeline
    // crate owns that function and this is the API. Kept in sync by the fact
    // that both are "{type}: {value}" truncated at 200, which is asserted in
    // `sauron-pipeline`'s tests.
    let value = event.exception_value.trim();
    let title = if value.is_empty() {
        original.clone()
    } else {
        let mut t = format!("{original}: {value}");
        if let Some((idx, _)) = t.char_indices().nth(200) {
            t.truncate(idx);
        }
        t
    };
    if event.title.as_deref() == Some(title.as_str()) {
        return;
    }

    let hot = event.occurred_at > Utc::now() - Duration::days(state.cfg.tier_hot_days);
    if hot {
        if let Ok(mut conn) = sauron_db::conn(&state.pool).await {
            let _ =
                sauron_db::repo::update_event_title(&mut conn, event.id, event.occurred_at, &title)
                    .await;
            let _ = sauron_db::repo::update_issue_display_if_latest(
                &mut conn,
                event.issue_id,
                &original,
                &title,
                event.occurred_at,
            )
            .await;
        }
    }
    event.title = Some(title);
}

#[cfg(test)]
mod tests {
    use super::*;

    use sauron_auth::perm;
    use serde_json::json;
    use std::collections::HashSet;

    /// An event with **every** field populated to something non-null, so the
    /// census test below can read "was this withheld?" straight off the
    /// serialized JSON instead of trusting a hand-kept list.
    fn fully_populated_event() -> ErrorEvent {
        let now = Utc::now();
        ErrorEvent {
            id: Uuid::nil(),
            app_id: Uuid::nil(),
            environment_id: Some(Uuid::nil()),
            issue_id: Uuid::nil(),
            fingerprint: "fp".into(),
            level: "error".into(),
            message: "boom".into(),
            exception_type: "TypeError".into(),
            exception_value: "undefined is not a function".into(),
            stacktrace: json!([{ "function": "boom" }]),
            breadcrumbs: json!([{ "message": "clicked" }]),
            context: json!({ "request": { "cookies": "session=secret" } }),
            tags: json!({ "customer": "acme" }),
            release: Some("1.2.3".into()),
            distinct_id: Some("person-1".into()),
            event_user: Some(json!({ "email": "person@example.test" })),
            sdk: Some(json!({ "name": "sauron.js" })),
            ip_address: Some("203.0.113.9".into()),
            occurred_at: now,
            received_at: now,
            session_id: Some("session-1".into()),
            device_key: Some("device-1".into()),
            screen: Some("Home".into()),
            stacktrace_symbolicated: Some(json!([{ "function": "boom", "context_line": "x" }])),
            symbolication_status: "symbolicated".into(),
            debug_meta: Some(json!({ "raw_stacktrace": "#00 abs 0x1" })),
            contexts: json!({ "app": { "build": "42" } }),
            extra: json!({ "api_key": "leak-me" }),
            handled: Some(true),
            title: Some("TypeError: undefined is not a function".into()),
            culprit: Some("boom (app.ts)".into()),
        }
    }

    fn perms(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// Keys of `v` whose value serialized to `null` — i.e. what the gate
    /// withheld.
    fn null_keys(v: &Value) -> Vec<String> {
        let mut ks: Vec<String> = v
            .as_object()
            .expect("ErrorEvent serializes to an object")
            .iter()
            .filter(|(_, val)| val.is_null())
            .map(|(k, _)| k.clone())
            .collect();
        ks.sort();
        ks
    }

    /// The census. Pins BOTH halves of the decision — which fields exist and
    /// which of them the strip withholds — so adding a field to `ErrorEvent`
    /// fails here and forces a body/shell ruling on it, rather than defaulting
    /// it into the shell and leaking silently.
    #[test]
    fn strip_event_body_pins_exactly_which_fields_survive() {
        let mut ev = fully_populated_event();
        assert!(
            null_keys(&serde_json::to_value(&ev).expect("serialize")).is_empty(),
            "the fixture must populate every field, or a withheld key below proves nothing"
        );

        strip_event_body(&mut ev);
        let v = serde_json::to_value(&ev).expect("serialize");

        let mut all: Vec<String> = v
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        all.sort();
        assert_eq!(
            all,
            [
                "app_id",
                "breadcrumbs",
                "context",
                "contexts",
                "culprit",
                "debug_meta",
                "device_key",
                "distinct_id",
                "environment_id",
                "event_user",
                "exception_type",
                "exception_value",
                "extra",
                "fingerprint",
                "handled",
                "id",
                "ip_address",
                "issue_id",
                "level",
                "message",
                "occurred_at",
                "received_at",
                "release",
                "screen",
                "sdk",
                "session_id",
                "stacktrace",
                "stacktrace_symbolicated",
                "symbolication_status",
                "tags",
                "title",
            ],
            "a field was added to or removed from `ErrorEvent` — decide whether it is body or \
             shell and update `strip_event_body` before updating this list"
        );

        assert_eq!(
            null_keys(&v),
            [
                "breadcrumbs",
                "context",
                "contexts",
                "culprit",
                "debug_meta",
                "event_user",
                "extra",
                "ip_address",
                "sdk",
                "stacktrace",
                "stacktrace_symbolicated",
                "tags",
            ],
            "the withheld set changed"
        );

        // Spot-check the shell by value, not just by non-nullness: the point of
        // stripping rather than dropping the row is that the occurrence stays
        // readable.
        assert_eq!(v["message"], "boom");
        assert_eq!(v["exception_type"], "TypeError");
        assert_eq!(v["release"], "1.2.3");
        assert_eq!(v["distinct_id"], "person-1");
        assert_eq!(v["session_id"], "session-1");
        assert_eq!(v["device_key"], "device-1");
        assert_eq!(v["screen"], "Home");
        assert_eq!(v["handled"], true);
        // The half of the title/culprit pair that survives: it is
        // `exception_type` and `exception_value` joined, both of which are two
        // lines above, so withholding it would withhold nothing.
        assert_eq!(v["title"], "TypeError: undefined is not a function");
    }

    #[test]
    fn gate_event_body_keeps_the_body_only_for_both_permissions() {
        let mut events = vec![fully_populated_event(), fully_populated_event()];
        gate_event_body(&perms(&[perm::ISSUE_READ, perm::EVENT_READ]), &mut events);
        for ev in &events {
            assert_eq!(ev.stacktrace, json!([{ "function": "boom" }]));
            assert!(ev.event_user.is_some());
        }
    }

    /// The three failing combinations, each asserted over TWO events — a gate
    /// written `.iter_mut().take(1)` would still pass on a one-element vec.
    #[test]
    fn gate_event_body_strips_when_either_permission_is_missing() {
        for held in [
            vec![perm::ISSUE_READ],
            vec![perm::EVENT_READ],
            vec![perm::SOURCE_READ],
            vec![],
        ] {
            let mut events = vec![fully_populated_event(), fully_populated_event()];
            gate_event_body(&perms(&held), &mut events);
            for (i, ev) in events.iter().enumerate() {
                assert!(
                    ev.stacktrace.is_null()
                        && ev.breadcrumbs.is_null()
                        && ev.contexts.is_null()
                        && ev.extra.is_null()
                        && ev.event_user.is_none()
                        && ev.stacktrace_symbolicated.is_none(),
                    "event #{i} kept a body for a caller holding {held:?}"
                );
                // Still an occurrence, not a hole.
                assert_eq!(ev.message, "boom");
            }
        }
    }

    /// `source:read` is layered on top of the body gate, not an escape from it:
    /// holding it without the pair must still yield no frames at all.
    #[test]
    fn source_read_does_not_substitute_for_the_body_pair() {
        let held = perms(&[perm::ISSUE_READ, perm::SOURCE_READ]);
        assert!(!may_read_event_body(&held));
        let mut events = vec![fully_populated_event()];
        gate_source_context(&held, &mut events);
        gate_event_body(&held, &mut events);
        assert!(events[0].stacktrace_symbolicated.is_none());
    }

    /// The searchable set must move in lockstep with the readable one.
    ///
    /// Asserted as an EQUIVALENCE against `strip_event_body`'s own behaviour
    /// rather than by restating "issue:read and event:read": the whole reason
    /// `text_search_reach` delegates to `may_read_event_body` is that a second
    /// copy of the predicate could drift, and a test that also restated it would
    /// drift with it. The loop covers every subset of the three permissions, so
    /// a future change to either side that breaks the correspondence fails here.
    #[test]
    fn the_searchable_columns_track_the_readable_ones_exactly() {
        let all = [perm::ISSUE_READ, perm::EVENT_READ, perm::SOURCE_READ];
        for mask in 0u8..8 {
            let held: Vec<&str> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, p)| *p)
                .collect();
            let p = perms(&held);

            let mut ev = fully_populated_event();
            gate_event_body(&p, std::slice::from_mut(&mut ev));
            // `extra`/`contexts`/`tags` are the three columns the payload scan
            // reads; if the gate nulled them, searching them would answer a
            // question the response refused to.
            let body_withheld = ev.extra.is_null() && ev.contexts.is_null() && ev.tags.is_null();

            assert_eq!(
                text_search_reach(&p) == sauron_db::repo::TextSearchReach::ShellOnly,
                body_withheld,
                "permissions {held:?}: free-text reach and the body gate disagree — one of them \
                 says the payload columns are off limits and the other does not, which is \
                 exactly the oracle `TextSearchReach` exists to close"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Transactions. `extra` here is where a request or response body lands, so
    // it is the same class of data as `ErrorEvent::extra` and gets the same
    // treatment.
    // -----------------------------------------------------------------------

    fn fully_populated_transaction() -> Transaction {
        let now = Utc::now();
        Transaction {
            id: Uuid::nil(),
            app_id: Uuid::nil(),
            environment_id: Some(Uuid::nil()),
            name: "POST /orders".into(),
            op: "http".into(),
            duration_ms: 128.4,
            status: Some("ok".into()),
            http_method: Some("POST".into()),
            http_status: Some(201),
            url: Some("https://api.example.com/orders".into()),
            distinct_id: Some("u_123".into()),
            session_id: Some("s_1".into()),
            device_key: Some("dev_1".into()),
            release: Some("1.2.3".into()),
            ip_address: Some("203.0.113.7".into()),
            occurred_at: now,
            received_at: now,
            workflow_id: Some("wf_1".into()),
            workflow_name: Some("checkout".into()),
            // Populated like every other field: the assertion below is that
            // NOTHING starts null, so a `None` here would make the strip's
            // effect indistinguishable from the fixture's own gaps.
            restored_pin_id: Some(Uuid::nil()),
            finished_at: Some(now),
            tags: json!({ "tier": "premium" }),
            extra: json!({ "request": "{\"item\":1}", "response": "{\"id\":9}" }),
        }
    }

    fn tx_null_keys(v: &Value) -> Vec<String> {
        let mut ks: Vec<String> = v
            .as_object()
            .expect("Transaction serializes to an object")
            .iter()
            .filter(|(_, val)| val.is_null())
            .map(|(k, _)| k.clone())
            .collect();
        ks.sort();
        ks
    }

    /// The census, for transactions. Pins BOTH halves — which fields exist and
    /// which the strip withholds — so adding a field to `Transaction` fails
    /// here and forces a body/shell ruling rather than defaulting it into the
    /// shell and leaking silently.
    #[test]
    fn strip_transaction_body_withholds_exactly_the_developer_payload() {
        let mut t = fully_populated_transaction();
        let before = serde_json::to_value(&t).expect("serialize");
        assert!(
            tx_null_keys(&before).is_empty(),
            "the fixture must start with nothing null, or this test proves nothing"
        );

        strip_transaction_body(&mut t);
        let v = serde_json::to_value(&t).expect("serialize");

        let mut all: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        all.sort();
        assert_eq!(
            all,
            [
                "app_id",
                "device_key",
                "distinct_id",
                "duration_ms",
                "environment_id",
                "extra",
                "finished_at",
                "http_method",
                "http_status",
                "id",
                "ip_address",
                "name",
                "occurred_at",
                "op",
                "received_at",
                "release",
                "restored_pin_id",
                "session_id",
                "status",
                "tags",
                "url",
                "workflow_id",
                "workflow_name",
            ],
            "a field was added to or removed from `Transaction` — decide whether it is body \
             or shell and update `strip_transaction_body` before updating this list"
        );

        assert_eq!(
            tx_null_keys(&v),
            ["extra", "ip_address", "tags"],
            "the withheld set changed"
        );

        // The shell has to SURVIVE, or a coarse-gated caller gets an empty list
        // that reads as "no data" rather than "this happened, and you may not
        // see what was attached to it".
        assert_eq!(v["name"], "POST /orders");
        assert_eq!(v["duration_ms"], 128.4);
        assert_eq!(v["http_status"], 201);
        // `url` stays on purpose: it is the label of an HTTP span, and `name`
        // usually repeats it, so withholding one and not the other would
        // withhold nothing while making every list unreadable.
        assert_eq!(v["url"], "https://api.example.com/orders");
    }

    /// **The invariant, asserted as a PAIR in one test.**
    ///
    /// What you may search must be exactly what you may read back. Split across
    /// two tests, the half that matters can pass while the other rots — and the
    /// rot that matters (searchable wider than readable) is silent, because a
    /// substring probe over a withheld column returns a row count rather than
    /// an error.
    #[test]
    fn transaction_read_and_search_gates_move_together() {
        for (label, perms) in [
            ("no perms", HashSet::<String>::new()),
            (
                "issue:read only — a span is not an issue",
                HashSet::from([perm::ISSUE_READ.to_string()]),
            ),
        ] {
            assert!(
                !may_read_transaction_body(&perms),
                "{label}: body must be withheld"
            );
            assert_eq!(
                transaction_text_search_reach(&perms),
                sauron_db::repo::TextSearchReach::ShellOnly,
                "{label}: the body is withheld but the free-text scan still reaches it — \
                 exactly the byte-at-a-time oracle this pairing exists to close"
            );

            let mut t = fully_populated_transaction();
            gate_transaction_body(&perms, std::slice::from_mut(&mut t));
            assert!(t.extra.is_null(), "{label}: extra survived the gate");
            assert!(t.tags.is_null(), "{label}: tags survived the gate");
        }

        // And the readable direction, so the test cannot pass by refusing
        // everyone.
        let allowed = HashSet::from([perm::EVENT_READ.to_string()]);
        assert!(may_read_transaction_body(&allowed));
        assert_eq!(
            transaction_text_search_reach(&allowed),
            sauron_db::repo::TextSearchReach::IncludingBody
        );
        let mut t = fully_populated_transaction();
        gate_transaction_body(&allowed, std::slice::from_mut(&mut t));
        assert_eq!(t.extra["request"], "{\"item\":1}");
        assert_eq!(t.tags["tier"], "premium");
    }

    /// A transaction body is gated on `event:read` ALONE — deliberately NOT
    /// `may_read_event_body`'s `issue:read AND event:read`.
    ///
    /// Requiring issue-reading rights to see an HTTP span's payload would read
    /// as a bug to whoever hit it, and `sessions::detail` already authorizes on
    /// `event:read`, so this composes there without widening that route.
    #[test]
    fn transaction_gating_does_not_require_issue_read() {
        let perms = HashSet::from([perm::EVENT_READ.to_string()]);
        assert!(may_read_transaction_body(&perms));
        // The error-body gate, for contrast, refuses the same caller.
        assert!(!may_read_event_body(&perms));
    }
}
