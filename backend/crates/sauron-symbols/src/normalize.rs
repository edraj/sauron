//! Canonical forms for the two keys an artifact is matched on.
//!
//! `symbol_artifacts.debug_id` and `symbol_artifacts.release` are compared with
//! plain SQL equality (`repo::find_artifact_by_debug_id`,
//! `repo::find_artifacts_for_release`), so a lookup key that differs from the
//! stored value by case or surrounding whitespace does not fail loudly — it
//! matches nothing, and the only symptom is `no_artifacts` on a crash weeks
//! later, indistinguishable from never having uploaded.
//!
//! Which is why normalization has to be applied on **both** sides, from **one**
//! definition. Normalizing the write path alone (where this started) does not
//! remove the asymmetry, it relocates it: with `debug_id` lowercased on upload
//! and the read side passing `debug_meta.build_id` through verbatim, a client
//! reporting `AB36…` stopped matching a stored `ab36…` that it used to match.
//!
//! Call sites, all of them:
//!
//! - write — `sauron-api`'s `routes::artifacts::upload` (the `?debug_id=` param);
//! - read — [`crate::engine::Symbolicator::symbolicate_dart`] and
//!   [`crate::engine::Symbolicator::symbolicate_js`], which is where the key is
//!   *chosen* (from `debug_meta`, or the trace's own `build_id` header) and
//!   therefore the one place upstream of every [`crate::engine::BlobFetch`]
//!   implementation. Normalizing in the implementations instead would mean two
//!   copies of the rule — one of them in the ingest worker, which has no test
//!   harness that could catch it drifting.
//!
//! Deliberately **not** applied in `dart_trace::parse`: a parser reports what the
//! VM printed. Canonicalizing for comparison is the comparer's job.

/// Canonical form of a build-id used as `symbol_artifacts.debug_id`: trimmed and
/// ASCII-lowercased.
///
/// It is hex, so case carries no information — `readelf`, [`crate::build_id_hex`]
/// and the Dart VM all print lowercase. The non-canonical sources are a human
/// pasting an id out of a build log into `?debug_id=`, and any toolchain or SDK
/// that reports it uppercase in `debug_meta.build_id`.
///
/// Case and whitespace only. Separators are **not** stripped: Dart build-ids
/// carry none, and a dashed id is the canonical form for other debug-id
/// flavours, so removing dashes would corrupt a correct value in order to rescue
/// a wrong one.
pub fn normalize_debug_id(debug_id: &str) -> String {
    debug_id.trim().to_ascii_lowercase()
}

/// Canonical form of a release string: trimmed, nothing else.
///
/// Case is **not** folded. A release is an opaque identifier chosen by the app
/// (`MyApp@1.4.2+12`), so lowercasing it would break matches that work today.
/// The realistic defect is surrounding whitespace: an SDK whose release is read
/// from a file or an env var at init carries the trailing newline with it, and
/// `"1.0.0\n"` never matches the `1.0.0` someone uploaded the map under.
pub fn normalize_release(release: &str) -> &str {
    release.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_ids_fold_case_and_strip_padding() {
        assert_eq!(normalize_debug_id(" AB36961B44BAEF9D "), "ab36961b44baef9d");
        assert_eq!(normalize_debug_id("ab36961b44baef9d"), "ab36961b44baef9d");
        // Idempotent — it is applied on both sides of the match, and a
        // second application must not move the value.
        let once = normalize_debug_id("\tAb36\n");
        assert_eq!(normalize_debug_id(&once), once);
    }

    #[test]
    fn debug_id_separators_survive() {
        // The canonical form of a Breakpad/PDB-flavoured id keeps its dashes;
        // stripping them would turn a correct value into a permanent non-match.
        assert_eq!(
            normalize_debug_id("A1B2C3D4-1234-5678-9ABC-DEF012345678"),
            "a1b2c3d4-1234-5678-9abc-def012345678"
        );
    }

    #[test]
    fn releases_are_trimmed_but_not_case_folded() {
        assert_eq!(normalize_release(" 1.0.0\n"), "1.0.0");
        assert_eq!(normalize_release("MyApp@1.4.2+12"), "MyApp@1.4.2+12");
        assert_eq!(normalize_release("   "), "");
    }
}
