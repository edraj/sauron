//! The access layer every tool goes through.
//!
//! The tools invent no permission logic of their own. Each one turns its
//! arguments into dashboard terms here (an app, an environment filter, a
//! release filter) and authorizes through the same `sauron_auth` calls the
//! REST handlers make, so a key sees exactly what its owner sees in the
//! dashboard, environment-scoped grants included.
//!
//! Two rules on top, both about not answering questions the caller has no
//! business asking:
//!
//! - **No existence oracle across tenants.** An app in an org where the caller
//!   holds no grant is "not found", in the same words as an app that does not
//!   exist, whether it was named by id or by slug. [`resolve_app`] checks
//!   membership before it looks at the org switch, so "MCP is turned off" is
//!   only ever said to that org's own members. Slugs go one step further:
//!   they match only among the apps the caller can READ (the `list_apps`
//!   set), so a slug cannot probe which sibling apps exist in their own org
//!   either.
//! - **No environment oracle.** An environment name the caller cannot see is
//!   "not found", in the same words as a name that does not exist, and the
//!   names offered back are only the ones they can see.
//!
//! Tools never emit progress or logging notifications (rmcp would silently
//! switch the reply to SSE), and nothing here may log the request `Parts`:
//! their headers carry the caller's raw API key.

use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::future::Future;
use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock};
use uuid::Uuid;

use sauron_auth::AuthError;
use sauron_core::Config;
use sauron_db::models::{App, Organization, Project};
use sauron_db::scope::{EnvFilter, ReadScope};
use sauron_db::{org_settings, repo, AsyncPgConnection};
use sauron_query::ReleaseFilter;

use crate::error::ApiError;
use crate::routes::environments::visible_app_environments;
use crate::routes::projects::{visible_apps, visible_projects};
use crate::AppState;

/// What a tool says when it runs out of time. The deadline sits inside the
/// request timeout, so Claude reads this instead of the `TimeoutLayer`'s bare
/// 503; a statement the database cancels reads the same.
pub(crate) const TIMED_OUT: &str = "query timed out; narrow the window or add an environment";

/// [`TIMED_OUT`] for a tool with nothing to narrow (no window, and no
/// environment argument that would make its read cheaper): `list_apps`,
/// `get_issue`, `describe_search`, `get_session_timeline`. They run under
/// [`within_deadline_saying`] with this text.
pub(crate) const TIMED_OUT_RETRY: &str = "query timed out; try again in a moment";

/// What a tool says when the authorization lookup itself failed in the
/// database: `sauron_auth` reports that as a bare `AuthError::Internal`,
/// whether it was a statement timeout or an outage.
const ACCESS_CHECK_FAILED: &str =
    "could not check your access in time (database slow or unavailable); try again";

/// Seconds the tool deadline leaves inside `API_REQUEST_TIMEOUT_SECS`, for
/// the error result to be built and written before the `TimeoutLayer` fires.
const DEADLINE_HEADROOM_SECS: u64 = 5;

/// `NoAccess`'s permission when a conversion has no way to know which one was
/// refused (a `Forbidden` from inside an extracted handler body).
const UNNAMED_PERMISSION: &str = "the permission this needs";

/// The dashboard route an app's links open on by default.
pub(crate) const APP_OVERVIEW_PATH: &str = "/overview";

/// One app with the project and org it belongs to: what a tool works on once
/// [`resolve_app`] has accepted the caller's reference.
#[derive(Debug, Clone)]
pub(crate) struct AppTarget {
    pub app: App,
    pub project: Project,
    pub org: Organization,
}

impl AppTarget {
    /// `org/project/app`: the reference `list_apps` prints and [`resolve_app`]
    /// accepts.
    pub(crate) fn reference(&self) -> String {
        format!("{}/{}/{}", self.org.slug, self.project.slug, self.app.slug)
    }
}

/// Why a tool could not answer. Every variant becomes an `isError: true`
/// result with one text block (see the `CallToolResult` conversion), never a
/// JSON-RPC error: MCP clients show a tool error's text to the model, which
/// can act on it, and render a protocol error opaquely.
#[derive(Debug)]
pub(crate) enum ToolError {
    /// The full message. Not found, or not visible to the caller; the two are
    /// deliberately indistinguishable.
    NotFound(String),
    /// Every full `org/project/app` reference a short one matched.
    Ambiguous(Vec<String>),
    NoAccess {
        perm: &'static str,
    },
    /// The org's name. Only ever produced for a member of that org.
    McpDisabled {
        org: String,
    },
    BadArgument(String),
    /// Could not answer in time, or not right now: the tool deadline, a
    /// cancelled statement, a rate limit, a rollup that is not ready. The
    /// message says which, and what to do.
    TimedOut(String),
    /// Logged server-side; the caller gets a generic message, since the
    /// detail can carry SQL, hosts or paths.
    Internal(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::NotFound(m) | ToolError::BadArgument(m) | ToolError::TimedOut(m) => {
                f.write_str(m)
            }
            ToolError::Ambiguous(refs) => write!(
                f,
                "that app reference matches more than one app: {}. Pass the full \
                 org/project/app reference.",
                refs.join(", ")
            ),
            ToolError::NoAccess { perm } => write!(
                f,
                "no access: your role lacks {perm} on this app or environment. Call \
                 list_apps to see what you can read."
            ),
            ToolError::McpDisabled { org } => write!(
                f,
                "MCP access is turned off for this organization ({org}). An admin of the \
                 organization can turn it back on in the Sauron dashboard."
            ),
            ToolError::Internal(_) => f.write_str(
                "internal error while running this tool; the details are in the Sauron \
                 server log",
            ),
        }
    }
}

/// The bare conversion: no log line. Tools go through
/// [`ToolError::into_result`], which logs first.
impl From<ToolError> for CallToolResult {
    fn from(e: ToolError) -> Self {
        CallToolResult::error(vec![ContentBlock::text(e.to_string())])
    }
}

