//! HTTP-level tests for the `?release=` query parameter — Task 6 of the
//! 2026-09-14 release-filter plan.
//!
//! Mirrors `tests/http_env_scoping.rs`'s own rationale for going through the
//! real spawned binary rather than calling handlers directly: whether a route
//! actually threads `release` into its query node, and whether every OTHER
//! route actually rejects it, is a fact about which extractor/middleware a
//! handler is wired to, not something a `parse_release`/`with_release` unit
//! test (already covered in `routes/scope.rs` and
//! `sauron-query::release_scope`) can see.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset — see `http_env_scoping.rs`'s identical convention.

/// The router scanner, shared with the binary's own OpenAPI parity test and
/// with `http_env_scoping.rs` — see that file's doc comment on this same
/// `#[path]` inclusion for why one parser matters here (an integration test
/// cannot `use` a binary crate).
#[path = "../src/route_table.rs"]
mod route_table;

/// The `release=` allowlist + guard middleware itself, included the same way
/// as `route_table` above so this test exercises the LITERAL source the
/// binary compiles rather than a copy of it. `release_guard.rs` is
/// deliberately free of `crate::` imports (see its own doc comment) so it
/// compiles cleanly under this `#[path]` inclusion, whose `crate` root is
/// this test binary, not `sauron-api`.
#[path = "../src/release_guard.rs"]
mod release_guard;

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant,
};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-release-scoping-test-secret-000000000000";

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

/// A fresh, migrated, ephemeral database plus a real spawned `sauron-api`
/// process pointed at it, and an HTTP client for driving it. See
/// `tests/http_env_scoping.rs`'s `TestServer` for the full reasoning; this is
/// the same shape (duplicated rather than shared — test files are
/// self-contained by this repo's convention).
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

        // Timestamp segment FIRST — the reaper in sauron-db's test common
        // parses it; see `http_env_scoping.rs` for the leak this prevents.
        let db_name = format!(
            "sauron_test_{}_rel{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
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
                .timeout(Duration::from_millis(200))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
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

    async fn get_status_and_body(&self, path: &str, token: &str) -> (u16, String) {
        let resp = self.get(path, token).await;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: failed to read body (status {status}): {e}"));
        (status, text)
    }

    /// Any method, with a raw body. Used by the write-side tests below, which
    /// care only about *which* layer answered — never about the handler
    /// succeeding — so this returns the status and the body text verbatim
    /// rather than insisting on JSON (a 405 or a 413 has no JSON envelope).
    async fn send_status_and_body(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> (u16, String) {
        let resp = self
            .client
            .request(method.clone(), format!("{}{path}", self.base))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{method} {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_else(|e| {
            panic!("{method} {path}: failed to read body (status {status}): {e}")
        });
        (status, text)
    }

    async fn get_json(&self, path: &str, token: &str) -> serde_json::Value {
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
                 never reached — the test likely panicked). Drop it manually:\n  \
                 DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

/// Define an environment on `project_id` and enroll `app_id` in it. Returns
/// the **enrollment** id — see `http_env_scoping.rs`'s identical helper.
async fn seed_env(
    conn: &mut sauron_db::PgConn,
    project_id: Uuid,
    app_id: Uuid,
    name: &str,
    public_key: &str,
    is_default: bool,
) -> Uuid {
    let env = repo::create_project_environment(conn, project_id, name)
        .await
        .unwrap_or_else(|e| panic!("create catalogue env {name}: {e}"));
    repo::create_app_environments(
        conn,
        &[NewAppEnvironment {
            app_id,
            environment_id: env.id,
            public_key,
            is_default,
        }],
    )
    .await
    .unwrap_or_else(|e| panic!("enroll app in {name}: {e}"))
    .remove(0)
    .id
}

/// One app, one environment, one org-wide token with read access to every
/// resource `release=` reaches (issues, occurrences, events, sessions,
/// transactions).
struct Fixture {
    app_id: Uuid,
    env_id: Uuid,
    token: String,
}

impl TestServer {
    async fn seed_fixture(&self) -> Fixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut conn, "release org", &format!("release-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "release project",
            &format!("release-p-{s}"),
        )
        .await
        .expect("project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "Release App",
            &format!("release-a-{s}"),
            "web",
        )
        .await
        .expect("app");
        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_release_{s}"),
            true,
        )
        .await;
        let user = repo::create_user(&mut conn, &format!("release-{s}@example.test"), "x", "U")
            .await
            .expect("user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "release role",
            "read",
            // `ARTIFACT_WRITE` is here for
            // `source_map_upload_with_release_is_not_guarded` — without it
            // that test would 403 before the handler, and a 403 would "pass"
            // an assertion that only checks the guard did not fire, proving
            // nothing about whether an upload actually works.
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::ARTIFACT_WRITE]),
        )
        .await
        .expect("role");
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
        .expect("grant");
        drop(conn);
        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (token, _) = keys.issue_access(user.id, false, None).expect("token");
        Fixture {
            app_id: app.id,
            env_id,
            token,
        }
    }
}

