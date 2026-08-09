//! HTTP-level tests for the credential redaction on the monitor routes,
//! driven through the real router.
//!
//! `GET /v1/monitors/{monitor_id}` serialized the whole `Monitor` row, and some
//! of its fields are credentials rather than settings:
//!
//!  * `webhook_url` — a Slack/Discord/PagerDuty hook is bearer-equivalent.
//!    There is no second factor; possession of the URL *is* the authority to
//!    post into that channel.
//!  * `config.headers` and `config.body` — `bins/sauron-monitor`'s `spec_of`
//!    copies both verbatim into the outbound probe request, so an
//!    `Authorization: Bearer …` header, or the password in a login probe's
//!    body, is a live credential.
//!  * anything else in `config`. It is free-form JSONB with no schema, and the
//!    first fix stripped exactly `headers` — a denylist, so every key added
//!    later shipped as a leak. The projection is now an allowlist
//!    (`sauron_db::models::PUBLIC_PROBE_CONFIG_KEYS`), derived from the only two
//!    real consumers: `spec_of`, and a dashboard that reads no config key at all.
//!
//! All of it was readable by anyone holding `monitor:read`, which the preset
//! *Viewer* role carries (`sauron_auth::rbac::VIEWER`). Nothing in the SPA ever
//! rendered any of these values, so the leak was invisible from the UI and only
//! observable on the wire — which is exactly why it needs a wire-level test
//! rather than a component one.
//!
//! What each test pins:
//!
//!  1. a Viewer reading the detail route gets neither secret *anywhere* in the
//!     raw body, gets the `has_webhook` / `probe_header_names` existence
//!     signals instead, still gets the non-secret half of `config` — and the
//!     row in Postgres is untouched, so the prober can still authenticate;
//!  2. the negative twin: a monitor configured with neither reports neither,
//!     so test 1 is not passing on a field that is simply always absent;
//!  3. `create` and `update` — both `monitor:write` — do not echo the URL back
//!     either, which is what stops the model-layer redaction from being
//!     quietly downgraded to a handler-only strip in `detail`;
//!  4. the project-scoped `list` route is unaffected, proving the redaction
//!     did not over-reach into `MonitorListRow`;
//!  5. the allowlist itself: a credential-shaped key the server has never heard
//!     of, plus the probe body, are absent from the response while the three
//!     allowlisted probe settings survive — and both omissions carry an
//!     existence signal, plus its negative twin.
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

use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-monitors-test-secret-000000000000000";

/// Likewise. Required (not optional) since the notification-channel key was
/// made fail-closed: `sauron-api` refuses to boot without it, so a harness
/// that omits it dies at startup with a config error rather than anything to
/// do with the routes under test.
const NOTIFY_SECRET_KEY: &str = "http-monitors-test-notify-key-0000000000";

