//! HTTP-level tests for the searched issues list (S2c Task 4): the query
//! language, the response envelope, and keyset pagination on
//! `GET /v1/apps/{app_id}/issues`.
//!
//! Two of these are the reason the slice exists at all:
//!
//! - **`query_and_filter_return_identical_rows`** — every shared URL and
//!   bookmark in the wild uses `filter=`/`q=`. Bridging them through
//!   `from_legacy` into the same AST the new grammar produces is only safe if
//!   the two spellings provably select the same rows, in the same order.
//! - **`deep_paging_never_repeats_a_row`** — the defect being removed. Its
//!   fixture makes every issue share ONE `last_seen`, so every page boundary
//!   lands inside a tie group: the exact case a `(last_seen)`-only ordering
//!   cannot resolve, and the reason the keyset tuple is `(last_seen, id)`.
//!
//! Spawns the actual compiled `sauron-api` binary against an ephemeral,
//! migrated database — same harness shape as `tests/http_workflows.rs` and
//! `tests/http_env_scoping.rs` (duplicated rather than shared; see those
//! files' `TestServer`/`swap_database` doc comments for why a
//! cross-test-binary dependency isn't worth it for machinery this small).
//! Only the subset those files' harnesses provide that these tests actually
//! call is reproduced here.
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
use sauron_db::models::{
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant,
};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-search-test-secret-0000000000000000000";

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
        // `insert` returns false if we have issued this port before. Tests in
        // one binary run on parallel threads and two `TestServer::start()`
        // calls race here; the loser's `sauron-api` died with "Address already
        // in use" and the harness reported it as "exited early", which reads
        // like a product fault rather than a harness one.
        if issued.lock().expect("port registry").insert(port) {
            return port;
        }
    }
    panic!("no unused ephemeral port after 100 attempts");
}

/// A fresh, migrated, ephemeral database plus a real spawned `sauron-api`
/// process, and an HTTP client for driving it.
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
        // to the uuid. The reaper in `sauron-db`'s
        // `tests/common::reap_stale_test_databases` parses the first
        // underscore-delimited segment after `sauron_test_` as a timestamp and
        // silently skips anything else, so a "sauron_test_sr_<ts>_<uuid>"
        // spelling would leak every database it creates. Do not reorder.
        let db_name = format!(
            "sauron_test_{}_sr{}",
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
        let mut child = tokio::process::Command::new(bin)
            .env("DATABASE_URL", &db_url)
            .env("REDIS_URL", &redis_url)
            .env("JWT_SECRET", JWT_SECRET)
            // Required and fail-closed since migration 000046: the API refuses
            // to boot without it.
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

impl TestServer {
    /// One org/project/app plus a token holding `issue:read` + `event:read`
    /// app-wide (granted at org scope). Returns `(app_id, token)`.
    ///
    /// `event:read` is included deliberately: without it the free-text reach
    /// narrows and tag predicates are refused, which is its own test's
    /// subject, not this fixture's.
    async fn seed_app(&self, label: &str) -> (Uuid, String) {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "search org", &format!("search-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "search project",
            &format!("search-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "search app",
            &format!("search-app-{label}-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let user = repo::create_user(
            &mut conn,
            &format!("search-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Search Owner",
        )
        .await
        .expect("create user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "search owner role",
            "app-wide event+issue read",
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
        .expect("grant role at org scope");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (token, _) = keys
            .issue_access(user.id, false, None)
            .expect("issue access token");
        (app.id, token)
    }

    /// Insert one issue per `(status, level)` group, `count` of them, each with
    /// a distinct fingerprint and a distinct `last_seen` so the default
    /// ordering is fully determined.
    ///
    /// `status` is applied with a follow-up `update_issue_status` because
    /// `NewIssue` carries no status column — the table defaults every new row
    /// to `unresolved`, exactly as ingest produces them.
    async fn seed_app_with_issues(&self, groups: &[(&str, &str, usize)]) -> (Uuid, String) {
        let (app_id, token) = self.seed_app("groups").await;
        let mut conn = self.conn().await;
        let base = Utc::now() - ChronoDuration::days(1);
        let mut n: i64 = 0;
        for (status, level, count) in groups {
            for _ in 0..*count {
                n += 1;
                let seen = base + ChronoDuration::seconds(n);
                let id = insert_issue(&mut conn, app_id, level, seen, seen).await;
                if *status != "unresolved" {
                    repo::update_issue_status(&mut conn, app_id, id, status)
                        .await
                        .expect("set issue status")
                        .expect("issue exists");
                }
            }
        }
        (app_id, token)
    }

    /// `count` issues that all share ONE `last_seen`.
    ///
    /// This is the fixture the paging test needs: with every row tied on the
    /// sort column, the ordering is decided entirely by the `id` tiebreaker,
    /// so a page boundary can never fall on a "next value" the way it would
    /// with distinct timestamps. An implementation that pages on `last_seen`
    /// alone either loops on the first page forever or skips the rest of the
    /// tie group — both show up as a length mismatch here.
    async fn seed_issues_sharing_a_timestamp(&self, count: usize) -> (Uuid, String) {
        let (app_id, token) = self.seed_app("ties").await;
        let mut conn = self.conn().await;
        let seen = Utc::now() - ChronoDuration::hours(2);
        for _ in 0..count {
            insert_issue(&mut conn, app_id, "error", seen, seen).await;
        }
        (app_id, token)
    }
}

/// One issue with a unique fingerprint. Returns its id.
async fn insert_issue(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    level: &str,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) -> Uuid {
    let fingerprint = format!("search-fp-{}", Uuid::new_v4().simple());
    repo::upsert_issue(
        conn,
        NewIssue {
            app_id,
            fingerprint: &fingerprint,
            type_: "Error",
            title: "search fixture issue",
            culprit: "search::fixture",
            level,
            first_seen,
            last_seen,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue")
}

/// The `id` column of every row in an envelope's `data`, in order.
fn ids(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("response has no `data` array: {v}"))
        .iter()
        .map(|r| r["id"].as_str().expect("row has an id").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The equivalence that makes the legacy bridge safe to ship: an old bookmark
/// and its `query=` spelling must select the same rows, in the same order.
#[tokio::test]
async fn query_and_filter_return_identical_rows() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv
        .seed_app_with_issues(&[
            ("unresolved", "error", 5),
            ("resolved", "error", 50),
            ("unresolved", "warning", 500),
        ])
        .await;

    let legacy = srv
        .get_json(
            &format!("/v1/apps/{app_id}/issues?filter=status:eq:unresolved&filter=level:eq:error"),
            &token,
        )
        .await;
    let modern = srv
        .get_json(
            &format!("/v1/apps/{app_id}/issues?query=status:unresolved%20level:error"),
            &token,
        )
        .await;

    assert_eq!(ids(&legacy), ids(&modern), "legacy and query= disagree");
    assert_eq!(legacy["total"], modern["total"]);
    // Pin the fixture too: an equivalence between two empty lists proves
    // nothing, and a predicate that silently matched everything would satisfy
    // the assertion above just as well.
    assert_eq!(legacy["total"], 5, "expected the 5 unresolved errors");
    assert_eq!(ids(&legacy).len(), 5);
    assert_eq!(legacy["total_is_capped"], false);

    srv.shutdown().await;
}

/// The defect this slice exists to remove.
#[tokio::test]
async fn deep_paging_never_repeats_a_row() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    // All sharing one last_seen, so every page boundary lands inside a tie
    // group — the case a (last_seen)-only index cannot order.
    let (app_id, token) = srv.seed_issues_sharing_a_timestamp(120).await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/issues?limit=25&cursor={c}"),
            None => format!("/v1/apps/{app_id}/issues?limit=25"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(deduped.len(), 120, "paging did not reach every row");

    srv.shutdown().await;
}

#[tokio::test]
async fn an_unsupported_sort_is_refused_not_served_unstably() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv
        .seed_app_with_issues(&[("unresolved", "error", 1)])
        .await;
    let (status, body) = srv
        .get_status_and_body(&format!("/v1/apps/{app_id}/issues?sort=times_seen"), &token)
        .await;
    assert_eq!(status, 400);
    assert!(
        body.contains("times_seen"),
        "error should name the field: {body}"
    );

    srv.shutdown().await;
}

/// `total` describes the whole match set, not the page — that is the entire
/// reason it is in the envelope. A `total` that silently equalled `data.len()`
/// would make every "1-25 of N" footer a lie.
#[tokio::test]
async fn total_counts_past_the_end_of_the_page() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_issues_sharing_a_timestamp(60).await;
    let page = srv
        .get_json(&format!("/v1/apps/{app_id}/issues?limit=10"), &token)
        .await;
    assert_eq!(ids(&page).len(), 10, "page must honour limit");
    assert_eq!(page["total"], 60);
    assert_eq!(page["total_is_capped"], false);
    assert!(
        page["next_cursor"].is_string(),
        "more rows exist, so a cursor must be offered: {page}"
    );

    srv.shutdown().await;
}

/// A cursor is opaque, which means clients WILL truncate, re-encode or invent
/// one. Every such shape is the caller's mistake and must be a 400 — never a
/// 500, and never a silent fall back to page one, which would loop a pager
/// forever.
#[tokio::test]
async fn a_malformed_cursor_is_a_bad_request() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv
        .seed_app_with_issues(&[("unresolved", "error", 2)])
        .await;
    for bad in ["not-a-cursor", "Zm9v", "!!!!"] {
        let (status, body) = srv
            .get_status_and_body(&format!("/v1/apps/{app_id}/issues?cursor={bad}"), &token)
            .await;
        assert_eq!(status, 400, "cursor={bad} returned {status}: {body}");
    }

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// The environment oracle: predicates must not reach across the env boundary
// ---------------------------------------------------------------------------
//
// `issues` has no `environment_id`, so an environment-scoped read decides
// visibility by MEMBERSHIP: does this issue have an occurrence in one of the
// caller's environments. That is the outer filter, and it is not sufficient on
// its own — the issue it admits still has occurrences in OTHER environments,
// and every `tag`/`workflow`/free-text predicate on this route is a correlated
// subquery over `error_events`. If those subqueries carry only the tenant key,
// a member scoped to `staging` can ask questions about production events and
// read the answer off the row count: `?q=sk_live_a`, `?q=sk_live_ab`, … one
// byte at a time, exactly the attack `TextSearchReach` closes for `event:read`,
// re-pointed at the environment boundary.
//
// The property is INDISTINGUISHABILITY, not "returns nothing": a route that
// answered zero rows for every search would satisfy a naive assertion and have
// destroyed the feature. So each probe has three legs — a marker that exists
// only outside the caller's reach, a control that exists nowhere, and the same
// marker found by a caller who does hold the other environment.

/// Present only in the environment the member holds NO grant on.
const OUT_OF_REACH_MARKER: &str = "sg-oor-marker";
/// Present nowhere at all — the control.
const ABSENT_MARKER: &str = "sg-absent-marker";
/// Present in the environment the member DOES hold.
const IN_REACH_MARKER: &str = "sg-inreach-marker";

struct EnvOracleFixture {
    app_id: Uuid,
    /// The one issue, needed by the occurrences-route tests below.
    issue_id: Uuid,
    /// Holds `issue:read`+`event:read` at `env` scope on `granted` only.
    member_token: String,
    /// Holds both app-wide.
    owner_token: String,
}

impl TestServer {
    /// One issue with an occurrence in EACH of two environments, and a member
    /// who may read only one of them.
    ///
    /// One issue, not two, is the whole point: the member legitimately sees it
    /// (it has an occurrence in their environment), so the outer membership
    /// filter admits it and every assertion below is about what the correlated
    /// subqueries may then ask about its *other* occurrences.
    async fn seed_env_oracle_fixture(&self) -> EnvOracleFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "oracle org", &format!("oracle-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "oracle project",
            &format!("oracle-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "oracle app",
            &format!("oracle-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let granted = seed_env(
            &mut conn,
            project.id,
            app.id,
            "granted",
            &format!("pk_oracle_granted_{suffix}"),
            true,
        )
        .await;
        let other = seed_env(
            &mut conn,
            project.id,
            app.id,
            "other",
            &format!("pk_oracle_other_{suffix}"),
            false,
        )
        .await;

        let now = Utc::now();
        let fingerprint = format!("oracle-fp-{suffix}");
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: &fingerprint,
                type_: "Error",
                title: "oracle fixture issue",
                culprit: "oracle::fixture",
                level: "error",
                first_seen: now,
                last_seen: now,
                times_seen: 1,
            },
        )
        .await
        .expect("upsert issue");

        // The occurrence that makes the issue visible to the member.
        seed_occurrence(
            &mut conn,
            app.id,
            granted,
            issue_id,
            &fingerprint,
            IN_REACH_MARKER,
            "granted-workflow",
            now,
        )
        .await;
        // The occurrence they must not be able to interrogate.
        seed_occurrence(
            &mut conn,
            app.id,
            other,
            issue_id,
            &fingerprint,
            OUT_OF_REACH_MARKER,
            "other-workflow",
            now,
        )
        .await;

        // The same pair one table over, for the analytics events list (S2c
        // Task 6). `analytics_events` is invisible to every `issues`/
        // `error_events` query above, so these rows change no existing count
        // here; they exist so the events route can be tested against the same
        // two-environment, one-member shape rather than a fourth fixture.
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(granted),
            "oracle_event",
            json!({ "oracle_key": IN_REACH_MARKER }),
            json!({}),
            json!({}),
            json!({ "oracle_key": IN_REACH_MARKER }),
            None,
            now,
        )
        .await;
        seed_analytics_event(
            &mut conn,
            app.id,
            Some(other),
            "oracle_event",
            json!({ "oracle_key": OUT_OF_REACH_MARKER }),
            json!({}),
            json!({}),
            json!({ "oracle_key": OUT_OF_REACH_MARKER }),
            None,
            now,
        )
        .await;

        let owner = repo::create_user(
            &mut conn,
            &format!("oracle-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Oracle Owner",
        )
        .await
        .expect("create owner");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "oracle owner role",
            "app-wide",
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ]),
        )
        .await
        .expect("create owner role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: owner_role.id,
                scope_type: "app".to_string(),
                scope_id: app.id,
            },
        )
        .await
        .expect("grant owner");

        let member = repo::create_user(
            &mut conn,
            &format!("oracle-member-{suffix}@example.test"),
            "unused-password-hash",
            "Oracle Member",
        )
        .await
        .expect("create member");
        let member_role = repo::create_role(
            &mut conn,
            org.id,
            "oracle member role",
            "one environment only",
            // `event:read` INCLUDED on purpose. Without it the payload scan is
            // dropped and the tag/workflow predicates refused by
            // `reject_withheld_dimensions`, so the test would pass for the
            // wrong reason and prove nothing about the ENVIRONMENT boundary.
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ]),
        )
        .await
        .expect("create member role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: member.id,
                role_id: member_role.id,
                scope_type: "env".to_string(),
                scope_id: granted,
            },
        )
        .await
        .expect("grant member on one environment");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (member_token, _) = keys
            .issue_access(member.id, false, None)
            .expect("member token");
        let (owner_token, _) = keys
            .issue_access(owner.id, false, None)
            .expect("owner token");

        EnvOracleFixture {
            app_id: app.id,
            issue_id,
            member_token,
            owner_token,
        }
    }
}

