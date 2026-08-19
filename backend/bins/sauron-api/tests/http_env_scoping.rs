//! HTTP-level tests for the `environment_id` scoping boundary, driven through
//! the real router.
//!
//! ## The `?environment_id=` empty-value regression (S2 Task 10)
//!
//! A `parse_env` unit test cannot see this bug: `routes/scope.rs`'s
//! `malformed_is_rejected_not_widened` test was green the entire time the bug
//! shipped, because the defect was never in `parse_env` — it was in *which*
//! `Query` extractor a handler happened to import upstream of it
//! (`axum_extra::extract::Query`'s codec silently turns `?environment_id=`
//! into "absent" for an `Option<String>` field; `axum::extract::Query`'s does
//! not). Only a test that goes through the real axum router, over real HTTP,
//! can see that.
//!
//! ## The env-scoped RBAC boundary (Slice 3 Task 6)
//!
//! `authorize_env_read`'s decision table has its own unit tests
//! (`sauron-auth::rbac`'s `resolve_env_filter` suite) and `sauron-db` has its
//! own environment-filtering tests (`tests/env_scoping.rs`), but neither can
//! see whether a route handler actually *calls* the decision function, or
//! calls it with the right permission, or wires its result into the query —
//! the S2 review's F7 finding was that zero rejecting routes were ever
//! exercised over HTTP, and the original Critical in this feature lived
//! entirely in which extractor a handler imported. This file's
//! `env_scoped_member_is_confined_over_http` and `app_wide_member_is_unaffected`
//! are that check for the RBAC boundary, the same way the empty-value tests
//! above are it for the wire-parsing bug.
//!
//! Every test here spawns the actual compiled `sauron-api` binary (via
//! Cargo's `CARGO_BIN_EXE_sauron-api`, so it is testing the literal shipped
//! artifact and its literal route table in `main.rs`, not a hand-assembled
//! subset) and drives it with `reqwest`. See [`TestServer`] for the shared
//! provision/spawn/teardown machinery every test below uses.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset, mirroring `sauron-db`'s own integration-test convention (see
//! `crates/sauron-db/tests/common/mod.rs`) — this repo's tests run against a
//! live stack by choice, opted into by exporting the variable, rather than
//! against a mock. `TEST_DATABASE_URL` is a *maintenance* Postgres URL from
//! which an ephemeral, randomly-named, migrated database is created for each
//! test alone and dropped again at the end. `TEST_REDIS_URL` is a Redis the
//! spawned `sauron-api` process can reach — `sauron-api` requires one to
//! start at all (auth-adjacent bookkeeping these tests never themselves
//! touch).

use std::cell::Cell;
use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewInspectorPolicy, NewIssue, NewRoleGrant,
};
use sauron_db::repo;

/// Define an environment on `project_id` and enroll `app_id` in it.
///
/// Returns the **enrollment** id — what event rows store in `environment_id`
/// and what an `env` role grant's `scope_id` names. An env grant addresses one
/// app's enrollment, never the catalogue entry, which is precisely what keeps
/// granting "prod" from spanning sibling apps.
async fn seed_env(
    conn: &mut diesel_async::AsyncPgConnection,
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

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-env-scoping-test-secret-0000000000000000";

/// Return `url` with its database (path) segment replaced by `new_db`,
/// preserving scheme and authority. Byte-for-byte the same tiny helper as
/// `crates/sauron-db/tests/common/mod.rs`'s `swap_database` — duplicated
/// rather than shared, for the same reason that file gives: pulling in the
/// `url` crate for one string rewrite isn't worth a cross-crate dependency
/// for a test-only helper this small.
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

/// Bind port 0 to get a free OS-assigned TCP port, then release it. Racy in
/// theory (another process could grab it before `sauron-api` binds); fine in
/// practice for a single, serially-run test.
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
/// process pointed at it, and an HTTP client for driving it — everything each
/// test below needs, provisioned once via [`TestServer::start`] and torn down
/// once via [`shutdown`](TestServer::shutdown).
///
/// Mirrors `sauron-db`'s own `tests/common::TestDb` (duplicated rather than
/// shared, same reasoning as [`swap_database`] above): `sauron-db`'s harness
/// lives in that crate's own test target and cannot be depended on from here,
/// and it has no notion of spawning a server anyway.
struct TestServer {
    child: tokio::process::Child,
    base: String,
    client: reqwest::Client,
    admin_url: String,
    db_name: String,
    pool: sauron_db::PgPool,
    /// Set by [`shutdown`](TestServer::shutdown), so `Drop` can tell whether
    /// it ever ran — mirrors `TestDb`'s own `cleaned_up` flag and the same
    /// reasoning: `Drop` cannot await `drop_database`, so a test that panics
    /// before reaching `shutdown()` leaks its ephemeral database; better to
    /// say so loudly than to leave it silently discoverable only via `psql`.
    cleaned_up: Cell<bool>,
}

impl TestServer {
    /// Provision an ephemeral, migrated database, spawn the real compiled
    /// `sauron-api` binary against it, and wait for `/health`.
    ///
    /// `None` when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset — callers
    /// skip (see the module docs above).
    async fn start() -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

        // The segment ORDER is load-bearing: timestamp FIRST, discriminator
        // glued to the uuid rather than separated by an underscore.
        //
        // `sauron-db`'s `tests/common::reap_stale_test_databases` is the only
        // process that ever collects abandoned `sauron_test_%` databases, and
        // it does `strip_prefix("sauron_test_")` -> `split('_').next()` ->
        // `parse::<i64>()`, silently SKIPPING (`else { continue }`) any name
        // whose first underscore-delimited segment is not a timestamp. The
        // previous "sauron_test_http_<ts>_<uuid>" spelling yielded "http",
        // failed that parse, and leaked every database it ever created —
        // permanently, and invisibly to the reaper. 26 of them had accumulated
        // on the shared dev server, the oldest several days old, each a fully
        // migrated copy of the schema. Do not reorder these segments.
        //
        // Length is also capped: `sauron_db::validate_db_ident` rejects
        // identifiers over 63 bytes. "sauron_test_" (12) + 10-digit timestamp
        // + "_" + "http" (4) + 32-hex uuid = 59.
        let db_name = format!(
            "sauron_test_{}_http{}",
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
        // Two slots, not one: `shutdown()` needs its own connection budget
        // independent of anything a test still holds via `conn()` at the
        // point it calls `shutdown()`.
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");

        let port = free_port();
        let bin = env!("CARGO_BIN_EXE_sauron-api");
        let mut child = tokio::process::Command::new(bin)
            .env("DATABASE_URL", &db_url)
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
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sauron-api binary");

        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();

        // Poll /health until the server is up (or the process died trying).
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

    /// Check out a connection to this server's own ephemeral database, for
    /// seeding fixtures directly via `repo::*` — the same connection pool the
    /// spawned `sauron-api` process is *not* using (it has its own, built from
    /// the same `DATABASE_URL`).
    async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    /// GET `path` against the running server with the given bearer token.
    async fn get(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"))
    }

    /// [`get`](TestServer::get), returning just the status code.
    async fn get_status(&self, path: &str, token: &str) -> u16 {
        self.get(path, token).await.status().as_u16()
    }

    /// [`get_status`](TestServer::get_status) plus the raw body, for
    /// assertions whose failure message is only useful with the server's own
    /// error text in it — a 500 from a malformed ORDER BY says which column
    /// Postgres rejected, and the status alone does not.
    async fn get_status_and_body(&self, path: &str, token: &str) -> (u16, String) {
        let resp = self.get(path, token).await;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: failed to read body (status {status}): {e}"));
        (status, text)
    }

    /// [`get`](TestServer::get), parsed as JSON — for tests that need to
    /// count rows, not just check the status. Parses via `.text()` +
    /// `serde_json::from_str` rather than `reqwest::Response::json` — this
    /// workspace's `reqwest` dependency deliberately omits the `json` feature
    /// (see the root `Cargo.toml`), so the method isn't available.
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

    /// GET an Overview *section* endpoint and return its PAYLOAD.
    ///
    /// These five endpoints no longer compute on the request path. They answer
    /// from a server-side cache with `{state, computed_at, data}` and enqueue a
    /// background recompute when what they have is missing or stale, so the
    /// FIRST read of any selection returns `state: "computing"` with a null
    /// `data` — by design, since the alternative was holding the request open
    /// past the 30s timeout layer and being shed as a 503.
    ///
    /// A test therefore has to wait for the recompute rather than read the
    /// first response. This polls until the section reports a payload, and
    /// unwraps the envelope so every assertion below is unchanged from when
    /// these endpoints answered synchronously.
    ///
    /// Panics with the recompute's own error rather than timing out silently:
    /// a failed aggregate is the thing a test most needs to see, and a bare
    /// "timed out after 10s" hides it behind the symptom.
    async fn get_section(&self, path: &str, token: &str) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut last = serde_json::Value::Null;
        while std::time::Instant::now() < deadline {
            let v = self.get_json(path, token).await;
            if let Some(err) = v["error"].as_str() {
                panic!("GET {path}: section recompute failed: {err}");
            }
            // `fresh` and `stale` both carry a payload; only `computing` does
            // not. Accepting either avoids a flake if the freshness window ever
            // shortens.
            if !v["data"].is_null() {
                return v["data"].clone();
            }
            last = v;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("GET {path}: section never left `computing` within 20s; last: {last}");
    }

    /// POST `path` with a JSON body against the running server with the
    /// given bearer token. Serializes via `serde_json::Value::to_string` and
    /// sets the content-type header by hand, same reason as [`get_json`]:
    /// this workspace's `reqwest` dependency has no `json` feature.
    async fn post(&self, path: &str, token: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"))
    }

    /// [`post`](TestServer::post), returning just the status code.
    async fn post_status(&self, path: &str, token: &str, body: serde_json::Value) -> u16 {
        self.post(path, token, body).await.status().as_u16()
    }

    /// PATCH `path` with a JSON body against the running server with the
    /// given bearer token. Mirrors [`post`](TestServer::post).
    async fn patch(&self, path: &str, token: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .patch(format!("{}{path}", self.base))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"))
    }

    /// [`patch`](TestServer::patch), returning just the status code.
    async fn patch_status(&self, path: &str, token: &str, body: serde_json::Value) -> u16 {
        self.patch(path, token, body).await.status().as_u16()
    }

    /// DELETE `path` against the running server with the given bearer token.
    async fn delete(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"))
    }

    /// [`delete`](TestServer::delete), returning just the status code.
    async fn delete_status(&self, path: &str, token: &str) -> u16 {
        self.delete(path, token).await.status().as_u16()
    }

    /// GET `path` and assert its status code. `label` names the case in the
    /// panic message (`path` alone doesn't say whether this was the "absent" /
    /// "empty" / "malformed" / "valid" leg of a group).
    async fn assert_status(&self, path: &str, token: &str, expected: u16, label: &str) {
        let status = self.get_status(path, token).await;
        assert_eq!(
            status, expected,
            "GET {path} ({label}): expected {expected}, got {status}"
        );
    }

    /// Kill the spawned server and drop its ephemeral database. Must be
    /// awaited explicitly at the end of each test — `Drop` cannot await, so it
    /// cannot run this itself (see `cleaned_up`'s doc comment).
    ///
    /// Takes `&mut self`, not `self` by value: `TestServer` implements `Drop`,
    /// and a type that does cannot have its fields moved out of it, even at
    /// the end of an owning function — see E0509. Leaving `self` in place and
    /// flipping `cleaned_up` is what lets the final (silent, because
    /// `cleaned_up` is now `true`) `Drop` at the end of the test's scope do no
    /// extra work.
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
        // Async work cannot run in `Drop`. If a test panicked (or otherwise
        // returned) before reaching `shutdown()`, make the leak loud rather
        // than attempt a runtime-in-Drop workaround. The child process itself
        // is still reaped: `kill_on_drop(true)` was set at spawn time.
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

/// Fixture for the environment-RBAC-boundary tests (Task 6): one org/project/
/// app with **two** environments, error and analytics rows in both, an owner
/// whose grant reaches the whole app, and two members each holding exactly one
/// `role_grants` row — `scope_type = 'env'` — on `granted_env` alone.
/// `other_env` is a sibling neither member holds any grant on. Also seeds one
/// `devices` row (via `bump_device`) plus one device-attributed analytics
/// event, both scoped to `granted_env` only — what the grouped-devices HTTP
/// tests (Task 3 of the devices-grouped-by-model-and-os plan) need to tell a
/// granted-env body apart from an other-env one.
///
/// The two members differ *only* in whether their role carries
/// `perm::SOURCE_READ`, so the source-code gate (fix round 1) has a genuine
/// positive and negative case at the same env scope rather than at two
/// different scopes.
struct EnvScopedFixture {
    org_id: Uuid,
    /// The project the app hangs off — needed by the project-scoped route
    /// enumeration below, which cannot derive it from `app_id`.
    project_id: Uuid,
    app_id: Uuid,
    granted_env: Uuid,
    other_env: Uuid,
    /// The one `devices` row seeded in `granted_env`. `family`/`model`/
    /// `os_name` are all non-NULL and pairwise distinct; `os_version` is
    /// deliberately left NULL. See the grouped-devices HTTP tests (Task 3 of
    /// the devices-grouped-by-model-and-os plan): the NULL `os_version` is
    /// what makes their absent-vs-`os_version=`-empty assertion load-bearing
    /// — with a non-NULL value, omitting the param (`IS NULL`) and sending it
    /// empty (`= ''`) both fail to match this device for unrelated reasons,
    /// which would make that assertion pass even if the two wire shapes were
    /// silently collapsed together upstream (the exact bug class
    /// `routes::scope`'s module docs are about, for a different parameter).
    device_key: String,
    /// The issue whose one error event lives in `granted_env`. Carries a
    /// pre-symbolicated frame with source context — see
    /// [`seed_issue_with_error`].
    granted_issue_id: Uuid,
    /// The issue whose one error event lives in `other_env`.
    other_issue_id: Uuid,
    owner_token: String,
    /// Env-scoped, **without** `source:read`.
    member_token: String,
    /// `member`'s email — grant-creation tests target a user by email, not id.
    member_email: String,
    /// `member`'s role — reused as the role a new grant is created with, so
    /// the grant-creation tests do not need a role of their own.
    member_role_id: Uuid,
    /// Env-scoped on the same environment, **with** `source:read`.
    source_member_token: String,
    /// Env-scoped on `granted_env`, like `member_token`, but whose role also
    /// carries `project:read`/`app:read` **and, deliberately, `app:update`/
    /// `app:delete`** — the persona Task 16 exists for: a member confined to
    /// one environment who must be able to navigate to it (see their project
    /// in the switcher, their app in it, `GET` the app) but must still be
    /// refused any mutation of the app, purely because the grant is env-
    /// scoped, not because the role lacks the permission. Kept separate from
    /// `member_token` rather than adding these permissions to it, so the
    /// issue/event-scoping tests above stay exercising exactly the
    /// permission set they always have.
    nav_member_token: String,
    /// Holds the true `Owner` preset role at **org** scope — distinct from
    /// `owner_token` above, whose app-scoped `EVENT_READ`/`ISSUE_READ` grant
    /// is not enough to call `POST /v1/orgs/{org}/grants` at all (it needs
    /// `member:manage`, and the escalation check then needs the caller to
    /// outrank whatever role the request grants). Named for what it is
    /// authorized to do, not reused from `owner_token`, so a grant-creation
    /// test failure can't be confused with the read-path fixture.
    org_owner_token: String,
}

impl TestServer {
    /// Build [`EnvScopedFixture`]. See its doc comment for the exact shape.
    async fn seed_env_scoped_fixture(&self) -> EnvScopedFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(
            &mut conn,
            "env-scoping org",
            &format!("env-scoping-org-{suffix}"),
        )
        .await
        .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "env-scoping project",
            &format!("env-scoping-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "env-scoping app",
            &format!("env-scoping-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");
        let granted_env = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_env_scoping_granted_{suffix}"),
            true,
        )
        .await;
        let other_env = seed_env(
            &mut conn,
            project.id,
            app.id,
            "staging",
            &format!("pk_env_scoping_other_{suffix}"),
            false,
        )
        .await;

        // -- owner: app-wide reach --------------------------------------------
        let owner = repo::create_user(
            &mut conn,
            &format!("env-scoping-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Env Scoping Owner",
        )
        .await
        .expect("create owner user");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "env-scoping owner role",
            "app-wide read, for the back-compat leg of the RBAC boundary test",
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
        .expect("grant owner role at app scope");

        // -- member: exactly one env-scoped grant, on granted_env only -------
        let member_email = format!("env-scoping-member-{suffix}@example.test");
        let member = repo::create_user(
            &mut conn,
            &member_email,
            "unused-password-hash",
            "Env Scoping Member",
        )
        .await
        .expect("create member user");
        let member_role = repo::create_role(
            &mut conn,
            org.id,
            "env-scoping member role",
            "single-environment read",
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
                scope_id: granted_env,
            },
        )
        .await
        .expect("grant member role at env scope");

        // -- nav_member: same env grant, but WITH project:read + app:read, AND
        // (deliberately) app:update + app:delete too — Task 16's persona: an
        // env-only member who must be able to navigate to their environment
        // (project switcher, app switcher, `GET` the app) but not mutate the
        // app.
        //
        // `app:update`/`app:delete` are included in this role ON PURPOSE, not
        // omitted: the read/write boundary this fixture exists to prove is
        // about SCOPE (env vs app), not about which permissions the role
        // happens to carry. If the role held only read permissions, PATCH/
        // DELETE would 403 for the trivial reason that the permission is
        // absent everywhere — which would pass even if `update_app`/
        // `delete_app` were mistakenly switched to the reach-aware
        // `authorize_app_reachable` (a real env-scoped grant carrying
        // `app:update` would then wrongly succeed; see the task report's
        // deliberate-break proof). Granting the mutation permissions here and
        // still asserting 403 is what actually pins that `authorize_app`'s
        // strict `env: None` resolution — not the permission set — is what
        // refuses them.
        let nav_member = repo::create_user(
            &mut conn,
            &format!("env-scoping-nav-member-{suffix}@example.test"),
            "unused-password-hash",
            "Env Scoping Nav Member",
        )
        .await
        .expect("create nav_member user");
        let nav_member_role = repo::create_role(
            &mut conn,
            org.id,
            "env-scoping nav member role",
            "single-environment read, plus project/app read for navigation, plus \
             app:update/app:delete to prove the boundary is about scope not permission",
            json!([
                perm::EVENT_READ,
                perm::ISSUE_READ,
                perm::ENV_READ,
                perm::PROJECT_READ,
                perm::APP_READ,
                perm::APP_UPDATE,
                perm::APP_DELETE,
            ]),
        )
        .await
        .expect("create nav_member role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: nav_member.id,
                role_id: nav_member_role.id,
                scope_type: "env".to_string(),
                scope_id: granted_env,
            },
        )
        .await
        .expect("grant nav_member role at env scope");

        // -- source_member: same env grant, but WITH source:read --------------
        // Identical to `member` in every other respect, so a difference in
        // what the two see can only be the `source:read` gate.
        let source_member = repo::create_user(
            &mut conn,
            &format!("env-scoping-source-member-{suffix}@example.test"),
            "unused-password-hash",
            "Env Scoping Source Member",
        )
        .await
        .expect("create source_member user");
        let source_member_role = repo::create_role(
            &mut conn,
            org.id,
            "env-scoping source member role",
            "single-environment read, including de-obfuscated source",
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::SOURCE_READ]),
        )
        .await
        .expect("create source_member role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: source_member.id,
                role_id: source_member_role.id,
                scope_type: "env".to_string(),
                scope_id: granted_env,
            },
        )
        .await
        .expect("grant source_member role at env scope");

        // -- org_owner: the true Owner preset role, at ORG scope --------------
        // `owner` above cannot call POST /v1/orgs/{org}/grants at all (no
        // member:manage); the Owner preset guarantees both member:manage and
        // a strict superset of any role a grant-creation test hands out, so
        // the escalation check in create_grant never blocks these tests for
        // reasons unrelated to the env scope_type itself.
        let org_owner = repo::create_user(
            &mut conn,
            &format!("env-scoping-org-owner-{suffix}@example.test"),
            "unused-password-hash",
            "Env Scoping Org Owner",
        )
        .await
        .expect("create org_owner user");
        let owner_preset = repo::get_system_role(&mut conn, "Owner")
            .await
            .expect("load Owner preset role")
            .expect("Owner preset role must exist");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: org_owner.id,
                role_id: owner_preset.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant org_owner the Owner preset role at org scope");

        // -- data in both environments ----------------------------------------
        // An issue (backed by an error event) in each environment, so the
        // owner's "All" read sees strictly more than the member's
        // auto-narrowed "Subset([granted_env])" read — the auto-narrowing
        // proof needs both bounds (m < o, and m > 0) to be real.
        let granted_issue_id = seed_issue_with_error(
            &mut conn,
            app.id,
            Some(granted_env),
            &format!("env-scoping-fp-granted-{suffix}"),
        )
        .await;
        let other_issue_id = seed_issue_with_error(
            &mut conn,
            app.id,
            Some(other_env),
            &format!("env-scoping-fp-other-{suffix}"),
        )
        .await;
        // Analytics rows too, alongside the error rows above — this fixture's
        // shape (error + analytics activity in both environments) is meant to
        // serve any future scoped-endpoint test built on it, not only the
        // issues-endpoint assertions this task's tests make.
        seed_analytics_event(&mut conn, app.id, Some(granted_env), "env-scoping.fixture").await;
        seed_analytics_event(&mut conn, app.id, Some(other_env), "env-scoping.fixture").await;

        // -- one device, attributed to granted_env only ------------------------
        // `seed_analytics_event` above hard-codes `device_key: None`, so it
        // satisfies device membership for NO device (Task 1 already learned
        // this the hard way). The grouped-devices HTTP tests need one real
        // device to compare a granted-env body against an other-env one, so
        // seed it directly here: one `devices` row via `bump_device`, plus one
        // `NewAnalyticsEvent` with `device_key` set, scoped to `granted_env`
        // alone.
        //
        // `os_version` is left NULL on purpose — see `EnvScopedFixture::device_key`'s
        // doc comment for why a NULL column, not a non-NULL one, is what the
        // absent-vs-empty drill-down assertion needs.
        let device_key = format!("env-scoping-device-{suffix}");
        let now = Utc::now();
        repo::bump_device(
            &mut conn,
            app.id,
            &device_key,
            Some("env-scoping-family"),
            Some("env-scoping-model"),
            Some("EnvScopingOS"),
            None,
            None,
            None,
            None,
            now,
            1,
            0,
        )
        .await
        .expect("bump device");
        repo::insert_analytics_event(
            &mut conn,
            NewAnalyticsEvent {
                id: Uuid::new_v4(),
                app_id: app.id,
                environment_id: Some(granted_env),
                name: "env-scoping.fixture.device".to_string(),
                distinct_id: format!("env-scoping-fixture-device-{suffix}"),
                properties: json!({}),
                context: json!({}),
                session_id: None,
                release: None,
                ip_address: None,
                occurred_at: now,
                device_key: Some(device_key.clone()),
                screen: None,
                workflow_id: None,
                workflow_name: None,
                tags: json!({}),
                contexts: json!({}),
                extra: json!({}),
            },
        )
        .await
        .expect("insert device-attributed analytics event");

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys
            .issue_access(owner.id, false, None)
            .expect("issue owner access token");
        let (member_token, _) = keys
            .issue_access(member.id, false, None)
            .expect("issue member access token");
        let (source_member_token, _) = keys
            .issue_access(source_member.id, false, None)
            .expect("issue source_member access token");
        let (nav_member_token, _) = keys
            .issue_access(nav_member.id, false, None)
            .expect("issue nav_member access token");
        let (org_owner_token, _) = keys
            .issue_access(org_owner.id, false, None)
            .expect("issue org_owner access token");

        EnvScopedFixture {
            org_id: org.id,
            project_id: project.id,
            app_id: app.id,
            granted_env,
            other_env,
            device_key,
            granted_issue_id,
            other_issue_id,
            owner_token,
            member_token,
            member_email,
            member_role_id: member_role.id,
            source_member_token,
            nav_member_token,
            org_owner_token,
        }
    }

    /// A second, unrelated org/project/app/environment — for proving the
    /// cross-tenant boundary on grant creation: `role_grants.scope_id`
    /// carries no foreign key, so `validate_scopes_in_org`'s
    /// `owner_org == org_id` filter is the only thing that can refuse an
    /// environment id belonging to someone else's org.
    async fn seed_second_org(&self) -> OtherOrgFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(
            &mut conn,
            "env-scoping second org",
            &format!("env-scoping-second-org-{suffix}"),
        )
        .await
        .expect("create second org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "env-scoping second project",
            &format!("env-scoping-second-project-{suffix}"),
        )
        .await
        .expect("create second project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "env-scoping second app",
            &format!("env-scoping-second-app-{suffix}"),
            "web",
        )
        .await
        .expect("create second app");
        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_env_scoping_second_org_{suffix}"),
            true,
        )
        .await;

        OtherOrgFixture {
            org_id: org.id,
            env_id,
        }
    }
}

