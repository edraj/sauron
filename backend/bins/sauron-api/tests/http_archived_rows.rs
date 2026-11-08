//! A person's profile and a session's timeline read BOTH tiers.
//!
//! The regression this pins: on a deployment running `tier.hot_days = 2`, a
//! three-day-old person opened on "0 events, 0 errors, no activity" while the
//! Users list counted 21 events and 2 errors for them — the timeline read
//! Postgres alone, the tier worker had moved that day to Parquet, and the
//! tiles were the timeline's row count. Sessions had the same hole: raw
//! sessions are kept longer than their events.
//!
//! Driven end to end with nothing simulated that the product does for real:
//! rows are written to Postgres, exported by the tier worker's own
//! `DuckEngine::export_from_postgres` (so the Parquet schema is the real one —
//! JSONB, UUIDs, timestamptz, the hive-stripped `app_id`), deleted from
//! Postgres the way a partition drop removes them, and the watermark is set.
//! Then a real `sauron-api` process answers, and each page must show exactly
//! what it showed before the data moved.
//!
//! Skips (prints, returns) when `TEST_DATABASE_URL` / `TEST_REDIS_URL` are
//! unset, like every `http_*` test here. A run that took 0.00s skipped.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, DurationRound, Utc};
use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{perm, JwtKeys};
use sauron_db::models::{
    NewAnalyticsEvent, NewAppEnvironment, NewErrorEvent, NewIssue, NewRoleGrant, NewTransaction,
};
use sauron_db::repo;
use sauron_tier::duck::DuckEngine;

const JWT_SECRET: &str = "http-archived-rows-test-secret-0000000000000";

