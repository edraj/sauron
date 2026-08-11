//! HTTP-level tests for the two permission gates layered over a read of an
//! `ErrorEvent`: the **body** gate (`issue:read` + `event:read`) and the
//! **source-context** gate (`source:read`).
//!
//! The file is named for the second because it was written for it. The first
//! arrived later — reversing the ruling this header used to state — and shares
//! this fixture rather than paying a second time for a spawned binary, a
//! migrated database and a symbolicated event row.
//!
//! ## The body gate
//!
//! `issue:read` is the COARSE gate: the issue list and everything issue-level
//! on it — title, culprit, fingerprint, level, counts. `event:read` is
//! ADDITIONALLY required to see an event **body**: `stacktrace`,
//! `stacktrace_symbolicated`, `breadcrumbs`, `context`, `contexts`, `extra`,
//! `tags`, `sdk`, `debug_meta`, `event_user`, `ip_address`. A body requires
//! BOTH; either permission alone yields the occurrence *shell* — when it
//! happened, on which release, session, device, screen and person — and no
//! payload.
//!
//! **This supersedes the ruling this file used to record.** The previous
//! header read: "the ruling on this hole was explicitly the least-disruptive
//! variant: `event:read` keeps returning stacktraces, breadcrumbs and frames;
//! only source lines go." That variant lost. A role holding one half of the
//! pair was reading crash payloads — request bodies, user identities,
//! dev-supplied `extra` — that neither half confers on its own. The tests below
//! now assert the opposite of what that sentence promised.
//!
//! `symbolicate::gate_event_body` enforces it. Six handlers reach a body and
//! each authorizes on only ONE of the pair, so each had to remember one call —
//! the identical shape as the `source:read` mistake recorded next.
//!
//! ## The source-context gate
//!
//! `perm::SOURCE_READ` gates de-obfuscated **source code** — the symbolication
//! context lines — and nothing else. Symbol name, file, `lineno` and `colno`
//! stay visible to a caller who holds the body pair but not `source:read`,
//! which the `assert_context_stripped` leg below measures directly rather than
//! inferring from `perm::SOURCE_READ`'s doc comment.
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
//! ## Personas
//!
//! Four members on one app, differing only in permissions, so every assertion
//! below is attributable to a permission and nothing else:
//!
//! | token | role | what it pins |
//! |---|---|---|
//! | `issue_only_token` | `issue:read` | coarse metadata yes, body no |
//! | `event_only_token` | `event:read` | signal routes reachable, body no |
//! | `plain_token` | both | full body, no source lines |
//! | `source_token` | both + `source:read` | full body WITH source lines |
//!
//! Every test drives its route from at least two of them and asserts both
//! directions, so a handler cannot be tested only on the leg that happens to
//! pass — and so a gate that returned nothing to everybody would fail just as
//! loudly as one that returned everything.
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

/// The symbol name / file / line that must survive the *source* gate — this is
/// the half of a symbolicated frame `source:read` deliberately does NOT cover.
/// (The *body* gate does cover it: without `issue:read`+`event:read` there is
/// no frame at all for `source:read` to trim.)
const FRAME_FUNCTION: &str = "sourceGateFixtureFrame";
const FRAME_FILENAME: &str = "src/source_gate_fixture.rs";

/// One distinctive string per body field, planted per event.
///
/// Asserted by grepping the RAW response rather than by checking a JSON key,
/// for the same reason `get_ok` returns the text: a payload that reappears
/// nested inside a variant this test does not know the shape of — a timeline
/// item, say — still shows up in the bytes. A key-absence check would miss it.
///
/// `BODY_MARKERS` is the whole set, so a body field added to the fixture
/// without being added here is a marker that nothing asserts on. Keep the two
/// in lockstep; `strip_event_body`'s own field census lives in
/// `symbolicate.rs`'s unit tests.
const RAW_FRAME_FUNCTION: &str = "sourceGateRawFrame";
const BREADCRUMB_MARKER: &str = "source gate breadcrumb";
const REQUEST_CONTEXT_MARKER: &str = "sg-request-payload-marker";
const TAG_MARKER: &str = "sg-tag-marker";
const CONTEXTS_MARKER: &str = "sg-contexts-marker";
const EXTRA_MARKER: &str = "sg-extra-marker";
const EVENT_USER_MARKER: &str = "sg-event-user-marker";
const DEBUG_META_MARKER: &str = "sg-debug-meta-marker";
const SDK_MARKER: &str = "sg-sdk-marker";
const BODY_MARKERS: [&str; 10] = [
    FRAME_FUNCTION,
    RAW_FRAME_FUNCTION,
    BREADCRUMB_MARKER,
    REQUEST_CONTEXT_MARKER,
    TAG_MARKER,
    CONTEXTS_MARKER,
    EXTRA_MARKER,
    EVENT_USER_MARKER,
    DEBUG_META_MARKER,
    SDK_MARKER,
];

/// Identity shared by all SEEDED_EVENTS rows and by all four routes: every
/// seeded row is reachable as a session member, a device's error, a screen's
/// exception and a person's error, so the four tests differ only in the URL they
/// call.
const SESSION_ID: &str = "source-gate-session";
const DEVICE_KEY: &str = "source-gate-device";
const SCREEN_NAME: &str = "SourceGateScreen";
const DISTINCT_ID: &str = "source-gate-person";

/// The seeded rows are also stamped into one workflow, so `workflows::detail`'s
/// `top_issues` — issue-level metadata reached through an `event:read` route —
/// has something to return.
const WORKFLOW_NAME: &str = "SourceGateWorkflow";
const WORKFLOW_ID: &str = "source-gate-workflow-run";

/// Issue-level strings the COARSE gate must still deliver. A body gate that
/// over-reached and emptied the issue itself would fail on these.
const ISSUE_TITLE: &str = "source gate fixture issue";
const ISSUE_CULPRIT: &str = "source_gate::fixture";

