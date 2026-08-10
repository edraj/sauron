//! HTTP-level tests for the three alerting rulings, driven through the real
//! router. Before these, `create_channel`, `update_channel` and `create_rule`
//! had **zero** test coverage of any kind — which is why all three defects
//! shipped with every gate green.
//!
//! What each group pins:
//!
//! **D7 — credential rebinding.** A channel's destination lives in `config`
//! (`matrix.homeserver`, `email.host`, `webhook.url`) while its credential lives
//! in the encrypted secret bundle, and `PATCH` lets the two move
//! independently. Omitting `secret` means "leave the stored one alone", so a
//! caller holding only `alert:write` — a permission whose own documentation
//! promises "secrets always redacted" — could repoint a channel at a host they
//! control, hit `POST …/test`, and have the server hand over the Matrix access
//! token or the SMTP password. The SSRF guard is irrelevant: the attacker's host
//! is public, which it permits by design.
//!
//! **D9 — rule targeting.** `alert:write` authorizes *configuring* alerting; it
//! never authorized the telemetry a rule emits. Notification bodies carry
//! verbatim issue titles and monitor targets, and the disclosure is durable —
//! `alert_events` keeps title/body and `GET /v1/orgs/{id}/alert-events` serves
//! them back. The un-narrowed rule (no `project_id`, no `app_id`) is the WIDER
//! hole, not the narrower one: it expands to every app in the org.
//!
//! Two follow-ups to D9 live here too, both from review of the original fix:
//!
//!  * the gate was **create-only**. A rule's target is immutable, so the bypass
//!    is not re-aiming an existing rule but re-routing one: org-scoped
//!    `alert:write` alone let a caller attach their own channel to (or re-enable,
//!    or re-condition) a rule over a project they cannot read.
//!  * **monitor triggers have no app dimension.** `monitors` carries only
//!    `project_id`, so `repo::alert_rules_for_monitor` matches org + project and
//!    an app-narrowed `monitor_down` rule fires project-wide — which made
//!    authorizing it at app scope narrower than its own blast radius.
//!
//! **D6 — config at rest.** `config` was plaintext JSONB, and for the generic
//! webhook kind that is where the target URL and an arbitrary header map live.
//!
//! Every test spawns the actual compiled `sauron-api` binary against a fresh,
//! migrated, ephemeral database. See `tests/http_env_scoping.rs`'s `TestServer`
//! for the full doc comments this file's copy abbreviates.
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
const JWT_SECRET: &str = "http-alerting-test-secret-00000000000000";

/// Required, not optional: `sauron-api` refuses to boot without it since the
/// channel key was made fail-closed, so a harness that omits it dies at startup
/// with a config error rather than anything to do with the routes under test.
const NOTIFY_SECRET_KEY: &str = "http-alerting-test-notify-key-00000000000";

const PASSWORD: &str = "correct-horse-battery-staple";

/// The values that must never come back out. Distinctive enough that a
/// substring search over the raw response body is a meaningful assertion: it
/// holds wherever in the payload the value might reappear, including under a key
/// this test has never heard of.
const MATRIX_TOKEN: &str = "syt_MATRIX_TOKEN_DO_NOT_LEAK";
const WEBHOOK_URL: &str = "https://hooks.example.com/services/T000/B000/SUPERSECRET";
const WEBHOOK_AUTH: &str = "Bearer sk-live-DO-NOT-LEAK";

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
        // The probe listener is dropped on return, so a concurrent
        // `TestServer::start()` on another thread can be handed the same port
        // before this process's child binds it.
        if issued.lock().expect("port registry").insert(port) {
            return port;
        }
    }
    panic!("no unused ephemeral port after 100 attempts");
}

/// Spawn a `sauron-api` child against an already-migrated database and wait for
/// `/health`.
///
/// Factored out of `TestServer::start` because the migration-000046 conversion
/// runs at BOOT, so testing it needs a second boot against the same database.
async fn spawn_api(db_url: &str, redis_url: &str) -> (tokio::process::Child, String) {
    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_sauron-api");
    let mut child = tokio::process::Command::new(bin)
        .env("DATABASE_URL", db_url)
        .env("REDIS_URL", redis_url)
        .env("JWT_SECRET", JWT_SECRET)
        .env("NOTIFY_SECRET_KEY", NOTIFY_SECRET_KEY)
        .env("API_PORT", port.to_string())
        .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
        // Paired with the per-server `X-Forwarded-For` below: the
        // register-rate-limit bucket is keyed on caller IP and Redis is shared
        // across test binaries on this host.
        .env("API_TRUST_FORWARDED_HEADERS", "1")
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sauron-api binary");

    let base = format!("http://127.0.0.1:{port}");
    let probe = reqwest::Client::new();
    for _ in 0..100 {
        if let Ok(Some(status)) = child.try_wait() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = s.read_to_string(&mut stderr).await;
            }
            panic!("sauron-api exited early with {status}; stderr:\n{stderr}");
        }
        if probe
            .get(format!("{base}/health"))
            .timeout(StdDuration::from_millis(200))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return (child, base);
        }
        tokio::time::sleep(StdDuration::from_millis(100)).await;
    }
    panic!("sauron-api never became ready on {base}/health");
}

struct TestServer {
    child: tokio::process::Child,
    base: String,
    client: reqwest::Client,
    admin_url: String,
    db_url: String,
    redis_url: String,
    db_name: String,
    pool: sauron_db::PgPool,
    cleaned_up: Cell<bool>,
}

impl TestServer {
    async fn start() -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

        // Segment order is load-bearing — timestamp FIRST, discriminator glued
        // to the uuid; `sauron-db`'s `reap_stale_test_databases` parses the
        // first segment as a timestamp and silently skips anything else.
        let db_name = format!(
            "sauron_test_{}_alr{}",
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

        let (child, base) = spawn_api(&db_url, &redis_url).await;

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

        Some(TestServer {
            child,
            base,
            client,
            admin_url,
            db_url,
            redis_url,
            db_name,
            pool,
            cleaned_up: Cell::new(false),
        })
    }

    async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    /// Stop the API and boot a fresh one against the same database, keeping the
    /// same HTTP client (and therefore the same rate-limit bucket and tokens —
    /// JWTs survive because the signing secret is a constant here).
    ///
    /// Exists for one reason: the plaintext-config conversion is a boot-time
    /// pass, so nothing short of a second boot exercises it.
    async fn restart(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        let (child, base) = spawn_api(&self.db_url, &self.redis_url).await;
        self.child = child;
        self.base = base;
    }

