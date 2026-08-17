//! HTTP-level tests for the combined active-users API (S4): the two
//! `routes::active_users::*` handlers — the JSON report and the `.csv`
//! download — driven through the real router.
//!
//! Spawns the actual compiled `sauron-api` binary against an ephemeral,
//! migrated database — same harness shape as `tests/http_env_scoping.rs`
//! (duplicated rather than shared; see that file's `TestServer`/`swap_database`
//! doc comments for why a cross-test-binary dependency isn't worth it for
//! machinery this small). Only the subset of that machinery these tests
//! actually call is reproduced here.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset.

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
const JWT_SECRET: &str = "http-active-users-test-secret-0000000000000";

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

    /// Every port this process has already handed out.
    static ISSUED: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();

    let issued = ISSUED.get_or_init(|| Mutex::new(HashSet::new()));
    for _ in 0..100 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        // `insert` returns false if we have issued this port before. The probe
        // listener is dropped on return so the child can bind, and the kernel is
        // then free to hand the same port to the next caller — which is exactly
        // what happens, because tests in one binary run on parallel threads and
        // two `TestServer::start()` calls race here. The loser's `sauron-api`
        // died with "Address already in use" and the harness reported it as
        // "exited early", which reads like a product fault rather than a
        // harness one. The probe bind still rules out ports held by other
        // processes; the set rules out the ones we handed to ourselves.
        if issued.lock().expect("port registry").insert(port) {
            return port;
        }
    }
    panic!("no unused ephemeral port after 100 attempts");
}

/// A fresh, migrated, ephemeral database plus a real spawned `sauron-api`
/// process, and an HTTP client for driving it. See
/// `tests/http_env_scoping.rs`'s `TestServer` for the full doc comments this
/// duplicates — only the subset of methods these tests call is reproduced.
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

        // Segment order is load-bearing — timestamp FIRST, discriminator glued
        // to the uuid. See the fuller account at the identical site in
        // `http_env_scoping.rs`: the reaper in `sauron-db`'s
        // `tests/common::reap_stale_test_databases` parses the first
        // underscore-delimited segment after `sauron_test_` as a timestamp and
        // silently skips anything else, so a "sauron_test_au_<ts>_<uuid>"
        // spelling leaks every database it creates. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "au" (2) + 32-hex
        // uuid = 57 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_au{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        // One migrated template, copied per test — see
        // `sauron_db::create_test_database`. Falls back to replaying the
        // migrations, so the resulting schema is identical either way.
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
            // Required and fail-closed since migration 000046: the API refuses to
            // boot without it (it is the only key that decrypts stored channels).
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

    /// GET an Overview *section* endpoint and return its PAYLOAD.
    ///
    /// The app-scoped `/analytics/active-users` is one of the five Overview
    /// sections, so it now answers from a server-side cache with
    /// `{state, computed_at, data}` and recomputes in the background rather
    /// than on the request path — see `overview_cache`'s module docs. The first
    /// read of any selection therefore reports `computing` with a null `data`.
    ///
    /// Polls until a payload lands and unwraps the envelope, so the assertions
    /// below are unchanged from when this answered synchronously. Panics with
    /// the recompute's own error rather than timing out opaquely.
    ///
    /// Duplicated from `http_env_scoping.rs` rather than shared: these two
    /// suites each carry their own `TestServer` (different fixtures, different
    /// spawn paths) and there is no common test module to hang it on.
    async fn get_section(&self, path: &str, token: &str) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut last = Value::Null;
        while std::time::Instant::now() < deadline {
            let v = self.get_json(path, token).await;
            if let Some(err) = v["error"].as_str() {
                panic!("GET {path}: section recompute failed: {err}");
            }
            if !v["data"].is_null() {
                return v["data"].clone();
            }
            last = v;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("GET {path}: section never left `computing` within 20s; last: {last}");
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

/// Two apps in one project. `owner_token` reaches both app-wide;
/// `env_member_token` holds `event:read` on app A's `env_a1` ONLY — the
/// persona §4.3 and §4.5 are about.
struct ActiveUsersFixture {
    project_id: Uuid,
    sibling_project_id: Uuid,
    sibling_app_id: Uuid,
    app_a: Uuid,
    app_b: Uuid,
    env_a1: Uuid,
    env_b1: Uuid,
    owner_token: String,
    env_member_token: String,
    outsider_token: String,
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

impl TestServer {
    async fn seed_active_users_fixture(&self) -> ActiveUsersFixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "au org", &format!("au-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "au project", &format!("au-p-{s}"))
            .await
            .expect("project");
        let sibling = repo::create_project(&mut conn, org.id, "au sibling", &format!("au-s-{s}"))
            .await
            .expect("sibling project");
        let app_a = repo::create_app(&mut conn, project.id, "A", &format!("au-a-{s}"), "web")
            .await
            .expect("app a");
        let app_b = repo::create_app(&mut conn, project.id, "B", &format!("au-b-{s}"), "web")
            .await
            .expect("app b");
        let sibling_app =
            repo::create_app(&mut conn, sibling.id, "S", &format!("au-sib-{s}"), "web")
                .await
                .expect("sibling app");
        let env_a1 = seed_env(
            &mut conn,
            project.id,
            app_a.id,
            "prod",
            &format!("pk_a1_{s}"),
            true,
        )
        .await;
        let env_b1 = seed_env(
            &mut conn,
            project.id,
            app_b.id,
            "prod-b",
            &format!("pk_b1_{s}"),
            true,
        )
        .await;

        let owner = repo::create_user(
            &mut conn,
            &format!("au-owner-{s}@example.test"),
            "x",
            "Owner",
        )
        .await
        .expect("owner");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "au owner role",
            "org-wide event read",
            json!([perm::EVENT_READ]),
        )
        .await
        .expect("owner role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: owner_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant owner");

        let member = repo::create_user(
            &mut conn,
            &format!("au-member-{s}@example.test"),
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
                role_id: owner_role.id,
                scope_type: "env".to_string(),
                scope_id: env_a1,
            },
        )
        .await
        .expect("grant member on env_a1 only");

        let outsider =
            repo::create_user(&mut conn, &format!("au-out-{s}@example.test"), "x", "Out")
                .await
                .expect("outsider");

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys
            .issue_access(owner.id, false, None)
            .expect("owner token");
        let (env_member_token, _) = keys
            .issue_access(member.id, false, None)
            .expect("member token");
        let (outsider_token, _) = keys
            .issue_access(outsider.id, false, None)
            .expect("outsider token");

        ActiveUsersFixture {
            project_id: project.id,
            sibling_project_id: sibling.id,
            sibling_app_id: sibling_app.id,
            app_a: app_a.id,
            app_b: app_b.id,
            env_a1,
            env_b1,
            owner_token,
            env_member_token,
            outsider_token,
        }
    }
}

