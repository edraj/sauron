//! End-to-end admin-initiated email change against the real compiled
//! `sauron-api` binary: the two authenticated org routes, the three
//! unauthenticated token routes, and the properties no unit test can observe.
//!
//! The ones worth naming, because each is a security property rather than a
//! behaviour:
//!
//!   * the mail sent to the OLD address never names the new one — asserted
//!     against the queued body, not the template;
//!   * the old address's link really does kill the new address's link;
//!   * a second request leaves exactly one live approve link;
//!   * confirming moves the login identity, leaves sessions alive (a deliberate
//!     choice, so a test guards it against a well-meaning "fix"), and kills
//!     outstanding password-reset links (which the address change would
//!     otherwise strand as a live takeover path);
//!   * two simultaneous confirmations of one link yield exactly one success.
//!
//! Spawns the actual binary against an ephemeral, migrated database — same
//! harness shape as `tests/http_password_reset.rs`, duplicated rather than
//! shared for the reason `tests/http_env_scoping.rs` documents.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-email-change-test-secret-000000000000";

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
            "sauron_test_{}_ec{}",
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

/// The raw token out of the newest mail of `kind` sent to `email`.
///
/// The raw token exists ONLY in the email — the table stores a SHA-256 — so it
/// is read out of the `mail_outbox` body. `body_text` survives because this
/// fixture's relay refuses every connection, so the row ends in
/// `mark_mail_failed`, which never blanks the body (`mark_mail_sent` does).
async fn token_from_mail(srv: &TestServer, email: &str, kind: &str) -> String {
    let body = mail_body(srv, email, kind).await;
    let marker = "?token=";
    let start = body
        .find(marker)
        .unwrap_or_else(|| panic!("no link in the newest {kind} mail to {email}:\n{body}"))
        + marker.len();
    body[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect()
}

/// The rendered body of the newest mail of `kind` to `email`.
async fn mail_body(srv: &TestServer, email: &str, kind: &str) -> String {
    #[derive(diesel::QueryableByName)]
    struct BodyRow {
        #[diesel(sql_type = Text)]
        body_text: String,
    }
    let mut conn = srv.conn().await;
    let row: BodyRow = diesel::sql_query(
        "SELECT body_text FROM mail_outbox WHERE recipient_key = $1 AND kind = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind::<Text, _>(email.to_lowercase())
    .bind::<Text, _>(kind)
    .get_result(&mut conn)
    .await
    .unwrap_or_else(|e| panic!("a {kind} row in mail_outbox for {email}: {e}"));
    row.body_text
}

async fn count_mail(srv: &TestServer, email: &str, kind: &str) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct CountRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let mut conn = srv.conn().await;
    let row: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM mail_outbox WHERE recipient_key = $1 AND kind = $2",
    )
    .bind::<Text, _>(email.to_lowercase())
    .bind::<Text, _>(kind)
    .get_result(&mut conn)
    .await
    .expect("count mail");
    row.n
}

async fn count_requests(srv: &TestServer) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct CountRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let mut conn = srv.conn().await;
    let row: CountRow = diesel::sql_query("SELECT count(*) AS n FROM email_change_requests")
        .get_result(&mut conn)
        .await
        .expect("count requests");
    row.n
}

async fn current_email(srv: &TestServer, user_id: Uuid) -> String {
    let mut conn = srv.conn().await;
    repo::get_user(&mut conn, user_id)
        .await
        .expect("get user")
        .expect("user exists")
        .email
}

/// Open a change request over the real route. Returns `(status, body text)`.
async fn request_change(
    srv: &TestServer,
    org_id: Uuid,
    user_id: Uuid,
    token: &str,
    new_email: &str,
) -> (u16, String) {
    srv.post_raw(
        &format!("/v1/orgs/{org_id}/members/{user_id}/email-change"),
        Some(token),
        json!({ "new_email": new_email }),
    )
    .await
}

async fn confirm(srv: &TestServer, token: &str) -> (u16, String) {
    srv.post_raw(
        "/v1/auth/email-change/confirm",
        None,
        json!({"token": token}),
    )
    .await
}

