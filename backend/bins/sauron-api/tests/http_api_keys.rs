//! End-to-end personal API key management: `/v1/me/api-keys`.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-api-keys-test-secret-000000000000000";

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
            "sauron_test_{}_ak{}",
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
            .env("AUTH_REVOCATION_POLL_SECS", "1")
            // Paired with the per-server `X-Forwarded-For` below. The database
            // is ephemeral but Redis is not: the auth limiters live there, keyed
            // on the caller's address, and are shared by every test binary on
            // this host. Registration is capped at 10/hour/IP, so with one
            // 127.0.0.1 bucket this suite's eight registrations exhaust the
            // budget and every rerun inside the hour 429s in `register_owner`.
            .env("API_TRUST_FORWARDED_HEADERS", "1")
            // Mail + dashboard config: the forced-reset route 503s without them.
            // A refused relay (port 1), not SMTP_SINK: see http_password_reset.rs.
            .env("SMTP_HOST", "127.0.0.1")
            .env("SMTP_PORT", "1")
            .env("SMTP_TLS", "none")
            .env("SMTP_FROM", "sauron@test.invalid")
            .env("DASHBOARD_URL", "https://dash.test")
            .env("MAIL_DRAIN_TICK_SECS", "3600")
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

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";

async fn jwt_for(server: &TestServer, label: &str) -> (String, Uuid, Uuid, String) {
    let (email, user_id, org_id) = register_owner(server, label).await;
    let (jwt, _) = server.login(&email, PASSWORD, UA).await;
    (email, user_id, org_id, jwt)
}

#[tokio::test]
async fn create_returns_token_once_and_list_never_does() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akcreate").await;
    let resp = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "laptop", "expiry": "90d"}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 201);
    let body: Value = resp.json().await.unwrap();
    let token = body["token"].as_str().unwrap();
    assert!(token.starts_with("sauron_pat_"));
    let list = server.get_json("/v1/me/api-keys", &jwt).await;
    assert_eq!(list[0]["prefix"], &token[..17]);
    assert_eq!(list[0]["name"], "laptop");
    assert_eq!(list[0]["expired"], false);
    assert!(!list.to_string().contains(token));
    server.shutdown().await;
}

#[tokio::test]
async fn expiry_choices_validated() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akexpiry").await;
    let bad = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "a", "expiry": "2d"}),
        )
        .await;
    assert_eq!(bad.status().as_u16(), 400);

    let never = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "n", "expiry": "never"}),
        )
        .await;
    assert_eq!(never.status().as_u16(), 201);
    let never: Value = never.json().await.unwrap();
    assert!(never["key"]["expires_at"].is_null());

    let d30 = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "m", "expiry": "30d"}),
        )
        .await;
    assert_eq!(d30.status().as_u16(), 201);
    let d30: Value = d30.json().await.unwrap();
    let at: chrono::DateTime<Utc> = d30["key"]["expires_at"].as_str().unwrap().parse().unwrap();
    let want = Utc::now() + chrono::Duration::days(30);
    assert!((at - want).num_seconds().abs() <= 60, "{at} vs {want}");
    server.shutdown().await;
}

#[tokio::test]
async fn blank_name_is_400() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akblank").await;
    let r = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "   ", "expiry": "90d"}),
        )
        .await;
    assert_eq!(r.status().as_u16(), 400);
    let r = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "x".repeat(65), "expiry": "90d"}),
        )
        .await;
    assert_eq!(r.status().as_u16(), 400);
    server.shutdown().await;
}

#[tokio::test]
async fn delete_revokes_and_second_delete_is_404() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akdel").await;
    let created: Value = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "k", "expiry": "30d"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = created["key"]["id"].as_str().unwrap();
    let r = server.delete(&format!("/v1/me/api-keys/{id}"), &jwt).await;
    assert_eq!(r.status().as_u16(), 200);
    let r: Value = r.json().await.unwrap();
    assert_eq!(r["ok"], true);
    let list = server.get_json("/v1/me/api-keys", &jwt).await;
    assert_eq!(list.as_array().unwrap().len(), 0, "{list}");
    let r = server.delete(&format!("/v1/me/api-keys/{id}"), &jwt).await;
    assert_eq!(r.status().as_u16(), 404);
    server.shutdown().await;
}

