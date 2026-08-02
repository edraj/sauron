//! End-to-end session management against the real compiled `sauron-api`
//! binary: the `/v1/me/sessions` surface, the admin force-logout guard matrix,
//! and the three behaviours no unit test can observe — the rotation-grace
//! interaction, the measured residual access-token window, and the fact that
//! "sign out other devices" does not fire the theft alarm fifteen minutes later.
//!
//! Runs with `AUTH_REVOCATION_POLL_SECS=1` in the child env so the timing
//! assertions are seconds.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-sessions-test-secret-000000000000000";

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
        // silently skips anything else, so a "sauron_test_sn_<ts>_<uuid>"
        // spelling leaks every database it creates. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "sn" (2) + 32-hex
        // uuid = 57 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_sn{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        sauron_db::create_database(&admin_url, &db_name)
            .await
            .expect("create ephemeral test database");
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::run_pending_migrations(&db_url)
            .await
            .expect("run migrations on ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");

        let port = free_port();
        let bin = env!("CARGO_BIN_EXE_sauron-api");
        let mut child = tokio::process::Command::new(bin)
            .env("DATABASE_URL", &db_url)
            .env("REDIS_URL", &redis_url)
            .env("JWT_SECRET", JWT_SECRET)
            .env("API_PORT", port.to_string())
            .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
            .env("AUTH_REVOCATION_POLL_SECS", "1")
            // Paired with the per-server `X-Forwarded-For` below. The database
            // is ephemeral but Redis is not: the auth limiters live there, keyed
            // on the caller's address, and are shared by every test binary on
            // this host. Registration is capped at 10/hour/IP, so with one
            // 127.0.0.1 bucket this suite's eight registrations exhaust the
            // budget and every rerun inside the hour 429s in `register_owner`.
            .env("API_TRUST_FORWARDED_HEADERS", "1")
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sauron-api binary");

        let base = format!("http://127.0.0.1:{port}");
        // Set on the client rather than per request, so every helper below —
        // including the ones copied verbatim — inherits the private bucket.
        let mut headers = reqwest::header::HeaderMap::new();
        let octets = Uuid::new_v4().as_bytes()[..3].to_vec();
        headers.insert(
            "x-forwarded-for",
            reqwest::header::HeaderValue::from_str(&format!(
                "10.{}.{}.{}",
                octets[0], octets[1], octets[2]
            ))
            .expect("client ip is a valid header value"),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("build test http client");

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

    async fn post_json(&self, path: &str, token: Option<&str>, body: Value) -> reqwest::Response {
        let mut req = self.client.post(format!("{}{path}", self.base)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"))
    }

    async fn delete(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"))
    }

    /// Log in over HTTP with a chosen `User-Agent`, returning
    /// `(access_token, refresh_token)`.
    async fn login(&self, email: &str, password: &str, ua: &str) -> (String, String) {
        let resp = self
            .client
            .post(format!("{}/v1/auth/login", self.base))
            .header(reqwest::header::USER_AGENT, ua)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .expect("login request");
        let status = resp.status();
        let body: Value = resp.json().await.expect("login body");
        assert!(status.is_success(), "login failed ({status}): {body}");
        (
            body["access_token"]
                .as_str()
                .expect("access_token")
                .to_string(),
            body["refresh_token"]
                .as_str()
                .expect("refresh_token")
                .to_string(),
        )
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

const PASSWORD: &str = "correct-horse-battery-staple";
const UA_CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";

/// Register an org owner over HTTP and return `(email, user_id, org_id)`.
async fn register_owner(server: &TestServer, label: &str) -> (String, Uuid, Uuid) {
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let resp = server
        .post_json(
            "/v1/auth/register",
            None,
            json!({
                "email": email,
                "password": PASSWORD,
                "name": label,
                "org_name": format!("{label} org"),
            }),
        )
        .await;
    let status = resp.status();
    let body: Value = resp.json().await.expect("register body");
    assert!(status.is_success(), "register failed ({status}): {body}");
    let user_id: Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let mut conn = server.conn().await;
    let orgs = repo::list_orgs_for_user(&mut conn, user_id)
        .await
        .expect("list orgs");
    let org_id = orgs.first().expect("owner has an org").id;
    drop(conn);
    (email, user_id, org_id)
}

/// Create a member of `org_id` with `role_perms`, log them in, and return
/// `(email, user_id, access_token, refresh_token)`.
async fn seed_member(
    server: &TestServer,
    org_id: Uuid,
    label: &str,
    role_perms: &[&str],
) -> (String, Uuid, String, String) {
    let mut conn = server.conn().await;
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let user = repo::create_user(&mut conn, &email, &hash, label)
        .await
        .expect("create member");
    // repo.rs:403 — `create_role(conn, org_id: Uuid, name: &str,
    // description: &str, permissions: Value)`. The description is a plain
    // `&str`, not an `Option`, and the permissions are an owned `Value`.
    let role = repo::create_role(
        &mut conn,
        org_id,
        &format!("{label}-role-{}", Uuid::new_v4().simple()),
        "http_sessions fixture",
        json!(role_perms),
    )
    .await
    .expect("create role");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: role.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant role");
    drop(conn);

    let (access, refresh) = server.login(&email, PASSWORD, UA_FIREFOX).await;
    (email, user.id, access, refresh)
}

/// Poll `GET /v1/me` with `token` until it stops returning 2xx, up to `secs`.
/// Returns the elapsed seconds, or panics with the last status.
async fn seconds_until_token_dies(server: &TestServer, token: &str, secs: u64) -> u64 {
    for elapsed in 0..=secs {
        let status = server.get_status("/v1/me", token).await;
        if status == 401 {
            return elapsed;
        }
        tokio::time::sleep(StdDuration::from_secs(1)).await;
    }
    panic!("access token still worked after {secs}s; the revocation snapshot never saw it");
}

#[tokio::test]
async fn two_logins_produce_two_sessions_with_exactly_one_current() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "twosessions").await;
    let (access_a, _refresh_a) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (_access_b, _refresh_b) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    let body = server.get_json("/v1/me/sessions", &access_a).await;
    let rows = body.as_array().expect("array of sessions");
    // register + two logins = three sessions.
    assert_eq!(rows.len(), 3, "one session per login: {body}");
    assert_eq!(
        rows.iter().filter(|r| r["current"] == json!(true)).count(),
        1,
        "exactly one row is the caller's own session: {body}"
    );
    let current = rows.iter().find(|r| r["current"] == json!(true)).unwrap();
    assert_eq!(current["browser"], json!("Chrome"));

    // The structural guarantee: `list_sessions` never touches `refresh_tokens`,
    // so a token hash cannot leak through this endpoint.
    let raw = body.to_string();
    assert!(
        !raw.contains("token_hash"),
        "session list leaked a token hash"
    );
    assert!(
        !raw.contains("revoked_by"),
        "session list leaked the revoking admin"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn revoking_the_session_you_are_using_is_refused() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "selfrevoke").await;
    let (access, _refresh) = server.login(&email, PASSWORD, UA_CHROME).await;

    let body = server.get_json("/v1/me/sessions", &access).await;
    let current = body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == json!(true))
        .expect("a current session");
    let sid = current["id"].as_str().unwrap();

    let resp = server
        .delete(&format!("/v1/me/sessions/{sid}"), &access)
        .await;
    assert_eq!(resp.status().as_u16(), 409);

    // An unknown id is 404, never 403 — a 403 would confirm the id exists.
    let resp = server
        .delete(&format!("/v1/me/sessions/{}", Uuid::new_v4()), &access)
        .await;
    assert_eq!(resp.status().as_u16(), 404);

    server.shutdown().await;
}

