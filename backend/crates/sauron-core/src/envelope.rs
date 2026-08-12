//! The Sauron ingest wire contract.
//!
//! One JSON [`Envelope`] carries a header, an envelope-wide context block, and a
//! list of tagged [`EnvelopeItem`]s (errors, product events, identify calls, or
//! a breadcrumb batch). Both SDKs (`@edraj/sauron-browser`, `sauron_flutter`) emit
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
    /// Dev-supplied structured context blocks (e.g. {"order":{"id":7}}). DISTINCT
    /// from the envelope-wide machine `context` — never conflate the two.
    #[serde(default)]
    pub contexts: serde_json::Value,
    /// Dev-supplied freeform JSON attached to this error.
    #[serde(default)]
    pub extra: serde_json::Value,
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
    /// Id of the workflow this error occurred within, if the SDK bounded one
    /// via `startWorkflow`/`endWorkflow`. Optional everywhere: apps that never
    /// use workflows must be byte-identical to before this field existed, so
    /// absent must serialize to nothing, never `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    /// Human-readable name of that workflow, denormalized alongside the id so
    /// downstream consumers don't need a join to display it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
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
    /// Id of the workflow this event occurred within, if the SDK bounded one
    /// via `startWorkflow`/`endWorkflow`. Optional everywhere: apps that never
    /// use workflows must be byte-identical to before this field existed, so
    /// absent must serialize to nothing, never `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    /// Human-readable name of that workflow, denormalized alongside the id so
    /// downstream consumers don't need a join to display it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// Current screen/route the SDK was on when the event was tracked.
    #[serde(default)]
    pub screen: Option<String>,
    /// Dev-supplied flat string tags for this track() event.
    #[serde(default)]
    pub tags: serde_json::Value,
    /// Dev-supplied structured context blocks (DISTINCT from machine `context`).
    #[serde(default)]
    pub contexts: serde_json::Value,
    /// Dev-supplied freeform JSON attached to this event.
    #[serde(default)]
    pub extra: serde_json::Value,
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
    /// Id of the workflow this transaction occurred within, if the SDK bounded
    /// one via `startWorkflow`/`endWorkflow`. Optional everywhere: apps that
    /// never use workflows must be byte-identical to before this field
    /// existed, so absent must serialize to nothing, never `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    /// Human-readable name of that workflow, denormalized alongside the id so
    /// downstream consumers don't need a join to display it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
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
    /// Resolved at the edge from the presented ingest key, never from client
    /// input. Not `Option`: a job cannot exist without a key, and a key cannot
    /// exist without an environment.
    pub environment_id: Uuid,
    #[serde(default)]
    pub release: Option<String>,
    pub received_at: DateTime<Utc>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub context: EnvelopeContext,
    /// Envelope-scoped SDK identity. `#[serde(default)]` is load-bearing: the
    /// queue is a Redis stream, so during a rolling upgrade jobs serialized by
    /// the previous ingest binary are still in flight and must keep
    /// deserializing against the new struct.
    #[serde(default)]
    pub sdk: Option<SdkInfo>,
    pub item: EnvelopeItem,
}

/// What the ingest edge actually enqueues: ONE envelope's shared tenancy and
/// request context, and every item that envelope carried.
///
/// The edge used to enqueue one [`IngestJob`] per item, which meant an SDK
/// sending 8 items in a batch had `app_id`, `project_id`, `org_id`,
/// `environment_id`, `release`, `received_at`, `ip`, `user_agent`, `context`
/// and `sdk` serialized, stored, read back and parsed **eight times over** —
/// once per item — for information the envelope only ever stated once. The
/// context block in particular is unbounded: it carries whatever `device`,
/// `os`, `app` and `runtime` maps the SDK attached.
///
/// That duplication was not only CPU. Stream entries are the unit
/// `MAXLEN ~ 1_000_000` counts, so N-times-larger, N-times-more-numerous
/// entries meant the trim threshold represented N times fewer real events —
/// and [the silent-loss soak] showed the trim is unalarmed and unconditional.
/// One entry per envelope makes the same cap cover the same traffic for
/// roughly `items_per_envelope` times longer.
///
/// Expanded back into per-item [`IngestJob`]s by the worker, so everything
/// downstream of the decode is untouched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestBatch {
    pub app_id: Uuid,
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub environment_id: Uuid,
    #[serde(default)]
    pub release: Option<String>,
    pub received_at: DateTime<Utc>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub context: EnvelopeContext,
    #[serde(default)]
    pub sdk: Option<SdkInfo>,
    /// CAN be empty. This previously read "never empty — the edge does not
    /// enqueue an envelope with no items", which is false: the accept handler
    /// bounds `items` only from above (`MAX_ENVELOPE_ITEMS`), and `items` is
    /// `#[serde(default)]` above, so a body with no `items` key is enqueued.
    /// Measured 2026-08-08 against an isolated ingest: 202 `{"accepted":0}` with
    /// XLEN +1. Anything expanding this must tolerate zero jobs.
    pub items: Vec<EnvelopeItem>,
}

