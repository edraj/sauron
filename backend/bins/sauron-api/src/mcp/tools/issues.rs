//! The issue tools: `trending_issues` (what is blowing up right now),
//! `get_issue` (one issue and an event of it in full) and `list_issue_events`
//! (an issue's occurrences, with a breakdown). The last two are described
//! at their section below; this header is about `trending_issues`.
//!
//! `trending_issues`: what is blowing up in an app right now.
//!
//! The dashboard's issue list ranks by lifetime totals, which answers "what
//! has ever been loud", not "what is spiking". This tool ranks by events
//! INSIDE the window, read from the `issue_stats_hourly` rollup (migration
//! 84), and compares each issue with the previous window of the same length.
//!
//! - **Fallback.** An app the rollup does not cover yet (created before its
//!   epoch, no `sauron-migrate backfill-issue-stats` marker) gets the Issues
//!   list's own ranking instead, said on the first line of the output. Never
//!   a silently wrong in-window ranking.
//! - **Users** are counted on read from raw `error_events` for the issues
//!   shown (or, sorting by users, for the 200 with the most events), under a
//!   5 s statement timeout. The rollup holds no users sketch by design.
//! - **`query`** selects candidate issues through the dashboard's own Issues
//!   search pipeline (`routes::issues::issue_ids_matching`); the ranking then
//!   runs over those.
//!
//! The patterns here are the ones the other issue tools follow: an argument
//! struct whose doc comments become the schema's descriptions, a body under
//! `within_deadline`, `resolve_app` then `read_scope`, `deep_link` per row,
//! and SDK-reported strings through `one_line`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, DurationRound, Utc};
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::models::{ErrorEvent, Issue};
use sauron_db::repo::{self, IssueSort};
use sauron_db::rollups::issue_stats::{self, RankedIssue};
use sauron_db::rollups::{self, SRC_ISSUE_STATS};
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use sauron_db::AsyncPgConnection;
use sauron_query::ReleaseFilter;

use crate::error::ApiError;
use crate::mcp::access::{
    deep_link, read_scope, release_arg, release_key_opt, resolve_app, within_deadline,
    within_deadline_saying, AppTarget, ToolError, TIMED_OUT_RETRY,
};
use crate::mcp::format::{
    below_tier_floor, clip, day, device_line, device_model, env_label, env_names, hot_store_note,
    minute, name_version, one_line, os_label, parse_limit, parse_window, plural, release_label,
    render_breadcrumbs, render_kv, render_stacktrace, rollup_lag_note, rollup_lag_tolerance,
    second, trimmed, BACKFILL_COMMAND, ECHO_MAX_CHARS, MAX_BREADCRUMBS, MAX_FRAMES, MAX_KV_ENTRIES,
    MAX_KV_VALUE_CHARS, VALUE_MAX_CHARS,
};
use crate::routes::issues::{
    finish_latest_event, issue_ids_matching, load_issue_scoped, search_issue_events,
    tool_issue_search, EventsParams, ToolIssueSearch, ISSUE_CANDIDATE_CAP,
};
use crate::routes::search::SearchEnvelope;
use crate::AppState;

/// The windows `trending_issues` offers. Hourly buckets make `1h` exact.
pub(crate) const TRENDING_WINDOWS: &[&str] = &["1h", "24h", "7d", "14d", "30d"];
const DEFAULT_WINDOW: &str = "24h";
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;
/// How many of the most-events issues `sort: users` counts users for and
/// re-ranks.
const USERS_POOL: i64 = 200;
/// Statement timeout for the users count.
const USERS_TIMEOUT_MS: u32 = 5000;
/// The `status` values, in the order the error message lists them.
const STATUSES: &[&str] = &["unresolved", "resolved", "ignored", "all"];
const TITLE_MAX_CHARS: usize = 200;
const CULPRIT_MAX_CHARS: usize = 200;
/// Trend thresholds: at or beyond these ratios a change is called up or down.
const TREND_UP: f64 = 1.2;
const TREND_DOWN: f64 = 0.8;

/// Arguments of `trending_issues`. Each field's doc comment is its
/// description in the tool's input schema, which is what Claude reads.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TrendingArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// Time window: `1h`, `24h` (default), `7d`, `14d` or `30d`. It covers the
    /// last N whole hours plus the current partial hour, and trends compare its
    /// event rate with the rate in the N hours before.
    pub window: Option<String>,
    /// `events` (default) ranks by events in the window. `users` re-ranks the
    /// 200 issues with the most events by distinct users affected.
    pub sort: Option<String>,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for events with no environment. Omit to cover every environment you
    /// can read.
    pub environment: Option<String>,
    /// A release as the SDK reported it, or `none` for events with no release.
    /// Omit to cover every release.
    pub release: Option<String>,
    /// Issue status: `unresolved` (default), `resolved`, `ignored` or `all`.
    pub status: Option<String>,
    /// Narrow which issues are ranked with a search in Sauron's query
    /// language, the one the dashboard's Issues search uses: for example
    /// `level:fatal`, `type:TypeError`, `culprit:*checkout*`.
    pub query: Option<String>,
    /// How many issues to return: 1 to 50, default 20.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Events,
    Users,
}

impl SortBy {
    fn parse(raw: Option<&str>) -> Result<Self, ToolError> {
        let value = raw.map(|s| s.trim().to_ascii_lowercase());
        match value.as_deref() {
            None | Some("events") => Ok(SortBy::Events),
            Some("users") => Ok(SortBy::Users),
            Some(other) => Err(ToolError::BadArgument(format!(
                "sort must be one of: events, users (got {:?})",
                clip(other, ECHO_MAX_CHARS)
            ))),
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortBy::Events => "events",
            SortBy::Users => "users",
        }
    }
}

/// The `status` argument as the filter the reads take: `None` for `all`.
fn parse_status(raw: Option<&str>) -> Result<Option<&'static str>, ToolError> {
    let value = raw.map(|s| s.trim().to_ascii_lowercase());
    match value.as_deref().unwrap_or("unresolved") {
        "all" => Ok(None),
        "unresolved" => Ok(Some("unresolved")),
        "resolved" => Ok(Some("resolved")),
        "ignored" => Ok(Some("ignored")),
        other => Err(ToolError::BadArgument(format!(
            "status must be one of: {} (got {:?})",
            STATUSES.join(", "),
            clip(other, ECHO_MAX_CHARS)
        ))),
    }
}

/// The arguments, validated. Nothing here touches the database, so a bad
/// argument is answered before any lookup.
struct Request {
    /// As the caller wrote it (trimmed), or the default: `24h`.
    window_label: String,
    window: Window,
    sort: SortBy,
    status: Option<&'static str>,
    release: ReleaseFilter,
    query: Option<String>,
    limit: u32,
    /// The trimmed `environment` argument, for the header.
    environment: Option<String>,
}

impl Request {
    fn parse(args: &TrendingArgs, now: DateTime<Utc>) -> Result<Self, ToolError> {
        let len = parse_window(args.window.as_deref(), TRENDING_WINDOWS, DEFAULT_WINDOW)?;
        let window_label = args
            .window
            .as_deref()
            .map(str::trim)
            .unwrap_or(DEFAULT_WINDOW)
            .to_string();
        Ok(Request {
            window_label,
            window: Window::ending_at(now, len),
            sort: SortBy::parse(args.sort.as_deref())?,
            status: parse_status(args.status.as_deref())?,
            release: release_arg(args.release.as_deref())?,
            query: args
                .query
                .as_deref()
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_string),
            limit: parse_limit(args.limit, DEFAULT_LIMIT, MAX_LIMIT)?,
            environment: args
                .environment
                .as_deref()
                .map(str::trim)
                .map(str::to_string),
        })
    }

    /// `limit` as a row count.
    fn max_rows(&self) -> usize {
        self.limit as usize
    }
}

/// The trending window and the one its trend compares against, on hour
/// boundaries, so every read takes whole hourly buckets and the two never
/// share one.
///
/// What is SHOWN ([`Window::current`]) is the last `len` of whole hours plus
/// the current partial hour, so it spans [`Window::shown_span`] (`len` plus
/// however far into the hour `now` is), while [`Window::previous`] spans
/// exactly `len`. The trend therefore compares RATES, events per second over
/// each span (ruling R21b): comparing the raw counts would make a flat-rate
/// issue read as rising at any minute past the hour, and dropping the
/// partial hour instead would make an issue that starts spiking right now
/// read as falling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    /// The first bucket read: the hour holding `now - len`.
    start: DateTime<Utc>,
    len: Duration,
    now: DateTime<Utc>,
}

impl Window {
    fn ending_at(now: DateTime<Utc>, len: Duration) -> Self {
        let from = now - len;
        Window {
            start: from.duration_trunc(Duration::hours(1)).unwrap_or(from),
            len,
            now,
        }
    }

    /// The last `len` of whole hours plus the current partial hour: the
    /// events and users shown, and the trend's numerator.
    fn current(&self) -> Range {
        Range::since(self.start)
    }

    /// How long [`Window::current`] has been running at `now`: `len` plus
    /// the elapsed part of the current hour, to the millisecond.
    fn shown_span(&self) -> Duration {
        self.now - self.start
    }

    /// The `len` of whole hours before [`Window::current`], ending where it
    /// starts. The trend's denominator, and the "N in the previous …" figure.
    fn previous(&self) -> Range {
        Range::new(self.start - self.len, Some(self.start))
    }
}

