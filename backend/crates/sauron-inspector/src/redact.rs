//! Everything written into `inspector_findings` passes through here first.
//!
//! A tool that reports PII is a tool that stores PII. The findings table has
//! no raw-value column and no hash column — a SHA-256 of an email is a stable
//! pseudonymous identifier of a person and is trivially brute-forced for
//! low-entropy values — so the ONLY things that reach it are a shape-only
//! preview and a path. Both are produced here, and both are property-tested
//! against a corpus for non-containment of the raw value.

use serde_json::Value;

use crate::detect::{detect_first, ALL_DETECTORS};

pub const PREVIEW_MAX: usize = 64;
pub const PATH_SEGMENT_MAX: usize = 64;
pub const PATH_MAX: usize = 512;
/// What a segment that carries data rather than a field name becomes.
pub const REDACTED_SEGMENT: &str = "<key>";

/// Truncate to `n` CODEPOINTS. `&s[..n]` panics mid-codepoint on the Arabic
/// and CJK keys real payloads contain.
pub fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A stable type name for the UI's "is this really an email or an enum?".
pub fn value_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A shape-only rendering: at most the first and last codepoint of a string,
/// and no magnitude at all for anything else.
pub fn preview(v: &Value) -> String {
    let Value::String(s) = v else {
        return format!("<{}>", value_type(v));
    };
    let chars: Vec<char> = s.chars().collect();
    // Below four codepoints, first-and-last IS the value.
    if chars.len() < 4 {
        return "<short string>".to_string();
    }
    truncate_chars(
        &format!("{}…{}", chars[0], chars[chars.len() - 1]),
        PREVIEW_MAX,
    )
}

/// Redact a walked path so it is safe to store, render, and export.
///
/// Object keys are arbitrary dev-controlled UTF-8, so a payload shaped
/// `extra.customers["jane@acme.com"].email` would otherwise write raw PII
/// straight into a column every `pii:read` holder can read with no reveal
/// call and no audit row.
pub fn redact_path(path: &str) -> String {
    let redacted: Vec<String> = path
        .split('.')
        .map(|seg| {
            if segment_is_data(seg) {
                REDACTED_SEGMENT.to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    truncate_chars(&redacted.join("."), PATH_MAX)
}

/// Whether a path segment carries data rather than naming a field.
///
/// Three independent tests, because a detector alone is not enough:
/// `ssn_123-45-6789` is not a bare SSN and would pass one.
fn segment_is_data(seg: &str) -> bool {
    // An array marker is structural, never data.
    let bare = seg.strip_suffix("[]").unwrap_or(seg);
    if bare.is_empty() {
        return false;
    }
    if bare.chars().count() > PATH_SEGMENT_MAX {
        return true;
    }
    if detect_first(&ALL_DETECTORS, bare).is_some() {
        return true;
    }
    // A field name is overwhelmingly letters plus `_`/`-`/digits. A segment
    // that is mostly digits, or carries `@`/`+`/`:`/`/`/whitespace, is an
    // interpolated identifier or a value.
    if bare
        .chars()
        .any(|c| matches!(c, '@' | '+' | ':' | '/' | '\\') || c.is_whitespace())
    {
        return true;
    }
    let digits = bare.chars().filter(|c| c.is_ascii_digit()).count();
    digits * 2 > bare.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The corpus every property assertion in this module runs against.
    const RAW: [&str; 8] = [
        "jane+receipts@acme.co.uk",
        "+213770123456",
        "123-45-6789",
        "4111111111111111",
        "192.168.1.10",
        "DE89370400440532013000",
        "Jane Q. Doe",
        "شارع محمد الخامس 12",
    ];

    #[test]
    fn preview_never_contains_the_raw_value() {
        for raw in RAW {
            let p = preview(&json!(raw));
            assert!(!p.contains(raw), "preview {p:?} leaked {raw:?}");
            assert!(p.chars().count() <= PREVIEW_MAX);
        }
    }

    #[test]
    fn preview_echoes_at_most_first_and_last_codepoint() {
        assert_eq!(preview(&json!("jane@acme.com")), "j…m");
        assert_eq!(preview(&json!("شارع محمد")), "ش…د");
    }

    #[test]
    fn short_strings_are_not_echoed_at_all() {
        for s in ["", "a", "ab", "abc"] {
            assert_eq!(preview(&json!(s)), "<short string>");
        }
    }

    /// Numbers and booleans must not leak magnitude: `cart_value_cents: 4200`
    /// is a customer's order total.
    #[test]
    fn scalars_render_without_magnitude() {
        assert_eq!(preview(&json!(4200)), "<number>");
        assert_eq!(preview(&json!(-0.5)), "<number>");
        assert_eq!(preview(&json!(true)), "<boolean>");
        assert_eq!(preview(&json!(null)), "<null>");
        assert_eq!(preview(&json!({"a": 1})), "<object>");
        assert_eq!(preview(&json!([1, 2])), "<array>");
    }

    #[test]
    fn value_types_are_stable_strings() {
        assert_eq!(value_type(&json!("x")), "string");
        assert_eq!(value_type(&json!(1)), "number");
        assert_eq!(value_type(&json!(true)), "boolean");
        assert_eq!(value_type(&json!(null)), "null");
        assert_eq!(value_type(&json!({})), "object");
        assert_eq!(value_type(&json!([])), "array");
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        let s = "شارعشارعشارع";
        let t = truncate_chars(s, 4);
        assert_eq!(t.chars().count(), 4);
        assert!(s.starts_with(&t));
    }

    /// The whole point of this module's second half.
    #[test]
    fn key_path_never_contains_the_raw_value() {
        for raw in RAW {
            let path = format!("extra.customers.{raw}.email");
            let r = redact_path(&path);
            assert!(!r.contains(raw), "key_path {r:?} leaked {raw:?}");
        }
    }

    /// The path is split on `.` FIRST, so an interpolated email is already
    /// three segments by the time redaction runs: `jane@acme`, `com`, and
    /// the real key `email`. Only the segment that trips a rule is replaced —
    /// `com` is indistinguishable from a field name and survives. What
    /// matters is the property asserted above: the raw value is no longer
    /// reconstructible from the path. Collapsing neighbours would be
    /// prettier and would also erase real field names next to a redaction.
    #[test]
    fn a_detector_tripping_segment_is_replaced_wholesale() {
        assert_eq!(
            redact_path("customers.jane@acme.com.email"),
            format!("customers.{REDACTED_SEGMENT}.com.email")
        );
    }

    #[test]
    fn an_over_long_segment_is_replaced_not_truncated() {
        let long = "x".repeat(PATH_SEGMENT_MAX + 1);
        assert_eq!(
            redact_path(&format!("a.{long}.b")),
            format!("a.{REDACTED_SEGMENT}.b")
        );
    }

    /// A segment that is mostly digits or punctuation is an id or an
    /// interpolated value, not a field name. `ssn_123-45-6789` shows why a
    /// detector alone is not enough: the segment is not a bare SSN.
    #[test]
    fn a_segment_that_looks_like_data_is_replaced() {
        assert_eq!(
            redact_path("extra.ssn_123-45-6789.value"),
            format!("extra.{REDACTED_SEGMENT}.value")
        );
        assert_eq!(redact_path("extra.order.id"), "extra.order.id");
        assert_eq!(
            redact_path("breadcrumbs[].data.email"),
            "breadcrumbs[].data.email"
        );
    }

    #[test]
    fn the_whole_path_is_capped() {
        let deep = (0..40)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join(".");
        let r = redact_path(&deep);
        assert!(r.chars().count() <= PATH_MAX);
    }
}
