//! HTTP-level tests for `POST /v1/apps/{app_id}/artifacts` — specifically the
//! server-side derivation of a Dart artifact's `debug_id` from the uploaded
//! ELF's GNU build-id note (Task 2 of the mobile-symbol-upload plan).
//!
//! Why these are HTTP tests and not unit tests on `sauron_symbols::build_id_hex`
//! (which already has its own): the thing that breaks here is *wiring*, and
//! every way it breaks is silent. Dart artifacts are matched at symbolication
//! time on `debug_id` alone, so a derivation that runs too late (after the
//! idempotency lookup), or leaks into the JS path, or maps a corrupt upload to
//! a 500 instead of a 400, produces an artifact row that looks perfectly fine
//! and simply never matches — surfacing weeks later as `no_artifacts`, which is
//! indistinguishable from never having uploaded at all. Only the status code
//! and the JSON body on the wire can tell those apart, which is what these five
//! tests pin:
//!
//!  1. `dart_symbols` with no `debug_id` gets the fixture's real build-id, in
//!     both `debug_id` and `derived_debug_id`;
//!  2. re-uploading the same bytes dedupes — proof the derived id reached
//!     `find_artifact_by_debug_id`, i.e. that derivation happens BEFORE the
//!     idempotency lookup and not after it;
//!  3. an explicit `debug_id` overrides the derived one, and the response still
//!     reports the real note value so a mismatch is visible at upload time;
//!  4. a `dart_symbols` body that is not an ELF is a 400, not a 500 and not a
//!     silent 201;
//!  5. a `js_sourcemap` upload derives nothing — the regression guard that the
//!     Dart path stayed on the Dart path.
//!
//! Five more were added in the Task 2 fix round:
//!
//!  6. the genuine lookup-then-insert race: a competing row lands *between* the
//!     idempotency lookup and the insert, and the loser gets the dedupe 200 the
//!     winner's re-uploader gets rather than a bare 500. Made deterministic with
//!     a barrier, for the reason spelled out on the test itself;
//!  7. the same thing in its production shape — several concurrent uploads of one
//!     file, asserting the invariants that hold however they interleave;
//!  8. the escape hatch: an unreadable note plus an explicit `debug_id` still
//!     uploads (this is the one arm that deviates from the brief, and nothing
//!     used to touch it);
//!  9. an explicit `debug_id` is trimmed and lowercased before it is stored, and
//!     so are `release`, `dist`, `name` and `arch`, which share the same helper.
//!
//! And two in the normalization-symmetry round:
//!
//! 10. a dedupe 200 reports the **stored row's** `blob_sha256`, not the request's,
//!     so two different files claiming one `debug_id` is visible to the uploader
//!     instead of a body that says the bytes were stored when they were not;
//! 11. the end-to-end version of the write/read symmetry: an event reporting its
//!     build-id **uppercase** in `debug_meta` symbolicates against the lowercase
//!     id the upload path derived and stored. That one is a real symbolication
//!     over the real DWARF, because the unit tests in `sauron-symbols` can only
//!     prove what the engine hands its `BlobFetch`, not that the value survives
//!     the round trip through Postgres.
//!
//! Every test spawns the actual compiled `sauron-api` binary (via Cargo's
//! `CARGO_BIN_EXE_sauron-api`) against a fresh, migrated, ephemeral database
//! and drives it with `reqwest`. See `tests/http_env_scoping.rs`'s `TestServer`
//! for the full doc comments this file's copy abbreviates.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use diesel::sql_types::{BigInt, Binary, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{NewErrorEvent, NewIssue, NewRoleGrant};
use sauron_db::repo;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-artifacts-test-secret-000000000000000";

/// The same ELF `sauron-symbols`' own `build_id` tests use. Embedded at compile
/// time rather than read at runtime so a moved fixture is a build error here
/// instead of a skipped assertion.
const SAMPLE_ELF: &[u8] =
    include_bytes!("../../../crates/sauron-symbols/tests/fixtures/sample.elf");

/// `readelf -n crates/sauron-symbols/tests/fixtures/sample.elf` ->
/// `Build ID: ab36961b44baef9d7e3b9296dff3ce3e59be51a3`. Asserted exactly, not
/// by shape: "looks like 40 hex chars" would pass on a *wrong* id, and a wrong
/// id is precisely the failure this feature exists to prevent.
const SAMPLE_ELF_BUILD_ID: &str = "ab36961b44baef9d7e3b9296dff3ce3e59be51a3";