/// How an issue's event RATE over the shown span compares with its rate
/// over the previous window: `(current / current_span) / (previous /
/// previous_span)`, with the spans in seconds (never below one, so a span
/// cannot divide by zero). No previous events is "new in window".
fn trend(current: i64, current_span: Duration, previous: i64, previous_span: Duration) -> String {
    if previous <= 0 {
        return "new in window".into();
    }
    let secs = |d: Duration| (d.num_milliseconds() as f64 / 1000.0).max(1.0);
    let ratio = (current as f64 / secs(current_span)) / (previous as f64 / secs(previous_span));
    if ratio >= TREND_UP {
        format!("↑{ratio:.1}×")
    } else if ratio < 0.1 {
        // `{:.1}` would print a fall from 100 to 1 as "↓0.0×".
        "↓<0.1×".into()
    } else if ratio <= TREND_DOWN {
        format!("↓{ratio:.1}×")
    } else {
        "≈".into()
    }
}

/// `10000` as `10,000`, for the limits quoted in prose.
pub(crate) fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// A span as `1h50m`, `24h37m`, or `7d0h37m` from two days up: what a shown
/// count covers.
fn span_label(d: Duration) -> String {
    let mins = d.num_minutes().max(0);
    let (days, hours, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    if days >= 2 {
        format!("{days}d{hours}h{m}m")
    } else {
        format!("{}h{m}m", mins / 60)
    }
}

pub(crate) async fn trending_issues(
    state: &AppState,
    user_id: Uuid,
    args: TrendingArgs,
) -> Result<CallToolResult, ToolError> {
    within_deadline(state, async {
        let req = Request::parse(&args, Utc::now())?;
        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        let (scope, perms) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::ISSUE_READ,
            req.environment.as_deref(),
        )
        .await?;
        let view = View {
            state,
            target: &target,
            scope: &scope,
            req: &req,
        };
        let text = if issue_stats::is_ready(&mut conn, target.app.id).await? {
            in_window(&mut conn, &view, &perms).await?
        } else {
            fallback(&mut conn, &view, &perms).await?
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

/// What every part of the rendering needs.
struct View<'a> {
    state: &'a AppState,
    target: &'a AppTarget,
    scope: &'a ReadScope,
    req: &'a Request,
}

impl View<'_> {
    /// `Trending issues in {org/project/app}, last {window}[, env {name}][,
    /// release {r}], by {events|users}`.
    fn header(&self) -> String {
        let mut h = format!(
            "Trending issues in {}, last {}",
            self.target.reference(),
            self.req.window_label
        );
        match self.req.environment.as_deref() {
            Some("none") => h.push_str(", env none (events without an environment)"),
            Some(env) => h.push_str(&format!(", env {}", one_line(env, ECHO_MAX_CHARS))),
            None => {}
        }
        match &self.req.release {
            ReleaseFilter::One(r) => {
                h.push_str(&format!(", release {}", one_line(r, ECHO_MAX_CHARS)))
            }
            ReleaseFilter::Unknown => h.push_str(", release none (events without a release)"),
            ReleaseFilter::All => {}
        }
        h.push_str(&format!(", by {}", self.req.sort.label()));
        h
    }

    /// What was asked, in words: the window's exact start, status and query.
    /// The trend clause only when a trend is actually shown (ruling R22b):
    /// the fallback and a "trend n/a" ranking show none, so saying what
    /// trends compare would describe something that is not there.
    fn scope_line(&self, trend_shown: bool) -> String {
        let status = match self.req.status {
            Some(s) => format!("status {s}"),
            None => "every status".into(),
        };
        let mut line = format!(
            "Window: since {} (hourly buckets, through now)",
            minute(self.req.window.start)
        );
        if trend_shown {
            line.push_str(&format!(
                "; trends compare the event rate since then with the rate in the {} before",
                self.req.window_label
            ));
        }
        line.push_str(&format!("; {status}"));
        if let Some(q) = &self.req.query {
            line.push_str(&format!("; query {:?}", one_line(q, ECHO_MAX_CHARS)));
        }
        if let EnvFilter::Subset(_) = self.scope.env {
            line.push_str("; covering the environments you can read");
        }
        line.push('.');
        line
    }

    /// How many rows, whether more exist, and how to get them.
    fn count_line(&self, shown: usize, has_more: bool) -> String {
        if shown == 0 {
            return "No issues match in this window.".into();
        }
        let mut line = format!("{shown} issue(s), most {} first.", self.req.sort.label());
        if !has_more {
            line.push_str(" That is all of them.");
        } else if self.req.limit < MAX_LIMIT {
            line.push_str(&format!(
                " More exist: pass a larger limit (up to {MAX_LIMIT}), or narrow with query, \
                 environment or release."
            ));
        } else {
            line.push_str(" More exist: narrow with query, environment or release.");
        }
        line
    }

    fn issue_link(&self, issue_id: Uuid) -> Option<String> {
        deep_link(
            &self.state.cfg,
            self.target,
            &format!("/issues/{issue_id}"),
            &self.scope.env,
            &self.req.release,
        )
    }

    /// `#{n} {title}: {culprit}` (no `: ` when the culprit is blank).
    fn title_line(n: usize, issue: &Issue) -> String {
        let title = one_line(&issue.title, TITLE_MAX_CHARS);
        let culprit = one_line(issue.culprit.trim(), CULPRIT_MAX_CHARS);
        if culprit.is_empty() {
            format!("#{n} {title}")
        } else {
            format!("#{n} {title}: {culprit}")
        }
    }

    /// The second line of a row: `id … · {level} · {counts…} · first seen
    /// {date}[ · NEW] · last seen … · {link}`.
    fn detail_line(&self, issue: &Issue, counts: Vec<String>) -> String {
        let mut parts = vec![
            format!("id {}", issue.id),
            one_line(&issue.level, ECHO_MAX_CHARS),
        ];
        parts.extend(counts);
        parts.push(format!("first seen {}", day(issue.first_seen)));
        // The issue's app-wide first sighting is inside the window.
        if issue.first_seen >= self.req.window.start {
            parts.push("NEW".into());
        }
        parts.push(format!("last seen {}", minute(issue.last_seen)));
        if let Some(link) = self.issue_link(issue.id) {
            parts.push(link);
        }
        format!("   {}", parts.join(" · "))
    }

    /// The Issues page itself, under the same scope.
    fn page_link(&self) -> Option<String> {
        deep_link(
            &self.state.cfg,
            self.target,
            "/issues",
            &self.scope.env,
            &self.req.release,
        )
    }
}

/// A query's parser message, or the permission it needs, is the caller's to
/// fix: say so with the query echoed. Everything else converts as usual; a
/// pipeline statement timeout already arrives as `query_timeout`
/// (`routes::search::map_plan_error`) and becomes `access::TIMED_OUT`.
fn search_error(query: Option<&str>, e: ApiError) -> ToolError {
    match (query, e) {
        (Some(q), ApiError::BadRequest(m) | ApiError::Forbidden(m)) => ToolError::BadArgument(
            format!("query {:?} cannot be used: {m}", clip(q, ECHO_MAX_CHARS)),
        ),
        (_, e) => e.into(),
    }
}

/// The ranking from `issue_stats_hourly`: steps 3 to 7 of the tool.
async fn in_window(
    conn: &mut AsyncPgConnection,
    v: &View<'_>,
    perms: &HashSet<String>,
) -> Result<String, ToolError> {
    let req = v.req;
    let release = release_key_opt(&req.release);
    let release = release.as_deref();
    let current = req.window.current();
    let mut notes: Vec<String> = Vec::new();

    let candidates = match &req.query {
        Some(q) => {
            let (ids, capped) = issue_ids_matching(
                conn,
                v.scope,
                perms,
                q,
                req.status,
                &req.release,
                req.window.start,
            )
            .await
            .map_err(|e| search_error(Some(q), e))?;
            if capped {
                let cap = grouped(ISSUE_CANDIDATE_CAP);
                notes.push(format!(
                    "the query matched more than {cap} issues (capped): only the {cap} seen \
                     most recently were ranked"
                ));
            }
            Some(ids)
        }
        None => None,
    };

    let pool_size = match req.sort {
        // One more than shown: the has-more probe.
        SortBy::Events => i64::from(req.limit) + 1,
        SortBy::Users => USERS_POOL,
    };
    let pool = issue_stats::ranked(
        conn,
        v.scope,
        release,
        current,
        req.status,
        candidates.as_deref(),
        pool_size,
    )
    .await?;

    let (rows, has_more, users) = match req.sort {
        SortBy::Events => {
            let has_more = pool.len() > req.max_rows();
            let mut rows = pool;
            rows.truncate(req.max_rows());
            let ids: Vec<Uuid> = rows.iter().map(|r| r.issue_id).collect();
            let users = repo::issue_users_in_window(
                conn,
                v.scope,
                release,
                &ids,
                current,
                USERS_TIMEOUT_MS,
            )
            .await?;
            if users.is_none() {
                notes.push(
                    "users could not be counted in time and show as ?; narrow the window or add \
                     an environment"
                        .into(),
                );
            }
            (rows, has_more, users)
        }
        SortBy::Users => {
            let ids: Vec<Uuid> = pool.iter().map(|r| r.issue_id).collect();
            let users = repo::issue_users_in_window(
                conn,
                v.scope,
                release,
                &ids,
                current,
                USERS_TIMEOUT_MS,
            )
            .await?
            .ok_or_else(|| {
                ToolError::TimedOut(
                    "counting users took too long; sort by events or narrow the window".into(),
                )
            })?;
            if pool.len() as i64 >= USERS_POOL {
                notes.push(format!(
                    "ranked by users among the {USERS_POOL} issues with the most events"
                ));
            } else if !pool.is_empty() {
                notes.push(format!(
                    "ranked by users among all {} issue(s) with events in the window",
                    pool.len()
                ));
            }
            let mut rows = rerank_by_users(pool, &users);
            let has_more = rows.len() > req.max_rows();
            rows.truncate(req.max_rows());
            (rows, has_more, Some(users))
        }
    };
    let ids: Vec<Uuid> = rows.iter().map(|r| r.issue_id).collect();

    // The trend: the shown events' rate against the previous window's
    // (`Window` says why rates), and only where the rollup holds the previous
    // window completely. A count over hours it never saw would read as a
    // rise. `None` from `comparison_floor` is "complete for every hour",
    // which an app created after the epoch is (ruling R19).
    let floor = issue_stats::comparison_floor(conn, v.target.app.id).await?;
    let previous_range = req.window.previous();
    let trend_counts = match floor {
        Some(f) if f > previous_range.from => {
            if !rows.is_empty() {
                notes.push(format!(
                    "previous window predates issue-stats coverage (coverage starts {}), so no \
                     trend is shown",
                    minute(f)
                ));
            }
            None
        }
        _ => Some(issue_stats::events_for(conn, v.scope, release, previous_range, &ids).await?),
    };
    // Coverage starting inside the window itself: what is shown is partial.
    if let Some(f) = floor.filter(|f| *f > req.window.start) {
        notes.push(format!(
            "issue-stats coverage starts {}, inside this window: events before it are not \
             counted, so these counts are partial",
            minute(f)
        ));
    }
    // The event counts stop at the fold's watermark. Users are counted from
    // raw rows with no watermark bound, so the note names events only.
    notes.extend(rollup_lag_note(
        "Event counts",
        rollups::as_of(conn, &[SRC_ISSUE_STATS]).await?,
        req.window.now,
        rollup_lag_tolerance(v.state.cfg.rollup_fold_secs, v.state.cfg.rollup_lag_secs),
    ));
    // Users (and, under an environment filter, the descriptions) are read
    // from raw events, which the tier moves out of Postgres.
    if !rows.is_empty() {
        notes.extend(trending_tier_note(
            req.window.start,
            issue_stats::error_events_floor(conn).await?,
            true,
            !matches!(v.scope.env, EnvFilter::All),
        ));
    }

    // Descriptions. Under an environment filter, title, culprit, level and
    // last seen are that environment's own, derived from its events the way
    // the dashboard's list derives them (ruling R20); `first_seen` stays
    // app-wide, because NEW means "first seen anywhere inside the window".
    let mut issues: HashMap<Uuid, Issue> = repo::issues_by_ids(conn, v.target.app.id, &ids)
        .await?
        .into_iter()
        .map(|i| (i.id, i))
        .collect();
    let env_stats = repo::issue_env_stats(conn, v.scope, &ids, req.window.start)
        .await
        .map_err(|e| search_error(None, crate::routes::search::map_plan_error(e)))?;
    for (id, stats) in &env_stats {
        if let Some(issue) = issues.get_mut(id) {
            describe_from_env(issue, stats);
        }
    }

    let mut out = vec![
        v.header(),
        v.scope_line(trend_counts.is_some() && !rows.is_empty()),
        v.count_line(rows.len(), has_more),
    ];
    // The span the shown counts cover, next to each count (ruling R22c): a
    // raw "N in the previous 24h" beside a RATE trend would otherwise read
    // as the two counts the trend divides.
    let span = span_label(req.window.shown_span());
    let mut n = 0;
    for row in &rows {
        // Deleted between the two reads: nothing left to describe.
        let Some(issue) = issues.get(&row.issue_id) else {
            continue;
        };
        n += 1;
        let mut counts = Vec::new();
        let events = format!("{} over {span}", plural(row.events, "event", "events"));
        match &trend_counts {
            Some(previous) => {
                let before = previous.get(&row.issue_id).copied().unwrap_or(0);
                let label = trend(row.events, req.window.shown_span(), before, req.window.len);
                counts.push(format!("{events} ({label})"));
                if before > 0 {
                    counts.push(format!("{before} in the previous {}", req.window_label));
                }
            }
            None => counts.push(format!("{events} (trend n/a)")),
        }
        counts.push(
            match users
                .as_ref()
                .map(|u| u.get(&row.issue_id).copied().unwrap_or(0))
            {
                Some(u) => plural(u, "user", "users"),
                None => "? users".into(),
            },
        );
        out.push(String::new());
        out.push(View::title_line(n, issue));
        out.push(v.detail_line(issue, counts));
    }
    finish(v, out, &notes)
}

/// Most users first, then most events, then id: a total order.
fn rerank_by_users(mut pool: Vec<RankedIssue>, users: &HashMap<Uuid, i64>) -> Vec<RankedIssue> {
    let users_of = |r: &RankedIssue| users.get(&r.issue_id).copied().unwrap_or(0);
    pool.sort_by(|a, b| {
        users_of(b)
            .cmp(&users_of(a))
            .then(b.events.cmp(&a.events))
            .then(a.issue_id.cmp(&b.issue_id))
    });
    pool
}

/// An issue's description as one environment sees it: the title, culprit
/// and level of its latest event there, and its last sighting there. Only
/// what the environment's events actually carry is replaced (a legacy
/// occurrence has no title or culprit), and never `first_seen` or the
/// counts: the tool shows its own counts, and NEW is app-wide.
fn describe_from_env(issue: &mut Issue, stats: &repo::IssueEnvStats) {
    issue.last_seen = stats.last_seen;
    if let Some(t) = &stats.title {
        issue.title = t.clone();
    }
    if let Some(c) = &stats.culprit {
        issue.culprit = c.clone();
    }
    if let Some(l) = &stats.level {
        issue.level = l.clone();
    }
}

/// Notes, then a link to the dashboard's Issues page under the same scope
/// (the one link the output carries when no row does).
fn finish(v: &View<'_>, mut out: Vec<String>, notes: &[String]) -> Result<String, ToolError> {
    if !notes.is_empty() {
        out.push(String::new());
        out.push("Notes:".into());
        out.extend(notes.iter().map(|n| format!("- {n}")));
    }
    if let Some(link) = v.page_link() {
        out.push(String::new());
        out.push(format!("Issues page: {link}"));
    }
    Ok(out.join("\n"))
}

/// Before the backfill: the Issues list's own ranking over the window,
/// labelled on the first line. Under no environment filter that is the
/// all-time count among issues seen in the window; under one it is the
/// environment's count in the window, derived from raw events over the
/// 10,000 issues seen most recently (the list's `ENV_RANK_CANDIDATES`).
/// No trend either way: there is nothing to compare against.
async fn fallback(
    conn: &mut AsyncPgConnection,
    v: &View<'_>,
    perms: &HashSet<String>,
) -> Result<String, ToolError> {
    let req = v.req;
    let env_scoped = !matches!(v.scope.env, EnvFilter::All);
    let search = ToolIssueSearch {
        query: req.query.as_deref(),
        status: req.status,
        release: &req.release,
        since: req.window.start,
        sort: match req.sort {
            SortBy::Events => IssueSort::TimesSeen,
            SortBy::Users => IssueSort::UsersSeen,
        },
        limit: i64::from(req.limit),
    };
    let mut rows = tool_issue_search(conn, v.scope, perms, &search)
        .await
        .map_err(|e| search_error(req.query.as_deref(), e))?;
    let has_more = rows.len() > req.max_rows();
    rows.truncate(req.max_rows());

    // Under an environment scope the rows already carry that environment's
    // values (`search_issues_by_env_count` applied them): its title, culprit,
    // level and last seen, which is what the dashboard shows there (ruling
    // R20). Their `first_seen` is the in-window minimum, though, which would
    // flag every row NEW, so only `first_seen` comes from the stored,
    // app-wide row.
    let app_wide: HashMap<Uuid, Issue> = if env_scoped {
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        repo::issues_by_ids(conn, v.target.app.id, &ids)
            .await?
            .into_iter()
            .map(|i| (i.id, i))
            .collect()
    } else {
        HashMap::new()
    };

    let w = &req.window_label;
    let label = if env_scoped {
        format!(
            "Fallback ranking: in-window issue counts are not available for this app yet, so \
             these are ranked by their count in the last {w} in your environment selection, \
             counted from raw events among the {} issues seen most recently, with no trend; the \
             full in-window ranking needs the issue-stats backfill (ask your Sauron admin to \
             run `{BACKFILL_COMMAND}`).",
            grouped(repo::ENV_RANK_CANDIDATES)
        )
    } else {
        format!(
            "Fallback ranking: in-window issue counts are not available for this app yet, so \
             issues seen in the last {w} are ranked by all-time count (the users figure is a \
             stored estimate that can lag), with no trend; in-window ranking needs the \
             issue-stats backfill (ask your Sauron admin to run `{BACKFILL_COMMAND}`)."
        )
    };

    let mut out = vec![
        label,
        v.header(),
        v.scope_line(false),
        v.count_line(rows.len(), has_more),
    ];
    for (i, row) in rows.iter_mut().enumerate() {
        if let Some(stored) = app_wide.get(&row.id) {
            row.first_seen = stored.first_seen;
        }
        let counts = fallback_counts(env_scoped, row.times_seen, row.users_seen);
        out.push(String::new());
        out.push(View::title_line(i + 1, row));
        out.push(v.detail_line(row, counts));
    }
    // The env-scoped ranking counts raw events: hot ones only.
    let notes: Vec<String> = if env_scoped && !rows.is_empty() {
        trending_tier_note(
            req.window.start,
            issue_stats::error_events_floor(conn).await?,
            false,
            env_scoped,
        )
        .into_iter()
        .collect()
    } else {
        Vec::new()
    };
    finish(v, out, &notes)
}

/// What a trending answer reads from raw `error_events`, which hold only the
/// events still in the hot store: users, always, on the in-window path;
/// under an environment filter also each issue's title, culprit, level and
/// last seen there (ruling R20); and on the env-scoped fallback the counts
/// too. `None` for the unscoped fallback, which reads stored issue rows.
fn trending_raw_parts(in_window: bool, env_scoped: bool) -> Option<&'static str> {
    match (in_window, env_scoped) {
        (true, false) => Some("users"),
        (true, true) => {
            Some("users and the environment's titles, culprits, levels and last-seen times")
        }
        (false, true) => {
            Some("events, users and the environment's titles, culprits, levels and last-seen times")
        }
        (false, false) => None,
    }
}

/// The note for a trending window that starts below the `error_events` tier
/// floor, naming what is read from hot events only.
fn trending_tier_note(
    window_start: DateTime<Utc>,
    floor: Option<DateTime<Utc>>,
    in_window: bool,
    env_scoped: bool,
) -> Option<String> {
    let parts = trending_raw_parts(in_window, env_scoped)?;
    below_tier_floor(window_start, floor).map(|f| hot_store_note(parts, f))
}

/// A fallback row's counts. Under an environment filter both are counted
/// exactly from raw events in the window; under none they are the issue
/// row's stored all-time counters, whose users figure is a HyperLogLog
/// estimate that ingest writes back on a throttle and can lag.
fn fallback_counts(env_scoped: bool, events: i64, users: i64) -> Vec<String> {
    if env_scoped {
        vec![
            format!("{} in window", plural(events, "event", "events")),
            format!("{} in window", plural(users, "user", "users")),
        ]
    } else {
        vec![
            format!("{} all-time", plural(events, "event", "events")),
            format!(
                "~{} all-time (stored estimate; can lag)",
                plural(users, "user", "users")
            ),
        ]
    }
}

// --- get_issue and list_issue_events -----------------------------------------
//
// Where a developer's Claude reads a crash and checks it against local source,
// so the body gates are the point. Neither tool gates anything itself: the
// event bodies come through the dashboard's own read paths
// (`routes::issues::finish_latest_event` for one event,
// `routes::issues::search_issue_events` for a page), which symbolicate, strip
// source lines without `source:read`, then `gate_event_body`. The tools only
// render what is left, and say what was withheld from the same permission
// set those gates read.

/// The windows `list_issue_events` offers.
pub(crate) const EVENTS_WINDOWS: &[&str] = &["24h", "7d", "14d", "30d"];
const EVENTS_DEFAULT_WINDOW: &str = "7d";
const EVENTS_DEFAULT_LIMIT: u32 = 10;
const EVENTS_MAX_LIMIT: u32 = 50;

/// Said, verbatim, when the caller's role may not read event bodies.
pub(crate) const EVENT_BODY_WITHHELD: &str =
    "Event details withheld: your role lacks event:read (stack trace, breadcrumbs, tags, user).";
/// Said, verbatim, when the caller may read bodies but not source lines.
pub(crate) const SOURCE_LINES_WITHHELD: &str =
    "Source lines withheld: your role lacks source:read.";
/// Said when the issue has no occurrence left in Postgres.
pub(crate) const NO_HOT_EVENT: &str =
    "No event body in the hot store (older events are archived or were purged).";
/// `list_issue_events`' counterpart of [`EVENT_BODY_WITHHELD`]: the parts of
/// its output that come from the event body.
pub(crate) const EVENTS_OS_USER_WITHHELD: &str =
    "OS, device and user withheld: your role lacks event:read.";

/// What `get_issue`'s counts are under no environment filter: the issue
/// row's stored counters. `users_seen` there is a HyperLogLog estimate whose
/// Postgres copy ingest writes back on a throttle, so it can lag, by about
/// one write-back interval (seconds, see `sauron_pipeline::users_seen`) once
/// `sauron-migrate backfill-users-seen` has repaired rows left stale by older
/// releases; `trending_issues` counts users exactly, per window, on read.
const STORED_COUNTS_NOTE: &str = "(all-time; the users figure is an estimate that \
                                             can lag — trending_issues counts users exactly per \
                                             window)";
/// Under an environment filter the same line is derived on read
/// (`repo::get_issue`'s env path): exact `count(*)` / `count(DISTINCT
/// distinct_id)` and min/max `occurred_at` over the rows still in Postgres.
const ENV_COUNTS_NOTE: &str =
    "events and users are exact all-time counts of the events still in the hot store; archived \
     events are not included";

/// `{events} · {users} … · first seen {t} · last seen {t}`, labelled with
/// what the numbers are, which depends on the environment filter.
fn issue_counts_and_seen(issue: &Issue, env: &EnvFilter) -> String {
    let events = plural(issue.times_seen, "event", "events");
    let users = plural(issue.users_seen, "user", "users");
    let seen = format!(
        "first seen {} · last seen {}",
        minute(issue.first_seen),
        minute(issue.last_seen)
    );
    match env {
        EnvFilter::All => format!("{events} · ~{users} {STORED_COUNTS_NOTE} · {seen}"),
        EnvFilter::Subset(_) => format!(
            "{events} · {users} · {seen} (over the environments you can read: {ENV_COUNTS_NOTE})"
        ),
        _ => format!("{events} · {users} · {seen} (in this environment: {ENV_COUNTS_NOTE})"),
    }
}

/// Arguments of `get_issue`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GetIssueArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// The issue's id, as `trending_issues` prints it.
    pub issue_id: String,
    /// One occurrence's id, as `list_issue_events` prints it, to show that
    /// occurrence instead of the latest one.
    pub event_id: Option<String>,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for events with no environment. Omit to cover every environment you
    /// can read. The counts and the latest event follow it.
    pub environment: Option<String>,
}