fn swap_database(url: &str, new_db: &str) -> String {
    let (base, query) = match url.find('?') {
        Some(i) => (&url[..i], &url[i..]),
        None => (url, ""),
    };
    let slash = base.rfind('/').expect("database url has a path");
    format!("{}/{}{}", &base[..slash], new_db, query)
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

/// A migrated ephemeral database, a cold directory, and a `sauron-api` process
/// pointed at both. Same shape as `http_release_scoping.rs`'s, plus
/// `TIER_COLD_PATH`.
struct TestServer {
    child: tokio::process::Child,
    base: String,
    client: reqwest::Client,
    admin_url: String,
    db_name: String,
    db_url: String,
    cold: std::path::PathBuf,
    pool: sauron_db::PgPool,
    cleaned_up: Cell<bool>,
}

impl TestServer {
    async fn start() -> Option<TestServer> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("TEST_REDIS_URL").ok()?;
        let db_name = format!(
            "sauron_test_{}_arch{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::create_test_database(&admin_url, &db_name)
            .await
            .expect("create migrated ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");
        let cold = std::env::temp_dir().join(format!("sauron-archived-rows-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&cold).expect("create cold dir");

        let port = free_port();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_sauron-api"))
            .env("DATABASE_URL", &db_url)
            .env("REDIS_URL", &redis_url)
            .env("JWT_SECRET", JWT_SECRET)
            .env(
                "NOTIFY_SECRET_KEY",
                "sauron-test-notify-secret-key-0000000000",
            )
            .env("API_PORT", port.to_string())
            .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
            .env("TIER_COLD_PATH", &cold)
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
            db_url,
            cold,
            pool,
            cleaned_up: Cell::new(false),
        })
    }

    async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }

    async fn get_json(&self, path: &str, token: &str) -> Value {
        let resp = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("request to {path} failed: {e}"));
        let status = resp.status();
        let text = resp.text().await.expect("body");
        assert!(status.is_success(), "GET {path} -> {status}: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("GET {path}: not JSON ({e}): {text}"))
    }

    async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        let _ = std::fs::remove_dir_all(&self.cold);
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
                "WARNING: ephemeral test database {} may remain. DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

async fn exec(conn: &mut sauron_db::PgConn, sql: &str) {
    diesel::sql_query(sql)
        .execute(conn)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Ids every row in the fixture is known by, so assertions read as the story.
struct Fixture {
    app: Uuid,
    prod: Uuid,
    token: String,
    /// Start of the archived day; every "old" row is on it.
    day: DateTime<Utc>,
    watermark: DateTime<Utc>,
    /// p1, prod, session s1, archived.
    e1: Uuid,
    /// p1, prod, session s1, archived — AND left in Postgres, as a partition
    /// still inside its drop grace is: must appear once, not twice.
    e2: Uuid,
    /// p1, prod, no session, archived.
    e3: Uuid,
    /// The guest `g1`, merged into p1 after it was archived: the archive still
    /// says `g1`.
    guest: Uuid,
    /// The guest `g1` INSIDE session s1, archived: on the session page it must
    /// read as p1 once the merge has completed, like its Postgres siblings.
    guest_s1: Uuid,
    /// p1, prod, recent: never archived.
    recent: Uuid,
    /// p1 in STAGING, archived: out of a prod-scoped read.
    staging: Uuid,
    /// Another person, archived: out of p1's profile.
    other: Uuid,
    /// p1's errors, prod, archived (x1 in session s1).
    x1: Uuid,
    x2: Uuid,
    /// p1's transaction in session s1, archived.
    tx1: Uuid,
}

#[allow(clippy::too_many_arguments)]
async fn event(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    distinct_id: &str,
    session: Option<&str>,
    at: DateTime<Utc>,
    name: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    repo::insert_analytics_event(
        c,
        NewAnalyticsEvent {
            id,
            app_id: app,
            environment_id: Some(env),
            name: name.to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({ "nested": { "list": [1, 2, 3] }, "label": name }),
            context: json!({ "os": { "name": "Android" } }),
            session_id: session.map(str::to_string),
            release: Some("v2.0.71".into()),
            ip_address: Some("10.1.2.3".into()),
            occurred_at: at,
            device_key: Some("dev-1".into()),
            screen: Some("Home".into()),
            workflow_id: None,
            workflow_name: None,
            tags: json!({ "t": "v" }),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert analytics event");
    id
}

async fn error(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    session: Option<&str>,
    at: DateTime<Utc>,
) -> Uuid {
    let issue_id = repo::upsert_issue(
        c,
        NewIssue {
            app_id: app,
            fingerprint: "archived-fixture",
            type_: "StateError",
            title: "archived fixture error",
            culprit: "fixture",
            level: "error",
            first_seen: at,
            last_seen: at,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue");
    let id = Uuid::new_v4();
    repo::insert_error_event(
        c,
        NewErrorEvent {
            id,
            app_id: app,
            environment_id: Some(env),
            issue_id,
            fingerprint: "archived-fixture".into(),
            level: "error".into(),
            message: "boom".into(),
            exception_type: "StateError".into(),
            exception_value: "boom".into(),
            stacktrace: json!([{ "function": "main", "lineno": 7 }]),
            breadcrumbs: json!([{ "message": "tap" }]),
            context: json!({}),
            tags: json!({}),
            release: Some("v2.0.71".into()),
            distinct_id: Some("p1".into()),
            event_user: Some(json!({ "id": "p1" })),
            sdk: Some(json!({ "name": "sauron.flutter" })),
            ip_address: None,
            occurred_at: at,
            session_id: session.map(str::to_string),
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_needed".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(false),
            title: Some("StateError: boom".into()),
            culprit: Some("main".into()),
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");
    id
}

impl TestServer {
    async fn seed(&self) -> Fixture {
        let mut c = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut c, "arch org", &format!("arch-org-{s}"))
            .await
            .unwrap();
        let project = repo::create_project(&mut c, org.id, "arch", &format!("arch-p-{s}"))
            .await
            .unwrap();
        let app = repo::create_app(&mut c, project.id, "client", &format!("arch-a-{s}"), "web")
            .await
            .unwrap();
        let mut env_ids = Vec::new();
        for (name, default) in [("production", true), ("staging", false)] {
            let pk = format!("pk_{name}_{s}");
            let env = repo::create_project_environment(&mut c, project.id, name)
                .await
                .unwrap();
            let id = repo::create_app_environments(
                &mut c,
                &[NewAppEnvironment {
                    app_id: app.id,
                    environment_id: env.id,
                    public_key: &pk,
                    is_default: default,
                }],
            )
            .await
            .unwrap()
            .remove(0)
            .id;
            env_ids.push(id);
        }
        let (prod, stg) = (env_ids[0], env_ids[1]);

        let user = repo::create_user(&mut c, &format!("arch-{s}@example.test"), "x", "U")
            .await
            .unwrap();
        let role = repo::create_role(
            &mut c,
            org.id,
            "arch role",
            "read",
            json!([perm::EVENT_READ, perm::ISSUE_READ]),
        )
        .await
        .unwrap();
        repo::create_grant(
            &mut c,
            NewRoleGrant {
                org_id: org.id,
                user_id: user.id,
                role_id: role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .unwrap();
        let (token, _) = JwtKeys::new(JWT_SECRET, 900)
            .issue_access(user.id, false, None)
            .unwrap();

        let today = Utc::now().duration_trunc(ChronoDuration::days(1)).unwrap();
        let day = today - ChronoDuration::days(10);
        let watermark = today - ChronoDuration::days(2);
        let h = |n: i64| day + ChronoDuration::minutes(n);
        let a = app.id;

        let e1 = event(&mut c, a, prod, "p1", Some("s1"), h(60), "e1").await;
        let e2 = event(&mut c, a, prod, "p1", Some("s1"), h(120), "e2").await;
        let e3 = event(&mut c, a, prod, "p1", None, h(300), "e3").await;
        let guest = event(&mut c, a, prod, "g1", None, h(180), "guest").await;
        let guest_s1 = event(&mut c, a, prod, "g1", Some("s1"), h(100), "guest-s1").await;
        let recent = event(
            &mut c,
            a,
            prod,
            "p1",
            None,
            Utc::now() - ChronoDuration::minutes(30),
            "recent",
        )
        .await;
        let staging = event(&mut c, a, stg, "p1", None, h(240), "staging").await;
        let other = event(&mut c, a, prod, "p2", None, h(360), "other").await;
        let x1 = error(&mut c, a, prod, Some("s1"), h(90)).await;
        let x2 = error(&mut c, a, prod, None, h(420)).await;
        let tx1 = Uuid::new_v4();
        repo::insert_transaction(
            &mut c,
            NewTransaction {
                id: tx1,
                app_id: a,
                environment_id: Some(prod),
                name: "GET /cart".into(),
                op: "http.client".into(),
                duration_ms: 123.5,
                status: Some("ok".into()),
                http_method: Some("GET".into()),
                http_status: Some(200),
                url: Some("https://api.example.test/cart".into()),
                distinct_id: Some("p1".into()),
                session_id: Some("s1".into()),
                device_key: None,
                workflow_id: None,
                workflow_name: None,
                release: Some("v2.0.71".into()),
                ip_address: None,
                occurred_at: h(75),
                finished_at: Some(h(75) + ChronoDuration::milliseconds(123)),
                tags: json!({}),
                extra: json!({ "k": 1 }),
            },
        )
        .await
        .expect("insert transaction");

        // The non-tiered side the product keeps: the session row, the person,
        // and the person rollup the Users list reads (with its backfill marker,
        // which is what routes the profile summary to it).
        diesel::sql_query(
            "INSERT INTO sessions (id, app_id, session_id, distinct_id, started_at, last_event_at, \
                                   events_count, errors_count, unhandled_errors_count, context, \
                                   environment_id) \
             VALUES (gen_random_uuid(), $1, 's1', 'p1', $2, $3, 2, 1, 1, '{}'::jsonb, $4)",
        )
        .bind::<SqlUuid, _>(a)
        .bind::<Timestamptz, _>(h(60))
        .bind::<Timestamptz, _>(h(120))
        .bind::<SqlUuid, _>(prod)
        .execute(&mut c)
        .await
        .expect("insert session");
        repo::upsert_event_user(&mut c, a, "p1", &json!({ "plan": "gold" }))
            .await
            .unwrap();
        repo::upsert_event_user(&mut c, a, "p2", &json!({}))
            .await
            .unwrap();
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, \
                events_count, errors_count, sessions_count) \
             VALUES ($1, 'p1', $2, $3, $4, 21, 2, 1), ($1, 'p1', $5, $6, $6, 1, 0, 0)",
        )
        .bind::<SqlUuid, _>(a)
        .bind::<SqlUuid, _>(prod)
        .bind::<Timestamptz, _>(h(60))
        .bind::<Timestamptz, _>(Utc::now() - ChronoDuration::minutes(30))
        .bind::<SqlUuid, _>(stg)
        .bind::<Timestamptz, _>(h(240))
        .execute(&mut c)
        .await
        .expect("insert person rollup");
        exec(
            &mut c,
            &format!(
                "INSERT INTO event_user_env_backfill (app_id, completed_at) VALUES ('{a}', now())"
            ),
        )
        .await;

        Fixture {
            app: a,
            prod,
            token,
            day,
            watermark,
            e1,
            e2,
            e3,
            guest,
            guest_s1,
            recent,
            staging,
            other,
            x1,
            x2,
            tx1,
        }
    }

    /// What the tier worker does to the archived day, for real: export it with
    /// the worker's own engine, drop it from Postgres (keeping `keep`, a row
    /// whose partition is still inside its drop grace), advance the watermark.
    /// Then the guest merge that happened AFTER the export: `g1` → `p1`.
    async fn tier_out(&self, f: &Fixture, keep: Uuid) {
        let (url, cold) = (self.db_url.clone(), self.cold.display().to_string());
        let (start, end) = (f.day, f.day + ChronoDuration::days(1));
        tokio::task::spawn_blocking(move || {
            let eng = DuckEngine::open().expect("open duckdb");
            for table in ["analytics_events", "error_events", "transactions"] {
                eng.export_from_postgres(&url, table, start, end, &format!("{cold}/{table}"))
                    .unwrap_or_else(|e| panic!("export {table}: {e:#}"));
            }
        })
        .await
        .unwrap();
        let mut c = self.conn().await;
        for table in ["analytics_events", "error_events", "transactions"] {
            diesel::sql_query(format!(
                "DELETE FROM {table} WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
                   AND id <> $4"
            ))
            .bind::<SqlUuid, _>(f.app)
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(end)
            .bind::<SqlUuid, _>(keep)
            .execute(&mut c)
            .await
            .unwrap_or_else(|e| panic!("drop {table}: {e}"));
            diesel::sql_query(
                "INSERT INTO tiering_state (table_name, watermark, dropped_thru, updated_at) \
                 VALUES ($1, $2, $2, now()) \
                 ON CONFLICT (table_name) DO UPDATE SET watermark = $2, dropped_thru = $2",
            )
            .bind::<Text, _>(table)
            .bind::<Timestamptz, _>(f.watermark)
            .execute(&mut c)
            .await
            .unwrap();
        }
        diesel::sql_query(
            "INSERT INTO identity_merges (id, app_id, alias_id, distinct_id, state, attempts, \
                 alias_first_seen, alias_last_seen, cold_stale, next_attempt_at, created_at, completed_at) \
             VALUES (gen_random_uuid(), $1, 'g1', 'p1', 'done', 1, $2, $2, true, now(), now(), now())",
        )
        .bind::<SqlUuid, _>(f.app)
        .bind::<Timestamptz, _>(f.day)
        .execute(&mut c)
        .await
        .expect("record the merge");
    }
}

fn ids_of(rows: &Value) -> Vec<String> {
    rows.as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().expect("id").to_string())
        .collect()
}

fn set(ids: &[Uuid]) -> BTreeSet<String> {
    ids.iter().map(Uuid::to_string).collect()
}

fn session_kinds(timeline: &Value) -> Vec<(String, String)> {
    timeline["timeline"]
        .as_array()
        .expect("timeline array")
        .iter()
        .map(|item| {
            let kind = item["kind"].as_str().unwrap().to_string();
            let body = &item[match kind.as_str() {
                "event" => "event",
                "error" => "error",
                _ => "transaction",
            }];
            (kind, body["id"].as_str().unwrap().to_string())
        })
        .collect()
}

#[tokio::test]
async fn profile_and_session_pages_read_the_archive_like_postgres() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping");
        return;
    };
    let f = srv.seed().await;
    let person = format!(
        "/v1/apps/{}/persons/p1/timeline?environment_id={}&limit=100",
        f.app, f.prod
    );
    let summary = format!(
        "/v1/apps/{}/persons/p1/summary?environment_id={}",
        f.app, f.prod
    );
    let session = format!(
        "/v1/apps/{}/sessions/s1/timeline?environment_id={}",
        f.app, f.prod
    );

    // Before anything moves: the pages as Postgres alone answers them.
    let before = srv.get_json(&person, &f.token).await;
    let session_before = srv.get_json(&session, &f.token).await;
    assert_eq!(
        ids_of(&before["events"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        set(&[f.e1, f.e2, f.e3, f.recent]),
        "fixture sanity: p1's prod events in Postgres"
    );
    assert_eq!(
        session_kinds(&session_before).len(),
        5,
        "fixture sanity: s1 has five rows"
    );

    srv.tier_out(&f, f.e2).await;

    // ---- the person profile -------------------------------------------------
    let after = srv.get_json(&person, &f.token).await;
    let events = ids_of(&after["events"]);
    assert_eq!(
        events.iter().cloned().collect::<BTreeSet<_>>(),
        set(&[f.e1, f.e2, f.e3, f.recent, f.guest, f.guest_s1]),
        "every prod event of p1 survives tiering, plus the merged guest's archived rows; \
         staging ({}) and another person ({}) stay out",
        f.staging,
        f.other,
    );
    assert_eq!(events.len(), 6, "e2 is in BOTH tiers and must appear once");
    assert_eq!(after["archive_incomplete"], false);
    let ats: Vec<DateTime<Utc>> = after["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["occurred_at"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(
        ats.windows(2).all(|w| w[0] >= w[1]),
        "newest first: {ats:?}"
    );
    for e in after["events"].as_array().unwrap() {
        assert_eq!(
            e["distinct_id"], "p1",
            "a guest row reads as the person, like the hot rewrite"
        );
    }
    // The archived row is the same row, field for field, as Postgres served it.
    let e3_before = before["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == f.e3.to_string())
        .unwrap();
    let e3_after = after["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == f.e3.to_string())
        .unwrap();
    assert_eq!(
        e3_before, e3_after,
        "an archived event must serialize exactly like its hot self"
    );

    assert_eq!(
        ids_of(&after["errors"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        set(&[f.x1, f.x2]),
    );
    let x2_before = before["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == f.x2.to_string())
        .unwrap();
    let x2_after = after["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == f.x2.to_string())
        .unwrap();
    assert_eq!(
        x2_before, x2_after,
        "an archived error must serialize exactly like its hot self"
    );

    // The guest id is not a person of its own any more, in either tier.
    let guest = srv
        .get_json(
            &format!(
                "/v1/apps/{}/persons/g1/timeline?environment_id={}",
                f.app, f.prod
            ),
            &f.token,
        )
        .await;
    assert!(
        guest["events"].as_array().unwrap().is_empty(),
        "g1's archived row belongs to p1: {guest}"
    );

    // The tiles' source: the rollup, as the Users list reads it — not a count
    // of whatever rows Postgres still holds (which would say 2 events here).
    let s = srv.get_json(&summary, &f.token).await;
    assert_eq!(s["user"]["events_count"], 21, "{s}");
    assert_eq!(s["user"]["errors_count"], 2, "{s}");
    assert_eq!(s["user"]["sessions_count"], 1, "{s}");

    // ---- the session timeline ------------------------------------------------
    let session_after = srv.get_json(&session, &f.token).await;
    assert_eq!(
        session_kinds(&session_after),
        vec![
            ("event".to_string(), f.e1.to_string()),
            ("transaction".to_string(), f.tx1.to_string()),
            ("error".to_string(), f.x1.to_string()),
            ("event".to_string(), f.guest_s1.to_string()),
            ("event".to_string(), f.e2.to_string()),
        ],
        "oldest first, all three tables, e2 once",
    );
    // Byte-identical across tiering, except the one thing the merge changed:
    // the guest's row now reads as the person, as its Postgres siblings would.
    let mut expected = session_before.clone();
    for item in expected["timeline"].as_array_mut().unwrap() {
        if item["kind"] == "event" && item["event"]["id"] == f.guest_s1.to_string() {
            assert_eq!(item["event"]["distinct_id"], "g1", "fixture sanity");
            item["event"]["distinct_id"] = "p1".into();
        }
    }
    assert_eq!(
        session_after, expected,
        "the session timeline across tiering"
    );

    // ---- an unreadable archive degrades, it does not take the page down ----
    // A truncated file the tier worker has not quarantined yet, in the
    // archived month of analytics_events only.
    let month_dir = srv.cold.join(format!(
        "analytics_events/app_id={}/year={}/month={}",
        f.app,
        f.day.format("%Y"),
        f.day.format("%-m")
    ));
    assert!(
        month_dir.is_dir(),
        "fixture sanity: {}",
        month_dir.display()
    );
    std::fs::write(month_dir.join("truncated.parquet"), b"").unwrap();

    let degraded = srv.get_json(&person, &f.token).await;
    assert_eq!(degraded["archive_incomplete"], true, "{degraded}");
    assert_eq!(
        ids_of(&degraded["events"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        set(&[f.e2, f.recent]),
        "events fall back to what Postgres holds"
    );
    assert_eq!(
        ids_of(&degraded["errors"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        set(&[f.x1, f.x2]),
        "another table's archive is unaffected"
    );
    let session_degraded = srv.get_json(&session, &f.token).await;
    assert_eq!(session_degraded["archive_incomplete"], true);
    assert_eq!(
        session_kinds(&session_degraded),
        vec![
            ("transaction".to_string(), f.tx1.to_string()),
            ("error".to_string(), f.x1.to_string()),
            ("event".to_string(), f.e2.to_string()),
        ],
    );

    srv.shutdown().await;
}
