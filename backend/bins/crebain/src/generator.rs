//! Pure builders that turn a [`VirtualUser`] + a sequence number into concrete
//! `sauron_core` envelopes. No randomness crate: variation is derived
//! deterministically from `(user.index + seq)` so runs are reproducible and the
//! backend still sees a realistic spread of error types, events, and routes.

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use sauron_core::envelope::{
    AnalyticsItem, Breadcrumb, BreadcrumbBatch, Envelope, EnvelopeContext, EnvelopeHeader,
    EnvelopeItem, ErrorItem, EventUser, ExceptionInfo, Frame, IdentifyItem, Level, Mechanism,
    SdkInfo, TransactionItem,
};

use crate::user::VirtualUser;

const SDK_NAME: &str = "sauron.crebain";
const SDK_VERSION: &str = "0.1.0";
const ENVIRONMENT: &str = "benchmark";
const RELEASE: &str = "crebain@0.1.0";

const ERROR_TYPES: &[(&str, &str)] = &[
    ("TypeError", "undefined is not a function"),
    ("RangeError", "index out of bounds"),
    ("NullPointerException", "null value dereferenced"),
    ("TimeoutError", "operation timed out after 30s"),
    ("StateError", "setState called after dispose"),
];
const EVENT_NAMES: &[&str] = &[
    "page_view",
    "button_click",
    "checkout_completed",
    "signed_up",
    "feature_used",
];
const TXN_OPS: &[(&str, &str)] = &[
    ("navigation", "/dashboard"),
    ("http", "GET /api/users"),
    ("resource", "app.bundle.js"),
    ("screen_load", "HomeScreen"),
];

/// Tally of signal items in an envelope, so metrics can attribute per-type.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ItemCounts {
    pub errors: u64,
    pub events: u64,
    pub identifies: u64,
    pub transactions: u64,
    pub breadcrumbs: u64,
}

impl ItemCounts {
    pub fn of(env: &Envelope) -> Self {
        let mut c = ItemCounts::default();
        for item in &env.items {
            match item {
                EnvelopeItem::Error(_) => c.errors += 1,
                EnvelopeItem::Event(_) => c.events += 1,
                EnvelopeItem::Identify(_) => c.identifies += 1,
                EnvelopeItem::Transaction(_) => c.transactions += 1,
                EnvelopeItem::BreadcrumbBatch(_) => c.breadcrumbs += 1,
            }
        }
        c
    }
}

fn header() -> EnvelopeHeader {
    EnvelopeHeader {
        dsn: None,
        sdk: SdkInfo {
            name: SDK_NAME.to_string(),
            version: SDK_VERSION.to_string(),
        },
        sent_at: Utc::now(),
        environment: Some(ENVIRONMENT.to_string()),
        release: Some(RELEASE.to_string()),
    }
}

fn context(user: &VirtualUser) -> EnvelopeContext {
    EnvelopeContext {
        device: json!({ "family": "crebain-sim", "model": "vX" }),
        os: json!({ "name": "linux", "version": "6.0" }),
        app: json!({ "version": "1.0.0", "build": user.index }),
        runtime: json!({ "name": "crebain", "version": SDK_VERSION }),
        user: Some(EventUser {
            id: Some(user.distinct_id.clone()),
            email: None,
            username: None,
            ip_address: None,
            traits: user.traits.clone(),
        }),
    }
}

fn breadcrumbs(user: &VirtualUser, n: usize) -> Vec<Breadcrumb> {
    (0..n)
        .map(|i| Breadcrumb {
            ty: "navigation".to_string(),
            category: Some("ui".to_string()),
            message: Some(format!("navigated step {i}")),
            level: Some("info".to_string()),
            timestamp: Utc::now(),
            data: json!({ "from": user.screen, "step": i }),
        })
        .collect()
}

/// `identify` — sent once when a user first starts.
pub fn identify_envelope(user: &VirtualUser) -> Envelope {
    Envelope {
        header: header(),
        context: context(user),
        items: vec![EnvelopeItem::Identify(IdentifyItem {
            distinct_id: user.distinct_id.clone(),
            anonymous_id: None,
            traits: user.traits.clone(),
            timestamp: Utc::now(),
        })],
    }
}