/// Arguments of `list_issue_events`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ListIssueEventsArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// The issue's id, as `trending_issues` prints it.
    pub issue_id: String,
    /// Time window: `24h`, `7d` (default), `14d` or `30d`, back from now.
    pub window: Option<String>,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for events with no environment. Omit to cover every environment you
    /// can read.
    pub environment: Option<String>,
    /// A release as the SDK reported it, or `none` for events with no
    /// release. Omit to cover every release.
    pub release: Option<String>,
    /// Narrow the listed events with a search in Sauron's query language,
    /// e.g. `os.name:iOS`, `handled:false` or `screen:CartScreen`. The
    /// breakdown is not narrowed by it.
    pub query: Option<String>,
    /// Events per page: 1 to 50, default 10.
    pub limit: Option<u32>,
    /// The cursor from the previous page's `more results` line, unchanged,
    /// with the same other arguments.
    pub cursor: Option<String>,
}

/// An id argument, or a refusal that says where ids come from.
fn parse_id(name: &str, raw: &str, source: &str) -> Result<Uuid, ToolError> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ToolError::BadArgument(format!(
            "{name} must be an id such as 0193a4f2-7c1e-7b3a-9d2e-5f6a7b8c9d0e (got {:?}); \
             {source}",
            clip(raw, ECHO_MAX_CHARS)
        ))
    })
}

