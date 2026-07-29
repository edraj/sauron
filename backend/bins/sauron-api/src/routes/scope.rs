//! HTTP-layer parsing of the `environment_id` query parameter into
//! `sauron_db::scope::ReadScope`/`EnvFilter` — the wire contract for every
//! environment-scoped read.
//!
//! | value | meaning |
//! |---|---|
//! | absent | every environment, including unattributed rows |
//! | `?environment_id=<uuid>` | that environment only |
//! | `?environment_id=none` | rows with `environment_id IS NULL` |
//! | anything else (including empty, i.e. `?environment_id=`) | `400` — **never** a silent fallback to "all" |
//!
//! A malformed value must be a `400`, not a silent fallback to `All`: falling
//! back would show the caller MORE data than they asked for, which is the
//! wrong direction to fail on a scoping parameter (and Slice 3 makes this an
//! access boundary, not just a display nicety).
//!
//! ## The extractor trap, and why callers use [`read_scope_raw`]
//!
//! `?environment_id=` — the parameter present with an **empty** value — is
//! wire-indistinguishable from "absent" to some deserializers but not others:
//!
//! - `axum::extract::Query` (`serde_urlencoded`) deserializes an empty value
//!   into `Option<String>` as `Some("")`, which [`parse_env`] correctly
//!   rejects.
//! - `axum_extra::extract::Query` (`serde_html_form`, needed elsewhere in this
//!   crate for repeated-key `Vec<String>` fields like `filter=a&filter=b`,
//!   which `serde_urlencoded` cannot deserialize) treats an empty value as
//!   equivalent to the key being *absent* for `Option<T>`, producing `None`.
//!   Fed into [`parse_env`], `None` means `EnvFilter::All` — so a caller who
//!   typed `?environment_id=` (e.g. an empty store value interpolated into a
//!   URL) silently got every environment instead of a `400`.
//!
//! This is exactly backwards for a scoping parameter, and it depended on
//! which extractor a route handler happened to import — nothing about the
//! bug was visible from `parse_env` itself, which is correct in isolation
//! either way. The fix is to stop asking a `Query<T>` deserializer at all:
//! [`raw_environment_id`] reads `environment_id` directly out of the raw
//! query string (via `axum::extract::RawQuery`, upstream of any `Query`
//! codec), so presence/absence/emptiness is decided the same way regardless
//! of which extractor a handler's *other* query parameters go through.
//! [`read_scope_raw`] is [`read_scope`] wired to that source; every
//! environment-scoped handler should call it (with `RawQuery`) instead of
//! `read_scope` (with a `Query<T>`-deserialized `Option<String>` field) so a
//! future route gets this right by construction rather than by remembering
//! to.

use sauron_db::scope::{EnvFilter, ReadScope};
use uuid::Uuid;

use crate::error::ApiError;

/// Read `environment_id`'s raw value directly out of the query string,
/// distinguishing "the key is absent" from "the key is present with an empty
/// value" — a distinction a `Query<T>`-deserialized `Option<String>` field
/// loses (inconsistently across codecs; see the module docs above).
///
/// Returns `None` when `environment_id` does not appear at all. Returns
/// `Some(value)` (percent-decoded) when it does — `Some("")` for a bare
/// `?environment_id` or `?environment_id=`. That `Some("")` is what makes
/// [`parse_env`] reject it with a `400` instead of a `Query<T>` extractor
/// silently having already turned it into `None`.
pub fn raw_environment_id(raw_query: Option<&str>) -> Option<String> {
    let raw_query = raw_query?;
    form_urlencoded::parse(raw_query.as_bytes())
        .find(|(k, _)| k == "environment_id")
        .map(|(_, v)| v.into_owned())
}

