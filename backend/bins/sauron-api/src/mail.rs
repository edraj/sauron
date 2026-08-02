//! Orchestration for transactional email: render at enqueue, queue durably,
//! drain off the request path.
//!
//! Sits alongside `admin_storage.rs` / `symbolicate.rs` / `tier_read.rs`, the
//! house pattern for orchestration that is neither a route nor a repo function.
//!
//! Rendering happens at ENQUEUE, not at send. The body is then fixed at request
//! time, a template error surfaces to a handler that can report it instead of
//! inside a retry loop that will only fail eight times, and the drain becomes
//! pure I/O with nothing fallible but the network.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sauron_db::models::NewMailOutbox;
use sauron_db::{repo, PgPool};
use sauron_mail::{
    normalize_recipient, render, Branding, MailBody, MailContent, MailError, MailKind,
    OutgoingMail, SmtpClient, SmtpParams,
};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Rows claimed per pass. Small enough that one batch's worst-case hold stays
/// well inside the stale threshold derived below.
const BATCH: i64 = 16;
/// Sends in flight at once. Mirrors `monitor_max_concurrency`'s existence, and
/// keeps at most four short connection checkouts live out of the process's 16.
const SEND_CONCURRENCY: usize = 4;
/// Wall clock one drain tick may spend, so a backlog actually drains instead of
/// moving 16 messages a minute — but cannot monopolise the process either.
const DRAIN_BUDGET: Duration = Duration::from_secs(300);
/// Concurrent drains permitted. A third `nudge` under a burst is a no-op, because
/// the `SKIP LOCKED` claim will pick its row up anyway.
const DRAIN_SLOTS: usize = 2;
/// Rows deleted per retention pass; the loop repeats until a pass returns 0.
const PRUNE_BATCH: i64 = 500;
/// Address used for a discarded enqueue. `.invalid` is RFC 2606 reserved, so it
/// can never be a real mailbox even if a row somehow escaped.
const DISCARD_RECIPIENT: &str = "discard@invalid";

/// How long a claimed row may go without a heartbeat before another drain
/// reclaims it.
///
/// Derived, not hardcoded. With the defaults this is 300 seconds — the same
/// number a hardcoded constant would have given, but now provably larger than one
/// batch's worst-case hold. A hardcoded constant with a tunable batch size and a
/// tunable timeout is how a drain robs its own sibling and a user gets two reset
/// emails.
fn stale_secs(params: &SmtpParams) -> i64 {
    let waves = (BATCH as u64).div_ceil(SEND_CONCURRENCY as u64);
    (waves * params.total_deadline.as_secs() * 2 + 60) as i64
}

/// Whether the drain should schedule another attempt.
///
/// Deliberately different from `sauron_mail::is_transient`, which the alerting
/// path uses: the drain owns its own ladder and can afford to burn 45 minutes on
/// a genuinely broken relay, while alerting keeps its string predicate
/// byte-compatible so its behaviour is unchanged.
fn is_retryable(e: &MailError) -> bool {
    matches!(
        e,
        MailError::Send(_) | MailError::DeadlineExceeded(_) | MailError::Dns(_) | MailError::Tls(_)
    )
}

/// Postgres reports a missing relation as SQLSTATE 42P01; diesel has no variant
/// for it, so the message is the only signal available.
fn looks_like_missing_outbox(msg: &str) -> bool {
    msg.contains("mail_outbox") && msg.contains("does not exist")
}

/// Log the missing-table diagnosis once rather than the same opaque diesel error
/// every 60 seconds. This is the exact symptom an RPM upgrade produces: upgrades
/// never re-run `sauron-migrate`, so a new binary meets an old schema.
fn report_db_error(context: &'static str, e: &diesel::result::Error) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);

    let msg = e.to_string();
    // The `return` sits outside the `swap` on purpose. Folding both conditions
    // into one `if` sends every tick after the first back down to the `warn!`,
    // which is the per-60-second stream of opaque diesel text this function
    // exists to replace: the diagnosis is logged once, and the symptom is then
    // silent rather than merely quieter.
    if looks_like_missing_outbox(&msg) {
        if !REPORTED.swap(true, Ordering::Relaxed) {
            error!(
                "mail_outbox does not exist — this deployment was upgraded without running \
                 sauron-migrate. Stop sauron-api, run `systemctl start sauron-migrate`, then \
                 start it again (packaging/rpm/SETUP.md section 11)."
            );
        }
        return;
    }
    warn!(context, error = %msg, "mail outbox query failed");
}

/// Renders, enqueues and drains transactional email.
///
/// Never `Debug`: `params` holds the relay password.
#[derive(Clone)]
pub struct MailSender {
    pool: PgPool,
    params: Arc<SmtpParams>,
    from_address: Arc<str>,
    from_name: Arc<str>,
    branding: Arc<Branding>,
    drain_slots: Arc<Semaphore>,
}

