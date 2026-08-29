//! HTTP-level tests for `routes::retention::*` — the grid, lifecycle and churn
//! handlers driven through the real router.
//!
//! Same harness shape as `tests/http_active_users.rs` (spawn the compiled
//! binary against an ephemeral migrated database; duplicated rather than
//! shared, see that file's doc comments). Skips when `TEST_DATABASE_URL` or
//! `TEST_REDIS_URL` is unset — so watch the elapsed time, not the green line.
//!
//! These exist because the original verification of this surface was a manual
//! curl drive: the behaviours held, and then had no regression net. Two of the
//! bugs that drive found live at exactly this layer and CANNOT be seen by the
//! `sauron-db` suite alone:
//!
//! * the `EnvFilter::One` scalar-vs-array bind (a 500 only when
//!   `?environment_id=` is present — the way the dashboard always calls);
//! * the handler envelope contracts (`ready:false` ships NO cohort rows;
//!   unelapsed periods are `null`, never 0; the cell budget caps the PRODUCT).

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{NewAppEnvironment, NewRoleGrant};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-retention-test-secret-00000000000000000";

/// See `tests/http_env_scoping.rs`'s identical helper for the full reasoning.
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

/// See `tests/http_env_scoping.rs`'s identical helper for the full reasoning.
fn free_port() -> u16 {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static ISSUED: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();

    let issued = ISSUED.get_or_init(|| Mutex::new(HashSet::new()));
    for _ in 0..100 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        if issued.lock().expect("port registry").insert(port) {
            return port;
        }
    }
    panic!("no unused ephemeral port after 100 attempts");
}

struct TestServer {
    child: tokio::process::Child,
    base: String,
    client: reqwest::Client,
    admin_url: String,
    db_name: String,
    pool: sauron_db::PgPool,
    cleaned_up: Cell<bool>,
}

impl TestServer {
    async fn start() -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

