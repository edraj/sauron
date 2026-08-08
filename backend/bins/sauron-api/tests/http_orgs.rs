//! HTTP-level tests for `DELETE /v1/orgs/{org_id}/roles/{role_id}` (Task 2 of
//! the admin-view-and-role-management plan), driven through the real router.
//!
//! Before this endpoint, `role:manage` gated role *create* and *update* but
//! nothing in the stack could ever remove one — custom roles were
//! create-and-edit-forever. A unit test on `repo::delete_role` alone cannot
//! see whether `delete_role_handler` wires the guard order or the
//! count-before-cascade sequencing correctly: both are only observable as a
//! wrong status code or a wrong JSON body on the wire, which is what these
//! five tests each pin:
//!
//!  1. deleting an unheld role reports zero revoked grants and the role
//!     disappears from `list_roles`;
//!  2. deleting a held role reports the holder count AND actually revokes
//!     access — asserted by driving `GET /access` as the former holders
//!     before and after, not just by trusting the count in the response;
//!  3. deleting a system preset is refused with 400, not 404 — presets are
//!     already public via `list_roles`, so confirming existence costs
//!     nothing (see `delete_role_handler`'s guard-order comment);
//!  4. deleting another org's role 404s rather than 403ing, so the status
//!     code alone can't confirm the id is valid;
//!  5. deleting the same role twice 404s the second time.
//!
//! Every test spawns the actual compiled `sauron-api` binary (via Cargo's
//! `CARGO_BIN_EXE_sauron-api`) against a fresh, migrated, ephemeral database
//! and drives it with `reqwest`. See `tests/http_env_scoping.rs`'s
//! `TestServer` for the full doc comments this file's copy abbreviates, and
//! `tests/http_sessions.rs` for the identical per-server `X-Forwarded-For`
//! workaround around the registration rate limit.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset.

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
const JWT_SECRET: &str = "http-orgs-test-secret-00000000000000000000";

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
        // See the identical comment in `http_env_scoping.rs` / `http_sessions.rs`:
        // the probe listener is dropped on return, so a concurrent
        // `TestServer::start()` on another thread can be handed the same
        // port before this process's child binds it. The registry rules out
        // ports this process has already issued to itself.
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

        // Segment order is load-bearing — timestamp FIRST, discriminator
        // glued to the uuid. See the fuller account at the identical site in
        // `http_env_scoping.rs`: the reaper in `sauron-db`'s
        // `tests/common::reap_stale_test_databases` parses the first
        // underscore-delimited segment after `sauron_test_` as a timestamp
        // and silently skips anything else, so a differently ordered name
        // leaks every database it creates. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "org" (3) +
        // 32-hex uuid = 58 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_org{}",
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
            // Paired with the per-server `X-Forwarded-For` below. Redis (and
            // therefore the register-rate-limit bucket, keyed on caller IP)
            // is shared by every test binary on this host, not reset per
            // test — see `tests/http_sessions.rs`'s identical comment. This
            // file registers up to two owners per test across five tests, so
            // without a private bucket a rerun inside the hour 429s.
            .env("API_TRUST_FORWARDED_HEADERS", "1")
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sauron-api binary");

        let base = format!("http://127.0.0.1:{port}");
        // Set on the client rather than per request, so every helper below
        // inherits the private bucket.
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

    /// DELETE `path` and parse the body as JSON regardless of status. Every
    /// case below inspects both: this endpoint puts information in the
    /// status code (which guard fired) as well as the body
    /// (`revoked_grants`).
    async fn delete_json(&self, path: &str, token: &str) -> (u16, Value) {
        let resp = self
            .client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_else(|e| {
            panic!("DELETE {path}: failed to read body (status {status}): {e}")
        });
        let body = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("DELETE {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        });
        (status, body)
    }

    /// Log in over HTTP and return the access token.
    async fn login(&self, email: &str, password: &str) -> String {
        let resp = self
            .client
            .post(format!("{}/v1/auth/login", self.base))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .expect("login request");
        let status = resp.status();
        let body: Value = resp.json().await.expect("login body");
        assert!(status.is_success(), "login failed ({status}): {body}");
        body["access_token"]
            .as_str()
            .expect("access_token")
            .to_string()
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

/// Create a custom role directly against the database — bypassing the HTTP
/// create-role endpoint, which is a different task's surface, not this one's
/// — and return its id.
async fn seed_role(server: &TestServer, org_id: Uuid, perms: &[&str]) -> Uuid {
    let mut conn = server.conn().await;
    let role = repo::create_role(
        &mut conn,
        org_id,
        &format!("custom-{}", Uuid::new_v4().simple()),
        "http_orgs fixture",
        json!(perms),
    )
    .await
    .expect("create role");
    drop(conn);
    role.id
}

/// The id of a seeded system preset (`Owner` / `Admin` / `Developer` /
/// `Viewer`), so a test can grant the *real* shipped role rather than a
/// hand-rolled imitation of it.
///
/// That distinction is the whole point of the `Admin`-cannot-delete test
/// below: the exploit it pins depends on `Admin`'s actual shipped permission
/// set (`rbac.rs:129` — `role:manage` yes, `org:manage` no), so a fixture role
/// that merely resembles Admin would still pass if that set ever changed.
async fn preset_role_id(server: &TestServer, name: &str) -> Uuid {
    let mut conn = server.conn().await;
    let role = repo::get_system_role(&mut conn, name)
        .await
        .expect("get_system_role")
        .unwrap_or_else(|| panic!("{name} preset is seeded at API boot"));
    drop(conn);
    role.id
}

/// Create a user holding `role_id` at org scope and nothing else, log them
/// in, and return `(user_id, access_token)`.
///
/// Granting nothing else is deliberate: it is what makes losing the grant
/// observable as `GET /access` flipping from 200 to 403, rather than merely a
/// shorter permission list — the strongest available proof that a delete's
/// cascade actually revoked access, not just that the response counted it.
async fn seed_sole_holder(
    server: &TestServer,
    org_id: Uuid,
    role_id: Uuid,
    label: &str,
) -> (Uuid, String) {
    let mut conn = server.conn().await;
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let user = repo::create_user(&mut conn, &email, &hash, label)
        .await
        .expect("create holder");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant role");
    drop(conn);

    let access = server.login(&email, PASSWORD).await;
    (user.id, access)
}

#[tokio::test]
async fn deleting_an_unheld_role_reports_zero_and_removes_it_from_the_list() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "delunheld").await;
    let access = server.login(&owner_email, PASSWORD).await;
    let role_id = seed_role(&server, org_id, &[perm::ISSUE_READ]).await;

    let before = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &access)
        .await;
    assert!(
        before
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(role_id)),
        "fixture role missing before delete: {before}"
    );

    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org_id}/roles/{role_id}"), &access)
        .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["revoked_grants"], json!(0));

    let after = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &access)
        .await;
    assert!(
        !after
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(role_id)),
        "deleted role still listed: {after}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn deleting_a_held_role_revokes_every_holder() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "delheld").await;
    let access = server.login(&owner_email, PASSWORD).await;
    let role_id = seed_role(&server, org_id, &[perm::ISSUE_READ]).await;

    let (_a_id, a_access) = seed_sole_holder(&server, org_id, role_id, "holdera").await;
    let (_b_id, b_access) = seed_sole_holder(&server, org_id, role_id, "holderb").await;

    // Before: both holders can read the org access view and see the
    // permission the about-to-be-deleted role conferred.
    for (label, tok) in [("a", &a_access), ("b", &b_access)] {
        let before = server
            .get_json(&format!("/v1/orgs/{org_id}/access"), tok)
            .await;
        assert!(
            before["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == &json!(perm::ISSUE_READ)),
            "holder {label} missing {} before delete: {before}",
            perm::ISSUE_READ
        );
    }

    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org_id}/roles/{role_id}"), &access)
        .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["revoked_grants"], json!(2));

    // After: the cascade actually revoked the grant rather than merely being
    // counted. Each holder's ONLY grant is gone, so `user_grants_in_org`
    // returns nothing and `/access` 403s — not a 200 with a shorter
    // permission list, which a count-only bug could still produce.
    for (label, tok) in [("a", &a_access), ("b", &b_access)] {
        let status = server
            .get_status(&format!("/v1/orgs/{org_id}/access"), tok)
            .await;
        assert_eq!(
            status, 403,
            "holder {label} still has org access after their sole role was deleted"
        );
    }

    server.shutdown().await;
}

