//! HTTP-level tests for app-store install metrics, driven through the real
//! router against the actual compiled `sauron-api` binary.
//!
//! What each group pins:
//!
//! **Write-only credentials.** A Google service-account key and an Apple `.p8`
//! are full-strength credentials: the first reads a Play Console reports bucket,
//! the second signs App Store Connect API calls. `app:read` is a permission the
//! preset Viewer role carries, so any path that echoes the credential back —
//! under any key, including one nobody thought to check — hands it to every
//! viewer in the org. The assertions here are substring searches over the RAW
//! response body, not field walks: a field walk keeps passing on the day
//! someone adds the field back under a new name.
//!
//! **Partial updates.** The `secret` field is a *double* option: absent means
//! "leave the stored credential alone", explicit `null` means "clear it".
//! Collapsing those two is a silent credential wipe whose only symptom is a
//! sync that starts failing hours later, long after the edit that caused it.
//!
//! **Pending, not zero.** Days the store has not published yet are reported in
//! `pending_days` and omitted from `series`. Zero-filling them would assert
//! "nobody installed this app that day" — a confident lie, and the same
//! silent-drop class this codebase has been bitten by before.
//!
//! **Designation validity.** `store_environment_id` must be an enrollment of
//! the app it is set on. Storing a foreign UUID would hide the Overview section
//! forever with nothing to explain why.
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

const JWT_SECRET: &str = "http-stores-test-secret-000000000000000";
const NOTIFY_SECRET_KEY: &str = "http-stores-test-notify-key-0000000000000";
const PASSWORD: &str = "correct-horse-battery-staple";

/// The values that must never come back out. Distinctive enough that a
/// substring search over the raw body is a meaningful assertion wherever in the
/// payload they might reappear.
const GOOGLE_SA_KEY: &str = "PRIVATE-KEY-GOOGLE-DO-NOT-LEAK";
const APPLE_P8_KEY: &str = "PRIVATE-KEY-APPLE-DO-NOT-LEAK";

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
    db_name: String,
    pool: sauron_db::PgPool,
    cleaned_up: Cell<bool>,
}

impl TestServer {
    async fn start() -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

