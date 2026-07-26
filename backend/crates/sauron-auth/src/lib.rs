//! `sauron-auth` — password hashing, JWT access/refresh handling, the axum
//! `AuthUser` extractor, and the fine-grained RBAC engine.

pub mod extractors;
pub mod guard;
pub mod jwt;
pub mod password;
pub mod rbac;

pub use extractors::{AuthError, AuthUser};
pub use guard::{
    check_no_escalation, check_role_edit, generate_temp_password, role_permissions, scope_parts,
};
pub use jwt::{hash_token, Claims, JwtKeys};
pub use password::{
    hash_password, hash_password_async, spend_dummy_verify, verify_password, verify_password_async,
};
pub use rbac::{
    authorize_app, authorize_org, authorize_project, effective_at, effective_at_org,
    ensure_preset_roles, perm, require_permission,
};
