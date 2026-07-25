//! Channel kinds and the resolution of a stored channel (`kind` + non-secret
//! `config` + decrypted `secret` bundle) into a typed [`Destination`] ready to
//! deliver to.
//!
//! Storage is intentionally generic: the DB keeps a free-form `config` JSONB and
//! an encrypted `secret` bundle. This module is the single place that knows what
//! each kind actually requires, so validation (on write) and resolution (on
//! deliver) never drift apart.

use serde_json::Value;

/// The supported delivery integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Email,
    Slack,
    Discord,
    Matrix,
    Telegram,
    Webhook,
}

impl ChannelKind {
    pub fn parse(s: &str) -> Option<ChannelKind> {
        Some(match s {
            "email" => ChannelKind::Email,
            "slack" => ChannelKind::Slack,
            "discord" => ChannelKind::Discord,
            "matrix" => ChannelKind::Matrix,
            "telegram" => ChannelKind::Telegram,
            "webhook" => ChannelKind::Webhook,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Email => "email",
            ChannelKind::Slack => "slack",
            ChannelKind::Discord => "discord",
            ChannelKind::Matrix => "matrix",
            ChannelKind::Telegram => "telegram",
            ChannelKind::Webhook => "webhook",
        }
    }

    pub const ALL: [ChannelKind; 6] = [
        ChannelKind::Email,
        ChannelKind::Slack,
        ChannelKind::Discord,
        ChannelKind::Matrix,
        ChannelKind::Telegram,
        ChannelKind::Webhook,
    ];
}

/// A fully-resolved delivery target (config + secrets merged and validated).
#[derive(Debug, Clone)]
pub enum Destination {
    Email(EmailDest),
    /// Slack + Discord + generic webhook all POST JSON to a single URL.
    Url(UrlDest),
    Matrix(MatrixDest),
    Telegram(TelegramDest),
}

#[derive(Debug, Clone)]
pub struct EmailDest {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub to: Vec<String>,
    /// `true` → implicit TLS (SMTPS, usually :465); `false` → STARTTLS (:587).
    pub implicit_tls: bool,
}

/// Which JSON body shape to render for a URL destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlFormat {
    Slack,
    Discord,
    Plain,
}

#[derive(Debug, Clone)]
pub struct UrlDest {
    pub url: String,
    pub format: UrlFormat,
    /// Extra headers (generic webhook only).
    pub headers: Vec<(String, String)>,
    /// Optional HMAC-SHA256 signing secret (generic webhook only).
    pub signing_secret: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MatrixDest {
    pub homeserver: String,
    pub room_id: String,
    pub access_token: String,
}

#[derive(Debug, Clone)]
pub struct TelegramDest {
    pub bot_token: String,
    pub chat_id: String,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|x| x.to_string())
        .filter(|x| !x.is_empty())
}

/// Validate a channel's `config` + `secret` on write. Returns a human-readable
/// error message on failure. Does NOT perform any network I/O.
pub fn validate(kind: ChannelKind, config: &Value, secret: &Value) -> Result<(), String> {
    // Resolution IS the validation — if it resolves, it's usable.
    resolve(kind, config, secret).map(|_| ())
}