/// Define an environment on `project_id` and enroll `app_id` in it. Returns the
/// enrollment id — what event rows store in `environment_id`, and what an
/// `env`-scoped grant names. Same helper as `http_env_scoping.rs`'.
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

/// One occurrence carrying `marker` in all three withheld payload columns plus
/// a tag key and a workflow stamp — so a single row is probeable by every
/// predicate shape the correlated subqueries build.
#[allow(clippy::too_many_arguments)]
async fn seed_occurrence(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env_id: Uuid,
    issue_id: Uuid,
    fingerprint: &str,
    marker: &str,
    workflow: &str,
    at: DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env_id),
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: "error".into(),
            message: "oracle fixture error".into(),
            exception_type: "Error".into(),
            exception_value: "oracle fixture error".into(),
            stacktrace: json!({}),
            breadcrumbs: json!([]),
            context: json!({}),
            // `oracle_key` is the tag key both `tag.k:v` and `has:k` probe.
            tags: json!({ "oracle_key": marker }),
            release: None,
            distinct_id: Some(format!("oracle-user-{}", Uuid::new_v4().simple())),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: at,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: Some(format!("wf-{workflow}")),
            workflow_name: Some(workflow.to_string()),
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({ "oracle_ctx": marker }),
            extra: json!({ "oracle_extra": marker }),
            handled: Some(false),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");
}

/// One label per correlated-subquery shape `IssuesLower` can emit.
const PROBES: [&str; 4] = [
    "free text (payload scan)",
    "tag equality",
    "tag substring",
    "workflow substring",
];

/// The query string that drives `label`'s probe against `marker`.
///
/// Deliberately mixes spellings: two go through `query=` (the language) and one
/// through `filter=` (the legacy bridge). Both reach the same leaves, so a fix
/// applied to only one entry point would show up here.
fn probe_query(label: &str, marker: &str) -> String {
    match label {
        "free text (payload scan)" => format!("q={marker}"),
        "tag equality" => format!("query=tag.oracle_key:{marker}"),
        "tag substring" => format!("query=tag.oracle_key:~{marker}"),
        "workflow substring" => format!("filter=workflow:contains:{marker}"),
        other => panic!("unknown probe {other}"),
    }
}

/// `(out_of_reach, absent, in_reach)` for `label`. The workflow probe matches a
/// workflow NAME rather than the payload marker, so it needs its own triple.
fn probe_markers(label: &str) -> (&'static str, &'static str, &'static str) {
    if label == "workflow substring" {
        ("other-workflow", "nope-workflow", "granted-workflow")
    } else {
        (OUT_OF_REACH_MARKER, ABSENT_MARKER, IN_REACH_MARKER)
    }
}

/// The correlated subqueries must not answer questions about occurrences the
/// caller holds no environment grant on.
#[tokio::test]
async fn predicates_cannot_probe_events_outside_the_callers_environment() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_env_oracle_fixture().await;
    let list = format!("/v1/apps/{}/issues", fx.app_id);

    // One shape per correlated subquery the lowerer can build. Every one is a
    // separate leaf, so every one needs its own probe: the defect this test
    // exists for had the tenant key in all five and the environment in none.
    for label in PROBES {
        let build = |m: &str| probe_query(label, m);
        let (out_of_reach, absent, in_reach) = probe_markers(label);

        let control = srv
            .get_json(&format!("{list}?{}", build(absent)), &fx.member_token)
            .await;
        assert_eq!(
            ids(&control).len(),
            0,
            "{label}: the control must match nothing, or it is not a control: {control}"
        );

        // Leg 1+2: the out-of-reach marker must be INDISTINGUISHABLE from the
        // control — same rows AND same total. `total` is a second query and
        // would be an oracle of its own if it were built from a different
        // predicate.
        let probe = srv
            .get_json(&format!("{list}?{}", build(out_of_reach)), &fx.member_token)
            .await;
        assert_eq!(
            ids(&probe),
            ids(&control),
            "{label}: `{out_of_reach}` exists ONLY in an environment this member holds no grant \
             on. It must be indistinguishable from `{absent}`, which exists nowhere — otherwise \
             the correlated subquery is a match/no-match oracle across the environment boundary: \
             {probe}"
        );
        assert_eq!(
            probe["total"], control["total"],
            "{label}: the envelope's `total` must not distinguish them either: {probe}"
        );

        // Leg 3: the narrowed search is still a search. The member's OWN
        // environment must still be reachable by the same predicate, or this
        // test proves only that the feature is broken.
        let mine = srv
            .get_json(&format!("{list}?{}", build(in_reach)), &fx.member_token)
            .await;
        assert_eq!(
            ids(&mine).len(),
            1,
            "{label}: the member must still match on their OWN environment's occurrence — the \
             fix scopes the subquery, it does not disable it: {mine}"
        );

        // And the other side: a caller who holds both environments DOES find
        // the out-of-reach marker. Without this the fixture could have stopped
        // seeding it and every assertion above would pass vacuously.
        let owner = srv
            .get_json(&format!("{list}?{}", build(out_of_reach)), &fx.owner_token)
            .await;
        assert_eq!(
            ids(&owner).len(),
            1,
            "{label}: an app-wide caller must still find `{out_of_reach}` — if this fails the \
             fixture is not seeding it and the whole test is vacuous: {owner}"
        );
    }

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// Per-environment issue statistics (Task 4b)
// ---------------------------------------------------------------------------
//
// `issues` has no `environment_id`, so its stored `times_seen`/`users_seen`/
// `first_seen`/`last_seen`/`level`/`culprit`/`title` are APP-WIDE. Under an
// environment selection those numbers describe events the caller is not being
// shown, which in an observability product is silently-wrong data: an issue
// that saw 3 events in `staging` reporting the app-wide 43.
//
// The fixture is deliberately lopsided — 3 staging occurrences against 40 in
// prod, with a different newest `level`/`culprit`/`title` in each — so every
// derived field has a distinct expected value per environment AND a third,
// distinct stored value. A row that reported the stored numbers, the wrong
// environment's numbers, or zero would each fail a different assertion.

/// What the fixture writes, and what each scope must therefore report.
mod env_stats {
    /// Stored on `issues`, and what an app-wide caller must still see.
    pub const STORED_TIMES_SEEN: i64 = 43;
    pub const STORED_LEVEL: &str = "fatal";
    pub const STORED_TITLE: &str = "stored app-wide title";
    pub const STORED_CULPRIT: &str = "stored::app::wide";

    pub const STAGING_OCCURRENCES: i64 = 3;
    /// Two of the three staging occurrences share a `distinct_id`, so
    /// `users_seen` cannot be satisfied by a `count(*)` that forgot DISTINCT.
    pub const STAGING_USERS: i64 = 2;
    pub const STAGING_LEVEL: &str = "warning";
    pub const STAGING_TITLE: &str = "staging newest title";
    pub const STAGING_CULPRIT: &str = "staging::newest";

    pub const PROD_OCCURRENCES: i64 = 40;
    pub const PROD_USERS: i64 = 40;
    pub const PROD_LEVEL: &str = "fatal";
    pub const PROD_TITLE: &str = "prod newest title";
    pub const PROD_CULPRIT: &str = "prod::newest";

    /// `EnvFilter::Unattributed` — rows written before Slice 1, surfaced under
    /// `?environment_id=none`. Its SQL fragment is a literal `IS NULL` and so
    /// consumes NO bind, unlike `One`/`Subset`; a phase-2 query that assumed
    /// otherwise would shift every placeholder by one.
    pub const NONE_OCCURRENCES: i64 = 2;
    pub const NONE_USERS: i64 = 1;
    pub const NONE_LEVEL: &str = "debug";
    pub const NONE_TITLE: &str = "unattributed newest title";
    pub const NONE_CULPRIT: &str = "unattributed::newest";
}

struct EnvStatsFixture {
    app_id: Uuid,
    /// The `app_environments` enrollment ids — what `?environment_id=` names.
    staging: Uuid,
    prod: Uuid,
    /// Holds `issue:read`+`event:read` at `env` scope on `staging` ALONE, so
    /// their unqualified list resolves to `EnvFilter::Subset([staging])`.
    staging_token: String,
    /// Holds both app-wide: `EnvFilter::All` unqualified, and free to select
    /// either environment explicitly.
    owner_token: String,
}

impl TestServer {
    /// One app enrolled in `staging` + `prod`, an app-wide owner, and a member
    /// who holds `staging` alone. No issues — the two fixtures below seed
    /// their own on top of it.
    async fn seed_two_env_app(&self) -> EnvStatsFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "stats org", &format!("stats-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "stats project",
            &format!("stats-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "stats app",
            &format!("stats-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let staging = seed_env(
            &mut conn,
            project.id,
            app.id,
            "staging",
            &format!("pk_stats_staging_{suffix}"),
            true,
        )
        .await;
        let prod = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_stats_prod_{suffix}"),
            false,
        )
        .await;

        let owner = repo::create_user(
            &mut conn,
            &format!("stats-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Stats Owner",
        )
        .await
        .expect("create owner");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "stats owner role",
            "app-wide",
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ]),
        )
        .await
        .expect("create owner role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: owner_role.id,
                scope_type: "app".to_string(),
                scope_id: app.id,
            },
        )
        .await
        .expect("grant owner");

        let member = repo::create_user(
            &mut conn,
            &format!("stats-member-{suffix}@example.test"),
            "unused-password-hash",
            "Stats Member",
        )
        .await
        .expect("create member");
        let member_role = repo::create_role(
            &mut conn,
            org.id,
            "stats member role",
            "staging only",
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ]),
        )
        .await
        .expect("create member role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: member.id,
                role_id: member_role.id,
                scope_type: "env".to_string(),
                scope_id: staging,
            },
        )
        .await
        .expect("grant member on staging only");
        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (staging_token, _) = keys
            .issue_access(member.id, false, None)
            .expect("member token");
        let (owner_token, _) = keys
            .issue_access(owner.id, false, None)
            .expect("owner token");

        EnvStatsFixture {
            app_id: app.id,
            staging,
            prod,
            staging_token,
            owner_token,
        }
    }

    /// ONE issue whose stored columns, `staging` occurrences and `prod`
    /// occurrences all disagree — see the section comment above.
    async fn seed_env_stats_fixture(&self) -> EnvStatsFixture {
        use env_stats::*;

        let fx = self.seed_two_env_app().await;
        let mut conn = self.conn().await;
        let now = Utc::now();
        let fingerprint = format!("stats-fp-{}", Uuid::new_v4().simple());

        // The STORED row: app-wide totals and the app-wide newest
        // level/title/culprit. Every value here differs from both
        // environments' derived values, so a row that failed to re-derive is
        // distinguishable from one that derived the wrong environment.
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: fx.app_id,
                fingerprint: &fingerprint,
                type_: "Error",
                title: STORED_TITLE,
                culprit: STORED_CULPRIT,
                level: STORED_LEVEL,
                first_seen: now - ChronoDuration::hours(40),
                last_seen: now - ChronoDuration::hours(1),
                times_seen: STORED_TIMES_SEEN,
            },
        )
        .await
        .expect("upsert issue");

        // staging: 3 occurrences, 2 distinct users, oldest at -5h, newest at
        // -3h. The newest carries staging's own level/title/culprit.
        for (i, (mins_ago, user)) in [(300, "u1"), (240, "u1"), (180, "u2")]
            .into_iter()
            .enumerate()
        {
            let newest = i == 2;
            seed_stat_occurrence(
                &mut conn,
                fx.app_id,
                Some(fx.staging),
                issue_id,
                &fingerprint,
                if newest { STAGING_LEVEL } else { "info" },
                if newest {
                    STAGING_TITLE
                } else {
                    "staging older"
                },
                if newest {
                    STAGING_CULPRIT
                } else {
                    "staging::older"
                },
                &format!("stats-staging-{user}"),
                now - ChronoDuration::minutes(mins_ago),
            )
            .await;
        }

        // prod: 40 occurrences, one per hour from -40h to -1h, every one a
        // distinct user. The newest (-1h) is also the app-wide newest, which
        // is what makes "leaked the wrong environment's newest event" a
        // distinguishable failure from "used the stored columns".
        for h in 0..PROD_OCCURRENCES {
            let newest = h == PROD_OCCURRENCES - 1;
            seed_stat_occurrence(
                &mut conn,
                fx.app_id,
                Some(fx.prod),
                issue_id,
                &fingerprint,
                if newest { PROD_LEVEL } else { "error" },
                if newest { PROD_TITLE } else { "prod older" },
                if newest { PROD_CULPRIT } else { "prod::older" },
                &format!("stats-prod-{h}"),
                now - ChronoDuration::hours(PROD_OCCURRENCES - h),
            )
            .await;
        }

        // Unattributed: 2 occurrences, ONE user (so DISTINCT still bites),
        // newest at -2h. Exercises the third `EnvFilter` shape, whose SQL
        // fragment consumes no bind.
        for (i, mins_ago) in [150, 120].into_iter().enumerate() {
            let newest = i == 1;
            seed_stat_occurrence(
                &mut conn,
                fx.app_id,
                None,
                issue_id,
                &fingerprint,
                if newest { NONE_LEVEL } else { "info" },
                if newest {
                    NONE_TITLE
                } else {
                    "unattributed older"
                },
                if newest {
                    NONE_CULPRIT
                } else {
                    "unattributed::older"
                },
                "stats-unattributed-u1",
                now - ChronoDuration::minutes(mins_ago),
            )
            .await;
        }
        fx
    }

    /// `count` issues whose STORED `last_seen` ordering is the exact reverse
    /// of their per-`staging` derived one.
    ///
    /// Stored `last_seen` runs over the last half hour; the staging
    /// occurrences run over the last `count` HOURS, inverted. So a cursor
    /// built from the derived (overwritten) `last_seen` filters
    /// `issues.last_seen < ~now-Nh`, which matches nothing at all — the walk
    /// stops dead after page one instead of paging.
    async fn seed_env_paged_fixture(&self, count: i64) -> EnvStatsFixture {
        let fx = self.seed_two_env_app().await;
        let mut conn = self.conn().await;
        let now = Utc::now();

        for k in 0..count {
            let fingerprint = format!("paged-fp-{}", Uuid::new_v4().simple());
            let stored_last = now - ChronoDuration::minutes(k + 1);
            let issue_id = repo::upsert_issue(
                &mut conn,
                NewIssue {
                    app_id: fx.app_id,
                    fingerprint: &fingerprint,
                    type_: "Error",
                    title: "paged fixture issue",
                    culprit: "paged::fixture",
                    level: "error",
                    first_seen: now - ChronoDuration::hours(count + 1),
                    last_seen: stored_last,
                    times_seen: 7,
                },
            )
            .await
            .expect("upsert issue");

            // Inverted: issue 0 (newest stored) is the OLDEST in staging.
            let staging_at = now - ChronoDuration::hours(count - k);
            for j in 0..2 {
                seed_stat_occurrence(
                    &mut conn,
                    fx.app_id,
                    Some(fx.staging),
                    issue_id,
                    &fingerprint,
                    "warning",
                    "paged staging",
                    "paged::staging",
                    &format!("paged-staging-{k}-{j}"),
                    staging_at - ChronoDuration::minutes(j),
                )
                .await;
            }
            for j in 0..5 {
                seed_stat_occurrence(
                    &mut conn,
                    fx.app_id,
                    Some(fx.prod),
                    issue_id,
                    &fingerprint,
                    "error",
                    "paged prod",
                    "paged::prod",
                    &format!("paged-prod-{k}-{j}"),
                    stored_last - ChronoDuration::seconds(j),
                )
                .await;
            }
        }
        fx
    }
}

