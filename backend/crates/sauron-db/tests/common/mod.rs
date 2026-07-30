//! Integration-test harness. The first in this repository — every other backend
//! test is an in-file unit test over pure functions.
//!
//! Skips rather than fails when `TEST_DATABASE_URL` is unset, so `cargo test
//! --workspace` stays green on a machine with no database (which is what CI is
//! today). A developer opts in by exporting the variable.
//!
//! `TEST_DATABASE_URL` names a *maintenance* connection (e.g. `.../sauron`) —
//! any existing database on the target server, used only to run `CREATE
//! DATABASE` / `DROP DATABASE`. Every [`TestDb::setup`] call provisions its own
//! throwaway database with a random suffix, migrates it from scratch, and drops
//! it again in [`TestDb::cleanup`]. It is never the shared `sauron` database:
//! that one holds ~210k real events that later tasks verify against, and a
//! fresh, empty, exactly-known database is what lets assertions be `== 3`
//! rather than `>= 3` — which is the only kind of assertion that catches an
//! over-broad filter.
//!
//! `mod common;` is compiled once per integration-test binary that declares
//! it (`env_scoping.rs`, and — since Task 3 of workflow grouping —
//! `workflows.rs` too), each as its own separate crate with its own
//! independent dead-code analysis. A helper with a real caller in one binary
//! but not the other looks unused from that *other* binary's point of view,
//! since Rust's per-crate dead-code check cannot see across that boundary.
//! Allowed at the module level, rather than chasing an ever-growing pile of
//! per-item allows as more binaries share this harness (a few of which
//! already existed here before Task 3, for the unrelated single-binary
//! reason `SeedIds`'s own doc comment explains).
#![allow(dead_code)]

use std::cell::Cell;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde_json::json;
use uuid::Uuid;

use sauron_db::models::{
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewIssue, NewTransaction,
};
use sauron_db::repo;

/// Define an environment on `project_id` and enroll `app_id` in it.
///
/// Returns the **enrollment** id, which is what event rows store in
/// `environment_id` and what `role_grants.scope_id` holds for `scope_type =
/// 'env'`. The catalogue id is deliberately not returned: no test asserts
/// against it, and returning the wrong one of the two would produce assertions
/// that pass for the wrong reason.
pub async fn seed_env(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    app_id: Uuid,
    name: &str,
    public_key: &str,
    is_default: bool,
) -> Uuid {
    let env = repo::create_project_environment(conn, project_id, name)
        .await
        .unwrap_or_else(|e| panic!("create catalogue env {name}: {e}"));
    repo::create_app_environments(
        conn,
        &[NewAppEnvironment {
            app_id,
            environment_id: env.id,
            public_key,
            is_default,
        }],
    )
    .await
    .unwrap_or_else(|e| panic!("enroll app in {name}: {e}"))
    .remove(0)
    .id
}

/// One throwaway database per [`TestDb`], created in [`TestDb::setup`] and
/// dropped in [`TestDb::cleanup`].
pub struct TestDb {
    pool: sauron_db::PgPool,
    admin_url: String,
    db_name: String,
    /// Set by `seed_two_envs` so `cleanup` can find the org without the caller
    /// having to thread the id back in.
    org_id: OnceLock<Uuid>,
    /// Tracked so `Drop` can tell whether `cleanup()` ever ran. A plain `Cell`
    /// (not `AtomicBool`) deliberately: with `diesel_async::RunQueryDsl` in
    /// scope, `AtomicBool::load`'s inherent method loses method-resolution
    /// priority to `RunQueryDsl`'s blanket-impl `load` (it matches the
    /// unreffed receiver first), so `.load(Ordering::SeqCst)` silently resolves
    /// to the wrong trait entirely. `Cell::get`/`set` share no names with it.
    cleaned_up: Cell<bool>,
}

/// Two names in `seed_two_envs`'s vocabulary. `home` appears in both `env_a`
/// and `env_b` (a wrong environment filter would then show up as a wrong
/// *count*, not a missing row — the stronger signal); `checkout` also appears
/// in both, for the same reason.
const SCREEN_HOME: &str = "home";
const SCREEN_CHECKOUT: &str = "checkout";

/// The two-step sequence seeded once per environment (see `seed_two_envs`),
/// used to make a ≥2-step funnel and `journey_graph`'s `links` half
/// observable. Identical names in both environments so a single funnel spec
/// can be asserted under `One(env_a)`, `One(env_b)` and `All` — matching the
/// "All equals the sum of the parts" shape every other seeded quantity here
/// has.
const FUNNEL_STEP_1: &str = "harness.funnel.step1";
const FUNNEL_STEP_2: &str = "harness.funnel.step2";