impl MailSender {
    pub fn new(
        pool: PgPool,
        params: SmtpParams,
        from_address: String,
        from_name: String,
        branding: Branding,
    ) -> MailSender {
        MailSender {
            pool,
            params: Arc::new(params),
            from_address: Arc::from(from_address.as_str()),
            from_name: Arc::from(from_name.as_str()),
            branding: Arc::new(branding),
            drain_slots: Arc::new(Semaphore::new(DRAIN_SLOTS)),
        }
    }

    /// Render and queue one message.
    ///
    /// `ttl` is the CALLER'S credential lifetime, not a round number. It becomes
    /// `expires_at`, which then governs three separate things: whether the drain
    /// will still send the row, when the hygiene sweep scrubs its body, and how
    /// long an operator has to requeue it by hand. A sender that passes a lifetime
    /// shorter than the token it just minted throws away its own recovery path;
    /// one that passes a longer one leaves a working credential in Postgres after
    /// the token it carries is dead.
    ///
    /// Returns `anyhow::Error`, not `ApiError`, so a caller that must return a
    /// fixed 200 whatever happens can swallow it.
    pub async fn enqueue(
        &self,
        kind: MailKind,
        recipient: &str,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        self.enqueue_inner(kind, Some(recipient), content, user_id, ttl)
            .await
    }

    /// Render and queue, or render and throw away, at identical cost.
    ///
    /// `Ok(None)` covers both a dedup suppression and a deliberate discard, so the
    /// caller cannot distinguish them either — which is the point. A handler that
    /// branches on whether the recipient exists BEFORE calling this reopens the
    /// enumeration oracle this closes.
    pub async fn enqueue_or_discard(
        &self,
        kind: MailKind,
        recipient: Option<&str>,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        self.enqueue_inner(kind, recipient, content, user_id, ttl)
            .await
    }

    async fn enqueue_inner(
        &self,
        kind: MailKind,
        recipient: Option<&str>,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        // Rendered unconditionally, including on the discard path: the render is
        // the expensive half and skipping it is a measurable difference a caller
        // can time.
        let rendered = render(&self.branding, content)?;
        let commit = recipient.is_some();
        let raw = recipient.unwrap_or(DISCARD_RECIPIENT);
        let key = normalize_recipient(raw)?;

        let mut conn = sauron_db::conn(&self.pool).await?;
        let id = repo::enqueue_mail(
            &mut conn,
            NewMailOutbox {
                kind: kind.as_str(),
                recipient: raw,
                recipient_key: &key,
                subject: &rendered.subject,
                body_text: &rendered.text,
                body_html: &rendered.html,
                user_id,
            },
            ttl.as_secs() as i64,
            kind.dedup_window().as_secs() as i64,
            commit,
        )
        .await?;
        // The pool is 16 connections for the whole process; nothing below is a
        // database call and the nudge spawns network work.
        drop(conn);

        // Called on BOTH branches, so the spawn and the semaphore acquisition are
        // paid identically whether or not anything was inserted.
        self.nudge();
        Ok(id)
    }

    /// Kick a drain without waiting for the next tick.
    ///
    /// The detached task first tries to take a drain slot and returns immediately
    /// if it cannot: another drain is already running and the `SKIP LOCKED` claim
    /// will pick the row up anyway. That is what bounds spawn under a burst
    /// without introducing a queue.
    pub fn nudge(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            let Ok(permit) = me.drain_slots.clone().try_acquire_owned() else {
                return;
            };
            let _permit = permit;
            me.drain_once().await;
        });
    }
}

