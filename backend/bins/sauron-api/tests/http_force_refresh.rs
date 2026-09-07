//! The server half of the global force refresh, against the real compiled
//! `sauron-api` binary.
//!
//! Four properties, each of which fails silently if it regresses:
//!
//!   * `?force=true` actually reaches `view_cache::read`'s force path and
//!     advances `computed_at` — asserted by observing the recompute, not by
//!     reading the source;
//!   * a second force inside the cooldown produces NO second recompute and
//!     answers 200 with the cached envelope, never 429 — a 429 would break a
//!     page render over a control the user pressed hopefully;
//!   * an HONOURED force clears the failure marker, and a DOWNGRADED one does
//!     not. The second is what keeps the cooldown bounding retries of a broken
//!     aggregate, and is the first thing a refactor loses;
//!   * `force` never reaches the cache key. A regression there is invisible
//!     except as "refresh is always slow", which reads as correct behaviour.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

#![allow(dead_code)]

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
const JWT_SECRET: &str = "http-force-refresh-test-secret-0000000000000";

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
    /// Per-server discriminator glued into every email address this file uses.
    /// See [`TestServer::addr`].
    tag: String,
}

impl TestServer {
    /// The ordinary fixture: a relay and a dashboard URL, so the mail path is
    /// exercised end to end.
    async fn start() -> Option<TestServer> {
        Self::start_with_mail(true).await
    }

    /// The deployment whose operator never configured SMTP. `state.mail` is
    /// `None` and `require_dashboard_url()` fails, which is the only way to
    /// reach the admin route's 503 and `forgot_password`'s swallow branch.
    async fn start_without_mail() -> Option<TestServer> {
        Self::start_with_mail(false).await
    }

    async fn start_with_mail(mail: bool) -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