/// The ids [`TestDb::seed_two_envs`] created, for the caller to query and
/// assert against.
///
/// ## Per-table row counts
///
/// | table              | env_a | env_b | unattributed | total |
/// |---------------------|:-----:|:-----:|:-------------:|:-----:|
/// | `analytics_events`  |   5   |   5   |       1       |   11  |
/// | `error_events`      |   4   |   2   |       1       |   7   |
/// | `sessions`          |   3   |   3   |       1       |   7   |
/// | `transactions`      |   5   |   2   |       1       |   8   |
///
/// `analytics_events` grew from 3/2/1 (11 total, up from 6) to close two
/// residual gaps a later re-review found — see the two paragraphs below for
/// what the extra rows are. Neither addition mints an 8th `event_users`/
/// `devices` identity: both reuse distinct_ids already registered by the
/// original seed.
///
/// `event_users`/`devices` themselves later grew from 7 to **8** rows — see
/// `session_only_distinct_id`/`session_only_device_key`'s own doc comments
/// below. Added by a Task 8 review round to close a real gap: no seeded
/// identity qualified for any environment *solely* via `sessions` (the
/// `sessions` seeding below mints three other session-only distinct_ids,
/// `-a-se-1`'s siblings `-a-se-2`/`-b-se-2`, none of which are ever passed to
/// `note_identity`, so none of them has an `event_users`/`devices` row
/// either — only `session_only_distinct_id`, added alongside them, is), so
/// the sessions leg of the person/device membership `EXISTS` had no seeded
/// case that actually required it. Deleting that leg and rerunning the full
/// suite caught this immediately (see `.superpowers/sdd/s2-task-8-report.md`'s
/// "Review findings applied" section for the proof) — every place in this
/// file and in `env_scoping.rs` that counted `event_users`/`devices` rows as
/// **7** now reads **8**.
///
/// Deliberately four *different* tuples, not four copies of the same one:
/// `overview_totals` computes all four counts in one statement from four
/// different sub-selects, one fragment each — with identical tuples, swapping
/// which sub-select gets which table's environment fragment would produce
/// identical output and the test would not catch it.
///
/// `error_events` is split across **two** issues, not one:
/// - `issue_id` spans all three buckets — 4 of `env_a`'s, 1 of `env_b`'s 2,
///   and the 1 unattributed row, 6 total, matching its stored `times_seen`.
/// - `issue_env_b_only` is the *other* row in `env_b` — 1 total, confined to
///   `env_b` alone (never `env_a`, never unattributed). This is what makes a
///   `LEFT JOIN LATERAL` (which drops no rows) distinguishable from an inner
///   join / `EXISTS` (which enforces membership): under `One(env_a)` this
///   issue must not appear at all.
///
/// `shared_distinct_id` / `shared_device_key` appear in `error_events` and
/// `sessions` in **both** `env_a` and `env_b` — the identity the person/device
/// membership filter (`EXISTS` over event/session tables, since neither
/// `event_users` nor `devices` itself carries an `environment_id`) exists to
/// handle correctly. `distinct_id_env_b_only` / `device_key_env_b_only` are
/// confined to `env_b` alone, across `analytics_events`, `error_events` and
/// one `sessions` row, and double as `env_b`'s two-step funnel identity.
/// `shared_distinct_id` doubles as `env_a`'s.
///
/// A third funnel identity (`distinct_id_cross_env`, local to `seed_two_envs`
/// — reusing one of `env_a`'s existing error-only distinct_ids rather than
/// minting a fresh person) fires
/// `FUNNEL_STEP_1` in `env_a` and `FUNNEL_STEP_2` in `env_b` — never both
/// steps in the same environment. This is what makes "scoped s0, forgot s1"
/// distinguishable from correct: with only `shared_distinct_id` (completes
/// both steps inside `env_a`) and `distinct_id_env_b_only` (completes both
/// steps inside `env_b`), an `s1` that forgets its own environment filter is
/// byte-identical to a correctly-scoped one. Add this cross-environment
/// identity and, under `One(env_a)`, a correctly-scoped funnel finds it a
/// step-0 candidate (its step1 *is* in `env_a`) but fails it at step 1 (its
/// step2 is not); a funnel that scopes `s0` but leaves `s1`'s environment
/// check off finds the step2 in `env_b` anyway and passes it — the two
/// implementations now disagree on `step1`'s count (1 vs. 2), not just on
/// this identity's fate.
///
/// Every `analytics_events` row now carries a real `screen`, and a few also
/// carry a real `session_id` pointing at one of the seeded `sessions` rows
/// (previously every row had `session_id: None`, so `screen_stats`/
/// `screen_list`'s dwell CTE — which requires `session_id IS NOT NULL` — was
/// always empty). One row in `env_a` is named `'$screen'` and paired, same
/// session, with the pre-existing baseline row 60 seconds later; two rows in
/// `env_b` are named `'$screen'`, one paired with `env_b`'s own funnel-step-1
/// row 90 seconds later, one alone in its own session (a view with no dwell
/// partner). That gives `views`/`total_dwell_ms`/`avg_dwell_ms` non-zero
/// values that also *differ* between `env_a` (views=1, dwell=60000ms) and
/// `env_b` (views=2, dwell=90000ms) — needed to catch a `dw` CTE that omits
/// its environment fragment entirely, which would otherwise leak dwell time
/// across environments while every other screen column stayed correct.
///
/// **F4 (final whole-branch review, pre-Slice-3 fix round) touched this seed
/// again, twice, in ways that change no row count in the table above but do
/// change specific timestamps/associations** — read this before trusting any
/// inline snippet elsewhere that shows the old shape:
/// 1. `shared_distinct_id`'s `env_b` error event and `session_b0` (the
///    session backing `shared_distinct_id`/`shared_device_key` in `env_b`)
///    both moved from the literal `now` to `now - 45s`. Before this, every
///    one of `shared_distinct_id`'s `env_a` *and* `env_b` signals that could
///    dominate a `max(occurred_at)` landed on exactly `now`, so the new
///    per-environment `last_seen` `list_persons`/`list_devices` derive (see
///    their own doc comments) was identical under `One(env_a)` and
///    `One(env_b)` — a real gap in what the seed could discriminate, not a
///    property of the fix.
/// 2. `issue_env_b_only`'s one error event (`distinct_id_env_b_only`) had its
///    `device_key` repointed from its own `device_key_env_b_only` to
///    `shared_device_key`, and its timestamp set to `now - 10s` (the most
///    recent `env_b` signal on that device). Before this, no device in the
///    seed was ever touched by two *different* distinct_ids, so there was no
///    seeded case where `devices.last_distinct_id` (the app-wide, disclosure-
///    prone column) and the new per-environment derivation could disagree —
///    the exact defect F4 exists to fix. This does not add or remove a row
///    (every count in the table above is unchanged) and does not affect
///    `distinct_id_env_b_only`'s own per-identity counts (`list_persons`
///    groups by `distinct_id`, not `device_key`) — only the small number of
///    *device*-level assertions this touches, called out individually where
///    they live in `env_scoping.rs`.
///
/// See `pinned_now` below and `env_scoping.rs`'s `person_and_device_seen_and_
/// identity_are_derived_per_environment` for what these two changes make
/// possible to assert. If this seed changes shape again, re-derive the
/// exact offsets from `seed_two_envs`'s own timestamps rather than trusting
/// the numbers named here or in that test.
///
/// **Task 8 moved item 1's offset a second time**, from `now - 45s` to
/// `now - 30s` (on the error event only — `session_b0` stays at `now - 45s`),
/// and gave that row an explicit `title`/`culprit` — see
/// [`issue_shared`](Self::issue_shared)'s own doc comment for why. Consequence:
/// `shared_distinct_id`'s single most recent `env_b` signal is now that error
/// event alone (previously a tie with `session_b0`), so `shared_b.last_seen`
/// / `user_b.last_seen` moved from `now - 45s` to `now - 30s` in
/// `env_scoping.rs`'s `person_and_device_seen_and_identity_are_derived_per_
/// environment` and `get_event_user_seen_is_derived_per_environment_not_app_
/// wide` — both updated there. Nothing else in item 1/2 above changed:
/// `session_b0` itself, `device_b`'s `now - 10s`, and every row count are
/// exactly as this comment already described.
// Several identity-string fields below (`shared_device_key`,
// `distinct_id_env_b_only`, `device_key_env_b_only`) exist for tests that
// address them by name rather than for this file's own use, and were verified
// live against a real database while writing this seed (see
// `.superpowers/sdd/s2-task-2-report.md`'s "Seed extension" and "Residual gap
// closure" sections for the proof and its real output) rather than kept as a
// permanent throwaway test, so they don't bit-rot unnoticed if the seed
// changes shape again.
//
// They used to carry individual `#[allow(dead_code)]` attributes, deliberately
// per-field rather than on the struct so a genuine dead-code warning on any
// OTHER field would still surface. That distinction stopped being available
// once this module gained a second consumer: `mod common;` is compiled
// separately into each integration-test binary that declares it, and the
// module-level `#![allow(dead_code)]` at the top of this file (see its own
// comment) now covers every item here regardless. The per-field attributes
// were removed as dead weight rather than left to imply a granularity that no
// longer exists.
pub struct SeedIds {
    pub app_id: Uuid,
    /// The project the app belongs to. Environments are defined here, so any
    /// test that adds one to the seeded fixture needs it.
    pub project_id: Uuid,
    /// The org `seed_two_envs` creates (previously tracked only internally, on
    /// `TestDb.org_id`, for `cleanup()`'s own use) — surfaced for Slice 3's
    /// `role_grants` tests, which need a real `org_id` to insert a grant under.
    pub org_id: Uuid,
    /// Email of a real `users` row `seed_two_envs` creates alongside the org
    /// (added for the same reason as `org_id`, above: Slice 3's `role_grants`
    /// tests need a real user to grant a role to; `seed_two_envs` did not
    /// create any user before this). Not otherwise a member of the org — no
    /// `role_grants` row is seeded for it, so a test that needs one inserts
    /// its own, as `env_scoping.rs`'s `role_grants_accepts_the_env_scope_type`
    /// does.
    pub owner_email: String,
    pub env_a: Uuid,
    pub env_b: Uuid,
    /// Spans all three buckets — 6 total error events, matching `times_seen`.
    pub issue_id: Uuid,
    /// Confined to `env_b` alone — 1 error event, `times_seen == 1`.
    pub issue_env_b_only: Uuid,
    /// Task 8: the same underlying issue as [`issue_id`](Self::issue_id),
    /// exposed under this second name because it is what Task 9's tests
    /// address it by. Two of `issue_id`'s six `error_events` rows carry
    /// explicit, deliberately different `title`/`culprit` per environment —
    /// the shape Task 9's per-environment derivation reads:
    ///   - env_a, `occurred_at = pinned_now + 5s`: `title = "TypeError:
    ///     staging cart is empty"`, `culprit = "checkout (staging/cart.ts)"`
    ///     (the `a-er-1` row). **Task 9 moved this offset** from the
    ///     originally-planned `pinned_now - 240s` — see the inline comment
    ///     at `a-er-1`'s `seed_error_event` call for why: three of env_a's
    ///     other rows for this issue tie at the literal `pinned_now`, so a
    ///     `- 240s` offset was NOT actually env_a's newest occurrence, and
    ///     the per-environment `title`/`culprit` derivation (newest row by
    ///     `occurred_at`, no other tiebreaker) silently picked one of those
    ///     title-less rows instead and fell back to the app-wide string.
    ///     `+ 5s` makes `a-er-1` the unambiguous newest env_a row for this
    ///     issue without moving any other row.
    ///   - env_b, `occurred_at = pinned_now - 30s`: `title = "TypeError:
    ///     prod cart is empty"`, `culprit = "checkout (prod/cart.ts)"` (the
    ///     `shared_distinct_id` row, same one F4 pinned for the
    ///     person/device `last_seen` tests below). This is env_b's ONLY
    ///     `error_events` row for this issue, so it is unambiguously env_b's
    ///     newest regardless of the env_a offset above.
    ///
    /// `issues.title`/`culprit` for this issue (under `EnvFilter::All`,
    /// which — unlike `One`/`Unattributed` — reads the stored `issues` row
    /// rather than deriving from `error_events`) are set directly by the one
    /// `upsert_issue` call for this issue, independent of either
    /// `error_events` row's timestamp: the **env_b** strings, `"TypeError:
    /// prod cart is empty"` / `"checkout (prod/cart.ts)"`. That is the fact
    /// Task 9's `All`-scope assertions turn on. The other four `error_events`
    /// rows for this issue (and every other seeded error event) keep
    /// `title`/`culprit = None`, on purpose — that is the pre-migration-30 /
    /// not-yet-scoped row shape Task 9's `COALESCE`-to-`issues` fallback has
    /// to handle too.
    pub issue_shared: Uuid,
    /// Present in `error_events` and `sessions` in both `env_a` and `env_b`.
    pub shared_distinct_id: String,
    pub shared_device_key: String,
    /// Confined to `env_b` alone; also `env_b`'s two-step funnel identity.
    pub distinct_id_env_b_only: String,
    pub device_key_env_b_only: String,
    /// Registered in `event_users`/`devices` (via `note_identity`) with **no**
    /// `analytics_events` or `error_events` row anywhere, in either
    /// environment — its only presence in any signal table is one `sessions`
    /// row in `env_a` (`seed_session`'s `-a-1` session, reusing the identity
    /// previously named inline as `-a-se-1`). Added specifically to exercise
    /// the sessions leg of the person/device membership `EXISTS` in
    /// `list_persons`/`list_devices`/`get_event_user`/`get_device`: without
    /// that leg, this identity has no analytics/error activity in `env_a` to
    /// qualify on, so it would silently vanish from every `One(env_a)` read
    /// instead of appearing with `events_count: 0, errors_count: 0,
    /// sessions_count: 1`. Must never appear under `One(env_b)` or
    /// `Unattributed` — it has no row in either.
    pub session_only_distinct_id: String,
    pub session_only_device_key: String,
    /// The pinned noon-UTC anchor `seed_two_envs` computes internally (see its
    /// own `now` local and the doc comment there on why it's pinned rather
    /// than volatile `Utc::now()`) — exposed so a test can assert an *exact*
    /// derived `first_seen`/`last_seen`/timestamp value (`pinned_now -
    /// Duration::seconds(n)`) instead of only a relative ordering. Added for
    /// F4's per-environment `first_seen`/`last_seen` tests, which need to
    /// name precise expected values, not just "earlier than" / "not equal to".
    pub pinned_now: DateTime<Utc>,
}