/// The one not-found for an issue id, whatever the reason.
fn issue_not_found(issue_id: Uuid, target: &AppTarget, env: &EnvFilter) -> ToolError {
    let mut msg = format!(
        "no issue {issue_id} on {} that you can read",
        target.reference()
    );
    if !matches!(env, EnvFilter::All) {
        msg.push_str(
            " in your environment selection (there, an issue is found through its events in the \
             hot store, so one whose events there were all archived or purged is not found either)",
        );
    }
    msg.push_str("; trending_issues lists issue ids");
    ToolError::NotFound(msg)
}

pub(crate) async fn get_issue(
    state: &AppState,
    user_id: Uuid,
    args: GetIssueArgs,
) -> Result<CallToolResult, ToolError> {
    // No window to narrow; an environment would only make the counts read
    // raw events instead of the stored row.
    within_deadline_saying(state, TIMED_OUT_RETRY, async {
        let issue_id = parse_id(
            "issue_id",
            &args.issue_id,
            "trending_issues lists issue ids",
        )?;
        let event_id = args
            .event_id
            .as_deref()
            .map(|e| {
                parse_id(
                    "event_id",
                    e,
                    "list_issue_events lists an issue's event ids",
                )
            })
            .transpose()?;
        let environment = trimmed(&args.environment);

        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        // The access layer first, for its refusals: an environment name the
        // caller cannot see is "not found", never "no access".
        let (requested, _) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::ISSUE_READ,
            environment.as_deref(),
        )
        .await?;
        let names = env_names(&mut conn, user_id, target.app.id).await?;
        drop(conn); // `load_issue_scoped` checks out its own

        // The dashboard's issue read: the same authorization (resolving the
        // already resolved filter again is a no-op), the same env-derived
        // counts, and the permission set the body gates below read.
        let (mut conn, scope, perms, issue) = load_issue_scoped(
            state,
            user_id,
            target.app.id,
            issue_id,
            requested.env.clone(),
        )
        .await
        .map_err(|e| match e {
            ApiError::NotFound => issue_not_found(issue_id, &target, &requested.env),
            e => e.into(),
        })?;
        let event = match event_id {
            None => repo::latest_error_event(&mut conn, scope.clone(), issue_id).await?,
            Some(id) => Some(
                repo::error_event_by_id(&mut conn, &scope, issue_id, id)
                    .await?
                    .ok_or_else(|| {
                        ToolError::NotFound(format!(
                            "no event {id} of issue {issue_id} that you can read in the hot \
                             store (it may belong to another issue, sit outside your \
                             environment selection, or have been archived or purged); \
                             list_issue_events lists this issue's event ids"
                        ))
                    })?,
            ),
        };
        drop(conn); // symbolication checks out its own
        let event = finish_latest_event(state, target.app.id, &perms, event).await;

        let view = IssueView {
            state,
            target: &target,
            scope: &scope,
            perms: &perms,
            names: &names,
            environment: environment.as_deref(),
        };
        let text = view.render(&issue, event.as_ref(), event_id.is_some());
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

/// What `get_issue`'s rendering needs.
struct IssueView<'a> {
    state: &'a AppState,
    target: &'a AppTarget,
    scope: &'a ReadScope,
    perms: &'a HashSet<String>,
    names: &'a HashMap<Uuid, String>,
    environment: Option<&'a str>,
}

impl IssueView<'_> {
    fn render(&self, issue: &Issue, event: Option<&ErrorEvent>, picked: bool) -> String {
        let mut out = self.summary(issue);
        out.push(String::new());
        match event {
            None => out.push(NO_HOT_EVENT.into()),
            Some(ev) => out.extend(self.event(ev, picked)),
        }
        out.join("\n")
    }

    /// The issue itself: what `issue:read` alone entitles the caller to.
    fn summary(&self, issue: &Issue) -> Vec<String> {
        let mut head = format!("Issue {} in {}", issue.id, self.target.reference());
        match self.environment {
            Some("none") => head.push_str(", env none (events without an environment)"),
            Some(env) => head.push_str(&format!(", env {}", one_line(env, ECHO_MAX_CHARS))),
            None => {}
        }
        let mut out = vec![head, one_line(&issue.title, TITLE_MAX_CHARS)];
        let culprit = one_line(issue.culprit.trim(), CULPRIT_MAX_CHARS);
        if !culprit.is_empty() {
            out.push(format!("Culprit: {culprit}"));
        }
        out.push(format!(
            "{} · {} · {}",
            one_line(&issue.level, ECHO_MAX_CHARS),
            one_line(&issue.status, ECHO_MAX_CHARS),
            issue_counts_and_seen(issue, &self.scope.env),
        ));
        if let Some(link) = deep_link(
            &self.state.cfg,
            self.target,
            &format!("/issues/{}", issue.id),
            &self.scope.env,
            &ReleaseFilter::All,
        ) {
            out.push(format!("Link: {link}"));
        }
        out
    }

    /// One occurrence: the shell `issue:read` covers, then (when the gates
    /// left one) the body.
    fn event(&self, ev: &ErrorEvent, picked: bool) -> Vec<String> {
        let body = crate::symbolicate::may_read_event_body(self.perms);
        let which = if picked { "Event" } else { "Latest event" };
        let mut out = vec![format!("{which} {}", ev.id)];
        if !body {
            out.push(EVENT_BODY_WITHHELD.into());
        } else if !self.perms.contains(perm::SOURCE_READ) {
            out.push(SOURCE_LINES_WITHHELD.into());
        }

        let exception = match (ev.exception_type.trim(), ev.exception_value.trim()) {
            ("", "") => one_line(&ev.message, TITLE_MAX_CHARS),
            (t, "") => one_line(t, TITLE_MAX_CHARS),
            ("", v) => one_line(v, TITLE_MAX_CHARS),
            (t, v) => format!(
                "{}: {}",
                one_line(t, TITLE_MAX_CHARS),
                one_line(v, TITLE_MAX_CHARS)
            ),
        };
        out.push(format!("Exception: {exception}"));
        let message = ev.message.trim();
        if !message.is_empty() && message != ev.exception_value.trim() {
            out.push(format!("Message: {}", one_line(message, TITLE_MAX_CHARS)));
        }
        let mut facts = vec![
            format!("occurred {}", second(ev.occurred_at)),
            format!("release {}", release_label(ev.release.as_deref())),
            format!("env {}", env_label(self.names, ev.environment_id)),
        ];
        match ev.handled {
            Some(false) => facts.push("unhandled (a crash)".into()),
            Some(true) => facts.push("handled".into()),
            None => {}
        }
        if let Some(screen) = ev.screen.as_deref().filter(|s| !s.trim().is_empty()) {
            facts.push(format!("screen {}", one_line(screen, VALUE_MAX_CHARS)));
        }
        out.push(facts.join(" · "));

        if body {
            if let Some(line) = device_line(&ev.context) {
                out.push(line);
            }
            if let Some(sdk) = ev.sdk.as_ref().and_then(|s| name_version(Some(s))) {
                out.push(format!("SDK: {sdk}"));
            }
            if let Some(user) = ev.distinct_id.as_deref().filter(|d| !d.trim().is_empty()) {
                out.push(format!("User: {}", one_line(user, VALUE_MAX_CHARS)));
            }
        }
        if let Some(sid) = ev.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
            out.push(format!(
                "Session: {} (pass it to get_session_timeline to see what the user did before \
                 this error)",
                one_line(sid, VALUE_MAX_CHARS)
            ));
        }

        if body {
            out.push(String::new());
            out.push(render_stacktrace(ev, MAX_FRAMES));
            out.push(String::new());
            out.push(render_breadcrumbs(&ev.breadcrumbs, MAX_BREADCRUMBS));
            out.push(String::new());
            let tags = render_kv(&ev.tags, MAX_KV_ENTRIES, MAX_KV_VALUE_CHARS);
            if tags.is_empty() {
                out.push("Tags: none.".into());
            } else {
                let n = ev.tags.as_object().map_or(0, |m| m.len());
                out.push(format!("Tags ({n}):"));
                out.push(tags);
            }
        }
        out
    }
}