#[tokio::test]
async fn deleting_a_system_preset_is_refused() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "delpreset").await;
    let access = server.login(&owner_email, PASSWORD).await;

    let roles = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &access)
        .await;
    let developer = roles
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == json!("Developer") && r["is_system"] == json!(true))
        .expect("Developer preset present");
    let developer_id = developer["id"].as_str().unwrap();

    // Guard-order pin: `role.org_id` (`None` for a preset) never equals
    // `Some(org_id)`, so if the cross-org check ran BEFORE the is_system
    // check, this would also 404. Only checking presets first yields 400.
    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org_id}/roles/{developer_id}"), &access)
        .await;
    assert_eq!(status, 400, "body: {body}");

    // Refused, not silently no-op'd: the preset is still there afterward.
    let after = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &access)
        .await;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(developer_id)),
        "Developer preset disappeared despite the 400: {after}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn deleting_another_orgs_role_is_not_found() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner1_email, _owner1_id, org1_id) = register_owner(&server, "delcross1").await;
    let (_owner2_email, _owner2_id, org2_id) = register_owner(&server, "delcross2").await;
    let access1 = server.login(&owner1_email, PASSWORD).await;
    let role2_id = seed_role(&server, org2_id, &[perm::ISSUE_READ]).await;

    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org1_id}/roles/{role2_id}"), &access1)
        .await;
    assert_eq!(status, 404, "body: {body}");

    // Not deleted: still fetchable directly, and still shows up in its own
    // org's role list.
    let mut conn = server.conn().await;
    let still_there = repo::get_role(&mut conn, role2_id).await.expect("get_role");
    drop(conn);
    assert!(
        still_there.is_some(),
        "cross-org delete attempt removed the role anyway"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn deleting_the_same_role_twice_404s_the_second_time() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "deltwice").await;
    let access = server.login(&owner_email, PASSWORD).await;
    let role_id = seed_role(&server, org_id, &[perm::ISSUE_READ]).await;

    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org_id}/roles/{role_id}"), &access)
        .await;
    assert_eq!(status, 200, "first delete body: {body}");

    let (status, body) = server
        .delete_json(&format!("/v1/orgs/{org_id}/roles/{role_id}"), &access)
        .await;
    assert_eq!(status, 404, "second delete body: {body}");

    server.shutdown().await;
}

