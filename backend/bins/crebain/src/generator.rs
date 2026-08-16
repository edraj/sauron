//! Pure builders that turn a [`VirtualUser`] + a sequence number into concrete
//! `sauron_core` envelopes. No randomness crate: variation is derived
//! deterministically from `(user.index + seq)` so runs are reproducible and the
//! backend still sees a realistic spread of error types, events, and routes.
//!
//! The `tags`/`contexts`/`extra`/`properties` JSONB payloads are deliberately
//! built with 8-15 keys and a realistic cardinality mix (mostly repeating
//! low-cardinality values, a couple of medium-cardinality buckets, at most a
//! couple of unique-per-event ids) rather than the 1-3 trivial keys it'd take
//! to just satisfy the schema — GIN index write-amplification measurements
//! are taken from this generator's output, and they only transfer to
//! production if the shape here resembles what real SDKs actually send.

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

// Pools for the dev-supplied-metadata payloads below (`tags`/`contexts`/`extra`/
// `properties`). These back a GIN-index write-amplification measurement, so
// their shapes deliberately mimic real SDK metadata: mostly low-cardinality
// repeating values (plan/region/locale/payment/version/variant), a couple of
// medium-cardinality buckets (cart value, item count, latency), and at most
// one or two genuinely unique-per-event ids. See the payload builders in
// `event_envelope`/`issue_envelope` for how they're mixed — do not collapse
// this back down to 1-3 keys, the whole point is that it's representative.
const PLAN_TIERS: &[&str] = &["free", "pro", "team", "enterprise"];
const REGIONS: &[&str] = &[
    "us-east-1",
    "us-west-2",
    "eu-west-1",
    "eu-central-1",
    "ap-southeast-1",
    "ap-northeast-1",
    "sa-east-1",
    "ca-central-1",
];
const LOCALES: &[&str] = &["en-US", "en-GB", "de-DE", "fr-FR", "ja-JP", "pt-BR"];
const PAYMENT_METHODS: &[&str] = &[
    "credit_card",
    "paypal",
    "apple_pay",
    "google_pay",
    "bank_transfer",
];
const APP_VERSIONS: &[&str] = &["3.4.0", "3.4.1", "3.5.0", "3.5.1", "3.6.0"];
const AB_VARIANTS: &[&str] = &["control", "variant_a", "variant_b"];
const DEVICE_TYPES: &[&str] = &["desktop", "mobile", "tablet"];

/// Workflow names a `--workflow-ratio > 0` run draws from. Kept small and
/// realistic on purpose: workflows are user JOURNEYS, so a real app has a
/// handful of them and every signal in a journey re-hits the same row. A large
/// pool would spread the writes thin and hide the contention the flag exists to
/// expose.
const WORKFLOW_NAMES: &[&str] = &[
    "checkout",
    "onboarding",
    "signup",
    "subscription_upgrade",
    "password_reset",
];

/// Items each generated tick contributes to an envelope: `event_envelope` emits
/// `[event, transaction]` and `issue_envelope` emits `[breadcrumb_batch, error]`.
/// Both are 2, which is what makes a single `--batch-items` ceiling correct for
/// either stream.
pub const ITEMS_PER_TICK: usize = 2;

/// Mirrors `MAX_ENVELOPE_ITEMS` in `bins/sauron-ingest/src/main.rs`: the edge
/// answers HTTP 400 `too_many_items` above this, so a run configured past it
/// would measure nothing but rejections. Validated at parse time rather than
/// discovered at runtime.
pub const MAX_ENVELOPE_ITEMS: usize = 1000;

/// Largest `--batch-items` whose envelope still fits under the edge's cap.
pub const MAX_BATCH_ITEMS: usize = MAX_ENVELOPE_ITEMS / ITEMS_PER_TICK;

/// Knobs that reshape the generated workload without touching the payload
/// bodies. Both defaults reproduce the original workload exactly, so an old
/// benchmark command re-run today still measures the same thing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// Fraction (0.0..=1.0) of ticks that carry `workflow_id`/`workflow_name`.
    /// The backend's per-item `bump_workflow` upsert only fires when they are
    /// set, so at 0.0 that whole code path is invisible to the benchmark.
    pub workflow_ratio: f64,
    /// Ticks coalesced into one envelope. Real SDKs batch many items per
    /// envelope and the edge issues one Redis round trip PER ITEM, so envelope
    /// size is what makes that per-item cost measurable.
    pub batch_items: usize,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            workflow_ratio: 0.0,
            batch_items: 1,
        }
    }
}