/// Advisory-lock key for the race barrier in
/// `a_row_landing_between_the_lookup_and_the_insert_dedupes_instead_of_500ing`.
/// Advisory locks are scoped to a database and every test here gets its own
/// ephemeral one, but the `pg_locks` probe filters on `database` anyway so a
/// neighbouring suite that happened to pick the same key could not be mistaken
/// for our own waiter.
const BARRIER_KEY: (i32, i32) = (74001, 1);

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
        // The probe listener is dropped on return so the child can bind, which
        // leaves the kernel free to hand the same port to a concurrent
        // `TestServer::start()` on another test thread; the loser's child then
        // dies with "Address already in use" and the harness reports it as
        // "exited early", which reads like a product fault. The registry rules
        // out ports this process already issued to itself.
        if issued.lock().expect("port registry").insert(port) {
            return port;
        }
    }
    panic!("no unused ephemeral port after 100 attempts");
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
        // silently skips anything else, so a differently ordered name leaks
        // every database it creates. Do not reorder.
        //
        // "sauron_test_" (12) + 10-digit timestamp + "_" + "art" (3) + 32-hex
        // uuid = 58 bytes, within `validate_db_ident`'s 63-byte cap.
        let db_name = format!(
            "sauron_test_{}_art{}",
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
        // 4, not the 2 the sibling harnesses use: the race test holds one
        // connection parked on a session-level advisory lock for the whole
        // barrier while a second connection polls `pg_locks` and inserts the
        // winning row. With max_size = 2 that is the entire pool, and any
        // additional checkout would deadlock the test rather than fail it.
        let pool = sauron_db::build_pool(&db_url, 4).expect("build test pool");

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

    /// Upload raw bytes as the request body with metadata in the query string,
    /// exactly as the real uploader does (the route takes `Bytes`, not
    /// multipart). Returns the status and the parsed body: both matter here —
    /// the status says which guard fired, the body carries the two ids.
    async fn upload(&self, app_id: Uuid, token: &str, query: &str, body: &[u8]) -> (u16, Value) {
        let path = format!("/v1/apps/{app_id}/artifacts?{query}");
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .body(body.to_vec())
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|e| panic!("POST {path}: failed to read body (status {status}): {e}"));
        let value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("POST {path}: expected a JSON body (status {status}): {e}\nbody: {text}")
        });
        (status, value)
    }

    /// `GET /v1/apps/{app_id}/artifacts` parsed. Used to check what was actually
    /// *stored*, which is the only claim that matters — the upload response
    /// echoes values it has not necessarily persisted.
    async fn list(&self, app_id: Uuid, token: &str) -> Value {
        self.client
            .get(format!("{}/v1/apps/{}/artifacts", self.base, app_id))
            .bearer_auth(token)
            .send()
            .await
            .expect("list artifacts")
            .json()
            .await
            .expect("list body")
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

/// One app plus a token holding `artifact:write` org-wide — the minimum the
/// upload route authorizes against.
struct ArtifactsFixture {
    app_id: Uuid,
    token: String,
}

impl TestServer {
    async fn seed_artifacts_fixture(&self) -> ArtifactsFixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "art org", &format!("art-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "art project", &format!("art-p-{s}"))
            .await
            .expect("project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "Mobile",
            &format!("art-a-{s}"),
            "flutter",
        )
        .await
        .expect("app");

        let user = repo::create_user(
            &mut conn,
            &format!("art-dev-{s}@example.test"),
            "x",
            "Uploader",
        )
        .await
        .expect("user");
        let role = repo::create_role(
            &mut conn,
            org.id,
            "art uploader",
            // `issue:read` is what `list` authorizes against, and one test
            // reads the stored row back through it rather than trusting the
            // upload response alone.
            //
            // `event:read` is here because these are SYMBOLICATION tests, and a
            // symbolicated stack trace is an event BODY: bodies require BOTH
            // halves of the pair (see `sauron_auth::perm::EVENT_READ`), so
            // without it `issues::detail` would hand this fixture a stripped
            // `latest_event` and every frame assertion below would fail on a
            // null `stacktrace_symbolicated`. This role is pinning "a caller
            // entitled to bodies gets fully symbolicated frames", NOT "the
            // minimum permission a symbolication read needs" — that second
            // question is what `http_source_context.rs`'s `issue_only_token`
            // persona answers, and it asserts the opposite outcome.
            "org-wide artifact write + issue/event read",
            json!([perm::ARTIFACT_WRITE, perm::ISSUE_READ, perm::EVENT_READ]),
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

        ArtifactsFixture {
            app_id: app.id,
            token,
        }
    }
}

/// 1. The headline case: no `debug_id` in the query, and the server still
///    stores a matchable one, read out of the file itself.
#[tokio::test]
async fn dart_upload_derives_the_debug_id_from_the_build_id_note() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=dart_symbols&platform=android&arch=arm64",
            SAMPLE_ELF,
        )
        .await;

    assert_eq!(status, 201, "expected created, body: {body}");
    assert_eq!(
        body["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the stored id must be the file's real build-id: {body}"
    );
    assert_eq!(
        body["derived_debug_id"], SAMPLE_ELF_BUILD_ID,
        "the response must show what was derived, not just what was stored: {body}"
    );
    assert_eq!(body["deduped"], json!(false), "first upload: {body}");

    // The response is not the contract on its own — `list` is what the
    // dashboard and the symbolicator read. Confirm the id was persisted, not
    // merely echoed.
    let listed: Value = h
        .client
        .get(format!("{}/v1/apps/{}/artifacts", h.base, f.app_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("list artifacts")
        .json()
        .await
        .expect("list body");
    assert_eq!(
        listed[0]["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the persisted row must carry the derived id: {listed}"
    );

    h.shutdown().await;
}

/// 2. Ordering pin: the derivation has to happen BEFORE the idempotency lookup,
///    because `find_artifact_by_debug_id` is what the lookup calls with it.
///
///    The two uploads deliberately carry **different `name`s**. Without that,
///    this test proves nothing: with no derived id the handler falls through to
///    `find_artifact_by_release_name`, which matches on
///    (app_id, release IS NULL, name IS NULL, blob_sha256) — identical bytes
///    would dedupe on content alone and the test would stay green with the
///    derivation moved anywhere. Differing names close that fallback, so the
///    only route to a 200 here is the derived `debug_id`.
#[tokio::test]
async fn re_uploading_the_same_dart_symbols_dedupes_on_the_derived_id() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;
    let base = "kind=dart_symbols&platform=android&arch=arm64";

    let (first_status, first) = h
        .upload(
            f.app_id,
            &f.token,
            &format!("{base}&name=app.android-arm64.symbols"),
            SAMPLE_ELF,
        )
        .await;
    assert_eq!(first_status, 201, "first upload: {first}");

    let (second_status, second) = h
        .upload(
            f.app_id,
            &f.token,
            &format!("{base}&name=renamed-by-ci.symbols"),
            SAMPLE_ELF,
        )
        .await;
    assert_eq!(second_status, 200, "re-upload should dedupe: {second}");
    assert_eq!(second["deduped"], json!(true), "{second}");
    assert_eq!(
        second["id"], first["id"],
        "dedupe must return the ORIGINAL row, not a new one: {second}"
    );
    assert_eq!(
        second["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the dedupe path reports the ids too: {second}"
    );
    assert_eq!(second["derived_debug_id"], SAMPLE_ELF_BUILD_ID, "{second}");

    // Belt and braces: the response saying "deduped" is not the same claim as
    // the table holding one row.
    let listed: Value = h
        .client
        .get(format!("{}/v1/apps/{}/artifacts", h.base, f.app_id))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("list artifacts")
        .json()
        .await
        .expect("list body");
    assert_eq!(
        listed.as_array().map(|a| a.len()),
        Some(1),
        "one debug-id, one row: {listed}"
    );

    h.shutdown().await;
}

/// 3. Precedence, and the reason both fields exist. An explicit id wins, but
///    the response also carries what the file actually says, so a paste error
///    is visible immediately instead of as a mute `no_artifacts` later.
#[tokio::test]
async fn an_explicit_debug_id_overrides_the_derived_one_and_both_are_reported() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=dart_symbols&platform=android&debug_id=deadbeef",
            SAMPLE_ELF,
        )
        .await;

    assert_eq!(status, 201, "{body}");
    assert_eq!(
        body["debug_id"], "deadbeef",
        "the explicit value must win: {body}"
    );
    assert_eq!(
        body["derived_debug_id"], SAMPLE_ELF_BUILD_ID,
        "the disagreement must be visible in the same response: {body}"
    );

    h.shutdown().await;
}

/// 4. A body we cannot parse, with nothing to fall back on, is the caller's
///    problem (400) — not ours (500) and, critically, not a 201 for an artifact
///    that could never match anything.
#[tokio::test]
async fn a_dart_symbols_body_that_is_not_an_elf_is_rejected_with_400() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=dart_symbols&platform=android",
            b"this is not an ELF file, it is a sentence",
        )
        .await;

    assert_eq!(
        status, 400,
        "an unparseable dart_symbols upload must be a 400, not a 500 and not a silent 201: {body}"
    );
    // The message has to name the problem — "corrupt artifact" alone leaves a
    // legitimately note-less file indistinguishable from garbage.
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("debug_id"),
        "the 400 must tell the uploader what to do: {body}"
    );

    h.shutdown().await;
}

