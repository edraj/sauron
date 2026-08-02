//! HTTP-level tests for the workflows API (Task 5): the four
//! `routes::workflows::*` handlers, driven through the real router, plus the
//! `workflow` filter field on `issues::list`.
//!
//! Spawns the actual compiled `sauron-api` binary against an ephemeral,
//! migrated database — same harness shape as `tests/http_env_scoping.rs`
//! (duplicated rather than shared; see that file's `TestServer`/`swap_database`
//! doc comments for why a cross-test-binary dependency isn't worth it for
//! machinery this small). Only the subset of that machinery these tests
//! actually call is reproduced here.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant};
use sauron_db::repo;
use sauron_db::repo::WorkflowAction;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-workflows-test-secret-00000000000000000";

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

/// Percent-encode a path *segment* the hard way (no `url`/`percent_encoding`
/// dev-dependency in this crate) — encodes every byte outside RFC 3986's
/// unreserved set, so a literal `/` becomes `%2F` (axum's `Path` extractor
/// decodes it back to a literal `/` within the one `{name}` segment, rather
/// than the request line being split on it) and every UTF-8 continuation byte
/// of a multi-byte character is escaped too.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

        // Segment order is load-bearing — timestamp FIRST, discriminator glued
        // to the uuid. See the fuller account at the identical site in
        // `http_env_scoping.rs`: the reaper in `sauron-db`'s
        // `tests/common::reap_stale_test_databases` parses the first
        // underscore-delimited segment after `sauron_test_` as a timestamp and
        // silently skips anything else, so a "sauron_test_wf_<ts>_<uuid>"
        // spelling leaks every database it creates. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "wf" (2) + 32-hex
        // uuid = 57 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_wf{}",
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

    async fn assert_status(&self, path: &str, token: &str, expected: u16, label: &str) {
        let status = self.get_status(path, token).await;
        assert_eq!(
            status, expected,
            "GET {path} ({label}): expected {expected}, got {status}"
        );
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

/// Define an environment on `project_id` and enroll `app_id` in it. Returns
/// the enrollment id — what event/workflow rows store in `environment_id`.
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

/// One org/project/app with two environments, an owner token with app-wide
/// `EVENT_READ`+`ISSUE_READ` reach (granted at org scope, mirroring
/// `http_env_scoping.rs`'s `empty_environment_id_returns_400...` fixture), and
/// a second token deliberately missing `EVENT_READ` for the permission test.
struct WorkflowFixture {
    app_id: Uuid,
    env_a: Uuid,
    env_b: Uuid,
    owner_token: String,
    /// Holds `ISSUE_READ` but NOT `EVENT_READ` — workflows/events routes must
    /// refuse this token with 403.
    no_event_read_token: String,
}

impl TestServer {
    async fn seed_workflow_fixture(&self) -> WorkflowFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(
            &mut conn,
            "workflows org",
            &format!("workflows-org-{suffix}"),
        )
        .await
        .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "workflows project",
            &format!("workflows-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "workflows app",
            &format!("workflows-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");
        let env_a = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_workflows_a_{suffix}"),
            true,
        )
        .await;
        let env_b = seed_env(
            &mut conn,
            project.id,
            app.id,
            "staging",
            &format!("pk_workflows_b_{suffix}"),
            false,
        )
        .await;

        let owner = repo::create_user(
            &mut conn,
            &format!("workflows-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Workflows Owner",
        )
        .await
        .expect("create owner user");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "workflows owner role",
            "app-wide event+issue read",
            json!([perm::EVENT_READ, perm::ISSUE_READ]),
        )
        .await
        .expect("create owner role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: owner_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant owner role at org scope");

        let no_event_read = repo::create_user(
            &mut conn,
            &format!("workflows-no-event-read-{suffix}@example.test"),
            "unused-password-hash",
            "Workflows No Event Read",
        )
        .await
        .expect("create no_event_read user");
        let no_event_read_role = repo::create_role(
            &mut conn,
            org.id,
            "workflows no-event-read role",
            "issue read only, no event read",
            json!([perm::ISSUE_READ]),
        )
        .await
        .expect("create no_event_read role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: no_event_read.id,
                role_id: no_event_read_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant no_event_read role at org scope");

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys
            .issue_access(owner.id, false, None)
            .expect("issue owner access token");
        let (no_event_read_token, _) = keys
            .issue_access(no_event_read.id, false, None)
            .expect("issue no_event_read access token");

        WorkflowFixture {
            app_id: app.id,
            env_a,
            env_b,
            owner_token,
            no_event_read_token,
        }
    }
}

/// Seed one workflow run via `Start` + a terminal (`End`/`Cancel`) lifecycle
/// event — the same two calls the ingest pipeline drives off a
/// `$workflow_start`/`$workflow_end`/`$workflow_cancel` pair. `started_at` is
/// the `Start` event's timestamp, `ended_at` the terminal event's.
#[allow(clippy::too_many_arguments)]
async fn seed_workflow_run(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env_id: Uuid,
    name: &str,
    workflow_id: &str,
    terminal: WorkflowAction,
    session_id: Option<&str>,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
) {
    repo::apply_workflow_lifecycle(
        conn,
        app_id,
        env_id,
        workflow_id,
        name,
        WorkflowAction::Start,
        None,
        session_id,
        None,
        started_at,
    )
    .await
    .expect("start workflow lifecycle");
    repo::apply_workflow_lifecycle(
        conn,
        app_id,
        env_id,
        workflow_id,
        name,
        terminal,
        None,
        session_id,
        None,
        ended_at,
    )
    .await
    .expect("terminal workflow lifecycle");
}

/// Insert one analytics event, optionally stamped with a
/// `workflow_id`/`workflow_name` — for the Events-page (`EVENT_FILTERS`)
/// level of the `workflow` filter test.
async fn seed_analytics_event_with_workflow(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Uuid,
    name: &str,
    workflow: Option<(&str, &str)>,
) {
    let (workflow_id, workflow_name) = match workflow {
        Some((id, wf_name)) => (Some(id.to_string()), Some(wf_name.to_string())),
        None => (None, None),
    };
    repo::insert_analytics_event(
        conn,
        sauron_db::models::NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            name: name.to_string(),
            distinct_id: format!("wf-filter-user-{}", Uuid::new_v4().simple()),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at: Utc::now(),
            device_key: None,
            screen: None,
            workflow_id,
            workflow_name,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert analytics event");
}

/// Insert one issue and one error event, optionally stamped with a
/// `workflow_id`/`workflow_name` — for the `issues::list` `workflow` filter
/// test. Mirrors `http_env_scoping.rs`'s `seed_issue_with_error`, plus the
/// workflow stamp.
///
/// Returns `(issue_id, error_event_id)`. The **event** id is what the
/// occurrence-level assertions discriminate on: `models::ErrorEvent` (the
/// read model the `/issues/{id}/events` endpoint serializes) deliberately
/// carries no `workflow_id`/`workflow_name` field, so the response body
/// cannot say which occurrence came back — only its `id` can. See this
/// task's report for that gap.
async fn seed_issue_with_workflow(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Uuid,
    fingerprint: &str,
    workflow: Option<(&str, &str)>,
) -> (Uuid, Uuid) {
    let now = Utc::now();
    let issue_id = repo::upsert_issue(
        conn,
        NewIssue {
            app_id,
            fingerprint,
            type_: "Error",
            title: "workflow filter fixture issue",
            culprit: "workflows::fixture",
            level: "error",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue");

    let (workflow_id, workflow_name) = match workflow {
        Some((id, name)) => (Some(id.to_string()), Some(name.to_string())),
        None => (None, None),
    };

    let event_id = Uuid::new_v4();
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: event_id,
            app_id,
            environment_id: Some(env),
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: "error".into(),
            message: "workflow filter fixture error".into(),
            exception_type: "FixtureError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: None,
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id,
            workflow_name,
            stacktrace_symbolicated: None,
            symbolication_status: "unsymbolicated".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(true),
            title: None,
            culprit: None,
        },
    )
    .await
    .expect("insert error event");

    (issue_id, event_id)
}

#[tokio::test]
async fn get_workflows_returns_rollup_rows() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let now = Utc::now();
    {
        let mut conn = h.conn().await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "checkout",
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            None,
            now - ChronoDuration::minutes(10),
            now - ChronoDuration::minutes(9),
        )
        .await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "checkout",
            &Uuid::new_v4().to_string(),
            WorkflowAction::Cancel,
            None,
            now - ChronoDuration::minutes(8),
            now - ChronoDuration::minutes(7),
        )
        .await;
    }

    let body = h
        .get_json(
            &format!("/v1/apps/{}/workflows?since_days=30", f.app_id),
            &f.owner_token,
        )
        .await;
    let rows = body.as_array().expect("workflows response is a JSON array");
    let checkout = rows
        .iter()
        .find(|r| r["name"] == "checkout")
        .unwrap_or_else(|| panic!("no \"checkout\" row in response: {rows:?}"));
    assert_eq!(checkout["started"].as_i64(), Some(2), "{checkout}");
    assert_eq!(checkout["completed"].as_i64(), Some(1), "{checkout}");
    assert_eq!(checkout["cancelled"].as_i64(), Some(1), "{checkout}");

    h.shutdown().await;
}