/// The anti-sabotage regression pin.
///
/// Deleting a role removes every permission it confers from every holder at
/// once, so it has to take the same guard the edit path takes: you may not
/// strip a permission you do not hold yourself. Both sibling handlers already
/// refuse the smaller version of this — `check_role_edit` (`guard.rs:78`)
/// refuses stripping `org:manage` from a role by editing it, and `delete_grant`
/// refuses deleting a single `org:manage` grant — so without guard 4 the DELETE
/// route would achieve in one call the strictly larger sabotage its two
/// siblings explicitly reject.
///
/// The caller here holds the **real shipped `Admin` preset**, not an imitation:
/// `Admin` has `role:manage` but not `org:manage` (`rbac.rs:129`), so this is
/// the live exploit path on a stock install with no custom setup at all.
///
/// The org:manage 409 guard cannot be what produces the 403 — the Owner's own
/// grant is a second source of `org:manage`, so
/// `count_org_manage_grants_excluding_role` returns non-zero and that guard
/// passes. The final Owner-succeeds step proves it: the same role, the same
/// request, a caller who *does* hold `org:manage` → 200. Guard 4 discriminates
/// by caller authority, it does not blanket-refuse org:manage-bearing roles.
#[tokio::test]
async fn an_admin_cannot_dissolve_a_role_conferring_a_permission_they_lack() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "delsabotage").await;
    let owner_access = server.login(&owner_email, PASSWORD).await;

    // A second role conferring org:manage, so the org has two sources of it
    // and the last-admin 409 guard is genuinely not the thing under test.
    let target_id = seed_role(&server, org_id, &[perm::ORG_MANAGE]).await;

    let admin_preset = preset_role_id(&server, "Admin").await;
    let (_admin_id, admin_access) =
        seed_sole_holder(&server, org_id, admin_preset, "stockadmin").await;

    // Sanity: the caller really does hold role:manage (so a 403 cannot be a
    // trivially-missing-permission result) and really does lack org:manage
    // (so the sabotage is genuinely out of their authority).
    let access_view = server
        .get_json(&format!("/v1/orgs/{org_id}/access"), &admin_access)
        .await;
    let held = access_view["permissions"].as_array().unwrap();
    assert!(
        held.iter().any(|p| p == &json!(perm::ROLE_MANAGE)),
        "Admin preset should hold role:manage: {access_view}"
    );
    assert!(
        !held.iter().any(|p| p == &json!(perm::ORG_MANAGE)),
        "Admin preset must NOT hold org:manage, or this test proves nothing: {access_view}"
    );

    let (status, body) = server
        .delete_json(
            &format!("/v1/orgs/{org_id}/roles/{target_id}"),
            &admin_access,
        )
        .await;
    assert_eq!(
        status, 403,
        "an Admin dissolved a role conferring org:manage — the sabotage \
         check_role_edit and delete_grant both refuse: {body}"
    );

    // Refused, not silently no-op'd.
    let after = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &admin_access)
        .await;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(target_id)),
        "role disappeared despite the 403: {after}"
    );

    // The discriminator: the Owner holds org:manage, so the same delete of the
    // same role succeeds. Proves the 403 was about caller authority and that
    // the 409 guard was never the cause.
    let (status, body) = server
        .delete_json(
            &format!("/v1/orgs/{org_id}/roles/{target_id}"),
            &owner_access,
        )
        .await;
    assert_eq!(
        status, 200,
        "the Owner must still be able to delete this role: {body}"
    );

    server.shutdown().await;
}