/// `list_issue_events`' arguments, validated before any lookup.
struct EventsRequest {
    issue_id: Uuid,
    window_label: String,
    window: Duration,
    release: ReleaseFilter,
    query: Option<String>,
    limit: u32,
    cursor: Option<String>,
    environment: Option<String>,
}

impl EventsRequest {
    fn parse(args: &ListIssueEventsArgs) -> Result<Self, ToolError> {
        let issue_id = parse_id(
            "issue_id",
            &args.issue_id,
            "trending_issues lists issue ids",
        )?;
        let window = parse_window(
            args.window.as_deref(),
            EVENTS_WINDOWS,
            EVENTS_DEFAULT_WINDOW,
        )?;
        let cursor = match args.cursor.as_deref().map(str::trim) {
            None => None,
            Some(c) => {
                // The events route would refuse it too, but as a 400 the
                // `query` echo would then claim; say what is wrong here.
                sauron_db::query_plan::cursor::decode(
                    c,
                    repo::OccurrenceSort::OccurredAt.column(),
                    repo::OccurrenceSort::OccurredAt.cursor_kind(),
                )
                .map_err(|e| {
                    ToolError::BadArgument(format!(
                        "cursor {:?} is not one this tool handed out ({e}); pass the cursor from \
                         the previous page's `more results` line unchanged, or omit it for the \
                         first page",
                        clip(c, ECHO_MAX_CHARS)
                    ))
                })?;
                Some(c.to_string())
            }
        };
        Ok(EventsRequest {
            issue_id,
            window_label: args
                .window
                .as_deref()
                .map(str::trim)
                .unwrap_or(EVENTS_DEFAULT_WINDOW)
                .to_string(),
            window,
            release: release_arg(args.release.as_deref())?,
            query: args
                .query
                .as_deref()
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_string),
            limit: parse_limit(args.limit, EVENTS_DEFAULT_LIMIT, EVENTS_MAX_LIMIT)?,
            cursor,
            environment: trimmed(&args.environment),
        })
    }
}

/// The OS breakdown reads `context`, part of the event body: shown only to a
/// caller who may read bodies, by the predicate `gate_event_body` uses. The
/// dashboard withholds the same thing (`os.name` is a withheld search
/// dimension without `event:read`).
fn gate_breakdown(perms: &HashSet<String>, b: &mut repo::OccurrenceBreakdown) {
    if !crate::symbolicate::may_read_event_body(perms) {
        b.by_os.clear();
    }
}

