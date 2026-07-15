//! Role-based access control.
//!
//! The resolution core ([`effective_permissions`] / [`has_permission`]) is a
//! pure function over a user's grants, so it is exhaustively unit-tested without
//! a database. The `authorize_*` helpers fetch grants and enforce.
//!
//! Cascade semantics: to check permission `P` on a resource, we pass the
//! resource's `(org, project?, app?)` ids. A grant contributes its permissions
//! when its scope matches one of those ids — so an **org** grant satisfies any
//! check in the org, a **project** grant satisfies checks on that project and
//! its apps (but not sibling projects), and an **app** grant satisfies only that
//! app. The result is a union down the tree with strict sibling isolation.

use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use sauron_db::models::{App, Project};
use sauron_db::{repo, AsyncPgConnection};

use crate::extractors::AuthError;

/// Canonical permission strings.
pub mod perm {
    pub const ISSUE_READ: &str = "issue:read";
    pub const ISSUE_WRITE: &str = "issue:write";
    pub const EVENT_READ: &str = "event:read";
    pub const FUNNEL_WRITE: &str = "funnel:write";
    pub const ARTIFACT_WRITE: &str = "artifact:write";
    /// View de-obfuscated **source code** (symbolication context lines). Symbol
    /// names / file / line are visible with `issue:read`; this gates the code.
    pub const SOURCE_READ: &str = "source:read";
    pub const MONITOR_READ: &str = "monitor:read";
    pub const MONITOR_WRITE: &str = "monitor:write";
    pub const APP_READ: &str = "app:read";
    pub const APP_CREATE: &str = "app:create";
    pub const APP_UPDATE: &str = "app:update";
    pub const APP_DELETE: &str = "app:delete";
    pub const APP_ROTATE_KEY: &str = "app:rotate_key";
    pub const PROJECT_READ: &str = "project:read";
    pub const PROJECT_CREATE: &str = "project:create";
    pub const PROJECT_UPDATE: &str = "project:update";
    pub const PROJECT_DELETE: &str = "project:delete";
    pub const MEMBER_READ: &str = "member:read";
    pub const MEMBER_MANAGE: &str = "member:manage";
    pub const ROLE_MANAGE: &str = "role:manage";
    pub const ORG_MANAGE: &str = "org:manage";

    /// Every permission, in canonical order.
    pub const ALL: [&str; 21] = [
        ISSUE_READ,
        ISSUE_WRITE,
        EVENT_READ,
        FUNNEL_WRITE,
        ARTIFACT_WRITE,
        SOURCE_READ,
        MONITOR_READ,
        MONITOR_WRITE,
        APP_READ,
        APP_CREATE,
        APP_UPDATE,
        APP_DELETE,
        APP_ROTATE_KEY,
        PROJECT_READ,
        PROJECT_CREATE,
        PROJECT_UPDATE,
        PROJECT_DELETE,
        MEMBER_READ,
        MEMBER_MANAGE,
        ROLE_MANAGE,
        ORG_MANAGE,
    ];
}

/// A seeded, non-editable role.
pub struct PresetRole {
    pub name: &'static str,
    pub description: &'static str,
    pub permissions: &'static [&'static str],
}

pub const OWNER: PresetRole = PresetRole {
    name: "Owner",
    description: "Full control including organization settings",
    permissions: &perm::ALL,
};

pub const ADMIN: PresetRole = PresetRole {
    name: "Admin",
    description: "Manage projects, apps, members and roles",
    permissions: &[
        perm::ISSUE_READ,
        perm::ISSUE_WRITE,
        perm::EVENT_READ,
        perm::FUNNEL_WRITE,
        perm::ARTIFACT_WRITE,
        perm::SOURCE_READ,
        perm::MONITOR_READ,
        perm::MONITOR_WRITE,
        perm::APP_READ,
        perm::APP_CREATE,
        perm::APP_UPDATE,
        perm::APP_DELETE,
        perm::APP_ROTATE_KEY,
        perm::PROJECT_READ,
        perm::PROJECT_CREATE,
        perm::PROJECT_UPDATE,
        perm::PROJECT_DELETE,
        perm::MEMBER_READ,
        perm::MEMBER_MANAGE,
        perm::ROLE_MANAGE,
    ],
};

