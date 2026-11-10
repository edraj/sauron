//! `slow_operations`: the slowest transactions of an app by p95.
//!
//! Reads the dashboard's performance summary (`repo::performance_summary`,
//! rollup-gated inside, behind `event:read`): one row per `(name, op)` PAIR,
//! since two operations may share a name and differ in op. The summary
//! returns at most 100 rows, the busiest by call count, so this re-sorts
//! THOSE by p95 (descending) and applies `limit`. A rarely called operation
//! outside the 100 busiest is not in it, and the output says so when the cap
//! was reached. Durations are milliseconds. There is no release filter:
//! the performance rollup has no release dimension.

use chrono::Utc;
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::repo::{self, PerfSummaryRow};
use sauron_db::scope::Range;

use crate::mcp::access::{deep_link, read_scope, resolve_app, within_deadline, ToolError};
use crate::mcp::format::{
    one_line, parse_limit, parse_window, plural, trimmed, ECHO_MAX_CHARS, NAME_MAX_CHARS,
};
use crate::AppState;

const WINDOWS: &[&str] = &["24h", "7d", "30d", "90d"];
const DEFAULT_WINDOW: &str = "7d";
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;
/// The most rows `performance_summary` returns.
const SUMMARY_CAP: usize = 100;

/// Arguments of `slow_operations`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SlowOperationsArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// Time window: `24h`, `7d` (default), `30d` or `90d`.
    pub window: Option<String>,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for transactions with no environment. Omit to cover every environment
    /// you can read.
    pub environment: Option<String>,
    /// Only operations of this kind, e.g. `http.server` or `db.query`.
    pub op: Option<String>,
    /// How many operations to return, slowest p95 first: 1 to 50, default 20.
    pub limit: Option<u32>,
}

pub(crate) async fn slow_operations(
    state: &AppState,
    user_id: Uuid,
    args: SlowOperationsArgs,
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
        let op =
            match trimmed(&args.op) {
                Some(o) if o.is_empty() => return Err(ToolError::BadArgument(
                    "op must not be empty: pass an operation kind such as http.server, or omit \
                     it to cover every operation"
                        .into(),
                )),
                other => other,
            };
        let environment = trimmed(&args.environment);

        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        // The dashboard's `/performance/summary` authorizes on `event:read`.
        let (scope, _perms) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::EVENT_READ,
            environment.as_deref(),
        )
        .await?;
        let rows = repo::performance_summary(
            &mut conn,
            scope.clone(),
            Range::since(Utc::now() - window),
            op.as_deref(),
            None,
        )
        .await?;
        drop(conn);

        let mut head = format!(
            "Slowest operations of {} by p95, last {window_label}",
            target.reference()
        );
        match environment.as_deref() {
            Some("none") => head.push_str(", env none (transactions without an environment)"),
            Some(env) => head.push_str(&format!(", env {}", one_line(env, ECHO_MAX_CHARS))),
            None => {}
        }
        if let Some(op) = &op {
            head.push_str(&format!(", op {}", one_line(op, ECHO_MAX_CHARS)));
        }
        let mut out = vec![format!("{head}."), String::new()];
        out.extend(render_rows(rows, limit));
        if let Some(link) = deep_link(
            &state.cfg,
            &target,
            "/performance",
            &scope.env,
            &sauron_query::ReleaseFilter::All,
        ) {
            out.push(String::new());
            out.push(format!("Performance page: {link}"));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.join("\n"),
        )]))
    })
    .await
}

/// Order for the slowest-first list: p95 descending, then busiest, then
/// `(name, op)`, so equal rows never reorder between calls.
fn by_p95(a: &PerfSummaryRow, b: &PerfSummaryRow) -> std::cmp::Ordering {
    b.p95
        .total_cmp(&a.p95)
        .then_with(|| b.count.cmp(&a.count))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.op.cmp(&b.op))
}

/// Milliseconds: one decimal under 100 ms, whole numbers above.
fn ms(v: f64) -> String {
    if v < 100.0 {
        format!("{v:.1}ms")
    } else {
        format!("{v:.0}ms")
    }
}

