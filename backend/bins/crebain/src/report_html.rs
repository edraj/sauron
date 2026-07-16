//! Renders a run's [`Summary`] + timeline to a single self-contained HTML file:
//! inline CSS, hand-drawn inline SVG line charts, native `<title>` hover
//! tooltips, zero JS and zero network requests.

use std::path::Path;

use crate::cli::Expected;
use crate::metrics::Summary;
use crate::report::{fmt_us, group, total};

/// Run context for the report header (everything not on `Summary`).
pub struct ReportMeta {
    pub mode_label: String,
    pub users: usize,
    pub duration_secs: u64,
    pub events_per_min: u32,
    pub issues_per_min: u32,
    pub gzip: bool,
    pub generated_at: String,
    pub ncpus: usize,
}

// Plot geometry (SVG user units). viewBox is 0 0 720 260; the plot area is inset
// for axis labels.
const VB_W: f64 = 720.0;
const VB_H: f64 = 260.0;
const PLOT_LEFT: f64 = 56.0;
const PLOT_RIGHT: f64 = 704.0;
const PLOT_TOP: f64 = 16.0;
const PLOT_BOTTOM: f64 = 220.0;

/// Map a time value (0..=x_max) to an x pixel; x_max ≤ 0 pins to the left edge.
fn map_x(t: f64, x_max: f64, left: f64, right: f64) -> f64 {
    if x_max <= 0.0 {
        left
    } else {
        left + (t / x_max) * (right - left)
    }
}

/// Map a value (0..=y_max) to a y pixel (0 → bottom, y_max → top); y_max ≤ 0
/// pins to the bottom edge (avoids NaN on a flat-zero series).
fn map_y(v: f64, y_max: f64, top: f64, bottom: f64) -> f64 {
    if y_max <= 0.0 {
        bottom
    } else {
        bottom - (v / y_max) * (bottom - top)
    }
}

struct Series<'a> {
    name: &'a str,
    color: &'a str,
    /// (t_secs, value)
    points: Vec<(f64, f64)>,
}

