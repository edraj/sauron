//! Store report connectors: Google Play and the Apple App Store.
//!
//! Neither store answers a simple REST call with install counts, and neither
//! reports anything per *environment* — they key their data to a package name
//! or a bundle id. That is why nothing in this crate knows about environments,
//! and why `store_daily_metrics` has no `environment_id` column.
//!
//! Everything here is pure fetch-and-parse: no Postgres, no Sauron models.
//! That is what lets both connectors be tested against committed fixture files
//! with no network and no database.

pub mod apple;
pub mod google;

use chrono::NaiveDate;

pub use apple::{AppleIdentifiers, AppleProgress};
pub use google::GoogleIdentifiers;

/// One store's counts for one calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyMetric {
    pub day: NaiveDate,
    pub installs: i64,
    pub uninstalls: i64,
}

/// The two stores.
///
/// `as_str` returns the values in migration 49's CHECK constraint; they are
/// also the URL path segment and the TypeScript union in the dashboard. One
/// spelling, four places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    GooglePlay,
    AppStore,
}

impl StoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StoreKind::GooglePlay => "google_play",
            StoreKind::AppStore => "app_store",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google_play" => Some(StoreKind::GooglePlay),
            "app_store" => Some(StoreKind::AppStore),
            _ => None,
        }
    }
}

/// Look a column up by NAME and return its index, or fail naming it.
///
/// Shared by both connectors because both reports are header-bearing delimited
/// text whose column *order* is not contractual. An index-based parser that
/// shifts by one column produces numbers rather than errors, which is the worst
/// available outcome: wrong data that looks right.
pub(crate) fn column_index(headers: &csv::StringRecord, name: &str) -> anyhow::Result<usize> {
    headers
        .iter()
        .position(|h| h.trim() == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "report is missing the {name:?} column; got columns: {:?}",
                headers.iter().collect::<Vec<_>>()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_kind_round_trips_through_its_wire_string() {
        for k in [StoreKind::GooglePlay, StoreKind::AppStore] {
            assert_eq!(StoreKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(StoreKind::parse("amazon_appstore"), None);
    }

    #[test]
    fn column_index_error_names_the_missing_column() {
        let headers = csv::StringRecord::from(vec!["Date", "Event"]);
        let err = column_index(&headers, "Deletions").unwrap_err().to_string();
        assert!(err.contains("Deletions"), "got: {err}");
        assert!(
            err.contains("Event"),
            "error should list what WAS found: {err}"
        );
    }

    #[test]
    fn column_index_ignores_surrounding_whitespace() {
        let headers = csv::StringRecord::from(vec![" Date ", "Event"]);
        assert_eq!(column_index(&headers, "Date").unwrap(), 0);
    }
}
