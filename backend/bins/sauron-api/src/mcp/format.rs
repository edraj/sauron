//! Text helpers shared by the tools: argument parsing that has to name its
//! allowed values, clipping for values that came from an SDK, and the
//! renderers for an error event's stack trace, breadcrumbs and key/value
//! blocks.
//!
//! The renderers draw what they are handed and nothing more. Permission
//! gates run before them, in the shared read paths
//! (`routes::issues::finish_latest_event`, `search_issue_events`): a body
//! the caller may not read arrives here already nulled, and renders as
//! absent.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sauron_db::models::ErrorEvent;
use sauron_db::AsyncPgConnection;
use serde_json::Value;
use uuid::Uuid;

use super::access::ToolError;
use crate::error::ApiError;

/// The most stack frames a tool prints.
pub(crate) const MAX_FRAMES: usize = 50;
/// How many of the latest breadcrumbs a tool prints.
pub(crate) const MAX_BREADCRUMBS: usize = 20;
/// Entries and value length for a tag or context block.
pub(crate) const MAX_KV_ENTRIES: usize = 30;
pub(crate) const MAX_KV_VALUE_CHARS: usize = 200;
/// A frame's source line, clipped.
const CONTEXT_LINE_MAX_CHARS: usize = 200;
/// A frame's function name and file path, clipped.
const FRAME_FIELD_MAX_CHARS: usize = 300;
/// A breadcrumb's one-line summary, clipped.
const BREADCRUMB_MAX_CHARS: usize = 200;
/// Cap on an echoed argument (environment, release, query, a session id the
/// caller passed) or a short label such as a level or an environment name.
pub(crate) const ECHO_MAX_CHARS: usize = 80;
/// Cap on a displayed name: an org, project, app or environment name, an
/// operation name, a session id or user id read back from the database.
pub(crate) const NAME_MAX_CHARS: usize = 120;
/// Cap on an SDK-reported value inside a line (release, OS, device, user).
pub(crate) const VALUE_MAX_CHARS: usize = 80;

/// The operator command that makes an app's per-issue hourly counts
/// complete, quoted wherever a tool degrades for lack of it.
pub(crate) const BACKFILL_COMMAND: &str = "sauron-migrate backfill-issue-stats";

/// Parse a `window` argument (`1h`, `24h`, `7d`, …) into a duration.
///
/// Only the values in `allowed` are accepted, even when another one would
/// parse: each tool offers the windows its data supports, and the error lists
/// them so Claude can retry with one. `None` is `default`, which must be one
/// of `allowed`.
pub(crate) fn parse_window(
    raw: Option<&str>,
    allowed: &[&str],
    default: &str,
) -> Result<chrono::Duration, ToolError> {
    let value = raw.map(str::trim).unwrap_or(default);
    if !allowed.contains(&value) {
        return Err(ToolError::BadArgument(format!(
            "window must be one of: {} (got {value:?})",
            allowed.join(", ")
        )));
    }
    window_duration(value)
        .ok_or_else(|| ToolError::Internal(format!("tool offers an unparseable window {value:?}")))
}

/// Parse a `limit` argument: `None` is `default`, anything outside
/// `1..=max` is refused with the range in the message (rather than silently
/// clamped, so Claude knows what it got).
pub(crate) fn parse_limit(raw: Option<u32>, default: u32, max: u32) -> Result<u32, ToolError> {
    match raw {
        None => Ok(default),
        Some(n) if (1..=max).contains(&n) => Ok(n),
        Some(n) => Err(ToolError::BadArgument(format!(
            "limit must be between 1 and {max} (got {n})"
        ))),
    }
}

/// `<n>h` or `<n>d` with a positive `n`.
fn window_duration(value: &str) -> Option<chrono::Duration> {
    let positive = |n: &str| n.parse::<i64>().ok().filter(|n| *n > 0);
    match value.strip_suffix('h') {
        Some(n) => chrono::Duration::try_hours(positive(n)?),
        None => chrono::Duration::try_days(positive(value.strip_suffix('d')?)?),
    }
}

/// `s` cut to its first `max` characters, with `…` appended when anything
/// was cut. Counts `char`s, so it never splits a multi-byte character the way
/// a byte slice would (and panic doing it).
pub(crate) fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_string(),
    }
}

/// Characters `one_line` turns into spaces: the control characters
/// (newlines, tabs, escapes), the Unicode line and paragraph separators
/// (U+2028, U+2029, which some renderers treat as line breaks) and the bidi
/// controls (U+202A to U+202E embeddings and overrides, U+2066 to U+2069
/// isolates), which can make a value display as something it is not.
fn is_layout_breaking(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// [`clip`], after turning layout-breaking characters (see
/// [`is_layout_breaking`]) into spaces. For SDK-reported strings shown inside
/// a line of tool output, so a value cannot break the layout, reorder what
/// follows it, or pass itself off as more output.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    clip(&s.replace(is_layout_breaking, " "), max)
}

// --- Shared line helpers ------------------------------------------------------

