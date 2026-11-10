//! `get_session_timeline`: what one user did before a crash.
//!
//! The dashboard's session detail, as text: the session header, then its
//! events, errors and transactions in time order. Authorization and bodies
//! are the dashboard's, not reimplemented. `routes::sessions::load_session_scoped`
//! authorizes `event:read` and loads the row; `session_timeline` merges the
//! hot and archived rows, symbolicates and gates them (`gate_event_body`,
//! `gate_source_context`, `gate_transaction_body`). This renders only what
//! survives those gates: an error line shows `exception_type: value` when
//! they are present after gating, and never a stack trace, breadcrumb, tag,
//! context or user, which the timeline does not print at all.

use chrono::{DateTime, Utc};
use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::models::{AnalyticsEvent, ErrorEvent, Session, Transaction};

use crate::error::ApiError;
use crate::mcp::access::{
    deep_link, read_scope, resolve_app, within_deadline_saying, AppTarget, ToolError,
    TIMED_OUT_RETRY,
};
use crate::mcp::format::{
    clip, device_line, env_label, env_names, one_line, release_label, second, trimmed,
    ECHO_MAX_CHARS, NAME_MAX_CHARS,
};
use crate::routes::sessions::{load_session_scoped, session_timeline, TimelineItem};
use crate::AppState;
use std::collections::HashMap;

/// Timeline lines printed. A session holds up to 500 rows of each kind.
pub(crate) const MAX_LINES: usize = 200;
/// A properties JSON, error value or similar, clipped.
const FIELD_MAX_CHARS: usize = 200;
/// The longest session id accepted: ids are SDK-generated, typically UUIDs.
const SESSION_ID_MAX_CHARS: usize = 256;
/// How many rows of each kind `session_timeline` reads (oldest first).
const READ_CAP: usize = 500;
const ARCHIVE_NOTE: &str = "older rows archived; timeline may be incomplete";

/// Arguments of `get_session_timeline`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SessionTimelineArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// The session id as the SDK reported it. `get_issue` prints the session
    /// id of the event it shows.
    pub session_id: String,
    /// An environment name such as `production` (see `list_apps`), or `none`
    /// for sessions with no environment. Omit to cover every environment you
    /// can read.
    pub environment: Option<String>,
}