/// One occurrence with a caller-chosen `level`/`title`/`culprit`/`distinct_id`
/// and timestamp — the four axes the per-environment derivation reads.
#[allow(clippy::too_many_arguments)]
async fn seed_stat_occurrence(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    // `None` seeds an UNATTRIBUTED occurrence (`environment_id IS NULL`) — the
    // third `EnvFilter` shape, and the one whose SQL fragment consumes no bind
    // at all.
    env_id: Option<Uuid>,
    issue_id: Uuid,
    fingerprint: &str,
    level: &str,
    title: &str,
    culprit: &str,
    distinct_id: &str,
    at: DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env_id,
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: level.into(),
            message: "stats fixture error".into(),
            exception_type: "Error".into(),
            exception_value: "stats fixture error".into(),
            stacktrace: json!({}),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some(distinct_id.to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: at,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(false),
            title: Some(title.to_string()),
            culprit: Some(culprit.to_string()),
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");
}

/// The single row of an envelope whose `data` must hold exactly one.
fn only_row(v: &Value) -> &Value {
    let rows = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("response has no `data` array: {v}"));
    assert_eq!(rows.len(), 1, "expected exactly one issue: {v}");
    &rows[0]
}

fn ts(row: &Value, field: &str) -> DateTime<Utc> {
    row[field]
        .as_str()
        .unwrap_or_else(|| panic!("row has no `{field}`: {row}"))
        .parse::<DateTime<Utc>>()
        .unwrap_or_else(|e| panic!("`{field}` is not a timestamp: {e}"))
}

/// With an environment selected, every statistic on the row must describe THAT
/// environment — not the app-wide stored columns, and not the other
/// environment's.
#[tokio::test]
async fn issue_statistics_are_derived_per_environment() {
    use env_stats::*;

    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_env_stats_fixture().await;
    let list = format!("/v1/apps/{}/issues", fx.app_id);

    // ---- 1. A member who may read only `staging` (EnvFilter::Subset) ----
    let page = srv.get_json(&list, &fx.staging_token).await;
    let row = only_row(&page);
    assert_eq!(
        row["times_seen"], STAGING_OCCURRENCES,
        "times_seen must count STAGING's occurrences, not the app-wide \
         {STORED_TIMES_SEEN}: {page}"
    );
    assert_eq!(
        row["users_seen"], STAGING_USERS,
        "users_seen must be a DISTINCT count within staging: {page}"
    );
    assert_eq!(
        row["level"], STAGING_LEVEL,
        "level must come from staging's NEWEST occurrence: {page}"
    );
    assert_eq!(row["culprit"], STAGING_CULPRIT, "{page}");
    assert_eq!(row["title"], STAGING_TITLE, "{page}");
    // The window, too: staging ran -5h..-3h, the app-wide row -40h..-1h.
    let now = Utc::now();
    let staging_first = ts(row, "first_seen");
    let staging_last = ts(row, "last_seen");
    assert!(
        (now - staging_first).num_minutes().abs() >= 290
            && (now - staging_first).num_minutes() <= 310,
        "first_seen must be staging's oldest (~-5h), got {staging_first}: {page}"
    );
    assert!(
        (now - staging_last).num_minutes() >= 170 && (now - staging_last).num_minutes() <= 190,
        "last_seen must be staging's newest (~-3h), got {staging_last}: {page}"
    );
    // Step 7: phase 2 changes values, never row membership.
    assert_eq!(
        page["total"], 1,
        "total must still agree with the page: {page}"
    );

    // ---- 2. The same environment named explicitly (EnvFilter::One) ----
    let one = srv
        .get_json(
            &format!("{list}?environment_id={}", fx.staging),
            &fx.owner_token,
        )
        .await;
    let row = only_row(&one);
    assert_eq!(row["times_seen"], STAGING_OCCURRENCES, "{one}");
    assert_eq!(row["users_seen"], STAGING_USERS, "{one}");
    assert_eq!(row["level"], STAGING_LEVEL, "{one}");
    assert_eq!(row["culprit"], STAGING_CULPRIT, "{one}");
    assert_eq!(row["title"], STAGING_TITLE, "{one}");

    // ---- 3. The OTHER environment, from the same stored row ----
    let other = srv
        .get_json(
            &format!("{list}?environment_id={}", fx.prod),
            &fx.owner_token,
        )
        .await;
    let row = only_row(&other);
    assert_eq!(
        row["times_seen"], PROD_OCCURRENCES,
        "prod must report its own 40, not staging's 3 and not the stored 43: {other}"
    );
    assert_eq!(row["users_seen"], PROD_USERS, "{other}");
    assert_eq!(row["level"], PROD_LEVEL, "{other}");
    assert_eq!(row["culprit"], PROD_CULPRIT, "{other}");
    assert_eq!(row["title"], PROD_TITLE, "{other}");

    // ---- 3b. Unattributed (EnvFilter::Unattributed) ----
    // The third shape, and the one that can break on its own: its SQL fragment
    // is a literal `IS NULL` and consumes NO bind, so a phase-2 query that
    // assumed every environment filter takes a placeholder would misnumber
    // `since` here and nowhere else. Asserting only a 200 would not catch a
    // MISNUMBERED bind, and an empty page would not run the query at all —
    // hence real derived values.
    let none = srv
        .get_json(&format!("{list}?environment_id=none"), &fx.owner_token)
        .await;
    let row = only_row(&none);
    assert_eq!(row["times_seen"], NONE_OCCURRENCES, "{none}");
    assert_eq!(row["users_seen"], NONE_USERS, "{none}");
    assert_eq!(row["level"], NONE_LEVEL, "{none}");
    assert_eq!(row["culprit"], NONE_CULPRIT, "{none}");
    assert_eq!(row["title"], NONE_TITLE, "{none}");

    // ---- 4. App-wide is UNCHANGED: the stored columns already are the truth ----
    let all = srv.get_json(&list, &fx.owner_token).await;
    let row = only_row(&all);
    assert_eq!(
        row["times_seen"], STORED_TIMES_SEEN,
        "EnvFilter::All must keep reading the stored app-wide count — it is \
         maintained at ingest and can see tiered-out data a re-derivation \
         cannot: {all}"
    );
    assert_eq!(row["level"], STORED_LEVEL, "{all}");
    assert_eq!(row["culprit"], STORED_CULPRIT, "{all}");
    assert_eq!(row["title"], STORED_TITLE, "{all}");

    srv.shutdown().await;
}

/// Phase 2 must not disturb the keyset walk: the cursor has to carry the
/// STORED `last_seen` the page was ordered by, never the per-environment value
/// that overwrites it on the way out. Building it from the overwritten field
/// would jump the walk to an unrelated point in the ordering — skipping rows
/// on some pages and repeating them on others.
#[tokio::test]
async fn env_scoped_paging_still_reaches_every_row() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_env_paged_fixture(30).await;
    let list = format!("/v1/apps/{}/issues", fx.app_id);

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("{list}?limit=7&cursor={c}"),
            None => format!("{list}?limit=7"),
        };
        let page = srv.get_json(&url, &fx.staging_token).await;
        // Every row is env-derived, so phase 2 ran on every page.
        for row in page["data"].as_array().expect("data array") {
            assert_eq!(
                row["times_seen"], 2,
                "each issue has 2 staging occurrences and 5 in prod: {page}"
            );
        }
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(deduped.len(), 30, "paging did not reach every row");

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// The per-issue OCCURRENCES list (S2c Task 5)
// ---------------------------------------------------------------------------
//
// `GET /v1/apps/{app_id}/issues/{issue_id}/events` — the path segment is
// `events`; the RESOURCE is `Occurrences`. Same envelope, same cursor, same
// seam as the issues list, over `(occurred_at, id)`.
//
// This route authorizes on `issue:read` ALONE, which makes the withheld-column
// refusal sharper here than it is on Issues: `R_OCCURRENCES` declares eight
// `Store::JsonRoot` dimensions (`os`/`browser`/`device`/`app` over `context`,
// plus `contexts`, `extra`, `user`, `sdk`, `stack`) over exactly the columns
// `symbolicate::strip_event_body` nulls for such a caller. Bridging onto
// `query=` must not reach a single one of them.

/// Present ONLY in `contexts`/`extra`/`tags` — the three columns
/// `strip_event_body` nulls. Never in `message`/`exception_*`.
const OCC_PAYLOAD_MARKER: &str = "occ-payload-marker";
/// Present only in `message`, a column `strip_event_body` KEEPS.
const OCC_SHELL_MARKER: &str = "occ-shell-marker";
/// The workflow stamp on the marked occurrences.
const OCC_WORKFLOW: &str = "occ-checkout";
/// How many of the fixture's occurrences carry all three markers.
const OCC_MARKED: usize = 3;
/// How many carry none of them.
const OCC_PLAIN: usize = 7;
/// The environment the marked occurrences are attributed to. Its NAME is the
/// thing `env:read` governs — see `search::reject_withheld_environment`.
const OCC_ENV_NAME: &str = "occ-prod";

struct OccFixture {
    app_id: Uuid,
    issue_id: Uuid,
    /// `issue:read` + `event:read`, and deliberately **no `env:read`**: the
    /// caller that proves `event:read` is not a substitute for it.
    full_token: String,
    /// `issue:read` ALONE: bodies nulled on the way out, so the payload must
    /// be unsearchable on the way in.
    shell_token: String,
    /// `issue:read` + `event:read` + `env:read`: the only one of the three
    /// entitled to resolve an environment NAME.
    env_token: String,
}

impl TestServer {
    /// One issue with `OCC_MARKED` marked + `OCC_PLAIN` plain occurrences, and
    /// three callers who differ only in permissions.
    ///
    /// The markers are split across the permission boundary on purpose:
    /// `OCC_SHELL_MARKER` lives in `message` (kept by `strip_event_body`) and
    /// `OCC_PAYLOAD_MARKER` in `contexts`/`extra`/`tags` (nulled by it). A
    /// narrowed caller must still find the first and must not find the second
    /// — "returns nothing" would satisfy half the property and have destroyed
    /// the feature.
    ///
    /// The marked occurrences are additionally attributed to a real
    /// environment named `OCC_ENV_NAME`, and the plain ones to none, so the
    /// `env:read` gate can be tested with a predicate that genuinely
    /// discriminates rather than one that matches everything or nothing.
    /// Every token here is granted at APP scope, so all three resolve to
    /// `EnvFilter::All` and still see all ten rows — attributing some of them
    /// changes no other test's counts.
    async fn seed_occurrence_fixture(&self) -> OccFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "occ org", &format!("occ-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "occ project",
            &format!("occ-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "occ app",
            &format!("occ-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            OCC_ENV_NAME,
            &format!("pk_occ_{suffix}"),
            true,
        )
        .await;

        let now = Utc::now();
        let fingerprint = format!("occ-fp-{suffix}");
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: &fingerprint,
                type_: "Error",
                title: "occ fixture issue",
                culprit: "occ::fixture",
                level: "error",
                first_seen: now - ChronoDuration::hours(2),
                last_seen: now,
                times_seen: (OCC_MARKED + OCC_PLAIN) as i64,
            },
        )
        .await
        .expect("upsert issue");

        for i in 0..OCC_MARKED {
            seed_occ_event(
                &mut conn,
                app.id,
                Some(env_id),
                issue_id,
                &fingerprint,
                &format!("boom {OCC_SHELL_MARKER}"),
                json!({ "occ_ctx": OCC_PAYLOAD_MARKER }),
                json!({ "token": OCC_PAYLOAD_MARKER }),
                json!({ "occ_key": OCC_PAYLOAD_MARKER }),
                Some(OCC_WORKFLOW),
                now - ChronoDuration::minutes(i as i64 + 1),
            )
            .await;
        }
        for i in 0..OCC_PLAIN {
            seed_occ_event(
                &mut conn,
                app.id,
                None,
                issue_id,
                &fingerprint,
                "plain occurrence",
                json!({}),
                json!({}),
                json!({}),
                None,
                now - ChronoDuration::minutes(i as i64 + 20),
            )
            .await;
        }

        // NO `env:read`, deliberately: this is the caller that proves
        // `event:read` does not stand in for it.
        let full = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("occ-full-{suffix}"),
            &[perm::EVENT_READ, perm::ISSUE_READ],
        )
        .await;
        let env = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("occ-env-{suffix}"),
            &[perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ],
        )
        .await;
        // `issue:read` ONLY. This is the caller every refusal below is about.
        let shell = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("occ-shell-{suffix}"),
            &[perm::ISSUE_READ],
        )
        .await;
        drop(conn);

        OccFixture {
            app_id: app.id,
            issue_id,
            full_token: full,
            shell_token: shell,
            env_token: env,
        }
    }

    /// `count` occurrences of ONE issue that all share a single `occurred_at`,
    /// plus a DECOY issue in the same app with occurrences of its own.
    ///
    /// The shared timestamp is the same trick `seed_issues_sharing_a_timestamp`
    /// plays one level up: every page boundary lands inside a tie group, which
    /// is exactly what an `(occurred_at)`-only ordering cannot resolve — it
    /// either loops on page one or skips the rest of the group. The decoy is
    /// the other half: `error_events_issue_time_id_idx` leads with `issue_id`,
    /// so a walk that lost the `issue_id` predicate would still return rows in
    /// a plausible order, just the wrong ones.
    async fn seed_issue_with_occurrences(&self, count: usize) -> (Uuid, Uuid, String) {
        let (app_id, token) = self.seed_app("occ-paging").await;
        let mut conn = self.conn().await;
        let at = Utc::now() - ChronoDuration::hours(1);

        let fingerprint = format!("occ-page-fp-{}", Uuid::new_v4().simple());
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id,
                fingerprint: &fingerprint,
                type_: "Error",
                title: "occ paging issue",
                culprit: "occ::paging",
                level: "error",
                first_seen: at,
                last_seen: at,
                times_seen: count as i64,
            },
        )
        .await
        .expect("upsert issue");
        for _ in 0..count {
            seed_occ_event(
                &mut conn,
                app_id,
                None,
                issue_id,
                &fingerprint,
                "paged occurrence",
                json!({}),
                json!({}),
                json!({}),
                None,
                at,
            )
            .await;
        }

        // The decoy: same app, same timestamp, different issue.
        let decoy_fp = format!("occ-decoy-fp-{}", Uuid::new_v4().simple());
        let decoy_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id,
                fingerprint: &decoy_fp,
                type_: "Error",
                title: "occ decoy issue",
                culprit: "occ::decoy",
                level: "error",
                first_seen: at,
                last_seen: at,
                times_seen: 25,
            },
        )
        .await
        .expect("upsert decoy issue");
        for _ in 0..25 {
            seed_occ_event(
                &mut conn,
                app_id,
                None,
                decoy_id,
                &decoy_fp,
                "decoy occurrence",
                json!({}),
                json!({}),
                json!({}),
                None,
                at,
            )
            .await;
        }
        (app_id, issue_id, token)
    }
}

