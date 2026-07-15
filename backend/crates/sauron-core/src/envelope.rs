//! The Sauron ingest wire contract.
//!
//! One JSON [`Envelope`] carries a header, an envelope-wide context block, and a
//! list of tagged [`EnvelopeItem`]s (errors, product events, identify calls, or
//! a breadcrumb batch). Both SDKs (`@sauron/browser`, `sauron_flutter`) emit
//! exactly this shape; the golden fixture in the SDK test suites guards parity.
//!
//! Transport: `POST /api/{project_id}/envelope`, `X-Sauron-Key: <public_key>`,
//! optional `Content-Encoding: gzip`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Severity level, shared by errors and breadcrumbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warning,
    #[default]
    Error,
    Fatal,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }
}

/// Top-level envelope posted by an SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub header: EnvelopeHeader,
    #[serde(default)]
    pub context: EnvelopeContext,
    #[serde(default)]
    pub items: Vec<EnvelopeItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeHeader {
    /// Full DSN — optional; the public key normally travels in `X-Sauron-Key`.
    #[serde(default)]
    pub dsn: Option<String>,
    pub sdk: SdkInfo,
    /// When the SDK flushed the batch — used for clock-skew correction.
    #[serde(default = "Utc::now")]
    pub sent_at: DateTime<Utc>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkInfo {
    pub name: String,
    pub version: String,
}

/// Envelope-wide context. Free-form JSON blocks keep the SDKs unopinionated
/// about platform-specific fields; only `user` is typed because the backend
/// resolves it to an identity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvelopeContext {
    #[serde(default)]
    pub device: serde_json::Value,
    #[serde(default)]
    pub os: serde_json::Value,
    #[serde(default)]
    pub app: serde_json::Value,
    #[serde(default)]
    pub runtime: serde_json::Value,
    #[serde(default)]
    pub user: Option<EventUser>,
}

/// A single item in the envelope, tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvelopeItem {
    Error(Box<ErrorItem>),
    Event(AnalyticsItem),
    Identify(IdentifyItem),
    BreadcrumbBatch(BreadcrumbBatch),
    Transaction(TransactionItem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorItem {
    #[serde(default = "Uuid::new_v4")]
    pub event_id: Uuid,
    #[serde(default)]
    pub level: Level,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub exception: Option<ExceptionInfo>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub breadcrumbs: Vec<Breadcrumb>,
    #[serde(default)]
    pub tags: serde_json::Value,
    /// Client-supplied fingerprint override (honored verbatim when present).
    #[serde(default)]
    pub fingerprint: Option<Vec<String>>,
    /// Optional per-item user override (falls back to envelope context user).
    #[serde(default)]
    pub user: Option<EventUser>,
    /// Session this error occurred in, if the SDK tracks one — ties the error
    /// onto the session timeline.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Current screen/route the SDK was on when the error was captured.
    #[serde(default)]
    pub screen: Option<String>,
    /// Verbatim platform stack trace for server-side symbolication that the
    /// neutral [`Frame`] model can't carry — notably Dart AOT PC-offset traces.
    #[serde(default)]
    pub raw_stacktrace: Option<String>,
    /// Debug metadata for matching symbol artifacts (Dart build-id, load base,
    /// arch, os).
    #[serde(default)]
    pub debug_meta: Option<DebugMeta>,
}

/// Symbol-matching metadata shipped alongside a `raw_stacktrace`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DebugMeta {
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub isolate_dso_base: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionInfo {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub mechanism: Option<Mechanism>,
    #[serde(default)]
    pub stacktrace: Vec<Frame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mechanism {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub handled: Option<bool>,
}

/// A platform-neutral stack frame. Frames are ordered with the crashing frame
/// **last** (call site → crash). Symbolication happens server-side later; the
/// SDK only ships raw frames plus the release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    #[serde(default)]
    pub function: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub abs_path: Option<String>,
    #[serde(default)]
    pub lineno: Option<u32>,
    #[serde(default)]
    pub colno: Option<u32>,
    #[serde(default)]
    pub in_app: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadcrumb {
    #[serde(rename = "type", default)]
    pub ty: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// A `track()` product-analytics event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsItem {
    pub name: String,
    pub distinct_id: String,
    #[serde(default)]
    pub properties: serde_json::Value,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Current screen/route the SDK was on when the event was tracked.
    #[serde(default)]
    pub screen: Option<String>,
}

/// A performance transaction: one timed operation (page/screen load, HTTP call,
/// resource fetch, or a custom span). Aggregated server-side into p50/p95/etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionItem {
    /// Route / screen / operation label (the grouping key on the dashboard).
    pub name: String,
    /// Operation class: `navigation` | `http` | `resource` | `screen_load` | `custom`.
    pub op: String,
    pub duration_ms: f64,
    /// `ok` | `error` | an HTTP status class — free-form; drives the error rate.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub http_method: Option<String>,
    #[serde(default)]
    pub http_status: Option<i32>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub distinct_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

/// An `identify()` call: attach traits to a person, optionally aliasing an
/// anonymous id to a known one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifyItem {
    pub distinct_id: String,
    #[serde(default)]
    pub anonymous_id: Option<String>,
    #[serde(default)]
    pub traits: serde_json::Value,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

/// A batch of breadcrumbs uploaded ahead of (or alongside) an error so the
/// backend can attach recent activity to a later crash for the same person.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreadcrumbBatch {
    #[serde(default)]
    pub distinct_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub breadcrumbs: Vec<Breadcrumb>,
}

/// The person a signal is attributed to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventUser {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub traits: serde_json::Value,
}