#[tokio::test]
async fn cannot_delete_another_users_key() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt_a) = jwt_for(&server, "akowner").await;
    let (_e2, _u2, _o2, jwt_b) = jwt_for(&server, "akother").await;
    let created: Value = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt_a),
            json!({"name": "mine", "expiry": "30d"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = created["key"]["id"].as_str().unwrap();
    let r = server
        .delete(&format!("/v1/me/api-keys/{id}"), &jwt_b)
        .await;
    assert_eq!(r.status().as_u16(), 404);
    let list = server.get_json("/v1/me/api-keys", &jwt_a).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let other = server.get_json("/v1/me/api-keys", &jwt_b).await;
    assert_eq!(other.as_array().unwrap().len(), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn key_cap_is_25() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akcap").await;
    // Seeded straight through the DB: the endpoint's own rate limit (20/min)
    // would otherwise stop the loop before the cap. One of the 25 is
    // expired: unrevoked keys count, expired or not, and the 409 says so.
    let mut conn = server.conn().await;
    for i in 0..25 {
        let m = sauron_auth::api_key::mint_api_key();
        let expires_at = (i == 0).then(|| chrono::Utc::now() - chrono::Duration::days(1));
        sauron_db::api_keys::insert(
            &mut conn,
            _u,
            &format!("k{i}"),
            &m.prefix,
            &m.hash,
            expires_at,
        )
        .await
        .expect("seed key");
    }
    drop(conn);
    let r = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "26th", "expiry": "90d"}),
        )
        .await;
    assert_eq!(r.status().as_u16(), 409);
    let body: Value = r.json().await.expect("409 body");
    assert_eq!(
        body["error"]["message"],
        "you already have 25 unrevoked keys (expired keys count too); revoke one first",
        "{body}"
    );
    server.shutdown().await;
}

/// The cap holds against concurrent creates: with one slot left, parallel
/// requests must yield exactly one key. A count-then-insert lets every one of
/// them see 24 and pass.
#[tokio::test]
async fn key_cap_holds_under_concurrent_creates() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, user_id, _o, jwt) = jwt_for(&server, "akrace").await;
    let mut conn = server.conn().await;
    for i in 0..24 {
        let m = sauron_auth::api_key::mint_api_key();
        sauron_db::api_keys::insert(
            &mut conn,
            user_id,
            &format!("k{i}"),
            &m.prefix,
            &m.hash,
            None,
        )
        .await
        .expect("seed key");
    }
    drop(conn);

    // Open ten keep-alive connections first, so the ten creates below reach
    // the server together instead of staggered by TCP handshakes; the race
    // window is one database round trip wide.
    let mut warm = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let req = server
            .client
            .get(format!("{}/v1/me/api-keys", server.base))
            .bearer_auth(&jwt);
        warm.spawn(async move { req.send().await.expect("warm-up GET") });
    }
    while let Some(r) = warm.join_next().await {
        assert_eq!(r.expect("join").status().as_u16(), 200);
    }

    // Ten at once, inside the endpoint's 20/min budget.
    let mut attempts = tokio::task::JoinSet::new();
    for i in 0..10 {
        let req = server
            .client
            .post(format!("{}/v1/me/api-keys", server.base))
            .bearer_auth(&jwt)
            .json(&json!({"name": format!("race{i}"), "expiry": "90d"}));
        attempts.spawn(async move { req.send().await.expect("POST /v1/me/api-keys") });
    }
    let mut statuses: Vec<u16> = Vec::new();
    while let Some(r) = attempts.join_next().await {
        statuses.push(r.expect("join").status().as_u16());
    }
    let created = statuses.iter().filter(|s| **s == 201).count();
    assert_eq!(
        created, 1,
        "exactly one create fits the last slot: {statuses:?}"
    );
    assert!(
        statuses.iter().all(|s| *s == 201 || *s == 409),
        "the rest are the cap's 409: {statuses:?}"
    );

    let mut conn = server.conn().await;
    let live = sauron_db::api_keys::count_live_for_user(&mut conn, user_id)
        .await
        .expect("count");
    drop(conn);
    assert_eq!(live, 25);
    server.shutdown().await;
}