/// 5. Regression guard: the JS path must be byte-identical to before. A
///    `js_sourcemap` is matched on (release, name, content), never on
///    `debug_id`, and it is not an ELF — deriving anything for it would at best
///    be noise and at worst a 400 on a perfectly good source map.
#[tokio::test]
async fn js_sourcemap_uploads_derive_nothing() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let map = br#"{"version":3,"sources":["app.ts"],"names":[],"mappings":"AAAA"}"#;
    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=js_sourcemap&platform=web&release=1.0.0&name=app.js.map",
            map,
        )
        .await;

    assert_eq!(status, 201, "a plain source map must still upload: {body}");
    assert_eq!(
        body["debug_id"],
        Value::Null,
        "JS artifacts must not acquire a debug_id: {body}"
    );
    assert_eq!(
        body["derived_debug_id"],
        Value::Null,
        "derivation must not run on the JS path: {body}"
    );

    h.shutdown().await;
}

// ===========================================================================
// Task 2 fix round
// ===========================================================================

/// How many backends are *waiting* on the race barrier's advisory lock, in this
/// database. A non-zero answer is positive proof that the handler has left its
/// idempotency lookup and entered the INSERT — the only statement the barrier
/// trigger fires on.
async fn barrier_waiters(conn: &mut sauron_db::AsyncPgConnection) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let row: N = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM pg_locks
          WHERE locktype = 'advisory'
            AND classid = $1 AND objid = $2
            AND NOT granted
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .bind::<diesel::sql_types::Integer, _>(BARRIER_KEY.0)
    .bind::<diesel::sql_types::Integer, _>(BARRIER_KEY.1)
    .get_result(conn)
    .await
    .expect("count advisory-lock waiters");
    row.n
}