impl IngestBatch {
    /// Expand into the per-item jobs the rest of the pipeline is written
    /// against.
    ///
    /// The shared header is cloned per item here, which is the same total copy
    /// the edge used to make — but in process, against an already-parsed
    /// structure, instead of through `serde_json::to_string`, a Redis round
    /// trip and `serde_json::from_str`.
    pub fn into_jobs(self) -> Vec<IngestJob> {
        self.into_jobs_counting_skew().0
    }

    /// [`Self::into_jobs`], plus how many timestamps [`EnvelopeItem::clamp_future`]
    /// had to rewrite — so the worker can meter clock skew instead of correcting
    /// it invisibly.
    pub fn into_jobs_counting_skew(mut self) -> (Vec<IngestJob>, usize) {
        let mut items = std::mem::take(&mut self.items);
        // THE chokepoint. Both the batched path and the legacy single-item path
        // reach the pipeline through here (`From<IngestJob> for IngestBatch`
        // wraps the old shape rather than carrying a second code path), so
        // clamping once here reaches every downstream consumer — the six
        // `.timestamp` reads across `batch.rs`/`process.rs` and every
        // `occurred_at` derived from them. Clamping at those call sites instead
        // would be six places to keep in agreement forever.
        let received_at = self.received_at;
        let skewed = items
            .iter_mut()
            .map(|it| it.clamp_future(received_at))
            .sum();
        let n = items.len();
        let mut out = Vec::with_capacity(n);
        for (i, item) in items.into_iter().enumerate() {
            // The final item MOVES the header instead of copying it, so a
            // single-item envelope — still the common case for an SDK that does
            // not batch — expands without allocating at all.
            if i + 1 == n {
                out.push(IngestJob {
                    app_id: self.app_id,
                    project_id: self.project_id,
                    org_id: self.org_id,
                    environment_id: self.environment_id,
                    release: self.release.take(),
                    received_at: self.received_at,
                    ip: self.ip.take(),
                    user_agent: self.user_agent.take(),
                    context: std::mem::take(&mut self.context),
                    sdk: self.sdk.take(),
                    item,
                });
                break;
            }
            out.push(IngestJob {
                app_id: self.app_id,
                project_id: self.project_id,
                org_id: self.org_id,
                environment_id: self.environment_id,
                release: self.release.clone(),
                received_at: self.received_at,
                ip: self.ip.clone(),
                user_agent: self.user_agent.clone(),
                context: self.context.clone(),
                sdk: self.sdk.clone(),
                item,
            });
        }
        (out, skewed)
    }
}

impl From<IngestJob> for IngestBatch {
    /// Wrap a legacy single-item job.
    ///
    /// The stream outlives a deploy: entries written by the previous binary are
    /// still pending when the new one starts reading, and the PEL can hand one
    /// back minutes later. The worker decodes into this shape either way rather
    /// than carrying two code paths past the parse.
    fn from(j: IngestJob) -> IngestBatch {
        IngestBatch {
            app_id: j.app_id,
            project_id: j.project_id,
            org_id: j.org_id,
            environment_id: j.environment_id,
            release: j.release,
            received_at: j.received_at,
            ip: j.ip,
            user_agent: j.user_agent,
            context: j.context,
            sdk: j.sdk,
            items: vec![j.item],
        }
    }
}

