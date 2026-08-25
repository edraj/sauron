//! Process configuration, loaded from the environment.
//!
//! Both binaries read the same struct; each uses the subset it needs. Parsing
//! is deliberately hand-rolled (no config crate) so the mapping from env var to
//! field is completely predictable in a container.

use anyhow::Context;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub ingest_port: u16,
    pub api_port: u16,
    /// The validated JWT signing secret, or the reason it is unusable.
    ///
    /// Private on purpose: reach it through [`Config::require_jwt_secret`] so a
    /// missing secret surfaces as a startup error in the services that need one
    /// rather than silently becoming a usable key.
    jwt_secret: Result<String, String>,
    pub jwt_access_ttl_secs: i64,
    pub jwt_refresh_ttl_secs: i64,
    /// How often each API replica refreshes its revoked-session snapshot, in
    /// seconds. **This is the real kill latency** — a revoked session's access
    /// token keeps working on a replica until that replica's next poll.
    ///
    /// Clamped at the use site to 1..=60, so a fat-fingered `0` cannot spin the
    /// poller and a `3600` cannot silently restore a one-hour revocation window.
    pub auth_revocation_poll_secs: u64,
    /// `SAURON_DEV=1`. Today this only relaxes the `JWT_SECRET` rule, but it is
    /// promoted to a field because it is also the second half of the dev-sink
    /// body-logging gate, and S1 reads it. A local that three places need is a
    /// field.
    pub dev_mode: bool,
    pub worker_concurrency: usize,
    /// Cadence of the rollup fold task (seconds between scheduled folds).
    pub rollup_fold_secs: u64,
    /// Safety lag behind now() for scheduled folds: rows committing with an
    /// already-stamped received_at must land before the watermark passes them.
    pub rollup_lag_secs: i64,
    /// The much shorter lag used for operator-kicked folds (Refresh button).
    pub rollup_kick_lag_secs: i64,
    /// Per-(app, bucket) distinct-name soft cap; tail folds into '~other'.
    pub rollup_name_cap: usize,
    pub cors_allowed_origins: Vec<String>,
    pub ingest_rate_limit_per_min: u32,
    pub ingest_max_body_bytes: usize,
    /// Optional Unix-domain-socket path to listen on instead of TCP. When
    /// unset, `sauron-ingest` binds `ingest_port` on TCP as before.
    pub ingest_uds_path: Option<String>,
    /// TCP `listen()` backlog (ignored when `ingest_uds_path` is set).
    pub ingest_backlog: u32,
    /// Honour `X-Forwarded-For` / `X-Real-IP` on ingest. Only enable when a
    /// trusted reverse proxy sets them — they are client-controlled otherwise.
    pub ingest_trust_forwarded_headers: bool,
    /// Honour `X-Forwarded-For` / `X-Real-IP` on the dashboard API.
    ///
    /// The per-IP auth limiters key on the connecting address. Behind a reverse
    /// proxy — which the shipped nginx config and `packaging/rpm/SETUP.md` both
    /// recommend — that address is the proxy for *every* request, so the
    /// registration limit (10/hour) and login limit (60/min) applied to the
    /// whole deployment at once rather than per client. Same trust caveat as
    /// the ingest flag: only enable when a proxy you control sets the header.
    pub api_trust_forwarded_headers: bool,
    pub monitor_tick_ms: u64,
    pub monitor_batch: i64,
    pub monitor_max_concurrency: usize,
    pub monitor_check_retention_days: i64,
    pub monitor_ssrf_allow_private: bool,
    /// How often each app-store connection is re-synced. Store reports are
    /// daily and lag 1-3 days behind, so polling faster buys nothing.
    pub store_sync_interval_secs: i64,
    pub store_sync_max_concurrency: usize,
    /// How far back the *first* sync of a new connection reaches. Later syncs
    /// use a short window; re-reading a year of reports every tick is waste.
    pub store_backfill_days: i64,
    pub tier_hot_days: i64,
    pub tier_granularity: String,
    pub tier_cold_path: String,
    pub tier_drop_lag_hours: i64,
    pub tier_tick_secs: u64,
    pub tier_partition_ahead: i64,
    /// Days of raw `sessions` rows to keep; `0` (the default) keeps them
    /// forever. Enforced by dropping whole day partitions — sessions have no
    /// cold copy, the session-day rollups are the surviving record. Non-zero
    /// values below 7 are clamped up (`SESSION_RETENTION_MIN_DAYS`).
    pub session_retention_days: i64,
    /// How often `sauron-tier` looks for a queued restore job.
    ///
    /// Separate from `tier_tick_secs` (default 3600) on purpose: a restore is
    /// triggered by a human clicking a button and waiting, so it cannot inherit
    /// an hourly cadence. The tiering cycle and the restore poller run as two
    /// independent loops for exactly this reason.
    pub restore_poll_secs: u64,
    /// Lease before another worker may re-claim a `running` restore. A restore
    /// that outlives this without a heartbeat is treated as crashed and
    /// resumed; the resume deletes its own partial output first.
    pub restore_lease_secs: i64,
    /// Window a `Cost::Scan` search query (an unindexed wildcard/substring/
    /// free-text match) is clamped to — `sauron_db::query_plan::prepare`.
    /// Defaults to `tier_hot_days`: clamping a scan to more than the tier
    /// worker's hot window buys nothing, since older rows are already gone
    /// from Postgres, so that default is simultaneously the honest cost bound
    /// and the honest coverage bound. Replaces `sauron_db::repo::
    /// MAX_PAYLOAD_SEARCH_DAYS`, a constant that was unreachable dead code
    /// (every route already passed an explicit `since`).
    pub search_scan_clamp_days: i64,
    // --- symbolication / source maps ---
    /// In-process parsed-index LRU byte budget (megabytes).
    pub symbols_cache_mb: usize,
    /// Warm-blob Redis for symbol artifacts; `None` disables the tier (in-proc
    /// cache only). For true isolation point this at a SEPARATE Redis INSTANCE:
    /// `maxmemory` is instance-wide, so a different DB index on the ingest Redis
    /// would still let symbol blobs evict stream state. The per-blob size cap
    /// (`symbols_redis_max_blob_mb`) is the backstop when isolation isn't used.
    pub symbols_redis_url: Option<String>,
    /// Blobs larger than this are never cached in Redis (in-proc only).
    pub symbols_redis_max_blob_mb: usize,
    /// Reject uploads whose raw file exceeds this size.
    pub symbols_max_artifact_mb: usize,
    /// Decompression-bomb guard: cap on a blob's uncompressed size.
    pub symbols_max_uncompressed_mb: usize,
    /// Ingest-path symbolication time box; on timeout store raw + `pending`.
    pub symbols_ingest_timeout_ms: u64,
    // --- alerting / notifications ---
    /// Validated key material for AES-GCM encryption of notification-channel
    /// payloads (config AND secret), or the reason it is unusable.
    ///
    /// Private on purpose, exactly like [`Config::jwt_secret`]: reach it through
    /// [`Config::require_notify_secret_key`]. It used to be a bare `Option` that
    /// every consumer silently fell back to `JWT_SECRET` for, which failed OPEN
    /// twice over — rotating the JWT signing secret made every stored channel
    /// secret undecryptable with no error anywhere, and there was no length
    /// floor at all.
    notify_secret_key: Result<String, String>,
    /// Metric-rule evaluator cadence.
    pub alerts_tick_secs: u64,
    /// Per-delivery HTTP/SMTP timeout.
    pub alerts_deliver_timeout_ms: u64,
    /// Allow alert deliveries to private/loopback targets (self-hosted setups
    /// whose Slack-compatible endpoints or SMTP live on the LAN).
    pub alerts_allow_private: bool,
    /// How long `alert_events` rows are kept. The table records every
    /// evaluation, including suppressed ones, so it needs a reaper.
    pub alert_event_retention_days: i64,
    // --- personal notifications ---
    /// Personal-subscription evaluation cadence. Deliberately slower than the
    /// 30s org tick: personal email does not need 30s latency, and cadence is
    /// the single largest cost lever in this subsystem. Clamped 30..3600.
    pub notify_subs_tick_secs: u64,
    /// Rows one drain claim takes. Unclamped, the claim's `RETURNING *` is
    /// unbounded. Clamped 1..5000.
    pub notify_subs_batch: i64,
    /// Per-org probe ceiling. A single GLOBAL ceiling would be a cross-tenant
    /// starvation vector: self-registered accounts saturating it would silently
    /// stop evaluating a paying tenant's subscriptions. Clamped 1..1000.
    pub notify_subs_max_probes_per_org: usize,
    /// Wall-clock budget for one drain pass, so a backlog cannot stall the
    /// tick. Clamped 500..60000.
    pub notify_drain_budget_ms: u64,
    /// Above this, a user's surviving rows are merged into ONE digest rather
    /// than dropped. Clamped 1..1000.
    pub notify_max_emails_per_user_per_hour: i64,
    /// `notification_queue` retention. `0` would evaluate to
    /// `now() - '0 days'` and wipe the table, hence the clamp. Clamped 1..365.
    pub notify_queue_retention_days: i64,
    // --- transactional email ---
    /// The deployment-level relay, or the reason there isn't a usable one.
    ///
    /// Private on purpose: reach it through [`Config::require_smtp`]. `from_env`
    /// must not bail, because `sauron-ingest` and `sauron-tier` read this same
    /// struct and never read this field.
    smtp: Result<SmtpSettings, String>,
    /// The browser-facing origin of the dashboard SPA, or the reason there isn't
    /// one. In the shipped nginx topology this is NOT the API's origin — nginx
    /// serves the SPA and does not proxy the API — so nothing can derive it.
    ///
    /// Private on purpose: reach it through [`Config::require_dashboard_url`].
    dashboard_url: Result<String, String>,
    /// How often `sauron-api` drains `mail_outbox`.
    pub mail_drain_tick_secs: u64,
    /// How long terminal (`sent`/`failed`/`sink`) outbox rows are kept.
    pub mail_outbox_retention_days: i64,
    // --- pii inspector ---
    /// Master switch. OFF by default: the scanner reads the same partitions the
    /// ingest path writes, so a deployment opts in deliberately.
    pub inspector_enabled: bool,
    /// Scheduler-loop cadence. Clamped 5..3600.
    pub inspector_tick_secs: u64,
    /// Rows read per phase-1 batch. The LIMIT sits on an index-bounded inner
    /// window, so this bounds SCANNED rows, not matches.
    pub inspector_batch_rows: i64,
    /// Sleep between batches. This plus the batch size is the duty cycle that
    /// keeps the ingest working set resident.
    pub inspector_batch_pause_ms: u64,
    /// A scan whose heartbeat is older than this is re-claimable.
    pub inspector_lease_secs: i64,
    /// After this many claims a scan finalizes as `failed`, so one poison unit
    /// cannot loop forever.
    pub inspector_max_attempts: i32,
    /// Per-connection `SET statement_timeout`, applied at checkout and RESET
    /// before `drop(conn)` — deadpool's recycle does not reset session state.
    pub inspector_statement_timeout_ms: u64,
    /// Scan window ceiling. Defaults to `search_scan_clamp_days`, which itself
    /// defaults to `tier_hot_days`: nothing older is in Postgres anyway.
    pub inspector_window_days: i64,
    /// Detector mode reads every row in the window and walks every string leaf —
    /// roughly 20x the CPU and 20x the bytes shipped — so it gets its own,
    /// much shorter window.
    pub inspector_detector_window_days: i64,
    /// Phase-2 rows per unit before `match_count_exact = false` and
    /// `coverage = 'partial'`.
    pub inspector_max_phase2_rows_per_unit: i64,
    /// Truncation point for the `_default`-partition sweep.
    pub inspector_default_sweep_rows: i64,
    /// A missed scheduled run older than this is skipped, not replayed.
    pub inspector_catchup_grace_hours: i64,
    /// Scans retained per policy.
    pub inspector_scan_keep: i64,
    pub inspector_finding_retention_days: i64,
    /// Rows rewritten per mask batch. Halved automatically when any target
    /// carries a wildcard, because the array rebuild re-serializes the whole
    /// array per row.
    pub inspector_mask_batch: i64,
    pub inspector_mask_pause_ms: u64,
    /// Confirm refuses above this unless the ceiling is raised explicitly.
    pub inspector_mask_max_rows: i64,
    /// A mask action claimed longer ago than this is re-claimable (crash resume).
    pub inspector_claim_stale_secs: i64,
    /// Measured from `previewed_at` — the preview COMPLETING — not from the
    /// request, or a queued preview expires before it is readable.
    pub inspector_preview_ttl_secs: i64,

    // --- admin data purge ---
    /// Rows deleted per purge batch, per kind.
    pub purge_batch_rows: i64,
    /// Pause between purge batches, so a multi-million-row delete does not
    /// monopolise WAL and the buffer cache against live ingest.
    pub purge_batch_pause_ms: u64,
    /// Rollup keys recomputed per page in the purge's second phase. Also the
    /// size of the `IN` list sent to DuckDB for the cold half, so it bounds
    /// both the Postgres and the Parquet side of one step.
    pub purge_recompute_batch: i64,
    /// Measured from `previewed_at` — the preview COMPLETING — not from the
    /// request, for the same reason as the inspector's: a preview queued behind
    /// a long-running purge would otherwise expire before it was readable.
    pub purge_preview_ttl_secs: i64,
    /// A purge job claimed longer ago than this is re-claimable (crash resume).
    pub purge_claim_stale_secs: i64,
    /// How recently the app must have received an event for a job to record
    /// `ingest_active`. Only ever reported, never used to block.
    pub purge_ingest_active_secs: i64,
    pub inspector_preview_gc_days: i64,
    /// 0 = never prune. This table grows per human action, not per rule
    /// evaluation, and it is the record a compliance question is answered from.
    pub inspector_audit_retention_days: i64,
    /// Age at which staff emails and `confirm_source` are nulled, keeping counts
    /// and targets. Without this the privacy feature is the only un-erasable
    /// store of staff PII in the schema.
    pub inspector_audit_pii_days: i64,
    pub inspector_export_max_rows: i64,
    /// Read by BOTH `sauron-ingest` (the enforcer's cache TTL) and `sauron-api`
    /// (the number the UI states literally). Declared in `sauron.env`, never in
    /// `inspector.env`, or the two diverge silently.
    pub inspector_policy_cache_secs: u64,
    /// Read by BOTH `sauron-inspector` and `sauron-api`. Clamped against
    /// `inspector_policy_cache_secs` at load.
    pub inspector_tail_sweep_secs: u64,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written rather than derived. Nothing prints `Config` today, so
        // this is a latent leak — but S0 adds the most tempting one, and a single
        // `debug!(?cfg)` typed during an incident would otherwise dump the
        // Postgres password, the JWT signing key and the SMTP password into the
        // journal at once, where they outlive the process and reach a wider
        // reader set than the database does.
        //
        // A field added later and forgotten here simply does not print. That is
        // the safe direction to fail.
        const R: &str = "<redacted>";
        f.debug_struct("Config")
            .field("database_url", &R)
            .field("redis_url", &R)
            .field("ingest_port", &self.ingest_port)
            .field("api_port", &self.api_port)
            .field("jwt_secret", &self.jwt_secret.as_ref().map(|_| R))
            .field("jwt_access_ttl_secs", &self.jwt_access_ttl_secs)
            .field("jwt_refresh_ttl_secs", &self.jwt_refresh_ttl_secs)
            .field("dev_mode", &self.dev_mode)
            .field("worker_concurrency", &self.worker_concurrency)
            .field("rollup_fold_secs", &self.rollup_fold_secs)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("ingest_rate_limit_per_min", &self.ingest_rate_limit_per_min)
            .field("ingest_max_body_bytes", &self.ingest_max_body_bytes)
            .field("ingest_uds_path", &self.ingest_uds_path)
            .field("ingest_backlog", &self.ingest_backlog)
            .field(
                "ingest_trust_forwarded_headers",
                &self.ingest_trust_forwarded_headers,
            )
            .field(
                "api_trust_forwarded_headers",
                &self.api_trust_forwarded_headers,
            )
            .field("monitor_tick_ms", &self.monitor_tick_ms)
            .field("monitor_batch", &self.monitor_batch)
            .field("monitor_max_concurrency", &self.monitor_max_concurrency)
            .field("store_sync_interval_secs", &self.store_sync_interval_secs)
            .field(
                "store_sync_max_concurrency",
                &self.store_sync_max_concurrency,
            )
            .field("store_backfill_days", &self.store_backfill_days)
            .field(
                "monitor_check_retention_days",
                &self.monitor_check_retention_days,
            )
            .field(
                "monitor_ssrf_allow_private",
                &self.monitor_ssrf_allow_private,
            )
            .field("tier_hot_days", &self.tier_hot_days)
            .field("tier_granularity", &self.tier_granularity)
            .field("tier_cold_path", &self.tier_cold_path)
            .field("tier_drop_lag_hours", &self.tier_drop_lag_hours)
            .field("tier_tick_secs", &self.tier_tick_secs)
            .field("restore_poll_secs", &self.restore_poll_secs)
            .field("restore_lease_secs", &self.restore_lease_secs)
            .field("tier_partition_ahead", &self.tier_partition_ahead)
            .field("session_retention_days", &self.session_retention_days)
            .field("search_scan_clamp_days", &self.search_scan_clamp_days)
            .field("symbols_cache_mb", &self.symbols_cache_mb)
            .field(
                "symbols_redis_url",
                &self.symbols_redis_url.as_ref().map(|_| R),
            )
            .field("symbols_redis_max_blob_mb", &self.symbols_redis_max_blob_mb)
            .field("symbols_max_artifact_mb", &self.symbols_max_artifact_mb)
            .field(
                "symbols_max_uncompressed_mb",
                &self.symbols_max_uncompressed_mb,
            )
            .field("symbols_ingest_timeout_ms", &self.symbols_ingest_timeout_ms)
            .field(
                "notify_secret_key",
                &self.notify_secret_key.as_ref().map(|_| R),
            )
            // `Result::as_ref().map()` above redacts the Ok payload; the Err arm
            // is a reason string with no secret in it, so it prints as-is.
            .field("alerts_tick_secs", &self.alerts_tick_secs)
            .field("alerts_deliver_timeout_ms", &self.alerts_deliver_timeout_ms)
            .field("alerts_allow_private", &self.alerts_allow_private)
            .field(
                "alert_event_retention_days",
                &self.alert_event_retention_days,
            )
            .field("smtp", &self.smtp.as_ref().map(|_| R))
            .field("dashboard_url", &self.dashboard_url)
            .field("mail_drain_tick_secs", &self.mail_drain_tick_secs)
            .field(
                "mail_outbox_retention_days",
                &self.mail_outbox_retention_days,
            )
            .finish()
    }
}