/// See [`TestServer::seed_second_org`].
struct OtherOrgFixture {
    /// Used by Task 16's `list_projects`/`list_apps` cross-tenant test: a
    /// caller with grants only in the FIRST org must not see anything when
    /// listing THIS org's projects.
    org_id: Uuid,
    env_id: Uuid,
}

/// The source-context keys `symbolicate::strip_source_context` removes when a
/// caller lacks `source:read`. Named here so the assertion and the seed cannot
/// drift apart.
const SOURCE_CONTEXT_KEYS: [&str; 4] = [
    "context_line",
    "pre_context",
    "post_context",
    "context_start_line",
];

/// A distinctive source line the gate tests grep the response body for.
const FIXTURE_CONTEXT_LINE: &str = "let secret_source_line = 42;";

/// Insert one issue and one error event attributing it to `env` (`None` for
/// the unattributed bucket). `list_issues`'s `One`/`Subset` path aggregates
/// via an inner-join over `error_events`, not a column on `issues` itself —
/// see `sauron_db::repo::list_issues`'s doc comment — so an issue with no
/// error event in an environment is invisible to a scoped read of it.
///
/// The event is stored **already symbolicated**, with one frame carrying all
/// four [`SOURCE_CONTEXT_KEYS`]. That matters for the `source:read` gate
/// tests: `symbolicate::symbolicate_with`'s fast path returns an event
/// untouched when `symbolication_status == "symbolicated"` and
/// `stacktrace_symbolicated` is a non-empty array, so these frames reach the
/// response verbatim and the ONLY thing that can remove the context keys is
/// `strip_source_context` — i.e. the gate itself. Seeded with an empty
/// `stacktrace` and no artifacts, symbolication would be a no-op and both the
/// with- and without-`source:read` responses would be identical, so the gate
/// test could not discriminate.
async fn seed_issue_with_error(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    fingerprint: &str,
) -> Uuid {
    let now = Utc::now();
    let issue_id = repo::upsert_issue(
        conn,
        NewIssue {
            app_id,
            fingerprint,
            type_: "Error",
            title: "env scoping fixture issue",
            culprit: "env_scoping::fixture",
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
            message: "env scoping fixture error".into(),
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
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: Some(json!([{
                "function": "envScopingFixture",
                "filename": "src/fixture.rs",
                "lineno": 42,
                "colno": 5,
                "context_line": FIXTURE_CONTEXT_LINE,
                "pre_context": ["fn env_scoping_fixture() {"],
                "post_context": ["}"],
                "context_start_line": 41,
            }])),
            symbolication_status: "symbolicated".into(),
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

/// Insert one analytics event attributed to `env`. See
/// [`seed_issue_with_error`]'s doc comment for why the fixture seeds both
/// signal kinds.
async fn seed_analytics_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    name: &str,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: name.to_string(),
            distinct_id: format!("env-scoping-fixture-{}", Uuid::new_v4().simple()),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
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

// ---------------------------------------------------------------------------
// S2 Task 10: the ?environment_id= empty-value regression.
// ---------------------------------------------------------------------------

/// The overview was split into four independently-addressable sections so the
/// dashboard can paint each card as its own answer lands, instead of waiting on
/// the SUM of five sequential aggregates. Measured at the SQL layer on a
/// 210k-event app: ~165 ms (events count) + ~160 ms (errors count) + ~180 ms
/// (top issues) + the series, all on one connection.
///
/// The risk that split introduces is DRIFT: four handlers computing what one
/// handler used to, diverging silently. So this asserts the sections agree with
/// `/overview` field for field, rather than merely that each returns 200.
#[tokio::test]
async fn overview_sections_agree_with_the_composite_route() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    let whole = srv
        .get_json(
            &format!("/v1/apps/{app}/overview?since_days=3650"),
            &f.owner_token,
        )
        .await;
    let totals = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/totals?since_days=3650"),
            &f.owner_token,
        )
        .await;
    let series = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/series?since_days=3650"),
            &f.owner_token,
        )
        .await;
    let top_issues = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/top-issues?since_days=3650"),
            &f.owner_token,
        )
        .await;
    let top_events = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/top-events?since_days=3650"),
            &f.owner_token,
        )
        .await;

    assert_eq!(whole["totals"], totals["totals"], "totals must not drift");
    assert_eq!(whole["error_rate"], totals["error_rate"]);
    assert_eq!(whole["crash_free_sessions"], totals["crash_free_sessions"]);
    assert_eq!(whole["events_series"], series["events_series"]);
    assert_eq!(whole["errors_series"], series["errors_series"]);
    assert_eq!(whole["top_issues"], top_issues);
    assert_eq!(whole["top_events"], top_events);

    // Non-vacuous: the fixture seeds errors, so a handler returning an empty
    // body everywhere would satisfy every equality above. Pin that there is
    // actually something to compare.
    assert!(
        totals["totals"]["errors"].as_i64().unwrap_or(0) > 0,
        "fixture must seed at least one error, else the equalities prove nothing"
    );
    assert!(
        top_issues
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "fixture must seed at least one issue"
    );

    srv.shutdown().await;
}

