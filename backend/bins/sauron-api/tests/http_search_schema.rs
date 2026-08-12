//! HTTP API Integration tests for GET /v1/apps/{app_id}/search/schema.
//!
//! Tiers covered:
//! - Tier 1: GET /v1/apps/{app_id}/search/schema?context=issues returning 200 OK schema envelope.
//! - Tier 2: Missing app_id, invalid context, permission denial.
//! - Tier 3: Permission-gated dimension withholding (event:read / env:read).
//! - Tier 4: Multi-context schema requests.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

const JWT_SECRET: &str = "http-search-schema-test-secret-000000000";

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

        let db_name = format!(
            "sauron_test_{}_sc{}",
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
            .env("NOTIFY_SECRET_KEY", "sauron-test-notify-secret-key-0000000000")
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

    async fn get_status_and_body(&self, path: &str, token: Option<&str>) -> (u16, String) {
        let mut req = self.client.get(format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: failed to read body (status {status}): {e}"));
        (status, text)
    }

    async fn get_json(&self, path: &str, token: &str) -> Value {
        let (status, text) = self.get_status_and_body(path, Some(token)).await;
        assert_eq!(status, 200, "GET {path} returned {status}: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("GET {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        })
    }

    async fn seed_app_with_permissions(&self, perms: &[&str]) -> (Uuid, String) {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "schema org", &format!("schema-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "schema project",
            &format!("schema-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "schema app",
            &format!("schema-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let user = repo::create_user(
            &mut conn,
            &format!("schema-user-{suffix}@example.test"),
            "unused-hash",
            "Schema Owner",
        )
        .await
        .expect("create user");

        let role = repo::create_role(
            &mut conn,
            org.id,
            "schema role",
            "test role",
            json!(perms),
        )
        .await
        .expect("create role");

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
        .expect("create grant");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (token, _) = keys
            .issue_access(user.id, false, None)
            .expect("issue token");
        (app.id, token)
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
                "WARNING: ephemeral test database {} may remain.",
                self.db_name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1: GET /v1/apps/{app_id}/search/schema?context=issues 200 OK envelope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_search_schema_issues_200_ok() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token) = server
        .seed_app_with_permissions(&[perm::EVENT_READ, perm::ISSUE_READ])
        .await;

    let path = format!("/v1/apps/{app_id}/search/schema?context=issues");
    let json_body = server.get_json(&path, &token).await;

    assert_eq!(json_body["resource"], "issues");
    assert!(json_body["variables"].is_array());
    assert!(json_body["dimensions"].is_array());
    assert!(json_body["available_tags"].is_array());
    assert!(json_body["available_labels"].is_array());

    // Check variable prefixes presence
    let vars = json_body["variables"].as_array().unwrap();
    let prefixes: Vec<&str> = vars
        .iter()
        .map(|v| v["prefix"].as_str().unwrap())
        .collect();
    assert!(prefixes.contains(&"@tag"));
    assert!(prefixes.contains(&"@context"));
    assert!(prefixes.contains(&"@extra"));

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 2: Missing app_id, invalid context, permission denial
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_search_schema_invalid_context() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token) = server
        .seed_app_with_permissions(&[perm::EVENT_READ])
        .await;

    let path = format!("/v1/apps/{app_id}/search/schema?context=invalid_context_foo");
    let (status, _body) = server.get_status_and_body(&path, Some(&token)).await;

    assert_eq!(status, 400, "invalid context returns 400 Bad Request");

    server.shutdown().await;
}

#[tokio::test]
async fn test_http_search_schema_unauthorized() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let app_id = Uuid::new_v4();
    let path = format!("/v1/apps/{app_id}/search/schema?context=issues");

    // Request without bearer token
    let (status, _body) = server.get_status_and_body(&path, None).await;
    assert!(
        status == 401 || status == 403,
        "unauthenticated request returns 401/403 (got {status})"
    );

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 3: Permission-gated dimension withholding (event:read)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_search_schema_permission_denial() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    // Token missing event:read (only has issue:read)
    let (app_id, token) = server
        .seed_app_with_permissions(&[perm::ISSUE_READ])
        .await;

    let path = format!("/v1/apps/{app_id}/search/schema?context=issues");
    let (status, _body) = server.get_status_and_body(&path, Some(&token)).await;

    assert_eq!(
        status, 403,
        "token lacking event:read returns 403 Forbidden"
    );

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 4: Multi-context schema requests (issues, sessions, events)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_search_schema_multi_context() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token) = server
        .seed_app_with_permissions(&[perm::EVENT_READ, perm::ISSUE_READ])
        .await;

    let contexts = vec!["issues", "sessions", "events", "occurrences"];

    for ctx in contexts {
        let path = format!("/v1/apps/{app_id}/search/schema?context={ctx}");
        let json_body = server.get_json(&path, &token).await;

        assert_eq!(
            json_body["resource"], ctx,
            "resource field matches context parameter '{ctx}'"
        );
        assert!(json_body["dimensions"].is_array());
    }

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Unit tests for catalog dimensions (fallback when database is unset)
// ---------------------------------------------------------------------------

#[test]
fn test_schema_catalog_dimensions_unit() {
    use sauron_query::catalog::{dimensions_for, label_dimension, tag_dimension, Resource};

    let issues_dims: Vec<_> = dimensions_for(Resource::Issues).collect();
    assert!(!issues_dims.is_empty(), "Resource::Issues has catalog dimensions");

    let sessions_dims: Vec<_> = dimensions_for(Resource::Sessions).collect();
    assert!(!sessions_dims.is_empty(), "Resource::Sessions has catalog dimensions");

    assert!(tag_dimension(Resource::Issues).is_some());
    assert!(label_dimension(Resource::Issues).is_some());
}
