//! Per-event CPU baseline for the ingest write path.
//!
//! Measures the work that the batching / pass-removal optimisations would
//! delete, so a later run can be diffed against this one:
//!
//!   1. `envelope_parse`   — edge, `from_slice::<Envelope>`      (main.rs:247)
//!   2. `job_serialize`    — edge, `to_string(&IngestJob)`       (main.rs:285)
//!   3. `job_parse`        — worker, `from_str::<IngestJob>`     (worker.rs:176)
//!   4. `job_reserialize`  — worker, `to_string(&job)` for the dead-letter.
//!      No longer on the steady-state path: it moved into `process_one_by_one`,
//!      so the batch path pays it zero times per event instead of once.
//!
//! plus `fingerprint` (every error event) and the `to_value`/`from_value`
//! round-trip `mask::apply_wire` performs — but ONLY when an app actually has
//! masked keys configured, since `apply_wire` returns early on an empty set.
//!
//! Deliberately harness-free: adding criterion would pull ~30 crates into the
//! lockfile for a measurement, and the quantities here differ by enough that
//! median-of-N is sufficient to act on.
//!
//! Run: `cargo bench -p sauron-core --bench json_tax`

use std::hint::black_box;
use std::time::Instant;

use chrono::Utc;
use sauron_core::envelope::{Envelope, IngestJob};
use sauron_core::fingerprint::fingerprint;
use uuid::Uuid;

/// One realistic error envelope, mirroring `crebain::generator::issue_envelope`
/// — two items (breadcrumb batch + error), rich `contexts`/`extra`, a 2-frame
/// stack. Sizes matter here, so this must stay representative of what the SDKs
/// actually emit.
const ENVELOPE_JSON: &str = r#"{
  "header": {
    "dsn": "https://pk_test@localhost:8081/1",
    "sdk": { "name": "sauron.javascript", "version": "0.3.0" },
    "sent_at": "2026-08-03T10:00:00Z",
    "release": "web@1.4.2"
  },
  "context": {
    "device": { "type": "desktop", "model": "generic", "arch": "x86_64" },
    "os": { "name": "Linux", "version": "6.9.0" },
    "app": { "name": "storefront", "version": "1.4.2", "build": "4821" },
    "runtime": { "name": "chrome", "version": "128.0.0" },
    "user": null
  },
  "items": [
    {
      "type": "breadcrumb_batch",
      "distinct_id": "crebain-user-417",
      "session_id": "0f2a5c1e-9d3b-4a77-8c21-6b4e5f0a1d33",
      "breadcrumbs": [
        { "type": "navigation", "category": "ui.route", "message": "/checkout", "level": "info", "timestamp": "2026-08-03T09:59:58Z", "data": { "from": "/cart", "to": "/checkout" } },
        { "type": "http", "category": "fetch", "message": "POST /api/cart", "level": "info", "timestamp": "2026-08-03T09:59:59Z", "data": { "status": 200, "duration_ms": 84 } },
        { "type": "ui.click", "category": "ui", "message": "button#place-order", "level": "info", "timestamp": "2026-08-03T10:00:00Z", "data": { "selector": "button#place-order" } }
      ]
    },
    {
      "type": "error",
      "event_id": "8b1d4f60-1c2e-4a55-9f77-2a3b4c5d6e7f",
      "level": "error",
      "timestamp": "2026-08-03T10:00:00Z",
      "exception": {
        "type": "TypeError",
        "value": "Cannot read properties of undefined (reading 'total')",
        "mechanism": { "type": "onerror", "handled": false },
        "stacktrace": [
          { "function": "main", "module": "app", "filename": "main.js", "lineno": 10, "colno": 1, "in_app": true },
          { "function": "handleRequest", "module": "app.server", "filename": "server.js", "lineno": 47, "colno": 5, "in_app": true }
        ]
      },
      "message": null,
      "breadcrumbs": [
        { "type": "navigation", "category": "ui.route", "message": "/checkout", "level": "info", "timestamp": "2026-08-03T09:59:58Z", "data": { "from": "/cart" } },
        { "type": "http", "category": "fetch", "message": "POST /api/cart", "level": "info", "timestamp": "2026-08-03T09:59:59Z", "data": { "status": 200 } }
      ],
      "tags": { "screen": "checkout" },
      "contexts": {
        "issue": { "seq": 4711, "request": { "id": "req-417-4711", "region": "eu-west-1" } },
        "plan": "growth",
        "locale": "en-GB",
        "payment_method": "card",
        "app_version": "1.4.2",
        "ab_variant": "b",
        "feature_flag_dark_mode": true,
        "device_type": "desktop",
        "tags": ["growth", "eu-west-1"]
      },
      "extra": {
        "lineno": 47,
        "order_id": "order-417-4711",
        "cart_value_cents": 8651,
        "item_count": 12,
        "latency_bucket_ms": 275,
        "retry_count": 2,
        "plan": "scale",
        "region": "us-east-1",
        "payment_method": "paypal",
        "app_version": "1.4.1",
        "feature_flag_checkout_v2": true
      },
      "fingerprint": null,
      "user": null,
      "session_id": "0f2a5c1e-9d3b-4a77-8c21-6b4e5f0a1d33",
      "screen": "checkout"
    }
  ]
}"#;