#[tokio::test]
async fn get_workflows_is_environment_scoped() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let now = Utc::now();
    {
        let mut conn = h.conn().await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "onboarding",
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            None,
            now - ChronoDuration::minutes(5),
            now - ChronoDuration::minutes(4),
        )
        .await;
    }

    // Seeded under env_a only; querying env_b must be 200 with an empty
    // array — not a 400, and not env_a's data.
    let body = h
        .get_json(
            &format!("/v1/apps/{}/workflows?environment_id={}", f.app_id, f.env_b),
            &f.owner_token,
        )
        .await;
    let rows = body.as_array().expect("workflows response is a JSON array");
    assert!(
        rows.is_empty(),
        "env_b must see no rows from env_a's workflow: {rows:?}"
    );

    // Sanity: env_a itself does see it.
    let body_a = h
        .get_json(
            &format!("/v1/apps/{}/workflows?environment_id={}", f.app_id, f.env_a),
            &f.owner_token,
        )
        .await;
    assert!(
        body_a
            .as_array()
            .expect("array")
            .iter()
            .any(|r| r["name"] == "onboarding"),
        "env_a must still see its own workflow: {body_a:?}"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn get_workflows_requires_event_read_permission() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;

    h.assert_status(
        &format!("/v1/apps/{}/workflows", f.app_id),
        &f.no_event_read_token,
        403,
        "a token lacking event:read must be refused",
    )
    .await;

    h.shutdown().await;
}

