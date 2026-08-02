//! Actually send a rendered alert to one resolved [`Destination`]. Every HTTP
//! path goes through the SSRF-guarded client in [`crate::net`]; SMTP is guarded
//! by pinning the resolved relay address before lettre connects.

use std::time::Duration;

use sauron_mail::{MailBody, OutgoingMail, SmtpParams, SmtpTls};
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
    // Everything this function used to do by hand — SSRF resolution, IP pinning,
    // TLS selection, credential wiring, message building — now happens once in
    // `sauron-mail`, so the reset-mail path and the alert path cannot drift apart
    // on any of it. The one behaviour that CHANGES here is the total deadline:
    // lettre applies its timeout per socket operation, so before this a
    // tarpitting relay could hold one alert delivery indefinitely.
    let params = SmtpParams {
        host: e.host.clone(),
        port: e.port,
        username: e.username.clone(),
        password: e.password.clone(),
        // Only ever Implicit or StartTls here, so this path's "never cleartext"
        // guarantee is preserved exactly — `SmtpTls::None` is unreachable from a
        // notification channel.
        tls: if e.implicit_tls {
            SmtpTls::Implicit
        } else {
            SmtpTls::StartTls
        },
        allow_private: opts.allow_private,
        // Unreachable rather than merely unset: the `tls` above is only ever
        // Implicit or StartTls, so the cleartext branch this waives never runs on
        // this path. A per-org notification channel must not be able to opt its
        // own delivery out of TLS.
        insecure_plaintext: false,
        op_timeout: opts.timeout,
        total_deadline: std::cmp::min(opts.timeout * 3, Duration::from_secs(60)),
        sink: false,
        sink_log_body: false,
    };

    let mail = OutgoingMail {
        from_address: e.from.clone(),
        from_name: None,
        to: e.to.clone(),
        reply_to: None,
        subject: render::email_subject(ctx),
        // Text, not Alternative: alert mail stays byte-identical to what it has
        // always been. Rendering it through the new HTML layout is an obvious
        // follow-up and an obvious way to break six channel kinds at once.
        body: MailBody::Text(render::email_body(ctx, message)),
    };

    sauron_mail::send(&params, &mail)
        .await
        .map_err(|err| err.to_string())
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
