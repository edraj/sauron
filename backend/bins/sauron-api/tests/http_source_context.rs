//! HTTP-level tests for the `source:read` gate on the four `ErrorEvent`-
//! returning handlers that are NOT the issues surface.
//!
//! `perm::SOURCE_READ` gates de-obfuscated **source code** — the symbolication
//! context lines — and nothing else. On these four routes symbol name, file,
//! `lineno` and `colno` stay visible with `event:read` alone, which the
//! `assert_context_stripped` leg below measures directly rather than inferring
//! from `perm::SOURCE_READ`'s doc comment.
//! `symbolicate::strip_source_context` is what enforces it, and
//! it had exactly two invocation sites, both in `routes/issues.rs`, while these
//! four handlers each returned whole `ErrorEvent` rows — persisted
//! `stacktrace_symbolicated` column, context lines and all — to any caller with
//! `event:read`:
//!
//! | route | handler |
//! |---|---|
//! | `GET /v1/apps/{app}/sessions/{session}` | `sessions::detail` |
//! | `GET /v1/apps/{app}/device?key=` | `devices::detail` |
//! | `GET /v1/apps/{app}/screens/detail?name=` | `screens::detail` |
//! | `GET /v1/apps/{app}/persons/{distinct_id}` | `analytics::person` |
//!
//! All four were measured leaking `context_line`, `pre_context`, `post_context`
//! and `context_start_line` before the gate was added to them, and each test
//! below fails if its handler's `gate_source_context` call is removed.
//!
//! Each test below drives one of them twice over real HTTP — once as a member
//! whose role carries `event:read` but NOT `source:read`, once as a member
//! identical except for holding `source:read` — and asserts that the first sees
//! no context lines while the second does, and that the *rest* of the payload
//! (the event, its frames, its symbol names) is present in both. The ruling on
//! this hole was explicitly the least-disruptive variant: `event:read` keeps
//! returning stacktraces, breadcrumbs and frames; only source lines go.
//!
//! Every test spawns the actual compiled `sauron-api` binary (via Cargo's
//! `CARGO_BIN_EXE_sauron-api`, so it exercises the literal shipped route table
//! in `main.rs`) against a fresh ephemeral database. A unit test cannot see this
//! defect at all: `strip_source_context` was always correct in isolation, and
//! the bug was entirely "which handlers call it".
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset,
//! mirroring the convention in `tests/http_env_scoping.rs`. A skip asserts
//! NOTHING — see [`skipped`], which prints why.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-source-context-test-secret-00000000";

/// The source-context keys `symbolicate::strip_source_context` removes when a
/// caller lacks `source:read`. Kept in lockstep with the seed below so the
/// assertion and the fixture cannot drift apart.
const SOURCE_CONTEXT_KEYS: [&str; 4] = [
    "context_line",
    "pre_context",
    "post_context",
    "context_start_line",
];

/// Distinctive strings planted in the three *string-valued* context fields, so
/// a test can grep the raw response body for leaked source code rather than
/// only checking that a JSON key is absent. (`context_start_line` is a number;
/// its key name is the only thing to assert on.)
const CONTEXT_LINE: &str = "let secret_source_line = 42;";
const PRE_CONTEXT_LINE: &str = "fn secret_pre_context_marker() {";
const POST_CONTEXT_LINE: &str = "} // secret_post_context_marker";

/// **More than one, and that is the whole point.** Each of these four handlers
/// returns a LIST of events — `analytics::person` up to 200 (`analytics.rs`'s
/// `limit.clamp(1, 200)`), `sessions::detail` 500, `devices::detail` 50,
/// `screens::detail` 20 — and the gate strips them in a loop. A one-event
/// fixture cannot tell "strips every event" from "strips `events[0]`".
///
/// Measured 2026-08-08: with ONE seeded event, mutating the gate's
/// `for ev in events.iter_mut()` to `.iter_mut().take(1)` — a live leak of every
/// event after the first — still reported `4 passed; 0 failed`. With three
/// events and the per-index markers below, that same mutation fails. Do not
/// reduce this to 1.
const SEEDED_EVENTS: usize = 3;

/// Per-event marker. Each seeded event carries its own index in every planted
/// string, so a failure names WHICH event leaked rather than only that one did.
fn ev_marker(base: &str, i: usize) -> String {
    format!("{base}#{i}")
}

/// The symbol name / file / line that must survive the gate — this is the half
/// of a symbolicated frame `source:read` deliberately does NOT cover.
const FRAME_FUNCTION: &str = "sourceGateFixtureFrame";
const FRAME_FILENAME: &str = "src/source_gate_fixture.rs";