/// One event tick: `[event, transaction]` — exercises analytics + performance.
pub fn event_envelope(user: &VirtualUser, seq: u64) -> Envelope {
    let pick = user.index.wrapping_add(seq as usize);
    let name = EVENT_NAMES[pick % EVENT_NAMES.len()];
    let (op, txn_name) = TXN_OPS[pick % TXN_OPS.len()];
    let duration_ms = 20.0 + (pick % 400) as f64;

    let event = EnvelopeItem::Event(AnalyticsItem {
        name: name.to_string(),
        distinct_id: user.distinct_id.clone(),
        properties: json!({ "screen": user.screen, "seq": seq, "value": pick % 100 }),
        timestamp: Utc::now(),
        session_id: Some(user.session_id.clone()),
        screen: Some(user.screen.to_string()),
        tags: json!({ "screen": user.screen }),
        contexts: json!({ "session": { "seq": seq } }),
        extra: json!({ "value": pick % 100 }),
    });
    let txn = EnvelopeItem::Transaction(TransactionItem {
        name: txn_name.to_string(),
        op: op.to_string(),
        duration_ms,
        status: Some("ok".to_string()),
        http_method: (op == "http").then(|| "GET".to_string()),
        http_status: (op == "http").then_some(200),
        url: (op == "http").then(|| "/api/users".to_string()),
        distinct_id: Some(user.distinct_id.clone()),
        session_id: Some(user.session_id.clone()),
        timestamp: Utc::now(),
    });
    Envelope {
        header: header(),
        context: context(user),
        items: vec![event, txn],
    }
}

/// One issue tick: `[breadcrumb_batch, error]` — exercises error grouping. The
/// error's line number varies with `seq` to confirm line-independent grouping.
pub fn issue_envelope(user: &VirtualUser, seq: u64) -> Envelope {
    let pick = user.index.wrapping_add(seq as usize);
    let (ty, value) = ERROR_TYPES[pick % ERROR_TYPES.len()];
    let lineno = 40 + (seq % 50) as u32;

    let batch = EnvelopeItem::BreadcrumbBatch(BreadcrumbBatch {
        distinct_id: Some(user.distinct_id.clone()),
        session_id: Some(user.session_id.clone()),
        breadcrumbs: breadcrumbs(user, 3),
    });
    let error = EnvelopeItem::Error(Box::new(ErrorItem {
        event_id: Uuid::new_v4(),
        level: if seq % 7 == 0 { Level::Fatal } else { Level::Error },
        timestamp: Utc::now(),
        exception: Some(ExceptionInfo {
            ty: ty.to_string(),
            value: Some(value.to_string()),
            mechanism: Some(Mechanism {
                ty: "onerror".to_string(),
                handled: Some(false),
            }),
            stacktrace: vec![
                Frame {
                    function: Some("main".to_string()),
                    module: Some("app".to_string()),
                    filename: Some("main.rs".to_string()),
                    abs_path: None,
                    lineno: Some(10),
                    colno: Some(1),
                    in_app: Some(true),
                },
                Frame {
                    function: Some("handle_request".to_string()),
                    module: Some("app::server".to_string()),
                    filename: Some("server.rs".to_string()),
                    abs_path: None,
                    lineno: Some(lineno),
                    colno: Some(5),
                    in_app: Some(true),
                },
            ],
        }),
        message: None,
        breadcrumbs: breadcrumbs(user, 2),
        tags: json!({ "screen": user.screen }),
        contexts: json!({ "issue": { "seq": seq } }),
        extra: json!({ "lineno": lineno }),
        fingerprint: None,
        user: None,
        session_id: Some(user.session_id.clone()),
        screen: Some(user.screen.to_string()),
        raw_stacktrace: None,
        debug_meta: None,
    }));
    Envelope {
        header: header(),
        context: context(user),
        items: vec![batch, error],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_serialize_and_reparse_as_sauron_core() {
        let user = VirtualUser::new(3);
        for env in [
            identify_envelope(&user),
            event_envelope(&user, 1),
            issue_envelope(&user, 1),
        ] {
            let json = serde_json::to_string(&env).expect("serialize");
            let back: Envelope = serde_json::from_str(&json).expect("reparse");
            assert_eq!(back.header.sdk.name, SDK_NAME);
        }
    }

    #[test]
    fn all_five_signal_types_are_produced() {
        let user = VirtualUser::new(0);
        let mut total = ItemCounts::default();
        for env in [
            identify_envelope(&user),
            event_envelope(&user, 0),
            issue_envelope(&user, 0),
        ] {
            let c = ItemCounts::of(&env);
            total.errors += c.errors;
            total.events += c.events;
            total.identifies += c.identifies;
            total.transactions += c.transactions;
            total.breadcrumbs += c.breadcrumbs;
        }
        assert_eq!(total.errors, 1);
        assert_eq!(total.events, 1);
        assert_eq!(total.identifies, 1);
        assert_eq!(total.transactions, 1);
        assert_eq!(total.breadcrumbs, 1);
    }
}
