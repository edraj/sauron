//! End-to-end password reset against the real compiled `sauron-api` binary:
//! the two unauthenticated routes (`forgot-password`, `reset-password`), the
//! org-admin forced reset, and the properties no unit test can observe — that
//! every account state answers `forgot-password` byte-identically, that every
//! dead-token state answers `reset-password` byte-identically, that two
//! simultaneous uses of one link yield exactly one success, and that an
//! admin-forced reset stops the old password at the login form rather than
//! merely gating the session it would have issued.
//!
//! Spawns the actual binary against an ephemeral, migrated database — same
//! harness shape as `tests/http_workflows.rs` and `tests/http_sessions.rs`
//! (duplicated rather than shared; see `tests/http_env_scoping.rs`'s
//! `TestServer`/`swap_database` doc comments for why a cross-test-binary
//! dependency isn't worth it for machinery this small).
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::{Duration as ChronoDuration, Utc};
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-password-reset-test-secret-00000000000";

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
            "sauron_test_{}_pr{}",
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

/// The raw token for a user's newest reset row.
///
/// The test has DB access, so nothing needs to be logged for it — but the raw
/// token exists only in the email, so it is read out of the `mail_outbox` body
/// rather than out of `password_reset_tokens`, which stores only the hash.
/// `body_text` survives because this fixture's relay refuses every connection;
/// see `start_with_mail`.
async fn newest_reset_token_from_mail(srv: &TestServer, email: &str) -> String {
    #[derive(diesel::QueryableByName)]
    struct BodyRow {
        #[diesel(sql_type = Text)]
        body_text: String,
    }
    let mut conn = srv.conn().await;
    let row: BodyRow = diesel::sql_query(
        "SELECT body_text FROM mail_outbox WHERE recipient = $1 AND kind = 'password_reset' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind::<Text, _>(email)
    .get_result(&mut conn)
    .await
    .expect("a password_reset row in mail_outbox");
    let marker = "?token=";
    let start = row
        .body_text
        .find(marker)
        .expect("a reset link in the body")
        + marker.len();
    row.body_text[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect()
}

/// Let the *next* `password_reset` mail to `email` through S0's dedup window.
///
/// `MailKind::PasswordReset.dedup_window()` is 300 seconds, and the probe inside
/// `repo::enqueue_mail` matches `(kind, recipient_key)` over rows newer than
/// that **whose status is not 'failed'**. Two reset mails to one address inside
/// one test run would otherwise suppress the second silently: `enqueue` returns
/// `Ok(None)` and neither the handler nor the test can tell that apart from a
/// send. Flipping the existing rows to 'failed' uses S0's own carve-out for a
/// legitimate retry, and unlike a DELETE it leaves them countable.
async fn unblock_reset_mail(srv: &TestServer, email: &str) {
    let mut conn = srv.conn().await;
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'failed' \
         WHERE recipient_key = $1 AND kind = 'password_reset'",
    )
    .bind::<Text, _>(email.to_lowercase())
    .execute(&mut conn)
    .await
    .expect("release the dedup window");
}

#[tokio::test]
async fn forgot_password_answers_identically_for_every_account_state() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let live = srv.addr("live");
    let dead = srv.addr("dead");
    let ghost = srv.addr("ghost");
    owner_of_new_org(&srv, &live, "correcthorse1").await;
    let (dead_id, _, _, _) = owner_of_new_org(&srv, &dead, "correcthorse2").await;
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, dead_id, false)
            .await
            .expect("deactivate");
    }

    let (s1, b1) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email": live}))
        .await;
    let (s2, b2) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email": dead}))
        .await;
    let (s3, b3) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email": ghost}))
        .await;
    assert_eq!((s1, s2, s3), (200, 200, 200));
    // Byte-identical, not merely equivalent. This is the whole contract.
    assert_eq!(b1, b2);
    assert_eq!(b2, b3);
    assert_eq!(b1, r#"{"ok":true}"#);

    let (s4, _) = srv
        .post_raw(
            "/v1/auth/forgot-password",
            None,
            json!({"email":"no-at-sign"}),
        )
        .await;
    assert_eq!(s4, 400, "shape validation may differ; it leaks nothing");

    // The discard branch commits nothing: an unknown address writes zero rows
    // to either table.
    let mut conn = srv.conn().await;
    let ghost_mail: i64 =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM mail_outbox WHERE recipient = $1")
            .bind::<Text, _>(&ghost)
            .get_result::<CountRow>(&mut conn)
            .await
            .expect("count")
            .n;
    assert_eq!(ghost_mail, 0);
    drop(conn);

    srv.shutdown().await;
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

#[tokio::test]
async fn every_dead_token_state_answers_identically() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let email = srv.addr("dead-token");
    let (user_id, _, _, _) = owner_of_new_org(&srv, &email, "correcthorse1").await;

    let mut bodies: Vec<(u16, String)> = Vec::new();

    // 1. Never existed. Generated, not a fixed literal: the per-token limiter is
    // keyed on the token's hash over a one-hour tumbling window, so a constant
    // token puts every run of this test into one shared bucket and the tenth run
    // within the hour fails with a 429 that reads like a product bug. A freshly
    // generated token is never inserted into `password_reset_tokens`, so it is
    // still "never existed", and it is 64 hex digits, so it clears
    // `is_reset_token_shape` and reaches the lookup this case is about.
    let never_issued = sauron_core::ids::opaque_token();
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": never_issued, "new_password": "brandnewpass1"}),
        )
        .await,
    );

    // 2. Consumed. Mint, use once, use again.
    srv.post_raw("/v1/auth/forgot-password", None, json!({"email": email}))
        .await;
    let t2 = newest_reset_token_from_mail(&srv, &email).await;
    let (ok, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": t2, "new_password": "brandnewpass1"}),
        )
        .await;
    assert_eq!(ok, 200);
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": t2, "new_password": "brandnewpass2"}),
        )
        .await,
    );

    // 3. Invalidated.
    let raw3 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw3),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() + ChronoDuration::hours(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
        repo::invalidate_password_reset_tokens_for_user(&mut conn, user_id, "superseded")
            .await
            .expect("invalidate");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw3, "new_password": "brandnewpass3"}),
        )
        .await,
    );

    // 4. Expired.
    let raw4 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw4),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() - ChronoDuration::minutes(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw4, "new_password": "brandnewpass4"}),
        )
        .await,
    );

    // 5. Stale fingerprint — the one that proves the column earns its keep.
    let raw5 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw5),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() + ChronoDuration::hours(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
        let other = sauron_auth::hash_password_async("adifferentpass1".to_string())
            .await
            .expect("hash");
        repo::set_user_password(&mut conn, user_id, &other)
            .await
            .expect("move the password out from under the link");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw5, "new_password": "brandnewpass5"}),
        )
        .await,
    );

    for (i, (status, body)) in bodies.iter().enumerate() {
        assert_eq!(*status, 401, "dead-token case {i}: {body}");
        assert_eq!(
            body, &bodies[0].1,
            "dead-token case {i} must be byte-identical to case 0"
        );
    }
    assert!(bodies[0].1.contains("invalid_token"));

    // The other half of the compare-and-swap contract, and the half a 401 alone
    // does not prove: the third password — the one that moved under case 5's
    // link — is still the account's. An implementation that consumed the link
    // and wrote anyway would 401 here just the same and be silently wrong.
    let (s_third, b_third) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": email, "password":"adifferentpass1"}),
        )
        .await;
    assert_eq!(
        s_third, 200,
        "the password set out from under the link stands: {b_third}"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn a_consumed_link_sets_the_password_and_kills_every_session() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let email = srv.addr("happy");
    let (_id, _org, _access, refresh) = owner_of_new_org(&srv, &email, "correcthorse1").await;

    srv.post_raw("/v1/auth/forgot-password", None, json!({"email": email}))
        .await;
    let token = newest_reset_token_from_mail(&srv, &email).await;

    let (status, body) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "thebrandnewone1"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // No auto-login: the caller proved control of a mailbox, not of a credential.
    assert_eq!(body, r#"{"ok":true}"#);
    assert!(!body.contains("access_token") && !body.contains("refresh_token"));

    let (s_new, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": email, "password":"thebrandnewone1"}),
        )
        .await;
    assert_eq!(s_new, 200);
    let (s_old, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": email, "password":"correcthorse1"}),
        )
        .await;
    assert_eq!(s_old, 401);
    let (s_refresh, _) = srv
        .post_raw("/v1/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(
        s_refresh, 401,
        "a refresh token captured before the reset must be dead"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn two_simultaneous_resets_with_one_token_yield_exactly_one_success() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let email = srv.addr("race");
    owner_of_new_org(&srv, &email, "correcthorse1").await;
    srv.post_raw("/v1/auth/forgot-password", None, json!({"email": email}))
        .await;
    let token = newest_reset_token_from_mail(&srv, &email).await;

    // A SELECT-then-UPDATE implementation fails this.
    let (a, b) = tokio::join!(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "racewinnerpass1"}),
        ),
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "racewinnerpass2"}),
        )
    );
    let mut codes = [a.0, b.0];
    codes.sort_unstable();
    assert_eq!(codes, [200, 401], "got {a:?} and {b:?}");

    srv.shutdown().await;
}

