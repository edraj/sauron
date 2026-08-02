//! Boot-time behaviour of the transactional-email wiring, driven against the
//! real compiled `sauron-api` binary on an ephemeral, migrated database.
//!
//! The regression these three cases exist for: bailing in `Config::from_env` on
//! a missing setting once took down `sauron-ingest` and `sauron-tier`. Every
//! configuration below must leave the API booting and serving.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-mail-outbox-test-secret-0000000000000";

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

fn ephemeral_db_name() -> String {
    format!(
        "sauron_mailtest_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )
}

/// Boot the binary with `extra_env` on top of the minimum, poll `/health` until
/// it answers, return the parsed body, then tear everything down.
///
/// Returns `None` when the test environment is not configured, so the caller can
/// skip rather than fail.
async fn health_body_with(extra_env: &[(&str, &str)]) -> Option<Value> {
    let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
    let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

    let db_name = ephemeral_db_name();
    sauron_db::create_database(&admin_url, &db_name)
        .await
        .expect("create ephemeral test database");
    let db_url = swap_database(&admin_url, &db_name);
    sauron_db::run_pending_migrations(&db_url)
        .await
        .expect("run migrations");

    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_sauron-api");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env("DATABASE_URL", &db_url)
        .env("REDIS_URL", &redis_url)
        .env("JWT_SECRET", JWT_SECRET)
        .env("API_PORT", port.to_string())
        .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn sauron-api binary");

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let mut body: Option<Value> = None;
    for _ in 0..100 {
        if let Ok(Some(status)) = child.try_wait() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = s.read_to_string(&mut stderr).await;
            }
            panic!("sauron-api exited early with {status}; stderr:\n{stderr}");
        }
        if let Ok(resp) = client
            .get(format!("{base}/health"))
            .timeout(Duration::from_millis(200))
            .send()
            .await
        {
            if resp.status().is_success() {
                body = resp.json::<Value>().await.ok();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = child.kill().await;
    sauron_db::drop_database(&admin_url, &db_name)
        .await
        .expect("drop ephemeral test database");

    Some(body.expect("/health never returned a successful JSON body"))
}

/// The hygiene task must run on a deployment that has never configured a relay:
/// it is the control that bounds credential-at-rest, and gating it on the feature
/// being switched on inverts it.
#[tokio::test]
async fn health_lists_hygiene_even_with_no_smtp_at_all() {
    let Some(body) = health_body_with(&[]).await else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    let names: Vec<&str> = body["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"mail_hygiene"), "got: {names:?}");
    assert!(
        !names.contains(&"mail_drain"),
        "the drain must not mount without a relay: {names:?}"
    );
    // Never a non-2xx and never a missing field: SETUP.md documents
    // `curl -fsS .../health` and http_env_scoping.rs polls it for readiness.
    let first = &body["tasks"][0];
    assert!(first["last_success_secs"].is_null() || first["last_success_secs"].is_u64());
    assert!(first["consecutive_failures"].is_u64());
}

#[tokio::test]
async fn sink_without_a_host_boots_and_mounts_the_drain() {
    let Some(body) = health_body_with(&[("SMTP_SINK", "1")]).await else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    let names: Vec<&str> = body["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"mail_drain"), "got: {names:?}");
    assert!(names.contains(&"mail_hygiene"), "got: {names:?}");
}

#[tokio::test]
async fn sink_with_a_from_address_boots_the_same_way() {
    let Some(body) =
        health_body_with(&[("SMTP_SINK", "1"), ("SMTP_FROM", "sauron@corp.test")]).await
    else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    assert!(body["tasks"].as_array().expect("tasks array").len() >= 2);
}
