//! Channel kinds and the resolution of a stored channel (`kind` + decrypted
//! `config` + decrypted `secret` bundle) into a typed [`Destination`] ready to
//! deliver to.
//!
//! Storage is intentionally generic: the DB keeps a free-form config bag and a
//! secret bundle, BOTH encrypted at rest. Splitting them by sensitivity was
//! tried and was wrong — the config holds a webhook's target URL and its
//! arbitrary header map, which is where an `Authorization: Bearer …` ends up.
//! The split that survives is by *role*: the config says where to deliver, the
//! secret says what proves we may.
//!
//! This module is the single place that knows what each kind actually requires,
//! so validation (on write), resolution (on deliver) and
//! [`credential_binding`] (the destination a secret is tied to) cannot drift
//! apart. Everything here is pure and takes plain `Value`s: decryption belongs
//! to the caller, above this seam.

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

/// The destination a channel's stored secret is **bound** to.
///
/// A credential is handed to Sauron for one relay/origin and no other. But the
/// destination lives in `config` (`email.host`, `matrix.homeserver`,
/// `webhook.url`) while the credential lives in the encrypted `secret` bundle,
/// and the update API lets those two be changed independently — so without this
/// binding a caller holding only `alert:write` (which never confers *reading*
/// the secret) can repoint a channel at a host they control and have the server
/// hand over the SMTP password or the Matrix access token on the next send.
///
/// Returns the **origin** — scheme + host + port — not the full URL:
/// * the scheme is in, because an `https` → `http` downgrade on the same host
///   discloses the credential to every on-path observer and is therefore a move;
/// * the path is out, because editing a path inside the same origin exposes the
///   secret to nobody new, and forcing operators to re-paste secrets for
///   innocuous edits trains them to paste secrets more often — a net loss.
///
/// Reads `config` **only**, never the secret, so callers can compare the
/// before/after binding without decrypting anything.
///
/// `None` means "this kind's destination is not config-controlled", so there is
/// nothing to bind: Telegram always posts to the hardcoded `api.telegram.org`,
/// and for Slack/Discord the incoming-webhook URL *is* the credential (it is
/// read from `secret` first, see [`resolve`]), so repointing it already
/// requires knowing it.
///
/// Lives next to [`resolve`] deliberately: this file is the single place that
/// knows what each kind requires, and a binding that disagreed with the fields
/// `resolve` actually reads would be a silent hole rather than a loud bug.
pub fn credential_binding(kind: ChannelKind, config: &Value) -> Option<String> {
    match kind {
        // The password is offered to this relay over SMTP AUTH. `implicit_tls`
        // is deliberately NOT part of the binding: both modes end up on
        // `Tls::Required` with real certificate verification in `sauron-mail`,
        // so flipping it is not a downgrade — and the ports differ anyway.
        ChannelKind::Email => Some(format!(
            "smtp://{}:{}",
            s(config, "host").unwrap_or_default().to_ascii_lowercase(),
            // Must track `resolve`'s default or an update that only *implies*
            // the port would read as a move.
            config.get("port").and_then(|p| p.as_u64()).unwrap_or(587)
        )),
        // The access token is sent as `Authorization: Bearer` to this origin.
        ChannelKind::Matrix => Some(origin_of(&s(config, "homeserver")?)),
        // The signing secret authenticates deliveries sent to this origin; an
        // attacker who repoints it gains a signing oracle for the real endpoint.
        ChannelKind::Webhook => Some(origin_of(&s(config, "url")?)),
        ChannelKind::Telegram | ChannelKind::Slack | ChannelKind::Discord => None,
    }
}

/// `scheme://host:port`, lowercased, with the scheme's default port made
/// explicit so `https://h` and `https://h:443` are one destination.
///
/// Falls back to the lowercased raw string rather than failing: `resolve` /
/// `require_https_url` reject unparseable URLs anyway, and a `None` here on a
/// value a later parse would accept is exactly how a binding check gets
/// bypassed.
fn origin_of(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
            match u.port_or_known_default() {
                Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
                None => format!("{}://{}", u.scheme(), host),
            }
        }
        Err(_) => url.trim().to_ascii_lowercase(),
    }
}

