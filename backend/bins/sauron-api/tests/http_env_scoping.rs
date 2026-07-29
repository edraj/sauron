//! HTTP-level regression test for the `?environment_id=` empty-value scoping
//! bug (S2 Task 10, `.superpowers/sdd/s2-task-10-empty-value-fix.md`).
//!
//! A `parse_env` unit test cannot see this bug: `routes/scope.rs`'s
//! `malformed_is_rejected_not_widened` test was green the entire time the bug
//! shipped, because the defect was never in `parse_env` — it was in *which*
//! `Query` extractor a handler happened to import upstream of it
//! (`axum_extra::extract::Query`'s codec silently turns `?environment_id=`
//! into "absent" for an `Option<String>` field; `axum::extract::Query`'s does
//! not). Only a test that goes through the real axum router, over real HTTP,
//! can see that. This is that test: it spawns the actual compiled
//! `sauron-api` binary (via Cargo's `CARGO_BIN_EXE_sauron-api`, so it is
//! testing the literal shipped artifact and its literal route table in
//! `main.rs`, not a hand-assembled subset) and drives it with `reqwest`.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is
//! unset, mirroring `sauron-db`'s own integration-test convention (see
//! `crates/sauron-db/tests/common/mod.rs`) — this repo's tests run against a
//! live stack by choice, opted into by exporting the variable, rather than
//! against a mock. `TEST_DATABASE_URL` is a *maintenance* Postgres URL from
//! which an ephemeral, randomly-named, migrated database is created for this
//! test alone and dropped again at the end. `TEST_REDIS_URL` is a Redis the
//! spawned `sauron-api` process can reach — `sauron-api` requires one to
//! start at all (auth-adjacent bookkeeping this test never itself touches).

use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use sauron_auth::JwtKeys;
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;

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

/// GET `path` against the running server with the given bearer token, and
/// assert its status code. `label` names the case in the panic message
/// (`path` alone doesn't say whether this was the "absent" / "empty" /
/// "malformed" / "valid" leg of a group).
async fn assert_status(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    expected: u16,
    label: &str,
) {
    let resp = client
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap_or_else(|e| panic!("request to {path} ({label}) failed: {e}"));
    let status = resp.status().as_u16();
    assert_eq!(
        status, expected,
        "GET {path} ({label}): expected {expected}, got {status}"
    );
}

#[tokio::test]
async fn empty_environment_id_returns_400_over_http_not_all_environments() {
    let Some(admin_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL unset — skipping http_env_scoping");
        return;
    };
    let Some(redis_url) = std::env::var("TEST_REDIS_URL").ok() else {
        eprintln!("TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };

    // --- provision an ephemeral, migrated database --------------------------
    let db_name = format!(
        "sauron_test_http_{}_{}",
        chrono::Utc::now().timestamp(),
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

    // --- seed one org -> project -> app -> environment, one user with just --
    // --- enough grant to read this app's issues/events -----------------------
    let suffix = Uuid::new_v4().simple().to_string();
    let (app_id, env_id, user_id) = {
        let mut conn = sauron_db::conn(&pool).await.expect("checkout");
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
        let env = repo::create_environment(
            &mut conn,
            app.id,
            "prod",
            &format!("pk_http_scoping_{suffix}"),
            true,
        )
        .await
        .expect("create environment");
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
            json!([sauron_auth::perm::EVENT_READ, sauron_auth::perm::ISSUE_READ,]),
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
        (app.id, env.id, user.id)
    };

    // --- mint an access token the spawned server will accept ---------------
    let keys = JwtKeys::new(JWT_SECRET, 900);
    let (token, _exp) = keys
        .issue_access(user_id, false)
        .expect("issue access token");

    // --- spawn the real, compiled sauron-api binary -------------------------
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

    // --- the actual regression: ?environment_id= must 400 everywhere it ----
    // --- appears, exactly like a malformed value, on every scoped read ------
    let bearer = token.as_str();

    // Group 1: routes that were broken (axum_extra::extract::Query) — proof
    // that removing the collapse fixes overview/issues/events-list, plus a
    // cross-tier timeseries endpoint whose reject-check had the same root
    // cause.
    for (path_prefix, group) in [
        (format!("/v1/apps/{app_id}/overview"), "overview"),
        (format!("/v1/apps/{app_id}/issues"), "issues"),
        (format!("/v1/apps/{app_id}/events/list"), "events/list"),
    ] {
        assert_status(
            &client,
            &base,
            &path_prefix,
            bearer,
            200,
            &format!("{group} absent"),
        )
        .await;
        assert_status(
            &client,
            &base,
            &format!("{path_prefix}?environment_id="),
            bearer,
            400,
            &format!("{group} empty"),
        )
        .await;
        assert_status(
            &client,
            &base,
            &format!("{path_prefix}?environment_id=not-a-uuid"),
            bearer,
            400,
            &format!("{group} malformed"),
        )
        .await;
        assert_status(
            &client,
            &base,
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
    assert_status(&client, &base, &ts_prefix, bearer, 200, "timeseries absent").await;
    assert_status(
        &client,
        &base,
        &format!("{ts_prefix}&environment_id="),
        bearer,
        400,
        "timeseries empty",
    )
    .await;
    assert_status(
        &client,
        &base,
        &format!("{ts_prefix}&environment_id=not-a-uuid"),
        bearer,
        400,
        "timeseries malformed",
    )
    .await;
    assert_status(
        &client,
        &base,
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
    assert_status(
        &client,
        &base,
        &sessions_prefix,
        bearer,
        200,
        "sessions absent",
    )
    .await;
    assert_status(
        &client,
        &base,
        &format!("{sessions_prefix}?environment_id="),
        bearer,
        400,
        "sessions empty",
    )
    .await;
    assert_status(
        &client,
        &base,
        &format!("{sessions_prefix}?environment_id=not-a-uuid"),
        bearer,
        400,
        "sessions malformed",
    )
    .await;
    assert_status(
        &client,
        &base,
        &format!("{sessions_prefix}?environment_id={env_id}"),
        bearer,
        200,
        "sessions valid uuid",
    )
    .await;

    // --- teardown ------------------------------------------------------------
    let _ = child.kill().await;
    let _ = child.wait().await;
    sauron_db::drop_database(&admin_url, &db_name)
        .await
        .expect("drop ephemeral test database");
}