/// Each section is its own HTTP request, so each must resolve the read scope on
/// its own. Sharing one authorization decision across them would mean trusting
/// the client to say it had already been authorized — and an env-scoped member
/// must not see another environment's numbers through a section route just
/// because the composite route confines them.
#[tokio::test]
async fn every_overview_section_is_env_scoped_independently() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;
    let granted = f.granted_env;
    let other = f.other_env;

    // The member may read `granted_env` only.
    for section in ["totals", "series", "top-issues", "top-events"] {
        let ok = srv
            .get_status(
                &format!(
                    "/v1/apps/{app}/overview/{section}?environment_id={granted}&since_days=3650"
                ),
                &f.member_token,
            )
            .await;
        assert_eq!(
            ok, 200,
            "{section} must be readable in the granted environment"
        );

        let denied = srv
            .get_status(
                &format!(
                    "/v1/apps/{app}/overview/{section}?environment_id={other}&since_days=3650"
                ),
                &f.member_token,
            )
            .await;
        assert_eq!(
            denied, 403,
            "{section} must refuse an environment the member has no grant in"
        );
    }

    // And the confinement is not merely a status code: the granted environment's
    // error count must exclude the other environment's event. Asserted on
    // `totals` because it is the section carrying the aggregate a leak would
    // show up in.
    let scoped = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/totals?environment_id={granted}&since_days=3650"),
            &f.member_token,
        )
        .await;
    let unscoped = srv
        .get_section(
            &format!("/v1/apps/{app}/overview/totals?since_days=3650"),
            &f.owner_token,
        )
        .await;
    assert!(
        scoped["totals"]["errors"].as_i64().unwrap_or(-1)
            < unscoped["totals"]["errors"].as_i64().unwrap_or(-1),
        "the env-scoped total must be strictly smaller than the app-wide one, \
         or the scope is not being applied: scoped={scoped:?} unscoped={unscoped:?}"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn empty_environment_id_returns_400_over_http_not_all_environments() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };

    // --- seed one org -> project -> app -> environment, one user with just --
    // --- enough grant to read this app's issues/events -----------------------
    let suffix = Uuid::new_v4().simple().to_string();
    let (app_id, env_id, user_id) = {
        let mut conn = h.conn().await;
        let org = repo::create_org(
            &mut conn,
            "http-scoping org",
            &format!("http-scoping-org-{suffix}"),
        )
        .await
        .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "http-scoping project",
            &format!("http-scoping-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "http-scoping app",
            &format!("http-scoping-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");
        let env_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_http_scoping_{suffix}"),
            true,
        )
        .await;
        let user = repo::create_user(
            &mut conn,
            &format!("http-scoping-{suffix}@example.test"),
            "unused-password-hash",
            "Http Scoping Test User",
        )
        .await
        .expect("create user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "http-scoping role",
            "just enough to read this test's app",
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
        (app.id, env_id, user.id)
    };

    // --- mint an access token the spawned server will accept ---------------
    let keys = JwtKeys::new(JWT_SECRET, 900);
    let (token, _exp) = keys
        .issue_access(user_id, false, None)
        .expect("issue access token");
    let bearer = token.as_str();

    // --- the actual regression: ?environment_id= must 400 everywhere it ----
    // --- appears, exactly like a malformed value, on every scoped read ------

    // Group 1: routes that were broken (axum_extra::extract::Query) — proof
    // that removing the collapse fixes overview/issues/events-list, plus a
    // cross-tier timeseries endpoint whose reject-check had the same root
    // cause.
    for (path_prefix, group) in [
        (format!("/v1/apps/{app_id}/overview"), "overview"),
        (format!("/v1/apps/{app_id}/issues"), "issues"),
        (format!("/v1/apps/{app_id}/events/list"), "events/list"),
    ] {
        h.assert_status(&path_prefix, bearer, 200, &format!("{group} absent"))
            .await;
        h.assert_status(
            &format!("{path_prefix}?environment_id="),
            bearer,
            400,
            &format!("{group} empty"),
        )
        .await;
        h.assert_status(
            &format!("{path_prefix}?environment_id=not-a-uuid"),
            bearer,
            400,
            &format!("{group} malformed"),
        )
        .await;
        h.assert_status(
            &format!("{path_prefix}?environment_id={env_id}"),
            bearer,
            200,
            &format!("{group} valid uuid"),
        )
        .await;
    }

    // Group 2: a cross-tier timeseries endpoint. Any environment_id at all —
    // including `?environment_id=` — must 400: this group's contract is "not
    // supported yet", not "narrow if given", so even a real environment id is
    // rejected here (that's intentional, unlike group 1/3).
    let ts_prefix = format!(
        "/v1/apps/{app_id}/events/timeseries?from=2024-01-01T00:00:00Z&to=2024-01-02T00:00:00Z"
    );
    h.assert_status(&ts_prefix, bearer, 200, "timeseries absent")
        .await;
    h.assert_status(
        &format!("{ts_prefix}&environment_id="),
        bearer,
        400,
        "timeseries empty",
    )
    .await;
    h.assert_status(
        &format!("{ts_prefix}&environment_id=not-a-uuid"),
        bearer,
        400,
        "timeseries malformed",
    )
    .await;
    h.assert_status(
        &format!("{ts_prefix}&environment_id={env_id}"),
        bearer,
        400,
        "timeseries valid uuid (still rejected — group contract, not a scoping bug)",
    )
    .await;

    // Group 3: sessions — already used plain `axum::extract::Query` and was
    // never broken. Included so the same test proves the fix didn't change
    // an already-correct handler's behaviour.
    let sessions_prefix = format!("/v1/apps/{app_id}/sessions");
    h.assert_status(&sessions_prefix, bearer, 200, "sessions absent")
        .await;
    h.assert_status(
        &format!("{sessions_prefix}?environment_id="),
        bearer,
        400,
        "sessions empty",
    )
    .await;
    h.assert_status(
        &format!("{sessions_prefix}?environment_id=not-a-uuid"),
        bearer,
        400,
        "sessions malformed",
    )
    .await;
    h.assert_status(
        &format!("{sessions_prefix}?environment_id={env_id}"),
        bearer,
        200,
        "sessions valid uuid",
    )
    .await;

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Slice 3 Task 6: the env-scoped RBAC boundary, over HTTP.
// ---------------------------------------------------------------------------

/// The wire contract's 403 rows, driven through the real router. A unit test
/// of the decision function cannot see these — the S2 review's F7 finding was
/// that zero rejecting routes were exercised over HTTP, and the original
/// Critical in this feature lived entirely in which extractor a handler
/// imported.
#[tokio::test]
async fn env_scoped_member_is_confined_over_http() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    // Their own environment: 200.
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues?environment_id={}",
            f.app_id, f.granted_env
        ),
        &f.member_token,
        200,
        "member must read the environment they hold",
    )
    .await;

    // A sibling environment in the same app: 403, not 200-with-zero-rows.
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues?environment_id={}",
            f.app_id, f.other_env
        ),
        &f.member_token,
        403,
        "sibling environment must be refused, not empty",
    )
    .await;

    // Unattributed: 403.
    h.assert_status(
        &format!("/v1/apps/{}/issues?environment_id=none", f.app_id),
        &f.member_token,
        403,
        "unattributed needs app-wide reach",
    )
    .await;

    // Absent: 200, auto-narrowed. Must return strictly fewer rows than the
    // owner sees, and more than zero, or the narrowing did not happen.
    let member_all = h
        .get_json(&format!("/v1/apps/{}/issues", f.app_id), &f.member_token)
        .await;
    let owner_all = h
        .get_json(&format!("/v1/apps/{}/issues", f.app_id), &f.owner_token)
        .await;
    // S2c Task 4: the issues list answers a `SearchEnvelope`, not a bare
    // array. `total` rather than `data.len()` — `data` is capped at one page
    // (50 by default), so once a fixture grows past that, two different-sized
    // row sets would both report 50 and the narrowing assertion below would
    // stop testing anything.
    let m = member_all["total"]
        .as_i64()
        .unwrap_or_else(|| panic!("issues response has no `total`: {member_all}"));
    let o = owner_all["total"]
        .as_i64()
        .unwrap_or_else(|| panic!("issues response has no `total`: {owner_all}"));
    assert!(
        m < o,
        "absent environment_id must auto-narrow for a partial-reach member: \
         member saw {m}, owner saw {o}"
    );
    assert!(m > 0, "auto-narrowing must not narrow to nothing");

    h.shutdown().await;
}

/// An owner's behaviour must be byte-identical to before this slice.
#[tokio::test]
async fn app_wide_member_is_unaffected() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    for (path, label) in [
        (format!("/v1/apps/{}/issues", f.app_id), "absent"),
        (
            format!(
                "/v1/apps/{}/issues?environment_id={}",
                f.app_id, f.granted_env
            ),
            "granted_env",
        ),
        (
            format!(
                "/v1/apps/{}/issues?environment_id={}",
                f.app_id, f.other_env
            ),
            "other_env",
        ),
        (
            format!("/v1/apps/{}/issues?environment_id=none", f.app_id),
            "unattributed",
        ),
    ] {
        h.assert_status(
            &path,
            &f.owner_token,
            200,
            &format!("owner must still reach {label}"),
        )
        .await;
    }

    // A well-formed but foreign environment id is now refused rather than
    // silently returning an empty list — the check `parse_env`'s doc comment
    // has been asking for since Slice 2.
    let foreign = Uuid::new_v4();
    h.assert_status(
        &format!("/v1/apps/{}/issues?environment_id={foreign}", f.app_id),
        &f.owner_token,
        403,
        "foreign environment id must not be a silent empty list",
    )
    .await;

    h.shutdown().await;
}

/// Regression test for the fix-round-1 bug: `issues::detail` and
/// `issues::events` were unreachable for a caller whose only grant is
/// env-scoped, **even for their own environment**.
///
/// Both handlers called `authorize_app_perms` (for the `source:read` gate),
/// which resolves permissions via `sauron_auth::effective_at` — and that
/// hardcodes `env: None`. `rbac::grant_applies`'s `Scope::Env(e)` arm is
/// `Some(e) == env`, so an env grant can never satisfy a `None` env, and both
/// handlers 403'd before `authorized_read_scope` was ever reached. The result
/// was a broken journey for exactly the caller this slice exists to serve: an
/// env-scoped member could *list* issues but could not *open* one.
#[tokio::test]
async fn env_scoped_member_can_open_an_issue_in_their_environment() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    // 1. The issue detail view, scoped to the environment they hold.
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues/{}?environment_id={}",
            f.app_id, f.granted_issue_id, f.granted_env
        ),
        &f.member_token,
        200,
        "env-scoped member must be able to OPEN an issue in their environment",
    )
    .await;

    // 2. That issue's occurrences.
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues/{}/events?environment_id={}",
            f.app_id, f.granted_issue_id, f.granted_env
        ),
        &f.member_token,
        200,
        "env-scoped member must be able to read an issue's events in their environment",
    )
    .await;

    // Both also work with the environment absent — auto-narrowed to
    // Subset([granted_env]), which still contains this issue's event.
    h.assert_status(
        &format!("/v1/apps/{}/issues/{}", f.app_id, f.granted_issue_id),
        &f.member_token,
        200,
        "env-scoped member must be able to open an issue with environment_id absent",
    )
    .await;

    // 3. A sibling environment is still refused — the boundary is intact, the
    //    fix widened reach for the caller's OWN environment only.
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues/{}?environment_id={}",
            f.app_id, f.other_issue_id, f.other_env
        ),
        &f.member_token,
        403,
        "sibling environment must still be refused on issue detail",
    )
    .await;
    h.assert_status(
        &format!(
            "/v1/apps/{}/issues/{}/events?environment_id={}",
            f.app_id, f.other_issue_id, f.other_env
        ),
        &f.member_token,
        403,
        "sibling environment must still be refused on issue events",
    )
    .await;

    h.shutdown().await;
}

/// The `source:read` gate must still discriminate **at env scope** — the fix
/// must not have turned it into a pass-through.
///
/// Two members, identical except that one's role carries `perm::SOURCE_READ`,
/// both granted on the same single environment. Asserts on the response
/// **body**, not the status: both get 200, and the difference is whether the
/// symbolicated frame still carries its source-context keys.
#[tokio::test]
async fn source_read_gate_still_discriminates_at_env_scope() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let detail_path = format!(
        "/v1/apps/{}/issues/{}?environment_id={}",
        f.app_id, f.granted_issue_id, f.granted_env
    );
    let events_path = format!(
        "/v1/apps/{}/issues/{}/events?environment_id={}",
        f.app_id, f.granted_issue_id, f.granted_env
    );

    // --- WITH source:read: context lines survive -------------------------
    let with_source = h.get_json(&detail_path, &f.source_member_token).await;
    let frames = symbolicated_frames(&with_source["latest_event"]);
    assert!(
        !frames.is_empty(),
        "fixture must deliver a symbolicated frame, else this test proves nothing: {with_source}"
    );
    for key in SOURCE_CONTEXT_KEYS {
        assert!(
            frames.iter().any(|fr| fr.get(key).is_some()),
            "a member WITH source:read on this environment must still see {key}: {with_source}"
        );
    }
    assert!(
        with_source.to_string().contains(FIXTURE_CONTEXT_LINE),
        "a member WITH source:read must see the actual source line: {with_source}"
    );

    // --- WITHOUT source:read: context lines stripped, event still there --
    let without_source = h.get_json(&detail_path, &f.member_token).await;
    let frames = symbolicated_frames(&without_source["latest_event"]);
    assert!(
        !frames.is_empty(),
        "the frame itself must still be present — only the source context is gated, \
         symbol/file/line are not: {without_source}"
    );
    for key in SOURCE_CONTEXT_KEYS {
        assert!(
            frames.iter().all(|fr| fr.get(key).is_none()),
            "a member WITHOUT source:read must NOT see {key} — the gate must not be a \
             pass-through: {without_source}"
        );
    }
    assert!(
        !without_source.to_string().contains(FIXTURE_CONTEXT_LINE),
        "a member WITHOUT source:read must not see the source line anywhere in the body: \
         {without_source}"
    );
    // The frame is still useful: symbol/file/line are NOT gated.
    assert!(
        frames.iter().any(|fr| fr.get("function").is_some()),
        "symbol names stay visible without source:read: {without_source}"
    );

    // --- same gate, same way, on the occurrences endpoint ----------------
    let with_source_events = h.get_json(&events_path, &f.source_member_token).await;
    assert!(
        with_source_events
            .to_string()
            .contains(FIXTURE_CONTEXT_LINE),
        "events: a member WITH source:read must see the source line: {with_source_events}"
    );
    let without_source_events = h.get_json(&events_path, &f.member_token).await;
    assert!(
        !without_source_events
            .to_string()
            .contains(FIXTURE_CONTEXT_LINE),
        "events: a member WITHOUT source:read must not see the source line: \
         {without_source_events}"
    );

    h.shutdown().await;
}

/// The `stacktrace_symbolicated` frames of an event JSON value, or an empty
/// vec if the field is absent/null/not an array.
fn symbolicated_frames(
    event: &serde_json::Value,
) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    event["stacktrace_symbolicated"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f.as_object()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Slice 3 Task 7: creating an env-scoped grant, and list_environments
// narrowed by reach.
// ---------------------------------------------------------------------------

/// An env-scoped member must see only the environments they hold — otherwise
/// the dashboard's environment picker offers an entry that 403s the moment it
/// is chosen.
#[tokio::test]
async fn list_environments_is_filtered_by_reach() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let owner = h
        .get_json(
            &format!("/v1/apps/{}/environments", f.app_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        owner
            .as_array()
            .expect("environments response is a JSON array")
            .len(),
        2,
        "an app-wide grant must still see both environments: {owner}"
    );

    let member = h
        .get_json(
            &format!("/v1/apps/{}/environments", f.app_id),
            &f.member_token,
        )
        .await;
    let envs = member
        .as_array()
        .expect("environments response is a JSON array");
    assert_eq!(
        envs.len(),
        1,
        "an env-scoped member must see only their own environment: {member}"
    );
    assert_eq!(
        envs[0]["id"].as_str().expect("id is a string"),
        f.granted_env.to_string(),
        "the one visible environment must be the granted one: {member}"
    );

    h.shutdown().await;
}

/// The grant API must accept `scope_type: "env"` end to end, and the new
/// grant must show up on `/access` so the dashboard can render it.
#[tokio::test]
async fn an_env_scoped_grant_can_be_created_over_http() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let status = h
        .post_status(
            &format!("/v1/orgs/{}/grants", f.org_id),
            &f.org_owner_token,
            json!({
                "email": f.member_email,
                "role_id": f.member_role_id,
                "scopes": [{ "scope_type": "env", "scope_id": f.other_env }],
            }),
        )
        .await;
    assert_eq!(status, 200, "env scope_type must be accepted");

    // And it must be reflected in /access, so the dashboard can render it.
    let access = h
        .get_json(&format!("/v1/orgs/{}/access", f.org_id), &f.member_token)
        .await;
    let has_env_grant = access["grants"]
        .as_array()
        .expect("grants is a JSON array")
        .iter()
        .any(|g| g["scope_type"] == "env" && g["scope_id"] == f.other_env.to_string());
    assert!(
        has_env_grant,
        "/access must surface the new env grant: {access}"
    );

    h.shutdown().await;
}