    /// GET `path` and return `(status, raw body text, parsed JSON)`. The raw
    /// text is returned alongside the parsed value because the strongest
    /// assertion available here is "this secret appears nowhere in the
    /// response", which a field-by-field walk cannot make.
    async fn get_raw(&self, path: &str, token: &str) -> (u16, String, Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read GET body");
        let body = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("GET {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        });
        (status, text, body)
    }

    async fn post_raw(&self, path: &str, token: Option<&str>, body: Value) -> (u16, String, Value) {
        let mut req = self.client.post(format!("{}{path}", self.base)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read POST body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }

    async fn post_ok(&self, path: &str, token: &str, body: Value) -> (String, Value) {
        let (status, text, v) = self.post_raw(path, Some(token), body).await;
        assert!(
            (200..300).contains(&status),
            "POST {path} ({status}): {text}"
        );
        (text, v)
    }

    async fn delete_raw(&self, path: &str, token: &str) -> (u16, String, Value) {
        let resp = self
            .client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read DELETE body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }

    async fn patch_raw(&self, path: &str, token: &str, body: Value) -> (u16, String, Value) {
        let resp = self
            .client
            .patch(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("PATCH {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read PATCH body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }

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

/// An org owner, two projects with an app each, and "Mallory" — a user holding
/// a CUSTOM org-scoped `[alert:read, alert:write]` role plus read access to
/// project A only.
///
/// The custom role is the whole point. All four presets are safe by accident:
/// Owner and Admin carry `alert:write` *alongside* `issue:read` at the same
/// scope, and Developer and Viewer carry neither write permission. The hole
/// opens the moment someone uses the role editor to mint the obvious "Alerts
/// Operator" / on-call-vendor role — which is exactly what that editor is for.
struct Fixture {
    org_id: Uuid,
    project_a: Uuid,
    project_b: Uuid,
    app_a: Uuid,
    app_b: Uuid,
    owner_token: String,
    mallory_token: String,
}

async fn seed(server: &TestServer, label: &str) -> Fixture {
    let email = format!("{label}-owner-{}@example.com", Uuid::new_v4().simple());
    let (status, text, body) = server
        .post_raw(
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
    assert!((200..300).contains(&status), "register ({status}): {text}");
    let owner_id: Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let mut conn = server.conn().await;
    let org_id = repo::list_orgs_for_user(&mut conn, owner_id)
        .await
        .expect("list orgs")
        .first()
        .expect("owner has an org")
        .id;

    let suffix = Uuid::new_v4().simple().to_string();
    let project_a = repo::create_project(&mut conn, org_id, "alpha", &format!("alpha-{suffix}"))
        .await
        .expect("create project a");
    let project_b = repo::create_project(&mut conn, org_id, "bravo", &format!("bravo-{suffix}"))
        .await
        .expect("create project b");
    let app_a = repo::create_app(
        &mut conn,
        project_a.id,
        "app-a",
        &format!("a-{suffix}"),
        "web",
    )
    .await
    .expect("create app a");
    let app_b = repo::create_app(
        &mut conn,
        project_b.id,
        "app-b",
        &format!("b-{suffix}"),
        "web",
    )
    .await
    .expect("create app b");

    // Org-scoped alerting rights with no read rights attached.
    let alerts_role = repo::create_role(
        &mut conn,
        org_id,
        "Alerts Operator",
        "configures alerting, reads nothing",
        json!([perm::ALERT_READ, perm::ALERT_WRITE]),
    )
    .await
    .expect("create custom alerting role");
    // …and read rights on project A only.
    //
    // Deliberately NOT the Developer preset, which also carries `monitor:read`
    // and `monitor:write`. Using it would make
    // `a_monitor_rule_is_authorized_on_monitor_read_not_issue_read` vacuous —
    // the whole point of that test is a principal who can read issues and
    // cannot read monitors, which no preset expresses.
    let reader_role = repo::create_role(
        &mut conn,
        org_id,
        "Issue Reader",
        "reads error signal, nothing else",
        json!([perm::ISSUE_READ, perm::EVENT_READ, perm::APP_READ]),
    )
    .await
    .expect("create custom reader role");

    let mallory_email = format!("{label}-mallory-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let mallory = repo::create_user(&mut conn, &mallory_email, &hash, "mallory")
        .await
        .expect("create mallory");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: mallory.id,
            role_id: alerts_role.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant the alerting role at org scope");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: mallory.id,
            role_id: reader_role.id,
            scope_type: "project".to_string(),
            scope_id: project_a.id,
        },
    )
    .await
    .expect("grant the reader role on project A");
    drop(conn);

    Fixture {
        org_id,
        project_a: project_a.id,
        project_b: project_b.id,
        app_a: app_a.id,
        app_b: app_b.id,
        owner_token: server.login(&email, PASSWORD).await,
        mallory_token: server.login(&mallory_email, PASSWORD).await,
    }
}

/// Create a channel as the owner and return its id.
async fn create_channel(server: &TestServer, fx: &Fixture, body: Value) -> Uuid {
    let (text, v) = server
        .post_ok(
            &format!("/v1/orgs/{}/notification-channels", fx.org_id),
            &fx.owner_token,
            body,
        )
        .await;
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created channel has no id: {text}"))
        .parse()
        .expect("channel id is a uuid")
}

async fn rule_count(server: &TestServer, org_id: Uuid) -> usize {
    let mut conn = server.conn().await;
    repo::list_alert_rules_for_org(&mut conn, org_id)
        .await
        .expect("list alert rules")
        .len()
}

/// Mint a user holding org-scoped `[alert:read, alert:write]` plus `read_perms`
/// at exactly `(scope_type, scope_id)`, and return their bearer token.
///
/// The whole D9 family turns on "alerting rights at one scope, read rights at
/// another", and the interesting cases are the ones no preset can express — a
/// `monitor:read` grant pinned to a single *app*, in particular, which is what
/// separates an app-scoped check from a project-scoped one.
async fn user_with_read_at(
    server: &TestServer,
    org_id: Uuid,
    label: &str,
    read_perms: &[&str],
    scope_type: &str,
    scope_id: Uuid,
) -> String {
    let mut conn = server.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let alerts_role = repo::create_role(
        &mut conn,
        org_id,
        &format!("{label} alerting {suffix}"),
        "configures alerting, reads nothing",
        json!([perm::ALERT_READ, perm::ALERT_WRITE]),
    )
    .await
    .expect("create alerting role");
    let read_role = repo::create_role(
        &mut conn,
        org_id,
        &format!("{label} reading {suffix}"),
        "reads one signal at one scope",
        json!(read_perms),
    )
    .await
    .expect("create reader role");

    let email = format!("{label}-{suffix}@example.com");
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let user = repo::create_user(&mut conn, &email, &hash, label)
        .await
        .expect("create user");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: alerts_role.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant alerting at org scope");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: read_role.id,
            scope_type: scope_type.to_string(),
            scope_id,
        },
    )
    .await
    .expect("grant reading at the requested scope");
    drop(conn);

    server.login(&email, PASSWORD).await
}

/// Create a rule as the owner (who reads everything) and return its id.
async fn create_rule_as_owner(server: &TestServer, fx: &Fixture, body: Value) -> String {
    let (text, v) = server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.owner_token,
            body,
        )
        .await;
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created rule has no id: {text}"))
        .to_string()
}

// --- D7: a stored secret may only go to the destination it was issued for ----

#[tokio::test]
async fn repointing_a_matrix_channel_without_the_secret_is_rejected() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "rebind").await;
    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "ops",
            "kind": "matrix",
            "config": { "homeserver": "https://matrix.example.org", "room_id": "!ops:example.org" },
            "secret": { "access_token": MATRIX_TOKEN },
        }),
    )
    .await;

    // The attack: replace the destination, omit `secret`, and the server would
    // previously decrypt the stored token purely to validate it against the new
    // host and then bless the pair.
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({
                "config": {
                    "homeserver": "https://collector.attacker.example",
                    "room_id": "!ops:example.org"
                }
            }),
        )
        .await;
    assert_eq!(status, 400, "repointing must be refused: {text}");
    assert!(
        text.contains("re-supplying its secret"),
        "the refusal must say what to do about it: {text}"
    );

    // Refused BEFORE the write. `repo::update_channel` writes field by field
    // with no transaction, so a check that ran after the config update would
    // leave the channel repointed and merely report failure.
    let (_, raw, body) = server
        .get_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.owner_token,
        )
        .await;
    assert_eq!(
        body["config"]["homeserver"],
        json!("https://matrix.example.org"),
        "the stored destination moved despite the 400: {raw}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn repointing_is_allowed_when_the_secret_is_re_supplied_or_cleared() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "rebindok").await;

    // Supplying a replacement means the caller chose this host for this
    // credential knowingly — the legitimate flow must keep working, or the fix
    // is a feature outage rather than a fix.
    let with_secret = create_channel(
        &server,
        &fx,
        json!({
            "name": "ops",
            "kind": "matrix",
            "config": { "homeserver": "https://matrix.example.org", "room_id": "!r:example.org" },
            "secret": { "access_token": MATRIX_TOKEN },
        }),
    )
    .await;
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{with_secret}"),
            &fx.mallory_token,
            json!({
                "config": { "homeserver": "https://matrix.other.example", "room_id": "!r:example.org" },
                "secret": { "access_token": "syt_A_NEW_TOKEN_FOR_THE_NEW_HOST" },
            }),
        )
        .await;
    assert_eq!(status, 200, "re-supplying the secret must succeed: {text}");

    // Clearing it removes the credential, so there is nothing left to
    // mis-deliver. Matrix needs a token to resolve, so use a generic webhook
    // (whose signing secret is optional) for this leg.
    let clearable = create_channel(
        &server,
        &fx,
        json!({
            "name": "hook",
            "kind": "webhook",
            "config": { "url": "https://a.example/x" },
            "secret": { "signing_secret": "s3cr3t" },
        }),
    )
    .await;
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{clearable}"),
            &fx.mallory_token,
            json!({ "config": { "url": "https://b.example/x" }, "secret": {} }),
        )
        .await;
    assert_eq!(status, 200, "clearing the secret must be allowed: {text}");

    server.shutdown().await;
}

