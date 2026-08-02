//! Orchestration: given a fired trigger and its context, throttle, resolve the
//! rule's channels, render, deliver (with bounded retries), and record every
//! outcome in `alert_events`.

use std::time::Duration;

use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use sauron_db::models::{AlertRule, NewAlertEvent, NotificationChannel};
use sauron_db::{repo, AsyncPgConnection, PgPool};
use sauron_redis::RedisStore;

use crate::channel::{resolve, ChannelKind, Destination};
use crate::crypto::SecretCipher;
use crate::deliver::{deliver, DeliverOpts};
use crate::render::AlertContext;

/// Everything the dispatcher needs, bundled so callers (API test-send, prober,
/// evaluator) share one code path.
#[derive(Clone)]
pub struct AlertEngine {
    pub cipher: SecretCipher,
    pub opts: DeliverOpts,
    /// Delivery retry attempts per channel (1 = no retry).
    pub max_attempts: u32,
}

impl AlertEngine {
    pub fn new(cipher: SecretCipher, allow_private: bool, timeout_ms: u64) -> Self {
        Self {
            cipher,
            opts: DeliverOpts {
                allow_private,
                timeout: Duration::from_millis(timeout_ms.clamp(1_000, 60_000)),
            },
            max_attempts: 3,
        }
    }

    /// Resolve a stored channel row into a typed destination (decrypting its
    /// secret bundle).
    pub fn destination(&self, ch: &NotificationChannel) -> Result<Destination, String> {
        let kind =
            ChannelKind::parse(&ch.kind).ok_or_else(|| format!("unknown kind {}", ch.kind))?;
        let secret: Value = match &ch.secret_enc {
            Some(blob) => {
                let plain = self
                    .cipher
                    .decrypt_str(blob)
                    .map_err(|e| format!("secret decrypt failed: {e}"))?;
                serde_json::from_str(&plain).unwrap_or(Value::Null)
            }
            None => Value::Null,
        };
        resolve(kind, &ch.config, &secret)
    }