impl EventUser {
    /// The stable analytics identity for this user, if any.
    pub fn distinct_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

/// How far ahead of `received_at` a device clock may be before the pipeline
/// stops believing it.
///
/// Every item timestamp on the wire is the DEVICE's wall clock — the SDKs all
/// read it correctly (`DateTime.now().toUtc()`, `new Date().toISOString()`,
/// `datetime.now(timezone.utc)`, `DateTimeOffset.UtcNow`), so a wrong value
/// here means the phone itself is wrong, and nothing in the app running on it
/// can tell. Only the server, which knows when the envelope actually arrived,
/// is in a position to notice.
///
/// 15 minutes is measured, not guessed. Against the live `sessions` table on
/// 2026-08-12, positive skew (`started_at > created_at`) decayed smoothly:
/// 65% under one minute, 98.4% within six, then sparse — six rows between 15
/// and 60 minutes, and no pile-up against the hour. That decay is NTP drift
/// plus flush latency and MUST survive untouched; the 66 rows an hour or more
/// ahead (four of them days, one over a month) are the ones that sort to the
/// top of every `started_at desc` list and make a 4% problem look total.
///
/// Retuning: raise it if legitimate offline queues replay stale-but-forward
/// clocks; lower it only with a fresh version of that skew histogram in hand,
/// because the cost of clamping honest drift is silently reordered timelines.
pub const MAX_CLOCK_SKEW: chrono::Duration = chrono::Duration::minutes(15);

/// Pin `ts` to `received_at` when it claims to be more than [`MAX_CLOCK_SKEW`]
/// in the future. Returns whether it was rewritten, so callers can count.
///
/// Pinned to `received_at` rather than to the tolerance edge: a clamped value
/// then reads as "arrived now", which is the one thing about it we actually
/// know to be true.
fn clamp_one(ts: &mut DateTime<Utc>, received_at: DateTime<Utc>) -> bool {
    if *ts > received_at + MAX_CLOCK_SKEW {
        *ts = received_at;
        return true;
    }
    false
}

impl EnvelopeItem {
    /// Clamp every device-clock timestamp this item carries, returning how
    /// many were rewritten (for the skew counter — a clamp that happens
    /// silently is a clamp nobody ever fixes at the source).
    ///
    /// Deliberately covers nested breadcrumbs and a transaction's
    /// `finished_at` as well as the top-level `timestamp`: they all come off
    /// the same broken clock, and clamping only the outer one would leave a
    /// breadcrumb trail dated after the crash it belongs to, or a span whose
    /// end precedes its start.
    pub fn clamp_future(&mut self, received_at: DateTime<Utc>) -> usize {
        let mut n = 0;
        match self {
            EnvelopeItem::Error(e) => {
                n += clamp_one(&mut e.timestamp, received_at) as usize;
                for c in &mut e.breadcrumbs {
                    n += clamp_one(&mut c.timestamp, received_at) as usize;
                }
            }
            EnvelopeItem::Event(e) => n += clamp_one(&mut e.timestamp, received_at) as usize,
            EnvelopeItem::Identify(i) => n += clamp_one(&mut i.timestamp, received_at) as usize,
            EnvelopeItem::BreadcrumbBatch(b) => {
                for c in &mut b.breadcrumbs {
                    n += clamp_one(&mut c.timestamp, received_at) as usize;
                }
            }
            EnvelopeItem::Transaction(t) => {
                n += clamp_one(&mut t.timestamp, received_at) as usize;
                if let Some(f) = t.finished_at.as_mut() {
                    n += clamp_one(f, received_at) as usize;
                }
            }
        }
        n
    }
}

#[cfg(test)]
mod clock_skew_tests {
    use super::*;
    use chrono::Duration;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// `received_at` for every case below. Item timestamps are expressed
    /// relative to this so the fixtures read as skew, not as dates.
    fn recv() -> DateTime<Utc> {
        at("2026-08-12T18:00:00Z")
    }

    fn event_at(ts: DateTime<Utc>) -> EnvelopeItem {
        EnvelopeItem::Event(AnalyticsItem {
            name: "checkout".into(),
            distinct_id: "u1".into(),
            properties: serde_json::Value::Null,
            timestamp: ts,
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: serde_json::Value::Null,
            contexts: serde_json::Value::Null,
            extra: serde_json::Value::Null,
        })
    }