#[tokio::test]
async fn get_workflow_detail_and_runs() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let now = Utc::now();
    {
        let mut conn = h.conn().await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "checkout",
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            None,
            now - ChronoDuration::minutes(10),
            now - ChronoDuration::minutes(9),
        )
        .await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "checkout",
            &Uuid::new_v4().to_string(),
            WorkflowAction::Cancel,
            None,
            now - ChronoDuration::minutes(8),
            now - ChronoDuration::minutes(7),
        )
        .await;
    }

    let detail = h
        .get_json(
            &format!("/v1/apps/{}/workflows/checkout", f.app_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(detail["name"], "checkout", "{detail}");
    assert_eq!(detail["started"].as_i64(), Some(2), "{detail}");
    assert_eq!(detail["completed"].as_i64(), Some(1), "{detail}");
    assert_eq!(detail["cancelled"].as_i64(), Some(1), "{detail}");
    assert!(detail["top_events"].is_array(), "{detail}");
    assert!(detail["top_issues"].is_array(), "{detail}");
    assert!(detail["duration_buckets"].is_array(), "{detail}");

    let runs = h
        .get_json(
            &format!("/v1/apps/{}/workflows/checkout/runs", f.app_id),
            &f.owner_token,
        )
        .await;
    let runs = runs.as_array().expect("runs response is a JSON array");
    assert_eq!(runs.len(), 2, "{runs:?}");

    // Unknown name must 404, not 500 — `workflow_detail` returns
    // `Err(NotFound)` when the name has no rows in scope, which `ApiError`'s
    // `From<diesel::result::Error>` maps to 404 (see `error.rs`).
    h.assert_status(
        &format!("/v1/apps/{}/workflows/never-seen-name", f.app_id),
        &f.owner_token,
        404,
        "an unknown workflow name must 404, not 500",
    )
    .await;

    // An invalid `status` filter on `runs` must 400.
    h.assert_status(
        &format!("/v1/apps/{}/workflows/checkout/runs?status=bogus", f.app_id),
        &f.owner_token,
        400,
        "an invalid status filter must 400",
    )
    .await;

    // A valid status filter narrows correctly.
    let completed_runs = h
        .get_json(
            &format!(
                "/v1/apps/{}/workflows/checkout/runs?status=completed",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        completed_runs
            .as_array()
            .expect("runs response is a JSON array")
            .len(),
        1,
        "{completed_runs:?}"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn workflow_name_with_slash_or_unicode_is_handled() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let now = Utc::now();
    let name = "checkout/step-1 日本語";
    {
        let mut conn = h.conn().await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            name,
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            None,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(1),
        )
        .await;
    }

    let encoded = percent_encode_segment(name);
    let detail = h
        .get_json(
            &format!("/v1/apps/{}/workflows/{encoded}", f.app_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        detail["name"].as_str(),
        Some(name),
        "the percent-encoded name must round-trip to the right row: {detail}"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn get_session_workflows_returns_spans_in_order() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let now = Utc::now();
    let session_id = format!("wf-session-{}", Uuid::new_v4().simple());
    {
        let mut conn = h.conn().await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "step-a",
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            Some(&session_id),
            now - ChronoDuration::minutes(10),
            now - ChronoDuration::minutes(9),
        )
        .await;
        seed_workflow_run(
            &mut conn,
            f.app_id,
            f.env_a,
            "step-b",
            &Uuid::new_v4().to_string(),
            WorkflowAction::End,
            Some(&session_id),
            now - ChronoDuration::minutes(5),
            now - ChronoDuration::minutes(4),
        )
        .await;
    }

    let body = h
        .get_json(
            &format!("/v1/apps/{}/sessions/{session_id}/workflows", f.app_id),
            &f.owner_token,
        )
        .await;
    let spans = body.as_array().expect("spans response is a JSON array");
    assert_eq!(spans.len(), 2, "{spans:?}");
    assert_eq!(spans[0]["name"], "step-a", "{spans:?}");
    assert_eq!(spans[1]["name"], "step-b", "{spans:?}");
    let first_started = spans[0]["started_at"]
        .as_str()
        .expect("started_at is a string");
    let second_started = spans[1]["started_at"]
        .as_str()
        .expect("started_at is a string");
    assert!(
        first_started < second_started,
        "spans must come back oldest first: {spans:?}"
    );

    h.shutdown().await;
}

/// The `workflow` filter chip at all three levels it is registered for —
/// `ISSUE_FILTERS` (Issues page), `EVENT_FILTERS` (Events page) and
/// `ERROR_EVENT_FILTERS` (an issue's occurrences) — with the `neq` legs that
/// pin the one semantic all three must agree on: **a row with no workflow at
/// all matches `neq`**. See `repo::list_error_events_for_issue`'s `workflow`
/// arms for why that required deviating from the `<>` precedent its own file
/// uses for every other nullable column.
#[tokio::test]
async fn workflow_filter_narrows_at_every_level_it_is_offered() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_workflows");
        return;
    };
    let f = h.seed_workflow_fixture().await;
    let suffix = Uuid::new_v4().simple().to_string();
    // Distinct fingerprints, because every other identifying field on these
    // two issues (`title`, `culprit`, `level`, `type`) is shared by the
    // fixture — asserting on `culprit` alone would be true of BOTH, so
    // inverting the predicate (shipping `NOT EXISTS` for `Eq`) would return
    // the wrong issue and still pass. `fingerprint` is unique per issue and
    // is in the response body.
    let checkout_fp = format!("wf-filter-checkout-{suffix}");
    let unrelated_fp = format!("wf-filter-unrelated-{suffix}");
    let (stamped_occurrence_id, unstamped_occurrence_id) = {
        let mut conn = h.conn().await;
        // The stamped issue gets TWO occurrences: one stamped `checkout`, one
        // with no workflow at all. That is what lets the occurrence-level
        // `eq`/`neq` legs below discriminate — with only the stamped one,
        // `neq` returning 0 rows would be correct under either semantic.
        let (_, stamped) = seed_issue_with_workflow(
            &mut conn,
            f.app_id,
            f.env_a,
            &checkout_fp,
            Some(("wf-run-checkout", "checkout")),
        )
        .await;
        let (_, unstamped) =
            seed_issue_with_workflow(&mut conn, f.app_id, f.env_a, &checkout_fp, None).await;
        seed_issue_with_workflow(&mut conn, f.app_id, f.env_a, &unrelated_fp, None).await;

        // Analytics events for the Events-page level.
        seed_analytics_event_with_workflow(
            &mut conn,
            f.app_id,
            f.env_a,
            "checkout.step",
            Some(("wf-run-checkout", "checkout")),
        )
        .await;
        seed_analytics_event_with_workflow(&mut conn, f.app_id, f.env_a, "unrelated.event", None)
            .await;
        (stamped, unstamped)
    };

    // --- level 1: Issues (ISSUE_FILTERS -> list_issues) -------------------
    let body = h
        .get_json(
            &format!("/v1/apps/{}/issues?filter=workflow:eq:checkout", f.app_id),
            &f.owner_token,
        )
        .await;
    let issues = body.as_array().expect("issues response is a JSON array");
    assert_eq!(
        issues.len(),
        1,
        "the workflow filter must narrow to only the stamped issue: {issues:?}"
    );
    assert_eq!(
        issues[0]["fingerprint"].as_str(),
        Some(checkout_fp.as_str()),
        "must be the STAMPED issue — an inverted predicate returns the other one, \
         which shares every field but this: {issues:?}"
    );

    // `neq` at the issue level: the unstamped issue matches, the stamped one
    // does not.
    let neq_issues = h
        .get_json(
            &format!("/v1/apps/{}/issues?filter=workflow:neq:checkout", f.app_id),
            &f.owner_token,
        )
        .await;
    let neq_issues = neq_issues
        .as_array()
        .expect("issues response is a JSON array");
    assert_eq!(
        neq_issues
            .iter()
            .filter_map(|i| i["fingerprint"].as_str())
            .collect::<Vec<_>>(),
        vec![unrelated_fp.as_str()],
        "workflow:neq must return the issue with NO workflow — an issue that is \
         part of no workflow is certainly not part of 'checkout': {neq_issues:?}"
    );

    // An unknown filter value narrows to nothing rather than erroring.
    let empty = h
        .get_json(
            &format!("/v1/apps/{}/issues?filter=workflow:eq:never-seen", f.app_id),
            &f.owner_token,
        )
        .await;
    assert!(
        empty
            .as_array()
            .expect("issues response is a JSON array")
            .is_empty(),
        "{empty:?}"
    );

    // --- level 2: Events page (EVENT_FILTERS -> list_analytics_events) ----
    // Registered on its own `FieldSpec` list; a missing entry here is not a
    // silent no-op but an outright `UnknownField` 400.
    let ev_eq = h
        .get_json(
            &format!(
                "/v1/apps/{}/events/list?filter=workflow:eq:checkout",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    let ev_eq = ev_eq.as_array().expect("events response is a JSON array");
    assert_eq!(
        ev_eq
            .iter()
            .filter_map(|e| e["name"].as_str())
            .collect::<Vec<_>>(),
        vec!["checkout.step"],
        "events workflow:eq must return only the stamped event: {ev_eq:?}"
    );

    let ev_neq = h
        .get_json(
            &format!(
                "/v1/apps/{}/events/list?filter=workflow:neq:checkout",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    let ev_neq = ev_neq.as_array().expect("events response is a JSON array");
    assert!(
        ev_neq.iter().any(|e| e["name"] == "unrelated.event"),
        "events workflow:neq must include the UNSTAMPED event — a bare SQL `<>` \
         drops it via three-valued logic, which is the exact inconsistency with \
         the issue level this assertion exists to prevent: {ev_neq:?}"
    );
    assert!(
        !ev_neq.iter().any(|e| e["name"] == "checkout.step"),
        "events workflow:neq must still exclude the stamped event: {ev_neq:?}"
    );

    // --- level 3: an issue's occurrences (ERROR_EVENT_FILTERS) ------------
    let issue_id = issues[0]["id"].as_str().expect("issue id is a string");
    let occ_eq = h
        .get_json(
            &format!(
                "/v1/apps/{}/issues/{issue_id}/events?filter=workflow:eq:checkout",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    let occ_eq = occ_eq
        .as_array()
        .expect("occurrences response is a JSON array");
    assert_eq!(
        occ_eq.len(),
        1,
        "occurrences workflow:eq must return only the stamped occurrence: {occ_eq:?}"
    );
    assert_eq!(
        occ_eq[0]["id"].as_str(),
        Some(stamped_occurrence_id.to_string().as_str()),
        "must be the STAMPED occurrence. Asserted on `id` rather than
         `workflow_name` because `models::ErrorEvent` does not carry the
         workflow columns — the count alone would also pass if the predicate
         were inverted, since this issue has exactly one of each: {occ_eq:?}"
    );

    let occ_neq = h
        .get_json(
            &format!(
                "/v1/apps/{}/issues/{issue_id}/events?filter=workflow:neq:checkout",
                f.app_id
            ),
            &f.owner_token,
        )
        .await;
    let occ_neq = occ_neq
        .as_array()
        .expect("occurrences response is a JSON array");
    assert_eq!(
        occ_neq.len(),
        1,
        "occurrences workflow:neq must return the UNSTAMPED occurrence. This is \
         the assertion that pins the two levels together: a bare `<>` returns 0 \
         here while the issue level returns the unstamped issue, so the same chip \
         would mean opposite things on either side of one drill-down: {occ_neq:?}"
    );
    assert_eq!(
        occ_neq[0]["id"].as_str(),
        Some(unstamped_occurrence_id.to_string().as_str()),
        "the surviving occurrence must be the UNSTAMPED one: {occ_neq:?}"
    );

    h.shutdown().await;
}