#[tokio::test]
async fn create_and_revoke_are_audited() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, org_id, jwt) = jwt_for(&server, "akaudit").await;
    let created: Value = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "audited", "expiry": "1y"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = created["key"]["id"].as_str().unwrap();
    let token = created["token"].as_str().unwrap().to_string();
    let r = server.delete(&format!("/v1/me/api-keys/{id}"), &jwt).await;
    assert_eq!(r.status().as_u16(), 200);

    use diesel_async::RunQueryDsl;
    let mut conn = server.conn().await;
    let rows: Vec<(String, Value)> = diesel::sql_query(
        "SELECT action, changes FROM audit_log WHERE org_id = $1 \
         AND action IN ('access_key.create','access_key.revoke') ORDER BY created_at",
    )
    .bind::<diesel::sql_types::Uuid, _>(org_id)
    .load::<AuditRow>(&mut conn)
    .await
    .expect("query audit_log")
    .into_iter()
    .map(|r| (r.action, r.changes))
    .collect();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, vec!["access_key.create", "access_key.revoke"]);
    for (_, changes) in &rows {
        assert!(changes.get("token").is_none(), "{changes}");
        assert!(!changes.to_string().contains(&token), "{changes}");
    }
    // A creation reads `from: null, to: …`; a revoke is the removal shape
    // (`from: …, to: null`), not a second creation.
    let (create, revoke) = (&rows[0].1, &rows[1].1);
    assert_eq!(
        create["name"],
        json!({"from": null, "to": "audited"}),
        "{create}"
    );
    assert_eq!(
        revoke["name"],
        json!({"from": "audited", "to": null}),
        "{revoke}"
    );
    assert!(revoke["prefix"]["from"].is_string(), "{revoke}");
    assert!(revoke["prefix"]["to"].is_null(), "{revoke}");
    assert!(revoke["expires_at"]["from"].is_string(), "{revoke}");
    assert!(revoke["expires_at"]["to"].is_null(), "{revoke}");
    drop(conn);
    server.shutdown().await;
}

#[derive(diesel::QueryableByName)]
struct AuditRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    action: String,
    #[diesel(sql_type = diesel::sql_types::Jsonb)]
    changes: Value,
}

#[tokio::test]
async fn api_key_is_rejected_on_rest_routes() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akrest").await;
    let created: Value = server
        .post_json(
            "/v1/me/api-keys",
            Some(&jwt),
            json!({"name": "rest", "expiry": "30d"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let token = created["token"].as_str().unwrap();
    assert_eq!(server.get_status("/v1/me", token).await, 401);
    server.shutdown().await;
}

/// Seed a member with a grant in `org_id` only; returns `(jwt, user_id)`.
async fn seed_member(
    server: &TestServer,
    org_id: Uuid,
    label: &str,
    role_perms: &[&str],
) -> (String, Uuid) {
    let mut conn = server.conn().await;
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let user = repo::create_user(&mut conn, &email, &hash, label)
        .await
        .expect("create member");
    let role = repo::create_role(
        &mut conn,
        org_id,
        &format!("{label}-role-{}", Uuid::new_v4().simple()),
        "http_api_keys fixture",
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
    let (jwt, _) = server.login(&email, PASSWORD, UA).await;
    (jwt, user.id)
}

/// The member mints a key with their own JWT; returns the raw token.
async fn member_key(server: &TestServer, jwt: &str) -> String {
    let created: Value = server
        .post_json(
            "/v1/me/api-keys",
            Some(jwt),
            json!({"name": "k", "expiry": "90d"}),
        )
        .await
        .json()
        .await
        .unwrap();
    created["token"].as_str().expect("token").to_string()
}

async fn key_is_live(server: &TestServer, token: &str) -> bool {
    let mut conn = server.conn().await;
    sauron_db::api_keys::authenticate(&mut conn, &sauron_auth::hash_token(token))
        .await
        .expect("authenticate")
        .is_some()
}

#[tokio::test]
async fn admin_end_all_sessions_revokes_member_keys() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _o, org_id) = register_owner(&server, "akadm").await;
    let (owner_jwt, _) = server.login(&owner_email, PASSWORD, UA).await;
    let (jwt, member_id) = seed_member(&server, org_id, "akmem", &["event:read"]).await;
    let token = member_key(&server, &jwt).await;
    assert!(key_is_live(&server, &token).await);

    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{member_id}/revoke-sessions"),
            Some(&owner_jwt),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["revoked_keys"], 1, "{body}");
    assert!(!key_is_live(&server, &token).await);
    server.shutdown().await;
}