/// Identity shared by all SEEDED_EVENTS rows and by all four routes: every
/// seeded row is reachable as a session member, a device's error, a screen's
/// exception and a person's error, so the four tests differ only in the URL they
/// call.
const SESSION_ID: &str = "source-gate-session";
const DEVICE_KEY: &str = "source-gate-device";
const SCREEN_NAME: &str = "SourceGateScreen";
const DISTINCT_ID: &str = "source-gate-person";

/// Print why a test asserted nothing. Without this a missing env var is
/// indistinguishable from a pass in `cargo test` output — a failure mode this
/// repo has actually shipped.
fn skipped(test: &str) {
    eprintln!(
        "SKIPPED {test}: TEST_DATABASE_URL and/or TEST_REDIS_URL unset — this test asserted \
         NOTHING. Export both to run it."
    );
}

/// Return `url` with its database (path) segment replaced by `new_db`. Same
/// tiny helper as `tests/http_env_scoping.rs`; see that file for why it is
/// duplicated rather than shared.
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

/// See `tests/http_env_scoping.rs`'s identical helper, including why the
/// issued-port set is needed on top of the probe bind.
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

/// A fresh migrated ephemeral database plus a real spawned `sauron-api`
/// process. Trimmed copy of `tests/http_env_scoping.rs`'s `TestServer` — only
/// the methods these tests call.
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
        // to the uuid. `sauron-db`'s `tests/common::reap_stale_test_databases`
        // parses the first underscore-delimited segment after `sauron_test_` as
        // a timestamp and silently skips any name that fails that parse, so a
        // "sauron_test_sc_<ts>_<uuid>" spelling would leak every database it
        // creates, invisibly to the reaper. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "sc" (2) + 32-hex
        // uuid = 57 bytes, within `validate_db_ident`'s 63-byte cap.
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

    /// GET `path` with `token`, asserting `200`, and return the body BOTH as
    /// parsed JSON and as the raw text. The raw text is what the leak
    /// assertions grep: a `context_line` nested anywhere in the payload —
    /// including inside a variant this test does not know the shape of — shows
    /// up in it.
    ///
    /// Parses via `.text()` + `serde_json::from_str` rather than
    /// `Response::json` because the RAW body is what the leak assertions grep —
    /// `.json()` would discard it and a leak nested in an unknown-shaped variant
    /// could slip past. (An earlier version of this comment claimed `reqwest`
    /// here omits the `json` feature. That is false: `sauron-api`'s
    /// `[dev-dependencies]` enables it and sibling tests such as
    /// `http_orgs.rs:275` call `.json()`. The `.text()` choice is right; that
    /// reason was not.)
    async fn get_ok(&self, path: &str, token: &str, label: &str) -> (Value, String) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path} ({label}) failed: {e}"));
        let status = resp.status();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path} ({label}): read body: {e}"));
        assert_eq!(
            status.as_u16(),
            200,
            "GET {path} ({label}): expected 200, got {status}\nbody: {text}"
        );
        let json = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("GET {path} ({label}): expected JSON: {e}\nbody: {text}"));
        (json, text)
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

/// One org/project/app/environment, [`SEEDED_EVENTS`] symbolicated error events
/// each carrying all four [`SOURCE_CONTEXT_KEYS`] under its own marker index,
/// and two members whose roles differ ONLY in `perm::SOURCE_READ`.
struct Fixture {
    app_id: Uuid,
    /// `event:read` (+ `issue:read`), **no** `source:read`.
    plain_token: String,
    /// Identical, **plus** `source:read`.
    source_token: String,
}