/// SplitMix64 finalizer, used only to decorrelate the workflow draw from the
/// payload pools.
///
/// `pick` (= `user.index + seq`) already selects the plan/region/error/variant
/// values via `% 3/4/5/6/8`. Bucketing the same number for the workflow
/// percentile would alias against those moduli — at ratio 0.2, `pick % 100 < 20`
/// tags only items whose `pick % 5 == 0`, i.e. exactly one error type, which
/// would quietly turn a "20% of traffic" run into "all of one error type".
/// Mixing breaks the correlation while staying a pure function of the existing
/// inputs: no rand crate, so runs stay reproducible.
fn mix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `(workflow_id, workflow_name)` for one tick, or `None` when this tick isn't
/// part of a workflow.
///
/// The id is `{name}-{user.index}` rather than anything unique-per-signal on
/// purpose: `bump_workflow` is an upsert that ACCUMULATES counters onto an
/// existing row, and a run where every id were fresh would benchmark inserts
/// instead of the accumulate-and-contend path. Keying on (user × name) keeps
/// the row set small and hot while still spreading writes over many rows.
fn workflow_tag(user: &VirtualUser, seq: u64, ratio: f64) -> Option<(String, String)> {
    if ratio <= 0.0 {
        return None;
    }
    let draw = mix64((user.index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seq);
    // 1/10_000 granularity: fine enough for any ratio a benchmark would dial,
    // and the strict `<` means ratio 1.0 tags everything (max bucket is 0.9999).
    if (draw % 10_000) as f64 / 10_000.0 >= ratio {
        return None;
    }
    let name = WORKFLOW_NAMES[(draw >> 32) as usize % WORKFLOW_NAMES.len()];
    Some((format!("{name}-{}", user.index), name.to_string()))
}

/// Sequence number for tick `tick` of an envelope whose base tick number is
/// `seq`. Each envelope gets its own non-overlapping block of `batch_items`
/// numbers, so consecutive batched envelopes carry genuinely different activity
/// instead of re-sending an overlapping window. At `batch_items == 1` this is
/// the identity — which is what keeps the default workload unchanged.
fn tick_seq(seq: u64, tick: usize, batch_items: usize) -> u64 {
    seq.wrapping_mul(batch_items as u64)
        .wrapping_add(tick as u64)
}

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

/// An envelope of event ticks: `[event, transaction]` per tick — exercises
/// analytics + performance. `shape.batch_items` ticks are coalesced into the one
/// envelope (1 = the historical single-tick shape).
pub fn event_envelope(user: &VirtualUser, seq: u64, shape: Shape) -> Envelope {
    let batch = shape.batch_items.max(1);
    let mut items = Vec::with_capacity(batch * ITEMS_PER_TICK);
    for tick in 0..batch {
        let seq = tick_seq(seq, tick, batch);
        // Ticks in one envelope must not be identical copies — a real SDK
        // batches a *sequence* of activity, and repeated items would let the
        // backend dedupe/collapse work in ways production never sees. Walking
        // the screen per tick mirrors what the engine does between single-tick
        // sends; it is idempotent for tick 0, so `batch_items == 1` reproduces
        // the original envelope exactly.
        let mut u = user.clone();
        u.advance_screen(seq);
        let (event, txn) = event_items(&u, seq, shape.workflow_ratio);
        items.push(event);
        items.push(txn);
    }
    Envelope {
        header: header(),
        context: context(user),
        items,
    }
}

/// The `[event, transaction]` pair for a single tick.
fn event_items(user: &VirtualUser, seq: u64, workflow_ratio: f64) -> (EnvelopeItem, EnvelopeItem) {
    let pick = user.index.wrapping_add(seq as usize);
    let name = EVENT_NAMES[pick % EVENT_NAMES.len()];
    let (op, txn_name) = TXN_OPS[pick % TXN_OPS.len()];
    let duration_ms = 20.0 + (pick % 400) as f64;
    // The event and its transaction belong to the SAME tick, so they share one
    // workflow tag — that's what a real SDK does, and it's also what makes the
    // backend's per-item workflow upsert contend on a single row.
    let workflow = workflow_tag(user, seq, workflow_ratio);
    let (workflow_id, workflow_name) = match &workflow {
        Some((id, name)) => (Some(id.clone()), Some(name.clone())),
        None => (None, None),
    };

    let event = EnvelopeItem::Event(AnalyticsItem {
        name: name.to_string(),
        distinct_id: user.distinct_id.clone(),
        // Deliberately mimics real SDK `properties` payloads (funnel/commerce
        // metadata a dev would actually attach) — GIN index cost measurements
        // are taken from this shape, so keep it rich; do not simplify.
        properties: json!({
            "screen": user.screen,
            "seq": seq,
            "value": pick % 100,
            "plan": PLAN_TIERS[pick % PLAN_TIERS.len()],
            "region": REGIONS[(pick / 2) % REGIONS.len()],
            "locale": LOCALES[(pick + 1) % LOCALES.len()],
            "payment_method": PAYMENT_METHODS[(pick + 3) % PAYMENT_METHODS.len()],
            "app_version": APP_VERSIONS[(pick + 7) % APP_VERSIONS.len()],
            "ab_variant": AB_VARIANTS[pick % AB_VARIANTS.len()],
            "feature_flag_checkout_v2": pick % 2 == 0,
            "cart_value_cents": (pick % 100) * 149,
            "item_count": (pick % 20) + 1,
            "order_id": format!("order-{}-{}", user.index, seq),
        }),
        timestamp: Utc::now(),
        session_id: Some(user.session_id.clone()),
        workflow_id,
        workflow_name,
        screen: Some(user.screen.to_string()),
        tags: json!({ "screen": user.screen }),
        // Deliberately mimics real SDK `contexts` payloads (nested
        // session/device metadata + a feature-flag list — real `contexts` are
        // nested) — GIN index cost measurements are taken from this shape,
        // so keep it rich; do not simplify.
        contexts: json!({
            "session": {
                "seq": seq,
                "device": {
                    "type": DEVICE_TYPES[pick % DEVICE_TYPES.len()],
                    "region": REGIONS[(pick + 2) % REGIONS.len()],
                },
            },
            "locale": LOCALES[(pick + 4) % LOCALES.len()],
            "plan": PLAN_TIERS[(pick + 1) % PLAN_TIERS.len()],
            "ab_variant": AB_VARIANTS[(pick + 2) % AB_VARIANTS.len()],
            "payment_method": PAYMENT_METHODS[(pick + 1) % PAYMENT_METHODS.len()],
            "app_version": APP_VERSIONS[(pick + 2) % APP_VERSIONS.len()],
            "feature_flag_dark_mode": pick % 3 == 0,
            "active_flags": [
                AB_VARIANTS[pick % AB_VARIANTS.len()],
                DEVICE_TYPES[pick % DEVICE_TYPES.len()],
            ],
            "latency_bucket_ms": (pick % 50) * 20,
        }),
        // Deliberately mimics real SDK `extra` payloads (dev-supplied debug
        // scalars) — GIN index cost measurements are taken from this shape,
        // so keep it rich; do not simplify.
        extra: json!({
            "value": pick % 100,
            "plan": PLAN_TIERS[(pick + 2) % PLAN_TIERS.len()],
            "region": REGIONS[(pick + 3) % REGIONS.len()],
            "locale": LOCALES[(pick + 2) % LOCALES.len()],
            "payment_method": PAYMENT_METHODS[(pick + 2) % PAYMENT_METHODS.len()],
            "app_version": APP_VERSIONS[(pick + 3) % APP_VERSIONS.len()],
            "ab_variant": AB_VARIANTS[(pick + 1) % AB_VARIANTS.len()],
            "feature_flag_checkout_v2": pick % 2 == 0,
            "cart_value_cents": (pick % 100) * 173,
            "item_count": (pick % 20) + 1,
            "retry_count": pick % 4,
        }),
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
        workflow_id: workflow.as_ref().map(|(id, _)| id.clone()),
        workflow_name: workflow.as_ref().map(|(_, name)| name.clone()),
        timestamp: Utc::now(),
        finished_at: None,
        tags: json!({ "tier": if pick % 3 == 0 { "premium" } else { "free" } }),
        extra: json!({ "retry_count": pick % 4 }),
    });
    (event, txn)
}

/// An envelope of issue ticks: `[breadcrumb_batch, error]` per tick — exercises
/// error grouping. `shape.batch_items` ticks are coalesced into the one envelope
/// (1 = the historical single-tick shape).
pub fn issue_envelope(user: &VirtualUser, seq: u64, shape: Shape) -> Envelope {
    let batch = shape.batch_items.max(1);
    let mut items = Vec::with_capacity(batch * ITEMS_PER_TICK);
    for tick in 0..batch {
        let seq = tick_seq(seq, tick, batch);
        let (batch_item, error) = issue_items(user, seq, shape.workflow_ratio);
        items.push(batch_item);
        items.push(error);
    }
    Envelope {
        header: header(),
        context: context(user),
        items,
    }
}

/// The `[breadcrumb_batch, error]` pair for a single tick. Unlike an event tick
/// the screen is NOT advanced: a crashing user isn't navigating, and the engine
/// has never advanced it for this stream either.
fn issue_items(user: &VirtualUser, seq: u64, workflow_ratio: f64) -> (EnvelopeItem, EnvelopeItem) {
    let pick = user.index.wrapping_add(seq as usize);
    let (ty, value) = ERROR_TYPES[pick % ERROR_TYPES.len()];
    let lineno = 40 + (seq % 50) as u32;
    let workflow = workflow_tag(user, seq, workflow_ratio);

    let batch = EnvelopeItem::BreadcrumbBatch(BreadcrumbBatch {
        distinct_id: Some(user.distinct_id.clone()),
        session_id: Some(user.session_id.clone()),
        breadcrumbs: breadcrumbs(user, 3),
    });
    let error = EnvelopeItem::Error(Box::new(ErrorItem {
        event_id: Uuid::new_v4(),
        level: if seq % 7 == 0 {
            Level::Fatal
        } else {
            Level::Error
        },
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
        // Deliberately mimics real SDK `contexts` payloads (dev-supplied
        // feature flags, plan/region metadata, and a nested request context —
        // real `contexts` are nested) — GIN index cost measurements are
        // taken from this shape, so keep it rich; do not simplify.
        contexts: json!({
            "issue": {
                "seq": seq,
                "request": {
                    "id": format!("req-{}-{}", user.index, seq),
                    "region": REGIONS[pick % REGIONS.len()],
                },
            },
            "plan": PLAN_TIERS[(pick + 2) % PLAN_TIERS.len()],
            "locale": LOCALES[(pick + 3) % LOCALES.len()],
            "payment_method": PAYMENT_METHODS[(pick + 2) % PAYMENT_METHODS.len()],
            "app_version": APP_VERSIONS[(pick + 4) % APP_VERSIONS.len()],
            "ab_variant": AB_VARIANTS[(pick + 1) % AB_VARIANTS.len()],
            "feature_flag_dark_mode": pick % 2 == 0,
            "device_type": DEVICE_TYPES[(pick + 1) % DEVICE_TYPES.len()],
            "tags": [
                PLAN_TIERS[pick % PLAN_TIERS.len()],
                REGIONS[(pick + 1) % REGIONS.len()],
            ],
        }),
        // Deliberately mimics real SDK `extra` payloads (dev-supplied debug
        // scalars: order/cart metrics, retry/latency counters) — GIN index
        // cost measurements are taken from this shape, so keep it rich; do
        // not simplify.
        extra: json!({
            "lineno": lineno,
            "order_id": format!("order-{}-{}", user.index, seq),
            "cart_value_cents": (pick % 100) * 211,
            "item_count": (pick % 20) + 1,
            "latency_bucket_ms": (pick % 60) * 25,
            "retry_count": pick % 4,
            "plan": PLAN_TIERS[(pick + 3) % PLAN_TIERS.len()],
            "region": REGIONS[(pick + 5) % REGIONS.len()],
            "payment_method": PAYMENT_METHODS[(pick + 4) % PAYMENT_METHODS.len()],
            "app_version": APP_VERSIONS[(pick + 1) % APP_VERSIONS.len()],
            "feature_flag_checkout_v2": pick % 3 == 0,
        }),
        fingerprint: None,
        user: None,
        session_id: Some(user.session_id.clone()),
        workflow_id: workflow.as_ref().map(|(id, _)| id.clone()),
        workflow_name: workflow.as_ref().map(|(_, name)| name.clone()),
        screen: Some(user.screen.to_string()),
        raw_stacktrace: None,
        debug_meta: None,
    }));
    (batch, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(workflow_id, workflow_name)` for every item that HAS those fields.
    /// Breadcrumb batches and identifies don't, and are skipped — otherwise a
    /// "ratio 1.0 tags everything" assertion would be checking items that could
    /// never carry a tag in the first place.
    fn tags_of(env: &Envelope) -> Vec<(Option<String>, Option<String>)> {
        env.items
            .iter()
            .filter_map(|i| match i {
                EnvelopeItem::Event(e) => Some((e.workflow_id.clone(), e.workflow_name.clone())),
                EnvelopeItem::Transaction(t) => {
                    Some((t.workflow_id.clone(), t.workflow_name.clone()))
                }
                EnvelopeItem::Error(e) => Some((e.workflow_id.clone(), e.workflow_name.clone())),
                EnvelopeItem::BreadcrumbBatch(_) | EnvelopeItem::Identify(_) => None,
            })
            .collect()
    }

    /// Strip the fields that legitimately differ between two builds of the same
    /// logical item (wall clock, fresh event ids), so payload shape can be
    /// compared for equality.
    fn strip_volatile(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                map.remove("timestamp");
                map.remove("sent_at");
                map.remove("event_id");
                for (_, child) in map.iter_mut() {
                    strip_volatile(child);
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip_volatile),
            _ => {}
        }
    }

    fn stable_json(item: &EnvelopeItem) -> serde_json::Value {
        let mut v = serde_json::to_value(item).expect("serialize item");
        strip_volatile(&mut v);
        v
    }

    #[test]
    fn envelopes_serialize_and_reparse_as_sauron_core() {
        let user = VirtualUser::new(3);
        for env in [
            identify_envelope(&user),
            event_envelope(&user, 1, Shape::default()),
            issue_envelope(&user, 1, Shape::default()),
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
            event_envelope(&user, 0, Shape::default()),
            issue_envelope(&user, 0, Shape::default()),
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

    #[test]
    fn default_ratio_tags_nothing() {
        let shape = Shape::default();
        assert_eq!(shape.workflow_ratio, 0.0);
        for index in 0..50 {
            let user = VirtualUser::new(index);
            for seq in 0..20 {
                for env in [
                    event_envelope(&user, seq, shape),
                    issue_envelope(&user, seq, shape),
                ] {
                    for (id, name) in tags_of(&env) {
                        assert_eq!(id, None, "untagged run leaked a workflow_id");
                        assert_eq!(name, None, "untagged run leaked a workflow_name");
                    }
                }
            }
        }
    }

    #[test]
    fn ratio_one_tags_every_taggable_item() {
        let shape = Shape {
            workflow_ratio: 1.0,
            ..Shape::default()
        };
        for index in 0..50 {
            let user = VirtualUser::new(index);
            for seq in 0..20 {
                for env in [
                    event_envelope(&user, seq, shape),
                    issue_envelope(&user, seq, shape),
                ] {
                    let tags = tags_of(&env);
                    assert!(!tags.is_empty());
                    for (id, name) in tags {
                        let name = name.expect("ratio 1.0 must set workflow_name");
                        let id = id.expect("ratio 1.0 must set workflow_id");
                        assert!(WORKFLOW_NAMES.contains(&name.as_str()));
                        assert_eq!(id, format!("{name}-{index}"));
                    }
                }
            }
        }
    }

    #[test]
    fn intermediate_ratio_hits_roughly_that_proportion() {
        let shape = Shape {
            workflow_ratio: 0.25,
            ..Shape::default()
        };
        let (mut tagged, mut total) = (0u32, 0u32);
        for index in 0..400 {
            let user = VirtualUser::new(index);
            for seq in 0..25 {
                let env = event_envelope(&user, seq, shape);
                for (id, _) in tags_of(&env) {
                    total += 1;
                    tagged += u32::from(id.is_some());
                }
            }
        }
        let got = tagged as f64 / total as f64;
        // Deterministic selection, so this is not flaky — but the tolerance is
        // wide enough that the test asserts "the ratio is honoured", not the
        // exact bit pattern of the mixer.
        assert!(
            (0.22..=0.28).contains(&got),
            "expected ~25% tagged, got {got:.4} ({tagged}/{total})"
        );
    }

    #[test]
    fn one_user_reuses_the_same_workflow_ids() {
        // The whole point of a stable id: `bump_workflow` must ACCUMULATE onto
        // an existing row. Over 400 ticks one user may walk several workflows,
        // but each name maps to exactly one id and the id set stays bounded by
        // the (small) name pool — never one row per signal.
        let shape = Shape {
            workflow_ratio: 1.0,
            ..Shape::default()
        };
        let user = VirtualUser::new(11);
        let mut by_name: std::collections::BTreeMap<String, String> = Default::default();
        for seq in 0..400 {
            for env in [
                event_envelope(&user, seq, shape),
                issue_envelope(&user, seq, shape),
            ] {
                for (id, name) in tags_of(&env) {
                    let (id, name) = (id.unwrap(), name.unwrap());
                    let seen = by_name.entry(name.clone()).or_insert_with(|| id.clone());
                    assert_eq!(*seen, id, "workflow {name} changed id across seqs");
                }
            }
        }
        assert!(
            by_name.len() > 1 && by_name.len() <= WORKFLOW_NAMES.len(),
            "expected a handful of distinct workflows, got {}",
            by_name.len()
        );
        for (name, id) in &by_name {
            assert_eq!(*id, format!("{name}-11"));
        }
    }

    #[test]
    fn event_and_its_transaction_share_one_workflow() {
        // A tick is one unit of user activity: splitting its tag would spread
        // the backend's upsert over two rows and halve the contention the flag
        // exists to reproduce.
        let shape = Shape {
            workflow_ratio: 1.0,
            ..Shape::default()
        };
        let env = event_envelope(&VirtualUser::new(4), 9, shape);
        let tags = tags_of(&env);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], tags[1]);
    }

    #[test]
    fn default_shape_keeps_the_single_tick_envelope() {
        let seq = 13u64;
        let mut user = VirtualUser::new(7);
        // Exactly what the engine does before an event send.
        user.advance_screen(seq);

        let ev = event_envelope(&user, seq, Shape::default());
        assert_eq!(ev.items.len(), 2);
        let EnvelopeItem::Event(e) = &ev.items[0] else {
            panic!("expected [event, transaction]");
        };
        assert!(matches!(ev.items[1], EnvelopeItem::Transaction(_)));
        // The builder re-advances the screen internally; that must be a no-op
        // at tick 0, or every default run's payload would silently shift.
        assert_eq!(e.screen.as_deref(), Some(user.screen));
        assert_eq!(e.properties["seq"], json!(seq));

        // Issues do not navigate: the base (un-advanced) screen is what the
        // engine has always sent for this stream.
        let plain = VirtualUser::new(7);
        let is = issue_envelope(&plain, seq, Shape::default());
        assert_eq!(is.items.len(), 2);
        assert!(matches!(is.items[0], EnvelopeItem::BreadcrumbBatch(_)));
        let EnvelopeItem::Error(err) = &is.items[1] else {
            panic!("expected [breadcrumb_batch, error]");
        };
        assert_eq!(err.screen.as_deref(), Some(plain.screen));
        assert_eq!(tick_seq(seq, 0, 1), seq);
    }

    #[test]
    fn batching_yields_two_items_per_tick_and_only_appends() {
        for batch in [2usize, 5, 64] {
            let shape = Shape {
                batch_items: batch,
                ..Shape::default()
            };
            let base = 3u64;
            let user = VirtualUser::new(6);

            for env in [
                event_envelope(&user, base, shape),
                issue_envelope(&user, base, shape),
            ] {
                assert_eq!(env.items.len(), batch * ITEMS_PER_TICK);
                // Item accounting is derived from the items themselves, so a
                // batched envelope reports every signal it actually carries.
                let counts = ItemCounts::of(&env);
                let per_type = counts.events + counts.transactions;
                let issue_types = counts.breadcrumbs + counts.errors;
                assert_eq!((per_type + issue_types) as usize, batch * ITEMS_PER_TICK);
            }

            // The first tick of a batched envelope is byte-for-byte the
            // single-tick envelope at that tick's seq: batching only APPENDS
            // activity, it never reshapes what was already being sent.
            let head = tick_seq(base, 0, batch);
            let mut u = user.clone();
            u.advance_screen(head);
            let batched = event_envelope(&user, base, shape);
            let single = event_envelope(&u, head, Shape::default());
            assert_eq!(
                stable_json(&batched.items[0]),
                stable_json(&single.items[0])
            );
            assert_eq!(
                stable_json(&batched.items[1]),
                stable_json(&single.items[1])
            );

            // ...and later ticks are genuinely different activity, not copies.
            assert_ne!(
                stable_json(&batched.items[0]),
                stable_json(&batched.items[2])
            );
        }
    }

    #[test]
    fn batch_ceiling_stays_under_the_edge_cap() {
        assert_eq!(MAX_BATCH_ITEMS * ITEMS_PER_TICK, MAX_ENVELOPE_ITEMS);
        let shape = Shape {
            batch_items: MAX_BATCH_ITEMS,
            ..Shape::default()
        };
        let env = event_envelope(&VirtualUser::new(1), 1, shape);
        assert!(env.items.len() <= MAX_ENVELOPE_ITEMS);
    }
}