        // Segment order is load-bearing — timestamp FIRST, discriminator glued
        // to the uuid. See the fuller account at the identical site in
        // `http_env_scoping.rs`: the reaper in `sauron-db`'s
        // `tests/common::reap_stale_test_databases` parses the first
        // underscore-delimited segment after `sauron_test_` as a timestamp and
        // silently skips anything else, so a "sauron_test_pr_<ts>_<uuid>"
        // spelling leaks every database it creates. Do not reorder.
        let db_name = format!(
            "sauron_test_{}_fr{}",
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
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("DATABASE_URL", &db_url)
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
            .kill_on_drop(true);
        if mail {
            // A relay that is guaranteed to refuse the connection, and
            // deliberately NOT `SMTP_SINK=1`. `MailSender::enqueue` ends in
            // `nudge()`, which spawns a drain immediately, so no
            // `MAIL_DRAIN_TICK_SECS` keeps a drain away from a row a handler has
            // just written. The sink "delivers" without opening a socket and
            // `mark_mail_sent` blanks `body_text` and `body_html` in the same
            // statement — and that column is the only place the raw token exists
            // for `newest_reset_token_from_mail` to read. A refused connect ends
            // in `mark_mail_failed`, which never touches the body.
            //
            // Port 1 is reserved and nothing listens on it, so the failure is an
            // immediate ECONNREFUSED rather than a timeout. `SMTP_TLS=none` is
            // accepted only for a host that resolves to loopback, which 127.0.0.1
            // does. `SMTP_FROM` is required the moment `SMTP_HOST` is set —
            // without it `require_smtp()` fails, `state.mail` is `None`, and this
            // fixture would silently become `start_without_mail`.
            cmd.env("SMTP_HOST", "127.0.0.1")
                .env("SMTP_PORT", "1")
                .env("SMTP_TLS", "none")
                .env("SMTP_FROM", "sauron@test.invalid")
                .env("DASHBOARD_URL", "https://dash.test")
                // Keeps the periodic drain from retrying every row every minute
                // for the length of the test. The nudge above is what actually
                // drains; this is noise control.
                .env("MAIL_DRAIN_TICK_SECS", "3600");
        }
        let mut child = cmd.spawn().expect("spawn sauron-api binary");

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
            tag: Uuid::new_v4().simple().to_string()[..8].to_string(),
        })
    }

    /// A per-run-unique address for `local`.
    ///
    /// The database is ephemeral but Redis is not: `forgot-password` spends
    /// `FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR = 3` against a key that is the
    /// address itself, in the *shared* `TEST_REDIS_URL`, over an hour-long
    /// window. A literal "happy@example.com" would therefore let this file run
    /// at most three times an hour, and the test that asks for two links in a
    /// row would fail on the second run — as a missing mail row, not as a 429.
    /// The db name discriminator is per-run for the same class of reason.
    fn addr(&self, local: &str) -> String {
        format!("{local}-{}@example.com", self.tag)
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

    async fn post_json(&self, path: &str, token: Option<&str>, body: Value) -> reqwest::Response {
        let mut req = self.client.post(format!("{}{path}", self.base)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"))
    }

    /// `(status, raw body text)`. The raw text matters: the anti-enumeration
    /// assertion is that two bodies are **byte-identical**, which a parsed
    /// `Value` comparison would not prove.
    async fn post_raw(&self, path: &str, token: Option<&str>, body: Value) -> (u16, String) {
        let resp = self.post_json(path, token, body).await;
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read body");
        (status, text)
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

/// Sign in over the real route. Returns `(access_token, refresh_token)`.
async fn login(srv: &TestServer, email: &str, password: &str) -> (String, String) {
    let (status, text) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": email, "password": password}),
        )
        .await;
    assert_eq!(status, 200, "login {email}: {text}");
    let v: Value = serde_json::from_str(&text).expect("login body is JSON");
    (
        v["access_token"]
            .as_str()
            .expect("access_token")
            .to_string(),
        v["refresh_token"]
            .as_str()
            .expect("refresh_token")
            .to_string(),
    )
}

/// Create an organization and its Owner, and sign them in. Returns
/// `(user_id, org_id, access_token, refresh_token)`.
///
/// Deliberately **not** `POST /v1/auth/register`, even though that is the route
/// a person uses. That route spends `REGISTER_ATTEMPTS_PER_HOUR = 10` keyed on
/// `sauron:auth:register:{client_addr}` — and `client_addr` is `127.0.0.1` for
/// every test in this file, in the *shared* `TEST_REDIS_URL`. This file needs
/// more owners than ten, so the eleventh call would 429; and because the window
/// is an hour, a second run inside the same hour would start already over
/// budget even if it needed fewer. `tests/http_workflows.rs` and
/// `tests/http_env_scoping.rs` build their fixtures out of `repo::create_user`
/// for exactly this reason. Login is safe to keep on the real route: its per-IP
/// budget is 60 per **60 seconds**, which self-heals.
///
/// This also gives every account exactly one org — an owner minted here holds no
/// grant anywhere else, which `guard_member_admin_action`'s unconditional
/// cross-org refusal requires of anything an admin test touches. Use
/// `create_member` for the targets.
async fn owner_of_new_org(
    srv: &TestServer,
    email: &str,
    password: &str,
) -> (Uuid, Uuid, String, String) {
    let (user_id, org_id) = {
        let mut conn = srv.conn().await;
        let hash = sauron_auth::hash_password_async(password.to_string())
            .await
            .expect("hash password");
        let user = repo::create_user(&mut conn, email, &hash, "Test Owner")
            .await
            .expect("create owner");
        let org = repo::create_org(
            &mut conn,
            &format!("Org {email}"),
            &format!("org-{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("create org");
        let owner_role = repo::get_system_role(&mut conn, "Owner")
            .await
            .expect("get Owner role")
            .expect("Owner preset role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: user.id,
                role_id: owner_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant Owner at org scope");
        (user.id, org.id)
    };
    let (access, refresh) = login(srv, email, password).await;
    (user_id, org_id, access, refresh)
}

/// Create a member who exists **only** in `org_id`, and return their user id.
///
/// `role_name` is matched against `repo::list_roles(conn, org_id)`, which
/// returns the four system presets plus this org's custom roles — so "Viewer",
/// "Admin" and a role the test just created all resolve here.
async fn create_member(
    srv: &TestServer,
    org_id: Uuid,
    email: &str,
    password: &str,
    role_name: &str,
) -> Uuid {
    let mut conn = srv.conn().await;
    let hash = sauron_auth::hash_password_async(password.to_string())
        .await
        .expect("hash password");
    let roles = repo::list_roles(&mut conn, org_id)
        .await
        .expect("list roles");
    let role = roles
        .iter()
        .find(|r| r.name == role_name)
        .unwrap_or_else(|| panic!("no role named {role_name} in this org"));
    let rows = repo::create_member_with_grants(
        &mut conn,
        email,
        &hash,
        "Test Member",
        org_id,
        role.id,
        &["org".to_string()],
        &[org_id],
    )
    .await
    .expect("create member");
    let user_id = rows[0].user_id;
    // That statement hardcodes `must_change_password = true`, which is
    // `create_member`'s reveal-once temp-password contract. Left set, every
    // access token this account gets is gated by `password_change_gate`, and the
    // guard-stack test's "caller has no permission" 403 would be
    // `password_change_required` rather than the RBAC refusal it claims to
    // prove — a green test asserting nothing.
    repo::set_user_must_change_password(&mut conn, user_id, false)
        .await
        .expect("clear the temp-password demand");
    user_id
}

/// A live connection to the same Redis the spawned server uses.
///
/// The failure-marker tests have to observe a key the API writes but never
/// exposes, so they read it directly rather than inferring it from behaviour.
async fn redis() -> sauron_redis::RedisStore {
    let url = std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL");
    sauron_redis::RedisStore::connect(&url)
        .await
        .expect("connect to test redis")
}

/// `GET /v1/admin/storage`, optionally forced. Returns `(status, parsed body)`.
///
/// Storage is the cleanest cached endpoint to drive: org-scoped, so it needs
/// only an Owner grant and no telemetry fixture, and it goes through exactly
/// the same `view_cache::read` + `honour_force` pair as every other one.
async fn storage(srv: &TestServer, token: &str, force: bool) -> (u16, Value) {
    let path = if force {
        "/v1/admin/storage?force=true"
    } else {
        "/v1/admin/storage"
    };
    let resp = srv.get(path, token).await;
    let status = resp.status().as_u16();
    let text = resp.text().await.expect("read body");
    let body: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("storage body is not JSON ({e}): {text}");
    });
    (status, body)
}

/// Poll until the envelope reports `fresh`, or give up. Returns `computed_at`.
///
/// Only valid for the FIRST computation, when there is nothing cached. After
/// that the entry stays fresh for five minutes, so this returns instantly with
/// the pre-existing value — use [`await_recompute`] to wait for a new one.
async fn await_first(srv: &TestServer, token: &str) -> String {
    for _ in 0..80 {
        let (_, body) = storage(srv, token, false).await;
        if body["state"] == "fresh" {
            return body["computed_at"]
                .as_str()
                .expect("computed_at")
                .to_string();
        }
        tokio::time::sleep(StdDuration::from_millis(250)).await;
    }
    panic!("storage never became fresh");
}

/// Poll until `computed_at` differs from `previous`. Returns the new value.
///
/// Waiting on `state == "fresh"` is NOT sufficient and was the bug in the first
/// version of this suite: `STORAGE_POLICY.fresh_for` is five minutes, so after
/// the first computation the entry is *already* fresh and a freshness poll
/// returns the pre-force value on its very first iteration — making a working
/// force look broken and a suppressed one look honoured. The observable that
/// actually means "a recompute landed" is the stamp changing.
async fn await_recompute(srv: &TestServer, token: &str, previous: &str) -> String {
    for _ in 0..80 {
        let (_, body) = storage(srv, token, false).await;
        let at = body["computed_at"].as_str().unwrap_or_default();
        if !at.is_empty() && at != previous {
            return at.to_string();
        }
        tokio::time::sleep(StdDuration::from_millis(250)).await;
    }
    panic!("storage never recomputed (computed_at stayed {previous})");
}

/// Long enough for a recompute to land if one was started at all.
///
/// The report is milliseconds on an empty ephemeral database; this is two
/// orders of magnitude of headroom, and it is only ever used to assert that
/// NOTHING happened.
const SETTLE: StdDuration = StdDuration::from_millis(1_500);

/// The `view_cache` key `/v1/admin/storage` uses for an owner of exactly one
/// org, in a database containing exactly one org.
///
/// Mirrors `routes::admin::storage`'s own `format!` verbatim:
/// `sauron:storage:v2:{deployment_org_count}:{hash of the sorted org id list}`.
/// The count is in the key because the report's `full_scope` flag — and with it
/// whether real database bytes are disclosed — flips when a second org appears.
///
/// Both `1`s hold because `owner_of_new_org` creates one org in a fresh
/// ephemeral database. An earlier version of this helper guessed the format and
/// watched a key nothing ever wrote, which made the marker assertions pass for
/// the wrong reason — the failure mode this comment exists to prevent.
fn storage_key(org_id: Uuid) -> String {
    format!(
        "sauron:storage:v2:1:{}",
        sauron_auth::hash_token(&org_id.to_string())
    )
}

#[tokio::test]
async fn a_force_recomputes_and_advances_computed_at() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, _org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    // Warm it, and wait for the first computation to land.
    storage(&srv, &token, false).await;
    let first_at = await_first(&srv, &token).await;

    // An unforced read inside the five-minute freshness window must NOT
    // recompute — the baseline the force is measured against.
    tokio::time::sleep(SETTLE).await;
    let (status, body) = storage(&srv, &token, false).await;
    assert_eq!(status, 200);
    assert_eq!(body["computed_at"].as_str().unwrap(), first_at);

    // The force does.
    let (status, _) = storage(&srv, &token, true).await;
    assert_eq!(
        status, 200,
        "a force answers immediately, it does not block"
    );
    let second_at = await_recompute(&srv, &token, &first_at).await;
    assert_ne!(second_at, first_at, "the force must actually recompute");

    srv.shutdown().await;
}

#[tokio::test]
async fn a_second_force_inside_the_cooldown_is_downgraded_not_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, _org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, false).await;
    let first_at = await_first(&srv, &token).await;

    let (s1, _) = storage(&srv, &token, true).await;
    assert_eq!(s1, 200);
    // Wait for the FIRST force's recompute to actually land before measuring
    // the second. Without this the two are indistinguishable.
    let at = await_recompute(&srv, &token, &first_at).await;

    // Second force, well inside FORCE_COOLDOWN_SECS.
    let (s2, body) = storage(&srv, &token, true).await;
    // 200 and NOT 429: the recompute the budget was spent on is already done,
    // so the honest answer is the cached envelope. A 429 would blank a page
    // over a control the user pressed hopefully.
    assert_eq!(s2, 200, "a downgraded force must never 429: {body}");
    assert_eq!(body["state"], "fresh");

    // And it must not have started a second recompute.
    tokio::time::sleep(SETTLE).await;
    let (_, after_second) = storage(&srv, &token, false).await;
    assert_eq!(
        after_second["computed_at"].as_str().unwrap(),
        at,
        "the cooldown must suppress the second recompute"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn an_honoured_force_clears_the_failure_marker() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, false).await;
    await_first(&srv, &token).await;

    // Stand in for "the last recompute failed". `view_cache::read` suppresses
    // re-enqueue under this marker EVEN WHEN FORCED, so without the clear a
    // user clicking Refresh on a broken section gets no attempt at all.
    let r = redis().await;
    let marker = format!("{}:fail", storage_key(org));
    r.set_ex(&marker, "boom", 300).await.expect("write marker");

    let (status, _) = storage(&srv, &token, true).await;
    assert_eq!(status, 200);

    assert!(
        r.get(&marker).await.expect("read marker").is_none(),
        "an honoured force must clear the failure marker"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn a_downgraded_force_leaves_the_failure_marker() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, false).await;
    await_first(&srv, &token).await;

    // Spend the budget.
    storage(&srv, &token, true).await;

    let r = redis().await;
    let marker = format!("{}:fail", storage_key(org));
    r.set_ex(&marker, "boom", 300).await.expect("write marker");

    // Second force is downgraded by the cooldown.
    let (status, _) = storage(&srv, &token, true).await;
    assert_eq!(status, 200);

    // THE test that keeps the cooldown meaningful. If a downgraded force also
    // cleared the marker, a permanently broken aggregate would be retried on
    // every click for as long as anyone kept pressing — which is exactly what
    // the marker exists to prevent, and exactly what a refactor loses first.
    assert!(
        r.get(&marker).await.expect("read marker").is_some(),
        "a force the cooldown refused must NOT clear the failure marker"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn force_does_not_reach_the_cache_key() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, true).await;
    await_first(&srv, &token).await;

    // The forced read's result must be readable at the ORDINARY key. If `force`
    // were folded into the key, Refresh would own a permanently-cold cache of
    // its own — and the only symptom would be that refreshing is always slow,
    // which reads as correct behaviour rather than as a bug.
    let r = redis().await;
    assert!(
        r.get(&storage_key(org)).await.expect("read key").is_some(),
        "a forced read must populate the same entry an unforced read reads"
    );

    // And behaviourally: an unforced read straight after sees it as fresh.
    let (_, body) = storage(&srv, &token, false).await;
    assert_eq!(body["state"], "fresh");

    srv.shutdown().await;
}