async fn cancel(srv: &TestServer, token: &str) -> (u16, String) {
    srv.post_raw(
        "/v1/auth/email-change/cancel",
        None,
        json!({"token": token}),
    )
    .await
}

async fn preview(srv: &TestServer, token: &str) -> (u16, String) {
    srv.post_raw(
        "/v1/auth/email-change/preview",
        None,
        json!({"token": token}),
    )
    .await
}

/// The ordinary fixture: an Owner admin and a Viewer target in one org, plus a
/// requested address. Returns `(org_id, admin_token, target_id, target_email,
/// wanted_email)`.
async fn fixture(srv: &TestServer) -> (Uuid, String, Uuid, String, String) {
    let admin_email = srv.addr("admin");
    let (_admin_id, org_id, admin_token, _r) =
        owner_of_new_org(srv, &admin_email, "correct-horse-1").await;
    let target_email = srv.addr("target");
    let target_id = create_member(srv, org_id, &target_email, "correct-horse-2", "Viewer").await;
    let wanted = srv.addr("wanted");
    (org_id, admin_token, target_id, target_email, wanted)
}

#[tokio::test]
async fn a_request_mails_both_addresses_and_changes_nothing_yet() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;

    let (status, body) = request_change(&srv, org, target, &admin, &wanted).await;
    assert_eq!(status, 200, "{body}");

    // The whole premise: the response is 200 and the account is untouched.
    assert_eq!(current_email(&srv, target).await, old);

    // One mail each way, to the two different addresses.
    assert_eq!(count_mail(&srv, &old, "email_change_notice").await, 1);
    assert_eq!(count_mail(&srv, &wanted, "email_change_approval").await, 1);
    // And nothing crossed over.
    assert_eq!(count_mail(&srv, &wanted, "email_change_notice").await, 0);
    assert_eq!(count_mail(&srv, &old, "email_change_approval").await, 0);

    srv.shutdown().await;
}

#[tokio::test]
async fn the_notice_body_never_contains_the_new_address() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    let (status, body) = request_change(&srv, org, target, &admin, &wanted).await;
    assert_eq!(status, 200, "{body}");

    // Asserted against what was actually queued for delivery, which is the only
    // thing that matters — a unit test over the renderer cannot see a leak
    // introduced by the outbox or the branding shell.
    let notice = mail_body(&srv, &old, "email_change_notice").await;
    assert!(
        !notice.contains(&wanted),
        "the old address must never learn the new one:\n{notice}"
    );
    // Sanity: this is genuinely the right mail, so the assertion above is not
    // passing because it read an empty or unrelated body.
    assert!(notice.contains("cancel-email-change"));

    srv.shutdown().await;
}

#[tokio::test]
async fn confirming_moves_the_login_identity() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;

    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;
    let (status, body) = confirm(&srv, &tok).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(current_email(&srv, target).await, wanted);

    // The point of moving the identity: the new address signs in and the old
    // one does not.
    let (ok, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": wanted, "password": "correct-horse-2"}),
        )
        .await;
    assert_eq!(ok, 200, "the new address must authenticate");
    let (dead, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": old, "password": "correct-horse-2"}),
        )
        .await;
    assert_eq!(dead, 401, "the old address must stop authenticating");

    srv.shutdown().await;
}