impl TestServer {
    async fn seed(&self) -> Fixture {
        let mut conn = self.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "source-gate org", &format!("sg-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "source-gate project",
            &format!("sg-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "source-gate app",
            &format!("sg-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");

        // One environment, and the app enrolled in it. The event rows below
        // carry its enrollment id, which is what `environment_id` means on a
        // signal row (see `http_env_scoping.rs`'s `seed_env`).
        let env = repo::create_project_environment(&mut conn, project.id, "prod")
            .await
            .expect("create catalogue env");
        let env_id = repo::create_app_environments(
            &mut conn,
            &[NewAppEnvironment {
                app_id: app.id,
                environment_id: env.id,
                public_key: &format!("pk_source_gate_{suffix}"),
                is_default: true,
            }],
        )
        .await
        .expect("enroll app in env")
        .remove(0)
        .id;

        let now = Utc::now();

        // --- the row all four routes reach --------------------------------
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: app.id,
                fingerprint: &format!("sg-fp-{suffix}"),
                type_: "Error",
                title: "source gate fixture issue",
                culprit: "source_gate::fixture",
                level: "error",
                first_seen: now,
                last_seen: now,
                times_seen: 1,
            },
        )
        .await
        .expect("upsert issue");

        // Stored **already symbolicated** with a non-empty frame array, so
        // `symbolicate::symbolicate_with`'s fast path returns it untouched and
        // the ONLY thing that can remove the context keys from a response is
        // `strip_source_context` — i.e. the gate itself. Seeded any other way
        // (empty `stacktrace`, no artifacts) both responses would be identical
        // and the test could not discriminate.
        // SEEDED_EVENTS of them, each with its own marker index — see that
        // const for why one is not enough to guard a per-element gate.
        for i in 0..SEEDED_EVENTS {
            repo::insert_error_event(
                &mut conn,
                NewErrorEvent {
                    id: Uuid::new_v4(),
                    app_id: app.id,
                    environment_id: Some(env_id),
                    issue_id,
                    fingerprint: format!("sg-fp-{suffix}-{i}"),
                    level: "error".into(),
                    message: "source gate fixture error".into(),
                    exception_type: "SourceGateError".into(),
                    exception_value: "seeded".into(),
                    stacktrace: json!([]),
                    breadcrumbs: json!([{ "message": "source gate breadcrumb" }]),
                    context: json!({}),
                    tags: json!({}),
                    release: None,
                    distinct_id: Some(DISTINCT_ID.to_string()),
                    event_user: None,
                    sdk: None,
                    ip_address: None,
                    occurred_at: now,
                    session_id: Some(SESSION_ID.to_string()),
                    device_key: Some(DEVICE_KEY.to_string()),
                    screen: Some(SCREEN_NAME.to_string()),
                    workflow_id: None,
                    workflow_name: None,
                    stacktrace_symbolicated: Some(json!([{
                        "function": ev_marker(FRAME_FUNCTION, i),
                        "filename": FRAME_FILENAME,
                        "lineno": 42,
                        "colno": 5,
                        "context_line": ev_marker(CONTEXT_LINE, i),
                        "pre_context": [ev_marker(PRE_CONTEXT_LINE, i)],
                        "post_context": [ev_marker(POST_CONTEXT_LINE, i)],
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
        }

        // `sessions::detail` 404s unless a `sessions` row exists;
        // `devices::detail` 404s unless a `devices` row does. The error event
        // above is what each one's error list then joins to by
        // `session_id` / `device_key`.
        repo::bump_session(
            &mut conn,
            app.id,
            SESSION_ID,
            Some(DISTINCT_ID),
            Some(DEVICE_KEY),
            now,
            &json!({}),
            None,
            Some(env_id),
            None,
            0,
            1,
        )
        .await
        .expect("bump session");
        repo::bump_device(
            &mut conn,
            app.id,
            DEVICE_KEY,
            Some("Desktop"),
            Some("fixture"),
            Some("Linux"),
            Some("6"),
            Some("x86_64"),
            Some("Firefox"),
            Some(DISTINCT_ID),
            now,
            0,
            1,
        )
        .await
        .expect("bump device");

        // --- two members differing only in source:read --------------------
        let plain_user = repo::create_user(
            &mut conn,
            &format!("sg-plain-{suffix}@example.test"),
            "unused-password-hash",
            "Source Gate Plain",
        )
        .await
        .expect("create plain user");
        let plain_role = repo::create_role(
            &mut conn,
            org.id,
            "source-gate plain role",
            "event/issue read WITHOUT source:read",
            json!([perm::EVENT_READ, perm::ISSUE_READ]),
        )
        .await
        .expect("create plain role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: plain_user.id,
                role_id: plain_role.id,
                scope_type: "app".to_string(),
                scope_id: app.id,
            },
        )
        .await
        .expect("grant plain role at app scope");

        let source_user = repo::create_user(
            &mut conn,
            &format!("sg-source-{suffix}@example.test"),
            "unused-password-hash",
            "Source Gate Source",
        )
        .await
        .expect("create source user");
        let source_role = repo::create_role(
            &mut conn,
            org.id,
            "source-gate source role",
            "event/issue read WITH source:read",
            json!([perm::EVENT_READ, perm::ISSUE_READ, perm::SOURCE_READ]),
        )
        .await
        .expect("create source role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: source_user.id,
                role_id: source_role.id,
                scope_type: "app".to_string(),
                scope_id: app.id,
            },
        )
        .await
        .expect("grant source role at app scope");

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (plain_token, _) = keys
            .issue_access(plain_user.id, false, None)
            .expect("issue plain access token");
        let (source_token, _) = keys
            .issue_access(source_user.id, false, None)
            .expect("issue source access token");

        Fixture {
            app_id: app.id,
            plain_token,
            source_token,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared assertions
// ---------------------------------------------------------------------------

/// Assert the `source:read` holder's response really does carry the source
/// context. This is the half that makes the negative assertion below
/// meaningful: if the fixture stopped producing context lines at all, this
/// fails and the whole test is exposed as vacuous rather than quietly passing.
fn assert_context_present(body: &str, route: &str) {
    for key in SOURCE_CONTEXT_KEYS {
        assert!(
            body.contains(key),
            "{route}: a caller WITH source:read must still receive {key} — if this fails the \
             fixture, not the gate, is broken and the negative case below proves nothing.\n\
             body: {body}"
        );
    }
    // EVERY seeded event, not just the first. If the gate (or the fixture)
    // only ever handled `events[0]`, the source:read leg would still look fine
    // on a one-event fixture — that is the blind spot this loop closes.
    for i in 0..SEEDED_EVENTS {
        for base in [CONTEXT_LINE, PRE_CONTEXT_LINE, POST_CONTEXT_LINE] {
            let line = ev_marker(base, i);
            assert!(
                body.contains(&line),
                "{route}: a caller WITH source:read must still receive the source line \
                 {line:?} (event #{i} of {SEEDED_EVENTS})\nbody: {body}"
            );
        }
    }
}

/// Assert the source context is gone — and that nothing else went with it.
fn assert_context_stripped(body: &str, route: &str) {
    for key in SOURCE_CONTEXT_KEYS {
        assert!(
            !body.contains(key),
            "{route}: a caller WITHOUT source:read must NOT receive {key}\nbody: {body}"
        );
    }
    // PER EVENT. A gate that strips only `events[0]` leaves event #1's source
    // line in the body; asserting the un-suffixed base string alone would also
    // catch that, but naming the index tells you which element leaked.
    for i in 0..SEEDED_EVENTS {
        for base in [CONTEXT_LINE, PRE_CONTEXT_LINE, POST_CONTEXT_LINE] {
            let line = ev_marker(base, i);
            assert!(
                !body.contains(&line),
                "{route}: de-obfuscated source line {line:?} (event #{i} of {SEEDED_EVENTS}) \
                 leaked to a caller without source:read — a per-element gate bug\nbody: {body}"
            );
        }
    }
    // The ruling was the least-disruptive variant: only source LINES go.
    // Symbol name, file, line, the event itself and its breadcrumbs stay.
    //
    // Checking the per-event frame marker also proves the gate DROPS NO EVENTS:
    // all SEEDED_EVENTS frames must still be present, so a gate that filtered
    // events out instead of stripping fields would fail here.
    for i in 0..SEEDED_EVENTS {
        let frame = ev_marker(FRAME_FUNCTION, i);
        assert!(
            body.contains(&frame),
            "{route}: frame {frame:?} (event #{i} of {SEEDED_EVENTS}) must survive the gate — \
             event:read still returns stacktraces and frames, and the gate must strip fields \
             rather than remove events\nbody: {body}"
        );
    }
    for kept in [FRAME_FILENAME, "SourceGateError", "source gate breadcrumb"] {
        assert!(
            body.contains(kept),
            "{route}: {kept:?} must survive the gate — event:read still returns stacktraces, \
             frames and breadcrumbs; only source lines are removed\nbody: {body}"
        );
    }
}

/// Both legs of one route in one place, so a handler cannot be tested only on
/// the leg that happens to pass.
async fn assert_gate(srv: &TestServer, fx: &Fixture, path: &str, route: &str) {
    let (_, with_source) = srv.get_ok(path, &fx.source_token, "with source:read").await;
    assert_context_present(&with_source, route);

    let (_, without_source) = srv
        .get_ok(path, &fx.plain_token, "without source:read")
        .await;
    assert_context_stripped(&without_source, route);
}

// ---------------------------------------------------------------------------
// One test per handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_detail_gates_source_context() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("sessions_detail_gates_source_context");
        return;
    };
    let fx = srv.seed().await;

    assert_gate(
        &srv,
        &fx,
        &format!("/v1/apps/{}/sessions/{SESSION_ID}", fx.app_id),
        "sessions::detail",
    )
    .await;

    srv.shutdown().await;
}

#[tokio::test]
async fn devices_detail_gates_source_context() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("devices_detail_gates_source_context");
        return;
    };
    let fx = srv.seed().await;

    assert_gate(
        &srv,
        &fx,
        &format!("/v1/apps/{}/device?key={DEVICE_KEY}", fx.app_id),
        "devices::detail",
    )
    .await;

    srv.shutdown().await;
}

#[tokio::test]
async fn screens_detail_gates_source_context() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("screens_detail_gates_source_context");
        return;
    };
    let fx = srv.seed().await;

    assert_gate(
        &srv,
        &fx,
        &format!("/v1/apps/{}/screens/detail?name={SCREEN_NAME}", fx.app_id),
        "screens::detail",
    )
    .await;

    srv.shutdown().await;
}

#[tokio::test]
async fn analytics_person_gates_source_context() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("analytics_person_gates_source_context");
        return;
    };
    let fx = srv.seed().await;

    assert_gate(
        &srv,
        &fx,
        &format!("/v1/apps/{}/persons/{DISTINCT_ID}", fx.app_id),
        "analytics::person",
    )
    .await;

    srv.shutdown().await;
}