const WINDOW: &str = "from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z";

fn url(f: &ActiveUsersFixture, extra: &str) -> String {
    format!(
        "/v1/projects/{}/active-users?{WINDOW}&{extra}",
        f.project_id
    )
}

#[tokio::test]
async fn active_users_http_contract() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_active_users");
        return;
    };
    let f = h.seed_active_users_fixture().await;

    // A caller with no grant at all in the project's org is a 403 (the project
    // itself resolves, so it is not a 404).
    assert_eq!(
        h.get_status(
            &url(&f, &format!("selection={}", f.app_a)),
            &f.outsider_token
        )
        .await,
        403,
        "non-member"
    );

    // Partial reach is a 403 that NAMES the app, never partial data.
    let resp = h
        .get(
            &url(
                &f,
                &format!("selection={}:{}&selection={}", f.app_a, f.env_a1, f.app_b),
            ),
            &f.env_member_token,
        )
        .await;
    assert_eq!(resp.status().as_u16(), 403);
    let text = resp.text().await.expect("body");
    assert!(
        text.contains(&f.app_b.to_string()),
        "the 403 must name the denied app so the page can drop a stale selection: {text}"
    );

    // The same member, asking only for what they hold, succeeds.
    assert_eq!(
        h.get_status(
            &url(&f, &format!("selection={}:{}", f.app_a, f.env_a1)),
            &f.env_member_token
        )
        .await,
        200,
        "own app+env"
    );

    // The §4.5 headline: a BARE selection from an env-scoped member resolves to
    // `subset`, never `all`. With `Option<Uuid>` this would render as "All
    // environments" over a number computed from one environment.
    let body: Value = h
        .get_json(
            &url(&f, &format!("selection={}", f.app_a)),
            &f.env_member_token,
        )
        .await;
    assert_eq!(
        body["selections"][0]["resolved"], "subset",
        "an env-scoped member's bare selection must be labelled subset: {body}"
    );

    // The dimension is per selection, so a global one is refused.
    assert_eq!(
        h.get_status(
            &url(
                &f,
                &format!("selection={}&environment_id={}", f.app_a, f.env_a1)
            ),
            &f.owner_token
        )
        .await,
        400,
        "environment_id"
    );

    // Window validation.
    assert_eq!(
        h.get_status(
            &format!(
                "/v1/projects/{}/active-users?from=2026-05-08T00:00:00Z&to=2026-05-01T00:00:00Z&selection={}",
                f.project_id, f.app_a
            ),
            &f.owner_token
        )
        .await,
        400,
        "to < from"
    );
    assert_eq!(
        h.get_status(
            &format!(
                "/v1/projects/{}/active-users?from=2026-01-01T00:00:00Z&to=2026-06-01T00:00:00Z&selection={}",
                f.project_id, f.app_a
            ),
            &f.owner_token
        )
        .await,
        400,
        "span > 92 days"
    );

    // An app that resolves into a DIFFERENT project is a 400, not a silent
    // zero-row leg — the caller's app ids carry no FK to the path's project.
    let status = h
        .get_status(
            &url(&f, &format!("selection={}", f.sibling_app_id)),
            &f.owner_token,
        )
        .await;
    assert_eq!(status, 400, "app in project {}", f.sibling_project_id);

    h.shutdown().await;
}

