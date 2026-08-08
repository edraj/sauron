//! App-scoped symbol artifacts: upload / list / delete source maps and Dart
//! debug-info. Content-addressed + deduped; gated by `artifact:write`.
//!
//! Upload is `POST /v1/apps/{app_id}/artifacts` with the raw file as the request
//! body and metadata as query params (avoids multipart). The body-size limit is
//! raised for these routes in `main.rs`.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{NewSymbolArtifact, SymbolArtifact};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

const KINDS: [&str; 2] = ["js_sourcemap", "dart_symbols"];
const PLATFORMS: [&str; 3] = ["web", "android", "ios"];

#[derive(Debug, Deserialize)]
pub struct UploadParams {
    pub kind: String,
    pub platform: String,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub dist: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub debug_id: Option<String>,
}

/// Trim, then treat an all-whitespace value as absent.
///
/// The trim is not cosmetic — the value returned here is the value *stored*, and
/// every one of these columns is later compared verbatim. This used to filter on
/// `v.trim().is_empty()` while returning the **untrimmed** `String`, so
/// `?debug_id=%20ab36…` was accepted and stored with its leading space, where it
/// could never match the id the VM prints at crash time: a mute `no_artifacts`
/// weeks later with nothing in the UI to explain it. `release` and `name` have
/// the same exposure through the JS matcher's equality tests.
///
/// The trim on `release` is the write half of `sauron_symbols::normalize_release`,
/// which the read path applies to the event's own release before matching. The
/// two must agree; if this ever needs to do more than trim, it belongs in that
/// module so both sides inherit it.
fn blank_to_none(s: Option<String>) -> Option<String> {
    let trimmed = s?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Did this statement lose a race against a UNIQUE index?
fn is_unique_violation(e: &DieselError) -> bool {
    matches!(
        e,
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _)
    )
}

/// The 200 body for an upload whose artifact already exists.
///
/// Built in one place because there are two routes to it: the idempotency
/// lookup, and the unique-violation recovery below. A caller that raced must not
/// be able to tell which one answered it, so the two must not be free to drift.
///
/// `blob_sha256` describes **the row being returned**, not the request that
/// asked for it. Those are the same thing on the JS path (matched on the blob
/// among other columns) but not on the Dart path, which matches on `debug_id`
/// alone and never compares content: upload two *different* files under one
/// explicit `?debug_id=`, and the loser used to be handed
/// `{id: <the other row>, blob_sha256: <its own bytes>}` — a body asserting its
/// bytes are stored under that artifact when they are not, with nothing
/// anywhere to say otherwise. Reporting the stored row's hash turns that into a
/// mismatch the uploader (or its CI job) can see and diff.
fn dedupe_response(
    art: &SymbolArtifact,
    debug_id: &Option<String>,
    derived_debug_id: &Option<String>,
) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "id": art.id,
            "blob_sha256": sauron_symbols::hex(&art.blob_sha256),
            "deduped": true,
            "debug_id": debug_id,
            "derived_debug_id": derived_debug_id,
        })),
    )
}

/// Log the one case where "already have it" is not the whole truth: the row we
/// are about to hand back holds **different bytes** than the ones just uploaded.
///
/// Only reachable through the `debug_id` lookup, which matches on the id alone —
/// two different ELFs uploaded under one explicit `?debug_id=` (a copy-pasted id,
/// or the same id passed for two architectures) both resolve to the first row,
/// and the second file is silently not stored. The response now reports the
/// stored row's `blob_sha256` so a caller can see it; this puts the same fact
/// where an operator debugging "why are my traces still obfuscated" will look.
fn warn_on_content_mismatch(art: &SymbolArtifact, uploaded_sha: &[u8]) {
    if art.blob_sha256 != uploaded_sha {
        tracing::warn!(
            app_id = %art.app_id,
            artifact_id = %art.id,
            debug_id = ?art.debug_id,
            stored_blob_sha256 = %sauron_symbols::hex(&art.blob_sha256),
            uploaded_blob_sha256 = %sauron_symbols::hex(uploaded_sha),
            "upload deduped onto an artifact holding DIFFERENT bytes; the uploaded file was \
             not stored (two files claiming one debug_id)"
        );
    }
}