pub(crate) async fn list_issue_events(
    state: &AppState,
    user_id: Uuid,
    args: ListIssueEventsArgs,
) -> Result<CallToolResult, ToolError> {
    within_deadline(state, async {
        let req = EventsRequest::parse(&args)?;
        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        let (scope, perms) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::ISSUE_READ,
            req.environment.as_deref(),
        )
        .await?;
        let names = env_names(&mut conn, user_id, target.app.id).await?;
        let tier_floor = issue_stats::error_events_floor(&mut conn).await?;
        drop(conn); // `search_issue_events` checks out its own

        // The dashboard's occurrence list, gates included. Its window is
        // whole days back from now; every window this tool offers is.
        let page = search_issue_events(
            state,
            user_id,
            target.app.id,
            req.issue_id,
            scope.env.clone(),
            req.release.clone(),
            EventsParams {
                filter: Vec::new(),
                q: None,
                query: req.query.clone(),
                sort: None,
                cursor: req.cursor.clone(),
                since_days: req.window.num_days(),
                limit: i64::from(req.limit),
                offset: 0,
            },
        )
        .await
        .map_err(|e| match e {
            ApiError::NotFound => issue_not_found(req.issue_id, &target, &scope.env),
            e => search_error(req.query.as_deref(), e),
        })?;

        let since = Utc::now() - req.window;
        // Both the breakdown and the list read raw events: hot ones only.
        let tier_note = below_tier_floor(since, tier_floor)
            .map(|f| hot_store_note("the breakdown and the event list", f));
        let release_key = release_key_opt(&req.release);
        // The breakdown is context for the event list, not the answer: when
        // its query fails or runs long, the list is still returned, with a
        // line saying the breakdown is missing.
        let breakdown = breakdown_or_reason(
            breakdown_budget(state.cfg.api_request_timeout_secs),
            async {
                let mut conn = crate::routes::db(state).await?;
                let b = repo::occurrence_breakdown(
                    &mut conn,
                    &scope,
                    req.issue_id,
                    Range::since(since),
                    release_key.as_deref(),
                )
                .await?;
                Ok(b)
            },
        )
        .await
        .map(|mut b| {
            gate_breakdown(&perms, &mut b);
            b
        });

        let view = EventsView {
            issue_link: deep_link(
                &state.cfg,
                &target,
                &format!("/issues/{}", req.issue_id),
                &scope.env,
                &req.release,
            ),
            target: &target,
            scope: &scope,
            names: &names,
            req: &req,
            body: crate::symbolicate::may_read_event_body(&perms),
            tier_note,
        };
        let text = view.render(since, breakdown.as_ref(), &page);
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

/// The line that stands in for the breakdown block when it could not be read.
fn breakdown_unavailable(reason: &str) -> String {
    format!("breakdown unavailable: {reason}")
}

/// How long the breakdown query may take. It runs after the event list, so
/// a slow one must not eat the whole tool deadline and take the list down
/// with it.
const BREAKDOWN_MAX_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// The breakdown's budget: [`BREAKDOWN_MAX_BUDGET`], or half the tool
/// deadline when that is shorter, so a small `API_REQUEST_TIMEOUT_SECS`
/// degrades the breakdown instead of timing out the whole tool.
fn breakdown_budget(request_timeout_secs: u64) -> std::time::Duration {
    BREAKDOWN_MAX_BUDGET.min(crate::mcp::access::tool_deadline(request_timeout_secs) / 2)
}

/// Run the breakdown read under `budget`, turning any failure into the short
/// reason the output prints after `breakdown unavailable:`. Never an error:
/// the caller renders the event list either way.
async fn breakdown_or_reason(
    budget: std::time::Duration,
    fut: impl std::future::Future<Output = Result<repo::OccurrenceBreakdown, ToolError>>,
) -> Result<repo::OccurrenceBreakdown, String> {
    match tokio::time::timeout(budget, fut).await {
        Err(_) => Err("timed out".into()),
        Ok(Ok(b)) => Ok(b),
        Ok(Err(ToolError::TimedOut(_))) => Err("timed out".into()),
        Ok(Err(e)) => {
            // `ToolError`'s Display already hides internal detail.
            if let ToolError::Internal(detail) = &e {
                tracing::warn!(error = %detail, "mcp list_issue_events: breakdown failed");
            }
            Err("database error".into())
        }
    }
}

/// What `list_issue_events`' rendering needs.
struct EventsView<'a> {
    /// The issue's dashboard link, when `DASHBOARD_URL` is set.
    issue_link: Option<String>,
    target: &'a AppTarget,
    scope: &'a ReadScope,
    names: &'a HashMap<Uuid, String>,
    req: &'a EventsRequest,
    /// Whether the caller may read event bodies (OS, device, user).
    body: bool,
    /// Set when the window reaches below the `error_events` tier floor.
    tier_note: Option<String>,
}

impl EventsView<'_> {
    fn render(
        &self,
        since: DateTime<Utc>,
        b: Result<&repo::OccurrenceBreakdown, &String>,
        page: &SearchEnvelope<ErrorEvent>,
    ) -> String {
        let req = self.req;
        let mut head = format!(
            "Events of issue {} in {}, last {}",
            req.issue_id,
            self.target.reference(),
            req.window_label
        );
        match req.environment.as_deref() {
            Some("none") => head.push_str(", env none (events without an environment)"),
            Some(env) => head.push_str(&format!(", env {}", one_line(env, ECHO_MAX_CHARS))),
            None => {}
        }
        match &req.release {
            ReleaseFilter::One(r) => {
                head.push_str(&format!(", release {}", one_line(r, ECHO_MAX_CHARS)))
            }
            ReleaseFilter::Unknown => head.push_str(", release none (events without a release)"),
            ReleaseFilter::All => {}
        }
        if let Some(q) = &req.query {
            head.push_str(&format!("; query {:?}", one_line(q, ECHO_MAX_CHARS)));
        }
        let mut window = format!("Window: since {}", minute(since));
        if let EnvFilter::Subset(_) = self.scope.env {
            window.push_str("; covering the environments you can read");
        }
        window.push('.');
        let mut out = vec![head, window];
        if let Some(note) = &self.tier_note {
            out.push(format!("Note: {note}."));
        }
        if let Some(link) = &self.issue_link {
            out.push(format!("Issue page: {link}"));
        }
        if !self.body {
            out.push(EVENTS_OS_USER_WITHHELD.into());
        }

        out.push(String::new());
        match b {
            Ok(b) => out.extend(self.breakdown(b)),
            Err(reason) => out.push(breakdown_unavailable(reason)),
        }

        out.push(String::new());
        out.extend(self.events(page));
        out.join("\n")
    }

    /// The breakdown block: the issue's events in the window by release,
    /// environment and OS, top first.
    fn breakdown(&self, b: &repo::OccurrenceBreakdown) -> Vec<String> {
        if b.total == 0 {
            return vec!["No events of this issue in the window.".into()];
        }
        let mut head = format!(
            "Breakdown of this issue's {} in the window",
            plural(b.total, "event", "events")
        );
        if self.req.query.is_some() {
            head.push_str(" (not narrowed by query)");
        }
        head.push(':');
        let by_release: Vec<(String, i64)> = b
            .by_release
            .iter()
            .map(|(r, n)| (release_label(Some(r)), *n))
            .collect();
        let by_env: Vec<(String, i64)> = b
            .by_environment
            .iter()
            .map(|(e, n)| (env_label(self.names, *e), *n))
            .collect();
        let by_os = if self.body {
            let os: Vec<(String, i64)> = b
                .by_os
                .iter()
                .map(|(o, n)| {
                    let o = o.trim();
                    let label = if o.is_empty() {
                        "unknown".to_string()
                    } else {
                        one_line(o, VALUE_MAX_CHARS)
                    };
                    (label, *n)
                })
                .collect();
            group_line(&os, b.total)
        } else {
            "withheld (your role lacks event:read)".to_string()
        };
        vec![
            head,
            format!("By release: {}", group_line(&by_release, b.total)),
            format!("By environment: {}", group_line(&by_env, b.total)),
            format!("By OS: {by_os}"),
        ]
    }

    /// The count line, one line per event, and the cursor when there is more.
    fn events(&self, page: &SearchEnvelope<ErrorEvent>) -> Vec<String> {
        let mut out = Vec::new();
        if page.data.is_empty() {
            out.push(if self.req.cursor.is_some() {
                "No more events.".to_string()
            } else {
                "No events match in this window.".to_string()
            });
        } else {
            let total = if page.total_is_capped {
                format!("more than {}", grouped(page.total))
            } else {
                grouped(page.total)
            };
            out.push(format!(
                "{} of {total} matching event(s), newest first. Pass an id to get_issue as \
                 event_id for the full event.",
                page.data.len()
            ));
        }
        if let Some(c) = &page.clamped {
            out.push(format!(
                "Note: the event list covers only the last {} ({}); the breakdown covers the \
                 whole window.",
                one_line(&c.to, ECHO_MAX_CHARS),
                one_line(&c.reason, 200)
            ));
        }
        for ev in &page.data {
            out.push(self.event_line(ev));
        }
        if let Some(cursor) = &page.next_cursor {
            out.push(format!("more results: pass cursor=\"{cursor}\""));
        }
        out
    }

    /// `{id} · {time} · release {r} · env {name} · {os}[ on {device}][ · user
    /// {id}]`. The OS and the device are read from the event's `context`,
    /// part of the body, so both follow the body gate.
    fn event_line(&self, ev: &ErrorEvent) -> String {
        let os = if self.body {
            let os = os_label(&ev.context).unwrap_or_else(|| "unknown OS".into());
            match device_model(&ev.context) {
                Some(device) => format!("{os} on {device}"),
                None => os,
            }
        } else {
            "OS and device withheld".into()
        };
        let mut line = format!(
            "{} · {} · release {} · env {} · {os}",
            ev.id,
            second(ev.occurred_at),
            release_label(ev.release.as_deref()),
            env_label(self.names, ev.environment_id),
        );
        if self.body {
            if let Some(user) = ev.distinct_id.as_deref().filter(|d| !d.trim().is_empty()) {
                line.push_str(&format!(" · user {}", one_line(user, VALUE_MAX_CHARS)));
            }
        }
        line
    }
}