    fn crumb_at(ty: &str, ts: DateTime<Utc>) -> Breadcrumb {
        Breadcrumb {
            ty: ty.into(),
            category: None,
            message: None,
            level: None,
            timestamp: ts,
            data: serde_json::Value::Null,
        }
    }

    fn error_at(ts: DateTime<Utc>, breadcrumbs: Vec<Breadcrumb>) -> EnvelopeItem {
        EnvelopeItem::Error(Box::new(ErrorItem {
            event_id: Uuid::new_v4(),
            level: Level::Error,
            timestamp: ts,
            exception: None,
            message: Some("boom".into()),
            breadcrumbs,
            tags: serde_json::Value::Null,
            contexts: serde_json::Value::Null,
            extra: serde_json::Value::Null,
            fingerprint: None,
            user: None,
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            raw_stacktrace: None,
            debug_meta: None,
        }))
    }

    fn txn_at(ts: DateTime<Utc>, finished_at: Option<DateTime<Utc>>) -> EnvelopeItem {
        EnvelopeItem::Transaction(TransactionItem {
            name: "/checkout".into(),
            op: "navigation".into(),
            duration_ms: 12.0,
            status: None,
            http_method: None,
            http_status: None,
            url: None,
            distinct_id: None,
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            timestamp: ts,
            finished_at,
        })
    }

    fn timestamp_of(item: &EnvelopeItem) -> DateTime<Utc> {
        match item {
            EnvelopeItem::Event(e) => e.timestamp,
            EnvelopeItem::Error(e) => e.timestamp,
            EnvelopeItem::Identify(i) => i.timestamp,
            EnvelopeItem::Transaction(t) => t.timestamp,
            EnvelopeItem::BreadcrumbBatch(b) => b.breadcrumbs[0].timestamp,
        }
    }

    /// The overwhelmingly common case: the device clock is BEHIND or equal,
    /// because the event genuinely happened before it was received. Nothing
    /// may be rewritten here — this is the 96% of real traffic.
    #[test]
    fn past_timestamps_are_never_touched() {
        let ts = recv() - Duration::hours(3);
        let mut item = event_at(ts);
        assert_eq!(item.clamp_future(recv()), 0);
        assert_eq!(
            timestamp_of(&item),
            ts,
            "a past event must survive verbatim"
        );
    }

    /// Measured 2026-08-12 against the live `sessions` table: positive skew
    /// decays smoothly — 65% under a minute, 98.4% within six. That is NTP
    /// drift plus flush latency, not a broken clock, and rewriting it would
    /// corrupt ordering for the bulk of traffic to fix nothing.
    #[test]
    fn drift_inside_the_tolerance_is_left_alone() {
        for minutes in [0, 1, 5, 14] {
            let ts = recv() + Duration::minutes(minutes);
            let mut item = event_at(ts);
            assert_eq!(item.clamp_future(recv()), 0, "{minutes}m must not clamp");
            assert_eq!(timestamp_of(&item), ts, "{minutes}m must survive verbatim");
        }
    }

    /// The 66 rows that put `next month` and `in 10 hours` at the top of a
    /// `started_at desc` list. Pinned to `received_at` — NOT to the tolerance
    /// edge, so a clamped row reads as "arrived now", which is true.
    #[test]
    fn far_future_is_pinned_to_received_at() {
        for skew in [
            Duration::minutes(16),
            Duration::hours(10),
            Duration::days(31),
        ] {
            let mut item = event_at(recv() + skew);
            assert_eq!(item.clamp_future(recv()), 1, "{skew} must clamp");
            assert_eq!(
                timestamp_of(&item),
                recv(),
                "{skew} must pin to received_at"
            );
        }
    }

    /// Every variant carries a device-clock timestamp; missing one leaves a
    /// hole that only shows up as a wrong chart months later.
    #[test]
    fn every_variant_is_covered() {
        let far = recv() + Duration::days(31);
        let mut items = vec![
            event_at(far),
            error_at(far, vec![crumb_at("navigation", far)]),
            EnvelopeItem::Identify(IdentifyItem {
                distinct_id: "u1".into(),
                anonymous_id: None,
                traits: serde_json::Value::Null,
                timestamp: far,
            }),
            txn_at(far, Some(far)),
            EnvelopeItem::BreadcrumbBatch(BreadcrumbBatch {
                distinct_id: None,
                session_id: None,
                breadcrumbs: vec![crumb_at("navigation", far)],
            }),
        ];
        for item in &mut items {
            assert!(item.clamp_future(recv()) > 0, "variant left unclamped");
            assert_eq!(timestamp_of(item), recv(), "variant not pinned");
        }
    }