pub const DEVELOPER: PresetRole = PresetRole {
    name: "Developer",
    description: "Work with issues and apps",
    permissions: &[
        perm::ISSUE_READ,
        perm::ISSUE_WRITE,
        perm::EVENT_READ,
        perm::FUNNEL_WRITE,
        perm::ARTIFACT_WRITE,
        perm::SOURCE_READ,
        perm::MONITOR_READ,
        perm::MONITOR_WRITE,
        perm::APP_READ,
        perm::APP_CREATE,
        perm::APP_UPDATE,
        perm::APP_ROTATE_KEY,
        perm::PROJECT_READ,
        perm::MEMBER_READ,
    ],
};

pub const VIEWER: PresetRole = PresetRole {
    name: "Viewer",
    description: "Read-only access",
    permissions: &[
        perm::ISSUE_READ,
        perm::EVENT_READ,
        perm::MONITOR_READ,
        perm::APP_READ,
        perm::PROJECT_READ,
        perm::MEMBER_READ,
    ],
};

pub const PRESETS: [PresetRole; 4] = [OWNER, ADMIN, DEVELOPER, VIEWER];

/// The level a grant applies at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Org(Uuid),
    Project(Uuid),
    App(Uuid),
}

/// A user's grant: a scope plus the permissions its role confers.
#[derive(Clone, Debug)]
pub struct Grant {
    pub scope: Scope,
    pub permissions: Vec<String>,
}

fn grant_applies(scope: Scope, org: Uuid, project: Option<Uuid>, app: Option<Uuid>) -> bool {
    match scope {
        Scope::Org(o) => o == org,
        Scope::Project(p) => Some(p) == project,
        Scope::App(a) => Some(a) == app,
    }
}

