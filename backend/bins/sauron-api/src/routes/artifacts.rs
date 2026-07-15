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
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::NewSymbolArtifact;
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

fn blank_to_none(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

pub async fn upload(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(p): Query<UploadParams>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
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

    let (release, dist, name, debug_id) = (
        blank_to_none(p.release),
        blank_to_none(p.dist),
        blank_to_none(p.name),
        blank_to_none(p.debug_id),
    );
    let arch = blank_to_none(p.arch);

    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ARTIFACT_WRITE).await?;

    let sha = sauron_symbols::sha256(&body);
    let sha_hex = sauron_symbols::hex(&sha);

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
        return Ok((
            StatusCode::OK,
            Json(json!({
                "id": a.id,
                "blob_sha256": sha_hex,
                "deduped": true,
            })),
        ));
    }

    let compressed = sauron_symbols::compress(&body);
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

    let art = repo::insert_symbol_artifact(
        &mut conn,
        NewSymbolArtifact {
            app_id,
            kind: p.kind,
            platform: p.platform,
            arch,
            release,
            dist,
            name,
            debug_id,
            blob_sha256: sha.to_vec(),
            prebuilt_index_sha256: None,
            uploaded_by: Some(auth.user_id),
        },
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": art.id,
            "blob_sha256": sha_hex,
            "deduped": false,
        })),
    ))
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
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
) -> Result<StatusCode, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ARTIFACT_WRITE).await?;
    if repo::delete_symbol_artifact(&mut conn, app_id, artifact_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
