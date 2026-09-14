//! `app_releases`: the observed release catalogue behind the dashboard's
//! release switcher. See migration 78 for why the identity is an expression
//! index. Three entry points: the pipeline's `upsert_seen`, the API's
//! `list_for_app`, and the operator-run `backfill_all`.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::scope::ReadScope;
use crate::PgPool;

#[derive(Debug, Clone, QueryableByName, serde::Serialize, utoipa::ToSchema)]
pub struct AppReleaseRow {
    #[diesel(sql_type = Text)]
    pub release: String,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen_at: DateTime<Utc>,
}

/// The widening tail, shared by `upsert_seen` and `backfill_all` so the two
/// can never drift into disagreeing about what a re-sighting does.
///
/// The ON CONFLICT target MUST name the COALESCE expression (migration 78's
/// `app_releases_identity_idx`): the bare column list compiles, runs, and
/// inserts duplicates for unattributed rows, because NULL never equals NULL.
/// The nil uuid is spelled out rather than interpolated because `const`s
/// cannot `format!` — it is the same literal the migration's index uses.
const ON_CONFLICT_WIDEN: &str = concat!(
    "ON CONFLICT (app_id, COALESCE(environment_id, ",
    "'00000000-0000-0000-0000-000000000000'::uuid",
    "), release) DO UPDATE SET ",
    "first_seen_at = LEAST(app_releases.first_seen_at, EXCLUDED.first_seen_at), ",
    "last_seen_at  = GREATEST(app_releases.last_seen_at, EXCLUDED.last_seen_at)"
);

/// Unicode `White_Space`, as the second argument to `btrim` — i.e. exactly the
/// set of characters Rust's `str::trim` strips, which is the rule the ingest
/// edge and `sauron_pipeline::releases` both already apply before a release
/// ever reaches `app_releases`. `backfill_all` reads rows written before those
/// rules existed, so it has to reproduce the rule rather than assume it.
///
/// **Not `btrim(release)` and not the regex `\s`.** Bare `btrim` strips ASCII
/// space only. Postgres's `\s` is `[[:space:]]`, a libc `iswspace()` class:
/// under this server's `en_US.utf8` it matches U+3000 but NOT U+00A0 (NBSP)
/// or U+2007, both of which Rust's `str::trim` does strip — so `\s` would
/// leave an NBSP-only release in the catalogue as a switcher entry with an
/// invisible label. Spelled as `\uXXXX` escapes so this file stays ASCII and
/// the set is reviewable character by character: U+0009..U+000D, U+0020,
/// U+0085, U+00A0, U+1680, U+2000..U+200A, U+2028, U+2029, U+202F, U+205F,
/// U+3000.
const WS: &str = r"E'\u0009\u000A\u000B\u000C\u000D \u0085\u00A0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200A\u2028\u2029\u202F\u205F\u3000'";

/// One row per (app, env, release), widened on conflict.
pub async fn upsert_seen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Option<Uuid>,
    release: &str,
    at: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::sql_query(format!(
        "INSERT INTO app_releases (app_id, environment_id, release, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $4) {ON_CONFLICT_WIDEN}"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Nullable<SqlUuid>, _>(environment_id)
    .bind::<Text, _>(release)
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Every observed release row the scope may see, newest `last_seen_at` first.
pub async fn list_for_app(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
) -> QueryResult<Vec<AppReleaseRow>> {
    // `EnvFilter::sql_fragment(bind_index)` returns a string that already
    // BEGINS with " AND " (or is empty for `All`) — see its doc comment — so
    // this interpolates directly onto `$1` with no separator of its own.
    let env_sql = scope.env.sql_fragment(2);
    let sql = format!(
        "SELECT release, environment_id, first_seen_at, last_seen_at \
         FROM app_releases WHERE app_id = $1{env_sql} \
         ORDER BY last_seen_at DESC, release, environment_id"
    );
    // `bind_env!` matches on the `EnvFilter` variant itself (not on
    // `bind_uuids()`) so `One` binds a scalar `Uuid` matching its `= $2` and
    // `Subset` binds an `Array<Uuid>` matching its `= ANY($2)` — the two
    // shapes are not interchangeable, and `All`/`Unattributed` bind nothing.
    let stmt = diesel::sql_query(sql)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id);
    let stmt = crate::bind_env!(stmt, &scope.env);
    stmt.load(conn).await
}

/// What one `backfill_all` run did, in the two numbers an operator needs to
/// see in the command's output.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillOutcome {
    /// `app_releases` rows inserted or widened.
    pub upserted: u64,
    /// Historical rows across all of [`RELEASE_TABLES`] whose stored `release`
    /// was rewritten in place — blanked to NULL, or trimmed. Named for the
    /// event tables because they are the bulk of it; it counts the session,
    /// transaction and workflow repairs too.
    pub normalised_event_rows: u64,
}

