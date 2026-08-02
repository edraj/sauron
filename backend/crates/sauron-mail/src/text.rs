//! The two string primitives every message body goes through.
//!
//! These moved here verbatim from `sauron_alerts::render`, tests included, so the
//! move is provably behaviour-preserving. They are `pub` here and were not there:
//! `html_escape` was a private `fn`, which is why an earlier plan for password
//! reset proposed widening it in a file this slice removes the code from.

use std::collections::BTreeMap;

/// Replace `{{key}}` occurrences with `vars[key]`. Unknown keys are left blank
/// (not echoed) so a template can't leak the literal placeholder. Whitespace
/// inside the braces is tolerated: `{{ key }}`.
///
/// This copies bytes and escapes NOTHING. Every value handed to it for an HTML
/// template must already be escaped by the caller.
pub fn substitute(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        // Everything before the placeholder is copied verbatim. Slicing on the
        // byte index returned by `find` is UTF-8 safe because `{{` is ASCII, so
        // the index always lands on a char boundary.
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                if let Some(val) = vars.get(after[..close].trim()) {
                    out.push_str(val);
                }
                rest = &after[close + 2..];
            }
            // Unterminated `{{` — emit it literally and stop scanning.
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape the four characters that break out of HTML text and double-quoted
/// attribute values.
///
/// It does NOT escape `'`. That is safe only because every attribute in the
/// house email layout is double-quoted — a property `LAYOUT_HTML`'s doc comment
/// states out loud rather than leaving as tribal knowledge.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_known_and_blanks_unknown() {
        let mut vars = BTreeMap::new();
        vars.insert("name".to_string(), "api".to_string());
        assert_eq!(substitute("hi {{name}}!", &vars), "hi api!");
        assert_eq!(substitute("{{ name }} up", &vars), "api up");
        assert_eq!(substitute("x {{missing}} y", &vars), "x  y");
        assert_eq!(substitute("no braces", &vars), "no braces");
        // Unterminated braces are passed through literally.
        assert_eq!(substitute("{{oops", &vars), "{{oops");
    }

    #[test]
    fn substitute_preserves_multibyte_text() {
        let mut vars = BTreeMap::new();
        vars.insert("svc".to_string(), "café".to_string());
        // Non-ASCII on both sides of the placeholder and in the value.
        assert_eq!(
            substitute("héllo {{svc}} — naïve ✅", &vars),
            "héllo café — naïve ✅"
        );
        assert_eq!(substitute("日本語のみ", &vars), "日本語のみ");
    }

    #[test]
    fn html_escape_covers_exactly_four_characters() {
        assert_eq!(
            html_escape("<script>alert(1)</script>&\""),
            "&lt;script&gt;alert(1)&lt;/script&gt;&amp;&quot;"
        );
        // The single quote is deliberately NOT escaped; the layout compensates by
        // double-quoting every attribute. Pinning it here means a future change to
        // the escape set is a deliberate act with a failing test attached.
        assert_eq!(html_escape("it's"), "it's");
    }
}