#[tokio::test]
async fn resetting_to_the_current_password_is_400_and_does_not_burn_the_link() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let email = srv.addr("reuse");
    owner_of_new_org(&srv, &email, "correcthorse1").await;
    srv.post_raw("/v1/auth/forgot-password", None, json!({"email": email}))
        .await;
    let token = newest_reset_token_from_mail(&srv, &email).await;

    let (s1, b1) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "correcthorse1"}),
        )
        .await;
    assert_eq!(s1, 400, "{b1}");
    assert!(b1.contains("must be different from the current one"));

    // The same token still works with a different password.
    let (s2, b2) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "somethingelse1"}),
        )
        .await;
    assert_eq!(s2, 200, "{b2}");

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_reset_stops_the_old_password_at_the_login_form() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner");
    let target_email = srv.addr("target");
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let target_id = create_member(&srv, org_id, &target_email, "correcthorse2", "Viewer").await;

    // A live session, so the revocation below has something to kill. The target
    // is created straight in the database, so this is the only place a refresh
    // token for them comes from.
    let (_target_access, target_refresh) = login(&srv, &target_email, "correcthorse2").await;

    let (status, body) = srv
        .post_raw(
            &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"action\":\"reset\""));
    // The response must never carry the token or the link.
    assert!(!body.contains("token"));

    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.must_change_password);
        assert!(u.credentials_invalidated_at.is_some());
    }

    let (s_refresh, _) = srv
        .post_raw(
            "/v1/auth/refresh",
            None,
            json!({"refresh_token": target_refresh}),
        )
        .await;
    assert_eq!(s_refresh, 401);

    // THE assertion. An implementation that merely gates the session passes
    // every other line in this file.
    let (s_login, b_login) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": target_email, "password":"correcthorse2"}),
        )
        .await;
    assert_eq!(s_login, 403, "{b_login}");
    assert!(b_login.contains("password_reset_required"));
    assert!(!b_login.contains("access_token"));

    // The emailed link clears both the flag and the invalidation in one write.
    let token = newest_reset_token_from_mail(&srv, &target_email).await;
    let (s_reset, b_reset) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "chosenbyme1234"}),
        )
        .await;
    assert_eq!(s_reset, 200, "{b_reset}");
    let (s_after, b_after) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": target_email, "password":"chosenbyme1234"}),
        )
        .await;
    assert_eq!(s_after, 200, "{b_after}");
    let v: Value = serde_json::from_str(&b_after).unwrap();
    let access = v["access_token"].as_str().unwrap();
    assert_eq!(
        srv.get_status("/v1/me", access).await,
        200,
        "the reset must have cleared must_change_password too"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_cancel_restores_login_but_keeps_the_change_demand() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner2");
    let target_email = srv.addr("target2");
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let target_id = create_member(&srv, org_id, &target_email, "correcthorse2", "Viewer").await;

    srv.post_raw(
        &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
        Some(&owner_token),
        json!({"action":"reset"}),
    )
    .await;
    let stale = newest_reset_token_from_mail(&srv, &target_email).await;

    let (status, body) = srv
        .post_raw(
            &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
            Some(&owner_token),
            json!({"action":"cancel"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"expires_at\":null"));

    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.credentials_invalidated_at.is_none());
        // Cancel does not pretend the admin never had a reason, and it cannot
        // tell this flag apart from one a temp password set long before.
        assert!(u.must_change_password);
    }

    let (s_login, b_login) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": target_email, "password":"correcthorse2"}),
        )
        .await;
    assert_eq!(s_login, 200, "{b_login}");
    let v: Value = serde_json::from_str(&b_login).unwrap();
    assert_eq!(
        srv.get_status("/v1/me", v["access_token"].as_str().unwrap())
            .await,
        403,
        "the change demand survives a cancel"
    );

    // The link the cancelled reset issued is dead.
    let (s_stale, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": stale, "new_password": "shouldnotwork1"}),
        )
        .await;
    assert_eq!(s_stale, 401);

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_guard_stack_refuses_each_case_distinctly() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner3");
    let member_email = srv.addr("member3");
    let outsider_email = srv.addr("outsider");
    let manager_email = srv.addr("manager3");
    let admin_email = srv.addr("admin3");
    let owner_b_email = srv.addr("ownerb");
    let (owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let member_id = create_member(&srv, org_id, &member_email, "correcthorse2", "Viewer").await;
    let (outsider_id, _o2, _a2, _r2) =
        owner_of_new_org(&srv, &outsider_email, "correcthorse3").await;

    let path = |uid: Uuid| format!("/v1/orgs/{org_id}/members/{uid}/password-reset");

    // Self-target.
    let (s, b) = srv
        .post_raw(
            &path(owner_id),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 409, "{b}");

    // Unknown user id, and a user with no grant here — deliberately
    // indistinguishable. `outsider` owns their own org and holds no grant in
    // this one, which is exactly the shape the membership check refuses with 404
    // *before* the cross-org rule is reached.
    let (s, _) = srv
        .post_raw(
            &path(Uuid::new_v4()),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 404);
    let (s, _) = srv
        .post_raw(
            &path(outsider_id),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 404);

    // A caller with `member:read` only.
    let (member_token, _) = login(&srv, &member_email, "correcthorse2").await;
    let (s, _) = srv
        .post_raw(
            &path(owner_id),
            Some(&member_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 403);

    // A caller holding `member:manage` but NOT `member:credential`. This is the
    // assertion that proves the route moved to the new permission rather than
    // merely mentioning it: delete `authorize_org(..., perm::MEMBER_CREDENTIAL)`
    // from the handler and every other line in this test still passes.
    {
        let mut conn = srv.conn().await;
        repo::create_role(
            &mut conn,
            org_id,
            "Member manager",
            "member:manage without member:credential",
            json!([perm::MEMBER_READ, perm::MEMBER_MANAGE]),
        )
        .await
        .expect("create the carve-out role");
    }
    create_member(
        &srv,
        org_id,
        &manager_email,
        "correcthorse5",
        "Member manager",
    )
    .await;
    let (manager_token, _) = login(&srv, &manager_email, "correcthorse5").await;
    let (s, b) = srv
        .post_raw(
            &path(member_id),
            Some(&manager_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(
        s, 403,
        "member:manage must not stand in for member:credential: {b}"
    );

    // An Admin acting on an Owner. Admin holds `member:credential` and
    // `member:manage`, so it clears the route's own gate and dies inside
    // `check_no_escalation` on the target's `org:manage` — the rule that stops
    // an Admin working through every Owner in turn.
    create_member(&srv, org_id, &admin_email, "correcthorse6", "Admin").await;
    let (admin_token, _) = login(&srv, &admin_email, "correcthorse6").await;
    let (s, b) = srv
        .post_raw(
            &path(owner_id),
            Some(&admin_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 403, "an Admin may not reset an Owner: {b}");

    // Inactive target.
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, member_id, false)
            .await
            .unwrap();
    }
    let (s, b) = srv
        .post_raw(
            &path(member_id),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(b.contains("reactivate this member"), "{b}");
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, member_id, true)
            .await
            .unwrap();
    }

    // Cross-org target, for BOTH actions. Last, because it is the only case that
    // has to mutate `member3` irreversibly for the rest of the file's sake: one
    // extra grant in a second org and the blanket refusal fires. `cancel` is
    // exempt from the SMTP precondition but not from this — it is a blast-radius
    // boundary, not a mail concern.
    let (_ob_id, org_b, _ob_token, _) =
        owner_of_new_org(&srv, &owner_b_email, "correcthorse4").await;
    grant_org_member(&srv, org_b, member_id).await;
    for action in ["reset", "cancel"] {
        let (s, b) = srv
            .post_raw(
                &path(member_id),
                Some(&owner_token),
                json!({"action": action}),
            )
            .await;
        assert_eq!(s, 409, "cross-org {action}: {b}");
        assert!(
            b.contains("another organization"),
            "cross-org {action}: {b}"
        );
    }

    srv.shutdown().await;
}

/// Give `user_id` a Viewer grant at org scope in `org_id`.
///
/// The only caller manufactures a **cross-org** target: everything else in this
/// file uses `create_member`, which creates the account and its single-org grant
/// in one statement. `repo::create_grants` takes an owned `Vec` and returns one
/// id per row.
async fn grant_org_member(srv: &TestServer, org_id: Uuid, user_id: Uuid) {
    let mut conn = srv.conn().await;
    let roles = repo::list_roles(&mut conn, org_id)
        .await
        .expect("list roles");
    let viewer = roles
        .iter()
        .find(|r| r.name == "Viewer")
        .expect("Viewer preset role");
    let ids = repo::create_grants(
        &mut conn,
        vec![NewRoleGrant {
            org_id,
            user_id,
            role_id: viewer.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        }],
    )
    .await
    .expect("grant");
    assert_eq!(ids.len(), 1, "create_grants returns one id per row");
}

#[tokio::test]
async fn each_mode_enqueues_one_mail_row_whose_expiry_matches_its_token() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner4");
    let target_email = srv.addr("target4");
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let target_id = create_member(&srv, org_id, &target_email, "correcthorse2", "Viewer").await;

    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email": target_email}),
    )
    .await;
    let self_token = newest_reset_token_from_mail(&srv, &target_email).await;
    assert_eq!(self_token.len(), 64);

    // Without this the admin message below is suppressed by S0's 300-second
    // per-recipient window and the counts underneath assert nothing.
    unblock_reset_mail(&srv, &target_email).await;

    srv.post_raw(
        &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
        Some(&owner_token),
        json!({"action":"reset"}),
    )
    .await;

    let mut conn = srv.conn().await;
    let rows: i64 = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM mail_outbox \
         WHERE recipient = $1 AND kind = 'password_reset'",
    )
    .bind::<Text, _>(&target_email)
    .get_result::<CountRow>(&mut conn)
    .await
    .unwrap()
    .n;
    assert_eq!(rows, 2, "one row per mode, no more");

    // The two clocks are tied: the message and the link it carries must die
    // together, or S0's manual-requeue path blanks a body whose token is still
    // good for another twenty-three hours.
    let spans: Vec<i64> = diesel::sql_query(
        "SELECT round(extract(epoch FROM (m.expires_at - m.created_at)))::bigint AS n \
         FROM mail_outbox m WHERE m.recipient = $1 \
           AND m.kind = 'password_reset' ORDER BY m.created_at",
    )
    .bind::<Text, _>(&target_email)
    .load::<CountRow>(&mut conn)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.n)
    .collect();
    assert_eq!(spans, vec![3600, 86400]);
    drop(conn);

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_resets_supersede_each_other_and_self_service_requests_do_not() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner5");
    let target_email = srv.addr("target5");
    let selfsup_email = srv.addr("selfsup");
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let target_id = create_member(&srv, org_id, &target_email, "correcthorse2", "Viewer").await;
    let admin_path = format!("/v1/orgs/{org_id}/members/{target_id}/password-reset");

    // An admin trigger is an authoritative act by an identified principal, so
    // the second reset supersedes the first: a re-issue after a bounce must not
    // leave two live links.
    let (s1, b1) = srv
        .post_raw(&admin_path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s1, 200, "{b1}");
    let first = newest_reset_token_from_mail(&srv, &target_email).await;
    unblock_reset_mail(&srv, &target_email).await;
    let (s2, b2) = srv
        .post_raw(&admin_path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s2, 200, "{b2}");
    let second = newest_reset_token_from_mail(&srv, &target_email).await;
    assert_ne!(first, second, "the second reset must mint a new link");

    let (s_first, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": first, "new_password": "supersededpass1"}),
        )
        .await;
    assert_eq!(s_first, 401, "the superseded link must be dead");
    let (s_second, b_second) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": second, "new_password": "thesecondlink1"}),
        )
        .await;
    assert_eq!(s_second, 200, "{b_second}");

    // Self-service is the opposite rule, deliberately: an attacker spamming
    // forgot-password against a known address would otherwise kill the link the
    // victim is about to click, turning the anti-abuse limiter into the abuse.
    owner_of_new_org(&srv, &selfsup_email, "correcthorse3").await;
    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email": selfsup_email}),
    )
    .await;
    let link_a = newest_reset_token_from_mail(&srv, &selfsup_email).await;
    unblock_reset_mail(&srv, &selfsup_email).await;
    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email": selfsup_email}),
    )
    .await;
    let link_b = newest_reset_token_from_mail(&srv, &selfsup_email).await;
    assert_ne!(link_a, link_b);
    {
        let mut conn = srv.conn().await;
        for raw in [&link_a, &link_b] {
            assert!(
                repo::find_live_password_reset_token(&mut conn, &sauron_auth::hash_token(raw))
                    .await
                    .expect("lookup")
                    .is_some(),
                "a self-service request must not invalidate an outstanding link"
            );
        }
    }

    // Consuming one kills the other — via the sibling sweep and, independently,
    // via the fingerprint.
    let (s_a, b_a) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": link_a, "new_password": "thefirstlink12"}),
        )
        .await;
    assert_eq!(s_a, 200, "{b_a}");
    let (s_b, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": link_b, "new_password": "thesecondlink2"}),
        )
        .await;
    assert_eq!(
        s_b, 401,
        "consuming one self-service link kills its siblings"
    );

    srv.shutdown().await;
}