/// The shared-`build_report` guarantee, checked rather than assumed.
#[tokio::test]
async fn active_users_csv_matches_the_json_route() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_active_users");
        return;
    };
    let f = h.seed_active_users_fixture().await;
    let query = format!(
        "selection={}:{}&selection={}:{}",
        f.app_a, f.env_a1, f.app_b, f.env_b1
    );

    let json: Value = h.get_json(&url(&f, &query), &f.owner_token).await;
    let series_len = json["series"].as_array().expect("series").len();

    let resp = h
        .get(
            &format!(
                "/v1/projects/{}/active-users.csv?{WINDOW}&{query}",
                f.project_id
            ),
            &f.owner_token,
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/csv; charset=utf-8")
    );
    let disposition = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let expected_prefix = format!(
        "attachment; filename=\"sauron-active-users-{}-",
        f.project_id
    );
    assert!(
        disposition.starts_with(&expected_prefix),
        "content-disposition: {disposition}"
    );
    let dates: String = disposition
        .trim_end_matches(".csv\"")
        .chars()
        .rev()
        .take(17)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    assert!(
        dates.len() == 17 && dates.as_bytes()[8] == b'_',
        "the filename must carry two YYYYMMDD dates joined by '_': {disposition}"
    );

    let body = resp.text().await.expect("csv body");
    let mut lines = body.split("\r\n");
    assert_eq!(
        lines.next(),
        Some("day,active_total,active_identified,active_guest")
    );
    let rows = lines.filter(|l| !l.is_empty()).count();
    assert_eq!(
        rows, series_len,
        "the CSV row count must equal the JSON route's series length for the same query"
    );

    h.shutdown().await;
}

// --- the APP-scoped cross-tier series (a different endpoint entirely) --------
//
// `/v1/apps/{app_id}/analytics/active-users` is not the project report above. It
// is app-scoped, reads ACROSS the hot/cold tiers (so it keeps answering past the
// rotation age, where the project report reports `truncated`), and returns
// `partial_days` for any day it could not count exactly.
//
// It had DB-level tests (`sauron-db/tests/active_users.rs`, mutation-checked) but
// nothing exercising the ROUTE — so the wiring, the permission gate and the
// environment narrowing were all uncovered until the dashboard started calling it.