#[tokio::test]
async fn confirming_leaves_existing_sessions_live() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;

    // A session held BEFORE the change.
    let (member_token, _refresh) = login(&srv, &old, "correct-horse-2").await;
    assert_eq!(srv.get_status("/v1/me", &member_token).await, 200);

    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;
    assert_eq!(confirm(&srv, &tok).await.0, 200);

    // DELIBERATE, and this assertion is what protects it: access and refresh
    // tokens carry a user id, not an address, so nothing about them went stale
    // and signing everybody out would be disruption without a security benefit.
    // If a later change starts revoking here, this fails rather than silently
    // logging every user out of every device on an address correction.
    assert_eq!(
        srv.get_status("/v1/me", &member_token).await,
        200,
        "sessions must survive an email change"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn confirming_invalidates_outstanding_password_reset_links() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;

    // A reset link mailed to the OLD address, before the change.
    let (rs, rb) = srv
        .post_raw(
            &format!("/v1/orgs/{org}/members/{target}/password-reset"),
            Some(&admin),
            json!({"action": "reset"}),
        )
        .await;
    assert_eq!(rs, 200, "{rb}");
    let reset_tok = token_from_mail(&srv, &old, "password_reset").await;

    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;
    assert_eq!(confirm(&srv, &tok).await.0, 200);

    // Without this, whoever still reads the old mailbox can set a password on
    // an account that has moved away from them — a full takeover, days later,
    // by exactly the person the change was meant to move the account away from.
    // `password_fingerprint` does not catch it: the PASSWORD never moved.
    let (status, body) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": reset_tok, "new_password": "a-brand-new-password"}),
        )
        .await;
    assert_eq!(
        status, 401,
        "a reset link mailed to the old address must die with the change: {body}"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn the_old_address_can_veto_the_change() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;

    let cancel_tok = token_from_mail(&srv, &old, "email_change_notice").await;
    let approve_tok = token_from_mail(&srv, &wanted, "email_change_approval").await;

    assert_eq!(cancel(&srv, &cancel_tok).await.0, 200);

    // The veto is the single thing standing between `member:credential` and an
    // account-takeover primitive, so it has to kill the other half of the row.
    let (status, body) = confirm(&srv, &approve_tok).await;
    assert_eq!(
        status, 401,
        "a vetoed change must not be confirmable: {body}"
    );
    assert_eq!(current_email(&srv, target).await, old);

    srv.shutdown().await;
}

#[tokio::test]
async fn preview_tells_the_two_sides_different_things() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;

    let approve_tok = token_from_mail(&srv, &wanted, "email_change_approval").await;
    let (s, body) = preview(&srv, &approve_tok).await;
    assert_eq!(s, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["role"], "approve");
    assert_eq!(v["new_email"], wanted);

    let cancel_tok = token_from_mail(&srv, &old, "email_change_notice").await;
    let (s, body) = preview(&srv, &cancel_tok).await;
    assert_eq!(s, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["role"], "cancel");
    // ABSENT, not null. A null would still be a key the page could render as
    // the string "null", and more importantly it would mean the server is
    // deciding to send the field and blank it rather than never sending it.
    assert!(
        v.get("new_email").is_none(),
        "the cancel side must not receive the key at all: {body}"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn a_second_request_supersedes_the_first() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, typo) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &typo).await;
    let typo_tok = token_from_mail(&srv, &typo, "email_change_approval").await;

    let corrected = srv.addr("corrected");
    let (status, body) = request_change(&srv, org, target, &admin, &corrected).await;
    assert_eq!(status, 200, "{body}");

    // The mistyped address must lose its claim the moment the correction is
    // issued, without the admin having to remember to withdraw it.
    let (dead, _) = confirm(&srv, &typo_tok).await;
    assert_eq!(dead, 401, "the superseded link must be dead");
    assert_eq!(current_email(&srv, target).await, old);

    let good_tok = token_from_mail(&srv, &corrected, "email_change_approval").await;
    assert_eq!(confirm(&srv, &good_tok).await.0, 200);
    assert_eq!(current_email(&srv, target).await, corrected);

    // The old address was warned BOTH times. A dedup window on the notice kind
    // would have swallowed the second one silently.
    assert_eq!(count_mail(&srv, &old, "email_change_notice").await, 2);

    srv.shutdown().await;
}

#[tokio::test]
async fn two_simultaneous_confirmations_yield_exactly_one_success() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;

    // Single-use has to be a property of one statement, not of a read followed
    // by a write — this is the test that would catch a SELECT-then-UPDATE.
    let (a, b) = tokio::join!(confirm(&srv, &tok), confirm(&srv, &tok));
    let wins = [a.0, b.0].iter().filter(|s| **s == 200).count();
    assert_eq!(wins, 1, "exactly one confirmation may win: {a:?} {b:?}");
    assert_eq!(current_email(&srv, target).await, wanted);

    srv.shutdown().await;
}

