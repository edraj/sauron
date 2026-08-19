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
//!
//! # Repeat-heavy error mode
//!
//! `Shape::distinct_issues` + `Shape::repeat_ratio` turn the error stream from
//! "every occurrence is a fresh-ish fingerprint" into "the same exception
//! recurs over and over for the same user + device + session" — the workload a
//! storage/dedup experiment has to be measured against. What a repeat holds
//! identical, and what it deliberately lets drift, is documented on
//! [`issue_items`]; that split is the whole measurement.

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

/// Distinct issue identities a run produces when `--distinct-issues` is not
/// given: one per [`ERROR_TYPES`] entry, i.e. exactly the five fingerprints
/// crebain has always emitted. Keeping this as the default is what lets an old
/// benchmark command re-run today measure the same thing it measured before.
pub const DEFAULT_DISTINCT_ISSUES: usize = ERROR_TYPES.len();

/// Ceiling on `--distinct-issues`. Not a technical limit — a guard rail. Every
/// distinct issue is an `issues` row plus its share of `error_events`
/// partitions, and a run that asked for a million of them would be measuring
/// issue-table insert cost, not the duplicate-storage question the flag exists
/// to pose.
pub const MAX_DISTINCT_ISSUES: usize = 100_000;

/// First line number of the synthetic crash frame. Non-repeats walk forward
/// from here exactly as they always have; repeats freeze to a per-slot value.
const BASE_LINENO: u32 = 40;

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
    /// How many DISTINCT issue identities (i.e. distinct backend fingerprints,
    /// and therefore distinct `issues` rows) the whole run can reach.
    ///
    /// This is the CARDINALITY knob, deliberately not the ratio knob — see the
    /// note on `repeat_ratio` for why the split is that way round.
    pub distinct_issues: usize,
    /// Fraction (0.0..=1.0) of error occurrences emitted as a REPEAT: a
    /// re-occurrence of this user's canonical issue, landing on the same
    /// fingerprint AND the same `(user, device, session)` tuple.
    ///
    /// # Why the ratio lives here and not on `distinct_issues`
    ///
    /// The duplicate ratio has to be something an operator can *set* and the
    /// summary can *report back*, and only this knob is both. `repeat_ratio` is
    /// by construction the fraction of error occurrences emitted as duplicates,
    /// so the dialled number and the achieved number are the same quantity and
    /// any gap between them is a real defect worth seeing.
    ///
    /// A `--distinct-issues`-only design cannot do that. With `N` distinct
    /// issues the duplicate ratio comes out as
    /// `1 - (users x N) / occurrences` — a quantity that moves with `--users`,
    /// `--duration`, `--issues-per-min` and `--batch-items`, so the same flag
    /// value describes a different workload on every run and can never be
    /// aimed at a target at all. `distinct_issues` is kept as the orthogonal
    /// axis it genuinely is: how many `issues` rows the occurrences pile onto.
    ///
    /// 0.0 (the default) reproduces the historical error stream exactly.
    pub repeat_ratio: f64,
    /// Total frames per stacktrace, including the two in-app frames that carry
    /// the issue identity.
    ///
    /// # Why this knob has to exist
    ///
    /// The generator used to emit a hardcoded 2-frame stacktrace (`main` ->
    /// the crash fn), which stores as ~200 B — making `stacktrace` the
    /// *smallest* of the fat columns here, below `context` (455 B),
    /// `contexts` (330 B), `extra` (315 B) and `breadcrumbs` (248 B). That
    /// ranking is an artifact of this file, not a property of the product:
    /// `contexts`/`extra` were deliberately built rich for GIN measurements
    /// while the stacktrace never got the same treatment, and real
    /// Flutter/JS/RN traces run 20-60 frames and several KB. Since
    /// `stacktrace` is the one column byte-identical across every repeat of an
    /// issue, it is also the only sensible pooling target — so a 2-frame trace
    /// silently biases every storage experiment *against* the right answer.
    ///
    /// # Why the padding frames are `in_app: false`
    ///
    /// `sauron_core::fingerprint::pick_frames` builds its pool from `in_app`
    /// frames alone whenever any frame is in-app, then takes the last
    /// `FRAME_DEPTH` (5). Padding with `in_app: false` library frames
    /// therefore cannot enter the pool at all, so the fingerprint — and hence
    /// `--distinct-issues` — is provably unchanged at any depth. Padding with
    /// in-app frames instead would push the identity-bearing crash frame out
    /// of the 5-frame window and silently collapse every issue into one.
    ///
    /// Values below 2 are clamped to 2 (the two identity frames are
    /// mandatory).
    pub stack_depth: usize,
}

