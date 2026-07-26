//! Pure authorization guards.
//!
//! These take already-fetched data and return a decision. Keeping them free of
//! DB and axum-state dependencies is what makes them testable: CI runs
//! `cargo test --workspace` with no Postgres, so any guard reachable only
//! through a handler is a guard that never gets tested.

use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::extractors::AuthError;
use crate::rbac::perm;

/// Length of a generated temp password.
pub const TEMP_PASSWORD_LEN: usize = 16;

/// Alphabet for generated temp passwords. Excludes `0 O 1 l I`: these are
/// dictated aloud and retyped by hand, and a password nobody can transcribe
/// gets replaced by one the admin invents.
pub const TEMP_PASSWORD_ALPHABET: &str =
    "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Read a role's `permissions` JSONB column into a list.
///
/// Malformed JSONB yields an empty list rather than an error. An unreadable
/// permission set must fail closed (no permissions), and the caller's
/// escalation check then denies anything that depends on it.
pub fn role_permissions(perms: &Value) -> Vec<String> {
    match perms {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Refuse to hand out a permission the caller does not hold at that scope.
pub fn check_no_escalation(caller: &HashSet<String>, required: &[String]) -> Result<(), AuthError> {
    for p in required {
        if !caller.contains(p) {
            return Err(AuthError::Forbidden);
        }
    }
    Ok(())
}

/// Guard a role edit in both directions.
///
/// Adding a permission the caller lacks is escalation. Removing one they lack
/// is sabotage: a Developer holding `role:manage` could otherwise strip
/// `org:manage` from the Admin role and disable everyone above them. Only the
/// symmetric difference is checked, so a reordered no-op edit is free.
pub fn check_role_edit(
    caller: &HashSet<String>,
    old: &[String],
    new: &[String],
) -> Result<(), AuthError> {
    let old_set: HashSet<&String> = old.iter().collect();
    let new_set: HashSet<&String> = new.iter().collect();
    for p in new_set.symmetric_difference(&old_set) {
        if !caller.contains(*p) {
            return Err(AuthError::Forbidden);
        }
    }
    Ok(())
}

/// True when an edit strips `org:manage` from a permission set.
///
/// Order-independent: only membership matters, not position. Used to guard
/// both a grant edit (does the grant stop conferring `org:manage`?) and a role
/// edit (does the role stop granting it to every holder?) against orphaning an
/// org that has no other source of `org:manage` left.
pub fn drops_org_manage(old: &[String], new: &[String]) -> bool {
    old.iter().any(|p| p == perm::ORG_MANAGE) && !new.iter().any(|p| p == perm::ORG_MANAGE)
}

/// Split a grant's scope into the `(project, app)` pair `effective_at` expects.
///
/// `project_of_app` is the app's parent project, when the scope is an app and
/// the ancestry lookup succeeded. Unknown scope types fall back to org scope —
/// the narrowest authority — so a bad column value cannot widen permissions.
pub fn scope_parts(
    scope_type: &str,
    scope_id: Uuid,
    project_of_app: Option<Uuid>,
) -> (Option<Uuid>, Option<Uuid>) {
    match scope_type {
        "project" => (Some(scope_id), None),
        "app" => (project_of_app, Some(scope_id)),
        _ => (None, None),
    }
}

/// Map random bytes onto the alphabet, up to [`TEMP_PASSWORD_LEN`] characters.
///
/// Pure and deterministic, which is the whole point: the caller supplies the
/// randomness, so this can be tested with fixed input and no seedable RNG (the
/// workspace has no `rand` dependency and does not need one).
///
/// Uses rejection sampling. 256 is not a multiple of the 57-character
/// alphabet, so a plain `% 57` would make the first 28 characters roughly
/// twice as likely as the rest. Bytes at or above the largest multiple of 57
/// are discarded instead. May return fewer than `TEMP_PASSWORD_LEN` characters
/// if `bytes` is short or heavily rejected — `generate_temp_password` loops
/// until it has enough.
pub fn temp_password_from_bytes(bytes: &[u8]) -> String {
    let alphabet: Vec<char> = TEMP_PASSWORD_ALPHABET.chars().collect();
    let n = alphabet.len();
    let limit = (256 / n) * n; // 228 for a 57-char alphabet
    let mut out = String::with_capacity(TEMP_PASSWORD_LEN);
    for &b in bytes {
        if out.chars().count() >= TEMP_PASSWORD_LEN {
            break;
        }
        if (b as usize) < limit {
            out.push(alphabet[(b as usize) % n]);
        }
    }
    out
}

/// Generate a temp password from OS randomness.
///
/// Follows the crate convention in `sauron-core::ids` (`getrandom::fill`)
/// rather than pulling in `rand`. Loops because rejection sampling can consume
/// more bytes than it emits.
pub fn generate_temp_password() -> String {
    let mut out = String::with_capacity(TEMP_PASSWORD_LEN);
    while out.chars().count() < TEMP_PASSWORD_LEN {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("OS RNG must be available");
        let chunk = temp_password_from_bytes(&buf);
        for c in chunk.chars() {
            if out.chars().count() >= TEMP_PASSWORD_LEN {
                break;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;
    use uuid::Uuid;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn role_permissions_parses_a_string_array() {
        let v = json!(["issue:read", "app:read"]);
        assert_eq!(role_permissions(&v), strings(&["issue:read", "app:read"]));
    }

    #[test]
    fn role_permissions_tolerates_malformed_jsonb() {
        // A non-array must yield empty rather than panic: this value comes from
        // a JSONB column, not from validated input.
        assert!(role_permissions(&json!({})).is_empty());
        assert!(role_permissions(&json!("issue:read")).is_empty());
        assert!(role_permissions(&json!(null)).is_empty());
        assert!(role_permissions(&json!([])).is_empty());
        // Non-string members are skipped, not fatal.
        assert_eq!(
            role_permissions(&json!(["issue:read", 7])),
            strings(&["issue:read"])
        );
    }

    #[test]
    fn no_escalation_allows_a_superset_caller() {
        let caller = set(&["issue:read", "app:read", "org:manage"]);
        assert!(check_no_escalation(&caller, &strings(&["issue:read"])).is_ok());
    }

    #[test]
    fn no_escalation_allows_an_empty_requirement() {
        assert!(check_no_escalation(&set(&[]), &[]).is_ok());
    }

    #[test]
    fn no_escalation_rejects_a_missing_permission() {
        let caller = set(&["issue:read"]);
        let err = check_no_escalation(&caller, &strings(&["issue:read", "org:manage"]));
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_rejects_adding_a_permission_the_caller_lacks() {
        let caller = set(&["issue:read", "role:manage"]);
        let err = check_role_edit(
            &caller,
            &strings(&["issue:read"]),
            &strings(&["issue:read", "org:manage"]),
        );
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_rejects_removing_a_permission_the_caller_lacks() {
        // A Developer with role:manage must not be able to defang the Admin
        // role by stripping permissions that outrank them.
        let caller = set(&["issue:read", "role:manage"]);
        let err = check_role_edit(
            &caller,
            &strings(&["issue:read", "org:manage"]),
            &strings(&["issue:read"]),
        );
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_allows_changes_within_the_callers_own_grant() {
        let caller = set(&["issue:read", "issue:write", "app:read"]);
        assert!(check_role_edit(
            &caller,
            &strings(&["issue:read"]),
            &strings(&["issue:write", "app:read"])
        )
        .is_ok());
    }

    #[test]
    fn role_edit_allows_a_noop() {
        let caller = set(&["issue:read"]);
        let same = strings(&["issue:read"]);
        assert!(check_role_edit(&caller, &same, &same).is_ok());
    }

    #[test]
    fn role_edit_ignores_ordering_differences() {
        let caller = set(&["issue:read", "app:read"]);
        assert!(check_role_edit(
            &caller,
            &strings(&["issue:read", "app:read"]),
            &strings(&["app:read", "issue:read"]),
        )
        .is_ok());
    }

    #[test]
    fn drops_org_manage_true_when_old_has_it_and_new_does_not() {
        let old = strings(&["issue:read", "org:manage"]);
        let new = strings(&["issue:read"]);
        assert!(drops_org_manage(&old, &new));
    }

    #[test]
    fn drops_org_manage_false_when_both_have_it() {
        let old = strings(&["org:manage", "issue:read"]);
        let new = strings(&["issue:read", "org:manage"]);
        assert!(!drops_org_manage(&old, &new));
    }

    #[test]
    fn drops_org_manage_false_when_neither_has_it() {
        let old = strings(&["issue:read"]);
        let new = strings(&["issue:read", "app:read"]);
        assert!(!drops_org_manage(&old, &new));
    }

    #[test]
    fn drops_org_manage_false_when_new_gains_it() {
        let old = strings(&["issue:read"]);
        let new = strings(&["issue:read", "org:manage"]);
        assert!(!drops_org_manage(&old, &new));
    }

    #[test]
    fn drops_org_manage_ignores_position() {
        // org:manage sitting at a different index in each list must not change
        // the answer — only set membership matters.
        let old = strings(&["org:manage", "issue:read", "app:read"]);
        let new = strings(&["issue:read", "app:read"]);
        assert!(drops_org_manage(&old, &new));

        let old2 = strings(&["issue:read", "app:read", "org:manage"]);
        let new2 = strings(&["app:read", "org:manage", "issue:read"]);
        assert!(!drops_org_manage(&old2, &new2));
    }

    #[test]
    fn scope_parts_maps_each_scope_type() {
        let id = Uuid::new_v4();
        let project = Uuid::new_v4();
        assert_eq!(scope_parts("org", id, None), (None, None));
        assert_eq!(scope_parts("project", id, None), (Some(id), None));
        assert_eq!(
            scope_parts("app", id, Some(project)),
            (Some(project), Some(id))
        );
        // An app whose ancestry lookup failed still scopes to the app itself.
        assert_eq!(scope_parts("app", id, None), (None, Some(id)));
        // Unknown scope types degrade to org scope, the narrowest grant of
        // authority, so a bad value cannot widen anyone's effective permissions.
        assert_eq!(scope_parts("nonsense", id, None), (None, None));
    }

    #[test]
    fn temp_password_has_the_documented_shape() {
        let pw = generate_temp_password();
        assert_eq!(pw.chars().count(), TEMP_PASSWORD_LEN);
        assert!(pw.chars().all(|c| TEMP_PASSWORD_ALPHABET.contains(c)));
    }

    #[test]
    fn temp_password_excludes_visually_ambiguous_characters() {
        // These get read off a screen and retyped by hand.
        for c in ['0', 'O', '1', 'l', 'I'] {
            assert!(!TEMP_PASSWORD_ALPHABET.contains(c), "alphabet contains {c}");
        }
    }

    #[test]
    fn temp_password_from_bytes_is_deterministic() {
        let bytes: Vec<u8> = (0u8..64).collect();
        assert_eq!(
            temp_password_from_bytes(&bytes),
            temp_password_from_bytes(&bytes)
        );
    }

    #[test]
    fn temp_password_from_bytes_varies_with_input() {
        let a: Vec<u8> = (0u8..64).collect();
        let b: Vec<u8> = (64u8..128).collect();
        assert_ne!(temp_password_from_bytes(&a), temp_password_from_bytes(&b));
    }

    #[test]
    fn temp_password_rejects_biased_bytes() {
        // 256 is not a multiple of the 57-char alphabet, so bytes >= 228 must
        // be discarded rather than folded in with modulo — otherwise the first
        // 256 % 57 = 28 characters are ~2x as likely as the rest.
        let alphabet: Vec<char> = TEMP_PASSWORD_ALPHABET.chars().collect();
        assert_eq!(alphabet.len(), 57);
        // Bytes in [228, 255] are all rejected, so an input of only those
        // yields nothing at all.
        let all_rejected: Vec<u8> = (228u8..=255).collect();
        assert_eq!(temp_password_from_bytes(&all_rejected), "");
    }

    #[test]
    fn temp_password_from_bytes_stops_at_the_documented_length() {
        // Plenty of input must still yield exactly TEMP_PASSWORD_LEN.
        let plenty = vec![7u8; 512];
        assert_eq!(
            temp_password_from_bytes(&plenty).chars().count(),
            TEMP_PASSWORD_LEN
        );
    }

    #[test]
    fn generated_passwords_differ() {
        assert_ne!(generate_temp_password(), generate_temp_password());
    }
}