impl ToolError {
    /// The variant's name, for a log line.
    fn kind(&self) -> &'static str {
        match self {
            ToolError::NotFound(_) => "not_found",
            ToolError::Ambiguous(_) => "ambiguous",
            ToolError::NoAccess { .. } => "no_access",
            ToolError::McpDisabled { .. } => "mcp_disabled",
            ToolError::BadArgument(_) => "bad_argument",
            ToolError::TimedOut(_) => "timed_out",
            ToolError::Internal(_) => "internal",
        }
    }

    /// The `isError` result for a failed call of `tool`, logged with the id
    /// of the key that made it (`key_id`, never the token): an internal
    /// failure with its detail, a timeout as a warning, and the caller's own
    /// refusals (not found, bad argument, …) at debug.
    pub(crate) fn into_result(self, tool: &'static str, key_id: Uuid) -> CallToolResult {
        match &self {
            ToolError::Internal(detail) => {
                tracing::error!(tool, key_id = %key_id, error = %detail, "mcp tool failed")
            }
            ToolError::TimedOut(_) => {
                tracing::warn!(tool, key_id = %key_id, kind = self.kind(), "mcp tool could not answer in time")
            }
            _ => tracing::debug!(tool, key_id = %key_id, kind = self.kind(), "mcp tool refused"),
        }
        CallToolResult::from(self)
    }
}

/// The generic not-found for an id an extracted handler body could not find.
fn generic_not_found() -> ToolError {
    ToolError::NotFound("not found, or not visible to you; call list_apps".into())
}

impl From<ApiError> for ToolError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Auth(a) => a.into(),
            ApiError::BadRequest(m) | ApiError::Conflict(m) | ApiError::Unprocessable(m) => {
                ToolError::BadArgument(m)
            }
            ApiError::Forbidden(_) => ToolError::NoAccess {
                perm: UNNAMED_PERMISSION,
            },
            ApiError::NotFound => generic_not_found(),
            // Aged out to cold storage or replaced: the message says which.
            ApiError::Gone(m) => ToolError::NotFound(m),
            ApiError::RateLimited => {
                ToolError::TimedOut("rate limited; wait a minute and try again".into())
            }
            ApiError::Unavailable("query_timeout", _) => ToolError::TimedOut(TIMED_OUT.into()),
            // Operator-facing text ("run the backfill", "X is not configured")
            // that the dashboard shows as-is.
            ApiError::Unavailable(_, m) => ToolError::TimedOut(m),
            ApiError::Internal(m) => ToolError::Internal(m),
        }
    }
}

impl From<AuthError> for ToolError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Forbidden => ToolError::NoAccess {
                perm: UNNAMED_PERMISSION,
            },
            AuthError::NotFound => generic_not_found(),
            // `sauron_auth`'s authorization lookups turn EVERY database
            // failure into a bare `Internal`, statement timeouts included, and
            // keep no detail. "Could not answer right now" is the honest
            // reading, and narrowing a window would not help an access check,
            // so this has its own text; the log says what is actually known.
            AuthError::Internal => {
                tracing::warn!(
                    "mcp tool: an authorization lookup failed in the database (a statement \
                     timeout or another database error; sauron-auth does not say which)"
                );
                ToolError::TimedOut(ACCESS_CHECK_FAILED.into())
            }
            // Credential states belong to `require_api_key`, which refuses
            // them before any tool runs; reaching one here is a bug.
            other => ToolError::Internal(format!("unexpected auth error in a tool: {other:?}")),
        }
    }
}

impl From<diesel::result::Error> for ToolError {
    fn from(e: diesel::result::Error) -> Self {
        match e {
            diesel::result::Error::NotFound => generic_not_found(),
            other if sauron_db::is_statement_timeout(&other) => {
                ToolError::TimedOut(TIMED_OUT.into())
            }
            other => ToolError::Internal(other.to_string()),
        }
    }
}

/// The one not-found for an app reference, whatever the reason: unknown, in
/// an org the caller is not a member of, or (by slug) an app their grants do
/// not reach. Only the echoed reference varies.
fn app_not_found(app_ref: &str) -> ToolError {
    ToolError::NotFound(format!(
        "no app {app_ref:?} among the apps you can read; call list_apps for the app references"
    ))
}

fn bad_app_ref(app_ref: &str) -> ToolError {
    ToolError::BadArgument(format!(
        "app {app_ref:?} is not an app id, `project/app` or `org/project/app` reference; call \
         list_apps for the app references"
    ))
}

/// Resolve the `app` argument a tool was given.
///
/// The order is fixed, and it is what makes the answers safe:
///
/// 1. Find the app: a UUID through `repo::app_ancestry`, otherwise
///    `[org/]project/app` slugs among the apps the caller can read, which is
///    exactly what `list_apps` prints (so an app in a foreign org, or one in
///    their own org that their grants do not reach, can neither match nor
///    make a reference ambiguous).
/// 2. The caller must hold a grant in the app's org, else the same not-found
///    as an app that does not exist. A UUID can name any org's app, which is
///    why this is checked rather than assumed.
/// 3. The org's MCP switch, else `McpDisabled`. Only members get this far,
///    so only members learn the switch is off.
///
/// By id, membership is all this checks: whether the caller may read the app
/// at all is the next call's job ([`read_scope`]), which answers `NoAccess`
/// to a member without reach, the same way the dashboard answers 403 for an
/// id. A slug is a name the caller typed, so it resolves only to apps they
/// can read; an unreadable one is the same not-found as a nonexistent one.
pub(crate) async fn resolve_app(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_ref: &str,
) -> Result<AppTarget, ToolError> {
    let app_ref = app_ref.trim();
    if app_ref.is_empty() {
        return Err(bad_app_ref(app_ref));
    }
    let found = match Uuid::parse_str(app_ref) {
        Ok(app_id) => find_app_by_id(conn, app_id).await?,
        Err(_) => find_app_by_slugs(conn, user_id, app_ref).await?,
    };
    let target = found.ok_or_else(|| app_not_found(app_ref))?;

    if repo::user_grants_in_org(conn, user_id, target.org.id)
        .await?
        .is_empty()
    {
        return Err(app_not_found(app_ref));
    }

    if !org_settings::mcp_enabled(conn, target.org.id).await? {
        return Err(ToolError::McpDisabled {
            org: target.org.name.clone(),
        });
    }
    Ok(target)
}