#[tokio::test]
async fn a_forced_read_of_fresh_data_still_reports_that_it_is_recomputing() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, _org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, false).await;
    await_first(&srv, &token).await;

    // The case the dashboard's Refresh button depends on, and the one a
    // freshness-only contract cannot express. `STORAGE_POLICY.fresh_for` is
    // five minutes, so the entry is still `fresh` when the force arrives — and
    // a client watching only `state` would conclude the refresh it just asked
    // for had already finished, stop its spinner, and keep showing the old
    // numbers until some later read happened to pick up the new ones.
    let (status, body) = storage(&srv, &token, true).await;
    assert_eq!(status, 200);
    assert_eq!(body["state"], "fresh", "the DATA is genuinely still fresh");
    assert_eq!(
        body["recomputing"], true,
        "a forced read must say fresher data is coming: {body}"
    );

    // An ordinary read inside the window starts nothing and says so.
    let (_, quiet) = storage(&srv, &token, false).await;
    assert_eq!(quiet["recomputing"], false);

    srv.shutdown().await;
}

#[tokio::test]
async fn an_unforced_read_never_spends_the_cooldown() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_uid, _org, token, _r) =
        owner_of_new_org(&srv, &srv.addr("owner"), "correct-horse-1").await;

    storage(&srv, &token, false).await;
    let at = await_first(&srv, &token).await;

    // Several ordinary reads, then a force. If ordinary reads spent the budget,
    // the force would be downgraded and the page would silently never refresh.
    for _ in 0..3 {
        storage(&srv, &token, false).await;
    }
    tokio::time::sleep(SETTLE).await;
    let (_, unchanged) = storage(&srv, &token, false).await;
    assert_eq!(
        unchanged["computed_at"].as_str().unwrap(),
        at,
        "an unforced read must not recompute inside the freshness window"
    );

    storage(&srv, &token, true).await;
    let after = await_recompute(&srv, &token, &at).await;
    assert_ne!(after, at, "only an explicit force may spend the cooldown");

    srv.shutdown().await;
}