/// `FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR` caps SENDS. It must never deny.
///
/// It used to answer 429, which made three anonymous requests a one-hour denial
/// of self-service reset against any address the caller could name, with no way
/// for an administrator to shorten it. The cap now suppresses only *redundant*
/// mail — a request past the budget still answers 200, and still mails, unless a
/// live self-service link is already outstanding.
///
/// Three properties, and the middle one is the one a naive implementation gets
/// wrong: the suppressed request must also mint NO token, or every spam request
/// refreshes the "already holds a live link" answer and the suppression latches
/// past the attack.
#[tokio::test]
async fn the_per_email_reset_budget_caps_mail_without_ever_locking_the_account_out() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let email = srv.addr("send-cap");
    let (user_id, _, _, _) = owner_of_new_org(&srv, &email, "correcthorse1").await;

    async fn mails_sent(srv: &TestServer, email: &str) -> i64 {
        let mut conn = srv.conn().await;
        let row: CountRow = diesel::sql_query(
            "SELECT COUNT(*) AS n FROM mail_outbox \
             WHERE recipient_key = $1 AND kind = 'password_reset'",
        )
        .bind::<Text, _>(email.to_lowercase())
        .get_result(&mut conn)
        .await
        .expect("count reset mail");
        row.n
    }
    async fn live_tokens(srv: &TestServer, user_id: Uuid) -> i64 {
        let mut conn = srv.conn().await;
        let row: CountRow = diesel::sql_query(
            "SELECT COUNT(*) AS n FROM password_reset_tokens \
             WHERE user_id = $1 AND mode = 'self' AND consumed_at IS NULL \
               AND invalidated_at IS NULL AND expires_at > now()",
        )
        .bind::<SqlUuid, _>(user_id)
        .get_result(&mut conn)
        .await
        .expect("count live tokens");
        row.n
    }
    async fn ask(srv: &TestServer, email: &str) -> (u16, String) {
        srv.post_raw("/v1/auth/forgot-password", None, json!({"email": email}))
            .await
    }

    // Burn the budget. Each in-budget request mails, so the dedup window has to
    // be released between them or S0 suppresses the 2nd and 3rd for an unrelated
    // reason and the counts below stop meaning anything.
    for i in 0..3 {
        let (s, b) = ask(&srv, &email).await;
        assert_eq!(s, 200, "in-budget request {i} was refused: {b}");
        unblock_reset_mail(&srv, &email).await;
    }
    assert_eq!(
        mails_sent(&srv, &email).await,
        3,
        "each in-budget request should have mailed exactly once"
    );
    let live_before = live_tokens(&srv, user_id).await;
    assert!(live_before > 0, "the in-budget requests left a live link");

    // Past the budget, with a live link outstanding. 200, and nothing new —
    // note the dedup window was just released, so suppression here is this
    // feature's doing and not S0's.
    let (s4, b4) = ask(&srv, &email).await;
    assert_eq!(s4, 200, "past the send cap must not be a 429: {b4}");
    assert_eq!(
        mails_sent(&srv, &email).await,
        3,
        "a duplicate link was mailed while a live one was outstanding"
    );
    assert_eq!(
        live_tokens(&srv, user_id).await,
        live_before,
        "the suppressed request minted a token, which latches the suppression \
         for an hour after the attacker stops"
    );

    // Same budget state, but every outstanding link has now expired — which is
    // the victim's position once the attacker's mail goes stale. Denying here is
    // the lockout this whole test exists to prevent.
    {
        let mut conn = srv.conn().await;
        diesel::sql_query(
            "UPDATE password_reset_tokens SET expires_at = now() - interval '1 hour' \
             WHERE user_id = $1",
        )
        .bind::<SqlUuid, _>(user_id)
        .execute(&mut conn)
        .await
        .expect("expire the outstanding links");
    }
    assert_eq!(live_tokens(&srv, user_id).await, 0, "links are stale now");

    let (s5, b5) = ask(&srv, &email).await;
    assert_eq!(s5, 200, "{b5}");
    assert_eq!(
        mails_sent(&srv, &email).await,
        4,
        "past the cap with no live link, the caller must still get a link — \
         this is the lockout, not the flood"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn with_no_mail_configured_reset_refuses_and_cancel_still_works() {
    let Some(mut srv) = TestServer::start_without_mail().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let owner_email = srv.addr("owner6");
    let target_email = srv.addr("target6");
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, &owner_email, "correcthorse1").await;
    let target_id = create_member(&srv, org_id, &target_email, "correcthorse2", "Viewer").await;
    let path = format!("/v1/orgs/{org_id}/members/{target_id}/password-reset");

    let (s_reset, b_reset) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s_reset, 503, "{b_reset}");
    assert!(b_reset.contains("unavailable"), "{b_reset}");

    // Nothing applied. The 503 sits above every write for exactly this reason: a
    // destructive change must never land when the message carrying its remedy
    // cannot be sent.
    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.credentials_invalidated_at.is_none());
        assert!(!u.must_change_password);
    }

    // Cancel is exempt. This is the assertion that stops the 503 check being
    // hoisted above the action parse in a tidy-up — gating the undo on the
    // configuration that motivates it makes it unreachable in precisely the
    // deployment that needs it.
    let (s_cancel, b_cancel) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"cancel"}))
        .await;
    assert_eq!(s_cancel, 200, "{b_cancel}");
    assert!(b_cancel.contains("\"action\":\"cancel\""), "{b_cancel}");

    // And a bad action is still a 400 here, not a 503: the parse runs first.
    let (s_bad, b_bad) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"nonsense"}))
        .await;
    assert_eq!(s_bad, 400, "{b_bad}");

    // forgot-password keeps its generic 200 on this deployment and writes
    // nothing. A status that flips with deployment configuration is a
    // config-state oracle handed to an anonymous caller.
    let (s_forgot, b_forgot) = srv
        .post_raw(
            "/v1/auth/forgot-password",
            None,
            json!({"email": target_email}),
        )
        .await;
    assert_eq!(s_forgot, 200, "{b_forgot}");
    assert_eq!(b_forgot, r#"{"ok":true}"#);
    {
        let mut conn = srv.conn().await;
        let queued: i64 = diesel::sql_query(
            "SELECT count(*)::bigint AS n FROM mail_outbox WHERE kind = 'password_reset'",
        )
        .get_result::<CountRow>(&mut conn)
        .await
        .expect("count")
        .n;
        assert_eq!(queued, 0, "an unconfigured deployment must enqueue nothing");
    }

    srv.shutdown().await;
}
