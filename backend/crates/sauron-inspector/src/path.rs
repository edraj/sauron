//! The mask-path grammar: dot-separated segments RELATIVE TO THE COLUMN,
//! plus AT MOST ONE wildcard, legal only as a bare leading `[*]`.
//!
//! The one-wildcard rule is not arbitrary. A non-wildcard path lowers to a
//! single `jsonb_set(coalesce(col,'{}'), $path::text[], '"****"', false)`. A
//! wildcard path lowers to a full array rebuild — `jsonb_agg` over
//! `jsonb_array_elements(col) WITH ORDINALITY` — which re-serializes the whole
//! array per row and is measurably more expensive, which is why the batch size
//! halves when any target carries one. Two wildcards would mean a nested
//! rebuild with no bound on the work per row.
//!
//! The bare-`[*]` rule is not arbitrary either: `jsonb_array_elements(col)`
//! means THE COLUMN IS THE ARRAY. A wildcard hanging off a named segment
//! (`breadcrumbs[*].data.email`) has no lowering, and if it were accepted the
//! statement would match nothing and the audit row would report a successful
//! mask that changed no bytes — the worst possible outcome for a privacy
//! control. `error_events.stacktrace` and `error_events.breadcrumbs` are both
//! arrays at their root (`process.rs` writes `json!([])` for each), so the
//! forms this grammar accepts are the forms the product actually needs.

use crate::redact::REDACTED_SEGMENT;
use crate::walk::MAX_DEPTH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    EmptySegment,
    /// An index is not stable across rows.
    NumericIndex,
    WildcardNotFirst,
    /// A wildcard on a NAMED segment. The only lowering is
    /// `jsonb_array_elements(col)`, so the array has to be the column itself.
    WildcardNotAtRoot,
    TooDeep,
    /// The finding's path segment was replaced by the redactor, so it names no
    /// real key.
    RedactedSegment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskPath {
    /// The first segment. EMPTY for a wildcard path, because a wildcard
    /// addresses the column itself.
    pub head: String,
    /// Whether the COLUMN is an array whose every element is addressed.
    pub wildcard: bool,
    /// Everything after `head`.
    pub rest: Vec<String>,
}

impl MaskPath {
    /// The full path as a bound `text[]` for the non-wildcard lowering.
    ///
    /// A wildcard path has an empty head, and pushing `""` would build the
    /// `text[]` `{"", "abs_path"}` — a path that exists in no document and
    /// would silently mask nothing. Wildcard callers use `sub_array()`.
    pub fn text_array(&self) -> Vec<String> {
        if self.head.is_empty() {
            return self.rest.clone();
        }
        let mut v = Vec::with_capacity(self.rest.len() + 1);
        v.push(self.head.clone());
        v.extend(self.rest.iter().cloned());
        v
    }

    /// The path WITHIN one array element, for the wildcard lowering.
    pub fn sub_array(&self) -> Vec<String> {
        self.rest.clone()
    }

    /// Round-trips `parse_mask_path`. This is what is stored in
    /// `inspector_masked_keys.json_path` and in a mask action's `targets`.
    pub fn to_wire(&self) -> String {
        let head = if self.wildcard {
            format!("{}[*]", self.head)
        } else {
            self.head.clone()
        };
        if self.rest.is_empty() {
            head
        } else {
            format!("{head}.{}", self.rest.join("."))
        }
    }
}

pub fn parse_mask_path(raw: &str) -> Result<MaskPath, PathError> {
    if raw.trim().is_empty() {
        return Err(PathError::Empty);
    }
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() > MAX_DEPTH {
        return Err(PathError::TooDeep);
    }
    let mut head = String::new();
    let mut wildcard = false;
    let mut rest = Vec::with_capacity(parts.len().saturating_sub(1));
    for (i, part) in parts.iter().enumerate() {
        let seg = *part;
        if seg.trim().is_empty() {
            return Err(PathError::EmptySegment);
        }
        let (bare, has_star) = match seg.strip_suffix("[*]") {
            Some(b) => (b, true),
            None => (seg, false),
        };
        if has_star && i != 0 {
            return Err(PathError::WildcardNotFirst);
        }
        // `[*]` is legal ONLY bare: the lowering is
        // `jsonb_array_elements(col)`, so the array is the column itself.
        if has_star && !bare.is_empty() {
            return Err(PathError::WildcardNotAtRoot);
        }
        if bare.is_empty() && !has_star {
            return Err(PathError::EmptySegment);
        }
        if bare.contains("[*]") || bare.contains('[') || bare.contains(']') {
            return Err(PathError::WildcardNotFirst);
        }
        // Guarded on non-empty: `"".bytes().all(..)` is vacuously true, and a
        // bare `[*]` head would otherwise be rejected as a numeric index.
        if !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PathError::NumericIndex);
        }
        if i == 0 {
            head = bare.to_string();
            wildcard = has_star;
        } else {
            rest.push(bare.to_string());
        }
    }
    Ok(MaskPath {
        head,
        wildcard,
        rest,
    })
}