/// Does a blob row exist for these exact bytes? The lookup dedupe path returns
/// *before* `put_blob`, so a stored blob is independent evidence that the
/// handler got past the lookup and is racing on the insert instead.
async fn blob_exists(conn: &mut sauron_db::AsyncPgConnection, sha: &[u8]) -> bool {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let row: N =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM symbol_blobs WHERE sha256 = $1")
            .bind::<Binary, _>(sha.to_vec())
            .get_result(conn)
            .await
            .expect("count blobs");
    row.n > 0
}

/// 6. **F1.** The lookup-then-insert race, which `insert_symbol_artifact` had no
///    `on_conflict` for. `symbol_artifacts_debugid_idx` is a real UNIQUE index on
///    `(app_id, debug_id)`, and Task 2 moved it from unreachable (no Dart upload
///    carried a `debug_id`) to routinely hit. The loser used to receive a bare
///    500 on a request whose correct answer is "already have it".
///
///    **Why the barrier, and not just two concurrent uploads.** Two concurrent
///    uploads are free to serialize: the second then finds the row in the
///    *lookup* and takes the ordinary dedupe path, the test passes, and the
///    recovery arm is never entered — so it would pin nothing, exactly the way a
///    second sequential upload pins nothing. A `BEFORE INSERT` trigger that parks
///    the handler's own INSERT on an advisory lock this test holds makes the
///    interleaving a fact instead of a hope: the competing row is inserted while
///    the handler is provably suspended inside the statement that must collide.
///
///    Two independent checks confirm the branch was taken rather than assumed: a
///    waiting `pg_locks` entry on the barrier key, and a `symbol_blobs` row for
///    the uploaded bytes (which the lookup path returns before writing).
#[tokio::test]
async fn a_row_landing_between_the_lookup_and_the_insert_dedupes_instead_of_500ing() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;
    let sha = sauron_symbols::sha256(SAMPLE_ELF).to_vec();

    // Fires only for `name = 'race-loser'`, so the winning row this test inserts
    // by hand is not parked as well.
    {
        let mut c = h.conn().await;
        diesel::sql_query(format!(
            // `BARRIER_KEY` is interpolated, not bound: this is DDL, the values
            // are `i32` constants, and the alternative is three copies of the
            // key that can silently drift apart from the `pg_locks` probe's.
            "CREATE FUNCTION art_race_barrier() RETURNS trigger LANGUAGE plpgsql AS $fn$
             BEGIN
               IF NEW.name = 'race-loser' THEN
                 PERFORM pg_advisory_xact_lock({}, {});
               END IF;
               RETURN NEW;
             END $fn$",
            BARRIER_KEY.0, BARRIER_KEY.1
        ))
        .execute(&mut *c)
        .await
        .expect("create the barrier function");
        diesel::sql_query(
            "CREATE TRIGGER art_race_barrier_trg BEFORE INSERT ON symbol_artifacts
             FOR EACH ROW EXECUTE FUNCTION art_race_barrier()",
        )
        .execute(&mut *c)
        .await
        .expect("create the barrier trigger");
    }

    // Held on its own connection for the duration of the barrier. `xact`-scoped
    // on the handler's side so its lock request dies with the failed statement's
    // transaction; session-scoped here so it survives our awaits.
    let mut holder = h.conn().await;
    diesel::sql_query(format!(
        "SELECT pg_advisory_lock({}, {})",
        BARRIER_KEY.0, BARRIER_KEY.1
    ))
    .execute(&mut *holder)
    .await
    .expect("take the barrier lock");

    // No artifact rows exist yet, so this upload misses the lookup, compresses,
    // `put_blob`s, and then parks inside its INSERT.
    let (base, token, app_id) = (h.base.clone(), f.token.clone(), f.app_id);
    let upload = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!(
                "{base}/v1/apps/{app_id}/artifacts\
                 ?kind=dart_symbols&platform=android&name=race-loser"
            ))
            .bearer_auth(token)
            .body(SAMPLE_ELF.to_vec())
            .send()
            .await
            .expect("send the racing upload");
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("racing upload body");
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("racing upload: expected JSON (status {status}): {e}\n{text}")
        });
        (status, value)
    });

    let mut probe = h.conn().await;
    let mut parked = false;
    for _ in 0..100 {
        if barrier_waiters(&mut probe).await > 0 {
            parked = true;
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(100)).await;
    }
    assert!(
        parked,
        "the upload never reached its INSERT — the barrier trigger did not fire, so this test \
         would prove nothing about the race"
    );
    assert!(
        blob_exists(&mut probe, &sha).await,
        "the handler should already have written the blob; if it has not, it never got past the \
         idempotency lookup and the collision under test is not the one being exercised"
    );

    // The competing writer: a different session, committed immediately, landing
    // squarely between the parked handler's lookup and its insert.
    #[derive(diesel::QueryableByName)]
    struct Id {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let winner: Id = diesel::sql_query(
        "INSERT INTO symbol_artifacts (app_id, kind, platform, name, debug_id, blob_sha256)
         VALUES ($1, 'dart_symbols', 'android', 'race-winner', $2, $3)
         RETURNING id",
    )
    .bind::<SqlUuid, _>(f.app_id)
    .bind::<Text, _>(SAMPLE_ELF_BUILD_ID)
    .bind::<Binary, _>(sha.clone())
    .get_result(&mut *probe)
    .await
    .expect("insert the winning artifact row");

    diesel::sql_query(format!(
        "SELECT pg_advisory_unlock({}, {})",
        BARRIER_KEY.0, BARRIER_KEY.1
    ))
    .execute(&mut *holder)
    .await
    .expect("release the barrier lock");

    let (status, body) = upload.await.expect("racing upload task");
    assert_eq!(
        status, 200,
        "the loser of the insert race must get the dedupe 200, not a 500 — 'already have it' is \
         the correct answer: {body}"
    );
    assert_eq!(body["deduped"], json!(true), "{body}");
    assert_eq!(
        body["id"],
        json!(winner.id),
        "the loser must be handed the WINNER's row, not a new one and not its own: {body}"
    );
    assert_eq!(
        body["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the recovery path reports the same ids the lookup path does: {body}"
    );
    assert_eq!(body["derived_debug_id"], SAMPLE_ELF_BUILD_ID, "{body}");

    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(
        listed.as_array().map(|a| a.len()),
        Some(1),
        "one debug-id, one row — the loser must not have inserted a second: {listed}"
    );

    drop(probe);
    drop(holder);
    h.shutdown().await;
}

/// 7. **F1 in its production shape.** Several uploads of the same symbols file in
///    flight at once — a slow upload and an impatient second (and third) click on
///    Task 3's form. Deliberately probabilistic about *which* path each request
///    takes, because the invariants are the same either way and they are the ones
///    a user notices: nobody gets a 5xx, exactly one request creates the row, and
///    the table holds exactly one.
///
///    The `name`s differ so the content-address fallback
///    (`find_artifact_by_release_name` matches on `(release, name, blob_sha256)`)
///    cannot supply the 200 — the derived `debug_id` is the only route to it.
#[tokio::test]
async fn concurrent_uploads_of_one_symbols_file_never_500() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let mut tasks = Vec::new();
    for i in 0..6 {
        let (base, token, app_id) = (h.base.clone(), f.token.clone(), f.app_id);
        tasks.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .post(format!(
                    "{base}/v1/apps/{app_id}/artifacts\
                     ?kind=dart_symbols&platform=android&name=click-{i}"
                ))
                .bearer_auth(token)
                .body(SAMPLE_ELF.to_vec())
                .send()
                .await
                .expect("send concurrent upload");
            let status = resp.status().as_u16();
            let text = resp.text().await.expect("concurrent upload body");
            (status, text)
        }));
    }

    let mut created = 0;
    let mut deduped = 0;
    let mut ids = Vec::new();
    for t in tasks {
        let (status, text) = t.await.expect("concurrent upload task");
        assert!(
            status < 500,
            "a concurrent upload of the same file must never be a server error: {status} {text}"
        );
        let body: Value = serde_json::from_str(&text).expect("JSON body");
        match status {
            201 => created += 1,
            200 => deduped += 1,
            other => panic!("unexpected status {other}: {text}"),
        }
        ids.push(body["id"].clone());
    }
    assert_eq!(created, 1, "exactly one request may create the artifact");
    assert_eq!(
        deduped, 5,
        "the other five must all be told it already exists"
    );
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "every request must name the same artifact row: {ids:?}"
    );

    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(
        listed.as_array().map(|a| a.len()),
        Some(1),
        "six concurrent uploads of one file, one row: {listed}"
    );

    h.shutdown().await;
}