/// A string seeded NOWHERE — not in a payload column, not in a shell column,
/// not in the issue. The control leg of the search-oracle tests: a caller whose
/// `?q=` cannot tell this apart from a marker that IS present in a withheld
/// column has no oracle, and that indistinguishability is the property under
/// test, not merely "fewer rows came back".
const ABSENT_MARKER: &str = "sg-marker-that-was-never-seeded";

/// A shell column's contents — `error_events.exception_type`, which
/// `strip_event_body` KEEPS. The positive leg of the same tests: a narrowed
/// search must still SEARCH. Without this, a route that answered `[]` to every
/// `?q=` from a coarse-gated caller would satisfy every oracle assertion
/// perfectly while having silently deleted the feature.
const SHELL_SEARCH_TERM: &str = "SourceGateError";

/// A shell column's contents at the ISSUE level — `issues.culprit`. Same role as
/// [`SHELL_SEARCH_TERM`] for `issues::list`, which searches
/// `title`/`type`/`culprit` rather than the occurrence columns. Chosen over
/// [`ISSUE_TITLE`] because it contains no spaces to URL-encode.
const SHELL_ISSUE_SEARCH_TERM: &str = "source_gate";

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

    /// GET `path` with `token` and return the status code and raw body, asserting
    /// NOTHING about the status.
    ///
    /// For the one assertion [`get_ok`](Self::get_ok) cannot make: that a request
    /// is REFUSED. A `tag` filter over a withheld column is answered 403, and
    /// `get_ok` would report that as a failure to fetch rather than as the
    /// behaviour under test.
    async fn get_status(&self, path: &str, token: &str, label: &str) -> (u16, String) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path} ({label}) failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("GET {path} ({label}): read body: {e}"));
        (status, text)
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
/// each carrying every [`BODY_MARKERS`] string and all four
/// [`SOURCE_CONTEXT_KEYS`] under its own marker index, and four members whose
/// roles differ ONLY in permissions (see the module docs' persona table).
struct Fixture {
    app_id: Uuid,
    /// The one issue every seeded event rolls up to.
    issue_id: Uuid,
    /// The app's single environment enrollment id — the value `?environment_id=`
    /// takes. Needed because `repo::list_issues_with_reach` has TWO
    /// implementations of the same `?q=` predicate (a diesel one for
    /// `EnvFilter::All`, a raw-SQL one for `One`/`Subset`/`Unattributed`), and a
    /// search oracle closed in one and left open in the other is closed only for
    /// callers who never touch the environment picker.
    env_id: Uuid,
    /// `issue:read` ONLY — the coarse gate with no body entitlement.
    issue_only_token: String,
    /// `event:read` ONLY — reaches the signal routes, entitled to no body.
    event_only_token: String,
    /// The body pair (`issue:read` + `event:read`), **no** `source:read`.
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
                title: ISSUE_TITLE,
                culprit: ISSUE_CULPRIT,
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
        //
        // EVERY body field is populated, each with its own [`BODY_MARKERS`]
        // string: the body gate nulls all of them at once, so a fixture that
        // planted only a stack trace could not tell "strips the body" from
        // "strips the stack trace and leaves `extra` behind".
        //
        // `debug_meta` deliberately carries no `raw_stacktrace`: the fast path
        // above already short-circuits symbolication, and a Dart trace here
        // would only add a way for the fixture to start doing real work.
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
                    stacktrace: json!([{ "function": ev_marker(RAW_FRAME_FUNCTION, i) }]),
                    breadcrumbs: json!([{ "message": ev_marker(BREADCRUMB_MARKER, i) }]),
                    context: json!({ "request": { "url": ev_marker(REQUEST_CONTEXT_MARKER, i) } }),
                    tags: json!({ "customer": ev_marker(TAG_MARKER, i) }),
                    release: Some("1.2.3".into()),
                    distinct_id: Some(DISTINCT_ID.to_string()),
                    event_user: Some(json!({ "email": ev_marker(EVENT_USER_MARKER, i) })),
                    sdk: Some(json!({ "name": ev_marker(SDK_MARKER, i) })),
                    ip_address: Some("203.0.113.9".into()),
                    occurred_at: now,
                    session_id: Some(SESSION_ID.to_string()),
                    device_key: Some(DEVICE_KEY.to_string()),
                    screen: Some(SCREEN_NAME.to_string()),
                    workflow_id: Some(WORKFLOW_ID.to_string()),
                    workflow_name: Some(WORKFLOW_NAME.to_string()),
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
                    debug_meta: Some(json!({ "build_id": ev_marker(DEBUG_META_MARKER, i) })),
                    contexts: json!({ "app": { "note": ev_marker(CONTEXTS_MARKER, i) } }),
                    extra: json!({ "note": ev_marker(EXTRA_MARKER, i) }),
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

        // `workflows::detail` 404s unless a `workflows` row exists; its
        // `top_issues` then joins the error events above by `workflow_name`.
        // Seeded so the *inverse* leak — issue-level metadata reached through an
        // `event:read`-authorized route — has something to be tested on.
        repo::bump_workflow(
            &mut conn,
            app.id,
            env_id,
            WORKFLOW_ID,
            WORKFLOW_NAME,
            Some(SESSION_ID),
            Some(DISTINCT_ID),
            Some(DEVICE_KEY),
            Some("1.2.3"),
            now,
            0,
            SEEDED_EVENTS as i32,
        )
        .await
        .expect("bump workflow");