#[tokio::test]
async fn edits_that_do_not_move_the_destination_never_require_the_secret() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "noop").await;
    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "hook",
            "kind": "webhook",
            "config": { "url": "https://h.example/path/a" },
            "secret": { "signing_secret": "s3cr3t" },
        }),
    )
    .await;

    // The dashboard's only real `updateChannel` call is the enabled toggle; if
    // the guard over-triggered here the whole Alerts page would break.
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "name": "renamed", "enabled": false }),
        )
        .await;
    assert_eq!(
        status, 200,
        "rename + toggle must not need a secret: {text}"
    );

    // A path-only edit stays inside the same origin, so the signing secret is
    // exposed to nobody new. Forcing a re-paste here would train operators to
    // handle secrets more often, for no security gain.
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "config": { "url": "https://h.example/path/b" } }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a same-origin path edit must be allowed: {text}"
    );

    // …but the https→http downgrade on that same host is a move.
    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "config": { "url": "http://h.example/path/b" } }),
        )
        .await;
    assert_eq!(
        status, 400,
        "an https→http downgrade hands the secret to any on-path observer: {text}"
    );

    server.shutdown().await;
}

// --- D6: the config is a credential too --------------------------------------

#[tokio::test]
async fn a_webhook_url_and_its_headers_leave_neither_the_api_nor_a_plaintext_row() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "cfgenc").await;
    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "collector",
            "kind": "webhook",
            "config": { "url": WEBHOOK_URL, "headers": { "Authorization": WEBHOOK_AUTH } },
        }),
    )
    .await;

    // The API half. Encrypting the column changes this response by zero bytes on
    // its own — the row is decrypted server-side and serialized anyway — so the
    // redacted projection is a separate, required part of the fix. `alert:read`
    // is held by the Developer preset, i.e. by most of an engineering org.
    for (label, path) in [
        ("detail", format!("/v1/notification-channels/{channel_id}")),
        (
            "list",
            format!("/v1/orgs/{}/notification-channels", fx.org_id),
        ),
    ] {
        let (status, raw, _) = server.get_raw(&path, &fx.mallory_token).await;
        assert_eq!(status, 200, "{label} read: {raw}");
        assert!(
            !raw.contains(WEBHOOK_URL),
            "{label}: the webhook URL is in the response:\n{raw}"
        );
        assert!(
            !raw.contains(WEBHOOK_AUTH),
            "{label}: the Authorization header value is in the response:\n{raw}"
        );
    }

    // The signals that replace them: enough to identify the channel, not enough
    // to use it. The origin identifies the vendor; the path segment is the
    // credential. Header NAMES are useful and safe, header values never leave.
    let (_, raw, body) = server
        .get_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
        )
        .await;
    assert_eq!(body["config"]["has_url"], json!(true), "body: {raw}");
    assert_eq!(
        body["config"]["url_origin"],
        json!("https://hooks.example.com:443"),
        "body: {raw}"
    );
    assert_eq!(
        body["config"]["header_names"],
        json!(["Authorization"]),
        "body: {raw}"
    );
    assert_eq!(body["config_error"], json!(false), "body: {raw}");

    // The at-rest half, read straight from Postgres. `config` must be the blank
    // legacy placeholder and the ciphertext must not contain the plaintext
    // bytes — the assertion that distinguishes encryption from encoding.
    let mut conn = server.conn().await;
    let row = repo::get_channel(&mut conn, channel_id)
        .await
        .expect("get_channel")
        .expect("channel row");
    assert_eq!(
        row.config,
        json!({}),
        "the legacy plaintext column still holds the config"
    );
    let blob = row.config_enc.expect("config_enc is populated");
    let bytes = String::from_utf8_lossy(&blob).to_string();
    for needle in [WEBHOOK_URL, WEBHOOK_AUTH, "Authorization"] {
        assert!(!bytes.contains(needle), "{needle} survived into config_enc");
    }
    drop(conn);

    server.shutdown().await;
}