/// [`seed_analytics_event`](http_env_scoping's identical helper), plus a
/// `release` value — `None` for "no release attributed", matching what
/// `?release=none` must select.
async fn seed_analytics_event_with_release(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    name: &str,
    release: Option<&str>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: name.to_string(),
            distinct_id: format!("release-scoping-fixture-{}", Uuid::new_v4().simple()),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: release.map(str::to_string),
            ip_address: None,
            occurred_at: Utc::now(),
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert analytics event");
}

/// One issue, and one `error_events` occurrence attributed to `release`
/// (`None` = no release). Calling it twice with one `fingerprint` gives ONE
/// issue with two occurrences — which is the fixture
/// `an_issue_with_both_a_released_and_an_unreleased_occurrence_appears_under_both`
/// needs, and which `upsert_issue` (keyed on `(app_id, fingerprint)`)
/// provides.
///
/// Adapted from `tests/http_env_scoping.rs`'s `seed_issue_with_error` — see
/// that helper for why an issue with no `error_events` row is invisible to a
/// scoped read. The symbolication payload is dropped here: this file asks
/// nothing about source context.
async fn seed_issue_occurrence(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    fingerprint: &str,
    release: Option<&str>,
) -> Uuid {
    let now = Utc::now();
    let issue_id = repo::upsert_issue(
        conn,
        NewIssue {
            app_id,
            fingerprint,
            type_: "Error",
            title: "release scoping fixture issue",
            culprit: "release_scoping::fixture",
            level: "error",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue");

    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: "error".into(),
            message: "release scoping fixture error".into(),
            exception_type: "FixtureError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: release.map(str::to_string),
            distinct_id: None,
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_needed".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(true),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");

    issue_id
}

/// Turn a route template into a concrete request path. Copied from
/// `tests/http_env_scoping.rs`'s `build_request_path` — see that file's doc
/// comment for why placeholders (not real fixture rows) are correct here, and
/// why routes needing their OWN required query parameter must get one (else a
/// `Query<T>` 400 would be indistinguishable from a `release=` rejection and
/// misclassify a narrowing/rejecting route).
fn build_request_path(template: &str, app_id: Uuid) -> String {
    let mut path = template.replace("{app_id}", &app_id.to_string());

    while let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .unwrap_or_else(|| panic!("build_request_path: unbalanced '{{' in {template:?}"));
        let param = path[start + 1..end].to_string();
        let replacement = match param.as_str() {
            "issue_id" => Uuid::new_v4().to_string(),
            "session_id" => "task-6-release-scoping-session".to_string(),
            "distinct_id" => "task-6-release-scoping-person".to_string(),
            "name" => "task-6-release-scoping-workflow".to_string(),
            // Reached only by the non-GET sweep below (no GET route carries
            // these). Like every other substitution here they name nothing
            // real: the sweep asserts which *layer* answered, and the guard
            // runs outermost — before routing, auth, or any existence check.
            "artifact_id" | "funnel_id" => Uuid::new_v4().to_string(),
            "store" => "apple".to_string(),
            other => panic!(
                "build_request_path: route template {template:?} has an unhandled path \
                 parameter {{{other}}} — add a substitution for it in build_request_path \
                 rather than letting this test silently send the literal '{{{other}}}' text as \
                 part of the URL"
            ),
        };
        path.replace_range(start..=end, &replacement);
    }

    let extra_query: Option<&str> = match template {
        "/v1/apps/{app_id}/device" => Some("key=task-6-release-scoping-device"),
        "/v1/apps/{app_id}/screens/detail" => Some("name=task-6-release-scoping-screen"),
        "/v1/apps/{app_id}/screens/events"
        | "/v1/apps/{app_id}/screens/exceptions"
        | "/v1/apps/{app_id}/screens/devices"
        | "/v1/apps/{app_id}/screens/users" => Some("name=task-6-release-scoping-screen"),
        "/v1/apps/{app_id}/errors/timeseries"
        | "/v1/apps/{app_id}/events/timeseries"
        | "/v1/apps/{app_id}/transactions/timeseries" => {
            Some("from=2024-01-01T00:00:00Z&to=2024-01-02T00:00:00Z")
        }
        _ => None,
    };
    if let Some(q) = extra_query {
        path.push('?');
        path.push_str(q);
    }
    path
}

