//! `list_apps`: the organizations, projects and apps a key can read.
//!
//! What it lists comes from the same functions the dashboard's switchers call
//! (`visible_projects`, `visible_apps`, `visible_app_environments`), so a key
//! sees exactly what its owner's org / project / app / environment pickers
//! show, env-only grants included. Orgs whose admin turned MCP off are left
//! out and counted at the end.

use rmcp::model::{CallToolResult, ContentBlock};
use uuid::Uuid;

use sauron_auth::{perm, AuthError};
use sauron_db::models::Organization;
use sauron_db::scope::EnvFilter;
use sauron_db::{releases, repo, AsyncPgConnection};
use sauron_query::ReleaseFilter;

use crate::error::ApiError;
use crate::mcp::access::{
    deep_link, visible_or_none, within_deadline_saying, AppTarget, ToolError, APP_OVERVIEW_PATH,
    TIMED_OUT_RETRY,
};
use crate::mcp::format::{one_line, NAME_MAX_CHARS};
use crate::routes::environments::visible_app_environments;
use crate::routes::projects::{visible_apps, visible_projects};
use crate::AppState;

/// How many releases each app shows, most recently seen first.
const RECENT_RELEASES: usize = 3;
/// Cap on a displayed release string, which an SDK reported.
const RELEASE_MAX_CHARS: usize = 80;

pub(crate) async fn list_apps(
    state: &AppState,
    user_id: Uuid,
) -> Result<CallToolResult, ToolError> {
    within_deadline_saying(state, TIMED_OUT_RETRY, async {
        let mut conn = crate::routes::db(state).await?;
        let text = render(state, &mut conn, user_id).await?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

async fn render(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<String, ToolError> {
    let (orgs, hidden): (Vec<Organization>, Vec<Organization>) =
        repo::list_orgs_for_user(conn, user_id)
            .await?
            .into_iter()
            .partition(|o| o.mcp_enabled);

    let mut body = Vec::new();
    let mut apps_listed = 0usize;
    for org in &orgs {
        body.push(String::new());
        body.push(format!(
            "Organization {} ({})",
            one_line(&org.name, NAME_MAX_CHARS),
            org.slug
        ));
        let projects = visible_or_none(visible_projects(conn, user_id, org.id).await)?;
        if projects.is_empty() {
            body.push("  (no projects you can read)".into());
        }
        for project in projects {
            body.push(format!(
                "  Project {} ({})",
                one_line(&project.name, NAME_MAX_CHARS),
                project.slug
            ));
            let apps = visible_or_none(visible_apps(conn, user_id, project.id).await)?;
            if apps.is_empty() {
                body.push("    (no apps you can read)".into());
            }
            for app in apps {
                apps_listed += 1;
                let target = AppTarget {
                    app,
                    project: project.clone(),
                    org: org.clone(),
                };
                render_app(state, conn, user_id, &target, &mut body).await?;
            }
        }
    }

    let mut out = vec![if apps_listed == 0 {
        "You cannot read any app through MCP.".to_string()
    } else {
        format!(
            "{apps_listed} app(s) you can read, in {} organization(s). This is the complete \
             list. Pass an app's reference (org/project/app; project/app is accepted when \
             unique among the apps you can read in organizations you belong to) as the `app` \
             argument of the other tools.",
            orgs.len()
        )
    }];
    out.extend(body);
    if !hidden.is_empty() {
        out.push(String::new());
        out.push(format!(
            "{} organization(s) hidden: MCP access is turned off by an admin",
            hidden.len()
        ));
    }
    Ok(out.join("\n"))
}

async fn render_app(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    t: &AppTarget,
    out: &mut Vec<String>,
) -> Result<(), ToolError> {
    out.push(format!(
        "    - {}: {}",
        t.reference(),
        one_line(&t.app.name, NAME_MAX_CHARS)
    ));
    out.push(format!("      type: {}", t.app.app_type));
    out.push(format!(
        "      environments: {}",
        environments(conn, user_id, t.app.id).await?
    ));
    out.push(format!(
        "      latest releases: {}",
        latest_releases(conn, user_id, t.app.id).await?
    ));
    if let Some(link) = deep_link(
        &state.cfg,
        t,
        APP_OVERVIEW_PATH,
        &EnvFilter::All,
        &ReleaseFilter::All,
    ) {
        out.push(format!("      link: {link}"));
    }
    Ok(())
}

fn withheld(permission: &str) -> String {
    format!("withheld (your role lacks {permission})")
}

/// The live environments the caller can see, by name, the default marked.
/// Reads only the name and the default flag: the rows also carry the ingest
/// `public_key`, which is for SDKs and never goes into tool output.
async fn environments(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
) -> Result<String, ToolError> {
    let envs = match visible_app_environments(conn, user_id, app_id).await {
        Ok(envs) => envs,
        Err(ApiError::Auth(AuthError::Forbidden)) => return Ok(withheld(perm::ENV_READ)),
        Err(e) => return Err(e.into()),
    };
    if envs.is_empty() {
        return Ok("none".into());
    }
    Ok(envs
        .iter()
        .map(|e| {
            let name = one_line(&e.name, NAME_MAX_CHARS);
            if e.enrollment.is_default {
                format!("{name} (default)")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(", "))
}

/// The most recently seen releases across the environments the caller can
/// read, behind `event:read` like the dashboard's `/releases`.
async fn latest_releases(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
) -> Result<String, ToolError> {
    let scope = match sauron_auth::authorize_env_read(
        conn,
        user_id,
        app_id,
        perm::EVENT_READ,
        EnvFilter::All,
    )
    .await
    {
        Ok(scope) => scope,
        Err(AuthError::Forbidden) => return Ok(withheld(perm::EVENT_READ)),
        Err(e) => return Err(e.into()),
    };
    // Cut in SQL: an app with a release per CI build has thousands of
    // catalogue rows, and this prints three names for every app listed.
    // Releases reach `app_releases` trimmed and non-blank (the ingest edge
    // and the pipeline normalise them first).
    let recent = releases::recent_for_app(conn, &scope, RECENT_RELEASES as i64).await?;
    if recent.is_empty() {
        return Ok("none seen yet".into());
    }
    Ok(recent
        .iter()
        .map(|r| one_line(r, RELEASE_MAX_CHARS))
        .collect::<Vec<_>>()
        .join(", "))
}