#[tokio::test]
async fn a_pre_existing_plaintext_config_is_read_correctly_and_converted_at_boot() {
    use diesel_async::SimpleAsyncConnection;

    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "backfill").await;
    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "legacy",
            "kind": "webhook",
            "config": { "url": WEBHOOK_URL, "headers": { "Authorization": WEBHOOK_AUTH } },
        }),
    )
    .await;

    // Rewind the row to its pre-000046 shape. Every deployment that has ever run
    // this product has rows in exactly this state, and they are the ones the
    // ruling is actually about — a fix that only encrypts NEW channels leaves
    // the existing credentials sitting in cleartext in every base backup.
    let legacy = json!({ "url": WEBHOOK_URL, "headers": { "Authorization": WEBHOOK_AUTH } });
    let mut conn = server.conn().await;
    conn.batch_execute(&format!(
        "UPDATE notification_channels SET config = '{legacy}'::jsonb, config_enc = NULL \
         WHERE id = '{channel_id}'"
    ))
    .await
    .expect("rewind the row to plaintext");
    drop(conn);

    // Dual read: the running API must serve the legacy row unchanged. Without
    // this the conversion would be a flag day rather than a migration.
    let (status, raw, body) = server
        .get_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "legacy row read: {raw}");
    assert_eq!(body["config"]["has_url"], json!(true), "body: {raw}");
    assert_eq!(
        body["config"]["header_names"],
        json!(["Authorization"]),
        "body: {raw}"
    );
    assert!(!raw.contains(WEBHOOK_AUTH), "legacy row leaked: {raw}");

    server.restart().await;

    // Converted, and the plaintext is gone rather than merely shadowed.
    let mut conn = server.conn().await;
    let row = repo::get_channel(&mut conn, channel_id)
        .await
        .expect("get_channel")
        .expect("channel row");
    assert_eq!(row.config, json!({}), "legacy plaintext survived the boot");
    let first = row
        .config_enc
        .expect("config_enc populated by the boot pass");
    assert!(
        !String::from_utf8_lossy(&first).contains(WEBHOOK_AUTH),
        "the header value survived into the ciphertext"
    );
    drop(conn);

    // Same value through the API, so the conversion is invisible to callers.
    let (status, raw, body) = server
        .get_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "converted row read: {raw}");
    assert_eq!(
        body["config"]["url_origin"],
        json!("https://hooks.example.com:443"),
        "body: {raw}"
    );

    // Idempotent: a second boot must not re-encrypt (which would now encrypt the
    // `{}` placeholder and silently destroy the channel).
    server.restart().await;
    let mut conn = server.conn().await;
    let again = repo::get_channel(&mut conn, channel_id)
        .await
        .expect("get_channel")
        .expect("channel row")
        .config_enc
        .expect("config_enc still populated");
    assert_eq!(again, first, "the second boot rewrote a converted row");
    drop(conn);

    server.shutdown().await;
}

// --- D9: a rule must be authorized against the telemetry it emits ------------

#[tokio::test]
async fn alert_write_alone_cannot_target_an_app_it_cannot_read() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "target").await;
    let before = rule_count(&server, fx.org_id).await;

    // Baseline: the app's own issue route already refuses her.
    let (status, _, _) = server
        .get_raw(&format!("/v1/apps/{}/issues", fx.app_b), &fx.mallory_token)
        .await;
    assert_eq!(status, 403, "fixture precondition: no read on project B");

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({ "name": "x", "trigger_type": "issue_new", "app_id": fx.app_b }),
        )
        .await;
    assert_eq!(status, 403, "targeting an unreadable app: {text}");
    // A 403 that still inserted would be the worst outcome: the rule would keep
    // firing and the caller would believe they had been refused.
    assert_eq!(rule_count(&server, fx.org_id).await, before, "row inserted");

    server.shutdown().await;
}

#[tokio::test]
async fn an_unnarrowed_rule_needs_org_wide_read_not_merely_alert_write() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "unnarrowed").await;
    let before = rule_count(&server, fx.org_id).await;

    // The case a `if let Some(app_id)` patch would miss entirely, and the
    // CHEAPER exploit: omitting both ids is not "no target", it is every app in
    // the org, because `apps_in_alert_scope` applies no filter.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({ "name": "everything", "trigger_type": "issue_new" }),
        )
        .await;
    assert_eq!(status, 403, "un-narrowed rule: {text}");
    assert_eq!(rule_count(&server, fx.org_id).await, before, "row inserted");

    // …and the owner, who does hold org-wide read, must still be able to create
    // exactly that rule. Without this the `(None, None)` arm could degrade into
    // a blanket refusal and nobody would notice until an on-call rotation broke.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.owner_token),
            json!({ "name": "everything", "trigger_type": "issue_new" }),
        )
        .await;
    assert_eq!(status, 200, "org-wide reader creating a wide rule: {text}");

    server.shutdown().await;
}

#[tokio::test]
async fn a_rule_narrowed_to_a_readable_scope_is_still_allowed() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "allowed").await;

    // App scope inside project A: she reads it, so she may alert on it.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({ "name": "ok-app", "trigger_type": "issue_new", "app_id": fx.app_a }),
        )
        .await;
    assert_eq!(status, 200, "targeting a readable app: {text}");

    // Project scope exercises the middle arm, which neither of the 403 tests
    // touches.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({ "name": "ok-proj", "trigger_type": "issue_new", "project_id": fx.project_a }),
        )
        .await;
    assert_eq!(status, 200, "targeting a readable project: {text}");

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({ "name": "bad-proj", "trigger_type": "issue_new", "project_id": fx.project_b }),
        )
        .await;
    assert_eq!(status, 403, "targeting an unreadable project: {text}");

    server.shutdown().await;
}

