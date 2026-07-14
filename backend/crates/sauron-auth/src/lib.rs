//! `sauron-auth` — password hashing, JWT access/refresh handling, the axum
//! `AuthUser` extractor, and the fine-grained RBAC engine.

pub mod extractors;
pub mod jwt;
pub mod password;
pub mod rbac;

pub use extractors::{AuthError, AuthUser};
pub use jwt::{hash_token, Claims, JwtKeys};
pub use password::{hash_password, verify_password};
pub use rbac::{
    authorize_app, authorize_org, authorize_project, effective_at, effective_at_org,
    ensure_preset_roles, perm, require_permission,
};