/// A cross-tenant environment id must be refused, exactly as an app id is.
/// `role_grants.scope_id` has no FK, so `validate_scopes_in_org`'s
/// `owner_org == org_id` filter is the only thing enforcing this.
#[tokio::test]
async fn an_env_scope_from_another_org_is_refused() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;
    let other = h.seed_second_org().await;

    let status = h
        .post_status(
            &format!("/v1/orgs/{}/grants", f.org_id),
            &f.org_owner_token,
            json!({
                "email": f.member_email,
                "role_id": f.member_role_id,
                "scopes": [{ "scope_type": "env", "scope_id": other.env_id }],
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "an environment outside the org must be refused"
    );

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Task 16: an env-only grant must be able to NAVIGATE to its environment —
// `list_projects`/`list_apps` must lift `reach.envs` the same way
// `list_environments` already does, and `GET /v1/apps/{id}` must be reachable
// (read-only) via `authorize_app_reachable` — without widening what an
// env-scoped grant may WRITE.
// ---------------------------------------------------------------------------

/// `nav_member_token`'s env grant also carries `project:read`/`app:read`
/// (unlike plain `member_token`, whose role is deliberately narrower — see
/// the fixture's doc comment). Before Task 16, `list_projects`/`list_apps`
/// never consulted `reach.envs`, so this caller's project and app switchers
/// were both empty and there was no path from login to their own app.
#[tokio::test]
async fn env_scoped_member_can_navigate_to_their_project_and_app_over_http() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    // 1. The project switcher: `GET /v1/orgs/{org_id}/projects`.
    let projects = h
        .get_json(
            &format!("/v1/orgs/{}/projects", f.org_id),
            &f.nav_member_token,
        )
        .await;
    let projects = projects
        .as_array()
        .expect("projects response is a JSON array");
    assert_eq!(
        projects.len(),
        1,
        "an env-only grant must lift to exactly its one ancestor project: {projects:?}"
    );
    let project_id = projects[0]["id"]
        .as_str()
        .expect("project id is a string")
        .to_string();

    // 2. The app switcher under that project: `GET /v1/projects/{id}/apps`.
    let apps = h
        .get_json(
            &format!("/v1/projects/{project_id}/apps"),
            &f.nav_member_token,
        )
        .await;
    let apps = apps.as_array().expect("apps response is a JSON array");
    assert_eq!(
        apps.len(),
        1,
        "an env-only grant must lift to exactly its one ancestor app: {apps:?}"
    );
    assert_eq!(
        apps[0]["id"].as_str().expect("app id is a string"),
        f.app_id.to_string(),
        "the one visible app must be the one the environment belongs to: {apps:?}"
    );

    // 3. The app itself: `GET /v1/apps/{app_id}` — needed so the dashboard
    //    has metadata to render on the way to the environment.
    h.assert_status(
        &format!("/v1/apps/{}", f.app_id),
        &f.nav_member_token,
        200,
        "an env-scoped member with app:read must be able to GET the app for navigation",
    )
    .await;

    h.shutdown().await;
}

/// The most important test in this task: reading the app for navigation must
/// NOT widen into being able to mutate it. `nav_member_token`'s role
/// carries `app:update` AND `app:delete` — deliberately, so this test proves
/// the boundary is about SCOPE, not about which permissions the role has: an
/// env-scoped grant, no matter how permissive its role, cannot reach a
/// mutation gated by `authorize_app`'s strict `env: None` resolution (the
/// `Scope::Env` arm of `grant_applies` can never satisfy `env: None` — see
/// `rbac.rs`'s module doc comment on cascade semantics). If this role held
/// only read permissions instead, the same 403 would be trivially true for
/// the wrong reason (permission absent everywhere), which would not catch a
/// bug that widened `update_app`/`delete_app` to `authorize_app_reachable`.
///
/// This is the proof that `authorize_app_reachable` was wired into
/// `get_app` only, and that `update_app`/`delete_app` are still gated by the
/// strict `authorize_app`. See this file's module docs / the task report for
/// the deliberate-break run that flips this exact assertion when
/// `update_app` is (temporarily) switched to `authorize_app_reachable`.
#[tokio::test]
async fn env_scoped_member_cannot_mutate_the_app_over_http() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    // Sanity: the read this member IS entitled to still succeeds.
    h.assert_status(
        &format!("/v1/apps/{}", f.app_id),
        &f.nav_member_token,
        200,
        "the read this test's member IS entitled to must still succeed",
    )
    .await;

    let patch_status = h
        .patch_status(
            &format!("/v1/apps/{}", f.app_id),
            &f.nav_member_token,
            json!({ "name": "renamed-by-env-scoped-member", "ingest_enabled": true }),
        )
        .await;
    assert_eq!(
        patch_status, 403,
        "an env-scoped grant must NOT be able to rename the app even though its role DOES \
         carry app:update — the grant's scope is narrower than the app, and that must be \
         what refuses it, not a missing permission"
    );

    let delete_status = h
        .delete_status(&format!("/v1/apps/{}", f.app_id), &f.nav_member_token)
        .await;
    assert_eq!(
        delete_status, 403,
        "an env-scoped grant must NOT be able to delete the app even though its role DOES \
         carry app:delete — same reasoning as the PATCH assertion above"
    );

    h.shutdown().await;
}

/// The cross-tenant boundary must not have widened: a caller whose only
/// grants live in the FIRST org must see nothing when listing a SECOND org's
/// projects, exactly as before `env_ancestries` was consulted. `env_ancestries`
/// results are filtered by `ancestor_org == org_id` in `list_projects` and by
/// `ancestor_project == project_id` in `list_apps`, mirroring `app_ancestries`'s
/// existing filter exactly.
#[tokio::test]
async fn env_scoped_member_does_not_see_another_orgs_project_over_http() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;
    let other = h.seed_second_org().await;

    // `nav_member` holds no grant at all in `other`'s org, so listing IT must
    // 403 (not a member of that org) — not silently return `[]`, and
    // certainly not leak the first org's project into the response.
    h.assert_status(
        &format!("/v1/orgs/{}/projects", other.org_id),
        &f.nav_member_token,
        403,
        "an env-scoped grant in one org must not see a different org's projects",
    )
    .await;

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Task 14: the router-enumeration test (closes F2, F6, F7).
//
// `environment_id` handling has been got wrong four separate times in this
// feature, each time because two hand-maintained lists had to agree and
// silently drifted:
//
//   1. A handler silently widened to "all environments" instead of rejecting
//      a malformed value (the `?environment_id=` empty-value regression at
//      the top of this file).
//   2. An opt-out list on the dashboard missed three route families, so they
//      400'd by default (see `dashboard/src/lib/api/scope.ts`'s module docs).
//   3. Three timeseries endpoints rejected the parameter via an inline check
//      a reconciliation grep could not see (`analytics.rs`'s
//      `TimeseriesQuery` doc comment).
//   4. A dashboard exclusion list whose own maintenance instructions pointed
//      at an incomplete grep.
//
// Both tests below are built so the two hand-maintained sides check each
// other instead of drifting: `app_scoped_get_route_templates` walks the REAL
// route table out of `main.rs`'s literal source (not a hand-copied list of
// paths), and `read_dashboard_exclusions` walks
// `dashboard/src/lib/api/scope.ts`'s literal source (not a hand-copied list
// of exclusions) — a route or an exclusion added on only one side now fails
// a test instead of shipping silently.
// ---------------------------------------------------------------------------

/// Every `.route("...", ...)` path in `main.rs`'s literal source that (a)
/// sits under `/v1/apps/{app_id}` (the bare app route counts too) and (b)
/// attaches a `get(...)` handler — read straight out of the exact router the
/// spawned server above is actually built from, not a hand-maintained copy
/// of it. No `regex` dependency: the balanced-paren scan below is exactly as
/// much parsing as `main.rs`'s `.route(path, method(handler)...)` shape
/// needs, and a parse of the real file is the accepted alternative here to a
/// hand-written list, which is the thing this test exists to eliminate.
fn app_scoped_get_route_templates() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs");
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("app_scoped_get_route_templates: could not read {path}: {e}"));
    let bytes = src.as_bytes();

    let marker = ".route(";
    let mut templates = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = src[search_from..].find(marker) {
        let open_paren = search_from + rel + marker.len() - 1;
        let mut depth = 0i32;
        let mut i = open_paren;
        let mut close_paren = None;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close_paren = Some(i);
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let close_paren = close_paren.unwrap_or_else(|| {
            panic!(
                "app_scoped_get_route_templates: unbalanced parens in a .route(...) call in \
                 {path} starting at byte {open_paren}"
            )
        });
        let args = &src[open_paren + 1..close_paren];
        if let Some(q1) = args.find('"') {
            if let Some(q2_rel) = args[q1 + 1..].find('"') {
                let route_path = &args[q1 + 1..q1 + 1 + q2_rel];
                let is_app_scoped = route_path == "/v1/apps/{app_id}"
                    || route_path.starts_with("/v1/apps/{app_id}/");
                let has_get = {
                    let a = args.as_bytes();
                    (0..a.len().saturating_sub(3)).any(|idx| {
                        &a[idx..idx + 4] == b"get(" && (idx == 0 || !is_ident_byte(a[idx - 1]))
                    })
                };
                if is_app_scoped && has_get {
                    templates.push(route_path.to_string());
                }
            }
        }
        search_from = close_paren + 1;
    }
    templates
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Turn a route template from [`app_scoped_get_route_templates`] into a
/// concrete request path: substitute `{app_id}` for a real one, substitute
/// the small number of OTHER path parameters these routes carry with
/// harmless placeholders, and attach whatever non-`environment_id` query
/// parameter a route needs just to get past its OWN `Query<T>` extraction —
/// without it, a missing-required-field `400` would be indistinguishable
/// from an `environment_id` rejection and would misclassify a narrowing
/// route as a rejecting one in the second test below.
///
/// Placeholders, not real fixture rows: every handler below decides whether
/// `environment_id` is well-formed and authorized BEFORE it ever looks up
/// the entity any other path/query parameter names (e.g. `issues::detail`
/// calls `authorized_read_scope_with_perms` before `repo::get_issue`; see
/// `routes::scope`'s module docs for why that ordering is deliberate). So a
/// syntactically-valid-but-nonexistent id reaches the exact code this test
/// cares about; a real seeded row would only matter for a test asserting
/// response BODIES, which these two are not.
fn build_request_path(template: &str, app_id: Uuid) -> String {
    let mut path = template.replace("{app_id}", &app_id.to_string());

    // Any OTHER `{param}` left in the template needs a deliberate
    // substitution below — an unhandled one panics rather than silently
    // sending the literal `{param}` text as part of the URL, which is the
    // same "silently wrong instead of loudly wrong" failure mode this whole
    // task exists to eliminate.
    while let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .unwrap_or_else(|| panic!("build_request_path: unbalanced '{{' in {template:?}"));
        let param = path[start + 1..end].to_string();
        let replacement = match param.as_str() {
            "issue_id" => Uuid::new_v4().to_string(),
            "session_id" => "task-14-route-enum-session".to_string(),
            "distinct_id" => "task-14-route-enum-person".to_string(),
            // `workflows::detail`/`workflows::runs` — a nonexistent name is
            // fine here (see this function's doc comment: `environment_id`
            // handling happens before any entity lookup), but it must not be
            // the literal `{name}` text.
            "name" => "task-14-route-enum-workflow".to_string(),
            other => panic!(
                "build_request_path: route template {template:?} has an unhandled path \
                 parameter {{{other}}} — add a substitution for it in build_request_path \
                 rather than letting this test silently send the literal '{{{other}}}' text as \
                 part of the URL"
            ),
        };
        path.replace_range(start..=end, &replacement);
    }

    // A few routes need a non-`environment_id` query parameter just to get
    // past their OWN required-field `Query<T>` extraction: `devices::detail`
    // and `screens::detail` take their target (`key` / `name`) as a query
    // param rather than a path segment, and the three cross-tier timeseries
    // handlers require `from`/`to` with no defaults.
    let extra_query: Option<&str> = match template {
        "/v1/apps/{app_id}/device" => Some("key=task-14-route-enum-device"),
        "/v1/apps/{app_id}/screens/detail" => Some("name=task-14-route-enum-screen"),
        // The four screen-detail sections take their target screen as a
        // required `name` query param, exactly like `screens/detail` above.
        //
        // Without this arm the probe sends no `name`, `Query<ScreenSectionQuery>`
        // fails to deserialize BEFORE the handler runs, and the resulting 400 is
        // indistinguishable from "this route rejects environment_id" — which put
        // all four in the rejecting set and broke
        // `the_backend_rejection_set_matches_the_dashboard_exclusion_list`,
        // while making `every_app_scoped_get_either_narrows_or_rejects_environment_id`
        // pass VACUOUSLY on them (it accepts any 400, and this one has nothing to
        // do with environment_id).
        //
        // Adding these to `scope.ts`'s BACKEND_REJECTS_ENVIRONMENT_ID instead
        // would go green and be WRONG: that array is what `shouldScopeUrl` reads,
        // so the dashboard would stop attaching `environment_id` and all four
        // cards would render every environment's rows under environment-scoped
        // stat tiles. These routes genuinely narrow — see `authorized_read_scope`
        // in each handler.
        "/v1/apps/{app_id}/screens/events"
        | "/v1/apps/{app_id}/screens/exceptions"
        | "/v1/apps/{app_id}/screens/devices"
        | "/v1/apps/{app_id}/screens/users" => Some("name=task-14-route-enum-screen"),
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

/// Append `?environment_id=<value>` (or `&environment_id=<value>` when the
/// path already carries its own required query parameters — see
/// [`build_request_path`]'s `extra_query`) onto a path.
fn with_environment_id(path: &str, value: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{path}{sep}environment_id={value}")
}

/// The app-scoped route families `dashboard/src/lib/api/scope.ts` believes
/// the backend rejects `environment_id` on outright — read straight out of
/// that file's `BACKEND_REJECTS_ENVIRONMENT_ID` array, the same way
/// `dashboard/src/lib/models/permissions.test.ts` reads `perm::ALL` out of
/// `rbac.rs` in the other direction (source, not a copy).
///
/// Deliberately NOT `scope.ts`'s full `APP_CONFIG_SUBPATHS` union (the one
/// `shouldScopeUrl` actually uses): that union also folds in
/// `UI_ONLY_EXCLUSIONS` (`first-event`), which the backend does NOT reject —
/// `first_event` narrows on `environment_id` just fine; the dashboard simply
/// chooses not to attach the currently-selected environment to that one call
/// site for a reason specific to it (see `scope.ts`'s comment on it). Folding
/// that into this comparison would make this test demand the backend reject
/// a route it correctly narrows on.
fn read_dashboard_exclusions() -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../dashboard/src/lib/api/scope.ts"
    );
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read_dashboard_exclusions: could not read {path}: {e}"));

    let marker = "const BACKEND_REJECTS_ENVIRONMENT_ID: RegExp[] = [";
    let body_start = src
        .find(marker)
        .map(|i| i + marker.len())
        .unwrap_or_else(|| {
            panic!("read_dashboard_exclusions: could not find {marker:?} in {path}")
        });
    let body_end = src[body_start..]
        .find("];")
        .map(|i| body_start + i)
        .unwrap_or_else(|| {
            panic!(
                "read_dashboard_exclusions: unterminated BACKEND_REJECTS_ENVIRONMENT_ID array \
                 in {path}"
            )
        });
    let body = &src[body_start..body_end];

    // The two shapes every entry in that array takes today: the bare
    // `/v1/apps/{id}` route (no trailing segment, optional trailing slash /
    // query string), or `/v1/apps/{id}/<literal segment>` followed by an
    // optional trailing slash/path/query string.
    const PREFIX: &str = "/^\\/v1\\/apps\\/[^/]+";
    const BARE_SUFFIX: &str = "\\/?(?:\\?.*)?$/";
    const SUBPATH_SUFFIX: &str = "(?:[/?].*)?$/";

    let mut templates = Vec::new();
    for raw_line in body.lines() {
        let line = raw_line.trim().trim_end_matches(',').trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rest = line.strip_prefix(PREFIX).unwrap_or_else(|| {
            panic!(
                "read_dashboard_exclusions: entry {line:?} in {path} does not start with the \
                 expected {PREFIX:?} — this parser's assumptions about the array's regex shape \
                 are stale; update read_dashboard_exclusions to match scope.ts"
            )
        });
        let template = if rest == BARE_SUFFIX {
            "/v1/apps/{app_id}".to_string()
        } else if let Some(segment) = rest.strip_suffix(SUBPATH_SUFFIX) {
            format!("/v1/apps/{{app_id}}{}", segment.replace("\\/", "/"))
        } else {
            panic!(
                "read_dashboard_exclusions: entry {line:?} in {path} ends with neither the \
                 bare ({BARE_SUFFIX:?}) nor the subpath ({SUBPATH_SUFFIX:?}) suffix this parser \
                 expects — update read_dashboard_exclusions to match scope.ts"
            )
        };
        templates.push(template);
    }
    templates.sort();
    templates
}