        // Timestamp segment FIRST — the reaper in sauron-db's test common
        // parses it; see `http_env_scoping.rs` for the leak this prevents.
        let db_name = format!(
            "sauron_test_{}_rt{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::create_test_database(&admin_url, &db_name)
            .await
            .expect("create migrated ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");

        let port = free_port();
        let bin = env!("CARGO_BIN_EXE_sauron-api");
        let mut child = tokio::process::Command::new(bin)
            .env("DATABASE_URL", &db_url)
            .env("REDIS_URL", &redis_url)
            .env("JWT_SECRET", JWT_SECRET)
            .env(
                "NOTIFY_SECRET_KEY",
                "sauron-test-notify-secret-key-0000000000",
            )
            .env("API_PORT", port.to_string())
            .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sauron-api binary");

        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();

        let mut ready = false;
        for _ in 0..100 {
            if let Ok(Some(status)) = child.try_wait() {
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_string(&mut stderr).await;
                }
                panic!("sauron-api exited early with {status}; stderr:\n{stderr}");
            }
            if client
                .get(format!("{base}/health"))
                .timeout(StdDuration::from_millis(200))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(100)).await;
        }
        assert!(ready, "sauron-api never became ready on {base}/health");

        Some(TestServer {
            child,
            base,
            client,
            admin_url,
            db_name,
            pool,
            cleaned_up: Cell::new(false),
        })
    }

    async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    async fn get(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"))
    }

    async fn get_status(&self, path: &str, token: &str) -> u16 {
        self.get(path, token).await.status().as_u16()
    }

    async fn get_json(&self, path: &str, token: &str) -> Value {
        let resp = self.get(path, token).await;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: failed to read body (status {status}): {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("GET {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        })
    }

    async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        sauron_db::drop_database(&self.admin_url, &self.db_name)
            .await
            .expect("drop ephemeral test database");
        self.cleaned_up.set(true);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if !self.cleaned_up.get() {
            eprintln!(
                "WARNING: ephemeral test database {} may remain (TestServer::shutdown() was \
                 never reached). Drop it manually:\n  DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

async fn seed_env(
    conn: &mut sauron_db::PgConn,
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

struct Fixture {
    app_id: Uuid,
    env_id: Uuid,
    /// A second enrolled environment `member_token` can NOT reach.
    env2_id: Uuid,
    owner_token: String,
    /// Holds `event:read` on `env_id` ONLY — resolves to `Subset([env_id])`,
    /// never `All`. The persona the cache-isolation test is about.
    member_token: String,
}

impl TestServer {
    /// One app with one environment and an org-wide `event:read` owner.
    ///
    /// The harness template pins `rollup_epoch` a decade out but leaves
    /// `person_days_epoch` at its migration stamp, so an app created here is
    /// implicitly READY — the same default `tests/person_days_rollup.rs`
    /// documents. Tests that need the not-ready state push the epoch forward
    /// themselves.
    async fn seed_fixture(&self) -> Fixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "rt org", &format!("rt-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "rt project", &format!("rt-p-{s}"))
            .await
            .expect("project");
        let app = repo::create_app(&mut conn, project.id, "R", &format!("rt-a-{s}"), "web")
            .await
            .expect("app");
        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_rt_{s}"),
            true,
        )
        .await;
        let env2_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "staging",
            &format!("pk_rt2_{s}"),
            false,
        )
        .await;

        let owner = repo::create_user(
            &mut conn,
            &format!("rt-owner-{s}@example.test"),
            "x",
            "Owner",
        )
        .await
        .expect("owner");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "rt role",
            "org-wide event read",
            json!([perm::EVENT_READ]),
        )
        .await
        .expect("role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant");

        let member = repo::create_user(
            &mut conn,
            &format!("rt-member-{s}@example.test"),
            "x",
            "Member",
        )
        .await
        .expect("member");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: member.id,
                role_id: role.id,
                scope_type: "env".to_string(),
                scope_id: env_id,
            },
        )
        .await
        .expect("grant member on env only");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys.issue_access(owner.id, false, None).expect("token");
        let (member_token, _) = keys
            .issue_access(member.id, false, None)
            .expect("member token");

        Fixture {
            app_id: app.id,
            env_id,
            env2_id,
            owner_token,
            member_token,
        }
    }

    /// One person in one environment: first seen `days_ago`, active on each
    /// `days_ago - offset` day.
    async fn seed_person(&self, f: &Fixture, who: &str, env: Uuid, days_ago: i64, offsets: &[i64]) {
        let mut conn = self.conn().await;
        seed_statement(
            &mut conn,
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, now() - make_interval(days => $4::int), \
                     now() - make_interval(days => $4::int), 4321)",
            f.app_id,
            who,
            env,
            days_ago as i32,
        )
        .await;
        for off in offsets {
            seed_statement(
                &mut conn,
                "INSERT INTO person_days \
                   (app_id, environment_id, distinct_id, day, events) \
                 VALUES ($1, $3, $2, current_date - $4::int, 1) \
                 ON CONFLICT (app_id, COALESCE(environment_id, \
                   '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
                 DO UPDATE SET events = person_days.events + 1",
                f.app_id,
                who,
                env,
                (days_ago - off) as i32,
            )
            .await;
        }
    }

    /// A cohort of two: both first seen `days_ago`, one returning two days
    /// later, with a non-zero per-person event counter (the counter matters —
    /// see `churn_decodes_nonzero_counters`).
    async fn seed_cohort(&self, f: &Fixture, days_ago: i64) {
        self.seed_person(f, "rt_u1", f.env_id, days_ago, &[0, 2])
            .await;
        self.seed_person(f, "rt_u2", f.env_id, days_ago, &[0]).await;
    }
}