/// Parse the `environment_id` query parameter into an [`EnvFilter`].
///
/// `raw` is the extracted `Option<String>` field's `.as_deref()`, i.e. `None`
/// when the parameter was absent from the query string.
///
/// **Syntax only.** A `One(uuid)` is only validated as a well-formed UUID —
/// this does not check that the environment exists, or that it belongs to the
/// `app_id` the caller is also passing. A foreign or made-up UUID therefore
/// currently ANDs against `app_id` in the query and matches nothing, i.e. it
/// narrows to zero rows rather than leaking another app's/environment's data.
/// That is the safe direction, which is why this is a note rather than a bug
/// fix here — but Slice 3 makes `environment_id` an RBAC access boundary, at
/// which point "matches nothing" is not the same guarantee as "caller is not
/// permitted to ask": this will need an existence + app-ownership check
/// (e.g. resolving `One(uuid)` against `environments` scoped by `app_id`)
/// before it can be trusted as a real boundary rather than a filter that
/// happens to match nothing.
pub fn parse_env(raw: Option<&str>) -> Result<EnvFilter, ApiError> {
    match raw {
        None => Ok(EnvFilter::All),
        Some("none") => Ok(EnvFilter::Unattributed),
        Some(s) => Uuid::parse_str(s).map(EnvFilter::One).map_err(|_| {
            ApiError::BadRequest(format!(
                "invalid environment_id: {s:?} (expected a UUID or \"none\")"
            ))
        }),
    }
}

/// Build a [`ReadScope`] for `app_id` from the raw `environment_id` query
/// parameter. The one call every scopeable handler in this crate makes.
pub fn read_scope(app_id: Uuid, raw: Option<&str>) -> Result<ReadScope, ApiError> {
    Ok(ReadScope::new(app_id, parse_env(raw)?))
}

/// [`read_scope`], but sourcing `environment_id` from the raw query string
/// (via [`raw_environment_id`]) instead of a `Query<T>`-deserialized field.
///
/// This is the one every environment-scoped handler in `analytics.rs` and
/// `issues.rs` calls, passing the `axum::extract::RawQuery`'s inner
/// `Option<String>` (`.as_deref()`'d). See the module docs for why: those two
/// files import `axum_extra::extract::Query` (for `Vec<String>` filter
/// fields), whose codec silently turns `?environment_id=` into "absent"
/// instead of the `400` a scoping parameter must get.
pub fn read_scope_raw(app_id: Uuid, raw_query: Option<&str>) -> Result<ReadScope, ApiError> {
    read_scope(app_id, raw_environment_id(raw_query).as_deref())
}

/// Reject `environment_id` outright rather than silently ignoring it.
///
/// For endpoints whose underlying resource has no environment dimension at
/// all — project-scoped monitors, org-scoped alert/notification config, saved
/// funnel *definitions* (as opposed to `funnels::compute`, which reads live
/// signal data and *is* scoped), symbol artifacts, and the admin storage
/// report. Accepting the parameter there and doing nothing with it is the
/// same class of bug as falling back to `All` on a scopeable endpoint: the
/// caller believes a filter was applied, and it silently wasn't.
pub fn reject_environment_id(raw: Option<&str>) -> Result<(), ApiError> {
    reject_environment_id_with_message(raw, "environment_id is not supported on this endpoint")
}

/// [`reject_environment_id`], but with a caller-supplied message instead of
/// the generic one — for an endpoint that wants to name its own specific
/// limitation (e.g. the cross-tier timeseries handlers in `analytics.rs`:
/// "cold storage is not partitioned by environment") rather than the generic
/// "not supported on this endpoint".
///
/// Still a `reject_environment_id*` call site as far as
/// `dashboard/src/lib/api/scope.ts`'s reconciliation grep is concerned — its
/// name is a prefix of `reject_environment_id`, so `grep reject_environment_id`
/// finds it too. That is deliberate: it is what lets an endpoint with a
/// bespoke rejection reason go through this module instead of hand-rolling an
/// inline `raw_environment_id(..).is_some()` check that the grep cannot see
/// (see the three `analytics.rs` timeseries handlers, and the review that
/// caught them not doing this).
pub fn reject_environment_id_with_message(
    raw: Option<&str>,
    message: &str,
) -> Result<(), ApiError> {
    if raw.is_some() {
        return Err(ApiError::BadRequest(message.into()));
    }
    Ok(())
}

/// Shared query shape for endpoints that only need to *reject* an
/// `environment_id`, with no query parameters of their own worth a bespoke
/// struct.
#[derive(Debug, serde::Deserialize)]
pub struct RejectEnvQuery {
    pub environment_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_means_all() {
        assert_eq!(parse_env(None).unwrap(), EnvFilter::All);
    }

    #[test]
    fn none_means_unattributed() {
        assert_eq!(parse_env(Some("none")).unwrap(), EnvFilter::Unattributed);
    }

    #[test]
    fn a_uuid_selects_one() {
        let id = Uuid::from_u128(42);
        assert_eq!(
            parse_env(Some(&id.to_string())).unwrap(),
            EnvFilter::One(id)
        );
    }