/// Every `/v1/apps/{id}/...` GET must either NARROW on `?environment_id=` or
/// reject it with a `400`. Silently ignoring it is the defect class that has
/// recurred four times: the caller believes a filter was applied and it was
/// not.
///
/// This walks the real router rather than a hand-written list, so a route
/// added tomorrow is covered without anyone remembering to add it here.
#[tokio::test]
async fn every_app_scoped_get_either_narrows_or_rejects_environment_id() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let templates = app_scoped_get_route_templates();
    assert!(
        templates.len() >= 20,
        "app_scoped_get_route_templates() returned only {} route(s): {templates:?} — expected \
         at least 20. Either main.rs's app-scoped GET route table shrank a lot, or this \
         function's parse is broken. A test that silently enumerates too few (or zero) routes \
         passes forever and guards nothing.",
        templates.len(),
    );

    let mut unhandled = Vec::new();
    for template in &templates {
        let base = build_request_path(template, f.app_id);
        let path = with_environment_id(&base, "not-a-uuid");
        let status = h.get_status(&path, &f.owner_token).await;
        // A narrowing route 400s on a malformed value; a rejecting route
        // 400s on any value. Either way, 200 means the parameter was
        // ignored.
        assert_ne!(
            status, 200,
            "GET {path} (route template {template:?}) accepted a malformed environment_id — it \
             is neither narrowing nor rejecting, so it is silently ignoring the parameter"
        );
        if status != 400 {
            unhandled.push(format!("{path} -> {status}"));
        }
    }
    assert!(
        unhandled.is_empty(),
        "malformed environment_id produced a non-400, non-200 status (permission/extraction \
         issue unrelated to environment_id handling?) on: {unhandled:?}"
    );

    h.shutdown().await;
}

/// The set of routes that reject `environment_id` outright (`400` even on a
/// perfectly valid value) must equal the set `dashboard/src/lib/api/scope.ts`
/// excludes from scoping for that same reason. Maintained in two files,
/// checked in one.
#[tokio::test]
async fn the_backend_rejection_set_matches_the_dashboard_exclusion_list() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let templates = app_scoped_get_route_templates();
    assert!(
        templates.len() >= 20,
        "app_scoped_get_route_templates() returned only {} route(s): {templates:?} — see the \
         sibling test's assertion for why this is fatal rather than merely suspicious.",
        templates.len(),
    );

    let mut rejecting = Vec::new();
    for template in &templates {
        let base = build_request_path(template, f.app_id);
        // A rejecting route 400s even on a perfectly VALID value.
        let path = with_environment_id(&base, &f.granted_env.to_string());
        let status = h.get_status(&path, &f.owner_token).await;
        if status == 400 {
            rejecting.push(template.clone());
        }
    }
    rejecting.sort();
    rejecting.dedup();

    let expected = read_dashboard_exclusions();
    assert_eq!(
        rejecting, expected,
        "the backend's rejecting-route set (400 even on a VALID environment_id) and \
         dashboard/src/lib/api/scope.ts's BACKEND_REJECTS_ENVIRONMENT_ID have diverged"
    );

    h.shutdown().await;
}

/// Every `.route("...", ...)` path in `main.rs`'s literal source that sits
/// under `/v1/projects/{project_id}` and attaches a `get(...)` handler — the
/// project-scoped twin of [`app_scoped_get_route_templates`], parsed out of
/// the same real router for the same reason.
fn project_scoped_get_route_templates() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs");
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("project_scoped_get_route_templates: could not read {path}: {e}")
    });
    let bytes = src.as_bytes();

    let marker = ".route(";
    let mut templates = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = src[search_from..].find(marker) {
        let open_paren = search_from + rel + marker.len() - 1;
        let mut depth = 0i32;
        let mut i = open_paren;
        let mut close_paren = None;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close_paren = Some(i);
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let close_paren = close_paren.unwrap_or_else(|| {
            panic!("project_scoped_get_route_templates: unbalanced parens in {path}")
        });
        let args = &src[open_paren + 1..close_paren];
        if let Some(q1) = args.find('"') {
            if let Some(q2_rel) = args[q1 + 1..].find('"') {
                let route_path = &args[q1 + 1..q1 + 1 + q2_rel];
                let is_project_scoped = route_path == "/v1/projects/{project_id}"
                    || route_path.starts_with("/v1/projects/{project_id}/");
                let has_get = {
                    let a = args.as_bytes();
                    (0..a.len().saturating_sub(3)).any(|idx| {
                        &a[idx..idx + 4] == b"get(" && (idx == 0 || !is_ident_byte(a[idx - 1]))
                    })
                };
                if is_project_scoped && has_get {
                    templates.push(route_path.to_string());
                }
            }
        }
        search_from = close_paren + 1;
    }
    templates
}

/// `dashboard/src/lib/api/scope.ts`'s `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID`,
/// read out of that file's literal source rather than hand-copied.
fn read_dashboard_project_exclusions() -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../dashboard/src/lib/api/scope.ts"
    );
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("read_dashboard_project_exclusions: could not read {path}: {e}")
    });
    let marker = "const PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID: RegExp[] = [";
    let body_start = src
        .find(marker)
        .map(|i| i + marker.len())
        .unwrap_or_else(|| panic!("read_dashboard_project_exclusions: {marker:?} not in {path}"));
    let body_end = src[body_start..]
        .find("];")
        .map(|i| body_start + i)
        .unwrap_or_else(|| panic!("read_dashboard_project_exclusions: unterminated array"));
    let body = &src[body_start..body_end];

    const PREFIX: &str = "/^\\/v1\\/projects\\/[^/]+";
    const SUFFIX: &str = "(?:[/?].*)?$/";

    let mut templates = Vec::new();
    for raw_line in body.lines() {
        let line = raw_line.trim().trim_end_matches(',').trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rest = line.strip_prefix(PREFIX).unwrap_or_else(|| {
            panic!(
                "read_dashboard_project_exclusions: entry {line:?} does not start with \
                 {PREFIX:?} — this parser's assumptions are stale; update it to match scope.ts"
            )
        });
        let segment = rest.strip_suffix(SUFFIX).unwrap_or_else(|| {
            panic!(
                "read_dashboard_project_exclusions: entry {line:?} does not end with {SUFFIX:?} \
                 — update this parser to match scope.ts"
            )
        });
        // `\/` -> `/` and `\.` -> `.`: the `.csv` route is the first entry in
        // either array whose literal segment contains a regex metacharacter.
        templates.push(format!(
            "/v1/projects/{{project_id}}{}",
            segment.replace("\\/", "/").replace("\\.", ".")
        ));
    }
    templates.sort();
    templates
}

/// Concrete request path for a project-scoped template, plus whatever
/// non-`environment_id` query parameters the route needs just to get past its
/// OWN `Query<T>` extraction — without them a missing-required-field 400 would
/// be indistinguishable from an `environment_id` rejection.
fn build_project_request_path(template: &str, project_id: Uuid, app_id: Uuid) -> String {
    let mut path = template.replace("{project_id}", &project_id.to_string());
    // `if let`, not the `while let` its app-scoped sibling uses: this branch
    // has no substitution table to iterate towards, it only ever panics, and
    // `-D warnings` rejects a `while` whose body cannot reach a second
    // iteration (`clippy::never_loop`).
    if let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .unwrap_or_else(|| {
                panic!("build_project_request_path: unbalanced '{{' in {template:?}")
            });
        let param = path[start + 1..end].to_string();
        panic!(
            "build_project_request_path: template {template:?} has an unhandled path parameter \
             {{{param}}} — add a substitution rather than sending the literal text"
        );
    }
    let extra_query: Option<String> = match template {
        "/v1/projects/{project_id}/active-users" | "/v1/projects/{project_id}/active-users.csv" => {
            Some(format!(
                "from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z&selection={app_id}"
            ))
        }
        _ => None,
    };
    if let Some(q) = extra_query {
        path.push('?');
        path.push_str(&q);
    }
    path
}

/// The set of `/v1/projects/{id}/…` GETs that reject `environment_id` outright
/// must equal `scope.ts`'s `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID`.
///
/// The active-users routes are the first telemetry reads outside
/// `/v1/apps/{id}/…`, so `APP_SCOPED_URL` never matches them and
/// `app_scoped_get_route_templates` never enumerates them. Compensating with
/// one bespoke case in a new file would mean the next author never learns to
/// replicate it; this makes `reject_environment_id` mandatory-by-test for
/// every future project-scoped telemetry route.
#[tokio::test]
async fn the_project_rejection_set_matches_the_dashboard_project_exclusion_list() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let templates = project_scoped_get_route_templates();
    assert!(
        templates.len() >= 4,
        "project_scoped_get_route_templates() returned only {} route(s): {templates:?} — a test \
         that silently enumerates too few routes passes forever and guards nothing.",
        templates.len(),
    );

    let mut rejecting = Vec::new();
    for template in &templates {
        let base = build_project_request_path(template, f.project_id, f.app_id);
        // A rejecting route 400s even on a perfectly VALID value.
        let path = with_environment_id(&base, &f.granted_env.to_string());
        // `org_owner_token` holds the Owner preset at org scope, so a non-400
        // here is about environment handling and never about permissions.
        let status = h.get_status(&path, &f.org_owner_token).await;
        if status == 400 {
            rejecting.push(template.clone());
        }
    }
    rejecting.sort();
    rejecting.dedup();

    let expected = read_dashboard_project_exclusions();
    assert_eq!(
        rejecting, expected,
        "the backend's project-scoped rejecting-route set and \
         dashboard/src/lib/api/scope.ts's PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID have diverged"
    );

    h.shutdown().await;
}

// ===========================================================================
// Environments are defined per PROJECT (migration 2026-07-30-000033)
// ===========================================================================
//
// The catalogue (`environments`, owned by a project) names an environment once;
// the enrollment (`app_environments`) is one app's membership in it and carries
// the ingest key. Everything below is driven over HTTP rather than through
// `repo::*` on purpose: the fan-out that keeps the two levels consistent —
// enroll every app when an environment is added, enroll every environment when
// an app is added — lives in the route layer, so a repo-level test would assert
// against the very thing it is supposed to be checking.

/// An org + a user holding every permission, granted at org scope. Everything
/// else each test needs is created through the API so that provisioning runs.
struct EnvLifecycleFixture {
    org_id: Uuid,
    bearer: String,
}

impl TestServer {
    async fn seed_env_lifecycle_fixture(&self) -> EnvLifecycleFixture {
        let suffix = Uuid::new_v4().simple().to_string();
        let mut conn = self.conn().await;
        let org = repo::create_org(
            &mut conn,
            "env lifecycle org",
            &format!("env-life-{suffix}"),
        )
        .await
        .expect("create org");
        let user = repo::create_user(
            &mut conn,
            &format!("env-life-{suffix}@example.test"),
            "unused-password-hash",
            "Env Lifecycle Test User",
        )
        .await
        .expect("create user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "env lifecycle role",
            "everything, so the test asserts behavior rather than permissions",
            json!(perm::ALL),
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
        let (token, _exp) = keys
            .issue_access(user.id, false, None)
            .expect("issue access token");
        EnvLifecycleFixture {
            org_id: org.id,
            bearer: token,
        }
    }
}

/// `POST /v1/orgs/{org}/projects`, returning the new project id.
async fn create_project_http(h: &TestServer, bearer: &str, org_id: Uuid, name: &str) -> Uuid {
    let resp = h
        .post(
            &format!("/v1/orgs/{org_id}/projects"),
            bearer,
            json!({ "name": name }),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200, "create project {name}");
    let body: serde_json::Value =
        serde_json::from_str(&resp.text().await.expect("project body")).expect("project json");
    body["id"].as_str().expect("project id").parse().unwrap()
}

/// `POST /v1/projects/{project}/apps`, returning the new app id.
async fn create_app_http(h: &TestServer, bearer: &str, project_id: Uuid, name: &str) -> Uuid {
    let resp = h
        .post(
            &format!("/v1/projects/{project_id}/apps"),
            bearer,
            json!({ "name": name, "app_type": "web" }),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200, "create app {name}");
    let body: serde_json::Value =
        serde_json::from_str(&resp.text().await.expect("app body")).expect("app json");
    body["id"].as_str().expect("app id").parse().unwrap()
}

/// The app's enrollments as `(environment name, enrollment row)`.
async fn enrollments(
    h: &TestServer,
    bearer: &str,
    app_id: Uuid,
) -> Vec<(String, serde_json::Value)> {
    let body = h
        .get_json(&format!("/v1/apps/{app_id}/environments"), bearer)
        .await;
    body.as_array()
        .expect("enrollment array")
        .iter()
        .map(|e| (e["name"].as_str().expect("name").to_string(), e.clone()))
        .collect()
}

/// Names in the project catalogue.
async fn catalogue(h: &TestServer, bearer: &str, project_id: Uuid) -> Vec<String> {
    let body = h
        .get_json(&format!("/v1/projects/{project_id}/environments"), bearer)
        .await;
    body.as_array()
        .expect("catalogue array")
        .iter()
        .map(|e| e["name"].as_str().expect("name").to_string())
        .collect()
}

#[tokio::test]
async fn a_new_project_is_born_with_the_default_environment() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let project_id = create_project_http(&h, &f.bearer, f.org_id, "born with dev").await;

    assert_eq!(
        catalogue(&h, &f.bearer, project_id).await,
        vec!["dev".to_string()],
        "a project must be created with exactly one environment — an empty \
         catalogue makes every app created in it unreachable by any SDK"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_new_app_is_enrolled_in_every_environment_of_its_project() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let project_id = create_project_http(&h, &f.bearer, f.org_id, "enrol new app").await;

    // Add a second environment BEFORE the app exists, so the app-create path is
    // what has to notice it. An app that only joined `dev` would be missing from
    // the `staging` picker its siblings already appear in.
    assert_eq!(
        h.post_status(
            &format!("/v1/projects/{project_id}/environments"),
            &f.bearer,
            json!({ "name": "staging" }),
        )
        .await,
        200,
        "create staging"
    );

    let app_id = create_app_http(&h, &f.bearer, project_id, "late app").await;
    let rows = enrollments(&h, &f.bearer, app_id).await;

    let mut names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["dev", "staging"],
        "a new app must be enrolled in every live environment of its project"
    );

    // Exactly one default, and it is `dev` — the deterministic preference order
    // (`production`, then `dev`, then alphabetical) that `pick_default_env`
    // documents, and that migration 000026 used for the same question.
    let defaults: Vec<&str> = rows
        .iter()
        .filter(|(_, e)| e["is_default"] == json!(true))
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(defaults, vec!["dev"], "exactly one default, chosen by rule");

    // Distinct keys per environment: one leaked key must not expose the others.
    let keys: HashSet<&str> = rows
        .iter()
        .map(|(_, e)| e["public_key"].as_str().expect("public_key"))
        .collect();
    assert_eq!(keys.len(), 2, "each enrollment holds its own ingest key");

    h.shutdown().await;
}