#[tokio::test]
async fn release_filters_the_events_list_and_none_means_null() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("TEST_DATABASE_URL/TEST_REDIS_URL unset")
    };
    let fx = srv.seed_fixture().await;
    {
        let mut conn = srv.conn().await;
        seed_analytics_event_with_release(
            &mut conn,
            fx.app_id,
            Some(fx.env_id),
            "with",
            Some("1.4.0"),
        )
        .await;
        seed_analytics_event_with_release(&mut conn, fx.app_id, Some(fx.env_id), "without", None)
            .await;
    }
    let all = srv
        .get_json(&format!("/v1/apps/{}/events/list", fx.app_id), &fx.token)
        .await;
    assert_eq!(all["total"], 2, "{all}");
    let one = srv
        .get_json(
            &format!("/v1/apps/{}/events/list?release=1.4.0", fx.app_id),
            &fx.token,
        )
        .await;
    assert_eq!(one["total"], 1, "{one}");
    assert_eq!(one["data"][0]["name"], "with");
    let none = srv
        .get_json(
            &format!("/v1/apps/{}/events/list?release=none", fx.app_id),
            &fx.token,
        )
        .await;
    assert_eq!(none["total"], 1, "{none}");
    assert_eq!(none["data"][0]["name"], "without");
    srv.shutdown().await;
}

#[tokio::test]
async fn empty_release_is_a_400() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let (status, body) = srv
        .get_status_and_body(
            &format!("/v1/apps/{}/events/list?release=", fx.app_id),
            &fx.token,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    // Matches `ApiError::BadRequest`'s envelope shape exactly (see
    // `bins/sauron-api/src/error.rs`'s `body()` helper): a rejection at the
    // handler (`parse_release`) and a rejection at the guard middleware must
    // both look like an ordinary `ApiError::BadRequest` to a caller — this
    // request hits `parse_release`, not the middleware (`release=` present
    // with an empty value is not itself a rejection at the allowlist gate,
    // since `/events/list` DOES accept `release`).
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| {
        panic!("expected JSON body: {e}\nbody: {body}");
    });
    assert_eq!(json["error"]["code"], "bad_request", "{json}");
    // The code alone is not enough: `bad_request` is what a malformed
    // `environment_id`, an unparseable cursor and a dozen other rejections on
    // this same route answer with, so asserting only the code would keep
    // passing if `parse_release` stopped being the thing that refused. The
    // message has to name the parameter the caller has to fix.
    let message = json["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("message must be a string: {json}"));
    assert!(
        message.to_ascii_lowercase().contains("release"),
        "the 400 must name `release` as the offending parameter, got: {message}"
    );
    srv.shutdown().await;
}