/// 8. **F2.** The escape hatch. `artifacts.rs`'s `Err(_) => None` arm is the one
///    place the implementation deliberately departs from the brief's `.ok()`, and
///    nothing exercised it, so deleting it — collapsing back to a 400 on the
///    count of derivations — was invisible to the suite.
///
///    What this pins: when the file's note cannot be read **but the caller
///    supplied an explicit `debug_id`**, the upload still succeeds, because a
///    toolchain whose note we cannot parse is precisely why the override param
///    exists. And `derived_debug_id` comes back `null`, which is the uploader's
///    only signal that nothing in the file corroborated the id they typed.
///
///    Note the pairing with test 4: byte-for-byte the same unparseable body, and
///    the presence or absence of `debug_id` is the whole difference between a 400
///    and a 201.
#[tokio::test]
async fn an_explicit_debug_id_keeps_an_unreadable_file_uploadable() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            &format!("kind=dart_symbols&platform=android&debug_id={SAMPLE_ELF_BUILD_ID}"),
            b"this is not an ELF file, it is a sentence",
        )
        .await;

    assert_eq!(
        status, 201,
        "an explicit debug_id must keep the upload open when the note is unreadable — that is \
         what the override is for: {body}"
    );
    assert_eq!(
        body["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the caller's id must be what gets used: {body}"
    );
    assert_eq!(
        body["derived_debug_id"],
        Value::Null,
        "nothing in the file corroborated the caller's id, and the response has to say so: {body}"
    );

    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(
        listed[0]["debug_id"], SAMPLE_ELF_BUILD_ID,
        "and it must be persisted, not merely echoed: {listed}"
    );

    h.shutdown().await;
}