        // --- four members differing only in permissions -------------------
        // Built by loop rather than four copy-pasted blocks. The entire design
        // of this fixture is "identical members except for one array", and a
        // hand-copied block is exactly where a stray extra permission hides —
        // which would make a "stripped" assertion pass for the wrong reason.
        let personas: [(&str, Vec<&str>); 4] = [
            ("issue-only", vec![perm::ISSUE_READ]),
            ("event-only", vec![perm::EVENT_READ]),
            ("pair", vec![perm::ISSUE_READ, perm::EVENT_READ]),
            (
                "pair-source",
                vec![perm::ISSUE_READ, perm::EVENT_READ, perm::SOURCE_READ],
            ),
        ];
        let keys = JwtKeys::new(JWT_SECRET, 900);
        let mut tokens: Vec<String> = Vec::with_capacity(personas.len());
        for (label, permissions) in personas {
            let user = repo::create_user(
                &mut conn,
                &format!("sg-{label}-{suffix}@example.test"),
                "unused-password-hash",
                &format!("Source Gate {label}"),
            )
            .await
            .unwrap_or_else(|e| panic!("create {label} user: {e}"));
            let role = repo::create_role(
                &mut conn,
                org.id,
                &format!("source-gate {label} role"),
                &format!("exactly {permissions:?}"),
                json!(permissions),
            )
            .await
            .unwrap_or_else(|e| panic!("create {label} role: {e}"));
            repo::create_grant(
                &mut conn,
                NewRoleGrant {
                    org_id: org.id,
                    user_id: user.id,
                    role_id: role.id,
                    scope_type: "app".to_string(),
                    scope_id: app.id,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("grant {label} role at app scope: {e}"));
            let (token, _) = keys
                .issue_access(user.id, false, None)
                .unwrap_or_else(|e| panic!("issue {label} access token: {e}"));
            tokens.push(token);
        }

        drop(conn);

        let mut tokens = tokens.into_iter();
        Fixture {
            app_id: app.id,
            issue_id,
            env_id,
            issue_only_token: tokens.next().expect("issue-only token"),
            event_only_token: tokens.next().expect("event-only token"),
            plain_token: tokens.next().expect("pair token"),
            source_token: tokens.next().expect("pair+source token"),
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
    // Only source LINES go. The caller under test holds the full body pair
    // (`issue:read` + `event:read`), so symbol name, file, line, the event
    // itself and its breadcrumbs all stay — this is the *source* gate, not the
    // body gate, and confusing the two would let an over-broad body strip pass
    // as a correct source strip.
    //
    // Checking the per-event frame marker also proves the gate DROPS NO EVENTS:
    // all SEEDED_EVENTS frames must still be present, so a gate that filtered
    // events out instead of stripping fields would fail here.
    for i in 0..SEEDED_EVENTS {
        let frame = ev_marker(FRAME_FUNCTION, i);
        assert!(
            body.contains(&frame),
            "{route}: frame {frame:?} (event #{i} of {SEEDED_EVENTS}) must survive the source \
             gate — a caller holding the body pair still gets stacktraces and frames, and the \
             gate must strip fields rather than remove events\nbody: {body}"
        );
    }
    for kept in [FRAME_FILENAME, "SourceGateError", BREADCRUMB_MARKER] {
        assert!(
            body.contains(kept),
            "{route}: {kept:?} must survive the source gate — a caller holding the body pair \
             still gets stacktraces, frames and breadcrumbs; only source lines are \
             removed\nbody: {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Body-gate assertions
// ---------------------------------------------------------------------------

/// Every body marker, on every event — for the five routes that return the
/// whole list. The positive leg: without it a body gate that returned nothing
/// to anyone would look perfect.
fn assert_body_present(body: &str, route: &str) {
    for i in 0..SEEDED_EVENTS {
        for base in BODY_MARKERS {
            let marker = ev_marker(base, i);
            assert!(
                body.contains(&marker),
                "{route}: a caller holding BOTH issue:read and event:read must receive \
                 {marker:?} (event #{i} of {SEEDED_EVENTS})\nbody: {body}"
            );
        }
    }
}

/// Every body FIELD, index unpinned — for `issues::detail`, which returns one
/// event (`latest_event`). The three seeded rows share an `occurred_at`, so
/// WHICH of them is "latest" is not determined and asserting on `#0` would be
/// a flake waiting to happen.
fn assert_some_body_present(body: &str, route: &str) {
    for base in BODY_MARKERS {
        assert!(
            body.contains(base),
            "{route}: a caller holding BOTH issue:read and event:read must receive a {base:?} \
             marker on the returned event\nbody: {body}"
        );
    }
}

/// No body marker, on any event — greped over the RAW text so a payload nested
/// inside a shape this test does not model (a timeline item, say) still counts
/// as a leak.
fn assert_body_stripped(body: &str, route: &str) {
    for base in BODY_MARKERS {
        // The un-suffixed base matches every per-event marker (`base#0`,
        // `base#1`, …), so one `contains` covers all SEEDED_EVENTS — including
        // the `.iter_mut().take(1)` gate bug the per-event markers exist to
        // catch, which would leave events #1 and #2 intact. `leaked` exists
        // only so the failure names WHICH events did.
        let leaked: Vec<usize> = (0..SEEDED_EVENTS)
            .filter(|i| body.contains(&ev_marker(base, *i)))
            .collect();
        assert!(
            !body.contains(base),
            "{route}: event body {base:?} (events {leaked:?}) leaked to a caller holding only \
             one half of the issue:read + event:read pair\nbody: {body}"
        );
    }
    // The masked IP is body too — and it is the one field whose serializer
    // already transformed it, so a strip that ran on the wrong copy shows up
    // here and nowhere else.
    assert!(
        !body.contains("203.0.113"),
        "{route}: ip_address leaked to a caller without the body pair\nbody: {body}"
    );
}

/// The occurrence SHELL a coarse-gated caller is still entitled to.
///
/// Counted, not merely found: a handler that returned an EMPTY list would
/// satisfy [`assert_body_stripped`] vacuously, and the whole point of nulling
/// fields instead of dropping rows is that "this happened, at this time, on
/// this release" survives. `occurrences` is how many error rows the route
/// should carry.
fn assert_shell_survives(body: &str, route: &str, occurrences: usize) {
    assert_eq!(
        body.matches("SourceGateError").count(),
        occurrences,
        "{route}: expected {occurrences} occurrence shell(s) — the body gate must null fields, \
         not drop rows\nbody: {body}"
    );
    for kept in [SESSION_ID, DEVICE_KEY, SCREEN_NAME, DISTINCT_ID, "1.2.3"] {
        assert!(
            body.contains(kept),
            "{route}: {kept:?} is shell, not body — it must survive the body gate\nbody: {body}"
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

// ---------------------------------------------------------------------------
// The body gate: `issue:read` + `event:read`
// ---------------------------------------------------------------------------

/// The `issue:read`-only leg of the issues surface: the coarse gate WORKS —
/// title, culprit, fingerprint, counts and the occurrence series all arrive —
/// and the body does not.
#[tokio::test]
async fn issues_detail_gives_metadata_without_a_body_to_issue_read_alone() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("issues_detail_gives_metadata_without_a_body_to_issue_read_alone");
        return;
    };
    let fx = srv.seed().await;
    let route = "issues::detail";
    let path = format!("/v1/apps/{}/issues/{}", fx.app_id, fx.issue_id);

    let (coarse, coarse_text) = srv
        .get_ok(&path, &fx.issue_only_token, "issue:read only")
        .await;
    // The coarse gate is a gate that GRANTS, not only one that withholds. If
    // this half regressed to a 403 or an empty issue, the stripped-body
    // assertions below would pass for entirely the wrong reason.
    assert_eq!(coarse["title"], ISSUE_TITLE, "{coarse}");
    assert_eq!(coarse["culprit"], ISSUE_CULPRIT, "{coarse}");
    assert!(coarse["fingerprint"].is_string(), "{coarse}");
    assert!(
        coarse["times_seen"].as_i64().is_some_and(|n| n >= 1),
        "{coarse}"
    );
    assert!(coarse["series"].is_array(), "{coarse}");
    assert!(
        coarse["latest_event"].is_object(),
        "the occurrence shell must survive — only its payload goes: {coarse}"
    );
    for withheld in [
        "stacktrace",
        "stacktrace_symbolicated",
        "breadcrumbs",
        "context",
        "contexts",
        "extra",
        "tags",
        "sdk",
        "debug_meta",
        "event_user",
        "ip_address",
    ] {
        assert!(
            coarse["latest_event"][withheld].is_null(),
            "{route}: latest_event.{withheld} must be null for issue:read alone: {coarse}"
        );
    }
    assert_body_stripped(&coarse_text, route);
    assert_shell_survives(&coarse_text, route, 1);

    let (both, both_text) = srv
        .get_ok(&path, &fx.plain_token, "issue:read + event:read")
        .await;
    assert_eq!(both["title"], ISSUE_TITLE, "{both}");
    assert_some_body_present(&both_text, route);

    srv.shutdown().await;
}

/// The occurrences list is *nothing but* bodies, and the ruling still answers
/// 200: `issue:read` entitles a caller to the occurrence rows (when, who,
/// which release), and `event:read` to what is inside them.
#[tokio::test]
async fn issues_events_gives_occurrences_without_bodies_to_issue_read_alone() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("issues_events_gives_occurrences_without_bodies_to_issue_read_alone");
        return;
    };
    let fx = srv.seed().await;
    let route = "issues::events";
    let path = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);

    // `envelope_rows`, not `rows`: S2c Task 5 moved this route onto a
    // `SearchEnvelope`. The gate property is unchanged — the assertions below
    // still run over the whole response TEXT, envelope included — but counting
    // `data` is what keeps "every occurrence must still be listed" meaning the
    // rows rather than the wrapper.
    let (coarse, coarse_text) = srv
        .get_ok(&path, &fx.issue_only_token, "issue:read only")
        .await;
    assert_eq!(
        envelope_rows(&coarse, "issue:read only"),
        SEEDED_EVENTS,
        "every occurrence must still be listed: {coarse}"
    );
    assert_eq!(
        coarse["total"], SEEDED_EVENTS,
        "the envelope's total describes the same rows, and the body gate must not change \
         which rows match: {coarse}"
    );
    assert_body_stripped(&coarse_text, route);
    assert_shell_survives(&coarse_text, route, SEEDED_EVENTS);

    let (both, both_text) = srv
        .get_ok(&path, &fx.plain_token, "issue:read + event:read")
        .await;
    assert_eq!(
        envelope_rows(&both, "issue:read + event:read"),
        SEEDED_EVENTS,
        "{both}"
    );
    assert_body_present(&both_text, route);

    srv.shutdown().await;
}

/// The other four handlers, from the other side of the pair. These authorize on
/// `event:read`, so `event:read` alone reaches them — and, until this ruling,
/// walked away with up to 500 whole crash payloads apiece.
///
/// All four in one test, one spawned binary: the fixture (migrate a database,
/// start a process, symbolicate a row) costs far more than the four requests,
/// and each leg names its own route in every assertion message.
#[tokio::test]
async fn event_read_alone_gets_no_body_on_the_four_signal_routes() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("event_read_alone_gets_no_body_on_the_four_signal_routes");
        return;
    };
    let fx = srv.seed().await;

    for (route, path) in [
        (
            "sessions::detail",
            format!("/v1/apps/{}/sessions/{SESSION_ID}", fx.app_id),
        ),
        (
            "devices::detail",
            format!("/v1/apps/{}/device?key={DEVICE_KEY}", fx.app_id),
        ),
        (
            "screens::detail",
            format!("/v1/apps/{}/screens/detail?name={SCREEN_NAME}", fx.app_id),
        ),
        (
            "analytics::person",
            format!("/v1/apps/{}/persons/{DISTINCT_ID}", fx.app_id),
        ),
    ] {
        let (_, coarse) = srv
            .get_ok(&path, &fx.event_only_token, "event:read only")
            .await;
        assert_body_stripped(&coarse, route);
        assert_shell_survives(&coarse, route, SEEDED_EVENTS);

        let (_, both) = srv
            .get_ok(&path, &fx.plain_token, "issue:read + event:read")
            .await;
        assert_body_present(&both, route);
    }

    srv.shutdown().await;
}

/// The INVERSE leak the same ruling closes: issue-level metadata reached
/// through an `event:read`-authorized route.
///
/// `analytics::overview` and `workflows::detail` both fold a `top_issues` list
/// into an otherwise-signal response. Title, culprit, fingerprint and counts
/// are precisely what `issue:read` is the coarse gate FOR, so a composite route
/// handing them out on `event:read` alone means the coarse gate is not a gate.
#[tokio::test]
async fn top_issues_needs_issue_read_on_the_composite_signal_routes() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("top_issues_needs_issue_read_on_the_composite_signal_routes");
        return;
    };
    let fx = srv.seed().await;

    // --- analytics::overview ------------------------------------------------
    let overview = format!("/v1/apps/{}/overview", fx.app_id);
    let (coarse, _) = srv
        .get_ok(&overview, &fx.event_only_token, "event:read only")
        .await;
    assert_eq!(
        coarse["top_issues"].as_array().map(Vec::len),
        Some(0),
        "analytics::overview: top_issues is issue metadata and must be empty without \
         issue:read: {coarse}"
    );
    // Empty, not absent, and the rest of the snapshot still arrives — the
    // carve-out must not turn into "no overview for you".
    assert!(coarse["totals"].is_object(), "{coarse}");
    assert!(coarse["events_series"].is_array(), "{coarse}");

    let (both, _) = srv
        .get_ok(&overview, &fx.plain_token, "issue:read + event:read")
        .await;
    assert_eq!(
        both["top_issues"][0]["title"], ISSUE_TITLE,
        "a caller holding issue:read must still get top_issues: {both}"
    );

    // --- workflows::detail --------------------------------------------------
    let workflow = format!("/v1/apps/{}/workflows/{WORKFLOW_NAME}", fx.app_id);
    let (coarse, _) = srv
        .get_ok(&workflow, &fx.event_only_token, "event:read only")
        .await;
    assert_eq!(
        coarse["top_issues"].as_array().map(Vec::len),
        Some(0),
        "workflows::detail: top_issues is issue metadata and must be empty without \
         issue:read: {coarse}"
    );
    assert_eq!(
        coarse["name"], WORKFLOW_NAME,
        "the workflow aggregate itself is signal and must survive: {coarse}"
    );

    let (both, _) = srv
        .get_ok(&workflow, &fx.plain_token, "issue:read + event:read")
        .await;
    assert_eq!(
        both["top_issues"][0]["title"], ISSUE_TITLE,
        "a caller holding issue:read must still get top_issues: {both}"
    );

    srv.shutdown().await;
}

