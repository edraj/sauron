//! HTTP API Integration tests for GET /v1/apps/{app_id}/sessions?query=...
//!
//! Tiers covered:
//! - Tier 1: GET /v1/apps/{app_id}/sessions?query=... with AST JSON and string expressions.
//! - Tier 2: Invalid AST JSON payloads, unknown field errors.
//! - Tier 3: Equivalence between legacy filter= and new query=.
//! - Tier 4: Pagination, sorting, and tenant isolation.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::batch::{bump_sessions, SessionBump};
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

const JWT_SECRET: &str = "http-sessions-search-test-secret-0000";

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
            "sauron_test_{}_ss{}",
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
            .env(
                "NOTIFY_SECRET_KEY",
                "sauron-test-notify-secret-key-0000000000",
            )
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

    async fn get_status_and_body(&self, path: &str, token: &str) -> (u16, String) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
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
        let (status, text) = self.get_status_and_body(path, token).await;
        assert_eq!(status, 200, "GET {path} returned {status}: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("GET {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        })
    }

    async fn seed_app_with_sessions(&self, label: &str) -> (Uuid, String, Vec<String>) {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "session org", &format!("sess-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "session project",
            &format!("sess-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "session app",
            &format!("sess-app-{label}-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let user = repo::create_user(
            &mut conn,
            &format!("sess-user-{suffix}@example.test"),
            "hash",
            "Session Owner",
        )
        .await
        .expect("create user");

        let role = repo::create_role(
            &mut conn,
            org.id,
            "session role",
            "read role",
            json!([perm::EVENT_READ, perm::ISSUE_READ]),
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
        .expect("grant role");

        let now = Utc::now();
        let session_ids = vec![
            format!("sess_a_{suffix}"),
            format!("sess_b_{suffix}"),
            format!("sess_c_{suffix}"),
        ];

        let bumps = vec![
            SessionBump {
                app_id: app.id,
                session_id: session_ids[0].clone(),
                distinct_id: Some("user_alpha".to_string()),
                device_key: Some("dev_1".to_string()),
                first_at: now - Duration::minutes(30),
                last_at: now - Duration::minutes(20),
                context: json!({"app_version": "3.0.2", "os": {"name": "iOS"}}),
                release: Some("v1.0.0".to_string()),
                environment_id: None,
                ip: Some("127.0.0.1".to_string()),
                events_delta: 12,
                errors_delta: 0,
            },
            SessionBump {
                app_id: app.id,
                session_id: session_ids[1].clone(),
                distinct_id: Some("user_beta".to_string()),
                device_key: Some("dev_2".to_string()),
                first_at: now - Duration::minutes(15),
                last_at: now - Duration::minutes(5),
                context: json!({"app_version": "3.0.2", "os": {"name": "Android"}}),
                release: Some("v1.1.0".to_string()),
                environment_id: None,
                ip: Some("127.0.0.2".to_string()),
                events_delta: 3,
                errors_delta: 2,
            },
            SessionBump {
                app_id: app.id,
                session_id: session_ids[2].clone(),
                distinct_id: Some("user_gamma".to_string()),
                device_key: Some("dev_3".to_string()),
                first_at: now - Duration::minutes(5),
                last_at: now,
                context: json!({"app_version": "2.9.0"}),
                release: Some("v1.1.0".to_string()),
                environment_id: None,
                ip: Some("127.0.0.3".to_string()),
                events_delta: 1,
                errors_delta: 5,
            },
        ];

        bump_sessions(&mut conn, &bumps)
            .await
            .expect("seed sessions");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (token, _) = keys
            .issue_access(user.id, false, None)
            .expect("issue token");

        (app.id, token, session_ids)
    }

    async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        sauron_db::drop_database(&self.admin_url, &self.db_name)
            .await
            .expect("drop database");
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
// Tier 1: GET /v1/apps/{app_id}/sessions?query=... with AST JSON & string expr
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_sessions_search_string_query() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier1_str").await;

    // Search by string expression
    let path = format!("/v1/apps/{app_id}/sessions?query=distinctId:user_alpha");
    let json_body = server.get_json(&path, &token).await;

    let data = json_body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 1, "matches exactly user_alpha session");
    assert_eq!(data[0]["distinct_id"], "user_alpha");

    server.shutdown().await;
}

#[tokio::test]
async fn test_http_sessions_search_ast_json_query() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier1_ast").await;

    // Search by AST JSON payload URL-encoded or in query string
    let ast = json!({
        "Pred": {
            "field": "distinctId",
            "value": "user_beta",
            "quoted": false,
            "at": 0
        }
    });

    let ast_json = ast.to_string();
    let encoded_ast =
        percent_encoding::utf8_percent_encode(&ast_json, percent_encoding::NON_ALPHANUMERIC);
    let path = format!("/v1/apps/{app_id}/sessions?query={encoded_ast}");
    let json_body = server.get_json(&path, &token).await;

    let data = json_body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 1, "matches user_beta session");
    assert_eq!(data[0]["distinct_id"], "user_beta");

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 2: Invalid AST JSON payloads, unknown field errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_sessions_search_invalid_ast_json() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier2_invalid_json").await;

    // Malformed JSON AST
    let path = format!("/v1/apps/{app_id}/sessions?query={{%22Pred%22:{{invalid");
    let (status, _body) = server.get_status_and_body(&path, &token).await;

    assert_eq!(status, 400, "malformed AST JSON returns 400 Bad Request");

    server.shutdown().await;
}