/// A user with exactly `perms`, granted at app scope. Returns their token.
async fn seed_reader(
    conn: &mut sauron_db::PgConn,
    org_id: Uuid,
    app_id: Uuid,
    label: &str,
    perms: &[&str],
) -> String {
    let user = repo::create_user(
        conn,
        &format!("{label}@example.test"),
        "unused-password-hash",
        label,
    )
    .await
    .expect("create user");
    let role = repo::create_role(
        conn,
        org_id,
        &format!("{label} role"),
        "occurrences fixture",
        json!(perms),
    )
    .await
    .expect("create role");
    repo::create_grant(
        conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: role.id,
            scope_type: "app".to_string(),
            scope_id: app_id,
        },
    )
    .await
    .expect("grant role at app scope");
    let keys = JwtKeys::new(JWT_SECRET, 900);
    let (token, _) = keys
        .issue_access(user.id, false, None)
        .expect("issue access token");
    token
}

/// One occurrence with caller-chosen environment/`message`/`contexts`/`extra`/
/// `tags`/workflow stamp — the axes every occurrence-route predicate reads.
#[allow(clippy::too_many_arguments)]
async fn seed_occ_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env_id: Option<Uuid>,
    issue_id: Uuid,
    fingerprint: &str,
    message: &str,
    contexts: Value,
    extra: Value,
    tags: Value,
    workflow: Option<&str>,
    at: DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env_id,
            issue_id,
            fingerprint: fingerprint.to_string(),
            level: "error".into(),
            message: message.into(),
            exception_type: "Error".into(),
            exception_value: "occ fixture error".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags,
            release: None,
            distinct_id: Some(format!("occ-user-{}", Uuid::new_v4().simple())),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: at,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: workflow.map(|w| format!("wf-{w}")),
            workflow_name: workflow.map(str::to_string),
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts,
            extra,
            handled: Some(false),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");
}

/// The defect this slice removes, at the occurrence level.
///
/// Every row shares one `occurred_at`, so the whole walk is decided by the `id`
/// tiebreaker. A decoy issue with its own 25 occurrences at the same instant
/// sits beside it: a walk that lost the `issue_id` predicate would still page
/// plausibly and return 115 rows, not 90.
#[tokio::test]
async fn occurrences_page_stably_within_one_issue() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, issue_id, token) = srv.seed_issue_with_occurrences(90).await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/issues/{issue_id}/events?limit=20&cursor={c}"),
            None => format!("/v1/apps/{app_id}/issues/{issue_id}/events?limit=20"),
        };
        let page = srv.get_json(&url, &token).await;
        // Asserted on EVERY page, not just the first: `total` describes the
        // match set, not the page, so it must not drift as the walk advances —
        // a count that quietly followed the cursor would make every "1-20 of N"
        // footer a lie, and only the later pages would show it.
        assert_eq!(
            page["total"], 90,
            "total must count this issue alone: {page}"
        );
        assert_eq!(page["total_is_capped"], false);
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(
        deduped.len(),
        90,
        "paging must reach every occurrence of THIS issue and none of the decoy's"
    );

    srv.shutdown().await;
}

/// The equivalence that makes the legacy bridge safe on this route too.
///
/// `workflow` is the interesting field: `sauron_db::filter::ERROR_EVENT_FILTERS`
/// has always accepted `filter=workflow:<op>:<value>` here, but the catalog
/// declared `workflow` on `R_ISSUES` alone until Task 5. Left that way, this
/// bookmark would not error — `resolve_field`'s step-4 fallback would read the
/// bare field as a TAG KEY and probe `error_events.tags` instead, returning a
/// different set of rows with a 200. That silent wrong answer is what the two
/// assertions below pin.
#[tokio::test]
async fn occurrence_query_and_filter_return_identical_rows() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    let legacy = srv
        .get_json(
            &format!("{list}?filter=workflow:eq:{OCC_WORKFLOW}"),
            &fx.full_token,
        )
        .await;
    let modern = srv
        .get_json(
            &format!("{list}?query=workflow:{OCC_WORKFLOW}"),
            &fx.full_token,
        )
        .await;

    assert_eq!(ids(&legacy), ids(&modern), "legacy and query= disagree");
    assert_eq!(legacy["total"], modern["total"]);
    // Pin the fixture: an equivalence between two empty lists proves nothing,
    // and a predicate that matched everything would satisfy it just as well.
    assert_eq!(
        legacy["total"],
        OCC_MARKED,
        "expected the {OCC_MARKED} stamped occurrences, not all {}: {legacy}",
        OCC_MARKED + OCC_PLAIN
    );
    assert_eq!(ids(&legacy).len(), OCC_MARKED);

    // The unfiltered list is strictly larger, so the filter is doing work.
    let all = srv.get_json(&list, &fx.full_token).await;
    assert_eq!(all["total"], OCC_MARKED + OCC_PLAIN, "{all}");

    srv.shutdown().await;
}

/// The hole this task had to not open: `filter=` could only name `tag` and
/// `workflow`, but `query=` addresses eight `Store::JsonRoot` dimensions over
/// the very columns this caller's rows arrive with nulled. Every spelling of
/// the same probe must be refused, and the refusal must lift for a caller who
/// may read what it probes — otherwise the test proves only that the feature
/// is broken.
#[tokio::test]
async fn occurrence_payload_predicates_are_refused_without_event_read() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // One per withheld storage kind reachable on this resource: the five JSON
    // roots over `strip_event_body`'d columns, the tag column in BOTH
    // spellings, and `workflow` (served only by endpoints requiring
    // `event:read`). The nested forms are there because a check that only
    // looked at the top of the tree would pass the flat ones.
    let probes = [
        format!("query=extra.token:~{OCC_PAYLOAD_MARKER}"),
        format!("query=contexts.occ_ctx:{OCC_PAYLOAD_MARKER}"),
        "query=user.email:a@b.com".to_string(),
        "query=stack.filename:app.js".to_string(),
        "query=os.name:Linux".to_string(),
        "query=sdk.name:sauron".to_string(),
        format!("query=tag.occ_key:{OCC_PAYLOAD_MARKER}"),
        format!("filter=tag:eq:occ_key={OCC_PAYLOAD_MARKER}"),
        format!("query=workflow:{OCC_WORKFLOW}"),
        format!("filter=workflow:eq:{OCC_WORKFLOW}"),
        format!("query=!extra.token:~{OCC_PAYLOAD_MARKER}"),
        format!("query=level:error OR extra.token:~{OCC_PAYLOAD_MARKER}"),
    ];
    for probe in &probes {
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?{probe}"), &fx.shell_token)
            .await;
        assert_eq!(
            status, 403,
            "`{probe}` is a question about a column this caller's rows arrive with nulled and \
             must be refused, not answered: {body}"
        );
        assert!(
            body.contains("event:read"),
            "`{probe}`: the refusal must name the permission that lifts it: {body}"
        );

        // …and it DOES lift. Without this leg the handler could refuse
        // everything and pass.
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?{probe}"), &fx.full_token)
            .await;
        assert_eq!(
            status, 200,
            "`{probe}` must be served to a caller holding event:read: {body}"
        );
    }

    srv.shutdown().await;
}

/// `environment:<name>` is an environment-NAME enumeration oracle, and
/// `event:read` is not the permission that governs it.
///
/// `prepare` resolves every name in the tree app-wide (`resolve_environments`
/// is keyed on `app_id` alone), and a name with no row lowers to `Uuid::nil()`
/// — matching nothing — while a real one matches its rows. So the answer to
/// "does an environment called X exist in this app" is readable straight off
/// `total`, even when `data` is empty. Environment names are served only by
/// endpoints requiring `env:read`; this route authorizes on `issue:read`.
///
/// Three callers, because the interesting one is in the middle: `full_token`
/// holds `issue:read + event:read` and must STILL be refused. Before the fix
/// round, `reach.includes_body()` short-circuited the whole check and that
/// caller walked straight through.
#[tokio::test]
async fn environment_names_are_not_enumerable_without_env_read() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // Nested and negated forms too: a check that only looked at the top of the
    // tree would let the last three through.
    let probes = [
        format!("query=environment:{OCC_ENV_NAME}"),
        format!("query=environment:[{OCC_ENV_NAME},other]"),
        "query=has:environment".to_string(),
        format!("query=!environment:{OCC_ENV_NAME}"),
        format!("query=level:error OR environment:{OCC_ENV_NAME}"),
    ];
    for (label, token) in [
        ("issue:read only", &fx.shell_token),
        // The one that matters: event:read is a different permission.
        ("issue:read + event:read, no env:read", &fx.full_token),
    ] {
        for probe in &probes {
            let (status, body) = srv
                .get_status_and_body(&format!("{list}?{probe}"), token)
                .await;
            assert_eq!(
                status, 403,
                "{label}: `{probe}` resolves an environment NAME against the whole app and must \
                 be refused: {body}"
            );
            assert!(
                body.contains("env:read"),
                "{label}: `{probe}`: the refusal must name the permission that lifts it — \
                 `event:read` is not it: {body}"
            );
        }
    }

    // …and it lifts for a caller who may read environment names, WITHOUT
    // becoming "accept and match nothing": the predicate must actually
    // discriminate, or a refusal implemented as an empty result would pass.
    let matched = srv
        .get_json(
            &format!("{list}?query=environment:{OCC_ENV_NAME}"),
            &fx.env_token,
        )
        .await;
    assert_eq!(
        ids(&matched).len(),
        OCC_MARKED,
        "env:read must still be able to narrow by name — the gate is a permission check, not a \
         feature removal: {matched}"
    );
    assert_eq!(matched["total"], OCC_MARKED, "{matched}");

    // The other side of the same predicate, which is what makes it an oracle
    // in the first place: a name that does not exist matches nothing.
    let ghost = srv
        .get_json(
            &format!("{list}?query=environment:occ-ghost"),
            &fx.env_token,
        )
        .await;
    assert_eq!(ids(&ghost).len(), 0, "{ghost}");
    assert_eq!(ghost["total"], 0, "{ghost}");

    // And `?environment_id=` — a different mechanism entirely, authorization
    // -checked in `scope::authorized_read_scope` rather than resolved from a
    // name — is untouched by the gate for a caller with no `env:read`.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?limit=100"), &fx.shell_token)
        .await;
    assert_eq!(
        status, 200,
        "the gate must only bite the `environment:` PREDICATE, not the route: {body}"
    );

    srv.shutdown().await;
}

/// Free text is NARROWED, never refused — and the narrowing is real.
///
/// `OccurrencesLower::text` had no `text_reach` before Task 5: it emitted the
/// `contexts`/`extra`/`tags` scan unconditionally. Bridging this route onto it
/// as-is would have reintroduced the D4 oracle through the front door, with
/// every gate still green — the request 200s, the rows come back with the
/// payload nulled, and the row COUNT spells the withheld value out one probe
/// at a time.
#[tokio::test]
async fn occurrence_free_text_is_narrowed_not_refused_without_event_read() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // The withheld marker: found by the caller who may read it…
    let full = srv
        .get_json(&format!("{list}?q={OCC_PAYLOAD_MARKER}"), &fx.full_token)
        .await;
    assert_eq!(
        ids(&full).len(),
        OCC_MARKED,
        "the marker IS in contexts/extra/tags — if this fails the fixture is not seeding it and \
         the assertion below is vacuous: {full}"
    );

    // …and invisible to the caller who may not. `total` too: it is a second
    // query and would be an oracle of its own if built from a wider predicate.
    let shell = srv
        .get_json(&format!("{list}?q={OCC_PAYLOAD_MARKER}"), &fx.shell_token)
        .await;
    assert_eq!(
        ids(&shell).len(),
        0,
        "a caller without event:read receives these rows with contexts/extra/tags nulled; \
         matching them here answers the question the response withholds: {shell}"
    );
    assert_eq!(
        shell["total"], 0,
        "the count must not distinguish them either: {shell}"
    );

    // Narrowed, NOT disabled: the columns this caller CAN read back are still
    // searched. A route that answered nothing would satisfy the assertion above
    // and have destroyed the feature.
    let shell_shell = srv
        .get_json(&format!("{list}?q={OCC_SHELL_MARKER}"), &fx.shell_token)
        .await;
    assert_eq!(
        ids(&shell_shell).len(),
        OCC_MARKED,
        "`message` is kept by strip_event_body, so it must still be searchable: {shell_shell}"
    );

    srv.shutdown().await;
}

/// The occurrences list must not leak across the environment boundary either.
///
/// `error_events` carries `environment_id`, so this is an ordinary `scope_env!`
/// filter rather than Issues' derived membership — which is exactly why it is
/// easy to drop while rewriting the query, and why it gets its own test.
#[tokio::test]
async fn occurrences_stay_within_the_callers_environment() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_env_oracle_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // The member holds ONE of the issue's two environments.
    let mine = srv.get_json(&list, &fx.member_token).await;
    assert_eq!(
        ids(&mine).len(),
        1,
        "the member must see their environment's occurrence and only it: {mine}"
    );
    assert_eq!(mine["total"], 1, "the count is scoped too: {mine}");
    assert_eq!(
        mine["data"][0]["tags"]["oracle_key"], IN_REACH_MARKER,
        "and it must be the RIGHT one: {mine}"
    );

    // The app-wide caller sees both — without this the fixture could have
    // stopped seeding the second occurrence and the assertion above would pass
    // vacuously.
    let owner = srv.get_json(&list, &fx.owner_token).await;
    assert_eq!(ids(&owner).len(), 2, "{owner}");
    assert_eq!(owner["total"], 2, "{owner}");

    // A predicate must not reach across it either: the out-of-reach marker has
    // to be indistinguishable from one that exists nowhere.
    let control = srv
        .get_json(&format!("{list}?q={ABSENT_MARKER}"), &fx.member_token)
        .await;
    let probe = srv
        .get_json(&format!("{list}?q={OUT_OF_REACH_MARKER}"), &fx.member_token)
        .await;
    assert_eq!(
        ids(&control).len(),
        0,
        "the control must match nothing: {control}"
    );
    assert_eq!(
        ids(&probe),
        ids(&control),
        "`{OUT_OF_REACH_MARKER}` exists only in an environment this member holds no grant on: \
         {probe}"
    );
    assert_eq!(probe["total"], control["total"], "{probe}");

    srv.shutdown().await;
}