#[tokio::test]
async fn a_monitor_rule_is_authorized_on_monitor_read_not_issue_read() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "monrule").await;

    // Mallory reads issues on project A but not monitors, and a monitor alert
    // discloses the probed target — often an internal hostname, so one
    // un-narrowed `monitor_down` rule enumerates the org's internal endpoints.
    // This pins the permission-selection `match`, the part of the fix most
    // likely to be silently wrong: fold monitors under `issue:read` and every
    // other test here still passes while the reconnaissance path stays open.
    //
    // The control is the test below it: the SAME scope with an issue trigger is
    // a 200, so a 403 here can only be about which permission was demanded.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({
                "name": "downs",
                "trigger_type": "monitor_down",
                "project_id": fx.project_a,
            }),
        )
        .await;
    assert_eq!(status, 403, "monitor rule without monitor:read: {text}");

    // The control: same caller, same scope, issue trigger → allowed.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({
                "name": "news",
                "trigger_type": "issue_new",
                "project_id": fx.project_a,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the 403 above must be about monitor:read, not about the scope: {text}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn the_scope_of_an_existing_rule_stays_immutable() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "immutable").await;

    // The create-only assumption the whole fix rests on: `UpdateRuleReq` has no
    // project/app field, so serde drops them and a rule cannot be re-targeted
    // after insert. If someone adds re-targeting later, this fails and points
    // them at applying the same check there.
    let (text, rule) = server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.mallory_token,
            json!({ "name": "pinned", "trigger_type": "issue_new", "app_id": fx.app_a }),
        )
        .await;
    let rule_id = rule["id"].as_str().unwrap_or_else(|| panic!("{text}"));

    let (status, text, updated) = server
        .patch_raw(
            &format!("/v1/alert-rules/{rule_id}"),
            &fx.mallory_token,
            json!({ "name": "renamed", "app_id": fx.app_b, "project_id": fx.project_b }),
        )
        .await;
    assert_eq!(status, 200, "rename: {text}");
    assert_eq!(updated["app_id"], json!(fx.app_a), "app moved: {text}");
    assert_eq!(
        updated["project_id"],
        json!(fx.project_a),
        "project moved: {text}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn editing_a_rule_you_could_not_have_created_is_refused() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "editgate").await;

    // The bypass a create-only gate leaves open. The target itself is immutable
    // (test above), so the exploit is not re-aiming the rule — it is RE-ROUTING
    // it: the owner legitimately created a rule over project B, Mallory cannot
    // read project B, and `alert:write` at org scope was enough to attach her own
    // channel to it. Every alert then carries project B's verbatim issue titles
    // to a destination she chose, and `alert_events` keeps a copy after she
    // detaches, so deleting the channel afterwards does not undo it.
    let rule_id = create_rule_as_owner(
        &server,
        &fx,
        json!({ "name": "owner's rule", "trigger_type": "issue_new", "app_id": fx.app_b }),
    )
    .await;
    let mine = create_channel(
        &server,
        &fx,
        json!({
            "name": "mallory's hook",
            "kind": "webhook",
            "config": { "url": "https://mallory.example/collect" },
        }),
    )
    .await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/alert-rules/{rule_id}"),
            &fx.mallory_token,
            json!({ "channel_ids": [mine] }),
        )
        .await;
    assert_eq!(
        status, 403,
        "re-routing a rule over an unreadable app: {text}"
    );

    // A 403 that still wrote is the worst outcome — the caller believes they
    // were refused while the alerts flow. Checked at the row, not by re-reading
    // the API, because `get_rule` needs only `alert:read` and would happily
    // report the pre-edit value even if the write had landed elsewhere.
    let mut conn = server.conn().await;
    let attached = repo::rule_channel_ids(&mut conn, rule_id.parse().expect("rule id is a uuid"))
        .await
        .expect("rule_channel_ids");
    assert!(
        attached.is_empty(),
        "the refused PATCH still attached a channel: {attached:?}"
    );
    drop(conn);

    // The other three levers the same request can pull, each individually
    // enough to turn a dormant rule into a live feed. Asserted separately so a
    // partial fix that only guarded `channel_ids` fails here rather than
    // passing the test above.
    for body in [
        json!({ "enabled": true }),
        json!({ "conditions": { "threshold": 1 } }),
        json!({ "message_template": "{{issue_title}}" }),
        json!({ "name": "renamed" }),
    ] {
        let (status, text, _) = server
            .patch_raw(
                &format!("/v1/alert-rules/{rule_id}"),
                &fx.mallory_token,
                body.clone(),
            )
            .await;
        assert_eq!(status, 403, "PATCH {body} on an unreadable target: {text}");
    }

    server.shutdown().await;
}

#[tokio::test]
async fn editing_a_rule_over_a_readable_target_still_works() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "editok").await;

    // The control for the test above, and the reason it cannot be satisfied by
    // making `update_rule` refuse everyone: Mallory reads project A, so she may
    // both create a rule there and go on editing it. Without this, a blanket
    // 403 in `update_rule` would look like a passing security fix and only
    // surface when an on-call rotation could not be adjusted.
    let rule_id = create_rule_as_owner(
        &server,
        &fx,
        json!({ "name": "readable", "trigger_type": "issue_new", "app_id": fx.app_a }),
    )
    .await;
    let hook = create_channel(
        &server,
        &fx,
        json!({
            "name": "team hook",
            "kind": "webhook",
            "config": { "url": "https://team.example/hook" },
        }),
    )
    .await;

    let (status, text, updated) = server
        .patch_raw(
            &format!("/v1/alert-rules/{rule_id}"),
            &fx.mallory_token,
            json!({ "name": "tuned", "channel_ids": [hook], "throttle_seconds": 60 }),
        )
        .await;
    assert_eq!(status, 200, "editing a readable target: {text}");
    assert_eq!(updated["name"], json!("tuned"), "body: {text}");
    assert_eq!(updated["channel_ids"], json!([hook]), "body: {text}");

    server.shutdown().await;
}