/// The values that must never appear in a response body. Distinctive enough
/// that a substring search over the raw JSON is a meaningful assertion: if any
/// of them ever shows up nested somewhere new, the search finds it without the
/// test having to know the shape it arrived in.
const WEBHOOK_URL: &str = "https://hooks.example.com/services/T000/B000/SUPERSECRET";
const PROBE_TOKEN: &str = "Bearer probe-token-6f3a9c1d";
/// The probe's request body. Credential-bearing for the same reason `headers`
/// is: `spec_of` sends it verbatim, so a monitor that probes a login endpoint
/// puts the password here.
///
/// Form-encoded rather than JSON deliberately — a body containing `"` would be
/// escaped in the response's JSON and the `raw.contains(...)` search below would
/// miss a real leak. The assertion has to be able to fail.
const PROBE_BODY: &str = "user=probe&password=body-secret-4b81e0";
/// Stands in for a `config` key the current server knows nothing about — the
/// case an allowlist handles and a strip-list does not.
const FUTURE_SECRET: &str = "sk-future-key-e19d7a2c";

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
        // The probe listener is dropped on return, so a concurrent
        // `TestServer::start()` on another thread can be handed the same port
        // before this process's child binds it. The registry rules out ports
        // this process has already issued to itself.
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
        // glued to the uuid. `sauron-db`'s `tests/common::reap_stale_test_
        // databases` parses the first underscore-delimited segment after
        // `sauron_test_` as a timestamp and silently skips anything else, so
        // a differently ordered name leaks every database it creates.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "mon" (3) +
        // 32-hex uuid = 58 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_mon{}",
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
            .env("NOTIFY_SECRET_KEY", NOTIFY_SECRET_KEY)
            .env("API_PORT", port.to_string())
            .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
            // Paired with the per-server `X-Forwarded-For` below. Redis (and
            // therefore the register-rate-limit bucket, keyed on caller IP)
            // is shared by every test binary on this host and is not reset
            // per test, so without a private bucket a rerun inside the hour
            // 429s.
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

    /// GET `path` and return `(status, raw body text, parsed JSON)`.
    ///
    /// The raw text is returned alongside the parsed value on purpose: the
    /// strongest assertion available here is "this secret string appears
    /// nowhere in the response", which a field-by-field walk of the parsed
    /// tree cannot make.
    async fn get_raw(&self, path: &str, token: &str) -> (u16, String, Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: failed to read body (status {status}): {e}"));
        let body = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("GET {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        });
        (status, text, body)
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

    /// POST `path` expecting success, returning `(raw body text, parsed JSON)`.
    async fn post_ok(&self, path: &str, token: &str, body: Value) -> (String, Value) {
        let resp = self.post_json(path, Some(token), body).await;
        let status = resp.status();
        let text = resp.text().await.expect("read POST body");
        assert!(status.is_success(), "POST {path} failed ({status}): {text}");
        let v = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("POST {path}: expected JSON: {e}\nbody: {text}"));
        (text, v)
    }

    /// PATCH `path` expecting success, returning `(raw body text, parsed JSON)`.
    async fn patch_ok(&self, path: &str, token: &str, body: Value) -> (String, Value) {
        let resp = self
            .client
            .patch(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("PATCH {path} failed: {e}"));
        let status = resp.status();
        let text = resp.text().await.expect("read PATCH body");
        assert!(
            status.is_success(),
            "PATCH {path} failed ({status}): {text}"
        );
        let v = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("PATCH {path}: expected JSON: {e}\nbody: {text}"));
        (text, v)
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

/// A registered org owner plus a project of theirs, and a second user holding
/// the *real* shipped `Viewer` preset at that project and nothing else.
///
/// Granting the shipped preset rather than a hand-rolled `["monitor:read"]`
/// role is the point of the fixture: the finding is that *Viewer* can read
/// these credentials, so if `rbac::VIEWER` ever stops carrying `monitor:read`
/// these tests should start exercising a 403 rather than silently keep
/// passing against an imitation of a role nobody has.
struct Fixture {
    project_id: Uuid,
    owner_token: String,
    viewer_token: String,
}

async fn seed(server: &TestServer, label: &str) -> Fixture {
    let email = format!("{label}-owner-{}@example.com", Uuid::new_v4().simple());
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
    let owner_id: Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let mut conn = server.conn().await;
    let org_id = repo::list_orgs_for_user(&mut conn, owner_id)
        .await
        .expect("list orgs")
        .first()
        .expect("owner has an org")
        .id;
    let project = repo::create_project(
        &mut conn,
        org_id,
        "uptime",
        &format!("uptime-{}", Uuid::new_v4().simple()),
    )
    .await
    .expect("create project");

    let viewer_role = repo::get_system_role(&mut conn, "Viewer")
        .await
        .expect("get_system_role")
        .expect("Viewer preset is seeded at API boot");
    let viewer_email = format!("{label}-viewer-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let viewer = repo::create_user(&mut conn, &viewer_email, &hash, "viewer")
        .await
        .expect("create viewer");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: viewer.id,
            role_id: viewer_role.id,
            scope_type: "project".to_string(),
            scope_id: project.id,
        },
    )
    .await
    .expect("grant Viewer at project scope");
    drop(conn);

    Fixture {
        project_id: project.id,
        owner_token: server.login(&email, PASSWORD).await,
        viewer_token: server.login(&viewer_email, PASSWORD).await,
    }
}

/// Create a monitor as the owner and return its id. `webhook`/`headers` are
/// the two credential-bearing inputs; either can be omitted.
async fn create_monitor(
    server: &TestServer,
    fx: &Fixture,
    name: &str,
    webhook: Option<&str>,
    headers: Option<Value>,
) -> (Uuid, String, Value) {
    let mut config = json!({ "expected_status": 204 });
    if let Some(h) = headers {
        config["headers"] = h;
    }
    let mut body = json!({
        "name": name,
        "kind": "http",
        "target": "https://example.com/health",
        "config": config,
    });
    if let Some(w) = webhook {
        body["webhook_url"] = json!(w);
    }
    let (text, v) = server
        .post_ok(
            &format!("/v1/projects/{}/monitors", fx.project_id),
            &fx.owner_token,
            body,
        )
        .await;
    let id: Uuid = v["id"]
        .as_str()
        .expect("created monitor has an id")
        .parse()
        .unwrap();
    (id, text, v)
}

