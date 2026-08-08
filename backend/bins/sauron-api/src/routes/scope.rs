//! HTTP-layer parsing of the `environment_id` query parameter into
//! `sauron_db::scope::ReadScope`/`EnvFilter`, and — via
//! [`authorized_read_scope`] — the single call every environment-scoped
//! handler makes to turn that parameter into an authorized read.
//!
//! | value | meaning | reach required |
//! |---|---|---|
//! | absent | every environment the caller may read (auto-narrowed to a `Subset` for a partial-reach caller) | any reach on the app |
//! | `?environment_id=<uuid>` | that environment only | a grant reaching that specific environment (app-wide or that env's own) |
//! | `?environment_id=none` | rows with `environment_id IS NULL` | app-wide reach — unattributed rows belong to no single environment |
//! | anything else (including empty, i.e. `?environment_id=`) | `400` — **never** a silent fallback to "all" | n/a |
//!
//! A malformed value must be a `400`, not a silent fallback to `All`: falling
//! back would show the caller MORE data than they asked for, which is the
//! wrong direction to fail on a scoping parameter. The reach column is what
//! Slice 3 added: `environment_id` is now an access boundary enforced by
//! [`sauron_auth::authorize_env_read`], not just a display filter — see
//! [`authorized_read_scope`].
//!
//! ## The extractor trap, and why callers use [`raw_environment_id`]
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
//! [`authorized_read_scope`] is built on that source; every environment-scoped
//! handler calls it (passing `RawQuery`'s inner `Option<&str>`) rather than
//! feeding a `Query<T>`-deserialized `Option<String>` field into `parse_env`
//! by hand, so a future route gets this right by construction rather than by
//! remembering to.

use sauron_db::scope::{EnvFilter, ReadScope};
use sauron_db::AsyncPgConnection;
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
/// `app_id` the caller is also passing. The existence + app-ownership check
/// this doc comment used to ask Slice 3 for now happens in
/// `sauron_auth::rbac::resolve_env_filter` (via [`authorized_read_scope`]):
/// a `One(uuid)` naming an environment that doesn't exist, or belongs to a
/// different app, is refused with a `403`, not silently narrowed to zero rows.
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

/// Authorize an environment-scoped read and produce its `ReadScope` in one
/// call, sourcing `environment_id` from the raw query string.
///
/// This supersedes calling `authorize_app` and a hand-built `ReadScope`
/// separately. Both orderings of that pair were correct, but only if both
/// were present — and the whole history of this feature is defects where two
/// things that had to agree were maintained by hand (this module's own
/// `?environment_id=` extractor-trap regression, documented above, is one of
/// them). One call cannot half-happen.
///
/// `raw_query` is `axum::extract::RawQuery`'s inner `Option<&str>` — every
/// caller must extract it via `RawQuery`, not read `environment_id` off a
/// `Query<T>`-deserialized struct field, for the same reason
/// [`raw_environment_id`] exists (see the module docs' "extractor trap"
/// section).
pub async fn authorized_read_scope(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    raw_query: Option<&str>,
) -> Result<ReadScope, ApiError> {
    let requested = parse_env(raw_environment_id(raw_query).as_deref())?;
    let scope =
        sauron_auth::authorize_env_read(conn, user_id, app_id, permission, requested).await?;
    Ok(scope)
}

/// [`authorized_read_scope`], plus the caller's effective permission set at the
/// **resolved** scope — for handlers that gate a second capability on top of the
/// read itself.
///
/// Every current caller gates the same one: `source:read` over the
/// de-obfuscated source lines in an `ErrorEvent`'s `stacktrace_symbolicated`.
/// As of this comment there are six: `issues::detail`, `issues::events`,
/// `sessions::detail`, `devices::detail`, `screens::detail`,
/// `analytics::person` — `grep -n authorized_read_scope_with_perms
/// src/routes/*.rs` finds each of them. That grep returns NINE lines, not eight:
/// the six call sites, this file's doc and definition lines, and `mod.rs:74`,
/// which is prose in a comment rather than a call site. The last four were added when the gate turned out to be
/// enforced only on the issues pair while those four returned whole event rows,
/// context lines included, off `event:read` alone.
///
/// Use this instead of pairing [`authorized_read_scope`] with a separate
/// permission lookup. The separate lookup it replaces (`super::authorize_app_perms`,
/// now deleted) resolved permissions at `env: None`, which no environment-scoped
/// grant can ever satisfy — so those handlers `403`'d an env-scoped caller even
/// on their own environment. See `sauron_auth::authorize_env_read_with_perms`
/// for the full account; the ordering guarantee is that authorization happens
/// first, and the permission set is computed only after the scope is resolved.
pub async fn authorized_read_scope_with_perms(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    raw_query: Option<&str>,
) -> Result<(ReadScope, std::collections::HashSet<String>), ApiError> {
    let requested = parse_env(raw_environment_id(raw_query).as_deref())?;
    let resolved =
        sauron_auth::authorize_env_read_with_perms(conn, user_id, app_id, permission, requested)
            .await?;
    Ok(resolved)
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
}