#[tokio::test]
async fn adding_an_environment_enrolls_every_existing_app_with_its_own_key() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let project_id = create_project_http(&h, &f.bearer, f.org_id, "fan out").await;
    let app_a = create_app_http(&h, &f.bearer, project_id, "app a").await;
    let app_b = create_app_http(&h, &f.bearer, project_id, "app b").await;

    assert_eq!(
        h.post_status(
            &format!("/v1/projects/{project_id}/environments"),
            &f.bearer,
            json!({ "name": "production" }),
        )
        .await,
        200,
        "create production"
    );

    let rows_a = enrollments(&h, &f.bearer, app_a).await;
    let rows_b = enrollments(&h, &f.bearer, app_b).await;
    for (label, rows) in [("app a", &rows_a), ("app b", &rows_b)] {
        assert!(
            rows.iter().any(|(n, _)| n == "production"),
            "{label} must be auto-enrolled when the project gains an environment"
        );
    }

    // The two apps' `production` enrollments are distinct rows with distinct
    // keys, which is the whole reason the credential lives on the enrollment:
    // a key must prove WHICH app an event belongs to, not just which
    // environment.
    let key_a = rows_a
        .iter()
        .find(|(n, _)| n == "production")
        .map(|(_, e)| e["public_key"].as_str().unwrap().to_string())
        .unwrap();
    let key_b = rows_b
        .iter()
        .find(|(n, _)| n == "production")
        .map(|(_, e)| e["public_key"].as_str().unwrap().to_string())
        .unwrap();
    assert_ne!(
        key_a, key_b,
        "two apps in the same environment must not share an ingest key"
    );

    // ...and they name the SAME catalogue entry, or the rename below would not
    // be project-wide.
    let env_a = rows_a
        .iter()
        .find(|(n, _)| n == "production")
        .map(|(_, e)| e["environment_id"].clone())
        .unwrap();
    let env_b = rows_b
        .iter()
        .find(|(n, _)| n == "production")
        .map(|(_, e)| e["environment_id"].clone())
        .unwrap();
    assert_eq!(
        env_a, env_b,
        "both enrollments must point at the one catalogue row"
    );

    // Renaming the catalogue entry is visible from both apps at once — the
    // single-home-for-the-name property this migration exists to establish.
    let env_id = env_a.as_str().unwrap();
    assert_eq!(
        h.patch_status(
            &format!("/v1/environments/{env_id}"),
            &f.bearer,
            json!({ "name": "prod" }),
        )
        .await,
        200,
        "rename production -> prod"
    );
    for (label, app_id) in [("app a", app_a), ("app b", app_b)] {
        let names: Vec<String> = enrollments(&h, &f.bearer, app_id)
            .await
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            names.contains(&"prod".to_string()) && !names.contains(&"production".to_string()),
            "{label} must see the rename — the name has exactly one home"
        );
    }

    h.shutdown().await;
}

#[tokio::test]
async fn retiring_an_environment_is_guarded_then_cascades_to_every_app() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let project_id = create_project_http(&h, &f.bearer, f.org_id, "retire cascade").await;
    let app_a = create_app_http(&h, &f.bearer, project_id, "app a").await;
    let app_b = create_app_http(&h, &f.bearer, project_id, "app b").await;

    let staging_id = {
        let resp = h
            .post(
                &format!("/v1/projects/{project_id}/environments"),
                &f.bearer,
                json!({ "name": "staging" }),
            )
            .await;
        assert_eq!(resp.status().as_u16(), 200, "create staging");
        let body: serde_json::Value =
            serde_json::from_str(&resp.text().await.expect("env body")).expect("env json");
        body["id"].as_str().unwrap().to_string()
    };

    // `dev` is every app's default, so retiring it must be refused with the
    // "still the default" reason rather than silently leaving those apps with
    // nowhere to report.
    let dev_id = {
        let rows = enrollments(&h, &f.bearer, app_a).await;
        rows.iter()
            .find(|(n, _)| n == "dev")
            .map(|(_, e)| e["environment_id"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(
        h.delete_status(&format!("/v1/environments/{dev_id}"), &f.bearer)
            .await,
        409,
        "retiring an environment that apps still default to must be refused"
    );

    // `staging` is nobody's default and is not the last one, so it retires —
    // and takes every app's enrollment with it.
    assert_eq!(
        h.delete_status(&format!("/v1/environments/{staging_id}"), &f.bearer)
            .await,
        200,
        "retire staging"
    );
    for (label, app_id) in [("app a", app_a), ("app b", app_b)] {
        let names: Vec<String> = enrollments(&h, &f.bearer, app_id)
            .await
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(
            !names.contains(&"staging".to_string()),
            "{label} must lose its enrollment when the environment is retired project-wide"
        );
    }
    assert_eq!(
        catalogue(&h, &f.bearer, project_id).await,
        vec!["dev".to_string()],
        "the retired entry leaves the live catalogue"
    );

    // Retiring is idempotent, not an error — the second call finds it already
    // retired and says so without changing anything.
    assert_eq!(
        h.delete_status(&format!("/v1/environments/{staging_id}"), &f.bearer)
            .await,
        200,
        "retiring an already-retired environment is idempotent"
    );

    // And now `dev` is the last one, so the count guard wins over the default
    // guard: "there is nothing to promote" is the more fundamental reason.
    assert_eq!(
        h.delete_status(&format!("/v1/environments/{dev_id}"), &f.bearer)
            .await,
        409,
        "the last environment can never be retired"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_duplicate_environment_name_is_refused_per_project_not_globally() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let p1 = create_project_http(&h, &f.bearer, f.org_id, "project one").await;
    let p2 = create_project_http(&h, &f.bearer, f.org_id, "project two").await;

    assert_eq!(
        h.post_status(
            &format!("/v1/projects/{p1}/environments"),
            &f.bearer,
            json!({ "name": "staging" }),
        )
        .await,
        200,
        "first staging"
    );
    assert_eq!(
        h.post_status(
            &format!("/v1/projects/{p1}/environments"),
            &f.bearer,
            json!({ "name": "staging" }),
        )
        .await,
        409,
        "a duplicate name within one project is refused"
    );
    // Uniqueness is scoped to the project, so a sibling project may use the
    // same name — that is the point of moving the catalogue down from global.
    assert_eq!(
        h.post_status(
            &format!("/v1/projects/{p2}/environments"),
            &f.bearer,
            json!({ "name": "staging" }),
        )
        .await,
        200,
        "the same name in another project is fine"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn rotating_one_apps_key_leaves_its_siblings_alone() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_lifecycle_fixture().await;
    let project_id = create_project_http(&h, &f.bearer, f.org_id, "rotate").await;
    let app_a = create_app_http(&h, &f.bearer, project_id, "app a").await;
    let app_b = create_app_http(&h, &f.bearer, project_id, "app b").await;

    let (enrollment_a, before_a) = {
        let rows = enrollments(&h, &f.bearer, app_a).await;
        let (_, row) = rows.into_iter().find(|(n, _)| n == "dev").unwrap();
        (
            row["id"].as_str().unwrap().to_string(),
            row["public_key"].as_str().unwrap().to_string(),
        )
    };
    let before_b = {
        let rows = enrollments(&h, &f.bearer, app_b).await;
        let (_, row) = rows.into_iter().find(|(n, _)| n == "dev").unwrap();
        row["public_key"].as_str().unwrap().to_string()
    };

    assert_eq!(
        h.post_status(
            &format!("/v1/app-environments/{enrollment_a}/rotate-key"),
            &f.bearer,
            json!({}),
        )
        .await,
        200,
        "rotate app a's dev key"
    );

    let after_a = {
        let rows = enrollments(&h, &f.bearer, app_a).await;
        let (_, row) = rows.into_iter().find(|(n, _)| n == "dev").unwrap();
        row["public_key"].as_str().unwrap().to_string()
    };
    let after_b = {
        let rows = enrollments(&h, &f.bearer, app_b).await;
        let (_, row) = rows.into_iter().find(|(n, _)| n == "dev").unwrap();
        row["public_key"].as_str().unwrap().to_string()
    };

    assert_ne!(before_a, after_a, "rotation must mint a new key");
    assert_eq!(
        before_b, after_b,
        "rotating one app's key must not disturb a sibling sharing the environment"
    );

    h.shutdown().await;
}

/// Everything the inspector-listing test needs. Built separately from
/// [`EnvScopedFixture`] because no persona there carries `pii:read`, and
/// bolting it on would change the permission set the issue/event-scoping tests
/// above have always exercised.
struct InspectorPolicyFixture {
    org_id: Uuid,
    app_id: Uuid,
    /// The `app_env` policy's target — an ENROLLMENT id, not a catalogue id.
    enrollment_id: Uuid,
    /// `pii:read` at APP scope on `app_id`.
    app_member_token: String,
    /// `pii:read` at PROJECT scope on the app's parent project.
    project_member_token: String,
    /// `pii:read` at ENV scope on `enrollment_id` only.
    env_member_token: String,
    app_policy_id: Uuid,
    env_policy_id: Uuid,
    /// A second app in the same org, with its own policy, that none of the
    /// three members may see. Without it a filter that returns everything
    /// passes every assertion below.
    other_app_policy_id: Uuid,
}

impl TestServer {
    async fn seed_inspector_policy_fixture(&self) -> InspectorPolicyFixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "pii org", &format!("pii-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "pii project",
            &format!("pii-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "pii app",
            &format!("pii-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");
        let enrollment_id = seed_env(
            &mut conn,
            project.id,
            app.id,
            "prod",
            &format!("pk_pii_{suffix}"),
            true,
        )
        .await;

        // The negative control: a sibling app whose policy must stay invisible.
        let other_app = repo::create_app(
            &mut conn,
            project.id,
            "pii other app",
            &format!("pii-other-app-{suffix}"),
            "web",
        )
        .await
        .expect("create other app");

        let role = repo::create_role(
            &mut conn,
            org.id,
            "pii reader",
            "pii:read only",
            json!([perm::PII_READ]),
        )
        .await
        .expect("create pii role");

        let member = |scope_type: &'static str, scope_id: Uuid, tag: &'static str| {
            let email = format!("pii-{tag}-{suffix}@example.test");
            let role_id = role.id;
            let org_id = org.id;
            async move {
                let mut c = self.conn().await;
                let u = repo::create_user(&mut c, &email, "unused-password-hash", "PII Member")
                    .await
                    .expect("create user");
                repo::create_grant(
                    &mut c,
                    NewRoleGrant {
                        org_id,
                        user_id: u.id,
                        role_id,
                        scope_type: scope_type.to_string(),
                        scope_id,
                    },
                )
                .await
                .expect("create grant");
                u.id
            }
        };

        let app_member = member("app", app.id, "app").await;
        let project_member = member("project", project.id, "project").await;
        let env_member = member("env", enrollment_id, "env").await;

        let mut conn = self.conn().await;
        let keys = json!([{"key": "email", "scope": "any"}]);
        let empty = json!([]);
        let rollups = json!(["issues", "event_users"]);
        let at = chrono::NaiveTime::from_hms_opt(3, 0, 0).expect("03:00 is a valid time");
        let policy = |target_type: &'static str, target_id: Uuid| {
            let (keys, empty, rollups) = (&keys, &empty, &rollups);
            NewInspectorPolicy {
                org_id: org.id,
                target_type,
                target_id,
                enabled: true,
                tracked_keys: keys,
                detectors: empty,
                scan_columns: None,
                rollups,
                window_days: 30,
                schedule_enabled: false,
                schedule_days: 0,
                schedule_time: at,
                schedule_tz: "UTC",
                created_by: None,
            }
        };

        let app_policy = repo::create_inspector_policy(&mut conn, policy("app", app.id))
            .await
            .expect("create app policy");
        let env_policy = repo::create_inspector_policy(&mut conn, policy("app_env", enrollment_id))
            .await
            .expect("create app_env policy");
        let other_app_policy =
            repo::create_inspector_policy(&mut conn, policy("app", other_app.id))
                .await
                .expect("create other-app policy");
        drop(conn);

        let jwt = JwtKeys::new(JWT_SECRET, 900);
        let tok = |id: Uuid| jwt.issue_access(id, false, None).expect("issue token").0;

        InspectorPolicyFixture {
            org_id: org.id,
            app_id: app.id,
            enrollment_id,
            app_member_token: tok(app_member),
            project_member_token: tok(project_member),
            env_member_token: tok(env_member),
            app_policy_id: app_policy.id,
            env_policy_id: env_policy.id,
            other_app_policy_id: other_app_policy.id,
        }
    }
}

/// `GET /v1/orgs/{org}/inspector/policies` must list exactly the policies the
/// caller can open by id.
///
/// The two surfaces are guarded by different code — the list filters on
/// `reach_for(PII_READ)`, `get_policy` calls `authorize_policy` — and they
/// drifted apart in the narrowing direction: an app-scoped member could `GET`
/// the `app_env` policy under their own app and never see it listed, and a
/// project-scoped member could open the app policy and never see it listed.
///
/// Silent omission is the dangerous half. An `app_env` policy SUBTRACTS from a
/// coarser policy's scan targets (`sauron_inspector::targets::resolve_targets`),
/// so a member who sees only the app-level policy reads its findings as
/// covering the whole app when an environment underneath was scanned by a
/// different policy — the confident-false-picture failure the inspector exists
/// to prevent. Each assertion below pairs the list against the by-id fetch, so
/// the two can never drift again without this test going red.
#[tokio::test]
async fn the_inspector_policy_list_matches_what_the_caller_can_open_by_id() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let f = h.seed_inspector_policy_fixture().await;
    let path = format!("/v1/orgs/{}/inspector/policies", f.org_id);

    let listed = |body: &serde_json::Value| -> HashSet<Uuid> {
        body.as_array()
            .expect("the policy list is a JSON array")
            .iter()
            .map(|p| {
                p["id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .expect("every listed policy carries a uuid id")
            })
            .collect()
    };

    // -- app-scoped member -------------------------------------------------
    // `authorize_policy`'s `app_env` arm resolves the enrollment to its parent
    // app and calls `authorize_app`, so this member can open BOTH.
    let ids = listed(&h.get_json(&path, &f.app_member_token).await);
    assert!(
        ids.contains(&f.app_policy_id),
        "app-scoped member cannot see their own app's policy: {ids:?}"
    );
    assert!(
        ids.contains(&f.env_policy_id),
        "app-scoped member cannot see the app_env policy under their own app — \
         they will read the app policy's findings as covering the whole app"
    );
    assert!(
        !ids.contains(&f.other_app_policy_id),
        "app-scoped member can see a sibling app's policy"
    );
    for (id, label) in [
        (f.app_policy_id, "app policy"),
        (f.env_policy_id, "app_env policy"),
    ] {
        h.assert_status(
            &format!("/v1/inspector/policies/{id}"),
            &f.app_member_token,
            200,
            &format!("app-scoped member opening the {label} by id"),
        )
        .await;
    }

    // -- project-scoped member ---------------------------------------------
    // `authorize_app` accepts a grant at the app's PARENT PROJECT, so both the
    // app policy and the app_env policy under it are open-able.
    let ids = listed(&h.get_json(&path, &f.project_member_token).await);
    assert!(
        ids.contains(&f.app_policy_id),
        "project-scoped member cannot see an app policy inside their project"
    );
    assert!(
        ids.contains(&f.env_policy_id),
        "project-scoped member cannot see an app_env policy inside their project"
    );
    assert!(
        ids.contains(&f.other_app_policy_id),
        "the sibling app is in the SAME project, so a project grant does reach it"
    );
    h.assert_status(
        &format!("/v1/inspector/policies/{}", f.env_policy_id),
        &f.project_member_token,
        200,
        "project-scoped member opening the app_env policy by id",
    )
    .await;

    // -- env-scoped member -------------------------------------------------
    // Deliberately WIDER than `authorize_policy`: an env grant cannot satisfy
    // `authorize_app` (`grant_applies` compares `Scope::Env` against the
    // check's `env`, which `authorize_app` passes as `None`), but the holder
    // still gets to see that their app has a policy at all. Pinned so the
    // asymmetry is a decision on record rather than an accident.
    let ids = listed(&h.get_json(&path, &f.env_member_token).await);
    assert!(
        ids.contains(&f.env_policy_id),
        "env-scoped member cannot see the policy on their own enrollment"
    );
    assert!(
        ids.contains(&f.app_policy_id),
        "env-scoped member cannot see their app's policy"
    );
    assert!(
        !ids.contains(&f.other_app_policy_id),
        "env-scoped member can see a sibling app's policy"
    );

    // The `project` arm stays strict on purpose: `authorize_project` resolves
    // at `(org, project, None, None)`, which no app- or env-scoped grant can
    // satisfy, so widening it would list rows that 403 on open.
    let mut conn = h.conn().await;
    let project_policy = {
        let keys = json!([{"key": "email", "scope": "any"}]);
        let empty = json!([]);
        let rollups = json!(["issues", "event_users"]);
        let project_id = repo::app_ancestries(&mut conn, &[f.app_id])
            .await
            .expect("app ancestry")
            .first()
            .map(|(_, project_id, _)| *project_id)
            .expect("the app resolves to a project");
        repo::create_inspector_policy(
            &mut conn,
            NewInspectorPolicy {
                org_id: f.org_id,
                target_type: "project",
                target_id: project_id,
                enabled: true,
                tracked_keys: &keys,
                detectors: &empty,
                scan_columns: None,
                rollups: &rollups,
                window_days: 30,
                schedule_enabled: false,
                schedule_days: 0,
                schedule_time: chrono::NaiveTime::from_hms_opt(3, 0, 0).expect("valid time"),
                schedule_tz: "UTC",
                created_by: None,
            },
        )
        .await
        .expect("create project policy")
    };
    drop(conn);

    let ids = listed(&h.get_json(&path, &f.app_member_token).await);
    assert!(
        !ids.contains(&project_policy.id),
        "an app-scoped grant must not list a PROJECT policy it cannot open"
    );
    h.assert_status(
        &format!("/v1/inspector/policies/{}", project_policy.id),
        &f.app_member_token,
        403,
        "app-scoped member opening a project policy by id",
    )
    .await;

    // The enrollment id is the app_env policy's target: if this ever became a
    // catalogue id the two lookups would silently stop matching.
    let mut conn = h.conn().await;
    let resolved = repo::app_id_for_enrollment(&mut conn, f.enrollment_id)
        .await
        .expect("resolve enrollment");
    drop(conn);
    assert_eq!(
        resolved,
        Some(f.app_id),
        "the app_env policy's target must be an ENROLLMENT id"
    );

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Devices grouped by model+OS (Task 3): the HTTP route.
// ---------------------------------------------------------------------------

/// The grouped Devices read is a new route on the same `EVENT_READ` +
/// `environment_id` contract as `/devices`. Both halves matter: a missing
/// permission gate and a silently-ignored `environment_id` both present as a
/// perfectly normal 200, so status alone cannot tell them from correct
/// behaviour — the env assertion below compares two environments' bodies.
#[tokio::test]
async fn device_groups_is_permission_gated_and_env_scoped() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;
    let granted = f.granted_env;
    let other = f.other_env;

    // The member holds one env-scoped grant, on `granted_env` only.
    let ok = srv
        .get_status(
            &format!("/v1/apps/{app}/device-groups?environment_id={granted}&since_days=3650"),
            &f.member_token,
        )
        .await;
    assert_eq!(ok, 200, "device-groups must be readable in the granted env");

    let denied = srv
        .get_status(
            &format!("/v1/apps/{app}/device-groups?environment_id={other}&since_days=3650"),
            &f.member_token,
        )
        .await;
    assert_eq!(
        denied, 403,
        "device-groups must refuse an environment the member holds no grant on"
    );

    // The route reads `environment_id` from the raw query string. If that
    // wiring were missing, both requests would return the same app-wide body
    // and the status assertions above would still pass.
    let granted_body = srv
        .get_json(
            &format!("/v1/apps/{app}/device-groups?environment_id={granted}&since_days=3650"),
            &f.owner_token,
        )
        .await;
    let other_body = srv
        .get_json(
            &format!("/v1/apps/{app}/device-groups?environment_id={other}&since_days=3650"),
            &f.owner_token,
        )
        .await;
    assert!(granted_body.is_array() && other_body.is_array());
    assert_ne!(
        granted_body, other_body,
        "two environments must not return identical grouped bodies — the \
         fixture seeds device activity in granted_env only"
    );

    srv.shutdown().await;
}

/// The drill-down sentinel. Without `group=1` the four descriptor parameters
/// are ignored entirely, which is what keeps every existing `/devices` caller
/// working unchanged.
#[tokio::test]
async fn devices_group_filter_applies_only_behind_the_sentinel() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    let unfiltered = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650"),
            &f.owner_token,
        )
        .await;

    // Same descriptor parameters, no sentinel: must be byte-identical to the
    // unfiltered read.
    let ignored = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650&model=no-such-model"),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        unfiltered, ignored,
        "without group=1 the descriptor params must not filter anything"
    );

    // With the sentinel, a model that matches nothing must return nothing.
    // On its own this is a weak check: an all-NULL `DeviceGroupKey`, or one
    // built with `family`/`model` transposed, also returns 0 rows against the
    // fixture's device (all three of its non-NULL descriptors are distinct
    // strings, so a wrong mapping fails to match it for the wrong reason just
    // as reliably as this deliberately-nonexistent model does). The positive
    // assertions below are what actually pin the mapping.
    let filtered = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650&group=1&model=no-such-model"),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        filtered.as_array().map(|a| a.len()),
        Some(0),
        "group=1 with an unmatched model must return an empty list"
    );

    // --- positive drill-down: the fixture's device, matched by its real ----
    // --- family/model/os_name, os_version left out (it is NULL) ------------
    let exact_path = format!(
        "/v1/apps/{app}/devices?since_days=3650&group=1&family=env-scoping-family&model=env-scoping-model&os_name=EnvScopingOS"
    );
    let exact = srv.get_json(&exact_path, &f.owner_token).await;
    let exact_rows = exact
        .as_array()
        .unwrap_or_else(|| panic!("GET {exact_path}: expected a JSON array, got {exact}"));
    assert_eq!(
        exact_rows.len(),
        1,
        "the fixture device's real family/model/os_name (os_version omitted, \
         which is NULL for this device) must return exactly that one device: {exact_rows:?}"
    );
    assert_eq!(
        exact_rows[0]["device_key"].as_str(),
        Some(f.device_key.as_str()),
        "the one row the exact descriptor combination returns must be the \
         fixture's own device: {exact_rows:?}"
    );

    // Transposing family and model must NOT still match — this rules out a
    // `DeviceGroupKey` built with the two fields swapped, which the plain
    // "unmatched model" check above cannot distinguish from a correct
    // mapping (both return 0 rows against this fixture either way).
    let transposed_path = format!(
        "/v1/apps/{app}/devices?since_days=3650&group=1&family=env-scoping-model&model=env-scoping-family&os_name=EnvScopingOS"
    );
    let transposed = srv.get_json(&transposed_path, &f.owner_token).await;
    assert_eq!(
        transposed.as_array().map(|a| a.len()),
        Some(0),
        "swapping family and model in the query must not match the fixture \
         device: {transposed:?}"
    );

    // --- absent vs. present-but-empty must NOT be equivalent for the same --
    // --- field ---------------------------------------------------------------
    // `os_version` is NULL for the fixture device (see
    // `EnvScopedFixture::device_key`'s doc comment for why that, not a real
    // value, is what this needs). Omitting the parameter maps to `None` ->
    // "os_version IS NULL", which matches (`exact_rows` above, computed with
    // `os_version` absent). Sending it present-but-empty must instead map to
    // `Some("")` -> "os_version = ''", which does NOT match a NULL column —
    // if a future `Query` extractor swap ever collapsed the two wire shapes
    // together (the exact defect class this file's module docs describe for
    // `environment_id`), this would silently start matching too.
    let empty_os_version_path = format!(
        "/v1/apps/{app}/devices?since_days=3650&group=1&family=env-scoping-family&model=env-scoping-model&os_name=EnvScopingOS&os_version="
    );
    let empty_os_version = srv.get_json(&empty_os_version_path, &f.owner_token).await;
    assert_eq!(
        empty_os_version.as_array().map(|a| a.len()),
        Some(0),
        "os_version= (present, empty) must filter to the literal empty string, \
         not NULL, so it must NOT match the fixture device: {empty_os_version:?}"
    );
    assert_ne!(
        exact, empty_os_version,
        "omitting os_version and sending it empty must not be equivalent"
    );

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// Devices sorting (S2c slice 3, Task 2): the `sort` query parameter.
//
// Lives here rather than in a file of its own so it reuses this suite's
// spawned-binary harness and its seeded app; the sort whitelist is enforced on
// the same two routes the section above already drives.
// ---------------------------------------------------------------------------

/// Both device lists build their ORDER BY by `format!`, so an unlisted `sort`
/// value that reached the SQL would be injection. `parse_sort` refuses it with
/// a 400 before any SQL is assembled — and the 400 is itself the proof the
/// parameter is wired at all: an ignored `sort` would return 200 here and the
/// caller would never learn their ordering was silently dropped.
#[tokio::test]
async fn a_sort_column_from_the_caller_never_reaches_the_sql() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    // `last_seen; DROP TABLE devices`, percent-encoded.
    let injected = "last_seen%3B%20DROP%20TABLE%20devices";
    for route in ["devices", "device-groups"] {
        let status = srv
            .get_status(
                &format!("/v1/apps/{app}/{route}?sort={injected}"),
                &f.owner_token,
            )
            .await;
        assert_eq!(
            status, 400,
            "/{route}: an unlisted sort column must be refused, not interpolated"
        );
    }

    // And the table is still there.
    for route in ["devices", "device-groups"] {
        assert_eq!(
            srv.get_status(&format!("/v1/apps/{app}/{route}"), &f.owner_token)
                .await,
            200,
            "/{route} must still be readable after the refused sort"
        );
    }

    // A column that belongs to the OTHER list is refused too — the two
    // whitelists are separate on purpose (`distinct_id` has no meaning for a
    // group, `device_count` none for a device), and sharing one would emit SQL
    // naming a column the query does not select.
    assert_eq!(
        srv.get_status(
            &format!("/v1/apps/{app}/device-groups?sort=distinct_id"),
            &f.owner_token
        )
        .await,
        400,
        "`distinct_id` is a flat-list column and must not be accepted for groups"
    );
    assert_eq!(
        srv.get_status(
            &format!("/v1/apps/{app}/devices?sort=device_count"),
            &f.owner_token
        )
        .await,
        400,
        "`device_count` is a grouped-list column and must not be accepted for devices"
    );

    srv.shutdown().await;
}