#[tokio::test]
async fn revoke_others_spares_the_caller_and_does_not_fire_the_theft_alarm() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "revokeothers").await;
    let (access_keep, refresh_keep) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (access_kill, refresh_kill) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    let resp = server
        .post_json(
            "/v1/me/sessions/revoke-others",
            Some(&access_keep),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    let listed = server.get_json("/v1/me/sessions", &access_keep).await;
    assert_eq!(
        listed.as_array().unwrap().len(),
        1,
        "only the spared session remains"
    );

    // THE REGRESSION TEST. The killed device presents its dead token; without the
    // DELIBERATE_REVOKE_REASONS branch this trips the family kill and the spared
    // session dies on the next line.
    let resp = server
        .post_json(
            "/v1/auth/refresh",
            None,
            json!({ "refresh_token": refresh_kill }),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 401);

    let resp = server
        .post_json(
            "/v1/auth/refresh",
            None,
            json!({ "refresh_token": refresh_keep }),
        )
        .await;
    assert_eq!(
        resp.status().as_u16(),
        200,
        "the spared session must still refresh AFTER the killed device knocks"
    );

    // The measured residual window, not a claim in prose.
    let elapsed = seconds_until_token_dies(&server, &access_kill, 10).await;
    assert!(elapsed <= 5, "revoked access token survived {elapsed}s");

    server.shutdown().await;
}