/// Insert one analytics event. Raw SQL because this test binary has no access to
/// the `sauron-db` test-common seeder, and the handful of NOT NULL columns is
/// smaller than the machinery to share it.
async fn seed_analytics_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    environment_id: Option<Uuid>,
    distinct_id: &str,
    occurred_at: chrono::DateTime<Utc>,
) {
    use diesel_async::RunQueryDsl;
    diesel::sql_query(
        "INSERT INTO analytics_events \
         (id, app_id, environment_id, name, distinct_id, properties, context, occurred_at, \
          received_at, tags, contexts, extra) \
         VALUES ($1, $2, $3, 'pageview', $4, '{}'::jsonb, '{}'::jsonb, $5, $5, \
                 '{}'::jsonb, '{}'::jsonb, '{}'::jsonb)",
    )
    .bind::<diesel::sql_types::Uuid, _>(Uuid::new_v4())
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Nullable<diesel::sql_types::Uuid>, _>(environment_id)
    .bind::<diesel::sql_types::Text, _>(distinct_id)
    .bind::<diesel::sql_types::Timestamptz, _>(occurred_at)
    .execute(conn)
    .await
    .expect("insert analytics event");
}

fn at(day: u32, hour: u32) -> chrono::DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(2026, 5, day, hour, 0, 0).unwrap()
}

#[tokio::test]
async fn the_app_scoped_active_users_series_is_reachable_and_env_scoped() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping");
        return;
    };
    let f = h.seed_active_users_fixture().await;

    // A SECOND environment on app_a. The fixture's `env_b1` belongs to app_b, so
    // using it here would have tagged an app_a event with an environment app_a is
    // not enrolled in — data the ingest cannot produce, and the kind of fixture
    // that makes a scoping assertion pass for the wrong reason.
    let env_a2 = {
        let mut conn = h.conn().await;
        let s = Uuid::new_v4().simple().to_string();
        seed_env(
            &mut conn,
            f.project_id,
            f.app_a,
            &format!("staging-{s}"),
            &format!("pk_a2_{s}"),
            false,
        )
        .await
    };

    {
        let mut conn = h.conn().await;
        // env_a1: alice twice on day 10 (one person), bob once. env_a2: carol.
        seed_analytics_event(&mut conn, f.app_a, Some(f.env_a1), "alice", at(10, 9)).await;
        seed_analytics_event(&mut conn, f.app_a, Some(f.env_a1), "alice", at(10, 17)).await;
        seed_analytics_event(&mut conn, f.app_a, Some(f.env_a1), "bob", at(10, 12)).await;
        seed_analytics_event(&mut conn, f.app_a, Some(env_a2), "carol", at(10, 10)).await;
    }

    // A wide window so the fixed 2026-05 dates fall inside it regardless of when
    // the suite runs. `since_days` is relative to now, so it must span back to May.
    let days = (Utc::now().date_naive() - at(10, 0).date_naive()).num_days() + 2;
    let path = format!(
        "/v1/apps/{}/analytics/active-users?since_days={days}",
        f.app_a
    );

    let v = h.get_section(&path, &f.owner_token).await;
    let series = v["series"].as_array().expect("series is an array");
    let day10 = series
        .iter()
        .find(|p| p["day"].as_str() == Some("2026-05-10"))
        .unwrap_or_else(|| panic!("no 2026-05-10 point in {v}"));
    assert_eq!(
        day10["count"].as_i64(),
        Some(3),
        "three distinct people across both envs despite four events: {v}"
    );
    // The field must be present even when empty — the dashboard reads its length
    // unconditionally, and `undefined.length` is a blank card.
    assert!(
        v["partial_days"].is_array(),
        "partial_days must always be an array: {v}"
    );

    // Env-scoped member sees only their environment's population. This is the
    // assertion that would have caught a handler wired to `app_id` alone.
    let scoped = h.get_section(&path, &f.env_member_token).await;
    let scoped_day10 = scoped["series"]
        .as_array()
        .expect("series")
        .iter()
        .find(|p| p["day"].as_str() == Some("2026-05-10"))
        .unwrap_or_else(|| panic!("no 2026-05-10 point for the env member: {scoped}"));
    assert_eq!(
        scoped_day10["count"].as_i64(),
        Some(2),
        "alice + bob in env_a1, NOT carol in env_b1: {scoped}"
    );

    // And it is authorized, not public.
    let status = h.get_status(&path, &f.outsider_token).await;
    assert!(
        status == 403 || status == 404,
        "a principal with no grant on this app must not read its population; got {status}"
    );

    h.shutdown().await;
}