/// Minimum accepted `JWT_SECRET` length. 32 chars is the shortest value that
/// still carries ~128 bits of entropy when generated as hex.
pub const MIN_JWT_SECRET_LEN: usize = 32;

pub const NOTIFY_SUBS_TICK_SECS_DEFAULT: u64 = 120;
pub const NOTIFY_SUBS_BATCH_DEFAULT: i64 = 200;
pub const NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT: usize = 50;
pub const NOTIFY_DRAIN_BUDGET_MS_DEFAULT: u64 = 10_000;
pub const NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT: i64 = 20;
pub const NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT: i64 = 14;

/// Only ever used when `SAURON_DEV=1` is explicitly set.
const DEV_JWT_SECRET: &str = "dev-insecure-change-me-please-0000000000000000";

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    var(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The tail sweep re-runs the enforcement seam once after a retro-mask. Its
/// window must exceed the pipeline's policy-cache TTL by a real margin or it
/// closes nothing at all. Clamped rather than `bail!`ed because
/// `Config::from_env` is shared by every binary — a bail here would take down
/// `sauron-ingest` over a setting it never reads.
pub fn clamp_tail_sweep(tail_sweep_secs: u64, policy_cache_secs: u64) -> u64 {
    tail_sweep_secs.max(policy_cache_secs.saturating_mul(4))
}

/// How the SMTP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Implicit TLS (SMTPS): handshake immediately, usually :465.
    Implicit,
    /// STARTTLS, and abort if the server will not upgrade. Usually :587.
    StartTls,
    /// Cleartext. Only ever accepted for a relay on this host — see
    /// [`build_smtp`] rule 6 and the matching structural check at connect time.
    None,
}

