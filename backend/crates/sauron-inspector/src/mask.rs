//! The mask applier over an owned `serde_json::Value`.
//!
//! Used twice with identical semantics: by the pipeline enforcer on inbound
//! wire payloads, and as the reference the SQL lowering's tests are written
//! against. The value at the path becomes the JSON string `"****"` and THE KEY
//! IS RETAINED — removing it changes row shape, breaks the `contexts`
//! named-block structure, and makes a `has:<key>` predicate report absence
//! where data existed, which is a second, subtler lie.
//!
//! Consequences that must stay in the spec, the dialog and the wiki: the TYPE
//! changes (`extra.cart_value_cents: 4200` becomes `"****"`, so arithmetic,
//! `@>` containment and B-tree comparison stop working for masked rows), and
//! masking `event_user.email` breaks the shipped `user.email:` search
//! dimension.

use serde_json::Value;

use crate::path::{parse_mask_path, MaskPath};

pub const MASK_SENTINEL: &str = "****";

/// Normalize a column value to an object, the way the SQL lowering's
/// `coalesce(col, '{}'::jsonb)` does.
///
/// `jsonb_set` returns NULL if any argument is NULL, and a NULL written into a
/// `NOT NULL DEFAULT '{}'` column is the single most likely implementation bug
/// in this slice. A scalar (`"[Circular]"` is real live data) normalizes the
/// same way rather than being masked into something that looks like a value.
pub fn object_or_empty(v: &mut Value) {
    if !v.is_object() {
        *v = Value::Object(serde_json::Map::new());
    }
}

/// Apply one parsed mask path. Returns how many values were replaced.
///
/// `doc` is one COLUMN's value and the path is relative to it, so a wildcard
/// iterates `doc` ITSELF — byte-for-byte the set of elements the SQL
/// lowering's `jsonb_array_elements(col)` produces. Reaching through a named
/// head here instead would make the ingest-time enforcer and the retro-mask
/// disagree about what one stored `json_path` means, and the disagreement
/// would only ever surface as data that quietly stayed raw.
pub fn apply_mask_path(doc: &mut Value, p: &MaskPath) -> usize {
    if p.wildcard {
        let Value::Array(items) = doc else {
            return 0;
        };
        let sub = p.sub_array();
        let mut n = 0;
        for item in items.iter_mut() {
            n += set_at(item, &sub);
        }
        return n;
    }
    set_at(doc, &p.text_array())
}

/// Apply a stored wire-form path (`inspector_masked_keys.json_path`).
///
/// An unparseable path is a NO-OP rather than an error: this runs on the
/// ingest hot path, and a stored row written by a newer binary must never be
/// able to drop an event.
pub fn apply_wire_path(doc: &mut Value, wire: &str) -> usize {
    match parse_mask_path(wire) {
        Ok(p) => apply_mask_path(doc, &p),
        Err(_) => 0,
    }
}