/// Every app-scoped GET that is NOT in the allowlist must reject `release=`
/// with 400 — the fail-loud rule `environment_id` already follows.
#[tokio::test]
async fn every_other_app_scoped_get_rejects_release() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let accepting: Vec<String> = release_guard::RELEASE_ACCEPTING_PATHS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for template in route_table::app_scoped_get_paths() {
        if accepting.contains(&template) {
            continue;
        }
        let path = build_request_path(&template, fx.app_id);
        let sep = if path.contains('?') { '&' } else { '?' };
        let (status, body) = srv
            .get_status_and_body(&format!("{path}{sep}release=1.0.0"), &fx.token)
            .await;
        assert_eq!(
            status, 400,
            "{template} must reject release=; got {status}: {body}"
        );
        // Distinguishes a genuine release rejection from an unrelated 400
        // (e.g. a route with its own required `Query<T>` field this test's
        // `build_request_path` didn't satisfy) — see that function's doc
        // comment on why `extra_query` exists at all.
        assert!(
            body.to_lowercase().contains("release"),
            "{template}: 400 body does not mention `release`, so this may be an unrelated \
             400 rather than the release guard — got: {body}"
        );
    }
    srv.shutdown().await;
}

#[tokio::test]
async fn allowlisted_routes_accept_release() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    for template in release_guard::RELEASE_ACCEPTING_PATHS {
        let path = build_request_path(template, fx.app_id);
        let sep = if path.contains('?') { '&' } else { '?' };
        let (status, body) = srv
            .get_status_and_body(&format!("{path}{sep}release=1.0.0"), &fx.token)
            .await;
        // `issues/{id}/events` with a random (nonexistent) issue id 404s —
        // `events` confirms the issue belongs to this app (`get_issue`)
        // before ever building the query node `with_release` extends, so a
        // fabricated issue id can never reach 200. Every other allowlisted
        // route has no such existence check ahead of the query and answers
        // 200 (an empty result set) instead.
        assert!(
            status == 200 || status == 404,
            "{template}: {status} {body}"
        );
    }
    srv.shutdown().await;
}