#[tokio::test]
async fn an_address_claimed_after_issue_is_refused_without_burning_the_link() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;

    // Somebody else takes the address during the link's 24-hour life.
    let squatter = {
        let mut conn = srv.conn().await;
        repo::create_user(&mut conn, &wanted, "hash", "Squatter")
            .await
            .expect("create squatter")
            .id
    };

    let (status, body) = confirm(&srv, &tok).await;
    assert_eq!(status, 409, "{body}");

    // The link must NOT have been spent: the address can be freed again, and a
    // burned link would strand a change that could still legitimately succeed.
    {
        let mut conn = srv.conn().await;
        diesel::sql_query("DELETE FROM users WHERE id = $1")
            .bind::<SqlUuid, _>(squatter)
            .execute(&mut conn)
            .await
            .expect("free the address");
    }
    let (status, body) = confirm(&srv, &tok).await;
    assert_eq!(
        status, 200,
        "the link must have survived the refusal: {body}"
    );
    assert_eq!(current_email(&srv, target).await, wanted);

    srv.shutdown().await;
}

#[tokio::test]
async fn smtp_unconfigured_returns_503_and_writes_no_row() {
    let Some(mut srv) = TestServer::start_without_mail().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;

    let (status, body) = request_change(&srv, org, target, &admin, &wanted).await;
    assert_eq!(status, 503, "{body}");

    // The ordering guarantee: a pending change must never exist when the mail
    // carrying its veto cannot be sent. A row here would be a change the member
    // was never warned about.
    assert_eq!(count_requests(&srv).await, 0);
    assert_eq!(current_email(&srv, target).await, old);

    srv.shutdown().await;
}

#[tokio::test]
async fn the_request_route_refuses_what_it_should() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let admin_email = srv.addr("admin");
    let (admin_id, org, admin, _r) = owner_of_new_org(&srv, &admin_email, "correct-horse-1").await;
    let target_email = srv.addr("target");
    let target = create_member(&srv, org, &target_email, "correct-horse-2", "Viewer").await;
    let wanted = srv.addr("wanted");

    // Self-target: an admin editing their own address belongs on an account
    // page, where their password can be demanded.
    assert_eq!(
        request_change(&srv, org, admin_id, &admin, &wanted).await.0,
        409
    );

    // Unchanged address.
    assert_eq!(
        request_change(&srv, org, target, &admin, &target_email)
            .await
            .0,
        409
    );

    // Malformed addresses. `contains('@')` is NOT sufficient: the mail path
    // normalizes with lettre's `Address::from_str`, and anything it rejects
    // would 500 AFTER the row was inserted and the veto notice queued —
    // warning the member about a change that can never complete and holding
    // the single live slot for 24 hours.
    for bad in [
        "not-an-address",
        "newuser@",
        "@example.com",
        "two parts@example.com",
        "a@b@example.com",
    ] {
        assert_eq!(
            request_change(&srv, org, target, &admin, bad).await.0,
            400,
            "address {bad:?} must be refused before anything is written"
        );
    }

    // Already taken by another account.
    assert_eq!(
        request_change(&srv, org, target, &admin, &admin_email)
            .await
            .0,
        409
    );

    // A member who is not in this org at all.
    assert_eq!(
        request_change(&srv, org, Uuid::new_v4(), &admin, &wanted)
            .await
            .0,
        404
    );

    // A caller without `member:credential`. Developer holds `member:manage`'s
    // neighbours but not this one, which is the carve-out working as intended.
    let dev_email = srv.addr("dev");
    create_member(&srv, org, &dev_email, "correct-horse-3", "Developer").await;
    let (dev_token, _r) = login(&srv, &dev_email, "correct-horse-3").await;
    assert_eq!(
        request_change(&srv, org, target, &dev_token, &wanted)
            .await
            .0,
        403
    );

    // Deactivated target.
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, target, false)
            .await
            .expect("deactivate");
    }
    assert_eq!(
        request_change(&srv, org, target, &admin, &wanted).await.0,
        409
    );

    assert_eq!(count_requests(&srv).await, 0, "no refusal may write a row");

    srv.shutdown().await;
}