#[tokio::test]
async fn admin_forced_reset_revokes_member_keys() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _o, org_id) = register_owner(&server, "akrst").await;
    let (owner_jwt, _) = server.login(&owner_email, PASSWORD, UA).await;
    let (jwt, member_id) = seed_member(&server, org_id, "akmem2", &["event:read"]).await;
    let token = member_key(&server, &jwt).await;
    assert!(key_is_live(&server, &token).await);

    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{member_id}/password-reset"),
            Some(&owner_jwt),
            json!({"action": "reset"}),
        )
        .await;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(!key_is_live(&server, &token).await);

    // The count lands in the audit row (the field must be allowlisted).
    use diesel_async::RunQueryDsl;
    let mut conn = server.conn().await;
    let rows = diesel::sql_query(
        "SELECT action, changes FROM audit_log WHERE org_id = $1 \
         AND action = 'member.reset_password'",
    )
    .bind::<diesel::sql_types::Uuid, _>(org_id)
    .load::<AuditRow>(&mut conn)
    .await
    .expect("query audit_log");
    drop(conn);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].changes["revoked_keys"]["to"], 1,
        "{}",
        rows[0].changes
    );
    server.shutdown().await;
}

#[tokio::test]
async fn own_password_change_keeps_keys() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (_e, _u, _o, jwt) = jwt_for(&server, "akpw").await;
    let token = member_key(&server, &jwt).await;
    let resp = server
        .post_json(
            "/v1/auth/password",
            Some(&jwt),
            json!({"current_password": PASSWORD, "new_password": "a-different-passphrase-9"}),
        )
        .await;
    let status = resp.status().as_u16();
    assert!(status < 300, "change password failed: {status}");
    assert!(key_is_live(&server, &token).await);
    server.shutdown().await;
}

