//! `list_releases`: the releases of an app, and whether the last one broke
//! things.
//!
//! The release list is the dashboard's switcher source
//! (`sauron_db::releases::list_for_app`, behind `event:read`), folded across
//! environments: first and last seen per release, newest first. Beside each
//! release, over a window, come its error events, distinct issues and NEW
//! issues (issues whose first hourly bucket falls in that release), read from
//! the `issue_stats_hourly` rollup.
//!
//! The error columns appear only when both hold: the caller has `issue:read`
//! (they count issues), and the app's rollup is ready (`issue_stats::is_ready`,
//! so a count is never silently partial). Otherwise a single note line names
//! the condition that failed, and the list stands without the columns. The
//! window applies to those columns; the list itself is every release, newest
//! `last_seen_at` first, cut by `limit`. When the rollup's fold trails the
//! clock, a line under the window line says how far the counts reach. Each release links to the
//! dashboard's Issues page scoped to it (when `DASHBOARD_URL` is set).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::releases::{self, AppReleaseRow};
use sauron_db::rollups::issue_stats::{self, ReleaseErrors};
use sauron_db::rollups::{self, SRC_ISSUE_STATS};
use sauron_db::scope::Range;
use sauron_db::AsyncPgConnection;
use sauron_query::ReleaseFilter;

use crate::mcp::access::{deep_link, read_scope, resolve_app, within_deadline, ToolError};
use crate::mcp::format::{
    day, env_label, env_names, minute, one_line, parse_limit, parse_window, plural,
    rollup_lag_note, rollup_lag_tolerance, trimmed, BACKFILL_COMMAND,
};
use crate::AppState;

const WINDOWS: &[&str] = &["24h", "7d", "14d", "30d"];
const DEFAULT_WINDOW: &str = "7d";
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;
const RELEASE_MAX_CHARS: usize = 80;
/// Environments named per release before the rest are counted.
const MAX_ENVS_SHOWN: usize = 5;

/// Arguments of `list_releases`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ListReleasesArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// The window the error columns cover: `24h`, `7d` (default), `14d` or
    /// `30d`. The release list itself is not narrowed by it.
    pub window: Option<String>,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for events with no environment. Omit to cover every environment you
    /// can read.
    pub environment: Option<String>,
    /// How many releases to return, newest first: 1 to 50, default 20.
    pub limit: Option<u32>,
}

/// One release, folded across the environments it was seen in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseRow {
    release: String,
    envs: BTreeSet<Option<Uuid>>,
    first_seen_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

/// Fold `app_releases` rows (one per release and environment) into one row
/// per release, `last_seen_at` descending, ties by release.
fn fold_releases(rows: Vec<AppReleaseRow>) -> Vec<ReleaseRow> {
    let mut by: HashMap<String, ReleaseRow> = HashMap::new();
    for r in rows {
        let e = by.entry(r.release.clone()).or_insert_with(|| ReleaseRow {
            release: r.release.clone(),
            envs: BTreeSet::new(),
            first_seen_at: r.first_seen_at,
            last_seen_at: r.last_seen_at,
        });
        e.envs.insert(r.environment_id);
        e.first_seen_at = e.first_seen_at.min(r.first_seen_at);
        e.last_seen_at = e.last_seen_at.max(r.last_seen_at);
    }
    let mut out: Vec<ReleaseRow> = by.into_values().collect();
    out.sort_by(|a, b| {
        b.last_seen_at
            .cmp(&a.last_seen_at)
            .then_with(|| a.release.cmp(&b.release))
    });
    out
}

/// The error columns' inputs, or the reason they are left out.
enum Errors {
    Present {
        by_release: HashMap<String, ReleaseErrors>,
        new_issues: HashMap<String, i64>,
        /// History before this instant is not counted (a partially
        /// backfilled app).
        covered_from: Option<DateTime<Utc>>,
        /// Set when the rollup trails the clock (`rollup_lag_note`).
        lag_note: Option<String>,
    },
    Omitted(String),
}

/// The note when the error columns are omitted: names each failed condition.
fn omitted_note(has_issue_read: bool, ready: bool) -> String {
    let mut reasons = Vec::new();
    if !has_issue_read {
        reasons.push("your role lacks issue:read".to_string());
    }
    if !ready {
        reasons.push(format!(
            "in-window counts need the issue-stats backfill (ask your Sauron admin to run \
             `{BACKFILL_COMMAND}`)"
        ));
    }
    format!(
        "Error columns (events, issues, new issues) omitted: {}.",
        reasons.join("; and ")
    )
}