// ---------------------------------------------------------------------------
// The search bypass: `?q=` as an oracle over the columns the body gate withholds
// ---------------------------------------------------------------------------
//
// The body gate above nulls `contexts`, `extra` and `tags` for a caller holding
// `issue:read` without `event:read`. The free-text `?q=` ran an ILIKE over those
// same three columns cast to text, on three routes that all authorize on
// `issue:read` ALONE (`issues::list`, `issues::events`, `issues::event_stats`).
//
// So the gate withheld the value and the query confirmed it. `?q=sk_live_a`,
// `?q=sk_live_ab`, `?q=sk_live_abc` — each request answers one yes/no question
// about data the response is forbidden to contain, and enough of them spell it
// out byte for byte. Nothing in a log distinguishes that from someone searching.
//
// `repo::TextSearchReach` closes it by making the searchable column set equal
// the readable one, and these tests pin the closure the only way it can be
// pinned: by showing the two answers are INDISTINGUISHABLE. "Returns 0 rows" is
// not the property — a route that returned 0 rows for every search would pass
// that and have destroyed the feature. Each test therefore carries three legs:
//
//   1. withheld marker, coarse caller  -> no match
//   2. absent marker, coarse caller    -> byte-identical response to (1)
//   3. shell term, coarse caller       -> matches, so search still works
//
// plus the full-permission caller finding the marker, so a fixture that stopped
// seeding payloads fails loudly instead of passing vacuously.