        // Segment order is load-bearing — timestamp FIRST, discriminator glued
        // to the uuid; `reap_stale_test_databases` parses the first segment as
        // a timestamp and silently skips anything else.
        let db_name = format!(
            "sauron_test_{}_str{}",
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
            db_name,
            pool,
            cleaned_up: Cell::new(false),
        })
    }

    async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    /// Returns `(status, raw body text, parsed JSON)`. The raw text is returned
    /// alongside the parsed value because the strongest assertion available
    /// here is "this secret appears nowhere in the response", which a
    /// field-by-field walk cannot make.
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
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }

    async fn put_raw(&self, path: &str, token: &str, body: Value) -> (u16, String, Value) {
        let resp = self
            .client
            .put(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("PUT {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read PUT body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }

    async fn post_raw(&self, path: &str, token: &str, body: Value) -> (u16, String, Value) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read POST body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
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

/// An org owner with two apps in separate projects, plus a viewer holding only
/// `app:read` — the permission that must be able to *see* a store connection
/// and must not be able to write one.
struct Fixture {
    app_a: Uuid,
    app_b: Uuid,
    env_a: Uuid,
    env_b: Uuid,
    owner_token: String,
    viewer_token: String,
}

async fn seed(server: &TestServer, label: &str) -> Fixture {
    let email = format!("{label}-owner-{}@example.com", Uuid::new_v4().simple());
    let (status, text, body) = server
        .post_raw_noauth(
            "/v1/auth/register",
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
        "flutter",
    )
    .await
    .expect("create app a");
    let app_b = repo::create_app(
        &mut conn,
        project_b.id,
        "app-b",
        &format!("b-{suffix}"),
        "flutter",
    )
    .await
    .expect("create app b");

    // Catalogue entry + enrollment, the two-level shape environments actually
    // have. `env_a`/`env_b` are ENROLLMENT ids — the id the dashboard's
    // switcher carries and the one `store_environment_id` must equal, not the
    // catalogue id. Returning the wrong one of the two would make the
    // designation test pass for the wrong reason.
    let env_a = seed_env(
        &mut conn,
        project_a.id,
        app_a.id,
        "production",
        &format!("pk-a-{suffix}"),
    )
    .await;
    let env_b = seed_env(
        &mut conn,
        project_b.id,
        app_b.id,
        "production",
        &format!("pk-b-{suffix}"),
    )
    .await;

    // `app:read` and nothing else — the narrowest principal that can reach the
    // list endpoint at all.
    let viewer_role = repo::create_role(
        &mut conn,
        org_id,
        "App Viewer",
        "reads app metadata, writes nothing",
        json!([perm::APP_READ]),
    )
    .await
    .expect("create viewer role");

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
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant the viewer role at org scope");
    drop(conn);

    Fixture {
        app_a: app_a.id,
        app_b: app_b.id,
        env_a,
        env_b,
        owner_token: server.login(&email, PASSWORD).await,
        viewer_token: server.login(&viewer_email, PASSWORD).await,
    }
}

impl TestServer {
    async fn post_raw_noauth(&self, path: &str, body: Value) -> (u16, String, Value) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read POST body");
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        (status, text, v)
    }
}

/// Define an environment on `project_id` and enroll `app_id` in it, returning
/// the ENROLLMENT id. Mirrors `sauron-db`'s `tests/common::seed_env`.
async fn seed_env(
    conn: &mut sauron_db::PgConn,
    project_id: Uuid,
    app_id: Uuid,
    name: &str,
    public_key: &str,
) -> Uuid {
    let env = repo::create_project_environment(conn, project_id, name)
        .await
        .unwrap_or_else(|e| panic!("create catalogue env {name}: {e}"));
    repo::create_app_environments(
        conn,
        &[sauron_db::models::NewAppEnvironment {
            app_id,
            environment_id: env.id,
            public_key,
            is_default: true,
        }],
    )
    .await
    .unwrap_or_else(|e| panic!("enroll app in {name}: {e}"))
    .remove(0)
    .id
}

fn google_ids() -> Value {
    json!({"package_name": "com.example.app", "gcs_bucket": "pubsite_prod_rev_01234"})
}

fn apple_ids() -> Value {
    json!({
        "bundle_id": "com.example.app",
        "apple_app_id": "1234567890",
        "issuer_id": "57246542-96fe-1a63-e053-0824d011072a",
        "key_id": "ABC123DEFG",
        "vendor_number": "0912345"
    })
}

// ---------------------------------------------------------------------------
// Write-only credentials
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stored_credential_never_appears_in_any_response() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "leak").await;

    // The PUT response itself is the first place a naive implementation echoes
    // the credential back.
    let (status, put_text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;
    assert_eq!(status, 200, "PUT google_play: {put_text}");
    assert!(
        !put_text.contains(GOOGLE_SA_KEY),
        "the PUT response echoed the credential: {put_text}"
    );

    server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/app_store", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": apple_ids(), "secret": APPLE_P8_KEY }),
        )
        .await;

    let (status, list_text, list) = server
        .get_raw(
            &format!("/v1/apps/{}/store-connections", fx.app_a),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "list: {list_text}");
    assert!(
        !list_text.contains(GOOGLE_SA_KEY) && !list_text.contains(APPLE_P8_KEY),
        "plaintext credential leaked in the list response: {list_text}"
    );
    assert!(
        !list_text.contains("secret_enc"),
        "ciphertext field leaked in the list response: {list_text}"
    );
    assert_eq!(list.as_array().expect("array").len(), 2);
    for c in list.as_array().unwrap() {
        assert_eq!(
            c["has_secret"],
            json!(true),
            "has_secret must report storage"
        );
    }

    // …and the chart feed embeds the same connection summaries.
    let (_, metrics_text, _) = server
        .get_raw(
            &format!("/v1/apps/{}/store-metrics?since_days=7", fx.app_a),
            &fx.owner_token,
        )
        .await;
    assert!(
        !metrics_text.contains(GOOGLE_SA_KEY) && !metrics_text.contains(APPLE_P8_KEY),
        "credential leaked through the metrics feed: {metrics_text}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn put_without_a_secret_field_preserves_the_stored_credential() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "preserve").await;
    let path = format!("/v1/apps/{}/store-connections/google_play", fx.app_a);

    server
        .put_raw(
            &path,
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;

    // Rename the package. No `secret` key at all — the shape the settings form
    // sends when the operator did not retype the credential.
    let renamed =
        json!({"package_name": "com.example.renamed", "gcs_bucket": "pubsite_prod_rev_01234"});
    let (status, text, body) = server
        .put_raw(&path, &fx.owner_token, json!({ "identifiers": renamed }))
        .await;
    assert_eq!(status, 200, "PUT rename: {text}");
    assert_eq!(
        body["has_secret"],
        json!(true),
        "editing identifiers wiped the credential: {text}"
    );
    assert_eq!(body["identifiers"]["package_name"], "com.example.renamed");

    // An explicit null is the one thing that clears it.
    let (status, text, body) = server
        .put_raw(
            &path,
            &fx.owner_token,
            json!({ "identifiers": renamed, "secret": Value::Null }),
        )
        .await;
    assert_eq!(status, 200, "PUT clear: {text}");
    assert_eq!(body["has_secret"], json!(false), "explicit null must clear");

    server.shutdown().await;
}

#[tokio::test]
async fn an_empty_secret_string_is_refused_rather_than_stored() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "empty").await;

    // A form that turns an untouched password field into "" would otherwise
    // store a credential that can never authenticate, and the failure would
    // surface hours later as a sync error.
    let (status, text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": "" }),
        )
        .await;
    assert_eq!(status, 400, "empty secret should be refused: {text}");

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_read_can_list_but_cannot_write_or_queue() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "rbac").await;

    let (status, _, _) = server
        .get_raw(
            &format!("/v1/apps/{}/store-connections", fx.app_a),
            &fx.viewer_token,
        )
        .await;
    assert_eq!(status, 200, "app:read must be able to see configuration");

    let (status, text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.viewer_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;
    assert_eq!(status, 403, "app:read must not write credentials: {text}");

    let (status, text, _) = server
        .delete_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.viewer_token,
        )
        .await;
    assert_eq!(status, 403, "app:read must not delete a connection: {text}");

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/apps/{}/store-connections/google_play/sync", fx.app_a),
            &fx.viewer_token,
            json!({}),
        )
        .await;
    assert_eq!(status, 403, "app:read must not queue a sync: {text}");

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Identifier validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identifiers_are_validated_against_the_store_slot() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "validate").await;

    // Apple identifiers posted to the Google slot must not be stored as a blob
    // the daemon can only choke on six hours later.
    let (status, text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": apple_ids() }),
        )
        .await;
    assert_eq!(status, 400, "mismatched identifiers should 400: {text}");

    // A missing Apple field is equally a 400, not a stored half-configuration.
    let (status, text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/app_store", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": {"bundle_id": "com.example.app"} }),
        )
        .await;
    assert_eq!(
        status, 400,
        "incomplete Apple identifiers should 400: {text}"
    );

    let (status, text, _) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/amazon", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": google_ids() }),
        )
        .await;
    assert_eq!(status, 400, "unknown store should 400: {text}");

    server.shutdown().await;
}

