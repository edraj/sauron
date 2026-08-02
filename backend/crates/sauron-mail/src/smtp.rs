//! Transmission: build one RFC 5322 message and get it to a relay, inside one
//! total deadline, without ever dialling an address that was not validated.

use std::str::FromStr;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::response::Severity;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::warn;

use sauron_core::config::{SmtpSettings, SmtpTls};
use sauron_monitor_core::ssrf::{is_blocked_ip, resolve_checked};

/// Everything one send needs, with no reference to where the message came from.
#[derive(Clone)]
pub struct SmtpParams {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: SmtpTls,
    pub allow_private: bool,
    /// Waives the loopback requirement on `SMTP_TLS=none`, for a LAN relay that
    /// genuinely cannot do TLS. Carried all the way here rather than settled in
    /// `build_smtp`, because the check that actually holds runs against the
    /// RESOLVED address in `connect_inner` — a `localhost` repointed by DNS
    /// passes the config-time check and fails that one.
    pub insecure_plaintext: bool,
    /// Applied by lettre per socket operation (connect, EHLO, STARTTLS, AUTH,
    /// MAIL FROM, RCPT TO, DATA, end-of-data, QUIT).
    pub op_timeout: Duration,
    /// Applied by us over the whole send, DNS included. Without it the worst case
    /// is unbounded: the per-operation timeout multiplies by the number of
    /// operations, and `resolve_checked`'s `lookup_host` has no timeout at all.
    pub total_deadline: Duration,
    /// Return before touching a socket and write the message to the log instead.
    pub sink: bool,
    /// Log the plain-text BODY as well as the header line. Requires both
    /// `SMTP_SINK=1` and `SAURON_DEV=1`; see the module doc on the sink below.
    pub sink_log_body: bool,
}

impl std::fmt::Debug for SmtpParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // This is the copy that lives inside the drain loop, and it is the struct
        // a contributor debugging a delivery failure reaches for with
        // `debug!(?params, ...)`. clippy would not object to a `#[derive(Debug)]`
        // here and it would bypass every redaction in `sauron-core`.
        f.debug_struct("SmtpParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("tls", &self.tls)
            .field("allow_private", &self.allow_private)
            .field("insecure_plaintext", &self.insecure_plaintext)
            .field("op_timeout", &self.op_timeout)
            .field("total_deadline", &self.total_deadline)
            .field("sink", &self.sink)
            .field("sink_log_body", &self.sink_log_body)
            .finish()
    }
}

/// Hard ceiling on the total deadline, whatever `SMTP_TIMEOUT_MS` says.
const MAX_TOTAL_DEADLINE: Duration = Duration::from_secs(60);