/// How many rows a `SearchEnvelope` response carries.
///
/// The bare-array counterpart this file used to carry alongside is **gone**,
/// deliberately: S2c Task 4 put `issues::list` behind an envelope and Task 5
/// `issues::events`, which were its only two callers. Leaving it behind as
/// dead code would be an invitation to reach for it on a third route and get a
/// panic that reads as a broken handler rather than a stale helper.
///
/// `issues::event_stats` is NOT an envelope route and must keep using the
/// plain object accessors: it answers totals, not a page, so there is nothing
/// to paginate and its `events`/`users`/`sessions` keys stay top-level.
///
/// The oracle property these tests pin is unaffected by the wrapper — the
/// coarse caller's two responses are still compared as whole response TEXT,
/// envelope included — but a `total` that differed while `data` matched would
/// be a NEW oracle, so counting rows out of `data` alone would be the weaker
/// check. See the `total` assertion at each probe site.
fn envelope_rows(body: &Value, label: &str) -> usize {
    body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: expected an envelope with a `data` array, got {body}"))
        .len()
}

/// The three columns `strip_event_body` nulls that the payload search used to
/// scan, each named by the marker seeded into it. Every one is probed, not just
/// `extra`: the predicate ORs all three, and a fix that dropped one of them from
/// the `OR` chain would leave the other two as live oracles.
const WITHHELD_COLUMN_MARKERS: [(&str, &str); 3] = [
    ("extra", EXTRA_MARKER),
    ("contexts", CONTEXTS_MARKER),
    ("tags", TAG_MARKER),
];