/// 9a. **Option C.** An explicit `debug_id` is trimmed and lowercased before it
///     is stored. A human pasting `AB36…` or `%20ab36…` is the only
///     demonstrated failure mode left in this feature: both sides of the real
///     match are machine-generated lowercase hex, so a verbatim-stored variant
///     could never match anything, and nothing in the UI said why.
///
///     Expressed as a dedupe rather than as a string comparison, because that is
///     the falsifiable form. Before the fix this upload stored a *second* row
///     under `" AB36…A3 "` — a distinct key, so not even the UNIQUE index
///     objected — and returned 201. Dropping either the trim or the lowercase
///     reproduces that.
#[tokio::test]
async fn an_explicit_debug_id_is_trimmed_and_lowercased_before_it_is_stored() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;
    let base = "kind=dart_symbols&platform=android";

    let (first_status, first) = h
        .upload(
            f.app_id,
            &f.token,
            &format!("{base}&name=from-ci"),
            SAMPLE_ELF,
        )
        .await;
    assert_eq!(first_status, 201, "{first}");
    assert_eq!(first["debug_id"], SAMPLE_ELF_BUILD_ID, "{first}");

    // Padded and upper-cased, as a paste out of a build log would be. `name`
    // differs so the content-address fallback cannot produce the 200 either.
    let pasted = SAMPLE_ELF_BUILD_ID.to_ascii_uppercase();
    let (second_status, second) = h
        .upload(
            f.app_id,
            &f.token,
            &format!("{base}&name=by-hand&debug_id=%20{pasted}%20"),
            SAMPLE_ELF,
        )
        .await;

    assert_eq!(
        second_status, 200,
        "a padded, upper-cased paste of the same id must normalize to the same key and dedupe, \
         not create an unmatchable second row: {second}"
    );
    assert_eq!(
        second["debug_id"], SAMPLE_ELF_BUILD_ID,
        "the normalized value is what the response reports: {second}"
    );
    assert_eq!(second["id"], first["id"], "{second}");

    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(
        listed.as_array().map(|a| a.len()),
        Some(1),
        "one id in two spellings is still one artifact: {listed}"
    );

    h.shutdown().await;
}