#[tokio::test]
async fn a_revoked_session_cannot_be_resurrected_inside_the_rotation_grace_window() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "gracewindow").await;
    // Session A stays live, so `user_has_active_refresh_token` is true and the
    // grace condition is genuinely reachable.
    let (access_a, _refresh_a) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (_access_b, refresh_b) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    // Rotate B, so its old token's reason is exactly `rotated`.
    let resp = server
        .post_json(
            "/v1/auth/refresh",
            None,
            json!({ "refresh_token": refresh_b }),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    // Kill B from A, inside the 10-second grace.
    let listed = server.get_json("/v1/me/sessions", &access_a).await;
    let other = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == json!(false))
        .expect("session B");
    let sid_b = other["id"].as_str().unwrap();
    let resp = server
        .delete(&format!("/v1/me/sessions/{sid_b}"), &access_a)
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    // B's other tab now presents the pre-rotation token: reason IS `rotated`,
    // it IS inside the grace, and the user DOES still hold a live token. Only
    // `WHERE auth_sessions.revoked_at IS NULL` inside the mint CTE stops this.
    let resp = server
        .post_json(
            "/v1/auth/refresh",
            None,
            json!({ "refresh_token": refresh_b }),
        )
        .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "the grace window resurrected a session the user had just revoked"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn deactivating_a_member_kills_their_access_token_within_the_poll_interval() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "deactivate").await;
    let (owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;
    let (_email, member_id, member_access, _member_refresh) =
        seed_member(&server, org_id, "victim", &[perm::ISSUE_READ]).await;

    assert!(server.get_status("/v1/me", &member_access).await < 400);

    let resp = server
        .client
        .patch(format!(
            "{}/v1/orgs/{org_id}/members/{member_id}",
            server.base
        ))
        .bearer_auth(&owner_access)
        .json(&json!({ "is_active": false }))
        .send()
        .await
        .expect("deactivate");
    assert_eq!(resp.status().as_u16(), 200);

    // The conversion this test exists for: without it the deactivated member
    // keeps full API access for up to 900 seconds.
    let elapsed = seconds_until_token_dies(&server, &member_access, 10).await;
    assert!(
        elapsed <= 5,
        "deactivated member kept access for {elapsed}s"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn the_admin_force_logout_guard_matrix_holds_over_http() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "adminkill").await;
    let (owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;

    // A custom role holding member:manage but NOT member:credential — the exact
    // role the carve-out exists to make possible.
    let (_a_email, _a_id, manage_only_access, _a_refresh) = seed_member(
        &server,
        org_id,
        "manageonly",
        &[perm::MEMBER_READ, perm::MEMBER_MANAGE],
    )
    .await;
    let (_v_email, victim_id, victim_access, _v_refresh) =
        seed_member(&server, org_id, "target", &[perm::ISSUE_READ]).await;

    let path = format!("/v1/orgs/{org_id}/members/{victim_id}/revoke-sessions");

    // 403: member:manage without member:credential. Both are required.
    let resp = server
        .post_json(&path, Some(&manage_only_access), json!({}))
        .await;
    assert_eq!(resp.status().as_u16(), 403);

    // 404: a real user with no grants in this org.
    let stranger = {
        let mut conn = server.conn().await;
        let hash = sauron_auth::hash_password(PASSWORD).expect("hash");
        let u = repo::create_user(
            &mut conn,
            &format!("stranger-{}@example.com", Uuid::new_v4().simple()),
            &hash,
            "stranger",
        )
        .await
        .expect("create stranger");
        drop(conn);
        u.id
    };
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{stranger}/revoke-sessions"),
            Some(&owner_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 404);

    // 409: self-target. `except: None` would log the admin out of the page they
    // are standing on.
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{owner_id}/revoke-sessions"),
            Some(&owner_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 409);

    // 200 happy path, and the member is ejected within the poll interval.
    let resp = server
        .post_json(&path, Some(&owner_access), json!({}))
        .await;
    assert_eq!(resp.status().as_u16(), 200);
    let elapsed = seconds_until_token_dies(&server, &victim_access, 10).await;
    assert!(
        elapsed <= 5,
        "force-logged-out member kept access for {elapsed}s"
    );

    // "Force login" is not "force password reset", and it is not deactivation.
    let mut conn = server.conn().await;
    let victim = repo::get_user(&mut conn, victim_id)
        .await
        .expect("get user")
        .expect("victim exists");
    drop(conn);
    assert!(
        !victim.must_change_password,
        "force-logout must not force a reset"
    );
    assert!(victim.is_active, "force-logout must not deactivate");

    server.shutdown().await;
}

#[tokio::test]
async fn admin_force_logout_refuses_a_target_who_outranks_or_reaches_outside_the_org() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "outrank").await;
    let (_owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;

    // An Admin-shaped caller: member:manage + member:credential, no org:manage.
    //
    // `issue:read` is not decoration. `guard_member_admin_action` runs
    // `check_no_escalation` BEFORE the cross-org blast-radius check, and the
    // multi-org target below holds `issue:read` through its grant in *this* org.
    // A caller without it is refused 403 by the escalation guard and never
    // reaches the 409 this test exists to pin.
    let (_admin_email, _admin_id, admin_access, _admin_refresh) = seed_member(
        &server,
        org_id,
        "adminish",
        &[
            perm::MEMBER_READ,
            perm::MEMBER_MANAGE,
            perm::MEMBER_CREDENTIAL,
            perm::ISSUE_READ,
        ],
    )
    .await;

    // 403: the target is the Owner, who holds org:manage.
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{owner_id}/revoke-sessions"),
            Some(&admin_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 403);

    // 409: the target also holds a grant in another org — outside this caller's
    // blast radius.
    let (_multi_email, multi_id, _multi_access, _multi_refresh) =
        seed_member(&server, org_id, "multiorg", &[perm::ISSUE_READ]).await;
    let (_other_email, _other_id, other_org_id) = register_owner(&server, "otherorg").await;
    {
        let mut conn = server.conn().await;
        let role = repo::create_role(
            &mut conn,
            other_org_id,
            &format!("outside-{}", Uuid::new_v4().simple()),
            "http_sessions fixture",
            json!([perm::ISSUE_READ]),
        )
        .await
        .expect("create outside role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: other_org_id,
                user_id: multi_id,
                role_id: role.id,
                scope_type: "org".to_string(),
                scope_id: other_org_id,
            },
        )
        .await
        .expect("grant outside role");
        drop(conn);
    }
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{multi_id}/revoke-sessions"),
            Some(&admin_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 409);

    server.shutdown().await;
}
