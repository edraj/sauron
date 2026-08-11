//! Apple App Store install and deletion reports.
//!
//! The classic Sales & Trends API reports downloads but has no concept of an
//! uninstall. Deletions exist only in the Analytics Reports API, which is
//! request-then-poll:
//!
//!   1. `POST /v1/analyticsReportRequests`            (accessType ONGOING, once)
//!   2. `GET  /v1/analyticsReportRequests/{id}/reports`
//!   3. `GET  /v1/analyticsReports/{id}/instances?filter[granularity]=DAILY`
//!   4. `GET  /v1/analyticsReportInstances/{id}/segments`  → a gzipped CSV url
//!
//! Apple takes roughly 24-48h after step 1 before the first instance appears.
//! That window is [`AppleProgress::Pending`] — a normal state, not an error.
//! Rendering it as a failure trains admins to ignore a badge that will later
//! mean something real.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::Context;
use chrono::NaiveDate;
use serde::Deserialize;

use crate::{column_index, DailyMetric};

pub const APPLE_HOST: &str = "api.appstoreconnect.apple.com";

const REPORT_NAME: &str = "App Store Installations and Deletions";

// The report's real shape, confirmed against Apple's documentation:
//
//   Date | Event | Counts | Unique Devices | App Apple Identifier | App Name |
//   App Version | Device | Territory | Platform Version | Source Type |
//   Source Info | Page Type | Page Title | Download Type | App Download Date
//
// There are NO `Installations` / `Deletions` columns. Installs and deletions
// are rows discriminated by `Event`, and one calendar day spans many rows —
// the dimensions above are crossed — so a day's total is the SUM over its rows.
const COL_DATE: &str = "Date";
const COL_EVENT: &str = "Event";

/// Preferred count column. Apple's `Unique Devices` is "the number of unique
/// devices on which events were generated", which is the direct analogue of
/// Google Play's `Daily Device Installs`; using it keeps the two halves of the
/// chart measuring the same thing.
const COL_UNIQUE_DEVICES: &str = "Unique Devices";
/// Fallback: total event count. A redownload onto the same device counts twice
/// here, so it runs slightly higher than the device-based figure — used only
/// when a report variant omits `Unique Devices`, so the connector degrades
/// instead of failing.
const COL_COUNTS: &str = "Counts";

/// `Event` values counted as an install.
///
/// `Reinstall` is included and `Update` is deliberately NOT: Play's
/// `Daily Device Installs` counts installs and reports upgrades in a separate
/// column this connector already ignores, so excluding updates is what makes
/// the two stores comparable rather than a judgement about which is more
/// interesting. Matched case-insensitively after trimming.
const EVENT_INSTALL: &[&str] = &["install", "reinstall"];

/// `Event` values counted as an uninstall. Both spellings are accepted because
/// the column is documented by description rather than by enumerated value.
const EVENT_DELETE: &[&str] = &["delete", "deletion"];

/// `Event` values that are real, understood, and deliberately counted as
/// neither — listing them explicitly is what keeps them out of the
/// unknown-value warning below.
const EVENT_IGNORED: &[&str] = &["update"];

#[derive(Debug, Clone, Deserialize)]
pub struct AppleIdentifiers {
    pub bundle_id: String,
    /// The numeric App Store id (Apple calls it the app's "Apple ID").
    pub apple_app_id: String,
    /// App Store Connect API key issuer UUID.
    pub issuer_id: String,
    /// App Store Connect API key id.
    pub key_id: String,
    /// Vendor number from Sales & Trends. TEXT, not a number: it is an opaque
    /// identifier that can carry leading zeros, not a quantity.
    pub vendor_number: String,
}

/// Whether Apple has published anything yet.
#[derive(Debug)]
pub enum AppleProgress {
    /// Report requested; Apple has not produced an instance yet (~24-48h).
    Pending,
    Ready(Vec<DailyMetric>),
}