/// Default frames per stacktrace. Sits in the middle of the 20-60 band real
/// mobile/web SDK traces occupy; the shipped node SDK caps at 50.
pub const DEFAULT_STACK_DEPTH: usize = 24;

/// Upper bound, matching the largest cap any shipped SDK applies.
pub const MAX_STACK_DEPTH: usize = 50;

/// Synthetic library frames used to pad a trace out to `stack_depth`.
///
/// These mimic the framework/runtime frames that dominate a real trace: long
/// package paths, generic parameters and async machinery. All are emitted with
/// `in_app: false` — see the note on [`Shape::stack_depth`].
const LIB_FRAMES: &[(&str, &str, &str)] = &[
    ("poll", "core::future::future", "future.rs"),
    (
        "poll_next",
        "futures_util::stream::stream::StreamExt",
        "stream.rs",
    ),
    (
        "call",
        "tower::buffer::service::Buffer<T,Request>",
        "service.rs",
    ),
    (
        "handle",
        "axum::routing::method_routing",
        "method_routing.rs",
    ),
    ("oneshot", "tower::util::Oneshot<S,Req>", "oneshot.rs"),
    (
        "run_until_pending",
        "tokio::runtime::scheduler::multi_thread::worker",
        "worker.rs",
    ),
    (
        "block_on",
        "tokio::runtime::park::CachedParkThread",
        "park.rs",
    ),
    (
        "serve_connection",
        "hyper::proto::h1::dispatch::Dispatcher<D,Bs,I,T>",
        "dispatch.rs",
    ),
];