/// The ids [`TestDb::seed_cross_env_session`] created.
///
/// Deliberately **separate** from [`SeedIds`]/[`TestDb::seed_two_envs`] — that
/// fixture is depended on by every other test in this file, and this shape is
/// a single, deliberately pathological session, not a general-purpose
/// two-environment seed. It exists to make one specific bug externally
/// observable: `bump_session`'s `ON CONFLICT` sets `environment_id =
/// COALESCE(EXCLUDED.environment_id, sessions.environment_id)` (the most
/// recent non-null value wins) while `errors_count` accumulates across every
/// call regardless of which environment it carried. So a session can end up
/// *labelled* one environment while its accumulated `errors_count` was
/// incremented by a signal from a *different* one — see `bump_session`'s own
/// doc comment, and Task 10's report
/// (`.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`)
/// for the read-side consequence and the measurement that decided whether to
/// fix it.
pub struct CrossEnvSessionIds {
    pub app_id: Uuid,
    pub env_a: Uuid,
    pub env_b: Uuid,
    /// The one pathological session: `sessions.environment_id` ends up
    /// `env_a` (the last `bump_session` call to touch it carried `env_a`),
    /// but its only `error_events` row is stamped `env_b`.
    pub session_id: String,
    pub pinned_now: DateTime<Utc>,
}

/// A fresh, safe database name for one test run. Lowercase hex only, so it
/// passes `sauron_db`'s identifier validation. Random suffix so two concurrent
/// `cargo test` runs — or two test functions racing within one binary — can
/// never collide.
///
/// Embeds the creation time (Unix seconds) in the name itself, not just a
/// random suffix: `pg_database` carries no creation timestamp, so encoding it
/// here is what lets [`reap_stale_test_databases`] tell an abandoned database
/// (a test that panicked before `cleanup()`, see `impl Drop` below) from one a
/// concurrent run is still using, without needing any out-of-band bookkeeping.
fn ephemeral_db_name() -> String {
    format!(
        "sauron_test_{}_{}",
        Utc::now().timestamp(),
        Uuid::new_v4().simple()
    )
}

/// Age past which a leftover `sauron_test_%` database is treated as abandoned
/// rather than concurrently in use.
const STALE_DB_MAX_AGE_SECS: i64 = 3 * 3600;

/// Best-effort reap of `sauron_test_%` databases left behind by a prior
/// panic. `Drop for TestDb` cannot await `drop_database` (see below), so a
/// test that panics between seeding and `cleanup()` leaks its database — with
/// ~20 tests across six implementers doing red/green cycles, that accumulates
/// fast without this. Errors here are logged, never propagated: a reaper that
/// fails the *current* test's `setup()` over a leftover from an unrelated test
/// would be worse than the leak it is trying to clean up.
async fn reap_stale_test_databases(admin_url: &str) {
    #[derive(QueryableByName)]
    struct DbName {
        #[diesel(sql_type = Text)]
        datname: String,
    }

    let mut conn = match AsyncPgConnection::establish(admin_url).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stale-db reaper: could not connect to maintenance db: {e}");
            return;
        }
    };

    let rows: Vec<DbName> = match diesel::sql_query(
        "SELECT datname FROM pg_database WHERE datname LIKE 'sauron_test_%'",
    )
    .get_results(&mut conn)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("stale-db reaper: listing sauron_test_% databases failed: {e}");
            return;
        }
    };

    let now = Utc::now().timestamp();
    for row in rows {
        // Name shape: sauron_test_<unix-seconds>_<uuid-simple>. A name that
        // doesn't parse (hand-created, or from before this scheme existed) is
        // left alone rather than guessed at.
        let Some(created_at) = row
            .datname
            .strip_prefix("sauron_test_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|ts| ts.parse::<i64>().ok())
        else {
            continue;
        };
        if now - created_at > STALE_DB_MAX_AGE_SECS {
            match sauron_db::drop_database(admin_url, &row.datname).await {
                Ok(()) => eprintln!(
                    "stale-db reaper: dropped abandoned database {}",
                    row.datname
                ),
                Err(e) => eprintln!("stale-db reaper: dropping {} failed: {e}", row.datname),
            }
        }
    }
}

/// Return `url` with its database (path) segment replaced by `new_db`,
/// preserving scheme, authority (`user:pass@host:port`), and any `?query`.
///
/// This mirrors `crebain`'s `db_url::swap_database` byte-for-byte, but is not
/// imported from it: that helper lives in a *binary* crate (`crebain`), which
/// `sauron-db`'s tests cannot depend on, so it is kept here too — deliberately
/// tiny and dependency-free rather than pulling in the `url` crate for one
/// string rewrite.
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