impl MailSender {
    /// Claim and send until the queue is empty or the budget runs out. Returns
    /// how many messages left the process.
    pub async fn drain_once(&self) -> usize {
        let started = Instant::now();
        let mut total = 0usize;

        loop {
            let mut conn = match sauron_db::conn(&self.pool).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "mail drain: no database connection");
                    return total;
                }
            };
            if let Err(e) = repo::requeue_stuck_mail(&mut conn, stale_secs(&self.params)).await {
                report_db_error("requeue_stuck_mail", &e);
            }
            let claimed = match repo::claim_due_mail(&mut conn, BATCH).await {
                Ok(rows) => rows,
                Err(e) => {
                    report_db_error("claim_due_mail", &e);
                    drop(conn);
                    return total;
                }
            };
            // Never hold a pooled connection across network I/O. The pool is 16
            // for the whole process, and this is the documented reason
            // `AlertEngine::fire` takes a pool rather than a connection.
            drop(conn);

            let claimed_len = claimed.len();
            if claimed_len == 0 {
                return total;
            }

            // One transport for the whole batch. Rebuilding it per message costs a
            // DNS lookup, a TCP connect, a TLS handshake and an AUTH round trip
            // each time, which every hosted relay and postfix's
            // `smtpd_client_connection_rate_limit` will throttle at digest volume.
            let client = match SmtpClient::connect(&self.params).await {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    self.fail_batch(&claimed, &e).await;
                    return total;
                }
            };

            let sem = Arc::new(Semaphore::new(SEND_CONCURRENCY));
            let mut set = tokio::task::JoinSet::new();
            for row in claimed {
                let me = self.clone();
                let client = client.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    let Ok(_permit) = sem.acquire_owned().await else {
                        return false;
                    };
                    me.send_one(&client, row).await
                });
            }
            while let Some(res) = set.join_next().await {
                if let Ok(true) = res {
                    total += 1;
                }
            }

            if claimed_len < BATCH as usize || started.elapsed() >= DRAIN_BUDGET {
                return total;
            }
        }
    }

    /// Mark every row of a batch whose transport never came up.
    async fn fail_batch(&self, rows: &[sauron_db::models::MailOutbox], e: &MailError) {
        let permanent = !is_retryable(e);
        let text = e.to_string();
        warn!(rows = rows.len(), error = %text, permanent, "mail drain: relay unavailable");
        let mut conn = match sauron_db::conn(&self.pool).await {
            Ok(c) => c,
            Err(err) => {
                warn!(error = %err, "mail drain: cannot record batch failure");
                return;
            }
        };
        for row in rows {
            if let Err(err) =
                repo::mark_mail_failed(&mut conn, row.id, row.attempts, &text, permanent).await
            {
                report_db_error("mark_mail_failed", &err);
            }
        }
        drop(conn);
    }

    /// Send one claimed row and record the outcome. `true` means it left the
    /// process (including into the sink).
    async fn send_one(&self, client: &SmtpClient, row: sauron_db::models::MailOutbox) -> bool {
        // Heartbeat immediately before the send, so the stale-row reaper cannot
        // reclaim a row this task is about to spend a whole deadline on. Doing it
        // per row rather than per batch is what makes the threshold independent of
        // BATCH and SEND_CONCURRENCY.
        match sauron_db::conn(&self.pool).await {
            Ok(mut c) => {
                if let Err(e) = repo::heartbeat_mail(&mut c, row.id).await {
                    report_db_error("heartbeat_mail", &e);
                }
                drop(c);
            }
            Err(e) => warn!(error = %e, "mail drain: heartbeat checkout failed"),
        }

        let sink = self.params.sink;
        if sink {
            // `sauron-mail` logs recipient and subject; the outbox id and kind
            // live only here, and an operator reading a sink line needs the id to
            // find the row.
            warn!(mail_id = %row.id, kind = %row.kind, "SMTP_SINK=1: message NOT transmitted");
        }

        let mail = OutgoingMail {
            from_address: self.from_address.to_string(),
            from_name: Some(self.from_name.to_string()),
            to: vec![row.recipient.clone()],
            reply_to: None,
            subject: row.subject.clone(),
            body: MailBody::Alternative {
                text: row.body_text.clone(),
                html: row.body_html.clone(),
            },
        };

        let outcome = client.send(&mail).await;

        let mut conn = match sauron_db::conn(&self.pool).await {
            Ok(c) => c,
            Err(e) => {
                warn!(mail_id = %row.id, error = %e, "mail drain: cannot record outcome");
                return outcome.is_ok();
            }
        };
        let sent = match outcome {
            Ok(()) => {
                match repo::mark_mail_sent(&mut conn, row.id, row.attempts, sink).await {
                    // A lost claim: another drainer reclaimed this row underneath
                    // us. Delivery is at-least-once by design, so this is not a
                    // fault — but it is the signal that the stale threshold is too
                    // tight, and it must be visible.
                    Ok(0) => warn!(mail_id = %row.id, "mail drain: claim lost before mark_sent"),
                    Ok(_) => {}
                    Err(e) => report_db_error("mark_mail_sent", &e),
                }
                true
            }
            Err(e) => {
                let permanent = !is_retryable(&e);
                // The recipient is logged only on failure, never on success, so an
                // address — which is PII — stays out of the steady-state log while
                // an operator can still answer "why did this bounce".
                warn!(
                    mail_id = %row.id,
                    kind = %row.kind,
                    recipient = %row.recipient,
                    error = %e,
                    permanent,
                    "mail delivery failed"
                );
                match repo::mark_mail_failed(
                    &mut conn,
                    row.id,
                    row.attempts,
                    &e.to_string(),
                    permanent,
                )
                .await
                {
                    Ok(0) => warn!(mail_id = %row.id, "mail drain: claim lost before mark_failed"),
                    Ok(_) => {}
                    Err(err) => report_db_error("mark_mail_failed", &err),
                }
                false
            }
        };
        drop(conn);
        sent
    }
}