/// The two tables the catalogue is SEEDED from, in a fixed order.
///
/// Only these two: the seed dates a release by `MIN`/`MAX(received_at)`, and
/// `received_at` is the column the pipeline stamps. `sessions` and `workflows`
/// have no `received_at` at all, and a release's `first_seen_at` derived from
/// two different clocks would jump backwards the first time it was re-sighted
/// live — see the `backfill_all` doc.
const EVENT_TABLES: [&str; 2] = ["error_events", "analytics_events"];

/// Every TELEMETRY table that stores a `release` value, i.e. everything the
/// historical repair has to visit — a superset of [`EVENT_TABLES`].
///
/// The three extras are the other telemetry tables with `release ->
/// Nullable<Text>` in `schema.rs` (`sessions`, `transactions`, `workflows`),
/// and they are not cosmetic:
/// Sessions and Transactions are both release-FILTERED in the query layer, and
/// that filter lowers to a plain column equality, so a stored `' 3.0.0 '`
/// makes the row answer NO to its own release. `workflows` carries the column
/// without a filter today; it is repaired anyway, because leaving one of the
/// five out is how the column drifts back into two spellings.
///
/// `sessions` and `transactions` are partitioned (by `started_at` and
/// `occurred_at` respectively) so they take the same per-partition path as the
/// event tables; `workflows` is not partitioned and falls back to one
/// statement against the table.
///
/// `symbol_artifacts.release` is the sixth column of that type and is
/// deliberately NOT repaired: artifact uploads have trimmed and blank→NULL'd
/// `release` since the table was created (`routes/artifacts.rs::blank_to_none`,
/// 2026-07-15), so no padded historical value can exist there.
const RELEASE_TABLES: [&str; 5] = [
    "error_events",
    "analytics_events",
    "sessions",
    "transactions",
    "workflows",
];

/// The seed statement for one table, built rather than inlined so a unit test
/// can pin the normalisation into the SQL itself — the projection, the
/// `GROUP BY` and the `WHERE` all have to apply the same rule, and a revert of
/// any one of them to a raw `release` changes no row count and throws no
/// error, it just puts padded duplicates back in the switcher.
fn seed_sql(table: &str) -> String {
    format!(
        "INSERT INTO app_releases (app_id, environment_id, release, first_seen_at, last_seen_at) \
         SELECT app_id, environment_id, btrim(release, {WS}), MIN(received_at), MAX(received_at) \
         FROM {table} WHERE release IS NOT NULL AND btrim(release, {WS}) <> '' \
         GROUP BY app_id, environment_id, btrim(release, {WS}) \
         {ON_CONFLICT_WIDEN}"
    )
}

/// The child partitions of `table`, as names already quoted by `regclass` and
/// safe to interpolate. Empty for a table that is not partitioned (or is
/// partitioned but has no partitions yet) — callers fall back to the table.
async fn child_partitions(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<Vec<String>> {
    #[derive(QueryableByName)]
    struct Child {
        #[diesel(sql_type = Text)]
        child: String,
    }
    let rows: Vec<Child> = diesel::sql_query(
        "SELECT inhrelid::regclass::text AS child FROM pg_inherits \
         WHERE inhparent = $1::regclass ORDER BY 1",
    )
    .bind::<Text, _>(table)
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.child).collect())
}