/// Convert a finding's `key_path` (walker form, `[]`) into a mask path
/// (`[*]`), or explain why the finding is not maskable.
pub fn finding_path_to_mask_path(key_path: &str) -> Result<String, PathError> {
    if key_path.split('.').any(|s| s == REDACTED_SEGMENT) {
        return Err(PathError::RedactedSegment);
    }
    let candidate = key_path.replace("[]", "[*]");
    Ok(parse_mask_path(&candidate)?.to_wire())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_parses() {
        let p = parse_mask_path("customer.email").unwrap();
        assert_eq!(p.head, "customer");
        assert!(!p.wildcard);
        assert_eq!(p.rest, ["email"]);
        assert_eq!(p.text_array(), ["customer", "email"]);
        assert_eq!(p.to_wire(), "customer.email");
    }

    #[test]
    fn a_single_segment_parses() {
        let p = parse_mask_path("email").unwrap();
        assert_eq!(p.text_array(), ["email"]);
        assert!(p.sub_array().is_empty());
    }

    /// A bare `[*]` is the ONLY wildcard form: the path is relative to the
    /// column, and the column value itself is the array.
    #[test]
    fn a_bare_leading_wildcard_parses() {
        let p = parse_mask_path("[*].data.email").unwrap();
        assert_eq!(p.head, "");
        assert!(p.wildcard);
        assert_eq!(p.sub_array(), ["data", "email"]);
        assert_eq!(p.to_wire(), "[*].data.email");
    }

    /// The wildcard lowering is `jsonb_agg` over `jsonb_array_elements(col)`,
    /// so an array one level INSIDE the column has no lowering. Rejecting it
    /// is the difference between "not maskable" and a mask that reports
    /// success having changed nothing.
    #[test]
    fn a_wildcard_on_a_named_segment_is_rejected() {
        assert_eq!(
            parse_mask_path("breadcrumbs[*].data.email"),
            Err(PathError::WildcardNotAtRoot)
        );
    }

    /// An index is not stable across rows, so a finding must never carry one
    /// and a mask must never accept one.
    #[test]
    fn a_numeric_index_is_rejected() {
        assert_eq!(
            parse_mask_path("breadcrumbs.3.data.email"),
            Err(PathError::NumericIndex)
        );
        assert_eq!(parse_mask_path("0"), Err(PathError::NumericIndex));
    }

    #[test]
    fn a_non_leading_wildcard_is_rejected() {
        assert_eq!(
            parse_mask_path("a.b[*].c"),
            Err(PathError::WildcardNotFirst)
        );
    }

    #[test]
    fn a_second_wildcard_is_rejected() {
        assert_eq!(
            parse_mask_path("[*].b[*]"),
            Err(PathError::WildcardNotFirst)
        );
    }

    #[test]
    fn empty_and_blank_segments_are_rejected() {
        assert_eq!(parse_mask_path(""), Err(PathError::Empty));
        assert_eq!(parse_mask_path("a..b"), Err(PathError::EmptySegment));
        assert_eq!(parse_mask_path("a. .b"), Err(PathError::EmptySegment));
    }

    #[test]
    fn a_path_deeper_than_the_walker_is_rejected() {
        assert_eq!(parse_mask_path("a.b.c.d.e.f.g"), Err(PathError::TooDeep));
    }

    /// The walker emits `[]`; the mask grammar spells the wildcard `[*]`.
    /// Converting rather than making them identical keeps `key_path` a
    /// faithful record of where the value was found. Both are relative to
    /// the column, so a finding on the `stacktrace` column reads `[].abs_path`
    /// and masks as `[*].abs_path`.
    #[test]
    fn a_finding_path_converts_to_a_mask_path() {
        assert_eq!(
            finding_path_to_mask_path("[].abs_path").unwrap(),
            "[*].abs_path"
        );
        assert_eq!(
            finding_path_to_mask_path("customer.email").unwrap(),
            "customer.email"
        );
    }

    /// An array that is not the column root cannot be expressed by a grammar
    /// whose only lowering is `jsonb_array_elements(col)`, so the finding is
    /// reported and simply is not maskable.
    #[test]
    fn an_array_below_the_column_root_has_no_mask_path() {
        assert_eq!(
            finding_path_to_mask_path("breadcrumbs[].data.email"),
            Err(PathError::WildcardNotAtRoot)
        );
        assert_eq!(
            finding_path_to_mask_path("a.items[].email"),
            Err(PathError::WildcardNotFirst)
        );
    }

    /// A redacted segment names no real key, so a mask built from it would
    /// target a path that exists in no row.
    #[test]
    fn a_redacted_segment_has_no_mask_path() {
        assert_eq!(
            finding_path_to_mask_path("extra.<key>.email"),
            Err(PathError::RedactedSegment)
        );
    }
}
