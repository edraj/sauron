//! Storage & records report endpoint (any authenticated user).

use axum::extract::State;
use axum::Json;

use sauron_auth::AuthUser;

use crate::admin_storage::{collect_storage, StorageReport};
use crate::error::ApiError;
use crate::AppState;

/// Deployment-wide storage & record report. Any authenticated user may view it.
pub async fn storage(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<StorageReport>, ApiError> {
    let report = collect_storage(&state).await?;
    Ok(Json(report))
}