/// The internal unit of work the ingest edge enqueues onto Redis: a single
/// envelope item plus the edge-resolved tenancy + request context. The worker
/// consumes these. Signals are written keyed by `app_id`; `project_id`/`org_id`
/// are carried for context and future roll-ups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestJob {
    pub app_id: Uuid,
    pub project_id: Uuid,
    pub org_id: Uuid,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    pub received_at: DateTime<Utc>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub context: EnvelopeContext,
    pub item: EnvelopeItem,
}

impl EventUser {
    /// The stable analytics identity for this user, if any.
    pub fn distinct_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden envelope both SDKs must emit. Kept in sync with
    /// `sdks/js/test/envelope.test.ts` and `sdks/flutter/test/envelope_test.dart`.
    const GOLDEN: &str = r#"{
      "header": {
        "dsn": "https://pk_test@localhost:8081/1",
        "sdk": { "name": "sauron.javascript", "version": "0.1.0" },
        "sent_at": "2026-07-12T10:30:00.123Z",
        "environment": "production",
        "release": "web@1.4.2"
      },
      "context": {
        "device": { "family": "Apple", "model": null, "arch": null },
        "os": { "name": "macOS", "version": "14.5" },
        "app": { "version": "1.4.2", "build": null },
        "runtime": { "name": "Chrome", "version": "126" },
        "user": { "id": "u_123", "email": null, "traits": {} }
      },
      "items": [
        { "type": "error", "timestamp": "2026-07-12T10:29:58.900Z", "level": "error",
          "exception": { "type": "TypeError", "value": "x is not a function",
            "mechanism": { "type": "onunhandledrejection", "handled": false },
            "stacktrace": [ { "function": "loadUser", "filename": "app.js", "lineno": 42, "colno": 13, "in_app": true } ] },
          "breadcrumbs": [ { "type": "navigation", "category": "history", "message": null, "level": "info", "timestamp": "2026-07-12T10:29:50.000Z", "data": { "from": "/", "to": "/settings" } } ],
          "fingerprint": null },
        { "type": "event", "name": "checkout_completed", "distinct_id": "u_123", "timestamp": "2026-07-12T10:29:40.000Z", "properties": { "cart_value": 42.5 } },
        { "type": "identify", "distinct_id": "u_123", "anonymous_id": null, "traits": { "plan": "pro" } }
      ]
    }"#;

    #[test]
    fn deserializes_golden_envelope() {
        let env: Envelope = serde_json::from_str(GOLDEN).expect("golden envelope must parse");
        assert_eq!(env.header.sdk.name, "sauron.javascript");
        assert_eq!(env.header.environment.as_deref(), Some("production"));
        assert_eq!(env.items.len(), 3);

        match &env.items[0] {
            EnvelopeItem::Error(e) => {
                let exc = e.exception.as_ref().unwrap();
                assert_eq!(exc.ty, "TypeError");
                assert_eq!(exc.stacktrace.len(), 1);
                assert_eq!(exc.mechanism.as_ref().unwrap().handled, Some(false));
                assert_eq!(e.breadcrumbs.len(), 1);
            }
            other => panic!("expected error item, got {other:?}"),
        }
        match &env.items[1] {
            EnvelopeItem::Event(ev) => {
                assert_eq!(ev.name, "checkout_completed");
                assert_eq!(ev.distinct_id, "u_123");
            }
            other => panic!("expected event item, got {other:?}"),
        }
        match &env.items[2] {
            EnvelopeItem::Identify(id) => assert_eq!(id.distinct_id, "u_123"),
            other => panic!("expected identify item, got {other:?}"),
        }
    }

    #[test]
    fn parses_breadcrumb_batch_item() {
        let json = r#"{"type":"breadcrumb_batch","distinct_id":"u1","session_id":"s1",
            "breadcrumbs":[{"type":"navigation","timestamp":"2026-07-12T10:00:00Z","data":{}}]}"#;
        let item: EnvelopeItem = serde_json::from_str(json).unwrap();
        match item {
            EnvelopeItem::BreadcrumbBatch(b) => {
                assert_eq!(b.distinct_id.as_deref(), Some("u1"));
                assert_eq!(b.breadcrumbs.len(), 1);
            }
            other => panic!("expected breadcrumb_batch, got {other:?}"),
        }
    }

    #[test]
    fn level_serializes_lowercase_for_every_variant() {
        let cases = [
            (Level::Debug, "\"debug\""),
            (Level::Info, "\"info\""),
            (Level::Warning, "\"warning\""),
            (Level::Error, "\"error\""),
            (Level::Fatal, "\"fatal\""),
        ];
        for (lvl, expected) in cases {
            assert_eq!(serde_json::to_string(&lvl).unwrap(), expected);
            let back: Level = serde_json::from_str(expected).unwrap();
            assert_eq!(back, lvl);
        }
    }

    #[test]
    fn error_item_defaults_missing_fields() {
        // Minimal error item: no event_id, no breadcrumbs, no tags.
        let json = r#"{"type":"error","timestamp":"2026-07-12T10:00:00Z",
            "exception":{"type":"X"}}"#;
        let item: EnvelopeItem = serde_json::from_str(json).unwrap();
        match item {
            EnvelopeItem::Error(e) => {
                assert_eq!(e.level, Level::Error); // default
                assert!(e.breadcrumbs.is_empty());
                assert!(e.fingerprint.is_none());
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn parses_transaction_item() {
        let json = r#"{"type":"transaction","name":"GET /api/users","op":"http",
            "duration_ms":128.4,"status":"ok","http_method":"GET","http_status":200,
            "url":"/api/users","distinct_id":"u1","session_id":"s1",
            "timestamp":"2026-07-13T10:00:00Z"}"#;
        let item: EnvelopeItem = serde_json::from_str(json).unwrap();
        match item {
            EnvelopeItem::Transaction(t) => {
                assert_eq!(t.name, "GET /api/users");
                assert_eq!(t.op, "http");
                assert_eq!(t.duration_ms, 128.4);
                assert_eq!(t.http_status, Some(200));
                assert_eq!(t.session_id.as_deref(), Some("s1"));
            }
            other => panic!("expected transaction, got {other:?}"),
        }
    }

    #[test]
    fn roundtrips_item_tag() {
        let item = EnvelopeItem::Event(AnalyticsItem {
            name: "signed_up".into(),
            distinct_id: "u_1".into(),
            properties: serde_json::json!({ "plan": "free" }),
            timestamp: Utc::now(),
            session_id: None,
            screen: None,
        });
        let s = serde_json::to_string(&item).unwrap();
        assert!(s.contains("\"type\":\"event\""));
        let back: EnvelopeItem = serde_json::from_str(&s).unwrap();
        matches!(back, EnvelopeItem::Event(_));
    }
}