/// Deployment-level SMTP relay. Distinct from the per-org SMTP credentials in
/// `notification_channels`: this one carries mail addressed to a *person*, which
/// is why it must exist even for a user who belongs to no org.
#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: String,
    pub tls: SmtpTls,
    pub allow_private: bool,
    /// Waives the loopback requirement on `SMTP_TLS=none`. See
    /// [`build_smtp`]'s rule 6.
    pub insecure_plaintext: bool,
    pub timeout_ms: u64,
    pub sink: bool,
}

impl std::fmt::Debug for SmtpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // This struct is reachable from `Config`, and `Config` is the thing an
        // engineer reaches for with `debug!("{cfg:?}")` during an incident. A
        // `#[derive(Debug)]` here would put the relay password in the journal and
        // clippy would not say a word.
        f.debug_struct("SmtpSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from_address", &self.from_address)
            .field("from_name", &self.from_name)
            .field("tls", &self.tls)
            .field("allow_private", &self.allow_private)
            .field("insecure_plaintext", &self.insecure_plaintext)
            .field("timeout_ms", &self.timeout_ms)
            .field("sink", &self.sink)
            .finish()
    }
}

/// Hosts for which `SMTP_TLS=none` is accepted. Cleartext SMTP puts the relay
/// password and every password-reset link on the wire; the only topology where
/// that is defensible is a relay listening on this machine.
const SMTP_LOOPBACK_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "::1", "[::1]"];