pub(crate) async fn list_releases(
    state: &AppState,
    user_id: Uuid,
    args: ListReleasesArgs,
) -> Result<CallToolResult, ToolError> {
    within_deadline(state, async {
        let window = parse_window(args.window.as_deref(), WINDOWS, DEFAULT_WINDOW)?;
        let window_label = args
            .window
            .as_deref()
            .map(str::trim)
            .unwrap_or(DEFAULT_WINDOW)
            .to_string();
        let limit = parse_limit(args.limit, DEFAULT_LIMIT, MAX_LIMIT)? as usize;
        let environment = trimmed(&args.environment);

        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        // The dashboard's `/releases` authorizes on `event:read`.
        let (scope, perms) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::EVENT_READ,
            environment.as_deref(),
        )
        .await?;
        let names = env_names(&mut conn, user_id, target.app.id).await?;
        let releases = fold_releases(releases::list_for_app(&mut conn, &scope).await?);

        let now = Utc::now();
        let lag_tolerance =
            rollup_lag_tolerance(state.cfg.rollup_fold_secs, state.cfg.rollup_lag_secs);
        let errors = error_columns(
            &mut conn,
            &scope,
            &perms,
            target.app.id,
            now,
            window,
            lag_tolerance,
        )
        .await?;
        drop(conn);

        // The Issues page scoped to one release: "what did this release
        // break".
        let has_issue_read = perms.contains(perm::ISSUE_READ);
        let release_link = |release: &ReleaseFilter| {
            issues_link_if_readable(has_issue_read, || {
                deep_link(&state.cfg, &target, "/issues", &scope.env, release)
            })
        };
        let text = render(
            &target.reference(),
            &window_label,
            environment.as_deref(),
            &names,
            &releases,
            &errors,
            limit,
            now - window,
            &release_link,
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

/// A link to the Issues page, only for a caller who can open it: the page
/// needs `issue:read`, which `/releases`' `event:read` does not imply.
fn issues_link_if_readable(
    has_issue_read: bool,
    link: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if has_issue_read {
        link()
    } else {
        None
    }
}

async fn error_columns(
    conn: &mut AsyncPgConnection,
    scope: &sauron_db::scope::ReadScope,
    perms: &std::collections::HashSet<String>,
    app_id: Uuid,
    now: DateTime<Utc>,
    window: Duration,
    lag_tolerance: Duration,
) -> Result<Errors, ToolError> {
    let has_issue_read = perms.contains(perm::ISSUE_READ);
    let ready = issue_stats::is_ready(conn, app_id).await?;
    if !has_issue_read || !ready {
        return Ok(Errors::Omitted(omitted_note(has_issue_read, ready)));
    }
    let from = now - window;
    let by_release = issue_stats::release_error_counts(conn, scope, Range::since(from))
        .await?
        .into_iter()
        .map(|r| (r.release.clone(), r))
        .collect();
    // New issues: first seen inside the window, and not before the app's
    // hourly history is complete. `None` floor: complete for every hour.
    let floor = issue_stats::comparison_floor(conn, app_id).await?;
    let bound = floor.map_or(from, |f| f.max(from));
    let new_issues = issue_stats::new_issues_by_release(conn, scope, bound).await?;
    // The counts stop at the fold's watermark.
    let lag_note = rollup_lag_note(
        "Counts",
        rollups::as_of(conn, &[SRC_ISSUE_STATS]).await?,
        now,
        lag_tolerance,
    )
    .map(|n| format!("{n}."));
    Ok(Errors::Present {
        by_release,
        new_issues,
        covered_from: floor.filter(|f| *f > from),
        lag_note,
    })
}

#[allow(clippy::too_many_arguments)]
fn render(
    app_ref: &str,
    window_label: &str,
    environment: Option<&str>,
    names: &HashMap<Uuid, String>,
    releases: &[ReleaseRow],
    errors: &Errors,
    limit: usize,
    window_start: DateTime<Utc>,
    release_link: &dyn Fn(&ReleaseFilter) -> Option<String>,
) -> String {
    let mut head = format!("Releases of {app_ref}, newest seen first");
    match environment {
        Some("none") => head.push_str(", env none (events without an environment)"),
        Some(env) => head.push_str(&format!(", env {}", one_line(env, RELEASE_MAX_CHARS))),
        None => {}
    }
    head.push('.');
    let mut out = vec![head];
    match errors {
        Errors::Omitted(note) => out.push(note.clone()),
        Errors::Present {
            covered_from,
            lag_note,
            ..
        } => {
            let mut line = format!(
                "Error columns cover the last {window_label} (since {}).",
                minute(window_start)
            );
            if let Some(from) = covered_from {
                line.push_str(&format!(
                    " Counts are partial: this app's history is complete only from {}.",
                    day(*from)
                ));
            }
            out.push(line);
            if let Some(note) = lag_note {
                out.push(note.clone());
            }
        }
    }
    out.push(String::new());

    // Events without a release have no `app_releases` row; they are in the
    // rollup under `''`.
    let unknown = match errors {
        Errors::Present { by_release, .. } => by_release.get("").filter(|r| r.events > 0),
        Errors::Omitted(_) => None,
    };
    if releases.is_empty() && unknown.is_none() {
        out.push("No release has been seen yet.".to_string());
        return out.join("\n");
    }
    for r in releases.iter().take(limit) {
        let mut line = release_line(r, names, errors);
        if let Some(link) = release_link(&ReleaseFilter::One(r.release.clone())) {
            line.push_str(&format!(" · {link}"));
        }
        out.push(line);
    }
    if let (Some(u), Errors::Present { new_issues, .. }) = (unknown, errors) {
        let mut line = format!(
            "none (events without a release) · {}",
            error_columns_text(Some(u), new_issues.get("").copied().unwrap_or(0))
        );
        if let Some(link) = release_link(&ReleaseFilter::Unknown) {
            line.push_str(&format!(" · {link}"));
        }
        out.push(line);
    }
    let shown = releases.len().min(limit);
    out.push(String::new());
    if releases.len() > shown {
        out.push(format!(
            "{shown} of {} releases shown; pass a larger limit (up to {MAX_LIMIT}) for more.",
            releases.len()
        ));
    } else {
        out.push(format!(
            "{}; that is all of them.",
            plural(shown as i64, "release", "releases")
        ));
    }
    out.join("\n")
}

/// `{release} · first seen {day} · last seen {day} · envs {names}[ · {errors}]`
/// (the caller appends the link).
fn release_line(r: &ReleaseRow, names: &HashMap<Uuid, String>, errors: &Errors) -> String {
    let mut env_names: Vec<String> = r.envs.iter().map(|e| env_label(names, *e)).collect();
    env_names.sort();
    let extra = env_names.len().saturating_sub(MAX_ENVS_SHOWN);
    env_names.truncate(MAX_ENVS_SHOWN);
    let mut envs = env_names.join(", ");
    if extra > 0 {
        envs.push_str(&format!(", +{extra} more"));
    }
    let mut line = format!(
        "{} · first seen {} · last seen {} · envs {envs}",
        one_line(&r.release, RELEASE_MAX_CHARS),
        day(r.first_seen_at),
        day(r.last_seen_at),
    );
    if let Errors::Present {
        by_release,
        new_issues,
        ..
    } = errors
    {
        line.push_str(" · ");
        line.push_str(&error_columns_text(
            by_release.get(&r.release),
            new_issues.get(&r.release).copied().unwrap_or(0),
        ));
    }
    line
}

/// `{n} events · {n} issues · {n} new issues`; zeros when the release had no
/// events in the window.
fn error_columns_text(counts: Option<&ReleaseErrors>, new: i64) -> String {
    let (events, issues) = counts.map_or((0, 0), |c| (c.events, c.issues));
    format!(
        "{} · {} · {new} new",
        plural(events, "event", "events"),
        plural(issues, "issue", "issues"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().expect("ts")
    }

    fn row(release: &str, env: Option<Uuid>, first: &str, last: &str) -> AppReleaseRow {
        AppReleaseRow {
            release: release.into(),
            environment_id: env,
            first_seen_at: ts(first),
            last_seen_at: ts(last),
        }
    }

    #[test]
    fn rows_fold_per_release_newest_first() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let got = fold_releases(vec![
            row(
                "1.0",
                Some(a),
                "2026-10-01T00:00:00Z",
                "2026-10-05T00:00:00Z",
            ),
            row(
                "2.0",
                Some(a),
                "2026-11-01T00:00:00Z",
                "2026-11-02T00:00:00Z",
            ),
            row(
                "1.0",
                Some(b),
                "2026-09-20T00:00:00Z",
                "2026-11-03T00:00:00Z",
            ),
            row("0.9", None, "2026-08-01T00:00:00Z", "2026-11-03T00:00:00Z"),
        ]);
        let order: Vec<&str> = got.iter().map(|r| r.release.as_str()).collect();
        // 1.0 and 0.9 tie on last seen; the release name breaks the tie.
        assert_eq!(order, vec!["0.9", "1.0", "2.0"]);
        let one = &got[1];
        assert_eq!(one.first_seen_at, ts("2026-09-20T00:00:00Z"));
        assert_eq!(one.last_seen_at, ts("2026-11-03T00:00:00Z"));
        assert_eq!(one.envs.len(), 2);
    }

    #[test]
    fn the_omission_note_names_each_failed_condition() {
        let both = omitted_note(false, false);
        assert!(
            both.contains("issue:read") && both.contains("backfill-issue-stats"),
            "{both}"
        );
        let perm_only = omitted_note(false, true);
        assert!(
            perm_only.contains("issue:read") && !perm_only.contains("backfill"),
            "{perm_only}"
        );
        let ready_only = omitted_note(true, false);
        assert!(
            !ready_only.contains("issue:read") && ready_only.contains("backfill"),
            "{ready_only}"
        );
    }

    /// Each release line ends with its Issues-page link when links are on
    /// (the events-without-a-release row with `release=none`); a lagging
    /// rollup adds its "as of" line to the window line.
    #[test]
    fn release_lines_link_to_their_issues_and_a_lagging_rollup_says_so() {
        let releases = fold_releases(vec![
            row("2.0", None, "2026-11-01T00:00:00Z", "2026-11-09T00:00:00Z"),
            row("1.0", None, "2026-10-01T00:00:00Z", "2026-11-02T00:00:00Z"),
        ]);
        let by_release: HashMap<String, ReleaseErrors> = [("2.0", 3, 2), ("", 4, 1)]
            .into_iter()
            .map(|(r, events, issues)| {
                (
                    r.to_string(),
                    ReleaseErrors {
                        release: r.into(),
                        events,
                        issues,
                    },
                )
            })
            .collect();
        let errors = Errors::Present {
            by_release,
            new_issues: HashMap::from([("2.0".to_string(), 1)]),
            covered_from: None,
            lag_note: Some(
                "Counts as of 2026-11-10 11:00 UTC (the rollup is behind; newer events are not \
                 counted yet)."
                    .into(),
            ),
        };
        let link = |r: &ReleaseFilter| {
            Some(match r {
                ReleaseFilter::One(v) => format!("https://s.example/#/issues?release={v}"),
                ReleaseFilter::Unknown => "https://s.example/#/issues?release=none".into(),
                ReleaseFilter::All => "https://s.example/#/issues".into(),
            })
        };
        let text = render(
            "acme/mobile/ios",
            "7d",
            None,
            &HashMap::new(),
            &releases,
            &errors,
            20,
            ts("2026-11-03T12:00:00Z"),
            &link,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1], "Error columns cover the last 7d (since 2026-11-03 12:00 UTC).",
            "{text}"
        );
        assert_eq!(
            lines[2],
            "Counts as of 2026-11-10 11:00 UTC (the rollup is behind; newer events are not \
             counted yet).",
            "{text}"
        );
        assert!(
            lines[4].starts_with("2.0 · ")
                && lines[4].ends_with(
                    " · 3 events · 2 issues · 1 new · https://s.example/#/issues?release=2.0"
                ),
            "{text}"
        );
        assert!(
            lines[5].ends_with(" · 0 new · https://s.example/#/issues?release=1.0"),
            "{text}"
        );
        assert!(
            lines[6].starts_with("none (events without a release) · 4 events")
                && lines[6].ends_with("https://s.example/#/issues?release=none"),
            "{text}"
        );

        // DASHBOARD_URL unset: no links, nothing else changes.
        let no_link = |_: &ReleaseFilter| None;
        let plain = render(
            "acme/mobile/ios",
            "7d",
            None,
            &HashMap::new(),
            &releases,
            &errors,
            20,
            ts("2026-11-03T12:00:00Z"),
            &no_link,
        );
        assert!(!plain.contains("https://"), "{plain}");
        assert!(plain.contains("2.0 · first seen 2026-11-01"), "{plain}");
    }

    #[test]
    fn issues_links_only_for_a_caller_with_issue_read() {
        let built = std::cell::Cell::new(0);
        let link = || {
            built.set(built.get() + 1);
            Some("https://s.example/#/issues".to_string())
        };
        assert_eq!(issues_link_if_readable(false, link), None);
        assert_eq!(built.get(), 0, "no link is even built without issue:read");
        assert_eq!(
            issues_link_if_readable(true, link).as_deref(),
            Some("https://s.example/#/issues")
        );
        // DASHBOARD_URL unset stays unset.
        assert_eq!(issues_link_if_readable(true, || None), None);
    }

    #[test]
    fn error_columns_read_zero_for_a_quiet_release() {
        assert_eq!(error_columns_text(None, 0), "0 events · 0 issues · 0 new");
        let c = ReleaseErrors {
            release: "1.0".into(),
            events: 1,
            issues: 1,
        };
        assert_eq!(error_columns_text(Some(&c), 1), "1 event · 1 issue · 1 new");
    }
}