/// Every whitelisted column, both directions, must produce SQL Postgres
/// accepts. A `match` arm naming a column the outer select does not expose
/// (`d.foo`, or an aggregate alias spelled wrong) is not a compile error and
/// no unit test on `SortSpec` can see it — it surfaces only as a 500 from a
/// real query, which is exactly what this asserts is absent.
#[tokio::test]
async fn every_whitelisted_device_sort_produces_valid_sql() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    for (route, columns) in [
        (
            "devices",
            &[
                "last_seen",
                "family",
                "os_name",
                "browser",
                "distinct_id",
                "sessions_count",
                "events_count",
                "errors_count",
            ][..],
        ),
        (
            "device-groups",
            &[
                "last_seen",
                "family",
                "os_name",
                "device_count",
                "sessions_count",
                "events_count",
                "errors_count",
            ][..],
        ),
    ] {
        for column in columns {
            for spec in [column.to_string(), format!("-{column}")] {
                let path = format!("/v1/apps/{app}/{route}?since_days=3650&sort={spec}");
                let (status, body) = srv.get_status_and_body(&path, &f.owner_token).await;
                assert_eq!(status, 200, "GET {path} returned {status}: {body}");
            }
        }
    }

    // The environment-scoped read selects `last_seen`/`events_count`/
    // `errors_count`/`last_distinct_id` from the LATERALs instead of from the
    // `devices` columns, so it is a different select list and a different set
    // of orderable names. Covered separately or not at all.
    let granted = f.granted_env;
    for column in ["last_seen", "distinct_id", "events_count", "sessions_count"] {
        let path = format!(
            "/v1/apps/{app}/devices?since_days=3650&environment_id={granted}&sort={column}"
        );
        let (status, body) = srv.get_status_and_body(&path, &f.owner_token).await;
        assert_eq!(status, 200, "GET {path} returned {status}: {body}");
    }

    srv.shutdown().await;
}

/// Three devices whose `browser`, `os_name` and `events_count` orderings are
/// each DIFFERENT from the other two, so no single expected sequence can be
/// produced by sorting on the wrong one of the three.
///
/// | key | browser | os_name | events |
/// |---|---|---|---|
/// | `sortprobe-a` | `Chrome` | `Alpha`   | 1 |
/// | `sortprobe-b` | `Brave`  | `Charlie` | 2 |
/// | `sortprobe-c` | `Amber`  | `Bravo`   | 3 |
///
/// descending by browser → a, b, c · by os_name → b, c, a · by events → c, b, a.
async fn seed_sort_probe_devices(srv: &TestServer, app_id: Uuid) -> Vec<String> {
    let mut conn = srv.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    // `chrono::Duration`, spelled out: this file's bare `Duration` is
    // `std::time::Duration`, which cannot be subtracted from a `DateTime`.
    let at = Utc::now() - chrono::Duration::seconds(30);
    let mut keys = Vec::new();
    for (n, browser, os_name, events) in [
        ("a", "Chrome", "Alpha", 1i64),
        ("b", "Brave", "Charlie", 2),
        ("c", "Amber", "Bravo", 3),
    ] {
        let key = format!("sortprobe-{suffix}-{n}");
        repo::bump_device(
            &mut conn,
            app_id,
            &key,
            Some("SortProbe"),
            Some("SortProbeModel"),
            Some(os_name),
            Some("1"),
            None,
            Some(browser),
            None,
            at,
            events,
            0,
        )
        .await
        .expect("bump_device");
        keys.push(key);
    }
    drop(conn);
    keys
}

/// End-to-end: a non-default column must come back in the requested order
/// through the real route, in both directions.
///
/// The unit tests in `routes::devices` pin the name→column mapping; this pins
/// the WIRING — that `?sort=` actually reaches `list_devices`' ORDER BY and
/// that the ORDER BY is the one asked for. Neither a 200-only assertion nor a
/// repo-level test using the test file's own copy of the mapping can see a
/// route that resolves `browser` to `d.os_name`: this fixture makes those two
/// orderings different on purpose, so a mis-map produces the wrong sequence
/// rather than a wrong-but-identical one.
#[tokio::test]
async fn a_non_default_sort_column_orders_the_response_through_the_route() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;
    let keys = seed_sort_probe_devices(&srv, app).await;
    let (a, b, c) = (keys[0].clone(), keys[1].clone(), keys[2].clone());

    // Only the three probe devices, in response order — the fixture's own
    // device is filtered out rather than positioned, so this asserts relative
    // order and stays independent of where NULLs land.
    async fn probe_order(
        srv: &TestServer,
        token: &str,
        path: &str,
        keys: &[String],
    ) -> Vec<String> {
        let body = srv.get_json(path, token).await;
        body.as_array()
            .unwrap_or_else(|| panic!("GET {path}: expected an array, got {body}"))
            .iter()
            .filter_map(|r| r["device_key"].as_str().map(str::to_owned))
            .filter(|k| keys.contains(k))
            .collect()
    }

    for (sort, expected) in [
        ("browser", vec![&a, &b, &c]),
        ("-browser", vec![&c, &b, &a]),
        ("os_name", vec![&b, &c, &a]),
        ("-os_name", vec![&a, &c, &b]),
        ("events_count", vec![&c, &b, &a]),
        ("-events_count", vec![&a, &b, &c]),
    ] {
        let path = format!("/v1/apps/{app}/devices?since_days=3650&sort={sort}");
        let got = probe_order(&srv, &f.owner_token, &path, &keys).await;
        let want: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            got, want,
            "?sort={sort} returned the wrong order — the route's `match` arm \
             may name a different column than the one requested"
        );
    }

    srv.shutdown().await;
}