#[tokio::test]
async fn an_app_narrowed_monitor_rule_is_authorized_at_project_scope() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "monradius").await;

    // First, the fact the authorization has to match: `monitors` has no
    // `app_id` column at all, so `repo::alert_rules_for_monitor` matches on
    // org + project and never consults `alert_rules.app_id`. An app-narrowed
    // `monitor_down` rule therefore fires for EVERY monitor in its project —
    // its blast radius is the project, whatever the row says.
    let narrowed = create_rule_as_owner(
        &server,
        &fx,
        json!({ "name": "app-narrowed", "trigger_type": "monitor_down", "app_id": fx.app_a }),
    )
    .await;
    let mut conn = server.conn().await;
    let firing = repo::alert_rules_for_monitor(
        &mut conn,
        fx.project_a,
        uuid::Uuid::from_u128(7),
        "monitor_down",
    )
    .await
    .expect("alert_rules_for_monitor");
    assert!(
        firing.iter().any(|r| r.id.to_string() == narrowed),
        "an app-narrowed monitor rule is expected to fire project-wide; if this \
         now filters on app_id, re-narrow the check in `authorize_rule_target`"
    );
    drop(conn);

    // So app-scoped `monitor:read` must NOT be enough to create it: authorizing
    // at app scope would be narrower than what the rule delivers, which is the
    // subtle half of the finding — the caller passes an `authorize_app` check
    // and still receives telemetry from apps they cannot read.
    // `issue:read` rides along at the same scope purely so the trigger-specific
    // control at the end of this test uses the SAME principal and the SAME
    // scope, leaving the trigger as the only variable.
    let app_reader = user_with_read_at(
        &server,
        fx.org_id,
        "appmon",
        &[perm::MONITOR_READ, perm::ISSUE_READ],
        "app",
        fx.app_a,
    )
    .await;
    let before = rule_count(&server, fx.org_id).await;
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&app_reader),
            json!({ "name": "app-scoped", "trigger_type": "monitor_down", "app_id": fx.app_a }),
        )
        .await;
    assert_eq!(
        status, 403,
        "app-scoped monitor:read must not authorize a project-wide radius: {text}"
    );
    assert_eq!(rule_count(&server, fx.org_id).await, before, "row inserted");

    // The control: the SAME request from a caller holding `monitor:read` at
    // project scope succeeds. Without it the assertion above would also pass
    // against a change that simply refused app-narrowed monitor rules outright,
    // which would 400 every such rule already in the table.
    let project_reader = user_with_read_at(
        &server,
        fx.org_id,
        "projmon",
        &[perm::MONITOR_READ],
        "project",
        fx.project_a,
    )
    .await;
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&project_reader),
            json!({ "name": "proj-scoped", "trigger_type": "monitor_down", "app_id": fx.app_a }),
        )
        .await;
    assert_eq!(
        status, 200,
        "project-scoped monitor:read matches the radius and must be allowed: {text}"
    );

    // And the widening is trigger-specific: an ISSUE rule genuinely is
    // app-narrowed at evaluation time (`apps_in_alert_scope` filters on
    // `apps.id`), so app-scoped read still authorizes it. Collapsing app→project
    // for every trigger would be a gratuitous permission escalation demand.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&app_reader),
            json!({ "name": "issue-app", "trigger_type": "issue_new", "app_id": fx.app_a }),
        )
        .await;
    assert_eq!(
        status, 200,
        "an issue rule really is app-scoped and must stay creatable: {text}"
    );

    server.shutdown().await;
}

// --- update_channel: the D9 exfiltration from the destination end ------------
//
// `update_rule` refuses to point YOUR rule at a channel you may not reach.
// These cover the mirror image: pointing a channel SOMEONE ELSE'S rule already
// uses at a destination you own. Both moves deliver the same protected titles to
// the same attacker; only the edited row differs.

#[tokio::test]
async fn repointing_a_channel_used_by_an_unreadable_rule_is_refused() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "retarget").await;

    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "ops-webhook",
            "kind": "webhook",
            "config": { "url": "https://ops.example.com/hook" },
        }),
    )
    .await;

    // The owner aims a rule at project B at issue signal. Mallory holds
    // org-scoped alert:read+alert:write but reads project A only.
    let rule_id = create_rule_as_owner(
        &server,
        &fx,
        json!({
            "name": "b-issues",
            "trigger_type": "issue_new",
            "project_id": fx.project_b,
            "channel_ids": [channel_id],
        }),
    )
    .await;
    assert!(!rule_id.is_empty());

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "config": { "url": "https://mallory.example.net/collect" } }),
        )
        .await;
    assert_eq!(
        status, 403,
        "re-aiming a channel that carries project B's issue titles must be refused: {text}"
    );
    assert!(
        text.contains("b-issues"),
        "the refusal must name the blocking rule, or a legitimate admin cannot tell \
         which attachment is the obstacle: {text}"
    );

    // And the destination did NOT move. Asserted separately because a route that
    // wrote first and refused afterwards would still return 403, and the status
    // alone cannot tell the two apart.
    let (status, text, v) = server
        .get_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "owner reads the channel back: {text}");
    assert_ne!(
        v["config"]["url"].as_str(),
        Some("https://mallory.example.net/collect"),
        "the refusal must be a refusal, not a 403 returned after the write: {v}"
    );

    server.shutdown().await;
}

/// The secret bundle is the destination for Slack and Discord (`webhook_url`),
/// so gating `config` alone would leave the two most common kinds redirectable.
#[tokio::test]
async fn repointing_via_the_secret_bundle_is_refused_too() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "retarget-secret").await;

    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "slack-ops",
            "kind": "slack",
            "secret": { "webhook_url": "https://hooks.slack.com/services/AAA/BBB/CCC" },
        }),
    )
    .await;
    let _ = create_rule_as_owner(
        &server,
        &fx,
        json!({
            "name": "b-issues-slack",
            "trigger_type": "issue_new",
            "project_id": fx.project_b,
            "channel_ids": [channel_id],
        }),
    )
    .await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "secret": { "webhook_url": "https://hooks.slack.com/services/X/Y/Z" } }),
        )
        .await;
    assert_eq!(
        status, 403,
        "for Slack the secret IS the destination, so it must be gated like config: {text}"
    );

    server.shutdown().await;
}

/// The gate is scoped to destination changes. A rename moves no data, and
/// 403-ing it would make a shared channel uneditable for cosmetic reasons.
#[tokio::test]
async fn renaming_or_disabling_a_shared_channel_stays_allowed() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "retarget-rename").await;

    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "ops-webhook",
            "kind": "webhook",
            "config": { "url": "https://ops.example.com/hook" },
        }),
    )
    .await;
    let _ = create_rule_as_owner(
        &server,
        &fx,
        json!({
            "name": "b-issues",
            "trigger_type": "issue_new",
            "project_id": fx.project_b,
            "channel_ids": [channel_id],
        }),
    )
    .await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "name": "ops webhook (primary)", "enabled": false }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a rename/disable redirects nothing and must not be gated: {text}"
    );

    server.shutdown().await;
}

