//! The searched TRANSACTIONS list — individual performance spans, as opposed to
//! `performance::summary`'s one-row-per-operation aggregate.
//!
//! This is the surface a developer-supplied `extra` is searchable on. Before it
//! existed, `Resource::Transactions` was declared in the query catalog with
//! nothing to return: `/performance/summary` and `/performance/series` are both
//! aggregates, and the only per-span display was the session timeline, which
//! you can only reach once you already know which session to open.
//!
//! **Gating lives on `event:read` and the reach is derived from it**, never
//! restated — see `symbolicate::transaction_text_search_reach`. `extra` is
//! where request and response bodies land, so "what you may search" and "what
//! you may read back" have to be the same set or `?q=` becomes an oracle over
//! the withheld half.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::Transaction;
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

/// The window columns this list accepts.
///
/// `received_at` is offered alongside `occurred_at` for the reason the other
/// lists offer it: a device with a skewed clock (or a long offline queue) files
/// spans under a timestamp the server never saw, and "what arrived in the last
/// hour" is a different and sometimes more useful question than "what happened
/// in the last hour".
pub const TIME_FIELDS: &[&str] = &["occurred_at", "received_at"];

/// The orderings this list can page. Every one is a keyset walk with `id` as
/// the tiebreaker — see `repo::TransactionSort`.
pub const SORT_FIELDS: &[&str] = &["occurred_at", "duration_ms", "name", "op"];

fn default_days() -> i64 {
    7
}

fn default_limit() -> i64 {
    50
}

/// Same ceiling as the Events list, and for the same reason: free text here
/// scans `jsonb::text` over a partitioned table that no index can serve, so the
/// window stays bounded rather than defaulting to effectively all history.
const MAX_SINCE_DAYS: i64 = 365;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    /// The query language. Wins over `filter`/`q` when non-empty.
    pub query: Option<String>,
    /// `column` or `-column`, restricted to [`SORT_FIELDS`].
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// `time_field` / `from` / `to` / `since_days`, flattened so the precedence
    /// between them is decided once, in `resolve_time_filter`.
    #[serde(flatten)]
    pub window: super::search::TimeFilterQuery,
    #[serde(default = "default_limit")]
    pub limit: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see
    // `routes::scope`'s module docs for the extractor trap that avoids.
}

/// `GET /v1/apps/{app_id}/transactions`
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<super::search::SearchEnvelope<Transaction>>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms` rather than the plain `authorized_read_scope`: `event:read`
    // authorizes the list, and the caller's `env:read` at that same resolved
    // scope decides whether an `environment:<name>` predicate is answerable.
    // One ancestry+grant resolution answers both.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Transactions,
    )?;

    // The reach is DERIVED, not chosen here. `transaction_text_search_reach`
    // and `gate_transaction_body` read the same predicate, which is the only
    // thing keeping "searchable" and "readable" the same set.
    let reach = crate::symbolicate::transaction_text_search_reach(&perms);
    super::search::reject_withheld_dimensions(
        &node,
        reach,
        super::search::EnvNameReach::for_perms(&perms),
    )?;

    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;

    let (sort_col, descending) =
        super::search::parse_sort(q.sort.as_deref(), SORT_FIELDS, "occurred_at")?;
    // `parse_sort` already refused anything outside the list, so this cannot be
    // None; the expect states that rather than inventing a fallback ordering
    // that would page unstably if the two lists ever drifted apart.
    let sort = repo::TransactionSort::from_column(&sort_col)
        .expect("SORT_FIELDS and TransactionSort::from_column must agree");
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            // `sort.is_temporal()` is passed so a cursor minted under one kind
            // of column cannot be replayed against another — the cursor's key
            // and its value tag are independent fields on the wire.
            sauron_db::query_plan::cursor::decode(c, &sort_col, sort.is_temporal())
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };

    // `Clamp.field` is the GENERIC name "since" — `prepare` does not know which
    // resource it ran for. On THIS resource the window column is `occurred_at`
    // (or `received_at`, when the caller asked for it).
    let window = super::search::resolve_time_filter(
        "occurred_at",
        TIME_FIELDS,
        &q.window,
        Utc::now(),
        default_days(),
        MAX_SINCE_DAYS,
        prepared.clamp,
    )?;
    let limit = q.limit.clamp(1, 200);

    let search = repo::TransactionSearch {
        node: &node,
        ctx: &prepared.ctx,
        text_reach: reach,
        since: window.from,
        until: window.to,
        sort,
        descending,
        after,
        limit,
    };

    let mut rows = repo::search_transactions(&mut conn, &scope, &search)
        .await
        .map_err(super::search::map_plan_error)?;
    let (total, total_is_capped) =
        repo::count_transactions(&mut conn, &scope, &search, super::search::COUNT_CAP)
            .await
            .map_err(super::search::map_plan_error)?;

    // `limit + 1` rows were fetched; the surplus one is the has-more probe and
    // must not be served.
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    // Minted BEFORE the body gate: `cursor_value` reads `occurred_at`/
    // `duration_ms`/`name`/`op`, none of which `strip_transaction_body` touches,
    // but taking the cursor off the pre-strip rows makes that independence
    // explicit rather than incidental.
    let next_cursor = has_more.then(|| {
        let last = rows.last().expect("has_more implies a row");
        sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
            key: sort_col.clone(),
            value: sort.cursor_value(last),
            id: last.id,
        })
    });

    // The withholding half of the same predicate `reach` came from. A caller
    // without `event:read` never reaches this line today (the route authorizes
    // on it), but the gate is what makes that a property of the code rather
    // than of the current authorization constant.
    crate::symbolicate::gate_transaction_body(&perms, &mut rows);

    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: window.clamped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two lists that decide which orderings exist must agree, or
    /// `parse_sort` admits a column `TransactionSort` cannot build and the
    /// `expect` above becomes a panic on a perfectly ordinary request.
    #[test]
    fn every_sort_field_maps_to_a_sort_variant() {
        for f in SORT_FIELDS {
            assert!(
                repo::TransactionSort::from_column(f).is_some(),
                "SORT_FIELDS advertises `{f}` but TransactionSort cannot build it"
            );
        }
    }

    /// And the reverse direction: a variant with no wire spelling is
    /// unreachable, which is a quieter bug than the one above.
    #[test]
    fn every_sort_variant_is_advertised() {
        for v in [
            repo::TransactionSort::OccurredAt,
            repo::TransactionSort::DurationMs,
            repo::TransactionSort::Name,
            repo::TransactionSort::Op,
        ] {
            assert!(
                SORT_FIELDS.contains(&v.column()),
                "TransactionSort::{v:?} is not advertised in SORT_FIELDS"
            );
        }
    }

    /// `occurred_at` is the only temporal column here. A cursor minted under
    /// `duration_ms` carries text, and `decode` is handed `is_temporal()` to
    /// enforce that — if this ever returned true for a numeric column, a
    /// timestamp-tagged cursor would be accepted against a double comparison.
    #[test]
    fn only_occurred_at_is_temporal() {
        assert!(repo::TransactionSort::OccurredAt.is_temporal());
        assert!(!repo::TransactionSort::DurationMs.is_temporal());
        assert!(!repo::TransactionSort::Name.is_temporal());
        assert!(!repo::TransactionSort::Op.is_temporal());
    }
}