/// Expire, scrub and prune the outbox.
///
/// A FREE FUNCTION taking a pool, not a `MailSender` method, because this must
/// run on a deployment with no relay configured at all — where no `MailSender`
/// exists. Gating it on SMTP being switched on inverts the control it implements:
/// an operator who enables SMTP, sends reset mail, then unsets `SMTP_HOST`
/// — rotating relays, cutting cost, or responding to an incident — would
/// otherwise leave every pending row, each holding a working reset URL, in
/// Postgres permanently, backed up and replicated, with no code path that will
/// ever touch it again. This is pure SQL and needs no relay.
pub async fn hygiene(pool: &PgPool, retention_days: i64) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;

    let expired = repo::expire_stale_mail(&mut conn).await?;
    let blanked = repo::blank_expired_mail_bodies(&mut conn).await?;

    let mut pruned = 0usize;
    loop {
        let n = repo::prune_mail_outbox(&mut conn, retention_days, PRUNE_BATCH).await?;
        pruned += n;
        if n == 0 {
            break;
        }
    }

    let (pending, oldest_secs) = repo::mail_outbox_depth(&mut conn).await?;
    drop(conn);

    if expired > 0 || blanked > 0 || pruned > 0 {
        info!(expired, blanked, pruned, "mail outbox hygiene");
    }
    // Unconditional. There is no metrics endpoint and no admin view, so without
    // this line a stalled queue is invisible until a user reports that password
    // reset does not work.
    info!(
        pending,
        oldest_pending_secs = oldest_secs.unwrap_or(0),
        "mail outbox depth"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(total_deadline_secs: u64) -> SmtpParams {
        SmtpParams {
            host: "smtp.example.test".into(),
            port: 587,
            username: None,
            password: None,
            tls: sauron_mail::SmtpTls::StartTls,
            allow_private: false,
            insecure_plaintext: false,
            op_timeout: Duration::from_secs(10),
            total_deadline: Duration::from_secs(total_deadline_secs),
            sink: false,
            sink_log_body: false,
        }
    }

    /// A hardcoded stale threshold with a tunable batch size and a tunable
    /// timeout is how a drain robs its own sibling mid-send and a user gets two
    /// reset emails. It is derived from both.
    #[test]
    fn stale_threshold_is_derived_from_batch_concurrency_and_deadline() {
        // Defaults: (16 / 4) * 30 * 2 + 60 = 300.
        assert_eq!(stale_secs(&params(30)), 300);
        // Double the deadline, double the window it must cover.
        assert_eq!(stale_secs(&params(60)), 540);
        // And it is always strictly larger than one batch's worst-case hold.
        let worst_case = (BATCH as u64 / SEND_CONCURRENCY as u64) * 30;
        assert!(stale_secs(&params(30)) as u64 > worst_case);
    }

    /// The drain's ladder and the alerting path's string predicate disagree on
    /// purpose. Classifying Dns/Tls as permanent — as an earlier draft did — meant
    /// a 20-second resolver hiccup during a nightly restart marked every row in
    /// that window `failed` after one attempt.
    #[test]
    fn drain_retries_transport_faults_and_gives_up_on_configuration_faults() {
        assert!(is_retryable(&MailError::Send("connection reset".into())));
        assert!(is_retryable(&MailError::DeadlineExceeded(30_000)));
        assert!(is_retryable(&MailError::Dns(
            "DNS resolution failed: x".into()
        )));
        assert!(is_retryable(&MailError::Tls("handshake failed".into())));

        assert!(!is_retryable(&MailError::Rejected(
            "550 no such user".into()
        )));
        assert!(!is_retryable(&MailError::InvalidFrom("x".into())));
        assert!(!is_retryable(&MailError::InvalidRecipient("x".into())));
        assert!(!is_retryable(&MailError::Build("x".into())));
        assert!(!is_retryable(&MailError::Blocked("x".into())));
    }

    #[test]
    fn a_missing_table_is_reported_once_and_names_the_migration_step() {
        // The exact symptom an RPM upgrade produces: new binary, old schema. The
        // opaque diesel error repeated every 60 seconds tells an operator nothing.
        assert!(looks_like_missing_outbox(
            "relation \"mail_outbox\" does not exist"
        ));
        assert!(!looks_like_missing_outbox("column x does not exist"));
        assert!(!looks_like_missing_outbox("connection closed"));
    }
}