/// Issues have no `release` column of their own — the filter bridges to
/// `error_events` — so `?release=` on the issues list asks about the issue's
/// OCCURRENCES, and one issue can legitimately match several releases at once.
///
/// The case that pins the semantics is an issue with one `1.4.0` occurrence
/// and one release-less occurrence: it must appear under BOTH `?release=1.4.0`
/// and `?release=none`. `none` is "has an occurrence with no release" (a
/// positive `EXISTS(… e.release IS NULL)`, mirroring `?environment_id=none`),
/// not "has no released occurrence" — under the latter reading this issue
/// would vanish from Unknown and be reachable only with the filter off.
#[tokio::test]
async fn an_issue_with_both_a_released_and_an_unreleased_occurrence_appears_under_both() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let fingerprint = format!("release-mixed-{}", Uuid::new_v4().simple());
    let issue_id = {
        let mut conn = srv.conn().await;
        let id = seed_issue_occurrence(
            &mut conn,
            fx.app_id,
            Some(fx.env_id),
            &fingerprint,
            Some("1.4.0"),
        )
        .await;
        let same =
            seed_issue_occurrence(&mut conn, fx.app_id, Some(fx.env_id), &fingerprint, None).await;
        assert_eq!(
            same, id,
            "upsert_issue must reuse the issue for one fingerprint"
        );
        id
    };

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["data"]
            .as_array()
            .unwrap_or_else(|| panic!("no data array: {v}"))
            .iter()
            .map(|r| r["id"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    let released = srv
        .get_json(
            &format!("/v1/apps/{}/issues?release=1.4.0", fx.app_id),
            &fx.token,
        )
        .await;
    assert!(
        ids(&released).contains(&issue_id.to_string()),
        "issue missing under release=1.4.0: {released}"
    );

    let unknown = srv
        .get_json(
            &format!("/v1/apps/{}/issues?release=none", fx.app_id),
            &fx.token,
        )
        .await;
    assert!(
        ids(&unknown).contains(&issue_id.to_string()),
        "issue missing under release=none — `none` must mean \"has a \
         release-less occurrence\", not \"has no released occurrence\": {unknown}"
    );

    // The filter still discriminates: a release nothing reported returns
    // nothing, so the two assertions above are not passing because
    // `?release=` is being ignored outright.
    let other = srv
        .get_json(
            &format!("/v1/apps/{}/issues?release=9.9.9", fx.app_id),
            &fx.token,
        )
        .await;
    assert!(
        !ids(&other).contains(&issue_id.to_string()),
        "issue must not match an unreported release: {other}"
    );
    srv.shutdown().await;
}

/// The events-stats caption beside the occurrence list must share the same
/// `?release=` predicate as the list itself — otherwise the count above the
/// rows can describe a wider set than what is actually shown.
///
/// Uses the same mixed-occurrence fixture as the issues-list test above: one
/// issue, one `1.4.0` occurrence and one release-less occurrence.
#[tokio::test]
async fn events_stats_honours_release_and_none_means_null() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let fingerprint = format!("release-stats-{}", Uuid::new_v4().simple());
    let issue_id = {
        let mut conn = srv.conn().await;
        let id = seed_issue_occurrence(
            &mut conn,
            fx.app_id,
            Some(fx.env_id),
            &fingerprint,
            Some("1.4.0"),
        )
        .await;
        let same =
            seed_issue_occurrence(&mut conn, fx.app_id, Some(fx.env_id), &fingerprint, None).await;
        assert_eq!(
            same, id,
            "upsert_issue must reuse the issue for one fingerprint"
        );
        id
    };

    let all = srv
        .get_json(
            &format!("/v1/apps/{}/issues/{issue_id}/events/stats", fx.app_id),
            &fx.token,
        )
        .await;
    assert_eq!(all["events"], 2, "{all}");

    let released = srv
        .get_json(
            &format!(
                "/v1/apps/{}/issues/{issue_id}/events/stats?release=1.4.0",
                fx.app_id
            ),
            &fx.token,
        )
        .await;
    assert_eq!(released["events"], 1, "{released}");

    let unknown = srv
        .get_json(
            &format!(
                "/v1/apps/{}/issues/{issue_id}/events/stats?release=none",
                fx.app_id
            ),
            &fx.token,
        )
        .await;
    assert_eq!(unknown["events"], 1, "{unknown}");
    srv.shutdown().await;
}

/// The literal message the guard middleware puts in its 400 body
/// (`src/release_guard.rs`'s `reject_release_outside_allowlist`). The
/// write-side tests below assert its ABSENCE, so they need the string itself
/// — any other 400 is an acceptable answer there, only this one is not.
const GUARD_MESSAGE: &str = "release is not supported";

/// Regression: the guard is a read-side rule and must not touch writes.
///
/// `POST /v1/apps/{app_id}/artifacts?…&release=…` is the source-map uploader.
/// Its `release` is the artifact's own attribute — the request body is the
/// raw map file, so every piece of metadata travels in the query string
/// (`routes/artifacts.rs`'s `UploadParams`). An allowlist that looked only at
/// the path answered this with the guard's 400 before `upload` ever ran,
/// breaking every source-map upload that names a release (which is every real
/// one: JS artifacts are matched on `(release, name, content)`).
///
/// Asserts 201, not merely "not the guard's 400": a 403 or a 400 from some
/// other layer would satisfy the weaker claim while the upload stayed broken.
#[tokio::test]
async fn source_map_upload_with_release_is_not_guarded() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let map = br#"{"version":3,"sources":["app.ts"],"names":[],"mappings":"AAAA"}"#;
    let (status, body) = srv
        .send_status_and_body(
            reqwest::Method::POST,
            &format!(
                "/v1/apps/{}/artifacts?kind=js_sourcemap&platform=web&release=1.0.0&name=app.js.map",
                fx.app_id
            ),
            &fx.token,
            "application/octet-stream",
            map.to_vec(),
        )
        .await;
    assert!(
        !body.contains(GUARD_MESSAGE),
        "the release guard answered a source-map upload: {status} {body}"
    );
    assert_eq!(status, 201, "source-map upload must succeed: {body}");
    srv.shutdown().await;
}