/// The four-bind shape every seed statement here shares: app, name, env, days.
async fn seed_statement(
    conn: &mut sauron_db::PgConn,
    sql: &str,
    app_id: Uuid,
    who: &str,
    env: Uuid,
    days: i32,
) {
    use diesel_async::RunQueryDsl;
    diesel::sql_query(sql)
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Uuid, _>(env)
        .bind::<diesel::sql_types::Integer, _>(days)
        .execute(conn)
        .await
        .unwrap_or_else(|e| panic!("seed statement failed: {e}\n{sql}"));
}

#[tokio::test]
async fn unbackfilled_app_reports_not_ready_with_no_cohorts() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;

    // Push THIS feature's epoch past the app's creation: the app now predates
    // it and has no marker, so the gate must close.
    {
        use diesel_async::RunQueryDsl;
        let mut conn = ts.conn().await;
        diesel::sql_query("UPDATE person_days_epoch SET started_at = now() + interval '1 hour'")
            .execute(&mut conn)
            .await
            .unwrap();
        // Rows exist — proving a not-ready response must not leak them as a
        // partial answer indistinguishable from a complete one.
        ts.seed_cohort(&f, 6).await;
    }

    let grid = ts
        .get_json(
            &format!("/v1/apps/{}/retention?granularity=day", f.app_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(grid["ready"], json!(false));
    assert!(
        grid["cohorts"].as_array().unwrap().is_empty(),
        "a not-ready response must ship NO cohort rows, got {grid}"
    );

    let life = ts
        .get_json(
            &format!("/v1/apps/{}/retention/lifecycle?granularity=day", f.app_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(life["ready"], json!(false));
    assert!(life["points"].as_array().unwrap().is_empty());

    ts.shutdown().await;
}

#[tokio::test]
async fn cell_budget_caps_the_product_not_the_dimensions() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;

    // 30 and 30 are each individually inside MAX_DIM; their 900-cell product
    // is not. Bounding the dimensions independently does not bound the work.
    let status = ts
        .get_status(
            &format!(
                "/v1/apps/{}/retention?granularity=day&cohorts=30&periods=30",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    assert_eq!(status, 400, "30 x 30 = 900 cells must be rejected");

    let status = ts
        .get_status(
            &format!(
                "/v1/apps/{}/retention?granularity=day&cohorts=12&periods=12",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    assert_eq!(status, 200, "12 x 12 = 144 cells is inside the budget");

    // Unknown enum values are named 400s, not silent defaults.
    for bad in [
        format!("/v1/apps/{}/retention?granularity=month", f.app_id),
        format!("/v1/apps/{}/retention?split=bogus", f.app_id),
    ] {
        assert_eq!(ts.get_status(&bad, &f.owner_token).await, 400, "{bad}");
    }

    // And no token is a 401, not a leak.
    let resp = ts
        .client
        .get(format!("{}/v1/apps/{}/retention", ts.base, f.app_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    ts.shutdown().await;
}

#[tokio::test]
async fn unelapsed_periods_are_null_and_elapsed_zeroes_are_zero() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;
    ts.seed_cohort(&f, 6).await;

    let grid = ts
        .get_json(
            &format!(
                "/v1/apps/{}/retention?granularity=day&cohorts=12&periods=12",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    assert_eq!(grid["ready"], json!(true));
    let cohorts = grid["cohorts"].as_array().unwrap();
    assert_eq!(cohorts.len(), 1, "one cohort seeded, got {grid}");
    let periods = cohorts[0]["periods"].as_array().unwrap();

    // Day 0 knowable, day 2 has the returner, day 1 is an elapsed TRUE zero,
    // and the tail beyond `as_of` is null — three distinct facts the wire
    // shape must keep apart.
    assert_eq!(periods[0], json!(2));
    assert_eq!(periods[1], json!(0), "elapsed-with-nobody is 0, not null");
    assert_eq!(periods[2], json!(1));
    assert!(
        periods[11].is_null(),
        "period 11 has not elapsed and must be null, never 0: {periods:?}"
    );

    ts.shutdown().await;
}

/// The regression net for the two bugs only the runtime drive caught: the
/// scalar-vs-array environment bind (a 500 only when `?environment_id=` is
/// present) and the NUMERIC `sum()` decode (a 500 only when a counter is
/// non-zero).
#[tokio::test]
async fn env_scoped_requests_and_nonzero_counters_survive() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;
    ts.seed_cohort(&f, 40).await;

    for path in [
        format!(
            "/v1/apps/{}/retention?granularity=day&environment_id={}",
            f.app_id, f.env_id
        ),
        format!(
            "/v1/apps/{}/retention/lifecycle?granularity=day&environment_id={}",
            f.app_id, f.env_id
        ),
        format!(
            "/v1/apps/{}/retention/churn?granularity=day&environment_id={}",
            f.app_id, f.env_id
        ),
    ] {
        let status = ts.get_status(&path, &f.owner_token).await;
        assert_eq!(status, 200, "environment-scoped {path} must not 500");
    }

    let churn = ts
        .get_json(
            &format!(
                "/v1/apps/{}/retention/churn?granularity=week&silent_periods=4",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    // Weekly granularity: 4 silent PERIODS = 28 silent days, so "churned"
    // means the same span the grid is drawn in.
    assert_eq!(churn["silent_days"], json!(28));
    let people = churn["people"].as_array().unwrap();
    assert!(
        !people.is_empty(),
        "both seeded people are 40 days silent: {churn}"
    );
    assert_eq!(
        people[0]["events_count"],
        json!(4321),
        "a NON-ZERO counter must decode — numeric sum() zeroes fit in the i64 \
         decoder by luck, so an all-zero fixture would pass while the endpoint \
         500s in production"
    );

    ts.shutdown().await;
}

/// The cache must actually serve: within the fresh window a second read
/// returns the SAME `computed_at` and the SAME numbers even after the
/// underlying table changed. Asserting on the mutation (not just the
/// timestamp) is what distinguishes "cached" from "recomputed fast".
#[tokio::test]
async fn grid_and_lifecycle_serve_from_cache_within_the_fresh_window() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;
    ts.seed_cohort(&f, 6).await;

    let grid_path = format!(
        "/v1/apps/{}/retention?granularity=day&cohorts=12&periods=12",
        f.app_id
    );
    let life_path = format!("/v1/apps/{}/retention/lifecycle?granularity=day", f.app_id);

    let first = ts.get_json(&grid_path, &f.owner_token).await;
    assert!(
        first["computed_at"].is_string(),
        "a computed response must stamp computed_at: {first}"
    );
    assert_eq!(first["cohorts"][0]["size"], json!(2));

    let life_first = ts.get_json(&life_path, &f.owner_token).await;
    assert!(life_first["computed_at"].is_string());

    // Mutate: a third person appears in the same cohort day.
    ts.seed_person(&f, "rt_u3", f.env_id, 6, &[0]).await;

    let second = ts.get_json(&grid_path, &f.owner_token).await;
    assert_eq!(
        second["computed_at"], first["computed_at"],
        "second read within the fresh window must be the cached entry"
    );
    assert_eq!(
        second["cohorts"][0]["size"],
        json!(2),
        "the cached entry must not see the post-cache mutation"
    );

    let life_second = ts.get_json(&life_path, &f.owner_token).await;
    assert_eq!(life_second["computed_at"], life_first["computed_at"]);

    ts.shutdown().await;
}

/// The key must hash the RESOLVED filter, never the request's. An env-scoped
/// member and an app-wide owner send byte-identical requests (no
/// `environment_id` at all) — if the key were built from the request, the
/// owner would prime an all-environments entry and the member would be served
/// numbers their grant does not cover. `active_users.rs` calls any deviation
/// here review-Critical; same rule.
#[tokio::test]
async fn cache_is_keyed_by_the_resolved_env_filter_not_the_request() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;
    ts.seed_cohort(&f, 6).await;
    // A third person visible ONLY app-wide: lives in the env the member's
    // grant does not reach.
    ts.seed_person(&f, "rt_u3", f.env2_id, 6, &[0]).await;

    let path = format!(
        "/v1/apps/{}/retention?granularity=day&cohorts=12&periods=12",
        f.app_id
    );

    // Owner primes the cache with the all-environments answer.
    let owner = ts.get_json(&path, &f.owner_token).await;
    assert_eq!(owner["cohorts"][0]["size"], json!(3));

    // The member's identical request must NOT be served that entry.
    let member = ts.get_json(&path, &f.member_token).await;
    assert_eq!(
        member["cohorts"][0]["size"],
        json!(2),
        "an env-scoped member was served the app-wide cached answer: {member}"
    );

    ts.shutdown().await;
}

/// Sorting and row-value paging on the at-risk list: order respects `sort`
/// (bare = descending, `-` = ascending), and a page walk under a counter sort
/// neither repeats nor skips rows — the composite (value, id) cursor is what
/// makes that hold when every row ties on the sort column.
#[tokio::test]
async fn churn_sorts_and_pages_by_row_value_cursor() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;
    // Three silent people; give them distinct event counters.
    for (who, events) in [("rt_a", 10i64), ("rt_b", 30), ("rt_c", 20)] {
        ts.seed_person(&f, who, f.env_id, 40, &[0]).await;
        let mut conn = ts.conn().await;
        use diesel_async::RunQueryDsl;
        diesel::sql_query(
            "UPDATE event_user_environments SET events_count = $2 \
              WHERE app_id = $1 AND distinct_id = $3",
        )
        .bind::<diesel::sql_types::Uuid, _>(f.app_id)
        .bind::<diesel::sql_types::BigInt, _>(events)
        .bind::<diesel::sql_types::Text, _>(who)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    let base = format!(
        "/v1/apps/{}/retention/churn?granularity=day&silent_periods=4",
        f.app_id
    );

    let by_events = ts
        .get_json(&format!("{base}&sort=events"), &f.owner_token)
        .await;
    let order: Vec<i64> = by_events["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["events_count"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![30, 20, 10], "bare sort=events is DESCENDING");

    let ascending = ts
        .get_json(&format!("{base}&sort=-events"), &f.owner_token)
        .await;
    let order: Vec<i64> = ascending["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["events_count"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![10, 20, 30], "-events is ASCENDING");

    // Page at limit=2: probe row proves a next page; following the cursor
    // yields the remaining row exactly once, then no further cursor.
    let page1 = ts
        .get_json(&format!("{base}&sort=events&limit=2"), &f.owner_token)
        .await;
    assert_eq!(page1["people"].as_array().unwrap().len(), 2);
    let cursor = page1["next_cursor"]
        .as_str()
        .expect("a full page with more rows behind it must mint a cursor");
    let page2 = ts
        .get_json(
            &format!("{base}&sort=events&limit=2&cursor={}", urlencode(cursor)),
            &f.owner_token,
        )
        .await;
    let rest: Vec<i64> = page2["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["events_count"].as_i64().unwrap())
        .collect();
    assert_eq!(rest, vec![10], "page 2 is the remaining row, once");
    assert!(page2["next_cursor"].is_null(), "no phantom third page");

    // Enriched aggregates ride every row.
    let p0 = &page1["people"][0];
    assert!(p0["first_seen"].is_string());
    assert!(p0["errors_count"].is_i64() || p0["errors_count"].is_u64());
    assert!(p0["sessions_count"].is_i64() || p0["sessions_count"].is_u64());

    // An unknown sort column is a named 400, not a silent default.
    assert_eq!(
        ts.get_status(&format!("{base}&sort=bogus"), &f.owner_token)
            .await,
        400
    );

    ts.shutdown().await;
}

/// Percent-encode a cursor for a query string.
fn urlencode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}