/// `issues::events` + `issues::event_stats`: the occurrences surface.
///
/// `event_stats` is the sharper half and is why it is tested beside the list
/// rather than trusted to share its fix: it answers an exact COUNT over the
/// whole matching set, with no page cap and no paging ambiguity, so before the
/// fix a SINGLE request answered "does any occurrence's `extra` contain this
/// substring" unambiguously.
#[tokio::test]
async fn q_cannot_probe_a_withheld_payload_column_on_the_occurrences_routes() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("q_cannot_probe_a_withheld_payload_column_on_the_occurrences_routes");
        return;
    };
    let fx = srv.seed().await;
    let events = format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id);
    let stats = format!("{events}/stats");

    // --- the control: what "no match" looks like to a coarse-gated caller ----
    let (absent_list, absent_list_text) = srv
        .get_ok(
            &format!("{events}?q={ABSENT_MARKER}"),
            &fx.issue_only_token,
            "issue:read only, absent term",
        )
        .await;
    assert_eq!(
        envelope_rows(&absent_list, "absent term"),
        0,
        "the control term must match nothing, or it is not a control: {absent_list}"
    );
    let (_, absent_stats_text) = srv
        .get_ok(
            &format!("{stats}?q={ABSENT_MARKER}"),
            &fx.issue_only_token,
            "issue:read only, absent term",
        )
        .await;

    for (column, marker) in WITHHELD_COLUMN_MARKERS {
        // --- leg 1 + 2: the coarse caller cannot tell present from absent -----
        let (_, present_list_text) = srv
            .get_ok(
                &format!("{events}?q={marker}"),
                &fx.issue_only_token,
                "issue:read only, withheld marker",
            )
            .await;
        assert_eq!(
            present_list_text, absent_list_text,
            "issues::events: `?q={marker}` (present in the withheld `{column}` of every seeded \
             event) must be INDISTINGUISHABLE from `?q={ABSENT_MARKER}` (present nowhere) for a \
             caller holding only issue:read. It is not — the response differs, which is a \
             match/no-match oracle over a column this caller's rows arrive with nulled."
        );
        // Named separately from the whole-body equality above, exactly as on
        // the issues list: `total` is a SECOND query (`repo::count_occurrences`)
        // over a separately-lowered copy of the same predicate, so a count built
        // from a wider one would answer the probe on its own even with `data`
        // empty. S2c Task 5 is what introduced that second query here.
        let present_list: Value = serde_json::from_str(&present_list_text).expect("json");
        assert_eq!(
            present_list["total"], absent_list["total"],
            "issues::events: the envelope's `total` must not distinguish `{marker}` from the \
             control either — it is a count over the same withheld `{column}`: {present_list}"
        );

        let (_, present_stats_text) = srv
            .get_ok(
                &format!("{stats}?q={marker}"),
                &fx.issue_only_token,
                "issue:read only, withheld marker",
            )
            .await;
        assert_eq!(
            present_stats_text, absent_stats_text,
            "issues::event_stats: the COUNT for `?q={marker}` (present in the withheld \
             `{column}`) must equal the count for `?q={ABSENT_MARKER}` (present nowhere) for a \
             caller holding only issue:read — a count is the cleanest oracle of all"
        );

        // --- the other side: the pair still finds it --------------------------
        let (both_list, _) = srv
            .get_ok(
                &format!("{events}?q={marker}"),
                &fx.plain_token,
                "issue:read + event:read, withheld marker",
            )
            .await;
        assert_eq!(
            envelope_rows(&both_list, "pair, withheld marker"),
            SEEDED_EVENTS,
            "a caller holding BOTH permissions must still search `{column}` and match every \
             seeded event — if this fails, the fix broke payload search instead of gating it, \
             or the fixture stopped seeding `{column}`: {both_list}"
        );
        let (both_stats, _) = srv
            .get_ok(
                &format!("{stats}?q={marker}"),
                &fx.plain_token,
                "issue:read + event:read, withheld marker",
            )
            .await;
        assert_eq!(
            both_stats["events"].as_i64(),
            Some(SEEDED_EVENTS as i64),
            "the pair's count must agree with the pair's rows — the stat strip describes the \
             list, and both are built from `error_events_for_issue_query`: {both_stats}"
        );
    }

    // --- leg 3: the narrowed search is still a search ------------------------
    let (shell, _) = srv
        .get_ok(
            &format!("{events}?q={SHELL_SEARCH_TERM}"),
            &fx.issue_only_token,
            "issue:read only, shell term",
        )
        .await;
    assert_eq!(
        envelope_rows(&shell, "issue:read only, shell term"),
        SEEDED_EVENTS,
        "a coarse-gated caller must still be able to search the columns it CAN read \
         (`exception_type` here) — the fix narrows the column set, it does not disable \
         search: {shell}"
    );

    srv.shutdown().await;
}

/// `issues::event_stats` says out loud that it narrowed the search.
///
/// The explicit half of the "do not silently return fewer results" ruling. The
/// two list routes answer bare JSON arrays and have nowhere to put a flag (see
/// `routes/issues.rs`' `list` for why that is acceptable and how a client
/// derives the same fact from its own permissions); this route answers an object,
/// so it carries the fact directly.
///
/// Three states, all asserted, because collapsing any two of them is the bug:
/// `null` = no free-text search ran, `false` = one ran with the payload columns
/// excluded, `true` = one ran over everything.
#[tokio::test]
async fn event_stats_reports_whether_the_payload_was_searched() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("event_stats_reports_whether_the_payload_was_searched");
        return;
    };
    let fx = srv.seed().await;
    let stats = format!("/v1/apps/{}/issues/{}/events/stats", fx.app_id, fx.issue_id);

    let (no_search, _) = srv.get_ok(&stats, &fx.plain_token, "no q at all").await;
    assert!(
        no_search["payload_searched"].is_null(),
        "no `?q=` ran, so `payload_searched` must be null — reporting `false` would claim a \
         narrowing on every unfiltered request: {no_search}"
    );
    assert_eq!(
        no_search["events"].as_i64(),
        Some(SEEDED_EVENTS as i64),
        "{no_search}"
    );

    // An EMPTY `?q=` is normalized to "no search" before the query runs, so it
    // must report `null` too — not `false`. This is the one case where the flag
    // and the predicate could disagree, because the flag is derived from the
    // normalized term rather than from the raw query parameter.
    let (empty_q, _) = srv
        .get_ok(&format!("{stats}?q="), &fx.plain_token, "empty q")
        .await;
    assert!(
        empty_q["payload_searched"].is_null(),
        "an empty `?q=` is not a search: {empty_q}"
    );

    let (narrowed, _) = srv
        .get_ok(
            &format!("{stats}?q={EXTRA_MARKER}"),
            &fx.issue_only_token,
            "issue:read only, real search",
        )
        .await;
    assert_eq!(
        narrowed["payload_searched"].as_bool(),
        Some(false),
        "a search that excluded the payload columns must say so: {narrowed}"
    );

    let (full, _) = srv
        .get_ok(
            &format!("{stats}?q={EXTRA_MARKER}"),
            &fx.plain_token,
            "pair, real search",
        )
        .await;
    assert_eq!(
        full["payload_searched"].as_bool(),
        Some(true),
        "a search that DID cover the payload columns must say so — otherwise the flag is a \
         constant and tells a client nothing: {full}"
    );

    srv.shutdown().await;
}