    /// A nested breadcrumb rides the same broken clock as its parent error, so
    /// clamping the error alone would leave the trail dated after the crash.
    #[test]
    fn nested_breadcrumbs_clamp_independently_of_the_parent() {
        // Parent is fine; only the second breadcrumb is skewed.
        let mut item = error_at(
            recv() - Duration::minutes(1),
            vec![
                crumb_at("ok", recv() - Duration::minutes(2)),
                crumb_at("skewed", recv() + Duration::days(31)),
            ],
        );
        assert_eq!(item.clamp_future(recv()), 1, "only the skewed crumb counts");
        let EnvelopeItem::Error(e) = &item else {
            unreachable!()
        };
        assert_eq!(
            e.timestamp,
            recv() - Duration::minutes(1),
            "parent untouched"
        );
        assert_eq!(e.breadcrumbs[0].timestamp, recv() - Duration::minutes(2));
        assert_eq!(e.breadcrumbs[1].timestamp, recv());
    }

    /// A transaction's `finished_at` shares the clock with its `timestamp`;
    /// clamping one and not the other invents a negative-length span.
    #[test]
    fn transaction_finished_at_clamps_too() {
        let far = recv() + Duration::days(31);
        let mut item = txn_at(far, Some(far));
        assert_eq!(item.clamp_future(recv()), 2, "timestamp and finished_at");
        let EnvelopeItem::Transaction(t) = &item else {
            unreachable!()
        };
        assert_eq!(t.timestamp, recv());
        assert_eq!(t.finished_at, Some(recv()));
    }

    /// The whole point of putting the clamp in `into_jobs`: it is the one
    /// chokepoint both the batched and the legacy single-item paths pass
    /// through, so no downstream consumer can be reached un-clamped.
    #[test]
    fn into_jobs_clamps_every_item() {
        let far = recv() + Duration::days(31);
        let batch = IngestBatch {
            app_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            release: None,
            received_at: recv(),
            ip: None,
            user_agent: None,
            context: EnvelopeContext::default(),
            sdk: None,
            // More than one item, so the moved-header final-item branch and
            // the cloned-header branch are both exercised.
            items: vec![event_at(far), event_at(far), event_at(recv())],
        };
        let jobs = batch.into_jobs();
        assert_eq!(jobs.len(), 3);
        for j in &jobs {
            assert_eq!(timestamp_of(&j.item), recv(), "escaped the chokepoint");
        }
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

    /// A stale SDK that has not yet dropped `environment` from the header it
    /// sends must keep ingesting: the field no longer exists on
    /// `EnvelopeHeader`, and serde ignores unknown fields by default, so the
    /// value is silently dropped rather than rejected. The environment for
    /// this envelope comes from the ingest key, not this string.
    #[test]
    fn tolerates_stale_sdk_still_sending_environment_in_header() {
        let json = r#"{
          "header": {
            "sdk": { "name": "sauron.javascript", "version": "0.1.0" },
            "environment": "production",
            "release": "web@1.4.2"
          },
          "items": []
        }"#;
        let env: Envelope =
            serde_json::from_str(json).expect("unknown `environment` field must be ignored");
        assert_eq!(env.header.sdk.name, "sauron.javascript");
        assert_eq!(env.header.release.as_deref(), Some("web@1.4.2"));
        // Re-serializing must not round-trip the field back onto the wire. Without
        // this the test would pass just as happily if `environment` were added back
        // to `EnvelopeHeader` tomorrow, which is the regression it exists to catch.
        let round_tripped = serde_json::to_string(&env.header).unwrap();
        assert!(
            !round_tripped.contains("environment"),
            "environment must not exist on the header: {round_tripped}"
        );
    }

