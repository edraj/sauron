//! Turn an [`AlertContext`] into per-channel payloads, plus safe `{{variable}}`
//! template substitution for admin-authored messages.
//!
//! Templating is deliberately just variable substitution — no expression
//! evaluation, no loops — so an admin template can never execute logic. Values
//! are HTML-escaped when they land in an HTML body (Matrix), and JSON-escaped
//! for free by `serde_json` everywhere else.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::channel::UrlFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn parse(s: &str) -> Severity {
        match s {
            "critical" => Severity::Critical,
            "info" => Severity::Info,
            _ => Severity::Warning,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
    /// Slack attachment / Discord embed accent colour.
    fn hex(self) -> &'static str {
        match self {
            Severity::Info => "#3b82f6",
            Severity::Warning => "#f59e0b",
            Severity::Critical => "#ef4444",
        }
    }
    fn discord_int(self) -> u32 {
        match self {
            Severity::Info => 0x3b82f6,
            Severity::Warning => 0xf59e0b,
            Severity::Critical => 0xef4444,
        }
    }
    fn emoji(self) -> &'static str {
        match self {
            Severity::Info => "🔵",
            Severity::Warning => "🟠",
            Severity::Critical => "🔴",
        }
    }
}

/// Everything a channel needs to render one alert.
#[derive(Debug, Clone)]
pub struct AlertContext {
    pub severity: Severity,
    pub trigger_type: String,
    /// Short headline, e.g. "Monitor down: api".
    pub title: String,
    /// One-line human summary used when no template is supplied.
    pub summary: String,
    /// Optional dashboard deep link.
    pub link: Option<String>,
    /// Substitution variables for admin templates (also shown as fields).
    pub vars: BTreeMap<String, String>,
}

impl AlertContext {
    pub fn new(severity: Severity, trigger_type: impl Into<String>) -> Self {
        Self {
            severity,
            trigger_type: trigger_type.into(),
            title: String::new(),
            summary: String::new(),
            link: None,
            vars: BTreeMap::new(),
        }
    }

    pub fn var(mut self, k: &str, v: impl Into<String>) -> Self {
        self.vars.insert(k.to_string(), v.into());
        self
    }

    /// The message body: the admin template with variables substituted, or the
    /// default summary when no template is set.
    pub fn message(&self, template: Option<&str>) -> String {
        match template {
            Some(t) if !t.trim().is_empty() => substitute(t, &self.vars),
            _ => self.summary.clone(),
        }
    }
}

/// Re-exported so `sauron_alerts::render::substitute` stays a working public
/// path: `AlertContext::message` and admin-authored channel templates both go
/// through it, and moving the definition must not move the name.
pub use sauron_mail::text::substitute;

use sauron_mail::text::html_escape;

// --- per-channel payloads --------------------------------------------------

/// Body for a URL destination (Slack / Discord / generic webhook).
pub fn url_payload(ctx: &AlertContext, format: UrlFormat, message: &str) -> Value {
    match format {
        UrlFormat::Slack => json!({
            "text": format!("{} {}", ctx.severity.emoji(), ctx.title),
            "attachments": [{
                "color": ctx.severity.hex(),
                "title": ctx.title,
                "text": message,
                "footer": "Sauron",
                "fields": slack_fields(ctx),
            }],
        }),
        UrlFormat::Discord => json!({
            "embeds": [{
                "title": format!("{} {}", ctx.severity.emoji(), ctx.title),
                "description": message,
                "color": ctx.severity.discord_int(),
                "fields": discord_fields(ctx),
                "footer": { "text": "Sauron" },
            }],
        }),
        UrlFormat::Plain => json!({
            "severity": ctx.severity.as_str(),
            "trigger_type": ctx.trigger_type,
            "title": ctx.title,
            "message": message,
            "link": ctx.link,
            "fields": ctx.vars,
        }),
    }
}

fn slack_fields(ctx: &AlertContext) -> Vec<Value> {
    ctx.vars
        .iter()
        .map(|(k, v)| json!({ "title": k, "value": v, "short": true }))
        .collect()
}

fn discord_fields(ctx: &AlertContext) -> Vec<Value> {
    ctx.vars
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v, "inline": true }))
        .collect()
}

/// Matrix `m.room.message` content (plain + HTML formatted body).
pub fn matrix_content(ctx: &AlertContext, message: &str) -> Value {
    let plain = format!("[{}] {}\n{}", ctx.severity.as_str(), ctx.title, message);
    let mut html = format!(
        "<strong>{} {}</strong><br/>{}",
        ctx.severity.emoji(),
        html_escape(&ctx.title),
        html_escape(message)
    );
    if let Some(link) = &ctx.link {
        html.push_str(&format!(
            "<br/><a href=\"{}\">Open in Sauron</a>",
            html_escape(link)
        ));
    }
    json!({
        "msgtype": "m.text",
        "body": plain,
        "format": "org.matrix.custom.html",
        "formatted_body": html,
    })
}

/// Telegram message text (Markdown-lite; we send as plain text to avoid parse
/// errors from user content).
pub fn telegram_text(ctx: &AlertContext, message: &str) -> String {
    let mut t = format!("{} {}\n{}", ctx.severity.emoji(), ctx.title, message);
    if let Some(link) = &ctx.link {
        t.push_str(&format!("\n{link}"));
    }
    t
}

/// Email subject + plaintext body.
pub fn email_subject(ctx: &AlertContext) -> String {
    format!("[Sauron/{}] {}", ctx.severity.as_str(), ctx.title)
}

pub fn email_body(ctx: &AlertContext, message: &str) -> String {
    let mut b = String::new();
    b.push_str(message);
    b.push_str("\n\n");
    for (k, v) in &ctx.vars {
        b.push_str(&format!("{k}: {v}\n"));
    }
    if let Some(link) = &ctx.link {
        b.push_str(&format!("\nOpen in Sauron: {link}\n"));
    }
    b.push_str("\n— Sauron alerting\n");
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AlertContext {
        AlertContext::new(Severity::Critical, "monitor_down")
            .var("monitor", "api")
            .var("status", "down")
    }

    #[test]
    fn message_uses_template_then_summary() {
        let mut c = ctx();
        c.summary = "default summary".into();
        assert_eq!(c.message(Some("{{monitor}} is {{status}}")), "api is down");
        assert_eq!(c.message(None), "default summary");
        assert_eq!(c.message(Some("   ")), "default summary");
    }

    #[test]
    fn matrix_html_escapes_user_content() {
        let mut c = ctx();
        c.title = "<script>alert(1)</script>".into();
        let content = matrix_content(&c, "body");
        let html = content["formatted_body"].as_str().unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn slack_and_discord_shapes() {
        let c = ctx();
        let s = url_payload(&c, UrlFormat::Slack, "msg");
        assert!(s["attachments"][0]["color"]
            .as_str()
            .unwrap()
            .starts_with('#'));
        let d = url_payload(&c, UrlFormat::Discord, "msg");
        assert!(d["embeds"][0]["color"].as_u64().is_some());
    }
}