pub(crate) async fn get_session_timeline(
    state: &AppState,
    user_id: Uuid,
    args: SessionTimelineArgs,
) -> Result<CallToolResult, ToolError> {
    // One session: no window to narrow, and an environment does not make
    // the read cheaper.
    within_deadline_saying(state, TIMED_OUT_RETRY, async {
        let session_id = args.session_id.trim();
        if session_id.is_empty() || session_id.chars().count() > SESSION_ID_MAX_CHARS {
            return Err(ToolError::BadArgument(
                "session_id must be a session id as the SDK reported it (get_issue prints the \
                 session id of an event)"
                    .into(),
            ));
        }
        let environment = trimmed(&args.environment);

        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        let (requested, _) = read_scope(
            &mut conn,
            user_id,
            &target,
            perm::EVENT_READ,
            environment.as_deref(),
        )
        .await?;
        let names = env_names(&mut conn, user_id, target.app.id).await?;
        drop(conn); // `load_session_scoped` checks out its own

        let (conn, scope, perms, session) =
            load_session_scoped(state, user_id, target.app.id, session_id, requested.env)
                .await
                .map_err(|e| match e {
                    ApiError::NotFound => session_not_found(session_id, &target),
                    e => e.into(),
                })?;
        let (timeline, archive_incomplete) =
            session_timeline(state, conn, scope.clone(), &perms, target.app.id, &session).await?;
        let link = deep_link(
            &state.cfg,
            &target,
            &format!("/sessions/{}", encode_segment(&session.session_id)),
            &scope.env,
            &sauron_query::ReleaseFilter::All,
        );
        let text = render(
            &target,
            link,
            &names,
            &session,
            &timeline,
            archive_incomplete,
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

fn session_not_found(session_id: &str, target: &AppTarget) -> ToolError {
    ToolError::NotFound(format!(
        "no session {:?} on {} that you can read (it may belong to another app, sit outside \
         your environment selection, or have been purged)",
        clip(session_id, ECHO_MAX_CHARS),
        target.reference()
    ))
}

/// `1h05m`, `4m12s`, `37s`.
fn duration_label(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn clock(t: DateTime<Utc>) -> String {
    t.format("%H:%M:%S%.3f").to_string()
}

fn render(
    target: &AppTarget,
    link: Option<String>,
    names: &HashMap<Uuid, String>,
    session: &Session,
    timeline: &[TimelineItem],
    archive_incomplete: bool,
) -> String {
    let mut out = vec![format!(
        "Session {} in {}",
        one_line(&session.session_id, NAME_MAX_CHARS),
        target.reference()
    )];
    let mut facts = vec![
        format!("started {}", second(session.started_at)),
        format!(
            "duration {}",
            duration_label(session.last_event_at - session.started_at)
        ),
        format!("release {}", release_label(session.release.as_deref())),
        format!("env {}", env_label(names, session.environment_id)),
        format!("{} events", session.events_count),
        format!(
            "errors_count {} ({} unhandled)",
            session.errors_count, session.unhandled_errors_count
        ),
    ];
    if let Some(user) = session
        .distinct_id
        .as_deref()
        .filter(|u| !u.trim().is_empty())
    {
        facts.push(format!("user {}", one_line(user, NAME_MAX_CHARS)));
    }
    out.push(facts.join(" · "));
    if let Some(device) = device_line(&session.context) {
        out.push(device);
    }
    if let Some(link) = link {
        out.push(format!("Link: {link}"));
    }
    if archive_incomplete {
        out.push(ARCHIVE_NOTE.to_string());
    }
    out.extend(truncation_notes(session, timeline));
    out.push(String::new());

    if timeline.is_empty() {
        out.push("No events, errors or transactions recorded for this session.".to_string());
        return out.join("\n");
    }
    // Time order; past the cap, the LAST rows: they lead up to the end of the
    // session, which is where a crash is.
    let skipped = timeline.len().saturating_sub(MAX_LINES);
    if skipped > 0 {
        out.push(format!(
            "{} rows in time order; showing the last {MAX_LINES} ({skipped} earlier rows \
             omitted).",
            timeline.len()
        ));
    } else {
        out.push(format!("{} rows in time order.", timeline.len()));
    }
    for item in &timeline[skipped..] {
        out.push(match item {
            TimelineItem::Event { at, event } => event_line(*at, event),
            TimelineItem::Error { at, error } => error_line(*at, error),
            TimelineItem::Transaction { at, transaction } => txn_line(*at, transaction),
        });
    }
    out.join("\n")
}

/// `session_timeline` reads only the OLDEST [`READ_CAP`] rows of each kind.
/// A kind is flagged when fewer rows came back than the session's own
/// counter says exist, or when the read hit the cap (the only signal for
/// transactions, which have no counter), since the tail (often the crash)
/// would otherwise read as complete. Each note says only what is known:
///
/// - short of the counter: `{kind}: read {n} of {expected}`, and where the
///   missing rows are, which is after the last row read only when the cap
///   cut the read (otherwise they were archived, purged or never stored, and
///   could be anywhere);
/// - at the cap with no counter saying more: the cap was reached, so later
///   rows may be missing.
fn truncation_notes(session: &Session, timeline: &[TimelineItem]) -> Vec<String> {
    let mut seen: [(&str, usize, Option<i64>); 3] = [
        ("events", 0, Some(session.events_count)),
        ("errors", 0, Some(session.errors_count)),
        // The session row keeps no transaction counter: only the cap shows.
        ("transactions", 0, None),
    ];
    let mut last: [Option<DateTime<Utc>>; 3] = [None; 3];
    for item in timeline {
        let (i, at) = match item {
            TimelineItem::Event { at, .. } => (0, *at),
            TimelineItem::Error { at, .. } => (1, *at),
            TimelineItem::Transaction { at, .. } => (2, *at),
        };
        seen[i].1 += 1;
        last[i] = Some(last[i].map_or(at, |l| l.max(at)));
    }
    let mut out = Vec::new();
    for (i, (kind, n, expected)) in seen.iter().enumerate() {
        let capped = *n >= READ_CAP;
        let short = expected.filter(|e| i64::try_from(*n).is_ok_and(|n| n < *e));
        let after = last[i].map_or_else(
            || "later rows".to_string(),
            |t| format!("rows after {}", t.format("%H:%M:%S")),
        );
        match (short, capped) {
            (Some(e), true) => out.push(format!(
                "Note: {kind}: read {n} of {e} (the session's count; only the oldest \
                 {READ_CAP} are read) — {after} may be missing"
            )),
            (Some(e), false) => out.push(format!(
                "Note: {kind}: read {n} of {e} (the session's count) — some rows may be missing"
            )),
            (None, true) => out.push(format!(
                "Note: {kind}: the {READ_CAP}-row read cap was reached — {after} may be missing"
            )),
            (None, false) => {}
        }
    }
    out
}

/// A path segment, percent-encoded.
fn encode_segment(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// `{HH:MM:SS.mmm} event {name} {properties}`.
fn event_line(at: DateTime<Utc>, e: &AnalyticsEvent) -> String {
    let mut line = format!("{} event {}", clock(at), one_line(&e.name, NAME_MAX_CHARS));
    if let Some(props) = compact_json(&e.properties) {
        line.push(' ');
        line.push_str(&one_line(&props, FIELD_MAX_CHARS));
    }
    line
}

/// `{HH:MM:SS.mmm} error {exception_type}: {value} (issue {issue_id})`. The
/// type and value are printed when present after gating; either one alone is
/// printed alone, and neither leaves just `error`.
fn error_line(at: DateTime<Utc>, e: &ErrorEvent) -> String {
    let ty = one_line(e.exception_type.trim(), NAME_MAX_CHARS);
    let value = one_line(e.exception_value.trim(), FIELD_MAX_CHARS);
    let what = match (ty.is_empty(), value.is_empty()) {
        (false, false) => format!(" {ty}: {value}"),
        (false, true) => format!(" {ty}"),
        (true, false) => format!(" {value}"),
        (true, true) => String::new(),
    };
    format!("{} error{what} (issue {})", clock(at), e.issue_id)
}

/// `{HH:MM:SS.mmm} txn {op} {name} {duration_ms}ms`.
fn txn_line(at: DateTime<Utc>, t: &Transaction) -> String {
    format!(
        "{} txn {} {} {}ms",
        clock(at),
        one_line(&t.op, NAME_MAX_CHARS),
        one_line(&t.name, NAME_MAX_CHARS),
        t.duration_ms.round() as i64
    )
}

/// Compact JSON for a properties object, or `None` when it is null or empty.
fn compact_json(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Object(m) if m.is_empty() => None,
        serde_json::Value::Array(a) if a.is_empty() => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(events: i64, errors: i64) -> Session {
        let now = Utc::now();
        Session {
            id: Uuid::new_v4(),
            app_id: Uuid::new_v4(),
            session_id: "s".into(),
            distinct_id: None,
            device_key: None,
            started_at: now,
            last_event_at: now,
            events_count: events,
            errors_count: errors,
            unhandled_errors_count: 0,
            context: serde_json::json!({}),
            release: None,
            environment_id: None,
            ip_address: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn event_item(at: DateTime<Utc>) -> TimelineItem {
        TimelineItem::Event {
            at,
            event: AnalyticsEvent {
                id: Uuid::new_v4(),
                app_id: Uuid::new_v4(),
                environment_id: None,
                name: "tap".into(),
                distinct_id: "u".into(),
                properties: serde_json::json!({}),
                context: serde_json::json!({}),
                session_id: None,
                release: None,
                ip_address: None,
                occurred_at: at,
                received_at: at,
                device_key: None,
                screen: None,
                tags: serde_json::json!({}),
                contexts: serde_json::json!({}),
                extra: serde_json::json!({}),
            },
        }
    }

    fn txn_item(at: DateTime<Utc>) -> TimelineItem {
        TimelineItem::Transaction {
            at,
            transaction: Transaction {
                id: Uuid::new_v4(),
                app_id: Uuid::new_v4(),
                environment_id: None,
                name: "GET /cart".into(),
                op: "http.client".into(),
                duration_ms: 12.0,
                status: None,
                http_method: None,
                http_status: None,
                url: None,
                distinct_id: None,
                session_id: None,
                device_key: None,
                release: None,
                ip_address: None,
                occurred_at: at,
                received_at: at,
                workflow_id: None,
                workflow_name: None,
                restored_pin_id: None,
                finished_at: None,
                tags: serde_json::json!({}),
                extra: serde_json::json!({}),
            },
        }
    }

    fn target() -> AppTarget {
        use sauron_db::models::{App, Organization, Project};
        let now = Utc::now();
        let (org_id, project_id) = (Uuid::new_v4(), Uuid::new_v4());
        AppTarget {
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
        }
    }

    /// Each note is true in its own case and names the kind; none claims
    /// "more than 500" when that is not what happened.
    #[test]
    fn truncation_notes_say_only_what_is_known() {
        let t0: DateTime<Utc> = "2026-11-09T10:00:00Z".parse().unwrap();
        let items: Vec<TimelineItem> = (0..500)
            .map(|i| event_item(t0 + chrono::Duration::seconds(i)))
            .collect();
        let note = |s: &Session, items: &[TimelineItem]| {
            render(&target(), None, &HashMap::new(), s, items, false)
        };

        // 500 read, the session counted 800: the cap cut the read.
        let text = note(&session(800, 0), &items);
        assert!(
            text.contains(
                "Note: events: read 500 of 800 (the session's count; only the oldest 500 are \
                 read) — rows after 10:08:19 may be missing"
            ),
            "{text}"
        );
        // Short of the counter, under the cap: the missing rows could be
        // anywhere, so no "after" and no cap claim.
        let text = note(&session(5, 0), &items[..3]);
        assert!(
            text.contains(
                "Note: events: read 3 of 5 (the session's count) — some rows may be missing"
            ),
            "{text}"
        );
        // Exactly 500 with the counter agreeing: the cap was still reached.
        let text = note(&session(500, 0), &items);
        assert!(
            text.contains(
                "Note: events: the 500-row read cap was reached — rows after 10:08:19 may be \
                 missing"
            ),
            "{text}"
        );
        // No row read at all, against a counter.
        let text = note(&session(0, 2), &[]);
        assert!(
            text.contains(
                "Note: errors: read 0 of 2 (the session's count) — some rows may be missing"
            ),
            "{text}"
        );
        // Complete: no note.
        let text = note(&session(3, 0), &items[..3]);
        assert!(!text.contains("Note:"), "{text}");
        // Never the old, sometimes-false wording.
        for s in [session(800, 0), session(5, 0), session(500, 0)] {
            assert!(!note(&s, &items).contains("more than"), "{s:?}");
        }
    }

    /// Transactions have no counter: the cap is the only signal.
    #[test]
    fn transactions_at_the_cap_say_the_cap_was_reached() {
        let t0: DateTime<Utc> = "2026-11-09T10:00:00Z".parse().unwrap();
        let items: Vec<TimelineItem> = (0..500)
            .map(|i| txn_item(t0 + chrono::Duration::seconds(i)))
            .collect();
        let text = render(
            &target(),
            None,
            &HashMap::new(),
            &session(0, 0),
            &items,
            false,
        );
        assert!(
            text.contains(
                "Note: transactions: the 500-row read cap was reached — rows after 10:08:19 may \
                 be missing"
            ),
            "{text}"
        );
        let text = render(
            &target(),
            None,
            &HashMap::new(),
            &session(0, 0),
            &items[..499],
            false,
        );
        assert!(!text.contains("Note:"), "{text}");
    }

    #[test]
    fn durations() {
        let s = chrono::Duration::seconds;
        assert_eq!(duration_label(s(0)), "0s");
        assert_eq!(duration_label(s(37)), "37s");
        assert_eq!(duration_label(s(252)), "4m12s");
        assert_eq!(duration_label(s(3900)), "1h05m");
        assert_eq!(duration_label(s(-5)), "0s");
    }

    #[test]
    fn session_ids_are_encoded_for_the_link_path() {
        assert_eq!(encode_segment("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(encode_segment("sess-1_x.y"), "sess-1_x.y");
    }

    #[test]
    fn empty_properties_print_nothing() {
        use serde_json::json;
        assert_eq!(compact_json(&json!({})), None);
        assert_eq!(compact_json(&json!(null)), None);
        assert_eq!(compact_json(&json!({"a": 1})), Some(r#"{"a":1}"#.into()));
    }
}