/// A labelled, gridded SVG line chart. `fmt_y` formats a y value for the axis
/// labels and tooltips (e.g. cores, MB, req/s). Returns a `<figure>…</figure>`.
fn line_chart(title: &str, x_max: f64, series: &[Series], fmt_y: &dyn Fn(f64) -> String) -> String {
    // Escape the caption / aria-label / tooltip text defensively, so a future
    // dynamic label routed through here can never break out of the SVG/HTML.
    let title = esc(title);
    let has_points = series.iter().any(|s| !s.points.is_empty());
    if !has_points {
        return format!(
            "<figure class=\"chart\"><figcaption>{title}</figcaption>\
             <div class=\"nodata\">no data</div></figure>"
        );
    }
    let y_max_raw = series
        .iter()
        .flat_map(|s| s.points.iter().map(|&(_, v)| v))
        .fold(0.0_f64, f64::max);
    let y_max = if y_max_raw <= 0.0 { 1.0 } else { y_max_raw * 1.1 };

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg viewBox=\"0 0 {VB_W} {VB_H}\" preserveAspectRatio=\"xMidYMid meet\" \
         role=\"img\" aria-label=\"{title}\">"
    ));
    // Horizontal gridlines + y labels (5 rows).
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = PLOT_BOTTOM - frac * (PLOT_BOTTOM - PLOT_TOP);
        let val = frac * y_max;
        svg.push_str(&format!(
            "<line class=\"grid\" x1=\"{PLOT_LEFT}\" y1=\"{y:.1}\" x2=\"{PLOT_RIGHT}\" y2=\"{y:.1}\"/>\
             <text class=\"ylab\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
            PLOT_LEFT - 6.0,
            y + 3.0,
            fmt_y(val),
        ));
    }
    // X axis labels: 0 and x_max.
    svg.push_str(&format!(
        "<text class=\"xlab\" x=\"{PLOT_LEFT}\" y=\"{:.1}\">0s</text>\
         <text class=\"xlab xend\" x=\"{PLOT_RIGHT}\" y=\"{:.1}\">{:.0}s</text>",
        PLOT_BOTTOM + 18.0,
        PLOT_BOTTOM + 18.0,
        x_max,
    ));
    // One polyline + hover dots per series.
    for s in series {
        if s.points.is_empty() {
            continue;
        }
        let pts = s
            .points
            .iter()
            .map(|&(t, v)| {
                format!(
                    "{:.1},{:.1}",
                    map_x(t, x_max, PLOT_LEFT, PLOT_RIGHT),
                    map_y(v, y_max, PLOT_TOP, PLOT_BOTTOM)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        svg.push_str(&format!(
            "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"2\" points=\"{pts}\"/>",
            s.color
        ));
        for &(t, v) in &s.points {
            svg.push_str(&format!(
                "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.5\" fill=\"{}\">\
                 <title>t={:.0}s · {}: {}</title></circle>",
                map_x(t, x_max, PLOT_LEFT, PLOT_RIGHT),
                map_y(v, y_max, PLOT_TOP, PLOT_BOTTOM),
                s.color,
                t,
                esc(s.name),
                fmt_y(v),
            ));
        }
    }
    svg.push_str("</svg>");

    let legend = series
        .iter()
        .map(|s| {
            format!(
                "<span class=\"lg\"><i style=\"background:{}\"></i>{}</span>",
                s.color,
                esc(s.name)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        "<figure class=\"chart\"><figcaption>{title}</figcaption>{svg}\
         <div class=\"legend\">{legend}</div></figure>"
    )
}

/// Minimal HTML text escaping for the few dynamic strings we interpolate.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn stat_card(label: &str, value: &str) -> String {
    format!("<div class=\"card\"><div class=\"v\">{value}</div><div class=\"l\">{label}</div></div>")
}

/// Build the complete HTML document.
pub fn render(summary: &Summary, expected: &Expected, meta: &ReportMeta) -> String {
    let s = summary;
    let secs = s.elapsed.as_secs_f64().max(1e-9);
    let achieved_rps = s.requests as f64 / secs;
    let target_rps = expected.requests / expected.duration_secs.max(1e-9);
    let accept_pct = if s.requests == 0 {
        "—".to_string()
    } else {
        format!("{:.1}%", 100.0 * s.accepted as f64 / s.requests as f64)
    };
    let failed = s.rate_limited + s.http_errors + s.transport;

    let has_resources = s.timeline.iter().any(|t| t.cpu_cores.is_some() || t.rss_bytes.is_some());
    let peak_cpu = s
        .timeline
        .iter()
        .filter_map(|t| t.cpu_cores)
        .fold(0.0_f64, f64::max);
    let peak_rss = s.timeline.iter().filter_map(|t| t.rss_bytes).max().unwrap_or(0);
    let x_max = s.timeline.last().map(|t| t.t_secs).unwrap_or(0.0);

    // ---- charts ----
    let rps_chart = line_chart(
        "Requests / sec",
        x_max,
        &[Series {
            name: "req/s",
            color: "#4f8cff",
            points: s.timeline.iter().map(|t| (t.t_secs, t.interval_rate)).collect(),
        }],
        &|v| format!("{v:.0}"),
    );
    let cum_chart = line_chart(
        "Records — cumulative (success vs fail)",
        x_max,
        &[
            Series {
                name: "accepted",
                color: "#2ecc71",
                points: s.timeline.iter().map(|t| (t.t_secs, t.cum_accepted as f64)).collect(),
            },
            Series {
                name: "failed",
                color: "#e74c3c",
                points: s.timeline.iter().map(|t| (t.t_secs, t.cum_failed as f64)).collect(),
            },
        ],
        &|v| group(v as u64),
    );
    let interval_chart = line_chart(
        "Records / sec (success vs fail)",
        x_max,
        &[
            Series {
                name: "accepted/s",
                color: "#2ecc71",
                points: s.timeline.iter().map(|t| (t.t_secs, t.interval_accepted as f64)).collect(),
            },
            Series {
                name: "failed/s",
                color: "#e74c3c",
                points: s.timeline.iter().map(|t| (t.t_secs, t.interval_failed as f64)).collect(),
            },
        ],
        &|v| format!("{v:.0}"),
    );

    let mut resource_charts = String::new();
    if has_resources {
        resource_charts.push_str(&line_chart(
            "CPU (cores)",
            x_max,
            &[Series {
                name: "cores",
                color: "#f39c12",
                points: s
                    .timeline
                    .iter()
                    .filter_map(|t| t.cpu_cores.map(|c| (t.t_secs, c)))
                    .collect(),
            }],
            &|v| format!("{v:.2}"),
        ));
        resource_charts.push_str(&line_chart(
            "Memory (RSS)",
            x_max,
            &[Series {
                name: "RSS MB",
                color: "#9b59b6",
                points: s
                    .timeline
                    .iter()
                    .filter_map(|t| t.rss_bytes.map(|b| (t.t_secs, b as f64 / 1_048_576.0)))
                    .collect(),
            }],
            &|v| format!("{v:.0}"),
        ));
    }

    // ---- stat cards ----
    let mut cards = String::new();
    cards.push_str(&stat_card("total requests", &group(s.requests)));
    cards.push_str(&stat_card(
        "req/s achieved",
        &format!("{}<span class=\"sub\"> / {} target</span>", group(achieved_rps.round() as u64), group(target_rps.round() as u64)),
    ));
    cards.push_str(&stat_card("accepted", &group(s.accepted)));
    cards.push_str(&stat_card("failed", &group(failed)));
    cards.push_str(&stat_card("accept rate", &accept_pct));
    cards.push_str(&stat_card("latency p50", &fmt_us(s.p50_us)));
    cards.push_str(&stat_card("latency p90", &fmt_us(s.p90_us)));
    cards.push_str(&stat_card("latency p99", &fmt_us(s.p99_us)));
    cards.push_str(&stat_card("latency max", &fmt_us(s.max_us)));
    if has_resources {
        cards.push_str(&stat_card(
            "peak CPU",
            &format!("{peak_cpu:.2}<span class=\"sub\"> / {} cores</span>", meta.ncpus),
        ));
        cards.push_str(&stat_card("peak RSS", &format!("{} MB", peak_rss / 1_048_576)));
    }

    // ---- tables ----
    let pct = |num: u64, den: u64| {
        if den == 0 {
            "—".to_string()
        } else {
            format!("{:.1}%", 100.0 * num as f64 / den as f64)
        }
    };
    let outcomes = format!(
        "<table><thead><tr><th>outcome</th><th>count</th><th>share</th></tr></thead><tbody>\
         <tr><td>accepted (2xx)</td><td>{}</td><td>{}</td></tr>\
         <tr><td>rate-limited</td><td>{}</td><td>{}</td></tr>\
         <tr><td>http errors</td><td>{}</td><td>{}</td></tr>\
         <tr><td>transport errors</td><td>{}</td><td>{}</td></tr>\
         </tbody></table>",
        group(s.accepted), pct(s.accepted, s.requests),
        group(s.rate_limited), pct(s.rate_limited, s.requests),
        group(s.http_errors), pct(s.http_errors, s.requests),
        group(s.transport), pct(s.transport, s.requests),
    );
    let item_row = |label: &str, a: u64, at: u64| {
        format!("<tr><td>{label}</td><td>{}</td><td>{}</td></tr>", group(a), group(at))
    };
    let items = format!(
        "<table><thead><tr><th>signal</th><th>accepted</th><th>attempted</th></tr></thead><tbody>\
         {}{}{}{}{}{}</tbody></table>",
        item_row("errors", s.accepted_items.errors, s.attempted.errors),
        item_row("events", s.accepted_items.events, s.attempted.events),
        item_row("transactions", s.accepted_items.transactions, s.attempted.transactions),
        item_row("identifies", s.accepted_items.identifies, s.attempted.identifies),
        item_row("breadcrumbs", s.accepted_items.breadcrumbs, s.attempted.breadcrumbs),
        item_row("total", total(&s.accepted_items), total(&s.attempted)),
    );
    let status_codes = if s.status_counts.is_empty() {
        String::new()
    } else {
        let codes = s
            .status_counts
            .iter()
            .map(|(c, n)| format!("<code>{c}</code>×{}", group(*n)))
            .collect::<Vec<_>>()
            .join(" &nbsp; ");
        format!("<p class=\"codes\">status codes: {codes}</p>")
    };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>crebain report — {mode}</title><style>{css}</style></head><body>\
         <header><h1>crebain <span>benchmark report</span></h1>\
         <p class=\"meta\">{mode} · {users} users · {dur}s · events {epm}/min · issues {ipm}/min · gzip {gz} · {gen}</p>\
         </header>\
         <section class=\"cards\">{cards}</section>\
         <section class=\"charts\">{rps}{cum}{interval}{resources}</section>\
         <section class=\"tables\"><div><h2>Outcomes</h2>{outcomes}{codes}</div>\
         <div><h2>Items (accepted / attempted)</h2>{items}</div></section>\
         <footer>Generated by crebain. CPU sampled once/sec from /proc; 1.0 core = one full CPU. Charts are static SVG — hover a point for its value.</footer>\
         </body></html>",
        mode = esc(&meta.mode_label),
        css = CSS,
        users = meta.users,
        dur = meta.duration_secs,
        epm = meta.events_per_min,
        ipm = meta.issues_per_min,
        gz = if meta.gzip { "on" } else { "off" },
        gen = esc(&meta.generated_at),
        cards = cards,
        rps = rps_chart,
        cum = cum_chart,
        interval = interval_chart,
        resources = resource_charts,
        outcomes = outcomes,
        codes = status_codes,
        items = items,
    )
}

/// Render and write the report to `path`.
pub fn write(
    path: &Path,
    summary: &Summary,
    expected: &Expected,
    meta: &ReportMeta,
) -> anyhow::Result<()> {
    let html = render(summary, expected, meta);
    std::fs::write(path, html)
        .map_err(|e| anyhow::anyhow!("write report {}: {e}", path.display()))
}

const CSS: &str = "\
:root{--bg:#f7f8fa;--fg:#1a1c20;--mut:#666;--card:#fff;--bd:#e3e6ea;--grid:#e8ebef}\
@media(prefers-color-scheme:dark){:root{--bg:#14161a;--fg:#e6e8eb;--mut:#9aa0a8;--card:#1d2026;--bd:#2a2e36;--grid:#262a31}}\
*{box-sizing:border-box}body{margin:0;padding:24px;font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg)}\
header h1{margin:0;font-size:22px}header h1 span{color:var(--mut);font-weight:400}\
.meta{color:var(--mut);margin:4px 0 20px}\
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px;margin-bottom:24px}\
.card{background:var(--card);border:1px solid var(--bd);border-radius:10px;padding:14px}\
.card .v{font-size:22px;font-weight:600}.card .v .sub{font-size:13px;color:var(--mut);font-weight:400}\
.card .l{color:var(--mut);font-size:12px;margin-top:2px;text-transform:uppercase;letter-spacing:.04em}\
.charts{display:grid;grid-template-columns:repeat(auto-fit,minmax(340px,1fr));gap:16px;margin-bottom:24px}\
.chart{background:var(--card);border:1px solid var(--bd);border-radius:10px;padding:14px;margin:0}\
.chart figcaption{font-weight:600;margin-bottom:8px}\
.chart svg{width:100%;height:auto}\
.nodata{color:var(--mut);padding:40px;text-align:center}\
.grid{stroke:var(--grid);stroke-width:1}.ylab{fill:var(--mut);font-size:10px;text-anchor:end}\
.xlab{fill:var(--mut);font-size:10px}.xend{text-anchor:end}\
.legend{margin-top:8px;color:var(--mut);font-size:12px}\
.legend .lg{margin-right:14px}.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:4px;vertical-align:middle}\
.tables{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:16px}\
.tables h2{font-size:15px;margin:0 0 8px}table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--bd);border-radius:10px;overflow:hidden}\
th,td{text-align:left;padding:8px 12px;border-bottom:1px solid var(--bd)}th{color:var(--mut);font-size:12px;text-transform:uppercase;letter-spacing:.04em}\
tr:last-child td{border-bottom:none}td:nth-child(n+2),th:nth-child(n+2){text-align:right;font-variant-numeric:tabular-nums}\
.codes{color:var(--mut);font-size:12px}.codes code{background:var(--bd);padding:1px 5px;border-radius:4px}\
footer{color:var(--mut);font-size:12px;margin-top:24px;border-top:1px solid var(--bd);padding-top:12px}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::ItemCounts;
    use crate::metrics::{Summary, TimePoint};
    use std::time::Duration;

    fn tp(t: f64, req: u64, ok: u64, fail: u64, cpu: Option<f64>, rss: Option<u64>) -> TimePoint {
        TimePoint {
            t_secs: t, cum_accepted: ok, cum_failed: fail,
            interval_rate: req as f64, interval_accepted: ok, interval_failed: fail,
            cpu_cores: cpu, rss_bytes: rss,
        }
    }

    fn summary(timeline: Vec<TimePoint>) -> Summary {
        Summary {
            elapsed: Duration::from_secs(3), users: 10, requests: 900, accepted: 880,
            rate_limited: 5, http_errors: 10, transport: 5,
            status_counts: vec![(202, 880), (429, 5)],
            attempted: ItemCounts { events: 500, errors: 400, ..Default::default() },
            accepted_items: ItemCounts { events: 490, errors: 390, ..Default::default() },
            p50_us: 1200, p90_us: 4500, p99_us: 9000, max_us: 25000,
            latency_samples: 900, latency_truncated: false, timeline,
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            mode_label: "isolated".into(), users: 10, duration_secs: 3,
            events_per_min: 10, issues_per_min: 10, gzip: true,
            generated_at: "2026-07-15 00:00:00 UTC".into(), ncpus: 8,
        }
    }

    #[test]
    fn maps_values_into_plot_box() {
        // y: 0 → bottom(100), max → top(0)
        assert!((map_y(0.0, 10.0, 0.0, 100.0) - 100.0).abs() < 1e-9);
        assert!((map_y(10.0, 10.0, 0.0, 100.0) - 0.0).abs() < 1e-9);
        // degenerate y_max → clamp to bottom (no NaN)
        assert!((map_y(5.0, 0.0, 0.0, 100.0) - 100.0).abs() < 1e-9);
        // x: 0 → left, x_max → right
        assert!((map_x(0.0, 4.0, 10.0, 90.0) - 10.0).abs() < 1e-9);
        assert!((map_x(4.0, 4.0, 10.0, 90.0) - 90.0).abs() < 1e-9);
        assert!((map_x(1.0, 0.0, 10.0, 90.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn renders_headline_numbers_and_all_charts_when_resources_present() {
        let s = summary(vec![
            tp(1.0, 300, 290, 10, Some(0.5), Some(50 * 1024 * 1024)),
            tp(2.0, 600, 585, 15, Some(0.9), Some(60 * 1024 * 1024)),
            tp(3.0, 900, 880, 20, Some(1.1), Some(64 * 1024 * 1024)),
        ]);
        let html = render(&s, &crate::cli::Expected { requests: 1000.0, duration_secs: 3.0 }, &meta());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("crebain"));
        assert!(html.contains("Requests / sec"));
        assert!(html.contains("CPU (cores)"));
        assert!(html.contains("Memory (RSS)"));
        assert!(html.contains("880")); // accepted total appears
        assert!(html.contains("<svg"));
    }

    #[test]
    fn omits_resource_charts_without_samples() {
        let s = summary(vec![
            tp(1.0, 300, 295, 5, None, None),
            tp(2.0, 600, 590, 10, None, None),
        ]);
        let html = render(&s, &crate::cli::Expected { requests: 1000.0, duration_secs: 3.0 }, &meta());
        assert!(html.contains("Requests / sec"));
        assert!(!html.contains("CPU (cores)"));
        assert!(!html.contains("Memory (RSS)"));
    }

    #[test]
    fn empty_timeline_still_renders() {
        let s = summary(vec![]);
        let html = render(&s, &crate::cli::Expected { requests: 0.0, duration_secs: 3.0 }, &meta());
        assert!(html.contains("no data"));
        assert!(html.starts_with("<!doctype html>"));
    }
}