/// Same two caller mistakes as the issues list, on the new route.
#[tokio::test]
async fn occurrence_paging_inputs_are_validated() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // An ordering with no supporting `(…, id)` index cannot page stably, so it
    // is refused rather than served.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?sort=received_at"), &fx.full_token)
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("received_at"), "must name the field: {body}");
    assert!(
        body.contains("occurred_at"),
        "must list what is allowed: {body}"
    );

    // The default IS allowed, in both directions.
    for sort in ["occurred_at", "-occurred_at"] {
        let page = srv
            .get_json(&format!("{list}?sort={sort}"), &fx.full_token)
            .await;
        assert_eq!(ids(&page).len(), OCC_MARKED + OCC_PLAIN, "{page}");
    }

    for bad in ["not-a-cursor", "Zm9v", "!!!!"] {
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?cursor={bad}"), &fx.full_token)
            .await;
        assert_eq!(status, 400, "cursor={bad} returned {status}: {body}");
    }

    srv.shutdown().await;
}

/// A cursor's `key` and its `t`/`s` value type tag are independent fields on
/// the wire (see `cursor.rs`'s module doc comment): matching the key alone
/// used to be enough to pass `decode`, so a `device_key|<uuid>|t:…`
/// cursor — the wrong VALUE kind for a text column — sailed through. Read via
/// `text_of`'s total fallback in `repo.rs`, that silently produced `""`,
/// which does not error: ascending, `COALESCE(device_key,'') > ''` matches
/// (almost) every row, so page two would repeat page one forever, not skip
/// or crash. `decode` now takes the sort's `is_temporal` alongside its key,
/// so this must be a 400, never a page — repeating or otherwise.
///
/// `device_key` doubles as the HTTP-level coverage neither this file nor a
/// browser session previously exercised: every other Occurrences paging test
/// above sorts by `occurred_at` only.
#[tokio::test]
async fn an_occurrence_cursor_with_a_forged_value_kind_is_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, issue_id, token) = srv.seed_issue_with_occurrences(5).await;
    let list = format!("/v1/apps/{app_id}/issues/{issue_id}/events");

    // `OccurrenceSort::DeviceKey::is_temporal()` is `false` — `device_key` is
    // a text column — so a cursor under that key correctly carries
    // `CursorValue::Text`. This one carries `CursorValue::Ts` instead: right
    // key, wrong kind, exactly the shape `decode`'s key-only check used to
    // let through.
    let forged = sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
        key: "device_key".to_string(),
        value: sauron_db::query_plan::cursor::CursorValue::Ts(Utc::now()),
        id: Uuid::new_v4(),
    });

    let (status, body) = srv
        .get_status_and_body(&format!("{list}?sort=device_key&cursor={forged}"), &token)
        .await;
    assert_eq!(status, 400, "{body}");
    // Wording unique to `CursorError::KindMismatch`, not just any 400 that
    // happens to mention the column: `parse_sort`'s "cannot sort by `X`"
    // rejection ALSO contains `device_key` (it lists the whole whitelist),
    // so `body.contains("device_key")` alone would pass on the wrong 400 —
    // e.g. a regression that broke the sort whitelist instead of the cursor
    // kind check. "requires a text value" appears only in `KindMismatch`'s
    // message, the same way `a_cursor_from_another_sort_is_refused` pins
    // `KeyMismatch` on "start from the first page".
    assert!(
        body.contains("requires a text value"),
        "error should be the cursor's KindMismatch specifically, not merely \
         a 400 that happens to mention the column: {body}"
    );

    srv.shutdown().await;
}

/// `-occurred_at` reverses the walk, and the reversed walk must reach the same
/// rows — a keyset predicate that disagreed with its own ORDER BY would page
/// one direction correctly and silently truncate the other.
#[tokio::test]
async fn ascending_occurrence_paging_reaches_every_row() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, issue_id, token) = srv.seed_issue_with_occurrences(45).await;
    let list = format!("/v1/apps/{app_id}/issues/{issue_id}/events");

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("{list}?sort=-occurred_at&limit=10&cursor={c}"),
            None => format!("{list}?sort=-occurred_at&limit=10"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(
        deduped.len(),
        45,
        "ascending paging did not reach every row"
    );

    srv.shutdown().await;
}

/// **The stats/list vocabulary split, closed (S2c Task 6).**
///
/// The stat strip describes the rows beside it — `dashboard/src/lib/api/issues.ts`
/// builds both requests from one `occurrenceParams` object — so the two must
/// accept the same inputs and answer over the same predicate. Until Task 6 they
/// did not: the list resolved against `Resource::Occurrences` while the stats
/// ran `parse_filters(…, ERROR_EVENT_FILTERS)`, whose whole vocabulary is
/// `tag`/`workflow`. `filter=level:eq:error` was a 200 on one and a 400 on the
/// other, from the same object. `query=` had to be refused here outright for
/// the same reason.
///
/// Every assertion below compares the stats against the LIST's envelope rather
/// than a hardcoded number wherever it can, because agreement between the two
/// is the actual property — a pair of numbers that were both wrong in the same
/// way would still be a working stat strip, and a pair that disagree is the
/// defect no matter which one is "right".
#[tokio::test]
async fn event_stats_answers_the_same_predicate_as_the_list_beside_it() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let stats = format!("/v1/apps/{}/issues/{}/events/stats", fx.app_id, fx.issue_id);
    let list = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    for params in [
        String::new(),
        format!("?filter=workflow:eq:{OCC_WORKFLOW}"),
        // The spelling that used to be a flat 400 here.
        format!("?query=workflow:{OCC_WORKFLOW}"),
        // The vocabulary split itself: `level` is a real dimension on
        // Occurrences and was never in `ERROR_EVENT_FILTERS`.
        "?filter=level:eq:error".to_string(),
        "?query=level:error".to_string(),
        // Free text, both spellings — the `payload_searched` input.
        format!("?q={OCC_SHELL_MARKER}"),
        format!("?query={OCC_SHELL_MARKER}"),
    ] {
        // `params` already carries its own `?` when non-empty, so the extra
        // parameter joins with `&` only in that case — an unconditional `&`
        // produced a pathless `…/events&limit=100`, which is a 404 rather than
        // the comparison this test means to make.
        let join = if params.is_empty() { "?" } else { "&" };
        let counted = srv
            .get_json(&format!("{stats}{params}"), &fx.full_token)
            .await;
        let listed = srv
            .get_json(&format!("{list}{params}{join}limit=100"), &fx.full_token)
            .await;
        assert_eq!(
            counted["events"], listed["total"],
            "`{params}`: the stat strip and the list it describes disagree: {counted} vs {listed}"
        );
    }

    // Pin the fixture so the agreements above are not all between zeroes.
    let all = srv.get_json(&stats, &fx.full_token).await;
    assert_eq!(all["events"], OCC_MARKED + OCC_PLAIN, "{all}");
    let stamped = srv
        .get_json(
            &format!("{stats}?query=workflow:{OCC_WORKFLOW}"),
            &fx.full_token,
        )
        .await;
    assert_eq!(stamped["events"], OCC_MARKED, "{stamped}");

    // `payload_searched`'s three states, now derived from the resolved tree so
    // both free-text spellings report identically.
    assert!(
        all["payload_searched"].is_null(),
        "no free-text term ran: {all}"
    );
    for params in [
        format!("?q={OCC_SHELL_MARKER}"),
        format!("?query={OCC_SHELL_MARKER}"),
    ] {
        let searched = srv
            .get_json(&format!("{stats}{params}"), &fx.full_token)
            .await;
        assert_eq!(
            searched["payload_searched"], true,
            "`{params}`: a caller with event:read searched the payload too: {searched}"
        );
        let narrowed = srv
            .get_json(&format!("{stats}{params}"), &fx.shell_token)
            .await;
        assert_eq!(
            narrowed["payload_searched"], false,
            "`{params}`: this caller's search WAS narrowed and must be told: {narrowed}"
        );
    }
    // An empty `?q=` is not a search that ran.
    let empty = srv.get_json(&format!("{stats}?q="), &fx.full_token).await;
    assert!(empty["payload_searched"].is_null(), "{empty}");

    srv.shutdown().await;
}

/// The bridge must not have widened what the stats will answer: a withheld
/// predicate is a sharper oracle as an exact COUNT than as a page of rows, so
/// every refusal the list makes, this route must make too.
#[tokio::test]
async fn event_stats_refuses_every_withheld_predicate_the_list_does() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let stats = format!("/v1/apps/{}/issues/{}/events/stats", fx.app_id, fx.issue_id);

    // Withheld from a bare `issue:read` caller — including the JSON roots no
    // `filter=` string could ever name, which is exactly what the old
    // string-level `reject_body_filters` could not see.
    for probe in [
        format!("query=extra.token:~{OCC_PAYLOAD_MARKER}"),
        format!("query=contexts.occ_ctx:{OCC_PAYLOAD_MARKER}"),
        "query=user.email:a@b.com".to_string(),
        "query=stack.filename:app.js".to_string(),
        format!("query=tag.occ_key:{OCC_PAYLOAD_MARKER}"),
        format!("filter=tag:eq:occ_key={OCC_PAYLOAD_MARKER}"),
        format!("query=workflow:{OCC_WORKFLOW}"),
        format!("filter=workflow:eq:{OCC_WORKFLOW}"),
    ] {
        let (status, body) = srv
            .get_status_and_body(&format!("{stats}?{probe}"), &fx.shell_token)
            .await;
        assert_eq!(status, 403, "`{probe}` must be refused here too: {body}");
        assert!(body.contains("event:read"), "`{probe}`: {body}");
    }

    // …and the environment gate, which `event:read` does not lift.
    for token in [&fx.shell_token, &fx.full_token] {
        let (status, body) = srv
            .get_status_and_body(&format!("{stats}?query=environment:{OCC_ENV_NAME}"), token)
            .await;
        assert_eq!(status, 403, "{body}");
        assert!(body.contains("env:read"), "{body}");
    }
    // It lifts for a caller who holds it, and still discriminates.
    let scoped = srv
        .get_json(
            &format!("{stats}?query=environment:{OCC_ENV_NAME}"),
            &fx.env_token,
        )
        .await;
    assert_eq!(scoped["events"], OCC_MARKED, "{scoped}");

    // Free text is narrowed rather than refused, and the narrowing is real:
    // the withheld marker is invisible in the COUNT, which is the sharpest
    // form of the oracle.
    let full = srv
        .get_json(&format!("{stats}?q={OCC_PAYLOAD_MARKER}"), &fx.full_token)
        .await;
    assert_eq!(full["events"], OCC_MARKED, "{full}");
    let shell = srv
        .get_json(&format!("{stats}?q={OCC_PAYLOAD_MARKER}"), &fx.shell_token)
        .await;
    assert_eq!(
        shell["events"], 0,
        "the count must not distinguish: {shell}"
    );
    // Narrowed, NOT disabled.
    let shell_shell = srv
        .get_json(&format!("{stats}?q={OCC_SHELL_MARKER}"), &fx.shell_token)
        .await;
    assert_eq!(shell_shell["events"], OCC_MARKED, "{shell_shell}");

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// The analytics EVENTS list (S2c Task 6)
// ---------------------------------------------------------------------------
//
// `GET /v1/apps/{app_id}/events/list` -> `routes::analytics::events_list`. Same
// envelope, same cursor, same seam as the two lists above, over
// `(occurred_at, id)` and backed by `analytics_events_app_time_id_idx`.
//
// Two things make this route DIFFERENT from the other two, and both get a test
// rather than a comment:
//
// 1. **It authorizes on `event:read`, and nothing in its response is stripped.**
//    `symbolicate::strip_event_body` is an `ErrorEvent` function; an
//    `AnalyticsEvent`'s `properties`/`contexts`/`extra`/`tags` are served in
//    full to every authorized caller. So a payload predicate here probes
//    nothing withheld and must be SERVED. The caller that proves it is one
//    holding `event:read` ALONE — `symbolicate::may_read_event_body` requires
//    `issue:read` too, so a naive `text_search_reach(&perms)` would hand that
//    caller `ShellOnly` and 403 their own analytics filters.
// 2. **`analytics_events` HAS an `environment_id` column**, so environment
//    scope is an ordinary `scope_env!` filter — and `environment:<name>` is a
//    real name-resolution oracle that `env:read` governs.

/// Present only in `properties`/`contexts`/`extra`/`tags` — the four columns a
/// payload predicate can address on this resource.
const EV_PAYLOAD_MARKER: &str = "ev-payload-marker";
/// The workflow stamp on the marked events.
const EV_WORKFLOW: &str = "ev-checkout";
/// The environment the marked events are attributed to.
const EV_ENV_NAME: &str = "ev-prod";
/// How many of the fixture's events carry all of the above.
const EV_MARKED: usize = 3;
/// How many carry none of them.
const EV_PLAIN: usize = 7;
/// Synthetic screen-view rows. They are `analytics_events` rows like any other
/// and must NEVER reach this list — see `EventsLower::base_scope`.
const EV_SCREENS: usize = 4;

struct EvFixture {
    app_id: Uuid,
    /// `event:read` + `issue:read` + `env:read`: the only one entitled to
    /// resolve an environment NAME.
    env_token: String,
    /// `event:read` + `issue:read`, deliberately **no `env:read`**: the caller
    /// that proves `event:read` is not a substitute for it.
    full_token: String,
    /// `event:read` ALONE — the plausible analytics-only custom role. Nothing
    /// on this resource is withheld from them, so every payload predicate must
    /// be served rather than refused.
    analytics_token: String,
}