/// Parse one gzipped report segment.
///
/// Rows are SUMMED per day. One calendar day spans many rows — Apple crosses
/// the day by event, territory, device, app version and more — so the daily
/// total is their sum. Taking the last row per day would under-report silently,
/// which is the worst available failure here.
pub fn parse_report_csv(gzipped: &[u8]) -> anyhow::Result<Vec<DailyMetric>> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(gzipped)
        .read_to_string(&mut text)
        .context("Apple report segment is not valid gzip")?;

    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr
        .headers()
        .context("Apple report segment has no header row")?
        .clone();

    let i_date = column_index(&headers, COL_DATE)?;
    let i_event = column_index(&headers, COL_EVENT)?;
    // Prefer the device-based column; fall back rather than fail if a report
    // variant omits it. If NEITHER is present the layout is not one we
    // understand, and the error names both so the operator can see what was
    // expected against the `got columns:` list.
    let i_count = column_index(&headers, COL_UNIQUE_DEVICES)
        .or_else(|_| column_index(&headers, COL_COUNTS))
        .with_context(|| {
            format!("Apple report has neither a {COL_UNIQUE_DEVICES:?} nor a {COL_COUNTS:?} column")
        })?;

    let mut by_day: BTreeMap<NaiveDate, (i64, i64)> = BTreeMap::new();
    let mut unknown_events: std::collections::BTreeSet<String> = Default::default();

    for rec in rdr.records() {
        let rec = rec.context("malformed row in Apple report segment")?;
        let raw_day = rec.get(i_date).unwrap_or_default().trim();
        if raw_day.is_empty() {
            continue;
        }
        let day = NaiveDate::parse_from_str(raw_day, "%Y-%m-%d")
            .with_context(|| format!("unparseable Date {raw_day:?} in Apple report"))?;

        let event = rec
            .get(i_event)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let n = parse_count(rec.get(i_count));
        let entry = by_day.entry(day).or_insert((0, 0));

        if EVENT_INSTALL.contains(&event.as_str()) {
            entry.0 += n;
        } else if EVENT_DELETE.contains(&event.as_str()) {
            entry.1 += n;
        } else if !EVENT_IGNORED.contains(&event.as_str()) && !event.is_empty() {
            // Neither counted nor silently discarded. If Apple adds a value we
            // do not know, the numbers would quietly run low with nothing to
            // explain it — so name it once per segment.
            unknown_events.insert(event);
        }
    }

    if !unknown_events.is_empty() {
        tracing::warn!(
            events = ?unknown_events,
            "Apple report contained Event values this build does not map; \
             their rows were counted as neither installs nor uninstalls"
        );
    }

    Ok(by_day
        .into_iter()
        .map(|(day, (installs, uninstalls))| DailyMetric {
            day,
            installs,
            uninstalls,
        })
        .collect())
}

fn parse_count(cell: Option<&str>) -> i64 {
    cell.unwrap_or_default()
        .trim()
        .replace(',', "")
        .parse::<i64>()
        .unwrap_or(0)
}

#[derive(Debug, serde::Serialize)]
struct AppleClaims<'a> {
    iss: &'a str,
    iat: i64,
    exp: i64,
    aud: &'a str,
}

/// ES256, signed with the `.p8`. `kid` is the key id; `aud` is fixed by Apple.
fn bearer(ids: &AppleIdentifiers, p8_pem: &str) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = AppleClaims {
        iss: &ids.issuer_id,
        iat: now,
        // Apple rejects tokens with a lifetime over 20 minutes.
        exp: now + 900,
        aud: "appstoreconnect-v1",
    };
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some(ids.key_id.clone());
    let key = jsonwebtoken::EncodingKey::from_ec_pem(p8_pem.as_bytes())
        .context("stored Apple credential is not a valid .p8 EC private key")?;
    Ok(jsonwebtoken::encode(&header, &claims, &key)?)
}

#[derive(Deserialize)]
struct DataList {
    data: Vec<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    id: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

async fn get_json(client: &reqwest::Client, token: &str, url: &str) -> anyhow::Result<DataList> {
    let resp = client.get(url).bearer_auth(token).send().await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "App Store Connect request failed ({status}): {}",
        body.chars().take(300).collect::<String>()
    );
    serde_json::from_str(&body).context("unexpected App Store Connect response shape")
}