/// `POST /v1/orgs/{org}/members/revoke-api-keys`: every live key of every
/// named member, recorded per member (reason, who, audit), and only theirs.
#[tokio::test]
async fn admin_bulk_revoke_revokes_every_named_members_keys() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "akbulk").await;
    let (owner_jwt, _) = server.login(&owner_email, PASSWORD, UA).await;
    let (jwt_a, a) = seed_member(&server, org_id, "akbulka", &["event:read"]).await;
    let (jwt_b, b) = seed_member(&server, org_id, "akbulkb", &["event:read"]).await;
    let (jwt_c, c) = seed_member(&server, org_id, "akbulkc", &["event:read"]).await;
    let a1 = member_key(&server, &jwt_a).await;
    let a2 = member_key(&server, &jwt_a).await;
    let b1 = member_key(&server, &jwt_b).await;
    let c1 = member_key(&server, &jwt_c).await;

    // The Members page sees who holds working keys.
    let members = server
        .get_json(&format!("/v1/orgs/{org_id}/members"), &owner_jwt)
        .await;
    let keys_of = |members: &Value, id: Uuid| -> Value {
        members
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["user_id"] == json!(id))
            .unwrap_or_else(|| panic!("{id} listed"))["api_keys"]
            .clone()
    };
    assert_eq!(keys_of(&members, a)["active"], 2, "{members}");
    assert_eq!(keys_of(&members, owner_id)["active"], 0);

    // A named twice counts once.
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/revoke-api-keys"),
            Some(&owner_jwt),
            json!({"user_ids": [a, b, a]}),
        )
        .await;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["revoked"], 3, "{body}");
    let per: std::collections::HashMap<String, i64> = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user_id"].as_str().unwrap().to_string(),
                m["revoked"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(per[&a.to_string()], 2, "{body}");
    assert_eq!(per[&b.to_string()], 1, "{body}");

    for t in [&a1, &a2, &b1] {
        assert!(
            !key_is_live(&server, t).await,
            "a named member's key is dead"
        );
    }
    assert!(
        key_is_live(&server, &c1).await,
        "an unnamed member's key lives"
    );

    // The member sees when and why on their own Account page.
    let mine = server
        .get_json("/v1/me/api-keys?include_revoked=1", &jwt_a)
        .await;
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 2, "{mine:?}");
    for k in mine {
        assert_eq!(k["revoked_reason"], "admin_revoked", "{k}");
        assert!(k["revoked_at"].is_string(), "{k}");
    }
    assert_eq!(
        server.get_json("/v1/me/api-keys", &jwt_a).await,
        json!([]),
        "without the flag only live keys are listed"
    );

    let members = server
        .get_json(&format!("/v1/orgs/{org_id}/members"), &owner_jwt)
        .await;
    assert_eq!(keys_of(&members, a)["active"], 0);
    assert_eq!(keys_of(&members, c)["active"], 1);
    assert!(keys_of(&members, a)["revoked_by_password_reset_at"].is_null());

    // One audit row per named member, with the count and the reason.
    use diesel_async::RunQueryDsl;
    let mut conn = server.conn().await;
    let rows = diesel::sql_query(
        "SELECT action, changes FROM audit_log WHERE org_id = $1 \
         AND action = 'access_key.revoke_all' ORDER BY (changes->'revoked_keys'->>'to')::int",
    )
    .bind::<diesel::sql_types::Uuid, _>(org_id)
    .load::<AuditRow>(&mut conn)
    .await
    .expect("query audit_log");
    drop(conn);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].changes["revoked_keys"]["to"], 1,
        "{}",
        rows[0].changes
    );
    assert_eq!(
        rows[1].changes["revoked_keys"]["to"], 2,
        "{}",
        rows[1].changes
    );
    for r in &rows {
        assert_eq!(r.changes["reason"]["to"], "admin_revoked", "{}", r.changes);
    }
    server.shutdown().await;
}

/// The refusals, each before anything is revoked.
#[tokio::test]
async fn admin_bulk_revoke_refuses_without_revoking_anything() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "akbulkr").await;
    let (owner_jwt, _) = server.login(&owner_email, PASSWORD, UA).await;
    let (jwt_a, a) = seed_member(&server, org_id, "akbulkra", &["event:read"]).await;
    let a1 = member_key(&server, &jwt_a).await;
    let path = format!("/v1/orgs/{org_id}/members/revoke-api-keys");
    let post = |token: String, user_ids: Value| {
        let server = &server;
        let path = path.clone();
        async move {
            let resp = server
                .post_json(&path, Some(&token), json!({"user_ids": user_ids}))
                .await;
            let status = resp.status().as_u16();
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            (status, body)
        }
    };

    let (s, body) = post(owner_jwt.clone(), json!([])).await;
    assert_eq!(s, 400, "no member named: {body}");

    let too_many: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();
    let (s, body) = post(owner_jwt.clone(), json!(too_many)).await;
    assert_eq!(s, 400, "more than 100: {body}");

    let (s, body) = post(owner_jwt.clone(), json!([a, owner_id])).await;
    assert_eq!(s, 409, "yourself: {body}");

    // A stranger's id among real members: 404, and the real member's key
    // survives (every check runs before any revoke). The stranger is not
    // named: that would be an email lookup by id.
    let stranger = register_owner(&server, "akbulkx").await;
    let (s, body) = post(owner_jwt.clone(), json!([a, stranger.1])).await;
    assert_eq!(s, 404, "not a member here: {body}");
    assert!(!body.to_string().contains(&stranger.0), "{body}");
    assert!(key_is_live(&server, &a1).await);

    // A member without member:credential may not.
    let (jwt_r, _) = seed_member(
        &server,
        org_id,
        "akbulkrd",
        &["member:read", "member:manage"],
    )
    .await;
    let (s, body) = post(jwt_r, json!([a])).await;
    assert_eq!(s, 403, "needs member:credential: {body}");
    assert!(key_is_live(&server, &a1).await);
    server.shutdown().await;
}