#[tokio::test]
async fn withdrawing_an_expired_request_reports_nothing_and_audits_nothing() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;

    {
        let mut conn = srv.conn().await;
        diesel::sql_query(
            "UPDATE email_change_requests SET expires_at = now() - interval '1 hour'",
        )
        .execute(&mut conn)
        .await
        .expect("age the request");
    }

    let before = email_change_audit(&srv).await.len();
    let resp = srv
        .client
        .delete(format!(
            "{}/v1/orgs/{org}/members/{target}/email-change",
            srv.base
        ))
        .bearer_auth(&admin)
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status().as_u16(), 200);
    let v: Value = resp.json().await.expect("json");

    // The underlying repo call DOES clear the expired row — it has to, because
    // an expired-but-uncancelled row still occupies the one-live-per-user
    // index and supersede would otherwise be blocked by it. But that is
    // tidying, not withdrawing: there was nothing live to withdraw.
    assert_eq!(
        v["withdrawn"], false,
        "a lapsed request cannot be withdrawn: {v}"
    );
    assert_eq!(
        email_change_audit(&srv).await.len(),
        before,
        "no cancellation entry for a request that had already expired"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn tidying_an_expired_request_still_frees_the_slot() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    {
        let mut conn = srv.conn().await;
        diesel::sql_query(
            "UPDATE email_change_requests SET expires_at = now() - interval '1 hour'",
        )
        .execute(&mut conn)
        .await
        .expect("age the request");
    }

    // The other half of the same rule: reporting `withdrawn: false` must not
    // mean the row was left in place, or the partial unique index would refuse
    // every future request for this member.
    let second = srv.addr("second");
    let (status, body) = request_change(&srv, org, target, &admin, &second).await;
    assert_eq!(
        status, 200,
        "an expired row must not block a new request: {body}"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn an_admin_can_withdraw_a_pending_change() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;

    let resp = srv
        .client
        .delete(format!(
            "{}/v1/orgs/{org}/members/{target}/email-change",
            srv.base
        ))
        .bearer_auth(&admin)
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status().as_u16(), 200);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["withdrawn"], true);

    assert_eq!(confirm(&srv, &tok).await.0, 401);
    assert_eq!(current_email(&srv, target).await, old);

    // Idempotent: withdrawing nothing is not an error, and reports itself.
    let resp = srv
        .client
        .delete(format!(
            "{}/v1/orgs/{org}/members/{target}/email-change",
            srv.base
        ))
        .bearer_auth(&admin)
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status().as_u16(), 200);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["withdrawn"], false);

    srv.shutdown().await;
}

#[tokio::test]
async fn an_expired_request_confirms_nothing() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;

    {
        let mut conn = srv.conn().await;
        diesel::sql_query(
            "UPDATE email_change_requests SET expires_at = now() - interval '1 minute'",
        )
        .execute(&mut conn)
        .await
        .expect("age the request");
    }

    assert_eq!(confirm(&srv, &tok).await.0, 401);
    assert_eq!(preview(&srv, &tok).await.0, 401);
    assert_eq!(current_email(&srv, target).await, old);

    srv.shutdown().await;
}

#[tokio::test]
async fn a_malformed_token_is_refused_on_shape() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };

    // Rejected before Redis or the database is touched, so a spray mints no
    // limiter keys — otherwise the per-token limiter turns a brute-force
    // attempt into a memory-exhaustion one.
    for bad in ["", "short", "zz", &"g".repeat(64)] {
        assert_eq!(confirm(&srv, bad).await.0, 401, "token {bad:?}");
        assert_eq!(cancel(&srv, bad).await.0, 401, "token {bad:?}");
        assert_eq!(preview(&srv, bad).await.0, 401, "token {bad:?}");
    }
    // Well-formed but unknown: same answer, so nothing distinguishes "no such
    // token" from "expired" or "already used".
    let unknown = "a".repeat(64);
    assert_eq!(confirm(&srv, &unknown).await.0, 401);

    srv.shutdown().await;
}

