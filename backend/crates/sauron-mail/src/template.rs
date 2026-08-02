//! One content model, two independent renderers, and the HTML shell they share.
//!
//! Nothing here ever strips tags to produce the plain-text part. Tag-stripping
//! leaves entities behind as `&amp;`, drops the CTA's href leaving a bare label
//! with nowhere to go, and turns the table scaffolding into ragged whitespace.
//! The text part is written, not derived.

use std::collections::BTreeMap;

use crate::text::{html_escape, substitute};

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error(
        "DASHBOARD_URL is not set, so an email containing a link cannot be rendered; \
         set it to the browser-facing origin of the dashboard"
    )]
    NoDashboardUrl,
    #[error("call-to-action url must start with http:// or https:// (got {0:?})")]
    BadCtaUrl(String),
}

/// A button. Constructed through [`Cta::new`] so the scheme check cannot be
/// skipped by building the struct literally.
#[derive(Debug, Clone)]
pub struct Cta {
    label: String,
    url: String,
}

impl Cta {
    /// Belt and braces against a `javascript:` href. Every URL this codebase
    /// builds today comes from the scheme-validated `DASHBOARD_URL`, so this is
    /// the check that survives the first caller that builds one from something
    /// else.
    pub fn new(label: impl Into<String>, url: impl Into<String>) -> Result<Cta, TemplateError> {
        let url = url.into();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(TemplateError::BadCtaUrl(url));
        }
        Ok(Cta {
            label: label.into(),
            url,
        })
    }
}

/// Deployment-level chrome: the product name, where links point, and the footer.
#[derive(Debug, Clone)]
pub struct Branding {
    pub product_name: String,
    /// `None` when `DASHBOARD_URL` is unset. Every link-building path then fails
    /// loudly at render time rather than guessing an origin.
    pub dashboard_url: Option<String>,
    pub footer: String,
}

impl Branding {
    /// Build an absolute dashboard URL for a hash route.
    ///
    /// The `#` is load-bearing: the dashboard is `svelte-spa-router`, so a reset
    /// link is `https://host/#/reset-password?token=...`. Drop the `#` and the
    /// browser asks the static server for a path it does not serve.
    ///
    /// This is where "any email containing a link requires DASHBOARD_URL" is
    /// actually enforced.
    pub fn link(&self, hash_path: &str) -> Result<String, TemplateError> {
        let base = self
            .dashboard_url
            .as_deref()
            .ok_or(TemplateError::NoDashboardUrl)?
            .trim_end_matches('/');
        Ok(format!("{base}/#{hash_path}"))
    }
}

/// What a sender writes. Deliberately structural rather than a blob of markup:
/// a sender that hands over HTML is a sender that can be talked into handing
/// over someone else's HTML.
#[derive(Debug, Clone)]
pub struct MailContent {
    pub subject: String,
    pub heading: String,
    pub paragraphs: Vec<String>,
    pub cta: Option<Cta>,
    pub footnotes: Vec<String>,
}