/// Replace the value at `segments`, if the whole path exists. Missing =
/// untouched, matching `jsonb_set(..., create_missing => false)`.
fn set_at(doc: &mut Value, segments: &[String]) -> usize {
    if segments.is_empty() {
        if doc.is_null() {
            return 0;
        }
        *doc = Value::String(MASK_SENTINEL.to_string());
        return 1;
    }
    let mut cur = doc;
    for seg in &segments[..segments.len() - 1] {
        match cur.get_mut(seg) {
            Some(next) => cur = next,
            None => return 0,
        }
    }
    let last = &segments[segments.len() - 1];
    match cur.get_mut(last) {
        Some(slot) => {
            *slot = Value::String(MASK_SENTINEL.to_string());
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::parse_mask_path;
    use serde_json::json;

    fn apply(doc: &mut serde_json::Value, path: &str) -> usize {
        apply_mask_path(doc, &parse_mask_path(path).unwrap())
    }

    #[test]
    fn masks_three_levels_into_extra() {
        let mut d = json!({"a": {"b": {"email": "jane@acme.com", "keep": 1}}});
        assert_eq!(apply(&mut d, "a.b.email"), 1);
        assert_eq!(d, json!({"a": {"b": {"email": "****", "keep": 1}}}));
    }

    /// `create_missing = false` semantics: a row lacking the path is
    /// untouched. Writing the sentinel into rows that never had the field
    /// would make a `has:<key>` predicate report presence where there was
    /// none — the exact inverse of the key-removal lie the design rejects.
    #[test]
    fn a_missing_path_leaves_the_document_byte_identical() {
        let before = json!({"a": {"b": 1}});
        let mut d = before.clone();
        assert_eq!(apply(&mut d, "a.c.email"), 0);
        assert_eq!(d, before);
    }

    /// If the value at the path is an object or array, the whole subtree
    /// collapses: the subtree IS the PII.
    #[test]
    fn an_object_value_collapses_wholesale() {
        let mut d = json!({"customer": {"email": "a@b.c", "name": "Jane"}});
        assert_eq!(apply(&mut d, "customer"), 1);
        assert_eq!(d, json!({"customer": "****"}));
    }

    /// Ordinality matters: `jsonb_agg` order is not guaranteed, and the Rust
    /// applier must match the SQL lowering's guarantee, not merely happen to.
    ///
    /// `doc` here is the COLUMN VALUE, and `error_events.breadcrumbs` is an
    /// array at its root — which is why the path is a bare `[*]` and not
    /// `breadcrumbs[*]`. Same bytes the SQL `jsonb_array_elements(col)` sees.
    #[test]
    fn a_wildcard_preserves_order_and_length() {
        let mut d = json!([
            {"data": {"email": "a@b.c"}, "n": 1},
            {"data": {"other": 2}, "n": 2},
            {"data": {"email": "d@e.f"}, "n": 3}
        ]);
        assert_eq!(apply(&mut d, "[*].data.email"), 2);
        let arr = d.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["data"]["email"], json!("****"));
        assert_eq!(arr[1]["data"], json!({"other": 2}));
        assert_eq!(arr[2]["n"], json!(3));
    }

    #[test]
    fn an_empty_array_stays_an_empty_array() {
        let mut d = json!([]);
        assert_eq!(apply(&mut d, "[*].data.email"), 0);
        assert_eq!(d, json!([]));
    }

    #[test]
    fn a_wildcard_over_a_non_array_does_nothing() {
        let mut d = json!("[Circular]");
        assert_eq!(apply(&mut d, "[*].data.email"), 0);
        assert_eq!(d, json!("[Circular]"));
    }

    /// `jsonb_set` returns NULL if ANY argument is NULL, and a NULL written
    /// into a `NOT NULL DEFAULT '{}'` column is the single most likely
    /// implementation bug in this slice. The Rust side normalizes the same
    /// way the SQL `coalesce(col, '{}'::jsonb)` does.
    #[test]
    fn a_null_column_normalizes_to_an_object_not_sql_null() {
        let mut d = json!(null);
        object_or_empty(&mut d);
        assert_eq!(d, json!({}));
        let mut s = json!("[Circular]");
        object_or_empty(&mut s);
        assert_eq!(s, json!({}));
        let mut keep = json!({"a": 1});
        object_or_empty(&mut keep);
        assert_eq!(keep, json!({"a": 1}));
    }

    #[test]
    fn the_key_is_retained_never_removed() {
        let mut d = json!({"email": "a@b.c"});
        assert_eq!(apply(&mut d, "email"), 1);
        assert!(d.as_object().unwrap().contains_key("email"));
    }

    /// The wire applier takes the same wire-form string the pipeline reads out
    /// of `inspector_masked_keys.json_path`, so the enforcer and the retro-mask
    /// can never disagree about what a path means.
    #[test]
    fn the_wire_applier_accepts_the_stored_form() {
        let mut d = json!({"user": {"email": "a@b.c"}});
        assert_eq!(apply_wire_path(&mut d, "user.email"), 1);
        assert_eq!(d["user"]["email"], json!("****"));
        // An unparseable stored path is a no-op, never a panic: the pipeline
        // runs this on the ingest hot path.
        assert_eq!(apply_wire_path(&mut d, "a..b"), 0);
    }
}
