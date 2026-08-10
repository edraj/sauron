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
const COL_DATE: &str = "Date";
const COL_INSTALLS: &str = "Installations";
const COL_DELETIONS: &str = "Deletions";

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
/// Rows are SUMMED per day. Apple segments a day's report across several rows
/// (by device, territory, source), so the daily total is their sum; taking the
/// last row per day under-reports silently, which is the worst available
/// failure here.
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
    let i_installs = column_index(&headers, COL_INSTALLS)?;
    let i_deletions = column_index(&headers, COL_DELETIONS)?;

    let mut by_day: BTreeMap<NaiveDate, (i64, i64)> = BTreeMap::new();
    for rec in rdr.records() {
        let rec = rec.context("malformed row in Apple report segment")?;
        let raw_day = rec.get(i_date).unwrap_or_default().trim();
        if raw_day.is_empty() {
            continue;
        }
        let day = NaiveDate::parse_from_str(raw_day, "%Y-%m-%d")
            .with_context(|| format!("unparseable Date {raw_day:?} in Apple report"))?;
        let entry = by_day.entry(day).or_insert((0, 0));
        entry.0 += parse_count(rec.get(i_installs));
        entry.1 += parse_count(rec.get(i_deletions));
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

async fn create_report_request(
    client: &reqwest::Client,
    token: &str,
    base: &str,
    apple_app_id: &str,
) -> anyhow::Result<String> {
    let body = serde_json::json!({
        "data": {
            "type": "analyticsReportRequests",
            "attributes": { "accessType": "ONGOING", "name": "Sauron install metrics" },
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
        if let Some(d) = inst.attributes.get("processingDate").and_then(|v| v.as_str()) {
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

    Ok((request_id, AppleProgress::Ready(fold_days(collected, since, today))))
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

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/apple_installs_deletions.csv.gz");

    fn gz(body: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(body).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn parses_gzipped_installs_and_deletions() {
        let rows = parse_report_csv(FIXTURE).expect("fixture parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].day, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(rows[0].installs, 880);
        assert_eq!(
            rows[0].uninstalls, 195,
            "Apple's Deletions column is what we call uninstalls"
        );
    }

    #[test]
    fn errors_by_name_when_deletions_column_is_absent() {
        let bytes = gz(b"Date,Installations\n2026-08-01,880\n");
        let err = parse_report_csv(&bytes).unwrap_err().to_string();
        assert!(err.contains("Deletions"), "must name the missing column, got: {err}");
    }

    #[test]
    fn rejects_input_that_is_not_gzip() {
        assert!(parse_report_csv(b"Date,Installations,Deletions\n").is_err());
    }

    #[test]
    fn aggregates_duplicate_days_within_a_segment() {
        // Apple splits a day across rows by dimension; the daily total is
        // their SUM. Taking the last row silently under-reports every day.
        let bytes = gz(b"Date,Installations,Deletions\n2026-08-01,500,100\n2026-08-01,380,95\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].installs, 880);
        assert_eq!(rows[0].uninstalls, 195);
    }

    #[test]
    fn fold_days_merges_across_segments_and_clips_to_the_window() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let rows = vec![
            DailyMetric { day: d(8, 1), installs: 5, uninstalls: 1 },
            DailyMetric { day: d(8, 1), installs: 7, uninstalls: 2 },
            // Outside the window — Apple returned it, we must not chart it.
            DailyMetric { day: d(7, 1), installs: 99, uninstalls: 99 },
        ];
        let folded = fold_days(rows, d(8, 1), d(8, 5));
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].installs, 12);
        assert_eq!(folded[0].uninstalls, 3);
    }

    #[test]
    fn thousands_separators_in_counts_are_parsed_not_truncated() {
        // "1,240" split naively on ',' becomes 1. Parsed here it is 1240.
        let bytes = gz(b"Date,Installations,Deletions\n2026-08-01,\"1,240\",\"310\"\n");
        let rows = parse_report_csv(&bytes).expect("parses");
        assert_eq!(rows[0].installs, 1240);
        assert_eq!(rows[0].uninstalls, 310);
    }
}