    /// Fire one rule: throttle on `dedup_key`, then deliver to every attached
    /// channel. Each channel outcome is persisted to `alert_events`. Returns the
    /// number of successful deliveries. Never propagates delivery errors — the
    /// caller's pipeline (prober/evaluator) must not fail because a webhook is
    /// down.
    ///
    /// Takes the pool rather than a connection on purpose. Delivery is network
    /// I/O — up to `max_attempts` tries with a per-attempt timeout, tens of
    /// seconds in the worst case — and holding a pooled connection across it
    /// starves everything else on that pool. In `sauron-monitor` that pool is
    /// shared with the probe writers, and with the 5s checkout timeout a burst
    /// of transitions against a slow webhook turned into failed probe writes.
    /// So: hold a connection for the short DB steps, and never across delivery.
    ///
    /// `channels` is supplied by the caller rather than loaded here. A rule that
    /// fires for twenty new issues calls this twenty times, and loading its
    /// channel list inside meant twenty identical queries — and, now that a
    /// connection is acquired per call, twenty checkouts — for a value that is
    /// constant across the whole batch.
    pub async fn fire(
        &self,
        pool: &PgPool,
        redis: &RedisStore,
        rule: &AlertRule,
        channels: &[NotificationChannel],
        ctx: &AlertContext,
        dedup_key: &str,
    ) -> usize {
        // Cross-process throttle: first Redis (fast, atomic), then the durable
        // alert_events check as a backstop when Redis is unavailable. The happy
        // path needs no database connection at all.
        let throttle_secs = rule.throttle_seconds.max(0) as u64;
        if throttle_secs > 0 {
            let rkey = format!("sauron:alert:throttle:{dedup_key}");
            match redis.set_nx_ex(&rkey, "1", throttle_secs).await {
                Ok(true) => {} // we won the claim → proceed
                Ok(false) => {
                    self.log_one(pool, rule, None, ctx, dedup_key, "throttled", None, 0)
                        .await;
                    return 0;
                }
                Err(e) => {
                    // Redis down → durable fallback so a flapping trigger can't spam.
                    warn!(error = %e, "alert throttle redis unavailable; using durable check");
                    if let Ok(mut conn) = sauron_db::conn(pool).await {
                        match repo::alert_recently_sent(&mut conn, dedup_key, rule.throttle_seconds)
                            .await
                        {
                            Ok(true) => {
                                self.log_event(
                                    &mut conn,
                                    rule,
                                    None,
                                    ctx,
                                    dedup_key,
                                    "throttled",
                                    None,
                                    0,
                                )
                                .await;
                                return 0;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                warn!(error = %e, "durable throttle check failed; delivering")
                            }
                        }
                    }
                }
            }
        }

        if channels.is_empty() {
            self.log_one(
                pool,
                rule,
                None,
                ctx,
                dedup_key,
                "skipped",
                Some("rule has no channels"),
                0,
            )
            .await;
            return 0;
        }

        let message = ctx.message(rule.message_template.as_deref());

        let mut sent = 0usize;
        let mut outcomes: Vec<(Uuid, &'static str, Option<String>, i32)> = Vec::new();
        for ch in channels.iter().filter(|c| c.enabled) {
            match self.deliver_channel(ch, ctx, &message).await {
                Ok(attempts) => {
                    sent += 1;
                    outcomes.push((ch.id, "sent", None, attempts));
                }
                Err((attempts, err)) => {
                    warn!(rule = %rule.id, channel = %ch.id, error = %err, "alert delivery failed");
                    outcomes.push((ch.id, "failed", Some(err), attempts));
                }
            }
        }

        if !outcomes.is_empty() {
            match sauron_db::conn(pool).await {
                Ok(mut conn) => {
                    for (channel_id, status, error, attempts) in &outcomes {
                        self.log_event(
                            &mut conn,
                            rule,
                            Some(*channel_id),
                            ctx,
                            dedup_key,
                            status,
                            error.as_deref(),
                            *attempts,
                        )
                        .await;
                    }
                }
                // The alert was delivered; only the audit row is lost.
                Err(e) => warn!(rule = %rule.id, error = %e, "could not record alert outcomes"),
            }
        }
        sent
    }

    /// Deliver to a single channel with bounded exponential backoff.
    /// Returns `Ok(attempts)` or `Err((attempts, last_error))`.
    pub async fn deliver_channel(
        &self,
        ch: &NotificationChannel,
        ctx: &AlertContext,
        message: &str,
    ) -> Result<i32, (i32, String)> {
        let dest = match self.destination(ch) {
            Ok(d) => d,
            Err(e) => return Err((0, e)),
        };
        let mut last_err = String::new();
        let mut made = 0i32;
        for attempt in 1..=self.max_attempts {
            made = attempt as i32;
            match deliver(&dest, ctx, message, &self.opts).await {
                Ok(()) => return Ok(made),
                Err(e) => {
                    // Config-level errors (SSRF-blocked, bad address) won't heal
                    // with retries; only transient transport errors are retried.
                    // The four substrings moved into `sauron-mail` alongside the
                    // errors that produce them, because "improving" one of those
                    // error strings would otherwise stop every alert email
                    // retrying with nothing failing to compile.
                    let transient = sauron_mail::is_transient(&e);
                    last_err = e;
                    if !transient || attempt == self.max_attempts {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
                }
            }
        }
        // Report the attempts actually made, not the ceiling: a permanently
        // rejected target stops after one try and saying "3" hides that.
        Err((made, last_err))
    }

    /// [`log_event`](Self::log_event) for the paths that hold no connection yet.
    /// Failing to record the row must never abort the alert, so a checkout
    /// failure is logged and swallowed.
    #[allow(clippy::too_many_arguments)]
    async fn log_one(
        &self,
        pool: &PgPool,
        rule: &AlertRule,
        channel_id: Option<Uuid>,
        ctx: &AlertContext,
        dedup_key: &str,
        status: &str,
        error: Option<&str>,
        attempts: i32,
    ) {
        match sauron_db::conn(pool).await {
            Ok(mut conn) => {
                self.log_event(
                    &mut conn, rule, channel_id, ctx, dedup_key, status, error, attempts,
                )
                .await
            }
            Err(e) => warn!(rule = %rule.id, error = %e, "could not record alert event"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn log_event(
        &self,
        conn: &mut AsyncPgConnection,
        rule: &AlertRule,
        channel_id: Option<Uuid>,
        ctx: &AlertContext,
        dedup_key: &str,
        status: &str,
        error: Option<&str>,
        attempts: i32,
    ) {
        let ev = NewAlertEvent {
            org_id: rule.org_id,
            rule_id: Some(rule.id),
            channel_id,
            trigger_type: &ctx.trigger_type,
            dedup_key,
            status,
            title: &ctx.title,
            body: &ctx.summary,
            error,
            attempts,
        };
        if let Err(e) = repo::insert_alert_event(conn, ev).await {
            warn!(error = %e, "failed to record alert event");
        }
    }
}

#[cfg(test)]
mod tests {
    use sauron_mail::{is_transient, MailError};

    /// The retry decision for alert email is made by substring-matching an error
    /// string produced in a different crate. Nothing about that coupling is
    /// visible to the compiler, so it is pinned from both sides: this is the
    /// `sauron-alerts` half, and `sauron_mail::smtp`'s
    /// `is_transient_matches_the_four_substrings_alerting_relies_on` is the other.
    #[test]
    fn every_mail_error_variant_keeps_the_retry_behaviour_it_had_before_the_move() {
        // Retried, exactly as before the transport moved crates.
        assert!(is_transient(
            &MailError::Send("connection reset".into()).to_string()
        ));
        assert!(is_transient(
            &MailError::DeadlineExceeded(30_000).to_string()
        ));
        // Still retried. Splitting SMTP 4xx from 5xx for the alerting path is a
        // deliberate follow-up with its own decision, NOT a side effect of this
        // refactor — a permanently misconfigured email channel burning three
        // attempts is the behaviour that exists today.
        assert!(is_transient(
            &MailError::Rejected("550 no such user".into()).to_string()
        ));

        // Never retried: configuration faults that will not heal.
        assert!(!is_transient(
            &MailError::InvalidFrom("x@".into()).to_string()
        ));
        assert!(!is_transient(
            &MailError::InvalidRecipient("x@".into()).to_string()
        ));
        assert!(!is_transient(
            &MailError::Blocked("blocked".into()).to_string()
        ));
        assert!(!is_transient(&MailError::Build("bad".into()).to_string()));
    }
}