impl TestServer {
    /// `EV_MARKED` marked + `EV_PLAIN` plain analytics events, `EV_SCREENS`
    /// synthetic `$screen` rows, and three callers who differ only in
    /// permissions.
    ///
    /// The marked events are attributed to a real environment named
    /// `EV_ENV_NAME` and the plain ones to none, so the `env:read` gate can be
    /// tested with a predicate that genuinely discriminates rather than one
    /// that matches everything or nothing. Every token is granted at APP scope,
    /// so all three resolve to `EnvFilter::All` and see the same rows.
    async fn seed_analytics_fixture(&self) -> EvFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "ev org", &format!("ev-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "ev project",
            &format!("ev-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "ev app",
            &format!("ev-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            EV_ENV_NAME,
            &format!("pk_ev_{suffix}"),
            true,
        )
        .await;

        let now = Utc::now();
        for i in 0..EV_MARKED {
            seed_analytics_event(
                &mut conn,
                app.id,
                Some(env_id),
                "checkout_started",
                json!({ "plan": EV_PAYLOAD_MARKER }),
                json!({ "ev_ctx": EV_PAYLOAD_MARKER }),
                json!({ "token": EV_PAYLOAD_MARKER }),
                json!({ "ev_key": EV_PAYLOAD_MARKER }),
                Some(EV_WORKFLOW),
                now - ChronoDuration::minutes(i as i64 + 1),
            )
            .await;
        }
        for i in 0..EV_PLAIN {
            seed_analytics_event(
                &mut conn,
                app.id,
                None,
                "page_view",
                json!({}),
                json!({}),
                json!({}),
                json!({}),
                None,
                now - ChronoDuration::minutes(i as i64 + 20),
            )
            .await;
        }
        // Synthetic screen views: real rows in the same table, excluded from
        // this list by definition rather than by a filter a query could opt out
        // of.
        for i in 0..EV_SCREENS {
            seed_analytics_event(
                &mut conn,
                app.id,
                Some(env_id),
                "$screen",
                json!({ "plan": EV_PAYLOAD_MARKER }),
                json!({}),
                json!({}),
                json!({}),
                Some(EV_WORKFLOW),
                now - ChronoDuration::minutes(i as i64 + 40),
            )
            .await;
        }

        let env = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("ev-env-{suffix}"),
            &[perm::EVENT_READ, perm::ISSUE_READ, perm::ENV_READ],
        )
        .await;
        // NO `env:read`: the caller that proves `event:read` does not stand in
        // for it.
        let full = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("ev-full-{suffix}"),
            &[perm::EVENT_READ, perm::ISSUE_READ],
        )
        .await;
        // `event:read` ONLY. `may_read_event_body` also wants `issue:read`, so
        // this caller is exactly the one a reach derived from that predicate
        // would wrongly narrow.
        let analytics = seed_reader(
            &mut conn,
            org.id,
            app.id,
            &format!("ev-analytics-{suffix}"),
            &[perm::EVENT_READ],
        )
        .await;
        drop(conn);

        EvFixture {
            app_id: app.id,
            env_token: env,
            full_token: full,
            analytics_token: analytics,
        }
    }

    /// `count` analytics events that all share ONE `occurred_at`.
    ///
    /// The same trick the other two paging fixtures play: with every row tied
    /// on the sort column the whole walk is decided by the `id` tiebreaker, so
    /// a page boundary can never fall on a "next value". Without Task 1's
    /// `(app_id, occurred_at DESC, id DESC)` index — and without `id` in the
    /// keyset tuple — this either loops on page one or skips the rest of the
    /// tie group.
    async fn seed_events_sharing_a_timestamp(&self, count: usize) -> (Uuid, String) {
        let (app_id, token) = self.seed_app("ev-paging").await;
        let mut conn = self.conn().await;
        let at = Utc::now() - ChronoDuration::hours(1);
        for _ in 0..count {
            seed_analytics_event(
                &mut conn,
                app_id,
                None,
                "paged_event",
                json!({}),
                json!({}),
                json!({}),
                json!({}),
                None,
                at,
            )
            .await;
        }
        (app_id, token)
    }
}

/// One analytics event with caller-chosen environment/name/payload/workflow.
#[allow(clippy::too_many_arguments)]
async fn seed_analytics_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env_id: Option<Uuid>,
    name: &str,
    properties: Value,
    contexts: Value,
    extra: Value,
    tags: Value,
    workflow: Option<&str>,
    at: DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env_id,
            name: name.to_string(),
            distinct_id: format!("ev-user-{}", Uuid::new_v4().simple()),
            properties,
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at: at,
            device_key: None,
            screen: None,
            workflow_id: workflow.map(|w| format!("wf-{w}")),
            workflow_name: workflow.map(str::to_string),
            tags,
            contexts,
            extra,
        },
    )
    .await
    .expect("insert analytics event");
}

/// The defect this slice removes, on the analytics stream.
///
/// Every row shares one `occurred_at`, so the ordering is decided entirely by
/// the `id` tiebreaker Task 1's index added.
#[tokio::test]
async fn events_page_stably_across_a_shared_timestamp() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_events_sharing_a_timestamp(75).await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/events/list?limit=20&cursor={c}"),
            None => format!("/v1/apps/{app_id}/events/list?limit=20"),
        };
        let page = srv.get_json(&url, &token).await;
        // Asserted on EVERY page: `total` describes the match set, not the
        // page, so a count that quietly followed the cursor would make every
        // "1-20 of N" footer a lie and only the later pages would show it.
        assert_eq!(page["total"], 75, "{page}");
        assert_eq!(page["total_is_capped"], false);
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        seen.len(),
        deduped.len(),
        "a row was returned on two pages — Task 1's index is missing or unused"
    );
    assert_eq!(deduped.len(), 75);

    srv.shutdown().await;
}

/// `-occurred_at` reverses the walk, and the reversed walk must reach the same
/// rows — a keyset predicate that disagreed with its own ORDER BY would page
/// one direction correctly and silently truncate the other.
#[tokio::test]
async fn ascending_event_paging_reaches_every_row() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_events_sharing_a_timestamp(45).await;
    let list = format!("/v1/apps/{app_id}/events/list");

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("{list}?sort=-occurred_at&limit=10&cursor={c}"),
            None => format!("{list}?sort=-occurred_at&limit=10"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(
        deduped.len(),
        45,
        "ascending paging did not reach every row"
    );

    srv.shutdown().await;
}

/// The equivalence that makes the legacy bridge safe on this route too.
///
/// `workflow` is the field that proves it: `sauron_db::filter::EVENT_FILTERS`
/// has always accepted `filter=workflow:<op>:<value>` here, but the catalog
/// declared `workflow` on `R_ISSUE_OCC` only until Task 6. Left that way this
/// bookmark would not error — `resolve_field`'s step-4 fallback would read the
/// bare field as a TAG KEY and probe `analytics_events.tags` instead, returning
/// a different set of rows with a 200.
#[tokio::test]
async fn event_query_and_filter_return_identical_rows() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    for (legacy_q, modern_q, expected) in [
        (
            format!("filter=workflow:eq:{EV_WORKFLOW}"),
            format!("query=workflow:{EV_WORKFLOW}"),
            EV_MARKED,
        ),
        (
            "filter=name:eq:page_view".to_string(),
            "query=name:page_view".to_string(),
            EV_PLAIN,
        ),
        (
            format!("filter=tag:eq:ev_key={EV_PAYLOAD_MARKER}"),
            format!("query=tag.ev_key:{EV_PAYLOAD_MARKER}"),
            EV_MARKED,
        ),
    ] {
        let legacy = srv
            .get_json(&format!("{list}?{legacy_q}"), &fx.full_token)
            .await;
        let modern = srv
            .get_json(&format!("{list}?{modern_q}"), &fx.full_token)
            .await;
        assert_eq!(
            ids(&legacy),
            ids(&modern),
            "`{legacy_q}` and `{modern_q}` disagree"
        );
        assert_eq!(legacy["total"], modern["total"]);
        // Pin the fixture: an equivalence between two empty lists proves
        // nothing, and a predicate matching everything would satisfy it too.
        assert_eq!(
            legacy["total"],
            expected,
            "`{legacy_q}` should select {expected} of {}: {legacy}",
            EV_MARKED + EV_PLAIN
        );
    }

    // The unfiltered list is strictly larger, so the filters are doing work.
    let all = srv.get_json(&list, &fx.full_token).await;
    assert_eq!(all["total"], EV_MARKED + EV_PLAIN, "{all}");

    srv.shutdown().await;
}

/// `workflow` must reach the real `workflow_name` column, not the tag store.
///
/// The failure mode this guards is silent: `resolve_field`'s step-4 fallback
/// makes `workflow:x` a TAG probe, which 200s with the wrong rows. The negated
/// spelling is the second half — `neq` must KEEP the unstamped rows, the way
/// `list_analytics_events`' hand-written arm did, or one chip means two
/// opposite things at two levels of the same drill-down.
#[tokio::test]
async fn event_workflow_is_the_real_column_and_negation_keeps_unstamped_rows() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    // A tag key literally named `workflow` exists nowhere in the fixture, so a
    // tag-store fallback would return 0 rather than EV_MARKED.
    let stamped = srv
        .get_json(
            &format!("{list}?query=workflow:{EV_WORKFLOW}"),
            &fx.full_token,
        )
        .await;
    assert_eq!(stamped["total"], EV_MARKED, "{stamped}");

    // `contains` is the third operator the legacy wire format accepted.
    let partial = srv
        .get_json(
            &format!("{list}?filter=workflow:contains:check"),
            &fx.full_token,
        )
        .await;
    assert_eq!(partial["total"], EV_MARKED, "{partial}");

    // The unstamped rows are exactly the ones `workflow_id IS NOT NULL` (the
    // partial-index term) excludes, so a `neq` that reused it would return 0.
    for spelling in [
        format!("query=!workflow:{EV_WORKFLOW}"),
        format!("filter=workflow:neq:{EV_WORKFLOW}"),
    ] {
        let unstamped = srv
            .get_json(&format!("{list}?{spelling}"), &fx.full_token)
            .await;
        assert_eq!(
            unstamped["total"], EV_PLAIN,
            "`{spelling}` must keep the unstamped rows: {unstamped}"
        );
    }

    srv.shutdown().await;
}

/// **The `properties`/`extra` gate decision, pinned.**
///
/// `search::NON_WITHHELD_JSON_COLUMNS` is keyed on the COLUMN name and lists
/// only `properties`, so a `ShellOnly` reach would refuse `extra.*` and
/// `contexts.*` here. That list is right for Occurrences — `error_events.extra`
/// really is nulled by `strip_event_body` — and wrong for this route, where an
/// `AnalyticsEvent` is served whole. The fix is at the CALLER (this route
/// passes the honest reach), never by widening the shared column list, which
/// would open the hole one level up.
///
/// The caller here holds `event:read` ALONE: `may_read_event_body` also wants
/// `issue:read`, so a reach derived from it would narrow exactly this caller.
#[tokio::test]
async fn analytics_payload_predicates_are_served_not_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    // Every payload shape reachable on `Resource::Events`, plus `workflow`
    // (served only by endpoints requiring `event:read` — which this route is).
    let probes = [
        format!("query=properties.plan:{EV_PAYLOAD_MARKER}"),
        format!("query=extra.token:{EV_PAYLOAD_MARKER}"),
        format!("query=contexts.ev_ctx:{EV_PAYLOAD_MARKER}"),
        format!("query=tag.ev_key:{EV_PAYLOAD_MARKER}"),
        format!("filter=tag:eq:ev_key={EV_PAYLOAD_MARKER}"),
        format!("query=workflow:{EV_WORKFLOW}"),
        format!("q={EV_PAYLOAD_MARKER}"),
    ];
    for probe in &probes {
        let page = srv
            .get_json(&format!("{list}?{probe}"), &fx.analytics_token)
            .await;
        assert_eq!(
            page["total"], EV_MARKED,
            "`{probe}` addresses a column this route serves in full — it must be answered, and \
             answered correctly: {page}"
        );
    }

    // …and the rows really do carry the payload back, so there is nothing the
    // predicate could be disclosing that the response withholds.
    let page = srv
        .get_json(
            &format!("{list}?query=properties.plan:{EV_PAYLOAD_MARKER}"),
            &fx.analytics_token,
        )
        .await;
    assert_eq!(
        page["data"][0]["properties"]["plan"], EV_PAYLOAD_MARKER,
        "{page}"
    );
    assert_eq!(
        page["data"][0]["extra"]["token"], EV_PAYLOAD_MARKER,
        "{page}"
    );
    assert_eq!(
        page["data"][0]["tags"]["ev_key"], EV_PAYLOAD_MARKER,
        "{page}"
    );

    srv.shutdown().await;
}

/// `environment:<name>` is a name-enumeration oracle here exactly as it is on
/// the occurrences list, and `event:read` — the permission that authorizes this
/// whole route — is not the one that governs it.
///
/// This is a pre-existing hole, not one Task 6 introduces:
/// `repo::list_analytics_events` already resolved `filter=environment:eq:<name>`
/// app-wide with no `env:read` check.
#[tokio::test]
async fn event_environment_names_are_not_enumerable_without_env_read() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    let probes = [
        format!("query=environment:{EV_ENV_NAME}"),
        format!("query=environment:[{EV_ENV_NAME},other]"),
        "query=has:environment".to_string(),
        format!("query=!environment:{EV_ENV_NAME}"),
        format!("query=name:page_view OR environment:{EV_ENV_NAME}"),
        // The legacy spelling reaches the identical predicate through
        // `from_legacy`, so a string-level check would be bypassed by it.
        format!("filter=environment:eq:{EV_ENV_NAME}"),
    ];
    for (label, token) in [
        ("event:read only", &fx.analytics_token),
        ("event:read + issue:read, no env:read", &fx.full_token),
    ] {
        for probe in &probes {
            let (status, body) = srv
                .get_status_and_body(&format!("{list}?{probe}"), token)
                .await;
            assert_eq!(
                status, 403,
                "{label}: `{probe}` resolves an environment NAME against the whole app and must \
                 be refused: {body}"
            );
            assert!(
                body.contains("env:read"),
                "{label}: `{probe}`: the refusal must name the permission that lifts it: {body}"
            );
        }
    }

    // …and it lifts for a caller who may read environment names, WITHOUT
    // becoming "accept and match nothing".
    let matched = srv
        .get_json(
            &format!("{list}?query=environment:{EV_ENV_NAME}"),
            &fx.env_token,
        )
        .await;
    assert_eq!(matched["total"], EV_MARKED, "{matched}");
    let ghost = srv
        .get_json(&format!("{list}?query=environment:ev-ghost"), &fx.env_token)
        .await;
    assert_eq!(ghost["total"], 0, "{ghost}");

    // The gate bites the PREDICATE, not the route.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?limit=100"), &fx.analytics_token)
        .await;
    assert_eq!(status, 200, "{body}");

    srv.shutdown().await;
}

/// Synthetic `$screen` rows belong to the Screens section, not the event
/// stream. That exclusion is part of what "an analytics event" MEANS here, so
/// it must survive every spelling — including a query that names `$screen`
/// explicitly, and a payload predicate that all four screen rows would match.
#[tokio::test]
async fn synthetic_screen_views_never_enter_the_event_stream() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    let all = srv
        .get_json(&format!("{list}?limit=200"), &fx.full_token)
        .await;
    assert_eq!(
        all["total"],
        EV_MARKED + EV_PLAIN,
        "the {EV_SCREENS} $screen rows must not be counted: {all}"
    );
    assert_eq!(ids(&all).len(), EV_MARKED + EV_PLAIN, "{all}");

    // Named explicitly, and reached through a payload predicate every screen
    // row matches: both must still exclude them.
    for probe in [
        "query=name:$screen".to_string(),
        "filter=name:eq:$screen".to_string(),
    ] {
        let page = srv
            .get_json(&format!("{list}?{probe}"), &fx.full_token)
            .await;
        assert_eq!(page["total"], 0, "`{probe}` must return nothing: {page}");
    }
    // `properties.plan` is set on the marked rows AND on every $screen row, so
    // a lost exclusion shows up as EV_MARKED + EV_SCREENS here.
    let payload = srv
        .get_json(
            &format!("{list}?query=properties.plan:{EV_PAYLOAD_MARKER}"),
            &fx.full_token,
        )
        .await;
    assert_eq!(payload["total"], EV_MARKED, "{payload}");

    srv.shutdown().await;
}