/// Create a monitor as the owner with a caller-supplied `config`, returning
/// `(id, raw create body, parsed create body)`.
///
/// Separate from `create_monitor` because the allowlist tests need to put keys
/// in `config` that no legitimate caller sends — the point being that a key the
/// server has never heard of must not come back out.
async fn create_monitor_with_config(
    server: &TestServer,
    fx: &Fixture,
    name: &str,
    config: Value,
) -> (Uuid, String, Value) {
    let (text, v) = server
        .post_ok(
            &format!("/v1/projects/{}/monitors", fx.project_id),
            &fx.owner_token,
            json!({
                "name": name,
                "kind": "http",
                "target": "https://example.com/health",
                "method": "POST",
                "config": config,
            }),
        )
        .await;
    let id: Uuid = v["id"]
        .as_str()
        .expect("created monitor has an id")
        .parse()
        .unwrap();
    (id, text, v)
}

/// Assert a serialized monitor object carries no credential, in the parsed tree
/// *and* in the raw bytes it came from.
fn assert_redacted(monitor: &Value, raw: &str, context: &str) {
    assert!(
        monitor.get("webhook_url").is_none(),
        "{context}: webhook_url is still serialized: {monitor}"
    );
    // Every key, not just `headers`: the projection is an allowlist, so the
    // assertion that matches it is "nothing outside the list survives". A
    // denylist test (`config.headers` is absent) passes for the next
    // credential-bearing key someone adds, which is the defect this replaces.
    if let Some(cfg) = monitor.get("config").and_then(|c| c.as_object()) {
        for key in cfg.keys() {
            assert!(
                sauron_db::models::PUBLIC_PROBE_CONFIG_KEYS.contains(&key.as_str()),
                "{context}: config carries the non-allowlisted key {key:?}: {monitor}"
            );
        }
    }
    // The substring checks are the ones that survive a refactor: they hold
    // wherever in the payload the value might reappear, including under a key
    // this test has never heard of.
    assert!(
        !raw.contains(WEBHOOK_URL),
        "{context}: the webhook URL appears in the raw body:\n{raw}"
    );
    assert!(
        !raw.contains(PROBE_TOKEN),
        "{context}: the probe token appears in the raw body:\n{raw}"
    );
}

#[tokio::test]
async fn viewer_reading_a_monitor_gets_the_signals_not_the_credentials() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "redact").await;
    let (monitor_id, _, _) = create_monitor(
        &server,
        &fx,
        "prod",
        Some(WEBHOOK_URL),
        Some(json!({ "Authorization": PROBE_TOKEN, "X-Api-Key": "k-123" })),
    )
    .await;

    let (status, raw, body) = server
        .get_raw(&format!("/v1/monitors/{monitor_id}"), &fx.viewer_token)
        .await;
    // 200, not 403: the Viewer is *supposed* to see this page. The fix is
    // about what the payload contains, not about who may fetch it — a test
    // that accepted a 403 here would pass against a regression that broke the
    // feature instead of securing it.
    assert_eq!(status, 200, "viewer detail read: {raw}");

    let monitor = &body["monitor"];
    assert_redacted(monitor, &raw, "viewer detail");

    // The existence signals that replace them.
    assert_eq!(monitor["has_webhook"], json!(true), "body: {raw}");
    assert_eq!(
        monitor["probe_header_names"],
        json!(["Authorization", "X-Api-Key"]),
        "header names are returned sorted, values are not returned at all: {raw}"
    );

    // The non-secret half of `config` must survive — redacting the whole
    // field would silently break every probe setting the page shows.
    assert_eq!(
        monitor["config"]["expected_status"],
        json!(204),
        "body: {raw}"
    );
    assert_eq!(monitor["target"], json!("https://example.com/health"));

    // And the redaction is serializer-only: the prober reads these columns
    // straight from Postgres, so losing them at rest would take uptime
    // notification and probe authentication down with the leak.
    let mut conn = server.conn().await;
    let row = repo::get_monitor(&mut conn, monitor_id)
        .await
        .expect("get_monitor")
        .expect("monitor row");
    assert_eq!(row.webhook_url.as_deref(), Some(WEBHOOK_URL));
    assert_eq!(row.config["headers"]["Authorization"], json!(PROBE_TOKEN));
    drop(conn);

    server.shutdown().await;
}