/// A rule over a scope the caller CAN read does not block them, so the check is
/// an authorization and not a blanket "shared channels are frozen".
#[tokio::test]
async fn repointing_a_channel_whose_rules_are_all_readable_is_allowed() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "retarget-ok").await;

    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "ops-webhook",
            "kind": "webhook",
            "config": { "url": "https://ops.example.com/hook" },
        }),
    )
    .await;
    // Project A — which mallory reads.
    let _ = create_rule_as_owner(
        &server,
        &fx,
        json!({
            "name": "a-issues",
            "trigger_type": "issue_new",
            "project_id": fx.project_a,
            "channel_ids": [channel_id],
        }),
    )
    .await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "config": { "url": "https://ops.example.com/hook2" } }),
        )
        .await;
    assert_eq!(
        status, 200,
        "mallory reads project A, so this redirect discloses nothing new: {text}"
    );

    server.shutdown().await;
}

/// An unattached channel carries nothing, so it must stay freely editable —
/// otherwise the ordinary create-then-configure flow would demand read access to
/// data the channel will never hold.
#[tokio::test]
async fn a_channel_with_no_rules_is_freely_editable() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "retarget-bare").await;

    let channel_id = create_channel(
        &server,
        &fx,
        json!({
            "name": "fresh",
            "kind": "webhook",
            "config": { "url": "https://ops.example.com/hook" },
        }),
    )
    .await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/notification-channels/{channel_id}"),
            &fx.mallory_token,
            json!({ "config": { "url": "https://ops.example.com/other" } }),
        )
        .await;
    assert_eq!(
        status, 200,
        "no rule delivers here, so there is no telemetry to redirect: {text}"
    );

    server.shutdown().await;
}

// --- list_history: the durable copy of what D9 protects ----------------------
//
// `AlertEngine::log_event` writes the rule's rendered title and body into
// `alert_events`, and issue triggers render the verbatim issue title. Serving
// that table on org-scoped `alert:read` alone hands every holder a permanent
// transcript of signal they may not read directly — the rule-target gate
// undone from the read side, after the fact, with the rule still innocent.

/// Insert one history row directly, so the test does not depend on the evaluator
/// loop firing. `rule_id: None` models a row whose rule was later deleted
/// (`ON DELETE SET NULL`).
async fn seed_history(
    server: &TestServer,
    org_id: Uuid,
    rule_id: Option<Uuid>,
    trigger_type: &str,
    title: &str,
) {
    let mut conn = server.conn().await;
    repo::insert_alert_event(
        &mut conn,
        sauron_db::models::NewAlertEvent {
            org_id,
            rule_id,
            channel_id: None,
            trigger_type,
            dedup_key: &format!("test-{}", Uuid::new_v4().simple()),
            status: "sent",
            title,
            body: "body",
            error: None,
            attempts: 1,
        },
    )
    .await
    .expect("insert alert event");
}

async fn history_titles(server: &TestServer, org_id: Uuid, token: &str) -> Vec<String> {
    let (status, text, v) = server
        .get_raw(&format!("/v1/orgs/{org_id}/alert-events?limit=200"), token)
        .await;
    assert_eq!(status, 200, "history is readable on alert:read: {text}");
    v.as_array()
        .expect("history is an array")
        .iter()
        .map(|r| r["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn history_withholds_rows_whose_rule_target_is_unreadable() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "history").await;

    // Two rules, same org: one over project A (mallory reads it), one over
    // project B (she does not).
    let readable: Uuid = create_rule_as_owner(
        &server,
        &fx,
        json!({ "name": "a-issues", "trigger_type": "issue_new", "project_id": fx.project_a }),
    )
    .await
    .parse()
    .expect("rule id");
    let withheld: Uuid = create_rule_as_owner(
        &server,
        &fx,
        json!({ "name": "b-issues", "trigger_type": "issue_new", "project_id": fx.project_b }),
    )
    .await
    .parse()
    .expect("rule id");

    seed_history(&server, fx.org_id, Some(readable), "issue_new", "A: boom").await;
    seed_history(
        &server,
        fx.org_id,
        Some(withheld),
        "issue_new",
        "B: secret boom",
    )
    .await;

    let owner = history_titles(&server, fx.org_id, &fx.owner_token).await;
    assert!(
        owner.contains(&"A: boom".to_string()) && owner.contains(&"B: secret boom".to_string()),
        "the owner reads both projects and must still see everything: {owner:?}"
    );

    let mallory = history_titles(&server, fx.org_id, &fx.mallory_token).await;
    assert!(
        mallory.contains(&"A: boom".to_string()),
        "project A is readable, so its history must remain visible: {mallory:?}"
    );
    assert!(
        !mallory.contains(&"B: secret boom".to_string()),
        "project B's issue title must not be served to a caller who cannot read \
         project B: {mallory:?}"
    );

    server.shutdown().await;
}

/// `rule_id` is `ON DELETE SET NULL`, so **deleting the rule is the laundering
/// step**: fire it, delete it, then read the transcript. Without the orphan arm
/// this whole fix would be one DELETE away from irrelevant.
#[tokio::test]
async fn deleting_the_rule_does_not_unlock_its_history() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "history-orphan").await;

    // An orphan carrying issue signal. Mallory holds issue:read on project A
    // ONLY, so she lacks org-scoped issue:read — which is the widest reading, and
    // the one an orphan is judged at.
    seed_history(&server, fx.org_id, None, "issue_new", "orphan: secret boom").await;

    let mallory = history_titles(&server, fx.org_id, &fx.mallory_token).await;
    assert!(
        !mallory.contains(&"orphan: secret boom".to_string()),
        "a row whose rule was deleted has no target left to check, so it must be \
         judged at the widest scope, not waved through: {mallory:?}"
    );

    let owner = history_titles(&server, fx.org_id, &fx.owner_token).await;
    assert!(
        owner.contains(&"orphan: secret boom".to_string()),
        "the owner does hold org-wide read, so orphans stay visible to them — \
         hiding them from everyone would lose real history: {owner:?}"
    );

    server.shutdown().await;
}

/// Monitor triggers disclose probed hostnames, not issue titles, so they are
/// judged on `monitor:read`. A principal with org-wide `issue:read` and no
/// monitor rights must not inherit them.
#[tokio::test]
async fn history_visibility_follows_the_trigger_s_own_permission() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "history-perm").await;

    // Org-wide issue:read, deliberately no monitor:read.
    let issue_reader = user_with_read_at(
        &server,
        fx.org_id,
        "issues-only",
        &[perm::ISSUE_READ, perm::APP_READ],
        "org",
        fx.org_id,
    )
    .await;

    seed_history(&server, fx.org_id, None, "issue_new", "orphan issue").await;
    seed_history(
        &server,
        fx.org_id,
        None,
        "monitor_down",
        "orphan monitor: db-01.internal",
    )
    .await;

    let titles = history_titles(&server, fx.org_id, &issue_reader).await;
    assert!(
        titles.contains(&"orphan issue".to_string()),
        "org-wide issue:read covers issue signal: {titles:?}"
    );
    assert!(
        !titles.contains(&"orphan monitor: db-01.internal".to_string()),
        "a monitor row leaks an internal hostname and needs monitor:read, which \
         issue:read must not stand in for: {titles:?}"
    );

    server.shutdown().await;
}