fn render_rows(mut rows: Vec<PerfSummaryRow>, limit: usize) -> Vec<String> {
    if rows.is_empty() {
        return vec!["No transactions in this window.".to_string()];
    }
    let total = rows.len();
    let capped = total >= SUMMARY_CAP;
    rows.sort_by(by_p95);
    let mut out = vec![format!(
        "{} of {total} {} shown, slowest p95 first. Each line: op name · p50 · p95 · calls · \
         error rate. Durations are milliseconds.",
        total.min(limit),
        if total == 1 {
            "operation"
        } else {
            "operations"
        },
    )];
    if capped {
        out.push(format!(
            "Note: Sauron's summary holds the {SUMMARY_CAP} busiest operations by call count, \
             and this ranks those; a rarely called slow operation may not be among them."
        ));
    }
    for r in rows.iter().take(limit) {
        out.push(format!(
            "{} {} · p50 {} · p95 {} · {} · {:.1}% errors",
            one_line(&r.op, NAME_MAX_CHARS),
            one_line(&r.name, NAME_MAX_CHARS),
            ms(r.p50),
            ms(r.p95),
            plural(r.count, "call", "calls"),
            r.error_rate * 100.0,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, op: &str, count: i64, p50: f64, p95: f64, err: f64) -> PerfSummaryRow {
        PerfSummaryRow {
            name: name.into(),
            op: op.into(),
            count,
            p50,
            p75: p95,
            p95,
            p99: p95,
            avg: p50,
            error_rate: err,
        }
    }

    #[test]
    fn rows_sort_by_p95_then_count_then_name_and_op() {
        let lines = render_rows(
            vec![
                row("GET /a", "http.server", 900, 10.0, 50.0, 0.0),
                row("GET /b", "http.server", 10, 400.0, 2400.0, 0.125),
                row("GET /b", "db.query", 10, 5.0, 2400.0, 0.0),
                row("GET /c", "http.server", 50, 20.0, 120.0, 0.0),
            ],
            3,
        );
        // Equal p95: the busier... both 10 calls, so name then op breaks it
        // (db.query before http.server).
        assert!(lines[1].starts_with("db.query GET /b"), "{lines:?}");
        assert!(lines[2].starts_with("http.server GET /b"), "{lines:?}");
        assert!(lines[3].starts_with("http.server GET /c"), "{lines:?}");
        assert_eq!(lines.len(), 4, "limit 3 leaves header + 3 rows: {lines:?}");
        assert!(
            lines[2].contains("p50 400ms · p95 2400ms · 10 calls · 12.5% errors"),
            "{}",
            lines[2]
        );
        assert!(
            lines[0].starts_with("3 of 4 operations shown"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn the_pair_is_the_identity_and_ms_are_rounded_sensibly() {
        assert_eq!(ms(4.26), "4.3ms");
        assert_eq!(ms(99.94), "99.9ms");
        assert_eq!(ms(100.4), "100ms");
        assert_eq!(ms(2400.0), "2400ms");
        let lines = render_rows(
            vec![
                row("same", "a", 1, 1.0, 5.0, 0.0),
                row("same", "b", 1, 1.0, 6.0, 0.0),
            ],
            10,
        );
        assert!(lines[1].starts_with("b same"), "{lines:?}");
        assert!(lines[2].starts_with("a same"), "{lines:?}");
        assert!(lines[1].contains("1 call ·"), "{lines:?}");
    }

    #[test]
    fn a_full_summary_says_it_was_capped() {
        let rows: Vec<PerfSummaryRow> = (0..SUMMARY_CAP)
            .map(|i| row(&format!("op{i}"), "x", 1, 1.0, i as f64, 0.0))
            .collect();
        let lines = render_rows(rows, 2);
        assert!(
            lines.iter().any(|l| l.contains("100 busiest operations")),
            "{lines:?}"
        );
        assert!(lines[2].starts_with("x op99"), "{lines:?}");
        let few = render_rows(vec![row("a", "x", 1, 1.0, 1.0, 0.0)], 5);
        assert!(!few.iter().any(|l| l.contains("busiest")), "{few:?}");
        assert_eq!(
            render_rows(Vec::new(), 5),
            vec!["No transactions in this window."]
        );
    }
}
