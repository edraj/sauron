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
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant,
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
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
        sauron_db::create_database(&admin_url, &db_name)
            .await
            .expect("create ephemeral test database");
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::run_pending_migrations(&db_url)
            .await
            .expect("run migrations on ephemeral test database");
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
/// `other_env` is a sibling neither member holds any grant on.
///
/// The two members differ *only* in whether their role carries
/// `perm::SOURCE_READ`, so the source-code gate (fix round 1) has a genuine
/// positive and negative case at the same env scope rather than at two
/// different scopes.
struct EnvScopedFixture {
    org_id: Uuid,
    app_id: Uuid,
    granted_env: Uuid,
    other_env: Uuid,
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

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys
            .issue_access(owner.id, false)
            .expect("issue owner access token");
        let (member_token, _) = keys
            .issue_access(member.id, false)
            .expect("issue member access token");
        let (source_member_token, _) = keys
            .issue_access(source_member.id, false)
            .expect("issue source_member access token");
        let (nav_member_token, _) = keys
            .issue_access(nav_member.id, false)
            .expect("issue nav_member access token");
        let (org_owner_token, _) = keys
            .issue_access(org_owner.id, false)
            .expect("issue org_owner access token");

        EnvScopedFixture {
            org_id: org.id,
            app_id: app.id,
            granted_env,
            other_env,
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
        .issue_access(user_id, false)
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
    let m = member_all
        .as_array()
        .expect("issues response is a JSON array")
        .len();
    let o = owner_all
        .as_array()
        .expect("issues response is a JSON array")
        .len();
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
            .issue_access(user.id, false)
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