#[tokio::test]
async fn test_http_sessions_search_unknown_field() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier2_unknown_field").await;

    // Unknown field query
    let path = format!("/v1/apps/{app_id}/sessions?query=non_existent_field_xyz:123");
    let (status, _body) = server.get_status_and_body(&path, &token).await;

    assert_eq!(status, 400, "unknown field query returns 400 Bad Request");

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 3: Equivalence between legacy distinct_id= / filter= and new query=
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_sessions_search_legacy_equivalence() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier3_equiv").await;

    // 1. Legacy parameter: distinct_id=user_alpha
    let path_legacy = format!("/v1/apps/{app_id}/sessions?distinct_id=user_alpha");
    let json_legacy = server.get_json(&path_legacy, &token).await;

    // 2. New query parameter: query=distinctId:user_alpha
    let path_query = format!("/v1/apps/{app_id}/sessions?query=distinctId:user_alpha");
    let json_query = server.get_json(&path_query, &token).await;

    assert_eq!(
        json_legacy["data"], json_query["data"],
        "legacy filter and query parameter return identical results"
    );

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tier 4: Pagination, sorting, and tenant isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_sessions_search_pagination_and_sorting() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id, token, _sids) = server.seed_app_with_sessions("tier4_page").await;

    // Ascending is the `-` prefix: a bare column sorts DESCENDING across this
    // API (see `parse_sort`), so `sort=events_count` would return 12, 3, 1.
    let path_sort = format!("/v1/apps/{app_id}/sessions?sort=-events_count");
    let json_sort = server.get_json(&path_sort, &token).await;
    let items = json_sort["data"].as_array().expect("array");
    assert_eq!(items.len(), 3);

    // Verify ordering: events_count (1, 3, 12)
    let counts: Vec<i64> = items
        .iter()
        .map(|i| i["events_count"].as_i64().unwrap())
        .collect();
    assert!(
        counts[0] <= counts[1] && counts[1] <= counts[2],
        "events_count ascending: {counts:?}"
    );

    // Test limit and offset pagination
    let path_p1 = format!("/v1/apps/{app_id}/sessions?limit=1&offset=0&sort=-events_count");
    let json_p1 = server.get_json(&path_p1, &token).await;
    let page1 = json_p1["data"].as_array().unwrap();
    assert_eq!(page1.len(), 1);

    let path_p2 = format!("/v1/apps/{app_id}/sessions?limit=1&offset=1&sort=-events_count");
    let json_p2 = server.get_json(&path_p2, &token).await;
    let page2 = json_p2["data"].as_array().unwrap();
    assert_eq!(page2.len(), 1);

    assert_ne!(
        page1[0]["id"], page2[0]["id"],
        "paginated rows do not repeat"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn test_http_sessions_search_tenant_isolation() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (app_id_1, token_1, _sids1) = server.seed_app_with_sessions("tenant1").await;
    let (app_id_2, _token_2, _sids2) = server.seed_app_with_sessions("tenant2").await;

    // Querying app_id_2 with app_id_1 token should be forbidden or return empty for app_id_2
    let path = format!("/v1/apps/{app_id_2}/sessions?query=distinctId:user_alpha");
    let (status, _body) = server.get_status_and_body(&path, &token_1).await;

    assert!(
        status == 403 || status == 404,
        "tenant isolation enforced across apps (got status {status})"
    );

    let path_app1 = format!("/v1/apps/{app_id_1}/sessions?query=distinctId:user_alpha");
    let json_app1 = server.get_json(&path_app1, &token_1).await;
    assert_eq!(json_app1["data"].as_array().unwrap().len(), 1);

    server.shutdown().await;
}