#[tokio::test]
async fn a_monitor_without_credentials_reports_them_absent() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "nocred").await;
    let (monitor_id, _, _) = create_monitor(&server, &fx, "plain", None, None).await;

    let (status, raw, body) = server
        .get_raw(&format!("/v1/monitors/{monitor_id}"), &fx.viewer_token)
        .await;
    assert_eq!(status, 200, "viewer detail read: {raw}");

    // The negative twin of the test above: without it, both assertions there
    // would still pass if the handler hard-coded `true` / a constant list.
    assert_eq!(body["monitor"]["has_webhook"], json!(false), "body: {raw}");
    assert_eq!(
        body["monitor"]["probe_header_names"],
        json!([]),
        "body: {raw}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn create_and_update_do_not_echo_the_webhook_back() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "echo").await;

    // The caller supplied the URL, so echoing it is not a privilege
    // violation — but these two routes are the reason the redaction lives on
    // the model rather than in `detail`. If someone later "simplifies" it to a
    // handler-level strip, these two assertions are what fails.
    let (create_raw, created) = {
        let (id, raw, v) = create_monitor(
            &server,
            &fx,
            "echoed",
            Some(WEBHOOK_URL),
            Some(json!({ "Authorization": PROBE_TOKEN })),
        )
        .await;
        assert_eq!(v["id"].as_str().unwrap().parse::<Uuid>().unwrap(), id);
        (raw, v)
    };
    assert_redacted(&created, &create_raw, "create response");
    assert_eq!(created["has_webhook"], json!(true), "body: {create_raw}");

    let monitor_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    // Omitting the key leaves the stored URL alone — the three-state contract
    // on `UpdateMonitorReq.webhook_url` is what lets the edit form work
    // without ever reading the current value back.
    let (raw, updated) = server
        .patch_ok(
            &format!("/v1/monitors/{monitor_id}"),
            &fx.owner_token,
            json!({ "name": "renamed" }),
        )
        .await;
    assert_redacted(&updated, &raw, "update response");
    assert_eq!(updated["name"], json!("renamed"), "body: {raw}");
    assert_eq!(
        updated["has_webhook"],
        json!(true),
        "omitting webhook_url must not clear it: {raw}"
    );

    // Explicit null clears it, and the signal follows.
    let (raw, cleared) = server
        .patch_ok(
            &format!("/v1/monitors/{monitor_id}"),
            &fx.owner_token,
            json!({ "webhook_url": null }),
        )
        .await;
    assert_eq!(cleared["has_webhook"], json!(false), "body: {raw}");

    server.shutdown().await;
}

#[tokio::test]
async fn the_project_monitor_list_is_unaffected_by_the_redaction() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "list").await;
    let (monitor_id, _, _) = create_monitor(
        &server,
        &fx,
        "listed",
        Some(WEBHOOK_URL),
        Some(json!({ "Authorization": PROBE_TOKEN })),
    )
    .await;

    // `list` returns `MonitorListRow`, which never selected the webhook
    // column — this asserts the fix did not over-reach and start blanking
    // fields the Uptime page needs.
    let (status, raw, body) = server
        .get_raw(
            &format!("/v1/projects/{}/monitors", fx.project_id),
            &fx.viewer_token,
        )
        .await;
    assert_eq!(status, 200, "viewer list read: {raw}");
    let row = body
        .as_array()
        .expect("list returns an array")
        .iter()
        .find(|r| r["id"] == json!(monitor_id))
        .unwrap_or_else(|| panic!("created monitor missing from list: {raw}"));
    assert_eq!(row["name"], json!("listed"));
    assert_eq!(row["kind"], json!("http"));
    assert_eq!(row["target"], json!("https://example.com/health"));
    assert!(
        !raw.contains(WEBHOOK_URL) && !raw.contains(PROBE_TOKEN),
        "list body carries a credential:\n{raw}"
    );

    server.shutdown().await;
}

// --- the config projection is an allowlist, not a strip-list -----------------

