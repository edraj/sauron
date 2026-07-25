//! Actually send a rendered alert to one resolved [`Destination`]. Every HTTP
//! path goes through the SSRF-guarded client in [`crate::net`]; SMTP is guarded
//! by pinning the resolved relay address before lettre connects.

use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::TlsParameters;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::json;

use crate::channel::{Destination, UrlFormat};
use crate::crypto::hmac_sha256_hex;
use crate::net::send_json_bytes;
use crate::render::{self, AlertContext};

/// Delivery tuning shared by every channel.
#[derive(Debug, Clone)]
pub struct DeliverOpts {
    /// Bypass the SSRF guard (trusted internal deployments only).
    pub allow_private: bool,
    pub timeout: Duration,
}

impl Default for DeliverOpts {
    fn default() -> Self {
        Self {
            allow_private: false,
            timeout: Duration::from_secs(10),
        }
    }
}

/// Send one alert. `message` is the already-rendered body (template applied).
pub async fn deliver(
    dest: &Destination,
    ctx: &AlertContext,
    message: &str,
    opts: &DeliverOpts,
) -> Result<(), String> {
    match dest {
        Destination::Email(e) => deliver_email(e, ctx, message, opts).await,
        Destination::Url(u) => match u.format {
            UrlFormat::Slack | UrlFormat::Discord => {
                let body = render::url_payload(ctx, u.format, message);
                post_json(&u.url, &body, &[], None, opts).await
            }
            UrlFormat::Plain => {
                let body = render::url_payload(ctx, UrlFormat::Plain, message);
                let raw = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
                // Optional HMAC signature over the exact bytes we send.
                let sig = u
                    .signing_secret
                    .as_ref()
                    .map(|s| format!("sha256={}", hmac_sha256_hex(s.as_bytes(), &raw)));
                let extra: Vec<(String, String)> = u.headers.clone();
                post_bytes(&u.url, raw, &extra, sig.as_deref(), opts).await
            }
        },
        Destination::Matrix(m) => {
            // A fresh txn id per send (idempotency key on Matrix's side).
            let txn = sauron_core::ids::random_hex(16);
            let url = format!(
                "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
                m.homeserver.trim_end_matches('/'),
                urlencode(&m.room_id),
                txn
            );
            let body = render::matrix_content(ctx, message);
            let auth = format!("Bearer {}", m.access_token);
            put_json(&url, &body, &[("authorization".into(), auth)], opts).await
        }
        Destination::Telegram(t) => {
            let url = format!("https://api.telegram.org/bot{}/sendMessage", t.bot_token);
            let body = json!({
                "chat_id": t.chat_id,
                "text": render::telegram_text(ctx, message),
                "disable_web_page_preview": true,
            });
            post_json(&url, &body, &[], None, opts).await
        }
    }
}

async fn post_json(
    url: &str,
    body: &serde_json::Value,
    headers: &[(String, String)],
    signature: Option<&str>,
    opts: &DeliverOpts,
) -> Result<(), String> {
    let raw = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    post_bytes(url, raw, headers, signature, opts).await
}

async fn post_bytes(
    url: &str,
    raw: Vec<u8>,
    headers: &[(String, String)],
    signature: Option<&str>,
    opts: &DeliverOpts,
) -> Result<(), String> {
    let mut hdrs = headers.to_vec();
    if let Some(sig) = signature {
        hdrs.push(("x-sauron-signature".into(), sig.to_string()));
    }
    send_json_bytes(
        reqwest::Method::POST,
        url,
        raw,
        &hdrs,
        opts.timeout,
        opts.allow_private,
    )
    .await
}

async fn put_json(
    url: &str,
    body: &serde_json::Value,
    headers: &[(String, String)],
    opts: &DeliverOpts,
) -> Result<(), String> {
    let raw = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    send_json_bytes(
        reqwest::Method::PUT,
        url,
        raw,
        headers,
        opts.timeout,
        opts.allow_private,
    )
    .await
}

async fn deliver_email(
    e: &crate::channel::EmailDest,
    ctx: &AlertContext,
    message: &str,
    opts: &DeliverOpts,
) -> Result<(), String> {
    let from: Mailbox = e
        .from
        .parse()
        .map_err(|_| format!("invalid from address: {}", e.from))?;
    let subject = render::email_subject(ctx);
    let body = render::email_body(ctx, message);

    let mut builder = Message::builder().from(from).subject(subject);
    for rcpt in &e.to {
        let mbox: Mailbox = rcpt
            .parse()
            .map_err(|_| format!("invalid recipient: {rcpt}"))?;
        builder = builder.to(mbox);
    }
    let email = builder
        .header(ContentType::TEXT_PLAIN)
        .body(body)
        .map_err(|err| format!("email build failed: {err}"))?;

    // SSRF: resolve the relay ONCE, validate it, and connect to that exact
    // address. TLS still validates the certificate against the configured
    // hostname, so pinning the IP costs no authenticity — it only removes the
    // second, unchecked resolution lettre would otherwise perform.
    let connect_host = if opts.allow_private {
        e.host.clone()
    } else {
        let addrs = sauron_monitor_core::ssrf::resolve_checked(&e.host, false).await?;
        addrs
            .first()
            .map(|a| a.ip().to_string())
            .ok_or_else(|| format!("{} did not resolve", e.host))?
    };

    // Implicit TLS (SMTPS) vs STARTTLS. We never fall back to cleartext:
    // `Tls::Wrapper` handshakes immediately and `Tls::Required` aborts if the
    // server will not upgrade.
    let tls_params = TlsParameters::new(e.host.clone())
        .map_err(|err| format!("smtp tls setup failed: {err}"))?;
    let tls = if e.implicit_tls {
        lettre::transport::smtp::client::Tls::Wrapper(tls_params)
    } else {
        lettre::transport::smtp::client::Tls::Required(tls_params)
    };

    let mut tb = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(connect_host)
        .tls(tls)
        .port(e.port)
        .timeout(Some(opts.timeout));
    if let (Some(u), Some(p)) = (e.username.clone(), e.password.clone()) {
        tb = tb.credentials(Credentials::new(u, p));
    }
    let transport = tb.build();
    transport
        .send(email)
        .await
        .map(|_| ())
        .map_err(|err| format!("smtp send failed: {err}"))
}

/// Minimal path-segment encoder for the few characters Matrix room ids contain
/// that are unsafe in a URL path (`!`, `:`, `/`, `#`, `?`, space).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_escapes_matrix_specials() {
        assert_eq!(urlencode("!abc:matrix.org"), "%21abc%3Amatrix.org");
        assert_eq!(urlencode("plain-ID_1.0"), "plain-ID_1.0");
    }
}