/// Validate the SMTP settings without reading the environment.
///
/// Takes already-read values on purpose: env vars are process-global and
/// `cargo test` runs a binary's tests on threads, so a `build_smtp` that read
/// `std::env` could not be tested without racing every other test in the crate.
///
/// Returns `Err(reason)` rather than panicking or bailing. `Config::from_env` is
/// shared by every binary; a `bail!` here would take down `sauron-ingest` and
/// `sauron-tier` over a relay setting they never read — which is exactly what
/// happened to `jwt_secret` and is why that field is a recorded `Result` too.
#[allow(clippy::too_many_arguments)]
pub fn build_smtp(
    host: Option<String>,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    from_address: Option<String>,
    from_name: String,
    tls_raw: Option<String>,
    allow_private: bool,
    insecure_plaintext: bool,
    timeout_ms: u64,
    sink: bool,
) -> Result<SmtpSettings, String> {
    // 1. The dev sink needs no relay at all, so it must not be blocked by the
    //    host/from rules below. A developer with SMTP_SINK=1 and nothing else set
    //    still exercises every template, every enqueue and the whole outbox state
    //    machine.
    if sink && host.is_none() {
        return Ok(SmtpSettings {
            host: "(sink)".to_string(),
            port,
            username,
            password,
            from_address: from_address.unwrap_or_else(|| "sauron@localhost".to_string()),
            from_name,
            tls: SmtpTls::StartTls,
            allow_private,
            insecure_plaintext,
            timeout_ms: timeout_ms.clamp(1_000, 60_000),
            sink: true,
        });
    }

    // 2. No relay configured. This is the ordinary state of a deployment that has
    //    not enabled transactional email, so the message tells an operator what to
    //    set rather than reading as a fault.
    let host = host.ok_or_else(|| {
        "SMTP_HOST is not set; transactional email is disabled. Set SMTP_HOST/SMTP_FROM, \
         or SMTP_SINK=1 to log mail instead of sending it."
            .to_string()
    })?;

    // 3-4. A From address that lettre will reject at send time is a message that
    //      fails eight times in a retry loop and reaches nobody. Catch the obvious
    //      shapes at boot, where a human is looking. Real parsing still happens in
    //      lettre.
    let from_address =
        from_address.ok_or_else(|| "SMTP_FROM is required when SMTP_HOST is set".to_string())?;
    let at_count = from_address.matches('@').count();
    let (local, domain) = from_address.split_once('@').unwrap_or(("", ""));
    if at_count != 1
        || local.is_empty()
        || domain.is_empty()
        || from_address.chars().any(|c| c.is_whitespace())
    {
        return Err(format!(
            "SMTP_FROM must be a bare address with exactly one '@' and no whitespace, \
             e.g. sauron@example.com (got {from_address:?})"
        ));
    }

    // 5. Unset follows the port, the same rule notification-channel resolution
    //    uses for `implicit_tls`.
    let tls = match tls_raw.as_deref().map(str::trim) {
        None | Some("") => {
            if port == 465 {
                SmtpTls::Implicit
            } else {
                SmtpTls::StartTls
            }
        }
        Some(v) if v.eq_ignore_ascii_case("implicit") || v.eq_ignore_ascii_case("smtps") => {
            SmtpTls::Implicit
        }
        Some(v) if v.eq_ignore_ascii_case("starttls") || v.eq_ignore_ascii_case("required") => {
            SmtpTls::StartTls
        }
        Some(v) if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("plain") => {
            SmtpTls::None
        }
        Some(other) => {
            return Err(format!(
                "SMTP_TLS={other:?} is not recognised; accepted values are \
                 implicit (or smtps), starttls (or required), none (or plain)"
            ))
        }
    };

    // 6. The syntactic half of the loopback rule. The structural half runs against
    //    the RESOLVED address inside `SmtpClient::connect`, which is what survives
    //    a `localhost` that has been pointed somewhere else. Both exist: this one
    //    is loud and early, that one is true.
    //
    //    Deliberately NOT gated on SMTP_ALLOW_PRIVATE. That flag would then be the
    //    only consent gate for shipping reset links across a LAN, and it is a flag
    //    an operator may have set for an unrelated internal webhook.
    //
    //    SMTP_INSECURE_PLAINTEXT is the escape hatch, and it is its own variable
    //    for the same reason: a LAN relay that genuinely cannot do TLS is a real
    //    deployment, but reaching it must be a single-purpose, explicitly-named
    //    act. The name is the documentation — an operator cannot set this one and
    //    later claim they did not know what it bought.
    if tls == SmtpTls::None && !insecure_plaintext && !SMTP_LOOPBACK_HOSTS.contains(&host.as_str())
    {
        return Err(format!(
            "SMTP_TLS=none sends the SMTP password and password-reset links in cleartext \
             and is only accepted for a relay on this host; SMTP_HOST={host} is not loopback. \
             Use SMTP_TLS=starttls, put a local relay in front, or set \
             SMTP_INSECURE_PLAINTEXT=true to accept cleartext on your network."
        ));
    }

    Ok(SmtpSettings {
        host,
        port,
        username,
        password,
        from_address,
        from_name,
        tls,
        allow_private,
        insecure_plaintext,
        // 7. Same bounds `AlertEngine::new` applies, so the two delivery paths
        //    cannot be tuned into disagreement.
        timeout_ms: timeout_ms.clamp(1_000, 60_000),
        sink,
    })
}