/// The primary authorization boundary.
///
/// Without this, substituting any other permission constant for
/// `perm::ROLE_MANAGE` at the handler's `authorize_org` call would leave every
/// other test in this file green.
#[tokio::test]
async fn a_caller_without_role_manage_cannot_delete() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "delnoperm").await;
    let owner_access = server.login(&owner_email, PASSWORD).await;
    let target_id = seed_role(&server, org_id, &[perm::ISSUE_READ]).await;

    // member:read lets them read the role list (so the "still there" assertion
    // is observable) but confers no role:manage.
    let weak_role = seed_role(&server, org_id, &[perm::MEMBER_READ]).await;
    let (_weak_id, weak_access) = seed_sole_holder(&server, org_id, weak_role, "readonly").await;

    let (status, body) = server
        .delete_json(
            &format!("/v1/orgs/{org_id}/roles/{target_id}"),
            &weak_access,
        )
        .await;
    assert_eq!(status, 403, "body: {body}");

    let after = server
        .get_json(&format!("/v1/orgs/{org_id}/roles"), &weak_access)
        .await;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(target_id)),
        "role disappeared despite the 403: {after}"
    );

    // The role really was deletable — by someone with the permission. Rules
    // out a false pass from the role being undeletable for some other reason.
    let (status, body) = server
        .delete_json(
            &format!("/v1/orgs/{org_id}/roles/{target_id}"),
            &owner_access,
        )
        .await;
    assert_eq!(status, 200, "owner delete body: {body}");

    server.shutdown().await;
}