    /// A malformed value is a 400, NOT a silent fallback to All. Falling back
    /// would show the caller MORE data than they asked for, which is the
    /// wrong direction to fail on a scoping parameter.
    ///
    /// **This test does NOT cover the wire-level empty-value bug** that
    /// shipped in `analytics.rs`/`issues.rs` (see the module docs' "extractor
    /// trap" section): `parse_env(Some(""))` is an input this function can
    /// never actually receive from those two files' real `Query` extractor,
    /// because `axum_extra::extract::Query`'s codec (`serde_html_form`) never
    /// hands `parse_env` a `Some("")` for `?environment_id=` — it collapses
    /// that case to `None` before `parse_env` is ever called, so `parse_env`
    /// was always correct and the bug was entirely upstream of it. A unit
    /// test at this level, however thorough, cannot see a defect that lives
    /// in which extractor a handler imports; only a test that goes through
    /// the real axum router (`tests/http_env_scoping.rs`) can. Do not read
    /// this test passing as evidence that the wire-level case is covered.
    #[test]
    fn malformed_is_rejected_not_widened() {
        assert!(parse_env(Some("not-a-uuid")).is_err());
        assert!(parse_env(Some("")).is_err());
    }

    #[test]
    fn read_scope_carries_the_app_id_through() {
        let app_id = Uuid::from_u128(1);
        let scope = read_scope(app_id, None).unwrap();
        assert_eq!(scope.app_id, app_id);
        assert_eq!(scope.env, EnvFilter::All);
    }

    #[test]
    fn read_scope_rejects_malformed_same_as_parse_env() {
        let app_id = Uuid::from_u128(1);
        assert!(read_scope(app_id, Some("nope")).is_err());
    }

    #[test]
    fn reject_environment_id_passes_when_absent() {
        assert!(reject_environment_id(None).is_ok());
    }

    #[test]
    fn reject_environment_id_rejects_any_value_even_a_valid_one() {
        assert!(reject_environment_id(Some("none")).is_err());
        let id = Uuid::from_u128(9);
        assert!(reject_environment_id(Some(&id.to_string())).is_err());
    }

    #[test]
    fn reject_environment_id_with_message_passes_when_absent() {
        assert!(reject_environment_id_with_message(None, "custom reason").is_ok());
    }

    #[test]
    fn reject_environment_id_with_message_carries_the_custom_reason() {
        let err = reject_environment_id_with_message(Some("none"), "cold storage reason")
            .expect_err("must reject");
        assert!(
            format!("{err:?}").contains("cold storage reason"),
            "the custom message must reach the error, not the generic one: {err:?}"
        );
    }

    // --- raw_environment_id: the part a Query<T> deserializer can't do -----

    #[test]
    fn raw_absent_query_string_is_none() {
        assert_eq!(raw_environment_id(None), None);
    }

    #[test]
    fn raw_key_missing_from_query_string_is_none() {
        assert_eq!(raw_environment_id(Some("since_days=7&limit=20")), None);
    }

    /// The whole point: present-but-empty must come back `Some("")`, NOT
    /// `None` — this is exactly what `axum_extra::extract::Query` gets wrong
    /// and this function exists to route around.
    #[test]
    fn raw_present_but_empty_is_some_empty_string_not_none() {
        assert_eq!(
            raw_environment_id(Some("environment_id=")),
            Some(String::new())
        );
        assert_eq!(
            raw_environment_id(Some("since_days=7&environment_id=&limit=20")),
            Some(String::new())
        );
    }

    #[test]
    fn raw_present_with_value_is_decoded() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            raw_environment_id(Some(&format!("environment_id={id}"))),
            Some(id.to_string())
        );
    }

    #[test]
    fn raw_environment_id_feeds_read_scope_raw_the_same_way_parse_env_expects() {
        let app_id = Uuid::from_u128(1);
        // Absent -> All.
        assert_eq!(read_scope_raw(app_id, None).unwrap().env, EnvFilter::All);
        // Present-but-empty -> rejected, matching the malformed case.
        assert!(read_scope_raw(app_id, Some("environment_id=")).is_err());
        // A real value -> One(id).
        let id = Uuid::from_u128(2);
        assert_eq!(
            read_scope_raw(app_id, Some(&format!("environment_id={id}")))
                .unwrap()
                .env,
            EnvFilter::One(id)
        );
    }
}