/// 9b. **Option C, the rest of `blank_to_none`'s callers.** The helper is shared
///     by `release`, `dist`, `name` and `arch`, and it returned the untrimmed
///     string for all of them. `release` and `name` are compared verbatim by the
///     JS matcher, so a stored `" app.js.map"` is the same silent non-match the
///     `debug_id` case is — asserted here against `list`, which is what the
///     dashboard and the symbolicator read.
#[tokio::test]
async fn surrounding_whitespace_is_stripped_from_release_arch_and_name() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let map = br#"{"version":3,"sources":["app.ts"],"names":[],"mappings":"AAAA"}"#;
    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=js_sourcemap&platform=web&release=%201.0.0%20&name=%20app.js.map%20\
             &dist=%20nightly%20&arch=%20x86_64%20",
            map,
        )
        .await;
    assert_eq!(status, 201, "{body}");

    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(listed[0]["release"], "1.0.0", "{listed}");
    assert_eq!(listed[0]["name"], "app.js.map", "{listed}");
    assert_eq!(listed[0]["dist"], "nightly", "{listed}");
    assert_eq!(listed[0]["arch"], "x86_64", "{listed}");

    h.shutdown().await;
}

// ===========================================================================
// Normalization-symmetry round
// ===========================================================================

/// 10. **N3.** A dedupe 200's `blob_sha256` has to describe the artifact whose
///     `id` it returns, not the request that asked for it.
///
///     The Dart lookup matches on `debug_id` **alone** and never compares
///     content, so two different files uploaded under one explicit `?debug_id=`
///     (a copy-pasted id, or one id passed for two architectures) both resolve to
///     the first row. The second file is not stored — and the response used to
///     say `{id: <row A>, blob_sha256: <B's hash>, deduped: true}`, which reads
///     as "your bytes are stored under that artifact". They are not, and nothing
///     in the response, the list or the log said so.
///
///     Both bodies are deliberately non-ELF: the explicit `debug_id` is what
///     keeps them uploadable (test 8), and it keeps this test about *content
///     identity* rather than about ELF parsing.
#[tokio::test]
async fn a_dedupe_reports_the_stored_rows_blob_not_the_uploaded_one() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    let file_a: &[u8] = b"symbols for arm64 -- file A";
    let file_b: &[u8] = b"symbols for armv7 -- file B, entirely different bytes";
    let sha_a = sauron_symbols::hex(&sauron_symbols::sha256(file_a));
    let sha_b = sauron_symbols::hex(&sauron_symbols::sha256(file_b));
    assert_ne!(sha_a, sha_b, "the fixture files must differ");

    let query = format!("kind=dart_symbols&platform=android&debug_id={SAMPLE_ELF_BUILD_ID}");

    let (first_status, first) = h.upload(f.app_id, &f.token, &query, file_a).await;
    assert_eq!(first_status, 201, "{first}");
    assert_eq!(
        first["blob_sha256"], sha_a,
        "the created response describes the bytes it stored: {first}"
    );

    let (second_status, second) = h.upload(f.app_id, &f.token, &query, file_b).await;
    assert_eq!(second_status, 200, "same debug_id must dedupe: {second}");
    assert_eq!(second["id"], first["id"], "{second}");
    assert_eq!(
        second["blob_sha256"], sha_a,
        "the dedupe body must report the STORED row's blob so the uploader can see its own file \
         was not the one kept: {second}"
    );
    assert_ne!(
        second["blob_sha256"], sha_b,
        "reporting the request's own hash is the bug: it asserts B is stored under an artifact \
         that holds A: {second}"
    );

    // And the row itself is untouched — B was never stored under that id.
    let listed = h.list(f.app_id, &f.token).await;
    assert_eq!(listed.as_array().map(|a| a.len()), Some(1), "{listed}");
    assert_eq!(listed[0]["blob_sha256"], sha_a, "{listed}");

    h.shutdown().await;
}

/// A Dart trace whose single frame resolves to `compute_total` in `sample.elf`
/// (the same address `sauron-symbols`' own engine tests use). `build_id` is
/// interpolated so a test can make the header disagree with `debug_meta`.
fn dart_trace_with(build_id: &str) -> String {
    format!(
        "*** *** ***\n\
         build_id: '{build_id}'\n\
         isolate_dso_base: 0, vm_dso_base: 0\n\
         \x20   #00 abs 0000000000400446 virt 0000000000400446 \
         _kDartIsolateSnapshotInstructions+0x446\n"
    )
}