/// Same two caller mistakes as the other two lists, on the new route.
#[tokio::test]
async fn event_paging_inputs_are_validated() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_analytics_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    let (status, body) = srv
        .get_status_and_body(&format!("{list}?sort=received_at"), &fx.full_token)
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("received_at"), "must name the field: {body}");
    assert!(
        body.contains("occurred_at"),
        "must list what is allowed: {body}"
    );

    for sort in ["occurred_at", "-occurred_at"] {
        let page = srv
            .get_json(&format!("{list}?sort={sort}"), &fx.full_token)
            .await;
        assert_eq!(ids(&page).len(), EV_MARKED + EV_PLAIN, "{page}");
    }

    for bad in ["not-a-cursor", "Zm9v", "!!!!"] {
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?cursor={bad}"), &fx.full_token)
            .await;
        assert_eq!(status, 400, "cursor={bad} returned {status}: {body}");
    }

    // **An unknown field is a 400 on every bridged route, and the message has
    // to be actionable.**
    //
    // An earlier revision of this test pinned the opposite: `resolve_field`'s
    // step-4 fallback read any unrecognised name as a TAG KEY, so `nope:eq:x`
    // answered 200 with zero rows. That is the same answer an honest "nothing
    // matched" gives, which made a typo — `enviroment`, `times_seeen` —
    // invisible: the page looked empty and correct. The fallback is gone, and
    // this pins the replacement rather than the fact that a 400 happens.
    //
    // Asserted on BOTH routes, and on `query=` as well as `filter=`, because
    // one vocabulary resolved two ways is the exact drift this slice exists to
    // prevent — the seam is shared precisely so `filter=` cannot end up laxer
    // than `query=`.
    for path in [
        format!("{list}?filter=nope:eq:x"),
        format!("{list}?query=nope:x"),
        format!("/v1/apps/{}/issues?filter=nope:eq:x", fx.app_id),
        format!("/v1/apps/{}/issues?query=nope:x", fx.app_id),
    ] {
        let (status, body) = srv.get_status_and_body(&path, &fx.full_token).await;
        assert_eq!(
            status, 400,
            "an unknown field must be refused, not read as a tag: {path} -> {body}"
        );
        assert!(
            body.contains("nope"),
            "must name the field: {path} -> {body}"
        );
        assert!(
            body.contains("tag.nope"),
            "must say how to filter on a tag instead: {path} -> {body}"
        );
        assert!(
            body.contains("Available fields"),
            "must say what IS filterable here: {path} -> {body}"
        );
    }
    // The list offered is the one for THAT resource, not a shared superset.
    let (_, body) = srv
        .get_status_and_body(&format!("{list}?filter=nope:eq:x"), &fx.full_token)
        .await;
    assert!(body.contains("distinctId"), "events fields: {body}");
    let (_, body) = srv
        .get_status_and_body(
            &format!("/v1/apps/{}/issues?filter=nope:eq:x", fx.app_id),
            &fx.full_token,
        )
        .await;
    assert!(body.contains("culprit"), "issues fields: {body}");
    // `session`, not `distinctId`. `distinctId` was the sentinel here until
    // Issues gained the three occurrence columns — it is now a legitimate
    // Issues field, so asserting its ABSENCE stopped testing "the lists are
    // per-resource" and started testing "this dimension has not been added
    // yet", which is a different and much less useful claim. `session` has no
    // `Resource::Issues` in its catalog entry and no reason to gain one: an
    // issue is a group, and asking which session it belongs to has no answer.
    assert!(
        !body.contains("session"),
        "issues must not advertise an Events-only field: {body}"
    );
    // The converse leg, so this cannot pass again by the lists collapsing into
    // one another from the other direction.
    let (_, events_body) = srv
        .get_status_and_body(&format!("{list}?filter=nope:eq:x"), &fx.full_token)
        .await;
    assert!(
        !events_body.contains("culprit"),
        "events must not advertise an Issues-only field: {events_body}"
    );

    // And the capability the fallback used to provide still exists, spelled
    // out — otherwise the 400 above would be a regression rather than a fix.
    for path in [
        format!("{list}?filter=tag:eq:nope=x"),
        format!("{list}?query=tag.nope:x"),
    ] {
        let (status, body) = srv.get_status_and_body(&path, &fx.full_token).await;
        assert_eq!(status, 200, "{path} -> {body}");
        let page: Value = serde_json::from_str(&body).expect("envelope");
        assert_eq!(page["total"], 0, "{path} -> {page}");
    }

    // Structural nonsense is still a 400 — `filter=` without its `:op:value`
    // segments cannot be bridged at all, so validation has not simply stopped.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?filter=nope"), &fx.full_token)
        .await;
    assert_eq!(status, 400, "{body}");

    // `offset=` is accepted and ignored: an existing bookmark must not 400.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?offset=50"), &fx.full_token)
        .await;
    assert_eq!(status, 200, "a bookmarked `offset=` must not 400: {body}");

    srv.shutdown().await;
}

/// The events list must not leak across the environment boundary either.
///
/// `analytics_events` carries `environment_id`, so this is an ordinary
/// `scope_env!` filter rather than Issues' derived membership — which is
/// exactly why it is easy to drop while rewriting the query, and why it gets
/// its own test. Both halves are asserted: the rows returned AND the answer a
/// predicate gives, since a filter that scoped the page but not the count (or
/// not the predicate) would still be an oracle.
#[tokio::test]
async fn events_stay_within_the_callers_environment() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_env_oracle_fixture().await;
    let list = format!("/v1/apps/{}/events/list", fx.app_id);

    let mine = srv.get_json(&list, &fx.member_token).await;
    assert_eq!(
        ids(&mine).len(),
        1,
        "the member must see their environment's event and only it: {mine}"
    );
    assert_eq!(mine["total"], 1, "the count is scoped too: {mine}");
    assert_eq!(
        mine["data"][0]["tags"]["oracle_key"], IN_REACH_MARKER,
        "and it must be the RIGHT one: {mine}"
    );

    // The app-wide caller sees both — without this the fixture could have
    // stopped seeding the second event and the assertion above would pass
    // vacuously.
    let owner = srv.get_json(&list, &fx.owner_token).await;
    assert_eq!(ids(&owner).len(), 2, "{owner}");
    assert_eq!(owner["total"], 2, "{owner}");

    // A predicate must not reach across the boundary either: the out-of-reach
    // marker has to be indistinguishable from one that exists nowhere. Three
    // probe shapes, because they lower through three different code paths —
    // free text, a JSONB containment, and the tag store.
    for probe in ["q", "query=properties.oracle_key", "query=tag.oracle_key"] {
        let url = |marker: &str| match probe {
            "q" => format!("{list}?q={marker}"),
            other => format!("{list}?{other}:{marker}"),
        };
        let control = srv.get_json(&url(ABSENT_MARKER), &fx.member_token).await;
        let leak = srv
            .get_json(&url(OUT_OF_REACH_MARKER), &fx.member_token)
            .await;
        assert_eq!(
            control["total"], 0,
            "`{probe}` control must match nothing: {control}"
        );
        assert_eq!(
            leak["total"], control["total"],
            "`{probe}`: `{OUT_OF_REACH_MARKER}` exists only in an environment this member holds \
             no grant on: {leak}"
        );
        // …and the same probe DOES find the row inside their own environment,
        // so the indistinguishability above is not "this route answers nothing".
        let found = srv.get_json(&url(IN_REACH_MARKER), &fx.member_token).await;
        assert_eq!(
            found["total"], 1,
            "`{probe}` must still work in-reach: {found}"
        );
    }

    srv.shutdown().await;
}

/// The planner's `clamped` notice, on the field name only this route can
/// supply — and applied, not merely announced.
///
/// `Clamp.field` is the generic `"since"`: `prepare` does not know which
/// resource it ran for, so mapping it onto `occurred_at` is the handler's job,
/// and a handler that echoed `"since"` (or copied Issues' `"last_seen"`) would
/// still serve correct ROWS. Only an assertion on the envelope catches it.
///
/// The row-level half matters more: a `clamped` field that was reported but
/// never folded into the query's `since` would be a label with no effect.
/// `ResolvedNode::Text(_)` is `Cost::Scan`, so a free-text term is what trips
/// it; a plain indexed predicate over the same window must NOT be clamped, or
/// the assertion could pass because everything is clamped always.
#[tokio::test]
async fn a_scanning_event_query_is_clamped_to_occurred_at_and_the_clamp_bites() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-clamp").await;
    let list = format!("/v1/apps/{app_id}/events/list");
    {
        let mut conn = srv.conn().await;
        // Inside the 30-day default clamp…
        seed_analytics_event(
            &mut conn,
            app_id,
            None,
            "clamp_recent",
            json!({}),
            json!({}),
            json!({}),
            json!({}),
            None,
            Utc::now() - ChronoDuration::days(2),
        )
        .await;
        // …and well outside it, but inside the route's own 365-day window.
        seed_analytics_event(
            &mut conn,
            app_id,
            None,
            "clamp_ancient",
            json!({}),
            json!({}),
            json!({}),
            json!({}),
            None,
            Utc::now() - ChronoDuration::days(100),
        )
        .await;
    }

    // An indexed predicate: no clamp, and the 100-day-old row IS reachable —
    // `name` is `IndexClass::Indexed` and `Eq` keeps it there (`Like`/`Contains`
    // are `Cost::Scan` whatever the index class, so a `~` control here would
    // have been clamped too and the contrast below would prove nothing).
    let indexed = srv
        .get_json(&format!("{list}?query=name:clamp_ancient"), &token)
        .await;
    assert!(
        indexed["clamped"].is_null(),
        "an indexed predicate must not be clamped: {indexed}"
    );
    assert_eq!(
        indexed["total"], 1,
        "the 100-day-old row is inside the route's own 365-day window and must be \
         reachable when nothing clamps it: {indexed}"
    );

    // Free text is `Cost::Scan`, so the planner clamps it — and the notice must
    // name THIS resource's window column, not the generic "since".
    let scanned = srv.get_json(&format!("{list}?q=ev-user"), &token).await;
    assert_eq!(
        scanned["clamped"]["field"], "occurred_at",
        "`Clamp.field` is the generic \"since\"; mapping it onto this resource's \
         column is the handler's job: {scanned}"
    );
    assert_eq!(scanned["clamped"]["to"], "30d", "{scanned}");
    assert!(
        scanned["clamped"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "{scanned}"
    );
    // …and it BITES: the 100-day-old row is gone, the 2-day-old one remains.
    assert_eq!(
        scanned["total"], 1,
        "a reported clamp that was not folded into `since` is a label with no \
         effect: {scanned}"
    );
    assert_eq!(scanned["data"][0]["name"], "clamp_recent", "{scanned}");

    srv.shutdown().await;
}

/// `clamped` must describe the window the response actually contains.
///
/// Two ways it did not, both fixed by `search::resolve_window`:
///
/// 1. This route bounds its window at 365 days. `default_events_since_days()`
///    returned **3650**, so every unparameterised request was narrowed tenfold
///    while `clamped` stayed `null` — the envelope asserting no narrowing had
///    happened. An explicit `?since_days=3650` did the same thing out loud.
/// 2. The planner's clamp was reported whenever it EXISTED, not when it BOUND.
///    A caller whose own `since_days` was tighter received their own narrower
///    window under the planner's wider label.
///
/// The second is the one a caption renders: seven days of rows under
/// `clamped: {"to": "30d"}` reads as "last 30 days" above 7 days of data.
#[tokio::test]
async fn the_events_clamp_notice_describes_the_window_actually_served() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-window").await;
    let list = format!("/v1/apps/{app_id}/events/list");
    {
        let mut conn = srv.conn().await;
        for (name, age) in [("win_recent", 2_i64), ("win_old", 200)] {
            seed_analytics_event(
                &mut conn,
                app_id,
                None,
                name,
                json!({}),
                json!({}),
                json!({}),
                json!({}),
                None,
                Utc::now() - ChronoDuration::days(age),
            )
            .await;
        }
    }

    // The default request. 365 days is both the default and the ceiling, so
    // nothing narrows and nothing is disclosed — and the 200-day-old row proves
    // the window really is a year, not the planner's 30 days.
    let defaulted = srv.get_json(&list, &token).await;
    assert!(
        defaulted["clamped"].is_null(),
        "the default window is the ceiling, so nothing was narrowed: {defaulted}"
    );
    assert_eq!(defaulted["total"], 2, "{defaulted}");

    // Asking past the ceiling. Served 365 days either way; the difference is
    // that the caller is now told, and told the number they actually got.
    let over = srv
        .get_json(&format!("{list}?since_days=3650"), &token)
        .await;
    assert_eq!(
        over["clamped"]["field"], "occurred_at",
        "the notice names this resource's window column: {over}"
    );
    assert_eq!(
        over["clamped"]["to"], "365d",
        "the window SERVED, not the 3650 requested: {over}"
    );
    assert_eq!(over["total"], 2, "{over}");

    // A scanning query the caller has already narrowed past the planner's
    // clamp. The planner would clamp to 30 days; the caller asked for 7; 7 is
    // what runs, so there is nothing for the planner to disclose.
    let tighter = srv
        .get_json(&format!("{list}?q=ev-user&since_days=7"), &token)
        .await;
    assert!(
        tighter["clamped"].is_null(),
        "a planner clamp wider than the caller's own window narrowed nothing, \
         and reporting it labels 7 days of rows \"30d\": {tighter}"
    );
    assert_eq!(
        tighter["total"], 1,
        "the caller's own 7-day window is what ran: {tighter}"
    );
    assert_eq!(tighter["data"][0]["name"], "win_recent", "{tighter}");

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// S2c Slice 2 Task 3: the widened Events sort whitelist (`name`, `distinct_id`,
// `session_id` alongside `occurred_at`) and the cursor `key` that keeps a page
// from being replayed against a different ordering.
// ---------------------------------------------------------------------------

impl TestServer {
    /// One analytics event per name, each at a DISTINCT `occurred_at` that
    /// runs in the OPPOSITE order to `names` (the first name is newest, the
    /// last is oldest). That inversion is deliberate: `occurred_at` is this
    /// route's default ordering, so a `sort=-name` request that silently fell
    /// back to it would still return `names` in insertion order — nothing
    /// alphabetical about it. With the two orderings provably different, a
    /// test against this fixture cannot pass by coincidence.
    async fn seed_named_events(&self, app_id: Uuid, names: &[&str]) {
        let mut conn = self.conn().await;
        let now = Utc::now();
        for (i, name) in names.iter().enumerate() {
            seed_analytics_event(
                &mut conn,
                app_id,
                None,
                name,
                json!({}),
                json!({}),
                json!({}),
                json!({}),
                None,
                now - ChronoDuration::minutes(i as i64),
            )
            .await;
        }
    }
}