/// `1 event`, `2 events`.
pub(crate) fn plural(n: i64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A trimmed optional argument, as the access layer takes it.
pub(crate) fn trimmed(raw: &Option<String>) -> Option<String> {
    raw.as_deref().map(str::trim).map(str::to_string)
}

pub(crate) fn day(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

pub(crate) fn minute(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

pub(crate) fn second(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// The names of the app's live environments the caller can see, by
/// enrollment id: `env:read` decides, exactly as for the dashboard's
/// environment picker. An environment they cannot see shows as its bare id.
pub(crate) async fn env_names(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
) -> Result<HashMap<Uuid, String>, ToolError> {
    match crate::routes::environments::visible_app_environments(conn, user_id, app_id).await {
        Ok(envs) => Ok(envs
            .into_iter()
            .map(|e| (e.enrollment.id, e.name))
            .collect()),
        Err(ApiError::Auth(sauron_auth::AuthError::Forbidden)) => Ok(HashMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// An event's environment: its name when the caller can see it,
/// `unattributed` when it has none, else the bare enrollment id.
pub(crate) fn env_label(names: &HashMap<Uuid, String>, env: Option<Uuid>) -> String {
    match env {
        None => "unattributed".into(),
        Some(id) => names
            .get(&id)
            .map(|n| one_line(n, ECHO_MAX_CHARS))
            .unwrap_or_else(|| id.to_string()),
    }
}

pub(crate) fn release_label(release: Option<&str>) -> String {
    match release.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => one_line(r, VALUE_MAX_CHARS),
        None => "none".into(),
    }
}

/// `name version` from a context object such as `context.os`.
pub(crate) fn name_version(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let name = v.get("name").and_then(Value::as_str).map(str::trim)?;
    if name.is_empty() {
        return None;
    }
    let mut out = one_line(name, VALUE_MAX_CHARS);
    if let Some(ver) = v
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push(' ');
        out.push_str(&one_line(ver, VALUE_MAX_CHARS));
    }
    Some(out)
}

/// The event's OS (`context.os`), where ingest stores what the SDK sent.
pub(crate) fn os_label(context: &Value) -> Option<String> {
    name_version(context.get("os"))
}

/// A non-blank string field of an SDK-reported object, clipped to one line.
fn context_text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| one_line(s, VALUE_MAX_CHARS))
}

/// The device's model (`context.device.model`), else its family.
pub(crate) fn device_model(context: &Value) -> Option<String> {
    let device = context.get("device");
    context_text(device.and_then(|d| d.get("model")))
        .or_else(|| context_text(device.and_then(|d| d.get("family"))))
}

/// `Device: … · OS: … · Runtime: … · App: …` from the event's `context`,
/// whichever of them the SDK reported.
pub(crate) fn device_line(context: &Value) -> Option<String> {
    let mut parts = Vec::new();
    let device = context.get("device");
    if let Some(model) = device_model(context) {
        match context_text(device.and_then(|d| d.get("arch"))) {
            Some(arch) => parts.push(format!("Device: {model} ({arch})")),
            None => parts.push(format!("Device: {model}")),
        }
    }
    if let Some(os) = os_label(context) {
        parts.push(format!("OS: {os}"));
    }
    if let Some(rt) = name_version(context.get("runtime")) {
        parts.push(format!("Runtime: {rt}"));
    }
    let app = context.get("app");
    if let Some(version) = context_text(app.and_then(|a| a.get("version"))) {
        match context_text(app.and_then(|a| a.get("build"))) {
            Some(build) => parts.push(format!("App: {version} ({build})")),
            None => parts.push(format!("App: {version}")),
        }
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

// --- Freshness and coverage notes --------------------------------------------

/// The least slack [`rollup_lag_tolerance`] allows, whatever the cadence.
pub(crate) const ROLLUP_LAG_FLOOR: chrono::Duration = chrono::Duration::minutes(5);

/// How far the `issue_stats` watermark may trail `now` before a tool says
/// so. A healthy fold runs every `fold_secs` (`ROLLUP_FOLD_SECS`) up to
/// `lag_secs` (`ROLLUP_LAG_SECS`) behind the clock, so its watermark is at
/// most `fold_secs + lag_secs` old plus the cycle's own run time (a minute
/// of slack). Never below [`ROLLUP_LAG_FLOOR`], so the default 60 s + 60 s
/// cadence keeps five minutes.
pub(crate) fn rollup_lag_tolerance(fold_secs: u64, lag_secs: i64) -> chrono::Duration {
    let fold = i64::try_from(fold_secs).unwrap_or(i64::MAX);
    let secs = fold.saturating_add(lag_secs.max(0)).saturating_add(60);
    chrono::Duration::try_seconds(secs)
        .unwrap_or(chrono::Duration::MAX)
        .max(ROLLUP_LAG_FLOOR)
}

/// The note a rollup-backed tool adds when the `issue_stats` watermark
/// (`rollups::as_of(conn, &[SRC_ISSUE_STATS])`) trails `now` by more than
/// `tolerance` ([`rollup_lag_tolerance`]), or is missing: the rollup's
/// counts stop at the watermark. `what` names those counts (`Counts`,
/// `Event counts`). No trailing period: the caller places it. `None` when
/// the rollup is current.
pub(crate) fn rollup_lag_note(
    what: &str,
    as_of: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    tolerance: chrono::Duration,
) -> Option<String> {
    match as_of {
        Some(t) if now - t <= tolerance => None,
        Some(t) => Some(format!(
            "{what} as of {} (the rollup is behind; newer events are not counted yet)",
            minute(t)
        )),
        None => Some(format!(
            "{what} as of an unknown time (the rollup has no progress marker; newer events may \
             not be counted yet)"
        )),
    }
}

/// The `error_events` tier floor when a window starting at `window_start`
/// reaches below it (some of the window's raw events are archived out of
/// Postgres), else `None`. `floor` is `issue_stats::error_events_floor`.
pub(crate) fn below_tier_floor(
    window_start: DateTime<Utc>,
    floor: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    floor.filter(|f| window_start < *f)
}

/// The note for [`below_tier_floor`]: `what` (the parts of the output read
/// from raw events) covers hot rows only.
pub(crate) fn hot_store_note(what: &str, floor: DateTime<Utc>) -> String {
    format!(
        "this window starts before {}, where the events still in the hot store begin (older \
         ones are archived): {what} read hot events only and may be incomplete for the older days",
        minute(floor)
    )
}

/// An event's stack trace as text: a header line, then one line per frame.
///
/// Follows the dashboard's `StacktraceView` (`dashboard/src/lib/components/`)
/// so Claude and the developer read the same trace:
///
/// - **Which frames.** `stacktrace_symbolicated` when at least one of its
///   frames was actually resolved (`"symbolicated": true`), else the raw
///   `stacktrace`. Symbolication can store `[]` or an all-passthrough array,
///   and showing that instead of the raw frames would show less.
/// - **Order.** Most recent call first. Frames are stored the way SDKs send
///   them, oldest call first with the crash LAST, and the dashboard reverses
///   them ("Sentry convention"); so does this, and the header says so.
/// - **A frame.** `  at {function} ({file}:{line}:{col})`, with ` [app]` for
///   an in-app frame. The file is `filename`, else `module`, else
///   `abs_path`; `:{col}` and `:{line}` drop out when absent, as on the
///   dashboard. A frame's `context_line` (the source line, which survives
///   only for a caller holding `source:read`) follows as `      > {line}`,
///   clipped to 200 characters.
/// - **Long traces.** Past `max_frames`, every in-app frame is kept (up to
///   `max_frames`), the rest of the budget goes to the frames nearest the
///   crash, and each run of dropped frames becomes `  … {n} frames omitted`.
/// - **No frames.** An obfuscated Dart build sends its trace as text
///   (`debug_meta.raw_stacktrace`); the dashboard shows that verbatim, and so
///   does this, a line per line up to `max_frames` lines.
///
/// SDK-reported strings go through [`one_line`], so a crafted function name
/// cannot break the layout or pass itself off as more output.
pub(crate) fn render_stacktrace(ev: &ErrorEvent, max_frames: usize) -> String {
    let (frames, symbolicated) = displayed_frames(ev);
    if frames.is_empty() {
        return match raw_trace(ev) {
            Some(raw) => render_raw_trace(raw, max_frames),
            None => "Stack trace: none recorded for this event.".into(),
        };
    }
    // Most recent call first.
    let ordered: Vec<&Value> = frames.iter().rev().collect();
    let keep = frames_to_keep(&ordered, max_frames);
    let shown = keep.iter().filter(|k| **k).count();

    let kind = if symbolicated {
        "symbolicated"
    } else {
        "as reported by the SDK, not symbolicated"
    };
    let count = if shown < ordered.len() {
        format!("{} frames, {shown} shown", ordered.len())
    } else if ordered.len() == 1 {
        "1 frame".to_string()
    } else {
        format!("{} frames", ordered.len())
    };
    let mut out = vec![format!(
        "Stack trace ({kind}; most recent call first; {count}):"
    )];
    let mut dropped = 0usize;
    for (frame, kept) in ordered.iter().zip(&keep) {
        if !kept {
            dropped += 1;
            continue;
        }
        if dropped > 0 {
            out.push(omitted(dropped, "frame"));
            dropped = 0;
        }
        out.push(frame_line(frame));
        if let Some(src) = str_field(frame, "context_line") {
            out.push(format!(
                "      > {}",
                one_line(src.trim(), CONTEXT_LINE_MAX_CHARS)
            ));
        }
    }
    if dropped > 0 {
        out.push(omitted(dropped, "frame"));
    }
    out.join("\n")
}

/// The frames the dashboard would show, and whether they are the resolved
/// ones.
fn displayed_frames(ev: &ErrorEvent) -> (&[Value], bool) {
    if let Some(Value::Array(sym)) = &ev.stacktrace_symbolicated {
        if sym
            .iter()
            .any(|f| f.get("symbolicated").and_then(Value::as_bool) == Some(true))
        {
            return (sym, true);
        }
    }
    match &ev.stacktrace {
        Value::Array(raw) => (raw, false),
        _ => (&[], false),
    }
}

/// Which of `ordered` (most recent first) to print: all of them up to
/// `max`; past it, the in-app frames (nearest the crash first, up to `max`),
/// then the remaining budget from the top of the list.
fn frames_to_keep(ordered: &[&Value], max: usize) -> Vec<bool> {
    if ordered.len() <= max {
        return vec![true; ordered.len()];
    }
    let mut keep = vec![false; ordered.len()];
    let mut budget = max;
    for (i, frame) in ordered.iter().enumerate() {
        if budget == 0 {
            break;
        }
        if frame.get("in_app").and_then(Value::as_bool) == Some(true) {
            keep[i] = true;
            budget -= 1;
        }
    }
    for k in keep.iter_mut() {
        if budget == 0 {
            break;
        }
        if !*k {
            *k = true;
            budget -= 1;
        }
    }
    keep
}

/// `  at {function} ({location})[ [app]]`.
fn frame_line(frame: &Value) -> String {
    let function = str_field(frame, "function").unwrap_or("<anonymous>");
    let file = str_field(frame, "filename")
        .or_else(|| str_field(frame, "module"))
        .or_else(|| str_field(frame, "abs_path"))
        .unwrap_or("<unknown file>");
    let mut location = one_line(file, FRAME_FIELD_MAX_CHARS);
    if let Some(line) = frame.get("lineno").and_then(Value::as_u64) {
        location.push_str(&format!(":{line}"));
        if let Some(col) = frame.get("colno").and_then(Value::as_u64) {
            location.push_str(&format!(":{col}"));
        }
    }
    let mut out = format!(
        "  at {} ({location})",
        one_line(function, FRAME_FIELD_MAX_CHARS)
    );
    if frame.get("in_app").and_then(Value::as_bool) == Some(true) {
        out.push_str(" [app]");
    }
    out
}

/// The obfuscated-Dart text trace, when the event carries one.
fn raw_trace(ev: &ErrorEvent) -> Option<&str> {
    ev.debug_meta
        .as_ref()?
        .get("raw_stacktrace")?
        .as_str()
        .filter(|s| !s.trim().is_empty())
}

fn render_raw_trace(raw: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = vec![
        "Stack trace (raw text as the SDK sent it; not symbolicated, so no frames could be \
         read; upload the build's symbols to resolve it):"
            .to_string(),
    ];
    out.extend(
        lines
            .iter()
            .take(max_lines)
            .map(|l| format!("  {}", one_line(l.trim(), CONTEXT_LINE_MAX_CHARS))),
    );
    if lines.len() > max_lines {
        out.push(omitted(lines.len() - max_lines, "line"));
    }
    out.join("\n")
}

/// `  … {n} {what}s omitted`.
fn omitted(n: usize, what: &str) -> String {
    if n == 1 {
        format!("  … 1 {what} omitted")
    } else {
        format!("  … {n} {what}s omitted")
    }
}

/// A non-blank string field of a JSON object.
fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// The last `last_n` breadcrumbs of an event's trail, oldest first (the
/// order they are stored in, and the order the dashboard lists them): a
/// header line, then `  {HH:MM:SS.mmm} {category}[ ({level})]: {summary}`.
///
/// The summary is the dashboard's `breadcrumbSummary`: the message, else the
/// data flattened to `k: v` pairs, else the category. The level is shown
/// unless it is `info`, the default. Each summary is clipped.
pub(crate) fn render_breadcrumbs(v: &Value, last_n: usize) -> String {
    let crumbs: &[Value] = match v {
        Value::Array(a) if !a.is_empty() => a,
        _ => return "Breadcrumbs: none recorded.".into(),
    };
    let header = if crumbs.len() > last_n {
        format!(
            "Breadcrumbs (last {last_n} of {}, oldest first):",
            crumbs.len()
        )
    } else {
        format!("Breadcrumbs ({}, oldest first):", crumbs.len())
    };
    let mut out = vec![header];
    out.extend(
        crumbs[crumbs.len().saturating_sub(last_n)..]
            .iter()
            .map(breadcrumb_line),
    );
    out.join("\n")
}

fn breadcrumb_line(b: &Value) -> String {
    let time = match str_field(b, "timestamp") {
        Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
            Ok(t) => t
                .with_timezone(&chrono::Utc)
                .format("%H:%M:%S%.3f")
                .to_string(),
            Err(_) => one_line(ts, 40),
        },
        None => "--:--:--".into(),
    };
    let ty = str_field(b, "type");
    let category = str_field(b, "category").or(ty).unwrap_or("breadcrumb");
    let level = str_field(b, "level")
        .filter(|l| !l.eq_ignore_ascii_case("info"))
        .map(|l| format!(" ({})", one_line(l, 20)))
        .unwrap_or_default();
    let data = match b.get("data") {
        Some(Value::Object(m)) if !m.is_empty() => Some(
            m.iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}: {s}"),
                    other => format!("{k}: {other}"),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    };
    let summary = str_field(b, "message")
        .map(str::to_string)
        .or(data)
        .unwrap_or_else(|| category.to_string());
    format!(
        "  {time} {}{level}: {}",
        one_line(category, 80),
        one_line(&summary, BREADCRUMB_MAX_CHARS)
    )
}

/// A key/value block (tags, a context object): one `  {key}: {value}` line
/// per entry, keys sorted, at most `max_entries` of them and then
/// `  … {n} more`. String values are shown as-is, anything else as compact
/// JSON; keys and values go through [`one_line`], values clipped to
/// `max_len`. The empty string when there is nothing to show (not an object,
/// or an empty one), so the caller decides whether to print a heading.
pub(crate) fn render_kv(v: &Value, max_entries: usize, max_len: usize) -> String {
    let Value::Object(map) = v else {
        return String::new();
    };
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut out: Vec<String> = entries
        .iter()
        .take(max_entries)
        .map(|(k, v)| {
            let value = match v {
                Value::String(s) => one_line(s, max_len),
                other => one_line(&other.to_string(), max_len),
            };
            format!("  {}: {value}", one_line(k, max_len))
        })
        .collect();
    if entries.len() > max_entries {
        out.push(format!("  … {} more", entries.len() - max_entries));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: &[&str] = &["1h", "24h", "7d", "14d", "30d"];

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().expect("timestamp")
    }

    #[test]
    fn rollup_lag_note_only_past_the_tolerance_or_without_a_watermark() {
        let now = at("2026-11-10T12:00:00Z");
        let tolerance = rollup_lag_tolerance(60, 60);
        let note = |as_of| rollup_lag_note("Counts", as_of, now, tolerance);
        // A healthy fold trails by a minute or two: nothing to say.
        assert_eq!(note(Some(now)), None);
        assert_eq!(note(Some(at("2026-11-10T11:58:00Z"))), None);
        assert_eq!(
            note(Some(now - tolerance)),
            None,
            "exactly at the tolerance is still current"
        );
        assert_eq!(
            note(Some(at("2026-11-10T11:54:30Z"))).as_deref(),
            Some(
                "Counts as of 2026-11-10 11:54 UTC (the rollup is behind; newer events are not \
                 counted yet)"
            )
        );
        let missing = note(None).expect("a missing watermark is a note");
        assert!(
            missing.starts_with("Counts as of an unknown time"),
            "{missing}"
        );
        assert!(!missing.ends_with('.'), "{missing}");
        let events = rollup_lag_note("Event counts", None, now, tolerance).expect("note");
        assert!(
            events.starts_with("Event counts as of an unknown time"),
            "{events}"
        );
    }

    /// The tolerance follows the configured fold cadence: five minutes at the
    /// default 60 s + 60 s, and `fold + lag + 60 s` for a slower one, so a
    /// healthy slow fold is not reported as behind.
    #[test]
    fn rollup_lag_tolerance_follows_a_slow_cadence() {
        use chrono::Duration;
        assert_eq!(rollup_lag_tolerance(60, 60), Duration::minutes(5));
        assert_eq!(rollup_lag_tolerance(0, -30), Duration::minutes(5));
        // ROLLUP_FOLD_SECS=600, ROLLUP_LAG_SECS=300: 600 + 300 + 60.
        let slow = rollup_lag_tolerance(600, 300);
        assert_eq!(slow, Duration::seconds(960));
        let now = at("2026-11-10T12:00:00Z");
        // Fifteen minutes behind is normal at that cadence, not lag...
        let wm = now - Duration::minutes(15);
        assert_eq!(rollup_lag_note("Counts", Some(wm), now, slow), None);
        // ...while the default cadence calls the same watermark behind.
        assert!(rollup_lag_note("Counts", Some(wm), now, rollup_lag_tolerance(60, 60)).is_some());
        assert!(rollup_lag_note("Counts", Some(now - Duration::minutes(17)), now, slow).is_some());
        assert_eq!(rollup_lag_tolerance(u64::MAX, i64::MAX), Duration::MAX);
    }

    #[test]
    fn below_tier_floor_only_when_the_window_starts_before_it() {
        let floor = at("2026-11-01T00:00:00Z");
        assert_eq!(below_tier_floor(floor, None), None);
        assert_eq!(below_tier_floor(floor, Some(floor)), None);
        assert_eq!(
            below_tier_floor(at("2026-11-02T00:00:00Z"), Some(floor)),
            None
        );
        assert_eq!(
            below_tier_floor(at("2026-10-31T23:59:59Z"), Some(floor)),
            Some(floor)
        );
        let note = hot_store_note("users", floor);
        assert!(note.contains("before 2026-11-01 00:00 UTC"), "{note}");
        assert!(note.contains(": users read hot events only"), "{note}");
    }

    #[test]
    fn env_labels_name_only_what_the_caller_can_see() {
        let prod = Uuid::new_v4();
        let hidden = Uuid::new_v4();
        let names: HashMap<Uuid, String> = [(prod, "production".to_string())].into();
        assert_eq!(env_label(&names, Some(prod)), "production");
        assert_eq!(env_label(&names, Some(hidden)), hidden.to_string());
        assert_eq!(env_label(&names, None), "unattributed");
    }

    #[test]
    fn device_line_takes_what_the_sdk_reported() {
        let full = serde_json::json!({
            "device": {"model": "Pixel 7", "family": "Pixel", "arch": "arm64"},
            "os": {"name": "Android", "version": "14"},
            "runtime": {"name": "Dart", "version": "3.5.0"},
            "app": {"version": "1.4.2", "build": "1402"},
            "user": {"email": "someone@example.com"},
        });
        assert_eq!(
            device_line(&full).as_deref(),
            Some("Device: Pixel 7 (arm64) · OS: Android 14 · Runtime: Dart 3.5.0 · App: 1.4.2 (1402)")
        );
        // The JS SDK's nulls, and a family without a model.
        let sparse = serde_json::json!({
            "device": {"family": "Chrome", "model": null},
            "os": {"name": null, "version": null},
        });
        assert_eq!(device_line(&sparse).as_deref(), Some("Device: Chrome"));
        assert_eq!(device_line(&serde_json::json!({})), None);
        assert_eq!(device_line(&Value::Null), None);
        assert_eq!(
            os_label(&serde_json::json!({"os": {"name": "iOS"}})).as_deref(),
            Some("iOS")
        );
    }

    #[test]
    fn device_model_prefers_the_model_then_the_family() {
        use serde_json::json;
        assert_eq!(
            device_model(&json!({"device": {"model": "Pixel 7", "family": "Pixel"}})).as_deref(),
            Some("Pixel 7")
        );
        assert_eq!(
            device_model(&json!({"device": {"model": "  ", "family": "iPhone"}})).as_deref(),
            Some("iPhone")
        );
        assert_eq!(device_model(&json!({"os": {"name": "iOS"}})), None);
        assert_eq!(
            device_model(&json!({"device": {"model": "a\nb"}})).as_deref(),
            Some("a b"),
            "one line"
        );
    }

    #[test]
    fn parse_window_accepts_allowed_and_lists_them_on_error() {
        assert_eq!(
            parse_window(None, ALLOWED, "24h").unwrap(),
            chrono::Duration::hours(24)
        );
        assert_eq!(
            parse_window(Some("1h"), ALLOWED, "24h").unwrap(),
            chrono::Duration::hours(1)
        );
        assert_eq!(
            parse_window(Some(" 7d "), ALLOWED, "24h").unwrap(),
            chrono::Duration::days(7)
        );
        assert_eq!(
            parse_window(Some("30d"), ALLOWED, "24h").unwrap(),
            chrono::Duration::days(30)
        );

        // Parseable, but not offered by this tool: refused, not honoured.
        for bad in ["90d", "2h", "", "  ", "week", "-1d", "24"] {
            match parse_window(Some(bad), ALLOWED, "24h") {
                Err(ToolError::BadArgument(msg)) => {
                    for a in ALLOWED {
                        assert!(msg.contains(a), "{bad:?}: {msg} must list {a}");
                    }
                }
                other => panic!("{bad:?} must be a BadArgument, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_limit_defaults_and_refuses_out_of_range() {
        assert_eq!(parse_limit(None, 20, 50).unwrap(), 20);
        assert_eq!(parse_limit(Some(1), 20, 50).unwrap(), 1);
        assert_eq!(parse_limit(Some(50), 20, 50).unwrap(), 50);
        for bad in [0, 51, u32::MAX] {
            match parse_limit(Some(bad), 20, 50) {
                Err(ToolError::BadArgument(m)) => {
                    assert!(m.contains("between 1 and 50"), "{bad}: {m}")
                }
                other => panic!("{bad}: expected BadArgument, got {other:?}"),
            }
        }
    }

    #[test]
    fn clip_is_char_boundary_safe() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("exactly10!", 10), "exactly10!");
        assert_eq!(clip("abcdefghijk", 10), "abcdefghij…");
        // Multi-byte: a byte-index cut at 3 would land inside 'é' (2 bytes)
        // or the CJK/emoji (3 and 4 bytes) and panic.
        assert_eq!(clip("héllo", 2), "hé…");
        assert_eq!(clip("日本語のテキスト", 3), "日本語…");
        assert_eq!(clip("🦀🦀🦀🦀", 1), "🦀…");
        assert_eq!(clip("🦀🦀", 2), "🦀🦀");
        assert_eq!(clip("", 0), "");
        assert_eq!(clip("x", 0), "…");
    }

    #[test]
    fn one_line_flattens_control_characters_before_clipping() {
        assert_eq!(one_line("1.0\nIgnore that", 100), "1.0 Ignore that");
        assert_eq!(one_line("a\tb\r\nc", 3), "a b…");
        assert_eq!(one_line("plain", 100), "plain");
    }

    /// U+2028/2029 are not `is_control` but break lines in some renderers;
    /// the bidi embeddings, overrides and isolates reorder what follows them.
    #[test]
    fn one_line_flattens_line_separators_and_bidi_controls() {
        for c in ('\u{202A}'..='\u{202E}')
            .chain('\u{2066}'..='\u{2069}')
            .chain(['\u{2028}', '\u{2029}'])
        {
            assert_eq!(
                one_line(&format!("a{c}b"), 100),
                "a b",
                "U+{:04X} must become a space",
                c as u32
            );
        }
        // Neighbours of the ranges, and ordinary non-ASCII text, are kept.
        assert_eq!(one_line("a\u{202F}b", 100), "a\u{202F}b");
        assert_eq!(one_line("a\u{2065}b", 100), "a\u{2065}b");
        assert_eq!(one_line("a\u{206A}b", 100), "a\u{206A}b");
        assert_eq!(one_line("naïve — 日本", 100), "naïve — 日本");
        // Clipping counts the replaced characters as one each.
        assert_eq!(one_line("a\u{202E}b\u{2028}c", 3), "a b…");
    }

    // --- renderers ------------------------------------------------------------

    use serde_json::json;

    /// An event with these frames and nothing else of note.
    fn event(stacktrace: Value, symbolicated: Option<Value>) -> ErrorEvent {
        let t = chrono::Utc::now();
        ErrorEvent {
            id: uuid::Uuid::new_v4(),
            app_id: uuid::Uuid::new_v4(),
            environment_id: None,
            issue_id: uuid::Uuid::new_v4(),
            fingerprint: "fp".into(),
            level: "error".into(),
            message: "boom".into(),
            exception_type: "TypeError".into(),
            exception_value: "boom".into(),
            stacktrace,
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: None,
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: t,
            received_at: t,
            session_id: None,
            device_key: None,
            screen: None,
            stacktrace_symbolicated: symbolicated,
            symbolication_status: "pending".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: None,
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        }
    }

    /// The rendered frame lines (`  at …`), in order.
    fn frame_lines(out: &str) -> Vec<&str> {
        out.lines()
            .filter(|l| l.trim_start().starts_with("at "))
            .collect()
    }

    /// Review focus 5: a 500-frame trace whose only in-app frames sit deep
    /// inside it. Stored oldest first (the crash is the LAST element), shown
    /// most recent call first, so stored indexes 400..=402 are display
    /// positions 97..=99: far past the first 50, and kept anyway.
    #[test]
    fn stacktrace_truncates_to_50_keeping_in_app_frames() {
        let frames: Vec<Value> = (0..500)
            .map(|i| {
                let in_app = (400..=402).contains(&i);
                let file = if in_app {
                    format!("lib/app/screen_{i}.dart")
                } else {
                    format!("node_modules/vendor/f{i}.js")
                };
                json!({"function": format!("fn{i}"), "filename": file, "lineno": i + 1, "colno": 3, "in_app": in_app})
            })
            .collect();
        let out = render_stacktrace(&event(Value::Array(frames), None), MAX_FRAMES);
        for i in 400..=402 {
            assert!(
                out.contains(&format!("lib/app/screen_{i}.dart:{}:3) [app]", i + 1)),
                "in-app frame {i} must survive:\n{out}"
            );
        }
        let lines = frame_lines(&out);
        assert!(lines.len() <= 51, "{} frame lines:\n{out}", lines.len());
        assert_eq!(lines.len(), 50, "{out}");
        assert!(out.contains("frames omitted"), "{out}");
        // The rest are the frames nearest the crash: stored 499 down to 453.
        assert!(
            lines[0].contains("fn499 (node_modules/vendor/f499.js:500:3)"),
            "the crash frame comes first: {}",
            lines[0]
        );
        assert!(lines[46].contains("fn453 "), "{}", lines[46]);
        // Display 47..=96 (stored 452 down to 403) dropped between them, and
        // display 100..=499 (stored 399 down to 0) after the in-app ones.
        assert!(out.contains("  … 50 frames omitted"), "{out}");
        assert!(out.contains("  … 400 frames omitted"), "{out}");
        assert!(
            out.lines()
                .next()
                .unwrap()
                .contains("most recent call first"),
            "{out}"
        );
        assert!(out.lines().next().unwrap().contains("500 frames"), "{out}");
        // Bounded: no line anywhere near a frame's worth of padding.
        assert!(out.lines().all(|l| l.chars().count() < 400), "{out}");
    }

    /// Review focus 5: a 50,000-character minified source line is clipped
    /// to 200 characters plus the `      > ` lead and the `…`.
    #[test]
    fn context_line_is_clipped() {
        let minified = "x".repeat(50_000);
        let sym = json!([{
            "function": "a", "filename": "app.min.js", "lineno": 1, "colno": 48211,
            "in_app": true, "symbolicated": true, "context_line": minified,
        }]);
        let out = render_stacktrace(&event(json!([]), Some(sym)), MAX_FRAMES);
        let line = out
            .lines()
            .find(|l| l.trim_start().starts_with("> "))
            .unwrap_or_else(|| panic!("no source line:\n{out}"));
        assert!(
            line.chars().count() <= 200 + 9,
            "{} chars",
            line.chars().count()
        );
        assert!(line.ends_with('…'), "{line}");
        assert!(line.starts_with("      > x"), "{line}");
    }

    /// Symbolicated frames win over the raw ones, as on the dashboard; an
    /// array with nothing resolved in it (symbolication can store `[]`) does
    /// not, and the raw frames are shown instead, labelled as such.
    #[test]
    fn symbolicated_frames_preferred() {
        let raw = json!([{"function": "a.b", "filename": "app.min.js", "lineno": 1, "colno": 9, "in_app": true}]);
        let sym = json!([{
            "function": "submitOrder", "filename": "src/checkout/cart.ts", "lineno": 42,
            "colno": 7, "in_app": true, "symbolicated": true,
            "context_line": "  const total = cart.items[0].price;",
        }]);
        let out = render_stacktrace(&event(raw.clone(), Some(sym)), MAX_FRAMES);
        assert!(
            out.contains("  at submitOrder (src/checkout/cart.ts:42:7) [app]"),
            "{out}"
        );
        assert!(
            out.contains("      > const total = cart.items[0].price;"),
            "{out}"
        );
        assert!(!out.contains("app.min.js"), "{out}");
        assert!(
            out.lines().next().unwrap().contains("symbolicated"),
            "{out}"
        );

        for unresolved in [
            json!([]),
            json!([{"function": "a.b", "filename": "app.min.js", "lineno": 1, "colno": 9, "symbolicated": false}]),
        ] {
            let out = render_stacktrace(&event(raw.clone(), Some(unresolved)), MAX_FRAMES);
            assert!(out.contains("  at a.b (app.min.js:1:9) [app]"), "{out}");
            assert!(
                !out.lines().next().unwrap().contains("(symbolicated"),
                "{out}"
            );
        }
    }

    /// The location forms the dashboard prints, and the placeholders for a
    /// missing function or file. Not in-app: no marker.
    #[test]
    fn frame_location_forms() {
        let raw = json!([
            {"filename": "a.js", "lineno": 3},
            {"function": "f", "module": "pkg.mod"},
            {"function": "", "abs_path": "/srv/app/x.py", "lineno": 7, "colno": 1, "in_app": false},
            {"function": "g\nfake output"},
        ]);
        let out = render_stacktrace(&event(raw, None), MAX_FRAMES);
        let lines = frame_lines(&out);
        // Most recent first: the stored LAST frame is printed first.
        assert_eq!(
            lines,
            vec![
                "  at g fake output (<unknown file>)",
                "  at <anonymous> (/srv/app/x.py:7:1)",
                "  at f (pkg.mod)",
                "  at <anonymous> (a.js:3)",
            ],
            "{out}"
        );
    }

    /// No frames: a raw (obfuscated Dart) trace is shown as the dashboard
    /// shows it, line by line and bounded; with neither, it says so.
    #[test]
    fn raw_trace_and_empty_trace() {
        let mut ev = event(json!([]), None);
        let raw: Vec<String> = (0..80)
            .map(|i| format!("#{i:02} abs 0x{i:x} virt 0x{i:x}"))
            .collect();
        ev.debug_meta = Some(json!({"raw_stacktrace": raw.join("\n")}));
        let out = render_stacktrace(&ev, MAX_FRAMES);
        assert!(out.contains("not symbolicated"), "{out}");
        assert!(out.contains("  #00 abs 0x0"), "{out}");
        assert!(out.contains("  #49 "), "{out}");
        assert!(!out.contains("  #50 "), "{out}");
        assert!(out.contains("  … 30 lines omitted"), "{out}");

        let out = render_stacktrace(&event(json!([]), None), MAX_FRAMES);
        assert_eq!(out, "Stack trace: none recorded for this event.");
        let out = render_stacktrace(&event(Value::Null, None), MAX_FRAMES);
        assert_eq!(out, "Stack trace: none recorded for this event.");
    }

    #[test]
    fn render_kv_caps_entries_and_value_length() {
        let mut map = serde_json::Map::new();
        for i in 0..40 {
            map.insert(format!("key{i:02}"), json!("v".repeat(1000)));
        }
        map.insert("a_number".into(), json!(42));
        map.insert("an_object".into(), json!({"nested": "x\ny"}));
        let out = render_kv(&Value::Object(map), MAX_KV_ENTRIES, MAX_KV_VALUE_CHARS);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 31, "30 entries and the overflow line:\n{out}");
        assert_eq!(lines[30], "  … 12 more", "{out}");
        // Sorted keys, so the first entries are the non-`key` ones.
        assert_eq!(lines[0], "  a_number: 42");
        assert_eq!(lines[1], r#"  an_object: {"nested":"x\ny"}"#);
        let long = lines[2];
        assert!(long.starts_with("  key00: vvv"), "{long}");
        assert!(long.ends_with('…'), "{long}");
        assert_eq!(long.chars().count(), "  key00: ".len() + 200 + 1, "{long}");

        // Control characters cannot break the layout.
        let out = render_kv(&json!({"k\nx": "line1\nline2"}), 30, 200);
        assert_eq!(out, "  k x: line1 line2");
        // Nothing to show is the empty string.
        for empty in [json!({}), Value::Null] {
            assert_eq!(render_kv(&empty, 30, 200), "");
        }
    }

    #[test]
    fn breadcrumbs_show_the_last_n_oldest_first() {
        let crumbs: Vec<Value> = (0..25)
            .map(|i| {
                json!({
                    "type": "default", "category": "ui.tap", "level": "info",
                    "message": format!("tap {i}"),
                    "timestamp": format!("2026-11-09T14:00:{i:02}.250Z"),
                    "data": {},
                })
            })
            .collect();
        let mut crumbs = crumbs;
        crumbs.push(json!({
            "type": "navigation", "category": null, "message": null, "level": "warning",
            "timestamp": "2026-11-09T14:01:00Z", "data": {"from": "/cart", "to": "/pay"},
        }));
        let out = render_breadcrumbs(&Value::Array(crumbs), MAX_BREADCRUMBS);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0], "Breadcrumbs (last 20 of 26, oldest first):",
            "{out}"
        );
        assert_eq!(lines.len(), 21, "{out}");
        assert_eq!(lines[1], "  14:00:06.250 ui.tap: tap 6", "{out}");
        assert_eq!(
            lines[20], "  14:01:00.000 navigation (warning): from: /cart, to: /pay",
            "{out}"
        );
        assert!(!out.contains("tap 5"), "{out}");

        let out = render_breadcrumbs(
            &json!([{"type": "http", "message": "GET /x", "timestamp": "bad"}]),
            20,
        );
        assert_eq!(out, "Breadcrumbs (1, oldest first):\n  bad http: GET /x");
        for none in [json!([]), Value::Null] {
            assert_eq!(render_breadcrumbs(&none, 20), "Breadcrumbs: none recorded.");
        }
        // A long message is clipped.
        let long = json!([{"type": "log", "message": "m".repeat(5000), "timestamp": "2026-11-09T14:00:00Z"}]);
        let out = render_breadcrumbs(&long, 20);
        assert!(out.lines().nth(1).unwrap().chars().count() < 260, "{out}");
    }

    #[test]
    fn window_duration_parses_only_positive_hours_and_days() {
        assert_eq!(window_duration("90d"), Some(chrono::Duration::days(90)));
        assert_eq!(window_duration("1h"), Some(chrono::Duration::hours(1)));
        for bad in ["", "h", "0d", "-1d", "7w", "d7", "1.5h", "7é"] {
            assert_eq!(window_duration(bad), None, "{bad:?}");
        }
    }
}