/// The union of all permissions the user has on the target `(org, project?, app?)`.
pub fn effective_permissions(
    grants: &[Grant],
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> HashSet<String> {
    let mut set = HashSet::new();
    for g in grants {
        if grant_applies(g.scope, org, project, app) {
            for p in &g.permissions {
                set.insert(p.clone());
            }
        }
    }
    set
}

/// Whether the user holds `permission` on the target (short-circuits).
pub fn has_permission(
    grants: &[Grant],
    permission: &str,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> bool {
    grants.iter().any(|g| {
        grant_applies(g.scope, org, project, app) && g.permissions.iter().any(|p| p == permission)
    })
}

/// Convert `(scope_type, scope_id, permissions_json)` rows into [`Grant`]s.
pub fn grants_from_rows(rows: Vec<(String, Uuid, Value)>) -> Vec<Grant> {
    rows.into_iter()
        .filter_map(|(scope_type, scope_id, perms)| {
            let scope = match scope_type.as_str() {
                "org" => Scope::Org(scope_id),
                "project" => Scope::Project(scope_id),
                "app" => Scope::App(scope_id),
                _ => return None,
            };
            let permissions = match perms {
                Value::Array(a) => a
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            };
            Some(Grant { scope, permissions })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Enforcement (DB-backed)
// ---------------------------------------------------------------------------

/// Load the user's grants in an org and check a permission at a target scope.
pub async fn require_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: &str,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> Result<(), AuthError> {
    let rows = repo::user_grants_in_org(conn, user_id, org)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);
    if has_permission(&grants, permission, org, project, app) {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

/// The user's effective permission set at an arbitrary target scope.
pub async fn effective_at(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> Result<HashSet<String>, AuthError> {
    let rows = repo::user_grants_in_org(conn, user_id, org)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);
    Ok(effective_permissions(&grants, org, project, app))
}

/// The user's effective permission set at an org (for `GET /me/access`).
pub async fn effective_at_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org: Uuid,
) -> Result<HashSet<String>, AuthError> {
    effective_at(conn, user_id, org, None, None).await
}

/// Authorize an **org**-scoped action.
pub async fn authorize_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    permission: &str,
) -> Result<(), AuthError> {
    require_permission(conn, user_id, permission, org_id, None, None).await
}

/// Authorize a **project**-scoped action; returns the project.
pub async fn authorize_project(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    project_id: Uuid,
    permission: &str,
) -> Result<Project, AuthError> {
    let project = repo::get_project(conn, project_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    require_permission(
        conn,
        user_id,
        permission,
        project.org_id,
        Some(project_id),
        None,
    )
    .await?;
    Ok(project)
}

/// Authorize an **app**-scoped action; returns the app.
pub async fn authorize_app(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
) -> Result<App, AuthError> {
    let app = repo::get_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    let (project_id, org_id) = repo::app_ancestry(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    require_permission(
        conn,
        user_id,
        permission,
        org_id,
        Some(project_id),
        Some(app_id),
    )
    .await?;
    Ok(app)
}

/// Idempotently sync the seeded preset roles from code (called at startup).
pub async fn ensure_preset_roles(conn: &mut AsyncPgConnection) -> anyhow::Result<()> {
    for preset in PRESETS {
        let perms = Value::Array(
            preset
                .permissions
                .iter()
                .map(|s| Value::String((*s).to_string()))
                .collect(),
        );
        repo::upsert_preset_role(conn, preset.name, preset.description, &perms)
            .await
            .map_err(|e| anyhow::anyhow!("seed preset {}: {e}", preset.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> Uuid {
        Uuid::from_u128(1)
    }
    fn proj_a() -> Uuid {
        Uuid::from_u128(10)
    }
    fn proj_b() -> Uuid {
        Uuid::from_u128(11)
    }
    fn app_a1() -> Uuid {
        Uuid::from_u128(100)
    }
    fn app_a2() -> Uuid {
        Uuid::from_u128(101)
    }
    fn app_b1() -> Uuid {
        Uuid::from_u128(110)
    }

    fn grant(scope: Scope, perms: &[&str]) -> Grant {
        Grant {
            scope,
            permissions: perms.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn preset_grant(scope: Scope, p: &PresetRole) -> Grant {
        Grant {
            scope,
            permissions: p.permissions.iter().map(|s| s.to_string()).collect(),
        }
    }

    // --- preset role permission sets ------------------------------------

    #[test]
    fn owner_has_every_permission() {
        for p in perm::ALL {
            assert!(OWNER.permissions.contains(&p), "Owner missing {p}");
        }
        assert_eq!(OWNER.permissions.len(), 21);
    }

    #[test]
    fn admin_is_all_except_org_manage() {
        assert!(!ADMIN.permissions.contains(&perm::ORG_MANAGE));
        assert_eq!(ADMIN.permissions.len(), 20);
        for p in perm::ALL {
            if p != perm::ORG_MANAGE {
                assert!(ADMIN.permissions.contains(&p), "Admin missing {p}");
            }
        }
    }

    #[test]
    fn developer_can_write_issues_not_manage_members() {
        assert!(DEVELOPER.permissions.contains(&perm::ISSUE_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::APP_ROTATE_KEY));
        assert!(!DEVELOPER.permissions.contains(&perm::MEMBER_MANAGE));
        assert!(!DEVELOPER.permissions.contains(&perm::PROJECT_DELETE));
        assert!(!DEVELOPER.permissions.contains(&perm::ROLE_MANAGE));
        assert!(DEVELOPER.permissions.contains(&perm::FUNNEL_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::ARTIFACT_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::SOURCE_READ));
        assert_eq!(DEVELOPER.permissions.len(), 14);
    }

    #[test]
    fn viewer_cannot_write_funnels() {
        assert!(VIEWER.permissions.contains(&perm::EVENT_READ));
        assert!(!VIEWER.permissions.contains(&perm::FUNNEL_WRITE));
    }

    #[test]
    fn viewer_is_read_only() {
        for p in VIEWER.permissions {
            assert!(p.ends_with(":read"), "Viewer has non-read perm {p}");
        }
        assert!(VIEWER.permissions.contains(&perm::ISSUE_READ));
        assert!(!VIEWER.permissions.contains(&perm::ISSUE_WRITE));
        assert_eq!(VIEWER.permissions.len(), 6);
    }

    #[test]
    fn preset_names_are_unique() {
        let names: HashSet<_> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), PRESETS.len());
    }

    #[test]
    fn all_permissions_are_unique() {
        let set: HashSet<_> = perm::ALL.iter().collect();
        assert_eq!(set.len(), perm::ALL.len(), "duplicate in perm::ALL");
        assert_eq!(perm::ALL.len(), 21);
    }

    #[test]
    fn every_preset_permission_is_a_known_permission() {
        for preset in PRESETS {
            for p in preset.permissions {
                assert!(
                    perm::ALL.contains(p),
                    "{} has unknown permission {p}",
                    preset.name
                );
            }
            // no duplicate perms within a preset
            let set: HashSet<_> = preset.permissions.iter().collect();
            assert_eq!(
                set.len(),
                preset.permissions.len(),
                "{} has dupes",
                preset.name
            );
        }
    }

    #[test]
    fn roles_form_a_strict_ladder() {
        // Viewer ⊂ Developer ⊂ Admin ⊂ Owner
        let v: HashSet<_> = VIEWER.permissions.iter().collect();
        let d: HashSet<_> = DEVELOPER.permissions.iter().collect();
        let a: HashSet<_> = ADMIN.permissions.iter().collect();
        let o: HashSet<_> = OWNER.permissions.iter().collect();
        assert!(v.is_subset(&d), "Viewer not ⊆ Developer");
        assert!(d.is_subset(&a), "Developer not ⊆ Admin");
        assert!(a.is_subset(&o), "Admin not ⊆ Owner");
    }

    #[test]
    fn multiple_grants_at_same_scope_union() {
        let g = vec![
            grant(Scope::App(app_a1()), &[perm::ISSUE_READ]),
            grant(Scope::App(app_a1()), &[perm::ISSUE_WRITE]),
        ];
        let eff = effective_permissions(&g, org(), Some(proj_a()), Some(app_a1()));
        assert!(eff.contains(perm::ISSUE_READ));
        assert!(eff.contains(perm::ISSUE_WRITE));
        assert_eq!(eff.len(), 2);
    }

    #[test]
    fn permission_match_is_exact_not_prefix_or_substring() {
        let g = vec![grant(Scope::Org(org()), &["issue:rea"])];
        assert!(!has_permission(&g, perm::ISSUE_READ, org(), None, None));
        let g2 = vec![grant(Scope::Org(org()), &[perm::ISSUE_READ])];
        assert!(!has_permission(&g2, "issue", org(), None, None));
        assert!(!has_permission(&g2, "issue:read:extra", org(), None, None));
    }

    #[test]
    fn org_scope_check_ignores_lower_scoped_grants() {
        // A user with only project/app grants has NO org-level permissions.
        let g = vec![
            preset_grant(Scope::Project(proj_a()), &OWNER),
            preset_grant(Scope::App(app_a1()), &OWNER),
        ];
        assert!(effective_permissions(&g, org(), None, None).is_empty());
        assert!(!has_permission(&g, perm::MEMBER_READ, org(), None, None));
    }

    // --- org-scope grant cascades to everything -------------------------

    #[test]
    fn org_grant_applies_at_every_level() {
        let g = vec![preset_grant(Scope::Org(org()), &DEVELOPER)];
        // org-level check
        assert!(has_permission(&g, perm::ISSUE_READ, org(), None, None));
        // project-level check
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None
        ));
        // app-level check
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
        // but not a permission the role lacks
        assert!(!has_permission(&g, perm::ORG_MANAGE, org(), None, None));
    }

    #[test]
    fn org_grant_for_a_different_org_never_applies() {
        let other = Uuid::from_u128(999);
        let g = vec![preset_grant(Scope::Org(other), &OWNER)];
        assert!(!has_permission(&g, perm::ISSUE_READ, org(), None, None));
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
    }

    // --- project-scope grant: its apps yes, siblings no -----------------

    #[test]
    fn project_grant_covers_its_apps_only() {
        let g = vec![preset_grant(Scope::Project(proj_a()), &DEVELOPER)];
        // app in project A
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
        // another app in project A
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a2())
        ));
        // app in project B — DENIED (sibling isolation)
        assert!(!has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_b()),
            Some(app_b1())
        ));
        // project A itself
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None
        ));
        // project B itself — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_b()),
            None
        ));
        // org level — DENIED (project grant doesn't grant org-wide)
        assert!(!has_permission(&g, perm::ISSUE_READ, org(), None, None));
    }

    // --- app-scope grant: that app only ---------------------------------

    #[test]
    fn app_grant_covers_that_app_only() {
        let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
        // sibling app — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a2())
        ));
        // project-level op — DENIED (app grant can't authorize project ops)
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None
        ));
        // org-level op — DENIED
        assert!(!has_permission(&g, perm::ISSUE_READ, org(), None, None));
    }

    // --- union of multiple grants ---------------------------------------

    #[test]
    fn permissions_union_across_grants() {
        let g = vec![
            grant(Scope::App(app_a1()), &[perm::ISSUE_READ]),
            grant(Scope::Project(proj_a()), &[perm::EVENT_READ]),
            grant(Scope::Org(org()), &[perm::APP_READ]),
        ];
        // app check sees all three levels unioned
        let eff = effective_permissions(&g, org(), Some(proj_a()), Some(app_a1()));
        assert!(eff.contains(perm::ISSUE_READ));
        assert!(eff.contains(perm::EVENT_READ));
        assert!(eff.contains(perm::APP_READ));
        assert_eq!(eff.len(), 3);

        // a sibling app in the SAME project inherits the project + org grants,
        // but NOT the app_a1-specific grant.
        let eff2 = effective_permissions(&g, org(), Some(proj_a()), Some(app_a2()));
        assert!(eff2.contains(perm::APP_READ)); // org grant
        assert!(eff2.contains(perm::EVENT_READ)); // project-A grant
        assert!(!eff2.contains(perm::ISSUE_READ)); // app_a1-specific grant does NOT apply
        assert_eq!(eff2.len(), 2);

        // an app in a DIFFERENT project inherits only the org grant.
        let eff3 = effective_permissions(&g, org(), Some(proj_b()), Some(app_b1()));
        assert!(eff3.contains(perm::APP_READ));
        assert!(!eff3.contains(perm::EVENT_READ));
        assert!(!eff3.contains(perm::ISSUE_READ));
        assert_eq!(eff3.len(), 1);
    }

    #[test]
    fn viewer_denied_write_but_allowed_read() {
        let g = vec![preset_grant(Scope::Org(org()), &VIEWER)];
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
        assert!(!has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
        assert!(!has_permission(&g, perm::MEMBER_MANAGE, org(), None, None));
    }

    #[test]
    fn empty_grants_deny_everything() {
        let g: Vec<Grant> = vec![];
        for p in perm::ALL {
            assert!(!has_permission(
                &g,
                p,
                org(),
                Some(proj_a()),
                Some(app_a1())
            ));
        }
        assert!(effective_permissions(&g, org(), Some(proj_a()), Some(app_a1())).is_empty());
    }

    #[test]
    fn monitor_perms_are_registered_and_seeded() {
        // Both perms exist in the canonical list.
        assert!(perm::ALL.contains(&perm::MONITOR_READ));
        assert!(perm::ALL.contains(&perm::MONITOR_WRITE));
        // Owner (=ALL) has both.
        assert!(OWNER.permissions.contains(&perm::MONITOR_WRITE));
        // Viewer reads but cannot write.
        assert!(VIEWER.permissions.contains(&perm::MONITOR_READ));
        assert!(!VIEWER.permissions.contains(&perm::MONITOR_WRITE));
        // Developer can write.
        assert!(DEVELOPER.permissions.contains(&perm::MONITOR_WRITE));
    }

    #[test]
    fn grants_from_rows_parses_scopes_and_perms() {
        let rows = vec![
            (
                "org".to_string(),
                org(),
                serde_json::json!(["issue:read", "app:read"]),
            ),
            ("project".to_string(), proj_a(), serde_json::json!([])),
            (
                "app".to_string(),
                app_a1(),
                serde_json::json!(["issue:write"]),
            ),
            ("bogus".to_string(), org(), serde_json::json!(["x"])), // dropped
        ];
        let grants = grants_from_rows(rows);
        assert_eq!(grants.len(), 3); // bogus dropped
        assert!(has_permission(&grants, perm::ISSUE_READ, org(), None, None));
        assert!(has_permission(
            &grants,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1())
        ));
    }
}