/// `(action, changes)` for every email-change audit row, oldest first.
async fn email_change_audit(srv: &TestServer) -> Vec<(String, Value)> {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        action: String,
        #[diesel(sql_type = diesel::sql_types::Jsonb)]
        changes: Value,
    }
    let mut conn = srv.conn().await;
    let rows: Vec<Row> = diesel::sql_query(
        "SELECT action, changes FROM audit_log WHERE action LIKE 'member.email_change%' \
         ORDER BY created_at",
    )
    .load(&mut conn)
    .await
    .expect("read audit trail");
    rows.into_iter().map(|r| (r.action, r.changes)).collect()
}

#[tokio::test]
async fn the_audit_trail_records_the_address_and_the_reason() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;

    // `audit::created` silently DROPS any field missing from the per-entity
    // allowlist — no error, no warning, and every status-code assertion in this
    // file still passes. The first version of this feature recorded an empty
    // `{}` for the approval and omitted `new_email` from the request for
    // exactly that reason, so these assertions are on the CONTENT.
    let trail = email_change_audit(&srv).await;
    assert_eq!(trail.len(), 1, "{trail:#?}");
    assert_eq!(trail[0].0, "member.email_change_request");
    assert_eq!(
        trail[0].1["new_email"]["to"], wanted,
        "the requested address is the point of the entry: {:#?}",
        trail[0].1
    );
    assert!(trail[0].1["expires_at"]["to"].is_string());

    // A member refusing an admin's attempt is the row worth spotting, and
    // `cancelled_reason` is the only thing that distinguishes it from a routine
    // admin withdrawal.
    let cancel_tok = token_from_mail(&srv, &old, "email_change_notice").await;
    assert_eq!(cancel(&srv, &cancel_tok).await.0, 200);

    let trail = email_change_audit(&srv).await;
    assert_eq!(trail.len(), 2, "{trail:#?}");
    assert_eq!(trail[1].0, "member.email_change_cancelled");
    assert_eq!(
        trail[1].1["cancelled_reason"]["to"], "user",
        "{:#?}",
        trail[1].1
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn the_approval_audit_entry_names_the_new_address() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;
    request_change(&srv, org, target, &admin, &wanted).await;
    let tok = token_from_mail(&srv, &wanted, "email_change_approval").await;
    assert_eq!(confirm(&srv, &tok).await.0, 200);

    let trail = email_change_audit(&srv).await;
    let approved = trail
        .iter()
        .find(|(a, _)| a == "member.email_change_approved")
        .expect("an approval entry");
    // This one recorded `{}` before the member allowlist learned `new_email` —
    // an entry saying an address changed, without saying to what.
    assert_eq!(approved.1["new_email"]["to"], wanted, "{:#?}", approved.1);

    srv.shutdown().await;
}

#[tokio::test]
async fn a_pending_change_is_visible_on_the_members_list() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (org, admin, target, _old, wanted) = fixture(&srv).await;

    // Before: the field is present and null, so the dashboard can rely on it.
    let body = srv.get(&format!("/v1/orgs/{org}/members"), &admin).await;
    let members: Value = body.json().await.expect("json");
    let row = members
        .as_array()
        .expect("array")
        .iter()
        .find(|m| m["user_id"] == target.to_string())
        .expect("the target is listed")
        .clone();
    assert!(row["pending_email_change"].is_null());

    request_change(&srv, org, target, &admin, &wanted).await;

    // After: without this the withdraw action exists on the server and is
    // unreachable from the UI, which is the same as not existing.
    let body = srv.get(&format!("/v1/orgs/{org}/members"), &admin).await;
    let members: Value = body.json().await.expect("json");
    let row = members
        .as_array()
        .expect("array")
        .iter()
        .find(|m| m["user_id"] == target.to_string())
        .expect("the target is listed")
        .clone();
    assert_eq!(row["pending_email_change"]["new_email"], wanted);

    srv.shutdown().await;
}