/// Find an existing ONGOING request for this app, if one is already there.
///
/// Apple allows exactly one ONGOING request per app and 409s on a second, so
/// creating blind is a permanent wedge whenever the id we persisted is not the
/// whole truth: the request outlives our row, so re-entering credentials,
/// restoring an older database, or one operator making the request by hand in
/// App Store Connect all leave a live request we have no id for. Looking first
/// makes first contact idempotent.
async fn find_report_request(
    client: &reqwest::Client,
    token: &str,
    base: &str,
    apple_app_id: &str,
) -> anyhow::Result<Option<String>> {
    let list = get_json(
        client,
        token,
        &format!(
            "{base}/v1/apps/{apple_app_id}/analyticsReportRequests\
             ?filter[accessType]=ONGOING&limit=200"
        ),
    )
    .await?;
    // `stoppedDueToInactivity` requests still occupy the app's ONGOING slot but
    // never produce another instance, so reusing one would leave the connection
    // Pending forever. Apple's own remedy is to delete and re-request, which is
    // an operator decision (it restarts the 24-48h window), so surface it
    // rather than silently adopting a dead request.
    Ok(list
        .data
        .into_iter()
        .find(|r| {
            !r.attributes
                .get("stoppedDueToInactivity")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .map(|r| r.id))
}

async fn create_report_request(
    client: &reqwest::Client,
    token: &str,
    base: &str,
    apple_app_id: &str,
) -> anyhow::Result<String> {
    if let Some(existing) = find_report_request(client, token, base, apple_app_id).await? {
        return Ok(existing);
    }
    // `accessType` is the ONLY writable attribute on this resource. There is no
    // `name` — sending one is a 409 ENTITY_ERROR.ATTRIBUTE.UNKNOWN, not a
    // warning, so the whole request fails and no report is ever created.
    let body = serde_json::json!({
        "data": {
            "type": "analyticsReportRequests",
            "attributes": { "accessType": "ONGOING" },
            "relationships": {
                "app": { "data": { "type": "apps", "id": apple_app_id } }
            }
        }
    });
    let resp = client
        .post(format!("{base}/v1/analyticsReportRequests"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    // 403 here is a ROLE problem, never a credential one, and it is worth
    // naming because this string is what lands in `last_error` and is the only
    // thing the admin sees.
    //
    // Reaching this line proves the key authenticated: `find_report_request`
    // above LISTED the app's report requests with the same token one call
    // earlier, and a bad .p8 / key_id / issuer_id is a 401 NOT_AUTHORIZED, not
    // a 403. A wrong apple_app_id would have 404'd that same GET. So the key is
    // valid and can READ analytics report requests but not CREATE one — which
    // is exactly Apple's split: only an **Admin** key may request a report type
    // for the first time, while Sales (Access to Reports) and Finance keys can
    // list requests and download reports once the request exists.
    //
    // That second remedy is real and cheaper than re-issuing a key, because
    // `find_report_request` looks before it creates: an Admin making the
    // ONGOING request by hand in App Store Connect leaves this key nothing to
    // create, and the next tick adopts it.
    if status.as_u16() == 403 {
        anyhow::bail!(
            "Apple refused to create the ongoing analytics report request (403). The API key \
             authenticated and can read this app's report requests, so the credential is fine — \
             it lacks the role to create one. Only an Admin key may request a report type for the \
             first time. Either supply an Admin key, or have an Admin create the ONGOING \
             \"{REPORT_NAME}\" request once in App Store Connect and this key will adopt it on \
             the next sync. Apple said: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    anyhow::ensure!(
        status.is_success(),
        "creating the Apple report request failed ({status}): {}",
        text.chars().take(300).collect::<String>()
    );
    let v: serde_json::Value = serde_json::from_str(&text)?;
    v["data"]["id"]
        .as_str()
        .map(str::to_string)
        .context("Apple report request response had no data.id")
}

/// Fetch installs and deletions, creating the ongoing report request on first
/// use.
///
/// Returns the (possibly newly created) request id so the caller can persist it
/// — creating a second request for the same app is wasteful and Apple may
/// reject it.
pub async fn fetch(
    client: &reqwest::Client,
    ids: &AppleIdentifiers,
    p8_pem: &str,
    request_id: Option<&str>,
    since: NaiveDate,
    today: NaiveDate,
) -> anyhow::Result<(String, AppleProgress)> {
    let token = bearer(ids, p8_pem)?;
    let base = format!("https://{APPLE_HOST}");

    let request_id = match request_id {
        Some(id) => id.to_string(),
        None => create_report_request(client, &token, &base, &ids.apple_app_id).await?,
    };

    let reports = get_json(
        client,
        &token,
        &format!(
            "{base}/v1/analyticsReportRequests/{request_id}/reports?filter[name]={}",
            REPORT_NAME.replace(' ', "%20")
        ),
    )
    .await?;

    let Some(report) = reports.data.into_iter().next() else {
        // Requested, nothing produced yet.
        return Ok((request_id, AppleProgress::Pending));
    };

    let instances = get_json(
        client,
        &token,
        &format!(
            "{base}/v1/analyticsReports/{}/instances?filter[granularity]=DAILY",
            report.id
        ),
    )
    .await?;

    let mut collected: Vec<DailyMetric> = Vec::new();
    for inst in instances.data {
        // `processingDate` is the day this instance covers. Skipping instances
        // outside the window avoids downloading every segment ever produced on
        // each tick.
        if let Some(d) = inst
            .attributes
            .get("processingDate")
            .and_then(|v| v.as_str())
        {
            if let Ok(day) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                if day < since || day > today {
                    continue;
                }
            }
        }
        let segments = get_json(
            client,
            &token,
            &format!("{base}/v1/analyticsReportInstances/{}/segments", inst.id),
        )
        .await?;
        for seg in segments.data {
            let Some(url) = seg.attributes.get("url").and_then(|v| v.as_str()) else {
                continue;
            };
            let bytes = client.get(url).send().await?.bytes().await?;
            collected.extend(parse_report_csv(&bytes)?);
        }
    }

    if collected.is_empty() {
        return Ok((request_id, AppleProgress::Pending));
    }

    Ok((
        request_id,
        AppleProgress::Ready(fold_days(collected, since, today)),
    ))
}

/// Segments from different instances can repeat a day, so fold once more at the
/// end and clip to the window.
fn fold_days(rows: Vec<DailyMetric>, since: NaiveDate, today: NaiveDate) -> Vec<DailyMetric> {
    let mut by_day: BTreeMap<NaiveDate, (i64, i64)> = BTreeMap::new();
    for m in rows {
        let e = by_day.entry(m.day).or_insert((0, 0));
        e.0 += m.installs;
        e.1 += m.uninstalls;
    }
    by_day
        .into_iter()
        .filter(|(day, _)| *day >= since && *day <= today)
        .map(|(day, (installs, uninstalls))| DailyMetric {
            day,
            installs,
            uninstalls,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Shaped like the real report: `Event`-discriminated rows, one calendar
    /// day crossed by device/territory, both count columns present.
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/apple_installs_deletions.csv.gz");

    fn gz(body: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(body).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn sums_event_rows_per_day_using_unique_devices() {
        let rows = parse_report_csv(FIXTURE).expect("fixture parses");
        assert_eq!(rows.len(), 2);

        // 2026-08-01: Install 600 (iPhone) + 180 (iPad) + Reinstall 100 = 880.
        // The Update row (4100) is excluded; Delete 195 is the uninstall side.
        assert_eq!(rows[0].day, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(
            rows[0].installs, 880,
            "install rows must be summed across dimensions"
        );
        assert_eq!(rows[0].uninstalls, 195);

        assert_eq!(rows[1].day, NaiveDate::from_ymd_opt(2026, 8, 2).unwrap());
        assert_eq!(rows[1].installs, 910);
        assert_eq!(rows[1].uninstalls, 201);
    }

    #[test]
    fn updates_are_not_counted_as_installs() {
        // The single most consequential mapping decision: Play reports upgrades
        // in a separate column this connector ignores, so counting Apple's
        // Update rows would make the App Store line several times too tall and
        // still look entirely plausible.
        let bytes =
            gz(b"Date,Event,Unique Devices\n2026-08-01,Update,5000\n2026-08-01,Install,10\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 10);
        assert_eq!(rows[0].uninstalls, 0);
    }

    #[test]
    fn reinstall_counts_as_an_install_and_delete_spellings_both_work() {
        let bytes = gz(b"Date,Event,Unique Devices\n2026-08-01,Reinstall,7\n2026-08-01,Delete,3\n2026-08-01,Deletion,4\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 7);
        assert_eq!(rows[0].uninstalls, 7);
    }

    #[test]
    fn event_matching_is_case_and_whitespace_insensitive() {
        let bytes =
            gz(b"Date,Event,Unique Devices\n2026-08-01,  INSTALL ,5\n2026-08-01,delete,2\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 5);
        assert_eq!(rows[0].uninstalls, 2);
    }

    #[test]
    fn falls_back_to_counts_when_unique_devices_is_absent() {
        let bytes = gz(b"Date,Event,Counts\n2026-08-01,Install,42\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 42);
    }

    #[test]
    fn unique_devices_wins_when_both_columns_are_present() {
        let bytes = gz(b"Date,Event,Counts,Unique Devices\n2026-08-01,Install,610,600\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(
            rows[0].installs, 600,
            "device-based column matches Play's semantics"
        );
    }

    #[test]
    fn errors_naming_both_count_columns_when_neither_is_present() {
        let bytes = gz(b"Date,Event,Territory\n2026-08-01,Install,US\n");
        let err = format!("{:#}", parse_report_csv(&bytes).unwrap_err());
        assert!(err.contains("Unique Devices"), "got: {err}");
        assert!(err.contains("Counts"), "got: {err}");
    }

    #[test]
    fn errors_by_name_when_the_event_column_is_absent() {
        // Column order and presence are not contractual; a missing
        // discriminator must be loud, not silently zero.
        let bytes = gz(b"Date,Unique Devices\n2026-08-01,880\n");
        let err = parse_report_csv(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("Event"),
            "must name the missing column, got: {err}"
        );
    }

    #[test]
    fn an_unmapped_event_value_is_not_counted_as_either() {
        // Warned about rather than guessed at. Silently folding an unknown
        // value into installs would be worse than under-reporting it.
        let bytes =
            gz(b"Date,Event,Unique Devices\n2026-08-01,Install,10\n2026-08-01,Teleport,999\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 10);
        assert_eq!(rows[0].uninstalls, 0);
    }

    #[test]
    fn rejects_input_that_is_not_gzip() {
        assert!(parse_report_csv(b"Date,Event,Counts\n").is_err());
    }

    #[test]
    fn thousands_separators_in_counts_are_parsed_not_truncated() {
        // "1,240" split naively on ',' becomes 1. Parsed here it is 1240.
        let bytes = gz("Date,Event,Unique Devices\n2026-08-01,Install,\"1,240\"\n".as_bytes());
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 1240);
    }

    #[test]
    fn fold_days_merges_across_segments_and_clips_to_the_window() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let rows = vec![
            DailyMetric {
                day: d(8, 1),
                installs: 5,
                uninstalls: 1,
            },
            DailyMetric {
                day: d(8, 1),
                installs: 7,
                uninstalls: 2,
            },
            // Outside the window - Apple returned it, we must not chart it.
            DailyMetric {
                day: d(7, 1),
                installs: 99,
                uninstalls: 99,
            },
        ];
        let folded = fold_days(rows, d(8, 1), d(8, 5));
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].installs, 12);
        assert_eq!(folded[0].uninstalls, 3);
    }
}