pub async fn upload(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(p): Query<UploadParams>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // Symbol artifacts are app-wide, not per-environment (see `list`'s
    // comment below); rejected here too, matching `list` in this same file
    // rather than silently discarding it on writes alone.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    if !KINDS.contains(&p.kind.as_str()) {
        return Err(ApiError::BadRequest(
            "kind must be 'js_sourcemap' or 'dart_symbols'".into(),
        ));
    }
    if !PLATFORMS.contains(&p.platform.as_str()) {
        return Err(ApiError::BadRequest(
            "platform must be 'web', 'android', or 'ios'".into(),
        ));
    }
    if body.is_empty() {
        return Err(ApiError::BadRequest("artifact body is empty".into()));
    }
    let max = state.cfg.symbols_max_artifact_mb * 1024 * 1024;
    if body.len() > max {
        return Err(ApiError::BadRequest(format!(
            "artifact exceeds {} MB",
            state.cfg.symbols_max_artifact_mb
        )));
    }

    let (release, dist, name) = (
        blank_to_none(p.release),
        blank_to_none(p.dist),
        blank_to_none(p.name),
    );
    let arch = blank_to_none(p.arch);
    // Canonical form of the id this artifact will be matched on, from the one
    // definition BOTH sides read (`sauron_symbols::normalize`) — the read path
    // applies the same function to `debug_meta.build_id` and to the trace's own
    // `build_id:` header before looking a row up.
    //
    // An earlier round normalized here only, on the theory that the read side's
    // input was machine-generated and therefore already canonical. That was
    // wrong: `debug_meta.build_id` is an untrusted `Option<String>` off the wire
    // (`sauron_core::envelope::DebugMeta`), passed verbatim into
    // `find_artifact_by_debug_id`. Lowercasing one side alone did not remove the
    // asymmetry, it moved it — an id uploaded and reported as `AB36…` matched
    // before and stopped matching after. Never normalize one side of an equality
    // test.
    let debug_id = blank_to_none(p.debug_id).map(|v| sauron_symbols::normalize_debug_id(&v));

    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ARTIFACT_WRITE).await?;

    // SHA-256 over up to `symbols_max_artifact_mb` of data is CPU-bound; running
    // it inline parks a Tokio worker for the whole hash. Offload it (and the
    // much heavier zstd compression below) to the blocking pool so uploads never
    // stall unrelated request handling.
    let sha = {
        let body = body.clone();
        tokio::task::spawn_blocking(move || sauron_symbols::sha256(&body))
            .await
            .map_err(|e| ApiError::Internal(format!("hash task failed: {e}")))?
    };
    let sha_hex = sauron_symbols::hex(&sha);

    // Dart artifacts are matched at symbolication time on `debug_id` ALONE, so
    // an absent or mistyped id is not an error anyone sees — it is a silent
    // `no_artifacts` weeks later, indistinguishable from never having uploaded.
    // Deriving the id from the file's own GNU build-id note is what lets a
    // human upload one without first running `readelf -n` and pasting the
    // result. Must run BEFORE the idempotency lookup below, which keys on it.
    //
    // Runs after `authorize_app` (and, like the hash above and the compression
    // below, on the blocking pool) — walking an ELF's notes is CPU-bound work
    // on up to `symbols_max_artifact_mb`, and neither an unauthorized caller
    // nor an unrelated request handler should pay for it.
    let derived_debug_id: Option<String> = if p.kind == "dart_symbols" {
        let elf = body.clone();
        let derived = tokio::task::spawn_blocking(move || sauron_symbols::build_id_hex(&elf))
            .await
            .map_err(|e| ApiError::Internal(format!("build-id task failed: {e}")))?;
        match derived {
            Ok(id) => Some(id),
            // Unreadable note + no explicit id = an artifact that can never
            // match anything. Refusing here (400, quoting the reason: not an
            // ELF, no note, implausible counts) is the whole point: a silent
            // 201 would defer the failure to a symbolication that just gives
            // up. Not our fault, so not a 500.
            Err(e) if debug_id.is_none() => {
                return Err(ApiError::BadRequest(format!(
                    "could not derive a debug_id from this dart_symbols file ({e}); \
                     pass debug_id explicitly"
                )))
            }
            // An explicit id was supplied, so the upload is still matchable.
            // Toolchains whose note we cannot read are exactly why the override
            // exists; the response reports `derived_debug_id: null` so the
            // uploader can see that nothing corroborated their value.
            Err(_) => None,
        }
    } else {
        None
    };
    // Explicit wins — but both go in the response, so a typo shows up as a
    // visible disagreement at upload time instead of as `no_artifacts` later.
    let debug_id = debug_id.or_else(|| derived_debug_id.clone());

    // Idempotency: by debug-id (Dart) or (release, name, content) for JS.
    let existing = match debug_id.as_deref() {
        Some(did) => repo::find_artifact_by_debug_id(&mut conn, app_id, did).await?,
        None => {
            repo::find_artifact_by_release_name(
                &mut conn,
                app_id,
                release.as_deref(),
                name.as_deref(),
                &sha,
            )
            .await?
        }
    };
    if let Some(a) = existing {
        warn_on_content_mismatch(&a, &sha);
        return Ok(dedupe_response(&a, &debug_id, &derived_debug_id));
    }

    // zstd level 19 on a 128 MB artifact is seconds of CPU — by far the most
    // expensive thing this handler does, and the reason it must not run on the
    // async executor.
    let compressed = {
        let body = body.clone();
        tokio::task::spawn_blocking(move || sauron_symbols::compress(&body))
            .await
            .map_err(|e| ApiError::Internal(format!("compress task failed: {e}")))?
    };
    repo::put_blob(
        &mut conn,
        &sha,
        &compressed,
        body.len() as i64,
        compressed.len() as i64,
    )
    .await?;
    state.symbols.put(&sha_hex, &compressed).await;

    // NOTE (slice 2): for kind == "js_sourcemap", parse the map on upload into a
    // compact index, `put_blob` it, and set `prebuilt_index_sha256`.

    let inserted = repo::insert_symbol_artifact(
        &mut conn,
        NewSymbolArtifact {
            app_id,
            kind: p.kind,
            platform: p.platform,
            arch,
            release,
            dist,
            name,
            debug_id: debug_id.clone(),
            blob_sha256: sha.to_vec(),
            prebuilt_index_sha256: None,
            uploaded_by: Some(auth.user_id),
        },
    )
    .await;

    // The lookup above and this insert are not atomic, and the gap between them
    // is seconds wide — the zstd level 19 pass over up to
    // `symbols_max_artifact_mb` sits in it. `symbol_artifacts_debugid_idx` is a
    // real UNIQUE index on (app_id, debug_id), and now that every `dart_symbols`
    // upload carries a derived id, that index went from unreachable to routinely
    // hit: two uploads of the same symbols file (a slow upload plus an impatient
    // second click on a form is the everyday shape) both miss the lookup, and
    // the loser's insert violates it.
    //
    // A bare 500 is the wrong answer for the loser. The right answer is the same
    // dedupe 200 the winner's re-uploader gets, because by the time the loser is
    // told anything, the artifact it wanted really does exist.
    //
    // Recovery re-runs the lookup rather than matching on the constraint name:
    // that is self-validating — the 200 is only produced when we can actually
    // hand back the row it refers to, and any other unique violation still
    // surfaces as itself.
    let art = match inserted {
        Ok(a) => a,
        Err(e) if is_unique_violation(&e) => {
            let raced = match debug_id.as_deref() {
                Some(did) => repo::find_artifact_by_debug_id(&mut conn, app_id, did).await?,
                None => None,
            };
            let Some(a) = raced else {
                return Err(e.into());
            };
            // Worth a line in the log: the loser paid for a full compress and
            // blob write whose only trace is `symbol_blobs.refcount` sitting one
            // higher than the number of artifacts referencing it, so deleting
            // the artifact will not GC the bytes. Benign (the same stale-refcount
            // condition `delete_symbol_artifact` already documents as
            // acceptable), and *not* corrected here on purpose: a bare decrement
            // can drive a still-referenced blob to zero, and the GC delete then
            // fails the foreign key — turning the 200 this arm exists to produce
            // straight back into a 500.
            tracing::warn!(
                app_id = %app_id,
                debug_id = ?debug_id,
                artifact_id = %a.id,
                "concurrent upload of the same debug_id lost the insert race; \
                 returning the existing artifact (symbol_blobs.refcount is now one high)"
            );
            warn_on_content_mismatch(&a, &sha);
            return Ok(dedupe_response(&a, &debug_id, &derived_debug_id));
        }
        Err(e) => return Err(e.into()),
    };

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": art.id,
            "blob_sha256": sha_hex,
            "deduped": false,
            "debug_id": debug_id,
            "derived_debug_id": derived_debug_id,
        })),
    ))
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    // Symbol artifacts (source maps / debug info) are app-wide, uploaded per
    // release/debug-id, not per environment; rejected rather than silently
    // accepted-and-ignored.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    let rows = repo::list_artifacts_with_sizes(&mut conn, app_id).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(a, uncompressed_size, compressed_size)| {
            json!({
                "id": a.id,
                "kind": a.kind,
                "platform": a.platform,
                "arch": a.arch,
                "release": a.release,
                "dist": a.dist,
                "name": a.name,
                "debug_id": a.debug_id,
                "blob_sha256": sauron_symbols::hex(&a.blob_sha256),
                "has_prebuilt_index": a.prebuilt_index_sha256.is_some(),
                "uncompressed_size": uncompressed_size,
                "compressed_size": compressed_size,
                "created_at": a.created_at,
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, artifact_id)): Path<(Uuid, Uuid)>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<StatusCode, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ARTIFACT_WRITE).await?;
    if repo::delete_symbol_artifact(&mut conn, app_id, artifact_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