/// The `Fixture` seeds projects and apps but no monitors, and a pinned rule
/// needs a real one because the column is a foreign key.
async fn create_monitor(server: &TestServer, token: &str, project_id: Uuid, name: &str) -> Uuid {
    let (_text, body) = server
        .post_ok(
            &format!("/v1/projects/{project_id}/monitors"),
            token,
            json!({
                "name": name,
                "kind": "http",
                "target": "https://example.test/health",
            }),
        )
        .await;
    body["id"].as_str().unwrap().parse().unwrap()
}

/// A monitor from another org must never become a rule's target: the rule's
/// `project_id` is DERIVED from the monitor, so accepting a foreign one would
/// hand the caller a rule scoped outside the org they authorized against.
#[tokio::test]
async fn a_rule_cannot_be_pinned_to_a_monitor_outside_the_org() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pinorg").await;
    let other = seed(&server, "pinorg-other").await;

    let foreign = create_monitor(&server, &other.owner_token, other.project_a, "foreign").await;

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.owner_token),
            json!({
                "name": "cross-org",
                "trigger_type": "monitor_down",
                "monitor_id": foreign,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "monitor from another org must be rejected: {text}"
    );

    server.shutdown().await;
}

/// `monitor_id` is meaningless on a trigger that never reads it. Rejecting at
/// the API keeps the CHECK constraint as a backstop rather than the only guard
/// — a 500 from a constraint violation is not an answer a caller can act on.
#[tokio::test]
async fn a_monitor_id_on_a_non_monitor_trigger_is_rejected() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pintrigger").await;
    let mon = create_monitor(&server, &fx.owner_token, fx.project_a, "api").await;

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.owner_token),
            json!({
                "name": "wrong trigger",
                "trigger_type": "issue_new",
                "monitor_id": mon,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "monitor_id on issue_new must be rejected: {text}"
    );

    server.shutdown().await;
}

/// Pinning must narrow, never widen: the derived `project_id` is what the
/// existing `authorize_rule_target` gate then checks `monitor:read` against.
#[tokio::test]
async fn pinning_a_monitor_derives_the_project_and_is_authorized_there() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pinderive").await;
    let mon_a = create_monitor(&server, &fx.owner_token, fx.project_a, "a-health").await;
    let mon_b = create_monitor(&server, &fx.owner_token, fx.project_b, "b-health").await;

    // Mallory holds no monitor:read anywhere, so she is the wrong witness here:
    // every arm of `authorize_rule_target` denies her regardless of which
    // project the monitor derives to, which would make a 403 mean nothing.
    // `monitor_reader` holds monitor:read at project A ONLY, so her 403/200
    // split can only be explained by the derived project, not by a missing
    // permission.
    let monitor_reader = user_with_read_at(
        &server,
        fx.org_id,
        "pinderive-reader",
        &[perm::MONITOR_READ],
        "project",
        fx.project_a,
    )
    .await;

    // A monitor-B-pinned rule must be refused even though she never named
    // project B in the request: the project is derived from the monitor, and
    // she cannot read at project B.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&monitor_reader),
            json!({
                "name": "sneaky",
                "trigger_type": "monitor_down",
                "monitor_id": mon_b,
            }),
        )
        .await;
    assert_eq!(
        status, 403,
        "pinning must be authorized at the monitor's project: {text}"
    );

    // The control: the SAME caller, a monitor-A-pinned rule, is allowed — proof
    // that the 403 above is about the derived project, not about a missing
    // monitor:read grant altogether.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&monitor_reader),
            json!({
                "name": "a down",
                "trigger_type": "monitor_down",
                "monitor_id": mon_a,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "monitor:read at the derived project must be enough: {text}"
    );

    // And the owner's pinned rule stores both the monitor and the derived project.
    let (_text, body) = server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.owner_token,
            json!({
                "name": "b down",
                "trigger_type": "monitor_down",
                "monitor_id": mon_b,
            }),
        )
        .await;
    assert_eq!(
        body["monitor_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
        mon_b
    );
    assert_eq!(
        body["project_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
        fx.project_b,
        "the project must be derived from the monitor, not left NULL"
    );

    server.shutdown().await;
}

/// `alert_rules.monitor_id` is `ON DELETE CASCADE` — deleting the monitor a
/// rule is pinned to silently deletes the rule too. That cascade is correct
/// (a `SET NULL` would widen the rule instead), but it must be DISCLOSED: the
/// delete response should tell the caller how many alert rules went with it,
/// and the rule really must be gone afterward.
///
/// If the disclosure were removed (the response reverted to plain
/// `{"ok": true}`), the `cascaded_alert_rules` assertion below would fail —
/// that's the point of asserting on the field, not just on the row count.
#[tokio::test]
async fn deleting_a_monitor_discloses_and_cascades_its_pinned_alert_rule() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pincascade").await;
    let mon = create_monitor(&server, &fx.owner_token, fx.project_a, "cascade-target").await;

    let (_text, rule) = server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.owner_token,
            json!({
                "name": "cascade rule",
                "trigger_type": "monitor_down",
                "monitor_id": mon,
            }),
        )
        .await;
    let rule_id = rule["id"].as_str().unwrap().to_string();

    // A second rule, un-pinned (org-wide), must survive the delete untouched —
    // proof that the cascade (and its count) is scoped to the pinned rule,
    // not to every rule in the org.
    server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.owner_token,
            json!({ "name": "unrelated rule", "trigger_type": "monitor_down" }),
        )
        .await;

    let (status, text, body) = server
        .delete_raw(&format!("/v1/monitors/{mon}"), &fx.owner_token)
        .await;
    assert_eq!(status, 200, "monitor delete failed: {text}");
    assert_eq!(
        body["cascaded_alert_rules"].as_i64(),
        Some(1),
        "delete response must disclose exactly the 1 pinned rule that cascaded: {text}"
    );

    let (status, text, _) = server
        .get_raw(&format!("/v1/alert-rules/{rule_id}"), &fx.owner_token)
        .await;
    assert_eq!(
        status, 404,
        "the pinned rule must actually be gone after the cascade: {text}"
    );

    assert_eq!(
        rule_count(&server, fx.org_id).await,
        1,
        "only the unrelated, un-pinned rule should remain"
    );

    server.shutdown().await;
}
