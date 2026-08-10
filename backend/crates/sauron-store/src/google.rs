//! Google Play install reports.
//!
//! Play does not expose installs over an API — the Play Developer Reporting
//! API covers vitals (crash rate, ANR rate), not installs. The numbers live as
//! CSVs in the Play Console's Cloud Storage reports bucket at
//! `stats/installs/installs_{package}_{YYYYMM}_overview.csv`, read with a
//! service account.
//!
//! Two properties of those files cost a day each if met by surprise:
//!
//!  * They are **UTF-16LE with a BOM**. Decoding as UTF-8 does not error — it
//!    yields mojibake that parses as a valid CSV with zero rows.
//!  * They are **monthly**. A 90-day backfill is four object fetches, not
//!    ninety.

use anyhow::Context;
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;

use crate::{column_index, DailyMetric};

const COL_DATE: &str = "Date";
const COL_INSTALLS: &str = "Daily Device Installs";
const COL_UNINSTALLS: &str = "Daily Device Uninstalls";

/// Hosts this connector is permitted to reach.
///
/// No operator-supplied URL is ever fetched — only a bucket *name* and a
/// package name, interpolated into paths on these two fixed hosts. That is why
/// the SSRF-guarding resolver `sauron-monitor` needs is not required here.
pub const GOOGLE_HOSTS: [&str; 2] = ["oauth2.googleapis.com", "storage.googleapis.com"];

#[derive(Debug, Clone, Deserialize)]
pub struct GoogleIdentifiers {
    pub package_name: String,
    /// The Play Console reports bucket, e.g. `pubsite_prod_rev_01234567890`.
    /// Stored as a bare name; any `gs://` prefix is stripped when saved.
    pub gcs_bucket: String,
}

/// The service-account key file, as downloaded from Google Cloud.
#[derive(Debug, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: i64,
    iat: i64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Decode UTF-16LE, tolerating a present or absent BOM.
///
/// Play writes the BOM; the tolerance is for hand-made fixtures and for the day
/// Google stops writing it.
pub fn decode_utf16le(bytes: &[u8]) -> anyhow::Result<String> {
    let body = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    anyhow::ensure!(
        body.len() % 2 == 0,
        "UTF-16LE body has an odd byte length ({}); the file is truncated or not UTF-16",
        body.len()
    );
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16(&units).context("Play report is not valid UTF-16LE")
}

/// Parse one monthly overview CSV into daily metrics.
pub fn parse_installs_csv(bytes: &[u8]) -> anyhow::Result<Vec<DailyMetric>> {
    let text = decode_utf16le(bytes)?;
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr.headers().context("Play report has no header row")?.clone();

    let i_date = column_index(&headers, COL_DATE)?;
    let i_installs = column_index(&headers, COL_INSTALLS)?;
    let i_uninstalls = column_index(&headers, COL_UNINSTALLS)?;

    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.context("malformed CSV row in Play report")?;
        let raw_day = rec.get(i_date).unwrap_or_default().trim();
        if raw_day.is_empty() {
            continue;
        }
        let day = NaiveDate::parse_from_str(raw_day, "%Y-%m-%d")
            .with_context(|| format!("unparseable Date {raw_day:?} in Play report"))?;
        out.push(DailyMetric {
            day,
            installs: parse_count(rec.get(i_installs)),
            uninstalls: parse_count(rec.get(i_uninstalls)),
        });
    }
    Ok(out)
}

/// Blank cells mean zero in these reports. A non-numeric cell is a real defect,
/// but discarding the whole month over one bad cell would be worse, so it reads
/// as zero and the surrounding rows survive.
fn parse_count(cell: Option<&str>) -> i64 {
    cell.unwrap_or_default()
        .trim()
        .replace(',', "")
        .parse::<i64>()
        .unwrap_or(0)
}

/// Every `YYYYMM` the window touches, inclusive at both ends.
fn months_spanned(since: NaiveDate, today: NaiveDate) -> Vec<i32> {
    let (mut y, mut m) = (since.year(), since.month());
    let (ey, em) = (today.year(), today.month());
    let mut out = Vec::new();
    while (y, m) <= (ey, em) {
        out.push(y * 100 + m as i32);
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }
    out
}

fn object_path(ids: &GoogleIdentifiers, yyyymm: i32) -> String {
    format!(
        "stats/installs/installs_{}_{}_overview.csv",
        ids.package_name, yyyymm
    )
}

/// GCS object names go in a path SEGMENT, so `/` must be escaped — an
/// unescaped `stats/installs/...` addresses a different (nonexistent) object
/// and 404s on a bucket that has the report.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Exchange the service-account key for a read-only storage access token.
async fn access_token(client: &reqwest::Client, sa: &ServiceAccount) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        iss: &sa.client_email,
        scope: "https://www.googleapis.com/auth/devstorage.read_only",
        aud: &sa.token_uri,
        exp: now + 3600,
        iat: now,
    };
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .context("service-account private_key is not a valid RSA PEM")?;
    let assertion = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )?;

    let resp = client
        .post(&sa.token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &assertion),
        ])
        .send()
        .await
        .context("Google token endpoint unreachable")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "Google token exchange failed ({status}): {}",
        body.chars().take(300).collect::<String>()
    );
    Ok(serde_json::from_str::<TokenResponse>(&body)
        .context("Google token response was not the expected shape")?
        .access_token)
}