/// `a 5 · b 2[ · other 3]`: the top groups, and what the groups past them
/// hold when the breakdown stopped at its top ten.
fn group_line(groups: &[(String, i64)], total: i64) -> String {
    let mut parts: Vec<String> = groups.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let rest = total - groups.iter().map(|(_, n)| n).sum::<i64>();
    if rest > 0 {
        parts.push(format!("other {rest}"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().expect("timestamp")
    }

    /// A search-pipeline statement timeout reaches the tool through the
    /// shared `map_plan_error`, as the request-budget `query_timeout`, and
    /// reads as the tool's own timeout text. With or without a `query`: a
    /// timeout is never the query's fault.
    #[test]
    fn a_pipeline_statement_timeout_is_the_tool_timeout() {
        use crate::mcp::access::TIMED_OUT;
        use sauron_db::query_plan::PlanError;
        for query in [Some("level:fatal"), None] {
            let e = crate::routes::search::map_plan_error(PlanError::Database(
                "canceling statement due to statement timeout".into(),
            ));
            match search_error(query, e) {
                ToolError::TimedOut(m) => assert_eq!(m, TIMED_OUT, "{query:?}"),
                other => panic!("{query:?}: expected TimedOut, got {other:?}"),
            }
        }
    }

    #[test]
    fn windows_are_whole_hours_and_the_previous_one_ends_where_this_starts() {
        let now = ts("2026-11-09T14:37:12Z");
        let w = Window::ending_at(now, Duration::hours(24));
        // Shown: the last 24 whole hours plus the current partial one.
        assert_eq!(w.start, ts("2026-11-08T14:00:00Z"));
        assert_eq!(w.current(), Range::since(ts("2026-11-08T14:00:00Z")));
        assert_eq!(
            w.shown_span(),
            Duration::hours(24) + Duration::minutes(37) + Duration::seconds(12)
        );
        // Previous: exactly 24 whole hours, ending where the shown span starts.
        assert_eq!(
            w.previous(),
            Range::new(ts("2026-11-07T14:00:00Z"), Some(ts("2026-11-08T14:00:00Z")))
        );
        let hour = Window::ending_at(now, Duration::hours(1));
        assert_eq!(hour.start, ts("2026-11-09T13:00:00Z"));
        assert_eq!(hour.previous().from, ts("2026-11-09T12:00:00Z"));
        // On the hour exactly, nothing to round and no partial hour yet.
        let aligned = Window::ending_at(ts("2026-11-09T14:00:00Z"), Duration::days(7));
        assert_eq!(aligned.start, ts("2026-11-02T14:00:00Z"));
        assert_eq!(aligned.shown_span(), Duration::days(7));
    }

    /// Events an issue firing at `per_hour` during whole hours, and at
    /// `partial_per_hour` during the current hour, has in `range` at `now`,
    /// read the way `issue_stats::ranked`/`events_for` read buckets: every
    /// hourly bucket overlapping the range (`hour >= trunc(from)`, `hour <
    /// to`, or `< now + 1h` when open), the current one filled to the second.
    fn events_at(range: Range, now: DateTime<Utc>, per_hour: i64, partial_per_hour: i64) -> i64 {
        let end = range.to.unwrap_or(now + Duration::hours(1));
        let mut hour = range.from.duration_trunc(Duration::hours(1)).unwrap();
        let mut total = 0;
        while hour < end {
            if hour + Duration::hours(1) <= now {
                total += per_hour;
            } else if hour <= now {
                total += partial_per_hour * (now - hour).num_seconds() / 3600;
            }
            hour += Duration::hours(1);
        }
        total
    }

    /// What the tool computes for one issue at `now`: the shown count, and
    /// the trend of its rate against the previous window's.
    fn shown_and_trend(
        now: DateTime<Utc>,
        len: Duration,
        per_hour: i64,
        partial_per_hour: i64,
    ) -> (i64, i64, String) {
        let w = Window::ending_at(now, len);
        let shown = events_at(w.current(), now, per_hour, partial_per_hour);
        let before = events_at(w.previous(), now, per_hour, partial_per_hour);
        let label = trend(shown, w.shown_span(), before, w.len);
        (shown, before, label)
    }

    /// A flat rate reads `≈` at any point in the hour, including :50 past
    /// and half a minute past (where whole minutes would divide by zero
    /// elapsed time). The shown count includes the partial hour.
    #[test]
    fn a_flat_rate_reads_flat_at_any_point_in_the_hour() {
        for now in [
            ts("2026-11-09T14:50:00Z"),
            ts("2026-11-09T14:00:30Z"),
            ts("2026-11-09T14:00:00Z"),
        ] {
            let into_hour = (now - now.duration_trunc(Duration::hours(1)).unwrap()).num_seconds();
            for len in [Duration::hours(1), Duration::hours(24), Duration::days(7)] {
                let (shown, before, label) = shown_and_trend(now, len, 3600, 3600);
                let hours = len.num_hours();
                assert_eq!(shown, 3600 * hours + into_hour, "{now} {len}");
                assert_eq!(before, 3600 * hours, "{now} {len}");
                assert_eq!(label, "≈", "{now} {len}");
            }
        }
    }

    /// The round-0 defect, kept as a guard: the shown count (len + 50 min)
    /// compared raw with a `len` window reads a flat rate as rising.
    #[test]
    fn raw_counts_over_unequal_spans_would_misread_a_flat_rate() {
        let now = ts("2026-11-09T14:50:00Z");
        let w = Window::ending_at(now, Duration::hours(1));
        let shown = events_at(w.current(), now, 60, 60);
        let before = events_at(w.previous(), now, 60, 60);
        assert_eq!((shown, before), (110, 60));
        // Treating both as the same length (what raw counts do):
        assert_eq!(trend(shown, w.len, before, w.len), "↑1.8×");
        // With the shown span's real length: flat.
        assert_eq!(trend(shown, w.shown_span(), before, w.len), "≈");
    }

    /// Ruling R21b: an issue that starts bursting in the current partial
    /// hour, after quiet whole hours, reads ↑, not ↓ (the round-1
    /// whole-hours rule read it as ↓<0.1×).
    #[test]
    fn a_burst_in_the_partial_hour_reads_up() {
        let now = ts("2026-11-09T14:20:00Z");
        // Previous window: 1/h every hour. Current window: quiet whole hours
        // (the burst issue only fires in the partial hour), 90/h in it.
        for len in [Duration::hours(1), Duration::hours(24)] {
            let w = Window::ending_at(now, len);
            let before = events_at(w.previous(), now, 1, 0);
            let shown = events_at(w.current(), now, 0, 90);
            assert_eq!(shown, 30, "{len}: 20 minutes at 90/h");
            assert_eq!(before, len.num_hours(), "{len}");
            let label = trend(shown, w.shown_span(), before, w.len);
            assert!(label.starts_with('↑'), "{len}: {label}");
        }
        // 30 events over 24h20m against 24 over 24h: 1.23×.
        let w = Window::ending_at(now, Duration::hours(24));
        assert_eq!(trend(30, w.shown_span(), 24, w.len), "↑1.2×");
    }

    #[test]
    fn trend_labels() {
        let day = Duration::hours(24);
        assert_eq!(trend(8, day, 2, day), "↑4.0×");
        assert_eq!(trend(12, day, 10, day), "↑1.2×");
        assert_eq!(trend(11, day, 10, day), "≈");
        assert_eq!(trend(9, day, 10, day), "≈");
        assert_eq!(trend(8, day, 10, day), "↓0.8×");
        assert_eq!(trend(1, day, 4, day), "↓0.2×");
        assert_eq!(trend(1, day, 100, day), "↓<0.1×");
        assert_eq!(trend(5, day, 0, day), "new in window");
        // Rates, not counts: 20 over two days is the same rate as 10 over one.
        assert_eq!(trend(20, day * 2, 10, day), "≈");
        // A zero span cannot divide by zero.
        assert_eq!(trend(5, Duration::zero(), 5, day), "↑86400.0×");
    }

    #[test]
    fn grouped_thousands() {
        assert_eq!(grouped(10_000), "10,000");
        assert_eq!(grouped(200), "200");
        assert_eq!(grouped(1_000_000), "1,000,000");
        assert_eq!(grouped(-1234), "-1,234");
        assert_eq!(grouped(0), "0");
    }

    #[test]
    fn sort_and_status_arguments() {
        assert_eq!(SortBy::parse(None).unwrap(), SortBy::Events);
        assert_eq!(SortBy::parse(Some(" Users ")).unwrap(), SortBy::Users);
        assert!(
            matches!(SortBy::parse(Some("time")), Err(ToolError::BadArgument(m)) if m.contains("events, users"))
        );
        assert_eq!(parse_status(None).unwrap(), Some("unresolved"));
        assert_eq!(parse_status(Some("all")).unwrap(), None);
        assert_eq!(parse_status(Some(" Ignored ")).unwrap(), Some("ignored"));
        match parse_status(Some("open")) {
            Err(ToolError::BadArgument(m)) => {
                assert!(m.contains("unresolved, resolved, ignored, all"), "{m}")
            }
            other => panic!("expected BadArgument, got {other:?}"),
        }
    }

    #[test]
    fn users_rerank_is_a_total_order() {
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let pool = vec![
            RankedIssue {
                issue_id: ids[0],
                events: 10,
            },
            RankedIssue {
                issue_id: ids[1],
                events: 3,
            },
            RankedIssue {
                issue_id: ids[2],
                events: 7,
            },
            RankedIssue {
                issue_id: ids[3],
                events: 7,
            },
        ];
        let users: HashMap<Uuid, i64> = [(ids[0], 1), (ids[1], 3), (ids[2], 1), (ids[3], 1)].into();
        let got: Vec<Uuid> = rerank_by_users(pool, &users)
            .into_iter()
            .map(|r| r.issue_id)
            .collect();
        let (lo, hi) = if ids[2] < ids[3] {
            (ids[2], ids[3])
        } else {
            (ids[3], ids[2])
        };
        assert_eq!(got, vec![ids[1], ids[0], lo, hi]);
    }

    #[test]
    fn span_labels() {
        assert_eq!(span_label(Duration::minutes(110)), "1h50m");
        assert_eq!(
            span_label(Duration::hours(24) + Duration::minutes(37)),
            "24h37m"
        );
        assert_eq!(span_label(Duration::hours(24)), "24h0m");
        assert_eq!(
            span_label(Duration::days(7) + Duration::minutes(37) + Duration::seconds(59)),
            "7d0h37m"
        );
        assert_eq!(span_label(Duration::zero()), "0h0m");
    }

    /// Final-review finding 2: the stored `users_seen` is an estimate that can
    /// lag (by about one ingest write-back interval), so `get_issue` must say
    /// so rather than print it bare next to `trending_issues`' exact
    /// per-window count. Under an
    /// environment filter the counts are exact, derived on read from hot rows.
    #[test]
    fn issue_counts_say_what_they_are_for_each_env_filter() {
        let t: DateTime<Utc> = "2026-11-09T10:00:00Z".parse().unwrap();
        let issue = Issue {
            id: Uuid::nil(),
            app_id: Uuid::nil(),
            fingerprint: "f".into(),
            type_: "Error".into(),
            title: "t".into(),
            culprit: String::new(),
            level: "error".into(),
            status: "unresolved".into(),
            first_seen: t,
            last_seen: t,
            times_seen: 8,
            users_seen: 1,
            assignee_id: None,
            created_at: t,
            updated_at: t,
            last_event_at: t,
        };
        let seen = "first seen 2026-11-09 10:00 UTC · last seen 2026-11-09 10:00 UTC";
        assert_eq!(
            issue_counts_and_seen(&issue, &EnvFilter::All),
            format!(
                "8 events · ~1 user (all-time; the users figure is an estimate that can lag — \
                 trending_issues counts users exactly per window) · {seen}"
            )
        );
        let in_env = issue_counts_and_seen(&issue, &EnvFilter::One(Uuid::nil()));
        assert_eq!(
            in_env,
            format!(
                "8 events · 1 user · {seen} (in this environment: events and users are exact \
                 all-time counts of the events still in the hot store; archived events are not \
                 included)"
            )
        );
        assert_eq!(
            issue_counts_and_seen(&issue, &EnvFilter::Unattributed),
            in_env
        );
        let subset = issue_counts_and_seen(&issue, &EnvFilter::Subset(vec![Uuid::nil()]));
        assert!(
            subset.starts_with("8 events · 1 user · first seen")
                && subset
                    .contains("(over the environments you can read: events and users are exact"),
            "{subset}"
        );
        for exact in [&in_env, &subset] {
            assert!(
                !exact.contains('~') && !exact.contains("estimate"),
                "{exact}"
            );
        }
    }

    #[test]
    fn fallback_counts_mark_the_stored_users_figure_as_an_estimate() {
        assert_eq!(
            fallback_counts(false, 55, 3),
            vec![
                "55 events all-time",
                "~3 users all-time (stored estimate; can lag)"
            ]
        );
        assert_eq!(
            fallback_counts(true, 5, 1),
            vec!["5 events in window", "1 user in window"]
        );
    }

    #[test]
    fn breakdown_lines_name_the_rest_past_the_top_groups() {
        let groups = vec![("2.0.0".to_string(), 5), ("none".to_string(), 1)];
        assert_eq!(group_line(&groups, 6), "2.0.0 5 · none 1");
        assert_eq!(group_line(&groups, 9), "2.0.0 5 · none 1 · other 3");
    }

    #[test]
    fn events_arguments_are_refused_before_any_lookup() {
        let args = |window: Option<&str>, cursor: Option<&str>| ListIssueEventsArgs {
            app: "mobile/ios".into(),
            issue_id: Uuid::nil().to_string(),
            window: window.map(str::to_string),
            environment: None,
            release: None,
            query: Some("  ".into()),
            limit: None,
            cursor: cursor.map(str::to_string),
        };
        let ok = EventsRequest::parse(&args(None, None)).expect("defaults");
        assert_eq!(ok.window_label, "7d");
        assert_eq!(ok.window.num_days(), 7);
        assert_eq!(ok.limit, EVENTS_DEFAULT_LIMIT);
        assert_eq!(ok.query, None, "a blank query is no query");
        assert_eq!(
            EventsRequest::parse(&args(Some("24h"), None))
                .unwrap()
                .window
                .num_days(),
            1
        );
        match EventsRequest::parse(&args(Some("1h"), None)) {
            Err(ToolError::BadArgument(m)) => assert!(m.contains("24h, 7d, 14d, 30d"), "{m}"),
            _ => panic!("1h must be refused"),
        }
        match EventsRequest::parse(&args(None, Some("bm90LWEtY3Vyc29y"))) {
            Err(ToolError::BadArgument(m)) => assert!(m.contains("cursor"), "{m}"),
            _ => panic!("a forged cursor must be refused"),
        }
        let mut bad_id = args(None, None);
        bad_id.issue_id = "42".into();
        match EventsRequest::parse(&bad_id) {
            Err(ToolError::BadArgument(m)) => assert!(m.contains("issue_id"), "{m}"),
            _ => panic!("a non-id must be refused"),
        }
    }

    fn some_breakdown() -> repo::OccurrenceBreakdown {
        repo::OccurrenceBreakdown {
            total: 0,
            by_release: Vec::new(),
            by_environment: Vec::new(),
            by_os: Vec::new(),
        }
    }

    #[test]
    fn breakdown_budget_is_capped_and_degrades_with_a_short_timeout() {
        use std::time::Duration;
        assert_eq!(breakdown_budget(60), Duration::from_secs(10));
        assert_eq!(breakdown_budget(30), Duration::from_secs(10));
        assert_eq!(breakdown_budget(15), Duration::from_secs(5));
        assert_eq!(breakdown_budget(5), Duration::from_millis(500));
    }

    /// R23a, render side: a breakdown that could not be read prints the
    /// reason and the event list still renders.
    #[test]
    fn a_missing_breakdown_still_renders_the_event_list() {
        use sauron_db::models::{App, Organization, Project};
        let now = Utc::now();
        let (org_id, project_id) = (Uuid::new_v4(), Uuid::new_v4());
        let target = AppTarget {
            app: App {
                id: Uuid::new_v4(),
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
        };
        let req = EventsRequest {
            issue_id: Uuid::new_v4(),
            window_label: "7d".into(),
            window: Duration::days(7),
            release: ReleaseFilter::All,
            query: None,
            limit: 10,
            cursor: None,
            environment: None,
        };
        let scope = ReadScope::all(target.app.id);
        let names = HashMap::new();
        let view = EventsView {
            issue_link: None,
            target: &target,
            scope: &scope,
            names: &names,
            req: &req,
            body: true,
            tier_note: None,
        };
        let page = SearchEnvelope::<ErrorEvent> {
            data: Vec::new(),
            total: 0,
            total_is_capped: false,
            next_cursor: None,
            clamped: None,
        };
        let reason = "timed out".to_string();
        let text = view.render(now, Err(&reason), &page);
        assert!(text.contains("breakdown unavailable: timed out"), "{text}");
        assert!(!text.contains("Breakdown of"), "{text}");
        assert!(text.contains("No events match in this window."), "{text}");
        assert!(
            text.contains(&format!("Events of issue {}", req.issue_id)),
            "{text}"
        );
        assert!(!text.contains("Note:"), "no tier floor, no note: {text}");

        // Below the tier floor: one note, right under the window line.
        let floor = ts("2026-11-01T00:00:00Z");
        let view = EventsView {
            tier_note: below_tier_floor(floor - Duration::days(2), Some(floor))
                .map(|f| hot_store_note("the breakdown and the event list", f)),
            ..view
        };
        let text = view.render(now, Err(&reason), &page);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].starts_with("Window: since"), "{text}");
        assert_eq!(
            lines[2],
            "Note: this window starts before 2026-11-01 00:00 UTC, where the events still in \
             the hot store begin (older ones are archived): the breakdown and the event list \
             read hot events only and may be incomplete for the older days.",
            "{text}"
        );
    }

    #[test]
    fn trending_names_what_it_reads_from_hot_events_below_the_tier_floor() {
        let floor = ts("2026-11-01T00:00:00Z");
        let below = floor - Duration::hours(1);
        // At or above the floor, or no floor at all: no note.
        assert_eq!(trending_tier_note(floor, Some(floor), true, false), None);
        assert_eq!(trending_tier_note(below, None, true, true), None);
        // The unscoped fallback reads stored issue rows only: no note.
        assert_eq!(trending_tier_note(below, Some(floor), false, false), None);

        let users = trending_tier_note(below, Some(floor), true, false).expect("note");
        assert!(users.contains(": users read hot events only"), "{users}");
        assert!(users.contains("2026-11-01 00:00 UTC"), "{users}");
        let env = trending_tier_note(below, Some(floor), true, true).expect("note");
        assert!(
            env.contains("users and the environment's titles, culprits, levels"),
            "{env}"
        );
        let fallback = trending_tier_note(below, Some(floor), false, true).expect("note");
        assert!(
            fallback.contains(": events, users and the environment's"),
            "{fallback}"
        );
    }

    /// R23a: a breakdown that fails, is cancelled by the database, or runs
    /// past its budget becomes a short reason, never an error that would
    /// take the event list down with it.
    #[tokio::test]
    async fn a_failing_breakdown_becomes_a_reason_not_an_error() {
        use std::time::Duration;
        let budget = Duration::from_millis(50);

        let ok = breakdown_or_reason(budget, async { Ok(some_breakdown()) }).await;
        assert!(ok.is_ok());

        let db_timeout = diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::Unknown,
            Box::new("canceling statement due to statement timeout".to_string()),
        );
        let got = breakdown_or_reason(budget, async { Err(ToolError::from(db_timeout)) }).await;
        assert_eq!(got.unwrap_err(), "timed out");

        let got = breakdown_or_reason(budget, async {
            Err(ToolError::Internal(
                "connection refused at 10.0.0.5:5432".into(),
            ))
        })
        .await;
        let reason = got.unwrap_err();
        assert_eq!(reason, "database error");
        assert!(!reason.contains("10.0.0.5"), "no internal detail: {reason}");

        let got = breakdown_or_reason(budget, async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(some_breakdown())
        })
        .await;
        assert_eq!(got.unwrap_err(), "timed out");

        assert_eq!(
            breakdown_unavailable("timed out"),
            "breakdown unavailable: timed out"
        );
    }

    #[test]
    fn bad_arguments_are_refused_before_any_lookup() {
        let args = |window: Option<&str>, limit: Option<u32>| TrendingArgs {
            app: "mobile/ios".into(),
            window: window.map(str::to_string),
            sort: None,
            environment: None,
            release: None,
            status: None,
            query: Some("   ".into()),
            limit,
        };
        let now = ts("2026-11-09T14:37:12Z");
        let ok = Request::parse(&args(None, None), now).expect("defaults");
        assert_eq!(ok.window_label, "24h");
        assert_eq!(ok.limit, DEFAULT_LIMIT);
        assert_eq!(ok.status, Some("unresolved"));
        assert_eq!(ok.query, None, "a blank query is no query");
        match Request::parse(&args(Some("2w"), None), now) {
            Err(ToolError::BadArgument(m)) => assert!(m.contains("1h, 24h, 7d, 14d, 30d"), "{m}"),
            _ => panic!("2w must be refused"),
        }
        assert!(matches!(
            Request::parse(&args(None, Some(51)), now),
            Err(ToolError::BadArgument(_))
        ));
    }
}