/// The `in_app: false` padding frames placed *before* the two identity frames,
/// so the trace reads crashing-last like a real one.
///
/// Deterministic in `slot` alone, so every occurrence of one issue produces a
/// byte-identical stacktrace — which is exactly the property the pooling
/// experiment measures.
fn pad_frames(slot: usize, stack_depth: usize) -> Vec<Frame> {
    let pad = stack_depth.max(2) - 2;
    (0..pad)
        .map(|i| {
            let (function, module, filename) = LIB_FRAMES[(slot + i) % LIB_FRAMES.len()];
            let depth = pad - i;
            Frame {
                function: Some(function.to_string()),
                module: Some(module.to_string()),
                filename: Some(filename.to_string()),
                abs_path: Some(format!(
                    "/rustc/registry/src/index.crates.io-6f17d22bba15001f/{module}/{filename}"
                )),
                // MUST stay false: these frames are invisible to the
                // fingerprint only because `pick_frames` filters to in-app
                // frames. Flipping this silently collapses `--distinct-issues`.
                in_app: Some(false),
                lineno: Some(BASE_LINENO + depth as u32),
                colno: Some(1 + (depth as u32 % 40)),
            }
        })
        .collect()
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            workflow_ratio: 0.0,
            batch_items: 1,
            distinct_issues: DEFAULT_DISTINCT_ISSUES,
            repeat_ratio: 0.0,
            stack_depth: DEFAULT_STACK_DEPTH,
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

/// Alphabetic tag for issue slot `slot`: 0 -> "a", 25 -> "z", 26 -> "ba".
///
/// ALPHABETIC ON PURPOSE, and this is the trap worth naming. The backend builds
/// a frame's fingerprint signature through `mask_volatile`
/// (`crates/sauron-core/src/fingerprint.rs`), which replaces every maximal run
/// of digits with a single `{n}` placeholder. A numeric suffix would therefore
/// make `handle_request_7` and `handle_request_42` normalize to the SAME
/// signature — `--distinct-issues 40` would silently collapse back to the five
/// fingerprints the type pool provides, and the run would look correct while
/// measuring the opposite workload. Letters pass through untouched.
fn slot_tag(slot: usize) -> String {
    let mut n = slot;
    let mut out = Vec::new();
    loop {
        out.push(b'a' + (n % 26) as u8);
        n /= 26;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ascii letters")
}

/// The identity of issue slot `slot`: `(exception_type, exception_value,
/// crashing frame function)`.
///
/// Those three are exactly the inputs the backend's fingerprint reads for a
/// frame-bearing exception — type plus the top in-app frame signatures, with
/// line/column deliberately dropped. Slots below [`DEFAULT_DISTINCT_ISSUES`]
/// reproduce the historical identities byte for byte; past that the crashing
/// frame's function carries an alphabetic [`slot_tag`], which is what actually
/// mints a new fingerprint (the type pool only has five entries and would
/// otherwise wrap).
fn issue_identity(slot: usize) -> (&'static str, String, String) {
    let (ty, value) = ERROR_TYPES[slot % ERROR_TYPES.len()];
    if slot < ERROR_TYPES.len() {
        (ty, value.to_string(), "handle_request".to_string())
    } else {
        let tag = slot_tag(slot);
        (
            ty,
            format!("{value} (call site {tag})"),
            format!("handle_request_{tag}"),
        )
    }
}

/// The crash line a REPEAT freezes to. Per slot rather than global so different
/// issues still crash on different lines — the fingerprint ignores line numbers,
/// but the stored `stacktrace` JSONB does not, and that column is the one a
/// content-addressed storage tier would be trying to fold.
fn canonical_lineno(slot: usize) -> u32 {
    BASE_LINENO + (slot % 50) as u32
}

/// The single issue every repeat for `user` lands on.
///
/// A function of the USER ALONE — never of `seq`. That is the property that
/// makes repeats pile onto ONE `(fingerprint, user, device, session)` group
/// instead of spraying across several: crebain already pins `distinct_id`,
/// `session_id` and (via the constant `context.device` block, which
/// `sauron_pipeline::enrich::device_info` folds into a `device_key`) the device
/// per virtual user, so freezing the fingerprint side is all that is left.
fn canonical_slot(user: &VirtualUser, distinct_issues: usize) -> usize {
    user.index % distinct_issues.max(1)
}

/// Whether this tick emits a repeat rather than the historical rotation.
///
/// Deterministic in `(user.index, seq)` — no rand crate, so a rerun of the same
/// command emits the same duplicate distribution. The constants differ from
/// [`workflow_tag`]'s so the two draws stay uncorrelated: sharing a mixer would
/// make `--workflow-ratio 0.5 --repeat-ratio 0.5` tag exactly the repeats.
fn is_repeat(user: &VirtualUser, seq: u64, ratio: f64) -> bool {
    if ratio <= 0.0 {
        return false;
    }
    let draw = mix64(
        (user.index as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)
            ^ seq.wrapping_add(0x5851_F42D_4C95_7F2D),
    );
    // 1/10_000 granularity and a strict `<`, matching `workflow_tag`: ratio 1.0
    // repeats everything (the largest bucket is 0.9999).
    (draw % 10_000) as f64 / 10_000.0 < ratio
}

/// How many of the error occurrences in `issue_envelope(user, seq, shape)` are
/// repeats.
///
/// Nothing on the wire marks an occurrence as a repeat — a genuine duplicate is
/// indistinguishable from a first sighting, which is the point — so the count
/// cannot be recovered from the envelope by `ItemCounts::of` and the engine asks
/// for it here instead. It re-runs [`is_repeat`], the same predicate
/// [`issue_items`] branched on, so the reported ratio cannot drift from the
/// emitted one.
pub fn repeat_count(user: &VirtualUser, seq: u64, shape: Shape) -> u64 {
    let batch = shape.batch_items.max(1);
    (0..batch)
        .filter(|&tick| is_repeat(user, tick_seq(seq, tick, batch), shape.repeat_ratio))
        .count() as u64
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
        let (batch_item, error) = issue_items(user, seq, shape);
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
///
/// # What a REPEAT holds identical, and what it lets drift
///
/// This split is the measurement, so it is spelled out rather than implied.
/// Held IDENTICAL across every repeat of one user's canonical issue:
///
/// * `exception.ty`, `exception.value`, `exception.mechanism`;
/// * every `stacktrace` frame, **including `lineno`/`colno`** — the fingerprint
///   drops line numbers, but the stored `error_events.stacktrace` JSONB does
///   not, and byte-identical frames are precisely what a content-addressed
///   stacktrace tier would fold away;
/// * `level` — it lands on `error_events` *and* on the `issues` upsert, so a
///   level that flapped would make the rows not-quite duplicates;
/// * `distinct_id`, `session_id` and `screen` (already per-user constants), and
///   the envelope's `context.device` block from which the backend derives
///   `device_key` (a constant descriptor for the whole run).
///
/// Left VARYING per occurrence, because real repeats vary there too and freezing
/// them would inflate any dedup win into something production could never
/// reproduce:
///
/// * `event_id` (a fresh UUID — occurrences are distinct rows by definition),
///   `timestamp`, and the wall-clock `breadcrumbs`;
/// * the `seq`-derived scalars inside `contexts` and `extra` (request id, order
///   id, cart value, latency bucket, ...). The single exception is
///   `extra.lineno`, which mirrors the frozen frame so the payload cannot
///   contradict the stacktrace.
fn issue_items(user: &VirtualUser, seq: u64, shape: Shape) -> (EnvelopeItem, EnvelopeItem) {
    let pick = user.index.wrapping_add(seq as usize);
    let repeat = is_repeat(user, seq, shape.repeat_ratio);
    let slot = if repeat {
        canonical_slot(user, shape.distinct_issues)
    } else {
        pick % shape.distinct_issues.max(1)
    };
    let (ty, value, crash_fn) = issue_identity(slot);
    let lineno = if repeat {
        canonical_lineno(slot)
    } else {
        BASE_LINENO + (seq % 50) as u32
    };
    let workflow = workflow_tag(user, seq, shape.workflow_ratio);

    let batch = EnvelopeItem::BreadcrumbBatch(BreadcrumbBatch {
        distinct_id: Some(user.distinct_id.clone()),
        session_id: Some(user.session_id.clone()),
        breadcrumbs: breadcrumbs(user, 3),
    });
    let error = EnvelopeItem::Error(Box::new(ErrorItem {
        event_id: Uuid::new_v4(),
        level: if repeat {
            // Per slot, not per seq: see the "held identical" list above.
            if slot % 7 == 0 {
                Level::Fatal
            } else {
                Level::Error
            }
        } else if seq % 7 == 0 {
            Level::Fatal
        } else {
            Level::Error
        },
        timestamp: Utc::now(),
        exception: Some(ExceptionInfo {
            ty: ty.to_string(),
            value: Some(value),
            mechanism: Some(Mechanism {
                ty: "onerror".to_string(),
                handled: Some(false),
            }),
            // Library padding first, then the two in-app identity frames, so
            // the trace reads crashing-last exactly as `pick_frames` documents
            // ("Frames arrive crashing-last, so we walk from the end").
            stacktrace: {
                let mut frames = pad_frames(slot, shape.stack_depth);
                frames.push(Frame {
                    function: Some("main".to_string()),
                    module: Some("app".to_string()),
                    filename: Some("main.rs".to_string()),
                    abs_path: None,
                    lineno: Some(10),
                    colno: Some(1),
                    in_app: Some(true),
                });
                frames.push(Frame {
                    function: Some(crash_fn),
                    module: Some("app::server".to_string()),
                    filename: Some("server.rs".to_string()),
                    abs_path: None,
                    lineno: Some(lineno),
                    colno: Some(5),
                    in_app: Some(true),
                });
                frames
            },
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

    // ---- repeat-heavy error mode ---------------------------------------------

    /// Every error item in `env`, as `(ExceptionInfo, distinct_id, session_id)`
    /// — the three things a "genuine duplicate" has to agree on.
    fn error_items(env: &Envelope) -> Vec<(ExceptionInfo, Option<String>, Option<String>)> {
        env.items
            .iter()
            .filter_map(|i| match i {
                EnvelopeItem::Error(e) => Some((
                    e.exception
                        .clone()
                        .expect("crebain always sends an exception"),
                    env.context.user.as_ref().and_then(|u| u.id.clone()),
                    e.session_id.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// The fingerprint the BACKEND would compute for this exception. Uses the
    /// real algorithm rather than a local re-implementation, so a change to the
    /// grouping rules shows up here instead of silently invalidating a run.
    fn backend_fingerprint(exc: &ExceptionInfo) -> String {
        sauron_core::fingerprint(Some(exc), None, None)
    }

    #[test]
    fn slot_tags_are_alphabetic_and_injective() {
        // Alphabetic: a numeric suffix would be erased by the backend's
        // `mask_volatile`, collapsing every slot onto one fingerprint.
        let mut seen = std::collections::BTreeSet::new();
        for slot in 0..2000 {
            let tag = slot_tag(slot);
            assert!(
                tag.chars().all(|c| c.is_ascii_lowercase()),
                "slot {slot} produced a non-alphabetic tag {tag:?}"
            );
            assert!(seen.insert(tag.clone()), "slot {slot} reused tag {tag:?}");
        }
    }

    /// The property the whole flag rests on: `--distinct-issues N` must yield N
    /// distinct BACKEND fingerprints. This is what catches the digit-masking
    /// trap — with a numeric call-site suffix every slot past the fifth would
    /// normalize to the same frame signature and N would silently collapse to 5.
    #[test]
    fn distinct_issues_yields_that_many_backend_fingerprints() {
        for n in [1usize, 2, 5, 6, 40, 300] {
            let shape = Shape {
                distinct_issues: n,
                repeat_ratio: 1.0,
                ..Shape::default()
            };
            let mut fps = std::collections::BTreeSet::new();
            // One user per slot, so every slot is reached via `canonical_slot`.
            for index in 0..n {
                let env = issue_envelope(&VirtualUser::new(index), 7, shape);
                for (exc, _, _) in error_items(&env) {
                    fps.insert(backend_fingerprint(&exc));
                }
            }
            assert_eq!(
                fps.len(),
                n,
                "--distinct-issues {n} produced {} groups",
                fps.len()
            );
        }
    }

    /// The default must reproduce the five fingerprints crebain has always had.
    #[test]
    fn the_default_is_still_five_issues_and_no_repeats() {
        let shape = Shape::default();
        assert_eq!(shape.distinct_issues, ERROR_TYPES.len());
        assert_eq!(shape.repeat_ratio, 0.0);
        let mut fps = std::collections::BTreeSet::new();
        for index in 0..50 {
            let user = VirtualUser::new(index);
            for seq in 0..40 {
                assert_eq!(repeat_count(&user, seq, shape), 0);
                for (exc, _, _) in error_items(&issue_envelope(&user, seq, shape)) {
                    // `.last()`, not `[1]`: the crash frame is the LAST frame
                    // (traces arrive crashing-last), and library padding now
                    // sits in front of it. Indexing positionally here asserted
                    // the padding frame instead.
                    assert_eq!(
                        exc.stacktrace.last().unwrap().function.as_deref(),
                        Some("handle_request"),
                        "the default must not tag the crash frame"
                    );
                    fps.insert(backend_fingerprint(&exc));
                }
            }
        }
        assert_eq!(fps.len(), ERROR_TYPES.len());
    }

    /// `--stack-depth` must be invisible to the fingerprint at every depth.
    ///
    /// The padding frames are `in_app: false` precisely so `pick_frames`
    /// (which pools in-app frames only, then takes the last 5) never sees
    /// them. If a future edit flips that flag, the identity-bearing crash
    /// frame gets pushed out of the 5-frame window and every issue silently
    /// collapses into one — a corrupt benchmark that still prints ok.
    #[test]
    fn stack_depth_never_changes_the_fingerprint() {
        let baseline: Vec<_> = (0..20)
            .map(|index| {
                let user = VirtualUser::new(index);
                let shape = Shape {
                    stack_depth: 2,
                    ..Shape::default()
                };
                let (exc, _, _) = error_items(&issue_envelope(&user, 0, shape))
                    .into_iter()
                    .next()
                    .unwrap();
                backend_fingerprint(&exc)
            })
            .collect();

        for depth in [2, 3, 8, 24, MAX_STACK_DEPTH] {
            let shape = Shape {
                stack_depth: depth,
                ..Shape::default()
            };
            let mut distinct = std::collections::BTreeSet::new();
            for (index, want) in baseline.iter().enumerate() {
                let user = VirtualUser::new(index);
                let (exc, _, _) = error_items(&issue_envelope(&user, 0, shape))
                    .into_iter()
                    .next()
                    .unwrap();
                assert_eq!(exc.stacktrace.len(), depth.max(2), "depth {depth}");
                let got = backend_fingerprint(&exc);
                assert_eq!(&got, want, "depth {depth} changed the fingerprint");
                distinct.insert(got);
            }
            // And the cardinality the whole experiment depends on survives.
            assert_eq!(
                distinct.len(),
                ERROR_TYPES.len(),
                "depth {depth} collapsed the issue set"
            );
        }
    }

    /// Everything two occurrences must agree on to be one duplicate: the
    /// backend fingerprint, plus the `(user, device, session)` tuple the
    /// pipeline rolls signals up by.
    #[derive(Debug, PartialEq)]
    struct DuplicateIdentity {
        fingerprint: String,
        frames: serde_json::Value,
        distinct_id: Option<String>,
        session_id: Option<String>,
        /// `device_key` is derived by the backend from `context.device`
        /// (`sauron_pipeline::enrich::device_info`), so comparing that block is
        /// comparing the device identity the backend will derive from it.
        device: serde_json::Value,
    }

    /// A repeat has to be a duplicate on BOTH axes: the same fingerprint and the
    /// same (user, device, session). Anything less lands on a different
    /// `error_events` grouping and measures the wrong thing.
    #[test]
    fn a_repeat_is_a_genuine_duplicate_on_both_axes() {
        let shape = Shape {
            repeat_ratio: 1.0,
            ..Shape::default()
        };
        for index in [0usize, 1, 7, 123] {
            let user = VirtualUser::new(index);
            let mut canonical: Option<DuplicateIdentity> = None;
            let mut occurrences = 0u32;
            for seq in 0..60 {
                let env = issue_envelope(&user, seq, shape);
                let device = serde_json::to_value(&env.context.device).unwrap();
                for (exc, distinct_id, session_id) in error_items(&env) {
                    let got = DuplicateIdentity {
                        fingerprint: backend_fingerprint(&exc),
                        frames: serde_json::to_value(&exc.stacktrace).unwrap(),
                        distinct_id,
                        session_id,
                        device: device.clone(),
                    };
                    occurrences += 1;
                    match &canonical {
                        None => canonical = Some(got),
                        Some(first) => {
                            assert_eq!(&got, first, "repeat diverged at seq {seq}")
                        }
                    }
                }
            }
            assert_eq!(occurrences, 60);
        }
    }

    /// `repeat_count` is what the summary reports, so it has to agree with what
    /// the BUILDER actually emitted — not merely with itself.
    ///
    /// Two properties, and the second one exists because of a real trap. The
    /// rotation can land on the canonical occurrence *by coincidence* (same
    /// slot, same crash line), and those occurrences are genuine duplicates
    /// too — they just cannot tell the two branches apart. So the exact
    /// equivalence is asserted only on unambiguous ticks, and the reported
    /// total is separately held to never exceed what was really frozen: the
    /// summary may understate the duplicate load, never inflate it.
    #[test]
    fn repeat_count_matches_the_occurrences_actually_frozen() {
        for batch in [1usize, 9] {
            let shape = Shape {
                repeat_ratio: 0.4,
                batch_items: batch,
                ..Shape::default()
            };
            let n = shape.distinct_issues;
            let mut checked = 0u32;
            for index in 0..40usize {
                let user = VirtualUser::new(index);
                let cs = canonical_slot(&user, n);
                let canonical = {
                    let always = Shape {
                        repeat_ratio: 1.0,
                        ..Shape::default()
                    };
                    let env = issue_envelope(&user, 0, always);
                    serde_json::to_value(&error_items(&env)[0].0).unwrap()
                };
                for seq in 0..15u64 {
                    let env = issue_envelope(&user, seq, shape);
                    let excs = error_items(&env);
                    assert_eq!(excs.len(), batch);

                    let mut frozen = 0u64;
                    for (tick, (exc, _, _)) in excs.iter().enumerate() {
                        let st = tick_seq(seq, tick, batch);
                        let is_canonical = serde_json::to_value(exc).unwrap() == canonical;
                        frozen += u64::from(is_canonical);
                        // The exception is fully determined by (slot, lineno),
                        // so the rotation reproduces the canonical one exactly
                        // when it draws the canonical slot on a matching line.
                        let ambiguous = user.index.wrapping_add(st as usize) % n == cs
                            && (st % 50) as usize == cs % 50;
                        if ambiguous {
                            continue;
                        }
                        assert_eq!(
                            is_canonical,
                            is_repeat(&user, st, shape.repeat_ratio),
                            "builder and counter disagree (user {index}, seq {seq}, tick {tick})"
                        );
                        checked += 1;
                    }
                    assert!(
                        repeat_count(&user, seq, shape) <= frozen,
                        "reported more repeats than were frozen (user {index}, seq {seq})"
                    );
                }
            }
            assert!(checked > 500, "only {checked} unambiguous ticks exercised");
        }
    }

    #[test]
    fn repeat_ratio_hits_roughly_that_proportion() {
        for target in [0.0, 0.25, 0.75, 1.0] {
            let shape = Shape {
                repeat_ratio: target,
                ..Shape::default()
            };
            let (mut repeats, mut total) = (0u64, 0u64);
            for index in 0..400 {
                let user = VirtualUser::new(index);
                for seq in 0..25 {
                    repeats += repeat_count(&user, seq, shape);
                    total += 1;
                }
            }
            let got = repeats as f64 / total as f64;
            // Deterministic, so not flaky; the tolerance asserts "the ratio is
            // honoured", not the bit pattern of the mixer. 0.0 and 1.0 are exact.
            assert!(
                (got - target).abs() <= 0.03,
                "--repeat-ratio {target} achieved {got:.4}"
            );
        }
    }

    /// Runs must stay reproducible: no rand crate, so the same inputs must
    /// rebuild the same item modulo the fields that are wall-clock by nature.
    #[test]
    fn repeat_selection_is_reproducible() {
        let shape = Shape {
            repeat_ratio: 0.5,
            distinct_issues: 17,
            ..Shape::default()
        };
        for index in 0..25 {
            let user = VirtualUser::new(index);
            for seq in 0..25 {
                assert_eq!(
                    repeat_count(&user, seq, shape),
                    repeat_count(&user, seq, shape)
                );
                let a = issue_envelope(&user, seq, shape);
                let b = issue_envelope(&user, seq, shape);
                assert_eq!(stable_json(&a.items[1]), stable_json(&b.items[1]));
            }
        }
    }

    /// The repeat draw must not alias the workflow draw. Sharing a mixer would
    /// make `--workflow-ratio 0.5 --repeat-ratio 0.5` tag exactly the repeats,
    /// quietly turning two independent knobs into one.
    #[test]
    fn repeat_and_workflow_draws_are_uncorrelated() {
        let shape = Shape {
            repeat_ratio: 0.5,
            workflow_ratio: 0.5,
            ..Shape::default()
        };
        let mut both = 0u32;
        let mut total = 0u32;
        for index in 0..500 {
            let user = VirtualUser::new(index);
            for seq in 0..20 {
                let r = repeat_count(&user, seq, shape) == 1;
                let w = workflow_tag(&user, seq, shape.workflow_ratio).is_some();
                total += 1;
                both += u32::from(r && w);
            }
        }
        // Independent halves overlap on ~25%; an aliased pair would sit at ~50%.
        let got = both as f64 / total as f64;
        assert!(
            (0.22..=0.28).contains(&got),
            "repeat/workflow overlap {got:.4} suggests the draws are correlated"
        );
    }
}