// ===========================================================================
// Ingest failure recovery — the deployment-admin boundary and the four routes
// ===========================================================================

/// The whole surface, over real HTTP, through the real router.
///
/// The unit tests below `sauron-db` prove the SQL is right and the classifier
/// tests prove the policy is right, but neither can see whether a handler
/// actually *calls* `require_deployment_admin`, whether the route table wires
/// the right method to the right handler, or whether the keyset cursor survives
/// a round trip through `Query`. Those are exactly the defects that ship green.
///
/// `require_deployment_admin` means org:manage in EVERY org. This database has
/// exactly one org, so one grant satisfies it — and the second user, holding a
/// role WITHOUT org:manage, is the negative case.
#[tokio::test]
async fn ingest_failures_require_deployment_admin_and_round_trip() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let suffix = Uuid::new_v4().simple().to_string();

    let (admin_id, outsider_id, failure_id) = {
        let mut conn = srv.conn().await;
        let org = repo::create_org(
            &mut conn,
            "ingest-failure org",
            &format!("ingest-failure-org-{suffix}"),
        )
        .await
        .expect("create org");

        let admin = repo::create_user(
            &mut conn,
            &format!("if-admin-{suffix}@example.test"),
            "unused-password-hash",
            "Deployment Admin",
        )
        .await
        .expect("create admin");
        let admin_role = repo::create_role(
            &mut conn,
            org.id,
            "deployment admin",
            "org:manage everywhere",
            json!([perm::ORG_MANAGE]),
        )
        .await
        .expect("create admin role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: admin.id,
                role_id: admin_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant org:manage");

        // Authenticated, but without org:manage — the shape that must 403.
        let outsider = repo::create_user(
            &mut conn,
            &format!("if-outsider-{suffix}@example.test"),
            "unused-password-hash",
            "Ordinary Member",
        )
        .await
        .expect("create outsider");
        let reader_role = repo::create_role(
            &mut conn,
            org.id,
            "reader",
            "no org:manage",
            json!([perm::EVENT_READ]),
        )
        .await
        .expect("create reader role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: outsider.id,
                role_id: reader_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant reader");

        // Two occurrences of one failure, so the response must show a group of
        // two rather than two groups of one.
        let mut id = Uuid::nil();
        for i in 0..2 {
            id = repo::record_ingest_failure(
                &mut conn,
                &sauron_db::models::NewIngestFailure {
                    fingerprint: &format!("http-fp-{suffix}"),
                    error_kind: "decode",
                    error_message: "expected value at line 1 column 1",
                    org_id: Some(org.id),
                    project_id: None,
                    app_id: None,
                },
                Some(&json!({ "seq": i })),
                0,
                100,
            )
            .await
            .expect("record failure")
            .id;
        }
        (admin.id, outsider.id, id)
    };

    let keys = JwtKeys::new(JWT_SECRET, 900);
    let (admin_token, _) = keys
        .issue_access(admin_id, false, None)
        .expect("issue admin token");
    let (outsider_token, _) = keys
        .issue_access(outsider_id, false, None)
        .expect("issue outsider token");

    // --- the RBAC boundary, on every one of the four routes ----------------
    //
    // Checked per route, not once: a guard is easy to add to three handlers
    // and forget on the fourth, and the forgotten one is always the mutating
    // one.
    for (method, path) in [
        ("GET", "/v1/admin/ingest-failures".to_string()),
        (
            "GET",
            format!("/v1/admin/ingest-failures/{failure_id}/payloads"),
        ),
        (
            "POST",
            format!("/v1/admin/ingest-failures/{failure_id}/retry"),
        ),
        ("DELETE", format!("/v1/admin/ingest-failures/{failure_id}")),
    ] {
        let status = match method {
            "GET" => srv.get_status(&path, &outsider_token).await,
            "POST" => srv.post_status(&path, &outsider_token, json!({})).await,
            _ => srv.delete_status(&path, &outsider_token).await,
        };
        assert_eq!(
            status, 403,
            "{method} {path} must refuse a caller without org:manage"
        );
    }

    // --- the list, as the admin ---------------------------------------------
    let body = srv
        .get_json("/v1/admin/ingest-failures", &admin_token)
        .await;
    let rows = body["failures"].as_array().expect("failures array");
    assert_eq!(rows.len(), 1, "two occurrences must fold into ONE group");
    assert_eq!(rows[0]["occurrences"], 2);
    assert_eq!(rows[0]["retained"], 2);
    assert_eq!(rows[0]["dropped"], 0);
    assert_eq!(rows[0]["error_kind"], "decode");
    assert_eq!(rows[0]["status"], "failed");

    // A filter that matches nothing must return nothing, not everything — the
    // classic `($1 IS NULL OR col = $1)` inversion.
    let none = srv
        .get_json(
            "/v1/admin/ingest-failures?error_kind=db_deadlock",
            &admin_token,
        )
        .await;
    assert_eq!(none["failures"].as_array().unwrap().len(), 0);

    // --- payloads ------------------------------------------------------------
    let payloads = srv
        .get_json(
            &format!("/v1/admin/ingest-failures/{failure_id}/payloads"),
            &admin_token,
        )
        .await;
    assert_eq!(payloads.as_array().expect("payload array").len(), 2);

    // --- retry --------------------------------------------------------------
    let retried = srv
        .post(
            &format!("/v1/admin/ingest-failures/{failure_id}/retry"),
            &admin_token,
            json!({}),
        )
        .await;
    assert_eq!(retried.status().as_u16(), 200);
    let retried: serde_json::Value = retried.json().await.expect("retry body");
    assert_eq!(
        retried["requeued"], 2,
        "both retained payloads must re-queue"
    );
    assert_eq!(retried["failed"], 0);
    assert_eq!(retried["unrecoverable"], 0);

    // --- drop, and the audit entry that must precede it ---------------------
    assert_eq!(
        srv.delete_status(
            &format!("/v1/admin/ingest-failures/{failure_id}"),
            &admin_token
        )
        .await,
        200,
    );
    assert_eq!(
        srv.get_status(
            &format!("/v1/admin/ingest-failures/{failure_id}/payloads"),
            &admin_token
        )
        .await,
        200,
        "a dropped group's payload listing is empty, not an error",
    );

    let gone = srv
        .get_json("/v1/admin/ingest-failures", &admin_token)
        .await;
    assert_eq!(gone["failures"].as_array().unwrap().len(), 0);

    // The drop is a hard DELETE, so this row is the ONLY surviving record that
    // those events existed. If it is missing, the deletion is untraceable.
    {
        use diesel::prelude::*;
        use diesel_async::RunQueryDsl;
        let mut conn = srv.conn().await;
        let rows: Vec<(String, serde_json::Value)> = sauron_db::schema::audit_log::table
            .filter(sauron_db::schema::audit_log::action.eq("ingest_failure.drop"))
            .select((
                sauron_db::schema::audit_log::action,
                sauron_db::schema::audit_log::changes,
            ))
            .load(&mut conn)
            .await
            .expect("read audit log");
        assert_eq!(rows.len(), 1, "the drop must leave exactly one audit entry");
        let changes = &rows[0].1;
        assert_eq!(changes["occurrences"]["to"], 2);
        assert_eq!(changes["error_kind"]["to"], "decode");
        // The allowlist guarantee, asserted rather than assumed: these rows are
        // masked copies of real user events, and the audit table is read by org
        // admins and kept forever.
        assert!(
            changes.get("payload").is_none(),
            "an audit entry must NEVER carry the payload it describes"
        );
    }

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// Offset-list sorting (S2c slice 3, Task 3): the `sort` query parameter on
// /persons, /screens, /sessions and /workflows.
//
// Here rather than in a file of its own for the reason the devices section
// above gives — this suite already has the spawned-binary harness and a seeded
// app+token, and `sauron-db` must not depend on the API binary, so the
// injection check cannot live in `crates/sauron-db/tests/offset_sort.rs`.
// ---------------------------------------------------------------------------

/// Every whitelisted `?sort=` value for the four Task 3 lists, by route.
const OFFSET_LIST_SORTS: &[(&str, &[&str])] = &[
    (
        "persons",
        &[
            "last_seen",
            "distinct_id",
            "first_seen",
            "sessions_count",
            "events_count",
            "errors_count",
        ],
    ),
    (
        "screens",
        &[
            "views",
            "screen",
            "events",
            "exceptions",
            "users",
            "avg_dwell_ms",
        ],
    ),
    (
        "sessions",
        &[
            "started_at",
            "distinct_id",
            "device_key",
            "duration_ms",
            "events_count",
            "errors_count",
        ],
    ),
    (
        "workflows",
        &[
            "started",
            "name",
            "completed",
            "cancelled",
            "abandoned",
            "completion_rate",
            "median_duration_ms",
            "p95_duration_ms",
            "users",
            "last_seen",
        ],
    ),
];

/// All four lists build their ORDER BY by `format!` (or, for `/sessions`, by
/// `diesel::dsl::sql`), so an unlisted `sort` value that reached the SQL would
/// be injection. `parse_sort` refuses it with a 400 before any SQL is
/// assembled.
///
/// The 400 is also the only end-to-end proof the parameter is WIRED: a handler
/// that resolved the spec and then forgot to pass it — or never read
/// `q.sort` — returns a perfectly normal 200 here and the caller never learns
/// their ordering was dropped.
#[tokio::test]
async fn a_sort_column_from_the_caller_never_reaches_the_offset_list_sql() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    // `<default>; DROP TABLE sessions`, percent-encoded, with each list's own
    // default as the prefix so the value looks legitimate up to the semicolon.
    for (route, columns) in OFFSET_LIST_SORTS {
        let injected = format!("{}%3B%20DROP%20TABLE%20sessions", columns[0]);
        let status = srv
            .get_status(
                &format!("/v1/apps/{app}/{route}?sort={injected}"),
                &f.owner_token,
            )
            .await;
        assert_eq!(
            status, 400,
            "/{route}: an unlisted sort column must be refused, not interpolated"
        );
    }

    // And every table is still there.
    for (route, _) in OFFSET_LIST_SORTS {
        assert_eq!(
            srv.get_status(&format!("/v1/apps/{app}/{route}"), &f.owner_token)
                .await,
            200,
            "/{route} must still be readable after the refused sort"
        );
    }

    // Each list refuses the OTHER lists' exclusive columns. The whitelists are
    // separate on purpose — `views` means nothing to a session, `duration_ms`
    // nothing to a screen — and sharing one would emit SQL naming a column the
    // query does not select, i.e. a 500.
    for (route, foreign) in [
        ("persons", "views"),
        ("screens", "duration_ms"),
        ("sessions", "views"),
        ("workflows", "duration_ms"),
        // `last_event_at` is a real `sessions` column AND that list's former
        // hard-coded ordering, so accepting it would look entirely reasonable.
        ("sessions", "last_event_at"),
        // `active` is a real alias of `workflow_list`'s select and would
        // produce valid SQL — only the whitelist stops it.
        ("workflows", "active"),
    ] {
        assert_eq!(
            srv.get_status(
                &format!("/v1/apps/{app}/{route}?sort={foreign}"),
                &f.owner_token
            )
            .await,
            400,
            "/{route} must refuse `{foreign}`"
        );
    }

    srv.shutdown().await;
}

/// Every whitelisted column, both directions, must produce SQL Postgres
/// accepts.
///
/// This is the only thing that can catch a `match` arm naming a column the
/// query does not expose — `total_dwell_ms` on the screens list, `users`
/// instead of `unique_users` on workflows, a typo'd alias anywhere. It is not
/// a compile error and no `SortSpec` unit test can see it; it surfaces only as
/// a 500 from a real query, which is exactly what this asserts is absent.
#[tokio::test]
async fn every_whitelisted_offset_list_sort_produces_valid_sql() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    for (route, columns) in OFFSET_LIST_SORTS {
        for column in *columns {
            for spec in [column.to_string(), format!("-{column}")] {
                let path = format!("/v1/apps/{app}/{route}?since_days=365&sort={spec}");
                let (status, body) = srv.get_status_and_body(&path, &f.owner_token).await;
                assert_eq!(status, 200, "GET {path} returned {status}: {body}");
            }
        }
    }

    // The environment-scoped read of /persons selects `first_seen`/`last_seen`
    // from the three LATERALs (`LEAST`/`GREATEST`) instead of from
    // `event_users`, so it is a different select list and a different set of
    // orderable names. Covered separately or not at all. The other three
    // lists' select lists do not vary with scope.
    let granted = f.granted_env;
    for column in ["last_seen", "first_seen", "distinct_id", "sessions_count"] {
        let path = format!("/v1/apps/{app}/persons?environment_id={granted}&sort={column}");
        let (status, body) = srv.get_status_and_body(&path, &f.owner_token).await;
        assert_eq!(status, 200, "GET {path} returned {status}: {body}");
    }

    srv.shutdown().await;
}

/// `devices::detail`'s "recent sessions" panel must stay ordered by
/// `last_event_at DESC` — its behaviour before Slice 3 — and must NOT follow
/// the sessions LIST's new `started_at DESC` default.
///
/// Both consumers call the same `repo::list_sessions`, so the two orderings can
/// only be kept apart by the call site pinning one. `routes::devices`'
/// `the_device_detail_session_panel_pins_last_event_at` asserts the constant;
/// this asserts the handler actually uses it, which the unit test cannot see.
///
/// The fixture is built so the two orderings disagree — a panel served with
/// either one returns the same two sessions, and only their ORDER tells them
/// apart:
///
/// | session | `started_at` | `last_event_at` |
/// |---|---|---|
/// | `early-start` | T-10m | T-1m |
/// | `late-start`  | T-5m  | T-4m |
///
/// `last_event_at DESC` -> early-start, late-start ·
/// `started_at DESC` -> late-start, early-start.
#[tokio::test]
async fn the_device_detail_sessions_panel_stays_ordered_by_last_event_at() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;
    let suffix = Uuid::new_v4().simple().to_string();
    let early = format!("panelprobe-{suffix}-early-start");
    let late = format!("panelprobe-{suffix}-late-start");

    {
        let mut conn = srv.conn().await;
        // `chrono::Duration` spelled out: this file's bare `Duration` is
        // `std::time::Duration`, which cannot be subtracted from a `DateTime`.
        let now = Utc::now();
        // `bump_session` writes `started_at` and `last_event_at` from one bind,
        // so a session whose two timestamps differ needs a second call at the
        // later instant — `LEAST`/`GREATEST` on conflict spread them apart.
        for (session_id, started_min, last_min) in [(&early, 10i64, 1i64), (&late, 5, 4)] {
            for at in [
                now - chrono::Duration::minutes(started_min),
                now - chrono::Duration::minutes(last_min),
            ] {
                repo::bump_session(
                    &mut conn,
                    app,
                    session_id,
                    None,
                    Some(&f.device_key),
                    at,
                    &serde_json::json!({}),
                    None,
                    Some(f.granted_env),
                    None,
                    0,
                    0,
                    0,
                )
                .await
                .expect("bump_session");
            }
        }
    }

    let body = srv
        .get_json(
            &format!("/v1/apps/{app}/device?key={}", f.device_key),
            &f.owner_token,
        )
        .await;
    // Filtered to the two probes rather than asserting the whole array, so an
    // unrelated session seeded later cannot turn this into a flake. The
    // relative order is the whole assertion and survives the filter.
    let served: Vec<String> = body["sessions"]
        .as_array()
        .expect("the device detail carries a `sessions` array")
        .iter()
        .filter_map(|s| s["session_id"].as_str().map(str::to_string))
        .filter(|s| s == &early || s == &late)
        .collect();
    assert_eq!(
        served,
        vec![early.clone(), late.clone()],
        "the device panel must order by last_event_at DESC; the sessions \
         list's `started_at DESC` default would return {:?}",
        vec![late, early]
    );

    srv.shutdown().await;
}