/// The sweep half of the test above: NO app-scoped write may be answered by
/// the guard, whatever its path.
///
/// `every_other_app_scoped_get_rejects_release` pins the positive rule on
/// reads; this pins the negative rule on everything else, so a future route
/// that takes `release` as a written attribute cannot be silently 400'd the
/// way `POST /artifacts` was. Any status is acceptable here (these requests
/// carry a placeholder body and placeholder ids, so 400/403/404/405/422 are
/// all expected) — the one forbidden outcome is the guard's own message.
#[tokio::test]
async fn no_app_scoped_write_is_answered_by_the_release_guard() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let mut swept = 0usize;
    for (method, template) in route_table::registered_operations() {
        if method == "GET" {
            continue;
        }
        if !(template == "/v1/apps/{app_id}" || template.starts_with("/v1/apps/{app_id}/")) {
            continue;
        }
        let path = build_request_path(&template, fx.app_id);
        let sep = if path.contains('?') { '&' } else { '?' };
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("known HTTP method");
        let (status, body) = srv
            .send_status_and_body(
                m,
                &format!("{path}{sep}release=1.0.0"),
                &fx.token,
                "application/json",
                b"{}".to_vec(),
            )
            .await;
        assert!(
            !body.contains(GUARD_MESSAGE),
            "{method} {template} was answered by the release guard ({status}): {body}"
        );
        swept += 1;
    }
    // Without this the whole test passes vacuously if the filter above ever
    // stops matching anything (a renamed `{app_id}` placeholder, say).
    assert!(
        swept >= 10,
        "expected at least 10 app-scoped writes to sweep, found {swept}"
    );
    srv.shutdown().await;
}