#[tokio::test]
async fn a_gs_prefixed_bucket_is_normalised_to_a_bare_name() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "bucket").await;

    // Operators copy `gs://bucket` out of the Play Console. Storing it verbatim
    // builds a URL with a doubled scheme that 404s on a bucket that exists.
    let (status, text, body) = server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({
                "identifiers": {
                    "package_name": "com.example.app",
                    "gcs_bucket": "gs://pubsite_prod_rev_01234/"
                }
            }),
        )
        .await;
    assert_eq!(status, 200, "PUT: {text}");
    assert_eq!(body["identifiers"]["gcs_bucket"], "pubsite_prod_rev_01234");

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// The store-environment designation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_environment_id_from_another_app_is_rejected() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "envdesig").await;

    let (status, text, _) = server
        .patch_raw(
            &format!("/v1/apps/{}", fx.app_a),
            &fx.owner_token,
            json!({"name": "app-a", "store_environment_id": fx.env_b}),
        )
        .await;
    assert_eq!(
        status, 400,
        "a foreign environment would hide the section forever: {text}"
    );

    // The app's own environment is accepted and returned.
    let (status, text, body) = server
        .patch_raw(
            &format!("/v1/apps/{}", fx.app_a),
            &fx.owner_token,
            json!({"name": "app-a", "store_environment_id": fx.env_a}),
        )
        .await;
    assert_eq!(status, 200, "own environment should be accepted: {text}");
    assert_eq!(body["store_environment_id"], json!(fx.env_a));

    server.shutdown().await;
}