#[tokio::test]
async fn config_keys_outside_the_allowlist_never_reach_the_api() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "allowlist").await;

    // Three classes of key in one config:
    //
    //  1. the allowlisted probe settings, which must survive;
    //  2. the two verbatim-request fields — `headers` and `body`. A monitor that
    //     probes an authenticated endpoint carries the credential in one or the
    //     other, and `spec_of` copies both into the outbound request byte for
    //     byte, so neither is "settings";
    //  3. `bearer_token` — a key the server has never heard of, standing in for
    //     the next field someone adds to this free-form JSONB column. Under the
    //     old strip-one-key serializer this went straight out to any
    //     `monitor:read` holder, which the preset Viewer is. That is the whole
    //     finding: a denylist is default-open, and `config` has no schema to
    //     bound what lands in it.
    let (monitor_id, _, _) = create_monitor_with_config(
        &server,
        &fx,
        "authed",
        json!({
            "expected_status": "200-299",
            "body_assertion": "\"ok\":true",
            "follow_redirects": false,
            "headers": { "Authorization": PROBE_TOKEN },
            "body": PROBE_BODY,
            "bearer_token": FUTURE_SECRET,
        }),
    )
    .await;

    let (status, raw, body) = server
        .get_raw(&format!("/v1/monitors/{monitor_id}"), &fx.viewer_token)
        .await;
    // 200, not 403: the Viewer is supposed to reach this page. The fix is about
    // what the payload contains.
    assert_eq!(status, 200, "viewer detail read: {raw}");
    let monitor = &body["monitor"];
    assert_redacted(monitor, &raw, "viewer detail with an unknown config key");

    // The substring checks are the load-bearing ones: they hold wherever in the
    // payload a value might reappear, including under a key this test has never
    // heard of, and they are what would catch a handler that re-attached the raw
    // row for convenience.
    for (label, secret) in [
        ("probe header value", PROBE_TOKEN),
        ("probe request body", PROBE_BODY),
        ("unknown credential-shaped key", FUTURE_SECRET),
    ] {
        assert!(
            !raw.contains(secret),
            "the {label} appears in the raw body:\n{raw}"
        );
    }
    assert!(
        monitor["config"].get("bearer_token").is_none(),
        "an unrecognised config key was passed through: {raw}"
    );

    // The allowlisted half must survive intact — a projection that dropped the
    // probe settings too would look like a passing security fix while making the
    // detail page useless.
    assert_eq!(
        monitor["config"],
        json!({
            "expected_status": "200-299",
            "body_assertion": "\"ok\":true",
            "follow_redirects": false,
        }),
        "config is exactly the allowlisted keys: {raw}"
    );

    // Existence signals stand in for the two request fields, so the omission is
    // visible rather than silent.
    assert_eq!(
        monitor["probe_header_names"],
        json!(["Authorization"]),
        "body: {raw}"
    );
    assert_eq!(monitor["has_probe_body"], json!(true), "body: {raw}");

    // And the projection is serializer-only. The prober reads these columns
    // straight from Postgres, so a redaction that reached the row would silently
    // stop authenticating the probe — the failure mode that looks like an
    // outage in the monitored service rather than a bug here.
    let mut conn = server.conn().await;
    let row = repo::get_monitor(&mut conn, monitor_id)
        .await
        .expect("get_monitor")
        .expect("monitor row");
    assert_eq!(row.config["headers"]["Authorization"], json!(PROBE_TOKEN));
    assert_eq!(row.config["body"], json!(PROBE_BODY));
    assert_eq!(row.config["bearer_token"], json!(FUTURE_SECRET));
    drop(conn);

    server.shutdown().await;
}

#[tokio::test]
async fn a_monitor_without_a_probe_body_reports_it_absent() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "nobody").await;
    let (monitor_id, _, _) = create_monitor_with_config(
        &server,
        &fx,
        "bodyless",
        json!({ "expected_status": "200-299" }),
    )
    .await;

    let (status, raw, body) = server
        .get_raw(&format!("/v1/monitors/{monitor_id}"), &fx.viewer_token)
        .await;
    assert_eq!(status, 200, "viewer detail read: {raw}");
    // The negative twin: without it, the `true` asserted above would still pass
    // against a handler that hard-coded the flag.
    assert_eq!(
        body["monitor"]["has_probe_body"],
        json!(false),
        "body: {raw}"
    );

    server.shutdown().await;
}