/// Fetch and parse every monthly report the window touches.
///
/// A month that 404s is SKIPPED, not fatal: the first month of a backfill
/// predates the app's release for every new connection, and one missing month
/// must not discard the months that did arrive.
pub async fn fetch(
    client: &reqwest::Client,
    ids: &GoogleIdentifiers,
    service_account_json: &str,
    since: NaiveDate,
    today: NaiveDate,
) -> anyhow::Result<Vec<DailyMetric>> {
    let sa: ServiceAccount = serde_json::from_str(service_account_json)
        .context("stored Google credential is not a service-account JSON key")?;
    let token = access_token(client, &sa).await?;

    let mut out = Vec::new();
    for yyyymm in months_spanned(since, today) {
        let url = format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
            ids.gcs_bucket,
            urlencode(&object_path(ids, yyyymm))
        );
        let resp = client.get(&url).bearer_auth(&token).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!(yyyymm, "no Play report for this month; skipping");
            continue;
        }
        let status = resp.status();
        anyhow::ensure!(
            status.is_success(),
            "Play report fetch failed for {yyyymm} ({status})"
        );
        let bytes = resp.bytes().await?;
        out.extend(parse_installs_csv(&bytes)?);
    }
    out.retain(|m| m.day >= since && m.day <= today);
    out.sort_by_key(|m| m.day);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/installs_com.example.app_202608_overview.csv");

    fn utf16le(s: &str) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
        bytes
    }

    #[test]
    fn parses_real_utf16le_play_report() {
        let rows = parse_installs_csv(FIXTURE).expect("fixture parses");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].day, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(rows[0].installs, 1240);
        assert_eq!(rows[0].uninstalls, 310);
        assert_eq!(
            rows[2].installs, 0,
            "a genuine zero-install day is data, not absence"
        );
        assert_eq!(rows[2].uninstalls, 12);
    }

    #[test]
    fn errors_by_name_when_a_column_is_missing() {
        // Column order in these reports is not contractual. An index-based
        // parser that shifts by one produces NUMBERS, not errors — so a
        // missing header must be loud and must say which one.
        let bytes = utf16le("Date,Package Name,Daily Device Uninstalls\n2026-08-01,com.example.app,310\n");
        let err = parse_installs_csv(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("Daily Device Installs"),
            "error must name the missing column, got: {err}"
        );
    }

    #[test]
    fn utf8_input_is_rejected_rather_than_silently_yielding_nothing() {
        // The whole reason decode_utf16le exists: this input is a perfectly
        // good UTF-8 CSV, and treating it as one would be the bug.
        let plain = b"Date,Daily Device Installs,Daily Device Uninstalls\n2026-08-01,5,1\n";
        assert!(parse_installs_csv(plain).is_err());
    }

    #[test]
    fn decodes_utf16le_with_and_without_bom() {
        assert_eq!(decode_utf16le(&[0xff, 0xfe, 0x41, 0x00, 0x42, 0x00]).unwrap(), "AB");
        assert_eq!(decode_utf16le(&[0x41, 0x00, 0x42, 0x00]).unwrap(), "AB");
    }

    #[test]
    fn rejects_odd_length_input() {
        assert!(decode_utf16le(&[0xff, 0xfe, 0x41]).is_err());
    }

    #[test]
    fn months_spanned_covers_the_backfill_window_inclusively() {
        // Files are MONTHLY. A 90-day backfill must resolve to 4 object names,
        // not 90 — getting this wrong is 90 HTTP 404s per tick per app.
        let since = NaiveDate::from_ymd_opt(2026, 5, 20).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        assert_eq!(months_spanned(since, today), vec![202605, 202606, 202607, 202608]);
    }

    #[test]
    fn months_spanned_handles_a_single_month_and_a_year_boundary() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        assert_eq!(months_spanned(d, d), vec![202608]);
        assert_eq!(
            months_spanned(
                NaiveDate::from_ymd_opt(2025, 12, 15).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 3).unwrap()
            ),
            vec![202512, 202601]
        );
    }

    #[test]
    fn object_path_matches_the_play_console_layout() {
        let ids = GoogleIdentifiers {
            package_name: "com.example.app".into(),
            gcs_bucket: "pubsite_prod_rev_01234".into(),
        };
        assert_eq!(
            object_path(&ids, 202608),
            "stats/installs/installs_com.example.app_202608_overview.csv"
        );
    }

    #[test]
    fn urlencode_escapes_the_path_separators() {
        assert_eq!(
            urlencode("stats/installs/x.csv"),
            "stats%2Finstalls%2Fx.csv",
            "an unescaped slash addresses a different object and 404s"
        );
    }
}
