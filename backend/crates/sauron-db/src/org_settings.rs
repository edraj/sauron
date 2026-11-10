//! Per-organization switches.

use diesel::prelude::*;
use diesel::sql_types::{Bool, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::organizations;

pub async fn mcp_enabled(conn: &mut AsyncPgConnection, org_id: Uuid) -> QueryResult<bool> {
    organizations::table
        .find(org_id)
        .select(organizations::mcp_enabled)
        .first(conn)
        .await
}

#[derive(QueryableByName)]
struct Previous {
    #[diesel(sql_type = Bool)]
    previous: bool,
}

/// Set the MCP switch and return the previous value. One statement: the CTE
/// locks the row and reads the old value, so concurrent writers each see the
/// value the other left.
pub async fn set_mcp_enabled(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    enabled: bool,
) -> QueryResult<bool> {
    let row: Previous = diesel::sql_query(
        "WITH old AS (SELECT mcp_enabled FROM organizations WHERE id = $1 FOR UPDATE) \
         UPDATE organizations o SET mcp_enabled = $2, updated_at = now() \
         FROM old WHERE o.id = $1 RETURNING old.mcp_enabled AS previous",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<Bool, _>(enabled)
    .get_result(conn)
    .await?;
    Ok(row.previous)
}