impl TestDb {
    /// `None` when `TEST_DATABASE_URL` is unset — callers skip.
    ///
    /// `TEST_DATABASE_URL` is a *maintenance* URL (any existing database on the
    /// target server, e.g. `.../sauron`) from which a brand-new, randomly named
    /// database is created and migrated. `sauron_db`'s real pool constructor is
    /// `build_pool(url, max_size) -> anyhow::Result<PgPool>` (synchronous — it
    /// does not connect eagerly), not the `async fn pool(url)` the original
    /// brief sketch guessed; matched to the real signature here.
    pub async fn setup() -> Option<TestDb> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        reap_stale_test_databases(&admin_url).await;
        let db_name = ephemeral_db_name();
        sauron_db::create_database(&admin_url, &db_name)
            .await
            .expect("create ephemeral test database");
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::run_pending_migrations(&db_url)
            .await
            .expect("run migrations on ephemeral test database");
        // Two, not one. `cleanup()` checks out its own connection, and a test's `conn`
        // local is not dropped until the end of its block — i.e. after `cleanup()` has
        // already been awaited. With a single slot that deadlocks for the full 5s pool
        // timeout and then panics with a pool error that looks nothing like the real
        // cause, leaking the ephemeral database on the way out. Two slots make the
        // whole failure mode unreachable rather than merely documented.
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");
        Some(TestDb {
            pool,
            admin_url,
            db_name,
            org_id: OnceLock::new(),
            cleaned_up: Cell::new(false),
        })
    }

    pub async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    /// Seed one org → project → app → two environments, then insert a known,
    /// deliberately asymmetric and richly-attributed set of rows into all four
    /// signal tables (`analytics_events`, `error_events`, `sessions`,
    /// `transactions`), `issues` (two of them), `event_users` and `devices`.
    ///
    /// See the doc comment on [`SeedIds`] for the exact per-table counts and
    /// what each cross-environment identity is for.
    pub async fn seed_two_envs(&self) -> SeedIds {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "harness org", &format!("harness-org-{suffix}"))
            .await
            .expect("create org");
        let _ = self.org_id.set(org.id);

        // A real `users` row, for tests that need to grant a role to someone
        // real (Slice 3's `role_grants` env-scope tests) rather than a bare
        // random uuid that no foreign key would accept. Not itself granted
        // any role here — that stays the caller's job.
        let owner_email = format!("harness-owner-{suffix}@example.com");
        repo::create_user(&mut conn, &owner_email, "harness-hash", "Harness Owner")
            .await
            .expect("create harness owner user");

        let project = repo::create_project(
            &mut conn,
            org.id,
            "harness project",
            &format!("harness-project-{suffix}"),
        )
        .await
        .expect("create project");

        let app = repo::create_app(
            &mut conn,
            project.id,
            "harness app",
            &format!("harness-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let env_a = seed_env(
            &mut conn,
            project.id,
            app.id,
            "env_a",
            &format!("pk_test_a_{suffix}"),
            true,
        )
        .await;
        let env_b = seed_env(
            &mut conn,
            project.id,
            app.id,
            "env_b",
            &format!("pk_test_b_{suffix}"),
            false,
        )
        .await;

        // -- Identities -------------------------------------------------------
        // See the doc comment on `SeedIds` for what each one is for.
        let shared_distinct_id = format!("harness-user-{suffix}-shared");
        let shared_device_key = format!("harness-device-{suffix}-shared");
        let distinct_id_env_b_only = format!("harness-user-{suffix}-b-only");
        let device_key_env_b_only = format!("harness-device-{suffix}-b-only");
        // The third funnel identity: STEP_1 in env_a, STEP_2 in env_b, never
        // both in the same environment. Local only (not a `SeedIds` field —
        // nothing outside this file needs to name it) and deliberately reused
        // below as one of env_a's error-only identities rather than minted
        // fresh, so it doesn't add an 8th `event_users`/`devices` row.
        let distinct_id_cross_env = format!("harness-user-{suffix}-cross");
        let device_key_cross_env = format!("harness-device-{suffix}-cross");

        // Session ids hoisted here (rather than inlined at each `seed_session`
        // call, as before) because the analytics events below need to name
        // them too, to link `analytics_events.session_id` to a real `sessions`
        // row for `screen_stats`/`screen_list`'s dwell CTE.
        let session_a0 = format!("harness-session-{suffix}-a-0");
        let session_b1 = format!("harness-session-{suffix}-b-1");
        let session_b2 = format!("harness-session-{suffix}-b-2");

        // -- Issues -------------------------------------------------------------
        // `issue_id` spans all three buckets (6 error events total, matching
        // `times_seen`); `issue_env_b_only` is confined to `env_b` alone (1
        // error event). See `SeedIds`'s doc comment for the exact split.
        //
        // Task 8: `title`/`culprit` set here to the env_b string
        // ("TypeError: prod cart is empty" / "checkout (prod/cart.ts)")
        // rather than a generic placeholder — this is the one `upsert_issue`
        // call for this issue (the per-row `seed_error_event` calls below
        // insert directly and never call `upsert_issue` again), so whatever
        // is written here is exactly what production's "last occurrence
        // processed wins" `upsert_issue` semantics would leave on the row if
        // env_b's occurrence were the one actually processed through the
        // real ingest path last. This literal is independent of either
        // `error_events` row's own `occurred_at` (the seed never calls
        // `upsert_issue` per-row the way real ingest does) — including
        // `a-er-1`'s Task-9 retime to `pinned_now + 5s`, below, which is
        // about giving `error_events`' own per-environment derivation an
        // unambiguous newest row within env_a, not about which environment
        // "wins" this already-fixed, hardcoded app-wide column.
        // `times_seen` deliberately stays untouched by this — it is not
        // derived from these two strings, and a second
        // `upsert_issue` call here would have incremented it to 7 and broken
        // every `times_seen == 6` / `issue_id_count.n == 6` assertion in
        // `env_scoping.rs` for no reason, since both title/culprit-bearing
        // rows are two of the six already accounted for below, not new
        // occurrences. No test in `env_scoping.rs` asserted the previous
        // placeholder string ("harness seeded issue"), so this change moved
        // no existing assertion.
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: "harness-fingerprint",
                type_: "Error",
                title: "TypeError: prod cart is empty",
                culprit: "checkout (prod/cart.ts)",
                level: "error",
                first_seen: far_past(),
                last_seen: Utc::now(),
                times_seen: 6,
            },
        )
        .await
        .expect("upsert issue");

        let issue_env_b_only = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: "harness-fingerprint-b-only",
                type_: "Error",
                title: "harness seeded issue (env_b only)",
                culprit: "harness::seed_two_envs",
                level: "error",
                first_seen: far_past(),
                last_seen: Utc::now(),
                times_seen: 1,
            },
        )
        .await
        .expect("upsert issue_env_b_only");

        // Pinned to today's date at a fixed mid-day time-of-day, not the volatile
        // `Utc::now()` wall-clock instant. Every timestamp below is an offset of at most
        // ~500s (`session_b2`'s duration) from `now`, and several functions
        // (`session_duration_series`, `active_user_series`, …) bucket by
        // `date_trunc('day', …)` — if `now` itself lands within ~8m20s of a UTC day
        // boundary, an offset can fall on the *other* side of midnight from `now`,
        // splitting one identity/session across two day-buckets and flipping a
        // per-bucket assertion (bucket count, or a `count(DISTINCT …)` sum that then
        // double-counts an identity straddling the boundary). Anchoring at noon UTC
        // — still "today", so `far_past()` (`Utc::now() - 3650 days`, evaluated
        // separately at assertion time) remains a valid lower bound — leaves over 11
        // hours of margin on both sides, so no seeded row can ever straddle a day
        // boundary regardless of what wall-clock time the suite happens to run at.
        // See `.superpowers/sdd/s2-task-6-report.md`'s "Review findings applied"
        // section for the two tests this was flaking (`session_duration_series_...`
        // and `active_user_series_...`) and the proof this closes the window.
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .expect("12:00:00 is a valid time")
            .and_utc();

        // -- analytics_events: env_a=5, env_b=5, none=1 ------------------------
        //
        // `env_a`'s first three rows are `shared_distinct_id`'s whole history:
        // a baseline event, then the two-step funnel in order. `env_b`'s first
        // two rows are `distinct_id_env_b_only`'s two-step funnel. Both use the
        // same event names (`FUNNEL_STEP_1`/`FUNNEL_STEP_2`) so a single funnel
        // spec is comparable across `One(env_a)` / `One(env_b)` / `All`, and
        // both screens (`home`/`checkout`) appear in both environments. The
        // remaining two rows per environment are the cross-environment funnel
        // identity and the `'$screen'`/dwell rows added below.
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_a),
            "harness.event",
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_HOME,
            now - chrono::Duration::minutes(3),
            // Linked to `session_a0`, shared with the `'$screen'` row below —
            // that pairing is what gives `dw` a non-null `LEAD` gap.
            Some(&session_a0),
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_a),
            FUNNEL_STEP_1,
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_HOME,
            now - chrono::Duration::minutes(2),
            None,
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_a),
            FUNNEL_STEP_2,
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_CHECKOUT,
            now - chrono::Duration::minutes(1),
            None,
        )
        .await;

        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_b),
            FUNNEL_STEP_1,
            &distinct_id_env_b_only,
            &device_key_env_b_only,
            SCREEN_HOME,
            now - chrono::Duration::minutes(2),
            // Linked to `session_b1`, shared with `screen_b1` below.
            Some(&session_b1),
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_b),
            FUNNEL_STEP_2,
            &distinct_id_env_b_only,
            &device_key_env_b_only,
            SCREEN_CHECKOUT,
            now - chrono::Duration::minutes(1),
            None,
        )
        .await;

        seed_analytics_event(
            &mut conn,
            app.id,
            None,
            "harness.event",
            &format!("harness-user-{suffix}-none-an-0"),
            &format!("harness-device-{suffix}-none-an-0"),
            SCREEN_HOME,
            now,
            None,
        )
        .await;

        // -- Cross-environment funnel identity (gap 1) -------------------------
        //
        // `distinct_id_cross_env` does FUNNEL_STEP_1 in env_a and FUNNEL_STEP_2
        // in env_b — never both steps in one environment. See the doc comment
        // on `SeedIds` for why: this is what makes a funnel that scopes `s0`
        // but forgets to scope `s1` disagree with a correctly-scoped one on
        // `step1`'s count (2 vs. 1) under `One(env_a)`, instead of computing
        // byte-identical output.
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_a),
            FUNNEL_STEP_1,
            &distinct_id_cross_env,
            &device_key_cross_env,
            SCREEN_HOME,
            now - chrono::Duration::seconds(150),
            None,
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_b),
            FUNNEL_STEP_2,
            &distinct_id_cross_env,
            &device_key_cross_env,
            SCREEN_CHECKOUT,
            now - chrono::Duration::seconds(80),
            None,
        )
        .await;

        // -- '$screen' / dwell rows (gap 2) -------------------------------------
        //
        // `name='$screen'` is what `ev`'s `views` column (and therefore
        // `avg_dwell_ms`, which divides by it) counts. Pairing each with a
        // same-session row lets `dw`'s `LEAD(occurred_at) - occurred_at`
        // produce a real, non-zero gap instead of always NULL. Durations
        // deliberately differ per environment (60s vs. 90s, and env_b gets a
        // second, unpaired '$screen' row) so `views`/`total_dwell_ms` differ
        // between `env_a` and `env_b` too — the shape needed to catch a `dw`
        // CTE that omits its environment fragment entirely (dwell would then
        // silently pool across environments instead of being merely absent).
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_a),
            "$screen",
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_HOME,
            now - chrono::Duration::minutes(4),
            Some(&session_a0),
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_b),
            "$screen",
            &distinct_id_env_b_only,
            &device_key_env_b_only,
            SCREEN_HOME,
            now - chrono::Duration::seconds(210),
            Some(&session_b1),
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(env_b),
            "$screen",
            &distinct_id_env_b_only,
            &device_key_env_b_only,
            SCREEN_HOME,
            now,
            // Alone in `session_b2` — no other analytics_events row shares it,
            // so `LEAD` is NULL: a view with no dwell partner, on purpose.
            Some(&session_b2),
        )
        .await;

        // -- error_events: env_a=4, env_b=2, none=1 ----------------------------
        //
        // `env_a`'s 4 and `none`'s 1 all belong to `issue_id`; `env_b`'s 2 split
        // one-and-one between `issue_id` (keeping its `times_seen == 6`) and
        // `issue_env_b_only` (confined to `env_b` alone).
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_a),
            issue_id,
            "harness-fingerprint",
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_HOME,
            now,
            None,
            None,
        )
        .await;
        // Task 8: this is `issue_shared`'s env_a occurrence (see `SeedIds`'s
        // doc comment on `issue_shared`) — `a-er-1` is otherwise a
        // single-purpose, error-only identity with no other row anywhere in
        // the seed, so retiming it and giving it a title/culprit touches no
        // other assertion.
        //
        // Task 9 moved the offset a second time, from `now - 240s` to
        // `now + 5s`, after live-database testing caught a real gap: Task
        // 8's own `now - 240s` is NOT env_a's newest occurrence for
        // `issue_id` — three of env_a's other four `error_events` rows for
        // this issue (the plain `shared_distinct_id` row above, the
        // `distinct_id_cross_env` row, and `a-er-3`, below) all land on the
        // literal `now`, strictly *after* `now - 240s`. Task 9's per-
        // environment `title`/`culprit` derivation picks the single newest
        // row by `occurred_at DESC LIMIT 1` with no other tiebreaker, so
        // with the old offset it silently picked one of those three
        // title-less `now` rows instead of `a-er-1`, `COALESCE`d down to
        // the app-wide `issues.title`, and made `One(env_a)` and
        // `One(env_b)` show the byte-identical string — reproduced live:
        // `issue_title_culprit_and_level_are_derived_per_environment`
        // failed with `left: "TypeError: prod cart is empty" right:
        // "TypeError: prod cart is empty"` before this change. Moving
        // `a-er-1` *forward* past `now` (rather than the three untitled
        // rows further back) makes it the unambiguous newest env_a row
        // for this issue while touching only this single, single-purpose
        // identity — the three other rows, and every identity/last_seen
        // assertion that depends on them (`shared_distinct_id`'s own
        // env_a `last_seen` in particular), are untouched. Still
        // deliberately far from `issue_shared`'s env_b occurrence below
        // (`now - 30s`) so which one is newer is unambiguous, and well
        // inside the noon-UTC anchor's day-boundary safety margin (see
        // `now`'s own doc comment above).
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_a),
            issue_id,
            "harness-fingerprint",
            &format!("harness-user-{suffix}-a-er-1"),
            &format!("harness-device-{suffix}-a-er-1"),
            SCREEN_CHECKOUT,
            now + chrono::Duration::seconds(5),
            Some("TypeError: staging cart is empty"),
            Some("checkout (staging/cart.ts)"),
        )
        .await;
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_a),
            issue_id,
            "harness-fingerprint",
            // Same identity as the FUNNEL_STEP_1/STEP_2 analytics rows above
            // (`distinct_id_cross_env`), not a fresh one — `note_identity`'s
            // upsert means it doesn't matter that this call and those two both
            // touch it; together they still add no 8th identity to
            // `event_users`/`devices`.
            &distinct_id_cross_env,
            &device_key_cross_env,
            SCREEN_HOME,
            now,
            None,
            None,
        )
        .await;
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_a),
            issue_id,
            "harness-fingerprint",
            &format!("harness-user-{suffix}-a-er-3"),
            &format!("harness-device-{suffix}-a-er-3"),
            SCREEN_CHECKOUT,
            now,
            None,
            None,
        )
        .await;

        // F4 (final whole-branch review, `.superpowers/sdd/s2-final-review.md`):
        // shifted from a plain `now` to `now - 45s` specifically so `env_a`'s
        // and `env_b`'s derived `last_seen` for `shared_distinct_id`/
        // `shared_device_key` stop tying at exactly `now` — see `session_b0`'s
        // matching shift a few lines below for the other half of this. Before
        // this shift both environments' error event AND session both landed on
        // the literal `now`, so `list_persons`/`list_devices`' new per-environment
        // `first_seen`/`last_seen` derivation had no seeded case where `One(env_a)`
        // and `One(env_b)` produced different `last_seen` values for the same
        // shared identity — a regression that swapped which environment's rows
        // fed the LATERAL would have gone undetected. `first_seen` already
        // discriminated without this (env_a's earliest analytics row predates
        // env_b's earliest session by seed construction), but `last_seen` did not.
        //
        // Task 8 moved the offset again, from `now - 45s` to `now - 30s`, and
        // added a title/culprit: this is also `issue_shared`'s env_b
        // occurrence (see `SeedIds`'s doc comment on `issue_shared`) — the
        // one and only `error_events` row for this issue in env_b, so it is
        // unambiguously env_b's newest regardless of env_a's own offset
        // (`a-er-1`, retimed again by Task 9 — see that row's own comment).
        // `issues.title`/`culprit` themselves come from the single, literal
        // `upsert_issue` call above, independent of any row's `occurred_at`;
        // this row's timestamp only governs the per-environment *derivation*
        // Task 9 reads (`error_events.title`/`culprit`), not the stored
        // app-wide column. `session_b0` below is NOT moved —
        // it stays at `now - 45s` — so this row (not the session) is now
        // `shared_distinct_id`'s single most recent env_b signal, which bumps
        // `shared_b.last_seen`/`user_b.last_seen` in `env_scoping.rs` from
        // `now - 45s` to `now - 30s` (both updated there; see this task's
        // report for the exact before/after).
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_b),
            issue_id,
            "harness-fingerprint",
            &shared_distinct_id,
            &shared_device_key,
            SCREEN_HOME,
            now - chrono::Duration::seconds(30),
            Some("TypeError: prod cart is empty"),
            Some("checkout (prod/cart.ts)"),
        )
        .await;
        // F4: device_key repointed from this row's "own" `device_key_env_b_only`
        // to `shared_device_key`, and its timestamp shifted to `now - 10s` (the
        // most recent env_b signal on that device). Before this repoint, no
        // device in the seed was ever touched by two *different* distinct_ids —
        // `shared_device_key` had exactly one identity (`shared_distinct_id`)
        // across both environments — so there was no seeded case that could
        // discriminate `list_devices`/`get_device`'s new per-environment
        // `last_distinct_id` from `devices.last_distinct_id` (the disclosure
        // vector F4 names): both would read the same value regardless of scope.
        // With this repoint, `shared_device_key` under `One(env_b)` has a real,
        // more-recent, *different* identity (`distinct_id_env_b_only`) than its
        // own `One(env_a)` activity (`shared_distinct_id`) — see
        // `env_scoping.rs`'s `person_and_device_seen_and_identity_are_derived_
        // per_environment` for the assertions this makes possible. Does not add
        // or remove any row (so every row-count in `SeedIds`'s doc comment table
        // is unaffected) and does not touch `distinct_id_env_b_only`'s own
        // per-identity counts (grouped by `distinct_id`, not `device_key`) — it
        // does change which *device* this one error row's count/membership
        // credits, which is why the handful of device-level assertions this
        // touches are called out individually where they live.
        seed_error_event(
            &mut conn,
            app.id,
            Some(env_b),
            issue_env_b_only,
            "harness-fingerprint-b-only",
            &distinct_id_env_b_only,
            &shared_device_key,
            SCREEN_CHECKOUT,
            now - chrono::Duration::seconds(10),
            None,
            None,
        )
        .await;

        seed_error_event(
            &mut conn,
            app.id,
            None,
            issue_id,
            "harness-fingerprint",
            &format!("harness-user-{suffix}-none-er-0"),
            &format!("harness-device-{suffix}-none-er-0"),
            SCREEN_HOME,
            now,
            None,
            None,
        )
        .await;

        // -- sessions: env_a=3, env_b=3, none=1 --------------------------------
        //
        // Durations vary per environment (env_a averages 120s, env_b 400s) so
        // `session_duration_series`'s average is no longer the same constant
        // (0) everywhere. One session per environment carries a non-zero
        // `errors_delta` so `session_stats.crashed` /
        // `overview_totals.crashed_sessions` are non-zero too.
        seed_session(
            &mut conn,
            app.id,
            &session_a0,
            &shared_distinct_id,
            &shared_device_key,
            Some(env_a),
            chrono::Duration::seconds(60),
            now,
            1,
        )
        .await;
        let session_only_distinct_id = format!("harness-user-{suffix}-a-se-1");
        let session_only_device_key = format!("harness-device-{suffix}-a-se-1");
        seed_session(
            &mut conn,
            app.id,
            &format!("harness-session-{suffix}-a-1"),
            &session_only_distinct_id,
            &session_only_device_key,
            Some(env_a),
            chrono::Duration::seconds(120),
            now,
            0,
        )
        .await;
        // Registers this identity in event_users/devices — see `SeedIds`'s doc
        // comment on `session_only_distinct_id` for why: it is the only
        // identity in the whole seed whose sole presence in any environment
        // is a `sessions` row, with zero `analytics_events`/`error_events`
        // rows anywhere. `events_delta=0, errors_delta=0`: unlike
        // `seed_analytics_event`/`seed_error_event`, this call has no
        // matching event of either kind — `process.rs`'s own `rollup()` has
        // no third call shape that bumps `devices` from a session alone, so
        // there is no real events_delta/errors_delta to mirror here; both
        // must stay 0 or `devices.events_count`/`errors_count` would silently
        // claim activity this identity never had.
        note_identity(
            &mut conn,
            app.id,
            &session_only_distinct_id,
            &session_only_device_key,
            now,
            0,
            0,
        )
        .await;
        seed_session(
            &mut conn,
            app.id,
            &format!("harness-session-{suffix}-a-2"),
            &format!("harness-user-{suffix}-a-se-2"),
            &format!("harness-device-{suffix}-a-se-2"),
            Some(env_a),
            chrono::Duration::seconds(180),
            now,
            0,
        )
        .await;

        // F4: `ends_at` shifted to `now - 45s` alongside the env_b error event
        // above — see that call's doc comment. Duration stays 300s (unaffected:
        // `started_at`/`last_event_at` both move by the same 45s, so
        // `session_duration_series`' env_b average, still driven by
        // 300/400/500s, is untouched).
        seed_session(
            &mut conn,
            app.id,
            &format!("harness-session-{suffix}-b-0"),
            &shared_distinct_id,
            &shared_device_key,
            Some(env_b),
            chrono::Duration::seconds(300),
            now - chrono::Duration::seconds(45),
            0,
        )
        .await;
        seed_session(
            &mut conn,
            app.id,
            &session_b1,
            &distinct_id_env_b_only,
            &device_key_env_b_only,
            Some(env_b),
            chrono::Duration::seconds(400),
            now,
            1,
        )
        .await;
        seed_session(
            &mut conn,
            app.id,
            &session_b2,
            &format!("harness-user-{suffix}-b-se-2"),
            &format!("harness-device-{suffix}-b-se-2"),
            Some(env_b),
            chrono::Duration::seconds(500),
            now,
            0,
        )
        .await;

        seed_session(
            &mut conn,
            app.id,
            &format!("harness-session-{suffix}-none-0"),
            &format!("harness-user-{suffix}-none-se-0"),
            &format!("harness-device-{suffix}-none-se-0"),
            None,
            chrono::Duration::seconds(200),
            now,
            0,
        )
        .await;

        // -- transactions: env_a=5, env_b=2, none=1 ----------------------------
        //
        // Durations and statuses vary (previously every transaction had
        // `duration_ms = 1.0, status = None`, so nothing but `count` ever
        // discriminated `performance_summary`'s rows).
        for (duration_ms, status) in [
            (10.0, Some("ok")),
            (20.0, Some("ok")),
            (30.0, Some("error")),
            (40.0, None),
            (50.0, Some("ok")),
        ] {
            seed_transaction(&mut conn, app.id, Some(env_a), duration_ms, status, now).await;
        }
        for (duration_ms, status) in [(100.0, Some("ok")), (200.0, Some("error"))] {
            seed_transaction(&mut conn, app.id, Some(env_b), duration_ms, status, now).await;
        }
        seed_transaction(&mut conn, app.id, None, 5.0, None, now).await;

        SeedIds {
            app_id: app.id,
            project_id: project.id,
            org_id: org.id,
            owner_email,
            env_a,
            env_b,
            issue_id,
            issue_env_b_only,
            issue_shared: issue_id,
            shared_distinct_id,
            shared_device_key,
            distinct_id_env_b_only,
            device_key_env_b_only,
            session_only_distinct_id,
            session_only_device_key,
            pinned_now: now,
        }
    }

    /// Seed one org → project → app → two environments, then a single
    /// pathological `(app_id, session_id)` row — see [`CrossEnvSessionIds`]'s
    /// own doc comment for why this is its own fixture.
    ///
    /// Sequence, mirroring what two real ingested signals for the same
    /// session id, minutes apart, in two different environments, would leave
    /// behind:
    ///
    /// 1. `bump_session(environment_id: Some(env_b), errors_delta: 1)` — the
    ///    INSERT branch (first touch of this session id): `environment_id =
    ///    env_b`, `errors_count = 1`. Immediately followed by the matching
    ///    `error_events` row real ingest would have written alongside it —
    ///    `environment_id: Some(env_b)`, `session_id` pointing at this same
    ///    session — the session's ONLY error occurrence, anywhere.
    /// 2. `bump_session(environment_id: Some(env_a), errors_delta: 0)` — the
    ///    ON CONFLICT branch: `errors_count = 1 + 0 = 1` (unchanged — no new
    ///    error), but `environment_id = COALESCE(Some(env_a),
    ///    sessions.environment_id) = env_a` (changed — a non-null value always
    ///    overwrites). This is the "device repointed from staging to prod
    ///    without a fresh session id" shape `events_for_session`'s doc comment
    ///    names.
    ///
    /// Final state: `sessions.environment_id = env_a`, `sessions.errors_count
    /// = 1`, and the only `error_events` row for this session carries
    /// `environment_id = env_b`. A reader that trusts the session's own label
    /// (`errors_count > 0 AND environment_id = env_a`) counts this session as
    /// crashed in `env_a`, which never saw the error.
    pub async fn seed_cross_env_session(&self) -> CrossEnvSessionIds {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(
            &mut conn,
            "harness cross-env org",
            &format!("harness-cross-org-{suffix}"),
        )
        .await
        .expect("create org");
        let _ = self.org_id.set(org.id);

        let project = repo::create_project(
            &mut conn,
            org.id,
            "harness cross-env project",
            &format!("harness-cross-project-{suffix}"),
        )
        .await
        .expect("create project");

        let app = repo::create_app(
            &mut conn,
            project.id,
            "harness cross-env app",
            &format!("harness-cross-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let env_a = seed_env(
            &mut conn,
            project.id,
            app.id,
            "env_a",
            &format!("pk_test_cross_a_{suffix}"),
            true,
        )
        .await;
        let env_b = seed_env(
            &mut conn,
            project.id,
            app.id,
            "env_b",
            &format!("pk_test_cross_b_{suffix}"),
            false,
        )
        .await;

        // Pinned to today's date at a fixed mid-day time-of-day — see
        // `seed_two_envs`'s own `now` local for why (day-bucket straddling).
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .expect("valid noon time")
            .and_utc();
        let error_at = now - chrono::Duration::seconds(120);

        let session_id = format!("harness-session-{suffix}-cross");
        let distinct_id = format!("harness-user-{suffix}-cross");
        let device_key = format!("harness-device-{suffix}-cross");
        let fingerprint = format!("harness-cross-fingerprint-{suffix}");

        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: &fingerprint,
                type_: "Error",
                title: "TypeError: cross-env session error",
                culprit: "checkout (cross/cart.ts)",
                level: "error",
                first_seen: error_at,
                last_seen: error_at,
                times_seen: 1,
            },
        )
        .await
        .expect("upsert issue");

        // 1. Establish the row under env_b, with its one and only error.
        repo::bump_session(
            &mut conn,
            app.id,
            &session_id,
            Some(&distinct_id),
            Some(&device_key),
            error_at,
            &json!({}),
            None,
            Some(env_b),
            None,
            1,
            1,
        )
        .await
        .expect("bump session (env_b, errors_delta=1)");

        repo::insert_error_event(
            &mut conn,
            NewErrorEvent {
                id: Uuid::new_v4(),
                app_id: app.id,
                environment_id: Some(env_b),
                issue_id,
                fingerprint: fingerprint.clone(),
                level: "error".into(),
                message: "harness cross-env error".into(),
                exception_type: "HarnessError".into(),
                exception_value: "seeded".into(),
                stacktrace: json!([]),
                breadcrumbs: json!([]),
                context: json!({}),
                tags: json!({}),
                release: None,
                distinct_id: Some(distinct_id.clone()),
                event_user: None,
                sdk: None,
                ip_address: None,
                occurred_at: error_at,
                session_id: Some(session_id.clone()),
                device_key: Some(device_key.clone()),
                screen: None,
                workflow_id: None,
                workflow_name: None,
                stacktrace_symbolicated: None,
                symbolication_status: "not_applicable".into(),
                debug_meta: None,
                contexts: json!({}),
                extra: json!({}),
                handled: Some(true),
                title: None,
                culprit: None,
            },
        )
        .await
        .expect("insert error event");

        // 2. A later, ordinary (no new error) signal from the SAME session
        // id, this time attributed to env_a. `errors_delta: 0` leaves
        // `errors_count` at 1 (the env_b error, unchanged); `environment_id`
        // flips to env_a via `bump_session`'s COALESCE.
        repo::bump_session(
            &mut conn,
            app.id,
            &session_id,
            None,
            None,
            now,
            &json!({}),
            None,
            Some(env_a),
            None,
            1,
            0,
        )
        .await
        .expect("bump session (env_a, errors_delta=0)");

        CrossEnvSessionIds {
            app_id: app.id,
            env_a,
            env_b,
            session_id,
            pinned_now: now,
        }
    }

    /// Deletes the seeded org — cascading through project → app → environments
    /// → every signal table via the schema's existing `ON DELETE CASCADE`
    /// chain — then drops the whole ephemeral database.
    ///
    /// Must be awaited explicitly at the end of each test: `Drop` cannot await,
    /// so it cannot run this itself. If a test panics before reaching this
    /// call, `impl Drop for TestDb` below only prints a loud warning rather
    /// than attempting a synchronous workaround — the same tradeoff crebain's
    /// `HarnessGuard` makes for the identical reason (see
    /// `backend/bins/crebain/src/harness.rs`).
    ///
    /// Checks out its own connection from the pool, independent of any `conn`
    /// a caller is still holding — the pool is sized 2 precisely so this can
    /// never deadlock against a test's own connection local, which is not
    /// dropped until the end of its lexical block (i.e. after this call has
    /// already been awaited, if it's the last statement). Calling `drop(conn)`
    /// before `cleanup()` is no longer required, but remains good hygiene: it
    /// frees the slot immediately instead of relying on the second one.
    pub async fn cleanup(&self) {
        if let Some(&org_id) = self.org_id.get() {
            let mut conn = self.conn().await;
            diesel::sql_query("DELETE FROM organizations WHERE id = $1")
                .bind::<diesel::sql_types::Uuid, _>(org_id)
                .execute(&mut conn)
                .await
                .expect("delete seeded org");
        }
        sauron_db::drop_database(&self.admin_url, &self.db_name)
            .await
            .expect("drop ephemeral test database");
        self.cleaned_up.set(true);
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Async work cannot run in `Drop`. If a test panicked (or otherwise
        // returned) before reaching `cleanup()`, make the leak loud rather than
        // attempt a runtime-in-Drop workaround.
        if !self.cleaned_up.get() {
            eprintln!(
                "WARNING: ephemeral test database {} may remain (TestDb::cleanup() was \
                 never reached — the test likely panicked). Drop it manually:\n  \
                 DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

/// Insert one `analytics_events` row and register its identity in
/// `event_users`/`devices` (via [`note_identity`]) — mirroring what
/// `sauron-pipeline`'s `process.rs` does on real ingest, since neither table
/// is populated by `repo::insert_analytics_event` itself.
#[allow(clippy::too_many_arguments)]
async fn seed_analytics_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    name: &str,
    distinct_id: &str,
    device_key: &str,
    screen: &str,
    occurred_at: DateTime<Utc>,
    session_id: Option<&str>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: name.to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: session_id.map(|s| s.to_string()),
            release: None,
            ip_address: None,
            occurred_at,
            device_key: Some(device_key.to_string()),
            screen: Some(screen.to_string()),
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert analytics event");
    // events_delta=1, errors_delta=0: this is an analytics event, not an
    // error — see `note_identity`'s doc comment for why the two callers pass
    // different values here.
    note_identity(conn, app_id, distinct_id, device_key, occurred_at, 1, 0).await;
}

/// Insert one `error_events` row and register its identity, same rationale as
/// [`seed_analytics_event`].
///
/// `title`/`culprit` default to `None` for every call except the two Task 8
/// wires deliberately: `None` exercises the pre-migration-30 / not-yet-scoped
/// row shape Task 9's `COALESCE`-to-`issues` fallback has to handle, so most
/// of the seed is left that way on purpose rather than backfilled to match
/// real ingest.
#[allow(clippy::too_many_arguments)]
async fn seed_error_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    issue_id: Uuid,
    fingerprint: &str,
    distinct_id: &str,
    device_key: &str,
    screen: &str,
    occurred_at: DateTime<Utc>,
    title: Option<&str>,
    culprit: Option<&str>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: "error".into(),
            message: format!("harness error {}", Uuid::new_v4().simple()),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some(distinct_id.to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at,
            session_id: None,
            device_key: Some(device_key.to_string()),
            screen: Some(screen.to_string()),
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(true),
            title: title.map(str::to_string),
            culprit: culprit.map(str::to_string),
        },
    )
    .await
    .expect("insert error event");
    // events_delta=0, errors_delta=1: this row is exactly what
    // `devices.errors_count` (and, for `event_users`-adjacent callers,
    // would-be person-level error counts) must fold in, and must NOT fold
    // into `devices.events_count` — see `note_identity`'s doc comment.
    note_identity(conn, app_id, distinct_id, device_key, occurred_at, 0, 1).await;
}

/// Insert one `transactions` row. Unlike analytics/error events, transactions
/// carry no distinct_id/device_key here — not needed by any of Tasks 7-9.
async fn seed_transaction(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    duration_ms: f64,
    status: Option<&str>,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_transaction(
        conn,
        NewTransaction {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: "harness.transaction".into(),
            op: "test".into(),
            duration_ms,
            status: status.map(|s| s.to_string()),
            http_method: None,
            http_status: None,
            url: None,
            distinct_id: None,
            session_id: None,
            device_key: None,
            workflow_id: None,
            workflow_name: None,
            release: None,
            ip_address: None,
            occurred_at,
        },
    )
    .await
    .expect("insert transaction");
}

/// Upsert one `sessions` row with a real, non-zero duration and an
/// intentional `errors_delta`, via **two** `bump_session` calls exploiting its
/// own upsert logic: the first establishes `started_at`, the second advances
/// `last_event_at` past it. A single `bump_session` call always writes the
/// same timestamp into both columns (`VALUES ($5, $5, ...)`), which is why
/// every session had a duration of exactly 0 before this helper existed.
#[allow(clippy::too_many_arguments)]
async fn seed_session(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    session_id: &str,
    distinct_id: &str,
    device_key: &str,
    env: Option<Uuid>,
    duration: chrono::Duration,
    ends_at: DateTime<Utc>,
    errors_delta: i64,
) {
    let starts_at = ends_at - duration;
    repo::bump_session(
        conn,
        app_id,
        session_id,
        Some(distinct_id),
        Some(device_key),
        starts_at,
        &json!({}),
        None,
        env,
        None,
        1,
        0,
    )
    .await
    .expect("bump session (start)");
    repo::bump_session(
        conn,
        app_id,
        session_id,
        None,
        None,
        ends_at,
        &json!({}),
        None,
        env,
        None,
        0,
        errors_delta,
    )
    .await
    .expect("bump session (end)");
}

/// Register one identity in `event_users` and `devices` — the two tables
/// `repo::upsert_event_user` / `repo::bump_device` maintain, called only from
/// `sauron-pipeline`'s `process.rs` on real ingest and never by the plain
/// `insert_*` repo functions the seed otherwise uses. Without this, both
/// tables stay empty for the seeded app regardless of how many
/// analytics/error events carry a `distinct_id`/`device_key`.
///
/// `events_delta`/`errors_delta` are threaded through to `bump_device` rather
/// than hardcoded, mirroring `process.rs`'s `rollup()` exactly (its own
/// `events_delta`/`errors_delta` parameters, passed `1, 0` from
/// `process_event` and `0, 1` from `process_error`): [`seed_analytics_event`]
/// passes `1, 0`, [`seed_error_event`] passes `0, 1`. Both deltas used to be
/// asymmetric — `errors_delta` threaded through, `events_delta` hardcoded to
/// `1` for every call regardless of which of the two callers it was — so
/// `devices.events_count` silently counted *every* analytics-or-error touch,
/// not just analytics ones (verified live: it summed to 19 against this
/// seed's real analytics total of 11, an 8-row overcount exactly matching
/// this app's `error_events` total minus the one row a seed change briefly
/// left uncounted). Harmless while `list_devices`/`get_device` read this
/// column through nothing but LATERALs (Task 8), which never depended on its
/// stored value being right; live-bugged the instant a Task 8 review round
/// put a direct read of it back for the `All` scope (see `list_devices`'s doc
/// comment) — a device seeded with a session-only identity and zero
/// analytics/error activity would have shown `events_count: 1`, not `0`.
async fn note_identity(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
    device_key: &str,
    at: DateTime<Utc>,
    events_delta: i64,
    errors_delta: i64,
) {
    repo::upsert_event_user(conn, app_id, distinct_id, &json!({}))
        .await
        .expect("upsert event user");
    repo::bump_device(
        conn,
        app_id,
        device_key,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(distinct_id),
        at,
        events_delta,
        errors_delta,
    )
    .await
    .expect("bump device");
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Count rows in `table` for `app_id`, optionally further filtered by
/// `environment_id` (`None` means the `IS NULL` bucket). `table` is always one
/// of the four hardcoded signal-table literals, never external input, so
/// interpolating it into the query string carries no injection risk.
///
/// Lives here rather than in `env_scoping.rs` (where it started): it is the
/// natural "did this table get the right rows" helper, and every later task's
/// test needs exactly this, not a bespoke reimplementation per file.
pub async fn count_in_env(
    conn: &mut sauron_db::PgConn,
    table: &str,
    app_id: Uuid,
    env: Option<Uuid>,
) -> i64 {
    let row: CountRow = match env {
        Some(env_id) => diesel::sql_query(format!(
            "SELECT count(*)::bigint AS n FROM {table} WHERE app_id = $1 AND environment_id = $2"
        ))
        .bind::<SqlUuid, _>(app_id)
        .bind::<SqlUuid, _>(env_id)
        .get_result(conn)
        .await
        .unwrap_or_else(|e| panic!("count query on {table} failed: {e}")),
        None => diesel::sql_query(format!(
            "SELECT count(*)::bigint AS n FROM {table} WHERE app_id = $1 AND environment_id IS NULL"
        ))
        .bind::<SqlUuid, _>(app_id)
        .get_result(conn)
        .await
        .unwrap_or_else(|e| panic!("count query on {table} failed: {e}")),
    };
    row.n
}

/// Count every row in `table` for `app_id`, with no environment filter at
/// all — what [`count_in_env`] cannot express for `event_users`/`devices`,
/// since neither carries an `environment_id` column (see the doc comment on
/// [`SeedIds`]). `table` is always one of this harness's own hardcoded
/// literals, never external input, so interpolating it into the query string
/// carries no injection risk.
pub async fn count_rows(conn: &mut sauron_db::PgConn, table: &str, app_id: Uuid) -> i64 {
    let row: CountRow = diesel::sql_query(format!(
        "SELECT count(*)::bigint AS n FROM {table} WHERE app_id = $1"
    ))
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await
    .unwrap_or_else(|e| panic!("count query on {table} failed: {e}"));
    row.n
}

/// The number of distinct, non-null environments `distinct_id` appears in,
/// across every signal table that carries both `app_id` and `distinct_id`
/// (`analytics_events`, `error_events`, `sessions`). `event_users`/`devices`
/// themselves carry no `environment_id` at all, which is exactly why the
/// person/device membership filter has to reach into these three tables to
/// decide scope — so this is the check that a refactor of `note_identity` (or
/// of the seed itself) hasn't silently collapsed `shared_distinct_id` back
/// down to a single environment.
pub async fn distinct_envs_for_identity(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
) -> i64 {
    let row: CountRow = diesel::sql_query(
        "SELECT count(DISTINCT environment_id)::bigint AS n FROM ( \
           SELECT environment_id FROM analytics_events \
             WHERE app_id = $1 AND distinct_id = $2 AND environment_id IS NOT NULL \
           UNION \
           SELECT environment_id FROM error_events \
             WHERE app_id = $1 AND distinct_id = $2 AND environment_id IS NOT NULL \
           UNION \
           SELECT environment_id FROM sessions \
             WHERE app_id = $1 AND distinct_id = $2 AND environment_id IS NOT NULL \
         ) envs",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .get_result(conn)
    .await
    .unwrap_or_else(|e| panic!("distinct-environment count for identity failed: {e}"));
    row.n
}

/// A `since` bound far enough back that no seeded row is excluded by it. Tests
/// assert on environment scoping, so the time window must never be the reason a
/// row is missing — otherwise a broken env filter and a too-narrow window are
/// indistinguishable from a failing assertion.
pub fn far_past() -> DateTime<Utc> {
    Utc::now() - chrono::Duration::days(3650)
}