/// The `name` column of every row in an envelope's `data`, in order. See `ids`
/// above for the same extraction over `id`.
fn names(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("response has no `data` array: {v}"))
        .iter()
        .map(|r| r["name"].as_str().expect("row has a name").to_string())
        .collect()
}

/// `-name` is ASCENDING under this API's inverted convention — a bare column
/// means descending, `-` reverses it, see `parse_sort`'s doc comment — so this
/// reads alphabetically forward and must page across the 2/2 boundary without
/// repeating or skipping `charlie`.
#[tokio::test]
async fn events_sort_by_name_orders_and_pages() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-sort-name").await;
    srv.seed_named_events(app_id, &["delta", "alpha", "charlie", "bravo"])
        .await;
    let list = format!("/v1/apps/{app_id}/events/list");

    let first = srv
        .get_json(&format!("{list}?sort=-name&limit=2"), &token)
        .await;
    assert_eq!(names(&first), ["alpha", "bravo"], "{first}");

    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second page exists")
        .to_string();
    let second = srv
        .get_json(
            &format!("{list}?sort=-name&limit=2&cursor={cursor}"),
            &token,
        )
        .await;
    assert_eq!(names(&second), ["charlie", "delta"], "{second}");

    srv.shutdown().await;
}

/// A cursor is a position within ONE ordering (see `cursor.rs`'s module doc
/// comment). Before `decode` took the requested sort key as a parameter and
/// `events_list` started passing it through, this returned 200 and silently
/// paged from the wrong position in the wrong ordering — wrong rows behind a
/// success status, no error anywhere.
#[tokio::test]
async fn a_cursor_from_another_sort_is_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-sort-mismatch").await;
    srv.seed_named_events(app_id, &["a", "b", "c"]).await;
    let list = format!("/v1/apps/{app_id}/events/list");

    let first = srv
        .get_json(&format!("{list}?sort=-name&limit=1"), &token)
        .await;
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second page exists")
        .to_string();

    // Same cursor, minted under `name`; this request sorts by `occurred_at`.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?sort=occurred_at&cursor={cursor}"), &token)
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("start from the first page"),
        "error should tell the caller how to recover: {body}"
    );

    srv.shutdown().await;
}

/// The whitelist this task widened to four columns still refuses a fifth.
#[tokio::test]
async fn an_unlisted_sort_column_is_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-sort-unlisted").await;
    let (status, body) = srv
        .get_status_and_body(
            &format!("/v1/apps/{app_id}/events/list?sort=properties"),
            &token,
        )
        .await;
    assert_eq!(status, 400, "{body}");

    srv.shutdown().await;
}

/// A cursor's `key` and its `t`/`s` value type tag are independent fields on
/// the wire (see `cursor.rs`'s module doc comment): matching the key alone
/// used to be enough to pass `decode`, so a `session_id|<uuid>|t:…`
/// cursor — the wrong VALUE kind for a text column — sailed through. Read via
/// `text_of`'s total fallback in `repo.rs`, that silently produced `""`,
/// which does not error: ascending, `COALESCE(session_id,'') > ''` matches
/// (almost) every row, so page two would repeat page one forever, not skip
/// or crash. `decode` now takes the sort's `is_temporal` alongside its key,
/// so this must be a 400, never a page — repeating or otherwise.
///
/// `session_id` doubles as the HTTP-level coverage neither this file nor a
/// browser session previously exercised: every other Events sort test above
/// exercises `name` (or the `occurred_at` default) only.
#[tokio::test]
async fn an_event_cursor_with_a_forged_value_kind_is_refused() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("ev-sort-kind-forge").await;
    srv.seed_named_events(app_id, &["a", "b", "c"]).await;
    let list = format!("/v1/apps/{app_id}/events/list");

    // `EventSort::SessionId::is_temporal()` is `false` — `session_id` is a
    // text column — so a cursor under that key correctly carries
    // `CursorValue::Text`. This one carries `CursorValue::Ts` instead: right
    // key, wrong kind, exactly the shape `decode`'s key-only check used to
    // let through.
    let forged = sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
        key: "session_id".to_string(),
        value: sauron_db::query_plan::cursor::CursorValue::Ts(Utc::now()),
        id: Uuid::new_v4(),
    });

    let (status, body) = srv
        .get_status_and_body(&format!("{list}?sort=session_id&cursor={forged}"), &token)
        .await;
    assert_eq!(status, 400, "{body}");
    // Wording unique to `CursorError::KindMismatch`, not just any 400 that
    // happens to mention the column — see the Occurrences sibling test's
    // identical comment for why `body.contains("session_id")` alone does not
    // discriminate from `parse_sort`'s "cannot sort by `X`" rejection, which
    // also lists `session_id` as part of the whitelist it names.
    assert!(
        body.contains("requires a text value"),
        "error should be the cursor's KindMismatch specifically, not merely \
         a 400 that happens to mention the column: {body}"
    );

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// Occurrence columns on the issues list — `screen`, `distinctId`, `deviceKey`
// ---------------------------------------------------------------------------

/// One issue plus one occurrence carrying the three occurrence columns the
/// issues list can now filter on. `screen`/`distinct_id`/`device_key` are all
/// nullable, and passing `None` is how the "recorded nothing" issue is built —
/// which is the row the negation cases turn on.
#[allow(clippy::too_many_arguments)]
async fn seed_issue_with_occurrence_columns(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    title: &str,
    screen: Option<&str>,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
    issue_last_seen: DateTime<Utc>,
    occurred_at: DateTime<Utc>,
) -> Uuid {
    let fingerprint = format!("occcol-fp-{}", Uuid::new_v4().simple());
    let issue_id = repo::upsert_issue(
        conn,
        NewIssue {
            app_id,
            fingerprint: &fingerprint,
            type_: "Error",
            title,
            culprit: "occcol::seed",
            level: "error",
            first_seen: occurred_at,
            last_seen: issue_last_seen,
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
            environment_id: None,
            issue_id,
            fingerprint: fingerprint.clone(),
            level: "error".into(),
            message: "occurrence-column fixture".into(),
            exception_type: "Error".into(),
            exception_value: "occurrence-column fixture".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: distinct_id.map(str::to_string),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at,
            session_id: None,
            device_key: device_key.map(str::to_string),
            screen: screen.map(str::to_string),
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(false),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");
    issue_id
}

/// `screen`, `distinctId` and `deviceKey` narrow the issues list, in both the
/// `query=` and the `filter=` spelling.
///
/// None of the three is an `issues` column — each lowers to a correlated
/// `EXISTS` into `error_events` — so this is the test that the plumbing
/// actually selects rows rather than merely compiling. The third issue records
/// none of the three columns, which is what makes the negation and `has:`
/// cases mean something.
#[tokio::test]
async fn occurrence_columns_narrow_the_issues_list() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("occ-columns").await;
    let at = Utc::now() - ChronoDuration::hours(1);
    let (alpha, beta, bare) = {
        let mut conn = srv.conn().await;
        let alpha = seed_issue_with_occurrence_columns(
            &mut conn,
            app_id,
            "alpha issue",
            Some("/checkout"),
            Some("u_alpha"),
            Some("d_alpha"),
            at,
            at,
        )
        .await;
        let beta = seed_issue_with_occurrence_columns(
            &mut conn,
            app_id,
            "beta issue",
            Some("/cart"),
            Some("u_beta"),
            Some("d_beta"),
            at,
            at,
        )
        .await;
        let bare = seed_issue_with_occurrence_columns(
            &mut conn,
            app_id,
            "bare issue",
            None,
            None,
            None,
            at,
            at,
        )
        .await;
        (alpha, beta, bare)
    };
    let list = format!("/v1/apps/{app_id}/issues");
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let expect = |ids: &[Uuid]| sorted(ids.iter().map(|i| i.to_string()).collect());

    // Sanity: without a filter all three are present. An equivalence between
    // two empty lists proves nothing.
    let all = srv.get_json(&list, &token).await;
    assert_eq!(all["total"], 3, "fixture did not land: {all}");

    for (params, want, why) in [
        (
            "query=screen:/checkout",
            vec![alpha],
            "an exact screen match",
        ),
        (
            "filter=screen:eq:/checkout",
            vec![alpha],
            "the legacy spelling of the same predicate",
        ),
        (
            "query=distinctId:u_beta",
            vec![beta],
            "the user who hit the issue",
        ),
        (
            "filter=distinctId:eq:u_beta",
            vec![beta],
            "the legacy spelling of the same predicate",
        ),
        (
            "query=deviceKey:d_alpha",
            vec![alpha],
            "the device the issue was seen on",
        ),
        (
            "query=screen:[/checkout,/cart]",
            vec![alpha, beta],
            "`In` over a bracketed list",
        ),
        (
            "query=screen:~check",
            vec![alpha],
            "a literal substring, which `/cart` must not satisfy",
        ),
        (
            "query=has:screen",
            vec![alpha, beta],
            "presence excludes only the issue that recorded no screen",
        ),
        // The one that would be wrong under `EXISTS(… <> …)`: `bare` recorded
        // no screen at all, and "not seen on /checkout" is true of it.
        (
            "query=!screen:/checkout",
            vec![beta, bare],
            "negation keeps the issue whose occurrences recorded no screen",
        ),
        (
            "query=!has:screen",
            vec![bare],
            "the complement of the `has:` case",
        ),
        (
            "query=screen:/checkout distinctId:u_alpha",
            vec![alpha],
            "two occurrence-column predicates compose",
        ),
        (
            "query=screen:/checkout distinctId:u_beta",
            vec![],
            "…and compose as AND, not as OR",
        ),
    ] {
        let got = srv.get_json(&format!("{list}?{params}"), &token).await;
        assert_eq!(
            sorted(ids(&got)),
            expect(&want),
            "`{params}` — {why}: {got}"
        );
        assert_eq!(
            got["total"],
            want.len(),
            "`{params}` — `total` must agree with `data`: {got}"
        );
    }

    srv.shutdown().await;
}

/// The predicate is bounded by the window the caller asked for, because the
/// subquery binds `e.occurred_at >= since`.
///
/// The fixture separates the two clocks that a single `since` is applied to:
/// the issue's `last_seen` is an hour old, so it is inside every window tested
/// here and the OUTER filter never excludes it — but its `/checkout`
/// occurrence is ten days old. Narrowing the range must therefore drop it from
/// the filtered list while leaving it in the unfiltered one, which is the only
/// arrangement that can tell the subquery's bound apart from the outer one.
#[tokio::test]
async fn an_occurrence_column_filter_respects_the_requested_window() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("occ-window").await;
    let now = Utc::now();
    let issue_id = {
        let mut conn = srv.conn().await;
        seed_issue_with_occurrence_columns(
            &mut conn,
            app_id,
            "stale-screen issue",
            Some("/checkout"),
            None,
            None,
            now - ChronoDuration::hours(1),
            now - ChronoDuration::days(10),
        )
        .await
    };
    let list = format!("/v1/apps/{app_id}/issues");

    // Wide enough to contain the occurrence: the filter matches.
    let wide = srv
        .get_json(
            &format!("{list}?since_days=30&query=screen:/checkout"),
            &token,
        )
        .await;
    assert_eq!(ids(&wide), vec![issue_id.to_string()], "{wide}");

    // Narrower than the occurrence, wider than `last_seen`: the issue is still
    // listed, but no longer matches the screen.
    let narrow_unfiltered = srv.get_json(&format!("{list}?since_days=3"), &token).await;
    assert_eq!(
        ids(&narrow_unfiltered),
        vec![issue_id.to_string()],
        "the issue must still be in range unfiltered, or this test proves nothing: \
         {narrow_unfiltered}"
    );
    let narrow = srv
        .get_json(
            &format!("{list}?since_days=3&query=screen:/checkout"),
            &token,
        )
        .await;
    assert_eq!(
        ids(&narrow),
        Vec::<String>::new(),
        "the /checkout occurrence is older than the requested window: {narrow}"
    );
    assert_eq!(narrow["total"], 0, "{narrow}");

    srv.shutdown().await;
}

/// `deviceKey` is `OPS_EQ` in the catalog, so a substring probe is a 400 —
/// and it must be refused by NAME, not answered as something else.
///
/// Pinned end to end because the dashboard chip for this field is `OPS_ENUM`
/// on the strength of it: if the catalog were widened, the chip is what would
/// look wrong, and this is the assertion that would notice first.
#[tokio::test]
async fn device_key_refuses_a_substring_probe_on_the_issues_list() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let (app_id, token) = srv.seed_app("occ-ops").await;
    let list = format!("/v1/apps/{app_id}/issues");

    for probe in ["query=deviceKey:~d_", "filter=deviceKey:contains:d_"] {
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?{probe}"), &token)
            .await;
        assert_eq!(status, 400, "`{probe}` must be refused: {body}");
        assert!(
            body.contains("deviceKey") || body.contains("device_key"),
            "`{probe}`: the refusal must name the field: {body}"
        );
    }

    // The neighbouring dimension DOES accept it, so the refusal above is about
    // this field's operator set and not about substrings being unsupported.
    let (status, body) = srv
        .get_status_and_body(&format!("{list}?query=screen:~check"), &token)
        .await;
    assert_eq!(status, 200, "`screen:~check` must be accepted: {body}");

    srv.shutdown().await;
}

/// The three new filters need no `event:read`, and that is a decision.
///
/// `reject_withheld_body` gates `Store::Tag`, non-allowlisted
/// `Store::JsonRoot` columns, and `workflow` by name. These three are plain
/// `Store::Column`s and pass it — which matches what the caller can already
/// read: `strip_event_body` withholds the crash payload but explicitly KEEPS
/// `screen`, `distinct_id` and `device_key` as issue-level shell. Filtering by
/// them therefore discloses nothing a caller holding `issue:read` alone cannot
/// already read off the occurrences list.
///
/// The `workflow` leg is the control: same page, same caller, a field that IS
/// gated. Without it this test would pass just as well against a build that
/// had stopped gating anything.
#[tokio::test]
async fn occurrence_column_filters_need_no_event_read() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_search");
        return;
    };
    let fx = srv.seed_occurrence_fixture().await;
    let list = format!("/v1/apps/{}/issues", fx.app_id);

    for probe in [
        "query=screen:/checkout",
        "query=distinctId:u_alpha",
        "query=deviceKey:d_alpha",
        "query=has:screen",
        "query=!screen:/checkout",
        "filter=screen:eq:/checkout",
    ] {
        let (status, body) = srv
            .get_status_and_body(&format!("{list}?{probe}"), &fx.shell_token)
            .await;
        assert_eq!(
            status, 200,
            "`{probe}` is a predicate over a column this caller already reads on the \
             occurrences list, so it must be answered: {body}"
        );
    }

    let (status, body) = srv
        .get_status_and_body(&format!("{list}?query=workflow:checkout"), &fx.shell_token)
        .await;
    assert_eq!(
        status, 403,
        "the control: `workflow` IS gated, so a build that gates nothing fails here: {body}"
    );

    srv.shutdown().await;
}