    /// A job serialized by the pre-upgrade ingest binary carries no `sdk` key.
    /// Those are still sitting in the Redis stream during a rolling upgrade, so
    /// the new worker has to keep reading them — that is what the
    /// `#[serde(default)]` on `IngestJob::sdk` buys.
    #[test]
    fn ingest_job_from_a_previous_binary_still_deserializes() {
        let legacy = r#"{
            "app_id": "00000000-0000-0000-0000-000000000001",
            "project_id": "00000000-0000-0000-0000-000000000002",
            "org_id": "00000000-0000-0000-0000-000000000003",
            "environment_id": "00000000-0000-0000-0000-000000000004",
            "received_at": "2026-07-12T10:30:00Z",
            "item": { "type": "identify", "distinct_id": "u_123" }
        }"#;
        let job: IngestJob = serde_json::from_str(legacy).expect("legacy job must parse");
        assert!(job.sdk.is_none());
    }

    /// `sdk` is stored as a JSON object because the query catalog declares it a
    /// JSON root — `sdk.name:…` lowers to containment against `{"name":…}`.
    #[test]
    fn ingest_job_sdk_serializes_as_an_object() {
        let sdk = SdkInfo {
            name: "sauron.javascript".to_string(),
            version: "0.3.0".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&sdk).unwrap(),
            serde_json::json!({ "name": "sauron.javascript", "version": "0.3.0" })
        );
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
    fn parses_error_and_event_scopes() {
        // New dev-owned scopes: tags/contexts/extra parse on BOTH errors and events.
        let json = r#"{
            "header": { "sdk": { "name": "t", "version": "0" } },
            "items": [
                { "type": "error", "timestamp": "2026-07-20T10:00:00Z",
                  "exception": { "type": "X" },
                  "tags": { "region": "eu" },
                  "contexts": { "order": { "id": 7 } },
                  "extra": { "cart": [1, 2] } },
                { "type": "event", "name": "checkout", "distinct_id": "u1",
                  "tags": { "plan": "pro" },
                  "contexts": { "trip": { "n": 1 } },
                  "extra": { "note": "x" } }
            ]
        }"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        match &env.items[0] {
            EnvelopeItem::Error(e) => {
                assert_eq!(e.tags["region"], "eu");
                assert_eq!(e.contexts["order"]["id"], 7);
                assert_eq!(e.extra["cart"][1], 2);
            }
            other => panic!("expected error, got {other:?}"),
        }
        match &env.items[1] {
            EnvelopeItem::Event(ev) => {
                assert_eq!(ev.tags["plan"], "pro");
                assert_eq!(ev.contexts["trip"]["n"], 1);
                assert_eq!(ev.extra["note"], "x");
            }
            other => panic!("expected event, got {other:?}"),
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
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: serde_json::json!({ "tier": "gold" }),
            contexts: serde_json::json!({ "order": { "id": 7 } }),
            extra: serde_json::json!({ "trace": "abc" }),
        });
        let s = serde_json::to_string(&item).unwrap();
        assert!(s.contains("\"type\":\"event\""));
        let back: EnvelopeItem = serde_json::from_str(&s).unwrap();
        match back {
            EnvelopeItem::Event(ev) => {
                assert_eq!(ev.tags["tier"], "gold");
                assert_eq!(ev.contexts["order"]["id"], 7);
                assert_eq!(ev.extra["trace"], "abc");
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn workflow_fields_round_trip_on_event_item() {
        let json = r#"{
            "type": "event",
            "name": "checkout_step",
            "distinct_id": "u1",
            "properties": {},
            "timestamp": "2026-07-29T00:00:00Z",
            "workflow_id": "wf-123",
            "workflow_name": "checkout"
        }"#;
        let item: EnvelopeItem = serde_json::from_str(json).expect("parses");
        match item {
            EnvelopeItem::Event(e) => {
                assert_eq!(e.workflow_id.as_deref(), Some("wf-123"));
                assert_eq!(e.workflow_name.as_deref(), Some("checkout"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn workflow_fields_are_omitted_when_absent() {
        let json = r#"{
            "type": "event",
            "name": "plain",
            "distinct_id": "u1",
            "properties": {},
            "timestamp": "2026-07-29T00:00:00Z"
        }"#;
        let item: EnvelopeItem = serde_json::from_str(json).expect("parses");
        let back = serde_json::to_value(&item).expect("serializes");
        assert!(
            back.get("workflow_id").is_none(),
            "absent field must not serialize"
        );
        assert!(back.get("workflow_name").is_none());
    }

    fn item(name: &str) -> EnvelopeItem {
        serde_json::from_str(&format!(
            r#"{{"type":"event","name":"{name}","distinct_id":"u1",
                 "timestamp":"2026-07-29T00:00:00Z"}}"#
        ))
        .expect("fixture parses")
    }

    fn batch(items: Vec<EnvelopeItem>) -> IngestBatch {
        IngestBatch {
            app_id: Uuid::from_u128(1),
            project_id: Uuid::from_u128(2),
            org_id: Uuid::from_u128(3),
            environment_id: Uuid::from_u128(4),
            release: Some("1.2.3".to_string()),
            received_at: Utc::now(),
            ip: Some("10.0.0.1".to_string()),
            user_agent: Some("ua/1".to_string()),
            context: EnvelopeContext {
                device: serde_json::json!({"model": "pixel"}),
                ..Default::default()
            },
            sdk: Some(SdkInfo {
                name: "sauron.javascript".to_string(),
                version: "0.1.0".to_string(),
            }),
            items,
        }
    }

    /// Every item must come out carrying the SAME header the envelope stated
    /// once. This is the whole safety argument for enqueueing one entry
    /// instead of N: the worker has to reconstruct what the edge used to write.
    #[test]
    fn expanding_a_batch_gives_every_item_the_shared_header() {
        let b = batch(vec![item("a"), item("b"), item("c")]);
        let (app, ip, ctx, at) = (b.app_id, b.ip.clone(), b.context.clone(), b.received_at);
        let jobs = b.into_jobs();

        assert_eq!(jobs.len(), 3);
        for j in &jobs {
            assert_eq!(j.app_id, app);
            assert_eq!(j.ip, ip);
            assert_eq!(j.received_at, at);
            assert_eq!(j.release.as_deref(), Some("1.2.3"));
            assert_eq!(j.user_agent.as_deref(), Some("ua/1"));
            assert_eq!(
                j.sdk.as_ref().map(|s| s.name.as_str()),
                Some("sauron.javascript")
            );
            // The last item MOVES the context rather than cloning it. If that
            // move ever took the value from under its siblings this is what
            // would catch it.
            assert_eq!(
                serde_json::to_value(&j.context).unwrap(),
                serde_json::to_value(&ctx).unwrap(),
            );
        }
        let names: Vec<&str> = jobs
            .iter()
            .map(|j| match &j.item {
                EnvelopeItem::Event(e) => e.name.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(names, ["a", "b", "c"], "order must be preserved");
    }

    #[test]
    fn expanding_a_single_item_batch_yields_one_job() {
        let jobs = batch(vec![item("solo")]).into_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].context.device,
            serde_json::json!({"model": "pixel"})
        );
    }

    /// A stream entry written by the previous binary must still be readable.
    /// The two shapes are distinguished by `item` vs `items`, and neither
    /// struct will parse the other's payload — which is exactly what makes the
    /// worker's try-then-fall-back decode unambiguous.
    #[test]
    fn a_legacy_single_item_job_round_trips_through_the_batch_shape() {
        let job = batch(vec![item("legacy")]).into_jobs().remove(0);
        let wire = serde_json::to_string(&job).expect("serializes");

        assert!(
            serde_json::from_str::<IngestBatch>(&wire).is_err(),
            "the legacy shape must NOT parse as a batch, or the fallback would never run"
        );

        let back = IngestBatch::from(
            serde_json::from_str::<IngestJob>(&wire).expect("legacy shape still parses"),
        );
        assert_eq!(back.items.len(), 1);
        assert_eq!(back.app_id, job.app_id);
        assert_eq!(back.ip, job.ip);
        assert_eq!(back.release, job.release);
        assert_eq!(back.received_at, job.received_at);
    }

    #[test]
    fn the_batch_shape_does_not_parse_as_a_legacy_job() {
        let wire = serde_json::to_string(&batch(vec![item("a"), item("b")])).expect("serializes");
        assert!(
            serde_json::from_str::<IngestJob>(&wire).is_err(),
            "a batch must not silently decode as a single job and lose its other items"
        );
        assert_eq!(
            serde_json::from_str::<IngestBatch>(&wire)
                .expect("parses")
                .items
                .len(),
            2
        );
    }
}