/// 11. **N1/N2, end to end.** The write path lowercases `debug_id`; if the read
///     path does not, an SDK reporting `AB36…` in `debug_meta.build_id` looks up a
///     key that cannot match the `ab36…` that was stored — and the only symptom is
///     `no_artifacts`, which is what "nobody ever uploaded symbols" looks like
///     too. Uploaded here, reported uppercase, and required to resolve.
///
///     Deliberately a real symbolication (upload → stored row → event → DWARF →
///     `compute_total`) and not a string assertion: the unit tests in
///     `sauron-symbols` can only pin what the engine hands its `BlobFetch`, while
///     the failure being fixed is an equality test inside Postgres.
///
///     The trace's own `build_id:` header is set to a value that matches nothing,
///     so the `debug_meta` value — the untrusted one the finding named — is the
///     only thing that can produce the match. (The header's own normalization is
///     pinned by `an_uppercase_build_id_in_the_trace_itself_is_normalized_too` in
///     `sauron-symbols`.)
#[tokio::test]
async fn an_uppercase_reported_build_id_symbolicates_against_the_stored_artifact() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_artifacts");
        return;
    };
    let f = h.seed_artifacts_fixture().await;

    // 1. Upload the symbols. The stored `debug_id` is the derived, lowercase id.
    let (status, body) = h
        .upload(
            f.app_id,
            &f.token,
            "kind=dart_symbols&platform=android&arch=arm64",
            SAMPLE_ELF,
        )
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["debug_id"], SAMPLE_ELF_BUILD_ID, "{body}");

    // 2. An unsymbolicated crash whose reported build-id is the SAME id in the
    //    other case — the shape a toolchain or SDK that upper-cases hex produces.
    let reported = SAMPLE_ELF_BUILD_ID.to_ascii_uppercase();
    let now = Utc::now();
    let fingerprint = format!("dart-upper-{}", Uuid::new_v4().simple());
    let issue_id = {
        let mut conn = h.conn().await;
        let issue_id = repo::upsert_issue(
            &mut conn,
            NewIssue {
                app_id: f.app_id,
                fingerprint: &fingerprint,
                type_: "error",
                title: "StateError",
                culprit: "main.dart",
                level: "error",
                first_seen: now,
                last_seen: now,
                times_seen: 1,
            },
        )
        .await
        .expect("issue");
        repo::insert_error_event(
            &mut conn,
            NewErrorEvent {
                id: Uuid::new_v4(),
                app_id: f.app_id,
                environment_id: None,
                issue_id,
                fingerprint: fingerprint.clone(),
                level: "error".into(),
                message: "Bad state".into(),
                exception_type: "StateError".into(),
                exception_value: "Bad state".into(),
                // The Dart path reads the verbatim trace out of `debug_meta`;
                // `stacktrace` stays empty exactly as real Flutter ingest leaves it.
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
                stacktrace_symbolicated: None,
                symbolication_status: "pending".into(),
                debug_meta: Some(json!({
                    "build_id": reported,
                    "isolate_dso_base": "0",
                    "arch": "arm64",
                    "os": "android",
                    "raw_stacktrace": dart_trace_with("a-header-that-matches-nothing"),
                })),
                contexts: json!({}),
                extra: json!({}),
                handled: Some(false),
                title: Some("StateError".into()),
                culprit: Some("main.dart".into()),
            },
        )
        .await
        .expect("error event");
        issue_id
    };

    // 3. The issue detail route symbolicates `latest_event` on read.
    let detail: Value = h
        .client
        .get(format!(
            "{}/v1/apps/{}/issues/{}",
            h.base, f.app_id, issue_id
        ))
        .bearer_auth(&f.token)
        .send()
        .await
        .expect("issue detail")
        .json()
        .await
        .expect("issue detail body");

    let event = &detail["latest_event"];
    assert_eq!(
        event["symbolication_status"], "symbolicated",
        "an uppercase reported build_id must resolve against the lowercase stored id — a status \
         still reading `pending` (or `no_artifacts`) here, with null frames, is exactly what the \
         write-only normalization produced: {detail}"
    );
    let frames = event["stacktrace_symbolicated"]
        .as_array()
        .unwrap_or_else(|| panic!("expected symbolicated frames: {detail}"));
    assert!(
        frames
            .iter()
            .any(|fr| fr["function"] == "compute_total" && fr["symbolicated"] == json!(true)),
        "the frame must be resolved through the uploaded ELF's DWARF: {detail}"
    );

    h.shutdown().await;
}