/// Task 7: `GET /v1/apps/{app_id}/releases` — the release switcher's list.
/// Not part of `RELEASE_ACCEPTING_PATHS` (it has no `?release=` filter of its
/// own — release IS what it lists), but it does honor `?environment_id=`,
/// grouping `app_releases` rows by `release` across the caller's reach.
#[tokio::test]
async fn releases_endpoint_groups_by_release_and_narrows_by_env() {
    let Some(mut srv) = TestServer::start().await else {
        panic!("env unset")
    };
    let fx = srv.seed_fixture().await;
    let now = chrono::Utc::now();
    // 1.4.0's two rows get DIFFERENT timestamps so the handler's per-release
    // min/max fold is actually exercised (a wrong implementation that just
    // keeps the first- or last-folded row's timestamps would pass a test
    // where both rows share one `now`, but fails this).
    let release_140_first_seen = now - chrono::Duration::hours(2);
    let release_140_last_seen = now;
    // 1.3.0 and 0.9.0 share the SAME last_seen_at, to exercise the sort's
    // tie-break (release ascending) when `last_seen_at` alone doesn't order
    // two releases.
    let tied_last_seen = now - chrono::Duration::days(1);
    {
        let mut conn = srv.conn().await;
        sauron_db::releases::upsert_seen(
            &mut conn,
            fx.app_id,
            Some(fx.env_id),
            "1.4.0",
            release_140_first_seen,
        )
        .await
        .unwrap();
        sauron_db::releases::upsert_seen(
            &mut conn,
            fx.app_id,
            None,
            "1.4.0",
            release_140_last_seen,
        )
        .await
        .unwrap();
        sauron_db::releases::upsert_seen(&mut conn, fx.app_id, None, "1.3.0", tied_last_seen)
            .await
            .unwrap();
        sauron_db::releases::upsert_seen(&mut conn, fx.app_id, None, "0.9.0", tied_last_seen)
            .await
            .unwrap();
    }

    let all = srv
        .get_json(&format!("/v1/apps/{}/releases", fx.app_id), &fx.token)
        .await;
    assert_eq!(all.as_array().unwrap().len(), 3, "{all}");

    // Full order: 1.4.0 (last_seen_at = now) first, then — among the two
    // releases tied on last_seen_at — release ascending: 0.9.0 before 1.3.0.
    assert_eq!(all[0]["release"], "1.4.0", "{all}");
    assert_eq!(all[1]["release"], "0.9.0", "{all}");
    assert_eq!(all[2]["release"], "1.3.0", "{all}");

    // The min/max fold: the grouped 1.4.0 entry must carry the EARLIER of
    // its two rows' timestamps as first_seen_at and the LATER as
    // last_seen_at, not whichever row the fold happened to see first/last.
    let parse_rfc3339 = |s: &str| -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap_or_else(|e| panic!("not RFC3339: {s}: {e}"))
            .with_timezone(&chrono::Utc)
    };
    let got_first_seen = parse_rfc3339(all[0]["first_seen_at"].as_str().unwrap());
    let got_last_seen = parse_rfc3339(all[0]["last_seen_at"].as_str().unwrap());
    assert!(
        (got_first_seen - release_140_first_seen)
            .num_milliseconds()
            .abs()
            < 1000,
        "1.4.0 first_seen_at {got_first_seen} not within 1s of {release_140_first_seen}: {all}"
    );
    assert!(
        (got_last_seen - release_140_last_seen)
            .num_milliseconds()
            .abs()
            < 1000,
        "1.4.0 last_seen_at {got_last_seen} not within 1s of {release_140_last_seen}: {all}"
    );

    // environment_ids for 1.4.0, as a SET: {Some(env_id), None} — one row
    // attributed to fx.env_id, one unattributed — length 2, no duplicates.
    let env_ids_140 = all[0]["environment_ids"].as_array().unwrap();
    assert_eq!(env_ids_140.len(), 2, "{all}");
    let env_ids_140_set: std::collections::HashSet<String> =
        env_ids_140.iter().map(|v| v.to_string()).collect();
    assert_eq!(
        env_ids_140_set.len(),
        2,
        "duplicate environment_ids in 1.4.0's entry: {all}"
    );
    let expected_140_set: std::collections::HashSet<String> =
        [format!("\"{}\"", fx.env_id), "null".to_string()]
            .into_iter()
            .collect();
    assert_eq!(env_ids_140_set, expected_140_set, "{all}");

    // 0.9.0 and 1.3.0 each have exactly one, unattributed row.
    assert_eq!(
        all[1]["environment_ids"].as_array().unwrap().as_slice(),
        [serde_json::Value::Null],
        "{all}"
    );
    assert_eq!(
        all[2]["environment_ids"].as_array().unwrap().as_slice(),
        [serde_json::Value::Null],
        "{all}"
    );

    let env_only = srv
        .get_json(
            &format!(
                "/v1/apps/{}/releases?environment_id={}",
                fx.app_id, fx.env_id
            ),
            &fx.token,
        )
        .await;
    assert_eq!(env_only.as_array().unwrap().len(), 1, "{env_only}");
    let unattributed = srv
        .get_json(
            &format!("/v1/apps/{}/releases?environment_id=none", fx.app_id),
            &fx.token,
        )
        .await;
    assert_eq!(unattributed.as_array().unwrap().len(), 3, "{unattributed}");
    srv.shutdown().await;
}