/// Merge a channel's non-secret `config` and decrypted `secret` bundle into a
/// typed [`Destination`]. `secret` may be `Value::Null` when no secret is set.
pub fn resolve(kind: ChannelKind, config: &Value, secret: &Value) -> Result<Destination, String> {
    match kind {
        ChannelKind::Email => {
            let host = s(config, "host").ok_or("email: host is required")?;
            let port = config
                .get("port")
                .and_then(|p| p.as_u64())
                .unwrap_or(587)
                .try_into()
                .map_err(|_| "email: port out of range")?;
            let from = s(config, "from").ok_or("email: from address is required")?;
            let to: Vec<String> = match config.get("to") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
                Some(Value::String(one)) if !one.is_empty() => vec![one.clone()],
                _ => Vec::new(),
            };
            if to.is_empty() {
                return Err("email: at least one recipient (to) is required".into());
            }
            let implicit_tls = config
                .get("implicit_tls")
                .and_then(|b| b.as_bool())
                .unwrap_or(port == 465);
            Ok(Destination::Email(EmailDest {
                host,
                port,
                username: s(config, "username"),
                password: s(secret, "password"),
                from,
                to,
                implicit_tls,
            }))
        }
        ChannelKind::Slack | ChannelKind::Discord => {
            let format = if kind == ChannelKind::Slack {
                UrlFormat::Slack
            } else {
                UrlFormat::Discord
            };
            // The incoming-webhook URL is the credential → stored in `secret`.
            let url = s(secret, "webhook_url")
                .or_else(|| s(config, "webhook_url"))
                .ok_or_else(|| format!("{}: webhook_url is required", kind.as_str()))?;
            require_https_url(&url)?;
            Ok(Destination::Url(UrlDest {
                url,
                format,
                headers: Vec::new(),
                signing_secret: None,
            }))
        }
        ChannelKind::Webhook => {
            let url = s(config, "url").ok_or("webhook: url is required")?;
            require_https_url(&url)?;
            let headers = match config.get("headers") {
                Some(Value::Object(o)) => o
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect(),
                _ => Vec::new(),
            };
            Ok(Destination::Url(UrlDest {
                url,
                format: UrlFormat::Plain,
                headers,
                signing_secret: s(secret, "signing_secret"),
            }))
        }
        ChannelKind::Matrix => {
            let homeserver = s(config, "homeserver").ok_or("matrix: homeserver is required")?;
            require_https_url(&homeserver)?;
            let room_id = s(config, "room_id").ok_or("matrix: room_id is required")?;
            let access_token =
                s(secret, "access_token").ok_or("matrix: access_token is required")?;
            Ok(Destination::Matrix(MatrixDest {
                homeserver,
                room_id,
                access_token,
            }))
        }
        ChannelKind::Telegram => {
            let bot_token = s(secret, "bot_token").ok_or("telegram: bot_token is required")?;
            let chat_id = s(config, "chat_id").ok_or("telegram: chat_id is required")?;
            Ok(Destination::Telegram(TelegramDest { bot_token, chat_id }))
        }
    }
}

/// Reject non-http(s) URLs early (the SSRF guard rejects private hosts at send
/// time; this stops obvious garbage like `file://` / `gopher://` schemes).
fn require_https_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(())
    } else {
        Err(format!("url must be http(s): {url}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_roundtrip() {
        for k in ChannelKind::ALL {
            assert_eq!(ChannelKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ChannelKind::parse("nope"), None);
    }

    #[test]
    fn slack_needs_webhook_url() {
        assert!(resolve(ChannelKind::Slack, &json!({}), &Value::Null).is_err());
        let d = resolve(
            ChannelKind::Slack,
            &json!({}),
            &json!({ "webhook_url": "https://hooks.slack.com/services/x/y/z" }),
        )
        .unwrap();
        match d {
            Destination::Url(u) => assert_eq!(u.format, UrlFormat::Slack),
            _ => panic!("wrong dest"),
        }
    }

    #[test]
    fn email_requires_recipient_and_from() {
        assert!(resolve(
            ChannelKind::Email,
            &json!({ "host": "smtp.x" }),
            &Value::Null
        )
        .is_err());
        let d = resolve(
            ChannelKind::Email,
            &json!({ "host": "smtp.x", "from": "a@x", "to": ["b@y"], "port": 465 }),
            &json!({ "password": "p" }),
        )
        .unwrap();
        match d {
            Destination::Email(e) => {
                assert_eq!(e.to, vec!["b@y"]);
                assert!(e.implicit_tls);
                assert_eq!(e.password.as_deref(), Some("p"));
            }
            _ => panic!("wrong dest"),
        }
    }

    #[test]
    fn webhook_rejects_non_http_scheme() {
        assert!(resolve(
            ChannelKind::Webhook,
            &json!({ "url": "file:///etc/passwd" }),
            &Value::Null
        )
        .is_err());
    }

    #[test]
    fn matrix_and_telegram_resolve() {
        assert!(resolve(
            ChannelKind::Matrix,
            &json!({ "homeserver": "https://matrix.org", "room_id": "!abc:matrix.org" }),
            &json!({ "access_token": "t" })
        )
        .is_ok());
        assert!(resolve(
            ChannelKind::Telegram,
            &json!({ "chat_id": "123" }),
            &json!({ "bot_token": "b" })
        )
        .is_ok());
    }
}