/// Any org's app. Membership is checked by the caller.
async fn find_app_by_id(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> Result<Option<AppTarget>, ToolError> {
    let Some((project_id, org_id)) = repo::app_ancestry(conn, app_id).await? else {
        return Ok(None);
    };
    // A concurrent delete can empty any of these; that is just not-found.
    let (Some(app), Some(project), Some(org)) = (
        repo::get_app(conn, app_id).await?,
        repo::get_project(conn, project_id).await?,
        repo::get_org(conn, org_id).await?,
    ) else {
        return Ok(None);
    };
    Ok(Some(AppTarget { app, project, org }))
}

/// `project/app` or `org/project/app`, matched exactly (slugs are stored
/// lowercase, and `list_apps` prints them as stored) among the apps the
/// caller can read: the reach-filtered `visible_projects` / `visible_apps`
/// that `list_apps` and the dashboard's switchers use, never every app of
/// every org they belong to. Otherwise a member granted one app could tell
/// a sibling app that exists (it would resolve, then `read_scope` would say
/// `NoAccess`) from one that does not (not found), and an `Ambiguous` answer
/// would list apps they cannot read.
///
/// Orgs whose MCP switch is off still take part, as they did before: picking
/// one of their apps answers `McpDisabled`, which is only said to members.
async fn find_app_by_slugs(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_ref: &str,
) -> Result<Option<AppTarget>, ToolError> {
    let segments: Vec<&str> = app_ref.split('/').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(bad_app_ref(app_ref));
    }
    let (org_slug, project_slug, app_slug) = match segments.as_slice() {
        [project, app] => (None, *project, *app),
        [org, project, app] => (Some(*org), *project, *app),
        _ => return Err(bad_app_ref(app_ref)),
    };

    let mut matches = Vec::new();
    for org in repo::list_orgs_for_user(conn, user_id).await? {
        if org_slug.is_some_and(|s| s != org.slug) {
            continue;
        }
        for project in visible_or_none(visible_projects(conn, user_id, org.id).await)? {
            if project.slug != project_slug {
                continue;
            }
            for app in visible_or_none(visible_apps(conn, user_id, project.id).await)? {
                if app.slug == app_slug {
                    matches.push(AppTarget {
                        app,
                        project: project.clone(),
                        org: org.clone(),
                    });
                }
            }
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => {
            let mut refs: Vec<String> = matches.iter().map(AppTarget::reference).collect();
            refs.sort();
            Err(ToolError::Ambiguous(refs))
        }
    }
}

/// A reach-filtered listing (`visible_projects`, `visible_apps`) read for
/// slug matching or for `list_apps`. `Forbidden` (no grant in that org any
/// more) and `NotFound` (the project went away between two reads) both mean
/// "nothing visible there", an empty listing, not a failed tool: a slug just
/// does not match in that org, and `list_apps` shows nothing under it.
pub(crate) fn visible_or_none<T>(r: Result<Vec<T>, ApiError>) -> Result<Vec<T>, ToolError> {
    match r {
        Ok(v) => Ok(v),
        Err(ApiError::Auth(AuthError::Forbidden)) | Err(ApiError::NotFound) => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// The names of the app's live environments the caller can see: the same
/// list the dashboard's environment picker shows them. Empty when they can
/// see none.
async fn visible_env_names(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
) -> Result<Vec<String>, ToolError> {
    match visible_app_environments(conn, user_id, app_id).await {
        Ok(envs) => Ok(envs.into_iter().map(|e| e.name).collect()),
        Err(ApiError::Auth(AuthError::Forbidden)) => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// The one not-found for an environment name: unknown, or not visible to the
/// caller. Only the echoed name varies; the list is what they can see.
fn env_not_found(target: &AppTarget, name: &str, visible: &[String]) -> ToolError {
    let offer = if visible.is_empty() {
        "you cannot read any environment of it by name; omit environment to cover every \
         environment you can see"
            .to_string()
    } else {
        format!("environments you can read: {}", visible.join(", "))
    };
    ToolError::NotFound(format!(
        "no environment named {name:?} on {} that you can read; {offer}",
        target.reference()
    ))
}

/// Authorize a read of `target` with `perm`, narrowed by the `environment`
/// argument, and return the scope plus the caller's permissions at it (for a
/// second gate such as `source:read`).
///
/// | `environment` | filter |
/// |---|---|
/// | absent | every environment the caller can read (a `Subset` for an env-scoped caller) |
/// | `none` | rows attributed to no environment (needs app-wide reach) |
/// | a UUID | that enrollment |
/// | anything else | the live environment with that name |
///
/// Refusals: `NoAccess { perm }`, except for a name, where an environment
/// the caller cannot see answers exactly like one that does not exist. A name
/// they CAN see (they hold `env:read` on it but not `perm`) is `NoAccess`:
/// it is already in their environment list, so hiding it would only make the
/// answer contradict itself.
pub(crate) async fn read_scope(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    target: &AppTarget,
    perm: &'static str,
    environment: Option<&str>,
) -> Result<(ReadScope, HashSet<String>), ToolError> {
    let app_id = target.app.id;
    let mut by_name = None;
    let filter = match environment.map(str::trim) {
        None => EnvFilter::All,
        Some("") => {
            return Err(ToolError::BadArgument(
                "environment must not be empty: pass a name such as production, or omit it to \
                 cover every environment you can see"
                    .into(),
            ))
        }
        Some("none") => EnvFilter::Unattributed,
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => EnvFilter::One(id),
            Err(_) => {
                let ids = repo::enrollment_ids_for_env_name(conn, &[app_id], raw).await?;
                match ids.as_slice() {
                    [] => {
                        let visible = visible_env_names(conn, user_id, app_id).await?;
                        return Err(env_not_found(target, raw, &visible));
                    }
                    [id] => {
                        by_name = Some(raw);
                        EnvFilter::One(*id)
                    }
                    // One live name per app is a unique index; never a Subset,
                    // which `resolve_env_filter` widens to All for app-wide
                    // callers.
                    _ => {
                        return Err(ToolError::Internal(format!(
                            "{} live enrollments named {raw:?} on app {app_id}",
                            ids.len()
                        )))
                    }
                }
            }
        },
    };

    match sauron_auth::authorize_env_read_with_perms(conn, user_id, app_id, perm, filter).await {
        Ok(scope) => Ok(scope),
        Err(AuthError::Forbidden) => match by_name {
            Some(name) => {
                let visible = visible_env_names(conn, user_id, app_id).await?;
                if visible.iter().any(|v| v == name) {
                    Err(ToolError::NoAccess { perm })
                } else {
                    Err(env_not_found(target, name, &visible))
                }
            }
            None => Err(ToolError::NoAccess { perm }),
        },
        Err(e) => Err(e.into()),
    }
}

/// The `release` argument, with `routes::scope::parse_release`'s rules:
/// absent is every release, `none` is events with no release, empty is
/// refused rather than read as "all".
pub(crate) fn release_arg(raw: Option<&str>) -> Result<ReleaseFilter, ToolError> {
    match raw.map(str::trim) {
        None => Ok(ReleaseFilter::All),
        Some("") => Err(ToolError::BadArgument(
            "release must not be empty: pass a version, none for events without a release, \
             or omit it to cover every release"
                .into(),
        )),
        Some("none") => Ok(ReleaseFilter::Unknown),
        Some(r) => Ok(ReleaseFilter::One(r.to_string())),
    }
}

/// The release key the rollup reads take: `None` for every release, `""`
/// for the unknown release. The same mapping as `analytics.rs`'s
/// `release_key`.
pub(crate) fn release_key_opt(r: &ReleaseFilter) -> Option<String> {
    match r {
        ReleaseFilter::All => None,
        ReleaseFilter::Unknown => Some(String::new()),
        ReleaseFilter::One(r) => Some(r.clone()),
    }
}

/// Run a tool body under the tool deadline. Every tool body runs inside this
/// (or [`within_deadline_saying`]), so a slow read comes back as
/// [`TIMED_OUT`] rather than as the outer `TimeoutLayer`'s bare 503, which
/// Claude cannot act on.
pub(crate) async fn within_deadline<T>(
    state: &AppState,
    fut: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    within_deadline_saying(state, TIMED_OUT, fut).await
}

/// [`within_deadline`] for a tool whose timeout advice is not [`TIMED_OUT`]'s
/// "narrow the window or add an environment" (see [`TIMED_OUT_RETRY`]): the
/// deadline, and every [`TIMED_OUT`] the body returns (a cancelled
/// statement), say `timed_out` instead. Other timeout texts (the access
/// check's, a rate limit's) pass through unchanged.
pub(crate) async fn within_deadline_saying<T>(
    state: &AppState,
    timed_out: &'static str,
    fut: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    run_with_deadline(
        tool_deadline(state.cfg.api_request_timeout_secs),
        timed_out,
        fut,
    )
    .await
}

pub(crate) fn tool_deadline(request_timeout_secs: u64) -> Duration {
    Duration::from_secs(
        request_timeout_secs
            .saturating_sub(DEADLINE_HEADROOM_SECS)
            .max(1),
    )
}

async fn run_with_deadline<T>(
    deadline: Duration,
    timed_out: &'static str,
    fut: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    match tokio::time::timeout(deadline, fut).await {
        Err(_) => Err(ToolError::TimedOut(timed_out.into())),
        Ok(Err(ToolError::TimedOut(m))) if m == TIMED_OUT => {
            Err(ToolError::TimedOut(timed_out.into()))
        }
        Ok(r) => r,
    }
}

/// A dashboard link to `hash_path` with the topbar scope set to `t` (and to
/// `env` / `release` when they narrow it), or `None` when `DASHBOARD_URL` is
/// unset: no link beats a link to a guessed origin.
pub(crate) fn deep_link(
    cfg: &Config,
    t: &AppTarget,
    hash_path: &str,
    env: &EnvFilter,
    release: &ReleaseFilter,
) -> Option<String> {
    let base = cfg.require_dashboard_url().ok()?;
    Some(link_url(base, t, hash_path, env, release))
}

/// The dashboard's shareable-scope URL (`dashboard/src/lib/models/scope-url.ts`):
/// scope in the real query string, the page in the hash. "All" is spelled by
/// absence, so `env` is set only for one environment or `none`, and a
/// `Subset` (the caller's own reach, not a selection) sets nothing.
fn link_url(
    base: &str,
    t: &AppTarget,
    hash_path: &str,
    env: &EnvFilter,
    release: &ReleaseFilter,
) -> String {
    let mut url = format!(
        "{base}/?org={}&project={}&app={}",
        t.org.id, t.project.id, t.app.id
    );
    match env {
        EnvFilter::One(id) => {
            let _ = write!(url, "&env={id}");
        }
        EnvFilter::Unattributed => url.push_str("&env=none"),
        EnvFilter::All | EnvFilter::Subset(_) => {}
    }
    match release {
        ReleaseFilter::One(r) => {
            url.push_str("&release=");
            url.extend(form_urlencoded::byte_serialize(r.as_bytes()));
        }
        ReleaseFilter::Unknown => url.push_str("&release=none"),
        ReleaseFilter::All => {}
    }
    url.push('#');
    url.push_str(hash_path);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use sauron_auth::perm;
    use sauron_db::models::{NewAppEnvironment, NewRoleGrant};
    use sauron_db::{org_settings, repo};
    use serde_json::json;

    // --- pure ------------------------------------------------------------------

    fn target() -> AppTarget {
        let now = Utc::now();
        let (org_id, project_id, app_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        AppTarget {
            app: App {
                id: app_id,
                name: "iOS".into(),
                slug: "ios".into(),
                platform: None,
                ingest_enabled: true,
                created_at: now,
                updated_at: now,
                app_type: "ios".into(),
                project_id,
                store_environment_id: None,
            },
            project: Project {
                id: project_id,
                org_id,
                name: "Mobile".into(),
                slug: "mobile".into(),
                created_at: now,
                updated_at: now,
            },
            org: Organization {
                id: org_id,
                name: "Acme".into(),
                slug: "acme".into(),
                created_at: now,
                updated_at: now,
                mcp_enabled: true,
            },
        }
    }

    fn text_of(r: &CallToolResult) -> String {
        assert_eq!(r.content.len(), 1, "exactly one content block");
        r.content[0].as_text().expect("a text block").text.clone()
    }

    #[test]
    fn tool_errors_render_as_one_text_block_flagged_is_error() {
        let cases = [
            ToolError::NotFound("no app \"x\"; call list_apps".into()),
            ToolError::Ambiguous(vec!["a/m/ios".into(), "b/m/ios".into()]),
            ToolError::NoAccess {
                perm: perm::EVENT_READ,
            },
            ToolError::McpDisabled { org: "Acme".into() },
            ToolError::BadArgument("window must be one of: 24h, 7d".into()),
            ToolError::TimedOut("query timed out".into()),
            ToolError::Internal("connection refused at 10.0.0.5:5432".into()),
        ];
        let texts: Vec<String> = cases
            .into_iter()
            .map(|e| {
                let r = CallToolResult::from(e);
                assert_eq!(r.is_error, Some(true));
                assert!(r.structured_content.is_none());
                text_of(&r)
            })
            .collect();
        assert!(texts[0].contains("call list_apps"), "{}", texts[0]);
        assert!(texts[1].contains("a/m/ios") && texts[1].contains("b/m/ios"));
        assert!(texts[1].contains("org/project/app"), "{}", texts[1]);
        assert!(texts[2].contains("event:read"), "{}", texts[2]);
        assert!(
            texts[3].contains("MCP access is turned off for this organization"),
            "{}",
            texts[3]
        );
        assert!(texts[3].contains("Acme"), "{}", texts[3]);
        assert!(texts[4].contains("24h, 7d"), "{}", texts[4]);
        assert!(texts[5].contains("timed out"), "{}", texts[5]);
        // The detail goes to the server log, never to the model.
        assert!(!texts[6].contains("10.0.0.5"), "{}", texts[6]);
        assert!(texts[6].contains("internal error"), "{}", texts[6]);
    }

    #[test]
    fn api_auth_and_diesel_errors_map_to_tool_errors() {
        assert!(matches!(
            ToolError::from(ApiError::BadRequest("bad".into())),
            ToolError::BadArgument(m) if m == "bad"
        ));
        assert!(matches!(
            ToolError::from(ApiError::NotFound),
            ToolError::NotFound(_)
        ));
        assert!(matches!(
            ToolError::from(ApiError::Auth(AuthError::Forbidden)),
            ToolError::NoAccess { .. }
        ));
        assert!(matches!(
            ToolError::from(AuthError::NotFound),
            ToolError::NotFound(_)
        ));
        assert!(matches!(
            ToolError::from(ApiError::Unavailable("query_timeout", "slow".into())),
            ToolError::TimedOut(m) if m == TIMED_OUT
        ));
        assert!(matches!(
            ToolError::from(ApiError::Internal("boom".into())),
            ToolError::Internal(m) if m == "boom"
        ));
        assert!(matches!(
            ToolError::from(diesel::result::Error::NotFound),
            ToolError::NotFound(_)
        ));
        assert!(matches!(
            ToolError::from(diesel::result::Error::BrokenTransactionManager),
            ToolError::Internal(_)
        ));
        // `sauron_auth` collapses every database failure in an authorization
        // lookup into `AuthError::Internal`, statement timeouts included, so
        // it reads as "could not check in time, try again", not as a tool bug
        // and not as advice to narrow a window.
        for e in [
            ToolError::from(AuthError::Internal),
            ToolError::from(ApiError::Auth(AuthError::Internal)),
        ] {
            assert!(
                matches!(&e, ToolError::TimedOut(m) if m == ACCESS_CHECK_FAILED),
                "{e:?}"
            );
            assert!(e.to_string().contains("try again"), "{e}");
            assert!(!e.to_string().contains("narrow the window"), "{e}");
        }
    }

    #[test]
    fn release_arg_maps_absent_none_and_rejects_empty() {
        assert_eq!(release_arg(None).unwrap(), ReleaseFilter::All);
        assert_eq!(release_arg(Some("none")).unwrap(), ReleaseFilter::Unknown);
        assert_eq!(
            release_arg(Some(" 1.4.2 ")).unwrap(),
            ReleaseFilter::One("1.4.2".into())
        );
        for empty in ["", "   "] {
            assert!(matches!(
                release_arg(Some(empty)),
                Err(ToolError::BadArgument(_))
            ));
        }
    }

    #[test]
    fn release_key_opt_is_the_rollup_key_shape() {
        assert_eq!(release_key_opt(&ReleaseFilter::All), None);
        assert_eq!(
            release_key_opt(&ReleaseFilter::Unknown),
            Some(String::new())
        );
        assert_eq!(
            release_key_opt(&ReleaseFilter::One("2.0".into())),
            Some("2.0".into())
        );
    }

    #[test]
    fn link_url_names_env_and_release_only_when_narrowed() {
        let t = target();
        let head = format!(
            "https://s.example/?org={}&project={}&app={}",
            t.org.id, t.project.id, t.app.id
        );
        assert_eq!(
            link_url(
                "https://s.example",
                &t,
                "/overview",
                &EnvFilter::All,
                &ReleaseFilter::All
            ),
            format!("{head}#/overview")
        );
        // A Subset is the caller's own reach, not a selection: no env param.
        assert_eq!(
            link_url(
                "https://s.example",
                &t,
                "/overview",
                &EnvFilter::Subset(vec![Uuid::new_v4()]),
                &ReleaseFilter::All
            ),
            format!("{head}#/overview")
        );
        let env = Uuid::new_v4();
        assert_eq!(
            link_url(
                "https://s.example",
                &t,
                "/issues/abc",
                &EnvFilter::One(env),
                &ReleaseFilter::One("1.0 beta+7&x=y".into())
            ),
            format!("{head}&env={env}&release=1.0+beta%2B7%26x%3Dy#/issues/abc")
        );
        assert_eq!(
            link_url(
                "https://s.example",
                &t,
                "/overview",
                &EnvFilter::Unattributed,
                &ReleaseFilter::Unknown
            ),
            format!("{head}&env=none&release=none#/overview")
        );
    }

    #[test]
    fn tool_deadline_sits_inside_the_request_timeout() {
        assert_eq!(tool_deadline(60), Duration::from_secs(55));
        assert_eq!(tool_deadline(30), Duration::from_secs(25));
        // Config clamps the request timeout to >= 5; never a zero deadline.
        assert_eq!(tool_deadline(5), Duration::from_secs(1));
        assert_eq!(tool_deadline(0), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn run_with_deadline_turns_an_overrun_into_timed_out() {
        let slow = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<_, ToolError>(1)
        };
        match run_with_deadline(Duration::from_millis(20), TIMED_OUT, slow).await {
            Err(ToolError::TimedOut(m)) => assert_eq!(m, TIMED_OUT),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        let fast = async { Ok::<_, ToolError>(7) };
        assert_eq!(
            run_with_deadline(Duration::from_secs(5), TIMED_OUT, fast)
                .await
                .unwrap(),
            7
        );
    }

    /// A tool with nothing to narrow says "try again", both when its deadline
    /// passes and when a statement inside it was cancelled (which converts
    /// to [`TIMED_OUT`]); the access check's and the rate limit's own texts
    /// are kept.
    #[tokio::test]
    async fn a_tool_without_a_window_gets_its_own_timeout_text() {
        let slow = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<_, ToolError>(1)
        };
        match run_with_deadline(Duration::from_millis(20), TIMED_OUT_RETRY, slow).await {
            Err(ToolError::TimedOut(m)) => assert_eq!(m, TIMED_OUT_RETRY),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        let cancelled = async { Err::<i32, _>(ToolError::TimedOut(TIMED_OUT.into())) };
        match run_with_deadline(Duration::from_secs(5), TIMED_OUT_RETRY, cancelled).await {
            Err(ToolError::TimedOut(m)) => assert_eq!(m, TIMED_OUT_RETRY),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert!(!TIMED_OUT_RETRY.contains("window"), "{TIMED_OUT_RETRY}");
        for kept in [
            ACCESS_CHECK_FAILED,
            "rate limited; wait a minute and try again",
        ] {
            let other = async move { Err::<i32, _>(ToolError::TimedOut(kept.into())) };
            match run_with_deadline(Duration::from_secs(5), TIMED_OUT_RETRY, other).await {
                Err(ToolError::TimedOut(m)) => assert_eq!(m, kept),
                other => panic!("expected TimedOut, got {other:?}"),
            }
        }
        let fine = async { Ok::<_, ToolError>(7) };
        assert_eq!(
            run_with_deadline(Duration::from_secs(5), TIMED_OUT_RETRY, fine)
                .await
                .unwrap(),
            7
        );
    }

    /// The log line itself is not captured (no tracing test subscriber in
    /// this crate); this pins that logging changes nothing the caller sees.
    #[test]
    fn into_result_returns_the_same_error_result_as_the_plain_conversion() {
        for (make, needle) in [
            (
                (|| ToolError::NotFound("no app \"x\"".into())) as fn() -> ToolError,
                "no app",
            ),
            (|| ToolError::TimedOut(TIMED_OUT.into()), "timed out"),
            (
                || ToolError::Internal("connection refused at 10.0.0.5:5432".into()),
                "internal error",
            ),
        ] {
            let logged = make().into_result("list_apps", Uuid::new_v4());
            let plain = CallToolResult::from(make());
            assert_eq!(logged.is_error, Some(true));
            assert_eq!(text_of(&logged), text_of(&plain));
            assert!(text_of(&logged).contains(needle), "{}", text_of(&logged));
        }
    }

    // --- database-backed: the authorization semantics ---------------------------

    /// See `tests/http_env_scoping.rs`'s identical helper for the full reasoning.
    fn swap_database(url: &str, new_db: &str) -> String {
        let (scheme, rest) = url.split_once("://").expect("scheme://...");
        let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..auth_end];
        let after = &rest[auth_end..];
        let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
        format!("{scheme}://{authority}/{new_db}{query}")
    }

    /// A fresh migrated database. `None` (the test skips) without
    /// `TEST_DATABASE_URL`.
    struct TestDb {
        admin_url: String,
        name: String,
        pool: sauron_db::PgPool,
    }

    impl TestDb {
        async fn setup() -> Option<TestDb> {
            let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
            // `sauron_test_<timestamp>_…`: the reaper in sauron-db's tests
            // parses the segment after `sauron_test_` as a timestamp.
            let name = format!(
                "sauron_test_{}_ma{}",
                Utc::now().timestamp(),
                Uuid::new_v4().simple()
            );
            sauron_db::create_test_database(&admin_url, &name)
                .await
                .expect("create migrated test database");
            let pool = sauron_db::build_pool(&swap_database(&admin_url, &name), 2)
                .expect("build test pool");
            Some(TestDb {
                admin_url,
                name,
                pool,
            })
        }

        async fn conn(&self) -> sauron_db::PgConn {
            sauron_db::conn(&self.pool).await.expect("checkout")
        }

        async fn teardown(self) {
            drop(self.pool);
            sauron_db::drop_database(&self.admin_url, &self.name)
                .await
                .expect("drop test database");
        }
    }

    async fn new_user(conn: &mut AsyncPgConnection) -> Uuid {
        repo::create_user(
            conn,
            &format!("access-{}@example.test", Uuid::new_v4().simple()),
            "unused-password-hash",
            "Access Test",
        )
        .await
        .expect("create user")
        .id
    }

    async fn grant(
        conn: &mut AsyncPgConnection,
        org_id: Uuid,
        user_id: Uuid,
        perms: serde_json::Value,
        scope_type: &str,
        scope_id: Uuid,
    ) {
        let role = repo::create_role(
            conn,
            org_id,
            &format!("role {}", Uuid::new_v4().simple()),
            "access-layer test role",
            perms,
        )
        .await
        .expect("create role");
        repo::create_grant(
            conn,
            NewRoleGrant {
                org_id,
                user_id,
                role_id: role.id,
                scope_type: scope_type.into(),
                scope_id,
            },
        )
        .await
        .expect("create grant");
    }

    fn reader() -> serde_json::Value {
        json!([
            perm::PROJECT_READ,
            perm::APP_READ,
            perm::ENV_READ,
            perm::EVENT_READ
        ])
    }

    /// `<org>/mobile/ios` in a fresh org.
    async fn org_with_app(conn: &mut AsyncPgConnection, label: &str) -> AppTarget {
        let org = repo::create_org(conn, label, &format!("{label}-{}", Uuid::new_v4().simple()))
            .await
            .expect("create org");
        let project = repo::create_project(conn, org.id, "Mobile", "mobile")
            .await
            .expect("create project");
        let app = repo::create_app(conn, project.id, "iOS", "ios", "ios")
            .await
            .expect("create app");
        AppTarget { app, project, org }
    }

    async fn enroll(
        conn: &mut AsyncPgConnection,
        t: &AppTarget,
        name: &str,
        default: bool,
    ) -> Uuid {
        let env = repo::create_project_environment(conn, t.project.id, name)
            .await
            .expect("create catalogue env");
        let key = format!("pk_{name}_{}", Uuid::new_v4().simple());
        repo::create_app_environments(
            conn,
            &[NewAppEnvironment {
                app_id: t.app.id,
                environment_id: env.id,
                public_key: &key,
                is_default: default,
            }],
        )
        .await
        .expect("enroll")
        .remove(0)
        .id
    }

    fn ref_of(t: &AppTarget) -> String {
        format!("{}/{}/{}", t.org.slug, t.project.slug, t.app.slug)
    }

    fn not_found(r: Result<impl std::fmt::Debug, ToolError>) -> String {
        match r {
            Err(ToolError::NotFound(m)) => m,
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// An app in an org the caller has no grant in answers exactly like an
    /// app that does not exist, by id and by slug. Anything else is an oracle
    /// for which ids and slugs exist in other tenants.
    #[tokio::test]
    async fn resolve_app_unknown_and_foreign_apps_read_the_same() {
        let Some(db) = TestDb::setup().await else {
            return;
        };
        let mut conn = db.conn().await;
        let mine = org_with_app(&mut conn, "mine").await;
        let foreign = org_with_app(&mut conn, "foreign").await;
        let user = new_user(&mut conn).await;
        grant(&mut conn, mine.org.id, user, reader(), "org", mine.org.id).await;

        // Controls: the caller's own app resolves every way it can be named,
        // and the foreign org's identical `mobile/ios` does not compete.
        for r in [
            mine.app.id.to_string(),
            ref_of(&mine),
            "mobile/ios".to_string(),
        ] {
            let got = resolve_app(&mut conn, user, &r).await.expect("own app");
            assert_eq!(got.app.id, mine.app.id, "{r}");
            assert_eq!(got.project.id, mine.project.id, "{r}");
            assert_eq!(got.org.id, mine.org.id, "{r}");
        }

        let foreign_id = foreign.app.id.to_string();
        let unknown_id = Uuid::new_v4().to_string();
        let by_foreign_id = not_found(resolve_app(&mut conn, user, &foreign_id).await);
        let by_unknown_id = not_found(resolve_app(&mut conn, user, &unknown_id).await);
        assert_eq!(
            by_foreign_id.replace(&foreign_id, "<ref>"),
            by_unknown_id.replace(&unknown_id, "<ref>")
        );
        assert!(by_unknown_id.contains("list_apps"), "{by_unknown_id}");

        let foreign_slug = ref_of(&foreign);
        let unknown_slug = format!("{}/mobile/nope", foreign.org.slug);
        let by_foreign_slug = not_found(resolve_app(&mut conn, user, &foreign_slug).await);
        let by_unknown_slug = not_found(resolve_app(&mut conn, user, &unknown_slug).await);
        assert_eq!(
            by_foreign_slug.replace(&foreign_slug, "<ref>"),
            by_unknown_slug.replace(&unknown_slug, "<ref>")
        );
        assert_eq!(
            by_foreign_slug.replace(&foreign_slug, "<ref>"),
            by_unknown_id.replace(&unknown_id, "<ref>"),
            "id and slug lookups fail with the same text"
        );

        drop(conn);
        db.teardown().await;
    }

    /// The switch is checked AFTER membership: a member learns MCP is off for
    /// their org; a non-member learns nothing, not even that the org exists.
    #[tokio::test]
    async fn resolve_app_reports_mcp_disabled_only_to_members() {
        let Some(db) = TestDb::setup().await else {
            return;
        };
        let mut conn = db.conn().await;
        let mine = org_with_app(&mut conn, "mine").await;
        let foreign = org_with_app(&mut conn, "foreign").await;
        let user = new_user(&mut conn).await;
        grant(&mut conn, mine.org.id, user, reader(), "app", mine.app.id).await;
        for org in [mine.org.id, foreign.org.id] {
            org_settings::set_mcp_enabled(&mut conn, org, false)
                .await
                .expect("turn MCP off");
        }

        for r in [mine.app.id.to_string(), ref_of(&mine)] {
            match resolve_app(&mut conn, user, &r).await {
                Err(ToolError::McpDisabled { org }) => assert_eq!(org, mine.org.name),
                other => panic!("{r}: expected McpDisabled, got {other:?}"),
            }
        }
        not_found(resolve_app(&mut conn, user, &foreign.app.id.to_string()).await);
        not_found(resolve_app(&mut conn, user, &ref_of(&foreign)).await);

        drop(conn);
        db.teardown().await;
    }

    #[tokio::test]
    async fn resolve_app_ambiguous_slug_lists_full_refs_and_rejects_bad_shapes() {
        let Some(db) = TestDb::setup().await else {
            return;
        };
        let mut conn = db.conn().await;
        let a = org_with_app(&mut conn, "alpha").await;
        let b = org_with_app(&mut conn, "beta").await;
        let user = new_user(&mut conn).await;
        grant(&mut conn, a.org.id, user, reader(), "org", a.org.id).await;
        grant(&mut conn, b.org.id, user, reader(), "org", b.org.id).await;

        match resolve_app(&mut conn, user, "mobile/ios").await {
            Err(ToolError::Ambiguous(refs)) => {
                let mut want = vec![ref_of(&a), ref_of(&b)];
                want.sort();
                assert_eq!(refs, want);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        let got = resolve_app(&mut conn, user, &ref_of(&b))
            .await
            .expect("full ref");
        assert_eq!(got.app.id, b.app.id);

        for bad in [
            "",
            "   ",
            "ios",
            "a/b/c/d",
            "mobile//ios",
            "/mobile/ios",
            "mobile/",
        ] {
            match resolve_app(&mut conn, user, bad).await {
                Err(ToolError::BadArgument(m)) => {
                    assert!(m.contains("list_apps"), "{bad:?}: {m}")
                }
                other => panic!("{bad:?}: expected BadArgument, got {other:?}"),
            }
        }

        drop(conn);
        db.teardown().await;
    }

    /// Environment names, from an env-scoped member's side. A sibling
    /// environment they hold no grant on reads exactly like a name that does
    /// not exist, and the list offered back is only what they can see.
    #[tokio::test]
    async fn read_scope_forbidden_and_unknown_env_names_read_the_same() {
        let Some(db) = TestDb::setup().await else {
            return;
        };
        let mut conn = db.conn().await;
        let t = org_with_app(&mut conn, "envs").await;
        let prod = enroll(&mut conn, &t, "production", true).await;
        let staging = enroll(&mut conn, &t, "staging", false).await;

        let owner = new_user(&mut conn).await;
        grant(&mut conn, t.org.id, owner, reader(), "org", t.org.id).await;
        let member = new_user(&mut conn).await;
        grant(&mut conn, t.org.id, member, reader(), "env", prod).await;
        // In the org, but reaching only a sibling app: no reach on `t`.
        let sibling = repo::create_app(&mut conn, t.project.id, "Android", "android", "android")
            .await
            .expect("sibling app");
        let outsider = new_user(&mut conn).await;
        grant(&mut conn, t.org.id, outsider, reader(), "app", sibling.id).await;

        let ev = perm::EVENT_READ;
        let scope = |r: Result<(ReadScope, HashSet<String>), ToolError>| match r {
            Ok((s, _)) => s.env,
            Err(e) => panic!("expected a scope, got {e:?}"),
        };

        // Owner: app-wide, so every form resolves as asked.
        assert_eq!(
            scope(read_scope(&mut conn, owner, &t, ev, None).await),
            EnvFilter::All
        );
        assert_eq!(
            scope(read_scope(&mut conn, owner, &t, ev, Some("staging")).await),
            EnvFilter::One(staging)
        );
        assert_eq!(
            scope(read_scope(&mut conn, owner, &t, ev, Some("none")).await),
            EnvFilter::Unattributed
        );
        assert_eq!(
            scope(read_scope(&mut conn, owner, &t, ev, Some(&prod.to_string())).await),
            EnvFilter::One(prod)
        );

        // Member: their own environment, by name or by omission.
        assert_eq!(
            scope(read_scope(&mut conn, member, &t, ev, None).await),
            EnvFilter::Subset(vec![prod])
        );
        let (s, perms) = read_scope(&mut conn, member, &t, ev, Some(" production "))
            .await
            .expect("own environment by name");
        assert_eq!(s.env, EnvFilter::One(prod));
        assert!(perms.contains(ev), "{perms:?}");

        let forbidden = not_found(read_scope(&mut conn, member, &t, ev, Some("staging")).await);
        let unknown = not_found(read_scope(&mut conn, member, &t, ev, Some("nope")).await);
        assert_eq!(
            forbidden.replace("\"staging\"", "\"<name>\""),
            unknown.replace("\"nope\"", "\"<name>\"")
        );
        assert!(unknown.contains("production"), "{unknown}");
        assert!(
            !unknown.contains("staging"),
            "lists only what they see: {unknown}"
        );

        for (arg, what) in [
            ("none", "unattributed"),
            (&*staging.to_string(), "sibling id"),
        ] {
            match read_scope(&mut conn, member, &t, ev, Some(arg)).await {
                Err(ToolError::NoAccess { perm }) => assert_eq!(perm, ev, "{what}"),
                other => panic!("{what}: expected NoAccess, got {other:?}"),
            }
        }
        assert!(matches!(
            read_scope(&mut conn, member, &t, ev, Some("  ")).await,
            Err(ToolError::BadArgument(_))
        ));

        // Outsider: an org member with no reach on this app at all.
        assert!(matches!(
            read_scope(&mut conn, outsider, &t, ev, None).await,
            Err(ToolError::NoAccess { perm }) if perm == ev
        ));
        let o_existing =
            not_found(read_scope(&mut conn, outsider, &t, ev, Some("production")).await);
        let o_unknown = not_found(read_scope(&mut conn, outsider, &t, ev, Some("nope")).await);
        assert_eq!(
            o_existing.replace("\"production\"", "\"<name>\""),
            o_unknown.replace("\"nope\"", "\"<name>\"")
        );
        assert!(!o_unknown.contains("staging"), "{o_unknown}");

        drop(conn);
        db.teardown().await;
    }

    /// Ruling R11's branch: the caller can SEE `staging` (env:read app-wide)
    /// but holds event:read on `production` only. Naming staging is
    /// `NoAccess` for event:read, not a not-found that lists staging as
    /// readable in the same breath. An unknown name is still `NotFound`, and
    /// offers both names they can see.
    #[tokio::test]
    async fn read_scope_visible_env_without_the_permission_is_no_access() {
        let Some(db) = TestDb::setup().await else {
            return;
        };
        let mut conn = db.conn().await;
        let t = org_with_app(&mut conn, "r11").await;
        let prod = enroll(&mut conn, &t, "production", true).await;
        enroll(&mut conn, &t, "staging", false).await;
        let user = new_user(&mut conn).await;
        grant(
            &mut conn,
            t.org.id,
            user,
            json!([perm::PROJECT_READ, perm::APP_READ, perm::ENV_READ]),
            "app",
            t.app.id,
        )
        .await;
        grant(
            &mut conn,
            t.org.id,
            user,
            json!([perm::EVENT_READ]),
            "env",
            prod,
        )
        .await;
        let ev = perm::EVENT_READ;

        // Control: event:read where they hold it.
        let (s, _) = read_scope(&mut conn, user, &t, ev, Some("production"))
            .await
            .expect("production is readable");
        assert_eq!(s.env, EnvFilter::One(prod));

        match read_scope(&mut conn, user, &t, ev, Some("staging")).await {
            Err(ToolError::NoAccess { perm }) => assert_eq!(perm, "event:read"),
            other => panic!("expected NoAccess for event:read, got {other:?}"),
        }
        let unknown = not_found(read_scope(&mut conn, user, &t, ev, Some("nope")).await);
        assert!(unknown.contains("\"nope\""), "{unknown}");
        assert!(unknown.contains("production"), "{unknown}");
        assert!(unknown.contains("staging"), "{unknown}");

        drop(conn);
        db.teardown().await;
    }
}