impl SmtpParams {
    pub fn from_settings(s: &SmtpSettings) -> Self {
        let op_timeout = Duration::from_millis(s.timeout_ms);
        Self {
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            password: s.password.clone(),
            tls: s.tls,
            allow_private: s.allow_private,
            insecure_plaintext: s.insecure_plaintext,
            op_timeout,
            total_deadline: std::cmp::min(op_timeout * 3, MAX_TOTAL_DEADLINE),
            sink: s.sink,
            // Off unless the caller opts in with the second variable.
            sink_log_body: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum MailBody {
    /// `text/plain`, byte-identical to what alert mail has always sent.
    Text(String),
    /// `multipart/alternative`.
    Alternative { text: String, html: String },
}

#[derive(Debug, Clone)]
pub struct OutgoingMail {
    pub from_address: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub body: MailBody,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid from address: {0}")]
    InvalidFrom(String),
    #[error("invalid recipient: {0}")]
    InvalidRecipient(String),
    #[error("smtp tls setup failed: {0}")]
    Tls(String),
    #[error("{0}")]
    Dns(String),
    #[error("{0}")]
    Blocked(String),
    #[error("email build failed: {0}")]
    Build(String),
    #[error("smtp send failed: {0}")]
    Send(String),
    /// A 5xx from the relay. Display is DELIBERATELY identical to `Send`'s —
    /// `test_channel` returns this string verbatim in an HTTP response body and
    /// persists it to `alert_events`, so adding a "(permanent)" infix would have
    /// changed a user-visible string while claiming byte-for-byte parity. The
    /// drain distinguishes the two by variant, which is free.
    #[error("smtp send failed: {0}")]
    Rejected(String),
    #[error("smtp send failed: deadline exceeded after {0}ms")]
    DeadlineExceeded(u64),
}

/// Whether an error message describes something worth retrying.
///
/// The four substrings are exactly the ones `sauron_alerts::engine` used inline
/// before this crate existed. Moving them here rather than reimplementing them is
/// the point: the coupling between an error's wording and whether alert email
/// retries is invisible to the compiler, so it has to be visible to a reader.
pub fn is_transient(msg: &str) -> bool {
    msg.contains("request failed")
        || msg.contains("HTTP 5")
        || msg.contains("HTTP 429")
        || msg.contains("smtp send failed")
}

/// Split `resolve_checked`'s untyped errors into the two that mean different
/// things, defaulting everything else to the transient side.
fn classify_resolve_error(e: String) -> MailError {
    if e.contains("resolves to a blocked address") {
        // The upstream message names the host but not the variable, and an
        // operator reading it in a journal has no way to know which flag governs.
        MailError::Blocked(format!(
            "{e}; set SMTP_ALLOW_PRIVATE=true only if the relay is deliberately on a \
             private network"
        ))
    } else {
        MailError::Dns(e)
    }
}

/// Parse, reject anything unparseable, and return the lowercased address for
/// `recipient_key`.
///
/// Delegating the entire header-injection barrier to a transitive dependency that
/// discards its unparsed remainder is not a barrier.
pub fn normalize_recipient(raw: &str) -> Result<String, MailError> {
    let trimmed = raw.trim();
    let addr = Address::from_str(trimmed)
        .map_err(|_| MailError::InvalidRecipient(trimmed.replace(['\r', '\n'], " ")))?;
    Ok(addr.to_string().to_lowercase())
}

fn build_message(mail: &OutgoingMail) -> Result<Message, MailError> {
    // Parsed as a bare `Address` and handed to `Mailbox::new` with the display
    // name separate, so lettre does the RFC 2047 encoding rather than us
    // `format!`-ing a header — which is how a display name containing a newline
    // becomes a second header.
    let from_addr = Address::from_str(mail.from_address.trim())
        .map_err(|_| MailError::InvalidFrom(mail.from_address.replace(['\r', '\n'], " ")))?;
    let from = Mailbox::new(mail.from_name.clone(), from_addr);

    let mut builder = Message::builder().from(from).subject(mail.subject.as_str());

    if let Some(rt) = &mail.reply_to {
        let addr = Address::from_str(rt.trim())
            .map_err(|_| MailError::InvalidFrom(rt.replace(['\r', '\n'], " ")))?;
        builder = builder.reply_to(Mailbox::new(None, addr));
    }

    for rcpt in &mail.to {
        let addr = Address::from_str(rcpt.trim())
            .map_err(|_| MailError::InvalidRecipient(rcpt.replace(['\r', '\n'], " ")))?;
        builder = builder.to(Mailbox::new(None, addr));
    }

    let built = match &mail.body {
        MailBody::Text(t) => builder.header(ContentType::TEXT_PLAIN).body(t.clone()),
        MailBody::Alternative { text, html } => builder.multipart(
            MultiPart::alternative_plain_html(text.clone(), html.clone()),
        ),
    };
    built.map_err(|e| MailError::Build(e.to_string()))
}

fn classify_smtp_error(e: lettre::transport::smtp::Error) -> MailError {
    if let Some(code) = e.status() {
        if code.severity == Severity::PermanentNegativeCompletion {
            return MailError::Rejected(e.to_string());
        }
    }
    MailError::Send(e.to_string())
}

/// A relay connection built once and reused for a batch.
///
/// The transport is the expensive part: a DNS lookup, a TCP connect, a full TLS
/// handshake and an AUTH round trip. Rebuilding it per message is tolerable for
/// one alert and is 10k connection+AUTH cycles at digest volume, which postfix's
/// `smtpd_client_connection_rate_limit` and every hosted relay will throttle.
pub struct SmtpClient {
    /// `None` means the dev sink: nothing was opened and nothing will be sent.
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    deadline: Duration,
    sink_log_body: bool,
}

impl SmtpClient {
    pub async fn connect(p: &SmtpParams) -> Result<SmtpClient, MailError> {
        let d = p.total_deadline;
        tokio::time::timeout(d, Self::connect_inner(p))
            .await
            .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
    }

    async fn connect_inner(p: &SmtpParams) -> Result<SmtpClient, MailError> {
        // The sink sits at the single narrowest point that would otherwise open a
        // connection, so every caller, every template and the whole outbox state
        // machine are exercised identically to production.
        if p.sink {
            return Ok(SmtpClient {
                transport: None,
                deadline: p.total_deadline,
                sink_log_body: p.sink_log_body,
            });
        }

        // Always resolve and always pin, so the value that was validated is the
        // value dialled. TLS still validates the certificate against the
        // configured hostname, so pinning costs no authenticity. The shipped
        // alerting path skipped resolution entirely when allow_private was set,
        // which quietly dropped the DNS-rebinding pin on exactly the deployments
        // most likely to need it.
        let addrs = resolve_checked(&p.host, p.allow_private || p.tls == SmtpTls::None)
            .await
            .map_err(classify_resolve_error)?;

        // The structural half of the loopback rule, against the address actually
        // dialled rather than the string configured.
        if p.tls == SmtpTls::None && !addrs.iter().all(|a| a.ip().is_loopback()) {
            if !p.insecure_plaintext {
                return Err(MailError::Blocked(format!(
                    "SMTP_TLS=none requires SMTP_HOST to resolve to loopback; {} resolves to {} \
                     — use SMTP_TLS=starttls, put a local relay in front, or set \
                     SMTP_INSECURE_PLAINTEXT=true",
                    p.host,
                    addrs[0].ip()
                )));
            }
            // The escape hatch buys a LAN, not the internet. `SMTP_INSECURE_PLAINTEXT`
            // is named for the operator who has a relay on their own network that
            // cannot do TLS; a host that resolves to a ROUTABLE address is either a
            // typo or a hijacked record, and either way it puts the relay password
            // and every password-reset link on the public internet in clear. There
            // is no deployment where that is the intended reading of this flag.
            if !addrs.iter().all(|a| is_blocked_ip(a.ip())) {
                return Err(MailError::Blocked(format!(
                    "SMTP_INSECURE_PLAINTEXT waives TLS for a relay on your own network, \
                     but {} resolves to the public address {} — refusing to send \
                     credentials and reset links in clear over the internet",
                    p.host,
                    addrs
                        .iter()
                        .map(|a| a.ip())
                        .find(|ip| !is_blocked_ip(*ip))
                        .expect("a non-private address, just matched")
                )));
            }
        }
        let pinned = addrs[0].ip().to_string();

        let tls = match p.tls {
            SmtpTls::Implicit => Tls::Wrapper(
                TlsParameters::new(p.host.clone()).map_err(|e| MailError::Tls(e.to_string()))?,
            ),
            // `Required` aborts if the server will not upgrade, so there is no
            // silent fallback to cleartext on this branch.
            SmtpTls::StartTls => Tls::Required(
                TlsParameters::new(p.host.clone()).map_err(|e| MailError::Tls(e.to_string()))?,
            ),
            SmtpTls::None => Tls::None,
        };

        let mut tb = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(pinned)
            .tls(tls)
            .port(p.port)
            .timeout(Some(p.op_timeout));
        if let (Some(u), Some(pw)) = (p.username.clone(), p.password.clone()) {
            tb = tb.credentials(Credentials::new(u, pw));
        }

        Ok(SmtpClient {
            transport: Some(tb.build()),
            deadline: p.total_deadline,
            sink_log_body: p.sink_log_body,
        })
    }

    pub async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        let d = self.deadline;
        tokio::time::timeout(d, self.send_inner(mail))
            .await
            .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
    }

    async fn send_inner(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        let msg = build_message(mail)?;
        match &self.transport {
            None => {
                // The header line always, at warn!, so a sink can never be
                // silently on in production.
                warn!(
                    to = %mail.to.join(","),
                    subject = %mail.subject,
                    "SMTP_SINK=1: message NOT transmitted"
                );
                if self.sink_log_body {
                    // Logs are routinely shipped to an aggregator with a broader
                    // reader set and a longer retention than the database, so a
                    // sink that logs bodies strictly worsens the exposure the rest
                    // of this design narrows. Two explicit variables gate it, and
                    // RUST_LOG is no gate: the shipped default is
                    // `info,sauron=debug` and EnvFilter matches targets by prefix.
                    //
                    // The PLAIN-TEXT body is the one logged, not the HTML: it is
                    // the readable one and it contains the same URL.
                    let text = match &mail.body {
                        MailBody::Text(t) => t.as_str(),
                        MailBody::Alternative { text, .. } => text.as_str(),
                    };
                    warn!(body = %text, "SMTP_SINK body (SAURON_DEV=1)");
                }
                Ok(())
            }
            Some(t) => t.send(msg).await.map(|_| ()).map_err(classify_smtp_error),
        }
    }
}

/// Connect, send, drop. What `sauron-alerts` calls: one alert, one relay, no
/// batch to amortise a transport over.
pub async fn send(p: &SmtpParams, mail: &OutgoingMail) -> Result<(), MailError> {
    let d = p.total_deadline;
    tokio::time::timeout(d, async {
        let client = SmtpClient::connect_inner(p).await?;
        client.send_inner(mail).await
    })
    .await
    .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipient_normalization_collapses_the_variants_one_mailbox_accepts() {
        let key = "victim@corp.test";
        assert_eq!(normalize_recipient("victim@corp.test").unwrap(), key);
        assert_eq!(normalize_recipient("Victim@Corp.Test").unwrap(), key);
        assert_eq!(normalize_recipient("victim@corp.test ").unwrap(), key);
        assert_eq!(normalize_recipient("  victim@corp.test").unwrap(), key);
    }

    #[test]
    fn recipient_normalization_rejects_rather_than_truncates() {
        // lettre's parser discards the unparsed remainder, so a "parse and keep
        // going" barrier is not a barrier: this string and `victim@corp.test`
        // would otherwise be two rows delivering to one mailbox and each getting
        // its own per-recipient budget.
        for bad in [
            "victim@corp.test <x>",
            "victim@corp.test, other@corp.test",
            "victim@corp.test\r\nBcc: attacker@evil.test",
            "not-an-address",
            "",
        ] {
            assert!(normalize_recipient(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn params_debug_redacts_the_password_and_keeps_the_username() {
        let p = SmtpParams {
            host: "smtp.example.test".into(),
            port: 587,
            username: Some("mailer".into()),
            password: Some("hunter2".into()),
            tls: SmtpTls::StartTls,
            allow_private: false,
            insecure_plaintext: false,
            op_timeout: Duration::from_millis(10_000),
            total_deadline: Duration::from_millis(30_000),
            sink: false,
            sink_log_body: false,
        };
        let printed = format!("{p:?}");
        assert!(printed.contains("<redacted>"), "got: {printed}");
        assert!(!printed.contains("hunter2"), "got: {printed}");
        assert!(printed.contains("mailer"), "got: {printed}");
    }

    #[test]
    fn total_deadline_is_three_operation_timeouts_capped_at_a_minute() {
        let base = sauron_core::config::build_smtp(
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
        let p = SmtpParams::from_settings(&base);
        assert_eq!(p.op_timeout, Duration::from_millis(10_000));
        assert_eq!(p.total_deadline, Duration::from_millis(30_000));

        let slow = sauron_core::config::build_smtp(
            Some("smtp.example.test".into()),
            587,
            None,
            None,
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            false,
            60_000,
            false,
        )
        .unwrap();
        let p = SmtpParams::from_settings(&slow);
        // Without the cap the worst case is 3 minutes of one drain slot held on a
        // tarpitting relay, which outlives the stale-row threshold and lets a
        // requeued duplicate race the original.
        assert_eq!(p.total_deadline, Duration::from_secs(60));
    }

    /// The alerting engine decides whether to retry by substring-matching the
    /// error's Display. Moving the transport into another crate turned that
    /// contract into a `thiserror` attribute in a different file, where
    /// "improving" the wording would silently stop every alert email retrying
    /// with nothing failing to compile. This test is the other side of that
    /// coupling; `sauron-alerts` carries the matching one.
    #[test]
    fn is_transient_matches_the_four_substrings_alerting_relies_on() {
        assert!(is_transient("request failed: connection refused"));
        assert!(is_transient("HTTP 503 from target"));
        assert!(is_transient("HTTP 429 from target"));
        assert!(is_transient(
            &MailError::Send("connection reset".into()).to_string()
        ));
        assert!(is_transient(
            &MailError::DeadlineExceeded(30_000).to_string()
        ));
        // Rejected's Display is byte-identical to Send's, so alerting keeps
        // retrying 5xx exactly as it did before this refactor.
        assert!(is_transient(
            &MailError::Rejected("550 no such user".into()).to_string()
        ));

        assert!(!is_transient(
            &MailError::InvalidFrom("x".into()).to_string()
        ));
        assert!(!is_transient(
            &MailError::InvalidRecipient("x".into()).to_string()
        ));
        assert!(!is_transient(&MailError::Blocked("x".into()).to_string()));
        assert!(!is_transient(&MailError::Build("x".into()).to_string()));
        assert!(!is_transient(&MailError::Dns("x".into()).to_string()));
        assert!(!is_transient(&MailError::Tls("x".into()).to_string()));
    }

    #[test]
    fn rejected_and_send_display_identically_because_a_route_returns_the_string() {
        // `POST /v1/notification-channels/{id}/test` returns this verbatim as the
        // `error` field and persists it to `alert_events`. An earlier draft added
        // a "(permanent)" infix; that would have changed a user-visible string
        // while claiming byte-for-byte parity. The drain distinguishes the two by
        // VARIANT, which is free.
        assert_eq!(
            MailError::Send("boom".into()).to_string(),
            MailError::Rejected("boom".into()).to_string()
        );
        assert!(MailError::Send("boom".into())
            .to_string()
            .starts_with("smtp send failed"));
    }

    #[test]
    fn resolve_errors_classify_toward_transient_when_unrecognised() {
        assert!(matches!(
            classify_resolve_error("DNS resolution failed: timed out".into()),
            MailError::Dns(_)
        ));
        assert!(matches!(
            classify_resolve_error("target x did not resolve".into()),
            MailError::Dns(_)
        ));
        assert!(matches!(
            classify_resolve_error("target x resolves to a blocked address".into()),
            MailError::Blocked(_)
        ));
        // Anything unrecognised is Dns, i.e. transient. That is the safe
        // direction: if the upstream wording drifts, mail retries and eventually
        // fails out, rather than being marked permanent on the first hiccup.
        assert!(matches!(
            classify_resolve_error("something new upstream".into()),
            MailError::Dns(_)
        ));
    }

    #[test]
    fn blocked_message_names_the_variable_the_upstream_error_omits() {
        let e = classify_resolve_error("target 127.0.0.1 resolves to a blocked address".into());
        let text = e.to_string();
        assert!(text.contains("SMTP_ALLOW_PRIVATE"), "got: {text}");
    }

    #[tokio::test]
    async fn the_sink_never_opens_a_socket() {
        // Host deliberately unresolvable. If the sink branch were placed after
        // resolution this would fail with a DNS error instead of succeeding.
        let p = SmtpParams {
            host: "no-such-host.invalid".into(),
            port: 587,
            username: None,
            password: None,
            tls: SmtpTls::StartTls,
            allow_private: false,
            insecure_plaintext: false,
            op_timeout: Duration::from_millis(10_000),
            total_deadline: Duration::from_millis(30_000),
            sink: true,
            sink_log_body: false,
        };
        let client = SmtpClient::connect(&p).await.expect("sink connect");
        let mail = OutgoingMail {
            from_address: "sauron@localhost".into(),
            from_name: Some("Sauron".into()),
            to: vec!["victim@corp.test".into()],
            reply_to: None,
            subject: "Reset your password".into(),
            body: MailBody::Alternative {
                text: "plain".into(),
                html: "<p>html</p>".into(),
            },
        };
        client.send(&mail).await.expect("sink send");
    }

    #[tokio::test]
    async fn cleartext_to_a_non_loopback_relay_is_blocked_at_connect() {
        // The structural half of the loopback rule: `build_smtp` checks the
        // configured string, this checks what it actually resolved to, which is
        // what survives a `localhost` that has been pointed off-box.
        //
        // The host is an IP literal, not a name: `tokio::net::lookup_host`
        // short-circuits a literal without touching the resolver, so this unit
        // test does no DNS and cannot stall or flake on a machine with no
        // network. A name here would put a live lookup inside
        // `cargo test --workspace`.
        let p = SmtpParams {
            host: "93.184.216.34".into(),
            port: 25,
            username: None,
            password: None,
            tls: SmtpTls::None,
            allow_private: false,
            insecure_plaintext: false,
            op_timeout: Duration::from_millis(2_000),
            total_deadline: Duration::from_millis(6_000),
            sink: false,
            sink_log_body: false,
        };
        match SmtpClient::connect(&p).await {
            Err(MailError::Blocked(m)) => assert!(m.contains("loopback"), "got: {m}"),
            // The Ok payload is never formatted: `SmtpClient` holds a lettre
            // `AsyncSmtpTransport`, which has no `Debug`, so a `{other:?}`
            // catch-all over the whole `Result` does not compile.
            Ok(_) => panic!("expected Blocked, got Ok"),
            Err(e) => panic!("expected Blocked, got {e:?}"),
        }
    }

    /// The escape hatch buys a LAN, never the public internet.
    ///
    /// `SMTP_INSECURE_PLAINTEXT` is named for the operator with a relay on their
    /// own network that cannot do TLS. A host resolving to a ROUTABLE address is
    /// a typo or a hijacked record, and honouring the flag there would put the
    /// relay password and every password-reset link on the open internet in
    /// clear — the exact outcome the loopback rule exists to prevent, reached by
    /// a flag the operator set for a different reason entirely.
    ///
    /// IP literals for the same no-DNS-in-unit-tests reason as the test above.
    #[tokio::test]
    async fn the_cleartext_escape_hatch_still_refuses_a_public_relay() {
        let public = SmtpParams {
            host: "93.184.216.34".into(),
            port: 25,
            username: None,
            password: None,
            tls: SmtpTls::None,
            allow_private: false,
            insecure_plaintext: true,
            op_timeout: Duration::from_millis(2_000),
            total_deadline: Duration::from_millis(6_000),
            sink: false,
            sink_log_body: false,
        };
        match SmtpClient::connect(&public).await {
            Err(MailError::Blocked(m)) => {
                assert!(m.contains("public address"), "got: {m}");
                assert!(m.contains("93.184.216.34"), "got: {m}");
            }
            Ok(_) => panic!("expected Blocked, got Ok"),
            Err(e) => panic!("expected Blocked, got {e:?}"),
        }

        // ...and the LAN address it IS for gets past the guard. Nothing listens
        // on port 25 of this address in CI, so the connect fails — but it fails
        // as a transport error, which is proof it was allowed to dial at all.
        let lan = SmtpParams {
            host: "192.168.0.2".into(),
            insecure_plaintext: true,
            ..public.clone()
        };
        match SmtpClient::connect(&lan).await {
            Err(MailError::Blocked(m)) => panic!("the hatch should admit a LAN relay: {m}"),
            _ => { /* dialled: connected, refused or timed out, all fine here */ }
        }
    }

    #[tokio::test]
    async fn starttls_to_a_private_address_is_refused_by_the_ssrf_guard() {
        let p = SmtpParams {
            host: "127.0.0.1".into(),
            port: 587,
            username: None,
            password: None,
            tls: SmtpTls::StartTls,
            allow_private: false,
            insecure_plaintext: false,
            op_timeout: Duration::from_millis(2_000),
            total_deadline: Duration::from_millis(6_000),
            sink: false,
            sink_log_body: false,
        };
        match SmtpClient::connect(&p).await {
            Err(MailError::Blocked(_)) => {}
            Ok(_) => panic!("expected Blocked, got Ok"),
            Err(e) => panic!("expected Blocked, got {e:?}"),
        }
    }
}