/// Operator-run seed of `app_releases`, and repair of every stored `release`.
///
/// # Two jobs, in this order
///
/// 1. **Normalise history in place, on all five [`RELEASE_TABLES`]** —
///    `error_events`, `analytics_events`, `sessions`, `transactions` and
///    `workflows`. The ingest edge trims `header.release` and turns a blank
///    one into NULL, and every current SDK refuses a blank release at init —
///    but none of that existed when the rows this command reads were written,
///    so `release` on those tables still holds `''`, `'\t'`, and `' 1.4.0 '`.
///    Left alone, `' 1.4.0 '` is a *different* release from `'1.4.0'` to every
///    filter in the query layer (the `?release=` rewrite lowers to a plain
///    column equality), which is a silent wrong-answer, not a cosmetic one —
///    and it is a wrong answer on Sessions and Transactions exactly as much as
///    on the two event lists, which is why the repair is not scoped to the
///    tables the catalogue is seeded from. So the backfill closes the
///    forward-only gap: blank-ish values become NULL and padded ones are
///    trimmed, on all five tables, before anything is seeded. This is the only
///    part of this command that writes to those tables, and it is scoped
///    by `WHERE` — but `release` is not the partition key, so nothing prunes
///    and both statements are a full scan. They are issued **per child
///    partition**, one statement each, rather than once against the parent:
///    a single parent-level `UPDATE` takes a lock on every partition of the
///    table and holds all of them for the whole run (hours at 158M rows),
///    which blocks `sauron-tier`'s `DETACH PARTITION` for exactly that long
///    and makes the command all-or-nothing. Per partition the locks are
///    per partition, the work is incremental, and an interrupted run has
///    already committed every partition it finished — re-running skips them
///    (the `WHERE` matches nothing the second time). A table with no children
///    — `workflows` — falls back to one statement against the table itself.
///
///    The partition list is **snapshotted per table, immediately before that
///    table is repaired**, so it is a plan, not a live view: a partition that
///    is detached and dropped after its table's snapshot was taken makes the
///    statement naming it fail, and the whole command aborts on that error
///    (re-run to resume — every partition already committed is a no-op the
///    second time). That is the concrete reason **`sauron-tier` must be
///    stopped for the run**, over and above the lock contention. See
///    `docs/approximate-analytics.md`.
/// 2. **Seed the catalogue, from the two event tables only.** `GROUP BY` the
///    *normalised* value, so `' 1.4.0 '`
///    and `'1.4.0'` fold into one switcher entry rather than two, and skip
///    whatever normalises to empty. The projection repeats step 1's rule
///    rather than trusting it: the two SELECTs are separate statements from
///    the two UPDATEs, and a seed leg that silently depended on a preceding
///    statement would be one refactor away from re-admitting blank labels.
///
/// `first_seen_at`/`last_seen_at` are `MIN`/`MAX` of **`received_at`**, not
/// `occurred_at`: the pipeline stamps `received_at` (`IngestJob::received_at`,
/// via `note_release`), and a catalogue whose two seeding paths used different
/// clocks would make `first_seen_at` jump backwards the first time a
/// backfilled release was re-sighted live. `received_at` is also the server's
/// own clock, so a device with a wrong clock cannot date a release to 2019.
///
/// Idempotent: re-running only widens, and step 1 is a no-op the second time.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<BackfillOutcome> {
    let mut conn = crate::conn(pool).await?;
    let mut out = BackfillOutcome::default();

    // --- job 1: repair the stored values on EVERY release-bearing table, ONE
    // PARTITION AT A TIME.
    //
    // `RELEASE_TABLES`, not `EVENT_TABLES`: `sessions`, `transactions` and
    // `workflows` store `release` too, and the first two are release-filtered.
    // Each statement locks only the partition it names, commits on its own,
    // and leaves nothing for a re-run to redo. Against the parent instead,
    // every partition of the table would be locked for the duration of the
    // whole command — see the fn doc.
    for table in RELEASE_TABLES {
        // Snapshotted here, per table, and then worked through: if the tier
        // worker drops one of these partitions while this table is being
        // repaired, the statement naming it errors and the command aborts.
        // Re-run to resume. See the fn doc, and stop `sauron-tier` first.
        let children = child_partitions(&mut conn, table).await?;
        let targets: Vec<String> = if children.is_empty() {
            vec![table.to_string()]
        } else {
            children
        };
        for target in targets {
            // Blank-ish first: after this, the trim below cannot produce `''`.
            let blanked = diesel::sql_query(format!(
                "UPDATE {target} SET release = NULL \
                 WHERE release IS NOT NULL AND btrim(release, {WS}) = ''"
            ))
            .execute(&mut conn)
            .await?;
            let trimmed = diesel::sql_query(format!(
                "UPDATE {target} SET release = btrim(release, {WS}) \
                 WHERE release IS NOT NULL AND release <> btrim(release, {WS})"
            ))
            .execute(&mut conn)
            .await?;
            out.normalised_event_rows += (blanked + trimmed) as u64;
        }
    }

    // --- job 2: seed. One statement per table: this leg only READS the event
    // tables, so it takes no lock worth splitting up, and a per-partition
    // `GROUP BY` would upsert the same key once per partition instead of once.
    for table in EVENT_TABLES {
        let n = diesel::sql_query(seed_sql(table))
            .execute(&mut conn)
            .await?;
        out.upserted += n as u64;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repair covers everything the seed reads, and more.
    ///
    /// The two lists are deliberately different — the seed needs
    /// `received_at`, which only the event tables have — and that difference
    /// is exactly what makes an accidental swap invisible: running the repair
    /// over `EVENT_TABLES` throws no error and changes no count an operator
    /// sees, it just leaves `sessions`/`transactions`/`workflows` holding
    /// padded values that their own release filter then cannot find.
    #[test]
    fn every_seeded_table_is_also_repaired() {
        for table in EVENT_TABLES {
            assert!(
                RELEASE_TABLES.contains(&table),
                "{table} is seeded but never repaired"
            );
        }
        assert!(
            RELEASE_TABLES.len() > EVENT_TABLES.len(),
            "the repair must reach the non-event tables that store `release` too"
        );
    }

    /// The seed leg must normalise in BOTH halves of the statement.
    ///
    /// `GROUP BY app_id, environment_id, btrim(release, …)` with a bare
    /// `release` in the projection is not a compile error and not a runtime
    /// error — Postgres accepts `release` as functionally dependent on nothing
    /// here only if it is in the GROUP BY, so the two are easy to "simplify"
    /// back to the raw column together, and the result is a catalogue where
    /// `' 1.4.0 '` and `'1.4.0'` are two switcher entries again. The other way
    /// round (normalised projection, raw GROUP BY) silently splits the groups
    /// and then upserts the same normalised value twice, which the ON CONFLICT
    /// hides. Neither shape is visible in the row counts, so pin the SQL.
    #[test]
    fn the_seed_leg_normalises_in_the_projection_and_the_group_by() {
        for table in EVENT_TABLES {
            let sql = seed_sql(table);
            let (head, group_by) = sql
                .split_once(" GROUP BY ")
                .unwrap_or_else(|| panic!("seed SQL for {table} has no GROUP BY: {sql}"));
            let select_list = head
                .split_once(" SELECT ")
                .unwrap_or_else(|| panic!("seed SQL for {table} has no SELECT: {sql}"))
                .1;
            // Split on " FROM " so each assertion below covers ONE clause.
            // Without this the "projection" slice ran to the end of `head` —
            // i.e. through the `WHERE`, which carries a `btrim(release, …)` of
            // its own — so reverting only the select list to a raw `release`
            // still satisfied it.
            let (projection, from_where) = select_list
                .split_once(" FROM ")
                .unwrap_or_else(|| panic!("seed SQL for {table} has no FROM: {sql}"));

            assert!(
                projection.contains("btrim(release, "),
                "the {table} seed projects the raw `release`, so a padded value \
                 becomes its own switcher entry: {projection}"
            );
            assert!(
                group_by.contains("btrim(release, "),
                "the {table} seed groups by the raw `release`, so `' 1.4.0 '` \
                 and `'1.4.0'` are two groups: {group_by}"
            );
            // The `WHERE` half of the same rule: a blank-ish value must not be
            // seeded as a release at all.
            assert!(
                from_where.contains("btrim(release, ") && from_where.contains("<> ''"),
                "the {table} seed does not exclude blank-ish releases: {from_where}"
            );
        }
    }
}
