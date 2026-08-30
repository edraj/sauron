//! Repeated `filter=` params on the list endpoints.
//!
//! `filter` is a `Vec<String>` fed by repeated query params
//! (`?filter=a:eq:b&filter=c:eq:d`). Deserializing that needs
//! `axum_extra::extract::Query` — plain `axum::extract::Query` is
//! `serde_urlencoded`, which cannot build a sequence from repeated keys and
//! fails the whole request with:
//!
//! ```text
//! Failed to deserialize query string: filter: invalid type: string "...",
//! expected a sequence
//! ```
//!
//! `issues` had the right extractor and a test that sends two filters
//! (`http_search.rs`); `transactions` and `sessions` had neither, so both
//! shipped broken — every filter chip on those two pages 400d. This file tests
//! the CLASS rather than the one endpoint that was reported, which is what
//! would have caught the other one.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{NewAppEnvironment, NewRoleGrant};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-list-filters-test-secret-0000000000000";

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
            "sauron_test_{}_lf{}",
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

/// One app, one environment, one org-wide `event:read` token.
struct Fixture {
    app_id: Uuid,
    token: String,
}

impl TestServer {
    async fn seed_fixture(&self) -> Fixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut conn, "lf org", &format!("lf-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "lf p", &format!("lf-p-{s}"))
            .await
            .expect("project");
        let app = repo::create_app(&mut conn, project.id, "L", &format!("lf-a-{s}"), "web")
            .await
            .expect("app");
        seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_lf_{s}"),
            true,
        )
        .await;
        let user = repo::create_user(&mut conn, &format!("lf-{s}@example.test"), "x", "U")
            .await
            .expect("user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "lf role",
            "read",
            json!([perm::EVENT_READ, perm::ISSUE_READ]),
        )
        .await
        .expect("role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: user.id,
                role_id: role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant");
        drop(conn);
        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (token, _) = keys.issue_access(user.id, false, None).expect("token");
        Fixture {
            app_id: app.id,
            token,
        }
    }
}

/// Every list endpoint taking `filter` must accept it once AND repeated.
///
/// One filter is the case a `Vec<String>` behind `serde_urlencoded` already
/// fails on ("invalid type: string, expected a sequence"), so both arities are
/// asserted: passing only the two-filter case would leave the single-chip path
/// — the common one — untested.
#[tokio::test]
async fn list_endpoints_accept_one_and_many_filter_params() {
    let Some(mut ts) = TestServer::start().await else {
        return;
    };
    let f = ts.seed_fixture().await;

    // (endpoint, one filter, a second filter of a different field)
    let cases: [(&str, &str, &str); 4] = [
        ("transactions", "op:eq:http", "name:eq:GET%20%2Fhome"),
        // `duration` and `release` are both Sessions-capable in
        // `sauron-query`'s catalog; `device`/`country` are NOT, and using them
        // here produced a legitimate 400 that masqueraded as the bug.
        ("sessions", "duration:gt:100", "release:eq:1.0.0"),
        // The control: issues already had the right extractor, so a
        // regression here means something reverted the working case too.
        ("issues", "status:eq:unresolved", "level:eq:error"),
        // The fifth endpoint carrying a `Vec<String>` filter. Correct today,
        // and listed so the sweep that found the other two is pinned rather
        // than repeated by hand. (`active-users` is the sixth, already covered
        // with repeated `selection=` params in `http_active_users.rs`.)
        // `release` is Events-capable; `screen` is NOT, and using it produced a
        // legitimate 400 that looked exactly like the extractor bug. Check the
        // field against `sauron-query`'s catalog before believing a 400 here.
        ("events/list", "name:eq:signup", "release:eq:1.0.0"),
    ];

    for (endpoint, one, two) in cases {
        let single = format!("/v1/apps/{}/{endpoint}?filter={one}", f.app_id);
        let status = ts.get_status(&single, &f.token).await;
        assert_eq!(
            status, 200,
            "{endpoint}: a SINGLE filter param must deserialize into Vec<String>, got {status}"
        );

        let many = format!("/v1/apps/{}/{endpoint}?filter={one}&filter={two}", f.app_id);
        let status = ts.get_status(&many, &f.token).await;
        assert_eq!(
            status, 200,
            "{endpoint}: REPEATED filter params must deserialize, got {status}"
        );
    }

    ts.shutdown().await;
}