/// What the transport sends.
#[derive(Debug, Clone)]
pub struct RenderedMail {
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// The one HTML shell every product email renders into.
///
/// ESCAPING RULE, FIRST BECAUSE IT IS THE ONE THAT BITES: `html_escape` replaces
/// exactly `& < > "` and does NOT escape `'`. Every attribute below is therefore
/// double-quoted. Adding a single-quoted attribute introduces attribute breakout
/// the first time a value containing an apostrophe lands in it.
///
/// `substitute` treats any `{{` as a placeholder opener and renders an unknown
/// key as an empty string, so two adjacent `{` anywhere in the stylesheet would
/// silently delete everything up to the next `}}` — no error, no failing test, an
/// email that still sends and merely looks broken. The layout avoids that by
/// construction; `layout_placeholders_are_exactly_the_known_set` is what keeps it
/// true after the next edit.
///
/// Tables, never divs: Outlook 2016+ renders through Word, which ignores flex and
/// most margins. The `width="600"` attribute is for Word, which ignores
/// `max-width`; `max-width:600px` is for everyone else; `width:100%` keeps it
/// fluid on a phone. No `<img>` anywhere: remote images are blocked by default in
/// Outlook and Gmail, so a logo is an empty box in most inboxes — the wordmark is
/// text.
///
/// Dark mode is best-effort and this comment says so. Gmail strips
/// `prefers-color-scheme`, Outlook.com rewrites CSS, and Apple Mail and some
/// Android clients force-invert on their own. The promise is not a pixel-matched
/// dark variant; it is that the *inline* palette is legible whether or not any of
/// that happens, because dark ink on white reads correctly either way.
const LAYOUT_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<title>{{subject}}</title>
<style>
:root { color-scheme: light dark; }
@media (prefers-color-scheme: dark) {
  .s-page { background-color: #0b0d12 !important; }
  .s-card { background-color: #151922 !important; border-color: #262c38 !important; }
  .s-h1 { color: #f3f4f6 !important; }
  .s-body { color: #d1d5db !important; }
  .s-muted { color: #9ca3af !important; }
  .s-foot { color: #6b7280 !important; }
}
</style>
</head>
<body class="s-page" style="margin:0;padding:0;background-color:#f4f5f7;-webkit-text-size-adjust:100%">
<span style="display:none;font-size:1px;color:#f4f5f7;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden">{{preheader}}&#8199;&#65279;</span>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="border-collapse:collapse;mso-table-lspace:0pt;mso-table-rspace:0pt">
<tr>
<td align="center" style="padding:32px 12px">
<table role="presentation" width="600" cellpadding="0" cellspacing="0" border="0" style="width:100%;max-width:600px;border-collapse:collapse">
<tr>
<td class="s-muted" style="padding:0 0 16px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:13px;font-weight:600;letter-spacing:0.08em;text-transform:uppercase;color:#6b7280">{{product}}</td>
</tr>
<tr>
<td class="s-card" style="background-color:#ffffff;border:1px solid #e5e7eb;border-radius:10px;padding:32px">
<h1 class="s-h1" style="margin:0 0 16px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:22px;line-height:1.3;font-weight:600;color:#111827">{{heading}}</h1>
{{paragraphs}}
{{cta}}
{{footnotes}}
</td>
</tr>
<tr>
<td class="s-foot" style="padding:16px 0 0;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:12px;line-height:1.5;color:#9ca3af">{{footer}}</td>
</tr>
</table>
</td>
</tr>
</table>
</body>
</html>
"##;

/// One body paragraph. `substitute` cannot loop, so repeated blocks render one at
/// a time into an accumulator and go in as a single pre-escaped variable.
const P_HTML: &str = r##"<p class="s-body" style="margin:0 0 14px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.6;color:#374151">{{text}}</p>
"##;

/// A footnote. `word-break:break-all` because the raw-URL fallback a CTA needs is
/// long enough to blow the 600px card open on a phone otherwise.
const FOOTNOTE_HTML: &str = r##"<p class="s-body" style="margin:14px 0 0;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:13px;line-height:1.6;color:#6b7280;word-break:break-all">{{text}}</p>
"##;

/// The bulletproof-button pattern: a one-cell table with `bgcolor` and a
/// border-radius wrapping an inline-block anchor, because Outlook ignores
/// padding on an `<a>` and background-color on anything it renders through Word.
/// The accent is the same blue as `Severity::Info`.
const CTA_HTML: &str = r##"<table role="presentation" cellpadding="0" cellspacing="0" border="0" style="margin:8px 0 4px;border-collapse:collapse">
<tr>
<td align="center" bgcolor="#3b82f6" style="border-radius:8px">
<a href="{{url}}" style="display:inline-block;padding:12px 26px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:15px;font-weight:600;line-height:1;color:#ffffff;text-decoration:none;border-radius:8px">{{label}}</a>
</td>
</tr>
</table>
"##;

/// Longest subject we will emit. A long subject is truncated by every client
/// anyway; the cap exists so a caller cannot push kilobytes into a header.
const MAX_SUBJECT_CHARS: usize = 200;

/// Flatten a header value to one line and cap it.
///
/// A user-supplied fragment in a subject — an app name, a display name — is
/// exactly how SMTP header injection happens: a bare CRLF ends the Subject header
/// and starts a `Bcc:` one.
fn sanitize_header(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .take(MAX_SUBJECT_CHARS)
        .collect()
}

fn render_html(b: &Branding, c: &MailContent) -> String {
    let mut paragraphs = String::new();
    for p in &c.paragraphs {
        let mut one = BTreeMap::new();
        one.insert("text".to_string(), html_escape(p));
        paragraphs.push_str(&substitute(P_HTML, &one));
    }

    let mut footnotes = String::new();
    for note in &c.footnotes {
        let mut one = BTreeMap::new();
        one.insert("text".to_string(), html_escape(note));
        footnotes.push_str(&substitute(FOOTNOTE_HTML, &one));
    }

    let cta = match &c.cta {
        None => String::new(),
        Some(cta) => {
            let mut v = BTreeMap::new();
            v.insert("url".to_string(), html_escape(&cta.url));
            v.insert("label".to_string(), html_escape(&cta.label));
            substitute(CTA_HTML, &v)
        }
    };

    // Named `escaped` because that is the invariant, not a description. Every
    // value below is either already through `html_escape` or is markup this
    // module built itself. `substitute` copies bytes and escapes nothing, so a
    // raw value here is stored XSS in someone's inbox.
    let mut escaped = BTreeMap::new();
    escaped.insert(
        "subject".to_string(),
        html_escape(&sanitize_header(&c.subject)),
    );
    escaped.insert(
        "preheader".to_string(),
        html_escape(c.paragraphs.first().map(String::as_str).unwrap_or("")),
    );
    escaped.insert("product".to_string(), html_escape(&b.product_name));
    escaped.insert("heading".to_string(), html_escape(&c.heading));
    escaped.insert("paragraphs".to_string(), paragraphs);
    escaped.insert("cta".to_string(), cta);
    escaped.insert("footnotes".to_string(), footnotes);
    escaped.insert("footer".to_string(), html_escape(&b.footer));
    substitute(LAYOUT_HTML, &escaped)
}

/// The plain-text part. No escaping anywhere, because it is not markup — a text
/// part carrying `&amp;` is the tell that someone derived it from the HTML.
fn render_text(b: &Branding, c: &MailContent) -> String {
    let mut out = String::new();
    out.push_str(&c.heading);
    out.push_str("\n\n");
    for p in &c.paragraphs {
        out.push_str(p);
        out.push_str("\n\n");
    }
    if let Some(cta) = &c.cta {
        out.push_str(&cta.label);
        out.push_str(":\n");
        out.push_str(&cta.url);
        out.push_str("\n\n");
    }
    for note in &c.footnotes {
        out.push_str(note);
        out.push('\n');
    }
    out.push_str("\n—\n");
    out.push_str(&b.product_name);
    out.push('\n');
    out
}

/// Render one message into both parts.
///
/// Returns `Result` even though nothing in the two renderers can fail today: the
/// fallible steps (`Cta::new`, `Branding::link`) run in the caller, and keeping
/// the signature fallible means adding a fallible step later is not a breaking
/// change across S1 and S3's call sites.
pub fn render(b: &Branding, c: &MailContent) -> Result<RenderedMail, TemplateError> {
    Ok(RenderedMail {
        subject: sanitize_header(&c.subject),
        text: render_text(b, c),
        html: render_html(b, c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn branding() -> Branding {
        Branding {
            product_name: "Sauron".into(),
            dashboard_url: Some("https://sauron.example.test".into()),
            footer: "Sent by Sauron.".into(),
        }
    }

    fn hostile_content() -> MailContent {
        MailContent {
            subject: "<script>alert(1)</script>&\"".into(),
            heading: "<script>alert(1)</script>&\"".into(),
            paragraphs: vec!["<script>alert(1)</script>&\"".into()],
            cta: Some(
                Cta::new(
                    "<script>alert(1)</script>&\"",
                    "https://sauron.example.test/#/reset-password?token=abc",
                )
                .unwrap(),
            ),
            footnotes: vec!["<script>alert(1)</script>&\"".into()],
        }
    }

    /// Collect every `{{key}}` a template declares. Used to pin the placeholder
    /// key sets, which is the only thing that catches a stray `{{` in the CSS
    /// silently deleting the stylesheet.
    fn placeholder_keys(t: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut rest = t;
        while let Some(open) = rest.find("{{") {
            let after = &rest[open + 2..];
            match after.find("}}") {
                Some(close) => {
                    keys.insert(after[..close].trim().to_string());
                    rest = &after[close + 2..];
                }
                None => break,
            }
        }
        keys
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn html_escapes_every_user_supplied_field() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(!out.html.contains("<script>"), "raw script tag in html");
        // Six escape sites, one per place a hostile value reaches the layout:
        // <title> (subject), the preheader span (first paragraph), the <h1>
        // (heading), the paragraph itself, the CTA label, and the footnote.
        // Counting them means dropping an escape site fails here rather than in
        // someone's inbox.
        assert_eq!(out.html.matches("&lt;script&gt;").count(), 6);
    }

    #[test]
    fn text_part_carries_user_content_verbatim_because_it_is_not_markup() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(out.text.contains("<script>alert(1)</script>&\""));
        assert!(
            !out.text.contains("&lt;"),
            "entities leaked into the text part"
        );
        assert!(
            !out.text.contains("&amp;"),
            "entities leaked into the text part"
        );
    }

    #[test]
    fn text_part_is_plain_readable_prose_with_the_url_on_its_own_line() {
        let content = MailContent {
            subject: "Reset your password".into(),
            heading: "Reset your password".into(),
            paragraphs: vec!["First paragraph.".into(), "Second paragraph.".into()],
            cta: Some(
                Cta::new(
                    "Choose a new password",
                    "https://sauron.example.test/#/reset-password?token=abc",
                )
                .unwrap(),
            ),
            footnotes: vec!["If the button does not work, paste the link above.".into()],
        };
        let out = render(&branding(), &content).unwrap();
        assert!(
            !out.text.contains('<'),
            "markup leaked into the text part: {}",
            out.text
        );
        assert!(out
            .text
            .contains("\nhttps://sauron.example.test/#/reset-password?token=abc\n"));
        let first = out.text.find("First paragraph.").unwrap();
        let second = out.text.find("Second paragraph.").unwrap();
        assert!(first < second, "paragraph order not preserved");
        assert!(out.text.contains("First paragraph.\n\nSecond paragraph."));
        assert!(out.text.trim_end().ends_with("Sauron"));
    }

    #[test]
    fn layout_placeholders_are_exactly_the_known_set() {
        assert_eq!(
            placeholder_keys(LAYOUT_HTML),
            set(&[
                "subject",
                "preheader",
                "product",
                "heading",
                "paragraphs",
                "cta",
                "footnotes",
                "footer",
            ])
        );
        assert_eq!(placeholder_keys(P_HTML), set(&["text"]));
        assert_eq!(placeholder_keys(FOOTNOTE_HTML), set(&["text"]));
        assert_eq!(placeholder_keys(CTA_HTML), set(&["url", "label"]));
    }

    #[test]
    fn layout_invariants_survive_editing() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(out.html.contains("max-width:600px"));
        assert!(out.html.contains("width=\"600\""));
        assert!(out.html.contains("role=\"presentation\""));
        assert!(out.html.contains("color-scheme"));
        // Remote images are blocked by default in Outlook and Gmail, so a logo is
        // an empty box in most inboxes. There is no <img> and there must not be.
        assert!(!out.html.contains("<img"));
        assert_eq!(out.html.matches("<!doctype").count(), 1);
        assert_eq!(out.html.matches("<head>").count(), 1);
        assert_eq!(out.html.matches("<body").count(), 1);
    }

    #[test]
    fn cta_rejects_every_scheme_that_is_not_http() {
        for bad in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "/reset",
            "reset-password",
            "HTTPS:/sauron.example.test",
        ] {
            assert!(Cta::new("Go", bad).is_err(), "{bad} was accepted");
        }
        assert!(Cta::new("Go", "http://localhost:3000/#/x").is_ok());
        assert!(Cta::new("Go", "https://sauron.example.test/#/x").is_ok());
    }

    #[test]
    fn link_requires_a_dashboard_url_and_produces_one_slash_before_the_hash() {
        let none = Branding {
            product_name: "Sauron".into(),
            dashboard_url: None,
            footer: String::new(),
        };
        assert!(matches!(
            none.link("/reset-password?token=abc"),
            Err(TemplateError::NoDashboardUrl)
        ));

        for base in [
            "https://sauron.example.test",
            "https://sauron.example.test/",
            "https://sauron.example.test///",
        ] {
            let b = Branding {
                product_name: "Sauron".into(),
                dashboard_url: Some(base.into()),
                footer: String::new(),
            };
            assert_eq!(
                b.link("/reset-password?token=abc").unwrap(),
                "https://sauron.example.test/#/reset-password?token=abc"
            );
        }
    }

    #[test]
    fn subject_cannot_carry_a_second_header() {
        let content = MailContent {
            subject: "Reset\r\nBcc: attacker@evil.test".into(),
            heading: "h".into(),
            paragraphs: vec![],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        assert_eq!(out.subject, "Reset  Bcc: attacker@evil.test");
        assert!(!out.subject.contains('\r'));
        assert!(!out.subject.contains('\n'));
    }

    #[test]
    fn subject_truncates_to_two_hundred_characters() {
        let content = MailContent {
            subject: "x".repeat(500),
            heading: "h".into(),
            paragraphs: vec![],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        assert_eq!(out.subject.chars().count(), 200);
    }

    #[test]
    fn preheader_is_the_first_paragraph_so_the_inbox_preview_is_not_garbage() {
        let content = MailContent {
            subject: "s".into(),
            heading: "h".into(),
            paragraphs: vec!["Someone asked to reset your password.".into()],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        let preheader_at = out
            .html
            .find("Someone asked to reset your password.")
            .expect("preheader missing");
        let body_at = out.html.find("<h1").expect("card heading missing");
        assert!(preheader_at < body_at, "preheader must precede the card");
    }
}