/// Median + mean nanoseconds per iteration, after a warmup.
fn bench(name: &str, iters: usize, mut f: impl FnMut()) -> f64 {
    for _ in 0..(iters / 10).max(100) {
        f();
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_nanos() as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[samples.len() / 2];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!("{name:<28} median {median:>9.0} ns   mean {mean:>9.0} ns");
    median
}

fn main() {
    let body = ENVELOPE_JSON.as_bytes();
    println!("envelope wire size: {} bytes, 2 items\n", body.len());

    // The edge, verbatim: parse the envelope, then build + serialize one job
    // per item.
    let envelope: Envelope = serde_json::from_slice(body).expect("golden envelope must parse");
    let jobs: Vec<IngestJob> = envelope
        .items
        .iter()
        .cloned()
        .map(|item| IngestJob {
            app_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            release: envelope.header.release.clone(),
            received_at: Utc::now(),
            ip: Some("203.0.113.9".to_string()),
            user_agent: Some(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/128.0.0.0".to_string(),
            ),
            context: envelope.context.clone(),
            sdk: Some(envelope.header.sdk.clone()),
            item,
        })
        .collect();
    let job_payloads: Vec<String> = jobs
        .iter()
        .map(|j| serde_json::to_string(j).unwrap())
        .collect();
    let queued_bytes: usize = job_payloads.iter().map(|p| p.len()).sum();
    println!(
        "queued job payloads: {queued_bytes} bytes across {} jobs\n",
        jobs.len()
    );

    println!("--- the four JSON passes, per ENVELOPE (2 items) ---");
    let p1 = bench("1 envelope_parse (edge)", 20_000, || {
        let e: Envelope = serde_json::from_slice(black_box(body)).unwrap();
        black_box(e);
    });
    let p2 = bench("2 job_serialize (edge)", 20_000, || {
        for j in &jobs {
            black_box(serde_json::to_string(black_box(j)).unwrap());
        }
    });
    let p3 = bench("3 job_parse (worker)", 20_000, || {
        for p in &job_payloads {
            let j: IngestJob = serde_json::from_str(black_box(p)).unwrap();
            black_box(j);
        }
    });
    let p4 = bench("4 job_reserialize (worker)", 20_000, || {
        for j in &jobs {
            black_box(serde_json::to_string(black_box(j)).unwrap());
        }
    });
    let total = p1 + p2 + p3 + p4;
    println!(
        "\nJSON tax per envelope: {total:.0} ns  ({:.0} ns/item)",
        total / jobs.len() as f64
    );
    println!(
        "  of which pass 4 (dead-letter that usually never fires): {p4:.0} ns = {:.1}%",
        100.0 * p4 / total
    );

    println!("\n--- per-error-event work ---");
    let err = jobs
        .iter()
        .find_map(|j| match &j.item {
            sauron_core::envelope::EnvelopeItem::Error(e) => Some(e.clone()),
            _ => None,
        })
        .expect("fixture carries an error item");
    bench("fingerprint", 50_000, || {
        black_box(fingerprint(
            black_box(err.exception.as_ref()),
            black_box(err.message.as_deref()),
            black_box(err.fingerprint.as_deref()),
        ));
    });

    println!("\n--- mask::apply_wire round-trip (ONLY when masks configured) ---");
    bench("breadcrumbs to_value+from_value", 20_000, || {
        let v = serde_json::to_value(black_box(&err.breadcrumbs)).unwrap();
        let back: Vec<sauron_core::envelope::Breadcrumb> = serde_json::from_value(v).unwrap();
        black_box(back);
    });
}