/// `issues::list`, in BOTH of its implementations.
///
/// `repo::list_issues_with_reach` answers `EnvFilter::All` through diesel and
/// `One`/`Subset`/`Unattributed` through a hand-written SQL string, and the two
/// carry independent copies of the `q` predicate. Closing the oracle in one and
/// not the other would leave it open to anyone who has touched the environment
/// picker — so every leg here runs twice, once with no `environment_id` and once
/// with the fixture's own.
#[tokio::test]
async fn q_cannot_probe_a_withheld_payload_column_on_the_issues_list() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("q_cannot_probe_a_withheld_payload_column_on_the_issues_list");
        return;
    };
    let fx = srv.seed().await;
    let list = format!("/v1/apps/{}/issues", fx.app_id);

    // Both environment branches. Before S2c Task 4 these were two different
    // query builders (diesel for `All`, raw SQL for `One`) and the fix had to
    // land in both; they are one builder now, with `One` adding a membership
    // `EXISTS`. Kept as two branches anyway — the predicate still differs, and
    // this is the test that would notice the narrowing being lost.
    for (branch, env) in [
        ("EnvFilter::All", String::new()),
        ("EnvFilter::One", format!("&environment_id={}", fx.env_id)),
    ] {
        let (absent, absent_text) = srv
            .get_ok(
                &format!("{list}?q={ABSENT_MARKER}{env}"),
                &fx.issue_only_token,
                branch,
            )
            .await;
        assert_eq!(
            envelope_rows(&absent, branch),
            0,
            "{branch}: the control term must match nothing: {absent}"
        );

        for (column, marker) in WITHHELD_COLUMN_MARKERS {
            let (_, present_text) = srv
                .get_ok(
                    &format!("{list}?q={marker}{env}"),
                    &fx.issue_only_token,
                    branch,
                )
                .await;
            assert_eq!(
                present_text, absent_text,
                "{branch}: `?q={marker}` (present in the withheld `{column}` of this issue's \
                 events) must be indistinguishable from `?q={ABSENT_MARKER}` for a caller \
                 holding only issue:read"
            );
            // Whole-body equality above already covers this, but naming it
            // separately is what keeps the envelope from quietly becoming a
            // NEW oracle: `total` is computed by a second query
            // (`repo::count_issues`), and a count built from a different
            // predicate than the page would answer the probe on its own.
            let present: Value = serde_json::from_str(&present_text).expect("json");
            assert_eq!(
                present["total"], absent["total"],
                "{branch}: the envelope's `total` must not distinguish `{marker}` from the \
                 control either — it is a count over the same withheld column: {present}"
            );

            let (both, _) = srv
                .get_ok(&format!("{list}?q={marker}{env}"), &fx.plain_token, branch)
                .await;
            assert_eq!(
                envelope_rows(&both, branch),
                1,
                "{branch}: a caller holding BOTH permissions must still reach the issue through \
                 a `{column}` match — otherwise this test proves nothing about gating, only \
                 that the payload scan is gone: {both}"
            );
        }

        let (shell, _) = srv
            .get_ok(
                &format!("{list}?q={SHELL_ISSUE_SEARCH_TERM}{env}"),
                &fx.issue_only_token,
                branch,
            )
            .await;
        assert_eq!(
            envelope_rows(&shell, branch),
            1,
            "{branch}: a coarse-gated caller must still be able to search `culprit`, which it \
             can read: {shell}"
        );
    }

    srv.shutdown().await;
}

/// The same oracle wearing a different hat: `filter=tag:…`.
///
/// `tags` is one of the ten columns `strip_event_body` nulls, and the `tag`
/// filter is a predicate over it — a SHARPER probe than `?q=`, which can only
/// scan the whole `contexts||extra||tags` blob: `tag:eq:k=v` tests one exact
/// value and `tag:contains:k=v` gives a per-key ILIKE.
///
/// Refused (403) rather than silently dropped, and the difference matters. The
/// `q` fix narrows the column set, which still honestly answers "find me rows".
/// A `tag` filter is an explicit NARROWING, so dropping it returns MORE rows
/// than were asked for, every one displayed under a chip claiming they match —
/// a wrong answer rather than a smaller one.
#[tokio::test]
async fn tag_filters_are_refused_without_event_read() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("tag_filters_are_refused_without_event_read");
        return;
    };
    let fx = srv.seed().await;

    // `=` and `#` percent-encoded: `=` because the filter value's own `key=value`
    // separator would otherwise be read as a second query-string assignment, `#`
    // because `sg-tag-marker#0` would start a URL fragment and never reach the
    // server at all — a mistake that would make this test pass for the wrong
    // reason (a filter the server never saw cannot be refused).
    let eq = format!("tag:eq:customer%3D{}%230", TAG_MARKER);
    let contains = format!("tag:contains:customer%3D{TAG_MARKER}");

    for (route, path) in [
        (
            "issues::events",
            format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id),
        ),
        (
            "issues::event_stats",
            format!("/v1/apps/{}/issues/{}/events/stats", fx.app_id, fx.issue_id),
        ),
        ("issues::list", format!("/v1/apps/{}/issues", fx.app_id)),
    ] {
        for filter in [&eq, &contains] {
            let url = format!("{path}?filter={filter}");
            let (status, body) = srv
                .get_status(&url, &fx.issue_only_token, "issue:read only, tag filter")
                .await;
            assert_eq!(
                status, 403,
                "{route}: `?filter={filter}` is a predicate over `tags`, which this caller's \
                 events arrive with nulled — it must be refused, not answered\nbody: {body}"
            );

            // The pair is unaffected: the refusal is a permission check, not a
            // feature removal. Without this leg, deleting `tag` from the filter
            // whitelist entirely would pass the assertion above.
            let (status, body) = srv
                .get_status(&url, &fx.plain_token, "pair, tag filter")
                .await;
            assert_eq!(
                status, 200,
                "{route}: a caller holding BOTH permissions must still be able to filter by \
                 tag\nbody: {body}"
            );
        }
    }

    // And the filter still WORKS for the pair — an `eq` on event #0's tag matches
    // exactly that one occurrence, so a refusal implemented as "accept and match
    // nothing" would fail here.
    let (one, _) = srv
        .get_ok(
            &format!(
                "/v1/apps/{}/issues/{}/events?filter={eq}",
                fx.app_id, fx.issue_id
            ),
            &fx.plain_token,
            "pair, tag:eq",
        )
        .await;
    assert_eq!(
        envelope_rows(&one, "pair, tag:eq"),
        1,
        "`tag:eq` on event #0's own marker must match exactly one occurrence: {one}"
    );

    srv.shutdown().await;
}