#[tokio::test]
async fn a_patch_that_omits_the_designation_leaves_it_alone() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "envkeep").await;

    server
        .patch_raw(
            &format!("/v1/apps/{}", fx.app_a),
            &fx.owner_token,
            json!({"name": "app-a", "store_environment_id": fx.env_a}),
        )
        .await;

    // A plain rename — the shape the existing settings form sends. Without the
    // double option this would silently clear the designation.
    let (status, text, body) = server
        .patch_raw(
            &format!("/v1/apps/{}", fx.app_a),
            &fx.owner_token,
            json!({"name": "renamed"}),
        )
        .await;
    assert_eq!(status, 200, "rename: {text}");
    assert_eq!(
        body["store_environment_id"],
        json!(fx.env_a),
        "an unrelated rename cleared the store designation: {text}"
    );

    // An explicit null is what clears it.
    let (_, _, body) = server
        .patch_raw(
            &format!("/v1/apps/{}", fx.app_a),
            &fx.owner_token,
            json!({"name": "renamed", "store_environment_id": Value::Null}),
        )
        .await;
    assert_eq!(body["store_environment_id"], Value::Null);

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// The chart feed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unpublished_days_are_reported_pending_not_zero_filled() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pending").await;

    server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;

    // Seed exactly ONE day inside a 7-day window.
    let day = Utc::now().date_naive() - chrono::Duration::days(3);
    {
        let mut conn = server.conn().await;
        repo::upsert_store_daily_metrics(&mut conn, fx.app_a, "google_play", &[(day, 100, 10)])
            .await
            .expect("seed metrics");
    }

    let (status, text, body) = server
        .get_raw(
            &format!("/v1/apps/{}/store-metrics?since_days=7", fx.app_a),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "metrics: {text}");

    let series = body["series"].as_array().expect("series array");
    assert_eq!(series.len(), 1, "only real days belong in series: {text}");
    assert_eq!(series[0]["google_play"]["installs"], json!(100));
    assert_eq!(
        series[0]["app_store"],
        Value::Null,
        "a store with no data must be ABSENT, not zero: {text}"
    );

    let pending = body["pending_days"].as_array().expect("pending_days array");
    assert!(
        !pending.is_empty(),
        "days the store has not published must be reported, not silently missing: {text}"
    );
    assert!(
        pending.iter().all(|p| p["day"] != json!(day.to_string())),
        "the day we DO have must not also be listed as pending: {text}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn an_app_with_no_connection_reports_nothing_pending() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "noconn").await;

    // Nothing is configured, so nothing is late. Listing 28 "pending" days for
    // an app nobody connected would be noise dressed as a problem.
    let (status, text, body) = server
        .get_raw(
            &format!("/v1/apps/{}/store-metrics?since_days=30", fx.app_b),
            &fx.owner_token,
        )
        .await;
    assert_eq!(status, 200, "metrics: {text}");
    assert!(body["series"].as_array().expect("series").is_empty());
    assert!(
        body["pending_days"].as_array().expect("pending").is_empty(),
        "no connection means nothing is pending: {text}"
    );
    assert!(body["stores"].as_array().expect("stores").is_empty());

    server.shutdown().await;
}

#[tokio::test]
async fn deleting_a_connection_keeps_the_collected_history() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "delkeep").await;
    let path = format!("/v1/apps/{}/store-connections/google_play", fx.app_a);

    server
        .put_raw(
            &path,
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;

    let day = Utc::now().date_naive() - chrono::Duration::days(3);
    {
        let mut conn = server.conn().await;
        repo::upsert_store_daily_metrics(&mut conn, fx.app_a, "google_play", &[(day, 100, 10)])
            .await
            .expect("seed metrics");
    }

    let (status, text, _) = server.delete_raw(&path, &fx.owner_token).await;
    assert_eq!(status, 204, "delete: {text}");

    let (_, text, body) = server
        .get_raw(
            &format!("/v1/apps/{}/store-metrics?since_days=7", fx.app_a),
            &fx.owner_token,
        )
        .await;
    assert_eq!(
        body["series"].as_array().expect("series").len(),
        1,
        "history is not a credential; removing the key must not erase it: {text}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn cors_advertises_put_so_the_dashboard_can_actually_save() {
    // Regression: `store-connections` is the API's first PUT route, and the
    // CORS layer's `allow_methods` list did not include PUT. Nothing in Rust
    // caught it — no test here is subject to CORS — and the preflight itself
    // still answered 200. The only symptom was `net::ERR_FAILED` on the real
    // request, in a browser, with a settings form that silently did nothing.
    let Some(mut server) = TestServer::start().await else {
        return;
    };

    let resp = server
        .client
        .request(
            reqwest::Method::OPTIONS,
            format!(
                "{}/v1/apps/{}/store-connections/google_play",
                server.base,
                Uuid::new_v4()
            ),
        )
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "PUT")
        .header(
            "Access-Control-Request-Headers",
            "authorization,content-type",
        )
        .send()
        .await
        .expect("preflight request");

    let allowed = resp
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(
        allowed.contains("PUT"),
        "CORS must advertise PUT or the browser can never save a store connection; got {allowed:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn queue_sync_accepts_without_doing_the_work_inline() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "queue").await;
    server
        .put_raw(
            &format!("/v1/apps/{}/store-connections/google_play", fx.app_a),
            &fx.owner_token,
            json!({ "identifiers": google_ids(), "secret": GOOGLE_SA_KEY }),
        )
        .await;

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/apps/{}/store-connections/google_play/sync", fx.app_a),
            &fx.owner_token,
            json!({}),
        )
        .await;
    // 202, not 200: nothing has been fetched. Answering 200 would invite the UI
    // to claim the data is fresh the moment the button returns.
    assert_eq!(status, 202, "queue sync: {text}");

    let mut conn = server.conn().await;
    let row = repo::get_store_connection(&mut conn, fx.app_a, "google_play")
        .await
        .expect("read connection")
        .expect("row exists");
    assert!(
        row.next_sync_at <= Utc::now(),
        "queueing must make the row due now"
    );
    assert!(
        row.last_synced_at.is_none(),
        "the request must not have performed a sync itself"
    );
    drop(conn);

    server.shutdown().await;
}