impl Config {
    /// The JWT signing secret, or an error explaining why there isn't a usable
    /// one. Call this from services that mint/verify tokens or derive the
    /// channel-secret key (`sauron-api`, `sauron-monitor`, `sauron-alerts`);
    /// they should propagate the error and refuse to start.
    pub fn require_jwt_secret(&self) -> anyhow::Result<&str> {
        match &self.jwt_secret {
            Ok(s) => Ok(s.as_str()),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }

    /// The notification-channel encryption key, or an error explaining why there
    /// isn't a usable one. Call this from every service that reads or writes
    /// notification channels (`sauron-api`, `sauron-monitor`, `sauron-alerts`);
    /// they must propagate the error and refuse to start.
    ///
    /// Fails CLOSED, and the failure mode matters more here than anywhere else
    /// in this file: this key is the ONLY thing that can decrypt a channel's
    /// stored config and secret. There is no escrow and no derivation from
    /// another value — the previous `JWT_SECRET` fallback was exactly that, and
    /// it turned a routine JWT rotation into silent, total loss of every stored
    /// channel credential. Lose this key and the ciphertext is unrecoverable:
    /// the only remedy is to delete and re-create every notification channel.
    ///
    /// Recorded rather than raised at load time for the same reason as
    /// `jwt_secret`: `sauron-ingest`, `sauron-tier` and `sauron-migrate` never
    /// touch channels and must still boot without it.
    pub fn require_notify_secret_key(&self) -> anyhow::Result<&str> {
        match &self.notify_secret_key {
            Ok(s) => Ok(s.as_str()),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }

    /// The configured SMTP relay, or an error explaining why there isn't one.
    ///
    /// Fails closed at the point of use. Callers must degrade rather than refuse
    /// to boot: a deployment with no relay has to serve everything else.
    pub fn require_smtp(&self) -> anyhow::Result<&SmtpSettings> {
        match &self.smtp {
            Ok(s) => Ok(s),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }

    /// The dashboard's browser-facing origin, trailing slashes already stripped.
    ///
    /// This is what makes "any email containing a link requires DASHBOARD_URL"
    /// enforceable: `sauron_mail::Branding::link` refuses to build a URL without
    /// it, rather than guessing an origin and sending a link to nowhere that
    /// every server-side signal reports as delivered.
    pub fn require_dashboard_url(&self) -> anyhow::Result<&str> {
        match &self.dashboard_url {
            Ok(s) => Ok(s.as_str()),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }

    /// Load configuration from environment variables. Only `DATABASE_URL` is
    /// strictly required; everything else has a sensible default.
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = var("DATABASE_URL")
            .context("DATABASE_URL is required (e.g. postgres://sauron:sauron@localhost/sauron)")?;

        // Fail CLOSED on the signing secret. Tokens are HS256, so the signing
        // key is symmetric: booting with a compiled-in default in a public
        // repository would let anyone forge an access token for any user id.
        // A weak/absent secret is only tolerated when the operator explicitly
        // opts into dev mode.
        //
        // The failure is *recorded*, not raised: `Config` is shared by every
        // binary, but only those that mint/verify tokens or derive the
        // channel-secret key actually need a secret. Bailing here took down
        // `sauron-ingest` and `sauron-tier` — which never read it — whenever a
        // deployment set JWT_SECRET on the API alone. `require_jwt_secret()`
        // raises it at the point of use instead, so the fail-closed guarantee
        // is unchanged for the services that matter.
        let dev_mode = var("SAURON_DEV")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let jwt_secret = match var("JWT_SECRET") {
            Some(s) if s.len() >= MIN_JWT_SECRET_LEN => Ok(s),
            Some(s) if dev_mode => Ok(s),
            Some(_) => Err(format!(
                "JWT_SECRET must be at least {MIN_JWT_SECRET_LEN} characters \
                 (set SAURON_DEV=1 to override for local development only)"
            )),
            None if dev_mode => Ok(DEV_JWT_SECRET.to_string()),
            None => Err(
                "JWT_SECRET is required — generate one with `openssl rand -hex 32` \
                 (set SAURON_DEV=1 to run with an insecure development key)"
                    .to_string(),
            ),
        };

        // Same fail-closed shape as JWT_SECRET, with no fallback of any kind.
        // The old behaviour — silently derive from JWT_SECRET behind a `warn!` —
        // is what made this key dangerous: it booted fine, encrypted real
        // credentials under a key the operator never chose, and then lost them
        // the next time JWT_SECRET was rotated. The same 32-character floor
        // applies; there was previously none at all, so a one-character
        // NOTIFY_SECRET_KEY was accepted.
        //
        // No SAURON_DEV escape hatch: a dev-mode default here would be a
        // *storage* key, so switching in or out of dev mode would silently make
        // existing rows undecryptable. Local development sets the variable.
        let notify_secret_key = match var("NOTIFY_SECRET_KEY") {
            Some(s) if s.len() >= MIN_JWT_SECRET_LEN => Ok(s),
            Some(_) => Err(format!(
                "NOTIFY_SECRET_KEY must be at least {MIN_JWT_SECRET_LEN} characters"
            )),
            None => Err(
                "NOTIFY_SECRET_KEY is required — generate one with `openssl rand -hex 32`. \
                 It must be IDENTICAL across api, monitor and alerts, and it must be backed \
                 up: it is the only key that can decrypt stored notification-channel \
                 configs and secrets, and losing it means every channel has to be \
                 re-created. Upgrading a deployment that relied on the old JWT_SECRET \
                 fallback? Set NOTIFY_SECRET_KEY to that deployment's existing JWT_SECRET \
                 value to keep the existing rows readable."
                    .to_string(),
            ),
        };

        let cors_allowed_origins = var("CORS_ALLOWED_ORIGINS")
            .unwrap_or_else(|| "http://localhost:3000".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // The SPA origin. Validated here so a typo is a boot-time message rather
        // than a message that renders, sends, reaches 'sent', and lands in a
        // mailbox with a link to nowhere.
        let dashboard_url = match var("DASHBOARD_URL") {
            None => Err(
                "DASHBOARD_URL is not set; any email containing a link cannot be \
                         rendered. Set it to the browser-facing origin of the dashboard, \
                         e.g. https://sauron.example.com"
                    .to_string(),
            ),
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
                Ok(u.trim_end_matches('/').to_string())
            }
            Some(u) => Err(format!(
                "DASHBOARD_URL must start with http:// or https:// (got {u:?})"
            )),
        };

        let smtp = build_smtp(
            var("SMTP_HOST"),
            parse("SMTP_PORT", 587u16),
            var("SMTP_USERNAME"),
            var("SMTP_PASSWORD"),
            var("SMTP_FROM"),
            var("SMTP_FROM_NAME").unwrap_or_else(|| "Sauron".to_string()),
            var("SMTP_TLS"),
            // Deliberately NOT inheriting ALERTS_ALLOW_PRIVATE: that flag unlocks
            // private delivery for USER-SUPPLIED webhook URLs, a strictly larger
            // surface. Declaring a LAN Slack endpoint is not declaring anything
            // about the relay.
            var("SMTP_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            // Separate from SMTP_ALLOW_PRIVATE on purpose. That flag says "the
            // relay is on my network"; this one says "and I accept that its
            // password and every password-reset link cross that network in
            // clear". Folding the second into the first would make one variable
            // silently buy both.
            var("SMTP_INSECURE_PLAINTEXT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            parse("SMTP_TIMEOUT_MS", 10_000u64),
            // Deliberately NOT inheriting SAURON_DEV: that variable exists to get
            // past a JWT_SECRET complaint, and an operator who sets it during a
            // stalled first boot must not thereby convert every reset link into a
            // log line.
            var("SMTP_SINK")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        );

        // Read once so `search_scan_clamp_days`'s default can track it below.
        let tier_hot_days = parse("TIER_HOT_DAYS", 30);
        // Bound once so `inspector_window_days` inherits the SAME ceiling the
        // query planner clamps a scan to. Two independent `parse` calls would
        // let an operator raise one and not the other, and the inspector would
        // then report coverage for days the search path cannot reach.
        let search_scan_clamp_days = parse("SEARCH_SCAN_CLAMP_DAYS", tier_hot_days);
        // Read once so the tail-sweep clamp can be computed against it below. Both
        // keys live in `sauron.env`, not `inspector.env`: `sauron-ingest` and
        // `sauron-api` never read `inspector.env`, so the "about 30 seconds" the API
        // reports to the UI would otherwise diverge from what the enforcer uses.
        let policy_cache_secs: u64 = parse("INSPECTOR_POLICY_CACHE_SECS", 30);

        Ok(Self {
            database_url,
            redis_url: var("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),
            ingest_port: parse("INGEST_PORT", 8081),
            api_port: parse("API_PORT", 8080),
            jwt_secret,
            jwt_access_ttl_secs: parse("JWT_ACCESS_TTL_SECS", 900),
            jwt_refresh_ttl_secs: parse("JWT_REFRESH_TTL_SECS", 2_592_000),
            auth_revocation_poll_secs: parse("AUTH_REVOCATION_POLL_SECS", 5),
            // 8, raised from 4. The two knobs interact, and sweeping either
            // alone gets the wrong answer: at the old batch size of 50, going
            // from 4 workers to 16 made throughput WORSE, which is what a
            // one-dimensional sweep would have concluded. Measured together, 8
            // workers at a batch of 200 more than doubled the write rate.
            // `INGEST_DB_POOL` must stay >= this.
            worker_concurrency: parse("WORKER_CONCURRENCY", 8),
            rollup_fold_secs: parse("ROLLUP_FOLD_SECS", 60),
            rollup_lag_secs: parse("ROLLUP_LAG_SECS", 60),
            rollup_kick_lag_secs: parse("ROLLUP_KICK_LAG_SECS", 2),
            rollup_name_cap: parse("ROLLUP_NAME_CAP", 2000),
            cors_allowed_origins,
            ingest_rate_limit_per_min: parse("INGEST_RATE_LIMIT_PER_MIN", 6000),
            ingest_max_body_bytes: parse("INGEST_MAX_BODY_BYTES", 1_048_576),
            ingest_uds_path: var("INGEST_UDS_PATH"),
            ingest_backlog: parse("INGEST_BACKLOG", 4096),
            ingest_trust_forwarded_headers: var("INGEST_TRUST_FORWARDED_HEADERS")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            api_trust_forwarded_headers: var("API_TRUST_FORWARDED_HEADERS")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            monitor_tick_ms: parse("MONITOR_TICK_MS", 1000),
            monitor_batch: parse("MONITOR_BATCH", 100),
            monitor_max_concurrency: parse("MONITOR_MAX_CONCURRENCY", 50),
            monitor_check_retention_days: parse("MONITOR_CHECK_RETENTION_DAYS", 30),
            monitor_ssrf_allow_private: var("MONITOR_SSRF_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            store_sync_interval_secs: parse("STORE_SYNC_INTERVAL_SECS", 21_600),
            store_sync_max_concurrency: parse("STORE_SYNC_MAX_CONCURRENCY", 8),
            store_backfill_days: parse("STORE_BACKFILL_DAYS", 90),
            tier_hot_days,
            tier_granularity: var("TIER_GRANULARITY").unwrap_or_else(|| "day".to_string()),
            tier_cold_path: var("TIER_COLD_PATH")
                .unwrap_or_else(|| "/var/lib/sauron/cold".to_string()),
            tier_drop_lag_hours: parse("TIER_DROP_LAG_HOURS", 24),
            tier_tick_secs: parse("TIER_TICK_SECS", 3600),
            tier_partition_ahead: parse("TIER_PARTITION_AHEAD", 7),
            session_retention_days: parse("SESSION_RETENTION_DAYS", 0),
            restore_poll_secs: parse("RESTORE_POLL_SECS", 5),
            restore_lease_secs: parse("RESTORE_LEASE_SECS", 300),
            search_scan_clamp_days,
            symbols_cache_mb: parse("SYMBOLS_CACHE_MB", 256),
            symbols_redis_url: var("SYMBOLS_REDIS_URL"),
            symbols_redis_max_blob_mb: parse("SYMBOLS_REDIS_MAX_BLOB_MB", 8),
            symbols_max_artifact_mb: parse("SYMBOLS_MAX_ARTIFACT_MB", 128),
            symbols_max_uncompressed_mb: parse("SYMBOLS_MAX_UNCOMPRESSED_MB", 512),
            symbols_ingest_timeout_ms: parse("SYMBOLS_INGEST_TIMEOUT_MS", 150),
            notify_secret_key,
            alerts_tick_secs: parse("ALERTS_TICK_SECS", 30),
            alert_event_retention_days: parse("ALERT_EVENT_RETENTION_DAYS", 90),
            alerts_deliver_timeout_ms: parse("ALERTS_DELIVER_TIMEOUT_MS", 10_000),
            alerts_allow_private: var("ALERTS_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            notify_subs_tick_secs: parse("NOTIFY_SUBS_TICK_SECS", NOTIFY_SUBS_TICK_SECS_DEFAULT),
            notify_subs_batch: parse("NOTIFY_SUBS_BATCH", NOTIFY_SUBS_BATCH_DEFAULT),
            notify_subs_max_probes_per_org: parse(
                "NOTIFY_SUBS_MAX_PROBES_PER_ORG",
                NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT,
            ),
            notify_drain_budget_ms: parse("NOTIFY_DRAIN_BUDGET_MS", NOTIFY_DRAIN_BUDGET_MS_DEFAULT),
            notify_max_emails_per_user_per_hour: parse(
                "NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR",
                NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT,
            ),
            notify_queue_retention_days: parse(
                "NOTIFY_QUEUE_RETENTION_DAYS",
                NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT,
            ),
            dev_mode,
            smtp,
            dashboard_url,
            mail_drain_tick_secs: parse::<u64>("MAIL_DRAIN_TICK_SECS", 60).clamp(10, 3600),
            mail_outbox_retention_days: parse("MAIL_OUTBOX_RETENTION_DAYS", 30),
            inspector_enabled: var("INSPECTOR_ENABLED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            inspector_tick_secs: parse::<u64>("INSPECTOR_TICK_SECS", 30).clamp(5, 3600),
            inspector_batch_rows: parse("INSPECTOR_BATCH_ROWS", 5_000),
            inspector_batch_pause_ms: parse("INSPECTOR_BATCH_PAUSE_MS", 200),
            inspector_lease_secs: parse("INSPECTOR_LEASE_SECS", 120),
            inspector_max_attempts: parse("INSPECTOR_MAX_ATTEMPTS", 3),
            inspector_statement_timeout_ms: parse("INSPECTOR_STATEMENT_TIMEOUT_MS", 30_000),
            inspector_window_days: parse("INSPECTOR_WINDOW_DAYS", search_scan_clamp_days),
            inspector_detector_window_days: parse("INSPECTOR_DETECTOR_WINDOW_DAYS", 7),
            inspector_max_phase2_rows_per_unit: parse(
                "INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT",
                200_000,
            ),
            inspector_default_sweep_rows: parse("INSPECTOR_DEFAULT_SWEEP_ROWS", 50_000),
            inspector_catchup_grace_hours: parse("INSPECTOR_CATCHUP_GRACE_HOURS", 6),
            inspector_scan_keep: parse("INSPECTOR_SCAN_KEEP", 20),
            inspector_finding_retention_days: parse("INSPECTOR_FINDING_RETENTION_DAYS", 90),
            inspector_mask_batch: parse("INSPECTOR_MASK_BATCH", 2_000),
            inspector_mask_pause_ms: parse("INSPECTOR_MASK_PAUSE_MS", 200),
            inspector_mask_max_rows: parse("INSPECTOR_MASK_MAX_ROWS", 20_000_000),
            inspector_claim_stale_secs: parse("INSPECTOR_CLAIM_STALE_SECS", 300),
            inspector_preview_ttl_secs: parse("INSPECTOR_PREVIEW_TTL_SECS", 900),
            purge_batch_rows: parse::<i64>("PURGE_BATCH_ROWS", 5_000).clamp(100, 100_000),
            purge_batch_pause_ms: parse("PURGE_BATCH_PAUSE_MS", 200),
            purge_recompute_batch: parse::<i64>("PURGE_RECOMPUTE_BATCH", 500).clamp(10, 5_000),
            purge_preview_ttl_secs: parse("PURGE_PREVIEW_TTL_SECS", 900),
            purge_claim_stale_secs: parse("PURGE_CLAIM_STALE_SECS", 300),
            purge_ingest_active_secs: parse("PURGE_INGEST_ACTIVE_SECS", 300),
            inspector_preview_gc_days: parse("INSPECTOR_PREVIEW_GC_DAYS", 7),
            inspector_audit_retention_days: parse("INSPECTOR_AUDIT_RETENTION_DAYS", 0),
            inspector_audit_pii_days: parse("INSPECTOR_AUDIT_PII_DAYS", 730),
            inspector_export_max_rows: parse("INSPECTOR_EXPORT_MAX_ROWS", 50_000),
            inspector_policy_cache_secs: policy_cache_secs,
            inspector_tail_sweep_secs: clamp_tail_sweep(
                parse("INSPECTOR_TAIL_SWEEP_SECS", 120),
                policy_cache_secs,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_smtp` takes already-read values rather than reading env itself:
    /// env vars are process-global and `cargo test` runs tests in threads, so a
    /// test that sets `SMTP_HOST` races every other test in the binary.
    #[allow(clippy::too_many_arguments)]
    fn call(
        host: Option<&str>,
        port: u16,
        from: Option<&str>,
        tls_raw: Option<&str>,
        timeout_ms: u64,
        sink: bool,
    ) -> Result<SmtpSettings, String> {
        build_smtp(
            host.map(|s| s.to_string()),
            port,
            None,
            None,
            from.map(|s| s.to_string()),
            "Sauron".to_string(),
            tls_raw.map(|s| s.to_string()),
            false,
            false,
            timeout_ms,
            sink,
        )
    }

    /// `call`, but with the cleartext escape hatch on. Separate helper rather
    /// than an eleventh parameter on `call`: every existing case asserts the
    /// behaviour with the hatch OFF, and that is the default worth keeping
    /// visually obvious at each call site.
    fn call_insecure(
        host: Option<&str>,
        port: u16,
        from: Option<&str>,
        tls_raw: Option<&str>,
    ) -> Result<SmtpSettings, String> {
        build_smtp(
            host.map(|s| s.to_string()),
            port,
            None,
            None,
            from.map(|s| s.to_string()),
            "Sauron".to_string(),
            tls_raw.map(|s| s.to_string()),
            false,
            true,
            10_000,
            false,
        )
    }

    #[test]
    fn unset_host_disables_mail_and_names_the_variable() {
        let err = call(None, 587, None, None, 10_000, false).unwrap_err();
        assert!(err.contains("SMTP_HOST"), "got: {err}");
        assert!(err.contains("SMTP_SINK"), "got: {err}");
    }

    #[test]
    fn host_without_from_names_the_missing_variable() {
        let err = call(Some("smtp.example.test"), 587, None, None, 10_000, false).unwrap_err();
        assert!(err.contains("SMTP_FROM"), "got: {err}");
    }

    #[test]
    fn from_is_shape_checked_at_boot_not_at_send() {
        for bad in [
            "nobody",
            "a@b@c",
            "a b@c.test",
            "@c.test",
            "a@",
            "a@c\r\nBcc: x@y",
        ] {
            let err = call(
                Some("smtp.example.test"),
                587,
                Some(bad),
                None,
                10_000,
                false,
            )
            .unwrap_err();
            assert!(err.contains("SMTP_FROM"), "{bad} gave: {err}");
        }
        assert!(call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            None,
            10_000,
            false
        )
        .is_ok());
    }

    #[test]
    fn tls_defaults_follow_the_port_the_way_channel_resolution_does() {
        let s = call(
            Some("smtp.example.test"),
            465,
            Some("a@c.test"),
            None,
            10_000,
            false,
        )
        .unwrap();
        assert_eq!(s.tls, SmtpTls::Implicit);
        let s = call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            None,
            10_000,
            false,
        )
        .unwrap();
        assert_eq!(s.tls, SmtpTls::StartTls);
    }

    #[test]
    fn tls_aliases_parse_and_garbage_lists_the_accepted_values() {
        for (raw, want) in [
            ("implicit", SmtpTls::Implicit),
            ("smtps", SmtpTls::Implicit),
            ("starttls", SmtpTls::StartTls),
            ("required", SmtpTls::StartTls),
            ("STARTTLS", SmtpTls::StartTls),
        ] {
            let s = call(
                Some("smtp.example.test"),
                587,
                Some("a@c.test"),
                Some(raw),
                10_000,
                false,
            )
            .unwrap();
            assert_eq!(s.tls, want, "raw={raw}");
        }
        let err = call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            Some("garbage"),
            10_000,
            false,
        )
        .unwrap_err();
        assert!(err.contains("implicit"), "got: {err}");
        assert!(err.contains("starttls"), "got: {err}");
        assert!(err.contains("none"), "got: {err}");
    }

    #[test]
    fn cleartext_is_refused_at_boot_unless_the_relay_is_loopback() {
        let err = call(
            Some("192.168.1.20"),
            25,
            Some("a@c.test"),
            Some("none"),
            10_000,
            false,
        )
        .unwrap_err();
        assert!(err.contains("SMTP_TLS"), "got: {err}");
        assert!(err.contains("192.168.1.20"), "got: {err}");

        for ok_host in ["localhost", "127.0.0.1", "::1", "[::1]"] {
            let s = call(
                Some(ok_host),
                25,
                Some("a@c.test"),
                Some("none"),
                10_000,
                false,
            )
            .unwrap_or_else(|e| panic!("{ok_host} rejected: {e}"));
            assert_eq!(s.tls, SmtpTls::None);
        }
    }

    /// The refusal above must name the way out, or the operator whose LAN relay
    /// genuinely cannot do TLS has a hard stop and no next step. A dead end in a
    /// boot error is how people end up disabling mail entirely.
    #[test]
    fn the_cleartext_refusal_names_its_escape_hatch() {
        let err = call(
            Some("192.168.1.20"),
            25,
            Some("a@c.test"),
            Some("none"),
            10_000,
            false,
        )
        .unwrap_err();
        assert!(err.contains("SMTP_INSECURE_PLAINTEXT"), "got: {err}");
    }

    /// `SMTP_INSECURE_PLAINTEXT=true` waives the loopback rule, and nothing else.
    ///
    /// It must NOT imply `SMTP_ALLOW_PRIVATE`, and it must not change what any
    /// other TLS mode does: an operator reaching for it has one problem, and a
    /// flag that quietly relaxes a second control is how a deployment ends up
    /// weaker than the person who configured it believes.
    #[test]
    fn the_escape_hatch_waives_the_loopback_rule_and_nothing_else() {
        let s = call_insecure(Some("192.168.1.20"), 25, Some("a@c.test"), Some("none"))
            .expect("the hatch admits a LAN relay");
        assert_eq!(s.tls, SmtpTls::None);
        assert!(s.insecure_plaintext);
        assert!(
            !s.allow_private,
            "the hatch must not silently grant SMTP_ALLOW_PRIVATE"
        );

        // Still refused for the reasons that have nothing to do with TLS.
        let err = call_insecure(Some("192.168.1.20"), 25, None, Some("none")).unwrap_err();
        assert!(err.contains("SMTP_FROM"), "got: {err}");

        // And an unrecognised TLS mode is still an unrecognised TLS mode.
        let err =
            call_insecure(Some("192.168.1.20"), 25, Some("a@c.test"), Some("maybe")).unwrap_err();
        assert!(err.contains("not recognised"), "got: {err}");
    }

    #[test]
    fn timeout_clamps_the_same_way_the_alert_engine_does() {
        let s = call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            None,
            10,
            false,
        )
        .unwrap();
        assert_eq!(s.timeout_ms, 1_000);
        let s = call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            None,
            900_000,
            false,
        )
        .unwrap();
        assert_eq!(s.timeout_ms, 60_000);
    }

    #[test]
    fn sink_without_a_host_is_a_working_configuration() {
        let s = call(None, 587, None, None, 10_000, true).unwrap();
        assert!(s.sink);
        assert_eq!(s.host, "(sink)");
        assert_eq!(s.from_address, "sauron@localhost");
        let s = call(None, 587, Some("noreply@corp.test"), None, 10_000, true).unwrap();
        assert_eq!(s.from_address, "noreply@corp.test");
    }

    #[test]
    fn smtp_settings_debug_redacts_the_password() {
        let s = build_smtp(
            Some("smtp.example.test".into()),
            587,
            Some("mailer".into()),
            Some("hunter2".into()),
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            false,
            10_000,
            false,
        )
        .unwrap();
        let printed = format!("{s:?}");
        assert!(printed.contains("<redacted>"), "got: {printed}");
        assert!(!printed.contains("hunter2"), "got: {printed}");
        // The username is not a secret and stays legible.
        assert!(printed.contains("mailer"), "got: {printed}");
    }

    /// `from_env` must never bail on a missing relay or dashboard URL. Bailing in
    /// `from_env` once took down `sauron-ingest` and `sauron-tier`, which read
    /// neither. The failure is recorded and raised at the point of use.
    #[test]
    fn config_records_rather_than_raises_missing_mail_settings() {
        let cfg = Config {
            smtp: Err("no relay".to_string()),
            dashboard_url: Err("no dashboard url".to_string()),
            ..sample_config()
        };
        assert!(cfg.require_smtp().is_err());
        assert!(cfg.require_dashboard_url().is_err());
    }

    #[test]
    fn require_accessors_hand_back_the_configured_values() {
        let settings = build_smtp(
            Some("smtp.example.test".into()),
            587,
            None,
            None,
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            false,
            10_000,
            false,
        )
        .unwrap();
        let cfg = Config {
            smtp: Ok(settings),
            dashboard_url: Ok("https://sauron.example.test".to_string()),
            ..sample_config()
        };
        assert_eq!(cfg.require_smtp().unwrap().host, "smtp.example.test");
        assert_eq!(
            cfg.require_dashboard_url().unwrap(),
            "https://sauron.example.test"
        );
    }

    /// The point of migration 000046's key policy: an absent or too-short
    /// `NOTIFY_SECRET_KEY` must make the channel-touching services refuse to
    /// start, and the error must be actionable.
    ///
    /// The regression this guards is not "someone deletes the check" — it is
    /// "someone re-adds the fallback". The previous code booted happily with the
    /// key unset, derived it from `JWT_SECRET`, and encrypted real SMTP
    /// passwords and webhook URLs under a key the operator had never chosen; the
    /// loss only surfaced, silently, at the next JWT rotation.
    #[test]
    fn require_notify_secret_key_fails_closed() {
        let unset = Config {
            notify_secret_key: Err(
                "NOTIFY_SECRET_KEY is required — generate one with `openssl rand -hex 32`"
                    .to_string(),
            ),
            ..sample_config()
        };
        let err = unset.require_notify_secret_key().unwrap_err().to_string();
        assert!(err.contains("NOTIFY_SECRET_KEY"), "got: {err}");
        assert!(
            err.contains("openssl rand -hex 32"),
            "the error must tell an operator how to produce one; got: {err}"
        );

        // No fallback: a perfectly good JWT_SECRET does not rescue it.
        let with_jwt = Config {
            jwt_secret: Ok("jwt-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            notify_secret_key: Err("unset".to_string()),
            ..sample_config()
        };
        assert!(with_jwt.require_notify_secret_key().is_err());

        let ok = Config {
            notify_secret_key: Ok("k".repeat(MIN_JWT_SECRET_LEN)),
            ..sample_config()
        };
        assert_eq!(
            ok.require_notify_secret_key().unwrap(),
            "k".repeat(MIN_JWT_SECRET_LEN)
        );
    }

    /// The floor itself, exercised through the same `match` `from_env` uses.
    /// There was previously no minimum at all on this variable, so a
    /// one-character key was accepted and silently became the AES key material.
    #[test]
    fn the_notify_key_has_the_same_length_floor_as_the_jwt_secret() {
        let classify = |v: Option<&str>| -> Result<String, String> {
            match v.map(str::to_string) {
                Some(s) if s.len() >= MIN_JWT_SECRET_LEN => Ok(s),
                Some(_) => Err(format!(
                    "NOTIFY_SECRET_KEY must be at least {MIN_JWT_SECRET_LEN} characters"
                )),
                None => Err("NOTIFY_SECRET_KEY is required".to_string()),
            }
        };
        assert!(classify(None).is_err());
        assert!(classify(Some("short")).is_err());
        assert!(classify(Some(&"k".repeat(MIN_JWT_SECRET_LEN - 1))).is_err());
        assert!(classify(Some(&"k".repeat(MIN_JWT_SECRET_LEN))).is_ok());
    }

    /// A single `debug!(?cfg)` added during an incident must not dump the
    /// Postgres password, the JWT signing key and the SMTP password at once.
    #[test]
    fn config_debug_redacts_every_secret_it_holds() {
        let settings = build_smtp(
            Some("smtp.example.test".into()),
            587,
            Some("mailer".into()),
            Some("smtp-hunter2".into()),
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            false,
            10_000,
            false,
        )
        .unwrap();
        let cfg = Config {
            database_url: "postgres://sauron:pg-hunter2@db/sauron".to_string(),
            redis_url: "redis://:redis-hunter2@cache:6379".to_string(),
            jwt_secret: Ok("jwt-hunter2-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            notify_secret_key: Ok("notify-hunter2-aaaaaaaaaaaaaaaaaaa".to_string()),
            symbols_redis_url: Some("redis://:symbols-hunter2@cache:6379/1".to_string()),
            smtp: Ok(settings),
            ..sample_config()
        };
        let printed = format!("{cfg:?}");
        for secret in [
            "pg-hunter2",
            "redis-hunter2",
            "jwt-hunter2",
            "notify-hunter2",
            "symbols-hunter2",
            "smtp-hunter2",
        ] {
            assert!(!printed.contains(secret), "{secret} leaked into: {printed}");
        }
        assert!(printed.contains("<redacted>"), "got: {printed}");
        // Non-secret fields must still be legible, or the impl is useless.
        assert!(printed.contains("api_port"), "got: {printed}");
    }

    /// A `Config` with every field at a harmless value, so a test can override
    /// exactly the two or three it cares about with struct-update syntax.
    fn sample_config() -> Config {
        Config {
            database_url: "postgres://localhost/sauron".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            ingest_port: 8081,
            api_port: 8080,
            jwt_secret: Err("unset".to_string()),
            jwt_access_ttl_secs: 900,
            jwt_refresh_ttl_secs: 2_592_000,
            auth_revocation_poll_secs: 5,
            dev_mode: false,
            worker_concurrency: 4,
            rollup_fold_secs: 60,
            rollup_lag_secs: 60,
            rollup_kick_lag_secs: 2,
            rollup_name_cap: 2000,
            cors_allowed_origins: vec![],
            ingest_rate_limit_per_min: 6000,
            ingest_max_body_bytes: 1_048_576,
            ingest_uds_path: None,
            ingest_backlog: 4096,
            ingest_trust_forwarded_headers: false,
            api_trust_forwarded_headers: false,
            monitor_tick_ms: 1000,
            monitor_batch: 100,
            monitor_max_concurrency: 50,
            monitor_check_retention_days: 30,
            monitor_ssrf_allow_private: false,
            store_sync_interval_secs: 21_600,
            store_sync_max_concurrency: 8,
            store_backfill_days: 90,
            tier_hot_days: 30,
            tier_granularity: "day".to_string(),
            tier_cold_path: "/var/lib/sauron/cold".to_string(),
            tier_drop_lag_hours: 24,
            tier_tick_secs: 3600,
            tier_partition_ahead: 7,
            session_retention_days: 0,
            restore_poll_secs: 5,
            restore_lease_secs: 300,
            search_scan_clamp_days: 30,
            symbols_cache_mb: 256,
            symbols_redis_url: None,
            symbols_redis_max_blob_mb: 8,
            symbols_max_artifact_mb: 128,
            symbols_max_uncompressed_mb: 512,
            symbols_ingest_timeout_ms: 150,
            notify_secret_key: Err("unset".to_string()),
            alerts_tick_secs: 30,
            alerts_deliver_timeout_ms: 10_000,
            alerts_allow_private: false,
            alert_event_retention_days: 90,
            notify_subs_tick_secs: NOTIFY_SUBS_TICK_SECS_DEFAULT,
            notify_subs_batch: NOTIFY_SUBS_BATCH_DEFAULT,
            notify_subs_max_probes_per_org: NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT,
            notify_drain_budget_ms: NOTIFY_DRAIN_BUDGET_MS_DEFAULT,
            notify_max_emails_per_user_per_hour: NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT,
            notify_queue_retention_days: NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT,
            smtp: Err("unset".to_string()),
            dashboard_url: Err("unset".to_string()),
            mail_drain_tick_secs: 60,
            mail_outbox_retention_days: 30,
            inspector_enabled: false,
            inspector_tick_secs: 30,
            inspector_batch_rows: 5_000,
            inspector_batch_pause_ms: 200,
            inspector_lease_secs: 120,
            inspector_max_attempts: 3,
            inspector_statement_timeout_ms: 30_000,
            inspector_window_days: 30,
            inspector_detector_window_days: 7,
            inspector_max_phase2_rows_per_unit: 200_000,
            inspector_default_sweep_rows: 50_000,
            inspector_catchup_grace_hours: 6,
            inspector_scan_keep: 20,
            inspector_finding_retention_days: 90,
            inspector_mask_batch: 2_000,
            inspector_mask_pause_ms: 200,
            inspector_mask_max_rows: 20_000_000,
            inspector_claim_stale_secs: 300,
            inspector_preview_ttl_secs: 900,
            purge_batch_rows: 5_000,
            purge_batch_pause_ms: 200,
            purge_recompute_batch: 500,
            purge_preview_ttl_secs: 900,
            purge_claim_stale_secs: 300,
            purge_ingest_active_secs: 300,
            inspector_preview_gc_days: 7,
            inspector_audit_retention_days: 0,
            inspector_audit_pii_days: 730,
            inspector_export_max_rows: 50_000,
            inspector_policy_cache_secs: 30,
            inspector_tail_sweep_secs: 120,
        }
    }

    /// Every one of the six personal-notification knobs is clamped at point of
    /// use, following `alerts_tick_secs`. This pins the defaults so a typo in a
    /// `parse(...)` default cannot ship silently.
    #[test]
    fn personal_notification_defaults() {
        // `from_env` reads the process environment, which other tests share, so
        // assert on the documented constants rather than constructing a Config.
        assert_eq!(NOTIFY_SUBS_TICK_SECS_DEFAULT, 120);
        assert_eq!(NOTIFY_SUBS_BATCH_DEFAULT, 200);
        assert_eq!(NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT, 50);
        assert_eq!(NOTIFY_DRAIN_BUDGET_MS_DEFAULT, 10_000);
        assert_eq!(NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT, 20);
        assert_eq!(NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT, 14);
    }
}

#[cfg(test)]
mod inspector_config_tests {
    use super::*;

    /// The inspector is OFF by default. It is a heavy scanner against the
    /// same Postgres the ingest path writes to, so a deployment must opt in.
    #[test]
    fn inspector_defaults_are_conservative() {
        // Nothing in the environment: every key falls back.
        let enabled = var("INSPECTOR_ENABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        assert!(!enabled, "INSPECTOR_ENABLED must default to false");
        assert_eq!(parse("INSPECTOR_TICK_SECS", 30u64), 30);
        assert_eq!(parse("INSPECTOR_MASK_MAX_ROWS", 20_000_000i64), 20_000_000);
        assert_eq!(parse("INSPECTOR_AUDIT_RETENTION_DAYS", 0i64), 0);
    }

    /// The tail sweep must outlast the pipeline's policy cache or it closes
    /// nothing: rows written between "mask applied" and the last replica's
    /// cache refresh stay raw forever, because the retro-mask is a one-shot
    /// job that ends at `done`.
    #[test]
    fn tail_sweep_is_clamped_above_the_cache_ttl() {
        assert_eq!(clamp_tail_sweep(10, 30), 120);
        assert_eq!(clamp_tail_sweep(600, 30), 600);
    }
}