/// Does `config` claim a webhook URL that [`resolve`] will silently ignore?
///
/// For Slack and Discord the stored secret wins over `config.webhook_url`. A
/// caller who edits the URL in `config` therefore gets a 200, sees the new URL
/// in every subsequent GET, and keeps delivering to the OLD endpoint forever —
/// a lying UI over a silent no-op. The update handler uses this to refuse the
/// edit instead.
pub fn config_claims_shadowed_webhook_url(kind: ChannelKind, config: &Value) -> bool {
    matches!(kind, ChannelKind::Slack | ChannelKind::Discord) && s(config, "webhook_url").is_some()
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

    // --- credential binding (D7) --------------------------------------------
    //
    // These pin the *shape* of the binding, which is the part of the guard that
    // can be wrong without anything failing: an over-tight binding makes
    // operators re-paste secrets for cosmetic edits, an under-tight one hands
    // the secret to a host the credential was never issued for.

    #[test]
    fn credential_binding_changes_when_the_matrix_homeserver_moves() {
        let before = credential_binding(
            ChannelKind::Matrix,
            &json!({ "homeserver": "https://matrix.example.org", "room_id": "!ops:example.org" }),
        );
        let after = credential_binding(
            ChannelKind::Matrix,
            &json!({ "homeserver": "https://collector.attacker.example", "room_id": "!ops:example.org" }),
        );
        assert!(before.is_some());
        assert_ne!(before, after, "a new homeserver is a new destination");
    }

    #[test]
    fn credential_binding_ignores_a_path_only_webhook_edit() {
        // Same origin ⇒ the signing secret is exposed to nobody new, so this
        // must NOT force the operator to re-enter it.
        assert_eq!(
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/a" })
            ),
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/b" })
            ),
        );
    }

    #[test]
    fn credential_binding_treats_an_https_to_http_downgrade_as_a_move() {
        // The subtle one: a host-only implementation passes every other case
        // here and still hands the secret to any on-path observer.
        assert_ne!(
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/x" })
            ),
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "http://h.example/x" })
            ),
        );
    }

    #[test]
    fn credential_binding_folds_the_default_port_into_the_origin() {
        // `https://h` and `https://h:443` are the same destination; treating
        // them as different is the over-tight failure mode.
        assert_eq!(
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/x" })
            ),
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example:443/x" })
            ),
        );
        assert_ne!(
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/x" })
            ),
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example:8443/x" })
            ),
        );
    }

    #[test]
    fn credential_binding_is_case_insensitive_on_host() {
        assert_eq!(
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://H.Example/x" })
            ),
            credential_binding(
                ChannelKind::Webhook,
                &json!({ "url": "https://h.example/x" })
            ),
        );
        assert_eq!(
            credential_binding(ChannelKind::Email, &json!({ "host": "SMTP.Example" })),
            credential_binding(ChannelKind::Email, &json!({ "host": "smtp.example" })),
        );
    }

    #[test]
    fn credential_binding_email_tracks_resolves_default_port() {
        // An omitted port means 587 in `resolve`; if the binding disagreed,
        // spelling out the default would look like a destination change.
        assert_eq!(
            credential_binding(ChannelKind::Email, &json!({ "host": "smtp.example" })),
            credential_binding(
                ChannelKind::Email,
                &json!({ "host": "smtp.example", "port": 587 })
            ),
        );
        assert_ne!(
            credential_binding(ChannelKind::Email, &json!({ "host": "smtp.example" })),
            credential_binding(
                ChannelKind::Email,
                &json!({ "host": "smtp.example", "port": 465 })
            ),
        );
        assert_ne!(
            credential_binding(ChannelKind::Email, &json!({ "host": "smtp.example" })),
            credential_binding(
                ChannelKind::Email,
                &json!({ "host": "smtp.attacker.example" })
            ),
        );
    }

    #[test]
    fn credential_binding_is_none_where_the_destination_is_not_config_controlled() {
        // Pins the reasoning, not just the value: Telegram's host is hardcoded
        // in `deliver`, and for Slack/Discord the URL *is* the credential, so
        // there is no config-only move to guard against.
        for k in [
            ChannelKind::Telegram,
            ChannelKind::Slack,
            ChannelKind::Discord,
        ] {
            assert_eq!(
                credential_binding(k, &json!({ "webhook_url": "https://x/y", "chat_id": "1" })),
                None,
                "{} must not claim a config-controlled binding",
                k.as_str()
            );
        }
    }

    #[test]
    fn a_slack_webhook_url_in_config_is_recognised_as_shadowed() {
        assert!(config_claims_shadowed_webhook_url(
            ChannelKind::Slack,
            &json!({ "webhook_url": "https://hooks.slack.com/services/a/b/c" })
        ));
        assert!(!config_claims_shadowed_webhook_url(
            ChannelKind::Slack,
            &json!({})
        ));
        // The generic webhook kind reads its URL from `config` by design.
        assert!(!config_claims_shadowed_webhook_url(
            ChannelKind::Webhook,
            &json!({ "url": "https://h/x" })
        ));
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