/// `filter=workflow:…` — the same oracle over a column that is not in the event
/// body at all, which is exactly why it was first ruled harmless.
///
/// The original reasoning was: `workflow_name` is not one of the ten columns
/// `strip_event_body` nulls, indeed not part of `ErrorEvent`'s wire shape, so
/// there is nothing to leak. But "absent from the body I already strip" is not
/// "public". The endpoints that serve workflow names — `workflows::list`,
/// `detail`, `runs` — all authorize on `event:read`, so a caller holding
/// `issue:read` alone may not learn them through any route, and
/// `workflow:contains:` handed them a per-prefix ILIKE to enumerate them.
///
/// The test that matters is which permission owns the column, not which struct
/// happens to carry it.
#[tokio::test]
async fn workflow_filters_are_refused_without_event_read() {
    let Some(mut srv) = TestServer::start().await else {
        skipped("workflow_filters_are_refused_without_event_read");
        return;
    };
    let fx = srv.seed().await;

    // A prefix, not the whole name: `contains` is the sharp form, and a prefix is
    // what an attacker would actually walk to enumerate names character by
    // character.
    let eq = format!("workflow:eq:{WORKFLOW_NAME}");
    let contains = "workflow:contains:SourceGate".to_string();

    for (route, path) in [
        (
            "issues::events",
            format!("/v1/apps/{}/issues/{}/events", fx.app_id, fx.issue_id),
        ),
        (
            "issues::event_stats",
            format!("/v1/apps/{}/issues/{}/events/stats", fx.app_id, fx.issue_id),
        ),
        ("issues::list", format!("/v1/apps/{}/issues", fx.app_id)),
    ] {
        for filter in [&eq, &contains] {
            let url = format!("{path}?filter={filter}");
            let (status, body) = srv
                .get_status(
                    &url,
                    &fx.issue_only_token,
                    "issue:read only, workflow filter",
                )
                .await;
            assert_eq!(
                status, 403,
                "{route}: `?filter={filter}` probes `workflow_name`, which only `event:read` \
                 may read — it must be refused, not answered\nbody: {body}"
            );

            // Without this leg, deleting `workflow` from the filter whitelist
            // altogether would satisfy the assertion above.
            let (status, body) = srv
                .get_status(&url, &fx.plain_token, "pair, workflow filter")
                .await;
            assert_eq!(
                status, 200,
                "{route}: a caller holding BOTH permissions must still be able to filter by \
                 workflow\nbody: {body}"
            );
        }
    }

    // And it still MATCHES for the pair, so a refusal implemented as "accept and
    // return nothing" would fail here. The seeded events are all stamped into
    // `WORKFLOW_NAME`, so an `eq` must return them rather than an empty list.
    let (matched, _) = srv
        .get_ok(
            &format!(
                "/v1/apps/{}/issues/{}/events?filter={eq}",
                fx.app_id, fx.issue_id
            ),
            &fx.plain_token,
            "pair, workflow:eq",
        )
        .await;
    assert!(
        envelope_rows(&matched, "pair, workflow:eq") > 0,
        "the seeded occurrences carry this workflow name, so the filter must match \
         them: {matched}"
    );

    srv.shutdown().await;
}

/// No handler may call the payload-inclusive repo entry points.
///
/// `repo::list_issues` and `repo::list_error_events_for_issue` kept their pre-D4
/// signatures — payload scan unconditionally ON — because
/// `crates/sauron-db/tests/env_scoping.rs` has ~30 call sites asserting the
/// environment scoping of that predicate, including of the payload scan itself.
/// That leaves two fail-OPEN functions in scope for every handler in this binary,
/// and the mistake they invite is invisible: the code compiles, every existing
/// test passes, and one role quietly regains the oracle.
///
/// So the ban is enforced mechanically, over the source, in the same spirit as
/// `dashboard/src/lib/api/scope.test.ts` parsing its own module off disk. Needs
/// no database and no server, so unlike its neighbours it always runs.
#[test]
fn no_handler_may_call_the_payload_inclusive_repo_entry_points() {
    /// The reach-taking siblings do not match these patterns: the `(` is part of
    /// the needle, and `list_issues_with_reach(` has `_with_reach` in between.
    const BANNED: [&str; 2] = ["list_issues(", "list_error_events_for_issue("];

    fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read source dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, found);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            for (i, line) in text.lines().enumerate() {
                for needle in BANNED {
                    if line.contains(needle) {
                        found.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                    }
                }
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    walk(&src, &mut found);
    assert!(
        found.is_empty(),
        "these call the payload-inclusive repo entry points, which search \
         `contexts`/`extra`/`tags` regardless of the caller's permissions. Use \
         `repo::list_issues_with_reach` / `repo::list_error_events_for_issue_with_reach` with \
         `symbolicate::text_search_reach(&perms)` instead:\n  {}",
        found.join("\n  ")
    );
}
